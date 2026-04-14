"""ReaderSession: reader-specific scenario orchestration built on Session."""

from __future__ import annotations

import time
from dataclasses import dataclass, replace

from tests.support.polling import poll_until, retry_later
from tests.support.runtime import ActiveTaskInfo, TaskEntry, app_name_matches

from .actions import ReaderActions
from .logs import find_matching_markers, latest_book_path
from .models import (
    PageTurnResult,
    ReaderExitResult,
    ReaderMenuState,
    ReaderState,
)
from .open_helpers import OpenFlow
from .patterns import (
    DEFAULT_CRASH_MARKERS,
    EXIT_ACTIVITY_PATTERNS,
    EXIT_CONTROL_APPS,
    HOME_APP,
    HOME_SURFACE_APPS,
    MENU_ACTIVITY_PATTERNS,
    OPEN_ACTIVITY_PATTERNS,
    PAGE_ACTIVITY_PATTERNS,
    READER_APP,
)
from .session import (
    ACTIVE_TASK_SAMPLE_TIMEOUT,
    FRAMEBUFFER_SETTLE,
    MENU_TIMEOUT,
    PAGE_TIMEOUT,
    SCENARIO_TIMEOUT,
    Session,
    remaining,
)


@dataclass
class _MenuContext:
    """Captured state passed to a single menu-invoke attempt."""

    state: ReaderState
    before_hash: str


@dataclass
class _ExitContext:
    """Captured state shared across exit-confirmation polling samples."""

    before_hash: str
    log_offset: int
    existing_keys: set[tuple[int, str]]


class ReaderSession:
    """Reader-specific scenario orchestration built on top of a Session."""

    HOME_APP = HOME_APP
    READER_APP = READER_APP
    HOME_SURFACE_APPS = HOME_SURFACE_APPS
    EXIT_CONTROL_APPS = EXIT_CONTROL_APPS
    CRASH_MARKERS: tuple[str, ...] = DEFAULT_CRASH_MARKERS

    def __init__(self, session: Session) -> None:
        self.session = session
        self.actions = ReaderActions(session)
        self._open = OpenFlow(session, self.actions)

    # --- state inspection ---------------------------------------------

    def current_state(self) -> ReaderState | None:
        """Return a ReaderState if the reader is already active, else None."""
        reader_task = self._current_reader_task()
        if reader_task is None:
            return None
        open_log = self.session.tail_log()
        opened_book_path = latest_book_path(open_log)
        if not opened_book_path:
            return None
        return ReaderState(
            emulator=self.session.emulator,
            log_offset=self.session.mark_log(),
            home_task=self.session.active_task(),
            reader_task=reader_task,
            opened_book_path=opened_book_path,
            open_log=open_log,
            open_markers=find_matching_markers(open_log, OPEN_ACTIVITY_PATTERNS),
        )

    def _current_reader_task(self) -> ActiveTaskInfo | None:
        """Return the foreground reader task, or None if another app is active."""
        try:
            current_task = self.session.active_task()
        except TimeoutError:
            return None
        if app_name_matches(current_task.appname, self.READER_APP):
            return current_task
        return None

    def _reader_task_entry(self) -> TaskEntry | None:
        """Return the real reader task entry from the task list, if present."""
        return self.session.find_task_matching(
            self.READER_APP,
            timeout=min(2.0, ACTIVE_TASK_SAMPLE_TIMEOUT),
        )

    def read_new_log(self, state: ReaderState) -> str:
        """Return monitor.log text written since the reader state was captured."""
        return self.session.logs_since(state.log_offset)

    def return_to_home(self, deadline: float) -> None:
        """Navigate back to a home-surface app."""
        self._open.return_to_home(deadline)

    # --- public scenarios ---------------------------------------------

    def ensure_open_from_home(
        self, timeout: float = SCENARIO_TIMEOUT,
    ) -> ReaderState:
        """Open the recent book from the home screen and return a ReaderState."""
        deadline = time.monotonic() + max(timeout, 30.0)
        current = self.current_state()
        if current is not None:
            return current
        for _ in range(2):
            result = self._open.attempt_open(
                self.HOME_APP, self.current_state, deadline,
            )
            if result is not None:
                return result
        current = self.current_state()
        if current is not None:
            return current
        raise TimeoutError("failed to open reader from home after two attempts")

    def open_menu(
        self, state: ReaderState, timeout: float = 10.0,
    ) -> ReaderMenuState:
        """Invoke a reader control gesture and wait for menu activity."""
        deadline = time.monotonic() + timeout
        ctx = _MenuContext(state=state, before_hash=self.session.framebuffer_hash())
        failures: list[str] = []
        for attempt in range(3):
            result = self._try_menu_attempt(attempt, deadline, ctx)
            if isinstance(result, ReaderMenuState):
                self._settle_menu_state(deadline)
                return result
            failures.append(result)
            time.sleep(0.1)
        raise TimeoutError("reader control gesture failed: " + " | ".join(failures))

    def turn_page(
        self, state: ReaderState, timeout: float = 10.0,
    ) -> PageTurnResult:
        """Advance one page and verify a framebuffer change or log activity."""
        deadline = time.monotonic() + timeout
        for attempt in range(3):
            result = self._try_page_turn(attempt, state, deadline)
            if result is not None:
                return result
        raise TimeoutError("reader page did not advance after all input methods")

    def exit(self, timeout: float = 12.0) -> ReaderExitResult:
        """Try to leave the reader and capture the resulting task/log state."""
        deadline = time.monotonic() + timeout
        tasks_before = self.session.emulator.list_tasks(
            timeout=min(2.0, remaining(deadline)),
        )
        ctx = _ExitContext(
            before_hash=self.session.framebuffer_hash(),
            log_offset=self.session.mark_log(),
            existing_keys={(t.task_id, t.name) for t in tasks_before},
        )
        self.actions.leave_reader(deadline)
        return poll_until(
            lambda: self._poll_exit(ctx, deadline),
            timeout=remaining(deadline),
            interval=0.2,
            timeout_message=lambda detail: (
                f"reader exit produced no home/control-surface signal"
                f" within timeout; detail={detail}"
            ),
        )

    # --- attempt helpers ----------------------------------------------

    def _try_menu_attempt(
        self, attempt: int, deadline: float, ctx: _MenuContext,
    ) -> ReaderMenuState | str:
        """One attempt at invoking the menu; returns the result or failure text."""
        log_offset = self.session.mark_log()
        self.actions.invoke_menu(attempt, deadline)
        try:
            return poll_until(
                lambda: self._menu_poll(ctx, log_offset),
                timeout=min(MENU_TIMEOUT, remaining(deadline)),
                interval=0.2,
                timeout_message=lambda detail: (
                    f"reader control gesture produced no menu activity"
                    f" within timeout; tail_len={detail}"
                ),
            )
        except TimeoutError as exc:
            return str(exc)

    def _menu_poll(self, ctx: _MenuContext, log_offset: int) -> ReaderMenuState:
        """One framebuffer+log sample for the menu-wait polling loop."""
        final_task = self.session.active_task()
        current_hash = self.session.framebuffer_hash()
        log_text = self.session.logs_since(log_offset)
        matched = find_matching_markers(log_text, MENU_ACTIVITY_PATTERNS)
        if not self._menu_surface_app(final_task.appname):
            retry_later(final_task.appname or "unknown-active-task")
        if (
            not app_name_matches(final_task.appname, self.READER_APP)
            and self._reader_task_entry() is None
        ):
            retry_later("reader-task-missing")
        if not matched and current_hash == ctx.before_hash:
            retry_later(str(len(log_text)))
        return ReaderMenuState(
            reader_state=ctx.state,
            final_task=final_task,
            before_hash=ctx.before_hash,
            after_hash=current_hash,
            log_text=log_text,
            matched_markers=matched,
        )

    def _menu_surface_app(self, appname: str) -> bool:
        """Return True when *appname* is reader or known menu/control UI."""
        if app_name_matches(appname, self.READER_APP):
            return True
        return any(app_name_matches(appname, app) for app in self.EXIT_CONTROL_APPS)

    def _try_page_turn(
        self, attempt: int, state: ReaderState, deadline: float,
    ) -> PageTurnResult | None:
        """One attempt at a page turn; returns a PageTurnResult or None."""
        before_hash = self.session.framebuffer_hash()
        log_offset = self.session.mark_log()
        self.actions.next_page(attempt, deadline)
        after_hash = self._wait_hash_change(before_hash, deadline)
        log_text, matched = self._wait_page_markers(log_offset, deadline)
        if after_hash == before_hash and not matched:
            return None
        reader_task = self._current_reader_task()
        if reader_task is None:
            return None
        return PageTurnResult(
            reader_state=replace(state, reader_task=reader_task),
            before_hash=before_hash,
            after_hash=after_hash,
            log_text=log_text,
            matched_markers=matched,
        )

    def _wait_hash_change(self, before_hash: str, deadline: float) -> str:
        """Try to observe a framebuffer change; return *before_hash* on timeout."""
        try:
            return self.session.wait_for_framebuffer_change(
                before_hash,
                timeout=min(FRAMEBUFFER_SETTLE, remaining(deadline)),
            )
        except TimeoutError:
            return before_hash

    def _wait_page_markers(
        self, log_offset: int, deadline: float,
    ) -> tuple[str, tuple[str, ...]]:
        """Wait for page-activity markers; fall back to empty on timeout."""
        try:
            return self.session.wait_for_log_markers(
                PAGE_ACTIVITY_PATTERNS,
                since=log_offset,
                timeout=min(PAGE_TIMEOUT, remaining(deadline)),
            )
        except TimeoutError:
            return self.session.logs_since(log_offset), ()

    def _settle_menu_state(self, deadline: float) -> None:
        """Give menu transitions a brief chance to finish before cleanup taps."""
        if remaining(deadline) <= 0:
            return
        try:
            self.session.emulator.wait_for_monitor_quiet(
                timeout=min(1.0, remaining(deadline)),
                quiet_period=0.2,
            )
        except TimeoutError:
            return

    def _poll_exit(self, ctx: _ExitContext, deadline: float) -> ReaderExitResult:
        """Single polling attempt for exit confirmation."""
        exit_log = self.session.logs_since(ctx.log_offset)
        matched = find_matching_markers(exit_log, EXIT_ACTIVITY_PATTERNS)
        final_task = self.session.active_task()
        returned_home = self._returned_home(final_task.appname)
        if not returned_home and not matched:
            retry_later(final_task.appname or str(len(exit_log)))
        new_control = self._collect_new_control_tasks(ctx.existing_keys, deadline)
        return ReaderExitResult(
            before_hash=ctx.before_hash,
            after_hash=self.session.framebuffer_hash(),
            exit_log=exit_log,
            final_task=final_task,
            returned_home=returned_home,
            control_surface_seen=True,
            matched_markers=matched,
            new_control_tasks=new_control,
        )

    def _collect_new_control_tasks(
        self, existing_keys: set[tuple[int, str]], deadline: float,
    ) -> tuple:
        """Return tasks that appeared after exit and look like control surfaces."""
        final_tasks = self.session.emulator.list_tasks(
            timeout=min(2.0, remaining(deadline)),
        )
        return tuple(
            task for task in final_tasks
            if (task.task_id, task.name) not in existing_keys
            and any(
                app_name_matches(task.name, app) for app in self.EXIT_CONTROL_APPS
            )
        )

    def _returned_home(self, appname: str) -> bool:
        """Return True when *appname* matches a home surface but not the reader."""
        if app_name_matches(appname, self.READER_APP):
            return False
        return any(app_name_matches(appname, app) for app in self.HOME_SURFACE_APPS)
