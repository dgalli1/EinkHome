# EinkHome Makefile — build the Rust guest app.
#
# The app is the eh_ui Rust workspace: the PocketBook .app is the eh_pb
# staticlib plus the sdk/pb-demo/main.c shim, linked against the firmware
# rootfs by sdk/build_armel.sh / build_armhf.sh (arm-linux-gnueabi-gcc
# inside the pbdev container).  Cross C compilation is needed only for
# the shim and libsqlite3-sys's bundled C — cargo zigbuild drives both
# with zig, pinning the firmware's glibc explicitly.
#
# The binary keeps the name bookshelf.app — the firmware's home task is
# selected by that exact name (see README.md).
#
# Usage:
#     make            # build/bookshelf.app     (emulator + armel devices)
#     make armhf      # build/bookshelf.armhf.app (InkPad One)
#     make pc         # build/bookshelf.pc      (SDL host binary, visible)
#     make test-host  # build/bookshelf.test    (headless IPC host, e2e)
#     make test-rust  # the Rust workspace unit tests (cargo test)
#     make clean
#
# Prerequisites:
#     rustup target add armv7-unknown-linux-gnueabi armv7-unknown-linux-gnueabihf
#     cargo install cargo-zigbuild   (or: pacman -S zig)
#     podman image ls localhost/pbdev   (sdk/install-sdk.sh)

PBEMU_DIR := $(abspath $(CURDIR)/pbemu)
PBEMU_FIRMWARE_DIR ?= $(PBEMU_DIR)/U633_6.8.2817
ARMHF_FIRMWARE_DIR ?= $(PBEMU_DIR)/U1030_6.11.1437

EH_UI := $(CURDIR)/eh_ui
ARM_TARGET := armv7-unknown-linux-gnueabi
ARMHF_TARGET := armv7-unknown-linux-gnueabihf
RUST_APP_LIB := $(EH_UI)/target/$(ARM_TARGET)/release/libeh_pb.a
RUST_APP_LIB_ARMHF := $(EH_UI)/target/$(ARMHF_TARGET)/release/libeh_pb.a
HOST_BIN := $(EH_UI)/target/release/bookshelf-test

BUILD_ARMEL := $(CURDIR)/sdk/build_armel.sh
BUILD_ARMHF := $(CURDIR)/sdk/build_armhf.sh

OUT := $(CURDIR)/build/bookshelf.app
OUT_ARMHF := $(CURDIR)/build/bookshelf.armhf.app
OUT_PC := $(CURDIR)/build/bookshelf.pc
OUT_TEST := $(CURDIR)/build/bookshelf.test

# Everything under eh_ui/crates plus the two manifests: any change
# rebuilds the staticlib (cargo's own dep tracking makes this cheap).
RUST_SRCS := $(EH_UI)/Cargo.toml $(wildcard $(EH_UI)/crates/*/Cargo.toml) $(wildcard $(EH_UI)/crates/*/src/*.rs) $(wildcard $(EH_UI)/crates/*/src/*/*.rs)

.PHONY: all armhf pc test-host test test-rust fmt doc-check help lint clippy lint-py clean

all: $(OUT)

armhf: $(OUT_ARMHF)

pc: $(OUT_PC)

test-host: $(OUT_TEST)

test:
	scripts/test.sh

test-rust:
	cd $(EH_UI) && cargo test --workspace

verify: fmt clippy doc-check test-rust lint-py

lint: clippy fmt doc-check lint-py

clippy:
	cd $(EH_UI) && cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cd $(EH_UI) && cargo fmt --check

doc-check:
	cd $(EH_UI) && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps


lint-py:
	@ruff check --fix api scripts tests
	@ruff format --check api
	@MYPYPATH="$(CURDIR)/api" mypy --config-file mypy.ini \
		--explicit-package-bases api/api api/providers api/storage
	@rm -f .coverage
	@python3 -m pytest api/tests -q --cov=api/api --cov=api/providers \
		--cov=api/storage --cov-report=term; rc=$$?; rm -f .coverage; exit $$rc

$(RUST_APP_LIB): $(RUST_SRCS)
	cd $(EH_UI) && cargo zigbuild --release \
		--target $(ARM_TARGET).2.23 -p eh_pb
	# MuPDF's resource blobs come out of mupdf-sys's make as host-ELF
	# objects; relink them for ARM before anything consumes the archive.
	scripts/fix-arm-archive.sh $@

$(RUST_APP_LIB_ARMHF): $(RUST_SRCS)
	cd $(EH_UI) && cargo zigbuild --release \
		--target $(ARMHF_TARGET).2.23 -p eh_pb
	FIX_CROSS_PREFIX=arm-linux-gnueabihf scripts/fix-arm-archive.sh $@

$(HOST_BIN): $(RUST_SRCS)
	cd $(EH_UI) && cargo build --release -p eh_host

$(OUT): $(RUST_APP_LIB) sdk/pb-demo/main.c sdk/build_armel.sh
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(PBEMU_FIRMWARE_DIR)" \
	$(BUILD_ARMEL) sdk/pb-demo/main.c $(RUST_APP_LIB) \
		--output build/bookshelf.app

$(OUT_ARMHF): $(RUST_APP_LIB_ARMHF) sdk/pb-demo/main.c sdk/build_armhf.sh
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(ARMHF_FIRMWARE_DIR)" \
	$(BUILD_ARMHF) sdk/pb-demo/main.c $(RUST_APP_LIB_ARMHF) \
		--output build/bookshelf.armhf.app

$(OUT_PC): $(HOST_BIN)
	mkdir -p $(CURDIR)/build
	cp $(HOST_BIN) $(OUT_PC)

$(OUT_TEST): $(HOST_BIN)
	mkdir -p $(CURDIR)/build
	cp $(HOST_BIN) $(OUT_TEST)

clean:
	rm -f $(OUT) $(OUT_ARMHF) $(OUT_PC) $(OUT_TEST)
	cd $(EH_UI) && cargo clean

HELP_VARS = $(filter HELP_TARGETS,$(.VARIABLES))
help:
	@echo "EinkHome build targets:"
	@echo "  make            build/bookshelf.app     (emulator + armel devices)"
	@echo "  make armhf      build/bookshelf.armhf.app (InkPad One)"
	@echo "  make pc         build/bookshelf.pc      (SDL host binary, visible window)"
	@echo "  make test-host  build/bookshelf.test    (headless IPC host, e2e)"
	@echo "  make test       rust + api unit tests + emulator e2e suite"
	@echo "  make test-rust  rust workspace unit tests only"
	@echo "  make fmt        check rust formatting (rustfmt)"
	@echo "  make clippy     rust linter (pedantic subset, -D warnings)"
	@echo "  make doc-check  rustdoc with warnings denied"
	@echo "  make lint       clippy + fmt + doc-check + python lints"
	@echo "  make lint-py    python only: ruff, mypy, api tests"
	@echo "  make clean      remove built artifacts"

.PHONY: help
