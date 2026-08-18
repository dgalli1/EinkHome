#!/bin/sh
#
# build_pc.sh - Build the EinkHome app as a native PC binary (x86_64),
# rendering to an SDL2 window (Wayland or X11, whichever SDL picks).
#
# This is the "second platform" backend: the app source is unchanged; we
# compile it on the host with EH_PLATFORM_SDL defined so app/platform/
# eh_plat.h selects the SDL compat headers, and we link the SDL surface
# implementation (app/platform/eh_plat_sdl.c) which provides every
# inkview/hwconfig symbol the app uses.
#
# Usage:
#     sdk/build_pc.sh                      # build/bookshelf.pc
#     sdk/build_pc.sh --output path        # custom output
#     sdk/build_pc.sh <src.c> ...          # extra sources/objects last
#
# SDL2, SDL2_ttf, SDL2_image and libcurl dev packages are required on
# the host.
set -eu

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)

OUTPUT=""
SRCS=""
while [ "$#" -gt 0 ]; do
	case "$1" in
	--output)
		OUTPUT="$2"
		shift 2
		continue
		;;
	--help | -h)
		sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	--* | -*)
		# Splice any other flag through as a gcc arg.
		EXTRA="${EXTRA:-} $1"
		shift
		continue
		;;
	esac
	case "$1" in
	*.c)
		SRCS="${SRCS} $1"
		;;
	*)
		EXTRA="${EXTRA:-} $1"
		;;
	esac
	shift
done

# Default: the app sources (mirrors Makefile's SOURCES, plus the SDL backend).
if [ -z "${SRCS}" ]; then
	SRCS="${REPO_ROOT}/app/platform/eh_plat_sdl.c \
		${REPO_ROOT}/app/core/eh_main.c \
		${REPO_ROOT}/app/core/eh_net.c \
		${REPO_ROOT}/app/core/eh_config.c \
		${REPO_ROOT}/app/core/eh_i18n.c \
		${REPO_ROOT}/app/core/eh_worker.c \
		${REPO_ROOT}/app/data/eh_store.c \
		${REPO_ROOT}/app/data/eh_model.c \
		${REPO_ROOT}/app/data/eh_local.c \
		${REPO_ROOT}/app/data/eh_progress.c \
		${REPO_ROOT}/app/data/eh_licenses.c \
		${REPO_ROOT}/app/ui/eh_screen.c \
		${REPO_ROOT}/app/ui/eh_grid.c \
		${REPO_ROOT}/app/ui/eh_topbar.c \
		${REPO_ROOT}/app/ui/eh_search.c \
		${REPO_ROOT}/app/ui/eh_popups.c \
		${REPO_ROOT}/app/ui/eh_overlays.c \
		${REPO_ROOT}/app/ui/eh_logview.c \
		${REPO_ROOT}/app/ui/eh_licenses.c \
		${REPO_ROOT}/app/ui/eh_browser.c \
		${REPO_ROOT}/app/action/eh_downloads.c \
		${REPO_ROOT}/app/action/eh_input.c \
		${REPO_ROOT}/app/action/eh_launcher.c \
		${REPO_ROOT}/app/action/eh_sysapp.c \
		${REPO_ROOT}/app/vendor/cJSON.c"
fi
if [ -z "${OUTPUT}" ]; then
	OUTPUT="${REPO_ROOT}/build/bookshelf.pc"
fi
mkdir -p "$(dirname "${OUTPUT}")"

CFLAGS="-I${REPO_ROOT}/app/core -I${REPO_ROOT}/app/data -I${REPO_ROOT}/app/ui"
CFLAGS="${CFLAGS} -I${REPO_ROOT}/app/action -I${REPO_ROOT}/app/vendor"
CFLAGS="${CFLAGS} -I${REPO_ROOT}/app/platform"
CFLAGS="${CFLAGS} -Wall -Wextra -O2 -g -DEH_PLATFORM_SDL"
# The IPC control socket (headless test driving) is TEST-ONLY: it is
# compiled in only when EH_ENABLE_TEST_IPC=1, so a normal `make pc`
# (the interactive desktop build) has no control socket.  The e2e
# fixture builds with this flag set.
if [ "${EH_ENABLE_TEST_IPC:-0}" = "1" ]; then
	CFLAGS="${CFLAGS} -DEH_ENABLE_TEST_IPC"
	echo "  (IPC control socket enabled — test build)"
fi
# Shellcheck: word splitting is intended for SRCS/EXTRA.
# shellcheck disable=SC2086
SDLPKG=$(pkg-config --cflags --libs sdl2 SDL2_ttf SDL2_image)
# shellcheck disable=SC2086
KPKG=$(pkg-config --cflags --libs libcurl sqlite3 zlib)
# shellcheck disable=SC2086
cc \
	${CFLAGS} \
	${SRCS} \
	${EXTRA:-} \
	-o "${OUTPUT}" \
	${SDLPKG} \
	-lm -lpthread \
	${KPKG}

echo "Built: ${OUTPUT}"
echo "Run with: ${OUTPUT}  (needs a Wayland or X11 display)"