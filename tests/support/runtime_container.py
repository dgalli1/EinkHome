"""Container shell helpers for live-emulator runtime tests."""

from __future__ import annotations

import os
import subprocess

from .polling import poll_until, retry_later

PODMAN = os.environ.get("PODMAN", "podman")
CONTAINER = os.environ.get("PB_SYSTEM_CONTAINER", "pb-pocketbook-ui")

__all__ = [
    "CONTAINER",
    "PODMAN",
    "container_exec",
    "container_running",
    "container_sh",
    "wait_for_guest_path",
]


def container_running() -> bool:
    """Return True when the emulator container currently exists."""
    return subprocess.run(
        [PODMAN, "container", "exists", CONTAINER],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    ).returncode == 0


def container_exec(
    *args: str,
    check: bool = True,
    capture_output: bool = True,
    timeout: float | None = 30.0,
) -> subprocess.CompletedProcess[str]:
    """Run one command inside the emulator container."""
    return subprocess.run(
        [PODMAN, "exec", CONTAINER, *args],
        check=check,
        capture_output=capture_output,
        text=True,
        timeout=timeout,
    )


def container_sh(
    script: str,
    *,
    check: bool = True,
    capture_output: bool = True,
    timeout: float | None = 30.0,
) -> subprocess.CompletedProcess[str]:
    """Run one shell snippet inside the emulator container."""
    return container_exec(
        "sh",
        "-lc",
        script,
        check=check,
        capture_output=capture_output,
        timeout=timeout,
    )


def wait_for_guest_path(
    script: str,
    *,
    detail: str,
    failure_message: str,
    timeout: float = 30.0,
) -> None:
    """Poll one shell predicate inside the container until it succeeds."""

    def _attempt() -> None:
        if container_sh(script, check=False).returncode != 0:
            retry_later(detail)

    poll_until(
        _attempt,
        interval=0.5,
        timeout=timeout,
        timeout_message=failure_message,
    )
