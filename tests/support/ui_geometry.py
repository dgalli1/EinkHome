"""Geometry helpers and value objects for test-side touch input."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .runtime import Emulator

__all__ = [
    "FrameSize",
    "Point",
    "SwipeGesture",
    "frame_size",
    "reader_center",
    "reader_next_page_point",
    "reader_next_page_swipe",
    "recent_book_points",
]

_FIRMWARE_RECENT_BOOK_RATIOS: tuple[
    tuple[str, tuple[tuple[float, float], ...]],
    ...,
] = (
    ("U628", ((0.16, 0.20), (3 / 14, 3 / 8), (0.24, 0.20))),
    ("U740", ((0.24, 0.24), (0.24, 0.20), (3 / 14, 3 / 8))),
)

_DEFAULT_RECENT_BOOK_RATIOS: tuple[tuple[float, float], ...] = (
    (3 / 14, 3 / 8),
    (0.24, 0.20),
    (0.16, 0.20),
)


@dataclass(frozen=True, slots=True)
class Point:
    """One logical framebuffer coordinate."""

    x: int
    y: int

    def as_tuple(self) -> tuple[int, int]:
        """Return the point as the legacy ``(x, y)`` tuple form."""
        return self.x, self.y


@dataclass(frozen=True, slots=True)
class FrameSize:
    """Framebuffer width and height with a few coordinate helpers."""

    width: int
    height: int

    def point(self, x_pos: int, y_pos: int) -> Point:
        """Clamp one coordinate pair into the framebuffer bounds."""
        return Point(
            min(self.width - 1, max(0, x_pos)),
            min(self.height - 1, max(0, y_pos)),
        )

    def point_at(self, x_ratio: float, y_ratio: float) -> Point:
        """Return one point computed from horizontal and vertical ratios."""
        return self.point(int(self.width * x_ratio), int(self.height * y_ratio))

    def center(self) -> Point:
        """Return the logical centre point."""
        return self.point(self.width // 2, self.height // 2)


@dataclass(frozen=True, slots=True)
class SwipeGesture:
    """A simple swipe path described by its endpoints and sample count."""

    start: Point
    end: Point
    steps: int = 4

    def __post_init__(self) -> None:
        if self.steps < 1:
            raise ValueError(f"steps must be >= 1, got {self.steps}")


def frame_size(emulator: Emulator, *, timeout: float = 5.0) -> FrameSize:
    """Read the current framebuffer size from the informer snapshot."""
    snapshot = emulator.wait_for_informer_snapshot(timeout=timeout)
    if snapshot.width is None or snapshot.height is None:
        raise TimeoutError("framebuffer geometry not available")
    return FrameSize(snapshot.width, snapshot.height)


def recent_book_points(
    emulator: Emulator,
    *,
    timeout: float = 5.0,
) -> tuple[Point, ...]:
    """Return candidate tap points for the home-screen recent-book card."""
    framebuffer = frame_size(emulator, timeout=timeout)
    points: list[Point] = []
    seen: set[tuple[int, int]] = set()
    for x_ratio, y_ratio in _recent_book_ratios(emulator):
        point = framebuffer.point_at(x_ratio, y_ratio)
        key = point.as_tuple()
        if key in seen:
            continue
        seen.add(key)
        points.append(point)
    return tuple(points)


def reader_center(emulator: Emulator, *, timeout: float = 5.0) -> Point:
    """Return the logical centre point of the current reading surface."""
    return frame_size(emulator, timeout=timeout).center()


def reader_next_page_point(
    emulator: Emulator,
    *,
    timeout: float = 5.0,
) -> Point:
    """Return the reader tap zone used to advance to the next page."""
    framebuffer = frame_size(emulator, timeout=timeout)
    return framebuffer.point((framebuffer.width * 5) // 6, framebuffer.height // 2)


def reader_next_page_swipe(
    emulator: Emulator,
    *,
    timeout: float = 5.0,
    steps: int = 4,
) -> SwipeGesture:
    """Return the standard leftward swipe used for next-page fallback."""
    framebuffer = frame_size(emulator, timeout=timeout)
    y_pos = framebuffer.height // 2
    return SwipeGesture(
        start=framebuffer.point((framebuffer.width * 5) // 6, y_pos),
        end=framebuffer.point(framebuffer.width // 6, y_pos),
        steps=steps,
    )


def _recent_book_ratios(emulator: Emulator) -> tuple[tuple[float, float], ...]:
    """Return firmware-specific tap ratios for the recent-book card."""
    firmware = getattr(emulator, "firmware", "")
    for prefix, ratios in _FIRMWARE_RECENT_BOOK_RATIOS:
        if firmware.startswith(prefix):
            return ratios
    return _DEFAULT_RECENT_BOOK_RATIOS
