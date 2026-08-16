"""E2E tests through the HTTP server, pointed at a live Kavita.

These tests stand up an in-process copy of the pbemu-api server
configured to use the Kavita provider and then drive it the same way
the in-emulator C app drives it: bearer-token auth, ?access_token=
on cover/file URLs, sync/delta, open-with.

Run with:

    export KAVITA_E2E_URL=https://kavita.example.com
    export KAVITA_E2E_API_KEY=<your-kavita-api-key>
    pytest api/tests/e2e/test_kavita_server.py -v
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import json
import os
import socketserver
import sys
import threading
from urllib import error, request

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if REPO_ROOT not in sys.path:
    sys.path.insert(0, REPO_ROOT)
if os.path.join(REPO_ROOT, "api") not in sys.path:
    sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.api.server import PbemuAPIServer, build_default_app  # noqa: E402

from .conftest import (  # noqa: E402
    KAVITA_API_KEY,
    KAVITA_PASS,
    KAVITA_TIMEOUT,
    KAVITA_URL,
    KAVITA_USER,
    SKIP_NO_AUTH,
    SKIP_NO_URL,
    SKIP_UNREACHABLE,
)

API_TOKEN = "pbemu-e2e-token"


def _http_get(url, headers=None, timeout=30):
    try:
        req = request.Request(url, headers=headers or {})
        with request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except error.HTTPError as e:
        return e.code, e.read()


def _http_post(url, body, headers=None, timeout=30):
    data = json.dumps(body).encode("utf-8")
    try:
        req = request.Request(
            url,
            data=data,
            method="POST",
            headers={"Content-Type": "application/json", **(headers or {})},
        )
        with request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except error.HTTPError as e:
        return e.code, e.read()


def _json_or(body, default):
    try:
        return json.loads(body)
    except (json.JSONDecodeError, ValueError):
        return default


# -- server fixture --------------------------------------------------------


@pytest.fixture(scope="module")
def live_server():
    """Boot the pbemu-api server pointed at live Kavita."""
    if not KAVITA_URL or not (KAVITA_API_KEY or (KAVITA_USER and KAVITA_PASS)):
        pytest.skip("live Kavita env vars missing")
    cfg = {
        "host": "127.0.0.1",
        "port": 0,
        "api_token": API_TOKEN,
        "provider": "kavita",
        "providers": {
            "kavita": {
                "kind": "kavita",
                "base_url": KAVITA_URL,
                "api_key": KAVITA_API_KEY,
                "username": KAVITA_USER,
                "password": KAVITA_PASS,
                "verify_tls": True,
                "timeout": KAVITA_TIMEOUT,
            }
        },
    }
    app = build_default_app(cfg)

    # Verify connectivity/auth lazily at runtime so a transient blip fails
    # the suite loudly (env vars are configured by now) instead of silently
    # skipping all coverage.
    h = app.provider.health()
    if not h.get("ok"):
        pytest.fail(
            f"Kavita host {KAVITA_URL!r} is configured (KAVITA_E2E_URL set) "
            f"but not usable: {h.get('detail')}"
        )

    RequestHandler = type("RequestHandler", (PbemuAPIServer,), {"app": app})
    httpd = socketserver.ThreadingTCPServer(("127.0.0.1", 0), RequestHandler)
    httpd.daemon_threads = True
    port = httpd.server_address[1]
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join(timeout=5)


def _auth_headers():
    return {"Authorization": f"Bearer {API_TOKEN}"}


# -- HTTP surface ----------------------------------------------------------


@SKIP_NO_URL
@SKIP_UNREACHABLE
def test_server_health_endpoint(live_server):
    """Healthz endpoint is public (liveness probes carry no token) and
    reports the active provider name plus the server pid."""
    status, body = _http_get(f"{live_server}/api/v1/healthz", timeout=15)
    assert status == 200, body
    data = _json_or(body, {})
    assert data["status"] == "ok"
    assert data["provider"] == "kavita"
    assert isinstance(data.get("pid"), int)


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_libraries_requires_auth(live_server):
    status, _ = _http_get(f"{live_server}/api/v1/libraries", timeout=10)
    assert status == 401


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_libraries_returns_real_library(live_server):
    status, body = _http_get(
        f"{live_server}/api/v1/libraries", headers=_auth_headers(), timeout=15
    )
    assert status == 200
    data = _json_or(body, {})
    assert data["count"] >= 1
    assert any(item["name"] for item in data["items"])


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_books_endpoint_paginates(live_server):
    status, body = _http_get(
        f"{live_server}/api/v1/books?limit=5",
        headers=_auth_headers(),
        timeout=30,
    )
    assert status == 200
    data = _json_or(body, {})
    assert 1 <= data["count"] <= 5
    items = data["items"]
    for b in items:
        assert b["id"].startswith("kavita_ch_")
        assert b["title"]
        assert b["format"] in {"epub", "pdf", "cbz", "cbr"}
        # cover + url must be paths the device can fetch
        assert b["cover"].startswith("/api/v1/books/")
        assert b["url"].startswith("/api/v1/books/")
    # Series index must be set for the first 5 epubs we get back.
    for b in items:
        if b["format"] == "epub":
            assert b["seriesIdx"] is not None
            assert b["seriesIdx"] > 0


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_sync_delta_returns_books(live_server):
    status, body = _http_post(
        f"{live_server}/api/v1/sync/delta",
        {"known": []},
        headers=_auth_headers(),
        timeout=60,
    )
    assert status == 200
    data = _json_or(body, {})
    assert data["provider"] == "kavita"
    assert data["serverTime"]
    assert isinstance(data["added"], list) and len(data["added"]) >= 1


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_sync_delta_cursor_protocol(live_server):
    """The cursor protocol ignores `known` in sync/delta bodies.

    A first delta with no cursor returns the full library (bounded by
    `limit`); a second delta with cursor=nextCursor returns only newer
    changes; and the `known` list is ignored — the server answers
    identically with or without it.
    """
    limit = 100

    # First delta: no cursor → the whole library, bounded by limit.
    status, body = _http_post(
        f"{live_server}/api/v1/sync/delta",
        {"known": [], "limit": limit},
        headers=_auth_headers(),
        timeout=60,
    )
    assert status == 200, body
    first = _json_or(body, {})
    assert len(first["added"]) <= limit
    assert first["nextCursor"] >= 0
    assert isinstance(first["more"], bool)

    # Second delta from nextCursor: only revs the device hasn't seen.
    # Nothing changed between the two calls, so in a quiet library this
    # is empty — but whatever comes back must not overlap the first
    # batch (revs strictly increase, so a replay is impossible).
    status, body = _http_post(
        f"{live_server}/api/v1/sync/delta",
        {"cursor": first["nextCursor"], "limit": limit},
        headers=_auth_headers(),
        timeout=60,
    )
    assert status == 200, body
    second = _json_or(body, {})
    seen_ids = {b["id"] for b in first["added"]}
    for b in second["added"]:
        assert b["id"] not in seen_ids
    assert all(bid not in seen_ids for bid in second["removed"])
    assert second["nextCursor"] >= first["nextCursor"]

    # `known` is ignored: same cursor, identical response.
    status, body = _http_post(
        f"{live_server}/api/v1/sync/delta",
        {
            "cursor": first["nextCursor"],
            "limit": limit,
            "known": list(seen_ids),
        },
        headers=_auth_headers(),
        timeout=60,
    )
    assert status == 200, body
    third = _json_or(body, {})
    assert third["added"] == second["added"]
    assert third["removed"] == second["removed"]
    assert third["nextCursor"] == second["nextCursor"]
    assert third["more"] == second["more"]


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_cover_endpoint_returns_jpeg(live_server):
    status, body = _http_get(
        f"{live_server}/api/v1/books?limit=1",
        headers=_auth_headers(),
        timeout=30,
    )
    books = _json_or(body, {}).get("items", [])
    assert books, "no books to test cover"
    cover_path = books[0]["cover"]
    # The libinkview image-fetcher uses ?access_token= (no Authorization header).
    sep = "&" if "?" in cover_path else "?"
    url = f"{live_server}{cover_path}{sep}access_token={API_TOKEN}"
    status, body = _http_get(url, timeout=30)
    assert status == 200
    assert body[:3] == b"\xff\xd8\xff", f"cover is not a JPEG (header={body[:3].hex()})"
    assert len(body) > 100


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_file_endpoint_streams_real_epub(live_server):
    status, body = _http_get(
        f"{live_server}/api/v1/books?limit=1",
        headers=_auth_headers(),
        timeout=30,
    )
    books = _json_or(body, {}).get("items", [])
    assert books, "no books"
    book = books[0]
    sep = "&" if "?" in book["url"] else "?"
    url = f"{live_server}{book['url']}{sep}access_token={API_TOKEN}"
    status, body = _http_get(url, timeout=120)
    assert status == 200
    # Real EPUB local file header is "PK\x03\x04"
    assert body[:4] == b"PK\x03\x04", f"file is not an EPUB (header={body[:4].hex()})"
    if book["size"] > 0:
        assert len(body) == book["size"], (
            f"expected {book['size']} bytes, got {len(body)}"
        )


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_open_with_resolves_to_eink_reader(live_server):
    status, body = _http_get(
        f"{live_server}/api/v1/books?limit=1",
        headers=_auth_headers(),
        timeout=30,
    )
    books = _json_or(body, {}).get("items", [])
    assert books
    book = books[0]
    status, body = _http_post(
        f"{live_server}/api/v1/open-with",
        {"id": book["id"], "ext": book["format"]},
        headers=_auth_headers(),
        timeout=30,
    )
    assert status == 200
    data = _json_or(body, {})
    # api/config/server.json maps epub → [eink-reader, bookshelf]
    assert data["app"] in {"eink-reader", "bookshelf"}
    assert data["url"].startswith(f"/api/v1/books/{book['id']}/file")
    assert data["ext"] == book["format"]


@SKIP_NO_AUTH
@SKIP_UNREACHABLE
def test_sync_state_accepts_post(live_server):
    """POST /api/v1/sync/state must return 202 (accepted, no-op)."""
    status, body = _http_post(
        f"{live_server}/api/v1/sync/state",
        {
            "deviceId": "pbemu-e2e",
            "known": ["kavita_ch_00000007"],
            "downloaded": ["kavita_ch_00000007"],
        },
        headers=_auth_headers(),
        timeout=30,
    )
    assert status in {200, 202}, (status, body)
    data = _json_or(body, {})
    assert data.get("ok")
