#!/bin/sh
#
# capture-visual.sh — screenshot every UI page of one firmware for visual
# layout validation, then copy the PNGs to tmp/screenshots/<fw>/.
#
# Expects build/bookshelf.app to exist (run scripts/test-all-firmwares.sh
# first, or make) and the firmware tree to be staged (pbemu install).
# The mock books must be staged too (stage-mock-books.sh runs here).
#
# Parallelism: give each worker its own container name + API port so
# multiple captures can run at once:
#   PB_SYSTEM_CONTAINER=pb-pocketbook-ui-U627 PBEMU_TEST_API_PORT=18766 \
#     scripts/capture-visual.sh U627_6.5.2898
#
# Usage:
#   scripts/capture-visual.sh <firmware> [<firmware> ...]
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
PBEMU_DIR="${REPO_ROOT}/pbemu"
OUT_DIR="${REPO_ROOT}/tmp/screenshots"

[ "$#" -gt 0 ] || {
	echo "usage: $0 <firmware> [<firmware> ...]" >&2
	exit 1
}
[ -f "${REPO_ROOT}/build/bookshelf.app" ] || {
	echo "ERROR: build/bookshelf.app missing; run 'make' or scripts/test-all-firmwares.sh first" >&2
	exit 1
}

for fw in "$@"; do
	echo "==> ${fw}: capture"

	# Tree staged (pbemu install); the boot below creates the .live tree.
	if [ ! -d "${PBEMU_DIR}/${fw}" ]; then
		echo "ERROR: ${fw} not staged; run scripts/test-all-firmwares.sh first" >&2
		exit 1
	fi
	# Emulator support artifacts (shim/informer/probes) are firmware-
	# specific (built against each firmware's headers/glibc), so build
	# them for THIS firmware before booting.
	"${PBEMU_DIR}/pbemu" build "${fw}" >/dev/null 2>&1 || true

	# Hard-float devices (InkPad One) need the armhf binary; build it
	# once and point the suite fixture at it (PBEMU_APP_BINARY).
	ABI=$(awk -F'\t' -v name="${fw}" 'NR > 1 && $2 == name { print $5 }' \
		"${REPO_ROOT}/resources/supported-firmwares.tsv")
	PBEMU_APP_BINARY=""
	if [ "${ABI}" = "armhf" ]; then
		AHF="${REPO_ROOT}/build/bookshelf.armhf.app"
		if [ ! -f "${AHF}" ]; then
			echo "  building armhf binary"
			make armhf >/dev/null 2>&1 || true
		fi
		PBEMU_APP_BINARY="${AHF}"
	fi

	if ! PBEMU_NO_KEEPID=1 \
		PBEMU_PODMAN_ARGS="--tmpfs /sys:rw,nodev,nosuid,mode=755,size=2m" \
		"${PBEMU_DIR}/pbemu" start "${fw}" --no-viewer --no-audio --no-build >/dev/null 2>&1; then
		echo "FAIL: ${fw} emulator boot" >&2
		exit 1
	fi
	"${PBEMU_DIR}/pbemu" stop >/dev/null 2>&1 || true

	"${REPO_ROOT}/scripts/stage-mock-books.sh" "${PBEMU_DIR}/${fw}" >/dev/null 2>&1

	rm -rf "${REPO_ROOT}/build/screenshots/visual"
	if ! (
		cd "${REPO_ROOT}"
		PB_TEST_FIRMWARE="${fw}" \
			PBEMU_MOCK_BOOKS_DIR="${fw}/.live/mnt/ext1/books" \
			PBEMU_SYS_TMPFS=1 \
			PBEMU_APP_BINARY="${PBEMU_APP_BINARY}" \
			"${PBEMU_DIR}/.venv/bin/python" -m pytest tests/test_visual_capture.py -q
	) >"${REPO_ROOT}/build/fwtest/${fw}-capture.log" 2>&1; then
		echo "FAIL: ${fw} capture (see build/fwtest/${fw}-capture.log)" >&2
		tail -4 "${REPO_ROOT}/build/fwtest/${fw}-capture.log" | sed 's/^/  /' >&2
		exit 1
	fi

	# Copy the PNGs to the validation folder.
	dest="${OUT_DIR}/${fw}"
	mkdir -p "${dest}"
	cp "${REPO_ROOT}"/build/screenshots/visual/*.png "${dest}/"
	ls "${dest}" | sed 's/^/  /'
	echo "==> ${fw}: done (${dest})"
done
