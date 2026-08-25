# EinkHome architecture

One page for the shape of the system: how the crates layer, where state
lives, which threads run, and how the three test tiers guard it.  Module
headers carry the fine detail (every module cites the C file it ports);
this page is the map that tells you which module to open.

## Crate layers (dependency direction points downward)

```
                eh_pb (staticlib)   eh_host (SDL binary)   eh_demo
                       |                   |
                  sdk/pb-demo/main.c       |
      ┌─────────────┴───────────────────────┴──────────────┐
      │                    eh_app                          │  the application:
      │  shelf · sync · downloads · launcher · viewer …    │  screens + data
      └───────┬──────────┬──────────┬──────────┬───────────┘
              │          │          │          │
         eh_shell   (ureq HTTP)  (SQLite)   eh_layout        UI runtime
      widget/screen runtime, one geometry source     taffy flexbox
              │
        eh_render (fontdue rasteriser)          eh_hal (trait contract)
              │                                 framebuffer · input · keyboard
      ┌───────┴──────────┬─────────────────┐    network seams · device profile
eh_backend_inkview  eh_backend_sdl   eh_backend_linuxfb
 PocketBook fw      desktop/headless   raw fb (experimental)
```

Rules that keep this shape:

* **`eh_hal` is the only seam.**  Everything above it compiles for any
  backend; only the `eh_backend_*` crates touch platform APIs.  This is
  what lets the whole app run under SDL on a PC *and* as an inkview
  `.app`, with one test suite driving both.
* **The shell owns geometry once.**  A `Widget` draws itself AND answers
  hit-tests from the same stored rect (taffy-computed).  Draw/hit drift —
  the C app's original disease — is structurally impossible.  New overlay
  code must follow it: derive tap rects from the paint path, never
  hand-write a second set of numbers.
* **Pure decisions are extracted and contract-tested.**  Examples:
  `store::view_rebuild` composes `append_grouped`/`append_flat`,
  `launcher::assemble`, `downloads::BatchUi::sheet_status`.  If a branch
  decides what the user sees, it lives in a pure function with a test,
  not inside a draw fn.
* **Errors carry their real reason — no sentinels, no swallowed errs.**
  `sync::SyncError` exists because transport failures once returned a
  bogus SQLite error that reached the failure popup; the e2e harness's
  `cmd()` raises on any `err` reply so a failed tap or screenshot can
  never pass silently.

## App lifecycle

`App<B>` owns the `Screen` and routes everything.  The type lives in
`eh_app/src/app/mod.rs`; its `impl` blocks are split by concern into
sibling modules — `app/frame.rs` (present, theme resources, the
self-drawn status strip), `app/events.rs` (`on_event`, keyboard
draining, worker ticks, back navigation) and `app/data.rs` (shelf
rebuilds, drill paging, config persistence).  Lifecycle shape:

* **boot** — `App::new` → `sync_fb_cache` → `boot()` (resolve reader,
  kick sync or the Local import scan, rebuild the materialised view,
  start the cover-warm pass).
* **events** — `on_event(InputEvent)` dispatches by overlay; long-press
  vs tap is decided here (`press_start` timing).  The firmware keyboard
  commits asynchronously through a static handler into a thread_local
  that the next event drains (`kb_arm` / `kb_take_pending`).
* **ticks** — `tick()` drives the background workers' landing pads:
  drain downloads, poll the sync worker channel, apply landed
  local-scan results, advance the sync glyph, re-stamp the clock strip.
* **present** — unchanged frames skip the flush entirely (an emulator
  full redraw is ~1 s); overlays draw onto the canvas and flush their
  dirty region only.

## Background workers (all off the UI thread)

| Worker | Spawn site | UI-side state | Landing pad |
|---|---|---|---|
| sync delta chain | `sync::start_sync` | `WorkerHandle` (rx + cancel flag) | `sync_poll` per tick |
| download jobs | `Downloader::new` | mpsc `Done` stream | `drain_downloads` per tick |
| cover warm pass | `cover::cover_warm_start` | `WarmHandle` atomics | `cover_warm_tick` gate |
| local import scan | `local::kick_import` (opens the sync sheet) | `ScanJob` rx + generation | `poll_import` per tick |

Two invariants every worker shares:

1. **Cancellation is generation-guarded.**  Re-kicks bump a counter
   (`scan_job.gen`) or set a shared flag (`WorkerHandle.cancel`), so a
   result that lands after a source switch or settings change is dropped
   instead of applied.  Committed work stands; cursors stay put.
2. **Batch/lifecycle state starts wholesale.**  `BatchUi::start_single`
   / `start_all` / `reset` replace the whole struct — never patch
   individual flags (that exact pattern once leaked a finished batch's
   tally into the next popup).

## Data plane

* **store.rs** — SQLite (`books` + FTS5 + materialised `view`), schema-
  compatible with the C app's DB.  The ungrouped shelf projects straight
  in SQL at any library size; grouped/drilled shapes scan in Rust bounded
  by `VIEW_SCAN_CAP` (documented RSS guard — truncation beats OOM).
* **network** — plain HTTP to the pbemu-api server over ureq; `util.rs`
  centralises transport + error mapping.  No TLS by design (LAN server).
* **config** — `bookshelf.cfg` key/value next to the DB; `/tmp` API
  overrides feed the e2e suite without leaking into the saved file.

## Testing pyramid

1. **Unit contracts** (`cargo test --workspace`, <1 s): pure logic —
   store SQL semantics, PDF/EPUB extraction, view engine, launcher merge,
   batch lifecycles.  Enforced by CI (`rust-tests` job + clippy
   `-D warnings`).  New decision logic lands WITH its contract test.
2. **SDL e2e** (`EH_TEST_BACKEND=sdl pytest tests/`, ~3 min): the real
   app behind `eh_host`'s newline IPC (`tap x y`, `type T`, `kb_commit`,
   `hash`, `shot P`, `state`, `quit`), driven like a user against the
   mock API.  Offline suite covers no-network behaviour.
3. **Emulator e2e** (CI, podman + real firmware): the same Python suite
   against the inkview build inside qemu — catches backend/firmware
   divergences the SDL tier cannot see.  Hosted-runner boot flakiness is
   absorbed by fresh-runner retry jobs.

Local recipes for tiers 2 and 3 (firmware-path symlink,
`stage-mock-books.sh`, the scale-suite env vars) live in the
README's Tests section — keep the two in sync when the env changes.

## Porting conventions

Every module cites the C file it replaces (`C eh_topbar.c`,
`C view_rebuild_group`, …) and keeps `EH_*` constant names so the two
codebases can be diffed behaviour-for-behaviour.  When a C quirk is
preserved deliberately (e.g. `lc_params`' string-form loss, series sort
without title tie-break), the comment says so — keep doing that instead
of "fixing" it silently.


## Development workflow

Every change must pass the full gate sequence before it lands:

```sh
make fmt          # rustfmt --check (formatting)
make clippy       # clippy -D warnings --all-targets --all-features
make doc-check    # cargo doc with -D warnings (no broken links)
make test-rust    # cargo test --workspace (unit contracts)
make lint-py      # ruff + mypy + api tests
```

`make lint` runs fmt + clippy + doc-check + lint-py in one shot.
CI mirrors this exact set in the `Lint` workflow, so anything that
passes locally passes remotely.

### Conventions

* **One concern per module.**  When a file grows past ~800 lines or
  starts mixing concerns, split it into a directory module (`app/`)
  or extract pure functions into their own file (`extract.rs`).
* **Untrusted input is hardened at the source.**  Byte-slicing on
  arbitrary strings must respect char boundaries; filenames from
  upstream headers are reduced to base names; config writes are
  atomic (tmp + rename).  Each of these has a regression test.
* **Errors carry their real reason.**  No sentinel abuse — use a
  dedicated error enum (`SyncError`, `SheetStatus`) whose variants
  mean what they say.
* **Batch state starts wholesale.**  Constructor replaces the whole
  struct; never patch individual flags (the stale-tally popup bug).
