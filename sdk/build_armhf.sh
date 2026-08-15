#!/bin/sh
#
# build_armhf.sh - Cross-compile the guest app for ARM hard-float
# (armhf) firmware, e.g. the InkPad One (U1030_6.11.1437).
#
# The default build (build_armel.sh) targets soft-float PocketBooks.
# Hard-float firmwares ship an armhf libinkview/libhwconfig and an armhf
# rootfs; the SDK's own libs are armel-only, so this variant links
# against the FIRMWARE's armhf libs instead.
#
# Usage:
#     PBEMU_FIRMWARE_DIR=pbemu/U1030_6.11.1437 \
#         sdk/build_armhf.sh <src.c> ... --output build/bookshelf.armhf.app
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

SDK="${REPO_ROOT}/sdk/pocketbook-sdk-b288"
FIRMWARE="${PBEMU_FIRMWARE_DIR:-${REPO_ROOT}/pbemu/U1030_6.11.1437}"
SYSROOT="${FIRMWARE}/rootfs"
FW_LIB="${FIRMWARE}/ebrmain/lib"

for path in \
	"${SDK}/include/inkview.h" \
	"${SYSROOT}/lib/libc.so.6" \
	"${FW_LIB}/libinkview.so" \
	"${FW_LIB}/libhwconfig.so"; do
	if [ ! -e "${path}" ]; then
		echo "ERROR: required path missing: ${path}" >&2
		exit 1
	fi
done

OUTPUT=""
SRCS=""
EXTRA_FLAGS=""
while [ "$#" -gt 0 ]; do
	case "$1" in
	--output)
		OUTPUT="$2"
		shift 2
		continue
		;;
	--output=*)
		OUTPUT="${1#--output=}"
		shift
		continue
		;;
	--* | -*)
		EXTRA_FLAGS="${EXTRA_FLAGS:-} $1"
		shift
		continue
		;;
	esac
	case "$1" in
	*.c) SRCS="${SRCS} $1" ;;
	*) EXTRA_FLAGS="${EXTRA_FLAGS:-} $1" ;;
	esac
	shift
done
[ -n "${SRCS}" ] || {
	echo "ERROR: no sources given" >&2
	exit 1
}
[ -n "${OUTPUT}" ] || OUTPUT="build/bookshelf.armhf.app"
mkdir -p "$(dirname "${REPO_ROOT}/${OUTPUT}")"

CONTAINER_SRCS=""
for _src in ${SRCS}; do
	CONTAINER_SRCS="${CONTAINER_SRCS} /work/$(echo "${_src}" | sed "s|^${REPO_ROOT}/||")"
done
case "${OUTPUT}" in
/*) CONTAINER_OUT="${OUTPUT}" ;;
*) CONTAINER_OUT="/work/${OUTPUT}" ;;
esac

podman run --rm \
	-v "${REPO_ROOT}:/work:z" \
	-w /work \
	localhost/pbdev:latest \
	/usr/bin/arm-linux-gnueabihf-gcc \
	"-I/work/sdk/pocketbook-sdk-b288/include" \
	"-I/work/app/core" "-I/work/app/data" "-I/work/app/ui" \
	"-I/work/app/action" "-I/work/app/vendor" "-I/work/app/platform" \
	"-I/usr/include" \
	"-L/work/pbemu/U1030_6.11.1437/rootfs/lib" \
	"--sysroot=/work/pbemu/U1030_6.11.1437/rootfs" \
	-nostartfiles \
	"/usr/arm-linux-gnueabihf/lib/crt1.o" \
	"/usr/arm-linux-gnueabihf/lib/crti.o" \
	"/usr/lib/gcc-cross/arm-linux-gnueabihf/12/crtbeginS.o" \
	-Wall -Wextra -Werror=implicit-function-declaration -O2 \
	${EXTRA_FLAGS:-} \
	${CONTAINER_SRCS} \
	-o "${CONTAINER_OUT}" \
	"-Wl,-dynamic-linker,/lib/ld-linux-armhf.so.3" \
	"-Wl,--allow-shlib-undefined" \
	"-Wl,--sysroot=/work/pbemu/U1030_6.11.1437/rootfs" \
	"/work/pbemu/U1030_6.11.1437/ebrmain/lib/libinkview.so" \
	"/work/pbemu/U1030_6.11.1437/ebrmain/lib/libhwconfig.so" \
	"/work/pbemu/U1030_6.11.1437/ebrmain/lib/libz.so" \
	"/work/pbemu/U1030_6.11.1437/ebrmain/lib/libsqlite3.so.0" \
	"/work/pbemu/U1030_6.11.1437/rootfs/lib/libm.so.6" \
	"/work/pbemu/U1030_6.11.1437/rootfs/lib/libpthread.so.0" \
	-lgcc -lgcc_s \
	"/usr/lib/gcc-cross/arm-linux-gnueabihf/12/crtendS.o" \
	"/usr/arm-linux-gnueabihf/lib/crtn.o"

echo
echo "Built: ${REPO_ROOT}/${OUTPUT}"
echo "Size:  $(stat -c%s "${REPO_ROOT}/${OUTPUT}") bytes"
