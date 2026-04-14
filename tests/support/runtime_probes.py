"""Probe execution helpers for live-emulator runtime tests."""

from __future__ import annotations

import subprocess

from pbemu.paths import CONTAINER_FIRMWARE
from pbemu.run import qemu_env_args

from .runtime_common import shquote
from .runtime_container import container_exec, container_sh

__all__ = ["run_arm_probe", "run_input", "run_probe"]


def run_probe(
    name: str,
    *args: str,
    check: bool = True,
    timeout: float = 15.0,
) -> subprocess.CompletedProcess[str]:
    """Run one host-side probe binary inside the container."""
    return container_exec(
        f"/workspace/src/viewer/build-pc/{name}",
        *args,
        check=check,
        timeout=timeout,
    )


def run_input(
    *args: str,
    check: bool = True,
    timeout: float = 15.0,
) -> subprocess.CompletedProcess[str]:
    """Run the stable host-side ``send_event`` probe."""
    return run_probe("send_event", *args, check=check, timeout=timeout)


def run_arm_probe(
    *args: str,
    check: bool = True,
    timeout: float = 30.0,
) -> subprocess.CompletedProcess[str]:
    """Run the ARM-side compatibility probe under qemu-arm."""
    argv = " ".join(shquote(arg) for arg in args)
    command = (
        f"exec qemu-arm -L {CONTAINER_FIRMWARE}/.live/guest "
        f"{qemu_env_args()} "
        "/workspace/src/probes/arm/build-arm/arm_probe"
    )
    if argv:
        command = f"{command} {argv}"
    return container_sh(command, check=check, timeout=timeout)
