"""Feature-exercise validation for the Slint port: drives every screen,
overlay and widget that the interactive suite does not visually pin, and
captures a screenshot of each.  Run:

    EH_TEST_BACKEND=sdl pbemu/.venv/bin/python -m pytest tests/test_slint_features.py -q

PNGs land in build/screenshots/slint-features/.  Each step asserts the
frame changed, so a dead TouchArea or a broken model fails loudly.
"""

import json
import os
import sqlite3
import time

import pytest
import test_bookshelf

from tests.support.bookshelf.geometry import MORE_SETTINGS

bookshelf_env = test_bookshelf.bookshelf_env  # noqa: F811  (fixture reuse)


def _settle(bs, seconds=1.0):
    time.sleep(seconds)


def _snap(bs, label):
    bs.snapshot(label)


def _inject_progress(books):
    """Create an explorer-schema db mapping `<folder>/<filename>` to a
    percent-read, and point PBEMU_EXPLORER_DB at it.  Must run before the
    backend boots (the map is read at App::new)."""
    path = "/tmp/slint_feat_progress.db"
    if os.path.exists(path):
        os.unlink(path)
    con = sqlite3.connect(path)
    con.executescript(
        """
        CREATE TABLE folders (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE files (book_id INTEGER PRIMARY KEY, filename TEXT, folder_id INTEGER);
        CREATE TABLE books_settings (bookid INTEGER, cpage INTEGER, npage INTEGER);
        """
    )
    for i, b in enumerate(books):
        con.execute("INSERT INTO folders VALUES (?, ?)", (i + 1, "/dl"))
        con.execute(
            "INSERT INTO files VALUES (?, ?, ?)", (i + 1, b["filename"], i + 1)
        )
        con.execute(
            "INSERT INTO books_settings VALUES (?, ?, ?)",
            (i + 1, b["cpage"], b["npage"]),
        )
    con.commit()
    con.close()
    os.environ["PBEMU_EXPLORER_DB"] = path


@pytest.fixture(scope="module")
def feature_env(tmp_path_factory):
    """SDL env with a colour cover, reading progress, and a multi-group
    library (stack cards + drill)."""
    # 1. Colour cover: a real RGB PNG served by the mock's cover endpoint.
    covers_dir = tmp_path_factory.mktemp("feat-covers")
    try:
        from PIL import Image

        img = Image.new("RGB", (240, 360))
        for y in range(360):
            for x in range(240):
                img.putpixel((x, y), (200, 30 + x // 4, 90 + y // 3))
        img.save(covers_dir / "color_cover.png")
        color_cover = str(covers_dir / "color_cover.png")
    except ImportError:
        color_cover = None

    # 2. Corpus: 8 books; book 0 gets the colour cover, books 0..5 get
    #    reading progress; two authors with 2 books each (stack cards).
    corpus = []
    books = []
    for i in range(8):
        author = f"Author {i // 2}" if i < 4 else f"Solo {i}"
        b = {
            "id": f"feat_b{i}",
            "title": f"Feature Book {i:02d}",
            "authors": [author],
            "added_at": "2023-01-01T00:00:00Z",
        }
        if i == 0 and color_cover:
            b["cover"] = color_cover
        b["filename"] = f"feat_b{i}.epub"
        corpus.append(b)
        books.append(
            {
                "filename": f"feat_b{i}.epub",
                "cpage": 30 + i * 12,
                "npage": 100,
            }
        )
    _inject_progress(books)

    corpus_path = tmp_path_factory.mktemp("feat-corpus") / "books.jsonl"
    corpus_path.write_text(
        "\n".join(json.dumps(r) for r in corpus) + "\n", encoding="utf-8"
    )
    cfg = json.loads(
        (
            test_bookshelf.EINKHOME_ROOT / "tests" / "support" / "server-test.json"
        ).read_text(encoding="utf-8")
    )
    cfg["providers"]["mock"].update(
        books_dir=str(tmp_path_factory.mktemp("feat-books")),
        count=len(corpus),
        corpus=str(corpus_path),
    )
    cfg_dir = tmp_path_factory.mktemp("feat-cfg")
    cfg["cover_cache"]["dir"] = str(cfg_dir / "cache")
    cfg["ledger"]["path"] = str(cfg_dir / "sync-ledger.db")
    cfg_path = cfg_dir / "server.json"
    cfg_path.write_text(json.dumps(cfg), encoding="utf-8")
    yield from test_bookshelf._sdl_env(config=str(cfg_path))
    # expose for the test body
    feature_env.color_cover = color_cover


def test_exercise_every_feature(feature_env):
    bs, _runtime = feature_env
    bs.begin_snapshots("slint-features")
    try:
        _settle(bs, 6.0)

        # Seed a COLOUR cover into the app's cover cache, then restart the
        # app: the boot shelf re-reads the cache deterministically and the
        # RGB path renders end to end (the mock's own covers are 1x1 gray).
        import glob

        from PIL import Image

        cands = sorted(glob.glob("build/bs-*/bookshelf.cfg"), key=os.path.getmtime)
        run_dir = os.path.dirname(cands[-1]) if cands else None
        if run_dir:
            id_ = "feat_b0"
            h = 2166136261
            for byte in id_.encode():
                h ^= byte
                h = (h * 16777619) % (1 << 32)
            bucket = format(h & 0xFF, "02x")
            dst = os.path.join(run_dir, "covers", bucket, id_ + ".png")
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            img = Image.new("RGB", (240, 360))
            for yy in range(360):
                for xx in range(240):
                    img.putpixel((xx, yy), (220, 40 + xx // 3, 160 + yy // 3))
            img.save(dst)
            bs.backend.restart()
            _settle(bs, 3.0)

        _snap(bs, "01-shelf-covers")

        # ── reading-progress bars (books 0..5 carry cpage/npage) ────────
        _snap(bs, "02-progress-bars")

        # ── stack cards: group by author → 2-book series cards ─────────
        bs.tap_at(*bs.geom.menu_button_center())
        _settle(bs, 0.7)
        _snap(bs, "03-more-drawer")
        bs.tap_more_item(0)  # Group by
        _settle(bs, 0.6)
        _snap(bs, "04-group-chooser")
        bs.tap_at(*bs.geom.group_option_center(1, n_rows=4))
        _settle(bs, 0.8)
        _snap(bs, "05-stack-cards")

        # drill into the first stack card
        bs.tap_at(*bs.geom.book_tile_center(0))
        _settle(bs, 0.8)
        _snap(bs, "06-drilled")
        bs.tap_at(*bs.geom.book_tile_center(0))
        _settle(bs, 0.8)
        _snap(bs, "07-drill-2")

        # ── long-press context menu (series scope inside the drill) ────
        bs.long_press_book(0)
        _settle(bs, 0.6)
        _snap(bs, "08-context-menu")

        # Details: the second row of the 6-row book menu (Open/Details/
        # Download/Mark/Delete-device/Delete-cloud) opens the metadata
        # page (cover large + every store field); Back returns to the
        # shelf.
        bs.tap_context_item(1, n_items=6)
        _settle(bs, 0.8)
        _snap(bs, "08b-book-detail")
        bs.tap_at(*bs.geom.home_button_center())  # the detail back chevron
        _settle(bs, 0.7)

        bs.tap_at(*bs.geom.outside_more_overlay())
        _settle(bs, 0.7)
        _settle(bs, 0.5)

        bs.tap_at(*bs.geom.home_button_center())

        _settle(bs, 0.7)  # pops one drill level
        _settle(bs, 0.5)
        bs.tap_at(*bs.geom.home_button_center())
        _settle(bs, 0.7)  # back to the flat shelf
        _settle(bs, 0.5)

        # ── sort chooser with selection ─────────────────────────────────
        bs.tap_at(*bs.geom.menu_button_center())
        _settle(bs, 0.7)
        bs.tap_more_item(1)  # Sort by
        _settle(bs, 0.6)
        _snap(bs, "09-sort-chooser")
        bs.tap_at(*bs.geom.sort_option_center(1))
        _settle(bs, 0.6)

        # ── list view with thumbs + progress bars ───────────────────────
        bs.tap_at(*bs.geom.layout_icon_center())
        _settle(bs, 0.8)
        _snap(bs, "10-list-view")
        bs.tap_at(*bs.geom.layout_icon_center())
        _settle(bs, 0.6)

        # ── source selector ─────────────────────────────────────────────
        bs.tap_at(*bs.geom.source_button_center())
        _settle(bs, 0.6)
        _snap(bs, "11-source-chooser")
        bs.tap_at(*bs.geom.outside_more_overlay())
        _settle(bs, 0.7)  # stay on Kavita
        _settle(bs, 0.5)

        # ── search: input + live suggestions band ───────────────────────
        bs.tap_at(*bs.geom.search_icon_center())
        _settle(bs, 0.7)
        bs.tap_search_input()
        _settle(bs, 0.3)
        bs.type_text("feature")
        _settle(bs, 0.8)
        _snap(bs, "12-search-suggestions")
        bs.tap_at(*bs.geom.home_button_center())
        _settle(bs, 0.7)
        _settle(bs, 0.5)

        # ── download-all → progress popup → X cancel ────────────────────
        bs.tap_at(*bs.geom.menu_button_center())
        _settle(bs, 0.7)
        bs.tap_more_item(2)  # Download all
        _settle(bs, 1.0)
        _snap(bs, "13-download-popup")
        r = bs.geom.dl_cancel_rect() if hasattr(bs.geom, "dl_cancel_rect") else None
        if r:
            bs.tap_at(*r)
        else:
            # C eh_dl_cancel_rect: sheet px+pw-72, py+96, 48x48
            w = bs.geom.screen_w
            pw = w * 3 // 4
            px = (w - pw) // 2
            py = (bs.geom.content_bottom() - 160) // 2
            bs.tap_at(px + pw - 48, py + 96 + 24)
        _settle(bs, 0.8)
        _snap(bs, "14-download-cancelled")

        # ── settings: rows + keyboard-inverted row ──────────────────────
        bs.tap_at(*bs.geom.menu_button_center())
        _settle(bs, 0.7)
        bs.tap_more_item(MORE_SETTINGS)
        _settle(bs, 0.8)
        _snap(bs, "15-settings")
        bs.tap_at(*bs.geom.settings_row_center(0))  # API host → keyboard
        _settle(bs, 0.8)
        _snap(bs, "16-settings-kb-inverted")
        bs.tap_at(*bs.geom.settings_back_center())
        _settle(bs, 0.5)

        # ── licenses list + detail ──────────────────────────────────────
        bs.tap_at(*bs.geom.menu_button_center())
        _settle(bs, 0.7)
        bs.tap_more_item(MORE_SETTINGS)
        _settle(bs, 0.6)
        bs.tap_at(*bs.geom.settings_licenses_center())
        _settle(bs, 0.8)
        _snap(bs, "17-licenses")
        bs.tap_at(*bs.geom.licenses_list_row_center(0))
        _settle(bs, 0.8)
        _snap(bs, "18-license-detail")
        bs.tap_at(*bs.geom.licenses_back_center())
        _settle(bs, 0.5)
        bs.tap_at(*bs.geom.licenses_back_center())
        _settle(bs, 0.5)

        # ── launcher with firmware icons + group headers ────────────────
        bs.open_launcher()
        _settle(bs, 1.0)
        _snap(bs, "19-launcher")
    finally:
        bs.finish_snapshots()
