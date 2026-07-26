"""BookshelfSession: high-level e2e interaction and verification helpers.

Wraps the generic Session + Emulator infrastructure with bookshelf-specific
tap targets (from geometry), framebuffer-hash-change detection, and
bookshelf log parsing.
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import TYPE_CHECKING

from tests.support.reader.session import Session
from tests.support.runtime_common import REPO_ROOT
from tests.support.ui_input import IV_KEY_BACK, press_key, tap
from tests.support.bookshelf.geometry import MORE_SETTINGS

if TYPE_CHECKING:
    from tests.support.bookshelf.geometry import BookshelfGeometry
    from tests.support.runtime import Emulator


# The guest's log_open() derives the log path from argv0, but that fopen()
# fails at runtime (the canonical /mnt/ext1/system/bin dir is not writable
# from the guest process), so it falls back to /tmp/bookshelf.log.  On the
# host /tmp maps to .live/tmp, while the argv0-derived path maps to
# .live/mnt/ext1/system/bin and only ever holds a stale leftover.  We therefore
# pick whichever candidate actually exists and is newest.
_LOG_OPEN_MARKER = "--- bookshelf.app log opened"


def _bookshelf_log_candidates(firmware: str) -> list[Path]:
    base = REPO_ROOT / firmware / ".live"
    return [
        base / "tmp" / "bookshelf.log",
        base / "mnt" / "ext1" / "system" / "bin" / "bookshelf.log",
    ]


def _bookshelf_log_path(firmware: str) -> Path:
    """Return the host path the guest is *actually* appending its log to."""
    existing = [p for p in _bookshelf_log_candidates(firmware) if p.exists()]
    if not existing:
        return _bookshelf_log_candidates(firmware)[1]
    return max(existing, key=lambda p: p.stat().st_mtime)


def read_bookshelf_log(firmware: str) -> str:
    """Return the full (accumulated) bookshelf log content."""
    path = _bookshelf_log_path(firmware)
    if not path.exists():
        return ""
    return path.read_text(encoding="utf-8", errors="replace")


def latest_invocation_log(firmware: str) -> str:
    """Return only the log slice from the most recent process launch.

    The log is opened append-only, so it accumulates across every
    kill/respawn cycle.  Per-test content assertions must look at the
    current invocation only, otherwise a string logged by an earlier test
    (or a stale file) yields false positives.
    """
    text = read_bookshelf_log(firmware)
    idx = text.rfind(_LOG_OPEN_MARKER)
    return text[idx:] if idx != -1 else text


def count_log_openings(firmware: str) -> int:
    """Count process launches recorded in the log (one per log_open)."""
    return read_bookshelf_log(firmware).count(_LOG_OPEN_MARKER)


class BookshelfSession:
    """High-level e2e interaction and verification helpers for bookshelf."""

    def __init__(
        self, session: Session, geom: BookshelfGeometry, firmware: str
    ) -> None:
        self._s = session
        self._g = geom
        self._firmware = firmware

    @property
    def session(self) -> Session:
        return self._s

    @property
    def emulator(self) -> Emulator:
        return self._s.emulator

    @property
    def geom(self) -> BookshelfGeometry:
        return self._g

    # -- low-level helpers ------------------------------------------------

    def frame_hash(self) -> str:
        """Return the current framebuffer content hash."""
        return self._s.framebuffer_hash()

    def wait_hash_change(self, before: str, *, timeout: float = 8.0) -> str:
        """Poll until the framebuffer hash differs from *before*."""
        return self._s.wait_for_framebuffer_change(before, timeout=timeout)

    # -- tap helpers ------------------------------------------------------

    def tap_at(self, x: int, y: int) -> None:
        """Send a tap at framebuffer coordinates (x, y)."""
        tap(self.emulator, x, y)

    def tap_home(self) -> None:
        """Tap the Home button (top-left)."""
        self.tap_at(*self._g.home_button_center())

    def tap_menu(self) -> None:
        """Tap the Menu button (top-right)."""
        self.tap_at(*self._g.menu_button_center())

    def tap_search(self) -> None:
        """Tap the search box."""
        self.tap_at(*self._g.search_box_center())

    def tap_book(self, index: int) -> None:
        """Tap book tile at grid *index* (0-based)."""
        self.tap_at(*self._g.book_tile_center(index))

    def tap_pager_next(self) -> None:
        """Tap the Next page button."""
        self.tap_at(*self._g.pager_next_center())

    def tap_pager_prev(self) -> None:
        """Tap the Prev page button."""
        self.tap_at(*self._g.pager_prev_center())

    def tap_more_item(self, item_index: int) -> None:
        """Tap item *item_index* in the More overlay (0-based)."""
        self.tap_at(*self._g.more_item_center(item_index))

    def tap_outside_more(self) -> None:
        """Tap outside the More overlay to dismiss it."""
        self.tap_at(*self._g.outside_more_overlay())

    def send_back_key(self) -> None:
        """Send the Back key event."""
        press_key(self.emulator, IV_KEY_BACK)

    # -- settings helpers -------------------------------------------------

    def open_settings(self) -> None:
        """Open the More overlay and tap the Settings item."""
        self.tap_menu()
        self.tap_at(*self._g.more_item_center(MORE_SETTINGS))

    def tap_settings_row(self, row: int) -> None:
        """Tap settings row *row* (0=API host, 1=API key, 2=reader)."""
        self.tap_at(*self._g.settings_row_center(row))

    def tap_settings_save(self) -> None:
        """Tap the Save & apply button."""
        self.tap_at(*self._g.settings_save_center())

    def tap_settings_back(self) -> None:
        """Tap the Back button."""
        self.tap_at(*self._g.settings_back_center())

    # -- tap + verify helpers ---------------------------------------------

    def tap_and_verify_change(
        self, x: int, y: int, *, timeout: float = 8.0
    ) -> str:
        """Tap (x, y) and verify the framebuffer changes. Returns new hash."""
        before = self.frame_hash()
        self.tap_at(x, y)
        return self.wait_hash_change(before, timeout=timeout)

    def tap_home_and_verify(self, *, timeout: float = 8.0) -> str:
        """Tap Home and verify framebuffer changes."""
        return self.tap_and_verify_change(
            *self._g.home_button_center(), timeout=timeout
        )

    def tap_menu_and_verify(self, *, timeout: float = 8.0) -> str:
        """Tap Menu and verify framebuffer changes (overlay appears)."""
        return self.tap_and_verify_change(
            *self._g.menu_button_center(), timeout=timeout
        )

    def tap_search_and_verify(self, *, timeout: float = 8.0) -> str:
        """Tap search box and verify framebuffer changes (keyboard)."""
        return self.tap_and_verify_change(
            *self._g.search_box_center(), timeout=timeout
        )

    def tap_book_and_verify(self, index: int, *, timeout: float = 8.0) -> str:
        """Tap book tile and verify framebuffer changes."""
        return self.tap_and_verify_change(
            *self._g.book_tile_center(index), timeout=timeout
        )

    def tap_pager_next_and_verify(self, *, timeout: float = 8.0) -> str:
        """Tap Next page and verify framebuffer changes."""
        return self.tap_and_verify_change(
            *self._g.pager_next_center(), timeout=timeout
        )

    def tap_pager_prev_and_verify(self, *, timeout: float = 8.0) -> str:
        """Tap Prev page and verify framebuffer changes."""
        return self.tap_and_verify_change(
            *self._g.pager_prev_center(), timeout=timeout
        )

    def tap_more_item_and_verify(
        self, item_index: int, *, timeout: float = 8.0
    ) -> str:
        """Tap More overlay item and verify framebuffer changes."""
        return self.tap_and_verify_change(
            *self._g.more_item_center(item_index), timeout=timeout
        )

    def tap_outside_and_verify(self, *, timeout: float = 8.0) -> str:
        """Tap outside overlay and verify framebuffer changes."""
        return self.tap_and_verify_change(
            *self._g.outside_more_overlay(), timeout=timeout
        )

    def send_back_and_verify(self, *, timeout: float = 8.0) -> str:
        """Send Back key and verify framebuffer changes."""
        before = self.frame_hash()
        self.send_back_key()
        return self.wait_hash_change(before, timeout=timeout)

    # -- log helpers ------------------------------------------------------

    def bookshelf_log(self) -> str:
        """Return the accumulated bookshelf log content."""
        return read_bookshelf_log(self._firmware)

    def current_log(self) -> str:
        """Return only the log slice from the current process launch."""
        return latest_invocation_log(self._firmware)

    def invocation_count(self) -> int:
        """Number of process launches recorded so far."""
        return count_log_openings(self._firmware)

    def wait_for_respawn(
        self,
        before: int,
        *,
        ready_marker: str = "EVT_INIT",
        timeout: float = 15.0,
    ) -> int:
        """Poll until a launch newer than *before* has finished init.

        Used after CloseApp(): bookshelf is the launcher replacement, so
        monitor.app respawns it; the new ``log_open`` header plus the
        *ready_marker* in that invocation's slice prove the close+respawn
        cycle completed and the new process is healthy.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.invocation_count() > before:
                if ready_marker in latest_invocation_log(self._firmware):
                    return self.invocation_count()
            time.sleep(0.3)
        raise TimeoutError(
            f"bookshelf did not respawn+init within {timeout}s "
            f"(invocation count was {self.invocation_count()}, expected > {before})"
        )

    def assert_log_contains(self, needle: str) -> None:
        """Assert the *current* invocation's log contains *needle*."""
        log = self.current_log()
        assert needle in log, (
            f"bookshelf log does not contain {needle!r}\n"
            f"--- current invocation tail ---\n{log[-2000:]}"
        )

    def assert_no_crash(self) -> None:
        """Assert the current invocation's log has no crash markers."""
        log = self.current_log()
        crash_markers = (
            "Segmentation fault",
            "SIGSEGV",
            "SIGABRT",
            "core dumped",
        )
        for marker in crash_markers:
            assert marker not in log, (
                f"bookshelf log contains crash marker {marker!r}\n"
                f"--- current invocation tail ---\n{log[-2000:]}"
            )

    # -- state helpers ----------------------------------------------------

    def wait_for_stable(self, *, timeout: float = 5.0) -> str:
        """Wait for the framebuffer to stabilise. Returns final hash."""
        h = self.frame_hash()
        deadline = time.monotonic() + timeout
        stable_since = time.monotonic()
        while time.monotonic() < deadline:
            time.sleep(0.3)
            new_h = self.frame_hash()
            if new_h != h:
                h = new_h
                stable_since = time.monotonic()
            elif time.monotonic() - stable_since >= 1.0:
                return h
        return h
