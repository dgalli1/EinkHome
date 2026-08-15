"""BookshelfSession: high-level e2e interaction and verification helpers.

BookshelfSession is the app-agnostic-to-app-specific bridge: it takes a
*backend* (see backends.py) — emulator, SDL-headless, or real-device —
and exposes the high-level gestures the interactive tests rely on (tap a
button, long-press, type, wait for a framebuffer change, read the log).

The public method surface is unchanged from the emulator-only version;
only the plumbing behind it is backend-agnostic now.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path
from typing import TYPE_CHECKING

from tests.support.bookshelf.backends import Backend
from tests.support.bookshelf.geometry import MORE_APPS, MORE_DOWNLOAD_ALL, MORE_SETTINGS
from tests.support.bookshelf.snapshots import SnapshotRecorder
from tests.support.runtime_common import REPO_ROOT

if TYPE_CHECKING:
    from tests.support.bookshelf.geometry import BookshelfGeometry

# The guest's log_open() derives the log path from argv0.  We back it with
# the backend's log reader; see backends.py for per-target locations.
_LOG_OPEN_MARKER = "--- bookshelf.app log opened"


# Backward-compatible module-level log helpers: the tests / env called
# read_bookshelf_log(firmware).  The modern path is BookshelfSession
# methods (which go through the backend); these shims keep any stragglers
# importing cleanly (they require an emulator backend's host .live path).
def read_bookshelf_log(firmware: str) -> str:
    from tests.support.bookshelf.backends import _EmulatorLog

    return _EmulatorLog(firmware).read()


def latest_invocation_log(firmware: str) -> str:
    text = read_bookshelf_log(firmware)
    idx = text.rfind(_LOG_OPEN_MARKER)
    return text[idx:] if idx != -1 else text


def count_log_openings(firmware: str) -> int:
    return read_bookshelf_log(firmware).count(_LOG_OPEN_MARKER)


class BookshelfSession:
    """High-level e2e interaction and verification helpers for bookshelf."""

    def __init__(
        self, backend: Backend, geom: BookshelfGeometry, firmware: str = ""
    ) -> None:
        self._backend = backend
        self._g = geom
        self._firmware = firmware
        # Screenshots land under <repo>/build/screenshots/<test>/.
        self._snapshots = SnapshotRecorder(REPO_ROOT.parent / "build" / "screenshots")
        self._fb_depth: int | None = None

    # -- snapshot recording ----------------------------------------------

    def begin_snapshots(self, test_name: str) -> None:
        """Start a fresh screenshot sequence for one test."""
        self._snapshots.begin(test_name)

    def finish_snapshots(self) -> None:
        """Write the per-test screenshot index."""
        self._snapshots.write_index()

    def snapshot(self, label: str) -> Path | None:
        """Capture the current framebuffer as a PNG (best-effort)."""
        rec = self._snapshots
        if not rec.active:
            return None
        name = rec.peek_name(label)
        raw = self._backend.frame_ppm(name)
        if raw is None or len(raw) == 0:
            return None
        return rec.finish_capture(name, label, raw)

    def _caller_label(self, default: str) -> str:
        """Name of the outermost BookshelfSession method that invoked us."""
        name: str | None = None
        frame = sys._getframe(2)
        while frame is not None:
            caller_self = frame.f_locals.get("self")
            if not isinstance(caller_self, BookshelfSession):
                break
            name = frame.f_code.co_name
            frame = frame.f_back
        return name or default

    @property
    def backend(self) -> Backend:
        return self._backend

    # Backward-compat aliases: tests/helpers reached the emulator via
    # .emulator; keep a property that raises if this backend has none.
    @property
    def emulator(self):
        emu = getattr(self._backend, "emulator", None)
        if emu is None:
            raise AttributeError(
                "this backend has no emulator (backend="
                f"{type(self._backend).__name__})"
            )
        return emu

    @property
    def geom(self) -> BookshelfGeometry:
        return self._g

    # -- low-level helpers ------------------------------------------------

    def frame_hash(self) -> str:
        """Return the current framebuffer content hash."""
        return self._backend.frame_hash()

    def wait_hash_change(self, before: str, *, timeout: float = 8.0) -> str:
        """Poll until the framebuffer hash differs from *before*."""
        return self._backend.wait_frame_change(before, timeout=timeout)

    # -- tap helpers ------------------------------------------------------

    def tap_at(self, x: int, y: int) -> None:
        """Send a tap at framebuffer coordinates (x, y)."""
        self._backend.tap(x, y)
        time.sleep(0.3)  # let the app draw before a screenshot
        self.snapshot(self._caller_label("tap"))

    def tap_home(self) -> None:
        self.tap_at(*self._g.home_button_center())

    def tap_menu(self) -> None:
        self.tap_at(*self._g.menu_button_center())

    def tap_search(self) -> None:
        self.tap_at(*self._g.search_icon_center())

    def tap_search_input(self) -> None:
        self.tap_at(*self._g.search_input_center())

    def tap_history_term(self, index: int) -> None:
        self.tap_at(*self._g.search_history_center(index))

    def tap_book(self, index: int) -> None:
        self.tap_at(*self._g.book_tile_center(index))

    def tap_pager_next(self) -> None:
        self.tap_at(*self._g.pager_next_center())

    def tap_pager_prev(self) -> None:
        self.tap_at(*self._g.pager_prev_center())

    def tap_pager_first(self) -> None:
        self.tap_at(*self._g.pager_first_center())

    def tap_pager_last(self) -> None:
        self.tap_at(*self._g.pager_last_center())

    def long_press_at(self, x: int, y: int, *, hold: float = 0.9) -> None:
        self._backend.down(x, y)
        time.sleep(hold)
        self._backend.up(x, y)
        time.sleep(0.3)
        self.snapshot(self._caller_label("long_press"))

    def long_press_book(self, index: int, *, hold: float = 0.9) -> None:
        self.long_press_at(*self._g.book_tile_center(index), hold=hold)

    def tap_sync_button(self) -> None:
        self.tap_at(*self._g.sync_button_center())

    def tap_context_item(self, item: int, n_items: int | None = None) -> None:
        if n_items is None:
            n_items = 3  # book menus default to three rows
        self.tap_at(*self._g.context_item_center(item, n_items=n_items))

    def tap_download_all(self) -> None:
        self.tap_menu()
        self.tap_at(*self._g.more_item_center(MORE_DOWNLOAD_ALL))

    def tap_more_item(self, item_index: int) -> None:
        self.tap_at(*self._g.more_item_center(item_index))

    def tap_outside_more(self) -> None:
        self.tap_at(*self._g.outside_more_overlay())

    def send_back_key(self) -> None:
        self._backend.key(0x1B)  # IV_KEY_BACK
        time.sleep(0.3)
        self.snapshot("back_key")

    def type_text(self, text: str, *, commit: bool = True) -> None:
        """Type *text* into the open keyboard, then commit (or not)."""
        self._backend.type_text(text)
        if commit:
            self.wait_for_stable(timeout=8.0)
            self.tap_at(*self._g.keyboard_return_center())
        else:
            time.sleep(0.3)
            self.snapshot(f"typed_{text}")

    # -- settings helpers -------------------------------------------------

    def open_settings(self) -> None:
        self.tap_menu()
        self.tap_at(*self._g.more_item_center(MORE_SETTINGS))

    def tap_settings_row(self, row: int) -> None:
        self.tap_at(*self._g.settings_row_center(row))

    def tap_settings_save(self) -> None:
        self.tap_at(*self._g.settings_save_center())

    def tap_settings_back(self) -> None:
        self.tap_at(*self._g.settings_back_center())

    def tap_settings_logs(self) -> None:
        self.tap_at(*self._g.settings_logs_center())

    def tap_log_back(self) -> None:
        self.tap_at(*self._g.log_back_center())

    # -- launcher helpers ------------------------------------------------

    def open_launcher(self) -> None:
        self.tap_menu_and_verify()
        self.tap_at(*self._g.more_item_center(MORE_APPS))

    def tap_launcher_back(self) -> None:
        self.tap_at(*self._g.launcher_back_center())

    def tap_launcher_app(self, index: int = 0) -> None:
        self.tap_at(*self._g.launcher_app_center(index))

    def scroll_launcher_down(self) -> None:
        cx, cy = self._g.launcher_body_center()
        self._backend.down(cx, cy)
        for step in range(1, 6):
            self._backend.move(cx, cy - step * 40)
        self._backend.up(cx, cy - 200)
        time.sleep(0.3)
        self.snapshot("launcher_scroll")

    # -- tap + verify helpers ---------------------------------------------

    def tap_and_verify_change(self, x: int, y: int, *, timeout: float = 8.0) -> str:
        before = self.frame_hash()
        self.tap_at(x, y)
        return self.wait_hash_change(before, timeout=timeout)

    def tap_home_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.home_button_center(), timeout=timeout
        )

    def tap_menu_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.menu_button_center(), timeout=timeout
        )

    def tap_search_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.search_icon_center(), timeout=timeout
        )

    def tap_search_input_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.search_input_center(), timeout=timeout
        )

    def tap_book_and_verify(self, index: int, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.book_tile_center(index), timeout=timeout
        )

    def tap_pager_next_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.pager_next_center(), timeout=timeout
        )

    def tap_pager_prev_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.pager_prev_center(), timeout=timeout
        )

    def tap_more_item_and_verify(
        self, item_index: int, *, timeout: float = 8.0
    ) -> str:
        return self.tap_and_verify_change(
            *self._g.more_item_center(item_index), timeout=timeout
        )

    def tap_outside_and_verify(self, *, timeout: float = 8.0) -> str:
        return self.tap_and_verify_change(
            *self._g.outside_more_overlay(), timeout=timeout
        )

    def send_back_and_verify(self, *, timeout: float = 8.0) -> str:
        before = self.frame_hash()
        self.send_back_key()
        return self.wait_hash_change(before, timeout=timeout)

    # -- log helpers ------------------------------------------------------

    def bookshelf_log(self) -> str:
        return self._backend.log()

    def current_log(self) -> str:
        return self._backend.current_log()

    def invocation_count(self) -> int:
        return self._backend.invocation_count()

    def wait_for_respawn(
        self,
        before: int,
        *,
        ready_marker: str = "EVT_INIT",
        timeout: float = 15.0,
    ) -> int:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.invocation_count() > before:
                if ready_marker in self.current_log():
                    self.snapshot("respawned")
                    return self.invocation_count()
            time.sleep(0.3)
        raise TimeoutError(
            f"bookshelf did not respawn+init within {timeout}s "
            f"(invocation count was {self.invocation_count()}, expected > {before})"
        )

    def assert_log_contains(self, needle: str) -> None:
        log = self.current_log()
        assert needle in log, (
            f"bookshelf log does not contain {needle!r}\n"
            f"--- current invocation tail ---\n{log[-2000:]}"
        )

    def assert_no_crash(self) -> None:
        log = self.current_log()
        crash_markers = ("Segmentation fault", "SIGSEGV", "SIGABRT", "core dumped")
        for marker in crash_markers:
            assert marker not in log, (
                f"bookshelf log contains crash marker {marker!r}\n"
                f"--- current invocation tail ---\n{log[-2000:]}"
            )

    # -- state helpers ----------------------------------------------------

    def wait_for_stable(self, *, timeout: float = 5.0) -> str:
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
                self.snapshot("stable")
                return h
        return h