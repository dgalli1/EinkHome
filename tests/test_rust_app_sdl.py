"""Live-suggest e2e tests for the RUST app (eh_demo_sdl_app, SDL backend).

The Rust App (eh_ui/crates/eh_app) is the migration target of the C
bookshelf.  These tests drive it through the same UNIX-socket control
plane the C build's bookshelf.test exposes (the protocol client is
shared), exercising the live-suggestion flow end to end on the fast
backend:

    tap the search icon -> tap the input row (arms the fake keyboard)
    type a prefix LIVE (no commit) -> the app's 200 ms suggest tick polls
    the keyboard buffer and draws the suggestion band
    tap a suggestion row -> the keyboard is CANCELLED (no commit fires)
    and the app commits the tapped term itself (C CloseKeyboard +
    history-tap sequence)

Run:  EH_TEST_BACKEND=sdl pytest tests/test_rust_app_sdl.py -v
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import time
from pathlib import Path

import pytest

from tests.support.bookshelf.env import EINKHOME_ROOT

pytestmark = pytest.mark.bookshelf

W, H = 1072, 1448
TOP_BAR_H = 128
SEARCH_ROW_H = 88
ROW_H = 96  # history/suggestion row pitch


# ── build + launch helpers ──────────────────────────────────────────────

_RUST_BIN = EINKHOME_ROOT / "eh_ui" / "target" / "debug" / "sdl_app"
_RUST_LOCK = EINKHOME_ROOT / "build" / ".rust-sdl-app.lock"


def _ensure_rust_binary() -> Path:
    """Build eh_demo_sdl_app once (lock-guarded); returns the binary path."""
    import fcntl

    if _RUST_BIN.exists():
        return _RUST_BIN
    _RUST_LOCK.parent.mkdir(parents=True, exist_ok=True)
    with open(_RUST_LOCK, "w") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)
        try:
            if not _RUST_BIN.exists():
                subprocess.run(
                    [
                        "cargo", "build", "-p", "eh_demo",
                        "--features", "sdl", "--bin", "sdl_app",
                    ],
                    cwd=EINKHOME_ROOT / "eh_ui",
                    check=True,
                )
        finally:
            fcntl.flock(fh, fcntl.LOCK_UN)
    assert _RUST_BIN.exists(), "cargo build did not produce sdl_app"
    return _RUST_BIN


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _ppm_ink_xs(ppm: bytes, x0: int, y0: int, x1: int, y1: int) -> list[int]:
    """X coordinates of dark ink in the P6 region (C visual checks)."""
    header = ppm.index(b"255\n") + 4
    w = int(ppm.split(b"\n")[1].split()[0])
    px = ppm[header:]
    xs: list[int] = []
    for y in range(y0, min(y1, H)):
        row = px[y * w * 3:(y + 1) * w * 3]
        for x in range(x0, min(x1, w)):
            if row[x * 3] < 128:
                xs.append(x)
                break
        # one hit per row is enough
    return sorted(set(xs))


@pytest.fixture()
def rust_app_env(tmp_path_factory):
    """Mock API server + the Rust test runner, headless over IPC.

    Function-scoped: each test boots its own app instance so keyboard /
    navigation state never leaks between tests.

    Yields a dict with the IPC connection, run dir and log path."""
    from tests.support.bookshelf.env import _start_api_server, _stop_api_server

    binary = _ensure_rust_binary()
    run_dir = tmp_path_factory.mktemp("rust-app")
    app_dir = run_dir / "app"
    app_dir.mkdir()

    corpus = [
        {"id": f"ra_{i}", "title": t, "authors": [a],
         "genre": "Sci-Fi", "added_at": f"2023-01-01T00:00:{59 - i:02d}Z"}
        for i, (t, a) in enumerate([
            ("Dune of Ashes", "Ada Lovelace"),
            ("Bright Harbor", "Bram Stockwell"),
            ("Echo Canyon", "Ada Lovelace"),
            ("Flame Tide", "Bram Stockwell"),
        ])
    ]
    cfg = json.loads(
        (EINKHOME_ROOT / "tests" / "support" / "server-test.json").read_text(
            encoding="utf-8"
        )
    )
    port = _free_port()
    cfg["port"] = port
    cfg["providers"]["mock"].update(
        books_dir=str(run_dir / "books"),
        count=len(corpus),
        corpus=str(run_dir / "corpus.jsonl"),
    )
    cfg["cover_cache"]["dir"] = str(run_dir / "cache")
    cfg["ledger"]["path"] = str(run_dir / "ledger.db")
    cfg_path = run_dir / "server.json"
    cfg_path.write_text(json.dumps(cfg), encoding="utf-8")
    (run_dir / "corpus.jsonl").write_text(
        "\n".join(json.dumps(r) for r in corpus) + "\n", encoding="utf-8"
    )

    api_proc = _start_api_server(
        port=port, log_path=run_dir / "api.log", config=str(cfg_path)
    )

    sock = run_dir / "app.sock"
    env = os.environ.copy()
    env.update(
        EH_SOCKET=str(sock),
        EH_APP_DIR=str(app_dir),
        API=f"http://127.0.0.1:{port}",
        SDL_VIDEODRIVER="dummy",
    )
    proc = subprocess.Popen(
        [str(binary)], cwd=run_dir, env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if sock.exists():
            break
        if proc.poll() is not None:
            _stop_api_server(api_proc)
            raise RuntimeError("sdl_app exited before opening the socket")
        time.sleep(0.2)
    else:
        proc.terminate()
        _stop_api_server(api_proc)
        raise TimeoutError("control socket never appeared")

    time.sleep(3.0)  # boot sync + first draw settle

    from tests.support.bookshelf.ipc_sdl import IpcBookshelf

    bs = IpcBookshelf(str(sock))
    try:
        yield {
            "bs": bs,
            "app_dir": app_dir,
            "log": app_dir / "bookshelf.log",
            "proc": proc,
        }
    finally:
        try:
            bs.quit()
        except Exception:  # noqa: BLE001
            pass
        proc.terminate()
        _stop_api_server(api_proc)


def _log_since(env) -> str:
    p = env["log"]
    return p.read_text(encoding="utf-8", errors="replace") if p.exists() else ""


def _open_search_keyboard(bs) -> None:
    """Search page -> input row tap (arms the fake keyboard buffer)."""
    first = W - (8 + 4 * 96)
    bs.tap_at(first + 48, TOP_BAR_H // 2)  # top-bar search icon
    time.sleep(0.5)
    bs.tap_at(W // 2, TOP_BAR_H + SEARCH_ROW_H // 2)  # input row
    time.sleep(0.5)


# ── tests ────────────────────────────────────────────────────────────────

def test_rust_live_suggest_band_draws_from_typed_prefix(rust_app_env):
    """Typing 'dune' into the live buffer draws the suggestion band within
    a couple of 200 ms ticks — visible as new frame content."""
    env = rust_app_env
    bs = env["bs"]
    _open_search_keyboard(bs)

    before = bs.frame_hash()
    bs.type_text("dune")
    after = bs.wait_hash_change(before, timeout=5.0)
    assert after != before, "live typing produced no band repaint"

    # The band shows a left-aligned row in [input_bottom, +ROW_H): ink at
    # x < 300 there (the centered placeholder never reaches that far left).
    bs.shot("/tmp/rust_band.ppm")
    ppm = Path("/tmp/rust_band.ppm").read_bytes()
    xs = _ppm_ink_xs(ppm, 24, TOP_BAR_H + SEARCH_ROW_H, 300,
                     TOP_BAR_H + SEARCH_ROW_H + ROW_H)
    assert xs, "no suggestion row ink in the band"


def test_rust_suggest_tap_cancels_keyboard_and_commits_term(rust_app_env):
    """Tapping the suggestion row commits the term through the app-side
    path: the keyboard is cancelled (no RETURN commit) and the grid
    re-projects under the query (view_rebuild view=1)."""
    env = rust_app_env
    bs = env["bs"]
    _open_search_keyboard(bs)
    bs.type_text("dune")
    time.sleep(0.6)  # let the tick draw the band

    before = _log_since(env)
    row_cy = TOP_BAR_H + SEARCH_ROW_H + ROW_H // 2
    bs.tap_at(W // 2, row_cy)
    time.sleep(0.8)
    new = _log_since(env)[len(before):]

    assert "suggest tap: term=`dune`" in new, f"suggest tap not logged:\n{new}"
    assert "search commit: query=`dune`" in new, "term not committed"
    assert "view_rebuild: view=1 " in new, "grid not filtered to 1 book"
    overlay, tab, _page = bs.state()
    assert (overlay, tab) == (0, 0), "commit must land on the library shelf"


def test_rust_kb_commit_return_path_filters_grid(rust_app_env):
    """The kb_commit command (a real RETURN press) fires the keyboard
    handler; an edited buffer commits, the shelf filters."""
    env = rust_app_env
    bs = env["bs"]
    _open_search_keyboard(bs)
    bs.type_text("echo")
    before = _log_since(env)
    bs.cmd("kb_commit")
    time.sleep(0.8)
    new = _log_since(env)[len(before):]

    assert "search commit: query=`echo`" in new, "RETURN commit missing"
    assert "view_rebuild: view=1 " in new, "grid not filtered to 1 book"
