#!/bin/sh
#
# install-koreader-device.sh — push KOReader (PocketBook build) to a real
# PocketBook over ssh, straight from the GitHub release.  Modeled on
# install-device.sh.
#
# Usage:
#   bookshelf/install-koreader-device.sh <device-ip> [version]
#
# Arguments:
#   <device-ip>  SSH target for the PocketBook (root@<ip>).  Passwordless
#                ssh must already be configured (ssh-copy-id once).
#   [version]    Release tag to install, e.g. `v2026.07.1`.  Omitted →
#                the latest release is resolved via the GitHub API.
#
# What it does:
#   1. Resolves the release tag (latest by default).
#   2. Downloads koreader-pocketbook-<tag>.zip from GitHub releases
#      (cached in /tmp; delete the file to force a re-download).
#   3. Removes any previous KOReader on the device, then streams the
#      archive over ssh via tar — scp cannot overwrite root-owned files
#      and does NOT preserve exec bits, which KOReader's binaries need.
#   4. Installs the official PocketBook layout under /mnt/ext1:
#        applications/koreader.app   (launcher script)
#        applications/koreader/      (the app itself)
#        system/bin/koreader.app     (firmware launcher entry)
#
# The bookshelf app probes /mnt/ext1/applications/koreader.app at
# startup and offers KOReader in Settings → Reader once it exists.
# Restart the bookshelf app (or reboot) after installing.
#
set -eu

# PocketBook firmware ships an older dropbear that still defaults to
# ssh-rsa for host keys.  OpenSSH 8.8+ disabled the matching signature
# algorithm by default, so we have to re-enable it explicitly on every
# ssh/scp invocation below — otherwise the handshake fails with
# "no matching host key type found".
SSH_COMMON='-o BatchMode=yes -o HostKeyAlgorithms=+ssh-rsa'

GITHUB_API="https://api.github.com/repos/koreader/koreader/releases"
GITHUB_DL="https://github.com/koreader/koreader/releases/download"

usage() {
	cat >&2 <<EOF
usage: $(basename "$0") <device-ip> [version]
       $(basename "$0") <device-ip> v2026.07.1

Pushes KOReader to <device-ip>:/mnt/ext1 (official PocketBook layout).
Version omitted → latest release.

EOF
	exit 64
}

DEVICE="${1:-}"
TAG="${2:-}"
if [ -z "${DEVICE}" ]; then
	usage
fi

# Resolve the tag to install: explicit argument, else the latest
# release's tag_name from the GitHub API.
if [ -z "${TAG}" ]; then
	echo "==> resolving latest KOReader release"
	TAG=$(curl -fsSL --max-time 30 "${GITHUB_API}/latest" | grep '"tag_name"' | head -1 | cut -d'"' -f4)
	if [ -z "${TAG}" ]; then
		echo "ERROR: could not resolve the latest release tag" >&2
		echo "       pass one explicitly: $0 <device-ip> v2026.07.1" >&2
		exit 1
	fi
fi
case "${TAG}" in
v*) ;;
*) TAG="v${TAG}" ;;
esac

ZIP="/tmp/koreader-pocketbook-${TAG}.zip"
UNPACK="/tmp/koreader-pocketbook-${TAG}"

# Sanity-check that we can ssh to the device non-interactively.  Refuse
# to continue if password auth would be required, so the user notices
# before the script half-pushes files and then hangs on ssh.  The ssh
# stderr is shown — it distinguishes "No route to host" (device asleep
# / off Wi-Fi) from a host-key or auth problem.
if ! ssh ${SSH_COMMON} -o ConnectTimeout=5 "root@${DEVICE}" true; then
	echo "ERROR: passwordless ssh to root@${DEVICE} failed" >&2
	echo "       run: ssh-copy-id root@${DEVICE}" >&2
	exit 1
fi

if [ ! -f "${ZIP}" ]; then
	echo "==> downloading ${GITHUB_DL}/${TAG}/koreader-pocketbook-${TAG}.zip"
	curl -fL --max-time 300 -o "${ZIP}" \
		"${GITHUB_DL}/${TAG}/koreader-pocketbook-${TAG}.zip"
else
	echo "==> using cached ${ZIP}"
fi

rm -rf "${UNPACK}"
mkdir -p "${UNPACK}"
unzip -o -q "${ZIP}" -d "${UNPACK}"

echo "==> staging ${TAG} to ${DEVICE}:/mnt/ext1"

# Remove any previous KOReader.  rm (not scp overwrite) is used because
# a previous install may be owned by a different user (the pbjb
# installer writes as root) — rm only needs directory write permission.
# The whole archive is then streamed over ssh with tar, which preserves
# the exec bits and symlinks that KOReader's binaries require (scp -r
# would flatten everything to 0644).
ssh ${SSH_COMMON} "root@${DEVICE}" rm -rf \
	/mnt/ext1/applications/koreader \
	/mnt/ext1/applications/koreader.app \
	/mnt/ext1/system/bin/koreader.app
# Stage the archive to a temp file first, then stream the file over ssh.
# Inside a ``tar | ssh`` pipeline a failed local tar would be masked by
# ssh's exit status (POSIX sh has no pipefail), silently leaving a
# half-installed KOReader; with the file staged first, set -e catches a
# tar failure directly and ssh's status still reflects the remote tar.
TARBALL="/tmp/koreader-pocketbook-${TAG}.tar"
tar -C "${UNPACK}" -cf "${TARBALL}" applications system
ssh ${SSH_COMMON} "root@${DEVICE}" 'cd /mnt/ext1 && tar -xf -' < "${TARBALL}"
rm -f "${TARBALL}"

echo "==> installed.  verify with:"
echo "    ssh root@${DEVICE} 'ls -l /mnt/ext1/applications/koreader.app'"
echo "    restart bookshelf (killall bookshelf.app) or reboot; then"
echo "    pick KOReader in the bookshelf Settings → Reader."
