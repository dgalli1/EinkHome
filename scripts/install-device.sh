#!/bin/sh
#
# install-device.sh — push the pbemu bookshelf.app to a real PocketBook.
#
# Usage:
#   scripts/install-device.sh [flags] <device-ip> [api-url]
#
# Arguments:
#   <device-ip>  SSH target for the PocketBook (root@<ip>).  Passwordless
#                ssh must already be configured (ssh-copy-id once).
#   [api-url]    Full URL the on-device binary should talk to.  When
#                omitted, the script picks the host's primary LAN IPv4
#                (lan_ip() in lib.sh) and uses http://<lan-ip>:${PBEMU_API_PORT:-8765}.
#                Override the port with PBEMU_API_PORT, or pass an
#                explicit api-url if the device is on a different subnet
#                than the host running the API.
#
# Flags:
#   --mock / --real  Pick which API backend the device ends up on.  Both
#                bind the same LAN host:port, so the api_url below is
#                identical whatever you choose — the server config is
#                what decides.  --mock (re)starts the host pbemu-api with
#                api/config/server-100k.json (mock provider, 100k OL
#                corpus); --real with api/config/server.json (kavita, the
#                real data source).  Mutually exclusive.
#   --reset-db  Completely reset the on-device library store before
#                installing: deletes bookshelf_lib.db, the pre-sqlite
#                bookshelf_lib.json and the covers/ cache under
#                /mnt/ext1/system/bin/, so the app re-syncs from scratch
#                on next launch.  Leaves the firmware reader's
#                explorer-3.db progress untouched.
#
# What it does:
#   1. Builds the ARM binary if `build/bookshelf.app` is missing
#      (--build forces it).
#   2. Optionally (--mock/--real) restarts the host pbemu-api server with
#      the matching config.
#   3. Optionally (--reset-db) wipes the on-device library store.
#   4. Writes a fresh `build/bookshelf.cfg` with the resolved api_url
#      and api_token=pbemu-dev-token (matches api/config/server.json).
#   5. SCPs both into `/mnt/ext1/system/bin/` on the device, named
#      `bookshelf.app` / `bookshelf.cfg`.
#   6. The binary IS the home task: monitor.app checks
#      /mnt/ext1/system/bin/bookshelf.app before the firmware's
#      /ebrmain/bin/bookshelf.app, so Home opens OUR app.  Installed
#      directly (no wrapper script) so the reader's book-open handshake
#      keeps working.
#   7. chmod +x and restarts any already-running copy.
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
# Shared helpers (lan_ip) — must stay POSIX sh.
. "${HERE}/lib.sh"
# lib-run.sh provides bs_run_env (common paths + a python interpreter)
# and bs_run_api_start, the (re)start logic for the pbemu-api server —
# reused for --mock/--real below.  All of lib-run.sh is POSIX-sh.
. "${HERE}/lib-run.sh"
bs_run_env

usage() {
	cat >&2 <<EOF
usage: $(basename "$0") [flags] <device-ip> [api-url]
       $(basename "$0") --abi armhf <device-ip> [api-url]
       $(basename "$0") --build [--abi armhf] <device-ip> [api-url]
       $(basename "$0") --mock[|--real] [--reset-db] <device-ip> [api-url]

Flags (combinable, any order before the positional args):
  --build      rebuild the binary first (default: build only when missing).
  --abi armhf  use the build/bookshelf.armhf.app hard-float build.
  --mock       use the 100k mock server: (re)start the host pbemu-api
               with api/config/server-100k.json (mock provider, 100k OL
               corpus) before installing.
  --real       use the real data endpoint: (re)start the host pbemu-api
               with api/config/server.json (kavita provider).
  --reset-db   completely reset the on-device library store before
               installing: removes bookshelf_lib.db, the pre-sqlite
               bookshelf_lib.json and the covers/ cache under
               /mnt/ext1/system/bin/.  Does NOT touch explorer-3.db
               (the firmware reader's progress) or the binary/config
               (those are reinstalled anyway).

Pushes build/bookshelf.app (or build/bookshelf.armhf.app with --abi
armhf, for the hard-float InkPad One) + a fresh config to
<device-ip>:/mnt/ext1/system/bin/.

EOF
	exit 64
}

DEVICE=""
API_URL=""
DO_BUILD=0
ABI="armel"
# "" (leave the host server alone) | mock | real
DATA_MODE=""
DO_RESET_DB=0

while [ $# -gt 0 ]; do
	case "${1}" in
	-h | --help) usage ;;
	--build)
		DO_BUILD=1
		;;
	--abi)
		ABI="${2:-}"
		case "${ABI}" in
		armel | armhf) ;;
		*)
			echo "ERROR: --abi must be armel or armhf (got: ${ABI})" >&2
			exit 64
			;;
		esac
		shift
		;;
	--mock | --real)
		if [ -n "${DATA_MODE}" ] && [ "${DATA_MODE}" != "${1#--}" ]; then
			echo "ERROR: --mock and --real are mutually exclusive" >&2
			exit 64
		fi
		DATA_MODE="${1#--}"
		;;
	--reset-db)
		DO_RESET_DB=1
		;;
	-*)
		echo "ERROR: unknown option: ${1}" >&2
		usage
		;;
	*)
		if [ -z "${DEVICE}" ]; then
			DEVICE="${1}"
		elif [ -z "${API_URL}" ]; then
			API_URL="${1}"
		else
			echo "ERROR: too many arguments: ${1}" >&2
			usage
		fi
		;;
	esac
	shift
done

if [ -z "${DEVICE}" ]; then
	usage
fi

# armel = the soft-float build every firmware but the InkPad One uses;
# armhf = the hard-float build linked against U1030_6.11.1437.  The
# destination name is bookshelf.app in both cases — monitor.app resolves
# the home task by that exact name.
SRC_APP="${REPO_ROOT}/build/bookshelf.app"
if [ "${ABI}" = "armhf" ]; then
	SRC_APP="${REPO_ROOT}/build/bookshelf.armhf.app"
fi
SRC_CFG="${REPO_ROOT}/build/bookshelf.cfg"

if [ "${DO_BUILD}" = "1" ] || [ ! -f "${SRC_APP}" ]; then
	echo "==> building ${SRC_APP}"
	if [ "${ABI}" = "armhf" ]; then
		make -C "${REPO_ROOT}" armhf
	else
		make -C "${REPO_ROOT}" all
	fi
fi

if [ ! -f "${SRC_APP}" ]; then
	echo "ERROR: ${SRC_APP} not found; pass --build or run ./scripts/run.sh first" >&2
	exit 1
fi

# --mock / --real: (re)start the host pbemu-api with the matching config.
# Both backends bind the same LAN host:port, so the api_url resolved
# below is identical either way — the config is what selects the source.
# bs_run_api_start passes --config and runs from REPO_ROOT (via
# PBEMU_API_CONFIG) so the 100k config's repo-relative corpus/books paths
# resolve.
if [ -n "${DATA_MODE}" ]; then
	case "${DATA_MODE}" in
	mock) _API_CFG="${REPO_ROOT}/api/config/server-100k.json" ;;
	real) _API_CFG="${REPO_ROOT}/api/config/server.json" ;;
	esac
	echo "==> (re)starting pbemu-api with ${DATA_MODE} config: ${_API_CFG}"
	PBEMU_API_CONFIG="${_API_CFG}" bs_run_api_start
fi

# Resolve the api_url the device should hit.  When the user doesn't
# override, pick the host's primary LAN IPv4 (lan_ip() in lib.sh: the
# first non-loopback IPv4 with a default route, with a magic fallback
# overridable via PBEMU_LAN_FALLBACK) and assume the API server is on
# ${API_PORT}.  If that fails, the fallback IP is wrong for a real
# device but lets the user notice the problem.
if [ -z "${API_URL}" ]; then
	LAN_IP=$(lan_ip)
	API_URL="http://${LAN_IP}:${API_PORT}"
fi

# Sanity-check that we can ssh to the device non-interactively.  Refuse
# to continue if password auth would be required, so the user notices
# before the script scp's half the files and then hangs on ssh.  The
# ssh stderr is shown — it distinguishes "No route to host" (device
# asleep / off Wi-Fi) from a host-key or auth problem.
if ! ssh ${SSH_COMMON} -o ConnectTimeout=5 "root@${DEVICE}" true; then
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

echo "==> staging to ${DEVICE}:/mnt/ext1/system/bin/"
echo "    api_url = ${API_URL}"

# A previously installed copy is often owned by a different user (the
# pbjb installer writes as root) or read-only, so scp cannot overwrite
# it in place — it fails with `dest open ...: Failure`.  Remove the
# stale files first: rm only needs write permission on the directory,
# which the ssh user has.
ssh ${SSH_COMMON} "root@${DEVICE}" rm -f \
	/mnt/ext1/system/bin/bookshelf.app /mnt/ext1/system/bin/bookshelf.cfg

# --reset-db: wipe the on-device library store so the app re-syncs from
# scratch on next launch.  The store, covers and legacy json live next
# to the config file in the app dir.  Deliberately leaves explorer-3.db
# (the firmware reader's reading-progress DB) alone.
if [ "${DO_RESET_DB}" = "1" ]; then
	echo "==> resetting on-device library db + cover cache"
	ssh ${SSH_COMMON} "root@${DEVICE}" sh -c '
		rm -f /mnt/ext1/system/bin/bookshelf_lib.db \
			/mnt/ext1/system/bin/bookshelf_lib.json
		rm -rf /mnt/ext1/system/bin/covers
	'
fi

# Push the binary and config.  The destination IS
# /mnt/ext1/system/bin/bookshelf.app: monitor.app resolves the home app
# by checking that path BEFORE the firmware's /ebrmain/bin/bookshelf.app
# (verified in the launcher disassembly at 0x33b48–0x33b74), so the
# binary installed there becomes the HOME task, registered under the
# app name "bookshelf.app" — pressing the Home button anywhere brings
# OUR bookshelf to the foreground (taskmgr's main_menu action), not the
# stock UI.  The binary is installed directly — no script wrapper: a
# wrapper's exec would register the home task as the wrapper, which
# breaks the reader's book-open handshake (the reader shows an
# hourglass and closes).  If the binary is ever missing, the launcher
# falls back to the stock /ebrmain/bin/bookshelf.app on its own.
scp ${SSH_COMMON} "${SRC_APP}" "root@${DEVICE}:/mnt/ext1/system/bin/bookshelf.app"
scp ${SSH_COMMON} "${SRC_CFG}" "root@${DEVICE}:/mnt/ext1/system/bin/bookshelf.cfg"

# Make the binary executable, kill any stale copy, restart cleanly.
# The `killall` is best-effort: it's OK if no process matches.
ssh ${SSH_COMMON} "root@${DEVICE}" sh -c '
	set -e
	chmod +x /mnt/ext1/system/bin/bookshelf.app
	# Clear any stale log so the next run is easy to read.
	: >/mnt/ext1/applications/bookshelf.log
	killall bookshelf.app 2>/dev/null || true
	sleep 1
'

echo "==> installed.  verify with:"
echo "    ssh root@${DEVICE} 'tail -f /mnt/ext1/applications/bookshelf.log'"
echo "    reboot the device; the custom bookshelf IS the home screen"
echo "    (Home button opens it; the binary is the home task directly)."
