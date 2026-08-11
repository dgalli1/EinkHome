"""
api/server.py - the pbemu bookshelf API.

A clean, provider-agnostic REST API that the in-emulator ARM app talks
to.  Designed to be small (stdlib only) and to map 1:1 onto the new
provider-neutral data model, NOT onto the legacy pbcloud API.

URL surface (all under /api/v1/):

  GET  /healthz                      - liveness
  GET  /libraries                    - list libraries/collections
  GET  /libraries/{id}/series        - series in a library
  GET  /authors                      - list authors
  GET  /books                        - list books
  GET  /books/{id}                   - book detail
  GET  /books/{id}/cover             - cover image
  GET  /books/{id}/file              - stream the actual file
  POST /sync/delta                   - metadata-only diff
  POST /sync/state                   - device posts its sync state
  POST /open-with                    - resolve file extension to app

Auth: bearer token (PBEMU_API_TOKEN env).  The device presents it in
the Authorization header.  Cover and file URLs embed the token in
their query string because PB's cover fetcher does not forward
Authorization.
"""

from __future__ import annotations

"""pbemu bookshelf REST API server.

This module lives at ``api/api/server.py``.  Its sibling subpackages are
``api.providers`` and ``api.storage``, but the module-level imports
below are written as ``providers``/``storage`` (not ``api.providers``)
so the file reads top-to-bottom without package-relative boilerplate.
To make that style work regardless of where the script is invoked
from — ``python -m api.api.server`` from the repo root, or
``python api/api/server.py`` directly — we prepend the ``api/``
directory to ``sys.path`` before importing the providers.
"""

import os
import sys

# Adjust sys.path BEFORE the heavy stdlib + third-party imports so the
# sibling ``providers``/``storage`` packages are importable as flat
# module names.
_API_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if _API_DIR not in sys.path:
    sys.path.insert(0, _API_DIR)

import argparse  # noqa: E402
import hmac  # noqa: E402
import http.server  # noqa: E402
import json  # noqa: E402
import re  # noqa: E402
import socketserver  # noqa: E402
import sqlite3  # noqa: E402
import tempfile  # noqa: E402
import time  # noqa: E402
import threading  # noqa: E402
import traceback  # noqa: E402
from dataclasses import dataclass, field  # noqa: E402
from typing import Any  # noqa: E402
from urllib.parse import parse_qs, unquote  # noqa: E402

from providers.base import BookMeta  # noqa: E402
from storage.cover_cache import CoverCache  # noqa: E402
from storage.ledger import SyncLedger  # noqa: E402
from storage.placeholder import PLACEHOLDER_PNG  # noqa: E402
from storage.suggest import search_text, suggest_terms  # noqa: E402

DEFAULT_CONFIG_PATH = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "config",
    "server.json",
)


# -- config loading ------------------------------------------------------


def _default_config() -> dict[str, Any]:
    """Return a fresh default config so the server boots with no
    file on disk.  Used by tests and by the `init` subcommand."""
    return {
        "api_token": "pbemu-dev-token",
        "provider": "mock",
        "host": "0.0.0.0",
        "port": 8765,
        "providers": {
            "mock": {
                "kind": "mock",
                "books_dir": "U633_6.8.2817/.live/mnt/ext1/books",
                "library_name": "pbemu demo library",
            },
            "kavita": {
                "kind": "kavita",
                "base_url": "",
                "api_key": "",
                "username": "",
                "password": "",
                "verify_tls": True,
                "timeout": 60,
                "library_ids": [],
            },
        },
        "open_with": {
            "epub": ["eink-reader", "bookshelf"],
            "pdf": ["eink-reader", "pdfviewer"],
            "fb2": ["eink-reader"],
            "txt": ["eink-reader"],
            "djvu": ["djvureader"],
            "default": ["eink-reader"],
        },
        "cover_cache_dir": ".cover-cache",
        "ledger": {
            "refresh_max_age_s": 30.0,
        },
    }


def load_config(path: str = DEFAULT_CONFIG_PATH) -> dict[str, Any]:
    """Read server.json (creating a sane default if missing)."""
    if not os.path.isfile(path):
        return _default_config()
    with open(path, encoding="utf-8") as f:
        try:
            return json.load(f)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"invalid config {path}: {exc}") from exc


# -- helpers -------------------------------------------------------------


def _auth_token(headers: dict[str, str]) -> str:
    """Extract bearer token from a request's headers."""
    raw = headers.get("Authorization", "")
    if raw.lower().startswith("bearer "):
        return raw[7:].strip()
    return ""


def _json(status: int, body: dict[str, Any]) -> tuple[int, dict[str, str], bytes]:
    payload = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode(
        "utf-8"
    )
    headers = {
        "Content-Type": "application/json; charset=utf-8",
        "Content-Length": str(len(payload)),
        "Cache-Control": "no-store",
    }
    return status, headers, payload


def _bytes(
    status: int, body: bytes, content_type: str, extra: dict[str, str] | None = None
) -> tuple[int, dict[str, str], bytes]:
    headers = {
        "Content-Type": content_type,
        "Content-Length": str(len(body)),
        "Cache-Control": "no-store",
    }
    if extra:
        headers.update(extra)
    return status, headers, body


def _stream(
    status: int,
    body_iter: Any,
    content_type: str,
    content_length: int | None,
    extra_headers: dict[str, str] | None = None,
) -> tuple[int, dict[str, str], Any]:
    headers = {
        "Content-Type": content_type,
        "Cache-Control": "no-store",
        "Transfer-Encoding": "chunked",
    }
    if content_length is not None:
        headers["Content-Length"] = str(content_length)
        headers.pop("Transfer-Encoding", None)
    if extra_headers:
        headers.update(extra_headers)
    return status, headers, body_iter


def _book_to_api(meta: BookMeta) -> dict[str, Any]:
    return {
        "id": meta.id,
        "title": meta.title,
        "authors": list(meta.authors),
        "series": meta.series,
        "seriesId": meta.series_id,
        "seriesIdx": meta.series_index,
        "summary": meta.summary,
        "lang": meta.language,
        "format": meta.file_format,
        "filename": meta.file_name,
        "size": meta.file_size,
        "pages": meta.page_count,
        "cover": f"/api/v1/books/{meta.id}/cover",
        "url": f"/api/v1/books/{meta.id}/file",
        "addedAt": meta.added_at,
        "updatedAt": meta.updated_at,
        "remoteOnly": meta.remote_only,
        "extra": dict(meta.extra),
    }


def _safe_filename(name: str) -> str:
    """Strip path traversal and quoting from a filename for the
    Content-Disposition header."""
    name = os.path.basename(name or "book")
    name = re.sub(r"[^A-Za-z0-9._-]", "_", name)
    return name[:80] or "book"


def _mime_for(app: "PbemuAPIServer", book_id: str, filename: str) -> str:
    ext = os.path.splitext(filename)[1].lower().lstrip(".")
    if ext in ("epub", "fb2", "mobi", "azw", "azw3"):
        return f"application/{ext}+zip"
    if ext == "pdf":
        return "application/pdf"
    if ext in ("djvu", "djv"):
        return "image/vnd.djvu"
    if ext in ("jpg", "jpeg"):
        return "image/jpeg"
    if ext == "png":
        return "image/png"
    return "application/octet-stream"


# -- server --------------------------------------------------------------


class PbemuAPIServer(http.server.BaseHTTPRequestHandler):
    """The pbemu bookshelf API HTTP request handler.

    A single instance is created per request.  State (provider,
    cover cache, config) lives on the `app` instance attribute which
    is injected at startup via `main()`.
    """

    app: Any
    """Per-process shared state (provider, cover_cache, config, open_with).
    Set by `main()`'s dynamic RequestHandler subclass."""

    config: Any
    provider: Any
    cover_cache: Any
    open_with: Any

    server_version = "pbemu-api/0.1"
    protocol_version = "HTTP/1.1"

    # The `app` instance attribute is set per-request from
    # `main()`'s RequestHandler subclass.  We don't set it here
    # because BaseHTTPRequestHandler.__init__ doesn't take it.

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002
        # `self.client_address[0]` instead of `address_string()`:
        # the latter does a reverse-DNS lookup per request, which is
        # pure latency on a LAN-bound device API.
        sys.stderr.write(
            f"[{time.strftime('%H:%M:%S')}] {self.client_address[0]} - {format % args}\n"
        )

    def log_request(self, code: int = -1, size: int = -1) -> None:
        """Log method + path + status, dropping the query string so an
        embedded ``?access_token=...`` never reaches stderr."""
        path = self.path.split("?", 1)[0]
        self.log_message('"%s %s" %s', self.command, path, code)

    # --- helpers ---------------------------------------------------------

    def _send(self, status: int, hdrs: dict[str, str], body: bytes) -> None:
        try:
            # HEAD: same status/headers as GET, no body bytes on the wire.
            if getattr(self, "_head_only", False):
                body = b""
            self.send_response(status)
            self.send_header("Server", self.server_version)
            for k, v in hdrs.items():
                self.send_header(k, v)
            # HTTP/1.1 correctness: every plain response carries an
            # explicit Content-Length (except bodyless 204/304) and
            # closes the connection so no keep-alive state machine has
            # to guess at framing.
            if "Content-Length" not in hdrs and status not in (204, 304):
                self.send_header("Content-Length", str(len(body)))
            if "Connection" not in hdrs:
                self.send_header("Connection", "close")
            self.end_headers()
            self._headers_sent = True
            if body:
                self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _stream_send(self, status: int, hdrs: dict[str, str], body_iter: Any) -> None:
        try:
            self.send_response(status)
            self.send_header("Server", self.server_version)
            for k, v in hdrs.items():
                self.send_header(k, v)
            if "Connection" not in hdrs:
                self.send_header("Connection", "close")
            self.end_headers()
            self._headers_sent = True
            for chunk in body_iter:
                if not chunk:
                    continue
                self.wfile.write(f"{len(chunk):x}\r\n".encode("ascii"))
                self.wfile.write(chunk)
                self.wfile.write(b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _split_path(self) -> tuple[str, dict[str, list[str]]]:
        """Return (cleaned_path, query_dict).

        The path is percent-unquoted and the query is parsed with
        ``parse_qs`` (which unquotes keys and values) so percent-encoded
        ids route correctly and ``?access_token=`` values containing
        reserved characters still authenticate.
        """
        full = self.path
        if "?" in full:
            path, qs = full.split("?", 1)
        else:
            path, qs = full, ""
        path = unquote(path.lstrip("/"))
        q = parse_qs(qs, keep_blank_values=True)
        return path, q

    def _auth_ok(self) -> bool:
        cfg = self.app.config
        if not cfg.get("api_token"):
            return True  # dev mode
        expected = str(cfg["api_token"])
        token = _auth_token(dict(self.headers))
        if token and hmac.compare_digest(token, expected):
            return True
        # For cover and file GETs, the device may pass the token in
        # `?access_token=...` because the cover loader does not
        # re-attach the Authorization header on subsequent fetches.
        # This is the same workaround the legacy PB cloud used.
        _, q = self._split_path()
        at = (q.get("access_token") or [""])[0]
        return bool(at) and hmac.compare_digest(at, expected)

    def _read_body_json(self, max_bytes: int = 1024 * 1024) -> dict[str, Any] | None:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except (TypeError, ValueError):
            return None
        if length <= 0:
            return {}
        if length > max_bytes:
            return None
        try:
            raw = self.rfile.read(length)
        except OSError:
            return None
        try:
            return json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return None

    def _safe_handle(self, fn: Any) -> None:
        """Outer safety net around the dispatch handlers.

        Any uncaught exception (e.g. a provider crash) is logged as a
        traceback and answered with a 500 JSON response instead of
        killing the connection with no status line.  Once headers have
        been sent there is nothing safe left to send, so the connection
        is simply dropped.
        """
        self._headers_sent = False
        try:
            fn()
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception:  # noqa: BLE001 — outer net; per-endpoint try/excepts run first
            traceback.print_exc()
            if not self._headers_sent:
                self._send(*_json(500, {"error": "internal server error"}))

    # --- routing ---------------------------------------------------------

    def do_GET(self) -> None:  # noqa: N802
        self._safe_handle(self._dispatch_get)

    def do_HEAD(self) -> None:  # noqa: N802
        # Route like GET but never send a body: `_send` suppresses it.
        self._head_only = True
        self._safe_handle(self._dispatch_get)

    def _dispatch_get(self) -> None:
        path, q = self._split_path()
        endpoint, full = self._route(path)
        if endpoint is None:
            self._send(*_json(404, {"error": "not found", "path": full}))
            return
        # `healthz` is public (mirrors the `_route` comment: health
        # probes don't need to know the API version or carry a token);
        # every other endpoint requires auth.
        if endpoint != "healthz" and not self._auth_ok():
            self._send(*_json(401, {"error": "unauthorized"}))
            return
        self._handle_get(endpoint, full)

    def do_POST(self) -> None:  # noqa: N802
        self._safe_handle(self._dispatch_post)

    def _dispatch_post(self) -> None:
        path, _ = self._split_path()
        endpoint, full = self._route(path)
        if endpoint is None:
            self._send(*_json(404, {"error": "not found", "path": full}))
            return
        if not self._auth_ok():
            self._send(*_json(401, {"error": "unauthorized"}))
            return
        self._handle_post(endpoint, full)

    def do_PUT(self) -> None:  # noqa: N802
        # PUT was previously an alias for GET; nothing writes via PUT.
        self._send(*_json(405, {"error": "method not allowed"}))

    def _route(self, path: str) -> tuple[str | None, str]:
        """Map a URL path onto an `endpoint` string.

        Strips `/api/v1` prefix when present, falls back to plain
        top-level routing so health probes don't have to know the
        API version.
        """
        full = path
        if path.startswith("api/v1/"):
            endpoint = path[len("api/v1/") :]
        elif path.startswith("v1/"):
            endpoint = path[len("v1/") :]
        else:
            endpoint = path
        endpoint = endpoint.rstrip("/")
        return endpoint or None, full

    # --- GET dispatch -----------------------------------------------------

    def _handle_get(self, endpoint: str, full_path: str) -> None:
        provider = self.app.provider

        if endpoint == "healthz":
            self._send(
                *_json(
                    200,
                    {
                        "status": "ok",
                        "provider": provider.name,
                        "detail": "pbemu-api ready",
                        "pid": os.getpid(),
                    },
                )
            )
            return

        if endpoint == "libraries":
            libs = [
                {
                    "id": lib.id,
                    "name": lib.name,
                    "count": lib.book_count,
                }
                for lib in provider.list_libraries()
            ]
            self._send(*_json(200, {"items": libs, "count": len(libs)}))
            return

        if endpoint.startswith("libraries/") and endpoint.endswith("/series"):
            lib_id = endpoint[len("libraries/") : -len("/series")]
            series = [
                {
                    "id": s.id,
                    "name": s.name,
                    "library": s.library_id,
                    "count": s.book_count,
                }
                for s in provider.list_series(library_id=lib_id)
            ]
            self._send(*_json(200, {"items": series, "count": len(series)}))
            return

        if endpoint == "authors":
            authors = provider.list_authors()
            self._send(*_json(200, {"items": authors, "count": len(authors)}))
            return

        if endpoint == "books":
            self._handle_list_books()
            return

        if endpoint.startswith("books/"):
            rest = endpoint[len("books/") :]
            if "/" in rest:
                book_id, sub = rest.split("/", 1)
            else:
                book_id, sub = rest, ""
            if sub in ("", "detail"):
                meta = provider.get_book(book_id)
                if meta is None:
                    self._send(*_json(404, {"error": "not found", "id": book_id}))
                    return
                self._send(*_json(200, _book_to_api(meta)))
                return
            if sub == "cover":
                self._handle_cover(book_id)
                return
            if sub == "file":
                if getattr(self, "_head_only", False):
                    # HEAD cannot stream without executing the handler
                    # twice; refuse it rather than fake a body.
                    self._send(
                        *_json(405, {"error": "method not allowed", "path": full_path})
                    )
                    return
                self._handle_file(book_id)
                return
        self._send(*_json(404, {"error": "not found", "path": full_path}))

    def _handle_list_books(self) -> None:
        _, q = self._split_path()
        mode = (q.get("mode") or ["all"])[0]
        library_id = (q.get("library") or [None])[0]
        series_id = (q.get("series") or [None])[0]
        author_id = (q.get("author") or [None])[0]
        search = (q.get("search") or [None])[0]
        try:
            limit = int((q.get("limit") or ["500"])[0])
        except (TypeError, ValueError):
            limit = 500
        limit = max(1, min(limit, 2000))
        try:
            offset = int((q.get("offset") or ["0"])[0])
        except (TypeError, ValueError):
            offset = 0
        offset = max(0, offset)
        since = (q.get("since") or [None])[0]
        provider = self.app.provider
        # Fetch one extra row to distinguish an exactly-full page from a
        # truncated one (same trick the delta sync uses).
        fetched = provider.list_books(
            mode=mode,
            library_id=library_id,
            series_id=series_id,
            author_id=author_id,
            search=search,
            limit=limit + 1,
            offset=offset,
            since=since,
        )
        has_more = len(fetched) > limit
        books = fetched[:limit]
        items = [_book_to_api(b) for b in books]
        self._send(
            *_json(
                200,
                {
                    "items": items,
                    "limit": limit,
                    "offset": offset,
                    "count": len(items),
                    "hasMore": has_more,
                },
            )
        )

    def _handle_cover(self, book_id: str) -> None:
        cache = self.app.cover_cache
        etag = cache.etag_for(book_id)
        inm = self.headers.get("If-None-Match")
        if inm and inm.strip('"') == etag:
            # ETag is derived from the book id, so a matching client
            # already holds the identical bytes; 304, no body.
            self._send(304, {"ETag": etag}, b"")
            return
        png = cache.read_png(book_id)
        if png is None:
            # Not pre-heated yet: process synchronously now so the device
            # still gets a real (small) cover instead of the raw multi-MB
            # upstream bytes.  Concurrent cold fetches for the same book
            # share one download/decode: the first thread registers an
            # event, the rest wait (bounded) and then serve from cache.
            inflight = self.app.inflight
            ev = inflight.get(book_id)
            if ev is not None:
                ev.wait(timeout=20.0)
                png = cache.read_png(book_id)
            if png is None:
                # Owner of the slot, or the waiter timed out — process
                # anyway (never deadlock on a slow owner).
                ev = inflight.setdefault(book_id, threading.Event())
                try:
                    raw = self.app.provider.get_cover(book_id)
                    if raw:
                        png = cache.process_and_store(book_id, raw)
                finally:
                    inflight.pop(book_id, None)
                    ev.set()
        if not png:
            png = PLACEHOLDER_PNG
        self._send(
            *_bytes(200, png, "image/png", {"ETag": etag})
        )

    def _handle_file(self, book_id: str) -> None:
        opened = self.app.provider.open_file(book_id)
        if opened is None:
            self._send(*_json(404, {"error": "no file for book", "id": book_id}))
            return
        filename, body_iter = opened
        status, hdrs, _ = _stream(
            200,
            body_iter,
            content_type=_mime_for(self.app, book_id, filename),
            content_length=None,
            extra_headers={
                "Content-Disposition": (
                    f'attachment; filename="{_safe_filename(filename)}"'
                ),
                "X-Pbemu-Provider": self.app.provider.name,
            },
        )
        self._stream_send(status, hdrs, body_iter)

    # --- POST dispatch ---------------------------------------------------

    def _handle_post(self, endpoint: str, full_path: str) -> None:
        if endpoint == "sync/delta":
            self._handle_sync_delta()
            return
        if endpoint == "sync/state":
            self._handle_sync_state()
            return
        if endpoint == "open-with":
            self._handle_open_with()
            return
        # Some clients (notably libinkview's QuickDownload on this
        # firmware) issue POSTs for cover/file even though our API
        # documents them as GET.  Accept POST for parity.
        if endpoint.startswith("books/") and "/" in endpoint[len("books/") :]:
            book_id, sub = endpoint[len("books/") :].split("/", 1)
            if sub == "cover":
                self._handle_cover(book_id)
                return
            if sub == "file":
                self._handle_file(book_id)
                return
        self._send(*_json(404, {"error": "not found", "path": full_path}))

    def _handle_sync_delta(self) -> None:
        """POST /api/v1/sync/delta - cursor-based metadata-only sync.

        Request:  {"cursor": <int>, "limit": <int>}   (cursor optional)
        Response: {"added": [BookMeta...], "removed": ["id"...],
                   "nextCursor": <int>, "more": <bool>,
                   "serverTime": "...", "provider": "..."}

        The device stores only ``nextCursor`` (one integer) and replays
        from there — no id lists cross the wire, so this scales to
        100k+ books.  ``added`` carries full metadata for new/changed
        rows; ``removed`` lists tombstoned ids.  Rows are served from
        the on-disk ledger (SQLite), never from the provider directly,
        so the endpoint stays fast even while the upstream is slow.
        The ledger refreshes from the provider at most once per
        ``ledger.refresh_max_age_s`` seconds.
        """
        body = self._read_body_json() or {}
        ledger = getattr(self.app, "ledger", None)
        if ledger is None:
            self._send(
                *_json(
                    503,
                    {"error": "sync ledger unavailable", "more": False},
                )
            )
            return
        try:
            cursor = int(body.get("cursor") or 0)
        except (TypeError, ValueError):
            cursor = 0
        try:
            limit = int(body.get("limit") or 500)
        except (TypeError, ValueError):
            limit = 500
        limit = max(1, min(limit, 2000))
        max_age = getattr(self.app, "ledger_max_age", 30.0)
        try:
            ledger.refresh(self.app.provider, max_age_s=max_age)
        except Exception as exc:  # noqa: BLE001 — provider may be down
            sys.stderr.write(f"sync/delta: ledger refresh failed: {exc}\n")
        entries, more = ledger.delta(cursor, limit)
        added: list[dict[str, Any]] = []
        removed: list[str] = []
        for e in entries:
            if e.added_at is None:
                removed.append(e.book_id)
                continue
            authors = json.loads(e.authors)
            added.append(
                {
                    "id": e.book_id,
                    "title": e.title,
                    "authors": authors,
                    "series": e.series,
                    "seriesId": e.series_id,
                    "seriesIdx": e.series_idx,
                    "format": e.format,
                    "filename": e.file_name,
                    "size": e.size,
                    "cover": f"/api/v1/books/{e.book_id}/cover",
                    "url": f"/api/v1/books/{e.book_id}/file",
                    "addedAt": e.added_at,
                    # Folded search target: suggestions are folded
                    # server-side, so the device must match LIKE
                    # against folded text too (a "songgong" suggestion
                    # from "sŏnggong" never matches the raw title).
                    "searchText": search_text(e.title, authors, e.series),
                    # Search-completion terms for the device-local
                    # index; computed at serve time from the same
                    # fields the fingerprint hashes, so any
                    # term-affecting edit already bumped the rev.
                    "suggest": suggest_terms(e.title, authors, e.series),
                }
            )
        next_cursor = entries[-1].rev if entries else cursor
        self._send(
            *_json(
                200,
                {
                    "added": added,
                    "removed": removed,
                    "nextCursor": next_cursor,
                    "more": more,
                    "serverTime": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    "provider": self.app.provider.name,
                },
            )
        )

    def _handle_sync_state(self) -> None:
        """POST /api/v1/sync/state - device posts its sync state.

        Logged for debugging; the device's last consumed rev (``cursor``)
        is persisted to the ledger, where it bounds tombstone compaction.
        """
        body = self._read_body_json(max_bytes=32 * 1024 * 1024) or {}
        device_id = body.get("deviceId", "unknown")
        known = body.get("known") or []
        downloaded = body.get("downloaded") or []
        sys.stderr.write(
            f"sync/state: device={device_id} known={len(known)} downloaded={len(downloaded)}\n"
        )
        cursor = body.get("cursor")
        if isinstance(cursor, int) and cursor >= 0:
            ledger = getattr(self.app, "ledger", None)
            if ledger is not None:
                ledger.record_device(body.get("device") or "default", cursor)
        self._send(*_json(202, {"ok": True, "deviceId": device_id}))

    def _handle_open_with(self) -> None:
        """POST /api/v1/open-with - resolve a file extension to an app.

        Request: `{"id": "<book-id>", "ext": "<optional>"}`
        Response: `{"app": "eink-reader", "alternates": [...], "url": ..., "ext": ...}`
        """
        body = self._read_body_json() or {}
        book_id = body.get("id")
        if not book_id:
            self._send(*_json(400, {"error": "missing id"}))
            return
        ext = body.get("ext")
        if not ext:
            meta = self.app.provider.get_book(book_id)
            ext = meta.file_format if meta else None
        if ext:
            ext = ext.lower()  # table keys are lowercase; accept any case
        table = self.app.open_with
        if ext and ext in table:
            primary = table[ext][0]
            alts = table[ext][1:]
        else:
            primary = table.get("default", ["eink-reader"])[0]
            alts = table.get("default", [])[1:]
        self._send(
            *_json(
                200,
                {
                    "app": primary,
                    "alternates": list(alts),
                    "url": f"/api/v1/books/{book_id}/file",
                    "ext": ext or "",
                },
            )
        )


# -- provider factory ----------------------------------------------------


def build_provider(cfg: dict[str, Any]):
    """Construct the configured provider.  Returns one of:
    - providers.mock.MockProvider
    - providers.kavita.KavitaProvider
    """
    kind = (cfg.get("provider") or "mock").lower()
    pcfg = (cfg.get("providers") or {}).get(kind) or {}
    if kind == "mock":
        from providers.mock import MockProvider

        return MockProvider(pcfg)
    if kind == "kavita":
        from providers.kavita import KavitaProvider

        return KavitaProvider(pcfg)
    raise SystemExit(f"unknown provider kind: {kind}")


# -- HTTP server glue ----------------------------------------------------


@dataclass
class _AppState:
    """Per-process shared state handed to each request handler."""

    config: dict[str, Any]
    provider: Any
    cover_cache: CoverCache
    open_with: dict[str, Any]
    ledger: SyncLedger | None
    ledger_max_age: float
    inflight: dict[str, threading.Event] = field(default_factory=dict)
    """Per-book-id cover-processing events: serialises concurrent cold
    cover fetches so only one thread downloads/decodes per book."""


def build_default_app(
    cfg: dict[str, Any] | None = None, *, config_path: str | None = None
) -> Any:
    """Resolve config + provider + cover cache + sync ledger into a
    single ``_AppState`` instance that the HTTP handler picks up.
    """
    cfg = cfg or load_config()
    provider = build_provider(cfg)
    cc_cfg = cfg.get("cover_cache") or {}
    cache_root = cc_cfg.get("dir") or cfg.get("cover_cache_dir") or ".cover-cache"
    cache_age = cc_cfg.get("max_age_seconds", 7 * 24 * 3600)
    cover_cache = CoverCache(cache_root, cache_age)
    ledger_cfg = cfg.get("ledger") or {}
    ledger_path = ledger_cfg.get("path") or os.path.join(cache_root, "sync-ledger.db")
    # A ledger inside volatile storage (e.g. /tmp) loses its rev history
    # on reboot, which would corrupt every device cursor.  When the
    # *default* path lands there, relocate next to the config file so
    # the ledger survives; an explicitly configured path is respected
    # but warned about.
    tmpdir = tempfile.gettempdir()
    if os.path.realpath(os.path.dirname(ledger_path)).startswith(tmpdir):
        if ledger_cfg.get("path"):
            sys.stderr.write(
                f"ledger: configured path {ledger_path} is volatile "
                f"({tmpdir}); sync history will not survive a reboot\n"
            )
        else:
            relocated = (
                os.path.join(
                    os.path.dirname(config_path),
                    f"{os.path.basename(config_path)}-ledger.db",
                )
                if config_path
                else os.path.join(os.getcwd(), "sync-ledger.db")
            )
            sys.stderr.write(
                f"ledger: default path {ledger_path} is volatile "
                f"({tmpdir}); relocating to {relocated}\n"
            )
            ledger_path = relocated
            if os.path.realpath(os.path.dirname(ledger_path)).startswith(tmpdir):
                # The config itself lives in volatile storage; there is
                # nowhere durable to go.  Warn and proceed anyway.
                sys.stderr.write(
                    f"ledger: final path {ledger_path} is volatile "
                    f"({tmpdir}); sync history will not survive a reboot\n"
                )
    try:
        os.makedirs(os.path.dirname(ledger_path) or ".", exist_ok=True)
        ledger: SyncLedger | None = SyncLedger(
            ledger_path,
            ack_empty_catalogue=bool(ledger_cfg.get("ack_empty_catalogue", False)),
        )
    except sqlite3.Error as exc:
        sys.stderr.write(f"ledger: cannot open {ledger_path}: {exc}\n")
        ledger = None
    state = _AppState(
        config=cfg,
        provider=provider,
        cover_cache=cover_cache,
        open_with=cfg.get("open_with") or {},
        ledger=ledger,
        ledger_max_age=float(ledger_cfg.get("refresh_max_age_s", 30.0)),
    )
    return state


class _ThreadingHTTPServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def _apply_runtime_overrides(cfg: dict[str, Any], args: Any) -> dict[str, Any]:
    """Layer CLI flag and env-var overrides on top of the loaded config.

    Resolution order, highest priority first:
        1. CLI flag   (--provider, --host, --port)
        2. Env var    (PBEMU_PROVIDER, PBEMU_HOST, PBEMU_PORT)
        3. Config file (api/config/server.json)

    Each level only overrides the field if it actually sets a value.
    The mutated config dict is returned for chaining.

    Provider-specific fields can be overridden via env vars of the form
    ``PBEMU_<PROVIDER>_<KEY>`` (uppercased).  For example, to point at a
    different Kavita instance without editing the config file::

        PBEMU_PROVIDER=kavita \\
        PBEMU_KAVITA_BASE_URL=https://kavita.example.com \\
        PBEMU_KAVITA_API_KEY=<your-kavita-api-key> \\
        python -m api.api.server

    The value is taken as a literal string; for booleans, use ``1`` /
    ``0`` / ``true`` / ``false``.
    """
    overrides = {
        "host": (args.host, os.environ.get("PBEMU_HOST")),
        "port": (args.port, os.environ.get("PBEMU_PORT")),
        "provider": (args.provider, os.environ.get("PBEMU_PROVIDER")),
    }
    for key, (cli_val, env_val) in overrides.items():
        if cli_val is not None:
            cfg[key] = cli_val
        elif env_val:
            cfg[key] = env_val

    # Provider-specific env-var overrides.  Scoped to whatever provider
    # is currently selected (CLI/env or config file).
    active = str(cfg.get("provider") or "mock").lower()
    provider_cfg = cfg.setdefault("providers", {}).setdefault(active, {})
    prefix = f"PBEMU_{active.upper()}_"
    for env_key, env_val in os.environ.items():
        if not env_key.startswith(prefix):
            continue
        field = env_key[len(prefix) :].lower()
        provider_cfg[field] = _coerce_env_value(env_val)
    return cfg


def _coerce_env_value(raw: str) -> Any:
    """Parse a string env-var value into the most likely Python type.

    Recognised tokens (case-insensitive): ``1`` / ``true`` / ``yes`` ->
    True; ``0`` / ``false`` / ``no`` -> False; integer literals stay
    int.  Anything else falls back to the raw string.
    """
    lowered = raw.strip().lower()
    if lowered in ("1", "true", "yes", "on"):
        return True
    if lowered in ("0", "false", "no", "off"):
        return False
    try:
        return int(raw)
    except ValueError:
        return raw


def _warm_covers(app: Any) -> None:
    """Background pre-heat: process every cover once so the very first
    sync's cover fetches already hit the cache.

    Runs in a daemon thread, never on the request path.  Walks the
    provider's full catalogue (no cap) chunk by chunk; each book is
    handled independently so one undecodable cover can't stall the rest,
    and already-processed books are skipped (idempotent re-runs).
    """
    cache = app.cover_cache
    provider = app.provider
    done = 0
    try:
        chunks = provider.walk_books(mode="all", chunk_size=500)
        for chunk in chunks:
            for meta in chunk:
                try:
                    if not cache.has_png(meta.id):
                        raw = provider.get_cover(meta.id)
                        if raw:
                            cache.process_and_store(meta.id, raw)
                except Exception as exc:  # noqa: BLE001
                    sys.stderr.write(f"cover warm-up: {meta.id} failed: {exc}\n")
                done += 1
            sys.stderr.write(f"cover warm-up: {done} processed\n")
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write(f"cover warm-up: walk_books failed: {exc}\n")
        return
    sys.stderr.write(f"cover warm-up: done {done}\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="api.api.server")
    parser.add_argument(
        "--host", default=None, help="bind host (env: PBEMU_HOST, default: from config)"
    )
    parser.add_argument(
        "--port",
        type=int,
        default=None,
        help="bind port (env: PBEMU_PORT, default: from config)",
    )
    parser.add_argument(
        "--provider",
        default=None,
        help="content provider name, e.g. mock or kavita "
        "(env: PBEMU_PROVIDER, default: from config)",
    )
    parser.add_argument(
        "--config",
        default=None,
        help="path to server.json (default: api/config/server.json)",
    )
    args = parser.parse_args(argv)
    cfg = load_config(args.config) if args.config else load_config()
    cfg = _apply_runtime_overrides(cfg, args)
    config_path = args.config or (
        DEFAULT_CONFIG_PATH if os.path.isfile(DEFAULT_CONFIG_PATH) else None
    )
    app = build_default_app(cfg, config_path=config_path)
    RequestHandler = type(
        "RequestHandler",
        (PbemuAPIServer,),
        {"app": app},
    )
    server = _ThreadingHTTPServer((cfg["host"], int(cfg["port"])), RequestHandler)
    sys.stderr.write(
        f"pbemu-api: listening on http://{cfg['host']}:{cfg['port']} "
        f"(provider={app.provider.name})\n"
    )
    # Pre-heat covers in the background so the first sync's cover fetches
    # hit the cache; the request path stays fast regardless.
    threading.Thread(target=_warm_covers, args=(app,), daemon=True).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
