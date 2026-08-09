#!/bin/sh
#
# install-koreader.sh — install KOReader (PocketBook build) into the
# pbemu submodule's emulator tree.
# emulator, straight from the GitHub release.
#
# Usage:
#   bookshelf/install-koreader.sh [version]
#
# Arguments:
#   [version]  Release tag to install, e.g. `v2026.07.1`.  Omitted → the
#              latest release is resolved via the GitHub API.
#
# What it does:
#   1. Resolves the release tag (latest by default).
#   2. Downloads koreader-pocketbook-<tag>.zip from GitHub releases
#      (cached in /tmp; delete the file to force a re-download).
#   3. Extracts the archive into the emulator's .live tree at
#      U633_6.8.2817/.live/mnt/ext1 — the container's /mnt is a bind
#      mount of that tree, so a running emulator sees the files
#      immediately (no podman cp needed).
#   4. The install mirrors the official PocketBook layout:
#        /mnt/ext1/applications/koreader.app   (launcher script)
#        /mnt/ext1/applications/koreader/      (the app itself)
#        /mnt/ext1/system/bin/koreader.app     (firmware launcher entry)
#
# The bookshelf app probes /mnt/ext1/applications/koreader.app at
# startup and offers KOReader in Settings → Reader once it exists.
# Restart the bookshelf app (or reboot the emulator) after installing.
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

GITHUB_API="https://api.github.com/repos/koreader/koreader/releases"
GITHUB_DL="https://github.com/koreader/koreader/releases/download"

# Resolve the tag to install: explicit argument, else the latest
# release's tag_name from the GitHub API.
TAG="${1:-}"
if [ -z "${TAG}" ]; then
	echo "==> resolving latest KOReader release"
	TAG=$(curl -fsSL --max-time 30 "${GITHUB_API}/latest" | grep '"tag_name"' | head -1 | cut -d'"' -f4)
	if [ -z "${TAG}" ]; then
		echo "ERROR: could not resolve the latest release tag" >&2
		echo "       pass one explicitly: $0 v2026.07.1" >&2
		exit 1
	fi
fi
case "${TAG}" in
v*) ;;
*) TAG="v${TAG}" ;;
esac

ZIP="/tmp/koreader-pocketbook-${TAG}.zip"
TARGET="${REPO_ROOT}/pbemu/U633_6.8.2817/.live/mnt/ext1"

if [ ! -d "${TARGET}/applications" ]; then
	echo "ERROR: emulator tree not staged at ${TARGET}" >&2
	echo "       run 'pbemu/pbemu install U633_6.8.2817' first" >&2
	exit 1
fi

if [ ! -f "${ZIP}" ]; then
	echo "==> downloading ${GITHUB_DL}/${TAG}/koreader-pocketbook-${TAG}.zip"
	curl -fL --max-time 300 -o "${ZIP}" \
		"${GITHUB_DL}/${TAG}/koreader-pocketbook-${TAG}.zip"
else
	echo "==> using cached ${ZIP}"
fi

echo "==> extracting into ${TARGET}"
rm -rf \
	"${TARGET}/applications/koreader" \
	"${TARGET}/applications/koreader.app" \
	"${TARGET}/system/bin/koreader.app"
unzip -o -q "${ZIP}" -d "${TARGET}"
# The archive carries the exec bits; re-assert them in case umask
# interfered (unzip honours the stored modes, so this is belt+braces).
chmod 755 "${TARGET}/applications/koreader.app" "${TARGET}/system/bin/koreader.app"
# The emulator guest runs as a non-root uid: the launcher script writes
# crash.log and settings into the koreader dir, so it must be writable
# by anyone (on a real device koreader runs as root).
chmod 777 "${TARGET}/applications/koreader"

echo "==> installed ${TAG}:"
ls -la "${TARGET}/applications/koreader.app" "${TARGET}/applications/koreader" | head -8
echo
echo "    The bookshelf detects KOReader at startup — restart the app"
echo "    (killall bookshelf.app in the container, or reboot the emulator)"
echo "    and pick it in Settings → Reader."
