#!/usr/bin/env sh
# Stage placeholder mock books for the bookshelf e2e suite.
#
# The e2e suite needs a populated library in the staged firmware's live
# /mnt tree (the mock API provider lists this dir and serves the files
# for download tests).  The book *content* is irrelevant — the reader is
# expected to fail opening them ("Document loading failed") and the
# tests assert on launch/log lines, not on rendered pages — so each
# book is a tiny stub file named like a real public-domain title.
#
# Usage:
#     scripts/stage-mock-books.sh <firmware-dir>   # e.g. pbemu/U633_6.8.2817
#
# The target dir is <firmware-dir>/.live/mnt/ext1/books, i.e. the host
# side of the guest's /mnt/ext1/books mount.  Run this AFTER the
# emulator has booted once (pbemu start/stop) so the .live tree exists;
# pbemu preserves it on later starts.  bgipc.epub is not created here —
# it ships in pbemu's baseline mnt tree and lands via the same staging.

set -eu

FIRMWARE_DIR="${1:?usage: scripts/stage-mock-books.sh <firmware-dir>}"
BOOKS_DIR="${FIRMWARE_DIR}/.live/mnt/ext1/books"

mkdir -p "${BOOKS_DIR}"

# Same set of titles the dev firmware tree carries, so the grid/pager
# page counts and sort orders match a local run.
for name in \
	Alice_in_Wonderland \
	Around_the_World_in_Eighty_Days \
	Dracula \
	Frankenstein \
	Grimm_Fairy_Tales \
	Jane_Eyre \
	Moby_Dick \
	Pride_and_Prejudice \
	The_Adventures_of_Tom_Sawyer \
	The_Jungle_Book \
	The_Time_Machine \
	The_War_of_the_Worlds \
	Treasure_Island \
	Twenty_Thousand_Leagues \
	Wuthering_Heights; do
	printf 'Mock book for e2e tests: %s\n' "${name}" > "${BOOKS_DIR}/${name}.epub"

	# A real (240x360, two-tone) cover PNG per title: the app refuses to
	# cache the API's 1x1 placeholder (validate_cover_bytes), so without
	# these the offline cover-cache assertions can never fire.  One
	# identical PNG for every title is enough — the suite asserts on
	# cache hits and log lines, not on rendered art.
	base64 -d > "${BOOKS_DIR}/${name}.png" <<'B64'
iVBORw0KGgoAAAANSUhEUgAAAPAAAAFoCAIAAADmdeFfAAACt0lEQVR42u3UQQ0AIAADsUHQ
gRKUzL8QPPAkrYTlsrFPA7+YJkDQIGgQNAgaQYOgQdAgaBA0ggZBg6BB0CBoBA2CBkGDoEHQ
CBoEDYIGQYOgETQIGgQNggZBI2gQNAgaBA2CRtAgaBA0CBpBg6BB0CBoEDSCBkGDoEHQIGgE
DYIGQYOgQdAIGgQNggZBg6ARNAgaBA2CBkEjaBA0CBoEDYJG0CBoEDQIGgSNoEHQIGgQNIIG
QYOgQdAgaAQNggZBg6BB0AgaBA2CBkGDoBE0CBoEDYIGQSNoEDQIGgQNgkbQIGgQNAgaBI2g
QdAgaBA0ggZBg6BB0CBoBA2CBkGDoEHQCBoEDYIGQYOgETQIGgQNggZBI2gQNAgaBA2CRtAg
aBA0CBoEjaBB0CBoEDQIGkGDoEHQIGgEDYIGQYOgQdAIGgQNggZBg6ARNAgaBA2CBkEjaBA0
CBoEDYJG0CBoEDQIGgSNoEHQIGh4t9paAQ8NggZBg6ARNAgaBA2CBkEjaBA0CBoEDYJG0CBo
EDQIGgSNoEHQIGgQNAgaQYOgQdAgaBA0ggZBg6BB0CBoBA2CBkGDoBE0CBoEDYIGQSNoEDQI
GgQNgkbQIGgQNAgaBI2gQdAgaBA0CBpBg6BB0CBoEDSCBkGDoEHQIGgEDYIGQYOgETQIGgQN
ggZBI2gQNAgaBA2CRtAgaBA0CBoEjaBB0CBoEDQIGkGDoEHQIGgQNIIGQYOgQdAgaAQNggZB
g6BB0AgaBA2CBkEjaBA0CBoEDYJG0CBoEDQIGgSNoEHQIGgQNAgaQYOgQdAgaBA0ggZBg6BB
0CBoBA2CBkGDoEHQCBoEDYIGQSNoEDQIGgQNgkbQIGgQNAgaBI2gQdAgaBA0CBpBg6BB0CBo
EDSCBkGDoEHQIGgEDYIGQYOgQdAIGgQNggZBQ3IB598ElDg9DcEAAAAASUVORK5CYII=
B64
done

echo "Staged $(find "${BOOKS_DIR}" -maxdepth 1 -name '*.epub' | wc -l) books in ${BOOKS_DIR}"

