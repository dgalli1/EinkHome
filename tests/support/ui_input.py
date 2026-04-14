"""Stable touch and key input helpers for the rewritten tests."""

from __future__ import annotations

from typing import TYPE_CHECKING

from .ui_events import (
    pointer_down as emit_pointer_down,
    pointer_move as emit_pointer_move,
    pointer_up as emit_pointer_up,
    press_key as emit_press_key,
    swipe as emit_swipe,
    tap as emit_tap,
)
from .ui_geometry import (
    FrameSize,
    Point,
    SwipeGesture,
    reader_center,
    reader_next_page_point as resolve_reader_next_page_point,
    reader_next_page_swipe,
    recent_book_points,
)

if TYPE_CHECKING:
    from .runtime import Emulator

IV_KEY_OK = 0x0A
IV_KEY_UP = 0x11
IV_KEY_DOWN = 0x12
IV_KEY_LEFT = 0x13
IV_KEY_RIGHT = 0x14
IV_KEY_MENU = 0x17
IV_KEY_PREV = 0x18
IV_KEY_NEXT = 0x19
IV_KEY_HOME = 0x1A
IV_KEY_BACK = 0x1B

__all__ = [
    "FrameSize",
    "IV_KEY_BACK",
    "IV_KEY_DOWN",
    "IV_KEY_HOME",
    "IV_KEY_LEFT",
    "IV_KEY_MENU",
    "IV_KEY_NEXT",
    "IV_KEY_OK",
    "IV_KEY_PREV",
    "IV_KEY_RIGHT",
    "IV_KEY_UP",
    "Point",
    "SwipeGesture",
    "home_recent_book_tap_points",
    "pointer_down",
    "pointer_move",
    "pointer_up",
    "press_key",
    "reader_center_point",
    "swipe",
    "swipe_reader_next_page",
    "tap",
    "tap_home_recent_book",
    "tap_reader_center",
    "tap_reader_next_page",
]


def tap(emulator: Emulator, x: int, y: int, *, timeout: float = 5.0) -> None:
    """Send a complete touch tap at one logical framebuffer coordinate."""
    emit_tap(emulator, Point(x, y), timeout=timeout)


def pointer_down(emulator: Emulator, x: int, y: int, *, timeout: float = 5.0) -> None:
    """Send one touch-down event at one logical framebuffer coordinate."""
    emit_pointer_down(emulator, Point(x, y), timeout=timeout)


def pointer_move(emulator: Emulator, x: int, y: int, *, timeout: float = 5.0) -> None:
    """Send one touch-move event at one logical framebuffer coordinate."""
    emit_pointer_move(emulator, Point(x, y), timeout=timeout)


def pointer_up(emulator: Emulator, x: int, y: int, *, timeout: float = 5.0) -> None:
    """Send one touch-up event at one logical framebuffer coordinate."""
    emit_pointer_up(emulator, Point(x, y), timeout=timeout)


def swipe(
    emulator: Emulator,
    start: SwipeGesture | tuple[int, int] | Point,
    end: tuple[int, int] | Point | None = None,
    **options: float,
) -> None:
    """Send a simple drag gesture across the logical framebuffer.

    Accepts either a ready-made ``SwipeGesture`` or the legacy
    ``(start, end, *, steps=..., timeout=...)`` calling convention.
    """
    gesture, timeout = _coerce_swipe(start, end, options)
    emit_swipe(emulator, gesture, timeout=timeout)


def press_key(emulator: Emulator, iv_key: int, *, timeout: float = 5.0) -> None:
    """Send one key press via the host send_event probe."""
    emit_press_key(emulator, iv_key, timeout=timeout)


def home_recent_book_tap_points(
    emulator: Emulator,
    *,
    timeout: float = 5.0,
) -> tuple[tuple[int, int], ...]:
    """Return candidate tap points for the recent-book card on the home screen."""
    return _as_tuples(recent_book_points(emulator, timeout=timeout))


def tap_home_recent_book(emulator: Emulator, *, timeout: float = 5.0) -> None:
    """Tap the best-known recent-book card position for the current firmware."""
    _tap_point(emulator, recent_book_points(emulator, timeout=timeout)[0], timeout)


def reader_center_point(emulator: Emulator, *, timeout: float = 5.0) -> tuple[int, int]:
    """Return the logical centre point of the current reading surface."""
    return reader_center(emulator, timeout=timeout).as_tuple()


def tap_reader_center(emulator: Emulator, *, timeout: float = 5.0) -> None:
    """Tap the centre of the current reading surface."""
    _tap_point(emulator, reader_center(emulator, timeout=timeout), timeout)


def tap_reader_next_page(emulator: Emulator, *, timeout: float = 5.0) -> None:
    """Tap the right-side page-turn zone on the reading surface."""
    _tap_point(
        emulator,
        resolve_reader_next_page_point(emulator, timeout=timeout),
        timeout,
    )


def swipe_reader_next_page(emulator: Emulator, *, timeout: float = 5.0) -> None:
    """Swipe left across the reading surface to trigger next-page."""
    emit_swipe(
        emulator,
        reader_next_page_swipe(emulator, timeout=timeout),
        timeout=timeout,
    )


def _as_tuples(points: tuple[Point, ...]) -> tuple[tuple[int, int], ...]:
    """Convert value-object points back into the legacy tuple form."""
    return tuple(point.as_tuple() for point in points)


def _coerce_point(point: Point | tuple[int, int]) -> Point:
    """Convert either a ``Point`` or one ``(x, y)`` tuple into a ``Point``."""
    if isinstance(point, Point):
        return point
    x_pos, y_pos = point
    return Point(x_pos, y_pos)


def _coerce_swipe(
    start: SwipeGesture | tuple[int, int] | Point,
    end: tuple[int, int] | Point | None,
    options: dict[str, float],
) -> tuple[SwipeGesture, float]:
    """Normalise legacy and value-object swipe arguments."""
    timeout = float(options.pop("timeout", 5.0))
    steps = int(options.pop("steps", 4))
    if options:
        unknown = ", ".join(sorted(options))
        raise TypeError(f"unexpected swipe options: {unknown}")
    if isinstance(start, SwipeGesture):
        if end is not None:
            raise TypeError("end must be omitted when start is a SwipeGesture")
        return start, timeout
    if end is None:
        raise TypeError("end point is required when start is not a SwipeGesture")
    return SwipeGesture(_coerce_point(start), _coerce_point(end), steps), timeout


def _tap_point(emulator: Emulator, point: Point, timeout: float) -> None:
    """Emit one tap at a point already resolved by a geometry helper."""
    emit_tap(emulator, point, timeout=timeout)
