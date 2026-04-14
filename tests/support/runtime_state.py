"""Foreground-task and informer-state helpers for runtime tests."""

from __future__ import annotations

from typing import NamedTuple

from .polling import poll_until, retry_later
from .runtime_common import parse_key_value_fields, parse_optional_int
from .runtime_container import container_sh
from .runtime_probes import run_probe

__all__ = [
    "ActiveTaskInfo",
    "InformerSnapshot",
    "TaskEntry",
    "list_tasks",
    "read_task_info",
    "wait_for_active_task_info",
    "wait_for_informer_snapshot",
]

_REQUIRED_SNAPSHOT_FIELDS = (
    "task_id",
    "fb_key",
    "fb_size",
    "width",
    "height",
    "depth",
    "stride",
)


class InformerSnapshot(NamedTuple):
    """Parsed output of the ``frame_dump`` host probe."""

    task_id: int | None = None
    subtask_id: int | None = None
    fb_key: int | None = None
    fb_size: int | None = None
    width: int | None = None
    height: int | None = None
    depth: int | None = None
    stride: int | None = None
    pixel_data_offset: int | None = None
    fb_y_offset: int | None = None

    @classmethod
    def from_text(cls, text: str) -> "InformerSnapshot":
        """Parse one ``frame_dump`` line-oriented key/value payload."""
        fields = parse_key_value_fields(text)
        return cls(**{
            name: parse_optional_int(fields.get(name))
            for name in cls._fields
        })

    def missing_required_fields(self) -> tuple[str, ...]:
        """Return the subset of required informer fields that are still unset."""
        return tuple(
            name for name in _REQUIRED_SNAPSHOT_FIELDS
            if getattr(self, name) is None
        )


class ActiveTaskInfo(NamedTuple):
    """Foreground task information used by the live tests."""

    active_task_id: int
    active_subtask_id: int
    appname: str = ""
    mainpid: int | None = None
    flags: int | None = None
    nsubtasks: int | None = None
    fb_shmkey: int | None = None
    fb_shmsize: int | None = None


class TaskEntry(NamedTuple):
    """One task row parsed from ``/var/run/task``."""

    task_id: int
    pid: str
    name: str


def wait_for_informer_snapshot(timeout: float = 30.0) -> InformerSnapshot:
    """Poll until ``frame_dump`` exposes a complete framebuffer snapshot."""
    last_detail = ""

    def _attempt() -> InformerSnapshot:
        nonlocal last_detail
        result = run_probe("frame_dump", check=False, timeout=min(timeout, 10.0))
        if result.returncode != 0:
            last_detail = result.stderr.strip()
            retry_later(last_detail)
        snapshot = InformerSnapshot.from_text(result.stdout)
        missing = snapshot.missing_required_fields()
        if missing:
            last_detail = ",".join(missing)
            retry_later(last_detail)
        return snapshot

    return poll_until(
        _attempt,
        interval=0.25,
        timeout=timeout,
        timeout_message=lambda missing: (
            f"frame_dump did not expose a valid snapshot within {timeout}s;"
            f" {missing or last_detail}"
        ),
    )


def read_task_info(*, timeout: float = 5.0) -> ActiveTaskInfo:
    """Return the current foreground task snapshot plus parsed task metadata."""
    snapshot = wait_for_informer_snapshot(timeout=timeout)
    task_id = snapshot.task_id if snapshot.task_id is not None else -1
    subtask_id = snapshot.subtask_id if snapshot.subtask_id is not None else -1
    fields = _read_task_fields(task_id, timeout=timeout)
    return ActiveTaskInfo(
        active_task_id=task_id,
        active_subtask_id=subtask_id,
        appname=fields.get("appname", ""),
        mainpid=parse_optional_int(fields.get("mainpid")),
        flags=parse_optional_int(fields.get("flags")),
        nsubtasks=parse_optional_int(fields.get("nsubtasks")),
        fb_shmkey=snapshot.fb_key,
        fb_shmsize=snapshot.fb_size,
    )


def wait_for_active_task_info(timeout: float = 30.0) -> ActiveTaskInfo:
    """Poll until active-task metadata contains both id and app name."""

    sample_timeout = min(5.0, max(1.0, timeout / 4.0))
    last_detail = ""

    def _attempt() -> ActiveTaskInfo:
        nonlocal last_detail
        try:
            info = read_task_info(timeout=sample_timeout)
        except TimeoutError as exc:
            last_detail = str(exc)
            retry_later(last_detail)
        if info.active_task_id <= 0 or not info.appname:
            last_detail = repr(info)
            retry_later(last_detail)
        return info

    return poll_until(
        _attempt,
        interval=0.2,
        timeout=timeout,
        timeout_message=lambda detail: (
            f"active task info not ready within {timeout}s;"
            f" {detail or last_detail}"
        ),
    )


_LIST_TASKS_SCRIPT = r"""
if [ ! -d /var/run/task ]; then
    exit 0
fi
for task_dir in /var/run/task/[0-9]*; do
    [ -d "$task_dir" ] || continue
    id=${task_dir##*/}
    pid='?'
    name='-'
    [ -r "$task_dir/mainpid" ] && pid=$(tr '\n' ' ' < "$task_dir/mainpid") && pid=${pid% }
    [ -r "$task_dir/appname" ] && name=$(tr '\n' ' ' < "$task_dir/appname") && name=${name% }
    printf 'task %s pid=%s name=%s\n' "$id" "$pid" "$name"
done
"""


def list_tasks(*, timeout: float = 5.0) -> tuple[TaskEntry, ...]:
    """Return the current ``/var/run/task`` directory as sorted entries."""
    result = container_sh(_LIST_TASKS_SCRIPT, check=False, timeout=timeout)
    return _parse_task_rows(result.stdout)


def _read_task_fields(task_id: int, *, timeout: float = 5.0) -> dict[str, str]:
    """Read selected task metadata files for one active task id."""
    if task_id <= 0:
        return {}
    script = f"""
task_dir=/var/run/task/{task_id}
[ -d "$task_dir" ] || exit 0
for field in appname mainpid flags nsubtasks; do
    if [ -r "$task_dir/$field" ]; then
        value=$(tr '\\n' ' ' < "$task_dir/$field")
        value=${{value% }}
        printf '%s=%s\\n' "$field" "$value"
    fi
done
"""
    result = container_sh(script, check=False, timeout=timeout)
    return parse_key_value_fields(result.stdout)


def _parse_task_rows(text: str) -> tuple[TaskEntry, ...]:
    """Parse ``list_tasks`` shell output into sorted rows."""
    tasks: list[TaskEntry] = []
    for line in text.splitlines():
        if not line.startswith("task "):
            continue
        parts = line.split(maxsplit=3)
        if len(parts) != 4:
            continue
        tasks.append(
            TaskEntry(
                task_id=int(parts[1], 10),
                pid=parts[2].split("=", 1)[1] if "=" in parts[2] else "",
                name=parts[3].split("=", 1)[1] if "=" in parts[3] else "",
            )
        )
    tasks.sort(key=lambda item: item.task_id)
    return tuple(tasks)
