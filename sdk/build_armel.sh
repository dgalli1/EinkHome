#!/bin/sh
#
# build_armel.sh - Cross-compile a guest app for firmware U633_6.8.2817.
#
# Builds a guest ELF that:
#   - links against the firmware's own libc.so.6 (glibc 2.23) and libm.so
#   - dynamically links libinkview.so + libhwconfig.so from
#     sdk/pocketbook-sdk-b288/lib
#   - loads transitive deps (libpng12, libjpeg8, libtiff5, libfreetype6,
#     libssl1.0.0, libicuuc58, libcurl4, libz) at run-time from the firmware
#     tree under /ebrmain/lib, where the real copies already live
#
# Usage:
#     sdk/build_armel.sh                       # builds sdk/hello/hello.c
#     sdk/build_armel.sh <src.c> [extra cflags...]
#     sdk/build_armel.sh <src.c> --output <path>
#
# Output:
#     build/<basename> in the repo root (or whatever --output specifies).

set -eu

# Locate repo root from this script's path: this file is <repo>/sdk/build_armel.sh.
HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)

SDK="${REPO_ROOT}/sdk/pocketbook-sdk-b288"
FIRMWARE="${REPO_ROOT}/U633_6.8.2817"
SYSROOT="${FIRMWARE}/rootfs"

# Sanity checks up front so the failure mode is clear instead of buried in
# the gcc error stream.
for path in \
	"${SDK}/include/inkview.h" \
	"${SYSROOT}/lib/libc.so.6" \
	"${FIRMWARE}/ebrmain/cramfs/lib/libz.so.1.2.11"; do
	if [ ! -f "${path}" ]; then
		echo "ERROR: required input missing: ${path}" >&2
		echo "  - run sdk/install-sdk.sh to fetch the PocketBook SDK headers" >&2
		echo "  - run ./pbemu install <firmware.zip> to stage U633_6.8.2817" >&2
		exit 1
	fi
done

# Default source: sdk/hello/hello.c (kept for backwards compatibility with
# the original proof-of-concept).
SRC_DEFAULT="${REPO_ROOT}/sdk/hello/hello.c"

# Parse args. We allow the caller to pass extra cflags which we splice
# through verbatim before the final output path.
OUTPUT=""
SRC=""
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
	--help | -h)
		sed -n '2,30p' "$0"
		exit 0
		;;
	--* | -*)
		# Treat any other flag as an extra gcc arg; we'll splice it
		# through later.
		EXTRA_FLAGS="${EXTRA_FLAGS:-} $1"
		shift
		continue
		;;
	esac
	# First non-option argument is the source file. Anything after it is
	# appended to EXTRA_FLAGS verbatim (extra gcc args).
	SRC="$1"
	shift
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
		--help | -h)
			sed -n '2,30p' "$0"
			exit 0
			;;
		*)
			EXTRA_FLAGS="${EXTRA_FLAGS:-} $1"
			shift
			;;
		esac
	done
	break
done

# Default source when the caller did not supply one.
if [ -z "${SRC}" ]; then
	SRC="${SRC_DEFAULT}"
fi

if [ ! -f "${SRC}" ]; then
	echo "ERROR: source file not found: ${SRC}" >&2
	exit 1
fi

# Resolve source path inside the bind mount: bind-mount is on /work.
SRC_REL=$(echo "${SRC}" | sed "s|^${REPO_ROOT}/||")

NAME=$(basename "${SRC}" .c)
OUT_REL="${OUTPUT:-build/${NAME}}"
mkdir -p "$(dirname "${REPO_ROOT}/${OUT_REL}")"

# All paths the compiler sees inside the container are /work/...
CONTAINER_SRC="/work/${SRC_REL}"
CONTAINER_OUT="/work/${OUT_REL}"
CONTAINER_SDK_INC="/work/sdk/pocketbook-sdk-b288/include"
CONTAINER_SDK_LIB="/work/sdk/pocketbook-sdk-b288/lib"
CONTAINER_FW_LIBZ="/work/U633_6.8.2817/ebrmain/cramfs/lib/libz.so.1.2.11"
CONTAINER_FW_LIBC="/work/U633_6.8.2817/rootfs/lib/libc.so.6"
CONTAINER_FW_LM="/work/U633_6.8.2817/rootfs/lib/libm.so.6"
CONTAINER_SYSROOT="/work/U633_6.8.2817/rootfs"
# Crt objects live with the cross compiler, not in the firmware rootfs.
CRT_CROSS_DIR="/usr/lib/gcc-cross/arm-linux-gnueabi/12"
CRT_FW_DIR="/usr/arm-linux-gnueabi/lib"
CRT1="${CRT_FW_DIR}/crt1.o"
CRTI="${CRT_FW_DIR}/crti.o"
CRTB="${CRT_CROSS_DIR}/crtbeginS.o"
CRTE="${CRT_CROSS_DIR}/crtendS.o"
CRTN="${CRT_FW_DIR}/crtn.o"

# shellcheck disable=SC2086
podman run --rm \
	-v "${REPO_ROOT}:/work:z" \
	-w /work \
	localhost/pbdev:latest \
	/usr/bin/arm-linux-gnueabi-gcc \
	"-I${CONTAINER_SDK_INC}" \
	"-I/usr/include" \
	"-Wl,-rpath,${CONTAINER_SDK_LIB}" \
	"-L${CONTAINER_SDK_LIB}" \
	"--sysroot=${CONTAINER_SYSROOT}" \
	-nostartfiles \
	"${CRT1}" \
	"${CRTI}" \
	"${CRTB}" \
	-Wall -Wextra -Werror=implicit-function-declaration -O2 \
	${EXTRA_FLAGS:-} \
	"${CONTAINER_SRC}" \
	-o "${CONTAINER_OUT}" \
	"-Wl,-dynamic-linker,/lib/ld-linux.so.3" \
	"-Wl,--allow-shlib-undefined" \
	"-Wl,--sysroot=${CONTAINER_SYSROOT}" \
	"${CONTAINER_SDK_LIB}/libinkview.so" \
	"${CONTAINER_SDK_LIB}/libhwconfig.so" \
	"${CONTAINER_FW_LIBZ}" \
	"${CONTAINER_FW_LIBC}" \
	"${CONTAINER_FW_LM}" \
	-lgcc -lgcc_s \
	"${CRTE}" \
	"${CRTN}"

echo
echo "Built: ${REPO_ROOT}/${OUT_REL}"
echo "Size:  $(stat -c%s "${REPO_ROOT}/${OUT_REL}") bytes"
echo "GLIBC: $(strings "${REPO_ROOT}/${OUT_REL}" | grep '^GLIBC_' | sort -u | tr '\n' ' ')"
