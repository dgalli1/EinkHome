"""HTTP smoke tests for the API server.

Run with:
    python -m pytest api/tests/ -v
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import json
import os
import socketserver
import sys
import threading
import time
from urllib import request

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.api.server import PbemuAPIServer, build_default_app  # noqa: E402


def _make_app(tmp_path, token="test-token", suggestions=True):
    (tmp_path / "Test.epub").write_bytes(b"abc")
    cfg = {
        "host": "127.0.0.1",
        "port": 0,
        "api_token": token,
        "provider": "mock",
        "suggestions": suggestions,
        "providers": {
            "mock": {
                "kind": "mock",
                "books_dir": str(tmp_path),
                "library_name": "test lib",
            }
        },
        "cover_cache_dir": str(tmp_path / ".cover-cache"),
        "ledger": {
            "path": str(tmp_path / "sync-ledger.db"),
            "refresh_max_age_s": 0,
        },
    }
    return build_default_app(cfg)


class _TestServer:
    def __init__(self, app, token="test-token"):
        self.app = app
        RequestHandler = type(
            "RequestHandler",
            (PbemuAPIServer,),
            {"app": app},
        )
        self.httpd = socketserver.ThreadingTCPServer(("127.0.0.1", 0), RequestHandler)
        self.httpd.daemon_threads = True
        self.port = self.httpd.server_address[1]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()
        self.token = token

    def url(self, path):
        return f"http://127.0.0.1:{self.port}{path}"

    def stop(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join()
        # The app owns a SyncLedger (two SQLite connections); release it
        # here or every server test leaks ResourceWarnings at gc time.
        ledger = getattr(self.app, "ledger", None)
        if ledger is not None:
            ledger.close()


@pytest.fixture
def server(tmp_path):
    app = _make_app(tmp_path, token="test-token")
    s = _TestServer(app, token="test-token")
    yield s
    s.stop()


def _http_get(url, headers=None):
    try:
        req = request.Request(url, headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8")
    except request.HTTPError as e:
        return e.code, e.read().decode("utf-8", errors="replace")


def _http_get_bytes(url, headers=None):
    try:
        req = request.Request(url, headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read()
    except request.HTTPError as e:
        return e.code, e.read()


def _http_post_bytes(url, headers=None):
    """POST with an empty body (the libinkview QuickDownload shape),
    returning the raw response bytes."""
    try:
        req = request.Request(url, data=b"", method="POST", headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read()
    except request.HTTPError as e:
        return e.code, e.read()


def _http_post(url, body, headers=None):
    data = json.dumps(body).encode("utf-8")
    return _http_post_raw(url, data, headers=headers)


def _http_post_raw(url, raw_body, headers=None):
    try:
        req = request.Request(
            url,
            data=raw_body,
            method="POST",
            headers={"Content-Type": "application/json", **(headers or {})},
        )
        with request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8")
    except request.HTTPError as e:
        return e.code, e.read().decode("utf-8", errors="replace")


def _http_put(url, headers=None):
    try:
        req = request.Request(url, method="PUT", headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8")
    except request.HTTPError as e:
        return e.code, e.read().decode("utf-8", errors="replace")


def _http_head(url, headers=None):
    try:
        req = request.Request(url, method="HEAD", headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read(), dict(r.headers)
    except request.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def _http_get_headers(url, headers=None):
    try:
        req = request.Request(url, headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read(), dict(r.headers)
    except request.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def _json_or_default(body, default):
    try:
        return json.loads(body)
    except (json.JSONDecodeError, ValueError):
        return default


def test_health_endpoint(server):
    # /healthz is public — liveness probes carry no token — and reports
    # the server's pid so test harnesses can detect stale servers.
    status, body = _http_get(server.url("/healthz"))
    assert status == 200
    data = _json_or_default(body, {})
    assert data["status"] == "ok"
    assert data["pid"] == os.getpid()


def test_libraries_requires_auth(server):
    status, _ = _http_get(server.url("/api/v1/libraries"))
    assert status == 401


def test_libraries_with_token(server):
    status, body = _http_get(
        server.url("/api/v1/libraries"),
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert data["count"] == 1
    assert data["items"][0]["name"] == "test lib"


def test_books_endpoint(server):
    status, body = _http_get(
        server.url("/api/v1/books"),
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert "items" in data
    assert len(data["items"]) == 1
    assert data["items"][0]["title"] == "Test"


def test_sync_delta_cursor_protocol(server, tmp_path):
    hdr = {"Authorization": "Bearer test-token"}
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": 0, "limit": 500}, headers=hdr
    )
    assert status == 200
    data = json.loads(body)
    assert [b["title"] for b in data["added"]] == ["Test"]
    assert data["removed"] == []
    assert data["more"] is False
    cursor = data["nextCursor"]
    assert cursor >= 1

    # A new book appears in the next delta batch.
    (tmp_path / "Second.epub").write_bytes(b"xyz")
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": cursor}, headers=hdr
    )
    assert status == 200
    data = json.loads(body)
    assert [b["title"] for b in data["added"]] == ["Second"]
    cursor = data["nextCursor"]

    # Removing it produces a tombstone the device replays as a delete.
    (tmp_path / "Second.epub").unlink()
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": cursor}, headers=hdr
    )
    assert status == 200
    data = json.loads(body)
    assert data["added"] == []
    assert len(data["removed"]) == 1
    assert data["more"] is False


def test_sync_delta_carries_suggest_terms(server, tmp_path):
    """Each delta `added` entry carries the book's suggestion terms,
    including word-aligned suffix phrases from the title."""
    hdr = {"Authorization": "Bearer test-token"}
    (tmp_path / "Harry Potter.epub").write_bytes(b"abc")
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": 0, "limit": 500}, headers=hdr
    )
    assert status == 200
    data = json.loads(body)
    hp = [b for b in data["added"] if b["title"] == "Harry Potter"]
    assert hp, "Harry Potter book missing from delta"
    suggest = hp[0]["suggest"]
    assert "potter" in suggest
    assert "harry potter" in suggest  # word-aligned suffix phrase
    assert "harry potter" in suggest and suggest.index("potter") < suggest.index(
        "harry potter"
    )
    # Every term is folded, deduped and bounded.
    assert suggest == sorted(set(suggest), key=suggest.index)
    assert all(t == t.casefold() for t in suggest)
    assert len(suggest) <= 96
    assert all(len(t) <= 79 for t in suggest)


def test_sync_delta_suggestions_disabled_flag(tmp_path):
    """With the top-level ``suggestions`` config flag off, delta
    entries carry an empty term list — the device's suggest index
    stays empty and the search UI falls back to history rows.
    ``searchText`` is kept: it also serves folded manual search."""
    hdr = {"Authorization": "Bearer test-token"}
    (tmp_path / "Harry Potter.epub").write_bytes(b"abc")
    app = _make_app(tmp_path, suggestions=False)
    srv = _TestServer(app)
    try:
        status, body = _http_post(
            srv.url("/api/v1/sync/delta"), {"cursor": 0, "limit": 500}, headers=hdr
        )
        assert status == 200
        data = json.loads(body)
        hp = [b for b in data["added"] if b["title"] == "Harry Potter"]
        assert hp, "Harry Potter book missing from delta"
        assert hp[0]["suggest"] == []
        assert "potter" in hp[0]["searchText"]  # folded blob kept
    finally:
        srv.stop()


def test_open_with_returns_app(server):
    books_resp = _json_or_default(
        _http_get(
            server.url("/api/v1/books"),
            headers={"Authorization": "Bearer test-token"},
        )[1],
        {},
    )
    book_id = books_resp["items"][0]["id"]
    status, body = _http_post(
        server.url("/api/v1/open-with"),
        {"id": book_id, "ext": "epub"},
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert data["app"] in {"eink-reader", "bookshelf"}
    assert data["url"].startswith("/api/v1/books/")
    assert data["ext"] == "epub"


def test_cover_endpoint(server):
    books_resp = _json_or_default(
        _http_get(
            server.url("/api/v1/books"),
            headers={"Authorization": "Bearer test-token"},
        )[1],
        {},
    )
    cover_url = books_resp["items"][0]["cover"]
    full = f"{server.url(cover_url)}?access_token=test-token"
    status, body_bytes = _http_get_bytes(full)
    assert status == 200
    # Mock provider serves the 1x1 PNG placeholder, so the cover endpoint
    # returns PNG here (real providers serve the processed JPEG).
    assert body_bytes.startswith(b"\x89PNG")  # placeholder PNG magic


def test_file_download(server):
    books_resp = _json_or_default(
        _http_get(
            server.url("/api/v1/books"),
            headers={"Authorization": "Bearer test-token"},
        )[1],
        {},
    )
    file_url = books_resp["items"][0]["url"]
    full = f"{server.url(file_url)}?access_token=test-token"
    status, body_bytes = _http_get_bytes(full)
    assert status == 200
    assert body_bytes == b"abc"


def test_wrong_token_rejected(server):
    status, _ = _http_get(
        server.url("/api/v1/libraries"),
        headers={"Authorization": "Bearer wrong-token"},
    )
    assert status == 401


def test_unknown_endpoint_404(server):
    hdr = {"Authorization": "Bearer test-token"}
    status, body = _http_get(server.url("/api/v1/nope"), headers=hdr)
    assert status == 404
    assert _json_or_default(body, {})["error"] == "not found"


def test_unknown_book_id_404(server):
    hdr = {"Authorization": "Bearer test-token"}
    status, body = _http_get(server.url("/api/v1/books/does-not-exist"), headers=hdr)
    assert status == 404
    assert _json_or_default(body, {})["id"] == "does-not-exist"
    # The file endpoint reports the same way.
    status, body = _http_get(
        server.url("/api/v1/books/does-not-exist/file"), headers=hdr
    )
    assert status == 404


def test_put_books_is_405(server):
    """PUT is not part of the surface — the old GET alias is gone."""
    status, body = _http_put(
        server.url("/api/v1/books"),
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 405
    data = _json_or_default(body, {})
    assert isinstance(data.get("error"), str) and data["error"]


def test_malformed_json_is_lenient(server):
    """Malformed JSON POST bodies fall back to an empty body (current
    behavior): the delta proceeds from cursor 0 with the default limit."""
    hdr = {"Authorization": "Bearer test-token"}
    status, body = _http_post_raw(
        server.url("/api/v1/sync/delta"), b"{definitely not json", headers=hdr
    )
    assert status == 200
    data = json.loads(body)
    assert [b["title"] for b in data["added"]] == ["Test"]


def test_sync_delta_cursor_clamps(server):
    hdr = {"Authorization": "Bearer test-token"}
    # Negative cursor behaves like 0: the whole catalogue, in rev order.
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": -1}, headers=hdr
    )
    assert status == 200
    negative = json.loads(body)
    assert [b["title"] for b in negative["added"]] == ["Test"]
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": 0}, headers=hdr
    )
    assert json.loads(body)["added"] == negative["added"]

    # A cursor past the highest rev is an empty (but valid) batch.
    status, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": 10**12}, headers=hdr
    )
    data = json.loads(body)
    assert data["added"] == [] and data["removed"] == []
    assert data["more"] is False
    assert data["nextCursor"] == 10**12


def test_sync_delta_limit_clamped(server, tmp_path):
    hdr = {"Authorization": "Bearer test-token"}
    (tmp_path / "Two.epub").write_bytes(b"x")
    # Negative limit clamps to 1: exactly one entry per batch.
    _, body = _http_post(
        server.url("/api/v1/sync/delta"), {"cursor": 0, "limit": -5}, headers=hdr
    )
    data = json.loads(body)
    assert len(data["added"]) == 1
    assert data["more"] is True


def test_books_limit_and_offset_clamped(server, tmp_path):
    hdr = {"Authorization": "Bearer test-token"}
    (tmp_path / "Two.epub").write_bytes(b"x")
    (tmp_path / "Three.epub").write_bytes(b"x")
    _, body = _http_get(server.url("/api/v1/books?limit=99999"), headers=hdr)
    data = json.loads(body)
    assert data["limit"] == 2000  # huge limit clamped
    assert data["count"] == 3
    _, body = _http_get(server.url("/api/v1/books?limit=-3"), headers=hdr)
    assert json.loads(body)["limit"] == 1
    _, body = _http_get(server.url("/api/v1/books?offset=-5"), headers=hdr)
    data = json.loads(body)
    assert data["offset"] == 0
    assert data["count"] == 3


def test_books_hasmore_on_exact_last_page(server, tmp_path):
    """A page that exactly fills the limit is the last page: hasMore
    must be False, not True."""
    hdr = {"Authorization": "Bearer test-token"}
    (tmp_path / "Two.epub").write_bytes(b"x")
    (tmp_path / "Three.epub").write_bytes(b"x")
    _, body = _http_get(server.url("/api/v1/books?limit=3"), headers=hdr)
    data = json.loads(body)
    assert data["count"] == 3
    assert data["hasMore"] is False

    # One more book than the page: hasMore flips, items stay bounded.
    (tmp_path / "Four.epub").write_bytes(b"x")
    _, body = _http_get(server.url("/api/v1/books?limit=3"), headers=hdr)
    data = json.loads(body)
    assert data["count"] == 3
    assert data["hasMore"] is True


def test_cover_etag_304(server):
    """If-None-Match matching the cover's ETag yields an empty 304."""
    books = _json_or_default(
        _http_get(
            server.url("/api/v1/books"),
            headers={"Authorization": "Bearer test-token"},
        )[1],
        {},
    )
    cover_url = books["items"][0]["cover"]
    url = f"{server.url(cover_url)}?access_token=test-token"
    status, body, hdrs = _http_get_headers(url)
    assert status == 200
    etag = hdrs.get("ETag")
    assert etag
    assert hdrs.get("Cache-Control") == "public, max-age=3600"
    status, body, hdrs2 = _http_get_headers(url, headers={"If-None-Match": etag})
    assert status == 304
    assert body == b""
    assert hdrs2.get("ETag") == etag
    assert hdrs2.get("Cache-Control") == "public, max-age=3600"


def test_head_healthz(server):
    """HEAD on the public healthz endpoint: 200 headers, empty body,
    Content-Length mirroring the GET payload."""
    status, body, hdrs = _http_head(server.url("/healthz"))
    assert status == 200
    assert body == b""
    _, get_body = _http_get(server.url("/healthz"))
    assert int(hdrs.get("Content-Length", "0")) == len(get_body)


# --- query-param auth (?access_token=) ---------------------------------


def test_query_param_auth_accepted(server):
    """A correct ?access_token= authenticates without any Authorization
    header — the cover loader on the device cannot re-attach headers."""
    status, body = _http_get(server.url("/api/v1/libraries?access_token=test-token"))
    assert status == 200
    assert _json_or_default(body, {})["count"] == 1


def test_query_param_wrong_token_401(server):
    status, body = _http_get(server.url("/api/v1/libraries?access_token=wrong"))
    assert status == 401
    assert _json_or_default(body, {})["error"] == "unauthorized"


def test_query_param_empty_token_401(server):
    """A blank access_token must not authenticate (dev mode is opt-in
    via config, not via an empty credential)."""
    status, body = _http_get(server.url("/api/v1/libraries?access_token="))
    assert status == 401
    assert _json_or_default(body, {})["error"] == "unauthorized"


def test_query_token_never_logged(server, capsys):
    """log_request drops the query string so the token stays off stderr."""
    _http_get(server.url("/api/v1/libraries?access_token=test-token"))
    err = capsys.readouterr().err
    assert "test-token" not in err
    assert '"GET /api/v1/libraries"' in err


# --- HEAD semantics -----------------------------------------------------


def test_head_matches_get_status_and_length(server):
    hdr = {"Authorization": "Bearer test-token"}
    get_status, get_body, _ = _http_get_headers(
        server.url("/api/v1/libraries"), headers=hdr
    )
    head_status, head_body, head_hdrs = _http_head(
        server.url("/api/v1/libraries"), headers=hdr
    )
    assert head_status == get_status
    assert head_body == b""
    assert int(head_hdrs["Content-Length"]) == len(get_body)


def test_head_file_is_405(server):
    """HEAD cannot stream a file without executing the handler twice,
    so it is refused rather than faked (pinned actual behavior)."""
    hdr = {"Authorization": "Bearer test-token"}
    _, body = _http_get(server.url("/api/v1/books"), headers=hdr)
    book_id = _json_or_default(body, {"items": [{}]})["items"][0]["id"]
    status, body, head_hdrs = _http_head(
        server.url(f"/api/v1/books/{book_id}/file"), headers=hdr
    )
    assert status == 405
    # Body is suppressed like any HEAD response, but the headers still
    # advertise the JSON error payload's length (pinned actual shape).
    assert head_hdrs.get("Content-Type", "").startswith("application/json")
    assert int(head_hdrs.get("Content-Length", "0")) > 0


# --- POST parity for cover / file ---------------------------------------


def _first_book_id(server):
    hdr = {"Authorization": "Bearer test-token"}
    _, body = _http_get(server.url("/api/v1/books"), headers=hdr)
    return _json_or_default(body, {"items": [{}]})["items"][0]["id"]


def test_post_cover_returns_image_bytes(server):
    """POST /books/{id}/cover serves the image like GET — libinkview's
    QuickDownload issues POSTs on this firmware."""
    book_id = _first_book_id(server)
    status, body = _http_post_bytes(
        server.url(f"/api/v1/books/{book_id}/cover"),
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    # Placeholder covers are PNG; processed ones are JPEG.
    assert body.startswith(b"\x89PNG") or body.startswith(b"\xff\xd8\xff")


def test_post_file_streams_epub_bytes(server):
    book_id = _first_book_id(server)
    status, body = _http_post_bytes(
        server.url(f"/api/v1/books/{book_id}/file"),
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    assert body == b"abc"


def test_post_cover_file_require_auth(server):
    book_id = _first_book_id(server)
    assert _http_post_bytes(server.url(f"/api/v1/books/{book_id}/cover"))[0] == 401
    assert _http_post_bytes(server.url(f"/api/v1/books/{book_id}/file"))[0] == 401


# --- open-with resolution -----------------------------------------------


def test_open_with_missing_id_400(server):
    status, body = _http_post(
        server.url("/api/v1/open-with"),
        {},
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 400
    assert _json_or_default(body, {})["error"] == "missing id"


def test_open_with_uppercase_ext_case_insensitive(server):
    # The fixture's app owns no open_with table; install one directly.
    server.app.open_with = {
        "epub": ["eink-reader", "alt-reader"],
        "default": ["def-app"],
    }
    book_id = _first_book_id(server)
    status, body = _http_post(
        server.url("/api/v1/open-with"),
        {"id": book_id, "ext": "EPUB"},
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert data["app"] == "eink-reader"
    assert data["alternates"] == ["alt-reader"]
    assert data["ext"] == "epub"


def test_open_with_unknown_ext_falls_back_to_default(server):
    server.app.open_with = {
        "epub": ["eink-reader"],
        "default": ["def-app", "def-alt"],
    }
    book_id = _first_book_id(server)
    status, body = _http_post(
        server.url("/api/v1/open-with"),
        {"id": book_id, "ext": "mobi"},
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert data["app"] == "def-app"
    assert data["alternates"] == ["def-alt"]


def test_open_with_ext_resolved_from_book_metadata(server):
    """Without an explicit ext the resolver falls back to the provider's
    file_format for the book (the mock reports "epub")."""
    server.app.open_with = {"epub": ["eink-reader"], "default": ["def-app"]}
    book_id = _first_book_id(server)
    status, body = _http_post(
        server.url("/api/v1/open-with"),
        {"id": book_id},
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert data["app"] == "eink-reader"
    assert data["url"].endswith(f"/books/{book_id}/file")


# --- degraded sync ledger -----------------------------------------------


def test_sync_delta_ledger_unavailable_503(tmp_path):
    """When the ledger failed to open the delta endpoint degrades to a
    well-formed 503 instead of crashing the request thread."""
    app = _make_app(tmp_path)
    ledger = app.ledger
    app.ledger = None
    s = _TestServer(app)
    try:
        status, body = _http_post(
            s.url("/api/v1/sync/delta"),
            {"cursor": 0},
            headers={"Authorization": "Bearer test-token"},
        )
        assert status == 503
        data = _json_or_default(body, {})
        assert data["error"] == "sync ledger unavailable"
        assert data["more"] is False
    finally:
        s.stop()
        if ledger is not None:
            ledger.close()


# --- outer crash net ----------------------------------------------------


def test_provider_crash_yields_500_json(server):
    """An exception escaping provider.list_books mid-request still gets
    the client a valid 500 JSON response, not a dropped connection."""

    def boom(*args, **kwargs):
        raise RuntimeError("provider exploded")

    server.app.provider.list_books = boom
    status, body = _http_get(
        server.url("/api/v1/books"), headers={"Authorization": "Bearer test-token"}
    )
    assert status == 500
    assert _json_or_default(body, {})["error"] == "internal server error"


# --- ?since= filter end-to-end ------------------------------------------


def _iso_epoch(t):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def test_books_since_filter_excludes_boundary(server):
    """?since=<iso> excludes books whose updated_at <= since (string
    compare of UTC ISO stamps; the mock derives updated_at from mtime)."""
    mtime = 1768478400  # 2026-01-15T12:00:00Z
    os.utime(os.path.join(server.app.provider.books_dir, "Test.epub"), (mtime, mtime))
    hdr = {"Authorization": "Bearer test-token"}
    exact = _iso_epoch(mtime)
    _, body = _http_get(server.url(f"/api/v1/books?since={exact}"), headers=hdr)
    assert _json_or_default(body, {})["count"] == 0
    before = _iso_epoch(mtime - 1)
    _, body = _http_get(server.url(f"/api/v1/books?since={before}"), headers=hdr)
    assert _json_or_default(body, {})["count"] == 1


def _http_delete(url, headers=None):
    try:
        req = request.Request(url, method="DELETE", headers=headers or {})
        with request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8")
    except request.HTTPError as e:
        return e.code, e.read().decode("utf-8", errors="replace")


def test_delete_book_drops_it_from_listings_and_get(server):
    """DELETE /books/{id} ("delete from cloud"): the mock tombstones the
    id — gone from listings, GET 404s, re-delete 404s."""
    hdr = {"Authorization": "Bearer test-token"}
    _, body = _http_get(server.url("/api/v1/books?limit=5"), headers=hdr)
    items = json.loads(body)["items"]
    assert items, "mock library must not be empty"
    book_id = items[0]["id"]

    status, body = _http_delete(server.url(f"/api/v1/books/{book_id}"), hdr)
    assert status == 200
    assert json.loads(body)["deleted"] is True

    _, body = _http_get(server.url("/api/v1/books?limit=50"), headers=hdr)
    ids = [b["id"] for b in json.loads(body)["items"]]
    assert book_id not in ids

    status, _ = _http_get(server.url(f"/api/v1/books/{book_id}"), headers=hdr)
    assert status == 404

    status, _ = _http_delete(server.url(f"/api/v1/books/{book_id}"), hdr)
    assert status == 404


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
