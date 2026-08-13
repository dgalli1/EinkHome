#!/bin/sh
#
# capture-resolutions.sh — screenshot every UI page once per screen
# RESOLUTION (one representative device each) instead of per device.
#
# The five resolution classes of the supported device list, each with a
# representative firmware — a COLOR device when the class has one:
#   758x1024  U617_6.6.906     Basic Lux 3 (6", no color model exists)
#   825x1200  U970_6.8.4644    InkPad Lite (7", no color model exists)
#   1072x1448 U633_6.8.2817    Color Moon Silver (7.8", color)
#   1264x1680 U700k3_6.10.2359 Era Color (7", color)
#   1404x1872 U743k3_6.10.2854 InkPad Color 3 (7.8", color)
#
# Screenshots land in tmp/screenshots-resolutions/<WxH>/ (per-firmware
# captures stay in tmp/screenshots/).  Same prerequisites as
# capture-visual.sh: staged firmware trees + build/bookshelf.app.
#
# Usage:
#   scripts/capture-resolutions.sh              # all five classes
#   scripts/capture-resolutions.sh 758x1024    # just one class
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
OUT_DIR="${REPO_ROOT}/tmp/screenshots-resolutions"

# resolution<TAB>firmware — the class representatives (color when
# available, see header).
CLASSES="758x1024	U617_6.6.906
825x1200	U970_6.8.4644
1072x1448	U633_6.8.2817
1264x1680	U700k3_6.10.2359
1404x1872	U743k3_6.10.2854"

WANTED="${1:-}"

_fail=0
while IFS=$'\t' read -r res fw; do
	[ -n "${res}" ] || continue
	if [ -n "${WANTED}" ] && [ "${WANTED}" != "${res}" ]; then
		continue
	fi
	echo "==> ${res} (${fw})"
	if ! "${REPO_ROOT}/scripts/capture-visual.sh" "${fw}" >"/tmp/capres-${fw}.log" 2>&1; then
		echo "  FAIL: ${res} (see /tmp/capres-${fw}.log)" >&2
		_fail=$((_fail + 1))
		continue
	fi
	mkdir -p "${OUT_DIR}/${res}"
	cp "${REPO_ROOT}"/tmp/screenshots/"${fw}"/*.png "${OUT_DIR}/${res}/"
	echo "  -> ${OUT_DIR}/${res}/ ($(ls "${OUT_DIR}/${res}" | wc -l) pngs)"
done <<EOF
${CLASSES}
EOF

if [ "${_fail}" -gt 0 ]; then
	echo "${_fail} class(es) failed" >&2
	exit 1
fi
echo "done: ${OUT_DIR}"
