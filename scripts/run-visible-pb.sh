#!/bin/sh
#
# run-visible-pb.sh — build, stage, and launch EinkHome (the PocketBook
# bookshelf replacement) in the pbemu emulator WITH the Wayland viewer
# window, so you can see and interact with it.
#
# This is the interactive counterpart to run.sh (which runs headless with
# --no-viewer for automated screenshots).  Steps (shared bootstrap lives
# in lib-run.sh):
#
#   1. Build the guest ELF (skip with --no-build).
#   2. (Re)start the API server on 127.0.0.1:8765.
#   3. Stop any running emulator container.
#   4. Stage the ELF + bookshelf.cfg into .live so monitor.app launches it.
#   5. Start the emulator WITH the viewer + audio relay. The Wayland window
#      appears on your desktop; tap the "S" button to sync the book list.
#   6. Stage the freshly built ELF INTO the running container and restart
#      bookshelf.app so monitor.app respawns OUR binary.
#
# Usage:
#   scripts/run-visible-pb.sh            # build + launch
#   scripts/run-visible-pb.sh --no-build # skip the ELF rebuild (faster)
#
# Stop everything afterwards with: pbemu/pbemu stop
#
# NOTE: shares the dev-server pidfile + log (/tmp/pbemu-api.{pid,log})
# with run.sh / run-visible-sdl.sh — only one may run at a time.  On
# success this script intentionally leaves the emulator + API server
# running; on failure a trap stops both and drops the pidfile.
#
set -eu

. "$(dirname "$0")/lib-run.sh"
eh_run_env

cleanup_on_error() { eh_run_cleanup; }
trap cleanup_on_error EXIT

DO_BUILD=1
for arg in "$@"; do
	case "${arg}" in
	--no-build) DO_BUILD=0 ;;
	-h | --help)
		sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'
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
	echo "==> 1/6  building bookshelf.app"
	make all
else
	echo "==> 1/6  skipping build (--no-build)"
	if [ ! -f "build/bookshelf.app" ]; then
		echo "ERROR: build/bookshelf.app missing; run without --no-build first" >&2
		exit 1
	fi
fi

eh_run_api_start

echo "==> 3/6  stopping any running emulator"
eh_run_stop_emulator

eh_run_stage_live build/bookshelf.app

echo "==> 5/6  starting emulator WITH viewer"
# --network=host so the guest reaches the API server at 127.0.0.1.
# No --no-build here: pbemu auto-builds any missing support artifacts
# (shim/informer/viewer) on first run, then reuses them.
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" \
	"${PBEMU_DIR}/pbemu" start "${FIRMWARE}"
# "pbemu start" returns once the container is created, but it may still
# be initializing.  Poll until it reports running so the podman cp/exec
# below fail loudly only on a real failure.
eh_run_wait_container

eh_run_stage_container build/bookshelf.app

cat <<EOF

Done. The Wayland viewer window should now be on your desktop.

  - Tap the "S" button (top-right) to sync the book list from the API.
  - The "⋯" menu opens Settings (API host / key / reader).
  - API server log:  ${API_LOGFILE}  (pid $(cat "${API_PIDFILE}"))

Stop everything with:  "${PBEMU_DIR}/pbemu" stop
EOF