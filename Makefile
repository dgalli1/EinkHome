# EinkHome Makefile — build the EinkHome guest app.
#
# The source list lives HERE and only here; scripts/run.sh,
# scripts/run-visible.sh, scripts/install-device.sh and tests/ all
# delegate to `make`.  Compilation is done by sdk/build_armel.sh
# (arm-linux-gnueabi-gcc inside the pbdev container, linked against
# the firmware rootfs staged in the pbemu submodule — the wrapper
# mounts it via PBEMU_FIRMWARE_DIR).  The include dir is exposed
# through PBEMU_APP_INCLUDE_DIR; the ELF lands in build/.
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
	cJSON.c \
	bs_browser.c \
	bs_config.c \
	bs_downloads.c \
	bs_extract.c \
	bs_grid.c \
	bs_i18n.c \
	bs_input.c \
	bs_launcher.c \
	bs_local.c \
	bs_logview.c \
	bs_main.c \
	bs_model.c \
	bs_net.c \
	bs_overlays.c \
	bs_popups.c \
	bs_progress.c \
	bs_screen.c \
	bs_search.c \
	bs_store.c \
	bs_topbar.c \
	bs_worker.c

SRC_PATHS := $(addprefix $(CURDIR)/app/,$(SOURCES))

.PHONY: all clean test

all: $(OUT)

test:
	scripts/test.sh

$(OUT): $(SRC_PATHS) $(wildcard $(CURDIR)/app/*.h) $(BUILD_ARMEL)
	mkdir -p $(CURDIR)/build
	PBEMU_FIRMWARE_DIR="$(PBEMU_FIRMWARE_DIR)" \
	PBEMU_APP_INCLUDE_DIR=/work/app \
	$(BUILD_ARMEL) $(SRC_PATHS) --output build/bookshelf.app

clean:
	rm -f $(OUT)
