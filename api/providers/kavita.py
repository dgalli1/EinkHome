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
        "cache_ttl_s": 60,                   # seconds upstream DTOs are
                                             # treated as immutable
        "library_ids": [1, 2]                # optional; default = all
    }
"""

from __future__ import annotations

import contextlib
import http.client
import json
import os
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request
from collections import OrderedDict
from collections.abc import Iterator
from datetime import UTC, datetime
from typing import Any
from urllib.parse import urlsplit

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


def _iso_utc(s: str) -> str:
    """Normalize an ISO-8601 timestamp to UTC ``YYYY-MM-DDTHH:MM:SSZ``.

    Used to compare ``updated_at`` / ``since`` values that may carry
    different offsets (or none at all — naive timestamps are treated
    as UTC, which is what Kavita emits for wall-clock fields), so
    mixed-offset strings compare correctly.
    """
    dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=UTC)
    return dt.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _redact_secret(v: str) -> str:
    """A peek, not a leak: enough to recognise WHICH configured value
    this is (length + first/last chars), never the whole secret."""
    if not v:
        return "<empty>"
    if len(v) <= 4:
        return f"***({len(v)} chars)"
    return f"{v[:2]}…{v[-2:]} ({len(v)} chars)"


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


class CoverTransportError(Exception):
    """A cover fetch failed for a transport reason (URL/network error,
    timeout, HTTP 5xx / non-404 error), so the cover's absence is NOT
    confirmed.

    `KavitaProvider.get_cover` raises this (propagating from
    `_KavitaClient.cover_bytes`) so callers like server.py's
    ``_cover_png`` can distinguish a transient transport failure from a
    confirmed-absent cover (HTTP 404 / no series) and avoid marking a
    book's cover missing for the cache TTL on a blip.
    """


# ── low-level Kavita client (intentionally self-contained) ──────────────────

# Hard cap on how many series a single library walk will fetch.  Real
# Kavita libraries hold a few hundred at most; this only guarantees the
# page loop in _series_in_library terminates against a server that never
# returns a short page.
_MAX_SERIES_PER_LIBRARY = 10000

# Backoff sleeps (seconds) between transient-failure retries (HTTP 5xx,
# URLError/TimeoutError/OSError) in _KavitaClient._request.
_RETRY_DELAYS = (0.5, 1.0, 2.0)

# LRU caps for the provider's per-chapter caches.  The chapter-files
# cache is the hot one: a 30s-interval catalogue refresh must not
# re-fetch every chapter's files over HTTP.
_CHAPTER_FILES_CACHE_MAX = 8192
_ID_CACHE_MAX = 16384
_BOOK_CACHE_MAX = 16384

# Default cap for _KavitaClient._resp_cache, the TTL-bounded cache of
# raw upstream DTO responses (series pages and volume DTOs — full
# chapter DTOs are deliberately NOT cached there; see
# KavitaProvider._chapter_files_cache).  Both this and
# _CHAPTER_FILES_CACHE_MAX grow adaptively to the walked catalogue
# size so a full catalogue fits without LRU thrashing mid-walk (see
# KavitaProvider._iter_chapter_metas).
_RESP_CACHE_MAX = 8192


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
        cache_ttl_s: float = 60.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.username = username
        self.password = password
        self.timeout = timeout
        self.cache_ttl_s = cache_ttl_s
        self._jwt: str | None = None
        self._jwt_expiry: float = 0.0
        # Serializes login so concurrent threads don't each fire a
        # /api/Account/login (see ensure_auth).  The 401 re-auth path
        # in _request re-enters ensure_auth; login never holds this
        # lock while doing network I/O that could re-enter it.
        self._auth_lock = threading.Lock()
        self._ssl = (
            ssl.create_default_context()
            if verify_tls
            else ssl._create_unverified_context()
        )
        # TTL-bounded cache of raw upstream JSON responses.  Keyed by
        # (method, path, body-json-string); value is (fetch_time,
        # status, parsed).  Series/volume DTOs are treated as immutable
        # for cache_ttl_s seconds — a metadata change upstream
        # propagates once the TTL expires (acceptable for a 30s-interval
        # catalogue refresh).  Login (with_auth=False), chapter DTOs
        # (cacheable=False — the provider's files cache already holds
        # what the walk needs) and binary endpoints (_request /
        # cover_bytes) bypass it.  LRU-evicted; the cap adapts to the
        # catalogue size (the provider raises _resp_cache_max after
        # each walk).
        self._resp_cache: OrderedDict[tuple[str, str, str], tuple[float, int, Any]] = (
            OrderedDict()
        )
        self._resp_cache_max = _RESP_CACHE_MAX

        # Persistent HTTP(S) connection, reused across requests so a
        # catalogue walk doesn't pay a fresh TLS handshake per chapter
        # (100k+ handshakes on the first walk otherwise).  Guarded by
        # _conn_lock: http.client connections are not thread-safe, and
        # the API server can issue requests from multiple threads.
        _parsed = urlsplit(self.base_url)
        self._scheme = _parsed.scheme or "https"
        self._host = _parsed.hostname or ""
        self._port = _parsed.port
        self._base_path = _parsed.path.rstrip("/")
        self._conn: http.client.HTTPConnection | None = None
        self._conn_lock = threading.Lock()

    def _url(self, path: str) -> str:
        if not path.startswith("/"):
            path = "/" + path
        return self.base_url + path

    def _headers(self, with_jwt: bool = True) -> dict[str, str]:
        h: dict[str, str] = {"Accept": "application/json"}
        if with_jwt and self._jwt:
            h["Authorization"] = f"Bearer {self._jwt}"
        return h

    def _new_conn(self) -> http.client.HTTPConnection:
        if self._scheme == "https":
            return http.client.HTTPSConnection(
                self._host, port=self._port, context=self._ssl, timeout=self.timeout
            )
        return http.client.HTTPConnection(
            self._host, port=self._port, timeout=self.timeout
        )

    def _conn_path(self, path: str) -> str:
        if not path.startswith("/"):
            path = "/" + path
        return self._base_path + path

    def _conn_request(
        self,
        method: str,
        path: str,
        headers: dict[str, str],
        data: bytes | None,
    ) -> tuple[int, bytes, dict[str, str]]:
        """Run one request on the persistent connection (thread-safe).

        Returns ``(status, body, headers)``.  http.client surfaces
        server responses (including 4xx/5xx) as normal responses, so
        transport failures here are only socket/TLS/timeout errors.  On
        any such failure the connection state is unknown — drop it so
        the next attempt reconnects cleanly.
        """
        with self._conn_lock:
            if self._conn is None:
                self._conn = self._new_conn()
            conn = self._conn
            try:
                conn.request(method, path, body=data, headers=headers)
                resp = conn.getresponse()
                status = resp.status
                body_bytes = resp.read()
                hdrs = dict(resp.getheaders())
                return status, body_bytes, hdrs
            except Exception:
                with contextlib.suppress(Exception):
                    conn.close()
                self._conn = None
                raise

    def _request(
        self,
        method: str,
        path: str,
        body: dict | None = None,
        with_auth: bool = True,
    ) -> tuple[int, bytes, dict[str, str]]:
        request_path = self._conn_path(path)
        retries = 0
        while True:
            data: bytes | None = None
            headers = self._headers(with_auth)
            if body is not None:
                data = json.dumps(body).encode("utf-8")
                headers["Content-Type"] = "application/json"
            try:
                status, body_bytes, hdrs = self._conn_request(
                    method, request_path, headers, data
                )
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                # URLError here is only URL-parse failures; transport
                # errors surface as OSError/socket/TimeoutError via
                # http.client.  drop the connection (done in
                # _conn_request) and retry transient failures.
                if not with_auth or retries >= len(_RETRY_DELAYS):
                    raise RuntimeError(
                        f"Kavita request {method} {path} failed: {exc}"
                    ) from exc
                time.sleep(_RETRY_DELAYS[retries])
                retries += 1
                continue
            # HTTP-level status handling.  The login call itself is
            # never retried (with_auth=False): a failed login must
            # surface immediately rather than re-entering ensure_auth.
            if not with_auth:
                return status, body_bytes, hdrs
            if status == 401 and self._jwt and retries == 0:
                # Server rejected the token — force one fresh login,
                # then retry exactly once with the new JWT.
                self._jwt_expiry = 0.0
                self.ensure_auth()
                retries += 1
                continue
            if 500 <= status < 600 and retries < len(_RETRY_DELAYS):
                time.sleep(_RETRY_DELAYS[retries])
                retries += 1
                continue
            return status, body_bytes, hdrs

    def _request_json(
        self,
        method: str,
        path: str,
        body: dict | None = None,
        with_auth: bool = True,
        cacheable: bool = True,
    ) -> tuple[int, Any]:
        # Response cache: skip entirely for login (with_auth=False) —
        # credential state is never cached — and for callers that pass
        # cacheable=False (chapter DTOs, which duplicate the provider's
        # files cache and would bloat _resp_cache to 2x the catalogue).
        cache_key: tuple[str, str, str] | None = None
        if with_auth and cacheable:
            cache_key = (method, path, json.dumps(body) if body is not None else "")
            now = time.time()
            hit = self._resp_cache.get(cache_key)
            if hit is not None:
                fetch_time, status, parsed = hit
                if now - fetch_time < self.cache_ttl_s:
                    self._resp_cache.move_to_end(cache_key)
                    return status, parsed
        status, body_bytes, _ = self._request(method, path, body, with_auth)
        if not body_bytes:
            return status, None
        try:
            parsed = json.loads(body_bytes.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            parsed = body_bytes.decode("utf-8", errors="replace")
        # Don't cache auth failures (401) or transient server errors
        # (5xx) — a retried call must see the fresh result.
        if cache_key is not None and status not in (401,) and not (500 <= status < 600):
            self._resp_cache[cache_key] = (time.time(), status, parsed)
            self._resp_cache.move_to_end(cache_key)
            if len(self._resp_cache) > self._resp_cache_max:
                self._resp_cache.popitem(last=False)
        return status, parsed

    def ensure_auth(self) -> None:
        # Fast, lock-free path: a live token needs no login.
        if self._jwt and time.time() < self._jwt_expiry - 30:
            return
        # Two threads can pass the fast check simultaneously; take the
        # lock so only one performs the login.  Re-check the expiry
        # after acquiring — the first thread may have finished while we
        # waited, so the rest reuse its fresh token instead of logging
        # in again.  The 401 re-auth branch in _request re-enters this
        # method from another thread; it simply blocks here until the
        # in-flight login completes, then sees the fresh token.
        with self._auth_lock:
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
            keys = sorted(body) if isinstance(body, dict) else type(body).__name__
            raise RuntimeError(
                f"Kavita login returned no token (response keys: {keys})"
            )

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
                        f"api_key={_redact_secret(self.api_key)} is not "
                        "a UUID — check you copied the right value"
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
            raise RuntimeError(
                f"Kavita GET /api/Library/libraries failed: HTTP {status}"
            )
        return body

    def list_series(
        self,
        library_id: int,
        page: int = 1,
        page_size: int = 50,
        *,
        with_total: bool = False,
    ) -> list[dict[str, Any]] | tuple[list[dict[str, Any]], int | None]:
        """Return ONE PAGE of series for ``library_id``.

        Kavita 0.8.x paginates by accepting ``PageNumber`` and ``PageSize``
        as **query parameters** and reading the heavy filter body
        (libraries, formats, sortOptions, ...).  The body also needs a
        ``libraries: [int]`` field; ``libraryId`` is silently ignored.
        Some Kavita builds return a ``{result: [...], totalCount, ...}``
        envelope, others return a bare list — we accept both.

        With ``with_total=True`` the return value is ``(items, total)``
        where ``total`` is the envelope's ``totalCount`` when the server
        sent one (None when it didn't — callers then loop until a short
        page).
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
            raise RuntimeError(
                f"Kavita POST /api/Series/v2 (library {library_id}) "
                f"failed: HTTP {status}"
            )
        total: int | None = None
        if isinstance(payload, dict):
            raw_total = payload.get("totalCount")
            if raw_total is not None:
                total = _safe_int(raw_total)
                if total <= 0:
                    total = None  # 0 is indistinguishable from absent
            items = payload.get("result")
        else:
            items = payload
        if not isinstance(items, list):
            # 200 with an unexpected shape — treat as empty rather than
            # failing a healthy server.
            return ([], total) if with_total else []
        return (items, total) if with_total else items

    def get_volumes(self, series_id: int) -> list[dict[str, Any]]:
        self.ensure_auth()
        status, body = self._request_json(
            "GET", f"/api/Series/volumes?seriesId={series_id}"
        )
        if status == 404:
            return []  # series gone — absent resource, not an error
        if status != 200:
            raise RuntimeError(
                f"Kavita GET /api/Series/volumes (series {series_id}) "
                f"failed: HTTP {status}"
            )
        if not isinstance(body, list):
            return []
        return body

    def get_chapter(self, chapter_id: int) -> dict[str, Any] | None:
        self.ensure_auth()
        # Full chapter DTOs are not _resp_cached: they carry the whole
        # files array (multi-KB at 100k books) and the provider's
        # files cache already holds what the walk needs.  Only the
        # small series/volume DTOs are cached upstream.
        status, body = self._request_json(
            "GET", f"/api/Chapter?chapterId={chapter_id}", cacheable=False
        )
        if status == 404:
            return None  # chapter gone — absent resource, not an error
        if status != 200:
            raise RuntimeError(
                f"Kavita GET /api/Chapter (chapter {chapter_id}) failed: HTTP {status}"
            )
        if not isinstance(body, dict):
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
        status, body = self._request_json(
            "GET", f"/api/Chapter?chapterId={chapter_id}", cacheable=False
        )
        if status == 404:
            return []  # chapter gone — absent resource, not an error
        if status != 200:
            raise RuntimeError(
                f"Kavita GET /api/Chapter (chapter {chapter_id}) failed: HTTP {status}"
            )
        if not isinstance(body, dict):
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
        return self.get_volume(vol_id)

    def get_volume(self, volume_id: int) -> dict[str, Any] | None:
        """Return the volume DTO for ``volume_id`` (or None if absent)."""
        status, body = self._request_json("GET", f"/api/Volume?volumeId={volume_id}")
        if status == 404:
            return None  # volume gone — absent resource, not an error
        if status != 200:
            raise RuntimeError(
                f"Kavita GET /api/Volume (volume {volume_id}) failed: HTTP {status}"
            )
        if not isinstance(body, dict):
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
        routes prefer ``apiKey=`` as a query parameter; when no
        api_key is configured we fall back to the JWT Authorization
        header so username/password-only setups still get covers.

        Returns ``None`` ONLY when the cover is confirmed absent (every
        candidate URL answered HTTP 404).  Raises
        :class:`CoverTransportError` for any transport-level failure
        (URLError/timeout/OSError, or a non-404 HTTP error) so callers
        can avoid marking a book's cover missing on a transient blip.
        """
        if not self.api_key:
            self.ensure_auth()
        # Track the first transport-level failure so we can still try
        # the fallback URL, but only ever report "no cover" (return
        # None) when EVERY candidate answered with a confirmed absent
        # (HTTP 404).  A URLError/timeout/5xx on any candidate means
        # absence is unconfirmed — raise CoverTransportError instead.
        saw_error: Exception | None = None
        for base in (
            f"/api/Image/chapter-cover?chapterId={chapter_id}",
            f"/api/Image/series-cover?seriesId={series_id}",
        ):
            if self.api_key:
                req = urllib.request.Request(self._url(f"{base}&apiKey={self.api_key}"))
            else:
                req = urllib.request.Request(
                    self._url(base), headers=self._headers(with_jwt=True)
                )
            try:
                resp = urllib.request.urlopen(
                    req, timeout=self.timeout, context=self._ssl
                )
                return resp.headers.get("Content-Type", "image/jpeg"), resp.read()
            except urllib.error.HTTPError as exc:
                if exc.code == 404:
                    continue  # confirmed absent for this candidate
                if saw_error is None:
                    saw_error = exc
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                if saw_error is None:
                    saw_error = exc
        if saw_error is not None:
            raise CoverTransportError(
                f"Kavita cover fetch failed for chapter {chapter_id}: {saw_error}"
            ) from saw_error
        return None


# ── provider adapter ────────────────────────────────────────────────────────


class KavitaProvider(Provider):
    name = "kavita"

    # Kavita only knows a handful of file types we can deliver. Anything
    # else we report in `file_format` but the open-with picker will not
    # be able to route it.
    _SUPPORTED_FORMATS = frozenset({"epub", "pdf", "cbz", "cbr"})

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        self.client = _KavitaClient(
            base_url=cfg.get("base_url") or _env("KAVITA_URL", "http://localhost:5000"),
            api_key=cfg.get("api_key") or _env("KAVITA_API_KEY", ""),
            username=cfg.get("username") or _env("KAVITA_USER", ""),
            password=cfg.get("password") or _env("KAVITA_PASS", ""),
            timeout=cfg.get("timeout", 60.0),
            verify_tls=cfg.get("verify_tls", True),
            cache_ttl_s=cfg.get("cache_ttl_s", 60.0),
        )
        # Adaptive LRU caps: sized up to the walked catalogue after
        # each walk so every DTO of a large catalogue stays cached
        # between refreshes (see _iter_chapter_metas).  Start at the
        # module defaults.
        self._chapter_files_cache_max = _CHAPTER_FILES_CACHE_MAX
        self._resp_cache_max = _RESP_CACHE_MAX
        self._catalogue_size = 0
        # chapter_id -> book-id (our string id) for stable URLs.
        # LRU-capped: one entry per chapter at 100k books would
        # otherwise grow without bound.
        self._id_cache: OrderedDict[int, str] = OrderedDict()
        # book-id -> BookMeta for single-book lookups (LRU-capped).
        self._book_cache: OrderedDict[str, BookMeta] = OrderedDict()
        # chapter_id -> (files, volume_id) so catalogue-walk refreshes
        # don't re-fetch every chapter's files over HTTP (LRU-capped).
        # Only the file DTOs and the owning volume id are stored — never
        # a copy of the volume DTO (one volume is shared by all its
        # chapters; caching it once per chapter would duplicate it
        # catalogue-wide at 100k books).
        self._chapter_files_cache: OrderedDict[
            int, tuple[list[dict[str, Any]], int | None]
        ] = OrderedDict()
        self._library_filter: set[int] = set()
        for x in cfg.get("library_ids") or ():
            v = _safe_int(x)
            if v:
                self._library_filter.add(v)

    # --- helpers -----------------------------------------------------------

    def _book_id(self, chapter_id: int) -> str:
        cached = self._id_cache.get(chapter_id)
        if cached:
            self._id_cache.move_to_end(chapter_id)
            return cached
        s = f"kavita_ch_{chapter_id:08x}"
        self._id_cache[chapter_id] = s
        if len(self._id_cache) > _ID_CACHE_MAX:
            self._id_cache.popitem(last=False)
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

    def _chapter_files_and_volume(
        self,
        chapter_id: int,
        *,
        chapter: dict[str, Any] | None = None,
        volume: dict[str, Any] | None = None,
    ) -> tuple[list[dict[str, Any]], int | None]:
        """Return ``(files, volume_id)`` for a chapter, LRU-cached.

        Only the file DTOs and the owning volume id are stored — never
        a copy of the volume DTO (one volume is shared by all its
        chapters; caching it once per chapter would duplicate it
        catalogue-wide).  ``chapter`` / ``volume`` may be passed by
        callers that already hold them (the catalogue walk, get_book)
        so a cold miss needs no extra round-trip.  Files are taken from
        ``chapter["files"]`` when present so the walk stops issuing a
        per-chapter GET entirely; ``get_chapter_files`` is only a
        fallback for DTOs that omit the files array (`get_chapter_volume`
        books whose volume id wasn't derivable).
        """
        cached = self._chapter_files_cache.get(chapter_id)
        if cached is not None:
            self._chapter_files_cache.move_to_end(chapter_id)
            return cached
        if chapter is not None:
            files = list(chapter.get("files") or [])
            volume_id = chapter.get("volumeId")
        else:
            files = []
            volume_id = volume.get("id") if volume is not None else None
        if not files:
            files = self.client.get_chapter_files(chapter_id)
        if volume_id is None:
            # Rare: caller had neither the chapter nor the volume DTO.
            # Resolve the owning volume id (the caller fetches the
            # volume DTO itself if it needs fields off it).
            vol = self.client.get_chapter_volume(chapter_id)
            volume_id = vol.get("id") if vol else None
        entry = (files, volume_id)
        self._chapter_files_cache[chapter_id] = entry
        if len(self._chapter_files_cache) > self._chapter_files_cache_max:
            self._chapter_files_cache.popitem(last=False)
        return entry

    def _chapter_to_meta(
        self,
        chapter: dict[str, Any],
        series: dict[str, Any],
        volume: dict[str, Any] | None = None,
    ) -> BookMeta | None:
        chapter_id = chapter.get("id")
        if not chapter_id:
            return None
        files, _ = self._chapter_files_and_volume(
            chapter_id, chapter=chapter, volume=volume
        )
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
        # Kavita paginates at 50/page; walk every page or the catalogue
        # silently truncates past the first one.  totalCount (when the
        # server sends an envelope) bounds the loop; otherwise a short
        # page marks the end.  The hard cap guarantees termination even
        # against a server that never returns a short page.
        page = 1
        page_size = 50
        fetched = 0
        while fetched < _MAX_SERIES_PER_LIBRARY:
            items, total = self.client.list_series(
                library_id, page=page, page_size=page_size, with_total=True
            )
            fetched += len(items)
            for s in items:
                if not isinstance(s, dict):
                    continue
                if target_id is not None and s.get("id") != target_id:
                    continue
                out.append(s)
            if len(items) < page_size:
                break  # last (partial) page
            if isinstance(total, int) and fetched >= total:
                break  # envelope's totalCount reached
            page += 1
        return out

    def _iter_chapter_metas(
        self,
        lids: list[int],
        series_id_filter: str | None = None,
    ) -> Iterator[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]]:
        """Yield ``(chapter, series, volume)`` for every chapter in ``lids``.

        Walks libraries → series → volumes → chapters exactly once, in
        stable order.  Shared by list_books and walk_books so both stay
        in the same order without duplicating the walk.

        When the walk ends (fully consumed or abandoned), the LRU caps
        are grown to fit the walked catalogue: at 100k books the
        default 8192-entry caps would thrash within a single walk and
        re-fetch everything on the next refresh.  The chapter-files
        cache is sized to ``2 * catalogue_size`` so every chapter's
        small ``(files, volume_id)`` entry stays cached between the
        30s-interval refreshes.  Full chapter DTOs are never held in
        ``_resp_cache`` (they duplicate the files cache); it only
        stores series pages and volume DTOs, so its cap is grown in
        lockstep but its real size stays bounded by distinct series.
        Partial walks only ever grow the caps (``max``), never shrink
        them.
        """
        walked = 0
        try:
            for lid in lids:
                for s in self._series_in_library(lid, series_id_filter):
                    sid = s.get("id")
                    if not sid:
                        continue
                    for vol in self.client.get_volumes(sid):
                        for ch in vol.get("chapters") or []:
                            walked += 1
                            yield ch, s, vol
        finally:
            self._catalogue_size = max(self._catalogue_size, walked)
            # The files cache is the one that must fit the whole
            # catalogue (one (files, volume_id) per chapter).  The
            # response cache cap is kept in lockstep for series/volume
            # DTOs; chapter DTOs never enter it (cacheable=False).
            self._chapter_files_cache_max = max(8192, self._catalogue_size * 2)
            self._resp_cache_max = max(8192, self._catalogue_size * 2)
            # The response cache lives on the client; keep its cap in
            # lockstep with the provider's.
            self.client._resp_cache_max = self._resp_cache_max

    # --- Provider interface -----------------------------------------------

    def health(self) -> dict[str, Any]:
        try:
            self.client.ensure_auth()
            return {
                "ok": True,
                "detail": f"connected to {self.client.base_url}",
            }
        except Exception as exc:  # noqa: BLE001 — health probe reports any failure
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
        # client.list_series returns ONE page (50 default); walk every
        # page so a library >50 series is fully listed.
        for s in self._series_in_library(lid):
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

    def walk_books(
        self, *, mode: str = "all", chunk_size: int = 500
    ) -> Iterator[list[BookMeta]]:
        """Stream the full catalogue in stable order, in bounded chunks.

        Single-pass override: each library → series → volume → chapter
        is visited exactly once (no offset-0 re-scan, no accumulated
        array slice), so a full-catalogue walk is linear in the number
        of chapters.  Only the unfiltered ``mode="all"`` walk takes
        this path; filtered walks fall back to the base offset-paged
        implementation (which reuses list_books' filters).
        """
        if mode != "all":
            yield from super().walk_books(mode=mode, chunk_size=chunk_size)
            return
        chunk: list[BookMeta] = []
        for ch, s, vol in self._iter_chapter_metas(self._all_library_ids()):
            meta = self._chapter_to_meta(ch, s, vol)
            if meta is None:
                continue
            chunk.append(meta)
            if len(chunk) >= chunk_size:
                yield chunk
                chunk = []
        if chunk:
            yield chunk

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

        # Normalize `since` to UTC once: `updated_at` and `since` are
        # both ISO-8601 strings, but mixed offsets would compare wrong
        # lexicographically.
        since_norm: str | None = None
        if since:
            try:
                since_norm = _iso_utc(since)
            except ValueError:
                since_norm = None  # unparseable — fall back to raw compare

        out: list[BookMeta] = []
        for ch, s, vol in self._iter_chapter_metas(
            lids, series_id if mode == "series" else None
        ):
            meta = self._chapter_to_meta(ch, s, vol)
            if meta is None:
                continue
            if search:
                q = search.lower()
                hay = (meta.title + " " + (meta.series or "")).lower()
                if q not in hay:
                    continue
            if since and meta.updated_at:
                if since_norm is None:
                    if meta.updated_at <= since:
                        continue
                else:
                    try:
                        if _iso_utc(meta.updated_at) <= since_norm:
                            continue
                    except ValueError:
                        pass  # unparseable updated_at — keep the book
            out.append(meta)
            if len(out) >= limit + offset:
                return out[offset : offset + limit]
        return out[offset : offset + limit]

    def get_book(self, book_id: str) -> BookMeta | None:
        if book_id in self._book_cache:
            self._book_cache.move_to_end(book_id)
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
        # volume carries seriesId.  The chapter cache now stores only
        # (files, volume_id); fetch the small volume DTO on demand for
        # its seriesId.
        _, volume_id = self._chapter_files_and_volume(chapter_id, chapter=chapter)
        volume = self.client.get_volume(volume_id) if volume_id else None
        series_id = volume.get("seriesId") if volume else None
        if not series_id:
            return None
        # Locate the matching series DTO for the metadata enrichment.
        # Walk every series page (client.list_series alone returns only
        # the first 50) so a book in a library >50 series is found.
        for lid in self._all_library_ids():
            for s in self._series_in_library(lid):
                if s.get("id") == series_id:
                    meta = self._chapter_to_meta(chapter, s, volume)
                    if meta is not None:
                        self._book_cache[book_id] = meta
                        if len(self._book_cache) > _BOOK_CACHE_MAX:
                            self._book_cache.popitem(last=False)
                    return meta
        return None

    def get_cover(self, book_id: str) -> bytes | None:
        if not book_id.startswith("kavita_ch_"):
            return None
        try:
            chapter_id = int(book_id[len("kavita_ch_") :], 16)
        except ValueError:
            return None
        # Only the owning volume (for its seriesId) is needed here — no
        # files round-trip.  get_chapter_volume resolves chapter →
        # volume DTO.
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
        except Exception as exc:  # noqa: BLE001 — any download failure is an error
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
