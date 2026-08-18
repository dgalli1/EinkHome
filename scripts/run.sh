#!/bin/sh
#
# run.sh — end-to-end driver for EinkHome (the bookshelf replacement),
# headless: boots the app in the emulator, takes a screenshot, and
# confirms the API endpoints.  The visible counterpart is
# run-visible-pb.sh (emulator + viewer) / run-visible-sdl.sh (PC).
#
# Shared bootstrap (API server, staging, container push) lives in
# lib-run.sh.
#
# Steps:
#   1. Build the guest ELF (make all).
#   2. (Re)start the API server on 127.0.0.1:8765.
#   3. Stop the running emulator.
#   4. Stage the ELF + bookshelf.cfg into the HOST .live tree.
#   5. Restart the emulator (headless: --no-viewer --no-audio).
#   6. Push the freshly built ELF into the running container + respawn.
#   7. Screenshot, dump emulator state, confirm /sync /open-with.
#
# The pbemu submodule must be checked out (git submodule update --init)
# and its firmware staged (pbemu/pbemu install) before running.
#
# NOTE: run.sh, run-visible-pb.sh and run-visible-sdl.sh share the
# dev-server pidfile + log (/tmp/pbemu-api.{pid,log}) — only one of them
# may run at a time.  On success this script intentionally leaves the
# emulator + API server running; on failure a trap stops both and drops
# the pidfile.

set -eu

. "$(dirname "$0")/lib-run.sh"
eh_run_env

cleanup_on_error() { eh_run_cleanup; }
trap cleanup_on_error EXIT

cd "${REPO_ROOT}"

echo "==> 1/7  building bookshelf.app"
# The source list lives in the root Makefile; build_armel.sh in the
# pbemu submodule does the actual cross-compile.
make all

echo "==> 2/7  stopping any running emulator"
eh_run_stop_emulator

eh_run_api_start

eh_run_stage_live build/bookshelf.app

echo "==> 5/7  starting container (headless)"
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" "${PBEMU_DIR}/pbemu" start "${FIRMWARE}" --no-viewer --no-audio --no-build
# "pbemu start" returns once the container is created, but it may still
# be initializing.  Poll until it reports running so the podman cp/exec
# below fail loudly only on a real failure.
eh_run_wait_container

eh_run_stage_container build/bookshelf.app
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
	if [ -f "${API_LOGFILE}" ] &&
		grep -q "POST /api/v1/sync/delta" <(tail -c +$((_APP_LOG_OFFSET + 1)) "${API_LOGFILE}" 2>/dev/null || true); then
		_APP_RESTARTED=1
		break
	fi
	_i=$((_i + 1))
	sleep 0.5
done
if [ "${_APP_RESTARTED}" -ne 1 ]; then
	echo "WARN: bookshelf.app did not resume /sync/delta within 30s (app may be slow)" >&2
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

cat <<EOF

Stop everything with:  "${PBEMU_DIR}/pbemu" stop
EOF