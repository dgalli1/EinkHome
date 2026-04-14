"""Shared constants and tiny helpers for live-emulator runtime tests."""

from __future__ import annotations

import os
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

__all__ = [
    "REPO_ROOT",
    "app_name_matches",
    "detect_firmware",
    "parse_key_value_fields",
    "parse_optional_int",
    "prepend_pythonpath",
    "shquote",
]


def detect_firmware() -> str:
    """Return the firmware directory selected for the current test run."""
    explicit = os.environ.get("PB_TEST_FIRMWARE")
    if explicit:
        return explicit

    candidates = sorted(
        path.name
        for path in REPO_ROOT.iterdir()
        if path.is_dir()
        and (path / "rootfs/lib/libc.so.6").is_file()
        and (path / "ebrmain").is_dir()
    )
    if len(candidates) == 1:
        return candidates[0]
    if not candidates:
        raise RuntimeError(
            f"no staged firmware found under {REPO_ROOT}; "
            "run ./pbemu install <firmware.zip>"
        )
    raise RuntimeError(
        "multiple staged firmwares found; set PB_TEST_FIRMWARE to one of: "
        + ", ".join(candidates)
    )


def app_name_matches(candidate: str, expected: str) -> bool:
    """Return True when app names match exactly or by basename."""
    return candidate == expected or Path(candidate).name == Path(expected).name


def prepend_pythonpath(env: dict[str, str], extra_path: str) -> None:
    """Prepend one path segment to ``PYTHONPATH`` in-place."""
    current = env.get("PYTHONPATH")
    env["PYTHONPATH"] = (
        extra_path if not current else f"{extra_path}{os.pathsep}{current}"
    )


def parse_key_value_fields(text: str) -> dict[str, str]:
    """Parse whitespace-separated ``key=value`` tokens from text."""
    fields: dict[str, str] = {}
    for token in text.split():
        if "=" not in token:
            continue
        key, value = token.split("=", 1)
        fields[key] = value
    return fields


def parse_optional_int(value: str | None) -> int | None:
    """Parse an optional integer literal in decimal or ``0x`` form."""
    if not value:
        return None
    try:
        return int(value, 0)
    except ValueError:
        return None


def shquote(value: str) -> str:
    """Return a shell-safe representation for a single argument."""
    if value and all(ch.isalnum() or ch in "_-./=:," for ch in value):
        return value
    return "'" + value.replace("'", "'\\''") + "'"
