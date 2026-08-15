"""pytest plugin collecting per-test results for the Playwright-style report.

Registered from ``tests/conftest.py`` (pytest_configure).  For every test
that actually runs it records the final status, duration, failure text and
the per-action screenshot steps harvested from the active SnapshotRecorder
(whose teardown/FAILED capture has already been written by the time the
teardown report fires), then writes ``build/report/results.json`` at session
finish — the input for ``scripts/gen_report.py``.

The plugin is deliberately defensive: a bug here must never break the
suite, so every hook body swallows exceptions.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

import pytest

from tests.support.bookshelf import snapshots as _snapshots

_REPO_ROOT = Path(__file__).resolve().parents[3]
_REPORT_DIR = _REPO_ROOT / "build" / "report"

_session_started = 0.0
_session_epoch = 0.0
_records: list[dict] = []
_ran_any = False


# -- hook implementations -------------------------------------------------

@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call) -> None:
    """Collect per-item outcomes; finalize (steps + attempts) at teardown."""
    outcome = yield
    try:
        _on_report(item, call, outcome.get_result())
    except Exception:  # noqa: BLE001 — never break the suite
        pass


def pytest_sessionstart(session) -> None:
    """Anchor the wall-clock start of the run."""
    global _session_started, _session_epoch
    try:
        _session_started = time.monotonic()
        _session_epoch = time.time()
    except Exception:  # noqa: BLE001
        pass


def pytest_sessionfinish(session, exitstatus) -> None:
    """Write build/report/results.json once the suite has finished.

    Only written when the session actually ran tests (never for
    ``--collect-only``), so a collection sanity check cannot clobber a
    previous run's report.
    """
    try:
        if _ran_any:
            _write_results(session)
    except Exception:  # noqa: BLE001
        pass


# -- report plumbing ------------------------------------------------------

def _on_report(item: pytest.Item, call, report) -> None:
    if call.when == "setup":
        # Setup failure/skip: no call phase follows, remember it so the
        # teardown finalize can still produce a failed/skipped entry.
        item._report_started_at = call.start  # type: ignore[attr-defined]
        if report.failed or report.skipped:
            item._report_setup = {  # type: ignore[attr-defined]
                "status": report.outcome,
                "duration_s": report.duration,
                "error": _error_text(report),
            }
        return
    if call.when == "call":
        if not hasattr(item, "_report_started_at"):  # no setup phase
            item._report_started_at = call.start  # type: ignore[attr-defined]
        item._report_finished_at = call.stop  # type: ignore[attr-defined]
        item._report_entry = {  # type: ignore[attr-defined]
            "status": report.outcome,
            "duration_s": report.duration,
            "error": _error_text(report),
        }
        return
    if call.when == "teardown":
        item._report_finished_at = call.stop  # type: ignore[attr-defined]
        # Fires after fixture teardown, so the recorder already holds the
        # teardown/FAILED capture of the finished test.
        _finalize(item, report)


def _finalize(item: pytest.Item, teardown_report) -> None:
    global _ran_any
    _ran_any = True

    call_info = getattr(item, "_report_entry", None)
    setup_info = getattr(item, "_report_setup", None)

    status = "passed"
    error = None
    if setup_info is not None and setup_info["status"] == "failed":
        status, error = "failed", setup_info["error"]
    if call_info is not None and call_info["status"] == "failed":
        status, error = "failed", call_info["error"]
    if teardown_report.failed and status != "failed":
        status, error = "failed", _error_text(teardown_report)
    if status == "passed":
        if call_info is not None and call_info["status"] == "skipped":
            status, error = "skipped", call_info["error"]
        elif setup_info is not None and setup_info["status"] == "skipped":
            status, error = "skipped", setup_info["error"]

    if call_info is not None:
        duration = call_info["duration_s"]
    elif setup_info is not None:
        duration = setup_info["duration_s"]
    else:
        duration = teardown_report.duration

    steps = _harvest_steps(item.name)

    started_at = getattr(item, "_report_started_at", None)
    finished_at = getattr(item, "_report_finished_at", None)
    if started_at is None:
        started_at = _session_epoch or None  # session anchor fallback
    if finished_at is None:
        finished_at = duration + started_at if started_at is not None else None

    record = {
        "id": item.nodeid,
        "file": item.nodeid.split("::")[0],
        "line": item.location[1] or 0,
        "title": item.name,
        "status": status,
        "duration_s": round(duration, 2),
        "started_at": started_at,
        "finished_at": finished_at,
        "error": error,
        "attempts": [
            {
                "status": status,
                "duration_s": round(duration, 2),
                "steps": steps,
            }
        ],
    }
    # Invocation ordinal range in the accumulated bookshelf log (set by
    # the fresh_bookshelf / scale_env fixtures); lets the report slicer
    # cut per-test logs at exact invocation boundaries.
    open_start = getattr(item, "_bs_log_open_start", None)
    open_end = getattr(item, "_bs_log_open_end", None)
    if isinstance(open_start, int) and isinstance(open_end, int):
        record["log_open_start"] = open_start
        record["log_open_end"] = open_end
    item._report_entry = record  # type: ignore[attr-defined]
    _records.append(record)


def _error_text(report) -> str | None:
    """Formatted failure text (traceback tail) or the skip reason, if any."""
    if report.longrepr is None:
        return None
    # Skipped tests carry (fspath, lineno, reason); unwrap to the reason.
    if isinstance(report.longrepr, tuple) and len(report.longrepr) == 3:
        return str(report.longrepr[2]) or None
    return report.longreprtext or None


def _safe_dir_name(name: str) -> str:
    """Mirror of SnapshotRecorder.begin()'s per-test directory sanitization."""
    return "".join(c if c.isalnum() or c in "._-" else "_" for c in name)


def _harvest_steps(test_name: str) -> list[dict]:
    """Screenshot steps of *test_name* from the active recorder, as
    destination-relative paths (``screenshots/<test>/NNN-label.png``).

    The recorder only matches when it was begun for this exact test, so a
    test that did not use the bookshelf fixture never inherits the previous
    test's steps.
    """
    rec = _snapshots.ACTIVE
    if rec is None or not rec.active or rec._dir is None:  # type: ignore[attr-defined]
        return []
    if rec._dir.name != _safe_dir_name(test_name):  # type: ignore[attr-defined]
        return []
    dir_name = rec._dir.name  # type: ignore[attr-defined]
    return [
        {**step, "png": f"screenshots/{dir_name}/{step['png']}"}
        for step in rec.entries()
    ]


def _write_results(session) -> None:
    _REPORT_DIR.mkdir(parents=True, exist_ok=True)
    payload = {
        "generated_at": datetime.now(timezone.utc)
        .replace(microsecond=0)
        .isoformat(),
        "total_time_s": round(time.monotonic() - _session_started, 1),
        "firmware": os.environ.get("PB_TEST_FIRMWARE", ""),
        "commit": _git_commit(),
        "tests": _records,
    }
    # Each xdist worker writes its own slice; a suffix keeps them from
    # clobbering build/report/results.json (serial runs keep the plain
    # name for the existing report consumer).
    (_REPORT_DIR / f"results{_worker_suffix(session)}.json").write_text(
        json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )


def _worker_suffix(session) -> str:
    """'.gw0'/'gw1'/... when running as an xdist worker, else ''."""
    workerinput = getattr(getattr(session, "config", None), "workerinput", None)
    if workerinput is not None:
        wid = workerinput.get("workerid")
        if wid and wid != "master":
            return f".{wid}"
    return ""


def _git_commit() -> str:
    """Short HEAD commit hash, best-effort ("" when unavailable)."""
    try:
        proc = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=str(_REPO_ROOT),
            capture_output=True,
            text=True,
            timeout=10,
        )
        return proc.stdout.strip()
    except Exception:  # noqa: BLE001
        return ""
