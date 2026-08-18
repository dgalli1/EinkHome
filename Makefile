# EinkHome Makefile — build the EinkHome guest app.
#
# The source list lives HERE and only here; scripts/run.sh,
# scripts/run-visible-pb.sh, scripts/install-device.sh and tests/ all
# delegate to `make`.  Compilation is done by sdk/build_armel.sh
# (arm-linux-gnueabi-gcc inside the pbdev container, linked against
# the firmware rootfs staged in the pbemu submodule — the wrapper
# mounts it via PBEMU_FIRMWARE_DIR).  The app headers live in the
# app/{core,data,ui,action,vendor} subdirs, each exposed via -I; the
# ELF lands in build/.
#
# Usage:
#     make            # build build/bookshelf.app
#     make clean      # remove the built ELF
#
# Note: the binary keeps the name bookshelf.app — the firmware's home
# task is selected by that exact name (see README.md).

PBEMU_DIR := $(abspath $(CURDIR)/pbemu)
# Firmware rootfs the app links against.  Override on the command line
# or via the environment to build against another staged firmware
# (e.g. `make PBEMU_FIRMWARE_DIR=$(PWD)/pbemu/U634k3_6.10.2544`).
PBEMU_FIRMWARE_DIR ?= $(PBEMU_DIR)/U633_6.8.2817
BUILD_ARMEL := $(CURDIR)/sdk/build_armel.sh
OUT := $(CURDIR)/build/bookshelf.app

# Hard-float (armhf) variant for the InkPad One (U1030_6.11.1437) —
# the only supported device whose firmware is armhf.  build_armhf.sh
# links against that firmware's own armhf libinkview/libhwconfig.
ARMHF_FIRMWARE_DIR ?= $(PBEMU_DIR)/U1030_6.11.1437
BUILD_ARMHF := $(CURDIR)/sdk/build_armhf.sh
OUT_ARMHF := $(CURDIR)/build/bookshelf.armhf.app

# PC (SDL2) native desktop build — the second platform backend.  The
# same app sources minus the PocketBook backend, plus sdk/build_pc.sh:
# host gcc with EH_PLATFORM_SDL so eh_plat.h selects the SDL backend.
# Requires SDL2/SDL2_ttf/SDL2_image/libcurl dev packages on the host.
BUILD_PC := $(CURDIR)/sdk/build_pc.sh
OUT_PC := $(CURDIR)/build/bookshelf.pc
# App sources excluding the PocketBook backend (replaced by the SDL one).
# Recursive (=) so this stays empty-proof: a `:=` here would expand before
# SOURCES is defined below and silently make `make pc` skip rebuilding when
# app sources change (bookshelf.pc would then never track edits).
PC_SOURCES = $(filter-out platform/eh_plat_pb.c,$(SOURCES))

# Rust native library (eh_lib): book metadata/cover extraction + future
# Rust-backed helpers.  FFI surface declared in app/data/eh_extract.h (used
# by eh_local.c/eh_grid.c).  Built by scripts/build-rust.sh; a separate
# archive per target — armv7 (+armhf) for devices / the emulator, x86_64
# for the PC/SDL build.
RUST_LIB_ARM := $(CURDIR)/build/libeh_lib.a
RUST_LIB_ARMHF := $(CURDIR)/build/libeh_lib_armhf.a
RUST_LIB_PC := $(CURDIR)/build/libeh_lib_host.a
RUST_CRATE := $(CURDIR)/eh_lib

SOURCES := \
	core/eh_main.c \
	core/eh_net.c \
	core/eh_config.c \
	core/eh_i18n.c \
	core/eh_worker.c \
	platform/eh_plat_pb.c \
	data/eh_store.c \
	data/eh_model.c \
	data/eh_local.c \
	data/eh_progress.c \
	data/eh_licenses.c \
	ui/eh_screen.c \
	ui/eh_grid.c \
	ui/eh_topbar.c \
	ui/eh_search.c \
	ui/eh_popups.c \
	ui/eh_overlays.c \
	ui/eh_logview.c \
	ui/eh_licenses.c \
	ui/eh_browser.c \
	action/eh_downloads.c \
	action/eh_input.c \
	action/eh_launcher.c \
	action/eh_sysapp.c \
	vendor/cJSON.c

SRC_PATHS := $(addprefix $(CURDIR)/app/,$(SOURCES))

.PHONY: all clean test armhf pc lint lint-c lint-py compile-commands build-rust

all: $(OUT)

# Build the Rust extraction staticlibs.  Requires the armv7 Rust target:
#   rustup target add armv7-unknown-linux-gnueabi
build-rust: $(RUST_LIB_ARM) $(RUST_LIB_ARMHF) $(RUST_LIB_PC)
	@echo "Rust extraction libs ready"

$(RUST_LIB_ARM): $(wildcard $(RUST_CRATE)/src/*.rs) $(RUST_CRATE)/Cargo.toml
	$(CURDIR)/scripts/build-rust.sh arm

$(RUST_LIB_ARMHF): $(wildcard $(RUST_CRATE)/src/*.rs) $(RUST_CRATE)/Cargo.toml
	$(CURDIR)/scripts/build-rust.sh armhf

$(RUST_LIB_PC): $(wildcard $(RUST_CRATE)/src/*.rs) $(RUST_CRATE)/Cargo.toml
	$(CURDIR)/scripts/build-rust.sh host

armhf: $(OUT_ARMHF)

pc: $(OUT_PC)

test:
	scripts/test.sh

# Static analysis.  `make lint` runs everything; the -c/-py suffixed
# targets isolate one half.  Each emits a non-zero exit when the tool
# reports above its (strict) threshold, which is what CI gates on.
lint: lint-c lint-py

# C: generate the compile DB, then clang-tidy over every app source,
# cppcheck (disposable pbdev container), and lizard (complexity +
# duplication gates).
lint-c:
	@python3 scripts/gen-compile-commands.py --output build/compile_commands.json
	@scripts/run-cppcheck.sh
	@python3 scripts/lint-lizard.py

# Python: ruff (lint over the main repo's Python; format-check over api/
# only — the tests/scripts suite is not formatter-normalised and forcing
# a mass reformat is unwanted churn).  mypy over the API production
# modules, and a coverage gate on the API.
lint-py:
	@ruff check --fix api scripts tests
	@ruff format --check api
	@MYPYPATH="$(CURDIR)/api" mypy --config-file mypy.ini \
		--explicit-package-bases api/api api/providers api/storage
	@rm -f .coverage
	@python3 -m pytest api/tests -q --cov=api/api --cov=api/providers \
		--cov=api/storage --cov-report=term; rc=$$?; rm -f .coverage; exit $$rc

# Regenerate build/compile_commands.json (clang-tidy -p target).
compile-commands:
	@python3 scripts/gen-compile-commands.py --output build/compile_commands.json

$(OUT): $(SRC_PATHS) $(wildcard $(CURDIR)/app/*/*.h) $(BUILD_ARMEL) $(RUST_LIB_ARM)
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(PBEMU_FIRMWARE_DIR)" \
	$(BUILD_ARMEL) $(SRC_PATHS) $(RUST_LIB_ARM) \
		-I/work/app/core -I/work/app/data -I/work/app/ui \
		-I/work/app/action -I/work/app/vendor -I/work/app/platform \
		--output build/bookshelf.app

$(OUT_ARMHF): $(SRC_PATHS) $(wildcard $(CURDIR)/app/*/*.h) $(BUILD_ARMHF) $(RUST_LIB_ARMHF)
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(ARMHF_FIRMWARE_DIR)" \
	$(BUILD_ARMHF) $(SRC_PATHS) $(RUST_LIB_ARMHF) \
		-I/work/app/platform \
		--output build/bookshelf.armhf.app

$(OUT_PC): $(addprefix $(CURDIR)/app/,$(PC_SOURCES)) app/platform/eh_plat_sdl.c $(wildcard $(CURDIR)/app/*/*.h) $(BUILD_PC) $(RUST_LIB_PC)
	mkdir -p $(CURDIR)/build
	$(BUILD_PC) --output build/bookshelf.pc $(RUST_LIB_PC)

clean:
	rm -f $(OUT) $(OUT_ARMHF) $(OUT_PC)
	rm -f $(RUST_LIB_ARM) $(RUST_LIB_ARMHF) $(RUST_LIB_PC)
