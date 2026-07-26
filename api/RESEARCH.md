# Kavita API integration — research findings

Date: 2026-06-30
Target: a self-hosted Kavita instance (Kavita 0.9.0.2)

This document records the actual wire format the adapter talks to,
plus the bugs found and fixed while writing the e2e tests.

## Setup

Kavita lives at `https://kavita.example.com`, version 0.9.0.2
(API surface unchanged from 0.8.x).  One library, "Ebooks",
96 series, 93+ book files (mostly epub, some pdf).

The `KavitaProvider` adapter in `api/providers/kavita.py` and the
end-to-end tests in `api/tests/e2e/` cover every endpoint the
device app talks to.

## Endpoints used by the adapter

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/api/Account/login` | JWT login (sends apiKey + username + password) |
| `GET`  | `/api/Library/libraries` | list libraries |
| `POST` | `/api/Series/v2?PageNumber=&PageSize=` | paginated series list |
| `GET`  | `/api/Series/volumes?seriesId=` | volumes + chapters |
| `GET`  | `/api/Chapter?chapterId=` | one chapter incl. `files[]` |
| `GET`  | `/api/Volume?volumeId=` | one volume incl. `seriesId` |
| `GET`  | `/api/Download/chapter?chapterId=` | raw file bytes |
| `GET`  | `/api/Image/chapter-cover?chapterId=&apiKey=` | cover (chapter→series fallback) |
| `GET`  | `/api/Image/series-cover?seriesId=&apiKey=` | cover backstop |

## Bugs found and fixed

### 1. `list_series` response shape mismatch

The adapter was written against a `{"result": [...], "totalCount": ...}`
envelope (the proxy's `kavita_client.py` uses the same shape).

**Reality**: Kavita 0.9.0.2 returns a bare JSON list, not an envelope.

**Fix**: `_KavitaClient.list_series()` now accepts both shapes —
returns `payload` if it's already a list, otherwise `payload["result"]`.

### 2. `list_series` pagination ignored

The adapter sent `{libraryId: 1, page: 0, pageSize: 500}` in the body.

**Reality**: Kavita 0.9.0.2 ignores those keys.  It requires
`?PageNumber=N&PageSize=M` as query parameters and reads `libraries`
(a list) from the body.  Sending the wrong body returned all 96
series in one go.

**Fix**: `_KavitaClient.list_series(library_id, page=1, page_size=50)`
now sends `?PageNumber=&PageSize=` as query parameters and
`{libraries: [library_id], formats: [], ...}` in the body.

### 3. `get_chapter_files` returned empty list for every book

The adapter used `/api/Book/{chapterId}/book-info` and expected a
nested `chapters[].files[]`.

**Reality**: `/api/Book/{id}/book-info` returns a flat book metadata
envelope with `chapters: []` on this server.

**Fix**: `_KavitaClient.get_chapter_files()` now uses
`/api/Chapter?chapterId={id}` and reads `body.files` (the top-level
array).

### 4. `_chapter_to_meta` couldn't resolve seriesId

After the previous fix, every chapter DTO has `volumeId` but
`seriesId: null`.

**Fix**: New helper `_KavitaClient.get_chapter_volume(chapter_id)`
fetches `/api/Volume?volumeId={volumeId}` (the only endpoint that
returns a single volume) and exposes its `seriesId`.  Adapter now
threads the volume into `_chapter_to_meta` so it can use
`volume.number` as the `series_index` (chapter.number is the
sentinel `-100000` on every chapter Kavita emits from
`/api/Series/volumes`).

### 5. `cover_bytes` failed with HTTP 401

The adapter hit `/api/Image/series-cover?seriesId=N` with the JWT in
the `Authorization` header.

**Reality**: Kavita 0.9.0.2 image routes reject JWT auth and require
`?apiKey=K` as a query parameter.

**Fix**: `_KavitaClient.cover_bytes(chapter_id, series_id)` now hits
`/api/Image/chapter-cover?chapterId=&apiKey=` first (Kavita returns
the chapter's own artwork when present, otherwise falls back to the
series cover server-side) and `/api/Image/series-cover?seriesId=&apiKey=`
as a backstop.  No `Authorization` header.

### 6. MangaFile enum values didn't match the live server

The adapter's hardcoded mapping was `{0: cbr, 1: pdf, 2: epub}`.

**Reality**: On the test server the enum is `{3: epub, 4: pdf}`.  Kavita
versions disagree on the numeric values for the same enum.

**Fix**: `_KavitaClient._format_to_ext()` prefers the `file.extension`
string field (which Kavita always sets reliably) and only consults
the enum as a last-resort fallback.  Tries two known enum mappings
to cover both Kavita versions.

### 7. `title` was empty for every chapter

The adapter read `chapter.titleName` first, then `series.name`.
For "Volume-as-chapter" series on the live server, `titleName` is
empty.

**Fix**: `_KavitaClient._format_to_ext()` was renamed to the
adapter-side helper `_chapter_to_meta`.  Title preference is now:
`titleName` → `title` (volume title for specials) → `series.name`.

### 8. Authors contained the library name

The adapter populated `authors=[series.libraryName]` as a placeholder,
which made every book look like it was written by "Ebooks".

**Fix**: Authors now come from `chapter.writers[]` (a list of person
DTOs with `name` fields).  Empty list if the chapter has no writers
recorded — the bookshelf's "by author" filter is documented as a
future improvement since Kavita has no clean authors endpoint.

### 9. `_login` returned bytes where dict was expected

`_KavitaClient._login()` called `self._request(...)` (which returns
`bytes`) then checked `not isinstance(body, dict)`, which was always
true and raised RuntimeError even on HTTP 200.

**Fix**: switched to `self._request_json(...)`.

### 10. Login payload missing required fields

The adapter sent only `{apiKey: ...}` when an api key was set.

**Reality**: Kavita 0.9.0.2 server-side model validation rejects
apiKey-only payloads with HTTP 400: `Username` and `Password`
fields are required even when authenticating by apiKey.

**Fix**: `_KavitaClient._login()` always sends
`{username, password, apiKey?}`.

## Endpoints we deliberately don't use

* **`/api/Person/all`** — 404 on 0.9.0.2.  Kavita doesn't expose a
  clean authors endpoint; authors come from per-chapter `writers[]`.
* **`/api/Book/{id}/file`** — 404; the only file download endpoint
  is `/api/Download/chapter?chapterId=`.
* **`/api/Chapter/{id}/book-info`** — 404.
* **`/api/Book/{id}`** — 404.

## Caching / id stability

The adapter caches `chapter_id → book_id` (`f"kavita_ch_{chapter_id:08x}"`)
and `book_id → BookMeta` for the session.  Two consecutive
`list_books(limit=20)` calls return the same id sequence (covered
by `test_book_id_stable_across_calls`).

## e2e test infrastructure

* `api/tests/e2e/conftest.py` — shared fixture for the live
  Kavita URL and credentials.  Defines skip markers (`SKIP_NO_URL`,
  `SKIP_NO_AUTH`, `SKIP_UNREACHABLE`) so the suite is a no-op when
  `KAVITA_E2E_URL` isn't set.
* `api/tests/e2e/test_kavita_provider.py` — 17 tests against the
  `KavitaProvider` adapter directly.  Covers health, libraries,
  series pagination, books with real metadata, search filter,
  round-trip id stability, real cover bytes, real epub download.
* `api/tests/e2e/test_kavita_server.py` — 10 tests through the full
  HTTP server, including `?access_token=` URLs for cover/file and
  the `POST /sync/delta` known-list semantics.

Run them with:

```sh
export KAVITA_E2E_URL=https://kavita.example.com
export KAVITA_E2E_API_KEY=<your-kavita-api-key>
export KAVITA_E2E_USER=<user> KAVITA_E2E_PASS=<pass>
pytest api/tests/ -v
```

Without those env vars all 27 e2e tests skip cleanly.

## Test results against the live server

```
api/tests/test_providers.py::test_mock_provider_lists_books ............. PASSED
api/tests/test_providers.py::test_mock_provider_open_file_iter .......... PASSED
api/tests/test_providers.py::test_mock_provider_get_cover ............... PASSED
api/tests/test_providers.py::test_mock_provider_id_stable_across_calls . PASSED
api/tests/test_providers.py::test_book_meta_dataclass .................. PASSED
api/tests/test_providers.py::test_mock_provider_unknown_book ........... PASSED
api/tests/test_runtime_overrides.py ............... 9 PASSED
api/tests/test_server.py ........................... 8 PASSED
api/tests/e2e/test_kavita_provider.py .............. 17 PASSED
api/tests/e2e/test_kavita_server.py ............... 10 PASSED
============================== 50 passed in 15s ==============================
```

## Future work

* **Pagination across the whole library**: `list_books` walks all
  series in a single library and is bounded by `limit`; for
  libraries with thousands of books we'll need either a wider
  endpoint or a server-side cached catalog.
* **Cover caching**: every `get_cover` call re-downloads the image.
  The adapter has no cover cache yet; the API server has one
  (`api/storage/cover_cache.py`) but the cover URL it returns points
  directly at Kavita, bypassing the cache.  Wire up the cache by
  caching bytes on the way out of `get_cover`.
* **Auth refresh**: JWT expires after ~30 minutes; the adapter
  re-logins on every first request after expiry.  A background
  refresh thread would be cleaner.
* **Format enum exposure**: the device app could use `series.format`
  (Book/Image/etc.) to prefer the right reader.  Currently only the
  per-file extension is propagated.
