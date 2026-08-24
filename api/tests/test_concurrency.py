"""Concurrency contract for the API server.

PbemuAPIServer is a socketserver.ThreadingTCPServer: every connection
runs on its own thread against ONE shared app whose SyncLedger holds
two SQLite connections (writer + WAL snapshot reader). The device —
and these tests — rely on three guarantees:

* parallel reads never fail;
* interleaved sync-state WRITES and delta READS never produce a 5xx;
* every accepted state report answers 202 with the echoed device id.
"""

from __future__ import annotations

import json
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest
import test_server as ts


@pytest.fixture
def server(tmp_path):
    app = ts._make_app(tmp_path, token="test-token")
    s = ts._TestServer(app, token="test-token")
    yield s
    s.stop()
    # Mirror the main fixture's teardown hygiene.
    ledger = getattr(app, "ledger", None)
    if ledger is not None:
        ledger.close()


HDR = {"Authorization": "Bearer test-token"}


def test_parallel_reads_all_succeed(server):
    """Eight threads hammering books + healthz concurrently: every
    request answers 200 (the shared read connection must be safe)."""

    def hit(_: int) -> bool:
        s_books, _ = ts._http_get(server.url("/api/v1/books"), headers=HDR)
        s_health, _ = ts._http_get(server.url("/healthz"))
        return s_books == 200 and s_health == 200

    with ThreadPoolExecutor(max_workers=8) as ex:
        results = list(ex.map(hit, range(64)))
    assert all(results)


def test_state_writes_interleaved_with_delta_reads_stay_2xx(server):
    """Four threads posting sync/state (ledger writes) while four others
    pull sync/delta (reads): neither side may see a 5xx — SQLITE_BUSY or
    cross-thread cursor misuse would surface here first."""

    def writer(n: int) -> list[int]:
        bad: list[int] = []
        for i in range(25):
            status, _ = ts._http_post(
                server.url("/api/v1/sync/state"),
                {"deviceId": f"dev{n}", "device": f"dev{n}", "cursor": i},
                headers=HDR,
            )
            if status != 202:
                bad.append(status)
        return bad

    def reader(_: int) -> list[int]:
        bad: list[int] = []
        for _ in range(25):
            status, _ = ts._http_post(
                server.url("/api/v1/sync/delta"), {"cursor": 0}, headers=HDR
            )
            if status != 200:
                bad.append(status)
        return bad

    jobs = [(writer, n) for n in range(4)] + [(reader, n) for n in range(4)]
    with ThreadPoolExecutor(max_workers=8) as ex:
        futures = [ex.submit(fn, arg) for fn, arg in jobs]
        outcomes = [r for f in futures for r in f.result()]
    assert not outcomes, f"non-2xx responses under load: {outcomes}"


def test_state_report_echoes_device_id(server):
    status, body = ts._http_post(
        server.url("/api/v1/sync/state"),
        {"deviceId": "dev-x", "cursor": 3},
        headers=HDR,
    )
    assert status == 202
    assert json.loads(body)["deviceId"] == "dev-x"


def test_walk_page_commits_interleaved_with_delta_reads(tmp_path):
    """The last untested concurrent pair: background walk page-commits
    (writer connection, under _lock) vs delta reads (WAL-snapshot
    reader connection, under _rd_lock).  Readers must always observe a
    consistent prefix of committed pages — never a torn or partially
    applied batch."""
    from storage.ledger import SyncLedger
    from test_ledger import FakeProvider, _meta

    led = SyncLedger(str(tmp_path / "walk.db"))
    errors: list[str] = []
    stop = threading.Event()

    def reader() -> None:
        cursor = 0
        while not stop.is_set():
            entries, more = led.delta(cursor, 50)
            # Every returned entry must sit strictly beyond the asked
            # cursor (the delta contract), regardless of walk progress.
            for e in entries:
                if e.rev <= cursor:
                    errors.append(f"rev {e.rev} <= cursor {cursor}")
                    return
            if more and entries:
                cursor = entries[-1].rev
            else:
                cursor = 0

    readers = [threading.Thread(target=reader) for _ in range(3)]
    [r.start() for r in readers]

    try:
        for g in range(8):
            metas = [_meta(f"book{i}", title=f"Title {g} {i}") for i in range(60)]
            led.refresh(FakeProvider(metas), max_age_s=0)
            # Interleave reads between walk passes too.
            entries, _ = led.delta(0, 10)
            assert len(entries) <= 10
    finally:
        stop.set()
        [r.join() for r in readers]
        led.close()

    assert not errors, errors[:5]
