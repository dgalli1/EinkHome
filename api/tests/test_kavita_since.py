"""Offline tests for KavitaProvider.list_books' `since` normalization.

The provider must compare mixed-offset ISO-8601 timestamps correctly:
`since` and every book's `updated_at` are normalized to UTC before the
comparison, unparseable `updated_at` values keep the book, and an
unparseable `since` degrades to a raw string compare.  No network: the
client's `_request_json` is stubbed with a fixed catalogue (same
pattern as tests/test_login_errors.py).

Run with:
    python -m pytest api/tests/test_kavita_since.py -v
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import os
import sys
import time

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from providers.kavita import KavitaProvider  # noqa: E402


def _chapter(chapter_id, title, last_modified_utc):
    return {
        "id": chapter_id,
        "volumeId": 10,
        "titleName": title,
        "createdUtc": "2025-01-01T00:00:00Z",
        "lastModifiedUtc": last_modified_utc,
        "files": [
            {
                "extension": ".epub",
                "format": 1,
                "bytes": 1234,
                "pages": 10,
                "filePath": f"/library/{title}.epub",
            }
        ],
    }


def _make_provider(chapters):
    """A KavitaProvider wired to a stubbed upstream: nothing listens on
    the base_url; `_request_json` answers from the fixture catalogue."""
    provider = KavitaProvider(
        {
            # Nothing accepts connections on port 1.
            "base_url": "http://127.0.0.1:1",
            "api_key": "00000000-0000-0000-0000-000000000000",
            "username": "user",
            "password": "pass",
            # Skips client.list_libraries entirely.
            "library_ids": [1],
        }
    )
    series = {
        "id": 7,
        "name": "Stub Series",
        "libraryId": 1,
        "summary": "stub",
    }
    volume = {"id": 10, "number": "1", "chapters": chapters}

    def fake_request_json(method, path, body=None, with_auth=True, cacheable=True):
        if path.startswith("/api/Series/v2"):
            return 200, {"result": [series], "totalCount": 1}
        if path.startswith("/api/Series/volumes?"):
            return 200, [volume]
        raise AssertionError(f"unexpected upstream request: {method} {path}")

    # A pre-issued JWT keeps ensure_auth off the network.
    provider.client._jwt = "test-jwt"
    provider.client._jwt_expiry = time.time() + 3600
    provider.client._request_json = fake_request_json
    return provider


def _titles(metas):
    return sorted(m.title for m in metas)


def test_since_normalizes_mixed_offsets(tmp_path):
    """`since` in Z form vs updated_at carrying a +01:30 offset: the
    comparison happens in UTC, so 13:30+01:30 == 12:00Z is the exact
    boundary and is excluded ("not newer than since")."""
    provider = _make_provider(
        [
            _chapter(0x11, "Boundary", "2026-01-15T13:30:00+01:30"),
            _chapter(0x22, "Newer", "2026-01-15T12:00:01+00:00"),
        ]
    )
    metas = provider.list_books(since="2026-01-15T12:00:00Z")
    assert _titles(metas) == ["Newer"]


def test_unparseable_updated_at_keeps_book():
    """An updated_at that cannot be parsed must not silently drop the
    book — it survives the filter even though its raw string would
    sort below the since stamp ("0000..." < "2026...")."""
    provider = _make_provider([_chapter(0x33, "WeirdStamp", "0000-01-01T00:00:00Z")])
    metas = provider.list_books(since="2026-01-15T12:00:00Z")
    assert _titles(metas) == ["WeirdStamp"]


def test_unparseable_since_falls_back_to_raw_compare():
    """When `since` itself is garbage the filter degrades to a raw
    string comparison against the raw updated_at values."""
    provider = _make_provider(
        [
            _chapter(0x44, "SortsBefore", "aaa"),
            _chapter(0x55, "SortsAfter", "zzz"),
        ]
    )
    metas = provider.list_books(since="mmm-not-a-date")
    assert _titles(metas) == ["SortsAfter"]


def test_since_with_offset_and_limit_slice():
    """Paging still applies to the books that survive the since filter."""
    provider = _make_provider(
        [
            _chapter(0x66, "Alpha", "2026-02-01T00:00:00Z"),
            _chapter(0x77, "Beta", "2026-02-02T00:00:00Z"),
            _chapter(0x88, "Gamma", "2026-02-03T00:00:00Z"),
        ]
    )
    page1 = provider.list_books(limit=2, offset=0, since="2026-01-01T00:00:00Z")
    page2 = provider.list_books(limit=2, offset=2, since="2026-01-01T00:00:00Z")
    assert _titles(page1) == ["Alpha", "Beta"]
    assert _titles(page2) == ["Gamma"]
    assert all(m.updated_at.startswith("2026-02") for m in page1 + page2)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
