"""Gesture sequences for reader interactions — fire-and-forget, no assertions."""

from __future__ import annotations

from typing import TYPE_CHECKING

from tests.support.ui_input import (
    IV_KEY_HOME,
    IV_KEY_MENU,
    IV_KEY_NEXT,
    press_key,
    swipe_reader_next_page,
    tap_reader_center,
    tap_reader_next_page,
)

from .session import remaining

if TYPE_CHECKING:
    from .session import Session


class ReaderActions:
    """Encapsulates the input fallback order for common reader gestures."""

    def __init__(self, session: Session) -> None:
        self._session = session

    def invoke_menu(self, attempt: int, deadline: float) -> None:
        """Send the menu gesture for the given attempt index."""
        timeout = min(2.0, remaining(deadline))
        if attempt == 0:
            tap_reader_center(self._session.emulator, timeout=timeout)
        elif attempt == 1:
            press_key(self._session.emulator, IV_KEY_MENU, timeout=timeout)
        else:
            tap_reader_center(self._session.emulator, timeout=timeout)

    def next_page(self, attempt: int, deadline: float) -> None:
        """Send the page-advance gesture for the given attempt index."""
        timeout = remaining(deadline)
        if attempt == 0:
            tap_reader_next_page(self._session.emulator, timeout=min(2.0, timeout))
        elif attempt == 1:
            press_key(self._session.emulator, IV_KEY_NEXT, timeout=min(2.0, timeout))
        else:
            swipe_reader_next_page(self._session.emulator, timeout=min(2.5, timeout))

    def leave_reader(self, deadline: float) -> None:
        """Press the Home key to exit the reader."""
        press_key(
            self._session.emulator,
            IV_KEY_HOME,
            timeout=min(2.0, remaining(deadline)),
        )
