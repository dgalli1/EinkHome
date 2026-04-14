"""Public façade for reader-flow test helpers.

Re-exports the stable public API from the internal ``tests.support.reader``
package so that test modules and fixtures can use a single import path.
"""

from __future__ import annotations

import time

from tests.support.reader.models import (
    PageTurnResult,
    ReaderExitResult,
    ReaderMenuState,
    ReaderState,
)
from tests.support.reader.patterns import (
    BOOK_INFO_APP,
    BOOK_PATH,
    BOOK_TITLE,
    CONTROL_PANEL_APP,
    DEFAULT_CRASH_MARKERS,
    EXIT_CONTROL_APPS,
    EXPLORER_APP,
    HOME_APP,
    HOME_SURFACE_APPS,
    READER_APP,
    READER_DB_NAME,
)
from tests.support.reader.reader_session import ReaderSession
from tests.support.reader.session import Session
from tests.support.runtime import Emulator, app_name_matches

__all__ = [
    "BOOK_INFO_APP",
    "BOOK_PATH",
    "BOOK_TITLE",
    "CONTROL_PANEL_APP",
    "DEFAULT_CRASH_MARKERS",
    "EXIT_CONTROL_APPS",
    "EXPLORER_APP",
    "HOME_APP",
    "HOME_SURFACE_APPS",
    "PageTurnResult",
    "READER_APP",
    "READER_DB_NAME",
    "ReaderExitResult",
    "ReaderMenuState",
    "ReaderSession",
    "ReaderState",
    "Session",
    "app_name_matches",
    "assert_no_crash_markers",
    "book_is_staged",
    "ensure_reader_open_from_home",
    "exit_reader",
    "open_reader_menu",
    "read_new_monitor_log",
    "reader_binary_exists",
    "return_to_home_screen",
    "turn_reader_page",
]


def return_to_home_screen(emulator: Emulator, *, timeout: float = 12.0) -> None:
    """Best-effort navigation back to a home or control surface."""
    session = Session(emulator)
    rs = ReaderSession(session)
    rs.return_to_home(time.monotonic() + timeout)


def ensure_reader_open_from_home(
    emulator: Emulator, *, timeout: float = 15.0
) -> ReaderState:
    """Open the current recent document from home and wait for reader log markers."""
    return ReaderSession(Session(emulator)).ensure_open_from_home(timeout=timeout)


def open_reader_menu(
    reader_state: ReaderState, *, timeout: float = 10.0
) -> ReaderMenuState:
    """Invoke a reader control gesture and wait for menu/control log activity."""
    return ReaderSession(Session(reader_state.emulator)).open_menu(
        reader_state, timeout=timeout
    )


def turn_reader_page(
    reader_state: ReaderState, *, timeout: float = 10.0
) -> PageTurnResult:
    """Advance one page using stable input fallbacks and verify a hash change."""
    return ReaderSession(Session(reader_state.emulator)).turn_page(
        reader_state, timeout=timeout
    )


def exit_reader(
    reader_state: ReaderState, *, timeout: float = 12.0
) -> ReaderExitResult:
    """Try to leave the reader with the Home key and capture the resulting state."""
    return ReaderSession(Session(reader_state.emulator)).exit(timeout=timeout)


def read_new_monitor_log(reader_state: ReaderState) -> str:
    """Return monitor.log text produced since the reader flow began."""
    return reader_state.emulator.read_monitor_log_since(reader_state.log_offset)


def assert_no_crash_markers(
    log_text: str, *, extra_markers: tuple[str, ...] = ()
) -> None:
    """Assert that new monitor log output does not contain crash markers."""
    offenders = [
        marker
        for marker in DEFAULT_CRASH_MARKERS + extra_markers
        if marker in log_text
    ]
    assert not offenders, (
        f"monitor.log contains failure markers {offenders}:\n{log_text[-2000:]}"
    )


def book_is_staged(emulator: Emulator) -> bool:
    """Return True when the real test book is present in the staged firmware."""
    result = emulator.run_arm_probe("stat", BOOK_PATH, check=False, timeout=10.0)
    return result.returncode == 0


def reader_binary_exists(emulator: Emulator) -> bool:
    """Return True when the reader app exists in the staged firmware."""
    result = emulator.run_arm_probe("stat", READER_APP, check=False, timeout=10.0)
    return result.returncode == 0
