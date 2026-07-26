"""
storage/cover_cache.py — on-disk cache for processed covers.

For each book we keep two artefacts derived (once) from the provider's raw
cover bytes by :mod:`storage.cover_proc`:

  * ``<key>.png``  — the small resized cover the device fetches and blits.
  * ``<key>.hash`` — the BlurHash string, also embedded in the sync
    metadata so the device can draw a placeholder with zero network cost.

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
    """Disk-backed cache for processed cover PNGs + blurhash strings."""

    def __init__(
        self,
        directory: str = "/tmp/pbemu-covers",
        max_age_seconds: int = 7 * 24 * 3600,
    ) -> None:
        self.directory = directory
        try:
            self.max_age = int(max_age_seconds)
        except (TypeError, ValueError):
            self.max_age = 7 * 24 * 3600
        with contextlib.suppress(OSError):
            os.makedirs(self.directory, exist_ok=True)

    # --- key handling ----------------------------------------------------

    @staticmethod
    def _key(book_id: str) -> str:
        return hashlib.sha256(book_id.encode("utf-8")).hexdigest()

    def png_path(self, book_id: str) -> str:
        return os.path.join(self.directory, self._key(book_id) + ".png")

    def _hash_path(self, book_id: str) -> str:
        return os.path.join(self.directory, self._key(book_id) + ".hash")

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
        """True when both the PNG and the blurhash are cached & fresh."""
        return self.has_png(book_id) and self.get_blurhash(book_id) is not None

    # --- reads -----------------------------------------------------------

    def get_blurhash(self, book_id: str) -> Optional[str]:
        path = self._hash_path(book_id)
        if not self._fresh(path):
            return None
        try:
            with open(path, encoding="utf-8") as f:
                s = f.read().strip()
        except OSError:
            return None
        return s or None

    def read_png(self, book_id: str) -> Optional[bytes]:
        path = self.png_path(book_id)
        if not self._fresh(path):
            return None
        try:
            with open(path, "rb") as f:
                return f.read()
        except OSError:
            return None

    # --- writes ----------------------------------------------------------

    @staticmethod
    def _atomic(path: str, data: bytes) -> None:
        tmp = path + ".tmp"
        try:
            with open(tmp, "wb") as f:
                f.write(data)
            os.replace(tmp, path)
        except OSError as exc:
            sys.stderr.write(f"cover_cache: write failed {path}: {exc}\n")
            with contextlib.suppress(OSError):
                if os.path.exists(tmp):
                    os.unlink(tmp)

    def store_png(self, book_id: str, png: bytes) -> None:
        self._atomic(self.png_path(book_id), png)

    def store_blurhash(self, book_id: str, bhash: str) -> None:
        self._atomic(self._hash_path(book_id), bhash.encode("utf-8"))

    def process_and_store(self, book_id: str, raw: bytes) -> Optional[str]:
        """Decode `raw`, cache the resized PNG + blurhash, return blurhash.

        Returns None (and caches nothing) if the bytes are not a decodable
        image; the caller then serves a 1x1 placeholder and an empty hash.
        """
        # Imported lazily so a missing Pillow never breaks cache reads.
        from storage import cover_proc

        result = cover_proc.process(raw)
        if result is None:
            return None
        png, bhash = result
        self.store_png(book_id, png)
        if bhash:
            self.store_blurhash(book_id, bhash)
        return bhash or None

    def purge(self, book_id: str) -> None:
        for path in (self.png_path(book_id), self._hash_path(book_id)):
            with contextlib.suppress(OSError):
                os.unlink(path)
