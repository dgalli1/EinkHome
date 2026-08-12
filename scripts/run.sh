#!/bin/sh
#
# run.sh — end-to-end driver for EinkHome (the bookshelf replacement).
#
# Steps:
#   1. Build the guest ELF (make all; the pbemu submodule cross-compiles).
#   2. (Re)start the API server (api/api/server.py) on
#      127.0.0.1:8765 so the in-emulator app has a target to talk to.
#   3. Stop the running emulator (removes the container).
#   4. Stage the ELF + bookshelf.cfg into the HOST .live tree
#      (${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.app) — this seeds
#      the fresh container's /mnt on the next start.
#   5. Restart the emulator.
#   6. Stage the freshly built ELF INTO the running container
#      (/workspace/firmware/.live/ebrmain/bin/bookshelf.app and
#      /mnt/ext1/system/bin/bookshelf.app) and restart bookshelf.app so
#      monitor.app respawns OUR binary — required because "pbemu start"
#      rebuilds .live/ebrmain from the stock firmware, and the ebr bin
#      takes priority over /mnt/ext1/system/bin.
#   7. Take a screenshot, dump emulator state, and confirm the API server
#      answers the device's /sync/delta / /sync/state / /open-with calls.
#
# The pbemu submodule must be checked out (git submodule update --init)
# and its firmware staged (pbemu/pbemu install) before running.
#
# NOTE: run.sh and run-visible.sh share the dev-server pidfile + log
# (/tmp/pbemu-api.{pid,log}) — only one of them may run at a time.  On
# success this script intentionally leaves the emulator + API server
# running; on failure a trap stops both and drops the pidfile.

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
API_PORT="${PBEMU_API_PORT:-8765}"
API_PIDFILE="/tmp/pbemu-api.pid"
API_LOGFILE="/tmp/pbemu-api.log"
FIRMWARE="U633_6.8.2817"

# Failure trap: stop the emulator + API server and remove the pidfile,
# but ONLY on error — the normal exit path leaves everything running.
cleanup_on_error() {
	_status=$?
	if [ "${_status}" -eq 0 ]; then
		return 0
	fi
	echo "ERROR: run.sh failed (exit ${_status}); stopping emulator + api server" >&2
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

cd "${REPO_ROOT}"

echo "==> 1/7  building bookshelf.app"
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
	echo "==> 2/7  stopping container ${CONTAINER}"
	"${PBEMU_DIR}/pbemu" stop
	;;
*)
	echo "==> 2/7  skipping container stop (not running)"
	;;
esac

echo "==> 3/7  (re)starting pbemu-api on 127.0.0.1:${API_PORT}"
# Kill any stale server first.  run.sh and run-visible.sh share
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
	>"${API_LOGFILE}" 2>&1 &
echo $! >"${API_PIDFILE}"
# Poll the healthz endpoint until it answers — a cold python import +
# sqlite init can exceed a fixed sleep — bounded by a retry budget.
_API_READY=0
_i=0
while [ "${_i}" -lt 60 ]; do
	if curl -sf --max-time 2 "http://127.0.0.1:${API_PORT}/api/v1/healthz" -H "Authorization: Bearer pbemu-dev-token" >/dev/null 2>&1; then
		_API_READY=1
		break
	fi
	_i=$((_i + 1))
	sleep 0.5
done
if [ "${_API_READY}" -ne 1 ]; then
	echo "ERROR: api server did not start within 30s; see ${API_LOGFILE}" >&2
	cat "${API_LOGFILE}" >&2
	exit 1
fi
echo "  api server up: $(curl -s -H 'Authorization: Bearer pbemu-dev-token' http://127.0.0.1:${API_PORT}/api/v1/healthz)"

# Surface the LAN address(es) so a real PocketBook on the same Wi-Fi
# can reach the API.  We pick the first non-loopback IPv4 with a
# default route (lan_ip() in lib.sh; PBEMU_LAN_FALLBACK overrides the
# fallback).  The user can then set PBEMU_API_URL on the device.
LAN_IP=$(lan_ip)
if [ -n "${LAN_IP}" ]; then
	cat <<EOF

  LAN address: http://${LAN_IP}:${API_PORT}
  To launch this binary on a REAL PocketBook on the same network:
    ssh root@<device-ip> 'export PBEMU_API_URL="http://${LAN_IP}:${API_PORT}"; \\
        /mnt/ext1/applications/bookshelf.app'

EOF
fi

echo "==> 4/7  staging into ${FIRMWARE}/.live (host side)"
if [ ! -d "${PBEMU_DIR}/${FIRMWARE}/.live" ]; then
	PBEMU_NO_KEEPID=1 "${PBEMU_DIR}/pbemu" start "${FIRMWARE}" --no-viewer --no-audio --reset --no-build
	"${PBEMU_DIR}/pbemu" stop
fi
mkdir -p "${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin"
install -m 0755 "build/bookshelf.app" \
	"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.app"
echo "  staged $(wc -c <build/bookshelf.app) bytes to .live/mnt/ext1/system/bin/bookshelf.app"
# The container is not running at this point (step 2 stopped it), so the
# binary is staged host-side only; it seeds the fresh container's /mnt
# (bind-mounted from .live/mnt).  The container-side stage — the one that
# actually wins, since the ebr bin takes priority — happens after
# "pbemu start" in step 6.

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

echo "==> 5/7  starting container"
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" "${PBEMU_DIR}/pbemu" start "${FIRMWARE}" --no-viewer --no-audio --no-build
# "pbemu start" returns once the container is created, but it may still
# be initializing.  Poll until it reports running (bounded retry budget)
# so the podman cp/exec below fail loudly only on a real failure.
_CONTAINER_READY=0
_i=0
while [ "${_i}" -lt 60 ]; do
	if podman container inspect -f '{{.State.Running}}' "${CONTAINER}" 2>/dev/null | grep -qx true; then
		_CONTAINER_READY=1
		break
	fi
	_i=$((_i + 1))
	sleep 0.5
done
if [ "${_CONTAINER_READY}" -ne 1 ]; then
	echo "ERROR: container ${CONTAINER} not running within 30s of 'pbemu start'" >&2
	exit 1
fi

echo "==> 6/7  staging built binary into running container"
# "pbemu start" rebuilt .live/ebrmain from the stock firmware tree, so the
# host-side staging alone would lose: monitor.app looks under both
# /ebrmain/bin and /mnt/ext1/system/bin, and the ebr bin takes priority.
# Push the freshly built binary into the running container so monitor.app
# launches OURS on next respawn.  These must fail loudly if the container
# is down, so no `|| true` masking.
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
# Wait for monitor.app to respawn bookshelf.app and for it to resume
# talking to the API (a fresh /sync/delta request in the server log after
# our kill) — the condition the screenshot needs.  Poll instead of sleeping.
_APP_LOG_OFFSET=0
if [ -f "${API_LOGFILE}" ]; then
	_APP_LOG_OFFSET=$(wc -c <"${API_LOGFILE}")
fi
_APP_RESTARTED=0
_i=0
while [ "${_i}" -lt 60 ]; do
	if [ -f "${API_LOGFILE}" ] && \
		tail -c +$((_APP_LOG_OFFSET + 1)) "${API_LOGFILE}" 2>/dev/null | grep -q 'POST /api/v1/sync/delta'; then
		_APP_RESTARTED=1
		break
	fi
	_i=$((_i + 1))
	sleep 0.5
done
if [ "${_APP_RESTARTED}" -ne 1 ]; then
	# Screenshot is best-effort (--force below); warn rather than fail.
	echo "WARN: bookshelf.app did not re-report to the API within 30s after restart; screenshot may be stale" >&2
fi

echo "==> 7/7  screenshot + state + API confirmation"
"${PBEMU_DIR}/pbemu" screenshot /tmp/pbemu_bookshelf.png --force
echo "screenshot -> /tmp/pbemu_bookshelf.png ($(wc -c </tmp/pbemu_bookshelf.png) bytes)"
"${PBEMU_DIR}/pbemu" state || true

echo
echo "API server confirmation:"
# The in-emulator app talks to these three endpoints; poke each one
# ourselves and summarize what the server reports.
API_TOKEN="pbemu-dev-token"
API_BASE="http://127.0.0.1:${API_PORT}/api/v1"

# /sync/delta — the device polls this for new/removed books; the reply
# carries a cursor for the next poll plus a `more` flag.
DELTA=$(curl -s -X POST -H "Authorization: Bearer ${API_TOKEN}" \
	-H "Content-Type: application/json" \
	-d '{"cursor":0,"limit":20}' \
	"${API_BASE}/sync/delta")
DELTA_CURSOR=$(printf '%s' "${DELTA}" | sed -n 's/.*"nextCursor":\([0-9][0-9]*\).*/\1/p')
DELTA_MORE=$(printf '%s' "${DELTA}" | sed -n 's/.*"more":\(true\|false\).*/\1/p')
echo "  /sync/delta -> nextCursor=${DELTA_CURSOR:-?} more=${DELTA_MORE:-?}"

# /sync/state — the device reports what it has; the server acks ok:true.
STATE=$(curl -s -X POST -H "Authorization: Bearer ${API_TOKEN}" \
	-H "Content-Type: application/json" \
	-d '{"deviceId":"pbemu-check","known":[],"downloaded":[]}' \
	"${API_BASE}/sync/state")
STATE_OK=$(printf '%s' "${STATE}" | sed -n 's/.*"ok":\(true\|false\).*/\1/p')
echo "  /sync/state -> ok=${STATE_OK:-?}"

# /open-with — resolves a file extension to the app that opens it.
OPENWITH=$(curl -s -X POST -H "Authorization: Bearer ${API_TOKEN}" \
	-H "Content-Type: application/json" \
	-d '{"id":"pbemu-check","ext":"epub"}' \
	"${API_BASE}/open-with")
OPENWITH_APP=$(printf '%s' "${OPENWITH}" | sed -n 's/.*"app":"\([^"]*\)".*/\1/p')
echo "  /open-with -> app=${OPENWITH_APP:-?}"

echo
echo "Done. Screenshot: /tmp/pbemu_bookshelf.png  api log: ${API_LOGFILE}"
