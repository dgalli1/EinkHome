#!/bin/sh
#
# run-visible.sh — build, stage, and launch the pbemu bookshelf replacement
# WITH the Wayland viewer window, so you can see and interact with it.
#
# This is the interactive counterpart to run.sh (which runs headless with
# --no-viewer for automated screenshots). Steps:
#
#   1. Build the guest ELF against the PocketBook SDK (skip with --no-build).
#   2. (Re)start the pbemu API server on 127.0.0.1:8765 so the in-emulator
#      app has a target to talk to.
#   3. Stop any running emulator container.
#   4. Stage the ELF + bookshelf.cfg into .live so monitor.app launches it.
#   5. Start the emulator WITH the viewer + audio relay. The Wayland window
#      appears on your desktop; tap the "S" button to sync the book list.
#
# Usage:
#   bookshelf/run-visible.sh             # build + launch
#   bookshelf/run-visible.sh --no-build  # skip the ELF rebuild (faster)
#
# Stop everything afterwards with: ./pbemu stop
#
set -eu

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)
CONTAINER="${PBEMU_CONTAINER:-pb-pocketbook-ui}"
FIRMWARE="${PBEMU_FIRMWARE:-U633_6.8.2817}"
BS_DIR="${HERE}"
OUT_REL="build/bookshelf.app"
API_PORT="${PBEMU_API_PORT:-8765}"

DO_BUILD=1
for arg in "$@"; do
	case "${arg}" in
	--no-build) DO_BUILD=0 ;;
	-h | --help)
		sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "ERROR: unknown argument: ${arg}" >&2
		exit 1
		;;
	esac
done

cd "${REPO_ROOT}"

# Pick a Python interpreter: prefer the repo venv, fall back to python3.
PYTHON="${REPO_ROOT}/.venv/bin/python"
if [ ! -x "${PYTHON}" ]; then
	PYTHON=$(command -v python3 || true)
fi
if [ -z "${PYTHON}" ]; then
	echo "ERROR: no python interpreter found (tried ${REPO_ROOT}/.venv/bin/python and python3)" >&2
	exit 1
fi

if [ "${DO_BUILD}" -eq 1 ]; then
	echo "==> 1/5  building bookshelf.app"
	make -C "${BS_DIR}" all
else
	echo "==> 1/5  skipping build (--no-build)"
	if [ ! -f "${OUT_REL}" ]; then
		echo "ERROR: ${OUT_REL} missing; run without --no-build first" >&2
		exit 1
	fi
fi

echo "==> 2/5  (re)starting pbemu-api on 127.0.0.1:${API_PORT}"
# Kill any stale server first.
pkill -f "api.api.server" 2>/dev/null || true
sleep 0.5
# Run from the repo root so the mock provider's relative
# `U633_6.8.2817/.live/mnt/ext1/books` resolves correctly.
cd "${REPO_ROOT}"
PYTHONPATH="${REPO_ROOT}:${REPO_ROOT}/api" \
	"${PYTHON}" -m api.api.server \
	--host 0.0.0.0 --port "${API_PORT}" \
	>/tmp/pbemu-api.log 2>&1 &
echo $! >/tmp/pbemu-api.pid
sleep 1
if ! curl -s --max-time 2 "http://127.0.0.1:${API_PORT}/api/v1/healthz" -H "Authorization: Bearer pbemu-dev-token" >/dev/null; then
	echo "ERROR: api server failed to start; see /tmp/pbemu-api.log" >&2
	tail -20 /tmp/pbemu-api.log >&2 || true
	exit 1
fi
echo "  api server up: $(curl -s -H 'Authorization: Bearer pbemu-dev-token' "http://127.0.0.1:${API_PORT}/api/v1/healthz")"

echo "==> 3/5  stopping any running emulator"
if podman container exists "${CONTAINER}" 2>/dev/null; then
	./pbemu stop
else
	echo "  not running"
fi

echo "==> 4/5  staging bookshelf.app + cfg into ${FIRMWARE}/.live"
if [ ! -d "${REPO_ROOT}/${FIRMWARE}/.live" ]; then
	echo "ERROR: ${REPO_ROOT}/${FIRMWARE}/.live missing; run ./pbemu start once first" >&2
	exit 1
fi
mkdir -p "${REPO_ROOT}/${FIRMWARE}/.live/mnt/ext1/system/bin"
install -m 0755 "build/bookshelf.app" \
	"${REPO_ROOT}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.app"
echo "  staged $(wc -c <build/bookshelf.app) bytes to .live/mnt/ext1/system/bin/bookshelf.app"
# Point the app at the host API server. With --network=host the container
# shares the host netns, so 127.0.0.1 reaches the server started above.
cat >"${REPO_ROOT}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.cfg" <<CFGEOF
api_url=http://127.0.0.1:${API_PORT}
api_token=pbemu-dev-token
CFGEOF
echo "  wrote bookshelf.cfg (api_url=http://127.0.0.1:${API_PORT})"

echo "==> 5/5  starting emulator WITH viewer"
# --network=host so the guest reaches the API server at 127.0.0.1.
# No --no-build here: pbemu auto-builds any missing support artifacts
# (shim/informer/viewer) on first run, then reuses them.
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" \
	./pbemu start "${FIRMWARE}"

cat <<EOF

Done. The Wayland viewer window should now be on your desktop.

  - Tap the "S" button (top-right) to sync the book list from the API.
  - The "⋯" menu opens Settings (API host / key / reader).
  - API server log:  /tmp/pbemu-api.log  (pid $(cat /tmp/pbemu-api.pid))

Stop everything with:  ./pbemu stop
EOF
