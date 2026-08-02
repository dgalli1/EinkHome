"""Low-level event emission helpers for test-side touch input."""

from __future__ import annotations

import time
from typing import TYPE_CHECKING

from .ui_geometry import Point, SwipeGesture

if TYPE_CHECKING:
    from .runtime import Emulator


def tap(emulator: Emulator, point: Point, *, timeout: float = 5.0) -> None:
    """Send a full touch tap at one logical framebuffer coordinate."""
    _run_input(emulator, "touch", str(point.x), str(point.y), timeout=timeout)


def pointer_down(
    emulator: Emulator,
    point: Point,
    *,
    timeout: float = 5.0,
) -> None:
    """Emit one touch-down event at the given coordinate."""
    _pointer_event(emulator, "down", point, timeout=timeout)


def pointer_move(
    emulator: Emulator,
    point: Point,
    *,
    timeout: float = 5.0,
) -> None:
    """Emit one touch-move event at the given coordinate."""
    _pointer_event(emulator, "move", point, timeout=timeout)


def pointer_up(
    emulator: Emulator,
    point: Point,
    *,
    timeout: float = 5.0,
) -> None:
    """Emit one touch-up event at the given coordinate."""
    _pointer_event(emulator, "up", point, timeout=timeout)


def swipe(emulator: Emulator, gesture: SwipeGesture, *, timeout: float = 5.0) -> None:
    """Emit one swipe gesture described by ``gesture``."""
    path = _swipe_points(gesture)
    step_timeout = max(0.25, timeout / float(gesture.steps + 1))
    pointer_down(emulator, path[0], timeout=step_timeout)
    for point in path[1:-1]:
        time.sleep(0.05)
        pointer_move(emulator, point, timeout=step_timeout)
    time.sleep(0.05)
    pointer_up(emulator, path[-1], timeout=step_timeout)


def press_key(emulator: Emulator, iv_key: int, *, timeout: float = 5.0) -> None:
    """Emit one key event through the host ``send_event`` probe."""
    _run_input(emulator, "key", hex(iv_key), timeout=timeout)


def type_text(emulator: Emulator, text: str, *, timeout: float = 15.0) -> None:
    """Type *text* into the guest's focused text widget in one probe call.

    Sends the string as a stream of ``EVT_EXT_KB`` characters (the same
    primitive the host Wayland viewer uses for printable text), so it is
    resolution-independent and pays a single ``podman exec`` regardless of
    length.  This lands in whatever text widget the guest currently has
    focused — typically an open on-screen keyboard's edit field — but does
    NOT commit it; committing is widget-specific (see
    ``BookshelfSession.type_text`` for the keyboard-commit helper).
    """
    _run_input(emulator, "type", text, timeout=timeout)


def _pointer_event(
    emulator: Emulator,
    action: str,
    point: Point,
    *,
    timeout: float,
) -> None:
    """Emit one pointer event verb at a single coordinate."""
    _run_input(emulator, action, str(point.x), str(point.y), timeout=timeout)


def _run_input(emulator: Emulator, *args: str, timeout: float) -> None:
    """Run the stable host-side ``send_event`` helper and validate it."""
    result = emulator.run_input(*args, timeout=timeout)
    if result.returncode == 0:
        return
    command = " ".join(args)
    stderr = result.stderr.strip()
    raise RuntimeError(
        f"send_event {command} failed (rc={result.returncode}): {stderr}"
    )


def _swipe_points(gesture: SwipeGesture) -> tuple[Point, ...]:
    """Expand a swipe gesture into its sampled intermediate points."""
    points = [gesture.start]
    start = gesture.start
    end = gesture.end
    for index in range(1, gesture.steps):
        points.append(
            Point(
                round(start.x + ((end.x - start.x) * index / gesture.steps)),
                round(start.y + ((end.y - start.y) * index / gesture.steps)),
            )
        )
    points.append(end)
    return tuple(points)
