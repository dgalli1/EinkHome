"""Unit tests for the sync ledger (api/storage/ledger.py).

Exercises the revision change log the cursor-based delta protocol is
built on: initial assignment, steady-state no-op walks, tombstones,
metadata-change rev bumps, batched delta reads, and persistence
across reopen.
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import os
import sys
import threading
import time

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

    def walk_books(self, *, mode="all", chunk_size=500):
        """Mirror providers.base.Provider.walk_books (offset paging,
        stop on an empty or short chunk, never yield empty chunks)."""
        offset = 0
        while True:
            chunk = self.list_books(mode=mode, limit=chunk_size, offset=offset)
            if not chunk:
                return
            yield chunk
            if len(chunk) < chunk_size:
                return
            offset += len(chunk)


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


def test_record_device_and_min_device_rev(ledger):
    """record_device persists per-device cursors; the minimum bounds
    tombstone compaction."""
    assert ledger.min_device_rev() is None  # no device has reported yet
    ledger.record_device("dev-a", 42)
    assert ledger.min_device_rev() == 42
    ledger.record_device("dev-b", 10)
    assert ledger.min_device_rev() == 10
    # Updating a cursor can only lower (or keep) the minimum.
    ledger.record_device("dev-a", 7)
    assert ledger.min_device_rev() == 7
    ledger.record_device("dev-b", 100)
    assert ledger.min_device_rev() == 7


def test_compact_tombstones_deletes_only_consumed_tombstones(ledger):
    prov = FakeProvider([_meta("a"), _meta("b"), _meta("c")])
    ledger.refresh(prov, max_age_s=0)
    ledger.delta(0, 100)  # a device consumes revs 1..3

    # Drop "a" and "b" -> tombstones at revs 4 and 5; "c" stays live.
    prov.metas = [_meta("c")]
    ledger.refresh(prov, max_age_s=0)
    assert ledger.count() == 1

    # No device has reported a cursor: compaction must delete nothing —
    # every tombstone may still need to be replayed.
    assert ledger.compact_tombstones() == 0
    entries, _ = ledger.delta(0, 100)
    assert [e.book_id for e in entries if e.added_at is None] == ["a", "b"]

    # A device that consumed rev 5 has seen "a" (rev 4); the "b"
    # tombstone (rev 5) is still replayable.  Compaction drops only the
    # consumed tombstone and never touches live rows.
    ledger.record_device("dev1", 5)
    assert ledger.min_device_rev() == 5
    assert ledger.compact_tombstones() == 1
    assert ledger.count() == 1
    entries, _ = ledger.delta(3, 10)
    assert [(e.book_id, e.added_at) for e in entries] == [("b", None)]

    # Explicit min_rev: tombstones strictly below it go; at-or-above
    # survives; live rows are never touched.
    assert ledger.compact_tombstones(min_rev=5) == 0
    assert ledger.compact_tombstones(min_rev=6) == 1  # "b"@5
    assert ledger.count() == 1
    entries, _ = ledger.delta(0, 100)
    assert [e.book_id for e in entries] == ["c"]
    assert entries[0].added_at is not None


def test_empty_catalogue_refusal_unless_acknowledged(tmp_path):
    """An empty provider walk must never tombstone a populated ledger —
    unless the operator opted into the wipe via ack_empty_catalogue."""
    led = SyncLedger(str(tmp_path / "guard.db"))
    try:
        led.refresh(FakeProvider([_meta("a"), _meta("b")]), max_age_s=0)
        with pytest.raises(RuntimeError):
            led.refresh(FakeProvider([]), max_age_s=0)
        assert led.count() == 2  # nothing was tombstoned
    finally:
        led.close()

    led2 = SyncLedger(str(tmp_path / "ack.db"), ack_empty_catalogue=True)
    try:
        led2.refresh(FakeProvider([_meta("a"), _meta("b")]), max_age_s=0)
        assert led2.refresh(FakeProvider([]), max_age_s=0) is True
        assert led2.count() == 0
        entries, _ = led2.delta(0, 10)
        assert [e.book_id for e in entries] == ["a", "b"]
        assert all(e.added_at is None for e in entries)  # tombstones
    finally:
        led2.close()


def test_delta_during_refresh_smoke(ledger):
    """A delta read racing an in-flight refresh must never raise."""
    walked = threading.Event()

    class SlowProvider(FakeProvider):
        def list_books(self, *, mode="all", limit=500, offset=0, **_kw):
            if offset == 0:
                walked.set()
                time.sleep(0.05)
            return super().list_books(mode=mode, limit=limit, offset=offset)

    metas = [_meta(f"b{i:03d}") for i in range(5000)]
    prov = SlowProvider(metas)
    ledger.refresh(prov, max_age_s=0)  # initial fill (warms the walk)
    walked.clear()

    t = threading.Thread(
        target=ledger.refresh, args=(prov,), kwargs={"max_age_s": 0}
    )
    t.start()
    assert walked.wait(2.0), "refresh thread never entered the provider walk"
    # The refresh holds the write lock; delta serialises behind it.
    entries, more = ledger.delta(0, 100)
    assert len(entries) == 100
    assert isinstance(more, bool)
    t.join(timeout=10)
    assert not t.is_alive(), "refresh thread hung"
    assert ledger.count() == 5000  # post-walk state fully visible


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
