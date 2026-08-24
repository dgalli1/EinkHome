"""Baseline/after screenshots for the Slint conversion (SDL backend).

Run with:

    EH_TEST_BACKEND=sdl pbemu/.venv/bin/python -m pytest \
        tests/test_slint_baseline.py -q

PNGs land in build/screenshots/<EH_CAPTURE_DIR or 'slint-capture'>/.
Capture BEFORE the conversion and again AFTER; diff the two dirs.
"""

import os
import time

import test_bookshelf

from tests.support.bookshelf.geometry import MORE_SETTINGS

bookshelf_env = test_bookshelf.bookshelf_env  # noqa: F811  (fixture reuse)


def _settle(bs: object, seconds: float = 1.2) -> None:
    """Let redraws/covers land before the capture."""
    time.sleep(seconds)


def test_slint_baseline(bookshelf_env):
    bs, _runtime = bookshelf_env
    bs.begin_snapshots(os.environ.get("EH_CAPTURE_DIR", "slint-capture"))
    try:
        _settle(bs, 6.0)
        bs.snapshot("01-shelf")

        bs.tap_pager_next()
        _settle(bs)
        bs.snapshot("02-shelf-page2")
        bs.tap_pager_prev()
        _settle(bs, 0.8)

        bs.tap_search()
        _settle(bs)
        bs.snapshot("03-search")
        bs.type_text("no")
        _settle(bs)
        bs.snapshot("04-search-typed")
        bs.tap_home()
        _settle(bs, 0.8)

        bs.tap_menu_and_verify()
        _settle(bs, 0.6)
        bs.snapshot("05-more")

        bs.tap_more_item(MORE_SETTINGS)
        _settle(bs, 0.8)
        bs.snapshot("06-settings")

        bs.tap_settings_logs()
        _settle(bs, 1.5)
        bs.snapshot("07-logviewer")
        bs.tap_log_back()
        _settle(bs, 0.8)

        bs.open_launcher()
        _settle(bs, 1.0)
        bs.snapshot("08-launcher")
    finally:
        bs.finish_snapshots()
