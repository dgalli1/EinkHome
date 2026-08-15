"""Scale e2e: a 100k-book mock library syncs into the SQLite-backed
bookshelf without unbounded RAM, and the paged grid stays navigable.

The mock provider layers ``PBEMU_MOCK_COUNT`` deterministic synthetic
books over the books dir (every 5th book joins its block's series), so
the collapsed view holds ~2 tiles per 5 books.  The guest store is
wiped before the run so the sync ingests the full library from
cursor 0; afterwards the guest's VmRSS must stay far below what an
in-memory 100k-book model would need, and the pager must reach the
last of several thousand pages.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time

import pytest

from tests.support.bookshelf import BookshelfGeometry, BookshelfSession
from tests.support.bookshelf.env import (
    API_TOKEN,
    CONTAINER,
    EINKHOME_ROOT,
    FIRMWARE,
    PBEMU_ROOT,
    PODMAN,
    _OFFLINE_DIR,
    _OFFLINE_STORE,
    _build_bookshelf,
    _parse_panel_h,
    _restore_cfg_file,
    _snapshot_cfg,
    _stage_binary,
    _start_emulator,
    _stop_api_server,
    _wait_bookshelf_active,
)
from tests.support.reader.session import Session
from tests.support.runtime import container_running, container_sh
from tests.support import ui_input as _UI_INPUT
from tests.support.bookshelf.backends import EmulatorBackend

SCALE_PORT = 18766
SCALE_COUNT = 100_000
SERIES_SIZE = 5
# Collapsed tiles: one flat tile per block's standalone book plus one
# series card per block (books-dir books add a few more).
EXPECTED_TILES = SCALE_COUNT // SERIES_SIZE * 2
_PAGESIZE = 6  # COLS * ROWS, must match the app geometry

pytestmark = pytest.mark.bookshelf


def _scale_api_env() -> dict[str, str]:
    """API server env with the mock provider scaled to 100k books."""
    env = os.environ.copy()
    api_dir = str(EINKHOME_ROOT / "api")
    root = str(EINKHOME_ROOT)
    env["PYTHONPATH"] = f"{root}{os.pathsep}{api_dir}"
    env["PBEMU_MOCK_COUNT"] = str(SCALE_COUNT)
    env["PBEMU_MOCK_SERIES_SIZE"] = str(SERIES_SIZE)
    # Absolute path: the server runs with cwd=PBEMU_ROOT, where the
    # config's relative ".cover-cache/..." would not resolve.  Env wins
    # over the config corpus key.
    env["PBEMU_MOCK_CORPUS"] = str(EINKHOME_ROOT / ".cover-cache" / "mock_books.jsonl")
    return env


def _start_scale_api() -> subprocess.Popen:  # type: ignore[type-arg]
    log_path = EINKHOME_ROOT / "build" / "pbemu-api-scale.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_fh = open(log_path, "w", encoding="utf-8")  # noqa: SIM115
    proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "api.api.server",
            "--config",
            str(EINKHOME_ROOT / "api" / "config" / "server-100k.json"),
            "--host",
            "0.0.0.0",
            "--port",
            str(SCALE_PORT),
            "--provider",
            "mock",
        ],
        # Server code lives in this repo (api/ on PYTHONPATH); cwd stays
        # the submodule so firmware-relative config paths resolve.
        cwd=PBEMU_ROOT,
        env=_scale_api_env(),
        stdout=log_fh,
        stderr=subprocess.STDOUT,
    )
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        try:
            import urllib.request

            req = urllib.request.Request(
                f"http://127.0.0.1:{SCALE_PORT}/api/v1/healthz",
                headers={"Authorization": f"Bearer {API_TOKEN}"},
            )
            with urllib.request.urlopen(req, timeout=2) as resp:
                body = json.loads(resp.read().decode("utf-8"))
        except Exception:  # noqa: BLE001
            time.sleep(0.3)
            continue
        # Same stale-listener guard as the main e2e fixture: a server
        # left over from an interrupted run may still answer on the
        # scale port while our fresh process never bound it.  Kill the
        # stale one and fail loudly instead of testing dead code.
        if body.get("pid") != proc.pid:
            stale_pid = body.get("pid")
            if isinstance(stale_pid, int) and stale_pid > 0:
                try:
                    os.kill(stale_pid, signal.SIGTERM)
                    time.sleep(0.3)
                    os.kill(stale_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except PermissionError:
                    pass
            proc.kill()
            raise RuntimeError(
                f"stale scale API server (pid {stale_pid}) answered on port "
                f"{SCALE_PORT} instead of the freshly spawned server "
                f"(pid {proc.pid}); killed the stale listener. Log:\n"
                f"{log_path.read_text()}"
            )
        return proc
    proc.kill()
    raise RuntimeError(
        f"scale API server did not start within 15s. Log:\n{log_path.read_text()}"
    )


def _stage_scale() -> None:
    """Stage binary + a config pointing at the scale server, and wipe
    the guest store + any /tmp config override so the 100k sync starts
    from cursor 0 against the scale port."""
    _stage_binary(_build_bookshelf())
    bin_dir = PBEMU_ROOT / FIRMWARE / ".live" / "mnt/ext1/system/bin"
    (bin_dir / "bookshelf.cfg").write_text(
        f"api_url=http://127.0.0.1:{SCALE_PORT}\napi_token={API_TOKEN}\n",
        encoding="utf-8",
    )
    if container_running():
        subprocess.run(
            [
                PODMAN,
                "cp",
                str(bin_dir / "bookshelf.cfg"),
                f"{CONTAINER}:/mnt/ext1/system/bin/bookshelf.cfg",
            ],
            check=False,
            capture_output=True,
            timeout=5,
        )
        container_sh(
            "rm -f /tmp/bookshelf.cfg /tmp/bookshelf_lib.db "
            "/tmp/bookshelf_lib.db-journal "
            "/mnt/ext1/system/bin/bookshelf_lib.db "
            "/mnt/ext1/system/bin/bookshelf_lib.db-journal",
            check=False,
            timeout=5,
        )


def _guest_rss_kb() -> int:
    """VmRSS of the guest bookshelf process (kB), -1 if not running."""
    out = container_sh(
        "for f in $(grep -l bookshelf.app /proc/[0-9]*/cmdline 2>/dev/null); do "
        "grep VmRSS ${f%cmdline}status; done",
        check=False,
        timeout=10,
    )
    m = re.search(r"VmRSS:\s+(\d+) kB", out.stdout or "")
    return int(m.group(1)) if m else -1


def _pager_roundtrip(bs: BookshelfSession, view: int) -> None:
    """Jump to the last of several thousand pages and back to the first."""
    pages = (view + _PAGESIZE - 1) // _PAGESIZE
    assert pages > 1000, f"expected thousands of pages, got {pages}"
    snap = bs.current_log()
    before = bs.frame_hash()
    bs.tap_pager_last()
    bs.wait_hash_change(before)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if f"page={pages - 1}" in bs.current_log()[len(snap):]:
            break
        time.sleep(0.5)
    else:
        raise AssertionError(f"last page {pages - 1} never rendered")
    before = bs.frame_hash()
    bs.tap_pager_first()
    bs.wait_hash_change(before)


@pytest.fixture(scope="module")
def scale_env(request):
    """API (100k mock) + staged binary + emulator, store wiped."""
    if shutil.which(PODMAN) is None:
        pytest.skip(f"{PODMAN} not available")

    # Snapshot the dev cfgs before staging rewrites them to the scale
    # port, so teardown can restore the state the main e2e fixture
    # expects — a leftover scale-pointing cfg or the 100k store on disk
    # would poison a later manual `pbemu start` / e2e run.
    live = PBEMU_ROOT / FIRMWARE / ".live"
    bin_cfg = live / "mnt/ext1/system/bin/bookshelf.cfg"
    tmp_cfg = live / "tmp/bookshelf.cfg"
    saved_bin_cfg = _snapshot_cfg(bin_cfg)
    saved_tmp_cfg = _snapshot_cfg(tmp_cfg)

    api = None
    emulator = None
    try:
        api = _start_scale_api()
        _stage_scale()
        emulator = _start_emulator()
        _stage_scale()  # container-side copies now that it is running
        container_sh(
            "killall bookshelf.app 2>/dev/null || true", check=False, timeout=5
        )
        _wait_bookshelf_active(emulator, timeout=60)

        snapshot = emulator.wait_for_informer_snapshot(timeout=10)
        geom = BookshelfGeometry(
            screen_w=snapshot.width or 1072,
            screen_h=snapshot.height or 1448,
            panel_h=_parse_panel_h(FIRMWARE),
        )
        backend = EmulatorBackend(
            emulator, FIRMWARE, session_cls=Session, ui_input=_UI_INPUT
        )
        bs = BookshelfSession(backend, geom, FIRMWARE)
        request.node._bs_log_open_start = bs.invocation_count()  # type: ignore[attr-defined]
        bs.begin_snapshots(request.node.name)
        bs.snapshot("boot")

        yield bs, emulator, api
    except BaseException:
        # A setup step failed part-way: stop whatever already started so
        # no stale listener leaks on the scale port (an emulator-start
        # failure would otherwise leave the API server bound to 18766
        # for a later run to adopt), and restore the dev cfgs the
        # staging rewrote, before propagating.
        if emulator is not None:
            emulator.stop(force=True)
        if api is not None:
            _stop_api_server(api)
        _restore_cfg_file(bin_cfg, saved_bin_cfg)
        _restore_cfg_file(tmp_cfg, saved_tmp_cfg, mode=0o666)
        raise

    request.node._bs_log_open_end = bs.invocation_count()  # type: ignore[attr-defined]

    report = getattr(request.node, "_bs_call_report", None)
    bs.snapshot("FAILED" if report is not None and report.failed else "teardown")
    bs.finish_snapshots()

    # Restore the dev cfgs first (the running guest holds the stale
    # scale-port URL in memory; stopping the emulator before restoring
    # could let a shutdown settings-save re-poison the files), then stop
    # the servers and drop the staged 100k store so the next boot is
    # clean.
    _restore_cfg_file(bin_cfg, saved_bin_cfg)
    _restore_cfg_file(tmp_cfg, saved_tmp_cfg, mode=0o666)
    _stop_api_server(api)
    emulator.stop(force=True)
    _OFFLINE_STORE.unlink(missing_ok=True)
    (_OFFLINE_DIR / "bookshelf_lib.db-journal").unlink(missing_ok=True)


def _last_view(log: str) -> int:
    m = None
    for m in re.finditer(r"view_rebuild: view=(\d+)", log):
        pass
    return int(m.group(1)) if m else 0


def _proc_rss_kb(pid: int) -> int:
    """VmRSS of a host process (kB), -1 when it is gone."""
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS"):
                    return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return -1


def test_scale_100k_sync_paging_memory(scale_env):
    """Full 100k sync completes, the pager reaches the last of several
    thousand pages, and guest RAM stays bounded."""
    bs, _, api = scale_env

    # The sync ingests 100k rows in 500-row rounds; wait for the final
    # view_rebuild to report the collapsed tile count.
    deadline = time.monotonic() + 600
    view = 0
    while time.monotonic() < deadline:
        view = _last_view(bs.current_log())
        if view >= EXPECTED_TILES:
            break
        time.sleep(2.0)
    assert view >= EXPECTED_TILES, (
        f"100k sync never completed (view={view}, expected >= {EXPECTED_TILES})"
    )

    # Pager: jump to the last of several thousand pages and back.
    _pager_roundtrip(bs, view)

    # Memory: bounded regardless of library size.  An in-memory model
    # of 100k books would need ~100MB; the paged store must stay far
    # below that.
    rss = _guest_rss_kb()
    print(f"\n[bookshelf-scale] guest VmRSS with 100k books: {rss} kB")
    assert rss > 0, "guest bookshelf process not found"
    assert rss < 64 * 1024, f"guest VmRSS {rss} kB exceeds 64MB with 100k books"

    # The API server must stay lean even while holding the 100k
    # catalogue (lazy corpus + bounded ledger): 100MB is the pipeline
    # budget so the suite can run on a small runner.
    srv_rss = _proc_rss_kb(api.pid)
    print(f"[bookshelf-scale] API server VmRSS with 100k books: {srv_rss} kB")
    assert srv_rss > 0, "scale API server process not found"
    assert srv_rss < 100 * 1024, (
        f"API server VmRSS {srv_rss} kB exceeds the 100MB budget at 100k books"
    )

    bs.assert_no_crash()
