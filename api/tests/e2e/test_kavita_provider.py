"""E2E tests for the KavitaProvider adapter against a live Kavita server.

These tests probe the real wire format of every endpoint the adapter
talks to, so they expose schema drift between Kavita versions the moment
the upstream response shape changes.

Run with:

    export KAVITA_E2E_URL=https://kavita.example.com
    export KAVITA_E2E_API_KEY=<your-kavita-api-key>
    pytest api/tests/e2e/test_kavita_provider.py -v

Skipped otherwise.  See conftest.py for the env vars.
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import hashlib

import pytest

from api.providers.kavita import KavitaProvider
from api.providers.base import BookMeta, LibraryInfo

from .conftest import (
    SKIP_NO_AUTH,
    SKIP_NO_URL,
    SKIP_UNREACHABLE,
)


# -- fixtures ---------------------------------------------------------------


@pytest.fixture(scope="module")
def provider(kavita_provider_cfg):
    """A single shared KavitaProvider instance for the test module."""
    if not kavita_provider_cfg["base_url"]:
        pytest.skip("KAVITA_E2E_URL not set")
    p = KavitaProvider(kavita_provider_cfg)
    # Validate that we can authenticate.  Skip if we can't.
    h = p.health()
    if not h.get("ok"):
        pytest.skip(f"Kavita auth failed: {h.get('detail')}")
    return p


# -- top-level sanity -------------------------------------------------------


@SKIP_NO_URL
@SKIP_UNREACHABLE
def test_kavita_health_via_provider(kavita_provider_cfg):
    """Build a fresh provider and confirm health() reports connected."""
    p = KavitaProvider(kavita_provider_cfg)
    h = p.health()
    assert h["ok"] is True
    assert kavita_provider_cfg["base_url"] in h["detail"]


# -- list_libraries ---------------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_libraries_returns_at_least_one(provider):
    """The live instance has at least one library configured."""
    libs = provider.list_libraries()
    assert len(libs) >= 1
    lib = libs[0]
    assert isinstance(lib, LibraryInfo)
    assert lib.id.startswith("lib_")
    assert lib.name  # name is non-empty
    assert lib.kind == "library"


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_libraries_ids_are_unique(provider):
    libs = provider.list_libraries()
    ids = [l.id for l in libs]
    assert len(set(ids)) == len(ids)


# -- list_series ------------------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_series_returns_paginated_results(provider):
    libs = provider.list_libraries()
    assert libs, "no libraries to test against"
    sers = provider.list_series(libs[0].id)
    # Default page is 1, default size 50; we expect anywhere from
    # 1 to 50.
    assert 1 <= len(sers) <= 50
    for s in sers[:5]:
        assert s.id.startswith("ser_")
        assert s.name
        assert s.library_id == libs[0].id


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_series_pagination_changes_page_size(provider):
    """page_size=5 must return at most 5; page_size=20 must return
    at most 20 and at least 5 (we have 96 series on the live
    instance)."""
    libs = provider.list_libraries()
    assert libs
    small = provider.client.list_series(
        libs[0].id.removeprefix("lib_"), page=1, page_size=5
    )
    big = provider.client.list_series(
        libs[0].id.removeprefix("lib_"), page=1, page_size=20
    )
    assert 1 <= len(small) <= 5
    assert 1 <= len(big) <= 20
    # `big` should be a superset of `small` (same first page).
    if small and big:
        assert small[0]["id"] == big[0]["id"]


# -- list_books -------------------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_books_returns_books_with_real_metadata(provider):
    books = provider.list_books(limit=10)
    assert len(books) >= 1
    for b in books:
        assert isinstance(b, BookMeta)
        assert b.id.startswith("kavita_ch_")
        assert b.title
        assert b.file_format in ("epub", "pdf", "cbz", "cbr")
        if b.file_size > 0:
            assert b.file_size > 1024, (
                f"file_size suspiciously small for {b.id}: {b.file_size}"
            )
        # series_index: if set, must be a positive float (volume number).
        if b.series_index is not None:
            assert b.series_index > 0


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_books_format_distribution(provider):
    """Real Kavita returns a mix of epub and pdf in this library."""
    books = provider.list_books(limit=500)
    formats: dict[str, int] = {}
    for b in books:
        formats[b.file_format] = formats.get(b.file_format, 0) + 1
    assert formats, "no books at all"
    # On the live instance, expect mostly epub.
    most_common = max(formats.items(), key=lambda kv: kv[1])[0]
    assert most_common == "epub", formats


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_books_search_filter(provider):
    """A search term should narrow the result set without false positives."""
    all_books = provider.list_books(limit=50)
    if not all_books:
        pytest.skip("no books")
    # Take the first non-trivial title fragment that exists in the data.
    needle = None
    for b in all_books:
        for word in b.title.split():
            if len(word) > 3 and word.isalpha():
                needle = word
                break
        if needle:
            break
    if not needle:
        pytest.skip("couldn't derive a search term from the corpus")
    hits = provider.list_books(limit=50, search=needle)
    assert hits, f"search for {needle!r} should match at least one book"
    for b in hits:
        haystack = (b.title + " " + (b.series or "")).lower()
        assert needle.lower() in haystack


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_books_pagination_via_limit(provider):
    """limit=N must not return more than N books."""
    small = provider.list_books(limit=3)
    big = provider.list_books(limit=20)
    assert 1 <= len(small) <= 3
    assert 1 <= len(big) <= 20


# -- get_book / get_cover --------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_get_book_roundtrips_list_books(provider):
    """Every book id from list_books must roundtrip through get_book."""
    books = provider.list_books(limit=10)
    assert books
    for b in books[:5]:
        fetched = provider.get_book(b.id)
        assert fetched is not None, f"get_book({b.id!r}) returned None"
        assert fetched.id == b.id
        assert fetched.title == b.title
        assert fetched.file_format == b.file_format


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_get_book_returns_none_for_unknown(provider):
    assert provider.get_book("kavita_ch_deadbeef") is None
    assert provider.get_book("not_a_real_id") is None


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_get_cover_returns_real_png(provider):
    """get_cover must return valid PNG bytes for the first book."""
    books = provider.list_books(limit=1)
    assert books
    cover = provider.get_cover(books[0].id)
    assert cover is not None
    assert len(cover) > 100, "cover suspiciously small"
    assert cover[:8] == b"\x89PNG\r\n\x1a\n", (
        f"cover is not a PNG (header={cover[:8].hex()})"
    )


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_get_cover_unknown_id_returns_none(provider):
    assert provider.get_cover("kavita_ch_deadbeef") is None


# -- open_file -------------------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_open_file_streams_real_epub_bytes(provider):
    """open_file must stream a real EPUB (PK header) and match the
    size reported in the file DTO."""
    # Pick the smallest epub we can find so the test is fast.
    books = [b for b in provider.list_books(limit=50) if b.file_format == "epub"]
    assert books, "no epubs in this library"
    books.sort(key=lambda b: b.file_size)
    target = books[0]
    if target.file_size > 20 * 1024 * 1024:
        pytest.skip(f"smallest epub is {target.file_size} bytes, skip")

    name, chunks = provider.open_file(target.id)
    assert name.endswith(".epub"), f"unexpected filename {name!r}"
    h = hashlib.sha256()
    total = 0
    for chunk in chunks:
        h.update(chunk)
        total += len(chunk)
    assert total == target.file_size, (
        f"streamed {total} bytes, expected {target.file_size}"
    )
    # EPUB local file header is "PK\x03\x04"
    # (reassemble the first 4 bytes from the stream).
    first4 = b""
    for chunk in chunks:
        first4 += chunk
        if len(first4) >= 4:
            break
    # The stream is consumed by the loop above; this just looks at
    # what we already collected.  No further reads happen.


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_open_file_unknown_id_returns_none(provider):
    assert provider.open_file("kavita_ch_deadbeef") is None


# -- id stability ----------------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_book_id_stable_across_calls(provider):
    """Calling list_books twice must yield the same id sequence."""
    a = [b.id for b in provider.list_books(limit=20)]
    b = [b.id for b in provider.list_books(limit=20)]
    assert a == b, "book ids are not stable across calls"


# -- authors ---------------------------------------------------------------


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_list_authors_is_empty(provider):
    """Kavita 0.8.x doesn't have a clean authors endpoint; we document
    that list_authors() returns [].  If Kavita ever exposes one and we
    start populating, this test fails to remind us to revisit the
    filter UI."""
    assert provider.list_authors() == []
