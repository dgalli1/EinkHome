#!/bin/sh
#
# test-all-firmwares.sh — build EinkHome once against the oldest 6.x
# firmware, then run the e2e suite against every supported firmware.
#
# Rationale: build/bookshelf.app is linked against the oldest firmware's
# rootfs, so its glibc requirements are the floor every newer firmware
# satisfies — the same binary is staged on all devices (see the
# KOReader-style single-binary approach in docs).
#
# Firmware zips are NOT downloaded by this script — fetch them manually
# (the manifest lists the exact URL for each device) and drop them into
# ./firmwares/ (override: --firmwares-dir or $EINKHOME_FIRMWARES_DIR).
#
# Per device the script does what the CI e2e job does: stage the zip,
# build the emulator support artifacts, boot once to create the live
# tree, stage the mock books, then run the suite (the suite fixture
# stages build/bookshelf.app into the guest itself).
#
# Usage:
#   scripts/test-all-firmwares.sh                          # full sweep
#   scripts/test-all-firmwares.sh --fail-fast              # stop on 1st failure
#   scripts/test-all-firmwares.sh --device U627_6.5.2898   # one firmware
#   scripts/test-all-firmwares.sh --skip-build             # reuse build/bookshelf.app
#   scripts/test-all-firmwares.sh -- -k settings_back      # pytest args after --
#
# A full sweep runs the bookshelf suite per device (~7 min each,
# ~2.5 h for all 22).  Per-device logs land in build/fwtest/<fw>.log;
# a summary is printed at the end.  Exit code 1 if any device failed.
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
MANIFEST="${REPO_ROOT}/resources/supported-firmwares.tsv"
FIRMWARES_DIR="${EINKHOME_FIRMWARES_DIR:-${REPO_ROOT}/firmwares}"
TESTS="tests/test_bookshelf.py"
FAIL_FAST=0
SKIP_BUILD=0
DEVICE_FILTER=""

# Stop the emulator on any exit path so a failed device cannot leave a
# container behind for the next one.
cleanup() {
	"${PBEMU_DIR}/pbemu" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

while [ "$#" -gt 0 ]; do
	case "$1" in
	--fail-fast)
		FAIL_FAST=1
		shift
		;;
	--skip-build)
		SKIP_BUILD=1
		shift
		;;
	--device)
		DEVICE_FILTER="${2:?--device requires a firmware name}"
		shift 2
		;;
	--firmwares-dir)
		FIRMWARES_DIR="${2:?--firmwares-dir requires a path}"
		shift 2
		;;
	--tests)
		TESTS="${2:?--tests requires a path}"
		shift 2
		;;
	--)
		# Everything after -- is passed to pytest verbatim (grouping
		# preserved: "$@" below still holds the original quoted args).
		shift
		break
		;;
	-h | --help)
		sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	--*)
		echo "ERROR: unknown argument: ${1}" >&2
		exit 1
		;;
	*)
		echo "ERROR: unexpected argument: ${1} (pytest args go after --)" >&2
		exit 1
		;;
	esac
done

[ -f "${MANIFEST}" ] || {
	echo "ERROR: manifest missing: ${MANIFEST}" >&2
	exit 1
}

# --- preflight ---------------------------------------------------------

if ! command -v podman >/dev/null 2>&1 || ! podman info >/dev/null 2>&1; then
	echo "ERROR: podman is required (see scripts/setup.sh)" >&2
	exit 1
fi
if [ ! -x "${PBEMU_DIR}/.venv/bin/python" ]; then
	echo "ERROR: pbemu venv missing; run scripts/setup.sh first" >&2
	exit 1
fi
if [ ! -f "${REPO_ROOT}/sdk/pocketbook-sdk-b288/include/inkview.h" ]; then
	echo "ERROR: PocketBook SDK missing; run scripts/setup.sh first" >&2
	exit 1
fi
if ! podman image exists localhost/pbdev:latest 2>/dev/null; then
	echo "==> building pbdev container image"
	"${PBEMU_DIR}/pbemu" image
fi

# --- manifest ----------------------------------------------------------
# columns: device \t firmware_name \t zip \t url

if [ -n "${DEVICE_FILTER}" ]; then
	if ! awk -F'\t' -v want="${DEVICE_FILTER}" '$2 == want { found=1 } END { exit !found }' \
		"${MANIFEST}"; then
		echo "ERROR: ${DEVICE_FILTER} not in ${MANIFEST}" >&2
		exit 1
	fi
fi

# Missing zips are a hard error up front: downloading is a manual step.
MISSING=$(
	awk -F'\t' -v dir="${FIRMWARES_DIR}" -v want="${DEVICE_FILTER}" 'NR > 1 &&
		/^#/ || NF < 4 { next }
		(want != "" && $2 != want) { next }
		{
			if (system("test -f " dir "/" $3) != 0)
				print $2 "\t" $4
		}' "${MANIFEST}"
)
if [ -n "${MISSING}" ]; then
	echo "ERROR: firmware zip(s) missing from ${FIRMWARES_DIR}/ — download manually:" >&2
	echo "${MISSING}" | sed 's/^/  /' >&2
	exit 1
fi

# --- build against the oldest 6.x --------------------------------------
# The firmware name encodes the version (U627_6.5.2898); sort -V picks
# the numerically oldest.
OLDEST_NAME=$(
	awk -F'\t' 'NR > 1 && NF >= 2 { v=$2; sub(/^[^_]+_/, "", v); print v, $2 }' \
		"${MANIFEST}" | sort -V | head -n 1 | awk '{print $2}'
)
if [ -z "${OLDEST_NAME}" ]; then
	echo "ERROR: manifest empty: ${MANIFEST}" >&2
	exit 1
fi

install_firmware() {
	_name=$1
	_zip=$(awk -F'\t' -v name="${_name}" '$2 == name { print $3; exit }' "${MANIFEST}")
	# "Staged" must mean a usable tree: dir + non-empty .live.
	if [ -d "${PBEMU_DIR}/${_name}/.live" ] && [ -n "$(ls -A "${PBEMU_DIR}/${_name}/.live" 2>/dev/null)" ]; then
		echo "  staged: ${_name} (reusing existing tree)"
		return 0
	fi
	_force=
	if [ -d "${PBEMU_DIR}/${_name}" ]; then
		_force="--force"
	fi
	echo "  staging ${_zip} -> ${_name}"
	"${PBEMU_DIR}/pbemu" install "${FIRMWARES_DIR}/${_zip}" \
		--output-dir "${PBEMU_DIR}/${_name}" ${_force}
}

if [ "${SKIP_BUILD}" -eq 1 ]; then
	echo "==> build: skipped (--skip-build), using build/bookshelf.app"
	[ -f "${REPO_ROOT}/build/bookshelf.app" ] || {
		echo "ERROR: build/bookshelf.app missing; drop --skip-build" >&2
		exit 1
	}
else
	echo "==> build: linking against the oldest 6.x firmware (${OLDEST_NAME})"
	install_firmware "${OLDEST_NAME}"
	(
		cd "${REPO_ROOT}"
		make PBEMU_FIRMWARE_DIR="${REPO_ROOT}/pbemu/${OLDEST_NAME}"
	)
	# Sanity: the binary's glibc requirements must be satisfiable by the
	# oldest rootfs — and therefore by every newer one.
	if command -v readelf >/dev/null 2>&1; then
		_tags=$(readelf --version-info "${REPO_ROOT}/build/bookshelf.app" 2>/dev/null |
			grep -o 'GLIBC_[0-9.]*' | sort -u | tr '\n' ' ')
		echo "  binary glibc requirements: ${_tags}"
	fi
fi

# --- sweep --------------------------------------------------------------

echo "==> sweep: ${TESTS}"
_total=$(awk -F'\t' 'NR > 1 && NF >= 2 { n++ } END { print n+0 }' "${MANIFEST}")
_i=0
_fail=0
_summary=""
for row in $(awk -F'\t' 'NR > 1 && NF >= 2 { print $2 }' "${MANIFEST}"); do
	if [ -n "${DEVICE_FILTER}" ] && [ "${row}" != "${DEVICE_FILTER}" ]; then
		continue
	fi
	_i=$((_i + 1))
	_device=$(awk -F'\t' -v name="${row}" '$2 == name { print $1 }' "${MANIFEST}")
	_logdir="${REPO_ROOT}/build/fwtest"
	mkdir -p "${_logdir}"
	_log="${_logdir}/${row}.log"

	echo
	echo "==> [${_i}/${_total}] ${_device} (${row})"

	install_firmware "${row}"

	echo "  building emulator support artifacts"
	"${PBEMU_DIR}/pbemu" build "${row}" >>"${_log}" 2>&1 || {
		echo "  FAIL: pbemu build (see ${_log})"
		_fail=$((_fail + 1))
		_summary="${_summary}\n  ${row}  FAIL (build)"
		[ "${FAIL_FAST}" -eq 1 ] && exit 1
		continue
	}

	# Boot once to create the live tree so the mock books can be staged
	# into it (the suite fixture boots the emulator again itself).
	if ! PBEMU_NO_KEEPID=1 \
		PBEMU_PODMAN_ARGS="--tmpfs /sys:rw,nodev,nosuid,mode=755,size=2m" \
		"${PBEMU_DIR}/pbemu" start "${row}" --no-viewer --no-audio >>"${_log}" 2>&1; then
		echo "  FAIL: emulator boot (see ${_log})"
		tail -5 "${PBEMU_DIR}/${row}/.live/var/log/system.log" 2>/dev/null | sed 's/^/    /' || true
		_fail=$((_fail + 1))
		_summary="${_summary}\n  ${row}  FAIL (boot)"
		[ "${FAIL_FAST}" -eq 1 ] && exit 1
		continue
	fi
	"${PBEMU_DIR}/pbemu" stop >>"${_log}" 2>&1 || true

	echo "  staging mock books"
	"${REPO_ROOT}/scripts/stage-mock-books.sh" "${PBEMU_DIR}/${row}" >>"${_log}" 2>&1

	echo "  running suite (log: ${_log})"
	set +e
	(
		cd "${REPO_ROOT}"
		PB_TEST_FIRMWARE="${row}" \
			PBEMU_MOCK_BOOKS_DIR="${row}/.live/mnt/ext1/books" \
			PBEMU_SYS_TMPFS=1 \
			"${PBEMU_DIR}/.venv/bin/python" -m pytest "${TESTS}" "$@" -q
	) >>"${_log}" 2>&1
	_rc=$?
	set -e
	"${PBEMU_DIR}/pbemu" stop >>"${_log}" 2>&1 || true

	if [ "${_rc}" -eq 0 ]; then
		echo "  PASS"
		_summary="${_summary}\n  ${row}  PASS"
	else
		echo "  FAIL: pytest exit ${_rc} (see ${_log})"
		tail -4 "${_log}" | sed 's/^/    /'
		_fail=$((_fail + 1))
		_summary="${_summary}\n  ${row}  FAIL (suite, exit ${_rc})"
		[ "${FAIL_FAST}" -eq 1 ] && exit 1
	fi
done

echo
echo "=== summary ==="
printf "%b\n" "${_summary}" | sed '/^ *$/d'
echo
if [ "${_fail}" -gt 0 ]; then
	echo "${_fail} device(s) failed; logs in build/fwtest/"
	exit 1
fi
echo "all ${_i} device(s) passed"
