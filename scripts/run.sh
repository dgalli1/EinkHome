#!/bin/sh
#
# run.sh — end-to-end driver for EinkHome (the bookshelf replacement).
#
# Steps:
#   1. Build the guest ELF via the pbemu submodule's sdk/build_armel.sh.
#   2. (Re)start the API server (api/api/server.py) on
#      127.0.0.1:8765 so the in-emulator app has a target to talk to.
#   3. Stop the running emulator.
#   4. Stage the ELF as /mnt/ext1/system/bin/bookshelf.app inside the
#      container so monitor.app picks our binary on next respawn.
#   5. Restart the emulator.
#   6. Wait for the foreground task to be our app, take a screenshot,
#      dump recent bookserver state reports, and confirm the API server
#      has seen the device's /sync/delta / /sync/state / /open-with calls.
#
# The pbemu submodule must be checked out (git submodule update --init)
# and its firmware staged (pbemu/pbemu install) before running.

set -eu

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)
PBEMU_DIR="${REPO_ROOT}/pbemu"
CONTAINER="${PBEMU_CONTAINER:-pb-pocketbook-ui}"
API_PORT="${PBEMU_API_PORT:-8765}"
FIRMWARE="U633_6.8.2817"

cd "${REPO_ROOT}"

echo "==> 1/6  building bookshelf.app"
# The source list lives in the root Makefile; build_armel.sh in the
# pbemu submodule does the actual cross-compile.
make all

if ! podman container exists "${CONTAINER}" 2>/dev/null; then
	echo "INFO: container ${CONTAINER} not running; will be started in step 5"
	CONTAINER_STATE="absent"
else
	CONTAINER_STATE="present"
fi

case "${CONTAINER_STATE}" in
present)
	echo "==> 2/6  stopping container ${CONTAINER}"
	"${PBEMU_DIR}/pbemu" stop
	;;
*)
	echo "==> 2/6  skipping container stop (not running)"
	;;
esac

echo "==> 3/6  (re)starting pbemu-api on 127.0.0.1:${API_PORT}"
# Kill any stale server first.
pkill -f "api.api.server" 2>/dev/null || true
sleep 0.5
# Run from the pbemu submodule so the config's firmware-relative
# paths (books_dir: U633_6.8.2817/.live/...) resolve correctly; the
# server code itself lives in this repo (api/ on PYTHONPATH).
cd "${PBEMU_DIR}"
PYTHON="${PBEMU_DIR}/.venv/bin/python"
if [ ! -x "${PYTHON}" ]; then
	PYTHON=$(command -v python3 || true)
fi
if [ -z "${PYTHON}" ]; then
	echo "ERROR: no python interpreter found (tried ${PBEMU_DIR}/.venv/bin/python and python3)" >&2
	exit 1
fi
# Run the server module via `python -m` with PYTHONPATH pointing
# at the repo root, plus the api dir explicitly.
PYTHONPATH="${REPO_ROOT}:${REPO_ROOT}/api" \
	"${PYTHON}" -m api.api.server \
	--host 0.0.0.0 --port "${API_PORT}" \
	>/tmp/pbemu-api.log 2>&1 &
echo $! >/tmp/pbemu-api.pid
sleep 1
# Verify the server is up.
if ! curl -s --max-time 2 "http://127.0.0.1:${API_PORT}/api/v1/healthz" -H "Authorization: Bearer pbemu-dev-token" >/dev/null; then
	echo "ERROR: api server did not start; see /tmp/pbemu-api.log" >&2
	cat /tmp/pbemu-api.log >&2
	exit 1
fi
echo "  api server up: $(curl -s -H 'Authorization: Bearer pbemu-dev-token' http://127.0.0.1:${API_PORT}/api/v1/healthz)"

# Surface the LAN address(es) so a real PocketBook on the same Wi-Fi
# can reach the API.  We pick the first non-loopback IPv4 with a
# default route.  The user can then set PBEMU_API_URL on the device.
LAN_IP=$(
	ip -4 -o addr show scope global 2>/dev/null |
		awk '{print $4}' |
		cut -d/ -f1 |
		head -n1
)
if [ -z "${LAN_IP}" ]; then
	LAN_IP=$(
		hostname -I 2>/dev/null |
			tr ' ' '\n' |
			grep -v '^127\.' |
			head -n1
	)
fi
if [ -n "${LAN_IP}" ]; then
	cat <<EOF

  LAN address: http://${LAN_IP}:${API_PORT}
  To launch this binary on a REAL PocketBook on the same network:
    ssh root@<device-ip> 'export PBEMU_API_URL="http://${LAN_IP}:${API_PORT}"; \\
        /mnt/ext1/applications/bookshelf.app'

EOF
fi

echo "==> 4/6  staging into ${CONTAINER:-new container}"
if [ ! -d "${PBEMU_DIR}/${FIRMWARE}/.live" ]; then
	PBEMU_NO_KEEPID=1 "${PBEMU_DIR}/pbemu" start "${FIRMWARE}" --no-viewer --no-audio --reset --no-build
	"${PBEMU_DIR}/pbemu" stop
fi
mkdir -p "${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin"
install -m 0755 "build/bookshelf.app" \
	"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.app"
echo "  staged $(wc -c <build/bookshelf.app) bytes to .live/mnt/ext1/system/bin/bookshelf.app"
# Also push the binary into the running container so monitor.app launches
# it on next respawn (it looks under both /ebrmain/bin and
# /mnt/ext1/system/bin; the ebr bin takes priority).
podman cp build/bookshelf.app "${CONTAINER}:/tmp/bookshelf.app.new" 2>/dev/null || true
podman exec "${CONTAINER}" /usr/bin/rm -f \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app 2>/dev/null || true
podman exec "${CONTAINER}" /usr/bin/mv /tmp/bookshelf.app.new \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app 2>/dev/null || true
podman exec "${CONTAINER}" /usr/bin/chmod +x \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app 2>/dev/null || true
podman exec "${CONTAINER}" /usr/bin/cp \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app \
	/mnt/ext1/system/bin/bookshelf.app 2>/dev/null || true
podman exec "${CONTAINER}" /usr/bin/chmod +x \
	/mnt/ext1/system/bin/bookshelf.app 2>/dev/null || true

# Write a bookshelf.cfg that points at 127.0.0.1 (works with --network=host
# and with the old shared-netns mode; 169.254.1.2 is unreachable under pasta).
mkdir -p "${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin"
cat >"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.cfg" <<CFGEOF
api_url=http://127.0.0.1:${API_PORT}
api_token=pbemu-dev-token
CFGEOF
# Owner-write only: `cat >` keeps a pre-existing file's mode, and a
# world-writable cfg would make the guest think the (unwritable) app
# dir is its settings home, breaking the store fallback to /tmp.
chmod 0644 "${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.cfg"
echo "  wrote bookshelf.cfg (api_url=http://127.0.0.1:${API_PORT})"

# The guest may have a settings override in /tmp/bookshelf.cfg (its app
# dir is not writable in the emulator, so settings saves land there).
# Refresh the api_url in it too, preserving any other settings — a stale
# override (e.g. from a test run pointing at a dead port) would otherwise
# win over the freshly written cfg above on the next launch.
TMP_CFG="${PBEMU_DIR}/${FIRMWARE}/.live/tmp/bookshelf.cfg"
if [ -f "${TMP_CFG}" ]; then
	sed -i "s|^api_url=.*|api_url=http://127.0.0.1:${API_PORT}|" "${TMP_CFG}"
	# The guest (container UID) rewrites this file on settings changes.
	chmod 666 "${TMP_CFG}"
	echo "  refreshed ${TMP_CFG} (api_url=http://127.0.0.1:${API_PORT})"
fi

echo "==> 5/6  starting container"
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" "${PBEMU_DIR}/pbemu" start "${FIRMWARE}" --no-viewer --no-audio --no-build
sleep 3
# kill the previous run if any
podman exec "${CONTAINER}" /usr/bin/killall bookshelf.app 2>/dev/null || true
sleep 5

echo "==> 6/6  screenshot + state"
"${PBEMU_DIR}/pbemu" screenshot /tmp/pbemu_bookshelf.png --force
echo "screenshot -> /tmp/pbemu_bookshelf.png ($(wc -c </tmp/pbemu_bookshelf.png) bytes)"
"${PBEMU_DIR}/pbemu" state || true

echo
echo "API server recent state:"
