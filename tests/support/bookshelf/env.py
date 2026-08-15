"""Shared environment builders for the bookshelf e2e suite.

Everything both test modules (test_bookshelf.py and
test_bookshelf_scale.py) need to stand up the environment: the repo /
firmware roots, the mock API server lifecycle, binary build + staging
into .live and the container, the emulator lifecycle, and the guest
task (bookshelf/reader) restart helpers.  Kept out of the test modules
so the scale suite does not have to import the (large) main test module.
"""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

from tests.support.bookshelf.session import (
    count_log_openings,
    latest_invocation_log,
    read_bookshelf_log,
)
from tests.support.reader.session import Session
from tests.support.runtime import Emulator, container_running, container_sh
from tests.support.runtime_common import REPO_ROOT

# The pbemu submodule provides the firmware tree, the emulator tooling
# (tools/ + api/), the container and the test support framework
# (tests/support).  This repository provides the app, the Makefile and
# the test files; EINKHOME_ROOT points back here from the submodule.
EINKHOME_ROOT = Path(__file__).resolve().parents[3]
PBEMU_ROOT = REPO_ROOT

# Staged firmware directory the suite boots (pbemu/<name>).  Override via
# PB_TEST_FIRMWARE — the same env var pbemu's own harness honours — so CI
# can run against a firmware other than the dev default.
FIRMWARE = os.environ.get("PB_TEST_FIRMWARE") or "U633_6.8.2817"
# Container runtime CLI.  Set PODMAN=docker to run the suite on docker.
PODMAN = os.environ.get("PODMAN") or "podman"
# Parallel runs (visual captures on multiple firmwares at once) use
# distinct container names + API ports per worker; pbemu honours the
# same PB_SYSTEM_CONTAINER override.
CONTAINER = os.environ.get("PB_SYSTEM_CONTAINER") or "pb-pocketbook-ui"
API_PORT = int(os.environ.get("PBEMU_TEST_API_PORT") or 18765)
API_TOKEN = "pbemu-dev-token"
BOOKSHELF_APP = "bookshelf.app"

# The guest resolves its config to /mnt/ext1/system/bin (writable since
# the staging commits made /mnt guest-writable), so the store, cover
# cache and legacy JSON live next to that config; /tmp/bookshelf.cfg is
# only a kv-override the loader re-applies on top.
_OFFLINE_TMP = PBEMU_ROOT / FIRMWARE / ".live" / "tmp"
_OFFLINE_DIR = PBEMU_ROOT / FIRMWARE / ".live" / "mnt" / "ext1" / "system" / "bin"
_OFFLINE_STORE = _OFFLINE_DIR / "bookshelf_lib.db"
_OFFLINE_LEGACY = _OFFLINE_DIR / "bookshelf_lib.json"
_OFFLINE_COVERS = _OFFLINE_DIR / "covers"
_OFFLINE_CFG = _OFFLINE_TMP / "bookshelf.cfg"


def _pbemu_env() -> dict[str, str]:
    """Return env dict with tools/ prepended to PYTHONPATH."""
    env = os.environ.copy()
    tools = str(PBEMU_ROOT / "tools")
    env["PYTHONPATH"] = (
        tools if not env.get("PYTHONPATH") else f"{tools}{os.pathsep}{env['PYTHONPATH']}"
    )
    return env


def _api_env() -> dict[str, str]:
    """Return env dict for the API server subprocess."""
    env = os.environ.copy()
    api_dir = str(EINKHOME_ROOT / "api")
    root = str(EINKHOME_ROOT)
    extra = f"{root}{os.pathsep}{api_dir}"
    env["PYTHONPATH"] = (
        extra if not env.get("PYTHONPATH") else f"{extra}{os.pathsep}{env['PYTHONPATH']}"
    )
    return env


def _start_api_server(
    port: int | None = None,
    log_path: Path | str | None = None,
) -> subprocess.Popen:  # type: ignore[type-arg]
    """Start the mock API server on the test port. Returns the Popen.

    *port* defaults to the module-wide API_PORT.  Parallel backends
    (SDL/xdist workers) pass a per-process port so concurrent servers
    don't fight over one listener, and *log_path* so each server's log
    (dumped on failure) isn't clobbered by the next worker.
    """
    port = API_PORT if port is None else port
    log_path = Path(
        log_path if log_path is not None
        else (EINKHOME_ROOT / "build" / "pbemu-api-test.log")
    )
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
            str(port),
            "--provider",
            "mock",
            "--config",
            str(EINKHOME_ROOT / "tests" / "support" / "server-test.json"),
        ],
        # The server code lives in this repo (api/ on PYTHONPATH), but it
        # runs with the submodule as cwd so the config's firmware-relative
        # paths (books_dir: U633_6.8.2817/.live/...) resolve correctly.
        cwd=PBEMU_ROOT,
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
                f"http://127.0.0.1:{port}/api/v1/healthz",
                headers={"Authorization": f"Bearer {API_TOKEN}"},
            )
            with urllib.request.urlopen(req, timeout=2) as resp:
                body = json.loads(resp.read().decode("utf-8"))
        except Exception:
            time.sleep(0.3)
            continue
        # The reply must come from the process we just spawned: a server
        # left over from an earlier (interrupted) run may still be
        # answering on the test port while our fresh process never bound
        # it (EADDRINUSE).  Testing against that stale server would run
        # the suite against dead code, so kill it and fail loudly.
        if body.get("pid") != proc.pid:
            stale_pid = body.get("pid")
            if isinstance(stale_pid, int) and stale_pid > 0:
                # The stale server reports its own pid via healthz;
                # terminate it so the next run gets a fresh listener.
                try:
                    os.kill(stale_pid, signal.SIGTERM)
                    time.sleep(0.3)
                    os.kill(stale_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except PermissionError:
                    pass
            proc.kill()  # our spawn lost the port race; reap it
            proc.wait()
            raise RuntimeError(
                f"stale API server (pid {stale_pid}) answered on port "
                f"{port} instead of the freshly spawned server "
                f"(pid {proc.pid}); killed the stale listener. Log:\n"
                f"{log_path.read_text()}"
            )
        return proc
    proc.kill()
    proc.wait()
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
    out = EINKHOME_ROOT / "build" / "bookshelf.app"
    # PBEMU_APP_BINARY overrides the binary without rebuilding — used for
    # armhf devices (InkPad One) whose ABI differs from the shared armel
    # build.
    override = os.environ.get("PBEMU_APP_BINARY")
    if override:
        path = Path(override)
        assert path.is_file(), f"PBEMU_APP_BINARY not found: {path}"
        return path
    # The source list lives in bookshelf/Makefile; build_armel.sh does
    # the cross-compile.
    try:
        subprocess.run(
            ["make", "-C", str(EINKHOME_ROOT)],
            cwd=EINKHOME_ROOT,
            check=True,
            capture_output=True,
            text=True,
            timeout=120,
        )
    except subprocess.CalledProcessError as exc:
        # check=True + capture_output swallow the compiler output, so a
        # failing build looks like a bare exit-code error; surface the
        # captured stderr (the same pattern _start_api_server uses to
        # dump its log) before re-raising.
        sys.stderr.write(exc.stderr or "")
        raise
    assert out.is_file(), f"build output missing: {out}"
    return out


def _newest_src_mtime() -> float:
    """Newest mtime across the app sources + build scripts (for the SDL
    test binary freshness check)."""
    newest = 0.0
    for base in (
        EINKHOME_ROOT / "app",
        EINKHOME_ROOT / "sdk",
        EINKHOME_ROOT / "Makefile",
    ):
        if base.is_file():
            newest = max(newest, base.stat().st_mtime)
        elif base.is_dir():
            for p in base.rglob("*"):
                if p.is_file():
                    newest = max(newest, p.stat().st_mtime)
    return newest


_SDL_TEST_OUT = EINKHOME_ROOT / "build" / "bookshelf.test"
_SDL_TEST_LOCK = EINKHOME_ROOT / "build" / ".sdl-bookshelf.test.lock"


def _ensure_sdl_test_binary() -> Path:
    """Build (once) the SDL test binary with the IPC control socket.

    Every parallel worker needs the same binary; the build is guarded by
    an fcntl lock so concurrent first-runs compile exactly once, and an
    mtime check skips rebuilding when build/bookshelf.test is already
    fresher than the app sources.  Returns the binary path.
    """
    with contextlib.suppress(OSError):
        if _SDL_TEST_OUT.is_file() and (
            _SDL_TEST_OUT.stat().st_mtime >= _newest_src_mtime()
        ):
            return _SDL_TEST_OUT
    _SDL_TEST_OUT.parent.mkdir(parents=True, exist_ok=True)
    lock_fh = open(_SDL_TEST_LOCK, "w", encoding="utf-8")  # noqa: SIM115
    fcntl.flock(lock_fh, fcntl.LOCK_EX)
    try:
        # Re-check under the lock: another worker may have built it while
        # we waited.
        if _SDL_TEST_OUT.is_file() and (
            _SDL_TEST_OUT.stat().st_mtime >= _newest_src_mtime()
        ):
            return _SDL_TEST_OUT
        env = os.environ.copy()
        env["BS_ENABLE_TEST_IPC"] = "1"
        proc = subprocess.run(
            ["bash", "sdk/build_pc.sh", "--output", "build/bookshelf.test"],
            cwd=EINKHOME_ROOT,
            check=False,
            capture_output=True,
            text=True,
            env=env,
        )
        if proc.returncode != 0:
            sys.stderr.write(proc.stderr or "")
            proc.check_returncode()
    finally:
        fcntl.flock(lock_fh, fcntl.LOCK_UN)
        lock_fh.close()
    return _SDL_TEST_OUT


def _snapshot_cfg(path: Path) -> str | None:
    """Return a cfg file's current text, or None when absent."""
    return path.read_text(encoding="utf-8") if path.is_file() else None


def _restore_cfg_file(path: Path, saved: str | None, mode: int = 0o644) -> None:
    """Restore a cfg file to its pre-test state (remove if absent)."""
    path.unlink(missing_ok=True)
    if saved is not None:
        path.write_text(saved, encoding="utf-8")
        path.chmod(mode)


def _stage_binary(binary: Path) -> None:
    """Stage the bookshelf binary + config into .live and container.

    Uses ``podman cp`` to copy the binary from host into the container,
    then ``podman exec`` with container-side paths to place it where
    monitor.app will find it (ebrmain/bin takes priority over
    /mnt/ext1/system/bin).

    The host-side .live staging is always lenient (it is the fallback
    when no container exists), but once the container IS running any
    failed container-side copy raises: a silently failed ``podman cp``
    would otherwise leave the previous binary in place and the guest
    would keep running stale code.
    """
    live = PBEMU_ROOT / FIRMWARE / ".live"
    bin_dir = live / "mnt/ext1/system/bin"
    bin_dir.mkdir(parents=True, exist_ok=True)

    # The app is linked with RUNPATH=/mnt/ext1/system/bin, so it picks
    # up these SDK copies instead of the firmware's libinkview/libhwconfig
    # — older firmwares ship libs with different touch/event behavior
    # (taps land at degenerate coordinates).  The SDK libinkview needs
    # legacy deps (libssl 1.0, libpng12, libicu58); only firmwares that
    # carry them can run it — the newer ones keep their own libs, whose
    # touch handling already matches the harness.  A real device has no
    # such files here and falls back to its own firmware libs.
    sdk_lib = EINKHOME_ROOT / "sdk" / "pocketbook-sdk-b288" / "lib"
    fw_lib = live / "ebrmain" / "lib"
    if (fw_lib / "libssl.so.1.0.0").exists():
        for so in ("libinkview.so", "libhwconfig.so"):
            shutil.copy2(sdk_lib / so, bin_dir / so)

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
            [PODMAN, "cp", str(binary), f"{CONTAINER}:/tmp/bookshelf.app.new"],
            check=True,
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
            check=True,
            timeout=10,
        )
        # 3. Copy config into container
        subprocess.run(
            [
                PODMAN,
                "cp",
                str(bin_dir / "bookshelf.cfg"),
                f"{CONTAINER}:/mnt/ext1/system/bin/bookshelf.cfg",
            ],
            check=True,
            capture_output=True,
            timeout=5,
        )


def _start_emulator() -> Emulator:
    """Stop any existing emulator and start a fresh one with --network=host.

    The guest boot is racy on hosted runners — the monitor can crash or
    hang at a random init step (observed: shmget EINVAL on hosts with a
    low kernel.shmmax, and hangs right after the safebox setup).  Each
    attempt is a full stop/start cycle; give up only after several
    tries so a flaky boot does not fail the whole suite.
    """
    last_exc: Exception | None = None
    for _attempt in range(5):
        try:
            return _start_emulator_once()
        except Exception as exc:  # noqa: BLE001
            last_exc = exc
            subprocess.run(
                [sys.executable, "-m", "pbemu", "stop"],
                cwd=REPO_ROOT,
                env=_pbemu_env(),
                check=False,
            )
            time.sleep(2.0)
    raise RuntimeError(f"emulator did not boot after 5 attempts: {last_exc}")


def _start_emulator_once() -> Emulator:
    """One emulator start attempt: stop, start, wait for monitor+hwevent."""
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
    # Hosted runners may lack /sys/class entries the emulator binds
    # (e.g. leds on cloud kernels), which crun cannot create inside the
    # read-only sysfs of a rootless container.  An in-container tmpfs
    # over /sys gives crun writable mount targets.  Off by default so
    # dev machines keep the real sysfs; CI sets PBEMU_SYS_TMPFS=1.
    if os.environ.get("PBEMU_SYS_TMPFS") == "1":
        env["PBEMU_PODMAN_ARGS"] += " --tmpfs /sys:rw,nodev,nosuid,mode=755,size=2m"
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
    """Parse panel_h from the bookshelf log (fallback: 0)."""
    geom = _parse_app_geometry(firmware)
    return geom[2] if geom is not None else 0


def _parse_app_geometry(firmware: str) -> tuple[int, int, int] | None:
    """Parse (sw, sh, panel_h) from the app's EVT_INIT line.

    The informer reports the emulator framebuffer, which can be rotated
    relative to the app's logical screen on portrait devices (e.g. the
    Basic Lux 3 framebuffer is 1024x758 while the app runs 758x1024);
    tap coordinates must live in the app's space.
    """
    log = read_bookshelf_log(firmware)
    m = re.search(r"EVT_INIT panel_h=(\d+) sw=(\d+) sh=(\d+)", log)
    if m:
        return int(m.group(2)), int(m.group(3)), int(m.group(1))
    m = re.search(r"EVT_INIT panel_h=(\d+)", log)
    if m:
        return 0, 0, int(m.group(1))
    return None


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
        monitor.app|informer|scanner.app|usage_stat.app|taskmgr.app|digital_frame.app|\
        calendar.app|settings.app|eink-cache-reader.app)
            continue ;;
    esac
    # Bookshelf, the reader, control panel, or a stray app the previous
    # test launched (launcher tests leave their app foregrounded, which
    # would otherwise keep the informer's active task off bookshelf).
    pid=""
    [ -r "$d/mainpid" ] && pid=$(cat "$d/mainpid" 2>/dev/null)
    if [ -n "$pid" ]; then
        kill -TERM "$pid" 2>/dev/null && pids="$pids $pid"
    fi
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
    # Hosted runners can take a while for the informer to re-register
    # the fresh task, so allow as long as the respawn wait itself.
    _wait_bookshelf_active(emulator, timeout=timeout)
    time.sleep(1.0)
