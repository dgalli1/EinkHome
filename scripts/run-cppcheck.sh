#!/usr/bin/env bash
#
# run-cppcheck.sh — static-analyse the app C sources with cppcheck.
#
# Runs inside the pbdev container (which has the toolchain but not
# cppcheck; we apt-install it into a --rm container so nothing is
# persisted).  Falls back to the host binary when present.  cJSON.c is
# vendored third-party code and is excluded from the analysis.
#
# Exit: non-zero when cppcheck reports errors or warnings (the CI gate).
# Usage: scripts/run-cppcheck.sh [cppcheck args...]
set -eu

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)

# App source dirs (repo-relative) to scan; excludes vendored cJSON.c.
SRC_PATHS="app/core app/data app/ui app/action app/platform"

# Check warnings/performance/portability (real defects); style is noise
# at the gate (shadow/variableScope/knownCondition are idiomatic here).
# Inline suppressions (--inline-suppr) silence the handful of
# verified-FP warnings without disabling whole classifiers.
CPPCHECK_ARGS=(--enable=warning,performance,portability --language=c
	--inline-suppr --error-exitcode=2)

if command -v cppcheck >/dev/null 2>&1; then
	# Host cppcheck: repo-relative paths resolve from $ROOT via include dirs.
	INCS=""
	for d in ${SRC_PATHS}; do
		INCS="${INCS} -I${ROOT}/${d}"
	done
	SRCS=""
	for d in ${SRC_PATHS}; do
		SRCS="${SRCS} ${ROOT}/${d}/*.c"
	done
	# shellcheck disable=SC2086
	cppcheck "${CPPCHECK_ARGS[@]}" "$@" ${INCS} ${SRCS}
	exit $?
fi

if command -v podman >/dev/null 2>&1; then
	# Disposable pbdev container.  A small runner script is mounted so the
	# `-c` string stays simple (no nested quoting across the podman exec).
	INCS=""
	for d in ${SRC_PATHS}; do
		INCS="${INCS} -I/ws/${d}"
	done
	SRCS=""
	for d in ${SRC_PATHS}; do
		SRCS="${SRCS} /ws/${d}/*.c"
	done
	RUNNER=$(mktemp)
	cat > "${RUNNER}" <<EOF
apt-get update -q >/dev/null 2>&1
apt-get install -y -q cppcheck >/dev/null 2>&1
cppcheck ${CPPCHECK_ARGS[*]} $* ${INCS} ${SRCS}
EOF
	chmod +x "${RUNNER}"
	podman run --rm -v "${ROOT}:/ws:z" -v "${RUNNER}:/run-cppcheck.sh:z" \
		-w /ws --entrypoint bash localhost/pbdev:latest /run-cppcheck.sh
	rc=$?
	rm -f "${RUNNER}"
	exit ${rc}
fi

echo "error: neither cppcheck nor podman available" >&2
exit 2