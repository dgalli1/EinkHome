"""
providers/mock.py — a stand-in provider for offline development.

Reads the same `U633_6.8.2817/.live/mnt/ext1/books` directory the
firmware stages user books into and exposes every file as a single
fake "book". Useful for:
  - running the API server without a real Kavita instance
  - running the in-emulator app without internet
  - CI / smoke tests
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
        return {
            "ok": True,
            "detail": f"mock: {len(self._scan())} books in {self.books_dir}",
        }

    def list_libraries(self) -> list[LibraryInfo]:
        return [
            LibraryInfo(
                id="mock_lib",
                name=self.library_name,
                book_count=len(self._scan()),
                kind="library",
            )
        ]

    def list_series(self, library_id: str) -> list[SeriesInfo]:
        seen: dict[str, SeriesInfo] = {}
        for entry in self._scan():
            meta = self._book_from_path(entry)
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
        for entry in self._scan():
            meta = self._book_from_path(entry)
            if series_id and meta.series_id != series_id:
                continue
            if search:
                q = search.lower()
                if q not in meta.title.lower():
                    continue
            if since and meta.updated_at and meta.updated_at <= since:
                continue
            out.append(meta)
        return out[offset : offset + limit]

    def get_book(self, book_id: str) -> BookMeta | None:
        for entry in self._scan():
            if self._book_id(entry["abs"]) == book_id:
                return self._book_from_path(entry)
        return None

    def get_cover(self, book_id: str) -> bytes | None:
        # No real covers in mock mode — return a 1x1 placeholder.
        return b"\x89PNG\r\n\x1a\n" + _TINY_PNG

    def open_file(self, book_id: str) -> tuple[str, Iterator[bytes]] | None:
        for entry in self._scan():
            if self._book_id(entry["abs"]) == book_id:
                return entry["name"], _file_iter(entry["abs"])
        return None


_TINY_PNG = bytes.fromhex(
    "0000000d49484452000000010000000108060000001f15c4"
    "890000000d49444154789c63f8cf000000030001006f5b"
    "2d3e0000000049454e44ae426082"
)


def _iso(t: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def _file_iter(path: str) -> Iterator[bytes]:
    try:
        f = open(path, "rb")
    except OSError:
        return
    try:
        while True:
            chunk = f.read(64 * 1024)
            if not chunk:
                break
            yield chunk
    finally:
        with suppress(OSError):
            f.close()
