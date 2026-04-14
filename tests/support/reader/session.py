"""Generic emulator interaction helpers with no reader-specific knowledge."""

from __future__ import annotations

import re
import time

from tests.support.polling import poll_until, retry_later
from tests.support.runtime import ActiveTaskInfo, Emulator, TaskEntry, app_name_matches

from .logs import find_matching_markers

# Default timeouts shared by reader scenarios. Flat module constants instead of
# a nested dataclass: no caller has ever overridden them.
SCENARIO_TIMEOUT = 15.0
FRAMEBUFFER_SETTLE = 4.0
MENU_TIMEOUT = 4.0
PAGE_TIMEOUT = 4.0
ACTIVE_TASK_SAMPLE_TIMEOUT = 1.0


def deadline_in(seconds: float) -> float:
    """Return a monotonic deadline ``seconds`` from now."""
    return time.monotonic() + seconds


def remaining(deadline: float, *, floor: float = 0.5) -> float:
    """Return seconds remaining until *deadline*, clamped to *floor*."""
    return max(floor, deadline - time.monotonic())


class Session:
    """Emulator interaction helpers with no reader-specific knowledge."""

    def __init__(self, emulator: Emulator) -> None:
        self.emulator = emulator

    def mark_log(self) -> int:
        """Return the current monitor.log byte offset."""
        return self.emulator.monitor_log_size()

    def logs_since(self, offset: int) -> str:
        """Return monitor.log content written after *offset*."""
        return self.emulator.read_monitor_log_since(offset)

    def tail_log(self, max_bytes: int = 131072) -> str:
        """Return the trailing *max_bytes* of monitor.log."""
        size = self.emulator.monitor_log_size()
        return self.emulator.read_monitor_log_since(max(0, size - max_bytes))

    def active_task(
        self, *, timeout: float = ACTIVE_TASK_SAMPLE_TIMEOUT,
    ) -> ActiveTaskInfo:
        """Return the current active task info snapshot."""
        return self.emulator.read_task_info(timeout=timeout)

    def _read_active_task_or_placeholder(
        self, *, timeout: float = ACTIVE_TASK_SAMPLE_TIMEOUT,
    ) -> ActiveTaskInfo:
        """Return the active task snapshot or a placeholder when sampling times out."""
        try:
            return self.emulator.read_task_info(timeout=timeout)
        except TimeoutError:
            return ActiveTaskInfo(-1, -1)

    def _wait_for_active_task_sample(self, *, timeout: float) -> ActiveTaskInfo:
        """Return one active-task sample or ask the poller to retry later."""
        try:
            return self.emulator.read_task_info(timeout=timeout)
        except TimeoutError as exc:
            retry_later(str(exc))

    def wait_for_active_app(
        self,
        *apps: str,
        timeout: float = SCENARIO_TIMEOUT,
    ) -> ActiveTaskInfo:
        """Poll until the active app name matches one of *apps*."""
        sample_timeout = min(ACTIVE_TASK_SAMPLE_TIMEOUT, max(0.5, timeout))
        last_info = self._read_active_task_or_placeholder(timeout=sample_timeout)

        def _attempt() -> ActiveTaskInfo:
            nonlocal last_info
            last_info = self._wait_for_active_task_sample(timeout=sample_timeout)
            if not any(app_name_matches(last_info.appname, app) for app in apps):
                retry_later(last_info.appname)
            return last_info

        return poll_until(
            _attempt,
            timeout=timeout,
            interval=0.1,
            timeout_message=lambda _: (
                f"active app did not become {apps!r} within {timeout}s;"
                f" last app={last_info.appname!r}"
                f" task_id={last_info.active_task_id}"
            ),
        )

    def wait_for_task_presence(
        self, app: str, timeout: float = SCENARIO_TIMEOUT,
    ) -> TaskEntry:
        """Poll until the task list contains an entry matching *app*."""
        last_tasks: tuple[TaskEntry, ...] = ()

        def _attempt() -> TaskEntry:
            nonlocal last_tasks
            last_tasks = self.emulator.list_tasks()
            for task in last_tasks:
                if app_name_matches(task.name, app):
                    return task
            retry_later(repr(last_tasks))

        return poll_until(
            _attempt,
            timeout=timeout,
            interval=0.1,
            timeout_message=lambda _: (
                f"task-list did not contain {app!r} within {timeout}s;"
                f" last tasks={last_tasks!r}"
            ),
        )

    def find_task_matching(
        self,
        *apps: str,
        timeout: float = ACTIVE_TASK_SAMPLE_TIMEOUT,
    ) -> TaskEntry | None:
        """Return the first task-list entry whose name matches one of *apps*."""
        for task in self.emulator.list_tasks(timeout=timeout):
            if any(app_name_matches(task.name, app) for app in apps):
                return task
        return None

    def wait_for_task_matching(
        self,
        *apps: str,
        timeout: float = SCENARIO_TIMEOUT,
    ) -> TaskEntry:
        """Poll until the task list contains one entry matching any of *apps*."""
        last_tasks: tuple[TaskEntry, ...] = ()

        def _attempt() -> TaskEntry:
            nonlocal last_tasks
            last_tasks = self.emulator.list_tasks()
            for task in last_tasks:
                if any(app_name_matches(task.name, app) for app in apps):
                    return task
            retry_later(repr(last_tasks))

        return poll_until(
            _attempt,
            timeout=timeout,
            interval=0.1,
            timeout_message=lambda _: (
                f"task-list did not contain any of {apps!r} within {timeout}s;"
                f" last tasks={last_tasks!r}"
            ),
        )

    def framebuffer_hash(self) -> str:
        """Return the current framebuffer pixel hash."""
        result = self.emulator.run_probe("frame_dump", "--hash")
        for token in result.stdout.split():
            if token.startswith("pixel_hash="):
                return token.split("=", 1)[1]
        raise AssertionError(
            f"pixel_hash not found in frame_dump output: {result.stdout!r}"
        )

    def wait_for_framebuffer_change(
        self, baseline: str, timeout: float = FRAMEBUFFER_SETTLE,
    ) -> str:
        """Poll until the framebuffer hash differs from *baseline* and is stable."""
        seen: list[str] = []
        candidate = ""
        stable = 0

        def _attempt() -> str:
            nonlocal candidate, stable
            current = self.framebuffer_hash()
            seen.append(current)
            if current == baseline:
                candidate, stable = "", 0
                retry_later(current)
            if current == candidate:
                stable += 1
            else:
                candidate, stable = current, 1
            if stable < 2:
                retry_later(current)
            return current

        return poll_until(
            _attempt,
            timeout=timeout,
            interval=0.1,
            timeout_message=lambda _: (
                f"framebuffer hash did not change from {baseline}"
                f" within {timeout}s; seen={seen[-12:]}"
            ),
        )

    def assert_no_markers(
        self, log_text: str, markers: tuple[str, ...],
    ) -> None:
        """Assert that *log_text* contains none of *markers*."""
        offenders = [m for m in markers if m in log_text]
        assert not offenders, (
            f"monitor.log contains failure markers"
            f" {offenders}:\n{log_text[-2000:]}"
        )

    def wait_for_log_markers(
        self,
        patterns: tuple[tuple[str, re.Pattern[str]], ...],
        *,
        since: int,
        timeout: float,
    ) -> tuple[str, tuple[str, ...]]:
        """Poll until at least one *pattern* matches monitor.log since *since*."""

        def _attempt() -> tuple[str, tuple[str, ...]]:
            log_text = self.emulator.read_monitor_log_since(since)
            matched = find_matching_markers(log_text, patterns)
            if not matched:
                retry_later(str(len(log_text)))
            return log_text, matched

        return poll_until(
            _attempt,
            timeout=timeout,
            interval=0.2,
            timeout_message=lambda detail: (
                f"monitor.log markers not observed within {timeout}s;"
                f" tail_len={detail}"
            ),
        )
