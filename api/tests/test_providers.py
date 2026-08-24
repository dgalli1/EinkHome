"""Basic smoke tests for the API server's data layer.

Run with:
    python -m pytest api/tests/ -v
or:
    pbemu test -- api/tests/test_providers.py
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import json
import os
import sys
import time

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.providers.base import BookMeta  # noqa: E402
from api.providers.mock import MockProvider  # noqa: E402
from api.storage.ledger import fingerprint  # noqa: E402


def _make_provider(tmp_path, name="test lib"):
    return MockProvider(
        {
            "books_dir": str(tmp_path),
            "library_name": name,
        }
    )


def test_mock_provider_lists_books(tmp_path):
    (tmp_path / "Alpha.epub").write_bytes(b"fake content")
    (tmp_path / "Beta.epub").write_bytes(b"more content")
    provider = _make_provider(tmp_path)
    libs = provider.list_libraries()
    assert len(libs) == 1
    assert libs[0].name == "test lib"

    books = list(provider.list_books())
    titles = sorted(b.title for b in books)
    assert titles == ["Alpha", "Beta"]


def test_mock_provider_open_file_iter(tmp_path):
    payload = b"hello world!" * 32
    (tmp_path / "Test.epub").write_bytes(payload)
    provider = _make_provider(tmp_path, name="t")
    books = list(provider.list_books())
    assert len(books) == 1
    result = provider.open_file(books[0].id)
    assert result is not None
    name, chunks = result
    assert name.endswith(".epub")
    assert b"".join(chunks) == payload


def test_mock_provider_get_cover(tmp_path):
    (tmp_path / "Coverless.epub").write_bytes(b"abc")
    provider = _make_provider(tmp_path, name="c")
    books = list(provider.list_books())
    cover = provider.get_cover(books[0].id)
    # No real cover in our test fixture, so we get a 1x1 placeholder.
    assert cover is not None
    assert len(cover) > 0


def test_mock_provider_id_stable_across_calls(tmp_path):
    (tmp_path / "Stable.epub").write_bytes(b"x")
    provider = _make_provider(tmp_path, name="s")
    books_a = list(provider.list_books())
    books_b = list(provider.list_books())
    assert books_a[0].id == books_b[0].id


def test_book_meta_dataclass():
    b = BookMeta(
        id="x",
        title="X",
        authors=["a", "b"],
        series="s",
        series_index=1.0,
        file_format="epub",
        file_size=123,
    )
    assert b.id == "x"
    assert b.authors == ["a", "b"]
    assert b.series_index == 1.0


def test_mock_walk_fingerprints_matches_walk_books_and_rebuilds(tmp_path):
    """The compact fingerprint walk must agree with the full walk_books
    pass (same ids, fingerprints, added_at) and rebuild its index when
    the corpus file changes — even when only the mtime moves."""
    corpus = tmp_path / "corpus.jsonl"
    corpus.write_text(
        "\n".join(
            json.dumps(rec)
            for rec in [
                {"id": "ol_1", "title": "Old", "authors": ["Ann"]},
                {"id": "ol_2", "title": "Two", "series": "Saga"},
                {"id": "ol_3", "title": "Three", "added_at": "2024-01-02T00:00:00Z"},
            ]
        )
        + "\n"
    )
    provider = MockProvider(
        {"books_dir": str(tmp_path / "books"), "corpus": str(corpus), "count": 3}
    )

    triples = list(provider.walk_fingerprints())
    metas = [
        m for batch in provider.walk_books(mode="all", chunk_size=10) for m in batch
    ]
    assert [t[0] for t in triples] == [m.id for m in metas]
    assert [t[1] for t in triples] == [fingerprint(m) for m in metas]
    assert [t[2] for t in triples] == [m.added_at for m in metas]

    # Rewrite record 1 with a same-length title (file size unchanged)
    # and bump only the mtime: the index key's mtime component is what
    # detects the edit, so the rebuilt walk must serve the new fp.
    corpus.write_text(
        "\n".join(
            json.dumps(rec)
            for rec in [
                {"id": "ol_1", "title": "New", "authors": ["Ann"]},
                {"id": "ol_2", "title": "Two", "series": "Saga"},
                {"id": "ol_3", "title": "Three", "added_at": "2024-01-02T00:00:00Z"},
            ]
        )
        + "\n"
    )
    st = corpus.stat()
    os.utime(corpus, ns=(st.st_atime_ns, st.st_mtime_ns + 1_000_000_000))
    triples2 = list(provider.walk_fingerprints())
    metas2 = [
        m for batch in provider.walk_books(mode="all", chunk_size=10) for m in batch
    ]
    assert [t[0] for t in triples2] == [m.id for m in metas2]
    assert [t[1] for t in triples2] == [fingerprint(m) for m in metas2]
    assert [t[2] for t in triples2] == [m.added_at for m in metas2]
    assert triples2[0][1] != triples[0][1]  # rebuild picked up the new title


def test_mock_provider_unknown_book(tmp_path):
    provider = _make_provider(tmp_path, name="u")
    assert provider.open_file("nonexistent-id") is None
    # get_cover for an unknown id returns a placeholder PNG, not None —
    # this lets the device UI still display a "no cover" tile.
    cover = provider.get_cover("nonexistent-id")
    assert cover is not None
    assert len(cover) > 0


# --- `since` filtering ---------------------------------------------------


def _iso_epoch(t):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def test_mock_list_books_since_boundary(tmp_path):
    """A book whose updated_at equals `since` exactly is excluded; the
    comparison is a plain string compare of UTC ISO stamps."""
    mtime = 1768478400  # 2026-01-15T12:00:00Z
    (tmp_path / "Alpha.epub").write_bytes(b"a")
    os.utime(tmp_path / "Alpha.epub", (mtime, mtime))
    provider = _make_provider(tmp_path)
    assert len(list(provider.list_books())) == 1
    # Boundary: equal timestamp is "not newer", so excluded.
    assert list(provider.list_books(since=_iso_epoch(mtime))) == []
    # One second earlier: the book is strictly newer, so kept.
    kept = list(provider.list_books(since=_iso_epoch(mtime - 1)))
    assert [b.title for b in kept] == ["Alpha"]
    assert kept[0].updated_at == _iso_epoch(mtime)


def test_mock_list_books_since_paginates(tmp_path):
    """With `since` set the unfiltered index fast path is bypassed and
    offset/limit paging still applies to the surviving books."""
    old_t = 1768478400
    new_t = old_t + 60
    for name, t in (("Old.epub", old_t), ("New1.epub", new_t), ("New2.epub", new_t)):
        (tmp_path / name).write_bytes(b"x")
        os.utime(tmp_path / name, (t, t))
    provider = _make_provider(tmp_path)
    page1 = provider.list_books(limit=1, offset=0, since=_iso_epoch(old_t))
    page2 = provider.list_books(limit=1, offset=1, since=_iso_epoch(old_t))
    titles = [b.title for b in page1 + page2]
    assert sorted(titles) == ["New1", "New2"]
    # The stale book never leaks through any page.
    everything = provider.list_books(limit=10, offset=0, since=_iso_epoch(old_t))
    assert all(b.title != "Old" for b in everything)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
