"""
api/storage/ledger.py — server-side sync ledger for cursor-based delta sync.

The bookshelf device app can hold 100k+ books, so the old protocol
(device posts every known id, server set-diffs) does not scale: the
request body alone would be megabytes and the diff is O(N) per call.

This module replaces it with a *monotonic revision change log*:

* Every book the provider serves carries a revision number.  Any state
  change — new book, metadata change, removal — bumps the book's rev to
  the next free number, so a single ``rev > cursor`` scan surfaces
  additions, updates AND removals in one ordered stream.
* ``refresh()`` walks ``provider.list_books`` page by page and folds
  the current catalogue into the ledger.  A short fingerprint over the
  render-relevant fields detects changes; steady-state walks touch no
  rows.  Walks are rate-limited by ``max_age_s`` so a busy device only
  triggers a provider scan at most once per interval.
* Removals become *tombstones*: the row keeps its id, ``added_at``
  flips to NULL and the rev is bumped.  Devices replay the rev and
  delete their local row.  A returning book resurrects the row the
  same way (bumped rev + restored metadata).
* ``delta(cursor, limit)`` returns bounded batches straight out of
  SQLite — the delta endpoint never touches the provider, so it stays
  fast even while the upstream is slow or down.

The device stores only the last cursor it consumed (one integer) and
replays from there; no id lists ever cross the wire.

The ledger is a small SQLite file (stdlib sqlite3).  It survives
server restarts, which matters because device cursors point at revs
assigned by previous runs; re-assigning from zero would corrupt the
per-device replay.

Schema:

    books(id TEXT PRIMARY KEY, rev INTEGER UNIQUE, added_at TEXT,
          title TEXT, authors TEXT, series TEXT, series_id TEXT,
          series_idx REAL, format TEXT, size INTEGER, fp TEXT)
        rev       — current revision; bumped on every change
        added_at  — ISO timestamp first seen; NULL = tombstone
        authors   — JSON array of names
        fp        — fingerprint of the metadata columns (change detect)

    state(key TEXT PRIMARY KEY, value INTEGER)
        next_rev   — next revision to hand out (starts at 1)
        last_walk  — unix time the last full provider walk finished

    device_cursors(device_id TEXT PRIMARY KEY, last_rev INTEGER NOT NULL,
                   updated_at TEXT NOT NULL)
        last_rev   — the highest rev that device has consumed.  The
                     minimum across devices bounds tombstone compaction:
                     no tombstone below it can ever be replayed.
"""

from __future__ import annotations

import hashlib
import json
import sqlite3
import threading
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from providers.base import BookMeta


# Page size when walking the provider.  Small enough to keep one
# list_books call's result tiny, large enough that a 100k catch-up is
# only ~50 round trips into the provider.
SCAN_PAGE = 2000

# Tombstones below every device's cursor are unreadable dead rows.
# Compact the pile once it grows past this many (checked once per walk).
TOMBSTONE_COMPACT_THRESHOLD = 5000


@dataclass(frozen=True)
class LedgerEntry:
    """One change a device must apply, in rev order."""

    rev: int
    book_id: str
    added_at: str | None  # None → tombstone; device deletes the row
    title: str
    authors: str  # JSON array
    series: str | None
    series_id: str | None
    series_idx: float | None
    format: str
    size: int
    file_name: str | None = None


def fingerprint(meta: BookMeta) -> str:
    """Cheap content hash over the fields the device renders.  Two
    metas with the same fingerprint are indistinguishable to the app,
    so a matching fp means the ledger row needs no update."""
    blob = "|".join(
        (
            meta.title or "",
            ",".join(meta.authors or []),
            meta.series or "",
            meta.series_id or "",
            repr(meta.series_index),
            meta.file_format or "",
            meta.file_name or "",
            str(meta.file_size),
            meta.added_at or "",
        )
    )
    return hashlib.sha1(blob.encode("utf-8")).hexdigest()


class SyncLedger:
    """SQLite-backed revision change log for one provider instance."""

    def __init__(self, path: str, *, ack_empty_catalogue: bool = False) -> None:
        self.con = sqlite3.connect(path, check_same_thread=False)
        self.con.execute("PRAGMA journal_mode=WAL")
        # Cross-process safety: two processes touching the same DB file
        # (e.g. a live server and a test harness) must wait out each
        # other's write locks instead of failing with SQLITE_BUSY.
        self.con.execute("PRAGMA busy_timeout=3000")
        self.con.execute(
            "CREATE TABLE IF NOT EXISTS books("
            "id TEXT PRIMARY KEY, rev INTEGER UNIQUE, added_at TEXT, "
            "title TEXT, authors TEXT, series TEXT, series_id TEXT, "
            "series_idx REAL, format TEXT, size INTEGER, fp TEXT, "
            "file_name TEXT)"
        )
        # Ledgers created before file_name existed get the column added;
        # the fingerprint change below then re-emits every row with it.
        try:
            self.con.execute("ALTER TABLE books ADD COLUMN file_name TEXT")
        except sqlite3.OperationalError:
            pass  # column already present
        self.con.execute(
            "CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY, value INTEGER)"
        )
        self.con.execute(
            "CREATE TABLE IF NOT EXISTS device_cursors("
            "device_id TEXT PRIMARY KEY, last_rev INTEGER NOT NULL, "
            "updated_at TEXT NOT NULL)"
        )
        self.con.execute(
            "INSERT OR IGNORE INTO state(key, value) VALUES ('next_rev', 1)"
        )
        self.con.execute(
            "INSERT OR IGNORE INTO state(key, value) VALUES ('last_walk', 0)"
        )
        self.con.commit()
        self._lock = threading.Lock()
        self._ack_empty_catalogue = ack_empty_catalogue

    # -- write side --------------------------------------------------------

    def refresh(self, provider: Any, max_age_s: float = 30.0) -> bool:
        """Fold the provider's current catalogue into the ledger —
        unless the last walk finished less than ``max_age_s`` ago, in
        which case this is a cheap no-op.  Returns True when a walk
        actually ran.

        A provider error mid-walk rolls the whole pass back (sqlite3
        leaves an implicit transaction open after DML) and re-raises,
        so a failed walk can never tombstone or partially update the
        ledger.  ``last_walk`` stays untouched, so the next refresh
        retries immediately and the server can keep serving the stale
        ledger."""
        with self._lock:
            last = self._state_get("last_walk")
            if time.time() - last < max_age_s:
                return False
            try:
                self._walk(provider)
            except Exception:
                self.con.rollback()
                raise
            # Bulk removals leave a pile of tombstones behind; once it
            # grows past the threshold, drop the rows every device has
            # already consumed (rev below the minimum device cursor).
            row = self.con.execute(
                "SELECT COUNT(*) FROM books WHERE added_at IS NULL"
            ).fetchone()
            if int(row[0]) > TOMBSTONE_COMPACT_THRESHOLD:
                self._compact_tombstones()
            self._state_set("last_walk", int(time.time()))
            self.con.commit()
            return True

    def _walk(self, provider: Any) -> None:
        """One full catalogue pass.  Only one provider page and its
        diff are materialised at a time; the persistent O(N) structure
        is the (id → fp, added_at) index, which lives server-side."""
        stored: dict[str, tuple[str, str | None]] = {
            r[0]: (r[1], r[2])
            for r in self.con.execute("SELECT id, fp, added_at FROM books")
        }
        seen: set[str] = set()
        first_page = True
        for batch in provider.walk_books(mode="all", chunk_size=SCAN_PAGE):
            if first_page:
                first_page = False
                # An empty first page while the ledger holds rows means
                # the provider error path collapsed the whole catalogue
                # to nothing — tombstones would wipe the library.  Refuse.
                if not batch and stored and not self._ack_empty_catalogue:
                    raise RuntimeError(
                        "provider returned an empty catalogue with "
                        f"{len(stored)} rows in the ledger; refusing to tombstone"
                    )
            if not batch:
                continue  # defensive: walkers must not yield empty pages
            inserts: list[BookMeta] = []
            updates: list[BookMeta] = []
            for meta in batch:
                seen.add(meta.id)
                fp = fingerprint(meta)
                row = stored.get(meta.id)
                if row is None:
                    inserts.append(meta)
                    stored[meta.id] = (fp, meta.added_at)
                elif row[0] != fp or row[1] is None:
                    # Metadata changed, or a tombstone resurrecting.
                    updates.append(meta)
                    stored[meta.id] = (fp, meta.added_at)
            self._apply_inserts(inserts)
            self._apply_updates(updates)
        if first_page and stored and not self._ack_empty_catalogue:
            # walk_books yields no pages for an empty catalogue — the
            # same collapsed-provider refusal as an empty first page.
            raise RuntimeError(
                "provider returned an empty catalogue with "
                f"{len(stored)} rows in the ledger; refusing to tombstone"
            )

        gone = [
            bid
            for bid, (_, added_at) in stored.items()
            if added_at is not None and bid not in seen
        ]
        self._apply_tombstones(gone)

    def _apply_inserts(self, metas: list[BookMeta]) -> None:
        if not metas:
            return
        next_rev = self._state_get("next_rev")
        rows = []
        for meta in metas:
            rows.append(self._meta_row(meta, next_rev))
            next_rev += 1
        self.con.executemany(
            "INSERT OR IGNORE INTO books(id, rev, added_at, title, authors, "
            "series, series_id, series_idx, format, size, fp, file_name) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            rows,
        )
        self._state_set("next_rev", next_rev)

    def _apply_updates(self, metas: list[BookMeta]) -> None:
        if not metas:
            return
        next_rev = self._state_get("next_rev")
        for meta in metas:
            self.con.execute(
                "UPDATE books SET rev=?, added_at=?, title=?, authors=?, "
                "series=?, series_id=?, series_idx=?, format=?, size=?, fp=?, "
                "file_name=? WHERE id=?",
                (
                    next_rev,
                    meta.added_at,
                    meta.title,
                    json.dumps(list(meta.authors or [])),
                    meta.series,
                    meta.series_id,
                    meta.series_index,
                    meta.file_format,
                    meta.file_size,
                    fingerprint(meta),
                    meta.file_name,
                    meta.id,
                ),
            )
            next_rev += 1
        self._state_set("next_rev", next_rev)

    def _apply_tombstones(self, ids: list[str]) -> None:
        if not ids:
            return
        next_rev = self._state_get("next_rev")
        for bid in ids:
            self.con.execute(
                "UPDATE books SET rev=?, added_at=NULL WHERE id=?",
                (next_rev, bid),
            )
            next_rev += 1
        self._state_set("next_rev", next_rev)

    @staticmethod
    def _meta_row(meta: BookMeta, rev: int) -> tuple[Any, ...]:
        return (
            meta.id,
            rev,
            meta.added_at,
            meta.title,
            json.dumps(list(meta.authors or [])),
            meta.series,
            meta.series_id,
            meta.series_index,
            meta.file_format,
            meta.file_size,
            fingerprint(meta),
            meta.file_name,
        )

    def _state_get(self, key: str) -> int:
        row = self.con.execute(
            "SELECT value FROM state WHERE key = ?", (key,)
        ).fetchone()
        return int(row[0]) if row else 0

    def _state_set(self, key: str, value: int) -> None:
        self.con.execute(
            "INSERT OR REPLACE INTO state(key, value) VALUES (?, ?)",
            (key, value),
        )

    # -- read side ----------------------------------------------------------

    def delta(self, cursor: int, limit: int) -> tuple[list[LedgerEntry], bool]:
        """Up to ``limit`` entries with rev > cursor, ordered by rev,
        plus whether more entries remain beyond the batch."""
        with self._lock:
            rows = self.con.execute(
                "SELECT rev, id, added_at, title, authors, series, series_id, "
                "series_idx, format, size, file_name FROM books "
                "WHERE rev > ? ORDER BY rev LIMIT ?",
                (cursor, limit + 1),
            ).fetchall()
        more = len(rows) > limit
        entries = [
            LedgerEntry(
                rev=r[0],
                book_id=r[1],
                added_at=r[2],
                title=r[3] or "",
                authors=r[4] or "[]",
                series=r[5],
                series_id=r[6],
                series_idx=r[7],
                format=r[8] or "",
                size=r[9] or 0,
                file_name=r[10],
            )
            for r in rows[:limit]
        ]
        return entries, more

    def cursor(self) -> int:
        """Highest rev assigned so far (0 for an empty ledger)."""
        with self._lock:
            row = self.con.execute("SELECT MAX(rev) FROM books").fetchone()
        return int(row[0]) if row and row[0] is not None else 0

    def count(self) -> int:
        """Number of live (non-tombstone) books."""
        with self._lock:
            row = self.con.execute(
                "SELECT COUNT(*) FROM books WHERE added_at IS NOT NULL"
            ).fetchone()
        return int(row[0])

    def record_device(self, device_id: str, last_rev: int) -> None:
        """Persist a device's last consumed revision.  The minimum of
        these cursors bounds tombstone compaction: no tombstone below
        it can ever be replayed, so it is safe to delete."""
        with self._lock:
            self.con.execute(
                "INSERT OR REPLACE INTO device_cursors(device_id, last_rev, "
                "updated_at) VALUES (?, ?, ?)",
                (
                    device_id,
                    last_rev,
                    time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                ),
            )
            self.con.commit()

    def min_device_rev(self) -> int | None:
        """Smallest ``last_rev`` any device has reported, or None when
        no device has ever posted a cursor."""
        with self._lock:
            row = self.con.execute(
                "SELECT MIN(last_rev) FROM device_cursors"
            ).fetchone()
        return int(row[0]) if row and row[0] is not None else None

    def compact_tombstones(self, min_rev: int | None = None) -> int:
        """Delete tombstoned rows whose rev is strictly below ``min_rev``
        (when given) or below the smallest cursor any device reported
        (when omitted).  Live rows are never touched.  Returns the
        number of rows deleted."""
        with self._lock:
            deleted = self._compact_tombstones(min_rev)
            self.con.commit()
            return deleted

    def _compact_tombstones(self, min_rev: int | None = None) -> int:
        """Unlocked core of :meth:`compact_tombstones`; callers must
        hold ``self._lock`` (``refresh`` runs it inside its own walk
        transaction)."""
        cur = self.con.execute(
            "DELETE FROM books WHERE added_at IS NULL AND rev < COALESCE(?, "
            "(SELECT MIN(last_rev) FROM device_cursors))",
            (min_rev,),
        )
        return cur.rowcount

    def close(self) -> None:
        self.con.close()
