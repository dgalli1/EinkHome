#!/bin/sh
#
# run-visible.sh — build, stage, and launch EinkHome (the bookshelf
# replacement) WITH the Wayland viewer window, so you can see and
# interact with it.
#
# This is the interactive counterpart to run.sh (which runs headless with
# --no-viewer for automated screenshots). Steps:
#
#   1. Build the guest ELF via the pbemu submodule (skip with --no-build).
#   2. (Re)start the API server on 127.0.0.1:8765 so the in-emulator
#      app has a target to talk to.
#   3. Stop any running emulator container.
#   4. Stage the ELF + bookshelf.cfg into .live so monitor.app launches it.
#   5. Start the emulator WITH the viewer + audio relay. The Wayland window
#      appears on your desktop; tap the "S" button to sync the book list.
#   6. Stage the freshly built ELF INTO the running container
#      (/workspace/firmware/.live/ebrmain/bin/bookshelf.app and
#      /mnt/ext1/system/bin/bookshelf.app) and restart bookshelf.app so
#      monitor.app respawns OUR binary — "pbemu start" rebuilds
#      .live/ebrmain from the stock firmware, and the ebr bin takes
#      priority over /mnt/ext1/system/bin.
#
# Usage:
#   scripts/run-visible.sh               # build + launch
#   scripts/run-visible.sh --no-build    # skip the ELF rebuild (faster)
#
# Stop everything afterwards with: pbemu/pbemu stop
#
# NOTE: run-visible.sh and run.sh share the dev-server pidfile + log
# (/tmp/pbemu-api.{pid,log}) — only one of them may run at a time.  On
# success this script intentionally leaves the emulator + API server
# running; on failure a trap stops both and drops the pidfile.
#
set -eu

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
# Shared helpers (lan_ip) — must stay POSIX sh.
. "${HERE}/lib.sh"
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)
PBEMU_DIR="${REPO_ROOT}/pbemu"
CONTAINER="${PBEMU_CONTAINER:-pb-pocketbook-ui}"
FIRMWARE="${PBEMU_FIRMWARE:-U633_6.8.2817}"
OUT_REL="build/bookshelf.app"
API_PORT="${PBEMU_API_PORT:-8765}"
API_PIDFILE="/tmp/pbemu-api.pid"
API_LOGFILE="/tmp/pbemu-api.log"

# Failure trap: stop the emulator + API server and remove the pidfile,
# but ONLY on error — the normal exit path leaves everything running.
cleanup_on_error() {
	_status=$?
	if [ "${_status}" -eq 0 ]; then
		return 0
	fi
	echo "ERROR: run-visible.sh failed (exit ${_status}); stopping emulator + api server" >&2
	"${PBEMU_DIR}/pbemu" stop 2>/dev/null || true
	if [ -f "${API_PIDFILE}" ]; then
		_apid=$(cat "${API_PIDFILE}" 2>/dev/null || true)
		if [ -n "${_apid}" ] && kill -0 "${_apid}" 2>/dev/null &&
			ps -p "${_apid}" -o args= 2>/dev/null | grep -q "api.api.server"; then
			kill "${_apid}" 2>/dev/null || true
		fi
		rm -f "${API_PIDFILE}"
	fi
	return 0
}
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

# Pick a Python interpreter: prefer the pbemu submodule venv, fall back
# to python3.
PYTHON="${PBEMU_DIR}/.venv/bin/python"
if [ ! -x "${PYTHON}" ]; then
	PYTHON=$(command -v python3 || true)
fi
if [ -z "${PYTHON}" ]; then
	echo "ERROR: no python interpreter found (tried ${PBEMU_DIR}/.venv/bin/python and python3)" >&2
	exit 1
fi

if [ "${DO_BUILD}" -eq 1 ]; then
	echo "==> 1/6  building bookshelf.app"
	make all
else
	echo "==> 1/6  skipping build (--no-build)"
	if [ ! -f "${OUT_REL}" ]; then
		echo "ERROR: ${OUT_REL} missing; run without --no-build first" >&2
		exit 1
	fi
fi

echo "==> 2/6  (re)starting pbemu-api on 127.0.0.1:${API_PORT}"
# Kill any stale server first.  run-visible.sh and run.sh share
# ${API_PIDFILE}, so this also stops a server left by the other script.
# The pid is sanity-checked against the process cmdline so a recycled
# pid can never kill an innocent process.
if [ -f "${API_PIDFILE}" ]; then
	OLD_PID=$(cat "${API_PIDFILE}" 2>/dev/null || true)
	if [ -n "${OLD_PID}" ] && kill -0 "${OLD_PID}" 2>/dev/null; then
		if ps -p "${OLD_PID}" -o args= 2>/dev/null | grep -q "api.api.server"; then
			echo "  stopping stale api server (pid ${OLD_PID})"
			# `|| true`: the process may have exited between the
			# checks and the kill; the wait below covers that.
			kill "${OLD_PID}" 2>/dev/null || true
			# Bounded wait (~5s) for it to actually exit; a lingering
			# server would otherwise collide on the port.
			_wait=0
			while kill -0 "${OLD_PID}" 2>/dev/null && [ "${_wait}" -lt 50 ]; do
				_wait=$((_wait + 1))
				sleep 0.1
			done
		else
			echo "WARN: ${API_PIDFILE} holds pid ${OLD_PID}, which is not the api server; ignoring" >&2
		fi
	fi
	rm -f "${API_PIDFILE}"
fi
# Run from the pbemu submodule so the config's firmware-relative
# paths resolve correctly; the server code lives in this repo (api/).
cd "${PBEMU_DIR}"
PYTHONPATH="${REPO_ROOT}:${REPO_ROOT}/api" \
	"${PYTHON}" -m api.api.server \
	--host 0.0.0.0 --port "${API_PORT}" \
	>"${API_LOGFILE}" 2>&1 &
echo $! >"${API_PIDFILE}"
sleep 1
if ! curl -sf --max-time 2 "http://127.0.0.1:${API_PORT}/api/v1/healthz" -H "Authorization: Bearer pbemu-dev-token" >/dev/null; then
	echo "ERROR: api server failed to start; see ${API_LOGFILE}" >&2
	tail -20 "${API_LOGFILE}" >&2 || true
	exit 1
fi
echo "  api server up: $(curl -s -H 'Authorization: Bearer pbemu-dev-token' "http://127.0.0.1:${API_PORT}/api/v1/healthz")"

# Surface the LAN address(es) so a real PocketBook on the same Wi-Fi
# can reach the API (lan_ip() in lib.sh; PBEMU_LAN_FALLBACK overrides
# the fallback).  The user can then set PBEMU_API_URL on the device.
LAN_IP=$(lan_ip)
if [ -n "${LAN_IP}" ]; then
	cat <<EOF

  LAN address: http://${LAN_IP}:${API_PORT}
  To launch this binary on a REAL PocketBook on the same network:
    ssh root@<device-ip> 'export PBEMU_API_URL="http://${LAN_IP}:${API_PORT}"; \\
        /mnt/ext1/applications/bookshelf.app'

EOF
fi

echo "==> 3/6  stopping any running emulator"
if podman container exists "${CONTAINER}" 2>/dev/null; then
	"${PBEMU_DIR}/pbemu" stop
else
	echo "  not running"
fi

cd "${REPO_ROOT}"
echo "==> 4/6  staging bookshelf.app + cfg into ${FIRMWARE}/.live"
if [ ! -d "${PBEMU_DIR}/${FIRMWARE}/.live" ]; then
	echo "ERROR: ${PBEMU_DIR}/${FIRMWARE}/.live missing; run "${PBEMU_DIR}/pbemu" start once first" >&2
	exit 1
fi
mkdir -p "${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin"
install -m 0755 "build/bookshelf.app" \
	"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.app"
echo "  staged $(wc -c <build/bookshelf.app) bytes to .live/mnt/ext1/system/bin/bookshelf.app"
# Point the app at the host API server. With --network=host the container
# shares the host netns, so 127.0.0.1 reaches the server started above.
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

echo "==> 5/6  starting emulator WITH viewer"
# --network=host so the guest reaches the API server at 127.0.0.1.
# No --no-build here: pbemu auto-builds any missing support artifacts
# (shim/informer/viewer) on first run, then reuses them.
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" \
	"${PBEMU_DIR}/pbemu" start "${FIRMWARE}"
sleep 3

echo "==> 6/6  staging built binary into running container"
# "pbemu start" rebuilt .live/ebrmain from the stock firmware tree, so the
# .live staging above would lose: monitor.app looks under both /ebrmain/bin
# and /mnt/ext1/system/bin, and the ebr bin takes priority. Push the
# freshly built binary into the running container so monitor.app launches
# OURS on next respawn. These must fail loudly if the container is down,
# so no `|| true` masking.
podman cp build/bookshelf.app "${CONTAINER}:/tmp/bookshelf.app.new"
podman exec "${CONTAINER}" /usr/bin/rm -f \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app
podman exec "${CONTAINER}" /usr/bin/mv /tmp/bookshelf.app.new \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app
podman exec "${CONTAINER}" /usr/bin/chmod +x \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app
podman exec "${CONTAINER}" /usr/bin/cp \
	/workspace/firmware/.live/ebrmain/bin/bookshelf.app \
	/mnt/ext1/system/bin/bookshelf.app
podman exec "${CONTAINER}" /usr/bin/chmod +x \
	/mnt/ext1/system/bin/bookshelf.app
# Restart bookshelf so monitor.app respawns the freshly staged binary.
# killall failing (app not running) is fine — monitor.app relaunches it
# either way, so this stays optional.
podman exec "${CONTAINER}" /usr/bin/killall bookshelf.app 2>/dev/null || true

cat <<EOF

Done. The Wayland viewer window should now be on your desktop.

  - Tap the "S" button (top-right) to sync the book list from the API.
  - The "⋯" menu opens Settings (API host / key / reader).
  - API server log:  ${API_LOGFILE}  (pid $(cat "${API_PIDFILE}"))

Stop everything with:  "${PBEMU_DIR}/pbemu" stop
EOF
