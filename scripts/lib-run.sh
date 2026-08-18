#!/bin/sh
#
# lib-run.sh — shared bootstrap for the EinkHome "run" scripts.
#
# Sourced by run.sh, run-visible-pb.sh and run-visible-sdl.sh.  Must stay
# POSIX-sh compatible (the callers run under `set -eu`).
#
# It owns the pieces every run path needs:
#   eh_run_env          — resolve the common paths/env (REPO_ROOT, pbemu,
#                         container, API port/pidfile, firmware, python)
#   eh_run_api_start    — (re)start the pbemu-api server + LAN surfacing
#   eh_run_cleanup      — failure trap: stop emulator + API, drop pidfile
# plus the PocketBook-emulator staging steps:
#   eh_run_stage_live   — stage binary + cfg into the HOST .live tree
#   eh_run_wait_container, eh_run_stage_container — push into the running
#   container + respawn bookshelf.app
#
# The SDL "visible" run (run-visible-sdl.sh) reuses eh_run_env,
# eh_run_api_start and eh_run_cleanup but skips the emulator stages.

# Common paths / env — call once at the top of each script that sources us.
eh_run_env() {
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
	API_PORT="${PBEMU_API_PORT:-8765}"
	API_PIDFILE="/tmp/pbemu-api.pid"
	API_LOGFILE="/tmp/pbemu-api.log"

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
}

# Failure trap: stop the emulator + API server and remove the pidfile,
# but ONLY on error — the normal exit path leaves everything running.
eh_run_cleanup() {
	_status=$?
	if [ "${_status}" -eq 0 ]; then
		return 0
	fi
	echo "ERROR: run failed (exit ${_status}); stopping emulator + api server" >&2
	if [ -d "${PBEMU_DIR}" ] && [ -x "${PBEMU_DIR}/pbemu" ]; then
		"${PBEMU_DIR}/pbemu" stop 2>/dev/null || true
	fi
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

# (Re)start the pbemu-api server on 127.0.0.1:${API_PORT} and wait until
# it answers /healthz.  Surface the LAN address for real devices.
#
# Optional env overrides:
#   PBEMU_API_CONFIG — absolute path to a server config (e.g. the 100k
#     mock in api/config/server-100k.json); passed as --config and the
#     server runs from PBEMU_API_CWD (default ${REPO_ROOT}) so the
#     config's repo-relative paths resolve.
#   PBEMU_API_CWD    — working dir for the server when a config is set.
eh_run_api_start() {
	echo "==> (re)starting pbemu-api on 127.0.0.1:${API_PORT}"
	# Kill any stale server first.  The run scripts share ${API_PIDFILE},
	# so this also stops a server left by another one.  The pid is
	# sanity-checked against the process cmdline so a recycled pid can
	# never kill an innocent process.
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
	#
	# With PBEMU_API_CONFIG set (install-device.sh --mock/--real) we pick
	# a non-default server config and run from PBEMU_API_CWD (default
	# ${REPO_ROOT}) so that config's repo-relative corpus/books paths
	# resolve — the firmware-relative default above only holds for the
	# pbemu submodule cwd.
	_SERVER_CWD="${PBEMU_DIR}"
	if [ -n "${PBEMU_API_CONFIG:-}" ]; then
		_SERVER_CWD="${PBEMU_API_CWD:-${REPO_ROOT}}"
	fi
	cd "${_SERVER_CWD}"
	PYTHONPATH="${REPO_ROOT}:${REPO_ROOT}/api" \
		"${PYTHON}" -m api.api.server \
		--host 0.0.0.0 --port "${API_PORT}" \
		${PBEMU_API_CONFIG:+--config "${PBEMU_API_CONFIG}"} \
		>"${API_LOGFILE}" 2>&1 &
	echo $! >"${API_PIDFILE}"
	# Poll the healthz endpoint until it answers — a cold python import +
	# sqlite init can exceed a fixed sleep — bounded by a retry budget.
	_API_READY=0
	_i=0
	while [ "${_i}" -lt 60 ]; do
		if curl -sf --max-time 2 "http://127.0.0.1:${API_PORT}/api/v1/healthz" \
			-H "Authorization: Bearer pbemu-dev-token" >/dev/null 2>&1; then
			_API_READY=1
			break
		fi
		_i=$((_i + 1))
		sleep 0.5
	done
	if [ "${_API_READY}" -ne 1 ]; then
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
}

# Stop any running emulator container.
eh_run_stop_emulator() {
	echo "==> stopping any running emulator"
	if podman container exists "${CONTAINER}" 2>/dev/null; then
		"${PBEMU_DIR}/pbemu" stop
	else
		echo "  not running"
	fi
}

# Stage the freshly built ${OUT_REL} + bookshelf.cfg into the HOST .live
# tree (${FIRMWARE}/.live/...).  Seeds the fresh container's /mnt.
# ${OUT_REL} is the build-relative output (e.g. build/bookshelf.app).
eh_run_stage_live() {
	_out="${1:-build/bookshelf.app}"
	cd "${REPO_ROOT}"
	echo "==> staging ${_out} + cfg into ${FIRMWARE}/.live"
	if [ ! -d "${PBEMU_DIR}/${FIRMWARE}/.live" ]; then
		echo "ERROR: ${PBEMU_DIR}/${FIRMWARE}/.live missing; run ${PBEMU_DIR}/pbemu start once first" >&2
		exit 1
	fi
	mkdir -p "${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin"
	install -m 0755 "${_out}" \
		"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/bookshelf.app"
	# The app's RUNPATH is its own directory: stage the SDK's libinkview /
	# libhwconfig next to it so older firmwares (whose own libs land taps at
	# wrong coordinates) run the same libs as the harness.  Only when the
	# firmware can satisfy the SDK lib's legacy deps (libssl 1.0) — newer
	# firmwares keep their own libs.  A real device has no such files here
	# and uses its own firmware libs.
	if [ -e "${PBEMU_DIR}/${FIRMWARE}/.live/ebrmain/lib/libssl.so.1.0.0" ]; then
		install -m 0644 "sdk/pocketbook-sdk-b288/lib/libinkview.so" \
			"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/libinkview.so"
		install -m 0644 "sdk/pocketbook-sdk-b288/lib/libhwconfig.so" \
			"${PBEMU_DIR}/${FIRMWARE}/.live/mnt/ext1/system/bin/libhwconfig.so"
	fi
	echo "  staged $(wc -c <"${_out}") bytes to .live/mnt/ext1/system/bin/bookshelf.app"
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
}

# Poll until the emulator container reports running (bounded retry budget),
# so the podman cp/exec in eh_run_stage_container fail loudly only on a
# real failure.  `pbemu start` returns as soon as the container is created,
# but it may still be initializing.
eh_run_wait_container() {
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
}

# Push the freshly built ${_out} into the RUNNING container and respawn
# bookshelf.app.  "pbemu start" rebuilt .live/ebrmain from the stock
# firmware tree, so the host-side staging alone would lose: monitor.app
# looks under both /ebrmain/bin and /mnt/ext1/system/bin, and the ebr bin
# takes priority.  These must fail loudly if the container is down, so no
# `|| true` masking.
eh_run_stage_container() {
	_out="${1:-build/bookshelf.app}"
	echo "==> staging ${_out} into running container"
	podman cp "${_out}" "${CONTAINER}:/tmp/bookshelf.app.new"
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
}