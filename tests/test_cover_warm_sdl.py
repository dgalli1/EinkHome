"""Post-sync cover warm-up e2e (SDL backend).

After a remote sync the app must download EVERY book's cover into the
on-disk cache — not just the page the user happens to be looking at —
so the library still shows real covers when it is later viewed offline.
The stock mock provider serves only 1x1 placeholder covers (the warm
pass is designed to abort on those), so this module boots the app
against a provider that serves REAL cover PNGs and asserts that the
covers directory gains a file for every corpus book with zero
navigation past the first page.
"""

from __future__ import annotations

import io
import json
import os
import re
import shutil
import socket
import socketserver
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

import pytest

from tests.support.bookshelf import BookshelfGeometry, BookshelfSession
from tests.support.bookshelf.backends import SdlBackend, wait_for
from tests.support.bookshelf.env import (
    API_TOKEN,
    EINKHOME_ROOT,
    _ensure_sdl_test_binary,
)

# The api package lives in repo_root/api; importing api.api.server first
# inserts that dir on sys.path (it does so at import time), which then
# makes the providers package importable.  Note: this test spans two
# worlds — the API server (Python, in-process) and the GUI app (the SDL
# PC build, a subprocess) — so a code change to either side must keep
# both compiling.
from api.api.server import PbemuAPIServer, build_default_app
from providers.mock import MockProvider

pytestmark = pytest.mark.bookshelf


# ── a mock provider that serves real (non-1x1) covers ──────────────────

_REAL_COVER: bytes | None = None


def _real_cover() -> bytes:
    """A real 240x360 PNG (a genuine cover's post-process shape), cached
    once.  The warm pass's placeholder check only needs it to not be 1x1;
    the server's cover_proc resizes whatever valid bytes it gets."""
    global _REAL_COVER
    if _REAL_COVER is None:
        try:
            from PIL import Image
        except ImportError:  # pragma: no cover
            pytest.skip("Pillow not installed (server cover processing needs it)")
        img = Image.new("RGB", (240, 360), (37, 99, 235))
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        _REAL_COVER = buf.getvalue()
    return _REAL_COVER


class RealCoverMock(MockProvider):
    """MockProvider whose every cover is a real PNG instead of the 1x1
    placeholder, so the device-side warm pass has something worth keeping."""

    def get_cover(self, book_id: str) -> bytes | None:  # noqa: D102
        return _real_cover()


def _corpus(count: int = 14) -> list[dict]:
    uniq = uuid.uuid4().hex[:8]
    return [
        {
            "id": f"{uniq}_{i}",
            "title": f"Book {i}",
            "authors": ["Cover Warm"],
            "genre": "Sci-Fi",
            "added_at": f"2024-01-01T00:{i:02d}:00Z",
        }
        for i in range(count)
    ]


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _last_draw_grid(log: str) -> tuple[int, int]:
    m = re.findall(r"draw_grid view=(\d+) page=(\d+)", log)
    if not m:
        return 0, 0  # no frame drawn yet (early boot)
    view, page = m[-1]
    return int(view), int(page)


# ── environment: app online against a real-cover in-process server ─────

@pytest.fixture(scope="module")
def warm_env(tmp_path_factory):
    """Independent SDL environment: the app syncs once against an
    in-process server whose provider serves real covers.  Yields
    (bs, run_dir, corpus)."""
    if os.environ.get("BS_TEST_BACKEND", "emulator") != "sdl":
        pytest.skip("cover-warm SDL tests need BS_TEST_BACKEND=sdl")

    binary = _ensure_sdl_test_binary()
    run_dir = EINKHOME_ROOT / "build" / f"bs-warm-{os.getpid()}"
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "bookshelf_lib.db").unlink(missing_ok=True)
    (run_dir / "bookshelf_lib.db-journal").unlink(missing_ok=True)
    shutil.rmtree(run_dir / "covers", ignore_errors=True)
    app_exe = run_dir / "bookshelf.test"
    if not app_exe.exists():
        app_exe.symlink_to(binary)

    corpus = _corpus()
    corpus_path = tmp_path_factory.mktemp("warm-corpus") / "books.jsonl"
    corpus_path.write_text(
        "\n".join(json.dumps(r) for r in corpus) + "\n", encoding="utf-8"
    )

    cfg = {
        "host": "127.0.0.1",
        "port": 0,
        "api_token": API_TOKEN,
        "provider": "mock",
        "providers": {
            "mock": {
                "kind": "mock",
                "books_dir": str(tmp_path_factory.mktemp("warm-books")),
                "count": len(corpus),
                "corpus": str(corpus_path),
            }
        },
        "cover_cache": {"dir": str(tmp_path_factory.mktemp("warm-cache") / "cache")},
        "ledger": {"path": str(tmp_path_factory.mktemp("warm-ledger") / "sync-ledger.db")},
    }

    app = build_default_app(cfg)
    # Swap in a provider that serves real covers.
    app.provider = RealCoverMock(cfg["providers"]["mock"])
    RequestHandler = type("RequestHandler", (PbemuAPIServer,), {"app": app})
    httpd = socketserver.ThreadingTCPServer(("127.0.0.1", 0), RequestHandler)
    httpd.daemon_threads = True
    port = httpd.server_address[1]
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()

    sock = f"/tmp/bs-warm-{os.getpid()}.sock"
    logpath = run_dir / "bookshelf.log"
    env = os.environ.copy()
    env["BS_SOCKET"] = sock
    env["SDL_VIDEODRIVER"] = "dummy"
    env["PBEMU_LOG_DIR"] = str(run_dir)
    _held = {"proc": None}

    def _launch():
        p = subprocess.Popen(
            [str(app_exe)], cwd=run_dir, env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        _held["proc"] = p
        return p

    api_url = f"http://127.0.0.1:{port}"
    (run_dir / "bookshelf.cfg").write_text(
        f"api_url={api_url}\napi_token={API_TOKEN}\n", encoding="utf-8"
    )

    _launch()
    backend = SdlBackend(sock, str(logpath), api_url=api_url, run_dir=run_dir,
                         relaunch=_launch)
    geom = BookshelfGeometry(screen_w=1072, screen_h=1448, panel_h=0)
    bs = BookshelfSession(backend, geom, "sdl")

    # Wait for the boot sync to materialise the whole corpus (the grid
    # draw loops until view == len(corpus)).
    deadline = time.monotonic() + 60
    view = 0
    while time.monotonic() < deadline:
        try:
            log = backend.current_log()
        except (ConnectionError, OSError):
            time.sleep(0.2)
            continue
        view, _ = _last_draw_grid(log)
        if view >= len(corpus):
            break
        time.sleep(0.3)
    if view < len(corpus):
        _stop(httpd, thread, _held)
        raise AssertionError(
            f"sync never loaded the corpus (view={view}, want {len(corpus)})\n--- log ---\n"
            f"{backend.current_log()[-2000:]}"
        )

    # The app sits on page 1; only mouse input moves it, and none is sent.
    try:
        yield bs, run_dir, corpus
    finally:
        try:
            backend.kill_all()
        except Exception:  # noqa: BLE001
            pass
        _stop(httpd, thread, _held)


def _stop(httpd, thread, held) -> None:
    try:
        httpd.shutdown()
        httpd.server_close()
        thread.join(timeout=5)
    except Exception:  # noqa: BLE001
        pass
    p = held["proc"]
    if p is not None:
        p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()


def _covers_dir(run_dir: Path) -> Path:
    return run_dir / "covers"


# ── the contract under test ────────────────────────────────────────────

def test_sync_warms_every_cover_without_navigation(warm_env):
    """After one remote sync, every book's cover lands on disk — including
    books on pages the app never displayed (PAGESIZE=6, corpus=14, so 8 of
    the 14 are off the first page) — with zero pointer input."""
    bs, run_dir, corpus = warm_env
    covers = _covers_dir(run_dir)
    wanted = {b["id"] for b in corpus}

    assert len(wanted) > 6, "corpus must span more than one page for this test"

    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        have = {p.stem for p in covers.rglob("*.png")}
        if wanted <= have:
            break
        time.sleep(0.4)
    assert wanted <= have, (
        f"warm pass cached {len(have & wanted)}/{len(wanted)} covers; off-page books "
        f"never downloaded.\nmissing: {sorted(wanted - have)[:5]}\n--- log ---\n"
        f"{bs.current_log()[-2000:]}"
    )

    # The switch to JPEG: every persisted cover must carry JPEG magic (the
    # ".png" filename is a cache-slot label — the device sniffs the bytes).
    for b in corpus:
        f = next(covers.rglob(f"{b['id']}.png"), None)
        assert f is not None, f"no cached file for {b['id']}"
        with open(f, "rb") as fh:
            magic = fh.read(3)
        assert magic == b"\xff\xd8\xff", (
            f"{b['id']} cover is not a JPEG (magic={magic.hex()})"
        )

    # The initial page's on-screen fetch alone would only cache the first
    # PAGESIZE(6); reaching all 14 proves the warm pass ran to completion.
    bs.assert_no_crash()