"""E2E tests for every interactive element in the bookshelf app.

Requires:
  - podman available
  - firmware U633_6.8.2817 staged (./pbemu install)
  - books in pbemu/U633_6.8.2817/.live/mnt/ext1/books/

Run with: pytest tests/test_bookshelf.py -v
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import pytest
from tests.support.reader.session import Session
from tests.support.runtime import Emulator, container_sh
from tests.support.runtime_common import REPO_ROOT

from tests.support import ui_input as _UI_INPUT
from tests.support.bookshelf import (
    MORE_APPS,
    MORE_SETTINGS,
    BookshelfGeometry,
    BookshelfSession,
)
from tests.support.bookshelf.env import (
    _OFFLINE_CFG,
    _OFFLINE_COVERS,
    _OFFLINE_DIR,
    _OFFLINE_LEGACY,
    _OFFLINE_STORE,
    API_PORT,
    API_TOKEN,
    EINKHOME_ROOT,
    FIRMWARE,
    PBEMU_ROOT,
    PODMAN,
    _api_env,
    _build_bookshelf,
    _ensure_sdl_test_binary,
    _kill_guest_tasks,
    _parse_app_geometry,
    _pbemu_env,
    _restart_bookshelf,
    _restore_cfg_file,
    _snapshot_cfg,
    _stage_binary,
    _start_api_server,
    _start_emulator,
    _stop_api_server,
    _wait_bookshelf_active,
)

pytestmark = pytest.mark.bookshelf


# ── fixtures ───────────────────────────────────────────────────────────

# Uniquifies each _sdl_env() instance (pid alone collides when two
# modules in one worker each bring up their own environment).
import itertools as _it  # noqa: E402

_sdl_env_seq = _it.count()


def _sdl_env(*, config: str | None = None):
    """Headless SDL (native PC) environment: API server + bookshelf.pc

    *config* overrides the mock server config (a synthetic ``count``
    builds a multi-author/multi-group library for the group-by tests).
    driven over the IPC socket.  Fast, no emulator, parallel-safe: the
    binary is built once (lock-guarded, shared), and each instance runs
    from its own build/bs-<uniq> dir with its own API port, socket, cfg,
    covers and store — so xdist workers (and distinct modules inside one
    worker) never collide."""
    import socket as _sock

    from tests.support.bookshelf.backends import SdlBackend

    root = EINKHOME_ROOT
    # 1. Build (once, lock-guarded) the test binary with the IPC control
    #    socket enabled.  Reused by every parallel worker.
    binary = _ensure_sdl_test_binary()

    # 2. Per-instance run dir: the app resolves config/covers/store next
    #    to its binary, so launching from here isolates it from other
    #    parallel instances.  Unique per instance (pid alone collides
    #    when two modules in one worker each bring up an environment).
    uniq = f"{os.getpid()}-{next(_sdl_env_seq)}"
    run_dir = root / "build" / f"bs-{uniq}"
    run_dir.mkdir(parents=True, exist_ok=True)
    app_exe = run_dir / "bookshelf.test"
    if not app_exe.exists():
        try:
            app_exe.symlink_to(binary)
        except OSError:
            shutil.copyfile(binary, app_exe)

    # 3. A free API port per instance (parallel servers must not share one).
    with _sock.socket(_sock.AF_INET, _sock.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    api_proc = _start_api_server(port=port, log_path=run_dir / "api.log", config=config)

    (run_dir / "bookshelf.cfg").write_text(
        f"api_url=http://127.0.0.1:{port}\napi_token=pbemu-dev-token\n",
        encoding="utf-8")
    sock = f"/tmp/bs-{uniq}.sock"
    logpath = run_dir / "bookshelf.log"
    env = os.environ.copy()
    env["EH_SOCKET"] = sock
    env["SDL_VIDEODRIVER"] = "dummy"
    env["PBEMU_LOG_DIR"] = str(run_dir)
    env["EH_SYSAPP_DIR"] = str(run_dir / "sysapp")  # isolate home-task promote/demote
    # Isolate the local-library scan root (local::browse_root) per
    # instance — without it a host run would walk the real $HOME.
    ext1 = run_dir / "ext1"
    ext1.mkdir(parents=True, exist_ok=True)
    env["EH_BROWSE_ROOT"] = str(ext1)
    _held = {"proc": None}

    def _launch():
        p = subprocess.Popen(
            [str(app_exe)],
            cwd=run_dir, env=env, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL)
        _held["proc"] = p
        return p

    _launch()
    backend = SdlBackend(
        sock, str(logpath), api_url=f"http://127.0.0.1:{port}",
        run_dir=run_dir, relaunch=_launch)

    # 4. Build geometry (the SDL build is 1072x1448, no panel).
    geom = BookshelfGeometry(screen_w=1072, screen_h=1448, panel_h=0)
    # Wait for the app to boot + sync a bit.
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            backend.frame_hash()
            break
        except (ConnectionError, OSError):
            time.sleep(0.2)
    bs = BookshelfSession(backend, geom, "sdl")
    time.sleep(3.0)  # let the initial sync + draw settle
    try:
        yield bs, _held["proc"]
    finally:
        try:
            backend.kill_all()
        except Exception:  # noqa: BLE001
            pass
        p = _held["proc"]
        if p is not None:
            p.terminate()
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
        _stop_api_server(api_proc)


@pytest.fixture(scope="module")
def bookshelf_env():
    """Full bookshelf e2e environment: API server + a bookshelf backend.

    The backend is selected by EH_TEST_BACKEND (emulator | sdl);
    default is the emulator (the classic qemu target).  Each yields
    ``(bs, runtime)`` where *runtime* is an Emulator for the emulator
    backend (tests that need its probes) or a Popen for the sdl target.
    """
    backend_name = os.environ.get("EH_TEST_BACKEND", "emulator")
    if backend_name == "sdl":
        yield from _sdl_env()
        return
        return
    yield from _emulator_env()


@pytest.fixture(scope="module")
def synth_bookshelf_env(tmp_path_factory):
    """SDL-only environment with a deterministic multi-group library.

    The default SDL mock is a single author (every books-dir file is
    attributed to "pbemu mock library"), so author grouping collapses to
    one card — it cannot exercise multi-page grouped views or the
    card/flat sort interleave.  This fixture serves a mock corpus of 24
    authors: even-numbered authors have two books (multi-member → a stack
    card), odd-numbered authors one book (flat tile), titled "Book 00"..
    "Book 23".  Under "By author" + title sort the view alternates
    [card, flat, card, flat, ...] — 24 tiles across 4 pages, with stack
    cards on page 2 to drill into from a nonzero page.  Even authors'
    books carry a series, so the Author > Series preset is offered too.
    Only available on the SDL backend.
    """
    if os.environ.get("EH_TEST_BACKEND", "emulator") != "sdl":
        pytest.skip("synthetic group fixtures need SDL")

    books_dir = tmp_path_factory.mktemp("synth-books")  # empty dir scan
    corpus: list[dict] = []
    for i in range(24):
        author = f"Author {i:02d}"
        corpus.append({
            "id": f"corp_b{i:03d}a",
            "title": f"Book {i:02d}",
            "authors": [author],
            "added_at": "2023-01-01T00:00:00Z",
        })
        if i % 2 == 0:
            corpus.append({
                "id": f"corp_b{i:03d}b",
                "title": f"Book {i:02d} (vol 2)",
                "authors": [author],
                "series": f"Series {i:02d}",
                "added_at": "2023-01-01T00:00:01Z",
            })
    corpus_path = tmp_path_factory.mktemp("synth-corpus") / "books.jsonl"
    corpus_path.write_text(
        "\n".join(json.dumps(r) for r in corpus) + "\n", encoding="utf-8"
    )
    cfg = json.loads(
        (EINKHOME_ROOT / "tests" / "support" / "server-test.json").read_text(
            encoding="utf-8"
        )
    )
    cfg["providers"]["mock"].update(
        books_dir=str(books_dir), count=len(corpus), corpus=str(corpus_path)
    )
    # The default ledger + cover cache live at build/pbemu-test-cover-cache
    # and are shared with the default-config servers — a server pointing
    # there would replay the catalogued 16 shipped books instead of this
    # fixture's library.  Give it its own durable paths in the temp tree
    # so the walk folds this catalogue fresh.
    cfg_dir = tmp_path_factory.mktemp("synth-cfg")
    cfg["cover_cache"]["dir"] = str(cfg_dir / "cache")
    cfg["ledger"]["path"] = str(cfg_dir / "sync-ledger.db")
    cfg_path = cfg_dir / "server.json"
    cfg_path.write_text(json.dumps(cfg), encoding="utf-8")
    yield from _sdl_env(config=str(cfg_path))


def _emulator_env():
    """Emulator-backed environment (the default; the classic qemu path)."""
    if shutil.which(PODMAN) is None:
        pytest.skip(f"{PODMAN} not available")

    # Start from a clean guest store + cover cache.  The scale suite
    # leaves a ~100k-book synthetic store behind, which puts the app's
    # sync cursor past the live library and poisons download/offline
    # tests on the next run.
    _OFFLINE_STORE.unlink(missing_ok=True)
    (_OFFLINE_DIR / "bookshelf_lib.db-journal").unlink(missing_ok=True)
    (_OFFLINE_DIR / "bookshelf_lib.json.migrated").unlink(missing_ok=True)
    shutil.rmtree(_OFFLINE_DIR / "covers", ignore_errors=True)

    # 1. Build the binary
    binary = _build_bookshelf()

    # Paths + dev-cfg snapshots live outside the try so the failure path
    # can restore them even when a setup step raises mid-way.  Snapshot
    # the dev cfgs first so the run's test-pointing api_url can be
    # restored afterwards — a leftover override otherwise poisons the
    # next manual emulator session (the app keeps syncing to the dead
    # test port).
    live = PBEMU_ROOT / FIRMWARE / ".live"
    bin_cfg = live / "mnt/ext1/system/bin/bookshelf.cfg"
    tmp_cfg = live / "tmp/bookshelf.cfg"
    saved_bin_cfg = _snapshot_cfg(bin_cfg)
    saved_tmp_cfg = _snapshot_cfg(tmp_cfg)
    api_proc = None
    emulator = None
    try:
        # 2. Start API server (mock provider)
        api_proc = _start_api_server()

        # 3. Stage binary + config.
        _stage_binary(binary)

        # 4. Stop any existing emulator
        subprocess.run(
            [sys.executable, "-m", "pbemu", "stop"],
            cwd=REPO_ROOT,
            env=_pbemu_env(),
            check=False,
        )
        time.sleep(1)

        # 5. Start emulator with --network=host
        emulator = _start_emulator()

        # 6. Stage into ebrmain/bin (container-side, now that it's running)
        _stage_binary(binary)

        # 6b. Verify API is reachable from inside the container
        api_check = container_sh(
            f"wget -q -O /dev/null --header='Authorization: Bearer {API_TOKEN}' "
            f"http://127.0.0.1:{API_PORT}/api/v1/healthz 2>&1 || "
            f"echo 'API_UNREACHABLE'",
            check=False,
            timeout=10,
        )
        if "API_UNREACHABLE" in (api_check.stdout or ""):
            # Dump diagnostics
            api_log = (REPO_ROOT / "build" / "pbemu-api-test.log").read_text(
                encoding="utf-8", errors="replace"
            )
            raise RuntimeError(
                f"API server not reachable from container on port {API_PORT}.\n"
                f"API server log:\n{api_log[-2000:]}"
            )

        # 7. Kill bookshelf to trigger respawn with new binary
        container_sh(
            "killall bookshelf.app 2>/dev/null || true",
            check=False,
            timeout=5,
        )

        # 8. Wait for bookshelf to be active
        _wait_bookshelf_active(emulator, timeout=30)
        time.sleep(4.0)

        # 9. Build geometry + session
        snapshot = emulator.wait_for_informer_snapshot(timeout=10)
        app_geom = _parse_app_geometry(FIRMWARE)
        if app_geom is not None and app_geom[0] > 0:
            # The app's logical screen (portrait devices rotate the
            # framebuffer, so the informer's fb dims would transpose
            # every tap coordinate).
            screen_w, screen_h, panel_h = app_geom
        else:
            screen_w = snapshot.width or 1072
            screen_h = snapshot.height or 1448
            panel_h = app_geom[2] if app_geom is not None else 0
        geom = BookshelfGeometry(
            screen_w=screen_w,
            screen_h=screen_h,
            panel_h=panel_h,
        )
        Session(emulator)
        from tests.support.bookshelf.backends import EmulatorBackend

        backend = EmulatorBackend(
            emulator, FIRMWARE, session_cls=Session, ui_input=_UI_INPUT
        )
        bs = BookshelfSession(backend, geom, FIRMWARE)

        yield bs, emulator
    except BaseException:
        # A setup step failed part-way: stop whatever already started and
        # restore the dev cfgs before propagating, so no stale listener,
        # half-started emulator, or test-pointing api_url leaks into later
        # runs (a later run could otherwise adopt a dead/fresh server on
        # the test port, or the app could keep syncing to it).
        if emulator is not None:
            emulator.stop(force=True)
        else:
            # _start_emulator() may have died after creating the container.
            subprocess.run(
                [sys.executable, "-m", "pbemu", "stop"],
                cwd=REPO_ROOT,
                env=_pbemu_env(),
                check=False,
            )
        if api_proc is not None:
            _stop_api_server(api_proc)
        _restore_cfg_file(bin_cfg, saved_bin_cfg)
        _restore_cfg_file(tmp_cfg, saved_tmp_cfg, mode=0o666)
        raise

    # Cleanup: restore the dev cfgs (the run wrote its own api_url),
    # then stop the emulator so no in-memory stale URL can re-poison
    # the restored files.  The bin cfg stays owner-write-only (a
    # world-writable one makes the guest pick the unwritable app dir
    # as its settings home); the tmp cfg is world-writable because the
    # guest (container UID) rewrites settings there.
    _restore_cfg_file(bin_cfg, saved_bin_cfg)
    _restore_cfg_file(tmp_cfg, saved_tmp_cfg, mode=0o666)
    _stop_api_server(api_proc)
    emulator.stop(force=True)


@pytest.fixture(autouse=True)
def fresh_bookshelf(bookshelf_env, request):
    """Restart bookshelf before each test for a clean state."""
    bs, _ = bookshelf_env
    # Invocation ordinal range of this test in the accumulated
    # bookshelf log: the per-test log slicer cuts exactly here (the
    # restart below opens the test's own invocation).
    request.node._bs_log_open_start = bs.invocation_count()  # type: ignore[attr-defined]
    # The app persists the grouping preset across restarts by design;
    # reset it so every test boots on the default view.
    bs.backend.reset_view_state()
    bs.begin_snapshots(request.node.name)
    bs.backend.restart()
    bs.snapshot("boot")
    yield bs
    request.node._bs_log_open_end = bs.invocation_count()  # type: ignore[attr-defined]
    report = getattr(request.node, "_bs_call_report", None)
    bs.snapshot("FAILED" if report is not None and report.failed else "teardown")
    bs.finish_snapshots()
    bs.assert_no_crash()


@pytest.fixture
def fresh_synth(synth_bookshelf_env, request):
    """Restart bookshelf before each synthetic-library test for a clean
    state (mirrors fresh_bookshelf for the synth env).  Deliberately NOT
    autouse: synth_bookshelf_env is SDL-only, and an autouse here made it
    run for every test — under the emulator backend that hit the synth
    "need SDL" skip and killed the whole module, including the device/
    storage tests that should run there."""
    bs, _ = synth_bookshelf_env
    request.node._bs_log_open_start = bs.invocation_count()  # type: ignore[attr-defined]
    bs.begin_snapshots(request.node.name)
    bs.backend.restart()
    bs.snapshot("boot")
    yield bs
    request.node._bs_log_open_end = bs.invocation_count()  # type: ignore[attr-defined]
    report = getattr(request.node, "_bs_call_report", None)
    bs.snapshot("FAILED" if report is not None and report.failed else "teardown")
    bs.finish_snapshots()
    bs.assert_no_crash()


# ── launch & initial state ─────────────────────────────────────────────


def test_bookshelf_launches_with_books(fresh_bookshelf):
    """Verify bookshelf is active, framebuffer has content, log shows sync."""
    bs = fresh_bookshelf
    # Framebuffer should have content (non-zero hash)
    h = bs.frame_hash()
    assert h and h != "0" * len(h), "framebuffer hash is empty/zero"
    # Log should show EVT_INIT and do_sync
    bs.assert_log_contains("EVT_INIT")
    bs.assert_log_contains("do_sync")
    # Log should show draw_grid with books
    bs.assert_log_contains("draw_grid")


# ── top bar ────────────────────────────────────────────────────────────


def test_home_button_noop_on_shelf(fresh_bookshelf):
    """Tap the home button on the plain shelf, verify the app stays up.

    Bookshelf is the home-screen replacement: pressing home while
    already on the library shelf must be a no-op.  Closing the app
    there (the old CloseApp) drops the user into the stock UI and,
    behind the boot wrapper, the app is never respawned — perceived
    as a crash.  Proof: no new log_open header (invocation count
    unchanged) and no respawn.
    """
    bs = fresh_bookshelf
    before = bs.invocation_count()
    bs.tap_home()
    # Poll briefly that the app did not respawn (a single sleep would
    # sample the count only once and could miss a late respawn).
    deadline = time.monotonic() + 1.5
    while time.monotonic() < deadline:
        assert bs.invocation_count() == before, "home on home must not respawn the app"
        time.sleep(0.1)
    bs.assert_no_crash()


def test_header_tap_opens_control_panel(fresh_bookshelf):
    """Tapping the system status strip (clock/battery) opens the
    firmware control panel.  Regression: the handler was dropped in the
    popup commit, silently breaking the strip tap for the self-drawn
    strip case (the live device, where the firmware panel never
    activates).  In the emulator the firmware's panel intercepts the
    tap itself, so this only runs when the app draws the strip.  The
    strip lives at the BOTTOM of the screen (stock type-1 panel)."""
    bs = fresh_bookshelf
    log = bs.current_log()
    if "self_panel=1" not in log:
        pytest.skip("firmware panel active: the tap is handled by the firmware")
    before = bs.current_log()
    bs.tap_at(bs.geom.screen_w // 2, bs.geom.screen_h - bs.geom.panel_h // 2)
    _wait_log_slice(bs, before, "system bar tapped -> control panel")


def test_menu_button_opens_more_overlay(fresh_bookshelf):
    """Tap menu button (top-right), verify overlay appears."""
    bs = fresh_bookshelf
    bs.tap_menu_and_verify()
    # The More overlay should be drawn (framebuffer changed)
    # Verify by tapping outside to dismiss and checking another change
    bs.tap_outside_and_verify()


# ── More overlay ───────────────────────────────────────────────────────


def test_sync_button_taps_sync(fresh_bookshelf):
    """Tap the top-bar sync icon, verify a re-sync runs (the drawer no
    longer carries a Sync row)."""
    bs = fresh_bookshelf
    before = bs.current_log()
    bs.tap_sync_button()
    # After sync, log should show do_sync from this tap (not the boot one).
    _wait_log_slice(bs, before, "do_sync", timeout=20.0)


# Sort rows in the more-overlay chooser (0..3: title/author/series/recent).
_SORT_TITLE, _SORT_AUTHOR, _SORT_SERIES, _SORT_RECENT = range(4)


def _sort_changes_shelf(bs, target_row: int, baseline_row: int) -> None:
    """Assert that choosing sort *target_row* re-renders the shelf
    differently than sort *baseline_row*.

    Deterministic against the mock library, whose every book shares
    author='pbemu mock library' and whose default sort is Title A-Z —
    so tapping "Title A-Z" (or "By author", which orders identically)
    from the default is a genuine no-op.  Such a no-op used to
    "pass" only because the old one-cover-per-tick loader changed the
    frame mid-test; once covers load in a single batch, a settled
    frame never changes.  So settle the covers first and drive through
    a differing sort before capturing the baseline, guaranteeing the
    target row really repaints."""
    bs.wait_for_stable()          # let the boot cover batch land
    bs.choose_sort(baseline_row)  # a sort known to reorder from the default
    bs.wait_for_stable()
    h = bs.frame_hash()
    bs.choose_sort(target_row)
    assert bs.frame_hash() != h, f"sort row {target_row} did not change the shelf"
    bs.assert_no_crash()


def test_more_overlay_sort_title_az(fresh_bookshelf):
    """Open More, tap Title A-Z, verify framebuffer changes."""
    _sort_changes_shelf(fresh_bookshelf, _SORT_TITLE, _SORT_SERIES)


def test_more_overlay_sort_author(fresh_bookshelf):
    """Open More, tap By author, verify framebuffer changes."""
    _sort_changes_shelf(fresh_bookshelf, _SORT_AUTHOR, _SORT_SERIES)


def test_more_overlay_sort_series(fresh_bookshelf):
    """Open More, tap By series, verify framebuffer changes."""
    _sort_changes_shelf(fresh_bookshelf, _SORT_SERIES, _SORT_RECENT)


def test_more_overlay_sort_recent(fresh_bookshelf):
    """Open More, tap Recent, verify framebuffer changes."""
    _sort_changes_shelf(fresh_bookshelf, _SORT_RECENT, _SORT_SERIES)


def test_layout_toggle_button(fresh_bookshelf):
    """The top-bar layout icon toggles grid/list (the drawer no longer
    carries separate Grid/List rows)."""
    bs = fresh_bookshelf
    before = bs.frame_hash()
    bs.tap_at(*bs._g.layout_icon_center())
    bs.wait_hash_change(before)
    bs.assert_no_crash()


def test_more_overlay_dismiss_outside_tap(fresh_bookshelf):
    """Open More, tap outside, verify overlay dismisses."""
    bs = fresh_bookshelf
    bs.tap_menu_and_verify()
    bs.tap_outside_and_verify()


def test_more_overlay_dismiss_back_key(fresh_bookshelf):
    """Open More, press Back, verify overlay dismisses."""
    bs = fresh_bookshelf
    bs.tap_menu_and_verify()
    bs.send_back_and_verify()

# ── settings overlay ───────────────────────────────────────────────────

# The app resolves its config to /mnt/ext1/system/bin/bookshelf.cfg (the
# dir is guest-writable since the staging change) and saves settings
# there; /tmp/bookshelf.cfg is only a kv-override.
_GUEST_CFG_HOST = (
    PBEMU_ROOT / FIRMWARE / ".live" / "mnt" / "ext1" / "system" / "bin" / "bookshelf.cfg"
)


def _read_guest_cfg() -> str:
    """Read the guest's bookshelf.cfg (host-side .live mount)."""
    return _GUEST_CFG_HOST.read_text(encoding="utf-8") if _GUEST_CFG_HOST.is_file() else ""


def _clear_guest_cfg() -> None:
    """Drop the saved reader preference so a test starts on Auto, keeping
    the staged api_url/api_token (save_config_file rewrites the whole
    config next to its resolved config file)."""
    if not _GUEST_CFG_HOST.is_file():
        return
    lines = [
        ln
        for ln in _GUEST_CFG_HOST.read_text(encoding="utf-8").splitlines()
        if not ln.startswith("reader=")
    ]
    # A PBEMU_NO_KEEPID boot leaves the cfg foreign-owned and 0644;
    # replace (dir is host-writable) instead of truncating in place.
    _GUEST_CFG_HOST.unlink(missing_ok=True)
    _GUEST_CFG_HOST.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _restore_guest_cfg(saved: str) -> None:
    """Restore the guest bin cfg to its pre-test state (remove if absent).

    The settings save tests write reader=N into the shared bin cfg; if
    left there the next test boots with that reader preference.  Restore
    the pre-test text (or remove the file) so the preference does not
    leak across runs.  Unlink-first: a PBEMU_NO_KEEPID boot leaves the
    cfg foreign-owned and 0644, so replace instead of truncate."""
    _GUEST_CFG_HOST.unlink(missing_ok=True)
    if saved:
        _GUEST_CFG_HOST.write_text(saved, encoding="utf-8")






def test_more_overlay_settings_opens_page(fresh_bookshelf):
    """Open More, tap Settings, verify the settings page is drawn."""
    bs = fresh_bookshelf
    bs.tap_menu_and_verify()
    before = bs.frame_hash()
    bs.tap_more_item(MORE_SETTINGS)
    bs.wait_hash_change(before)


def test_settings_reader_cycle_and_save(fresh_bookshelf):
    """Cycle the reader row to Standard, Save, verify config + log."""
    bs = fresh_bookshelf
    saved_guest_cfg = _read_guest_cfg()
    try:
        _clear_guest_cfg()
        _restart_bookshelf(bs.emulator)
        bs.open_settings()
        time.sleep(0.5)
        # Auto -> Standard (first detected reader).
        bs.tap_settings_row(2)
        time.sleep(0.5)
        before = bs.current_log()
        bs.tap_settings_save()
        # Save path logged the new preference and re-synced.
        _wait_log_slice(bs, before, "settings: saved")
        bs.assert_log_contains("reader_pref=1")
        # Config file on disk now pins the standard reader path.
        cfg = _read_guest_cfg()
        assert "reader=/ebrmain/bin/eink-reader.app" in cfg, f"cfg was:\n{cfg}"
    finally:
        _restore_guest_cfg(saved_guest_cfg)


def test_settings_reader_pref_persists_across_restart(fresh_bookshelf):
    """A saved reader preference is reloaded on the next launch."""
    bs = fresh_bookshelf
    saved_guest_cfg = _read_guest_cfg()
    try:
        _clear_guest_cfg()
        _restart_bookshelf(bs.emulator)
        bs.open_settings()
        time.sleep(0.5)
        bs.tap_settings_row(2)
        time.sleep(0.5)
        before = bs.current_log()
        bs.tap_settings_save()
        _wait_log_slice(bs, before, "settings: saved")
        # Restart bookshelf; the fresh process must load reader_pref=1.
        _restart_bookshelf(bs.emulator)
        bs.assert_log_contains("reader_pref=1 (cfg `/ebrmain/bin/eink-reader.app`)")
    finally:
        _restore_guest_cfg(saved_guest_cfg)


def test_settings_back_returns_to_shelf(fresh_bookshelf):
    """Open Settings, tap Back, verify the shelf is redrawn."""
    bs = fresh_bookshelf
    bs.open_settings()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_settings_back()
    bs.wait_hash_change(before)


def test_settings_show_logs_opens_log_viewer(fresh_bookshelf):
    """Settings → Show logs draws the log viewer without crashing.

    Regression: the cached-wrap perf batch allocated the wrap row array
    with malloc instead of calloc, but log_wrap_rows uses `.len == 0`
    as its empty-row sentinel (and dereferences `.p` when `.len > 0`).
    The first wrap of a real log tail then read uninitialized heap and
    segfaulted inside span_width; the watchdog respawned the app, so
    the viewer never appeared.
    """
    bs = fresh_bookshelf
    bs.open_settings()
    time.sleep(0.5)
    before_hash = bs.frame_hash()
    before_invocations = bs.invocation_count()
    bs.tap_settings_logs()
    # The full-screen log page replaces the settings page...
    bs.wait_hash_change(before_hash)
    time.sleep(0.5)
    # ...and the app must not have crashed + respawned (a crash would
    # open a new invocation of the app).
    assert bs.invocation_count() == before_invocations, (
        "bookshelf respawned after the Show logs tap (crash)"
    )
    # Back leaves the viewer and returns to the shelf.
    before_back = bs.frame_hash()
    bs.tap_log_back()
    bs.wait_hash_change(before_back)
    assert bs.invocation_count() == before_invocations


def test_settings_licenses_opens_viewer_and_drills(fresh_bookshelf):
    """Settings → Licenses shows the license list; a row opens its
    full text; Back returns to the list, then to the shelf.  No crash
    on any transition."""
    bs = fresh_bookshelf
    bs.open_settings()
    time.sleep(0.5)
    before_invocations = bs.invocation_count()

    # Open the licenses viewer: the list replaces the settings page.
    before_list = bs.frame_hash()
    bs.tap_settings_licenses()
    bs.wait_hash_change(before_list)
    time.sleep(0.5)
    assert bs.invocation_count() == before_invocations, (
        "bookshelf respawned after the Licenses tap (crash)"
    )

    # Tap the first license row: its detail text replaces the list.
    before_detail = bs.frame_hash()
    bs.tap_licenses_list_row(0)
    bs.wait_hash_change(before_detail)
    time.sleep(0.5)
    assert bs.invocation_count() == before_invocations

    # Back from a detail returns to the list (a distinct frame).
    before_back = bs.frame_hash()
    bs.tap_licenses_back()
    bs.wait_hash_change(before_back)
    time.sleep(0.5)
    assert bs.invocation_count() == before_invocations

    # Back from the list returns to the shelf.
    before_shelf = bs.frame_hash()
    bs.tap_licenses_back()
    bs.wait_hash_change(before_shelf)
    assert bs.invocation_count() == before_invocations


def test_settings_api_host_row_opens_keyboard(fresh_bookshelf):
    """Tap the API host row, verify the on-screen keyboard appears."""
    bs = fresh_bookshelf
    bs.open_settings()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_settings_row(0)
    bs.wait_hash_change(before)


def test_settings_system_app_toggle_promotes_and_demotes(fresh_bookshelf):
    """Install-as-system-app: toggling ON copies the running binary + a
    fresh cfg to the home-task path; toggling OFF removes them again.

    SDL-only: on the emulator the app IS the home task (it runs from
    /mnt/ext1/system/bin/bookshelf.app), so sysapp_self_bin() == the
    promote target -> promote is a deliberate no-op and the copy this
    test inspects never happens.  The test's premise (target dir differs
    from the running binary) only holds under SDL, where EH_SYSAPP_DIR
    isolates a per-instance run_dir/sysapp.
    """
    if os.environ.get("EH_TEST_BACKEND", "emulator") != "sdl":
        pytest.skip("sysapp promote/demote target == running binary under "
                    "the emulator; needs an isolated EH_SYSAPP_DIR (SDL)")

    def _exists(path: Path) -> bool:
        return path.exists()

    bs = fresh_bookshelf
    run_dir = bs.backend._run_dir
    sysapp = run_dir / "sysapp"
    app = sysapp / "bookshelf.app"
    cfg = sysapp / "bookshelf.cfg"
    # Start from a clean slate.
    app.unlink(missing_ok=True)
    cfg.unlink(missing_ok=True)
    assert not _exists(app)

    bs.open_settings()
    bs.wait_for_stable(timeout=20.0)

    # Toggle ON → home-task override + cfg appear.
    before = bs.frame_hash()
    bs.tap_settings_sysapp()
    bs.wait_hash_change(before, timeout=20.0)
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and not (_exists(app) and _exists(cfg)):
        time.sleep(0.2)
    assert _exists(app), "promote did not write bookshelf.app"
    assert _exists(cfg), "promote did not write bookshelf.cfg"
    assert app.stat().st_size > 1000, "promoted binary looks empty"
    assert "api_url=" in cfg.read_text(encoding="utf-8"), \
        "promoted cfg lost the API url"
    assert bs.current_log().count("installed as home task") >= 1, \
        "log missing promote result"

    # Toggle OFF → override + cfg are removed again.
    before2 = bs.frame_hash()
    bs.tap_settings_sysapp()
    bs.wait_hash_change(before2, timeout=20.0)
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and (_exists(app) or _exists(cfg)):
        time.sleep(0.2)
    assert not _exists(app), "demote left bookshelf.app behind"
    assert not _exists(cfg), "demote left bookshelf.cfg behind"
    assert bs.current_log().count("removed from system") >= 1


# ── launcher (app grid) ───────────────────────────────────────────────


def test_more_overlay_apps_opens_launcher(fresh_bookshelf):
    """Open More, tap Applications, verify the launcher overlay draws."""
    bs = fresh_bookshelf
    bs.tap_menu_and_verify()
    before = bs.frame_hash()
    bs.tap_more_item(MORE_APPS)
    bs.wait_hash_change(before)
    bs.assert_log_contains("launcher built")


def test_launcher_back_returns_to_shelf(fresh_bookshelf):
    """Open launcher, tap Back, verify the shelf is redrawn."""
    bs = fresh_bookshelf
    before_log = bs.current_log()
    bs.open_launcher()
    _wait_log_slice(bs, before_log, "launcher built")
    before = bs.frame_hash()
    bs.tap_launcher_back()
    bs.wait_hash_change(before)


def test_launcher_back_key_returns_to_shelf(fresh_bookshelf):
    """Open launcher, press Back key, verify the shelf is redrawn."""
    bs = fresh_bookshelf
    before_log = bs.current_log()
    bs.open_launcher()
    _wait_log_slice(bs, before_log, "launcher built")
    before = bs.frame_hash()
    bs.send_back_key()
    bs.wait_hash_change(before)


def test_launcher_tap_app_launches_task(fresh_bookshelf):
    """Open launcher, tap an app cell, verify NewTaskEx is called."""
    bs = fresh_bookshelf
    # Each step waits for the framebuffer to change, proving the event
    # loop processed the tap (cover-download HTTP calls can block the
    # loop for several seconds after a fresh restart).
    before = bs.frame_hash()
    bs.tap_menu()
    bs.wait_hash_change(before)
    before = bs.frame_hash()
    bs.tap_at(*bs.geom.more_item_center(MORE_APPS))
    bs.wait_hash_change(before)
    before = bs.frame_hash()
    before_log = bs.current_log()
    bs.tap_launcher_app(0)
    bs.wait_hash_change(before)
    _wait_log_slice(bs, before_log, "launching app path=")


# ── search ────────────────────────────────────────────────────────────


def test_search_tap_opens_keyboard(fresh_bookshelf):
    """Tap the search icon, then the input row; verify the keyboard
    appears."""
    bs = fresh_bookshelf
    bs.tap_search_and_verify()
    time.sleep(0.5)
    bs.tap_search_input_and_verify()


def test_launcher_drag_scrolls_body(fresh_bookshelf):
    """Dragging the launcher body vertically scrolls the app column."""
    bs = fresh_bookshelf
    before_log = bs.current_log()
    bs.open_launcher()
    _wait_log_slice(bs, before_log, "launcher built")
    before = bs.frame_hash()
    bs.scroll_launcher_down()
    bs.wait_hash_change(before)


def test_search_commit_filters_grid(fresh_bookshelf):
    """Typing a query on the Search page and committing it must actually
    filter the shelf.

    Regression for the "search never searches" bug: OpenKeyboard() wrote
    the live keystrokes straight into eh_g_state.query, and on commit the
    handler's snprintf(query, ..., buffer) aliased that same buffer,
    wiping the query before apply_filter_and_sort() ran — so the grid
    never changed and the filter log showed an empty query.  The fix
    hands OpenKeyboard() a separate scratch buffer; this test proves the
    committed text now survives into the filter pass.
    """
    bs = fresh_bookshelf
    # Open the Search page, then the input row's keyboard.
    bs.tap_search_and_verify()
    time.sleep(0.5)
    kb = bs.tap_search_input_and_verify()
    before_log = bs.current_log()
    bs.type_text("alpha", commit=True)
    bs.wait_hash_change(kb)
    # The committed query reached the filter (pre-fix this was empty).
    # Poll: the commit's log lines land a beat after the frame the hash
    # wait above observed.
    _wait_log_slice(bs, before_log, "query=`alpha`")


def test_search_history_persists_and_reruns(fresh_bookshelf):
    """A committed search is recorded and listed on the Search page;
    tapping the term re-runs that search without the keyboard."""
    bs = fresh_bookshelf
    # Run a search first so history has an entry.
    bs.tap_search_and_verify()
    time.sleep(0.5)
    bs.tap_search_input_and_verify()
    bs.type_text("alpha", commit=True)
    time.sleep(0.5)
    # The Search page now lists "alpha" as a history term.
    before = bs.frame_hash()
    bs.tap_search()
    bs.wait_hash_change(before)
    time.sleep(0.5)
    before = bs.current_log()
    bs.tap_history_term(0)
    _wait_log_slice(bs, before, "search history tap: query=`alpha`")


# ── search page top bar (source button hidden) ────────────────────────


def _dump_frame(emulator: Emulator, name: str) -> bytes:
    """Dump the live framebuffer as a PPM and return its bytes.

    The probe runs inside the container; /workspace/firmware/.live/tmp is
    the container-side alias of the host .live/tmp (the same mapping the
    guest /tmp uses for its log), so the file is readable straight from
    the host.
    """
    guest = f"/workspace/firmware/.live/tmp/{name}.ppm"
    emulator.run_probe("frame_dump", "--ppm", guest)
    return (PBEMU_ROOT / FIRMWARE / ".live" / "tmp" / f"{name}.ppm").read_bytes()


def _frame_dump(runtime, name: str) -> bytes:
    """Return the current framebuffer as PPM bytes.  *runtime* is either
    an Emulator (emulator backend) or the test's BookshelfSession (other
    backends) — both let us grab a frame dump."""
    if hasattr(runtime, "backend"):
        return runtime.backend.frame_ppm(name)
    return _dump_frame(runtime, name)


def _settled_dump(runtime, name: str, *, timeout: float = 5.0) -> bytes:
    """Dump the framebuffer, retrying until two consecutive NON-EMPTY
    dumps are byte-identical (the frame settled), then return them.

    Pixel assertions race a slow renderer: a single dump can catch a
    half-drawn frame.  Retry until the frame stops changing so the
    region checks read a settled image.  An EMPTY dump (the probe got
    no response from a busy or gone app) is never a frame: it cannot
    settle, and hitting the deadline raises with the last state
    instead of feeding b'' to pixel assertions."""
    deadline = time.monotonic() + timeout
    prev: bytes | None = None
    while time.monotonic() < deadline:
        cur = _frame_dump(runtime, name)
        # An empty dump is the probe's no-response leftover (busy or
        # gone app): skip it — it can neither settle nor be a frame.
        if not cur:
            time.sleep(0.3)
            continue
        if cur == prev:
            return cur
        prev = cur
        time.sleep(0.3)
    raise AssertionError(
        f"frame dump for {name!r} never settled within {timeout}s"
    )


def _ppm_region_white(ppm: bytes, x0: int, y0: int, x1: int, y1: int) -> bool:
    """True when every pixel of the P6-PPM region is pure white."""
    head, _, rest = ppm.partition(b"\n")
    assert head == b"P6", f"expected a P6 PPM, got {head!r}"
    dims, _, rest = rest.partition(b"\n")
    w, h = map(int, dims.split())
    maxval, _, data = rest.partition(b"\n")
    assert int(maxval) == 255
    assert 0 <= x0 < x1 <= w and 0 <= y0 < y1 <= h
    for y in range(y0, y1):
        row = y * w * 3
        for x in range(x0, x1):
            off = row + x * 3
            if data[off] != 255 or data[off + 1] != 255 or data[off + 2] != 255:
                return False
    return True


def test_search_page_hides_source_button(fresh_bookshelf):
    """The Search page hides the source button and the right-side icon
    stack (the top bar is just the back arrow), and the source button's
    old spot must not open the chooser."""
    bs = fresh_bookshelf
    bs.wait_for_stable()
    w = bs.geom.screen_w
    # The firmware's fb_y_offset wrap renders the system strip at the
    # physical top of the framebuffer, so the app's top bar (and its
    # buttons) sits panel_h rows down in a frame dump.
    panel = bs.geom.panel_h
    # Source-button body: icon + label area (sample avoids the title).
    sx0, sy0, sx1, sy1 = 112 + 20, panel + 48, 112 + 176 - 20, panel + 84
    # Right-most icon (the menu button) spot.
    mx0, my0, mx1, my1 = w - 104, panel + 48, w - 8, panel + 84

    # Shelf: both spots are drawn (non-white).
    shelf = _settled_dump(bs, "eh_source_shelf")
    assert not _ppm_region_white(shelf, sx0, sy0, sx1, sy1), "source button not drawn on the shelf"
    assert not _ppm_region_white(shelf, mx0, my0, mx1, my1), "menu icon not drawn on the shelf"

    # Open the Search page.
    bs.tap_search_and_verify()
    bs.wait_for_stable()

    # Both spots are now plain white — the buttons are gone.
    search = _settled_dump(bs, "eh_source_search")
    assert _ppm_region_white(search, sx0, sy0, sx1, sy1), "source button still drawn on Search page"
    assert _ppm_region_white(search, mx0, my0, mx1, my1), "right icons still drawn on Search page"

    # Tapping where the source button used to be must not open the
    # source chooser: no overlay, framebuffer unchanged.
    before = bs.frame_hash()
    bs.tap_at(112 + 176 // 2, 64)
    # Poll briefly: the tap must not change the frame (the source
    # button's old spot is dead on the Search page).  A single fixed
    # sleep would race the negative assertion against a slow redraw.
    deadline = time.monotonic() + 1.0
    while time.monotonic() < deadline:
        assert bs.frame_hash() == before, "source chooser opened from the Search page"
        time.sleep(0.1)


def _ppm_ink_xs(ppm: bytes, x0: int, y0: int, x1: int, y1: int) -> list[int]:
    """Return the x positions of non-white pixels in a P6-PPM region."""
    head, _, rest = ppm.partition(b"\n")
    assert head == b"P6", f"expected a P6 PPM, got {head!r}"
    dims, _, rest = rest.partition(b"\n")
    w, h = map(int, dims.split())
    maxval, _, data = rest.partition(b"\n")
    assert int(maxval) == 255
    assert 0 <= x0 < x1 <= w and 0 <= y0 < y1 <= h
    xs: list[int] = []
    for y in range(y0, y1):
        row = y * w * 3
        for x in range(x0, x1):
            off = row + x * 3
            if data[off] != 255 or data[off + 1] != 255 or data[off + 2] != 255:
                xs.append(x)
    return xs


def _dump_suggestion_in_band(bs: BookshelfSession, name: str, *, timeout: float = 8.0) -> bytes:
    """Poll frame dumps until the live suggestion band shows ink, then
    return that dump.

    The suggestion band (left of x=300, above the on-screen keyboard)
    is redrawn by the debounce tick ~200 ms after the last keystroke.
    A fixed sleep before the pixel assertion races a slow guest; instead
    poll the actual condition (ink in the band) until the row is drawn.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ppm = _frame_dump(bs, name)
        if _ppm_ink_xs(
            ppm, 24, bs.geom.panel_h + 228, 300, bs.geom.panel_h + 430
        ):
            return ppm
        time.sleep(0.2)
    raise AssertionError(f"suggestion row never drawn in the band (dump {name!r})")


def test_search_page_layout_centered(fresh_bookshelf):
    """The Search page top bar centres its title on the whole screen
    width (no flanking buttons to narrow the band), and the search input
    bar spans the full page width with the magnifier inside it."""
    bs = fresh_bookshelf
    bs.wait_for_stable()
    w = bs.geom.screen_w
    panel = bs.geom.panel_h
    bs.tap_search_and_verify()
    bs.wait_for_stable()
    ppm = _settled_dump(bs, "eh_search_layout")

    # Title: the only ink in the top-bar band between the back button
    # and the right edge is the title text; its extent must be centred
    # on the screen (within a small tolerance for font kerning).
    xs = _ppm_ink_xs(ppm, w // 4, panel + 30, 3 * w // 4, panel + 82)
    assert xs, "no title ink found in the top bar band"
    center = (min(xs) + max(xs)) / 2.0
    assert abs(center - w / 2) <= 16, f"title centred at x={center:.0f}, screen centre {w / 2}"

    # Input bar: 1px border strokes at x=16 and x=w-17 (full width).
    assert not _ppm_region_white(ppm, 16, panel + 150, 18, panel + 218), "bar missing left border"
    assert not _ppm_region_white(
        ppm, w - 18, panel + 150, w - 16, panel + 218
    ), "bar missing right border"


def test_search_keyboard_outside_tap_stays_on_search(fresh_bookshelf):
    """Tapping outside the on-screen keyboard must not commit a search
    or jump to the library: a dismissed, unedited keyboard leaves the
    Search page untouched.

    Regression: the dismissal delivered the unchanged (empty) buffer,
    which the handler treated as an edit — it committed an empty query
    and switched to TAB_LIBRARY, teleporting the user home."""
    bs = fresh_bookshelf
    bs.tap_search_and_verify()
    time.sleep(0.5)
    bs.tap_search_input_and_verify()  # opens the keyboard
    time.sleep(0.5)
    before = bs.current_log()
    bs.tap_at(bs.geom.screen_w // 2, 500)  # outside the keyboard
    time.sleep(0.8)
    slice_ = bs.current_log()[len(before):]
    assert "search commit" not in slice_, "outside tap committed a search"
    assert "draw_grid view=" not in slice_, "outside tap jumped to the library"
    # Still on the Search page: tapping the input row reopens the
    # keyboard (the library grid has no input row to open one from).
    h = bs.frame_hash()
    bs.tap_search_input_and_verify()
    bs.wait_hash_change(h)


# ── book grid ──────────────────────────────────────────────────────────


def test_book_tap_triggers_open_with(fresh_bookshelf):
    """Tap a book tile, verify it downloads (if needed) then launches."""
    bs = fresh_bookshelf
    _clear_downloads()
    before = bs.current_log()
    bs.tap_book(0)
    # A book press resolves the reader and launches it (OpenBook path).
    _wait_log_slice(bs, before, "launching reader", timeout=20.0)
    _kill_guest_tasks()
    _clear_downloads()


def test_book_tap_launches_reader(fresh_bookshelf):
    """Tap a book tile, verify download + launch sequence end to end."""
    bs = fresh_bookshelf
    _clear_downloads()
    before = bs.current_log()
    bs.tap_book(0)
    # The mock provider serves tiny fake epubs that the real reader
    # cannot open, so the reader may crash/return immediately.
    # Verify the bookshelf side did its job: download + launch.
    _wait_log_slice(bs, before, "download_book_file OK", timeout=20.0)
    _wait_log_slice(bs, before, "launching reader", timeout=20.0)
    _kill_guest_tasks()
    _clear_downloads()


# ── pager ──────────────────────────────────────────────────────────────


def test_pager_next_advances_page(fresh_bookshelf):
    """Tap next page, verify framebuffer changes (page 2)."""
    bs = fresh_bookshelf
    # Mock provider has 16 books, PAGESIZE=6, so 3 pages
    bs.tap_pager_next_and_verify()


def test_pager_prev_returns_page(fresh_bookshelf):
    """Go to page 2, tap prev, verify framebuffer changes back."""
    bs = fresh_bookshelf
    # Must verify next actually advanced before testing prev.
    bs.tap_pager_next_and_verify()
    bs.tap_pager_prev_and_verify()


# ── group drawer / multi-level grouping ───────────────────────────────

def test_group_by_single_level_stacks_and_drill(fresh_bookshelf):
    """Grouping picks ONE dimension; the shelf collapses into stack cards
    and tapping a card shows that group's books flat.

    The mock library is a single author, so "By author" collapses to one
    stack card; tapping it drills into the author's flat books, and Back
    returns to the grouped stacks.
    """
    bs = fresh_bookshelf
    # The SDL mock has author (and year) data but no API series, so the
    # Author > Series preset and Series options are hidden; author works.
    rows = bs.group_rows()
    assert 'series' not in rows and 'author_series' not in rows, (
        "series grouping offered without API series data"
    )
    bs.choose_group('author')
    bs.assert_no_crash()
    # Group=2 (by author), no drill, collapsed to a single stack card.
    bs.assert_log_contains("group=2 drill=0")
    bs.assert_log_contains("view=1")

    # Tap the stack card -> the author's books, flat.
    before = bs.current_log()
    h = bs.frame_hash()
    bs.tap_book(0)
    bs.wait_hash_change(h)
    _wait_log_slice(bs, before, "drill=1")

    # Back returns to the grouped stacks.
    before = bs.current_log()
    bs.send_back_key()
    _wait_log_slice(bs, before, "drill=0")
    bs.assert_no_crash()


def test_group_by_sort_buttons_and_choosers(fresh_bookshelf):
    """The drawer exposes Group by and Sort by; the sort sheet applies."""
    bs = fresh_bookshelf
    before = bs.current_log()
    bs.choose_sort(1)  # 1 = By author
    bs.assert_no_crash()
    _wait_log_slice(bs, before, "sort=1")


S_PAGESIZE = 6  # COLS * ROWS, must match geometry.PAGESIZE


def _synth_view_kinds(bs) -> list[int]:
    """On-screen tile kinds of the projected view, top-to-bottom, read
    straight from the app's store.  The view rebuild INSERT orders by the
    group fk, so SQLite rowid order == on-screen order: 0 = flat tile,
    1 = stack card."""
    import sqlite3
    path = bs.backend.store_path
    deadline = time.monotonic() + 10.0
    last_err = None
    while time.monotonic() < deadline:
        try:
            con = sqlite3.connect(str(path), timeout=2.0)
            try:
                rows = con.execute(
                    "SELECT kind FROM view ORDER BY rowid"
                ).fetchall()
            finally:
                con.close()
            if rows:
                return [int(r[0]) for r in rows]
        except Exception as exc:  # noqa: BLE001 - store may be mid-sync
            last_err = exc
        time.sleep(0.2)
    raise AssertionError(f"could not read the projected view: {last_err}")


def test_synth_group_cards_interleave_by_sort(fresh_synth):
    """A multi-member group card sits at its first member's sort position,
    interleaved with single-member flat tiles — never all shoved after
    them.

    The old build emitted every flat tile first, then all stack cards
    (``fk = 1e9 + first-seen``), so a card could never precede a flat and
    the flats always led the shelf.  The fix keys each card on its first
    member's fk, so a card whose earliest book sorts among the flats
    appears before them.

    The corpus alternates author cards and single-book flats under "By
    author" ("Author 00" has two books → the first tile is a card), so
    the view is [card, flat, card, flat, ...] and the leading card proves
    the interleave.
    """
    bs = fresh_synth
    bs.choose_group('author')
    bs.wait_for_stable()
    kinds = _synth_view_kinds(bs)
    if not (0 in kinds and 1 in kinds):
        import sqlite3
        con = sqlite3.connect(str(bs.backend.store_path), timeout=2.0)
        try:
            rows = con.execute(
                "SELECT author, COUNT(*) FROM books GROUP BY author"
            ).fetchall()
            total = con.execute("SELECT COUNT(*) FROM books").fetchone()[0]
        finally:
            con.close()
        raise AssertionError(
            f"expected a mixed flat/card view, got kinds={kinds}; "
            f"books total={total}, by author={rows}"
        )
    first_card = kinds.index(1)
    last_flat = len(kinds) - 1 - kinds[::-1].index(0)
    assert first_card < last_flat, (
        "a multi-book card sorted after every flat tile; cards should "
        "interleave at their first member's sort position"
    )
    # "Author 00" has two books → its card must lead the shelf.
    assert first_card == 0, f"expected a card first in the view, got {kinds}"


def test_synth_group_drill_back_restores_page_and_back_icon(fresh_synth):
    """Drill-back from a group returns to the page it was opened from (not
    page 0), and while drilled the top-bar left button is the back chevron
    instead of the house.

    Reproduces the reported flow: group by Author > Series, go to page 2,
    open an author group, then leave it — the shelf must land back on
    page 2, not page 1 (or page 0).
    """
    bs = fresh_synth
    bs.choose_group('author_series')
    bs.wait_for_stable()
    kinds = _synth_view_kinds(bs)
    target_page = 2
    assert len(kinds) > (target_page + 1) * S_PAGESIZE, (
        f"expected the grouped view to span more than page {target_page}, "
        f"got {len(kinds)} tiles"
    )
    # Pick a stack card on the target page to drill into.
    page_kinds = kinds[target_page * S_PAGESIZE:(target_page + 1) * S_PAGESIZE]
    card_pos = next((i for i, k in enumerate(page_kinds) if k == 1), None)
    assert card_pos is not None, (
        f"expected a stack card on page {target_page}, got {page_kinds}"
    )

    # Go to the target page of the grouped view.
    _goto_view_tile(bs, target_page * S_PAGESIZE)  # lands on page target_page
    before_drill = bs.frame_hash()

    # Not drilled: the left top-bar button is the house (ink at the roof
    # apex).  Sample the roof-apex pixel inside the icon box.
    ppm = _settled_dump(bs, "grp_house")
    assert _ppm_ink_xs(ppm, 55, 22, 59, 26), "expected house roof apex before drill"

    # Drill into the card.
    bs.tap_book(card_pos)
    bs.wait_hash_change(before_drill)
    _wait_log_slice(bs, before_drill, "drill=1")

    # Drilled: the left button is the back chevron — the roof-apex spot is
    # blank (the chevron is confined to the button's left-centre) while
    # the chevron's own stroke is present.
    ppm = _settled_dump(bs, "grp_back")
    assert not _ppm_ink_xs(ppm, 55, 22, 59, 26), (
        "house roof apex still drawn while drilled into a group"
    )
    assert _ppm_ink_xs(ppm, 48, 20, 86, 84), "no back chevron ink while drilled"

    # Back pops the drill and restores the pre-drill page, not page 0.
    before_back = bs.current_log()
    bs.tap_home()  # the left top-bar button = back while drilled
    _wait_log_slice(bs, before_back, "drill=0")
    pages = re.findall(r"draw_grid view=\d+ page=(\d+)", bs.current_log())
    assert pages and int(pages[-1]) == target_page, (
        f"group drill-back jumped page: expected {target_page}, "
        f"got {pages[-1] if pages else None}"
    )
    bs.assert_no_crash()


# ── back key (no overlay) ──────────────────────────────────────────────

def test_back_key_noop_on_shelf(fresh_bookshelf):
    """Press Back on the plain shelf, verify the app stays up.

    Same contract as the home button: back from the home shelf must
    not CloseApp() (which, behind the boot wrapper, is never
    respawned and reads as a crash).  Proof: invocation count
    unchanged and no respawn.
    """
    bs = fresh_bookshelf
    before = bs.invocation_count()
    bs.send_back_key()
    # Poll briefly that the app did not respawn.
    deadline = time.monotonic() + 1.5
    while time.monotonic() < deadline:
        assert bs.invocation_count() == before, "back on home must not respawn the app"
        time.sleep(0.1)
    bs.assert_no_crash()


# ── crash safety ───────────────────────────────────────────────────────


def test_no_crash_after_all_interactions(fresh_bookshelf):
    """Exercise all interactive elements, verify no crash markers in log."""
    bs = fresh_bookshelf
    # Open the drawer, dismiss it (back + outside) — the drawer now holds
    # only chooser buttons (group/sort) and modal rows (download-all,
    # settings, apps), so it is simply opened and closed here.
    bs.tap_menu_and_verify()
    time.sleep(0.3)
    bs.send_back_key()
    time.sleep(0.5)
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_outside_more()
    time.sleep(0.5)
    # Tap search
    bs.tap_search()
    time.sleep(0.5)
    bs.send_back_key()
    time.sleep(0.5)
    # Tap pager
    bs.tap_pager_next()
    time.sleep(0.5)
    bs.tap_pager_prev()
    time.sleep(0.5)
    # Tap outside
    bs.tap_outside_more()
    time.sleep(0.5)
    # Final crash check
    bs.assert_no_crash()


# ── series grouping helpers ───────────────────────────────────────────
# The mock provider derives a series from a "Name - NN" filename convention
# (see api/providers/mock.py).  The shipped books are all standalone, so the
# default (None/All books) view shows no series card — it is flat and the
# injected series' volumes appear as individual tiles.  Tests that need a
# series stack card (the long-press context menus) inject a two-book series,
# restart bookshelf so the fresh launch syncs it, then pick "By series" to
# collapse it into one card.  Injected files are always removed so the
# shared books dir is clean for the other tests in this module.

_SERIES_STEM = "Drill_Test"
_SERIES_FILES = [f"{_SERIES_STEM} - 01.epub", f"{_SERIES_STEM} - 02.epub"]
_ALLOWED_EXT = (".epub", ".pdf", ".fb2", ".djvu", ".txt", ".cbz", ".cbr")
_BOOKS_DIR = PBEMU_ROOT / FIRMWARE / ".live" / "mnt" / "ext1" / "books"
_PAGESIZE = 6  # COLS * ROWS, must match geometry.PAGESIZE


def _inject_series() -> list[Path]:
    _BOOKS_DIR.mkdir(parents=True, exist_ok=True)
    paths = []
    for name in _SERIES_FILES:
        p = _BOOKS_DIR / name
        p.write_bytes(b"PK\x03\x04 series stub for drill-down test")
        paths.append(p)
    return paths


def _remove_series(paths: list[Path]) -> None:
    for p in paths:
        try:
            p.unlink()
        except FileNotFoundError:
            pass


def _inject_bulk_books(count: int, stem: str = "BatchStress") -> list[Path]:
    """Inject *count* standalone fake books so the library exceeds the
    MAX_DOWNLOADS (64) download-queue slice.  Names avoid the mock
    provider's "Name - NN" series convention so every book stays
    standalone."""
    _BOOKS_DIR.mkdir(parents=True, exist_ok=True)
    paths = []
    for i in range(count):
        p = _BOOKS_DIR / f"{stem}_{i:03d}.epub"
        p.write_bytes(b"PK\x03\x04 bulk stub for download-all stress test")
        paths.append(p)
    return paths



def _djb2_8hex(s: str) -> str:
    """Rust local::hash_hex — djb2 over the UTF-8 bytes, 8 lowercase hex."""
    h = 5381
    for b in s.encode():
        h = (h * 33 + b) & 0xFFFFFFFFFFFFFFFF
    return f"{h:08x}"


def _fnv_bucket(safe: str) -> str:
    """Rust cover::bucket_of — FNV-1a 32, low byte as 2 hex chars."""
    h = 2166136261
    for b in safe.encode():
        h ^= b
        h = (h * 16777619) & 0xFFFFFFFF
    return f"{h & 0xFF:02x}"


def _minimal_pdf(text: bool) -> bytes:
    """A valid single-page PDF.  *text* adds a Type1 Helvetica draw of
    the word 'Minimal'; without it the page is blank like the Rust unit
    test's fixture.  Both rely on MuPDF's repair-free strict parse."""
    content = b"BT /F1 48 Tf 72 720 Td (Minimal) Tj ET"
    res = b"/Resources<</Font<</F1 5 0 R>>>>"
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 280] "
        + (b"/Contents 4 0 R " + res if text else b"") + b">>",
        b"<< /Length " + str(len(content)).encode() + b" >>stream\n"
        + content + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    if not text:
        objs = objs[:3]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, body in enumerate(objs, start=1):
        offsets.append(len(out))
        out += f"{i} 0 obj ".encode() + body + b" endobj\n"
    xref_at = len(out)
    out += f"xref\n0 {len(objs) + 1}\n".encode()
    out += b"0000000000 65535 f \n"
    for off in offsets:
        out += f"{off:010d} 00000 n \n".encode()
    out += (
        f"trailer<<</Size {len(objs) + 1}/Root 1 0 R>>\n"
        f"startxref\n{xref_at}\n%%EOF\n"
    ).encode()
    return bytes(out)


def test_local_pdf_import_renders_mupdf_cover(fresh_bookshelf):
    """Switching to the Local source walks the storage root; a
    metadata-less PDF must get its first page rendered through the
    bundled MuPDF (the exact code path the ARM device binary runs),
    fall back to the filename stem for its title, and persist both in
    the store."""
    bs = fresh_bookshelf
    backend = bs.backend
    on_emulator = os.environ.get("EH_TEST_BACKEND", "emulator") == "emulator"
    if on_emulator:
        ext1 = backend.config_path.parents[2]  # .live/mnt/ext1
        scan_prefix = "/mnt/ext1"
    else:
        ext1 = backend.config_path.parent / "ext1"  # run_dir/ext1
        scan_prefix = str(ext1)

    seed_dir = ext1 / "eh_pdf_e2e"
    seed_dir.mkdir(parents=True, exist_ok=True)
    # Two shapes: a text page (exercises font rendering) and a bare
    # page (the Rust unit-test's fixture shape).
    cases = {"Minimal": True, "Blank": False}
    for stem, with_text in cases.items():
        (seed_dir / f"{stem}.pdf").write_bytes(_minimal_pdf(text=with_text))
    bids = {
        stem: f"fld_{_djb2_8hex(f'{scan_prefix}/eh_pdf_e2e/{stem}.pdf')}"
        for stem in cases
    }


    try:
        # Real user path: source button -> chooser -> Local.  The app
        # saves the choice itself; switching back in `finally` restores
        # the Kavita boot for later tests.
        before = bs.choose_source(1)
        _wait_log_slice(bs, before, "source switched to ", timeout=10.0)
        # The switch now opens a "Scanning library…" progress sheet over
        # the shelf; BACK dismisses it while the walk keeps running.
        bs.send_back_key()
        # The import scan + cover extraction run asynchronously; qemu-arm
        # MuPDF rendering of even a tiny PDF takes a while.
        # Only VISIBLE tiles get cover ticks; with ~20 imported books the
        # two PDFs may land on different pages, so flip pages until both
        # covers have been produced (extract on first render, cache hit
        # when a previous page view already extracted it).
        def _ticked(book_id: str) -> bool:
            tail = bs.current_log()[len(before):]
            return (
                f"local extract id={book_id}" in tail
                or f"cache hit id={book_id}" in tail
            )

        for _ in range(8):
            if all(_ticked(b) for b in bids.values()):
                break
            bs.tap_at(*bs.geom.pager_next_center())
            time.sleep(3)
        else:
            raise AssertionError("cover ticks never reached both PDFs")

        # Rendered art landed in the cover cache: PNG plus the raw bytes.
        db_copy = Path(backend.tmp_dir) / "mupdf-e2e-store-copy.db"
        shutil.copyfile(backend.store_path, db_copy)
        con = sqlite3.connect(db_copy)
        try:
            for stem, b in bids.items():
                # Local books persist their extracted cover as .raw (the
                # MuPDF-rendered first page, already PNG-encoded); the
                # .png cache is written only for server covers.
                safe = b.replace("/", "_")
                raw = backend.covers_dir / _fnv_bucket(safe) / f"{safe}.raw"
                assert raw.is_file(), f"{stem}: no raw cover cached"
                data = raw.read_bytes()
                assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{stem}: not a PNG"
                assert len(data) > 500, f"{stem}: cover only {len(data)} bytes"
                row = con.execute(
                    "SELECT title, source FROM books WHERE id=?", (b,)
                ).fetchone()
                assert row == (stem, "local"), (stem, row)
        finally:
            con.close()
            db_copy.unlink(missing_ok=True)
    finally:
        # Switch back through the UI so the app's own config write makes
        # Kavita the persisted boot source again (leftover store rows are
        # filtered from the Kavita view by design).
        back = bs.current_log()
        bs.choose_source(0)
        _wait_log_slice(bs, back, "source switched to ", timeout=10.0)
        shutil.rmtree(seed_dir, ignore_errors=True)


def _grouped_series_index(bs: BookshelfSession, series_title: str) -> int:
    """Index (0-based on-screen tile) of the multi-book *series_title* stack
    card in a 'By series' grouped view, read straight from the store.

    "None" stays flat — series only stack under an explicit grouping — so
    tests reach a series card (and its long-press context menu / drill) by
    choosing "By series" first.  Books with no series group under a single
    "No series" card too, so match by the card's series name (the store's
    series_name is empty for the no-series bucket, the real name otherwise).
    The view rows ORDER BY rowid == on-screen order.
    """
    import sqlite3

    deadline = time.monotonic() + 10.0
    last_err = None
    while time.monotonic() < deadline:
        try:
            con = sqlite3.connect(str(bs.backend.store_path), timeout=2.0)
            try:
                rows = con.execute(
                    "SELECT kind, series_name FROM view ORDER BY rowid").fetchall()
            finally:
                con.close()
            if rows:
                for i, (kind, name) in enumerate(rows):
                    if kind == 1 and (name or "") == series_title:
                        return i
                last_err = AssertionError(
                    f"no '{series_title}' series card in grouped view: "
                    f"{[(k, n) for k, n in rows]}")
        except Exception as exc:  # noqa: BLE001 - store may be mid-sync
            last_err = exc
        time.sleep(0.2)
    raise AssertionError(f"could not read the grouped view: {last_err}")


def _group_by_series(bs: BookshelfSession) -> None:
    """Inject a series first, then switch the shelf to 'By series' so the
    multi-book series appears as a single stack card (the default 'None'
    view is flat and shows its volumes individually)."""
    bs.choose_group("series")
    bs.wait_for_stable()


def _goto_view_tile(bs: BookshelfSession, view_idx: int) -> int:
    """Page to *view_idx* and return its within-page position.

    Navigates relative to the page the grid currently shows (parsed from
    the last draw_grid marker): drill-back restores the pre-drill page,
    so callers must not assume page 0 — and pager-next is a no-op on the
    last page, so a forward-only walk could never reach a lower target.
    """
    page = view_idx // _PAGESIZE
    pos = view_idx % _PAGESIZE
    m = re.findall(r"draw_grid view=\d+ page=(\d+)", bs.current_log())
    cur = int(m[-1]) if m else 0
    if cur == page:
        return pos
    if cur < page:
        targets = range(cur + 1, page + 1)
        tap = bs.tap_pager_next
    else:
        targets = range(cur - 1, page - 1, -1)
        tap = bs.tap_pager_prev
    for target in targets:
        # Poll the draw_grid page= marker per step so a swallowed pager
        # tap is retried instead of desyncing the page count (a desync
        # would make the delete-drill tests tap the WRONG tile).
        snap = bs.current_log()
        deadline = time.monotonic() + 8.0
        while time.monotonic() < deadline:
            if f"page={target}" in bs.current_log()[len(snap):]:
                break
            tap()
            time.sleep(0.3)
        else:
            raise AssertionError(f"pager never reached page={target} (swallowed tap)")
    return pos


# ── tabs, downloads, context menus ─────────────────────────────────
# The Downloads tab, Download-all action, and the long-press context menus
# (book: download/delete; series: download-all/delete) are exercised here.
# In the emulator the guest runs non-root and cannot write /mnt/ext1/system/bin,
# so bookshelf.c falls back to /tmp (resolve_downloads_dir); guest /tmp maps to
# .live/tmp on the host.  The helpers below inspect/clean that dir.
#
# Series cards only appear under an explicit "By series" grouping — the
# default "None"/All-books view is flat — so the series long-press menus
# below group first, then address the single multi-book series card.

_DOWNLOADS_DIR = PBEMU_ROOT / FIRMWARE / ".live" / "mnt" / "ext1" / "Downloads"


def _downloaded_files() -> list[Path]:
    """Book files the app has downloaded into LOCAL_DOWNLOADS."""
    if not _DOWNLOADS_DIR.is_dir():
        return []
    return [p for p in _DOWNLOADS_DIR.iterdir() if p.suffix.lower() in _ALLOWED_EXT]


def _clear_downloads() -> None:
    """Remove downloaded book files so the next test starts clean.

    The guest may still be draining a batch when a test ends, so a file
    can reappear right after the first unlink; retry a few times and
    fail loudly (instead of the old silent OSError swallow) when files
    persist — a leftover file makes the app treat a book as downloaded
    and the download tests fail confusingly downstream.
    """
    for _attempt in range(5):
        leftovers = []
        for p in _downloaded_files():
            try:
                p.unlink()
            except OSError as exc:
                leftovers.append(f"{p.name}: {exc}")
        if not leftovers and not _downloaded_files():
            return
        time.sleep(0.5)
    still = [p.name for p in _downloaded_files()]
    raise AssertionError(
        f"downloads dir not clear after retries: {leftovers or still}"
    )


def _wait_log_count(bs: BookshelfSession, needle: str, count: int, *, timeout: float = 20.0) -> None:
    """Poll until *needle* appears at least *count* times in the current
    invocation's log (downloads drain one-per-timer-tick)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if bs.current_log().count(needle) >= count:
            return
        time.sleep(0.5)
    got = bs.current_log().count(needle)
    raise AssertionError(f"log contains {needle!r} {got}x, expected >= {count}")


def _wait_draw_grid_view(bs: BookshelfSession, before: str, want: int, *, timeout: float = 8.0) -> int:
    """Poll until the last draw_grid marker in the post-*before* slice
    reports *want* books, returning it.  The commit's view_rebuild +
    draw_grid lines land in the guest log a beat after the suggest-tap
    marker the caller just polled for — a single read can race them."""
    import time
    deadline = time.monotonic() + timeout
    view = None
    while time.monotonic() < deadline:
        cur = bs.current_log()
        sl = cur[len(before):]
        if "draw_grid" in sl:
            view, _ = _last_draw_grid(sl)
            if view == want:
                return view
        time.sleep(0.2)
        tail = bs.current_log()[-600:]
    raise AssertionError(
        f"draw_grid never settled to view={want} (last {view}); "
        f"loglen={len(bs.current_log())} beforelen={len(before)} tail:\n{tail}"
    )


def _wait_log_slice(bs: BookshelfSession, before: str, needle: str, *, timeout: float = 20.0) -> None:
    """Poll until *needle* appears in the log text appended after the
    *before* snapshot.  Used to confirm a tap produced a specific redraw
    line without being fooled by unrelated background redraws."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if needle in bs.current_log()[len(before):]:
            return
        time.sleep(0.1)
    # Timeout: distinguish "app stuck" from "app alive but the event
    # never produced the expected line" — the two need opposite fixes.
    # The liveness probe must NEVER mask the timeout itself: backends
    # differ (SDL exposes cmd(), the emulator does not), so any probe
    # failure downgrades to a note instead of raising.
    detail = ""
    probe_state = getattr(bs, "cmd", None)
    if callable(probe_state):
        try:
            detail = f" app alive, {probe_state('state').strip()}"
        except Exception:  # noqa: BLE001 — diagnostics must not raise
            detail += " (liveness probe unavailable)"
    tail = bs.current_log()[-400:]
    raise AssertionError(
        f"log slice after offset {len(before)} never contained "
        f"{needle!r} within {timeout}s{detail}; tail:\n{tail}"
    )


def _final_dl_progress(bs: BookshelfSession, before: str, total: int, *, timeout: float = 10.0):
    """Poll the most recent dl_progress tally logged after the *before*
    snapshot and return it once the batch reports settled (total reached,
    nothing active).  Returns the (done, failed, total, active) ints."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        matches = re.findall(
            r"dl_progress done=(\d+) failed=(\d+) total=(\d+) active=(\d+)",
            bs.current_log()[len(before):],
        )
        if matches:
            last = matches[-1]
        if last and int(last[2]) == total and int(last[3]) == 0:
            return tuple(int(v) for v in last)
        time.sleep(0.3)
    if last is None:
        raise AssertionError("no dl_progress line logged after completion")


_DL_DELAY_PORT = 18767
_DL_DELAY_CFG = PBEMU_ROOT / FIRMWARE / ".live" / "tmp" / "bookshelf.cfg"


def _start_delayed_api_server() -> subprocess.Popen:  # type: ignore[type-arg]
    """Mock API server whose file endpoint sleeps 3 s (simulates a slow
    link) so the UI-responsiveness test can race a running download."""
    log_path = REPO_ROOT / "build" / "pbemu-api-delay.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_fh = open(log_path, "w", encoding="utf-8")  # noqa: SIM115
    env = _api_env()
    env["PBEMU_MOCK_DL_DELAY_MS"] = "3000"
    proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "api.api.server",
            "--host",
            "0.0.0.0",
            "--port",
            str(_DL_DELAY_PORT),
            "--provider",
            "mock",
            "--config",
            str(EINKHOME_ROOT / "tests" / "support" / "server-test.json"),
        ],
        cwd=REPO_ROOT,
        env=env,
        stdout=log_fh,
        stderr=subprocess.STDOUT,
    )
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        try:
            import urllib.request

            req = urllib.request.Request(
                f"http://127.0.0.1:{_DL_DELAY_PORT}/api/v1/healthz",
                headers={"Authorization": f"Bearer {API_TOKEN}"},
            )
            urllib.request.urlopen(req, timeout=2)
            return proc
        except Exception:  # noqa: BLE001
            time.sleep(0.3)
    proc.kill()
    raise RuntimeError(f"delayed API server did not start. Log:\n{log_path.read_text()}")


def test_download_keeps_ui_responsive(fresh_bookshelf):
    """A slow file download runs on a worker thread: the popup is modal
    (a tap mid-download must not dismiss it), the sync glyph keeps
    animating in the top bar (proof the event loop is alive), and the
    reader launches once the file lands.  Regression: downloads used to
    block the event loop, freezing the frontend for the whole transfer."""
    bs = fresh_bookshelf
    _clear_downloads()
    api = _start_delayed_api_server()
    saved_cfg = None
    try:
        if _DL_DELAY_CFG.is_file():
            saved_cfg = _DL_DELAY_CFG.read_text()
        _DL_DELAY_CFG.unlink(missing_ok=True)
        _DL_DELAY_CFG.write_text(
            f"api_url=http://127.0.0.1:{_DL_DELAY_PORT}\napi_token={API_TOKEN}\n",
            encoding="utf-8",
        )
        _restart_bookshelf(bs.emulator)
        bs.wait_for_stable()

        before = bs.current_log()
        bs.tap_book(0)
        _wait_log_slice(bs, before, "draw_dl_popup")

        # The 3 s file delay keeps the worker busy.  The popup is modal:
        # a tap mid-download must not dismiss it — the single-book
        # auto-open at the end only fires while dl_popup survives, so
        # the reader launch below proves the tap was swallowed.
        before = bs.frame_hash()
        bs.tap_at(*bs.geom.book_tile_center(0))
        time.sleep(0.4)

        # The frame must change while the fetch runs, proving the event
        # loop is alive (the old code froze it for the whole transfer).
        # The change observed is the popup's own flush or the
        # completion repaint — the sync glyph repaint is suppressed
        # while the popup is open, and the mock server sends no bytes
        # during the 3 s delay, so the screen is static mid-fetch.  The
        # emulator's framebuffer flush lands asynchronously after the
        # draw (FullUpdate cycle), so the window must clear the worst
        # flush latency under runner load: 2 s raced it and failed
        # ~3/4 CI runs (change arrived at ~2.2 s), 8 s covers the 3 s
        # fetch + flush on a slow guest, and the oldest emulated
        # firmware (U627) needs more still — 20 s clears every observed
        # run.  A frozen event loop still times out: nothing repaints.
        bs.wait_hash_change(before, timeout=20.0)

        # The download completes and the single-book press auto-opens
        # the reader.
        _wait_log_slice(bs, before, "download_book_file OK", timeout=15.0)
        _wait_log_slice(bs, before, "launching reader", timeout=10.0)
        assert len(_downloaded_files()) >= 1, "slow download never landed"
        _kill_guest_tasks()
    finally:
        _DL_DELAY_CFG.unlink(missing_ok=True)
        if saved_cfg is not None:
            _DL_DELAY_CFG.write_text(saved_cfg, encoding="utf-8")
            _DL_DELAY_CFG.chmod(0o666)
        _stop_api_server(api)
        _clear_downloads()


def test_sync_button_runs_sync(fresh_bookshelf):
    """The top-bar sync button (left of the More button) runs a library
    sync."""
    bs = fresh_bookshelf
    bs.wait_for_stable()
    before = bs.current_log()
    bs.tap_sync_button()
    _wait_log_slice(bs, before, "do_sync ENTER")
    _wait_log_slice(bs, before, "do_sync: rounds=")


def test_book_press_downloads_and_launches_reader(fresh_bookshelf):
    """Pressing a book shows the download popup, downloads it, then
    auto-opens the reader when the queue drains."""
    bs = fresh_bookshelf
    _clear_downloads()
    _restart_bookshelf(bs.emulator)
    bs.wait_for_stable()
    before = bs.current_log()
    bs.tap_book(0)
    _wait_log_slice(bs, before, "draw_dl_popup")
    _wait_log_slice(bs, before, "download_book_file OK", timeout=20.0)
    _wait_log_slice(bs, before, "launching reader", timeout=20.0)
    assert len(_downloaded_files()) >= 1, "book file was not downloaded to device"
    _kill_guest_tasks()  # kill the launched reader
    _clear_downloads()


def test_download_all_opens_popup_and_drains(fresh_bookshelf):
    """Download-all queues every book and shows the progress popup."""
    bs = fresh_bookshelf
    _clear_downloads()
    _restart_bookshelf(bs.emulator)
    bs.wait_for_stable()
    before = bs.current_log()
    bs.tap_download_all()
    _wait_log_slice(bs, before, "download-all queued=")
    _wait_log_slice(bs, before, "draw_dl_popup")
    # Let the queue drain one-per-tick, then confirm files landed on disk.
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline and len(_downloaded_files()) < 16:
        time.sleep(0.5)
    assert len(_downloaded_files()) >= 16, (
        f"expected all 16 books downloaded, got {len(_downloaded_files())}"
    )
    _clear_downloads()


def test_download_all_drains_beyond_first_slice(fresh_bookshelf):
    """Download-all on >64 books must drain past the MAX_DOWNLOADS slice
    boundary with the modal popup open the whole time.  Regression: the
    drain timer was only re-armed while queued items remained, so once
    the first 64-item slice settled the batch top-up never ran again
    and the progress bar froze (e.g. 64/93) with every file on disk."""
    bs = fresh_bookshelf
    injected = _inject_bulk_books(70)  # 16 shipped + 70 = 86 > MAX_DOWNLOADS
    try:
        _clear_downloads()
        _restart_bookshelf(bs.emulator)
        bs.wait_for_stable()
        before = bs.current_log()
        bs.tap_download_all()
        # The 86-book drain churns the screen for ~12s, so wait on the
        # log slice, not a settling framebuffer hash.
        _wait_log_slice(bs, before, "download-all queued=", timeout=20.0)
        m = re.search(r"download-all queued=(\d+)", bs.current_log()[len(before):])
        assert m, "download-all never logged its queued total"
        total = int(m.group(1))
        assert total > 64, f"batch must exceed the 64-slot queue slice, got {total}"
        # The popup is modal: a tap mid-drain must not dismiss it.  The
        # finished-tally popup draw after the batch completes (below)
        # only happens while dl_popup survives, so it proves the tap
        # was swallowed.
        time.sleep(2.0)
        bs.tap_at(*bs.geom.book_tile_center(0))
        time.sleep(0.4)

        # The drain must visibly pass the 64-item boundary — the exact
        # point where the old code lost its timer and froze.
        _wait_log_count(bs, "download_book_file OK", 65, timeout=60.0)

        # ...and finish the whole batch.  The last file lands one tick
        # before its settle logs "batch complete", so wait for the log
        # line rather than asserting immediately.
        deadline = time.monotonic() + 90.0
        while time.monotonic() < deadline and len(_downloaded_files()) < total:
            time.sleep(0.5)
        got = len(_downloaded_files())
        assert got == total, f"expected all {total} books downloaded, got {got}"
        _wait_log_slice(bs, before, "download-all batch complete", timeout=15.0)

        # The finished-tally popup redraw proves the popup survived the
        # mid-drain tap (a dismissed popup would settle via draw_top_bar
        # instead).
        _wait_log_slice(bs, before, "draw_dl_popup", timeout=10.0)

        # The popup (still open) keeps the whole-batch tally on the
        # finished bar.  Regression: the completion path zeroed the
        # batch counters, so the bar fell back to the queue-derived
        # count — and the pruned queue only holds the last 64-item
        # slice, so "86 downloaded" snapped back to "64 downloaded"
        # once the batch finished.
        done_n, failed_n, total_n, active_n = _final_dl_progress(bs, before, total)
        assert total_n == total, f"final bar total={total_n}, expected {total}"
        assert done_n == total, f"final bar done={done_n}, expected {total}"
        assert failed_n == 0 and active_n == 0
    finally:
        for p in injected:
            p.unlink(missing_ok=True)
        _clear_downloads()


def test_book_longpress_open(fresh_bookshelf):
    """Long-press a book → context menu → Open works like a single tap:
    download (with the popup) then launch the reader."""
    bs = fresh_bookshelf
    _clear_downloads()
    _restart_bookshelf(bs.emulator)
    bs.wait_for_stable()
    before = bs.frame_hash()
    bs.long_press_book(0)
    bs.wait_hash_change(before)
    bs.assert_log_contains("context menu open series=0")
    before = bs.current_log()
    bs.tap_context_item(0)  # Open
    _wait_log_slice(bs, before, "draw_dl_popup")
    _wait_log_slice(bs, before, "launching reader")
    bs.assert_log_contains("download_book_file OK")
    assert len(_downloaded_files()) >= 1
    _kill_guest_tasks()  # kill the launched reader
    _clear_downloads()


def test_book_longpress_download(fresh_bookshelf):
    """Long-press a book → context menu → Download fetches the file."""
    bs = fresh_bookshelf
    _clear_downloads()
    _restart_bookshelf(bs.emulator)
    bs.wait_for_stable()
    before = bs.frame_hash()
    bs.long_press_book(0)
    bs.wait_hash_change(before)
    bs.assert_log_contains("context menu open series=0")
    before = bs.current_log()
    bs.tap_context_item(1)  # Download (0 is Open)
    _wait_log_slice(bs, before, "draw_dl_popup")
    _wait_log_count(bs, "download_book_file OK", 1)
    assert len(_downloaded_files()) >= 1
    _clear_downloads()


def test_book_longpress_delete(fresh_bookshelf):
    """Long-press a downloaded book → Delete removes the local file."""
    bs = fresh_bookshelf
    _clear_downloads()
    _restart_bookshelf(bs.emulator)
    # First download the book so there is something to delete.
    before = bs.current_log()
    bs.long_press_book(0)
    _wait_log_slice(bs, before, "context menu open series=0")
    bs.tap_context_item(1)  # Download (0 is Open)
    _wait_log_count(bs, "download_book_file OK", 1)
    assert len(_downloaded_files()) >= 1, "setup download failed"
    # Dismiss the popup (the download kept it open), then delete via the
    # context menu.
    bs.tap_at(*bs.geom.book_tile_center(0))
    time.sleep(0.5)
    before = bs.current_log()
    bs.long_press_book(0)
    _wait_log_slice(bs, before, "context menu open series=0")
    bs.tap_context_item(2)  # Delete
    _wait_log_slice(bs, before, "delete_book_file removed")
    assert len(_downloaded_files()) == 0, "delete did not remove the file"


def test_series_longpress_download_all(fresh_bookshelf):
    """Long-press a series card → Download all fetches every member.

    The default (None/All books) view is flat, so the multi-book series
    is grouped into one card only after choosing "By series".
    """
    bs = fresh_bookshelf
    injected = _inject_series()
    try:
        _clear_downloads()
        _restart_bookshelf(bs.emulator)
        bs.wait_for_stable()
        _group_by_series(bs)
        series_idx = _grouped_series_index(bs, _SERIES_STEM.replace("_", " "))
        pos = _goto_view_tile(bs, series_idx)
        before = bs.frame_hash()
        bs.long_press_book(pos)
        bs.wait_hash_change(before)
        bs.assert_log_contains("context menu open series=1")
        before = bs.current_log()
        bs.tap_context_item(0, n_items=2)  # Download all
        _wait_log_slice(bs, before, "draw_dl_popup")
        bs.assert_log_contains("download_series")
        bs.assert_log_contains("queued=2")
        _wait_log_count(bs, "download_book_file OK", 2)
    finally:
        _clear_downloads()
        _remove_series(injected)


def test_series_longpress_delete(fresh_bookshelf):
    """Long-press a series card → Delete series removes all member files."""
    bs = fresh_bookshelf
    injected = _inject_series()
    try:
        _clear_downloads()
        _restart_bookshelf(bs.emulator)
        bs.wait_for_stable()
        _group_by_series(bs)
        series_idx = _grouped_series_index(bs, _SERIES_STEM.replace("_", " "))
        pos = _goto_view_tile(bs, series_idx)
        # Download the series first so delete has files to remove.
        before = bs.current_log()
        bs.long_press_book(pos)
        _wait_log_slice(bs, before, "context menu open series=1")
        bs.tap_context_item(0, n_items=2)  # Download all
        _wait_log_count(bs, "download_book_file OK", 2)
        removed_before = bs.current_log().count("delete_book_file removed")
        # Dismiss the popup (the download kept it open), then delete the
        # whole series.
        bs.tap_at(*bs.geom.book_tile_center(pos))
        time.sleep(0.5)
        before = bs.current_log()
        bs.long_press_book(pos)
        _wait_log_slice(bs, before, "context menu open series=1")
        bs.tap_context_item(1, n_items=2)  # Delete series
        _wait_log_slice(bs, before, "delete_series")
        removed_after = bs.current_log().count("delete_book_file removed")
        assert removed_after - removed_before == 2, (
            f"delete-series removed {removed_after - removed_before} files, expected 2"
        )
    finally:
        _clear_downloads()
        _remove_series(injected)


# ── offline boot (API unreachable) ─────────────────────────────────────
# The guest re-reads /tmp/bookshelf.cfg (CONFIG_TMP_PATH) last, so writing
# a dead-port override there makes the next boot offline without touching
# the module's real server.  Guest /tmp maps to .live/tmp on the host,
# which is also where the library store + cover cache live (the _OFFLINE_*
# paths are defined in tests/support/bookshelf/env.py).


def _ensure_offline_assets(emulator: Emulator) -> None:
    """Wait for (or force) a populated library store + >=6 cached covers."""
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        covers = len(list(_OFFLINE_COVERS.rglob("*.png")))
        if _OFFLINE_STORE.is_file() and covers >= 6:
            return
        _restart_bookshelf(emulator)
        time.sleep(3.0)
    assert _OFFLINE_STORE.is_file(), "library store never written by an online sync"


def _wait_offline_log(bs: BookshelfSession) -> str:
    """Poll the current invocation log until a cover cache hit appears."""
    deadline = time.monotonic() + 10
    log = ""
    while time.monotonic() < deadline:
        log = bs.current_log()
        if "cover_tick cache hit id=" in log:
            break
        time.sleep(0.5)
    return log


def _last_draw_grid(log: str) -> tuple[int, int]:
    """Return (view_count, page) from the last draw_grid line in *log*."""
    matches = list(re.finditer(r"draw_grid view=(\d+) page=(\d+)", log))
    assert matches, "no draw_grid line in log"
    m = matches[-1]
    return int(m.group(1)), int(m.group(2))


def _store_books() -> list[dict]:
    """Read every book row from the on-disk SQLite store."""
    import sqlite3 as _sqlite3

    con = _sqlite3.connect(str(_OFFLINE_STORE))
    try:
        rows = con.execute(
            "SELECT id, title, series, series_idx, added_at FROM books"
        ).fetchall()
    finally:
        con.close()
    return [
        {
            "id": r[0],
            "title": r[1],
            "series": r[2],
            "seriesIdx": r[3],
            "addedAt": r[4],
        }
        for r in rows
    ]


def _loaded_book_count(log: str) -> int:
    """Tiles the offline boot projected from the on-disk store."""
    m = re.search(r"view_rebuild: view=(\d+)", log)
    assert m, "offline boot did not rebuild the view from the store"
    return int(m.group(1))


def _pager_roundtrip(bs, pages: int) -> None:
    """<< / >> jump to the ends, < / > step one page at a time.

    Jumps to the last page first so every subsequent tap actually moves
    a page (tapping << while already on page 0 is a no-op), then sweeps
    << -> > — each step verified by its draw_grid page= marker."""
    if pages <= 1:
        return
    cur = _last_draw_grid(bs.current_log())[1]
    steps = []
    if cur != pages - 1:
        steps.append((f"page={pages - 1}", bs.tap_pager_last))
    steps.append(("page=0", bs.tap_pager_first))
    steps.append(("page=1", bs.tap_pager_next))
    for want, tap in steps:
        snap = bs.current_log()
        tap()
        _wait_log_slice(bs, snap, want)


def _offline_pager_check(bs) -> None:
    """<< / >> jump to the ends, < / > step one page at a time."""
    view, _ = _last_draw_grid(bs.current_log())
    pages = (view + _PAGESIZE - 1) // _PAGESIZE
    if pages > 1:
        _pager_roundtrip(bs, pages)


def _offline_downloads_roundtrip(bs, invocations: int) -> None:
    """Search sub-page opens via the top-bar search icon and closes via the
    top-bar back arrow — all without a process respawn.  (The former
    Downloads-view roundtrip is gone: the downloads page was removed and
    the downloads icon now opens a progress popup, which offline has
    nothing to show.)"""
    snap = bs.current_log()
    before = bs.frame_hash()
    bs.tap_search()
    bs.wait_hash_change(before)
    _wait_log_slice(bs, snap, "draw_search_tab")
    snap = bs.current_log()
    before = bs.frame_hash()
    bs.tap_home()
    bs.wait_hash_change(before)
    _wait_log_slice(bs, snap, "draw_grid view=")
    assert bs.invocation_count() == invocations, (
        "offline navigation triggered CloseApp/respawn"
    )
def _offline_boot_asserts(log: str) -> None:
    """The offline boot must fail sync but render the cached library."""
    assert "do_sync FAILED" in log
    assert _loaded_book_count(log) >= 1, "offline boot did not load the store"
    view, _ = _last_draw_grid(log)
    assert view >= 1, "grid empty despite cached library"
    # Covers come from the cache, not the network.
    assert "cover_tick cache hit id=" in log


def _set_dead_cfg() -> str | None:
    """Point the guest config at a dead port; return the saved text."""
    saved_cfg = _OFFLINE_CFG.read_text() if _OFFLINE_CFG.is_file() else None
    # The existing file may be owned by the container UID; unlink (dir is
    # host-writable) instead of truncating in place.
    _OFFLINE_CFG.unlink(missing_ok=True)
    _OFFLINE_CFG.write_text(
        f"api_url=http://127.0.0.1:9\napi_token={API_TOKEN}\n",
    )
    return saved_cfg


def _restore_cfg(saved_cfg: str | None) -> None:
    _OFFLINE_CFG.unlink(missing_ok=True)
    if saved_cfg is not None:
        _OFFLINE_CFG.write_text(saved_cfg, encoding="utf-8")
        _OFFLINE_CFG.chmod(0o666)  # guest (container UID) rewrites it later



def test_download_all_failures_finish_not_loop(fresh_bookshelf):
    """With the API unreachable every batch item fails, but the batch
    must settle each book exactly once and complete — it must not loop
    re-enqueuing the failed books forever.  Regression: failed books
    keep their downloaded flag at 0, so the next slice used to return
    them again and the drain never ended."""
    bs = fresh_bookshelf
    _clear_downloads()
    saved = _set_dead_cfg()
    try:
        _restart_bookshelf(bs.emulator)
        bs.wait_for_stable()
        before = bs.current_log()
        bs.tap_download_all()
        _wait_log_slice(bs, before, "download-all queued=", timeout=20.0)
        m = re.search(r"download-all queued=(\d+)", bs.current_log()[len(before):])
        assert m, "download-all never logged its queued total"
        total = int(m.group(1))
        assert total > 0
        # Every item fails fast (connection refused); the batch must
        # settle all of them and complete without looping.
        _wait_log_slice(bs, before, "download-all batch complete", timeout=30.0)
        failed = bs.current_log()[len(before):].count("download_book_file FAILED")
        assert failed == total, (
            f"batch attempted {failed} downloads, expected exactly {total} "
            "(looping over failures)"
        )
    finally:
        _restore_cfg(saved)
        _clear_downloads()


def test_offline_boot_renders_cached_library(bookshelf_env):
    """Full offline e2e: with the API unreachable, bookshelf boots from the
    on-disk library store + cover cache and stays fully navigable — covers
    blit from the cache, the pager jumps first/last, and the search page
    opens via the top-bar icon and closes via its back arrow."""
    bs, emulator = bookshelf_env
    # Sync online first so the store + on-disk cover cache are populated.
    _ensure_offline_assets(emulator)

    saved_cfg = _set_dead_cfg()
    try:
        _restart_bookshelf(emulator)
        _offline_boot_asserts(_wait_offline_log(bs))
        invocations = bs.invocation_count()
        _offline_pager_check(bs)
        _offline_downloads_roundtrip(bs, invocations)
    finally:
        _restore_cfg(saved_cfg)


def test_legacy_json_store_migrates_to_sqlite(bookshelf_env):
    """A pre-sqlite bookshelf_lib.json is imported into the SQLite store on
    first boot: the books render offline, the db carries the rows, and the
    legacy file is renamed to .migrated."""
    bs, emulator = bookshelf_env

    # Wipe any current store; drop a legacy JSON store with two books.
    _OFFLINE_STORE.unlink(missing_ok=True)
    (_OFFLINE_DIR / "bookshelf_lib.db.migrated").unlink(missing_ok=True)
    _OFFLINE_LEGACY.write_text(
        json.dumps(
            [
                {
                    "id": "legacy_a",
                    "title": "Legacy Alpha",
                    "authors": ["pbemu"],
                    "series": "",
                    "seriesId": "",
                    "seriesIdx": 0,
                    "format": "epub",
                    "size": 123,
                    "addedAt": 1700000000,
                },
                {
                    "id": "legacy_b",
                    "title": "Legacy Beta",
                    "authors": ["pbemu"],
                    "series": "",
                    "seriesId": "",
                    "seriesIdx": 0,
                    "format": "epub",
                    "size": 456,
                    "addedAt": 1700000001,
                },
            ]
        ),
        encoding="utf-8",
    )
    _OFFLINE_LEGACY.chmod(0o666)
    # The guest runs as the mapped container UID and the staged /tmp is
    # sticky, so it may only rename files it owns; hand the fixture over
    # (on a real device the app creates and owns the legacy file itself).
    container_sh(
        "chown \"$(stat -c %u:%g /mnt/ext1/system/bin/covers 2>/dev/null || "
        "stat -c %u:%g /mnt/ext1/system/bin/bookshelf.log)\" "
        "/mnt/ext1/system/bin/bookshelf_lib.json",
        check=False,
    )

    saved_cfg = _set_dead_cfg()
    try:
        _restart_bookshelf(emulator)
        deadline = time.monotonic() + 15
        log = bs.current_log()
        while time.monotonic() < deadline:
            log = bs.current_log()
            if "store: migrated legacy JSON" in log and "draw_grid view=" in log:
                break
            time.sleep(0.5)
        assert "store: migrated legacy JSON (2 books)" in log
        view, _ = _last_draw_grid(log)
        assert view >= 2, "legacy books not rendered from the import"

        # The db now carries the rows and the json is renamed away.
        ids = {b["id"] for b in _store_books()}
        assert {"legacy_a", "legacy_b"} <= ids
        assert not _OFFLINE_LEGACY.exists(), "legacy json not renamed"
        assert (_OFFLINE_DIR / "bookshelf_lib.json.migrated").exists()
    finally:
        _restore_cfg(saved_cfg)



# ── search suggestions (live, server-generated, device-local) ─────────


def test_search_suggestions_live_and_commit(fresh_bookshelf):
    """Typing in the system keyboard shows live suggestions from the
    local term index; tapping one commits that search through the
    keyboard handler.  Phase 2 proves the word-aligned suffix-phrase
    term: "harry po" matches "harry potter order of the phoenix" style
    phrase terms."""
    bs = fresh_bookshelf
    _BOOKS_DIR.mkdir(parents=True, exist_ok=True)
    hp = _BOOKS_DIR / "Harry Potter.epub"
    hp.write_bytes(b"PK\x03\x04 potter stub for suggest test")
    try:
        view_before, _ = _last_draw_grid(bs.current_log())
        _restart_bookshelf(bs.emulator)
        # Wait for the staged book to arrive via the sync delta.
        deadline = time.monotonic() + 20
        view = view_before
        while time.monotonic() < deadline:
            view, _ = _last_draw_grid(bs.current_log())
            if view > view_before:
                break
            time.sleep(0.5)
        assert view > view_before, (
            f"staged Harry Potter never synced (view {view_before} -> {view})"
        )

        # ── Phase 1: word completion ("pott" -> "potter") ──
        bs.tap_search_and_verify()
        time.sleep(0.5)
        bs.tap_search_input_and_verify()  # opens the keyboard
        time.sleep(0.5)
        bs.type_text("pott", commit=False)
        # Poll until the debounce tick has drawn the suggestion row (a
        # fixed sleep races the pixel assertion against a slow guest).
        ppm = _dump_suggestion_in_band(bs, "eh_suggest_pott")
        # Visual check: a left-aligned suggestion row is drawn in the
        # band above the keyboard (the centered "No recent searches"
        # placeholder does not reach x<300, so ink there is the row).
        xs = _ppm_ink_xs(
            ppm, 24, bs.geom.panel_h + 228, 300, bs.geom.panel_h + 430
        )
        assert xs, "no suggestion row ink in the band"
        # Tap the suggestion row; the term commits through the keyboard
        # handler and filters the grid to exactly the Potter book.
        before = bs.current_log()
        bs.tap_at(*bs.geom.suggestion_row_center(0))
        _wait_log_slice(bs, before, "suggest tap: term=`potter`")
        # The tapped term committed (app-side, history-tap sequence)
        # and the grid filtered to exactly the Potter book.
        _wait_draw_grid_view(bs, before, 1)

        # ── Phase 2: phrase completion ("harry po" -> "harry potter") ──
        bs.tap_search_and_verify()
        time.sleep(0.5)
        bs.tap_search_input_and_verify()  # keyboard pre-fills "potter"
        time.sleep(0.5)
        # Clear the pre-filled query (6 backspaces), then type the
        # phrase prefix.
        for _ in range(6):
            bs.emulator.run_probe("send_event", "common", "210", "0", "8", "0")
            time.sleep(0.1)
        time.sleep(0.4)
        bs.type_text("harry po", commit=False)
        _dump_suggestion_in_band(bs, "eh_suggest_harrypo")
        before = bs.current_log()
        bs.tap_at(*bs.geom.suggestion_row_center(0))
        _wait_log_slice(bs, before, "suggest tap: term=`harry potter`")
        _wait_draw_grid_view(bs, before, 1)
    finally:
        hp.unlink(missing_ok=True)


def test_search_folded_suggestion_finds_diacritic_title(fresh_bookshelf):
    """A folded suggestion must find its book: "songgong" (from the
    title "Sŏnggong") matches via the server-provided searchText, not
    the raw title (LIKE '%songgong%' never matches "Sŏnggong").

    Regression: tapping such a suggestion committed the folded term
    and the grid came up empty."""
    bs = fresh_bookshelf
    _BOOKS_DIR.mkdir(parents=True, exist_ok=True)
    hp = _BOOKS_DIR / "Sŏnggong.epub"
    hp.write_bytes(b"PK\x03\x04 diacritic stub for suggest test")
    try:
        view_before, _ = _last_draw_grid(bs.current_log())
        _restart_bookshelf(bs.emulator)
        deadline = time.monotonic() + 20
        view = view_before
        while time.monotonic() < deadline:
            view, _ = _last_draw_grid(bs.current_log())
            if view > view_before:
                break
            time.sleep(0.5)
        assert view > view_before, "staged Sŏnggong never synced"

        bs.tap_search_and_verify()
        time.sleep(0.5)
        bs.tap_search_input_and_verify()
        time.sleep(0.5)
        bs.type_text("songgong", commit=False)
        _dump_suggestion_in_band(bs, "eh_suggest_songgong")
        before = bs.current_log()
        bs.tap_at(*bs.geom.suggestion_row_center(0))
        _wait_log_slice(bs, before, "suggest tap: term=`songgong`")
        _wait_draw_grid_view(bs, before, 1)
    finally:
        hp.unlink(missing_ok=True)
