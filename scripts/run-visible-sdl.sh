#!/bin/sh
#
# run-visible-sdl.sh — build and launch EinkHome as a NATIVE PC binary
# (x86_64, SDL2 window).  This is the desktop (Wayland/X11) counterpart to
# run-visible-pb.sh: no emulator, no qemu — the same app source compiled
# for the host with the SDL render backend (bs_plat_sdl.c).
#
# Steps (shared bootstrap in lib-run.sh):
#   1. Build the PC binary (build/bookshelf.pc) via sdk/build_pc.sh.
#   2. (Re)start the API server on 127.0.0.1:8765 — the same mock/everyday
#      server, so the synced library populates in the window.
#   3. Write build/bookshelf.cfg pointing at the API.
#   4. Launch bookshelf.pc; the SDL window appears on your desktop.
#
# Usage:
#   scripts/run-visible-sdl.sh            # build + launch
#   scripts/run-visible-sdl.sh --no-build # skip the rebuild (faster)
#   scripts/run-visible-sdl.sh --host URL # API base URL override
#
# Stop the app with Ctrl-C (also stops the API server it started).
#
# NOTE: shares the dev-server pidfile + log (/tmp/pbemu-api.{pid,log})
# with run.sh / run-visible-pb.sh — only one may run at a time.
#
set -eu

. "$(dirname "$0")/lib-run.sh"
bs_run_env

cleanup_on_error() { bs_run_cleanup; }
# On the SDL path the app runs in the foreground; Ctrl-C (INT) should stop
# everything too.  EXIT alone is installed for the shared error cleanup.
trap cleanup_on_error EXIT

DO_BUILD=1
API_URL_OVERRIDE=""
for arg in "$@"; do
	case "${arg}" in
	--no-build) DO_BUILD=0 ;;
	--host)
		API_URL_OVERRIDE="${2:?--host requires a URL}"
		shift 2
		;;
	-h | --help)
		sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "ERROR: unknown argument: ${arg}" >&2
		exit 1
		;;
	esac
done

cd "${REPO_ROOT}"

if [ "${DO_BUILD}" -eq 1 ]; then
	echo "==> 1/3  building bookshelf.pc (SDL backend)"
	make pc
else
	echo "==> 1/3  skipping build (--no-build)"
	if [ ! -f "build/bookshelf.pc" ]; then
		echo "ERROR: build/bookshelf.pc missing; run without --no-build first" >&2
		exit 1
	fi
fi

bs_run_api_start

cd "${REPO_ROOT}"
echo "==> 3/3  writing build/bookshelf.cfg + launching SDL window"
if [ -n "${API_URL_OVERRIDE}" ]; then
	API_BASE="${API_URL_OVERRIDE}"
else
	API_BASE="http://127.0.0.1:${API_PORT}"
fi
cat >build/bookshelf.cfg <<CFGEOF
api_url=${API_BASE}
api_token=pbemu-dev-token
CFGEOF
chmod 0644 build/bookshelf.cfg
echo "  wrote build/bookshelf.cfg (api_url=${API_BASE})"

echo
echo "  launching: build/bookshelf.pc  (Ctrl-C to stop; brings the API down too)"
echo
./build/bookshelf.pc
_rc=$?
echo "  bookshelf.pc exited (${_rc}) — stopping the API server"
exit "${_rc}"