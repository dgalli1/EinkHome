"""Basic smoke tests for the API server's data layer.

Run with:
    python -m pytest api/tests/ -v
or:
    pbemu test -- api/tests/test_providers.py
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import os
import sys

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.providers.base import BookMeta  # noqa: E402
from api.providers.mock import MockProvider  # noqa: E402


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


def test_mock_provider_unknown_book(tmp_path):
    provider = _make_provider(tmp_path, name="u")
    assert provider.open_file("nonexistent-id") is None
    # get_cover for an unknown id returns a placeholder PNG, not None —
    # this lets the device UI still display a "no cover" tile.
    cover = provider.get_cover("nonexistent-id")
    assert cover is not None
    assert len(cover) > 0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
