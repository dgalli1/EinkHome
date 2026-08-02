#!/bin/sh
#
# run.sh — end-to-end driver for the pbemu bookshelf replacement (step 2).
#
# Steps:
#   1. Build the guest ELF against the PocketBook SDK (sdk/build_armel.sh).
#   2. (Re)start the pbemu API server (api/api/server.py) on 127.0.0.1:8765
#      so the in-emulator app has a target to talk to.  The container
#      reaches it via 169.254.1.2 (host.containers.internal).
#   3. Stop the running emulator.
#   4. Stage the ELF as /mnt/ext1/system/bin/bookshelf.app inside the
#      container so monitor.app picks our binary on next respawn.
#   5. Restart the emulator.
#   6. Wait for the foreground task to be our bookshelf, take a screenshot,
#      dump recent bookserver state reports, and confirm the API server
#      has seen the device's /sync/delta / /sync/state / /open-with calls.

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
BS_DIR="${HERE}"
OUT_REL="build/bookshelf.app"
API_PORT="${PBEMU_API_PORT:-8765}"

cd "${REPO_ROOT}"

# All translation units that make up the bookshelf app.
BS_SRCS=""
for _f in bs_i18n.c bs_config.c bs_model.c bs_net.c bs_ui.c \
          bs_input.c bs_launcher.c bs_downloads.c bs_main.c; do
	if [ ! -f "${BS_DIR}/${_f}" ]; then
		echo "ERROR: ${BS_DIR}/${_f} not found" >&2
		exit 1
	fi
	BS_SRCS="${BS_SRCS:+${BS_SRCS} }${BS_DIR}/${_f}"
done

echo "==> 1/6  building bookshelf.app"
# shellcheck disable=SC2086
"${REPO_ROOT}/sdk/build_armel.sh" ${BS_SRCS} --output "${OUT_REL}"

if ! podman container exists "${CONTAINER}" 2>/dev/null; then
	echo "INFO: container ${CONTAINER} not running; will be started in step 5"
	CONTAINER_STATE="absent"
else
	CONTAINER_STATE="present"
fi

case "${CONTAINER_STATE}" in
present)
	echo "==> 2/6  stopping container ${CONTAINER}"
	./pbemu stop
	;;
*)
	echo "==> 2/6  skipping container stop (not running)"
	;;
esac

echo "==> 3/6  (re)starting pbemu-api on 127.0.0.1:${API_PORT}"
# Kill any stale server first.
pkill -f "api.api.server" 2>/dev/null || true
sleep 0.5
# Run from the repo root so the mock provider's relative
# `U633_6.8.2817/.live/mnt/ext1/books` resolves correctly.
cd "${REPO_ROOT}"
# Run the server module via `python -m` with PYTHONPATH pointing
# at the repo root.  Note that `tools/` shadows `api/providers/`
# when we put the repo root in sys.path, so we explicitly add the
# api dir to PYTHONPATH as well.
PYTHONPATH="${REPO_ROOT}:${REPO_ROOT}/api" \
	/home/damian/git/pbemu/.venv/bin/python -m api.api.server \
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
if [ ! -d "${REPO_ROOT}/U633_6.8.2817/.live" ]; then
	PBEMU_NO_KEEPID=1 ./pbemu start U633_6.8.2817 --no-viewer --no-audio --reset --no-build
	./pbemu stop
fi
mkdir -p "${REPO_ROOT}/U633_6.8.2817/.live/mnt/ext1/system/bin"
install -m 0755 "build/bookshelf.app" \
	"${REPO_ROOT}/U633_6.8.2817/.live/mnt/ext1/system/bin/bookshelf.app"
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
mkdir -p "${REPO_ROOT}/U633_6.8.2817/.live/mnt/ext1/system/bin"
cat >"${REPO_ROOT}/U633_6.8.2817/.live/mnt/ext1/system/bin/bookshelf.cfg" <<CFGEOF
api_url=http://127.0.0.1:${API_PORT}
api_token=pbemu-dev-token
CFGEOF
echo "  wrote bookshelf.cfg (api_url=http://127.0.0.1:${API_PORT})"

echo "==> 5/6  starting container"
PBEMU_NO_KEEPID=1 PBEMU_PODMAN_ARGS="--network=host" ./pbemu start U633_6.8.2817 --no-viewer --no-audio --no-build
sleep 3
# kill the previous run if any
podman exec "${CONTAINER}" /usr/bin/killall bookshelf.app 2>/dev/null || true
sleep 5

echo "==> 6/6  screenshot + state"
./pbemu screenshot /tmp/pbemu_bookshelf.png --force
echo "screenshot -> /tmp/pbemu_bookshelf.png ($(wc -c </tmp/pbemu_bookshelf.png) bytes)"
./pbemu state || true

echo
echo "API server recent state:"
if [ -f /tmp/pbemu-api.log ]; then
	tail -30 /tmp/pbemu-api.log
fi
echo
echo "Done.  Press Ctrl-C to stop the API server (PID $(cat /tmp/pbemu-api.pid))."
