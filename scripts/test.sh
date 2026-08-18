#!/usr/bin/env bash
#
# test.sh — run the EinkHome test suites.
#
#   scripts/test.sh                # api unit tests + the emulator e2e suite
#   scripts/test.sh --api-only     # api unit tests only (no podman/firmware)
#   scripts/test.sh --pbemu        # ... plus the pbemu submodule's own suite
#   scripts/test.sh -- -k offline  # pass pytest args through to the e2e suite
#
# Run the e2e suite against the native PC (SDL) build, parallelised (needs
# pytest-xdist in the venv):
#   EH_TEST_BACKEND=sdl scripts/test.sh -- -n auto
#
# The SDL suite also carries the offline (no-internet) tests
# (tests/test_offline_sdl.py): the mock API is the app's only network, and
# the app is launched with EH_OFFLINE=1 so the SDL build reports no
# connection — the library must stay navigable, an already-downloaded book
# must open, and sort/group/search must keep working from the cached store.
#
# Requirements: the pbemu submodule venv (cd pbemu && ./setup-venv.sh),
# podman, the staged firmware (pbemu/pbemu install) and staged books in
# pbemu/U633_6.8.2817/.live/mnt/ext1/books/.
#
# Exit status: 0 when every selected suite passed, 1 otherwise.

set -eu
set -o pipefail

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)
PBEMU_DIR="${REPO_ROOT}/pbemu"
PY="${PBEMU_DIR}/.venv/bin/python"
FIRMWARE_DIR="${PBEMU_DIR}/U633_6.8.2817"
BOOKS_DIR="${FIRMWARE_DIR}/.live/mnt/ext1/books"

RUN_API=1
RUN_E2E=1
RUN_PBEMU=0
RUN_API_ONLY=0
args=()

while [ "$#" -gt 0 ]; do
	case "$1" in
	--api-only) RUN_API_ONLY=1; RUN_E2E=0; RUN_PBEMU=0; shift ;;
	--pbemu) RUN_PBEMU=1; shift ;;
	--) shift; args=("$@"); break ;;
	--help|-h)
		sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
	esac
done

die() {
	echo "ERROR: $*" >&2
	exit 1
}

if [ ! -x "${PY}" ]; then
	die "venv missing at ${PY} — run: (cd pbemu && ./setup-venv.sh)"
fi
if ! "${PY}" -m pytest --version >/dev/null 2>&1; then
	die "pytest not installed in the venv — run: (cd pbemu && ./setup-venv.sh)"
fi

# The emulator backend needs podman + a staged firmware + books; the SDL
# backend (EH_TEST_BACKEND=sdl) runs the native PC build and needs none
# of those, so its prechecks are relaxed.
BACKEND="${EH_TEST_BACKEND:-emulator}"

if [ "${RUN_PBEMU}" = 1 ]; then
	command -v podman >/dev/null 2>&1 || die "podman not found"
fi
if [ "${RUN_E2E}" = 1 ] && [ "${BACKEND}" != "sdl" ]; then
	command -v podman >/dev/null 2>&1 || die "podman not found"
	[ -d "${FIRMWARE_DIR}" ] || die "firmware not staged — run: pbemu/pbemu install"
	[ -d "${BOOKS_DIR}" ] && [ -n "$(ls -A "${BOOKS_DIR}" 2>/dev/null)" ] \
		|| die "no books staged in ${BOOKS_DIR} — the e2e suite needs a populated library"
fi

rc=0

if [ "${RUN_API}" = 1 ]; then
	echo "==> api unit tests (api/tests)"
	# Forwarded pytest args apply uniformly: in the default run they reach
	# both the api and e2e suites; in --api-only mode they reach the api
	# suite alone.
	(cd "${REPO_ROOT}" && "${PY}" -m pytest api/tests -q "${args[@]}") || rc=1
fi

if [ "${RUN_E2E}" = 1 ]; then
	echo "==> EinkHome e2e suite (tests/)"
	(cd "${REPO_ROOT}" && "${PY}" -m pytest tests/ -q "${args[@]}") || rc=1
fi

if [ "${RUN_PBEMU}" = 1 ]; then
	echo "==> pbemu submodule suite (pbemu/tests)"
	(cd "${PBEMU_DIR}" && ./.venv/bin/python -m pytest tests/ -q) || rc=1
fi

if [ "${rc}" -eq 0 ]; then
	echo "All selected suites passed."
else
	echo "One or more suites failed." >&2
fi
exit "${rc}"
