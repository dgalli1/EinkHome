# EinkHome Makefile — build the EinkHome guest app.
#
# The source list lives HERE and only here; scripts/run.sh,
# scripts/run-visible.sh, scripts/install-device.sh and tests/ all
# delegate to `make`.  Compilation is done by the pbemu submodule's
# sdk/build_armel.sh (arm-linux-gnueabi-gcc inside the pbdev
# container, linked against the firmware rootfs staged in the
# submodule).  Our app dir is exposed to the container through
# PBEMU_EXTRA_MOUNTS and the include dir through
# PBEMU_APP_INCLUDE_DIR; the ELF lands in build/.
#
# Usage:
#     make            # build build/bookshelf.app
#     make clean      # remove the built ELF
#
# Note: the binary keeps the name bookshelf.app — the firmware's home
# task is selected by that exact name (see README.md).

PBEMU_DIR := $(abspath $(CURDIR)/pbemu)
BUILD_ARMEL := $(PBEMU_DIR)/sdk/build_armel.sh
OUT := $(CURDIR)/build/bookshelf.app

SOURCES := \
	bs_i18n.c \
	bs_config.c \
	bs_model.c \
	bs_net.c \
	bs_ui.c \
	bs_input.c \
	bs_launcher.c \
	bs_downloads.c \
	bs_folder.c \
	bs_local.c \
	bs_browse.c \
	bs_extract.c \
	bs_progress.c \
	bs_store.c \
	bs_main.c

SRC_PATHS := $(addprefix $(CURDIR)/app/,$(SOURCES))

.PHONY: all clean

all: $(OUT)

$(OUT): $(SRC_PATHS) $(BUILD_ARMEL)
	mkdir -p $(CURDIR)/build
	PBEMU_EXTRA_MOUNTS="$(CURDIR):/work/einkhome" \
	PBEMU_APP_INCLUDE_DIR=/work/einkhome/app \
	$(BUILD_ARMEL) $(SRC_PATHS) --output /work/einkhome/build/bookshelf.app

clean:
	rm -f $(OUT)
