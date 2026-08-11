"""Click on the first book on the frontpage and capture page-10 screenshots.

This drives the pbemu emulator via the same test infrastructure used in
``tests/test_reader.py`` (ReaderSession / OpenFlow / turn_page). It does
not depend on the touch input path because that route has been observed
to fill the /hwevent queue and force bookshelf.app into a crash loop in
this sandbox.

Steps:
    1. Start the emulator (clean reset)
    2. Take a screenshot of the home screen (frontpage)
    3. Open the only staged book (bgipc.epub) via ReaderSession.open
    4. Take a screenshot of page 1
    5. Turn pages until we reach page 10
    6. Take a screenshot of page 10
    7. Print the saved screenshot paths and stop the emulator
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_PBEMU = _REPO_ROOT / "pbemu"
sys.path.insert(0, str(_PBEMU))
sys.path.insert(0, str(_PBEMU / "tools"))  # tests.support.* + the pbemu CLI live in the submodule

from tests.support.reader_flow import ReaderSession, Session  # noqa: E402
from tests.support.runtime import Emulator  # noqa: E402


FIRMWARE = "U633_6.8.2817"
SHOT_DIR = _REPO_ROOT / "screenshots"
SHOT_DIR.mkdir(exist_ok=True)


def save_screenshot(name: str) -> Path:
    """Capture a screenshot via the pbemu CLI and save it to disk."""
    out = SHOT_DIR / name
    subprocess.run(
        [sys.executable, "-m", "pbemu", "screenshot", "--out", str(out)],
        cwd=_REPO_ROOT,
        env={**os.environ, "PYTHONPATH": str(_PBEMU / "tools")},
        check=True,
    )
    return out


def main() -> int:
    emu = Emulator(firmware=FIRMWARE)
    emu.start()
    try:
        # 1. Wait for the emulator to be ready.
        emu.wait_for_monitor(timeout=120.0)
        emu.wait_for_hwevent(timeout=120.0)
        emu.wait_for_informer_snapshot(timeout=120.0)
        emu.wait_for_active_task_info(timeout=120.0)
        emu.wait_for_monitor_quiet(timeout=20.0, quiet_period=1.5)

        # 2. Screenshot of the frontpage (home screen).
        home_shot = save_screenshot("01-frontpage.png")
        print(f"frontpage screenshot saved: {home_shot}")

        # 3. Open the staged book via the reader session.
        rs = ReaderSession(Session(emu))
        state = rs.ensure_open_from_home(timeout=20.0)
        print(f"opened: {state.opened_book_path}")

        # 4. Screenshot of page 1.
        page1 = save_screenshot("02-book-page1.png")
        print(f"page 1 screenshot saved: {page1}")

        # 5. Turn to page 10.
        for page_idx in range(2, 11):
            turn = rs.turn_page(state, timeout=10.0)
            print(f"turned to page {page_idx}: hash changed={turn.after_hash != turn.before_hash}")
            time.sleep(0.5)

        # 6. Screenshot of page 10.
        page10 = save_screenshot("03-book-page10.png")
        print(f"page 10 screenshot saved: {page10}")
        return 0
    finally:
        emu.stop()


if __name__ == "__main__":
    raise SystemExit(main())