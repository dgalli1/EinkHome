#!/bin/sh
# Install the PocketBook SDK-B288 headers + libraries needed to build guest
# applications against firmware U633_6.8.2817 (and other 6.5-based firmwares).
#
# Downloads from the public pocketbook/SDK_6.3.0 GitHub repository
# (branch 6.5) which serves the same revision that the firmware was built
# against (per the RPATH recorded inside cramfs/bin/bookshelf.app).
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

HEADERS="hwconfig.h inkview.h inkinternal.h inkplatform.h inklog.h scrollview.h selection_list.h line_color_improver.h time_test.h"
LIBS="libhwconfig.so libhwconfig.static.a libinkview.so"

echo "Downloading headers -> ${DEST}/include"
for h in ${HEADERS}; do
	printf '  %-25s ' "${h}"
	if curl -fsSL --max-time 60 -o "${DEST}/include/${h}" "${SRC_BASE}/include/${h}"; then
		echo "ok ($(stat -c%s "${DEST}/include/${h}") bytes)"
	else
		echo "FAIL"
		exit 1
	fi
done

echo "Downloading libraries -> ${DEST}/lib"
for l in ${LIBS}; do
	printf '  %-25s ' "${l}"
	if curl -fsSL --max-time 120 -o "${DEST}/lib/${l}" "${SRC_BASE}/lib/${l}"; then
		echo "ok ($(stat -c%s "${DEST}/lib/${l}") bytes)"
	else
		echo "FAIL"
		exit 1
	fi
done

echo
echo "SDK installed at: ${DEST}"
echo "Cross compiler expected: arm-linux-gnueabi-gcc"
echo "Verify with:"
echo "  arm-linux-gnueabi-gcc -I${DEST}/include -L${DEST}/lib ..."
