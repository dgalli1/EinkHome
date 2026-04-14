"""Public runtime façade for live-emulator test helpers."""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from . import runtime_logs, runtime_probes, runtime_state
from .runtime_common import (
    REPO_ROOT,
    app_name_matches,
    detect_firmware,
    prepend_pythonpath,
)
from .runtime_container import (
    CONTAINER,
    PODMAN,
    container_exec,
    container_running,
    container_sh,
    wait_for_guest_path,
)
from .runtime_state import ActiveTaskInfo, InformerSnapshot, TaskEntry

__all__ = [
    "ActiveTaskInfo",
    "CONTAINER",
    "Emulator",
    "InformerSnapshot",
    "PODMAN",
    "REPO_ROOT",
    "TaskEntry",
    "app_name_matches",
    "container_exec",
    "container_running",
    "container_sh",
    "detect_firmware",
    "wait_for_monitor_quiet",
]


@dataclass
class Emulator:
    """Test-side runtime façade with a wide but explicit method surface."""

    firmware: str

    # --- lifecycle -----------------------------------------------------

    def start(self, *, force: bool = False, no_build: bool = False) -> None:
        """Start the emulator unless a ready external one was supplied."""
        if not force and os.environ.get("PB_EMULATOR_EXTERNAL") == "1":
            if container_running():
                return
        command = [sys.executable, "-m", "pbemu", "start", "--no-viewer", "--no-audio"]
        if no_build:
            command.append("--no-build")
        command.append(self.firmware)
        subprocess.run(command, cwd=REPO_ROOT, env=_pbemu_env(), check=True)

    def stop(self, *, force: bool = False) -> None:
        """Stop the emulator unless ownership stays with an external caller."""
        if not force and os.environ.get("PB_EMULATOR_EXTERNAL") == "1":
            return
        subprocess.run(
            [sys.executable, "-m", "pbemu", "stop"],
            cwd=REPO_ROOT,
            env=_pbemu_env(),
            check=False,
        )

    # --- container readiness ------------------------------------------

    def wait_for_monitor(self, timeout: float = 30.0) -> None:
        """Poll until ``monitor.app`` is alive inside the guest."""
        wait_for_guest_path(
            "ps -eo args | grep -q '[q]emu-arm.*monitor.app'",
            detail="monitor-not-found",
            failure_message="monitor.app did not start within timeout",
            timeout=timeout,
        )

    def wait_for_hwevent(self, timeout: float = 30.0) -> None:
        """Poll until the monitor has created the ``/hwevent`` queue."""
        wait_for_guest_path(
            "test -e /dev/mqueue/hwevent",
            detail="hwevent-missing",
            failure_message="/hwevent queue not created within timeout",
            timeout=timeout,
        )

    # --- probes --------------------------------------------------------

    def run_probe(
        self,
        name: str,
        *args: str,
        check: bool = True,
        timeout: float = 15.0,
    ) -> subprocess.CompletedProcess[str]:
        """Run one host-side probe binary inside the container."""
        return runtime_probes.run_probe(name, *args, check=check, timeout=timeout)

    def run_input(
        self,
        *args: str,
        check: bool = True,
        timeout: float = 15.0,
    ) -> subprocess.CompletedProcess[str]:
        """Run the stable host-side ``send_event`` probe."""
        return runtime_probes.run_input(*args, check=check, timeout=timeout)

    def run_arm_probe(
        self,
        *args: str,
        check: bool = True,
        timeout: float = 30.0,
    ) -> subprocess.CompletedProcess[str]:
        """Run the ARM-side compatibility probe under qemu-arm."""
        return runtime_probes.run_arm_probe(*args, check=check, timeout=timeout)

    # --- task / informer state ----------------------------------------

    def wait_for_informer_snapshot(
        self, timeout: float = 30.0,
    ) -> InformerSnapshot:
        """Poll until the informer exposes a complete framebuffer snapshot."""
        return runtime_state.wait_for_informer_snapshot(timeout=timeout)

    def read_task_info(self, *, timeout: float = 5.0) -> ActiveTaskInfo:
        """Return the current foreground task snapshot and parsed metadata."""
        return runtime_state.read_task_info(timeout=timeout)

    def wait_for_active_task_info(
        self, timeout: float = 30.0,
    ) -> ActiveTaskInfo:
        """Poll until active-task metadata contains both id and app name."""
        return runtime_state.wait_for_active_task_info(timeout=timeout)

    def list_tasks(self, *, timeout: float = 5.0) -> tuple[TaskEntry, ...]:
        """Return the current ``/var/run/task`` directory as sorted entries."""
        return runtime_state.list_tasks(timeout=timeout)

    # --- monitor log ---------------------------------------------------

    def monitor_log_path(self) -> Path:
        """Return the host path for the staged guest ``monitor.log``."""
        return runtime_logs.monitor_log_path(self.firmware)

    def monitor_log_size(self) -> int:
        """Return the current size of ``monitor.log`` in bytes."""
        return runtime_logs.monitor_log_size(self.firmware)

    def read_monitor_log_since(self, offset: int) -> str:
        """Return ``monitor.log`` content written after one byte offset."""
        return runtime_logs.read_monitor_log_since(self.firmware, offset)

    def wait_for_monitor_log(
        self, needle: str, *, since: int = 0, timeout: float = 30.0,
    ) -> str:
        """Poll until ``needle`` appears in new ``monitor.log`` output."""
        return runtime_logs.wait_for_monitor_log(
            self.firmware, needle, since=since, timeout=timeout,
        )

    def wait_for_monitor_quiet(
        self, *, quiet_period: float = 1.0, timeout: float = 15.0,
    ) -> None:
        """Wait until ``monitor.log`` stops growing for one quiet window."""
        runtime_logs.wait_for_monitor_quiet(
            self.firmware, quiet_period=quiet_period, timeout=timeout,
        )


def wait_for_monitor_quiet(
    target: Emulator | str,
    *,
    quiet_period: float = 1.0,
    timeout: float = 15.0,
) -> None:
    """Compatibility wrapper for callers that still import this helper.

    Prefer ``Emulator.wait_for_monitor_quiet()`` for clarity.
    """
    firmware = target.firmware if isinstance(target, Emulator) else target
    runtime_logs.wait_for_monitor_quiet(
        firmware,
        quiet_period=quiet_period,
        timeout=timeout,
    )


def _pbemu_env() -> dict[str, str]:
    """Return an environment dict with ``tools/`` prepended to PYTHONPATH."""
    env = os.environ.copy()
    prepend_pythonpath(env, str(REPO_ROOT / "tools"))
    return env
