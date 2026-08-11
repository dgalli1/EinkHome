"""Unit tests for the on-disk cover cache (api/storage/cover_cache.py).

Covers the atomic write/read roundtrip, the startup sweep of orphaned
``.tmp`` files, freshness expiry, the future-mtime clamp, the
placeholder-skip in ``process_and_store`` and the has_png/read_png
read side.  All hermetic — no network, no Pillow needed.
"""

from __future__ import annotations

import os
import sys
import time

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from storage.cover_cache import CoverCache  # noqa: E402
from storage.placeholder import PLACEHOLDER_PNG  # noqa: E402


def _cache(tmp_path, **kw):
    return CoverCache(str(tmp_path / "cc"), **kw)


def test_atomic_write_read_roundtrip(tmp_path):
    cache = _cache(tmp_path)
    payload = b"\x89PNG\r\n\x1a\n" + b"\x00" * 512
    # Book ids may contain slashes/colons; the cache keys them by sha256.
    cache.store_png("book/with:slash", payload)
    assert cache.read_png("book/with:slash") == payload
    # The atomic write leaves no half-written tmp files behind.
    leftovers = [n for n in os.listdir(cache.directory) if n.endswith(".tmp")]
    assert leftovers == []


def test_startup_sweep_removes_orphaned_tmp(tmp_path):
    d = tmp_path / "cc"
    d.mkdir()
    stale = d / "stale.tmp"
    stale.write_bytes(b"half-written")
    old = time.time() - 7200  # older than the 1h sweep cutoff
    os.utime(stale, (old, old))
    fresh = d / "fresh.tmp"  # recent: could be an in-flight write
    fresh.write_bytes(b"in-flight")
    keep = d / "keep.png"
    keep.write_bytes(b"png")

    _cache(tmp_path)  # constructor runs the sweep
    assert not stale.exists(), "stale orphaned tmp must be swept"
    assert fresh.exists(), "recent tmp must be left alone"
    assert keep.exists(), "real cache entries are never touched"


def test_freshness_expiry(tmp_path):
    cache = _cache(tmp_path, max_age_seconds=60)
    cache.store_png("b1", b"png-data")
    assert cache.has_png("b1")
    assert cache.read_png("b1") == b"png-data"

    old = time.time() - 120  # older than max_age
    os.utime(cache.png_path("b1"), (old, old))
    assert not cache.has_png("b1")
    assert cache.read_png("b1") is None


def test_future_mtime_counts_as_fresh(tmp_path):
    """A file with a future mtime (clock skew / restored fs) must not be
    treated as stale forever — it is clamped and reads as fresh."""
    cache = _cache(tmp_path, max_age_seconds=60)
    cache.store_png("b1", b"png-data")
    future = time.time() + 3600
    os.utime(cache.png_path("b1"), (future, future))
    assert cache.has_png("b1") is True
    assert cache.read_png("b1") == b"png-data"


def test_has_png_missing_vs_present(tmp_path):
    cache = _cache(tmp_path)
    assert not cache.has_png("missing")
    assert cache.read_png("missing") is None
    cache.store_png("present", b"png")
    assert cache.has_png("present")
    assert cache.read_png("present") == b"png"


def test_process_and_store_skips_placeholder(tmp_path):
    """Placeholder sources (e.g. the mock provider's coverless books)
    are served but never written to the disk cache."""
    cache = _cache(tmp_path)
    out = cache.process_and_store("no-cover", PLACEHOLDER_PNG)
    assert out == PLACEHOLDER_PNG
    assert not cache.has_png("no-cover")
    assert not os.path.exists(cache.png_path("no-cover"))
    # A subsequent read still misses — nothing was stored.
    assert cache.read_png("no-cover") is None


def test_negative_cache_ttl(tmp_path):
    """Marked-missing ids report missing until the TTL expires, then
    become fetchable again."""
    cache = _cache(tmp_path)
    assert not cache.is_missing("b1")
    cache.mark_missing("b1")
    assert cache.is_missing("b1")
    # Expire the entry by backdating it past _MISSING_TTL.
    from storage.cover_cache import _MISSING_TTL

    cache._missing["b1"] = time.time() - (_MISSING_TTL + 60)
    assert not cache.is_missing("b1")
    # Expired entries are dropped, not kept around.
    assert "b1" not in cache._missing


def test_negative_cache_bounded(tmp_path):
    """The negative cache never grows past _MISSING_MAX entries; the
    oldest are evicted first."""
    from storage.cover_cache import _MISSING_MAX

    cache = _cache(tmp_path)
    for i in range(_MISSING_MAX + 100):
        cache.mark_missing(f"b{i}")
    assert len(cache._missing) == _MISSING_MAX
    assert not cache.is_missing("b0"), "oldest entry must be evicted"
    assert cache.is_missing(f"b{_MISSING_MAX + 99}"), "newest entry must survive"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
