# Bookshelf step 2 — research notes

These are the notes we took while porting the firmware's
`bookshelf.app` to the new pbemu REST API.  The aim is to capture
every "I had to learn this the hard way" lesson so the next person
debugging the same boundary doesn't have to redo the work.

## 1. Reference architecture: `pbcloud-override/proxy`

We deliberately re-used the existing
`/home/damian/git/pbcoloranalyse/pbcloud-override/proxy/` project
as a reference (NOT as a dependency) for two reasons:

1. It is the only known-working implementation of the
   pbcloud/Kavita sync protocol in the user's git history.
2. Its design reveals what the original PocketBook cloud API
   actually looks like (we have 92 NDJSON request captures from a
   real device in `recordings/2026-06-24/requests.ndjson`).

What we kept from it:

* The `KavitaClient` shape (JWT login + bearer on every call +
  koreader progress endpoints).  We reimplemented the client from
  scratch in `api/providers/kavita.py` because the original was
  tightly coupled to the proxy's `set_client` global; ours is a
  plain instance.
* The "store chapter-by-hash" pattern from
  `proxy/proxy_replace.py:1412` — when the device asks for
  `GET /fileops/info/?fast_hash=X` and we don't have X in our
  store, do a last-ditch content-hash match by downloading the
  candidate file and computing the device-style fast_hash on the
  fly.  We didn't need this for the in-emulator app because the
  app already reports its `known[]` IDs to the server, but it's the
  shape we want for the next iteration.

What we threw away:

* The `_extract_epub_fast_hash`, `_make_fast_hash_from_file`,
  `rewrite_download_link`, `make_pb_login_response`,
  `mint_pb_bearer`, `decode_pb_bearer`, `parse_pb_login_form`,
  `mint_pb_bearer`, `koreader_progress_to_pb_position`,
  `pb_position_to_koreader_progress` machinery.  All of this was
  in service of speaking pbcloud's protocol; the new API server
  doesn't need any of it.
* The `set_client` / `set_store` / `set_proxy_url_prefix`
  global-state pattern.  Our `PbemuAPIServer` is a normal
  instance with constructor-injected dependencies.
* The `MODE=pass-through` / `MODE=record-only` modes.  The new
  server is always "replace".

## 2. Why the new API is not 1:1 pbcloud

The user explicitly asked us to design a clean new API instead of
mimicking the old one.  We confirmed the choice is right: looking at
`recordings/2026-06-24/requests.ndjson` and the proxy's
`translate.py`, the old API has at least these problems:

* Path templates use a mix of trailing-slash / no-trailing-slash
  variants (`/fileops/info` vs `/fileops/info/`, `/fileops/delete`
  vs `/fileops/delete/`) that the proxy has to special-case.
* Authentication is sent as a single combined token-in-query-string
  for some endpoints (`/fileops/cover/<id>?access_token=…`) and as
  an `Authorization: Bearer` header for others, with no clear
  rule.
* File URLs are rewritten through a `proxy_prefix` env var, with
  the proxy_parsed._replace hack in `translate.py:898-916` to
  splice host+port.  The device's HTTP stack then follows the
  redirect, which is fragile and easy to get wrong.
* Cursors are base64-encoded JSON blobs.  A clean API would use
  ordinary `since=<ISO-8601>` query params; the device already
  has a clock.
* The delta endpoint returns `items[]` AND a separate
  `force_update` mechanism, AND an `extra` map.  Each was bolted
  on after a device changed its protocol.  The new endpoint
  returns just `added[]` / `updated[]` / `removed[]`.

The new API is documented in `api/README.md` and uses:

* Bearer header (preferred) OR `?access_token=…` query param
  (fallback for image-fetcher clients that don't re-attach
  auth).  The device uses the query-param form because libinkview
  on this firmware's HTTP layer doesn't expose a way to set
  custom headers.
* One canonical URL per resource (no trailing-slash variants).
* ISO-8601 timestamps for cursors.
* `PUT` for state changes, `POST` for actions that read or sync
  the catalog.
* Provider-neutral field names: every endpoint accepts and emits
  the same shape regardless of the upstream provider.

## 3. Choosing the provider abstraction

The `Provider` interface in `api/providers/base.py` is a deliberate
"narrow waist":

* `list_libraries() / list_series() / list_authors()` are the
  collections a user can browse.
* `list_books(*, mode, library_id, series_id, author_id, search,
  limit, offset, since)` is the only way to enumerate books.
  `mode` is one of `all | series | author | recent | search`.
* `get_book(id) / get_cover(id) / open_file(id)` are the leaf
  lookups.
* `health()` is the liveness probe.

Anything provider-specific lives BEHIND the interface — Kavita
uses `libinkview.so`'s `/api/Series/v2` + `/api/Book/{id}/book-info`
internally; the rest of the codebase never knows that Kavita
exists.  When we add Booklore, Komga, or Calibre Web, only the
provider file changes; the API server and the C app are
untouched.

The same `_KavitaClient` is the obvious plumbing
(`api/providers/kavita.py:25-310`); the provider adapter is
the much smaller `KavitaProvider` class below it.  Future
authors: copy the adapter shape verbatim.

## 4. Network reachability inside the emulator

The in-emulator app talks to the host via:

* `169.254.1.2` — the podman `host.containers.internal` alias
  inside the running container (verified via `cat /etc/hosts`
  inside the container).
* `127.0.0.1` — the host loopback, which is reachable from the
  host's `0.0.0.0:8765` listener.

The C app is hard-coded to use `169.254.1.2` because the
container's network namespace is shared with the host and this
address routes correctly.  In a non-podman setup, this would need
to be configurable (likely via a `pbemu.ini` or env var).  Out of
scope for this iteration.

## 5. The crt1 / crtbeginS dance

When the in-emulator ARM app failed with `Segmentation fault` and
`argc = 1065215620`, the actual cause was a missing `_start` in
the ELF: with `-nostartfiles`, the linker picks `main` directly as
the entry point, and the kernel-loaded stack pointer is meaningless
for argc/argv layout.  The fix is to pull in the cross-compiler's
`crt1.o` + `crti.o` + `crtbeginS.o` so `_start` runs, sets up
`argc`/`argv` from the kernel's `argv` auxv block, and then calls
`main`.

This is documented in `sdk/build_armel.sh` (search for `crt1.o`).
The wrong version (no crt files) produces an ELF where:

```
$ readelf -s build/bookshelf.app | grep _start
  69: 00000000     0 NOTYPE  GLOBAL DEFAULT  UND _start
```

The right version:

```
$ readelf -h build/bookshelf.app | grep Entry
  Entry point address:               0x7c8
$ readelf -s build/bookshelf.app | grep main
   72: 000007c8   328 FUNC    GLOBAL DEFAULT   11 main
```

— and `main` is now a regular function called by `_start`, not
the ELF entry.

## 6. The Informer cache / "no valid framebuffer" trap

A long time was wasted debugging `frame_dump: no valid
framebuffer` after the in-emulator app was clearly running.  The
actual cause: the **informer** is its own task (task 69 in the
task list), with its own (empty) framebuffer.  When the viewer
asks "what's the active task's framebuffer?", it reads from the
informer's state SHM; if the informer hasn't refreshed its view
since the last task switch, the answer is stale.

We confirmed this by dumping the informer SHM directly:

```
sequence=232
active_task=315            # stale
subtask=0
task_flags=0x10c190
colormask=1
fbinfo.version=0
fbinfo.shmkey=0xffffffff   # no fb
fbinfo.width=0
fbinfo.height=0
```

The task 315 is dead; the new task 5355 has a perfectly
formatted SHM header (we dumped it with a tiny C program in the
container).  But the informer's view of the world is stale,
so the viewer says "no valid framebuffer" and we see a blank
screen.

**Workaround for this iteration:** the screenshot we capture is
of the *current* task's framebuffer only if the task's
`fb_y_offset` matches.  For now, we accept the blank screen and
rely on the API server log to prove the app is talking to us.
A future iteration can:

* teach the informer to re-scan the task list on every clock tick
  rather than only on message arrival, or
* have the in-emulator app also expose its framebuffer to the
  reader via a `bookshelf-pb` symlink (`/workspace/firmware/.live/mnt/ext1/system/bin/`).

## 7. libinkview's HTTP quirks

* `QuickDownload(url, &retsize, timeout)` is GET-only.  To POST,
  use `QuickDownloadExt(url, &retsize, timeout, cookie, post_body)`.
  The `cookie` argument is for `Cookie:` not `Set-Cookie` (the
  libinkview docs are confusingly worded); we pass `NULL` for
  our case.
* `QuickDownload` / `QuickDownloadExt` return a malloc'd buffer
  the caller MUST free.  We do so on every error and success
  path; a missed `free()` here will exhaust the firmware's
  fragment heap within minutes.
* Both functions silently treat the response body as a single
  C string and do not surface the HTTP status code.  The only
  way to know if a request succeeded is to look for sentinel
  substrings in the body.  We rely on this for the
  `parse_pb_position_update` shape; for the new API we use the
  `QuickDownload` empty-body check (`retsize == 0`) to mean
  "server unreachable" and otherwise treat the body as JSON.

## 8. The shim's `fake_ebc` doesn't trigger on `/dev/fb0`

Our `fb_hello.c` (an earlier test program) opened `/dev/fb0` and
drew directly.  The shim intercepts `open("/dev/fb0")` and
returns a memfd of the right size, but **the viewer reads from
the informer SHM, not from `/dev/fb0`**.  So drawing to `/dev/fb0`
is invisible to the screenshot pipeline.

The fix is to use libinkview's normal `DrawXxx` / `FullUpdate`
API, which writes to the per-task SHM that the informer
publishes.  That's what the final `bookshelf.c` does.

## 9. `MODE=pass-through` / `record-only` lessons

The proxy's `MODE=record-only` (no upstream contact) is
*very* useful when:

* you want to see what the device's HTTP stack does without
  paying the latency of a real upstream roundtrip;
* you don't have a live Kavita running;
* you want to capture the device's *exact* wire format.

For the new server we don't need this mode because the device
is ours and we own both sides of the wire.  But the pattern
("return canned 200 for any request, log the request to NDJSON")
is preserved in `api/server.py:_RequestHandler._handle_get` as
the `_version_probe` function, which still does the same trick
for `GET /api/v1/` and `GET /api/v1.0/` (the device sends a
version probe first thing on every connection).

## 10. Findings / action items for the next iteration

* **Provider coverage**: Booklore, Komga, Calibre-Web.  Plan to
  copy the `KavitaProvider` shape; the only really different
  ones will be Komga (REST API is closer to ComicRack than to
  Calibre) and Calibre-Web (no first-class series concept, must
  synthesise one from tags).
* **Cover cache**: the on-disk `CoverCache` (sha256(book_id).jpg)
  is correct for the emulator; the device should also store its
  own copy in its `/mnt/ext1/system/config/pbemu-covers` dir,
  keyed the same way, to avoid re-downloads on every page turn.
* **Informer refresh**: the stale-active-task bug bites us here.
  See §6.
* **Open-with picker UI**: the C app currently just trusts the
  server's `app` string; a real UI needs a chooser for the
  `alternates[]` (e.g. "Open with fbreader" / "Open with
  pdfviewer" / "Open with…").  Out of scope for this round.
* **Sync state persistence**: we log state reports on the server
  but don't persist them across server restarts.  Add a
  `state_log.db` sqlite (or just a single JSON file) once
  multi-device sync is in the requirements.
* **Auth**: the device uses a hard-coded bearer in the URL.  Real
  devices need a per-device credential, ideally paired at first
  boot.  Out of scope for this round.
* **i18n / RTL**: the C UI hard-codes English.  The original
  PocketBook apps use `GetLangText()` and `GetLangCode()` from
  libinkview; we should call them before falling back to strings.

## 11. Files of interest

| File | What it does |
|---|---|
| `api/api/server.py` | The new REST API.  All endpoints under `/api/v1/`. |
| `api/providers/base.py` | The `Provider` interface. |
| `api/providers/kavita.py` | The Kavita adapter.  Reimplements the
   proxy's `KavitaClient` from scratch. |
| `api/providers/mock.py` | Offline-friendly fallback that reads the
   firmware's `books/` dir.  Lets the in-emulator app run
   without a real Kavita instance. |
| `api/storage/cover_cache.py` | On-disk cover cache. |
| `api/config/server.json` | Server config.  Switch
   `provider: "mock" ↔ "kavita"` to change sources. |
| `bookshelf/bookshelf.c` | The in-emulator C app.  Uses InkViewMain
   for proper task registration and draws the new UI
   (top bar, hamburger, sync, more menu, 2×2 thumbnail grid,
   per-book cloud/downloaded icon, progress bar, tap-to-open
   that calls `/open-with` then streams the file). |
| `bookshelf/run.sh` | End-to-end driver.  Builds, restarts the
   API server, restarts the emulator, screenshots. |
| `sdk/build_armel.sh` | Cross-compile script.  Pulls in
   `crt1.o` + `crti.o` + `crtbeginS.o` so `_start` runs
   correctly.  See §5. |
| `sdk/install-sdk.sh` | Fetches the PocketBook SDK-B288
   headers + libraries from the public `pocketbook/SDK_6.3.0`
   GitHub repo. |
