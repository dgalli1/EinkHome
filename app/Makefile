# bookshelf/Makefile — build the bookshelf guest app.
#
# The source list lives HERE and only here: run.sh, run-visible.sh,
# install-device.sh and tests/test_bookshelf.py all delegate to `make`.
# The actual cross-compile (arm-linux-gnueabi-gcc inside the pbdev
# container, linked against the firmware rootfs) is done by
# sdk/build_armel.sh, which owns the toolchain flags.
#
# Usage:
#     make -C bookshelf           # build build/bookshelf.app
#     make -C bookshelf clean     # remove the built ELF

REPO_ROOT := $(abspath $(CURDIR)/..)
BUILD_ARMEL := $(REPO_ROOT)/sdk/build_armel.sh
OUT := $(REPO_ROOT)/build/bookshelf.app

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

SRC_PATHS := $(addprefix $(CURDIR)/,$(SOURCES))

.PHONY: all clean

all: $(OUT)

$(OUT): $(SRC_PATHS) $(BUILD_ARMEL)
	$(BUILD_ARMEL) $(SRC_PATHS) --output build/bookshelf.app

clean:
	rm -f $(OUT)
