#!/bin/sh
#
# make-release.sh — build the EinkHome release zip.
#
# Builds both ABIs (armel for every PocketBook but the InkPad One,
# armhf for the hard-float InkPad One) and packages them with a
# POSIX-sh installer that auto-detects the device ABI over ssh:
#
#   build/einkhome-<version>.zip
#     bookshelf.app          (armel)
#     bookshelf.armhf.app    (armhf)
#     install.sh             (usage: install.sh <device-ip> [api-url])
#
# install.sh on the device:
#   1. probes /lib/ld-linux-armhf.so.3 to pick the armhf binary
#      (present on the InkPad One, absent on every soft-float device),
#   2. writes /mnt/ext1/system/bin/bookshelf.cfg with the API url +
#      api_token=pbemu-dev-token,
#   3. installs the binary as /mnt/ext1/system/bin/bookshelf.app —
#      the home task monitor.app checks before the firmware's own
#      /ebrmain/bin/bookshelf.app.  Direct install (no wrapper script):
#      a wrapper's exec would register the home task as the wrapper,
#      breaking the reader's book-open handshake.
#
# The version is `git describe --tags --always --dirty` (fallback "dev").
#
# Usage:
#   scripts/make-release.sh              # build + zip
#   scripts/make-release.sh --no-build   # zip only (reuse build/)
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

DO_BUILD=1
case "${1:-}" in
"" ) ;;
--no-build) DO_BUILD=0 ;;
-h|--help)
	sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
	exit 0
	;;
*)
	echo "ERROR: unknown argument: ${1}" >&2
	exit 1
	;;
esac

if [ "${DO_BUILD}" = "1" ]; then
	echo "==> building armel + armhf binaries"
	make -C "${REPO_ROOT}" all
	make -C "${REPO_ROOT}" armhf
fi

[ -f "${REPO_ROOT}/build/bookshelf.app" ] || {
	echo "ERROR: build/bookshelf.app missing (drop --no-build)" >&2
	exit 1
}
[ -f "${REPO_ROOT}/build/bookshelf.armhf.app" ] || {
	echo "ERROR: build/bookshelf.armhf.app missing (drop --no-build)" >&2
	exit 1
}

# Version: git describe, fallback "dev".
VERSION=$(
	git -C "${REPO_ROOT}" describe --tags --always --dirty 2>/dev/null || true
)
[ -n "${VERSION}" ] || VERSION="dev"
ZIP="${REPO_ROOT}/build/einkhome-${VERSION}.zip"

STAGE=$(mktemp -d)
trap 'rm -rf "${STAGE}"' EXIT INT TERM

cp "${REPO_ROOT}/build/bookshelf.app" "${STAGE}/bookshelf.app"
cp "${REPO_ROOT}/build/bookshelf.armhf.app" "${STAGE}/bookshelf.armhf.app"

cat >"${STAGE}/install.sh" <<'INSTALL_EOF'
#!/bin/sh
#
# install.sh — install EinkHome on a PocketBook over ssh.
#
# Usage: install.sh <device-ip> [api-url]
#
#   <device-ip>  SSH target (root@<ip>); passwordless ssh must be set
#                up first (ssh-copy-id).  PocketBook firmware ships an
#                older dropbear that needs ssh-rsa host keys, which
#                OpenSSH 8.8+ disabled by default — re-enabled below.
#   [api-url]    Full URL the on-device binary talks to.  When omitted,
#                the installer picks the host's primary LAN IPv4 and
#                uses http://<lan-ip>:${PBEMU_API_PORT:-8765}.
#
# Installs to /mnt/ext1/system/bin/bookshelf.app — the home task.
# Auto-selects the armhf binary (InkPad One U1030) by probing for
# /lib/ld-linux-armhf.so.3 on the device; every other PocketBook is
# soft-float and gets the armel binary.
set -eu

SSH_COMMON='-o BatchMode=yes -o HostKeyAlgorithms=+ssh-rsa'

DEVICE="${1:-}"
API_URL="${2:-}"
if [ -z "${DEVICE}" ]; then
	echo "usage: $0 <device-ip> [api-url]" >&2
	exit 64
fi

# ABI auto-detect: armhf firmware (InkPad One) ships the hard-float
# loader; soft-float firmwares ship only /lib/ld-linux.so.3.
if ssh ${SSH_COMMON} -o ConnectTimeout=5 "root@${DEVICE}" \
	test -e /lib/ld-linux-armhf.so.3 2>/dev/null; then
	BIN="bookshelf.armhf.app"
	echo "==> device is armhf (InkPad One) — using ${BIN}"
else
	BIN="bookshelf.app"
	echo "==> device is armel — using ${BIN}"
fi

# Resolve the api_url.  Same rule as scripts/install-device.sh: the
# host's primary LAN IPv4 (first non-loopback IPv4 with a default
# route) on port ${PBEMU_API_PORT:-8765}; explicit api-url wins.
if [ -z "${API_URL}" ]; then
	API_IP=$(
		ip -4 -o addr show scope global 2>/dev/null |
			awk '{print $4}' |
			cut -d/ -f1 |
			head -n1
	)
	if [ -z "${API_IP}" ]; then
		API_IP="${PBEMU_LAN_FALLBACK:-192.168.1.42}"
		echo "WARN: could not detect LAN ip; falling back to ${API_IP}" >&2
	fi
	API_URL="http://${API_IP}:${PBEMU_API_PORT:-8765}"
fi

# Sanity-check ssh before touching anything.
if ! ssh ${SSH_COMMON} -o ConnectTimeout=5 "root@${DEVICE}" true; then
	echo "ERROR: cannot ssh to root@${DEVICE} (passwordless ssh required)" >&2
	exit 1
fi

echo "==> installing ${BIN} to ${DEVICE}:/mnt/ext1/system/bin/"
echo "    api_url = ${API_URL}"

# Fresh config; api_token matches api/config/server.json.
printf 'api_url=%s\napi_token=pbemu-dev-token\n' "${API_URL}" >/tmp/bookshelf.cfg.$$

# A previously installed copy is often owned by a different user (the
# pbjb installer writes as root) or read-only, so scp cannot overwrite
# it in place.  Remove the stale files first (rm only needs write
# permission on the directory).
ssh ${SSH_COMMON} "root@${DEVICE}" rm -f \
	/mnt/ext1/system/bin/bookshelf.app /mnt/ext1/system/bin/bookshelf.cfg

scp ${SSH_COMMON} "${BIN}" "root@${DEVICE}:/mnt/ext1/system/bin/bookshelf.app"
scp ${SSH_COMMON} /tmp/bookshelf.cfg.$$ "root@${DEVICE}:/mnt/ext1/system/bin/bookshelf.cfg"
rm -f /tmp/bookshelf.cfg.$$

# Make it executable, clear the stale log, restart any running copy.
ssh ${SSH_COMMON} "root@${DEVICE}" sh -c '
	set -e
	chmod +x /mnt/ext1/system/bin/bookshelf.app
	: >/mnt/ext1/applications/bookshelf.log
	killall bookshelf.app 2>/dev/null || true
	sleep 1
'

echo "==> installed.  verify with:"
echo "    ssh root@${DEVICE} 'tail -f /mnt/ext1/applications/bookshelf.log'"
echo "    reboot the device; the custom bookshelf IS the home screen"
INSTALL_EOF
chmod +x "${STAGE}/install.sh"

# Build the zip with install.sh at the root so `unzip einkhome-*.zip`
# drops all three files into the current directory.
(
	cd "${STAGE}"
	zip -q -X "${ZIP}" bookshelf.app bookshelf.armhf.app install.sh
)

echo "==> ${ZIP}"
unzip -l "${ZIP}"
