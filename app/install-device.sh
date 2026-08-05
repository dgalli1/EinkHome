#!/bin/sh
#
# install-device.sh — push the pbemu bookshelf.app to a real PocketBook.
#
# Usage:
#   bookshelf/install-device.sh <device-ip> [api-url]
#
# Arguments:
#   <device-ip>  SSH target for the PocketBook (root@<ip>).  Passwordless
#                ssh must already be configured (ssh-copy-id once).
#   [api-url]    Full URL the on-device binary should talk to.  When
#                omitted, the script picks the host's primary LAN IPv4
#                and uses http://<lan-ip>:8765.  Override if the device
#                is on a different subnet than the host running the API.
#
# What it does:
#   1. Builds the ARM binary if `build/bookshelf.app` is missing.
#   2. Writes a fresh `build/bookshelf.cfg` with the resolved api_url
#      and api_token=pbemu-dev-token (matches api/config/server.json).
#   3. SCPs both files into `/mnt/ext1/applications/` on the device.
#   4. Renames the binary on-device to `books.app` so the firmware's
#      launcher dispatches to OUR binary instead of its built-in
#      bookshelf.app (which would shadow it).
#   5. chmod +x and restarts any already-running copy.
#
# This script intentionally does NOT auto-rebuild the binary — `run.sh`
# already handles building.  Pass `--build` to force a rebuild here too.
#
# Refuses to run if `ssh root@<ip>` is not passwordless.  Use
#     ssh-copy-id root@<device-ip>
# once before invoking this script.

set -eu

# PocketBook firmware ships an older dropbear that still defaults to
# ssh-rsa for host keys.  OpenSSH 8.8+ disabled the matching signature
# algorithm by default, so we have to re-enable it explicitly on every
# ssh/scp invocation below — otherwise the handshake fails with
# "no matching host key type found".
SSH_COMMON='-o BatchMode=yes -o HostKeyAlgorithms=+ssh-rsa'

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)

usage() {
	cat >&2 <<EOF
usage: $(basename "$0") <device-ip> [api-url]
       $(basename "$0") --build <device-ip> [api-url]

Pushes build/bookshelf.app + a fresh config to <device-ip>:/mnt/ext1/applications/.

EOF
	exit 64
}

DEVICE=""
API_URL=""
DO_BUILD=0

case "${1:-}" in
"" | -h | --help) usage ;;
--build)
	DO_BUILD=1
	shift
	DEVICE="${1:-}"
	API_URL="${2:-}"
	;;
*)
	DEVICE="${1:-}"
	API_URL="${2:-}"
	;;
esac

if [ -z "${DEVICE}" ]; then
	usage
fi

SRC_APP="${REPO_ROOT}/build/bookshelf.app"
SRC_CFG="${REPO_ROOT}/build/bookshelf.cfg"

if [ "${DO_BUILD}" = "1" ] || [ ! -f "${SRC_APP}" ]; then
	echo "==> building ${SRC_APP}"
	BS_SRCS=""
	for _f in bs_i18n.c bs_config.c bs_model.c bs_net.c bs_ui.c \
	          bs_input.c bs_launcher.c bs_downloads.c bs_store.c bs_main.c; do
		BS_SRCS="${BS_SRCS:+${BS_SRCS} }${HERE}/${_f}"
	done
	# shellcheck disable=SC2086
	"${REPO_ROOT}/sdk/build_armel.sh" ${BS_SRCS} --output "${SRC_APP}"
fi

if [ ! -f "${SRC_APP}" ]; then
	echo "ERROR: ${SRC_APP} not found; pass --build or run ./bookshelf/run.sh first" >&2
	exit 1
fi

# Resolve the api_url the device should hit.  When the user doesn't
# override, pick the host's primary LAN IPv4 (the first non-loopback
# IPv4 with a default route) and assume the API server is on :8765.
# If that fails, fall back to the build default (which is wrong for a
# real device but lets the user notice the problem).
if [ -z "${API_URL}" ]; then
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
	if [ -z "${LAN_IP}" ]; then
		echo "WARN: could not detect LAN ip; falling back to 192.168.1.42:8765" >&2
		LAN_IP="192.168.1.42"
	fi
	API_URL="http://${LAN_IP}:8765"
fi

# Sanity-check that we can ssh to the device non-interactively.  Refuse
# to continue if password auth would be required, so the user notices
# before the script scp's half the files and then hangs on ssh.
if ! ssh ${SSH_COMMON} -o ConnectTimeout=5 "root@${DEVICE}" true 2>/dev/null; then
	echo "ERROR: cannot ssh to root@${DEVICE} non-interactively." >&2
	echo "       run 'ssh-copy-id root@${DEVICE}' once before invoking this script." >&2
	exit 1
fi

# Write a fresh config with the resolved api_url.  api_token matches
# api/config/server.json so the binary authenticates against the same
# default server.
cat >"${SRC_CFG}" <<EOF
api_url=${API_URL}
api_token=pbemu-dev-token
EOF

echo "==> staging to ${DEVICE}:/mnt/ext1/applications/"
echo "    api_url = ${API_URL}"

# Push both files.  The destination name is `books.app`, NOT
# `bookshelf.app` — PocketBook's launcher dispatches by basename, so
# keeping our name as `bookshelf.app` would re-launch the firmware's
# original and silently exit.
scp ${SSH_COMMON} "${SRC_APP}" "root@${DEVICE}:/mnt/ext1/applications/books.app"
scp ${SSH_COMMON} "${SRC_CFG}" "root@${DEVICE}:/mnt/ext1/applications/bookshelf.cfg"

# Deploy the startup wrapper.  monitor.app resolves the home app by
# checking /mnt/ext1/system/bin/bookshelf.app BEFORE the firmware's
# /ebrmain/bin/bookshelf.app (verified in the launcher disassembly).
# The wrapper launches our books.app in the background, then execs the
# real firmware bookshelf so the stock UI keeps working.  This is what
# makes the custom bookshelf appear on boot instead of requiring a
# manual launch from the task list.
WRAPPER="${HERE}/bookshelf-wrapper.sh"
if [ -f "${WRAPPER}" ]; then
	echo "==> deploying startup wrapper to /mnt/ext1/system/bin/bookshelf.app"
	ssh ${SSH_COMMON} "root@${DEVICE}" 'mkdir -p /mnt/ext1/system/bin'
	scp ${SSH_COMMON} "${WRAPPER}" "root@${DEVICE}:/mnt/ext1/system/bin/bookshelf.app"
fi

# Make the binaries executable, kill any stale copy, restart cleanly.
# The `killall` is best-effort: it's OK if no process matches.
ssh ${SSH_COMMON} "root@${DEVICE}" sh -c '
	set -e
	chmod +x /mnt/ext1/applications/books.app
	if [ -f /mnt/ext1/system/bin/bookshelf.app ]; then chmod +x /mnt/ext1/system/bin/bookshelf.app; fi
	# Clear any stale log so the next run is easy to read.
	: >/mnt/ext1/applications/bookshelf.log
	killall books.app 2>/dev/null || true
	sleep 1
'

echo "==> installed.  verify with:"
echo "    ssh root@${DEVICE} 'tail -f /mnt/ext1/applications/bookshelf.log'"
echo "    reboot the device; the custom bookshelf launches on startup"
echo "    alongside the stock library (wrapper execs the firmware bookshelf)."
