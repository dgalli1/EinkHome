#!/bin/sh
#
# setup.sh — one-time bootstrap for a fresh EinkHome checkout.
#
#   1. Initialise the pbemu submodule (emulator, firmware staging,
#      SDK bootstrap, test support).
#   2. Create the pbemu venv (pytest + editable pbemu install).
#   3. Build the pbdev container image if missing (cross-compiler).
#   4. Download + stage the PocketBook firmware (U633_6.8.2817).
#   5. Fetch the PocketBook SDK headers/libraries.
#   6. Build the emulator support artifacts (shim, informer, viewer,
#      probes) inside the submodule.
#
# Afterwards run ./scripts/run-visible.sh to build the app and start
# the Wayland viewer.
#
# The firmware zip is cached in /tmp; delete it to force a re-download.
# All steps are idempotent — re-running is safe.

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
FIRMWARE="U633_6.8.2817"
FIRMWARE_URL="https://download.pocketbook-int.com/fw/Yitoa/633/user/20231228/sw_20231228_U633_6.8.2817_user.zip"
FW_ZIP="/tmp/$(basename "${FIRMWARE_URL}")"

PBEMU_RUN() {
	cd "${PBEMU_DIR}"
	# The venv has an editable pbemu install; PYTHONPATH pins the
	# submodule's tools in front of it.
	PYTHONPATH="${PBEMU_DIR}/tools" .venv/bin/python -m pbemu "$@"
}

echo "==> 1/6  initialising pbemu submodule"
git -C "${REPO_ROOT}" submodule update --init --recursive

echo "==> 2/6  pbemu venv"
if [ ! -x "${PBEMU_DIR}/.venv/bin/python" ]; then
	"${PBEMU_DIR}/setup-venv.sh"
fi

echo "==> 3/6  pbdev container image"
if ! podman image exists localhost/pbdev:latest 2>/dev/null; then
	PBEMU_RUN image
fi

echo "==> 4/6  firmware ${FIRMWARE}"
if [ ! -f "${FW_ZIP}" ]; then
	echo "    downloading ${FIRMWARE_URL}"
	curl -fL --max-time 1800 -o "${FW_ZIP}" "${FIRMWARE_URL}"
fi
echo "    zip: $(wc -c <"${FW_ZIP}") bytes"
if [ ! -d "${PBEMU_DIR}/${FIRMWARE}" ]; then
	PBEMU_RUN install "${FW_ZIP}"
else
	echo "    already staged at ${PBEMU_DIR}/${FIRMWARE}"
fi

echo "==> 5/6  PocketBook SDK"
if [ ! -f "${PBEMU_DIR}/sdk/pocketbook-sdk-b288/include/inkview.h" ]; then
	(cd "${PBEMU_DIR}" && sh sdk/install-sdk.sh)
else
	echo "    already installed"
fi

echo "==> 6/6  emulator support artifacts (shim, informer, viewer, probes)"
if [ ! -f "${PBEMU_DIR}/src/shim/build-arm/libshim.so" ]; then
	PBEMU_RUN build
else
	echo "    already built"
fi

echo
echo "Setup complete. Next:"
echo "    ./scripts/run-visible.sh   # builds the app and starts the Wayland viewer"
