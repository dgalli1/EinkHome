"""Visual capture: screenshot every UI page for manual layout review.

Run per firmware with the same env as the e2e suite:

    PB_TEST_FIRMWARE=U627_6.5.2898 \
    PBEMU_MOCK_BOOKS_DIR=U627_6.5.2898/.live/mnt/ext1/books \
    pbemu/.venv/bin/python -m pytest tests/test_visual_capture.py -q

PNGs land in build/screenshots/visual/ (the SnapshotRecorder's per-test
dir); scripts/capture-visual.sh copies them to tmp/screenshots/<fw>/.
"""

import time

import pytest

# The module fixture (same dir — pytest inserts tests/ on sys.path) does
# the full environment bring-up: build (no-op when build/bookshelf.app is
# fresh), API server, binary staging, emulator boot, geometry.
import test_bookshelf
from tests.support.bookshelf.geometry import MORE_SETTINGS

bookshelf_env = test_bookshelf.bookshelf_env  # noqa: F811  (fixture reuse)


def _settle(bs: object, seconds: float = 1.2) -> None:
    """Let redraws/covers land before the capture."""
    time.sleep(seconds)


def test_visual_capture(bookshelf_env):
    bs, _emulator = bookshelf_env
    bs.begin_snapshots("visual")
    try:
        # Let the initial sync + cover downloads settle.
        _settle(bs, 6.0)
        bs.snapshot("01-shelf")

        # Second grid page: pager + grid on a non-first page.
        bs.tap_pager_next()
        _settle(bs)
        bs.snapshot("02-shelf-page2")
        bs.tap_pager_prev()
        _settle(bs, 0.8)

        # Search sub-page (back arrow + input row + history).
        bs.tap_search()
        _settle(bs)
        bs.snapshot("03-search")
        bs.tap_home()  # top-left back arrow pops the search page
        _settle(bs, 0.8)

        # More overlay (menu panel right-anchored).
        bs.tap_menu_and_verify()
        _settle(bs, 0.6)
        bs.snapshot("04-more")

        # Settings page (header back icon + rows) — the Settings row
        # closes the overlay for us.
        bs.tap_more_item(MORE_SETTINGS)
        _settle(bs, 0.8)
        bs.snapshot("05-settings")

        # Full-screen log viewer.
        bs.tap_settings_logs()
        _settle(bs, 1.5)
        bs.snapshot("06-logviewer")
        bs.tap_log_back()
        _settle(bs, 0.8)

        # App launcher (header + app grid).
        bs.open_launcher()
        _settle(bs, 1.0)
        bs.snapshot("07-launcher")
    finally:
        bs.finish_snapshots()
