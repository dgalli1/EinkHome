#!/bin/sh
#
# build-rust.sh - Build the Rust native library (eh_lib) static archives
# that the C app links.  Exposes the book metadata/cover extraction API
# declared in app/data/eh_extract.h.
#
# Produces three staticlibs:
#   build/libeh_lib.a       armv7-unknown-linux-gnueabi   (armel / emulator)
#   build/libeh_lib_armhf.a armv7-unknown-linux-gnueabihf (armhf InkPad One)
#   build/libeh_lib_host.a  x86_64-unknown-linux-gnu      (PC/SDL desktop)
#
# The armv7 targets are the firmware's soft-/hard-float EABI ABIs. rustup's
# prebuilt std for each targets the base glibc for that ABI (older than the
# firmware's glibc 2.23), so the device archives need no Cargo-zigbuild; the
# final link against the firmware libc happens in sdk/build_armel.sh /
# build_armhf.sh. (inkview-rs uses `cargo zigbuild --target
# armv7-unknown-linux-gnueabi.2.23` for the same reason — pinning the glibc
# explicitly — but the plain target is sufficient for a staticlib.)
#
# Usage:
#     scripts/build-rust.sh            # all three archives
#     scripts/build-rust.sh arm        # device (gnueabi) archive only
#     scripts/build-rust.sh armhf      # armhf archive only
#     scripts/build-rust.sh host       # host archive only

set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "${HERE}/.." && pwd)
CRATE="${REPO_ROOT}/eh_lib"
OUT_DIR="${REPO_ROOT}/build"

ARM_TARGET=armv7-unknown-linux-gnueabi
ARMHF_TARGET=armv7-unknown-linux-gnueabihf

mkdir -p "${OUT_DIR}"

MODE="${1:-all}"
case "${MODE}" in
all | arm)
	echo "==> armv7 gnueabi staticlib (${ARM_TARGET})"
	(cd "${CRATE}" && cargo build --release --target "${ARM_TARGET}")
	cp "${CRATE}/target/${ARM_TARGET}/release/libeh_lib.a" "${OUT_DIR}/libeh_lib.a"
	echo "    -> ${OUT_DIR}/libeh_lib.a"
	;;
esac

case "${MODE}" in
all | armhf)
	echo "==> armv7 gnueabihf staticlib (${ARMHF_TARGET})"
	(cd "${CRATE}" && cargo build --release --target "${ARMHF_TARGET}")
	cp "${CRATE}/target/${ARMHF_TARGET}/release/libeh_lib.a" "${OUT_DIR}/libeh_lib_armhf.a"
	echo "    -> ${OUT_DIR}/libeh_lib_armhf.a"
	;;
esac

case "${MODE}" in
all | host)
	echo "==> host staticlib (x86_64-unknown-linux-gnu)"
	(cd "${CRATE}" && cargo build --release)
	cp "${CRATE}/target/release/libeh_lib.a" "${OUT_DIR}/libeh_lib_host.a"
	echo "    -> ${OUT_DIR}/libeh_lib_host.a"
	;;
esac

# fail loudly on an unknown mode
case "${MODE}" in
all | arm | armhf | host) ;;
*)
	echo "unknown mode: ${MODE} (expected: all|arm|armhf|host)" >&2
	exit 1
	;;
esac