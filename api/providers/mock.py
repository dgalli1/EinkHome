"""
providers/mock.py — a stand-in provider for offline development.

Reads the same `U633_6.8.2817/.live/mnt/ext1/books` directory the
firmware stages user books into and exposes every file as a single
fake "book". Useful for:
  - running the API server without a real Kavita instance
  - running the in-emulator app without internet
  - CI / smoke tests

Synthetic scale mode: ``count`` in the provider config generates that
many books (with stable, derived metadata and no files on disk) on top
of whatever the books dir holds.  Used to exercise the device app and
the delta protocol at 100k entries without materialising a library.
"""

from __future__ import annotations

import hashlib
import os
import time
from collections.abc import Iterator
from contextlib import suppress
from typing import Any

from .base import (
    AuthorInfo,
    BookMeta,
    LibraryInfo,
    Provider,
    SeriesInfo,
)

# Deterministic vocabulary for synthetic titles/authors.  Indexing by
# (i mod len) keeps every synthetic book reproducible across runs.
_SYN_SERIES = ("Orbit", "Quartz", "Lumen", "Fathom", "Cinder", "Vale")
_SYN_AUTHORS = (
    "Ada Quill",
    "Bram Hallow",
    "Cora Voss",
    "Dane Pryce",
    "Edda Marn",
    "Finn Ocker",
)
_SYN_FMTS = ("epub", "epub", "epub", "pdf", "fb2")
# Fixed epoch for synthetic timestamps so the whole library has a stable
# added_at ordering (i-based) independent of the server's wall clock.
_SYN_EPOCH = 1_700_000_000.0


class MockProvider(Provider):
    name = "mock"

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        # Default to the pbemu books dir
        self.books_dir = cfg.get(
            "books_dir",
            os.path.join(
                os.path.dirname(os.path.dirname(os.path.dirname(__file__))),
                "..",
                "U633_6.8.2817",
                ".live",
                "mnt",
                "ext1",
                "books",
            ),
        )
        self.library_name = cfg.get("library_name", "pbemu demo library")
        # Number of synthetic books layered over the books-dir scan
        # (0 = off).  Synthetic ids/metadata are fully deterministic so
        # a given config always describes the same library.
        try:
            self.synthetic_count = int(cfg.get("count") or 0)
        except (TypeError, ValueError):
            self.synthetic_count = 0
        # Books per synthetic series.  Every `synthetic_series_size`-th
        # book joins a series (so series collapse has something to chew
        # on); the remainder stay standalone.
        try:
            self.synthetic_series_size = int(cfg.get("series_size") or 5)
        except (TypeError, ValueError):
            self.synthetic_series_size = 5
        self.synthetic_series_size = max(2, self.synthetic_series_size)
        # Stable in-memory book id cache
        self._id_cache: dict[str, str] = {}
        self._rev_cache: dict[str, dict[str, Any]] = {}

    # --- helpers -----------------------------------------------------------

    def _book_id(self, abs_path: str) -> str:
        cached = self._id_cache.get(abs_path)
        if cached:
            return cached
        s = "mock_" + hashlib.sha1(abs_path.encode("utf-8")).hexdigest()[:16]
        self._id_cache[abs_path] = s
        return s

    def _scan(self) -> list[dict[str, Any]]:
        if not os.path.isdir(self.books_dir):
            return []
        out: list[dict[str, Any]] = []
        try:
            entries = os.listdir(self.books_dir)
        except OSError:
            return []
        for path in sorted(entries):
            full = os.path.join(self.books_dir, path)
            if not os.path.isfile(full):
                continue
            if not path.lower().endswith(
                (".epub", ".pdf", ".fb2", ".djvu", ".txt", ".cbz", ".cbr")
            ):
                continue
            try:
                st = os.stat(full)
            except OSError:
                continue
            ext = path.rsplit(".", 1)[-1].lower() if "." in path else "epub"
            out.append(
                {
                    "abs": full,
                    "name": path,
                    "ext": ext,
                    "size": st.st_size,
                    "mtime": st.st_mtime,
                }
            )
        return out

    # --- synthetic books ----------------------------------------------------

    def _syn_id(self, i: int) -> str:
        return f"syn_{i:07d}"

    def _syn_index(self, book_id: str) -> int | None:
        """Reverse-map a synthetic id back onto its sequence number."""
        if not book_id.startswith("syn_"):
            return None
        try:
            i = int(book_id[4:])
        except ValueError:
            return None
        return i if 0 <= i < self.synthetic_count else None

    def _synthetic(self, i: int) -> BookMeta:
        """Metadata for synthetic book #i — pure arithmetic, O(1)."""
        book_id = self._syn_id(i)
        fmt = _SYN_FMTS[i % len(_SYN_FMTS)]
        author = _SYN_AUTHORS[i % len(_SYN_AUTHORS)]
        series_name: str | None = None
        series_id: str | None = None
        series_index: float | None = None
        if i % self.synthetic_series_size != 0:
            # Members 1..size-1 of each block join the block's series.
            block = i // self.synthetic_series_size
            name = f"{_SYN_SERIES[block % len(_SYN_SERIES)]} {block:04d}"
            series_name = name
            series_id = "syn_ser_" + hashlib.sha1(name.encode()).hexdigest()[:12]
            series_index = float(i % self.synthetic_series_size)
        title = f"Synthetic Book {i:07d}"
        ts = _iso(_SYN_EPOCH + i)
        return BookMeta(
            id=book_id,
            title=title,
            authors=[author],
            series=series_name,
            series_id=series_id,
            series_index=series_index,
            summary=f"Synthetic mock book #{i}",
            language=None,
            file_format=fmt,
            file_size=10_000 + (i % 900_000),
            page_count=0,
            cover_url=f"/api/v1/books/{book_id}/cover",
            download_url=f"/api/v1/books/{book_id}/file",
            added_at=ts,
            updated_at=ts,
            remote_only=True,
            extra={"synthetic": True, "index": i},
        )

    def _all(self, series_id: str | None, search: str | None, since: str | None):
        """Yield every live BookMeta (dir books first, then synthetic),
        applying the cheap provider-side filters.  Generator so callers
        slice without materialising the whole library."""
        for entry in self._scan():
            meta = self._book_from_path(entry)
            if series_id and meta.series_id != series_id:
                continue
            if search and search.lower() not in meta.title.lower():
                continue
            if since and meta.updated_at and meta.updated_at <= since:
                continue
            yield meta
        if search:
            q = search.lower()
        else:
            q = None
        for i in range(self.synthetic_count):
            meta = self._synthetic(i)
            if series_id and meta.series_id != series_id:
                continue
            if q is not None and q not in meta.title.lower():
                continue
            if since and meta.updated_at and meta.updated_at <= since:
                continue
            yield meta

    def _book_from_path(self, entry: dict[str, Any]) -> BookMeta:
        book_id = self._book_id(entry["abs"])
        ext = entry["ext"]
        stem = os.path.splitext(entry["name"])[0]
        title = stem.replace("_", " ").strip() or entry["name"]

        # Series convention: "Series Name - 03" → series="Series Name",
        # series_index=3, series_id=stable hash.  Plain "book_NNN" names
        # stay standalone (series=None) so existing tests are unaffected.
        series_name: str | None = None
        series_id: str | None = None
        series_index: float | None = None
        dash_pos = stem.rfind(" - ")
        if dash_pos > 0:
            tail = stem[dash_pos + 3 :].strip()
            if tail.isdigit():
                series_name = stem[:dash_pos].replace("_", " ").strip()
                series_index = float(tail)
                series_id = (
                    "mock_ser_"
                    + hashlib.sha1(series_name.encode()).hexdigest()[:12]
                )

        return BookMeta(
            id=book_id,
            title=title,
            authors=["pbemu mock library"],
            series=series_name,
            series_id=series_id,
            series_index=series_index,
            summary=f"Mock book from {entry['abs']}",
            language=None,
            file_format=ext,
            file_size=entry["size"],
            page_count=0,
            cover_url=f"/api/v1/books/{book_id}/cover",
            download_url=f"/api/v1/books/{book_id}/file",
            added_at=_iso(entry["mtime"]),
            updated_at=_iso(entry["mtime"]),
            remote_only=True,
            extra={"abs_path": entry["abs"]},
        )

    # --- Provider interface -----------------------------------------------

    def health(self) -> dict[str, Any]:
        total = len(self._scan()) + self.synthetic_count
        return {
            "ok": True,
            "detail": f"mock: {total} books ({self.synthetic_count} synthetic)",
        }

    def list_libraries(self) -> list[LibraryInfo]:
        return [
            LibraryInfo(
                id="mock_lib",
                name=self.library_name,
                book_count=len(self._scan()) + self.synthetic_count,
                kind="library",
            )
        ]

    def list_series(self, library_id: str) -> list[SeriesInfo]:
        seen: dict[str, SeriesInfo] = {}
        for meta in self._all(None, None, None):
            if meta.series_id and meta.series_id not in seen:
                seen[meta.series_id] = SeriesInfo(
                    id=meta.series_id,
                    name=meta.series or "Unknown",
                    library_id=library_id,
                    book_count=0,
                )
            if meta.series_id and meta.series_id in seen:
                seen[meta.series_id].book_count += 1
        return list(seen.values())

    def list_authors(self, library_id: str | None = None) -> list[AuthorInfo]:
        return []

    def list_books(
        self,
        *,
        mode: str = "all",
        library_id: str | None = None,
        series_id: str | None = None,
        author_id: str | None = None,
        search: str | None = None,
        limit: int = 500,
        offset: int = 0,
        since: str | None = None,
    ) -> list[BookMeta]:
        out: list[BookMeta] = []
        skipped = 0
        for meta in self._all(series_id, search, since):
            if skipped < offset:
                skipped += 1
                continue
            out.append(meta)
            if len(out) >= limit:
                break
        return out

    def get_book(self, book_id: str) -> BookMeta | None:
        idx = self._syn_index(book_id)
        if idx is not None:
            return self._synthetic(idx)
        for entry in self._scan():
            if self._book_id(entry["abs"]) == book_id:
                return self._book_from_path(entry)
        return None

    def get_cover(self, book_id: str) -> bytes | None:
        # No real covers in mock mode — return a 1x1 placeholder.
        return b"\x89PNG\r\n\x1a\n" + _TINY_PNG

    def open_file(self, book_id: str) -> tuple[str, Iterator[bytes]] | None:
        idx = self._syn_index(book_id)
        if idx is not None:
            meta = self._synthetic(idx)
            return f"{meta.title}.{meta.file_format}", _synthetic_bytes(idx)
        for entry in self._scan():
            if self._book_id(entry["abs"]) == book_id:
                return entry["name"], _file_iter(entry["abs"])
        return None


_TINY_PNG = bytes.fromhex(
    "0000000d4948445200000001000000010802000000907753"
    "de0000000c49444154789c636868680000030401814bd3d2"
    "100000000049454e44ae426082"
)


def _iso(t: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def _file_iter(path: str) -> Iterator[bytes]:
    try:
        f = open(path, "rb")  # noqa: SIM115
    except OSError:
        return
    try:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            yield chunk
    finally:
        with suppress(OSError):
            f.close()


def _synthetic_bytes(i: int) -> Iterator[bytes]:
    """Tiny deterministic payload for synthetic book downloads — big
    enough to look like a file, small enough to stream instantly."""
    header = f"SYNTHETIC BOOK #{i}\n".encode()
    yield header
    yield b"\x00" * max(0, 4096 - len(header))
