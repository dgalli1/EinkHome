# EinkHome Makefile — build the EinkHome guest app.
#
# The source list lives HERE and only here; scripts/run.sh,
# scripts/run-visible.sh, scripts/install-device.sh and tests/ all
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

SOURCES := \
	core/bs_main.c \
	core/bs_net.c \
	core/bs_config.c \
	core/bs_i18n.c \
	core/bs_worker.c \
	data/bs_store.c \
	data/bs_model.c \
	data/bs_local.c \
	data/bs_extract.c \
	data/bs_progress.c \
	ui/bs_screen.c \
	ui/bs_grid.c \
	ui/bs_topbar.c \
	ui/bs_search.c \
	ui/bs_popups.c \
	ui/bs_overlays.c \
	ui/bs_logview.c \
	ui/bs_browser.c \
	action/bs_downloads.c \
	action/bs_input.c \
	action/bs_launcher.c \
	vendor/cJSON.c

SRC_PATHS := $(addprefix $(CURDIR)/app/,$(SOURCES))

.PHONY: all clean test

all: $(OUT)

test:
	scripts/test.sh

$(OUT): $(SRC_PATHS) $(wildcard $(CURDIR)/app/*/*.h) $(BUILD_ARMEL)
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(PBEMU_FIRMWARE_DIR)" \
	$(BUILD_ARMEL) $(SRC_PATHS) \
		-I/work/app/core -I/work/app/data -I/work/app/ui \
		-I/work/app/action -I/work/app/vendor \
		--output build/bookshelf.app

clean:
	rm -f $(OUT)
