"""Log text matching helpers for reader flow scenarios."""

from __future__ import annotations

import re

from .patterns import OPEN_BOOK_PATTERNS


def find_matching_markers(
    log_text: str,
    patterns: tuple[tuple[str, re.Pattern[str]], ...],
) -> tuple[str, ...]:
    """Return the labels of all patterns that match *log_text*."""
    return tuple(label for label, pattern in patterns if pattern.search(log_text))


def latest_book_path(log_text: str) -> str | None:
    """Return the most recently mentioned book path in *log_text*, or None."""
    latest_path: str | None = None
    latest_end = -1
    for pattern in OPEN_BOOK_PATTERNS:
        for match in pattern.finditer(log_text):
            if match.end() >= latest_end:
                latest_end = match.end()
                latest_path = match.group("path")
    return latest_path
