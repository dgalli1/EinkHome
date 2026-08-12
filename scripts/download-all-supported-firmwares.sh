#!/bin/sh
#
# download-all-supported-firmwares.sh — fetch every firmware zip listed in
# resources/supported-firmwares.tsv into ./firmwares/ (or
# $EINKHOME_FIRMWARES_DIR / --firmwares-dir).
#
# Existing files are skipped; a zip that fails `unzip -t` is re-downloaded.
# Use --force to re-download everything regardless.
#
# Usage:
#   scripts/download-all-supported-firmwares.sh
#   scripts/download-all-supported-firmwares.sh --force
#   scripts/download-all-supported-firmwares.sh --firmwares-dir /mnt/bigdisk/fw
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
MANIFEST="${REPO_ROOT}/resources/supported-firmwares.tsv"
FIRMWARES_DIR="${EINKHOME_FIRMWARES_DIR:-${REPO_ROOT}/firmwares}"
FORCE=0

while [ "$#" -gt 0 ]; do
	case "$1" in
	--force)
		FORCE=1
		shift
		;;
	--firmwares-dir)
		FIRMWARES_DIR="${2:?--firmwares-dir requires a path}"
		shift 2
		;;
	-h | --help)
		sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "ERROR: unknown argument: ${1}" >&2
		exit 1
		;;
	esac
done

[ -f "${MANIFEST}" ] || {
	echo "ERROR: manifest missing: ${MANIFEST}" >&2
	exit 1
}
mkdir -p "${FIRMWARES_DIR}"

# Columns: device \t firmware_name \t zip \t url
_TOTAL=$(awk -F'\t' 'NR > 1 && NF >= 4 { n++ } END { print n+0 }' "${MANIFEST}")
_i=0
_fail=0
while IFS=$'\t' read -r _device _name _zip _url; do
	[ -n "${_zip}" ] || continue
	_i=$((_i + 1))
	_out="${FIRMWARES_DIR}/${_zip}"
	printf '[%s/%s] %s (%s)\n' "${_i}" "${_TOTAL}" "${_device}" "${_zip}"

	if [ "${FORCE}" -eq 0 ] && [ -f "${_out}" ] &&
		unzip -t "${_out}" >/dev/null 2>&1; then
		printf '  ok (already present, %s bytes)\n' "$(wc -c <"${_out}")"
		continue
	fi
	if [ -f "${_out}" ]; then
		printf '  stale/corrupt, re-downloading\n'
		rm -f "${_out}"
	fi

	if ! curl -fL --retry 3 --retry-delay 5 -o "${_out}" "${_url}"; then
		echo "  FAIL: download failed: ${_url}" >&2
		rm -f "${_out}"
		_fail=$((_fail + 1))
		continue
	fi
	if ! unzip -t "${_out}" >/dev/null 2>&1; then
		echo "  FAIL: zip integrity check failed: ${_out}" >&2
		rm -f "${_out}"
		_fail=$((_fail + 1))
		continue
	fi
	printf '  ok (%s bytes)\n' "$(wc -c <"${_out}")"
done < <(awk -F'\t' 'NR > 1 && NF >= 4 { print }' "${MANIFEST}")

echo
echo "=== summary ==="
du -sh "${FIRMWARES_DIR}" 2>/dev/null || true
if [ "${_fail}" -gt 0 ]; then
	echo "${_fail} firmware(s) failed; re-run with --force to retry all"
	exit 1
fi
echo "all ${_TOTAL} firmwares present in ${FIRMWARES_DIR}"
