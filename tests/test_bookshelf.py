"""E2E tests for every interactive element in the bookshelf app.

Requires:
  - podman available
  - firmware U633_6.8.2817 staged (./pbemu install)
  - books in U633_6.8.2817/.live/mnt/ext1/books/

Run with: pytest tests/test_bookshelf.py -v
"""

from __future__ import annotations

import json
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
    MORE_SETTINGS,
    MORE_APPS,
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
            "--config",
            str(REPO_ROOT / "tests" / "support" / "server-test.json"),
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
    bs_dir = REPO_ROOT / "bookshelf"
    out = REPO_ROOT / "build" / "bookshelf.app"
    build_script = REPO_ROOT / "sdk" / "build_armel.sh"
    assert build_script.is_file(), f"build script missing: {build_script}"
    srcs = [
        str(bs_dir / f)
        for f in [
            "bs_i18n.c", "bs_config.c", "bs_model.c", "bs_net.c",
            "bs_ui.c", "bs_input.c", "bs_launcher.c",
            "bs_downloads.c", "bs_store.c", "bs_main.c",
        ]
    ]
    for s in srcs:
        assert Path(s).is_file(), f"source missing: {s}"
    subprocess.run(
        [str(build_script), *srcs, "--output", str(out.relative_to(REPO_ROOT))],
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

    # Start with --network=host.  The U633 is a colour device: advertise
    # the 24-bit framebuffer so the guest's RGB24 cover decodes render
    # (and the app's device_display_colormask() path is exercised).
    env = _pbemu_env()
    env["PBEMU_NO_KEEPID"] = "1"
    env["PBEMU_PODMAN_ARGS"] = "--network=host"
    env["SHIM_PBEMU_COLOR_FB"] = "1"
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
    time.sleep(1.5)
    assert bs.invocation_count() == before, "home on home must not respawn the app"
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




# ── launcher (app grid) ───────────────────────────────────────────────


def test_more_overlay_apps_opens_launcher(fresh_bookshelf):
    """Open More, tap Applications, verify the launcher overlay draws."""
    bs = fresh_bookshelf
    bs.tap_menu()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_more_item(MORE_APPS)
    bs.wait_hash_change(before)
    bs.assert_log_contains("launcher built")


def test_launcher_back_returns_to_shelf(fresh_bookshelf):
    """Open launcher, tap Back, verify the shelf is redrawn."""
    bs = fresh_bookshelf
    bs.open_launcher()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.tap_launcher_back()
    bs.wait_hash_change(before)


def test_launcher_back_key_returns_to_shelf(fresh_bookshelf):
    """Open launcher, press Back key, verify the shelf is redrawn."""
    bs = fresh_bookshelf
    bs.open_launcher()
    time.sleep(0.5)
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
    bs.tap_launcher_app(0)
    bs.wait_hash_change(before)
    bs.assert_log_contains("launching app path=")


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
    bs.open_launcher()
    time.sleep(0.5)
    before = bs.frame_hash()
    bs.scroll_launcher_down()
    bs.wait_hash_change(before)


def test_search_commit_filters_grid(fresh_bookshelf):
    """Typing a query on the Search page and committing it must actually
    filter the shelf.

    Regression for the "search never searches" bug: OpenKeyboard() wrote
    the live keystrokes straight into g_state.query, and on commit the
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
    bs.type_text("alpha", commit=True)
    bs.wait_hash_change(kb)
    # The committed query reached the filter (pre-fix this was empty).
    bs.assert_log_contains("query=`alpha`")


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
    bs.tap_history_term(0)
    time.sleep(0.5)
    bs.assert_log_contains("search history tap: query=`alpha`")


# ── book grid ──────────────────────────────────────────────────────────


def test_book_tap_triggers_open_with(fresh_bookshelf):
    """Tap a book tile, verify it downloads (if needed) then launches."""
    bs = fresh_bookshelf
    _clear_downloads()
    bs.tap_book(0)
    time.sleep(3.0)
    # A book press resolves the reader and launches it (OpenBook path).
    bs.assert_log_contains("launching reader")
    _kill_guest_tasks()
    _clear_downloads()


def test_book_tap_launches_reader(fresh_bookshelf):
    """Tap a book tile, verify download + launch sequence end to end."""
    bs = fresh_bookshelf
    _clear_downloads()
    bs.tap_book(0)
    time.sleep(5.0)
    # The mock provider serves tiny fake epubs that the real reader
    # cannot open, so the reader may crash/return immediately.
    # Verify the bookshelf side did its job: download + launch.
    bs.assert_log_contains("download_book_file OK")
    bs.assert_log_contains("launching reader")
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
    time.sleep(0.5)
    bs.tap_pager_prev_and_verify()


# ── menu overlay (group) ──────────────────────────────────────────────
# Note: The menu/group overlay (All books, By author, etc.) is currently
# unreachable from the UI (g_state.menu_open is never set to 1 from any
# tap handler). The hit_top_bar function returns 3 for the right button
# which opens the More overlay, not the menu overlay. The menu overlay
# code exists but is dead code. We skip testing it.


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
    time.sleep(1.5)
    assert bs.invocation_count() == before, "back on home must not respawn the app"
    bs.assert_no_crash()


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


# ── tabs, downloads, context menus ─────────────────────────────────────
# The Downloads tab, Download-all action, and the long-press context menus
# (book: download/delete; series: download-all/delete) are exercised here.
# In the emulator the guest runs non-root and cannot write /mnt/ext1/system/bin,
# so bookshelf.c falls back to /tmp (resolve_downloads_dir); guest /tmp maps to
# .live/tmp on the host.  The helpers below inspect/clean that dir.

_DOWNLOADS_DIR = REPO_ROOT / FIRMWARE / ".live" / "tmp"


def _downloaded_files() -> list[Path]:
    """Book files the app has downloaded into LOCAL_DOWNLOADS."""
    if not _DOWNLOADS_DIR.is_dir():
        return []
    return [p for p in _DOWNLOADS_DIR.iterdir() if p.suffix.lower() in _ALLOWED_EXT]


def _clear_downloads() -> None:
    for p in _downloaded_files():
        try:
            p.unlink()
        except OSError:
            pass


def _wait_log_count(bs: BookshelfSession, needle: str, count: int, *, timeout: float = 20.0) -> None:
    """Poll until *needle* appears at least *count* times in the current
    invocation's log (downloads drain one-per-timer-tick)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if bs.current_log().count(needle) >= count:
            return
        time.sleep(0.5)
    got = bs.current_log().count(needle)
    raise AssertionError(f"log contains {needle!r} {got}×, expected >= {count}")


def _wait_log_slice(bs: BookshelfSession, before: str, needle: str, *, timeout: float = 8.0) -> None:
    """Poll until *needle* appears in the log text appended after the
    *before* snapshot.  Used to confirm a tap produced a specific redraw
    line without being fooled by unrelated background redraws."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if needle in bs.current_log()[len(before):]:
            return
        time.sleep(0.1)
    raise AssertionError(
        f"log slice after offset {len(before)} never contained {needle!r}"
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
_DL_DELAY_CFG = REPO_ROOT / FIRMWARE / ".live" / "tmp" / "bookshelf.cfg"


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
            str(REPO_ROOT / "tests" / "support" / "server-test.json"),
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
        time.sleep(2.0)
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

        # The sync glyph spins while the fetch runs: the frame must
        # change well before the 3 s fetch completes, proving the event
        # loop is alive (the old code froze it for the whole transfer).
        bs.wait_hash_change(before, timeout=2.0)

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
    time.sleep(2.0)
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
    time.sleep(2.0)
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
        time.sleep(2.0)
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
    time.sleep(2.0)
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
    time.sleep(2.0)
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
    time.sleep(2.0)
    # First download the book so there is something to delete.
    bs.long_press_book(0)
    time.sleep(1.0)
    bs.tap_context_item(1)  # Download (0 is Open)
    _wait_log_count(bs, "download_book_file OK", 1)
    assert len(_downloaded_files()) >= 1, "setup download failed"
    # Dismiss the popup (the download kept it open), then delete via the
    # context menu.
    bs.tap_at(*bs.geom.book_tile_center(0))
    time.sleep(0.5)
    bs.long_press_book(0)
    time.sleep(1.0)
    bs.tap_context_item(2)  # Delete
    time.sleep(2.0)
    bs.assert_log_contains("delete_book_file removed")
    assert len(_downloaded_files()) == 0, "delete did not remove the file"


def test_series_longpress_download_all(fresh_bookshelf):
    """Long-press a series card → Download all fetches every member."""
    bs = fresh_bookshelf
    injected = _inject_series()
    try:
        _clear_downloads()
        _restart_bookshelf(bs.emulator)
        time.sleep(2.0)
        bs.wait_for_stable()
        standalone = _standalone_view_count()
        series_idx = standalone
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
        time.sleep(2.0)
        bs.wait_for_stable()
        standalone = _standalone_view_count()
        series_idx = standalone
        pos = _goto_view_tile(bs, series_idx)
        # Download the series first so delete has files to remove.
        bs.long_press_book(pos)
        time.sleep(1.0)
        bs.tap_context_item(0, n_items=2)  # Download all
        _wait_log_count(bs, "download_book_file OK", 2)
        removed_before = bs.current_log().count("delete_book_file removed")
        # Dismiss the popup (the download kept it open), then delete the
        # whole series.
        bs.tap_at(*bs.geom.book_tile_center(pos))
        time.sleep(0.5)
        bs.long_press_book(pos)
        time.sleep(1.0)
        bs.tap_context_item(1, n_items=2)  # Delete series
        time.sleep(2.0)
        bs.assert_log_contains("delete_series")
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
# which is also where the library store + cover cache live.

_OFFLINE_TMP = REPO_ROOT / FIRMWARE / ".live" / "tmp"
_OFFLINE_STORE = _OFFLINE_TMP / "bookshelf_lib.db"
_OFFLINE_LEGACY = _OFFLINE_TMP / "bookshelf_lib.json"
_OFFLINE_COVERS = _OFFLINE_TMP / "covers"
_OFFLINE_CFG = _OFFLINE_TMP / "bookshelf.cfg"


def _ensure_offline_assets(emulator: Emulator) -> None:
    """Wait for (or force) a populated library store + >=6 cached covers."""
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        covers = len(list(_OFFLINE_COVERS.glob("*.png")))
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
    m = None
    for m in re.finditer(r"draw_grid view=(\d+) page=(\d+)", log):
        pass
    assert m, "no draw_grid line in log"
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


def _wait_store_series(series_title: str) -> list[dict]:
    """Wait until the on-disk store holds >=2 books of *series_title*."""
    deadline = time.monotonic() + 20
    store: list[dict] = []
    while time.monotonic() < deadline:
        store = _store_books()
        if sum(1 for b in store if b.get("series") == series_title) >= 2:
            break
        time.sleep(1.0)
    books = [b for b in store if b.get("series") == series_title]
    assert len(books) >= 2, "online sync never stored the series"
    return books


def _wait_cover_file(member_id: str) -> None:
    """Wait for the online cover cache to gain *member_id*.png."""
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if (_OFFLINE_COVERS / f"{member_id}.png").is_file():
            return
        time.sleep(0.5)
    raise AssertionError(f"cover cache never written for series member {member_id}")


def _wait_cover_log(bs, member_id: str) -> None:
    """Wait for a cache-hit blit of *member_id* in the guest log."""
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if f"cover_tick cache hit id={member_id}" in bs.current_log():
            return
        time.sleep(0.5)
    raise AssertionError("series card did not render its cached thumbnail offline")


def _loaded_book_count(log: str) -> int:
    """Tiles the offline boot projected from the on-disk store."""
    m = re.search(r"view_rebuild: view=(\d+)", log)
    assert m, "offline boot did not rebuild the view from the store"
    return int(m.group(1))


def _pager_roundtrip(bs, pages: int) -> None:
    """<< / >> jump to the ends, < / > step one page at a time."""
    targets = (
        ("page=0", bs.tap_pager_first),
        (f"page={pages - 1}", bs.tap_pager_last),
        ("page=0", bs.tap_pager_first),
        ("page=1", bs.tap_pager_next),
    )
    for want, tap in targets:
        snap = bs.current_log()
        before = bs.frame_hash()
        tap()
        bs.wait_hash_change(before)
        _wait_log_slice(bs, snap, want)


def _offline_drill_back(bs, pos, page_before: int) -> None:
    """Drill into the offline series card; back must restore the page."""
    before = bs.frame_hash()
    bs.tap_book(pos)
    bs.wait_hash_change(before)
    bs.assert_log_contains("drilled into series 'Drill Test'")
    snap = bs.current_log()
    before = bs.frame_hash()
    bs.tap_home()
    bs.wait_hash_change(before)
    _wait_log_slice(bs, snap, "drilled back to top level")
    assert _last_draw_grid(bs.current_log())[1] == page_before, (
        "offline drill-back did not restore the previous page"
    )


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
def _seed_online_series(bs, emulator) -> str:
    """Seed a two-book series online so the store carries a collapsed
    series card and cover_tick caches its member cover; return the id."""
    injected = _inject_series()
    try:
        _restart_bookshelf(emulator)
        time.sleep(2.0)
        bs.wait_for_stable()
        _ensure_offline_assets(emulator)
        series_books = _wait_store_series(_SERIES_STEM.replace("_", " "))
        member_id = max(
            series_books, key=lambda b: (b.get("seriesIdx") or 0, b.get("addedAt") or 0)
        )["id"]
        _goto_view_tile(bs, _standalone_view_count())
        _wait_cover_file(member_id)
    finally:
        _remove_series(injected)
    return member_id


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
        time.sleep(2.0)
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
    on-disk library store + cover cache and stays fully navigable — series
    cards keep their cached thumbnail, drill-in/back restores the page, the
    pager jumps first/last, and the downloads view opens via the sync icon
    and closes via its back arrow."""
    bs, emulator = bookshelf_env
    member_id = _seed_online_series(bs, emulator)

    saved_cfg = _set_dead_cfg()
    try:
        _restart_bookshelf(emulator)
        _offline_boot_asserts(_wait_offline_log(bs))
        invocations = bs.invocation_count()

        # Page to the series card offline: its member thumbnail must blit
        # from the on-disk cache (there is no network to fall back to).
        pos = _goto_view_tile(bs, _standalone_view_count())
        page_before = _last_draw_grid(bs.current_log())[1]
        _wait_cover_log(bs, member_id)
        _offline_drill_back(bs, pos, page_before)
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
    (_OFFLINE_TMP / "bookshelf_lib.db.migrated").unlink(missing_ok=True)
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
        "chown \"$(stat -c %u:%g /tmp/covers 2>/dev/null || "
        "stat -c %u:%g /tmp/bookshelf.log)\" /tmp/bookshelf_lib.json",
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
        assert (_OFFLINE_TMP / "bookshelf_lib.json.migrated").exists()
    finally:
        _restore_cfg(saved_cfg)
