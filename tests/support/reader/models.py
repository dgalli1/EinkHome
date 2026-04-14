"""Result dataclasses for reader-flow scenarios."""

from __future__ import annotations

from dataclasses import dataclass

from tests.support.runtime import ActiveTaskInfo, Emulator, TaskEntry


@dataclass(frozen=True)
class ReaderState:
    """Snapshot recorded after the reader has been opened."""

    emulator: Emulator
    log_offset: int
    home_task: ActiveTaskInfo
    reader_task: ActiveTaskInfo
    opened_book_path: str
    open_log: str
    open_markers: tuple[str, ...]


@dataclass(frozen=True)
class ReaderMenuState:
    """Result of an attempt to invoke the reader menu."""

    reader_state: ReaderState
    final_task: ActiveTaskInfo
    before_hash: str
    after_hash: str
    log_text: str
    matched_markers: tuple[str, ...]


@dataclass(frozen=True)
class PageTurnResult:
    """Result of a page-turn attempt."""

    reader_state: ReaderState
    before_hash: str
    after_hash: str
    log_text: str
    matched_markers: tuple[str, ...]


@dataclass(frozen=True)
class ReaderExitResult:
    """Result of an attempt to exit the reader."""

    before_hash: str
    after_hash: str
    exit_log: str
    final_task: ActiveTaskInfo
    returned_home: bool
    control_surface_seen: bool
    matched_markers: tuple[str, ...]
    new_control_tasks: tuple[TaskEntry, ...]
