"""Open-flow helpers extracted from ReaderSession to reduce module size."""

from __future__ import annotations

from collections.abc import Callable

from tests.support.polling import poll_until, retry_later
from tests.support.runtime import ActiveTaskInfo, app_name_matches
from tests.support.ui_input import home_recent_book_tap_points, tap

from .actions import ReaderActions
from .logs import find_matching_markers, latest_book_path
from .models import ReaderState
from .patterns import (
    EXIT_ACTIVITY_PATTERNS,
    HOME_SURFACE_APPS,
    OPEN_ACTIVITY_PATTERNS,
    READER_APP,
)
from .session import Session, remaining


class OpenFlow:
    """Helpers for the 'open-from-home' scenario step."""

    def __init__(self, session: Session, actions: ReaderActions) -> None:
        self._s = session
        self._a = actions

    def return_to_home(self, deadline: float) -> None:
        """Navigate back to a home-surface app, retrying once if needed."""
        if self._at_home():
            return
        for _ in range(2):
            if self._try_leave_once(deadline):
                return
        last_log = self._s.tail_log(max_bytes=4096)
        raise TimeoutError(
            f"failed to return to home screen; tail=\n{last_log[-2000:]}"
        )

    def attempt_open(
        self,
        home_app: str,
        current_state_fn: Callable[[], ReaderState | None],
        deadline: float,
    ) -> ReaderState | None:
        """One attempt at navigating home and tapping the recent-book card."""
        try:
            self.return_to_home(deadline)
        except TimeoutError:
            pass
        home_task = self._wait_for_home_task(home_app, deadline)
        if home_task is None:
            return current_state_fn()
        return self._tap_all_points(home_task, deadline)

    # --- internals ----------------------------------------------------

    def _at_home(self) -> bool:
        """Return True when the current active app is a home-surface app."""
        try:
            task = self._s.active_task()
        except TimeoutError:
            return False
        return any(
            app_name_matches(task.appname, app) for app in HOME_SURFACE_APPS
        )

    def _try_leave_once(self, deadline: float) -> bool:
        """Send one Home gesture and wait briefly for a home-surface app."""
        log_offset = self._s.mark_log()
        self._a.leave_reader(deadline)
        try:
            self._s.wait_for_active_app(
                *HOME_SURFACE_APPS,
                timeout=min(5.0, remaining(deadline)),
            )
            return True
        except TimeoutError:
            tail = self._s.logs_since(log_offset)
            return bool(find_matching_markers(tail, EXIT_ACTIVITY_PATTERNS))

    def _wait_for_home_task(
        self, home_app: str, deadline: float,
    ) -> ActiveTaskInfo | None:
        """Wait for home to become active and quiesce; return task or None."""
        try:
            home_task = self._s.wait_for_active_app(
                home_app, timeout=min(8.0, remaining(deadline)),
            )
            self._s.emulator.wait_for_monitor_quiet(
                timeout=min(4.0, remaining(deadline)),
                quiet_period=0.5,
            )
            return home_task
        except TimeoutError:
            return None

    def _tap_all_points(
        self, home_task: ActiveTaskInfo, deadline: float,
    ) -> ReaderState | None:
        """Try tapping each recent-book candidate; return first success."""
        for point in home_recent_book_tap_points(
            self._s.emulator, timeout=min(5.0, remaining(deadline)),
        ):
            result = self._tap_book_and_wait(home_task, point, deadline)
            if result is not None:
                return result
        return None

    def _tap_book_and_wait(
        self,
        home_task: ActiveTaskInfo,
        tap_point: tuple[int, int],
        deadline: float,
    ) -> ReaderState | None:
        """Tap a recent-book candidate and return a ReaderState if successful."""
        x_pos, y_pos = tap_point
        log_offset = self._s.mark_log()
        tap(
            self._s.emulator, x_pos, y_pos,
            timeout=min(2.0, remaining(deadline)),
        )
        try:
            return self._wait_for_reader(home_task, log_offset, deadline)
        except (TimeoutError, RuntimeError):
            try:
                self.return_to_home(deadline)
            except TimeoutError:
                pass
            return None

    def _wait_for_reader(
        self,
        home_task: ActiveTaskInfo,
        log_offset: int,
        deadline: float,
    ) -> ReaderState:
        """Wait for reader to become active after tap and capture its state."""
        reader_entry = self._s.wait_for_task_matching(
            READER_APP,
            timeout=min(12.0, remaining(deadline)),
        )
        open_log, open_markers, opened_path = self._wait_for_open_log(
            log_offset, deadline,
        )
        return ReaderState(
            emulator=self._s.emulator,
            log_offset=log_offset,
            home_task=home_task,
            reader_task=ActiveTaskInfo(
                active_task_id=reader_entry.task_id,
                active_subtask_id=-1,
                appname=reader_entry.name,
                mainpid=int(reader_entry.pid) if reader_entry.pid.isdigit() else None,
            ),
            opened_book_path=opened_path,
            open_log=open_log,
            open_markers=open_markers,
        )

    def _wait_for_open_log(
        self, since: int, deadline: float,
    ) -> tuple[str, tuple[str, ...], str]:
        """Poll until monitor.log contains a valid book-path entry."""
        timeout = remaining(deadline)

        def _attempt() -> tuple[str, tuple[str, ...], str]:
            log_text = self._s.logs_since(since)
            opened_book_path = latest_book_path(log_text)
            if not opened_book_path:
                retry_later(str(len(log_text)))
            return (
                log_text,
                find_matching_markers(log_text, OPEN_ACTIVITY_PATTERNS),
                opened_book_path,
            )

        return poll_until(
            _attempt,
            timeout=timeout,
            interval=0.3,
            timeout_message=lambda tail_len: (
                f"monitor.log did not record a reader open path"
                f" within {timeout}s; tail_len={tail_len}"
            ),
        )
