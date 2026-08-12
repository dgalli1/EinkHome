"""
storage/cover_cache.py — on-disk cache for processed covers.

For each book we keep the small resized cover PNG derived (once) from the
provider's raw cover bytes by :mod:`storage.cover_proc`:

  * ``<key>.png``  — the small resized cover the device fetches and blits.

``<key>`` is ``sha256(book_id)`` so ids with slashes/colons are safe as
filenames.  Writes go through a ``.tmp`` + ``os.replace`` so a reader
never sees a half-written file.
"""

from __future__ import annotations

import contextlib
import hashlib
import os
import sys
import threading
import time
from collections import OrderedDict
from typing import Optional

from storage.placeholder import PLACEHOLDER_PNG

# Negative-cache bounds: a book whose cover fetch failed (provider miss
# or undecodable bytes) is remembered as missing for at most _MISSING_TTL
# seconds, and the in-memory table never holds more than _MISSING_MAX
# ids (oldest evicted first).  This stops 100k-catalogue warm-ups from
# hammering the provider every pass for books that provably have no cover.
_MISSING_MAX = 10000
_MISSING_TTL = 3600.0


class CoverCache:
    """Disk-backed cache for processed cover PNGs."""

    def __init__(
        self,
        directory: str,
        max_age_seconds: float = 7 * 24 * 3600,
        create_dir: bool = True,
    ) -> None:
        self.directory = directory
        self.max_age = max_age_seconds
        if create_dir:
            os.makedirs(self.directory, exist_ok=True)
        self._missing: OrderedDict[str, float] = OrderedDict()
        self._missing_lock = threading.Lock()
        self._sweep_stale_tmp()

    def _sweep_stale_tmp(self) -> None:
        """Remove orphaned ``.tmp`` write leftovers older than an hour.

        ``_atomic`` cleans up after itself, but a crash between writing
        the tmp file and ``os.replace`` leaks one; sweep them at startup
        so they never accumulate."""
        cutoff = time.time() - 3600.0
        try:
            names = os.listdir(self.directory)
        except OSError:
            return
        for name in names:
            if not name.endswith(".tmp"):
                continue
            path = os.path.join(self.directory, name)
            try:
                if os.path.getmtime(path) < cutoff:
                    os.unlink(path)
            except OSError:
                continue

    # --- key handling ----------------------------------------------------

    @staticmethod
    def _key(book_id: str) -> str:
        return hashlib.sha256(book_id.encode("utf-8")).hexdigest()

    def png_path(self, book_id: str) -> str:
        return os.path.join(self.directory, self._key(book_id) + ".png")

    def etag_for(self, book_id: str) -> str:
        return self._key(book_id)[:16]

    # --- freshness -------------------------------------------------------

    def _fresh(self, path: str) -> bool:
        if not os.path.isfile(path):
            return False
        try:
            mtime = os.path.getmtime(path)
        except OSError:
            return False
        now = time.time()
        if mtime > now + 60.0:
            # Clock skew / restored filesystem: a future mtime must not
            # count as "too fresh to be stale" forever — clamp to now.
            mtime = now
        return (now - mtime) < self.max_age

    def has_png(self, book_id: str) -> bool:
        return self._fresh(self.png_path(book_id))

    # --- reads -----------------------------------------------------------

    def read_png(self, book_id: str) -> Optional[bytes]:
        path = self.png_path(book_id)
        if not self._fresh(path):
            return None
        try:
            with open(path, "rb") as fh:
                return fh.read()
        except OSError:
            return None

    # --- negative cache --------------------------------------------------

    def is_missing(self, book_id: str) -> bool:
        """True if ``book_id`` was recently seen without a usable cover.

        Entries older than ``_MISSING_TTL`` are dropped on access (the
        book becomes fetchable again); fresh entries are refreshed to
        the LRU end so a hot set never gets evicted.
        """
        now = time.time()
        with self._missing_lock:
            stamp = self._missing.get(book_id)
            if stamp is None:
                return False
            if now - stamp > _MISSING_TTL:
                self._missing.pop(book_id, None)
                return False
            self._missing.move_to_end(book_id)
            return True

    def mark_missing(self, book_id: str) -> None:
        """Record that ``book_id`` has no usable cover (TTL-bounded)."""
        now = time.time()
        with self._missing_lock:
            self._missing[book_id] = now
            self._missing.move_to_end(book_id)
            while len(self._missing) > _MISSING_MAX:
                self._missing.popitem(last=False)

    # --- writes ----------------------------------------------------------

    @staticmethod
    def _atomic(path: str, data: bytes) -> None:
        # Unique tmp per writer (pid + thread id): the cover warmer and
        # request handlers can race on the same key, and a shared fixed
        # name would let interleaved writes corrupt each other before
        # os.replace makes the final file atomic.  The tmp file is
        # flushed but deliberately NOT fsynced, then renamed into place;
        # os.replace already gives rename atomicity, and skipping the
        # per-write fsync keeps 100k warm-up writes off slow flash.  A
        # crash before the rename only loses a re-fetchable PNG, and the
        # startup sweep cleans up any leaked tmp.  The parent-directory
        # fsync is likewise skipped for the same reason.
        tmp = f"{path}.{os.getpid()}.{threading.get_ident()}.tmp"
        try:
            with open(tmp, "wb") as fh:
                fh.write(data)
                fh.flush()
            os.replace(tmp, path)
        except OSError as exc:
            sys.stderr.write(f"cover_cache: write failed {path}: {exc}\n")
            with contextlib.suppress(OSError):
                if os.path.exists(tmp):
                    os.unlink(tmp)

    def store_png(self, book_id: str, png: bytes) -> None:
        self._atomic(self.png_path(book_id), png)

    def process_and_store(self, book_id: str, raw: bytes) -> Optional[bytes]:
        """Decode `raw`, cache the resized PNG, return the PNG bytes.

        Returns None (and caches nothing) if the bytes are not a decodable
        image; the caller then serves a 1x1 placeholder.  Placeholder
        sources are returned as-is without touching the disk — a provider
        without real covers (e.g. mock) must not fill the cache with
        thousands of identical 1x1 PNGs.
        """
        if raw == PLACEHOLDER_PNG:
            return PLACEHOLDER_PNG
        # Imported lazily so a missing Pillow never breaks cache reads.
        from storage import cover_proc

        png = cover_proc.process(raw)
        if png is None:
            return None
        if png == PLACEHOLDER_PNG:
            return png  # decoded to the placeholder: serve, don't store
        self.store_png(book_id, png)
        return png
