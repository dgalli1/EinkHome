"""Monitor-log helpers for live-emulator runtime tests."""

from __future__ import annotations

import time
import re
from pathlib import Path

from .polling import poll_until, retry_later
from .runtime_common import REPO_ROOT

__all__ = [
    "monitor_log_path",
    "monitor_log_size",
    "read_monitor_log_since",
    "wait_for_monitor_log",
    "wait_for_monitor_quiet",
]

_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*m")
_IGNORABLE_MONITOR_LINE_RES = (
    re.compile(r"check_power error\s*$"),
    re.compile(r"\(hw_ipcrequest_int\)request failed: type = 0x128; r = 0; ipc_ready = 1\s*$"),
)


def _strip_ansi(text: str) -> str:
    return _ANSI_ESCAPE_RE.sub("", text)


def _is_ignorable_monitor_line(line: str) -> bool:
    stripped = _strip_ansi(line).strip()
    if not stripped:
        return True
    return any(pattern.search(stripped) for pattern in _IGNORABLE_MONITOR_LINE_RES)


def monitor_log_path(firmware: str) -> Path:
    """Return the host path for the staged guest ``monitor.log``."""
    return PBEMU_ROOT / firmware / ".live/var/log/monitor.log"


def monitor_log_size(firmware: str) -> int:
    """Return the current size of ``monitor.log`` in bytes."""
    path = monitor_log_path(firmware)
    return path.stat().st_size if path.exists() else 0


def read_monitor_log_since(firmware: str, offset: int) -> str:
    """Return ``monitor.log`` content written after one byte offset."""
    path = monitor_log_path(firmware)
    if not path.exists():
        return ""
    with path.open("rb") as handle:
        handle.seek(offset)
        return handle.read().decode("utf-8", errors="replace")


def wait_for_monitor_log(
    firmware: str,
    needle: str,
    *,
    since: int = 0,
    timeout: float = 30.0,
) -> str:
    """Poll until a string appears in new ``monitor.log`` output."""

    def _attempt() -> str:
        tail = read_monitor_log_since(firmware, since)
        if needle not in tail:
            retry_later(str(len(tail)))
        return tail

    return poll_until(
        _attempt,
        interval=0.3,
        timeout=timeout,
        timeout_message=lambda tail_len: (
            f"monitor.log did not contain {needle!r} within {timeout}s;"
            f" tail_len={tail_len}"
        ),
    )


def wait_for_monitor_quiet(
    firmware: str,
    *,
    quiet_period: float = 1.0,
    timeout: float = 15.0,
) -> None:
    """Wait until ``monitor.log`` stops growing for one quiet window."""
    last_size = monitor_log_size(firmware)
    stable_since = time.monotonic()

    def _attempt() -> None:
        nonlocal last_size, stable_since
        current_size = monitor_log_size(firmware)
        if current_size != last_size:
            delta = read_monitor_log_since(firmware, last_size)
            last_size = current_size
            if any(
                not _is_ignorable_monitor_line(line)
                for line in delta.splitlines()
            ):
                stable_since = time.monotonic()
                retry_later(f"size={current_size}")
        if time.monotonic() - stable_since < quiet_period:
            retry_later(f"size={current_size}")

    poll_until(
        _attempt,
        timeout=timeout,
        interval=0.1,
        timeout_message=(
            f"monitor.log did not go quiet within {timeout}s"
            f" (quiet_period={quiet_period}s)"
        ),
    )
