"""Unit tests for the sync ledger (api/storage/ledger.py).

Exercises the revision change log the cursor-based delta protocol is
built on: initial assignment, steady-state no-op walks, tombstones,
metadata-change rev bumps, batched delta reads, and persistence
across reopen.
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import os
import sys

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from providers.base import BookMeta
from storage.ledger import SyncLedger


def _meta(book_id, title="T", fmt="epub", size=10, series=None):
    return BookMeta(
        id=book_id,
        title=title,
        authors=["A"],
        series=series,
        series_id=("ser_" + series) if series else None,
        series_index=1.0 if series else None,
        file_format=fmt,
        file_size=size,
        added_at="2026-01-01T00:00:00Z",
        updated_at="2026-01-01T00:00:00Z",
    )


class FakeProvider:
    """Minimal provider double whose catalogue is a plain list."""

    name = "fake"

    def __init__(self, metas):
        self.metas = list(metas)
        self.calls = 0

    def list_books(self, *, mode="all", limit=500, offset=0, **_kw):
        self.calls += 1
        return self.metas[offset : offset + limit]


@pytest.fixture
def ledger(tmp_path):
    led = SyncLedger(str(tmp_path / "ledger.db"))
    yield led
    led.close()


def test_initial_walk_assigns_revs(ledger):
    prov = FakeProvider([_meta("a"), _meta("b"), _meta("c")])
    assert ledger.refresh(prov, max_age_s=0) is True
    assert ledger.count() == 3
    assert ledger.cursor() == 3


def test_steady_state_walk_touches_nothing(ledger):
    prov = FakeProvider([_meta("a"), _meta("b")])
    ledger.refresh(prov, max_age_s=0)
    cursor_before = ledger.cursor()
    assert ledger.refresh(prov, max_age_s=0) is True
    assert ledger.cursor() == cursor_before
    assert ledger.count() == 2


def test_rate_limit_skips_walk(ledger):
    prov = FakeProvider([_meta("a")])
    assert ledger.refresh(prov, max_age_s=30) is True
    calls = prov.calls
    assert ledger.refresh(prov, max_age_s=30) is False
    assert prov.calls == calls  # second refresh never hit the provider


def test_delta_batches_and_more_flag(ledger):
    prov = FakeProvider([_meta(f"b{i:03d}") for i in range(10)])
    ledger.refresh(prov, max_age_s=0)

    entries, more = ledger.delta(0, 4)
    assert [e.rev for e in entries] == [1, 2, 3, 4]
    assert more is True

    entries, more = ledger.delta(4, 10)
    assert len(entries) == 6
    assert more is False

    entries, more = ledger.delta(10, 4)
    assert entries == []
    assert more is False


def test_removal_tombstones_and_reports_removed(ledger):
    prov = FakeProvider([_meta("a"), _meta("b"), _meta("c")])
    ledger.refresh(prov, max_age_s=0)
    # Device consumes everything.
    entries, _ = ledger.delta(0, 100)
    cursor = entries[-1].rev
    # Provider drops "b".
    prov.metas = [_meta("a"), _meta("c")]
    ledger.refresh(prov, max_age_s=0)
    assert ledger.count() == 2

    entries, more = ledger.delta(cursor, 100)
    assert more is False
    assert len(entries) == 1
    assert entries[0].book_id == "b"
    assert entries[0].added_at is None  # tombstone


def test_metadata_change_bumps_rev(ledger):
    prov = FakeProvider([_meta("a", title="Old")])
    ledger.refresh(prov, max_age_s=0)
    cursor = ledger.cursor()

    prov.metas = [_meta("a", title="New")]
    ledger.refresh(prov, max_age_s=0)

    entries, more = ledger.delta(cursor, 10)
    assert more is False
    assert len(entries) == 1
    assert entries[0].book_id == "a"
    assert entries[0].title == "New"
    assert entries[0].added_at is not None  # update, not tombstone


def test_resurrected_book_is_added_again(ledger):
    prov = FakeProvider([_meta("a"), _meta("b")])
    ledger.refresh(prov, max_age_s=0)
    prov.metas = [_meta("a")]
    ledger.refresh(prov, max_age_s=0)
    cursor = ledger.cursor()  # device saw the tombstone up to here

    prov.metas = [_meta("a"), _meta("b")]
    ledger.refresh(prov, max_age_s=0)
    entries, _ = ledger.delta(cursor, 10)
    assert [e.book_id for e in entries] == ["b"]
    assert entries[0].added_at is not None


def test_persistence_across_reopen(tmp_path):
    path = str(tmp_path / "ledger.db")
    led = SyncLedger(path)
    led.refresh(FakeProvider([_meta("a"), _meta("b")]), max_age_s=0)
    cursor = led.cursor()
    led.close()

    led2 = SyncLedger(path)
    try:
        # Revs must not restart at zero — device cursors depend on it.
        assert led2.cursor() == cursor == 2
        assert led2.count() == 2
        # New books continue the numbering.
        led2.refresh(FakeProvider([_meta("a"), _meta("b"), _meta("c")]), max_age_s=0)
        entries, _ = led2.delta(cursor, 10)
        assert [(e.book_id, e.rev) for e in entries] == [("c", 3)]
    finally:
        led2.close()


def test_large_walk_pages_the_provider(tmp_path):
    """A 5k-book catalogue must be walked page by page and land in the
    ledger completely (mirrors the 100k device scenario at 1/20 scale)."""
    metas = [_meta(f"b{i:05d}") for i in range(5000)]
    prov = FakeProvider(metas)
    led = SyncLedger(str(tmp_path / "big.db"))
    try:
        led.refresh(prov, max_age_s=0)
        assert led.count() == 5000
        assert led.cursor() == 5000
        # Provider was paged (SCAN_PAGE=2000 → 3 pages for 5k books).
        assert prov.calls == 3
        # Tail batch reads cleanly.
        entries, more = led.delta(4999, 10)
        assert len(entries) == 1 and more is False
    finally:
        led.close()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
