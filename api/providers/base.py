"""
providers/base.py — abstract interface every content provider must implement.

A "provider" is the upstream content service (Kavita, Komga, Booklore, Calibre
Web, ...) that owns the user's library. The API server sits in front of one or
more providers; the in-emulator app talks to the API server only — it never
sees the provider's native API.

Design rules:

* Every method returns plain Python data (dicts, lists, bytes). No provider-
  specific types ever leak across the boundary.
* All IDs are strings (opaque). The API server is responsible for keeping the
  same ID consistent across multiple calls.
* Cover bytes are returned as raw bytes (or a path to a cached file). Cover
  URLs in the metadata must be absolute URLs that resolve to *our* API server
  (never to the upstream provider's host) so the device always comes back
  to us.
* File downloads are streamed; providers must implement a yield-bytes
  iterator rather than buffering whole files in memory.
* The provider is fully responsible for talking to its upstream (auth, retry,
  pagination, error translation). The API server treats it as a black box.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any, Iterator


@dataclass
class BookMeta:
    """Provider-neutral book metadata.

    This is what the device app shows in the library view. Every field
    below is derived from the upstream provider's data, normalised into
    a common shape.
    """

    id: str
    """Stable opaque id, unique per provider. Used to address this book
    in cover/file downloads and to key it in the device's sync state."""

    title: str
    authors: list[str] = field(default_factory=list)
    series: str | None = None
    series_id: str | None = None
    """Stable opaque id for the series this book belongs to, or None
    for standalone works.  Used by the device to collapse series into
    a single card in the grid view."""
    series_index: float | None = None
    summary: str | None = None
    language: str | None = None
    file_format: str = "epub"
    """Lowercase extension without the dot. Used to drive the open-with
    picker on the device."""
    file_name: str | None = None
    """Original filename as stored on the provider (e.g. the Kavita
    MangaFile's fileName).  The device saves downloads under this name
    instead of the opaque book id when present."""
    file_size: int = 0
    page_count: int = 0
    cover_url: str | None = None
    download_url: str | None = None
    """Absolute URL the device should hit to fetch the file. The provider
    may leave this empty; the API server will fill it in."""
    added_at: str | None = None
    """ISO-8601 timestamp the book was added to the library."""
    updated_at: str | None = None
    """ISO-8601 timestamp of the last upstream change. Used by the
    device's delta-sync."""
    remote_only: bool = True
    """Always True for provider-sourced books. The device tracks
    downloaded vs not separately."""
    extra: dict[str, Any] = field(default_factory=dict)
    """Provider-specific extras that the device doesn't need to render
    the book but might want (e.g. genre, tags, publisher). The device
    ignores unknown keys."""


@dataclass
class LibraryInfo:
    """A provider-side library/collection.

    On Kavita these are the "Libraries" (root folders). On Komga they
    are "Libraries" too. On Calibre-Web they are "Libraries" (named
    tags). Booklore doesn't really have a concept, so the Booklore
    adapter synthesises one ("All").
    """

    id: str
    name: str
    book_count: int = 0
    kind: str = "library"
    """library | collection | tag — used by the device for the
    hamburger-menu chooser."""


@dataclass
class SeriesInfo:
    id: str
    name: str
    library_id: str
    book_count: int = 0


@dataclass
class AuthorInfo:
    id: str
    name: str
    book_count: int = 0


class Provider(ABC):
    """Interface every content provider must implement.

    Implementations live in `providers/<name>.py` and are selected at
    startup from the configured `provider` setting in `config/server.json`.
    """

    name: str = ""
    """Short identifier; used in `provider` field of API responses."""

    @abstractmethod
    def health(self) -> dict[str, Any]:
        """Lightweight probe: returns `{"ok": bool, "detail": str}`.

        Used by `GET /api/v1/healthz`. Must not raise; on failure
        return `{"ok": False, "detail": "<reason>"}`."""
        raise NotImplementedError

    @abstractmethod
    def list_libraries(self) -> list[LibraryInfo]:
        """List all libraries/collections the user has access to."""
        raise NotImplementedError

    @abstractmethod
    def list_series(self, library_id: str) -> list[SeriesInfo]:
        """List series in a library, with book counts."""
        raise NotImplementedError

    @abstractmethod
    def list_authors(self, library_id: str | None = None) -> list[AuthorInfo]:
        """List authors, optionally filtered by library. `None` means
        every library the user has access to."""
        raise NotImplementedError

    @abstractmethod
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
        """List books in a chosen view.

        `mode` is one of:
            all       — every book in scope (library, or all if None)
            series    — books in a particular series (require series_id)
            author    — books by a particular author (require author_id)
            recent    — most recently added first
            search    — full-text search, requires `search`

        `since` (ISO-8601): only include books whose `updated_at > since`.
        Used by the device's delta sync. Providers that can't filter
        server-side may return the full list; the API server will
        filter again.
        """
        raise NotImplementedError

    @abstractmethod
    def get_book(self, book_id: str) -> BookMeta | None:
        """Look up a single book by ID. Returns None if missing."""
        raise NotImplementedError

    @abstractmethod
    def get_cover(self, book_id: str) -> bytes | None:
        """Return the raw cover image bytes (jpeg or png). None if
        missing. The API server will resize/cache this for the device
        if needed."""
        raise NotImplementedError

    @abstractmethod
    def open_file(self, book_id: str) -> tuple[str, Iterator[bytes]] | None:
        """Begin downloading a book's file.

        Returns `(filename, byte_iter)` where `filename` is the suggested
        file name (used for `Content-Disposition`) and `byte_iter`
        yields chunks. Returns None if the book doesn't have a file.

        Implementations should NOT buffer the whole file in memory;
        the API server streams straight through to the device.
        """
        raise NotImplementedError
