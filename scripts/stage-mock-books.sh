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
done

echo "Staged $(find "${BOOKS_DIR}" -maxdepth 1 -name '*.epub' | wc -l) books in ${BOOKS_DIR}"
