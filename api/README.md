# pbemu API

A clean, provider-agnostic REST API for the in-emulator
`bookshelf.app` replacement.  Stand-in for the firmware's
`cloud.pocketbook.digital` so the in-emulator app can fetch book
metadata and book files without going to PocketBook's cloud.

The design replaces the messy `pbcloud-override` proxy with a
small, single-purpose server.  See the EinkHome repository's
`RESEARCH.md` for the full rationale; the short version is "we are
not required to mimic the legacy pbcloud API 1:1, so we made a
better one".

## What it does

* Exposes a `/api/v1/...` URL surface that the in-emulator C
  app talks to.
* Reads from one of several content providers — currently Kavita
  and a `mock` provider that just lists the firmware's `books/`
  dir.  Booklore / Komga / Calibre Web adapters slot in without
  changes elsewhere.
* Speaks provider-neutral JSON everywhere.  Kavita's MangaFile
  format enum, Komga's nested library/series/issue model,
  etc. all get normalised to the same shape on the wire.
* Serves cover bytes and book files streamed from the
  upstream.  The device never sees the upstream's auth scheme.

## URL surface

```
GET  /api/v1/healthz                — liveness
GET  /api/v1/libraries              — list libraries/collections
GET  /api/v1/libraries/{id}/series  — series in a library
GET  /api/v1/authors                — list authors
GET  /api/v1/books                  — list books
GET  /api/v1/books/{id}             — book detail
GET  /api/v1/books/{id}/cover       — cover image
GET  /api/v1/books/{id}/file        — stream the file
POST /api/v1/sync/delta             — metadata-only diff
POST /api/v1/sync/state             — device posts its sync state
POST /api/v1/open-with              — resolve extension to app
```

### Auth

Bearer token.  Send as `Authorization: Bearer <token>` on
metadata calls.  For `GET /books/{id}/cover` and
`GET /books/{id}/file`, the device uses
`?access_token=<token>` because the legacy PB image-fetcher
does not re-attach the Authorization header on subsequent
requests (the same trick the original `pbcloud-override`
proxy uses).

## Run

```
../scripts/run.sh        # (EinkHome repo) builds the in-emulator app AND the API server, restarts everything
# or, just the server:
PYTHONPATH=. python -m api.api.server --host 0.0.0.0 --port 8765
```

The server reads its config from `api/config/server.json`.
Switch the `provider` key between `mock` and `kavita` to
change sources.  Each provider has its own config block under
`providers.<name>`; the `kavita` block accepts `base_url`,
`api_key`, `username`, `password`, `verify_tls`, `timeout`,
and (optionally) `library_ids` to restrict the visible scope.

### Runtime overrides

You don't have to edit `server.json` to switch providers or
point at a different Kavita instance — the config is layered
with CLI-flag and env-var overrides at startup.

Resolution order (highest priority first):

1. **CLI flag** — `--provider`, `--host`, `--port`
2. **Env var**   — `PBEMU_PROVIDER`, `PBEMU_HOST`, `PBEMU_PORT`
3. **Config file** — `api/config/server.json`

Examples:

```bash
# Just override the provider, keep everything else from the config file
python -m api.api.server --provider kavita

# Or via env var (handy in systemd / docker-compose / .env files)
PBEMU_PROVIDER=kavita python -m api.api.server
```

Provider-specific fields can also be overridden via env vars
of the form `PBEMU_<PROVIDER>_<FIELD>` (uppercased).  Values are
parsed as booleans (`true`/`false`/`1`/`0`) or integers when
possible; everything else stays a string.

```bash
PBEMU_PROVIDER=kavita \
PBEMU_KAVITA_BASE_URL=https://kavita.example.com \
PBEMU_KAVITA_API_KEY=<your-kavita-api-key> \
PBEMU_KAVITA_TIMEOUT=60 \
python -m api.api.server
```

Only env vars for the *currently active* provider are honoured —
`PBEMU_KAVITA_*` is ignored when `provider: mock` is selected,
which keeps the other provider blocks in your config file
untouched.

## File layout

```
api/
├── README.md                       — this file
├── RESEARCH.md                     — live Kavita integration notes
├── config/server.json              — runtime config
├── api/
│   ├── __init__.py
│   └── server.py                   — REST server + bootstrap
├── providers/
│   ├── __init__.py
│   ├── base.py                     — abstract `Provider` interface
│   ├── kavita.py                   — Kavita adapter
│   └── mock.py                     — offline mock for development
├── storage/
│   ├── __init__.py
│   └── cover_cache.py              — on-disk cover cache
└── tests/
    ├── test_providers.py           — offline unit tests
    ├── test_server.py              — offline HTTP smoke tests
    ├── test_runtime_overrides.py   — CLI / env-var override tests
    └── e2e/                        — live-server tests (skipped by default)
        ├── conftest.py
        ├── test_kavita_provider.py  — adapter tests against a live Kavita
        └── test_kavita_server.py   — full HTTP-server tests against it
```

## Tests

Two tiers:

* **Offline unit tests** (`test_providers.py`, `test_server.py`,
  `test_runtime_overrides.py`) — run anywhere, no network, fast.
* **Live e2e tests** (`tests/e2e/`) — boot the API server against a
  real Kavita and exercise every endpoint.  Skipped automatically
  unless `KAVITA_E2E_URL` is set.  See `RESEARCH.md` for the bugs
  they caught and what they cover.

Run everything:

```sh
pytest api/tests/ -v
```

Run only the e2e tier:

```sh
export KAVITA_E2E_URL=https://kavita.example.com
export KAVITA_E2E_API_KEY=<your-kavita-api-key>
export KAVITA_E2E_USER=<user> KAVITA_E2E_PASS=<pass>
pytest api/tests/e2e/ -v
```

## Endpoints in detail

### `GET /api/v1/books`

Query parameters:

* `mode`         — `all | series | author | recent | search`
                   (default: `all`).
* `library`      — restrict to one library id (e.g.
                   `lib_1` for the library with Kavita id=1).
* `series`       — restrict to one series id
                   (e.g. `ser_42`).
* `author`       — restrict to one author id.
* `search`       — full-text search against title + series.
* `limit`        — max books to return (default 500).
* `offset`       — pagination.
* `since`        — ISO-8601 timestamp; only return books
                   whose `updatedAt > since`.

Response:

```
{
    "items":  [ BookMeta, BookMeta, ... ],
    "limit":  500,
    "offset": 0,
    "count":  17,
    "hasMore": false
}
```

`BookMeta` JSON shape:

```
{
    "id":         "kavita_ch_00000042",
    "title":      "Some Book",
    "authors":    ["Author One"],
    "series":     "Cool Series",
    "seriesIdx":  1.0,
    "summary":    "...",
    "lang":       null,
    "format":     "epub",
    "size":       1234567,
    "pages":      0,
    "cover":      "/api/v1/books/kavita_ch_00000042/cover",
    "url":        "/api/v1/books/kavita_ch_00000042/file",
    "addedAt":    "2026-01-01T12:34:56Z",
    "updatedAt":  "2026-02-01T00:00:00Z",
    "remoteOnly": true,
    "extra":      { ... provider-specific ... }
}
```

### `POST /api/v1/sync/delta`

Body:

```
{
    "known":   ["<book-id>", ...],
    "since":   "2026-01-01T00:00:00Z",   // optional
    "limit":   500                       // optional
}
```

Response:

```
{
    "added":     [ BookMeta, ... ],   // books the device hasn't seen
    "updated":   [ BookMeta, ... ],   // books whose updatedAt > since
    "removed":   [ "<id>", ... ],      // book ids the device has but we don't
    "serverTime": "...",
    "provider":   "kavita"
}
```

This endpoint NEVER includes file bytes.  Downloads are
explicit, on the device's request, via `GET /books/{id}/file`.

### `POST /api/v1/sync/state`

Body:

```
{
    "deviceId":   "pbemu-app",
    "known":      ["<id>", ...],
    "downloaded": ["<id>", ...]
}
```

Server responds `202 {"ok": true}` and logs the report for
debugging.  Persistence is a future-work item; the new server
just keeps the most recent report per device in memory.

### `POST /api/v1/open-with`

Body:

```
{
    "id":  "<book-id>",
    "ext": "epub"     // optional, defaults to book.file_format
}
```

Response:

```
{
    "app":        "eink-reader",
    "alternates": ["bookshelf"],
    "url":        "/api/v1/books/<book-id>/file",
    "ext":        "epub"
}
```

The `app` field is the canonical handler for the file extension;
`alternates` lists other handlers from the configured
`open_with` table.  The `url` is where the device fetches the
file from.

## Adding a new provider

1. Drop a `providers/<name>.py` that subclasses
   `providers.base.Provider`.
2. Add a config block under `providers.<name>` in
   `api/config/server.json`.
3. Wire the kind in `api/api/server.py:_build_provider`:

   ```python
   if kind == "<name>":
       from providers.<name> import <Name>Provider
       return <Name>Provider(pcfg)
   ```

That's it.  The API server, the in-emulator C app, and all
existing providers are unaffected.
