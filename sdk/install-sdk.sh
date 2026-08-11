#!/bin/sh
# Install the PocketBook SDK-B288 headers + libraries needed to build guest
# applications against firmware U633_6.8.2817 (and other 6.5-based firmwares).
#
# Downloads from the public pocketbook/SDK_6.3.0 GitHub repository
# (branch 6.5) which serves the same revision that the firmware was built
# against (per the RPATH recorded inside cramfs/bin/bookshelf.app).
#
# Every file's SHA-256 is pinned below; a changed or tampered file fails
# loudly instead of silently breaking builds.  Re-record the digests
# after an intentional upstream change with:
#     curl -fsSL "${SRC_BASE}/include/<file>" | sha256sum -
#
# Re-running this script is safe: it overwrites files in place.

set -eu

# Resolve the script's own directory and the destination tree.
HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
DEST="${HERE}/pocketbook-sdk-b288"
SRC_BASE="https://raw.githubusercontent.com/pocketbook/SDK_6.3.0/6.5/SDK-B288/usr/arm-obreey-linux-gnueabi/sysroot/usr/local"

mkdir -p "${DEST}/include" "${DEST}/lib"

# Pinned SHA-256 digests of the files as served by SDK_6.3.0@6.5
# (recorded 2026-08-11).
expected_sha256() {
	case "$1" in
	hwconfig.h) echo 4203be3a6448bdfcb0a5cc47625070999db13fcf905b04857f0704c00d421ed6 ;;
	inkview.h) echo 26d728cdeaa386d34778c7dbb8c94002054da5b0e70c87359a3da7d7aed8673b ;;
	inkinternal.h) echo e3168d506d13742890c8ccbd812a2bd3040663f5387c794d40484c6a56b32676 ;;
	inkplatform.h) echo 2aa30ecdafaef54d96b51196640d38df733176f2e65874a03d776b3d150a3404 ;;
	inklog.h) echo a0b891773daa985149c9ebe9f5c2f71de8e384df3de5af0005ded2553e9dddfc ;;
	scrollview.h) echo 935773193b4fb145458d63e6ad54771afa363084f3eb0a8af349596065e6c2cf ;;
	selection_list.h) echo 55b577a9a963860ae744cd85370a0c256f4ac4ef3734841e24844385c76a000e ;;
	line_color_improver.h) echo 3ddbaed9e28b1b88b47ac628335045a117720018cfcbb7df3882194c9a99a2ab ;;
	time_test.h) echo 7df806210c32359fff11ed092a61677e44ca00b6e91cf177d29d858a1bb015ad ;;
	libhwconfig.so) echo f4179061bd5935343e11f1dd1c43c80cfe68b5d81ffd0ab228a93c615cc3d69e ;;
	libhwconfig.static.a) echo 888006c7fad5a334e9a686d610027d7b682d0046125c9cefe52c23040a1f909d ;;
	libinkview.so) echo 3a7f2e4fdd3be5cf91d7c380c15f21b692c4ca72d7e140b07af261c99ef88209 ;;
	*) echo "" ;;
	esac
}

# fetch_verify <src-subdir>/<file> <dest> <timeout> — stream one file,
# verifying its pinned SHA-256 while saving it (a single download via
# `curl | tee | sha256sum -`).  Exits 1 on mismatch, printing the
# file + expected + actual digests, and removes the bad file.
fetch_verify() {
	_rel="$1"
	_dest="$2"
	_timeout="$3"
	_expected=$(expected_sha256 "$(basename "${_rel}")")
	_actual=$(
		curl -fsSL --max-time "${_timeout}" "${SRC_BASE}/${_rel}" |
			tee "${_dest}" |
			sha256sum - |
			awk '{print $1}'
	)
	if [ -z "${_expected}" ]; then
		echo "FAIL (no pinned digest for ${_rel})" >&2
		rm -f "${_dest}"
		exit 1
	fi
	if [ "${_actual}" != "${_expected}" ]; then
		echo "FAIL (sha256 mismatch)" >&2
		echo "  file:     ${_rel}" >&2
		echo "  expected: ${_expected}" >&2
		echo "  actual:   ${_actual}" >&2
		rm -f "${_dest}"
		exit 1
	fi
}

HEADERS="hwconfig.h inkview.h inkinternal.h inkplatform.h inklog.h scrollview.h selection_list.h line_color_improver.h time_test.h"
LIBS="libhwconfig.so libhwconfig.static.a libinkview.so"

echo "Downloading headers -> ${DEST}/include"
for h in ${HEADERS}; do
	printf '  %-25s ' "${h}"
	fetch_verify "include/${h}" "${DEST}/include/${h}" 60
	echo "ok ($(stat -c%s "${DEST}/include/${h}") bytes)"
done

echo "Downloading libraries -> ${DEST}/lib"
for l in ${LIBS}; do
	printf '  %-25s ' "${l}"
	fetch_verify "lib/${l}" "${DEST}/lib/${l}" 120
	echo "ok ($(stat -c%s "${DEST}/lib/${l}") bytes)"
done

echo
echo "SDK installed at: ${DEST}"
echo "Cross compiler expected: arm-linux-gnueabi-gcc"
echo "Verify with:"
echo "  arm-linux-gnueabi-gcc -I${DEST}/include -L${DEST}/lib ..."
