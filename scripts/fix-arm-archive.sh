#!/bin/bash
# fix-arm-archive.sh — make cargo-zigbuild's libeh_pb.a linkable by the
# pbdev container's ARM GNU ld.
#
# MuPDF's resource blobs (base14 font *.cff, hyph-all.zip) are produced
# by GNU make's BUILT-IN `LD = ld`, i.e. the HOST linker, so under
# cargo-zigbuild they end up as ELF64-x86-64 relocatable objects inside
# an otherwise ELF32-ARM archive — and the cross ld rejects the whole
# archive ("file format not recognized").
#
# Fix: re-link each bad blob for ARM.  The payload is pure data, so we
# extract it on the host (the cross objcopy cannot read ELF64-x86) and
# rebuild the object inside the container with
#   <cross>-ld -r -b binary -o <name>.o <payload>
# naming the payload so the derived _binary_<base>_start/_end symbols
# match what noto.o / hyphen.o expect.
#
# Usage: scripts/fix-arm-archive.sh <path/to/libeh_pb.a>
# Env:   FIX_CROSS_PREFIX  cross toolchain prefix (default arm-linux-gnueabi;
#                          use arm-linux-gnueabihf for armhf archives)
set -eu

LIB=$(realpath "$1")
[ -f "$LIB" ] || { echo "ERROR: $1 missing" >&2; exit 1; }

HERE=$(unset CDPATH; cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "${HERE}/.." && pwd)
cd "$REPO_ROOT"
CONTAINER=${PBDEV_CONTAINER:-localhost/pbdev}
CROSS_PREFIX=${FIX_CROSS_PREFIX:-arm-linux-gnueabi}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/host" "$WORK/guest"

# 1. Extract every member on the host (host ar/binutils read both ELFs).
(cd "$WORK/host" && ar x "$(realpath "$LIB")")

# 2. Find ELF64 members (EI_CLASS == 2) and dump their data payload plus
#    the symbol base (_binary_<BASE>_start).
shopt -s nullglob
bad=0
for o in "$WORK/host"/*.o; do
	class=$(dd if="$o" bs=1 skip=4 count=1 2>/dev/null | od -An -tu1)
	[ "$((class))" -eq 2 ] || continue
	base=$(${CROSS_NM:-nm} "$o" | awk '$3 ~ /^_binary_/ && $3 ~ /_start$/ {sub(/^_binary_/, "", $3); sub(/_start$/, "", $3); print $3; exit}')
	if [ -z "$base" ]; then
		echo "ERROR: no _binary_*_start symbol in $(basename "$o")" >&2
		exit 1
	fi
	objcopy -I elf64-x86-64 -O binary --only-section=.data "$o" "$WORK/guest/$base"
	cp "$o" "$WORK/guest/$(basename "$o")"
	# member-name -> payload-name pair for the container-side relinker
	echo "$(basename "$o")|$base" >> "$WORK/guest/relink.list"
	bad=$((bad + 1))
done

if [ "$bad" -eq 0 ]; then
	echo "==> archive is clean (no ELF64 members)"
	exit 0
fi

# 3. Re-link each payload for ARM inside the pbdev container, writing
#    the object OVER its original member-name copy, then replace the
#    members in the archive.  Seed the output with the untouched archive;
#    ar r matches replacements by basename.
cp "$LIB" "$WORK/fixed.a"
podman run --rm \
	-e CROSS_PREFIX="$CROSS_PREFIX" \
	-v "$WORK:/job:z" -w /job/guest "$CONTAINER" \
	bash -euo pipefail -c '
	while IFS="|" read -r orig base; do
		"${CROSS_PREFIX}-ld" -r -b binary -z noexecstack \
			-o "$orig" "$base"
	done < relink.list
	for f in *.o; do
		"${CROSS_PREFIX}-ar" r /job/fixed.a "$f"
	done
'

mv "$WORK/fixed.a" "$LIB"
echo "==> relinked $bad resource object(s) for ARM in ${LIB#$REPO_ROOT/}"
