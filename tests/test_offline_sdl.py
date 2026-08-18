"""Offline / no-internet e2e tests for the bookshelf app (SDL backend).

The SDL backend runs the native PC build against a local *mock* API
server — that server is the app's only reachable "internet".  These
tests assert the app's offline contract: the on-disk SQLite store and
the cover cache are the single source of truth, so once the API is made
unreachable the library must stay fully navigable, an already-downloaded
book must open with no network fetch, and sort / group-by / search must
keep working as client-side projections over the cached library.

Mechanics: a module-scoped fixture first runs the app ONLINE against a
synthetic multi-author corpus (sync populates the store + cover cache;
tapping book 0 downloads it to the /tmp dropbox).  It records the local
path from the reader-launch log line.  It then stops the app, points the
config at a dead port and relaunches — the SDL build's QueryNetwork()
always reports online, so the app attempts its boot sync, the transport
fails, and it keeps the cached rows (the same "do_sync FAILED but library
renders" path the emulator offline test exercises).

Run:  EH_TEST_BACKEND=sdl pytest tests/test_offline_sdl.py -v
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import time
from pathlib import Path

import pytest

from tests.support.bookshelf import BookshelfGeometry, BookshelfSession
from tests.support.bookshelf.backends import SdlBackend, wait_for
from tests.support.bookshelf.env import (
    API_TOKEN,
    EINKHOME_ROOT,
    _ensure_sdl_test_binary,
    _start_api_server,
    _stop_api_server,
)

pytestmark = pytest.mark.bookshelf


# ── corpus: deterministic two-author library ────────────────────────────
# Book ids are unique per run so files dropped into the shared /tmp
# dropbox (the SDL host has no /mnt/ext1, so the app falls back there)
# never collide across parallel workers.  Book 0 (index 0 → epub) is the
# one the fixture downloads online and the offline-spawn test reopens.
# 14 books across two authors, NO series (so nothing collapses) — the
# flat grid renders 14 tiles over PAGESIZE(6) = 3 pages for the pager,
# author grouping stacks 2 cards, and "dune" matches exactly one title.
def _corpus(uniq: str) -> list[dict]:
    def b(i: str, title: str, authors: list[str], added_at: str) -> dict:
        return {
            "id": f"{uniq}_{i}",
            "title": title,
            "authors": authors,
            "genre": "Sci-Fi",
            "added_at": added_at,
        }

    ada = ["Ada Lovelace"]
    bram = ["Bram Stockwell"]
    # Interleave authors so title-A-Z and by-author orders really differ.
    return [
        b("off_a1", "Althea Atlas",  ada,  "2023-01-01T00:00:13Z"),
        b("off_b1", "Bright Harbor", bram, "2023-01-01T00:00:12Z"),
        b("off_a2", "Cobalt Harbor", ada,  "2023-01-01T00:00:11Z"),
        b("off_b2", "Dune of Ashes", bram, "2023-01-01T00:00:10Z"),
        b("off_a3", "Echo Canyon",   ada,  "2023-01-01T00:00:09Z"),
        b("off_b3", "Flame Tide",    bram, "2023-01-01T00:00:08Z"),
        b("off_a4", "Glacier Point", ada,  "2023-01-01T00:00:07Z"),
        b("off_b4", "Harbor Key",    bram, "2023-01-01T00:00:06Z"),
        b("off_a5", "Iron Veil",     ada,  "2023-01-01T00:00:05Z"),
        b("off_b5", "Jade Moon",     bram, "2023-01-01T00:00:04Z"),
        b("off_a6", "Kindred Dawn",  ada,  "2023-01-01T00:00:03Z"),
        b("off_b6", "Lunar Fog",     bram, "2023-01-01T00:00:02Z"),
        b("off_a7", "Marble Falls",  ada,  "2023-01-01T00:00:01Z"),
        b("off_b7", "Nightfall Key", bram, "2023-01-01T00:00:00Z"),
    ]


# ── log helpers (mirror the ones in test_bookshelf.py) ─────────────────

def _last_draw_grid(log: str) -> tuple[int, int]:
    """(view_count, page) from the last draw_grid line in *log*."""
    matches = list(re.finditer(r"draw_grid view=(\d+) page=(\d+)", log))
    assert matches, "no draw_grid line in log"
    m = matches[-1]
    return int(m.group(1)), int(m.group(2))


def _wait_log_slice(bs, before: str, needle: str, *, timeout: float = 10.0) -> None:
    """Poll until *needle* appears in the log appended after *before*."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if needle in bs.current_log()[len(before):]:
            return
        time.sleep(0.2)
    raise TimeoutError(
        f"log slice never contained {needle!r} within {timeout}s\n"
        f"--- tail ---\n{bs.current_log()[-1500:]}"
    )


def _wait_offline_boot(bs) -> str:
    """Poll until the offline boot rendered the cached library grid.

    With EH_OFFLINE the app's QueryNetwork() reports no connection, so
    it skips the boot auto-sync entirely: the grid must render from the
    on-disk store with covers coming from the cache, and no sync request
    may have been attempted (proving zero network traffic)."""
    deadline = time.monotonic() + 20
    log = ""
    while time.monotonic() < deadline:
        log = bs.current_log()
        if "cover_tick cache hit id=" in log and "draw_grid view=" in log:
            break
        time.sleep(0.3)
    assert "do_sync ENTER" not in log, (
        "offline boot attempted a network sync (EH_OFFLINE not honoured)"
    )
    view, _ = _last_draw_grid(log)
    assert view >= 1, f"offline boot rendered an empty grid (view={view})"
    assert "cover_tick cache hit id=" in log, (
        "offline covers came from the network, not the cache"
    )
    return log


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# ── the offline environment ────────────────────────────────────────────
# Phase 1 (online): run against the live mock so the store + cover cache
# populate and book 0 downloads to disk.  Record the download path from
# the reader-launch log line (the app logs it verbatim — OpenBook on the
# PC build is a no-op but eh_launch_reader prints the path first).
# Phase 2 (offline): stop the app, point the config at a dead port,
# relaunch.  The module yields the offline session; each test restarts
# it (still pointing at the dead port) for a clean UI state.

@pytest.fixture(scope="module")
def offline_sdl_env(tmp_path_factory):
    """Independent SDL environment, brought up online then flipped
    offline (dead API port).  Yields (bs, {run_dir, book0_id, book0_path}).

    Fully self-contained: its own run dir, synthetic corpus, config, API
    port and cover/ledger state, so it never collides with the shared
    bookshelf_env module in test_bookshelf.py.
    """
    if os.environ.get("EH_TEST_BACKEND", "emulator") != "sdl":
        pytest.skip("offline SDL tests need EH_TEST_BACKEND=sdl")

    binary = _ensure_sdl_test_binary()

    # Per-module run dir: the app resolves config/store/covers next to
    # its binary, so this isolates the fixture from every other worker.
    run_dir = EINKHOME_ROOT / "build" / f"bs-offline-{os.getpid()}"
    run_dir.mkdir(parents=True, exist_ok=True)
    # A pid can be reused across pytest runs (or a prior run left state):
    # start from a clean store + covers so the online phase really syncs
    # and downloads afresh instead of inheriting a past run's flags.
    (run_dir / "bookshelf_lib.db").unlink(missing_ok=True)
    (run_dir / "bookshelf_lib.db-journal").unlink(missing_ok=True)
    shutil.rmtree(run_dir / "covers", ignore_errors=True)
    app_exe = run_dir / "bookshelf.test"
    if not app_exe.exists():
        app_exe.symlink_to(binary)

    # Synthetic corpus + an isolated server config (own ledger + cover
    # cache in the temp tree, so the mock walk folds our corpus fresh
    # instead of replaying the shipped 16 books).  Book ids are unique
    # per worker (pid suffix) so the /tmp dropbox never collides across
    # parallel runs; drop any leftover files from that id family too.
    uniq = str(os.getpid())
    corpus = _corpus(uniq)
    for leftover in Path("/tmp").glob(f"{uniq}_*"):
        leftover.unlink(missing_ok=True)
    cfg = json.loads(
        (EINKHOME_ROOT / "tests" / "support" / "server-test.json").read_text(
            encoding="utf-8"
        )
    )
    cfg["providers"]["mock"].update(
        books_dir=str(tmp_path_factory.mktemp("offline-books")),
        count=len(corpus),
        corpus=str(tmp_path_factory.mktemp("offline-corpus") / "books.jsonl"),
    )
    cfg["cover_cache"]["dir"] = str(tmp_path_factory.mktemp("offline-cache") / "cache")
    cfg["ledger"]["path"] = str(tmp_path_factory.mktemp("offline-ledger") / "sync-ledger.db")
    cfg_path = tmp_path_factory.mktemp("offline-cfg") / "server.json"
    cfg_path.parent.mkdir(parents=True, exist_ok=True)
    cfg_path.write_text(json.dumps(cfg), encoding="utf-8")
    # Write the corpus file the config points at.
    corpus_path = Path(cfg["providers"]["mock"]["corpus"])
    corpus_path.parent.mkdir(parents=True, exist_ok=True)
    corpus_path.write_text(
        "\n".join(json.dumps(r) for r in corpus) + "\n", encoding="utf-8"
    )

    # A free port per run: parallel workers' API servers must not share one.
    port = _free_port()
    cfg["port"] = port
    cfg_path.write_text(json.dumps(cfg), encoding="utf-8")
    api_proc = _start_api_server(
        port=port, log_path=run_dir / "api.log", config=str(cfg_path)
    )

    sock = f"/tmp/bs-offline-{os.getpid()}.sock"
    logpath = run_dir / "bookshelf.log"
    env = os.environ.copy()
    env["EH_SOCKET"] = sock
    env["SDL_VIDEODRIVER"] = "dummy"
    env["PBEMU_LOG_DIR"] = str(run_dir)
    _held = {"proc": None, "live_api": f"http://127.0.0.1:{port}"}

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

    proc = _launch()
    backend = SdlBackend(
        sock, str(logpath), api_url=api_url, run_dir=run_dir, relaunch=_launch)

    geom = BookshelfGeometry(screen_w=1072, screen_h=1448, panel_h=0)
    bs = BookshelfSession(backend, geom, "sdl")

    # ── Phase 1: online sync ──────────────────────────────────────────
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            backend.frame_hash()
            break
        except (ConnectionError, OSError):
            time.sleep(0.2)
    # Let sync + the initial draw settle, then confirm the corpus loaded.
    deadline = time.monotonic() + 20
    view = 0
    while time.monotonic() < deadline:
        log = backend.current_log()
        view, _ = _last_draw_grid(log)
        if view >= len(corpus):
            break
        time.sleep(0.3)
    if view < len(corpus):
        # Walk away cleanly instead of leaking the server.
        _stop_api_server(api_proc)
        proc.terminate()
        raise RuntimeError(
            f"online sync never loaded the corpus (view={view}, want "
            f"{len(corpus)})\n--- log tail ---\n{backend.current_log()[-2000:]}"
        )

    # Download book 0 so the offline-spawn test has an on-disk book.
    before = backend.current_log()
    bs.tap_book(0)
    _wait_log_slice(bs, before, "download_book_file OK", timeout=20.0)
    m = re.search(
        r"download_book_file OK id=(\S+) path=(\S+)",
        bs.current_log()[len(before):],
    )
    assert m, "online download never logged its destination path"
    book0_id = corpus[0]["id"]
    book0_path = m.group(2)
    assert m.group(1) == book0_id, (
        f"downloaded id {m.group(1)!r} != {book0_id!r}"
    )

    # ── Phase 2: go offline and relaunch ──────────────────────────────
    # EH_OFFLINE makes the SDL build's QueryNetwork() report offline, so
    # the app skips its boot auto-sync and skips remote cover fetches —
    # it renders purely from the on-disk store + cover cache (the same
    # behaviour a real device shows with WiFi off).  The relaunch reuses
    # *env*, so every later per-test restart inherits EH_OFFLINE too.
    backend.kill_all()  # quit the online instance
    env["EH_OFFLINE"] = "1"
    (run_dir / "bookshelf.cfg").write_text(
        f"api_url=http://127.0.0.1:9\napi_token={API_TOKEN}\n", encoding="utf-8"
    )
    _launch()
    # Wait for the offline boot to finish in the LOG first (no socket
    # needed): once the grid is drawn the event loop is idle and the IPC
    # control socket answers promptly.  Connecting mid-boot races the
    # app's single-client accept loop and can stall for its whole 5 s
    # read timeout per attempt.
    _wait_offline_boot(bs)
    wait_for(
        lambda: _socket_ready(backend),
        timeout=30.0, label="offline relaunch socket ready",
    )

    meta = {
        "run_dir": run_dir,
        "book0_id": book0_id,
        "book0_path": book0_path,
        "corpus": corpus,
    }
    try:
        yield bs, meta
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


def _socket_ready(backend) -> bool:
    try:
        backend.frame_hash()
        return True
    except (ConnectionError, OSError):
        return False


@pytest.fixture
def offline_bookshelf(offline_sdl_env, request):
    """Restart the (offline) app before each test for a clean state."""
    bs, meta = offline_sdl_env
    bs.begin_snapshots(request.node.name)
    bs.backend.restart()  # relaunches on the same dead-port config
    bs.snapshot("boot")
    _wait_offline_boot(bs)
    yield bs, meta
    bs.finish_snapshots()
    bs.assert_no_crash()


# ── 1. library navigates without internet ──────────────────────────────

def test_offline_library_navigable(offline_bookshelf):
    """With the API unreachable, bookshelf boots from the on-disk store +
    cover cache and stays fully navigable: the grid renders, covers
    blit from the cache, the pager moves, and no tap respawns the app."""
    bs, meta = offline_bookshelf
    invocations = bs.invocation_count()
    log = _wait_offline_boot(bs)
    view, page = _last_draw_grid(log)
    assert view == len(meta["corpus"]), (
        f"offline grid shows {view} books, expected {len(meta['corpus'])}"
    )

    # Pager advances and returns, all without touching the network.
    before = bs.frame_hash()
    bs.tap_pager_next()
    bs.wait_hash_change(before)
    _, page2 = _last_draw_grid(bs.current_log())
    assert page2 > page, "pager next did not advance the page offline"

    before = bs.frame_hash()
    bs.tap_pager_prev()
    bs.wait_hash_change(before)
    _, page_back = _last_draw_grid(bs.current_log())
    assert page_back == page, "pager prev did not return to the original page"

    # Offline navigation must not have respawned or crashed the app.
    assert bs.invocation_count() == invocations, "offline navigation respawned the app"
    bs.assert_no_crash()


# ── 2. an already-downloaded book opens without internet ───────────────

def test_offline_already_downloaded_book_opens(offline_bookshelf):
    """A book whose file is already on disk opens via the reader with NO
    network fetch: the tap must log a reader launch through the local
    file path, and must NOT log a download attempt."""
    bs, meta = offline_bookshelf
    _wait_offline_boot(bs)
    # The downloaded flag persisted in the store from the online phase,
    # so tapping book 0 must open the on-disk file right away.

    before = bs.current_log()
    bs.tap_book(0)
    _wait_log_slice(bs, before, "launching reader via OpenBook", timeout=15.0)
    opened = bs.current_log()[len(before):]
    assert meta["book0_path"] in opened, (
        f"reader launched for path {meta['book0_path']!r}, log shows:\n{opened[-1500:]}"
    )
    assert "download_book_file" not in opened, (
        "offline open re-downloaded the book instead of using the local file"
    )


# ── 3. sort / group-by / search keep working without internet ──────────

def test_offline_sort_reorders_shelf(offline_bookshelf):
    """Sorting reorders the cached library offline (author vs title).

    The corpus's interleaved authors mean by-author and title-A-Z order
    differ, so applying each sort must change the frame — all projected
    from the local store, with no network on the path."""
    bs, _ = offline_bookshelf
    _wait_offline_boot(bs)

    bs.choose_sort(_SORT_TITLE_AZ)  # title A-Z
    title_hash = bs.wait_for_stable()
    bs.choose_sort(_SORT_AUTHOR)  # by author
    author_hash = bs.wait_for_stable()
    assert author_hash != title_hash, "author sort did not reorder the offline shelf"
    assert "download_book_file" not in bs.current_log()
    bs.assert_no_crash()


def test_offline_group_by_author_and_drill(offline_bookshelf):
    """Group-by-author collapses the cached library into stack cards
    offline, and drilling in/back works."""
    bs, _ = offline_bookshelf
    _wait_offline_boot(bs)

    before = bs.frame_hash()
    bs.choose_group("author")  # 2 authors -> 2 cards
    bs.wait_hash_change(before)

    # Grouping re-projects the cached store: view count drops to the
    # card count (2), then rising to the member count when drilled.
    group_view, _ = _last_draw_grid(bs.current_log())
    assert group_view == 2, f"author grouping did not stack 2 cards (view={group_view})"

    # Drill into a group card, then back — both stay offline.
    before = bs.current_log()
    bs.tap_group_header()
    _wait_log_slice(bs, before, f"view_rebuild: view={7} sort=", timeout=10.0)
    before = bs.frame_hash()
    bs.send_back_key()  # pop the drill back to the 2-card shelf
    bs.wait_hash_change(before)
    drill_view, _ = _last_draw_grid(bs.current_log())
    assert drill_view == 2, f"drill-back did not restore the card shelf (view={drill_view})"
    bs.assert_no_crash()


def test_offline_search_filters_grid(offline_bookshelf):
    """Search narrows the cached library offline to the matching book.

    The SDL build's on-screen keyboard is not rendered, so the commit is
    driven over the IPC control plane: tap the input row (opens the
    keyboard buffer), feed "dune" into it, then commit exactly like a
    RETURN press.  The filter is a client-side SQL projection over the
    cached store — no network.
    """
    bs, _ = offline_bookshelf
    _wait_offline_boot(bs)

    bs.tap_search()
    time.sleep(0.5)
    bs.tap_search_input()  # sets search_kb + opens the keyboard buffer
    time.sleep(0.3)
    bs.backend.type_text("dune")  # AppendIpcText -> keyboard buffer
    time.sleep(0.3)
    bs.backend._conn().cmd("kb_commit")  # fire the keyboard handler (RETURN)

    view, _ = _last_draw_grid(bs.current_log())
    assert view == 1, f"offline search did not filter to 1 book (view={view})"
    bs.assert_no_crash()


# Sort option rows in the "Sort by" sheet (title A-Z, author, series,
# recent) — same indices the stock suite uses.
_SORT_TITLE_AZ = 0
_SORT_AUTHOR = 1
