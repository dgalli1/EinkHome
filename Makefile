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
# host gcc with BS_PLATFORM_SDL so bs_plat.h selects the SDL backend.
# Requires SDL2/SDL2_ttf/SDL2_image/libcurl dev packages on the host.
BUILD_PC := $(CURDIR)/sdk/build_pc.sh
OUT_PC := $(CURDIR)/build/bookshelf.pc
# App sources excluding the PocketBook backend (replaced by the SDL one).
# Recursive (=) so this stays empty-proof: a `:=` here would expand before
# SOURCES is defined below and silently make `make pc` skip rebuilding when
# app sources change (bookshelf.pc would then never track edits).
PC_SOURCES = $(filter-out platform/bs_plat_pb.c,$(SOURCES))

SOURCES := \
	core/bs_main.c \
	core/bs_net.c \
	core/bs_config.c \
	core/bs_i18n.c \
	core/bs_worker.c \
	platform/bs_plat_pb.c \
	data/bs_store.c \
	data/bs_model.c \
	data/bs_local.c \
	data/bs_extract.c \
	data/bs_progress.c \
	data/bs_licenses.c \
	ui/bs_screen.c \
	ui/bs_grid.c \
	ui/bs_topbar.c \
	ui/bs_search.c \
	ui/bs_popups.c \
	ui/bs_overlays.c \
	ui/bs_logview.c \
	ui/bs_licenses.c \
	ui/bs_browser.c \
	action/bs_downloads.c \
	action/bs_input.c \
	action/bs_launcher.c \
	action/bs_sysapp.c \
	vendor/cJSON.c

SRC_PATHS := $(addprefix $(CURDIR)/app/,$(SOURCES))

.PHONY: all clean test armhf pc

all: $(OUT)

armhf: $(OUT_ARMHF)

pc: $(OUT_PC)

test:
	scripts/test.sh

$(OUT): $(SRC_PATHS) $(wildcard $(CURDIR)/app/*/*.h) $(BUILD_ARMEL)
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(PBEMU_FIRMWARE_DIR)" \
	$(BUILD_ARMEL) $(SRC_PATHS) \
		-I/work/app/core -I/work/app/data -I/work/app/ui \
		-I/work/app/action -I/work/app/vendor -I/work/app/platform \
		--output build/bookshelf.app

$(OUT_ARMHF): $(SRC_PATHS) $(wildcard $(CURDIR)/app/*/*.h) $(BUILD_ARMHF)
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(ARMHF_FIRMWARE_DIR)" \
	$(BUILD_ARMHF) $(SRC_PATHS) \
		-I/work/app/platform \
		--output build/bookshelf.armhf.app

$(OUT_PC): $(addprefix $(CURDIR)/app/,$(PC_SOURCES)) app/platform/bs_plat_sdl.c $(BUILD_PC)
	mkdir -p $(CURDIR)/build
	$(BUILD_PC) --output build/bookshelf.pc

clean:
	rm -f $(OUT) $(OUT_ARMHF) $(OUT_PC)
