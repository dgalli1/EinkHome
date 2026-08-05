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
import time
from typing import Optional


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
        return (time.time() - mtime) < self.max_age

    def has_png(self, book_id: str) -> bool:
        return self._fresh(self.png_path(book_id))

    def is_ready(self, book_id: str) -> bool:
        """True when the cover PNG is cached & fresh."""
        return self.has_png(book_id)

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

    # --- writes ----------------------------------------------------------

    @staticmethod
    def _atomic(path: str, data: bytes) -> None:
        tmp = path + ".tmp"
        try:
            with open(tmp, "wb") as fh:
                fh.write(data)
                fh.flush()
                os.fsync(fh.fileno())
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
        image; the caller then serves a 1x1 placeholder.
        """
        # Imported lazily so a missing Pillow never breaks cache reads.
        from storage import cover_proc

        png = cover_proc.process(raw)
        if png is None:
            return None
        self.store_png(book_id, png)
        return png

    def purge(self, book_id: str) -> None:
        with contextlib.suppress(OSError):
            os.unlink(self.png_path(book_id))
