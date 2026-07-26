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
from urllib import request

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from api.api.server import PbemuAPIServer, build_default_app  # noqa: E402


def _make_app(tmp_path, token="test-token"):
    (tmp_path / "Test.epub").write_bytes(b"abc")
    cfg = {
        "host": "127.0.0.1",
        "port": 0,
        "api_token": token,
        "provider": "mock",
        "providers": {
            "mock": {
                "kind": "mock",
                "books_dir": str(tmp_path),
                "library_name": "test lib",
            }
        },
    }
    return build_default_app(cfg)


class _TestServer:
    def __init__(self, app, token="test-token"):
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


def _http_post(url, body, headers=None):
    data = json.dumps(body).encode("utf-8")
    try:
        req = request.Request(
            url,
            data=data,
            method="POST",
            headers={"Content-Type": "application/json", **(headers or {})},
        )
        with request.urlopen(req) as r:
            return r.status, r.read().decode("utf-8")
    except request.HTTPError as e:
        return e.code, e.read().decode("utf-8", errors="replace")


def _json_or_default(body, default):
    try:
        return json.loads(body)
    except (json.JSONDecodeError, ValueError):
        return default


def test_health_endpoint(server):
    # /healthz requires auth (the server always enforces the bearer
    # token in the current implementation).
    status, body = _http_get(
        server.url("/healthz"),
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert data["status"] == "ok"


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


def test_sync_delta_post(server):
    status, body = _http_post(
        server.url("/api/v1/sync/delta"),
        {"known": []},
        headers={"Authorization": "Bearer test-token"},
    )
    assert status == 200
    data = _json_or_default(body, {})
    assert len(data["added"]) == 1
    assert data["added"][0]["title"] == "Test"


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
    assert body_bytes.startswith(b"\x89PNG")  # PNG magic


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


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
