"""
providers/kavita.py — Kavita implementation of the `Provider` interface.

Configuration (read from `config/server.json` → `providers.kavita`):

    {
        "base_url":   "https://kavita.example.com",
        "api_key":    "74241d5e-...",         # preferred
        "username":   "alice",                # only if no api_key
        "password":   "secret",               # only if no api_key
        "verify_tls": true,
        "timeout":    60,
        "library_ids": [1, 2]                # optional; default = all
    }
"""

from __future__ import annotations

import contextlib
import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Iterator
from typing import Any

from .base import (
    AuthorInfo,
    BookMeta,
    LibraryInfo,
    Provider,
    SeriesInfo,
)


# ── env helpers (mirrors pbcloud-override/proxy/kavita_client.py) ───────────


def _env(name: str, default: str = "") -> str:
    v = os.environ.get(name)
    return v if v is not None and v != "" else default


def _safe_int(v: Any) -> int:
    if v is None:
        return 0
    try:
        return int(v)
    except (TypeError, ValueError):
        return 0


def _safe_float(v: Any) -> float | None:
    if v is None:
        return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def _filename_from_content_disposition(cd: str) -> str | None:
    """Extract filename*= / filename= from a Content-Disposition header."""
    if not cd:
        return None
    for part in cd.split(";"):
        part = part.strip()
        if part.startswith("filename*="):
            try:
                _, value = part.split("=", 1)
                _enc, _, raw = value.partition("''")
                if raw:
                    from urllib.parse import unquote

                    return unquote(raw)
            except ValueError:
                pass
        if part.startswith("filename="):
            return part.split("=", 1)[1].strip('"')
    return None


# ── low-level Kavita client (intentionally self-contained) ──────────────────


class _KavitaClient:
    """Minimal stdlib-only HTTP client for the Kavita endpoints we use.

    Deliberately small (vs. the proxy's full client) — the API server only
    needs the read paths: libraries, series, volumes, chapters, files,
    covers. Progress sync is out of scope for this iteration (KOReader
    handles it).
    """

    def __init__(
        self,
        base_url: str,
        api_key: str = "",
        username: str = "",
        password: str = "",
        timeout: float = 60.0,
        verify_tls: bool = True,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.username = username
        self.password = password
        self.timeout = timeout
        self._jwt: str | None = None
        self._jwt_expiry: float = 0.0
        self._ssl = (
            ssl.create_default_context()
            if verify_tls
            else ssl._create_unverified_context()
        )

    def _url(self, path: str) -> str:
        if not path.startswith("/"):
            path = "/" + path
        return self.base_url + path

    def _headers(self, with_jwt: bool = True) -> dict[str, str]:
        h: dict[str, str] = {"Accept": "application/json"}
        if with_jwt and self._jwt:
            h["Authorization"] = f"Bearer {self._jwt}"
        return h

    def _request(
        self,
        method: str,
        path: str,
        body: dict | None = None,
        with_auth: bool = True,
    ) -> tuple[int, bytes, dict[str, str]]:
        url = self._url(path)
        data: bytes | None = None
        headers = self._headers(with_auth)
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            resp_ctx = urllib.request.urlopen(
                req, timeout=self.timeout, context=self._ssl
            )
            status = resp_ctx.status
            body_bytes = resp_ctx.read()
            hdrs = dict(resp_ctx.getheaders())
            return status, body_bytes, hdrs
        except urllib.error.HTTPError as exc:
            return (
                exc.code,
                exc.read() if exc.fp else b"",
                dict(exc.headers.items()),
            )
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise RuntimeError(f"Kavita request {method} {path} failed: {exc}") from exc

    def _request_json(
        self,
        method: str,
        path: str,
        body: dict | None = None,
        with_auth: bool = True,
    ) -> tuple[int, Any]:
        status, body_bytes, _ = self._request(method, path, body, with_auth)
        if not body_bytes:
            return status, None
        try:
            return status, json.loads(body_bytes.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return status, body_bytes.decode("utf-8", errors="replace")

    def ensure_auth(self) -> None:
        if self._jwt and time.time() < self._jwt_expiry - 30:
            return
        self._login()

    def _login(self) -> None:
        # Kavita 0.8.x rejects an apiKey-only login with 400 unless
        # the Password and Username fields are also present (model
        # validation requires all three, server picks the credential
        # to trust).  Always send every credential we have.
        payload: dict[str, Any] = {
            "username": self.username,
            "password": self.password,
        }
        if self.api_key:
            payload["apiKey"] = self.api_key
        status, body = self._request_json(
            "POST", "/api/Account/login", payload, with_auth=False
        )
        if status != 200 or not isinstance(body, dict):
            # Kavita returns 401 with a generic message regardless of
            # which credential was wrong, so the user has no way to
            # tell whether it's the api_key or the username/password.
            # We classify the common misconfigurations so the error
            # log points the user at the right knob to turn.
            msg = self._format_login_error(status, body)
            raise RuntimeError(msg)
        self._jwt = body.get("token")
        self._jwt_expiry = time.time() + (body.get("expiresIn", 1800) or 1800) - 60
        if not self._jwt:
            raise RuntimeError(f"Kavita login returned no token: {body!r}")

    def _format_login_error(self, status: int, body: Any) -> str:
        """Translate a Kavita login failure into an actionable message.

        Kavita's 401 / 400 responses are opaque on purpose; this
        helper inspects the local config state to call out the most
        likely culprit (typo in api_key, missing user/pass, etc.).
        """
        raw = body if isinstance(body, str) else repr(body)
        if status == 400:
            return (
                f"Kavita login rejected with 400 (model validation): {raw}. "
                "Check api/config/server.json — the kavita block must "
                "include at least username+password, or a valid api_key."
            )
        if status == 401:
            clues: list[str] = []
            if not self.api_key:
                clues.append("no api_key configured")
            else:
                # An api_key is a UUID; flag if it doesn't look like one
                # so users spot copy-paste errors of the wrong key.
                import re

                if not re.fullmatch(
                    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
                    self.api_key,
                ):
                    clues.append(
                        f"api_key={self.api_key!r} is not a UUID — "
                        "check you copied the right value"
                    )
            if not self.username:
                clues.append("no username configured")
            if not self.password:
                clues.append("no password configured")
            suffix = (
                f" Likely causes: {', '.join(clues)}."
                if clues
                else " Credentials look syntactically valid; "
                "double-check username/password and api_key against "
                "your Kavita instance."
            )
            return f"Kavita login failed: HTTP 401 body={raw!r}.{suffix}"
        return f"Kavita login failed: HTTP {status} body={raw!r}"

    # --- data accessors ---

    def list_libraries(self) -> list[dict[str, Any]]:
        self.ensure_auth()
        status, body = self._request_json("GET", "/api/Library/libraries")
        if status != 200 or not isinstance(body, list):
            return []
        return body

    def list_series(
        self,
        library_id: int,
        page: int = 1,
        page_size: int = 50,
    ) -> list[dict[str, Any]]:
        """Return ONE PAGE of series for ``library_id``.

        Kavita 0.8.x paginates by accepting ``PageNumber`` and ``PageSize``
        as **query parameters** and reading the heavy filter body
        (libraries, formats, sortOptions, ...).  The body also needs a
        ``libraries: [int]`` field; ``libraryId`` is silently ignored.
        Some Kavita builds return a ``{result: [...], totalCount, ...}``
        envelope, others return a bare list — we accept both.
        """
        self.ensure_auth()
        body = {
            "libraries": [library_id],
            "formats": [],
            "genres": [],
            "writers": [],
            "pencillers": [],
            "inkers": [],
            "colorists": [],
            "letterers": [],
            "coverArtists": [],
            "editors": [],
            "publishers": [],
            "characters": [],
            "translators": [],
            "tags": [],
            "ageRating": [],
            "languages": [],
            "publicationStatus": [],
            "seriesNameQuery": "",
            "sortOptions": {"sortField": 0, "isAscending": True},
            "readStatus": 0,
        }
        path = f"/api/Series/v2?PageNumber={page}&PageSize={page_size}"
        status, payload = self._request_json("POST", path, body=body)
        if status != 200 or payload is None:
            return []
        if isinstance(payload, list):
            return payload
        if isinstance(payload, dict):
            return payload.get("result") or []
        return []

    def get_volumes(self, series_id: int) -> list[dict[str, Any]]:
        self.ensure_auth()
        status, body = self._request_json(
            "GET", f"/api/Series/volumes?seriesId={series_id}"
        )
        if status != 200 or not isinstance(body, list):
            return []
        return body

    def get_chapter(self, chapter_id: int) -> dict[str, Any] | None:
        self.ensure_auth()
        status, body = self._request_json("GET", f"/api/Chapter?chapterId={chapter_id}")
        if status != 200 or not isinstance(body, dict):
            return None
        return body

    def get_chapter_files(self, chapter_id: int) -> list[dict[str, Any]]:
        """Return the list of physical file DTOs for one chapter.

        Kavita 0.8.x: ``/api/Book/{id}/book-info`` returns a flat book
        metadata envelope with an *empty* ``chapters`` array on this
        server, so it can't be used to discover files.  The
        ``/api/Chapter?chapterId=N`` endpoint on the other hand returns
        the full chapter DTO whose top-level ``files`` array has the
        file paths, sizes, formats and extensions we need.
        """
        self.ensure_auth()
        status, body = self._request_json("GET", f"/api/Chapter?chapterId={chapter_id}")
        if status != 200 or not isinstance(body, dict):
            return []
        return list(body.get("files") or [])

    def get_chapter_volume(self, chapter_id: int) -> dict[str, Any] | None:
        """Return the volume DTO that owns ``chapter_id`` (or None).

        Kavita's chapter DTO carries a ``volumeId`` but no ``seriesId``.
        The volume carries ``seriesId``.  We need the series id for
        cover fetches and for round-tripping a book to its parent
        series, so this helper resolves ``chapter_id → volumeId →
        volumeDTO`` (which has ``seriesId``).
        """
        ch = self.get_chapter(chapter_id)
        if not ch:
            return None
        vol_id = ch.get("volumeId")
        if vol_id is None:
            return None
        status, body = self._request_json("GET", f"/api/Volume?volumeId={vol_id}")
        if status != 200 or not isinstance(body, dict):
            return None
        return body

    def download_chapter(self, chapter_id: int) -> tuple[str, Any]:
        self.ensure_auth()
        url = self._url(f"/api/Download/chapter?chapterId={chapter_id}")
        req = urllib.request.Request(url, headers=self._headers())
        resp = urllib.request.urlopen(req, timeout=self.timeout, context=self._ssl)
        cd = resp.headers.get("Content-Disposition", "")
        filename = (
            _filename_from_content_disposition(cd) or f"chapter_{chapter_id}.epub"
        )
        return filename, resp

    def cover_bytes(
        self, *, chapter_id: int, series_id: int
    ) -> tuple[str, bytes] | None:
        """Fetch cover artwork for one (chapter, series) pair.

        Tries ``chapter-cover`` first (chapter-specific artwork when
        present, server falls back to series cover automatically),
        then ``series-cover`` as a backstop.  Kavita 0.8.x image
        routes refuse JWT auth (``401``) and require ``apiKey=`` as a
        query parameter.
        """
        for path in (
            f"/api/Image/chapter-cover?chapterId={chapter_id}&apiKey={self.api_key}",
            f"/api/Image/series-cover?seriesId={series_id}&apiKey={self.api_key}",
        ):
            req = urllib.request.Request(self._url(path))
            try:
                resp = urllib.request.urlopen(
                    req, timeout=self.timeout, context=self._ssl
                )
                return resp.headers.get("Content-Type", "image/jpeg"), resp.read()
            except (urllib.error.URLError, urllib.error.HTTPError, OSError):
                continue
        return None


# ── provider adapter ────────────────────────────────────────────────────────


class KavitaProvider(Provider):
    name = "kavita"

    # Kavita only knows a handful of file types we can deliver. Anything
    # else we report in `file_format` but the open-with picker will not
    # be able to route it.
    _SUPPORTED_FORMATS = {"epub", "pdf", "cbz", "cbr"}

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        self.client = _KavitaClient(
            base_url=cfg.get("base_url") or _env("KAVITA_URL", "http://localhost:5000"),
            api_key=cfg.get("api_key") or _env("KAVITA_API_KEY", ""),
            username=cfg.get("username") or _env("KAVITA_USER", ""),
            password=cfg.get("password") or _env("KAVITA_PASS", ""),
            timeout=cfg.get("timeout", 60.0),
            verify_tls=cfg.get("verify_tls", True),
        )
        # chapter_id -> book-id (our string id) for stable URLs
        self._id_cache: dict[int, str] = {}
        self._book_cache: dict[str, BookMeta] = {}
        self._library_filter: set[int] = set()
        for x in cfg.get("library_ids") or ():
            v = _safe_int(x)
            if v:
                self._library_filter.add(v)

    # --- helpers -----------------------------------------------------------

    def _book_id(self, chapter_id: int) -> str:
        cached = self._id_cache.get(chapter_id)
        if cached:
            return cached
        s = f"kavita_ch_{chapter_id:08x}"
        self._id_cache[chapter_id] = s
        return s

    def _format_to_ext(self, kavita_file: dict[str, Any]) -> str:
        """Resolve a file DTO to a lowercase extension string.

        Kavita 0.8.x always sets ``file.extension`` (e.g. ``.epub``).
        The ``format`` enum is **inconsistent across versions** —
        MangaFile.Archive=0/EPUB=1/PDF=2 in some builds and
        Images=3/Book=4 in others — so we trust the file extension
        over the enum and only fall back to the enum when the
        extension is missing (rare; happens for old installs).
        """
        raw_ext = kavita_file.get("extension") or ""
        raw_ext = raw_ext.lstrip(".").lower()
        if raw_ext:
            return raw_ext
        # Last-resort fallback by enum.  We map both known schemes.
        fmt = _safe_int(kavita_file.get("format"))
        for mapping in (
            {0: "cbr", 1: "pdf", 2: "epub"},
            {0: "cbr", 1: "epub", 2: "pdf", 3: "epub", 4: "pdf"},
        ):
            if fmt in mapping:
                return mapping[fmt]
        return "epub"

    def _chapter_to_meta(
        self,
        chapter: dict[str, Any],
        series: dict[str, Any],
        volume: dict[str, Any] | None = None,
    ) -> BookMeta | None:
        chapter_id = chapter.get("id")
        if not chapter_id:
            return None
        files = self.client.get_chapter_files(chapter_id)
        if not files:
            return None
        chosen: dict[str, Any] | None = None
        for f in files:
            if self._format_to_ext(f) in self._SUPPORTED_FORMATS:
                chosen = f
                break
        if chosen is None:
            return None
        book_id = self._book_id(chapter_id)
        ext = self._format_to_ext(chosen)
        size = _safe_int(chosen.get("bytes"))
        pages = _safe_int(chosen.get("pages"))

        # Title preference:
        #   - chapter.titleName  (overridden by user; usually empty for specials)
        #   - chapter.title       (volume title for specials)
        #   - series.name
        title = (
            chapter.get("titleName")
            or chapter.get("title")
            or series.get("name")
            or "Untitled"
        )

        # Series index: chapter.number is the placeholder "-100000"
        # on every chapter Kavita emits from /api/Series/volumes on
        # this server, so we use the volume's number instead.  We
        # fall back to chapter's number for builds that do report a
        # real per-chapter ordinal.
        series_index = (
            _safe_float(volume.get("number"))
            if volume is not None
            else _safe_float(chapter.get("number"))
        )
        if series_index is not None and series_index <= -100000:
            # Some builds use a large negative sentinel for
            # "no ordinal assigned" — keep None rather than the
            # sentinel.
            series_index = None

        # Authors: Kavita 0.8.x doesn't expose a clean "author"
        # endpoint; we extract the first writer/person from the
        # chapter DTO if present.  Falls back to empty list — the
        # bookshelf's "by author" filter is a future improvement.
        authors: list[str] = []
        for person in chapter.get("writers") or []:
            name = person.get("name") if isinstance(person, dict) else str(person)
            if name:
                authors.append(name)

        return BookMeta(
            id=book_id,
            title=title,
            authors=authors,
            series=series.get("name"),
            series_id=str(series.get("id")) if series.get("id") else None,
            series_index=series_index,
            summary=chapter.get("summary") or series.get("summary"),
            language=None,
            file_format=ext,
            file_name=os.path.basename(chosen.get("filePath") or "") or None,
            file_size=size,
            page_count=pages,
            cover_url=f"/api/v1/books/{book_id}/cover",
            download_url=f"/api/v1/books/{book_id}/file",
            added_at=chapter.get("createdUtc") or series.get("created"),
            updated_at=chapter.get("lastModifiedUtc") or series.get("lastModified"),
            remote_only=True,
            extra={
                "kavita_series_id": series.get("id"),
                "kavita_chapter_id": chapter_id,
                "kavita_library_id": series.get("libraryId"),
            },
        )

    def _all_library_ids(self) -> list[int]:
        if self._library_filter:
            return list(self._library_filter)
        out: list[int] = []
        for lib in self.client.list_libraries():
            lid = _safe_int(lib.get("id"))
            if lid:
                out.append(lid)
        return out

    def _series_in_library(
        self, library_id: int, series_id_filter: str | None = None
    ) -> list[dict[str, Any]]:
        out: list[dict[str, Any]] = []
        target_id: int | None = None
        if series_id_filter:
            stripped = series_id_filter.removeprefix("ser_")
            try:
                target_id = int(stripped)
            except ValueError:
                return []
        for s in self.client.list_series(library_id):
            if target_id is not None and s.get("id") != target_id:
                continue
            out.append(s)
        return out

    # --- Provider interface -----------------------------------------------

    def health(self) -> dict[str, Any]:
        try:
            self.client.ensure_auth()
            return {
                "ok": True,
                "detail": f"connected to {self.client.base_url}",
            }
        except Exception as exc:
            return {"ok": False, "detail": str(exc)}

    def list_libraries(self) -> list[LibraryInfo]:
        out: list[LibraryInfo] = []
        for lib in self.client.list_libraries():
            lid_raw = lib.get("id")
            if lid_raw is None:
                continue
            lid_i = _safe_int(lid_raw)
            if self._library_filter and lid_i not in self._library_filter:
                continue
            out.append(
                LibraryInfo(
                    id=f"lib_{lid_i}",
                    name=lib.get("name", f"Library {lid_i}"),
                    book_count=0,
                    kind="library",
                )
            )
        return out

    def list_series(self, library_id: str) -> list[SeriesInfo]:
        try:
            lid = int(library_id.removeprefix("lib_"))
        except ValueError:
            return []
        out: list[SeriesInfo] = []
        for s in self.client.list_series(lid):
            sid = s.get("id")
            if sid is None:
                continue
            out.append(
                SeriesInfo(
                    id=f"ser_{sid}",
                    name=s.get("name", f"Series {sid}"),
                    library_id=library_id,
                    book_count=_safe_int(s.get("booksCount") or s.get("books")),
                )
            )
        return out

    def list_authors(self, library_id: str | None = None) -> list[AuthorInfo]:
        # Kavita's API doesn't have a clean "list authors" endpoint;
        # authors are a free-text field on the series.  We expose an
        # empty list rather than fabricating data; the UI surfaces
        # "by author" as a future improvement.
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
        lids: list[int]
        if library_id:
            try:
                lids = [int(library_id.removeprefix("lib_"))]
            except ValueError:
                return []
        else:
            lids = self._all_library_ids()

        out: list[BookMeta] = []
        for lid in lids:
            if mode == "series" and series_id:
                series_iter = self._series_in_library(lid, series_id)
            else:
                series_iter = self._series_in_library(lid)
            for s in series_iter:
                sid = s.get("id")
                if not sid:
                    continue
                for vol in self.client.get_volumes(sid):
                    for ch in vol.get("chapters") or []:
                        meta = self._chapter_to_meta(ch, s, vol)
                        if meta is None:
                            continue
                        if search:
                            q = search.lower()
                            hay = (meta.title + " " + (meta.series or "")).lower()
                            if q not in hay:
                                continue
                        if since and meta.updated_at and meta.updated_at <= since:
                            continue
                        out.append(meta)
                        if len(out) >= limit + offset:
                            return out[offset : offset + limit]
        return out[offset : offset + limit]

    def get_book(self, book_id: str) -> BookMeta | None:
        if book_id in self._book_cache:
            return self._book_cache[book_id]
        if not book_id.startswith("kavita_ch_"):
            return None
        try:
            chapter_id = int(book_id[len("kavita_ch_") :], 16)
        except ValueError:
            return None
        chapter = self.client.get_chapter(chapter_id)
        if not chapter:
            return None
        # Kavita's chapter DTO has volumeId but no seriesId; the
        # volume carries seriesId.
        volume = self.client.get_chapter_volume(chapter_id)
        series_id = volume.get("seriesId") if volume else None
        if not series_id:
            return None
        # Locate the matching series DTO for the metadata enrichment.
        for lid in self._all_library_ids():
            for s in self.client.list_series(lid):
                if s.get("id") == series_id:
                    meta = self._chapter_to_meta(chapter, s, volume)
                    if meta is not None:
                        self._book_cache[book_id] = meta
                    return meta
        return None

    def get_cover(self, book_id: str) -> bytes | None:
        if not book_id.startswith("kavita_ch_"):
            return None
        try:
            chapter_id = int(book_id[len("kavita_ch_") :], 16)
        except ValueError:
            return None
        volume = self.client.get_chapter_volume(chapter_id)
        series_id = volume.get("seriesId") if volume else None
        if not series_id:
            return None
        out = self.client.cover_bytes(chapter_id=chapter_id, series_id=series_id)
        if out is None:
            return None
        return out[1]  # bytes

    def open_file(self, book_id: str) -> tuple[str, Iterator[bytes]] | None:
        if not book_id.startswith("kavita_ch_"):
            return None
        try:
            chapter_id = int(book_id[len("kavita_ch_") :], 16)
        except ValueError:
            return None
        try:
            filename, resp = self.client.download_chapter(chapter_id)
        except Exception as exc:
            sys.stderr.write(
                f"kavita download failed for chapter {chapter_id}: {exc}\n"
            )
            return None
        return filename, _chunk_iter(resp, 64 * 1024)


def _chunk_iter(resp: Any, chunk_size: int) -> Iterator[bytes]:
    try:
        while True:
            chunk = resp.read(chunk_size)
            if not chunk:
                break
            yield chunk
    finally:
        with contextlib.suppress(OSError):
            resp.close()
