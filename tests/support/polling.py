"""Minimal polling helpers for the rewritten test harness."""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import NoReturn, TypeVar

T = TypeVar("T")

__all__ = ["RetryRequested", "poll_until", "retry_later"]


class RetryRequested(Exception):
    """Internal signal raised by helpers that want another polling attempt."""

    def __init__(self, detail: str = "") -> None:
        super().__init__(detail)
        self.detail = detail


def retry_later(detail: str = "") -> NoReturn:
    """Abort the current polling attempt and ask the caller to retry later."""
    raise RetryRequested(detail)


def poll_until(
    action: Callable[[], T],
    *,
    timeout: float,
    interval: float = 0.25,
    timeout_message: str | Callable[[str], str],
) -> T:
    """Run ``action`` until it succeeds or the timeout expires."""
    deadline = time.monotonic() + timeout
    last_detail = ""
    while True:
        try:
            return action()
        except RetryRequested as exc:
            last_detail = exc.detail
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                message = (
                    timeout_message(last_detail)
                    if callable(timeout_message)
                    else timeout_message
                )
                raise TimeoutError(message) from exc
            time.sleep(min(interval, max(0.01, remaining)))
