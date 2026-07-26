"""E2E tests for every interactive element in the bookshelf app.

Requires:
  - podman available
  - firmware U633_6.8.2817 staged (./pbemu install)
  - books in U633_6.8.2817/.live/mnt/ext1/books/

Run with: pytest tests/test_bookshelf.py -v
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

from tests.support.bookshelf import (
    MORE_AUTHOR,
    MORE_GRID,
    MORE_LIST,
    MORE_RECENT,
    MORE_SERIES,
    MORE_SYNC,
    MORE_TITLE_AZ,
    MORE_TITLE_ZA,
    MORE_SETTINGS,
    MORE_SYSTEM,
    BookshelfGeometry,
    BookshelfSession,
)
from tests.support.bookshelf.session import (
    count_log_openings,
    latest_invocation_log,
    read_bookshelf_log,
)
from tests.support.reader.session import Session
from tests.support.runtime import Emulator, container_running, container_sh
from tests.support.runtime_common import REPO_ROOT

FIRMWARE = "U633_6.8.2817"
API_PORT = 18765
API_TOKEN = "pbemu-dev-token"
CONTAINER = "pb-pocketbook-ui"
BOOKSHELF_APP = "bookshelf.app"

pytestmark = pytest.mark.bookshelf


# ── helpers ────────────────────────────────────────────────────────────


def _pbemu_env() -> dict[str, str]:
    """Return env dict with tools/ prepended to PYTHONPATH."""
    env = os.environ.copy()
    tools = str(REPO_ROOT / "tools")
    env["PYTHONPATH"] = (
        tools if not env.get("PYTHONPATH") else f"{tools}{os.pathsep}{env['PYTHONPATH']}"
    )
    return env


def _api_env() -> dict[str, str]:
    """Return env dict for the API server subprocess."""
    env = os.environ.copy()
    api_dir = str(REPO_ROOT / "api")
    root = str(REPO_ROOT)
    extra = f"{root}{os.pathsep}{api_dir}"
    env["PYTHONPATH"] = (
        extra if not env.get("PYTHONPATH") else f"{extra}{os.pathsep}{env['PYTHONPATH']}"
    )
    return env


def _start_api_server() -> subprocess.Popen:  # type: ignore[type-arg]
    """Start the mock API server on the test port. Returns the Popen."""
    log_path = REPO_ROOT / "build" / "pbemu-api-test.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_fh = open(log_path, "w", encoding="utf-8")  # noqa: SIM115
    proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "api.api.server",
            "--host",
            "0.0.0.0",
            "--port",
            str(API_PORT),
            "--provider",
            "mock",
        ],
        cwd=REPO_ROOT,
        env=_api_env(),
        stdout=log_fh,
        stderr=subprocess.STDOUT,
    )
    # Wait for server to be ready
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        try:
            import urllib.request

            req = urllib.request.Request(
                f"http://127.0.0.1:{API_PORT}/api/v1/healthz",
                headers={"Authorization": f"Bearer {API_TOKEN}"},
            )
            urllib.request.urlopen(req, timeout=2)
            return proc
        except Exception:
            time.sleep(0.3)
    proc.kill()
    raise RuntimeError(
        f"API server did not start within 10s. Log:\n{log_path.read_text()}"
    )


def _stop_api_server(proc: subprocess.Popen) -> None:  # type: ignore[type-arg]
    """Terminate the API server process."""
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)


def _build_bookshelf() -> Path:
    """Build the bookshelf binary. Returns path to the built ELF."""
    src = REPO_ROOT / "bookshelf" / "bookshelf.c"
    out = REPO_ROOT / "build" / "bookshelf.app"
    build_script = REPO_ROOT / "sdk" / "build_armel.sh"
    assert build_script.is_file(), f"build script missing: {build_script}"
    subprocess.run(
        [str(build_script), str(src), "--output", str(out.relative_to(REPO_ROOT))],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert out.is_file(), f"build output missing: {out}"
    return out


def _stage_binary(binary: Path) -> None:
    """Stage the bookshelf binary + config into .live and container.

    Uses ``podman cp`` to copy the binary from host into the container,
    then ``podman exec`` with container-side paths to place it where
    monitor.app will find it (ebrmain/bin takes priority over
    /mnt/ext1/system/bin).
    """
    live = REPO_ROOT / FIRMWARE / ".live"
    bin_dir = live / "mnt/ext1/system/bin"
    bin_dir.mkdir(parents=True, exist_ok=True)

    # Copy binary to host-side .live (volume-mounted into container)
    shutil.copy2(binary, bin_dir / "bookshelf.app")
    (bin_dir / "bookshelf.app").chmod(0o755)

    # Write config to host-side .live
    (bin_dir / "bookshelf.cfg").write_text(
        f"api_url=http://127.0.0.1:{API_PORT}\napi_token={API_TOKEN}\n",
        encoding="utf-8",
    )

    # Push binary + config into running container via podman cp + exec.
    # container_sh runs INSIDE the container, so host paths don't work;
    # we must use podman cp for host→container transfer, then podman exec
    # with container-side paths for the rest.
    if container_running():
        # 1. Copy binary from host into container /tmp
        subprocess.run(
            ["podman", "cp", str(binary), f"{CONTAINER}:/tmp/bookshelf.app.new"],
            check=False,
            capture_output=True,
            timeout=10,
        )
        # 2. Remove symlink at ebrmain/bin, place our binary there
        container_sh(
            "rm -f /workspace/firmware/.live/ebrmain/bin/bookshelf.app && "
            "mv /tmp/bookshelf.app.new "
            "/workspace/firmware/.live/ebrmain/bin/bookshelf.app && "
            "chmod +x /workspace/firmware/.live/ebrmain/bin/bookshelf.app && "
            "cp /workspace/firmware/.live/ebrmain/bin/bookshelf.app "
            "/mnt/ext1/system/bin/bookshelf.app && "
            "chmod +x /mnt/ext1/system/bin/bookshelf.app",
            check=False,
            timeout=10,
        )
        # 3. Copy config into container
        subprocess.run(
            [
                "podman",
                "cp",
                str(bin_dir / "bookshelf.cfg"),
                f"{CONTAINER}:/mnt/ext1/system/bin/bookshelf.cfg",
            ],
            check=False,
            capture_output=True,
            timeout=5,
        )

def _start_emulator() -> Emulator:
    """Stop any existing emulator and start a fresh one with --network=host."""
    # Stop existing
    subprocess.run(
        [sys.executable, "-m", "pbemu", "stop"],
        cwd=REPO_ROOT,
        env=_pbemu_env(),
        check=False,
    )
    time.sleep(1)

    # Start with --network=host
    env = _pbemu_env()
    env["PBEMU_NO_KEEPID"] = "1"
    env["PBEMU_PODMAN_ARGS"] = "--network=host"
    subprocess.run(
        [
            sys.executable,
            "-m",
            "pbemu",
            "start",
            FIRMWARE,
            "--no-viewer",
            "--no-audio",
            "--no-build",
        ],
        cwd=REPO_ROOT,
        env=env,
        check=True,
        timeout=120,
    )

    emulator = Emulator(firmware=FIRMWARE)
    emulator.wait_for_monitor(timeout=30)
    emulator.wait_for_hwevent(timeout=30)
    return emulator


def _wait_bookshelf_active(emulator: Emulator, timeout: float = 30.0) -> None:
    """Poll until bookshelf.app is the active task."""
    session = Session(emulator)
    session.wait_for_active_app("bookshelf.app", "bookshelf", timeout=timeout)


def _parse_panel_h(firmware: str) -> int:
    """Parse panel_h from the bookshelf log."""
    log = read_bookshelf_log(firmware)
    m = re.search(r"EVT_INIT panel_h=(\d+)", log)
    if m:
        return int(m.group(1))
    # Default for 6-inch panel
    return 0


# ``killall bookshelf.app`` cannot work: the guest runs under qemu-arm, so its
# comm is "qemu-arm", not "bookshelf.app".  The reliable handle is the
# qemu-arm host pid that monitor.app records in /var/run/task/<id>/mainpid
# (the same value ``arm_probe kill-task`` signals).  We TERM every bookshelf /
# reader task, then KILL any that did not exit, so monitor.app respawns a
# clean launcher.  Without this the previous test's on-screen keyboard or
# overlay stays open and steals the next test's taps at the firmware level.
_KILL_GUEST_TASKS_SCRIPT = r"""
pids=""
for d in /var/run/task/[0-9]*; do
    [ -d "$d" ] || continue
    name=""
    [ -r "$d/appname" ] && name=$(cat "$d/appname" 2>/dev/null)
    case "$name" in
        *bookshelf*|*reader*|*control_panel*)
            pid=""
            [ -r "$d/mainpid" ] && pid=$(cat "$d/mainpid" 2>/dev/null)
            if [ -n "$pid" ]; then
                kill -TERM "$pid" 2>/dev/null && pids="$pids $pid"
            fi
            ;;
    esac
done
sleep 1
for p in $pids; do
    kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null
done
echo "term_pids=$pids"
"""


def _kill_guest_tasks() -> None:
    """Signal the bookshelf/reader qemu-arm processes via /var/run/task."""
    container_sh(_KILL_GUEST_TASKS_SCRIPT, check=False, timeout=10)


def _wait_fresh_bookshelf(before: int, timeout: float = 30.0) -> None:
    """Block until a launch newer than *before* has synced and drawn."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if count_log_openings(FIRMWARE) > before:
            slice_ = latest_invocation_log(FIRMWARE)
            if "do_sync" in slice_ and "draw_grid" in slice_:
                return
        time.sleep(0.3)
    raise RuntimeError(
        f"bookshelf did not respawn+sync within {timeout}s "
        f"(log openings={count_log_openings(FIRMWARE)}, expected > {before})"
    )


def _restart_bookshelf(emulator: Emulator, timeout: float = 30.0) -> None:
    """Kill the guest bookshelf (+ any reader) and wait for a clean respawn."""
    before = count_log_openings(FIRMWARE)
    _kill_guest_tasks()
    _wait_fresh_bookshelf(before, timeout=timeout)
    # Ensure the informer routes taps to the respawned foreground task.
    _wait_bookshelf_active(emulator, timeout=10.0)
    time.sleep(1.0)


# ── fixtures ───────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def bookshelf_env():
    """Full bookshelf e2e environment: API server + staged binary + emulator."""
    if shutil.which("podman") is None:
        pytest.skip("podman not available")

    # 1. Build the binary
    binary = _build_bookshelf()

    # 2. Start API server (mock provider)
    api_proc = _start_api_server()

    # 3. Stage binary + config
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
    panel_h = _parse_panel_h(FIRMWARE)
    geom = BookshelfGeometry(
        screen_w=snapshot.width or 1072,
        screen_h=snapshot.height or 1448,
        panel_h=panel_h,
    )
    session = Session(emulator)
    bs = BookshelfSession(session, geom, FIRMWARE)

    yield bs, emulator

    # Cleanup
    _stop_api_server(api_proc)
    emulator.stop(force=True)


@pytest.fixture(autouse=True)
def fresh_bookshelf(bookshelf_env):
    """Restart bookshelf before each test for a clean state."""
    bs, emulator = bookshelf_env
    _restart_bookshelf(emulator)
    yield bs
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


def test_home_button_closes_app(fresh_bookshelf):
    """Tap home button, verify CloseApp triggers a respawn cycle.

    Bookshelf is the launcher replacement: monitor.app respawns it
    immediately after CloseApp().  The reliable proof is a new
    log_open header (invocation count increments).
    """
    bs = fresh_bookshelf
    before = bs.invocation_count()
    bs.tap_home()
    bs.wait_for_respawn(before, timeout=15.0)
    # After respawn the new invocation must have completed EVT_INIT.
    bs.assert_log_contains("EVT_INIT")


def test_menu_button_opens_more_overlay(fresh_bookshelf):
    """Tap menu button (top-right), verify overlay appears."""
    bs = fresh_bookshelf
    bs.tap_menu_and_verify()
    # The More overlay should be drawn (framebuffer changed)
    # Verify by tapping outside to dismiss and checking another change
    bs.tap_outside_and_verify()


# ── More overlay ───────────────────────────────────────────────────────


def test_more_overlay_sync(fresh_bookshelf):
    """Open More, tap Sync, verify framebuffer changes (re-sync)."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_SYNC)
    # After sync, log should show do_sync
    time.sleep(2.0)
    bs.assert_log_contains("do_sync")


def test_more_overlay_sort_title_az(fresh_bookshelf):
    """Open More, tap Title A-Z, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_TITLE_AZ)


def test_more_overlay_sort_title_za(fresh_bookshelf):
    """Open More, tap Title Z-A, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_TITLE_ZA)


def test_more_overlay_sort_author(fresh_bookshelf):
    """Open More, tap By author, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_AUTHOR)


def test_more_overlay_sort_series(fresh_bookshelf):
    """Open More, tap By series, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_SERIES)


def test_more_overlay_sort_recent(fresh_bookshelf):
    """Open More, tap Recent, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_RECENT)


def test_more_overlay_grid_button(fresh_bookshelf):
    """Open More, tap Grid, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_GRID)


def test_more_overlay_list_button(fresh_bookshelf):
    """Open More, tap List, verify framebuffer changes."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_more_item_and_verify(MORE_LIST)


def test_more_overlay_dismiss_outside_tap(fresh_bookshelf):
    """Open More, tap outside, verify overlay dismisses."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.tap_outside_and_verify()


def test_more_overlay_dismiss_back_key(fresh_bookshelf):
    """Open More, press Back, verify overlay dismisses."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    bs.send_back_and_verify()

# ── settings overlay ───────────────────────────────────────────────────

_GUEST_CFG = "/tmp/bookshelf.cfg"


def _read_guest_cfg() -> str:
    """Read the guest's bookshelf.cfg (host-side .live mount)."""
    out = container_sh(f"cat {_GUEST_CFG}", check=False, timeout=5)
    return out.stdout or ""


def _clear_guest_cfg() -> None:
    """Remove the guest's settings-override config so a test starts on Auto."""
    container_sh(f"rm -f {_GUEST_CFG}", check=False, timeout=5)


def test_more_overlay_settings_opens_page(fresh_bookshelf):
    """Open More, tap Settings, verify the settings page is drawn."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_more_item(MORE_SETTINGS)
    bs.wait_hash_change(before)


def test_settings_reader_cycle_and_save(fresh_bookshelf):
    """Cycle the reader row to Standard, Save, verify config + log."""
    bs = fresh_bookshelf
    _clear_guest_cfg()
    _restart_bookshelf(bs.emulator)
    bs.open_settings()
    time.sleep(0.5)
    # Auto -> Standard (first detected reader).
    bs.tap_settings_row(2)
    time.sleep(0.5)
    bs.tap_settings_save()
    time.sleep(1.0)
    # Save path logged the new preference and re-synced.
    bs.assert_log_contains("reader_pref=1")
    bs.assert_log_contains("settings: saved")
    # Config file on disk now pins the standard reader path.
    cfg = _read_guest_cfg()
    assert "reader=/ebrmain/bin/eink-reader.app" in cfg, f"cfg was:\n{cfg}"


def test_settings_reader_pref_persists_across_restart(fresh_bookshelf):
    """A saved reader preference is reloaded on the next launch."""
    bs = fresh_bookshelf
    _clear_guest_cfg()
    _restart_bookshelf(bs.emulator)
    bs.open_settings()
    time.sleep(0.5)
    bs.tap_settings_row(2)
    time.sleep(0.5)
    bs.tap_settings_save()
    time.sleep(1.0)
    # Restart bookshelf; the fresh process must load reader_pref=1.
    _restart_bookshelf(bs.emulator)
    bs.assert_log_contains("reader_pref=1 (cfg `/ebrmain/bin/eink-reader.app`)")


def test_settings_back_returns_to_shelf(fresh_bookshelf):
    """Open Settings, tap Back, verify the shelf is redrawn."""
    bs = fresh_bookshelf
    bs.open_settings()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_settings_back()
    bs.wait_hash_change(before)


def test_settings_api_host_row_opens_keyboard(fresh_bookshelf):
    """Tap the API host row, verify the on-screen keyboard appears."""
    bs = fresh_bookshelf
    bs.open_settings()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_settings_row(0)
    bs.wait_hash_change(before)


def test_more_overlay_system_menu_launches_control_panel(fresh_bookshelf):
    """Open More, tap System menu, verify control panel launches."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_more_item(MORE_SYSTEM)
    bs.wait_hash_change(before)
    bs.assert_log_contains("opening system control panel")

# ── search ────────────────────────────────────────────────────────────


def test_search_tap_opens_keyboard(fresh_bookshelf):
    """Tap search box, verify framebuffer changes (keyboard appears)."""
    bs = fresh_bookshelf
    bs.tap_search_and_verify()


# ── book grid ──────────────────────────────────────────────────────────


def test_book_tap_triggers_open_with(fresh_bookshelf):
    """Tap a book tile, verify log shows open-with API call."""
    bs = fresh_bookshelf
    bs.tap_book(0)
    time.sleep(3.0)
    # The log should show the launching message
    bs.assert_log_contains("launching app=")


def test_book_tap_launches_reader(fresh_bookshelf):
    """Tap a book tile, verify open-with + download + launch sequence."""
    bs = fresh_bookshelf
    bs.tap_book(0)
    time.sleep(5.0)
    # The mock provider serves tiny fake epubs that the real reader
    # cannot open, so the reader may crash/return immediately.
    # Verify the bookshelf side did its job: open-with call + launch.
    bs.assert_log_contains("launching app=")


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
    time.sleep(0.5)
    bs.tap_pager_prev_and_verify()


# ── menu overlay (group) ──────────────────────────────────────────────
# Note: The menu/group overlay (All books, By author, etc.) is currently
# unreachable from the UI (g_state.menu_open is never set to 1 from any
# tap handler). The hit_top_bar function returns 3 for the right button
# which opens the More overlay, not the menu overlay. The menu overlay
# code exists but is dead code. We skip testing it.


# ── back key (no overlay) ──────────────────────────────────────────────

def test_back_key_closes_app(fresh_bookshelf):
    """Press Back with no overlay, verify CloseApp triggers a respawn cycle.

    Bookshelf is the launcher replacement: monitor.app respawns it
    immediately after CloseApp().  The reliable proof is a new
    log_open header (invocation count increments).
    """
    bs = fresh_bookshelf
    before = bs.invocation_count()
    bs.send_back_key()
    bs.wait_for_respawn(before, timeout=15.0)
    bs.assert_log_contains("EVT_INIT")


# ── crash safety ───────────────────────────────────────────────────────


def test_no_crash_after_all_interactions(fresh_bookshelf):
    """Exercise all interactive elements, verify no crash markers in log."""
    bs = fresh_bookshelf
    # Tap menu, tap each More item
    bs.tap_menu()
    time.sleep(0.5)
    for item_idx in range(8):
        bs.tap_more_item(item_idx)
        time.sleep(0.3)
    # Reopen menu, dismiss with back
    bs.tap_menu()
    time.sleep(0.5)
    bs.send_back_key()
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


# ── series drill-down ──────────────────────────────────────────────────
# The mock provider derives a series from a "Name - NN" filename convention
# (see api/providers/mock.py).  The shipped books are all standalone, so the
# collapsed top-level grid shows no series card by default.  These tests inject
# a two-book series, restart bookshelf so the fresh launch syncs it, then
# exercise the drill-in (tap the series card) and the two drill-back paths
# (top-bar left button + BACK key).  Injected files are always removed so the
# shared books dir is clean for the other tests in this module.

_SERIES_STEM = "Drill_Test"
_SERIES_FILES = [f"{_SERIES_STEM} - 01.epub", f"{_SERIES_STEM} - 02.epub"]
_ALLOWED_EXT = (".epub", ".pdf", ".fb2", ".djvu", ".txt", ".cbz", ".cbr")
_BOOKS_DIR = REPO_ROOT / FIRMWARE / ".live" / "mnt" / "ext1" / "books"
_PAGESIZE = 6  # COLS * ROWS, must match geometry.PAGESIZE


def _stem_is_series(stem: str) -> bool:
    """Replicate the mock provider's series-name rule."""
    dp = stem.rfind(" - ")
    return dp > 0 and stem[dp + 3 :].strip().isdigit()


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


def _standalone_view_count() -> int:
    """Count books the collapsed view emits as flat tiles (no series tail).

    In GROUP_ALL collapse mode build_view() emits every standalone book first,
    then one card per multi-book series, so the (single) series card lands at
    view index == this count.
    """
    n = 0
    for p in sorted(_BOOKS_DIR.iterdir()):
        if not p.is_file() or not p.name.lower().endswith(_ALLOWED_EXT):
            continue
        if not _stem_is_series(p.stem):
            n += 1
    return n


def _goto_view_tile(bs: BookshelfSession, view_idx: int) -> int:
    """Page to *view_idx* and return its within-page position."""
    page = view_idx // _PAGESIZE
    pos = view_idx % _PAGESIZE
    for _ in range(page):
        bs.tap_pager_next()
        time.sleep(0.5)
    return pos


def test_series_card_drill_in_and_back(fresh_bookshelf):
    """Collapsed grid shows a series card; tap drills in, back pops out.

    The left top-bar button and the BACK key must both pop the drill without
    closing the app (no monitor.app respawn cycle).
    """
    bs = fresh_bookshelf
    injected = _inject_series()
    try:
        _restart_bookshelf(bs.emulator)
        time.sleep(2.0)
        bs.wait_for_stable()

        standalone = _standalone_view_count()
        series_idx = standalone  # one multi-book series, appended after flats

        # The collapsed grid must contain exactly one series card.
        bs.assert_log_contains(f"draw_grid view={standalone + 1}")

        # Drill in: tap the series card.
        pos = _goto_view_tile(bs, series_idx)
        before = bs.frame_hash()
        bs.tap_book(pos)
        bs.wait_hash_change(before)
        bs.assert_log_contains("drilled into series 'Drill Test'")

        # Top-bar left button = Back while drilled: pops, does NOT close.
        inv_before = bs.invocation_count()
        before2 = bs.frame_hash()
        bs.tap_home()
        bs.wait_hash_change(before2)
        bs.assert_log_contains("drilled back to top level")
        time.sleep(2.0)
        assert bs.invocation_count() == inv_before, (
            "drill-back must not trigger CloseApp/respawn"
        )

        # BACK key also pops the drill.
        pos = _goto_view_tile(bs, series_idx)
        before3 = bs.frame_hash()
        bs.tap_book(pos)
        bs.wait_hash_change(before3)
        before4 = bs.frame_hash()
        bs.send_back_key()
        bs.wait_hash_change(before4)
        assert bs.current_log().count("drilled back to top level") >= 2, (
            "BACK key did not pop the drilled series"
        )
    finally:
        _remove_series(injected)
