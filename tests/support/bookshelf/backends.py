"""Backend abstraction for the bookshelf e2e suite.

The suite's interactive tests drive a *bookshelf backend* — the same
app logic, but the render/input/log plumbing differs per target:

  EmulatorBackend   ARM binary under pbemu's qemu (the classic target)
  SdlBackend        x86_64 SDL binary, headless, driven over the IPC
                    control socket (fast, parallel-safe)

`BookshelfSession` talks to a Backend, so the interactive tests run
unchanged against either target.  Backends also expose the data locations
the tests inspect (books dir, downloads, store, covers, config) — on the
emulator those are `.live` host paths; on SDL they are local build dirs.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Protocol

from tests.support.runtime_common import REPO_ROOT  # noqa: F401  (re-exported)

if TYPE_CHECKING:
    pass

# IV key codes — PB/inkview values, backend-independent
IV_KEY_BACK = 0x1B
IV_KEY_MENU = 0x17
IV_KEY_HOME = 0x1A
IV_KEY_PREV = 0x18
IV_KEY_NEXT = 0x19


class Backend(Protocol):
    """What BookshelfSession needs from a bookshelf runtime."""

    # -- input ----------------------------------------------------------
    def tap(self, x: int, y: int) -> None: ...
    def down(self, x: int, y: int) -> None: ...
    def move(self, x: int, y: int) -> None: ...
    def up(self, x: int, y: int) -> None: ...
    def key(self, iv_key: int, *, repeat: bool = False) -> None: ...
    def type_text(self, text: str) -> None: ...

    # -- frame ----------------------------------------------------------
    def frame_hash(self) -> str: ...
    def wait_frame_change(self, before: str, *, timeout: float = 8.0) -> str: ...
    def frame_ppm(self, name: str) -> bytes: ...

    # -- log ------------------------------------------------------------
    def log(self) -> str: ...
    def current_log(self) -> str: ...
    def invocation_count(self) -> int: ...

    # -- lifecycle ------------------------------------------------------
    def restart(self, *, wait_init: bool = True) -> None: ...
    def kill_all(self) -> None: ...

    # -- data locations (what tests inspect / seed) ---------------------
    @property
    def books_dir(self) -> Path: ...
    @property
    def downloads_dir(self) -> Path: ...
    @property
    def config_path(self) -> Path: ...
    @property
    def store_path(self) -> Path: ...
    @property
    def covers_dir(self) -> Path: ...
    @property
    def tmp_dir(self) -> Path: ...
    @property
    def screen_size(self) -> tuple[int, int]: ...
    @property
    def panel_h(self) -> int: ...


@dataclass
class BookSeed:
    """A fake book file the mock provider serves / tests inject."""
    stem: str
    suffix: str = ".epub"

    @property
    def filename(self) -> str:
        return f"{self.stem}{self.suffix}"


def wait_for(
    pred, *, timeout: float = 10.0, interval: float = 0.15, label: str = "condition"
):
    """Poll *pred* (callable -> truthy) until it holds; else TimeoutError."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if pred():
            return True
        time.sleep(interval)
    raise TimeoutError(f"timed out waiting for {label}")

# ── per-target log readers ─────────────────────────────────────────────

class _EmulatorLog:
    """The emulator guest appends to .live/tmp/bookshelf.log (or the
    argv0-derived bin dir).  Read whichever exists and is newest — the
    guest falls back to /tmp because its canonical dir is not writable."""

    def __init__(self, firmware: str) -> None:
        self.firmware = firmware
        self.base = REPO_ROOT / firmware / ".live"

    def _candidates(self) -> list[Path]:
        b = self.base
        return [
            b / "tmp" / "bookshelf.log",
            b / "mnt" / "ext1" / "system" / "bin" / "bookshelf.log",
        ]

    def path(self) -> Path:
        return _pick_newest(self._candidates())

    def read(self) -> str:
        p = self.path()
        return p.read_text(encoding="utf-8", errors="replace") if p.exists() else ""


class _ProcessLog:
    """The SDL/device app logs to a file we choose at launch
    (BS_LOG_FILE).  Read it directly; invocation slicing uses the
    log-open marker the app writes."""

    def __init__(self, path: str) -> None:
        self.path = Path(path)

    def read(self) -> str:
        return (
            self.path.read_text(encoding="utf-8", errors="replace")
            if self.path.exists()
            else ""
        )


def _pick_newest(cands: list[Path]) -> Path:
    existing = [p for p in cands if p.exists()]
    if not existing:
        return cands[-1]
    if len(existing) == 1:
        return existing[0]
    existing.sort(key=lambda p: p.stat().st_mtime)
    return existing[-1]


# Session.py's backward-compat shims read the *emulator* live log.
_LIVE_LOG_READER = _EmulatorLog("U633_6.8.2817")


# ── EmulatorBackend ────────────────────────────────────────────────────

class EmulatorBackend:
    """The classic target: the ARM binary under pbemu's qemu.  Input is
    injected via the emulator (hwevent mqueue), the framebuffer is read
    via frame_dump, and the log is read from the .live host tree."""

    def __init__(self, emulator, firmware: str, *, session_cls=None,
                 ui_input=None) -> None:
        from tests.support.reader.session import Session as _Session

        self._emu = emulator
        self._firmware = firmware
        self._session = (session_cls or _Session)(emulator)
        self._ui = ui_input  # pbemu's ui_input module (tap/down/up/key/type)
        self._log = _EmulatorLog(firmware)

    # input
    def tap(self, x, y):
        self._ui.tap(self._emu, x, y) if self._ui else self._emu.tap(x, y)
    def down(self, x, y):
        self._ui.pointer_down(self._emu, x, y) if self._ui else None
    def move(self, x, y):
        self._ui.pointer_move(self._emu, x, y) if self._ui else None
    def up(self, x, y):
        self._ui.pointer_up(self._emu, x, y) if self._ui else None
    def key(self, iv_key, *, repeat=False):
        if self._ui:
            self._ui.press_key(self._emu, iv_key)
    def type_text(self, text):
        if self._ui:
            self._ui.type_text(self._emu, text)

    # frame
    def frame_hash(self):
        return self._session.framebuffer_hash()
    def wait_frame_change(self, before, *, timeout=8.0):
        return self._session.wait_for_framebuffer_change(before, timeout=timeout)
    def frame_ppm(self, name):
        raw = REPO_ROOT / self._firmware / ".live" / "tmp" / f"{name}.ppm"
        # frame_dump --ppm writes the guest path; the file lands in .live
        self._emu.run_probe(
            "frame_dump", "--ppm", f"/workspace/firmware/.live/tmp/{name}.ppm",
            check=False)
        return raw.read_bytes() if raw.is_file() else b""

    # log
    def log(self):
        return self._log.read()
    def current_log(self):
        text = self._log.read()
        idx = text.rfind(_LOG_OPEN_MARKER)
        return text[idx:] if idx != -1 else text
    def invocation_count(self):
        return self._log.read().count(_LOG_OPEN_MARKER)

    # lifecycle / data dirs
    def restart(self, *, wait_init=True):
        from tests.support.bookshelf import env as _env
        _env._restart_bookshelf(self._emu) if hasattr(_env, "_restart_bookshelf") else None
    def kill_all(self):
        from tests.support.bookshelf import env as _env
        if hasattr(_env, "_kill_guest_tasks"):
            _env._kill_guest_tasks()

    @property
    def emulator(self):
        return self._emu
    @property
    def books_dir(self):
        return REPO_ROOT / self._firmware / ".live" / "mnt" / "ext1" / "books"
    @property
    def downloads_dir(self):
        return REPO_ROOT / self._firmware / ".live" / "mnt" / "ext1" / "Downloads"
    @property
    def config_path(self):
        return REPO_ROOT / self._firmware / ".live" / "mnt" / "ext1" / "system" / "bin" / "bookshelf.cfg"
    @property
    def store_path(self):
        return REPO_ROOT / self._firmware / ".live" / "mnt" / "ext1" / "system" / "bin" / "bookshelf_lib.db"
    @property
    def covers_dir(self):
        return REPO_ROOT / self._firmware / ".live" / "mnt" / "ext1" / "system" / "bin" / "covers"
    @property
    def tmp_dir(self):
        return REPO_ROOT / self._firmware / ".live" / "tmp"
    @property
    def screen_size(self):
        return 1072, 1448
    @property
    def panel_h(self):
        from tests.support.bookshelf import env as _env
        return _env._parse_panel_h(self._firmware) if hasattr(_env, "_parse_panel_h") else 0


_LOG_OPEN_MARKER = "--- bookshelf.app log opened"


# ── SdlBackend ─────────────────────────────────────────────────────────

class SdlBackend:
    """The native PC build (x86_64 SDL), run headless and driven over
    the IPC control socket.  Fast, no emulator, parallel-safe (each
    instance has its own socket + log)."""

    def __init__(self, sock_path: str, log_path: str, *,
                 api_url: str = "http://127.0.0.1:8765",
                 run_dir: str | Path | None = None,
                 relaunch=None) -> None:
        from tests.support.bookshelf.ipc_sdl import IpcBookshelf

        self._sock_path = sock_path
        self._log = _ProcessLog(log_path)
        self._api_url = api_url
        # Per-instance data dir.  Parallel workers point this at their own
        # build/bs-<pid> dir so config/covers/store never collide.
        self._run_dir = Path(run_dir) if run_dir is not None else REPO_ROOT / "build"
        self._relaunch = relaunch  # callable() -> Popen; owned by the fixture
        self._ipc: IpcBookshelf | None = None

    def _conn(self):
        if self._ipc is None:
            from tests.support.bookshelf.ipc_sdl import IpcBookshelf
            self._ipc = IpcBookshelf(self._sock_path)
        return self._ipc

    # input
    def tap(self, x, y):
        self._conn().tap_at(x, y)
    def down(self, x, y):
        self._conn().pointer_down(x, y)
    def move(self, x, y):
        self._conn().pointer_move(x, y)
    def up(self, x, y):
        self._conn().pointer_up(x, y)
    def key(self, iv_key, *, repeat=False):
        self._conn().key(iv_key)
    def type_text(self, text):
        self._conn().type_text(text)

    # frame
    def frame_hash(self):
        return self._conn().frame_hash()
    def wait_frame_change(self, before, *, timeout=8.0):
        return self._conn().wait_hash_change(before, timeout=timeout)
    def frame_ppm(self, name):
        import tempfile
        p = f"{tempfile.gettempdir()}/bs-{name}.ppm"
        self._conn().shot(p)
        return Path(p).read_bytes() if Path(p).is_file() else b""

    # log
    def log(self):
        return self._log.read()
    def current_log(self):
        text = self._log.read()
        idx = text.rfind(_LOG_OPEN_MARKER)
        return text[idx:] if idx != -1 else text
    def invocation_count(self):
        return self._log.read().count(_LOG_OPEN_MARKER)

    # lifecycle — restart relaunches a clean process (per-test isolation)
    def restart(self, *, wait_init=True):
        # Kill the running instance, give the socket time to drain, then
        # relaunch a fresh process on the same socket (the app unlinks the
        # stale path on bind).  A new process is what "fresh bookshelf"
        # per-test isolation means.
        self.kill_all()
        time.sleep(0.4)
        if self._relaunch is not None:
            self._relaunch()
            # wait for the (new) socket to come back
            from tests.support.bookshelf import backends as _B

            _B.wait_for(
                lambda: self._socket_ready(),
                timeout=20.0, label="SDL relaunch socket ready",
            )
    def kill_all(self):
        if self._ipc is not None:
            try:
                self._ipc.quit()
            except (ConnectionError, OSError):
                pass
            self._ipc = None
    def _socket_ready(self):
        try:
            self._conn()
            return True
        except (ConnectionError, OSError):
            return False

    # data dirs — the SDL app resolves config/covers/store next to its
    # binary; a per-instance run_dir keeps parallel workers isolated.
    # downloads fall back to /tmp (like the emulator guest).
    @property
    def books_dir(self):
        return self._run_dir
    @property
    def downloads_dir(self):
        return self._run_dir / "downloads"
    @property
    def config_path(self):
        return self._run_dir / "bookshelf.cfg"
    @property
    def store_path(self):
        return self._run_dir / "bookshelf_lib.db"
    @property
    def covers_dir(self):
        return self._run_dir / "covers"
    @property
    def tmp_dir(self):
        return self._run_dir
    @property
    def screen_size(self):
        return 1072, 1448
    @property
    def panel_h(self):
        return 0  # no system panel on the PC build

