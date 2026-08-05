# Bookshelf replacement (Step 1 — done)

A drop-in replacement for the firmware's `bookshelf.app`, designed to:

* fetch a list of available books from a local HTTP server,
* draw the book list on the e-ink framebuffer using libinkview,
* post state reports back to the server so we can verify the emulator
  is actually talking to us.

This is the first step of the pbemu bookshelf-replacement project: prove
the PocketBook SDK we downloaded builds a real ARM guest app that runs
under `qemu-arm` + `libshim.so`, displays content, and reports back to
a host service.

## What this PR delivered

1. **`sdk/pocketbook-sdk-b288/`** — the PocketBook SDK-B288 (matching the
   `RPATH` recorded inside `cramfs/bin/bookshelf.app`) vendored from the
   public `pocketbook/SDK_6.3.0` GitHub repository, branch `6.5`. Includes
   `inkview.h`, `inkinternal.h`, `inkplatform.h`, `inklog.h`, `scrollview.h`,
   `selection_list.h`, `line_color_improver.h`, `time_test.h`, `hwconfig.h`
   and the matching `libinkview.so` / `libhwconfig.so` / static archive.
   Re-fetched any time with `sdk/install-sdk.sh`.

2. **`sdk/build_armel.sh`** — `arm-linux-gnueabi-gcc` cross-compile script
   that:
   * builds inside the pbdev container so it picks up the cross toolchain,
   * links the resulting ELF against the firmware's own `libc.so.6` /
     `libm.so.6` / `libz.so.1.2.11` so the binary is GLIBC_2.23-only and
     loads transitive deps (`libpng12`, `libjpeg8`, `libtiff5`, etc.) at
     run-time from the firmware's `/ebrmain/lib`,
   * uses the cross-compiler's `crt1.o` / `crti.o` / `crtbeginS.o` so
     `main(int argc, char **argv)` is invoked with a real stack and
     well-defined `argc` / `argv` (without the crt objects, `argc` came
     through as a garbage value and `main` segfaulted).

3. **`sdk/hello/hello.c`** + a couple of sanity-check programs (`test_simple.c`,
   `test_http.c`, `fb_hello.c`) to validate the SDK build pipeline end to end
   (basic draw, HTTP fetch via `QuickDownload`, raw `/dev/fb0` writes).

4. **`bookshelf/bookshelf.c`** — the actual bookshelf replacement. Fetches
   `/api/v1/books` from the local bookserver, draws the list with libinkview,
   lets the user select / open books, posts `/api/v1/state` after every UI
   change.

5. **`bookshelf/bookserver.py`** — host-side Python HTTP server (stdlib only)
   that exposes:
   * `GET  /healthz`
   * `GET  /api/v1/books`
   * `GET  /api/v1/books/<id>`
   * `GET  /api/v1/state`
   * `POST /api/v1/state`

6. **`bookshelf/run.sh`** — end-to-end driver: builds, stages the binary into
   the running container's `/mnt/ext1/system/bin/bookshelf.app`, restarts
   the emulator, takes a screenshot, and prints the most recent state
   reports from the bookserver.

7. **Dockerfile change** — adds `zlib1g-dev` so the cross-compile find
   `zlib.h` for the few symbols we need.

## End-to-end validation evidence

After `./pbemu start` and `./bookshelf/run.sh`:

```
==> 6/6  screenshot + bookserver state
wrote /tmp/pbemu_bookshelf.png
screenshot -> /tmp/pbemu_bookshelf.png
task_id=5355 subtask_id=0
fb_key=0xefbd4eb fb_size=4956144
...

Bookserver state reports (last 5):
  2026-06-28T11:08:05.961345+00:00  remote=192.168.178.101  app=bookshelf.app  screen=library_list
  2026-06-28T11:08:06.241597+00:00  remote=192.168.178.101  app=bookshelf.app  screen=library_list
```

`192.168.178.101` is the running container — proof that the in-emulator
process is actually talking to the host. The bookserver log also shows:

```
[2026-06-28T11:08:05+00:00] 192.168.178.101 - "POST /api/v1/state HTTP/1.1" 202 -
[2026-06-28T11:08:06+00:00] 192.168.178.101 - "GET /api/v1/books HTTP/1.1" 200 -
[2026-06-28T11:08:06+00:00] 192.168.178.101 - "POST /api/v1/state HTTP/1.1" 202 -
```

i.e. the in-emulator app successfully fetches the book list and reports
state back through the loopback container-to-host network.

## Build

```
sdk/install-sdk.sh                         # one-off: fetch SDK headers + libs
sdk/build_armel.sh bookshelf/bookshelf.c --output build/bookshelf.app
```

## Run

```
./pbemu start U633_6.8.2817 --no-viewer --no-audio
./bookshelf/bookserver.py &
./bookshelf/run.sh
```

`run.sh` will:

1. Build the ELF (overwriting `build/bookshelf.app`).
2. Stop the running emulator.
3. Stage the ELF as `U633_6.8.2817/.live/mnt/ext1/system/bin/bookshelf.app`
   so `monitor.app`'s launcher picks our binary on next respawn.
4. Start the emulator again.
5. Wait for the foreground task to be our `bookshelf.app`, take a
   screenshot to `/tmp/pbemu_bookshelf.png` and dump the last few state
   reports that `bookserver.py` has received.

### Interactive run (with viewer)

`run.sh` is headless (`--no-viewer`) for automated screenshots. To **see and
use** the app in a Wayland window on your desktop, use the interactive
driver instead — same build/stage/API-server steps, but it starts the
emulator with the viewer + audio relay:

```
./bookshelf/run-visible.sh             # build + launch with viewer
./bookshelf/run-visible.sh --no-build  # skip the ELF rebuild (faster)
```

It (re)starts the API server in the background (`/tmp/pbemu-api.log`,
pid in `/tmp/pbemu-api.pid`), stages the binary + `bookshelf.cfg`, and
runs `./pbemu start` with the viewer. The window appears on your desktop;
tap **⋯** → **Sync** to sync, **⋯** → **Settings** to edit the API host /
key / reader. Stop everything with `./pbemu stop`.

## Network reachability

The container's qemu-arm shares the container's network namespace, so
the guest can reach the host's bookserver via:

* `169.254.1.2` — the podman `host.containers.internal` alias inside
  the running container (verified via `cat /etc/hosts`).

That is what `bookshelf.c` defaults to. Override the URL by passing
argv[1] (books URL) and argv[2] (state URL) when launching the binary
manually.

## Debugging tips

If `frame_dump` reports `no valid framebuffer`, the foreground task has
crashed. Common causes:

* The host's `bookserver.py` is not running on a port the guest can
  reach — start it (`./bookshelf/bookserver.py &`) and check
  `curl http://169.254.1.2:8765/healthz` from inside the container.
* `monitor.app` has stopped relaunching tasks because its fork budget
  is exhausted — restart with `./pbemu stop && ./pbemu start`.
* The `libinkview.so` HTTP layer is hanging on a transient network
  error — `QuickDownload`'s 5-second timeout will eventually bail out
  and the UI will render with `g_state.http_ok = 0`.

Inspect the binary's own stderr via:

```
podman exec pb-pocketbook-ui qemu-arm -L /workspace/firmware/.live/guest \
    -E LD_PRELOAD=/workspace/src/shim/build-arm/libshim.so \
    -E PB_INKVIEW_WINDOW_TITLE=bookshelf.app \
    -E PB_DEVICE_STAGE_ROOT=/workspace/firmware/.live \
    /mnt/ext1/system/bin/bookshelf.app -i 2>&1 | tee /tmp/bookshelf.log
```

The `[bookshelf] EVT_INIT ...` lines confirm whether the HTTP fetch and
state POST succeeded.

## Configuring the API endpoint

The binary resolves its API base URL in this order:

1. **Config file** — searched at these paths:
   * `<dir-of-binary>/bookshelf.cfg` (drop a file next to the binary
     on the device — recommended)
   * `/etc/pbemu/bookshelf.cfg` (system-wide override)
   The file is a tiny `key=value` list:

   ```
   # Comments start with `#` or `;`
   api_url=http://192.168.1.42:8765
   api_token=pbemu-dev-token
   reader=auto
   ```

   `reader` selects which app opens a tapped book. `auto` (the default)
   honours the server's `open-with` resolution; an absolute app path
   (e.g. `/ebrmain/bin/eink-reader.app` or
   `/mnt/ext1/applications/koreader.app`) pins that reader directly.
   You normally don't edit this by hand — the in-app **Settings** page
   (below) writes it for you.

   The `api_url` may be just a host (`192.168.1.42:9000`) or a full
   URL; if it doesn't start with `http://` or `https://`, the binary
   prepends `http://` and the port (defaulting to 8765 if you only
   specify a host).

2. **Environment variables** at startup:
   * `PBEMU_API_URL` (full URL, e.g. `http://192.168.1.42:8765`)
   * `PBEMU_API_HOST` (host only; see above for default-port rule)

3. **Build-time default**: the pbemu-internal `host.containers.internal`
   alias (`169.254.1.2:8765`), which works inside the pbemu container
   but **not** on a real device.

### In-app settings

The **More** menu (top-right `⋯`) has a **Settings** entry that opens a
full-screen page where you can edit the API host, the API key, and the
reader app, then **Save & apply** (which rewrites `bookshelf.cfg`,
rebuilds the endpoint URLs, and re-syncs immediately). The reader row
cycles through **Auto (server)** plus every reader detected as installed
on the device — the standard PocketBook reader
(`/ebrmain/bin/eink-reader.app`) and KOReader
(`/mnt/ext1/applications/koreader.app`) when present.

Settings are saved next to the binary when that directory is writable;
otherwise (e.g. the emulator's non-root guest) they fall back to
`/tmp/bookshelf.cfg`, which is re-applied as an override on the next
launch.

For a real PocketBook, use the bundled `install-device.sh` script to
push the binary + config and install the startup wrapper:

```bash
bookshelf/install-device.sh <device-ip>            # auto-detects host LAN ip
bookshelf/install-device.sh <device-ip> http://kavita.lan:8765
bookshelf/install-device.sh --build <device-ip>    # also rebuilds the binary
```

The script:

* sshes non-interactively to `<device-ip>` as root (run `ssh-copy-id`
  once beforehand),
* writes a fresh `build/bookshelf.cfg` with `api_url` set to the
  host's primary LAN IPv4 (or the override you pass),
* scps the binary + config into `/mnt/ext1/applications/`,
* renames the binary on-device to **`books.app`**, not `bookshelf.app`,
  because PocketBook's launcher dispatches by basename — leaving the
  original name there would launch the firmware's built-in
  bookshelf.app and silently exit,
* deploys **`bookshelf-wrapper.sh`** to
  `/mnt/ext1/system/bin/bookshelf.app` — the startup hook that makes
  the custom bookshelf launch on boot (see below),
* clears the on-device log and kills any stale `books.app` so the
  next launch starts clean.

### Startup wrapper (auto-launch on boot)

The firmware's launcher, `monitor.app`, resolves the home/startup app
by checking `/mnt/ext1/system/bin/bookshelf.app` **before** the
read-only `/ebrmain/bin/bookshelf.app` (verified in the launcher's
disassembly at `0x33b48`–`0x33b74`). `/mnt/ext1` is the writable user
partition, so dropping a file there overrides the boot path with no
root, no flash, and no signing.

`bookshelf-wrapper.sh` is installed at that override path. On every
boot it:

1. launches the custom bookshelf (`/mnt/ext1/applications/books.app`)
   in the background, fire-and-forget, guarded by a PID file so it
   never spawns duplicates;
2. `exec`s the **real** firmware bookshelf with the original argv, so
   the stock library UI keeps working exactly as before.

The result: the custom bookshelf runs as a separate task alongside the
stock home screen. If it crashes, nothing breaks — the stock bookshelf
is the one `monitor.app` is actually tracking, so the crash-loop guard
(`bookshelf.self.check`) never trips on our app.

To remove everything and restore the stock boot path:

```bash
bookshelf/uninstall-device.sh <device-ip>
# then reboot the device
```

Or do the install by hand:

```bash
scp build/bookshelf.app        root@<device-ip>:/mnt/ext1/applications/books.app
scp build/bookshelf.cfg        root@<device-ip>:/mnt/ext1/applications/
scp bookshelf/bookshelf-wrapper.sh root@<device-ip>:/mnt/ext1/system/bin/bookshelf.app
ssh root@<device-ip> 'chmod +x /mnt/ext1/applications/books.app /mnt/ext1/system/bin/bookshelf.app'
```

Edit the config to point at your API server before copying if you're
not using `install-device.sh`.

The next `monitor.app` respawn will log the resolved URL on stderr:

```
[bookshelf] config: /mnt/ext1/applications/bookshelf.cfg
[bookshelf] api_base  = http://192.168.1.42:8765
[bookshelf] api_token = pbemu-dev-token
```

### Logs on the device

The firmware does **not** redirect stderr to a file, so the binary
opens its own log at `<dir-of-binary>/bookshelf.log` (next to the
binary on the device).  Same format, same lines; tail it to see what
the app is doing:

```bash
ssh root@<device-ip> 'tail -f /mnt/ext1/applications/bookshelf.log'
```

### Initial sync is automatic

The binary syncs metadata on startup (the EVT_INIT handler calls
`do_sync()` before the first draw), so the shelf populates without a
manual tap.  The local store is opened and the grid built from it
*before* the network sync, so an unreachable API still shows the
cached library.  Subsequent syncs are triggered via **⋯** → **Sync**,
or via the sync button in the top-bar right corner while the Downloads
view is showing.

## UI

The on-screen layout has four regions stacked vertically, all drawn
between the system top status bar (drawn by the firmware with day-of-week
* 24h time, e.g. "Wed 13:01") and the system bottom status bar (drawn by
the firmware with day-of-week + 24h time + down-arrow + lightbulb + battery):

```
+--------------------------------------------------+ <- 0
|  Wed 13:01                                       |   system top status bar
+--------------------------------------------------+ <- ~28 px
|  [⌂]                                  [⬇] [☰] |   TOP_BAR_H (128 px)
+--------------------------------------------------+
|  [Q] search...                                  |   SEARCH_ROW_H (88 px)
+--------------------------------------------------+
|                                                  |
|   [book]    [book]    [book]                     |
|   [book]    [book]    [book]                     |   3×2 grid of thumbnails
|                                                  |
+--------------------------------------------------+
|              <  1 / 2  >                          |   PAGER_H
+--------------------------------------------------+   <- ScreenHeight - BOTTOM_RESERVED
|  Wed 13:01       ⌄      💡     🔋 100%   ←  bottom status bar
+--------------------------------------------------+ <- ScreenHeight
```

The top bar style matches the firmware's standard `sudoku.app`
header:

* **Left** — a 96×96 house outline (home button).  Tapping returns to
  the launcher (`CloseApp()`); while the Downloads view or a drilled-in
  series is showing it acts as "back" instead.
* **Center** — a title only while a drilled-in series (series name) or
  the Downloads view ("Downloads") is showing; the plain library shelf
  carries no title.
* **Right** — a 96×96 solid black square with three white hamburger
  lines.  Tapping opens the in-app "More" menu (sort, view, sync).
* **Left of the menu button** — a 96×96 downloads icon (down arrow
  into a tray, Firefox-style).  Tapping it opens the Downloads view
  (queued/finished downloads); it carries a pending-count badge while
  a download queue drains.

On the Downloads view the hamburger is gone — the sub-navigation keeps
only back (left), the Downloads title (center), and the right slot,
which now holds the 96×96 sync button: a solid black square with two
white arc arrows that rotate a few degrees per second while a sync or
download is in flight.  Tapping it runs a library sync without leaving
the view.

The "More" menu is a right-anchored 75%-width panel with the
following items, in order: **Sync**, **Title A–Z**, **By author**,
**By series**, **Recent**, **Grid**, **List**, **Download all**,
**Settings**, **System menu**, **Applications**.

### How the firmware system status bars are enabled

PocketBook apps like `sudoku.app`, `dictionary`, `notes`, the original
firmware's `bookshelf.app` etc. all display the standard system status
bars at the **top** ("Wed 13:01" — day + 24h time) and **bottom** ("Wed
13:01 + down-arrow + lightbulb + battery") of the screen.  These bars
are drawn by the firmware's `libinkview`; the app just needs to declare
itself as a reader-style app and ask the firmware to show them.

`bookshelf.c` does this in `EVT_INIT` with the following calls (in
this exact order — the SDK docstring on `SetCurrentApplicationAttribute`
says "set this attribute **before first access to panel API**"):

```c
SetCurrentApplicationAttribute(APPLICATION_READER, 1);   /* flag as reader */

/* Set the framebuffer orientation FIRST.  SetOrientation() recomputes
 * the per-task iv_fbinfo (clearing the framebuffer to white and
 * resetting fb_y_offset to 0).  If it runs AFTER SetPanelType() it
 * wipes the panel's fb_y_offset and iv_update_panel() then bails —
 * it reads fb_y_offset==0 as "no panel".  Doing it first lets
 * SetPanelType() write the correct fb_y_offset into the final layout.
 */
SetOrientation(0);

SetShowPanelReader(1);            /* 1 = show the panel reader bars */
SetPanelSeparatorEnabled(1);      /* thin separator above the bar */
SetPanelTransparent(0);           /* 0 = opaque bar (no see-through) */
SetPanelType(PANEL_ENABLED);      /* enable the status panel */
g_state.panel_h = PanelHeight();  /* height reserved at the BOTTOM */

/* Force the firmware to draw the status bar now.  Repaint() enqueues
 * EVT_SHOW (=23); the firmware's iv_actualize_panel() handler calls
 * iv_update_panel() to blit the clock / battery / wifi strip.  Without
 * this the bar is only redrawn on later state changes (minute tick,
 * battery %, net state) — on a fresh task with no change yet it is
 * blank.  Repaint() forces an immediate one-shot redraw.
 */
Repaint();
```

The status bar is **not** drawn by the app — `libinkview` does it
automatically once the panel is enabled and the app is flagged as a
reader.  Crucially, on this firmware the panel always renders at the
**bottom** of the screen: bit 3 of the internal panel-state byte is
clear after `SetPanelType(PANEL_ENABLED)`, and the only way to set it
(`SetPanelType(9)`) also zeroes `fb_y_offset`, which makes the panel's
inner draw function bail entirely (it treats `fb_y_offset==0` as "no
panel").  So `panel_h` is a **bottom reservation**, not a top offset:
our content starts at `y=0` and the pager is placed `panel_h` pixels
above the bottom edge so it never overlaps the status bar.

We deliberately **do not** call `SetPanelType(PANEL_DISABLED)` or
`iv_fullscreen()`.  Without those the system panel stays visible —
swipe down from the top to access Wi-Fi / Bluetooth / Sync, swipe up
from the bottom to see the time/battery bar plus the system settings.

The original firmware's `bookshelf.app` also imports
`SetDefaultOrientation` (called before `InkViewMain`).  We don't call
it because on the pbemu shim the framebuffer isn't attached until the
task is registered, so calling `set_fb_orientation()` that early hits
a NULL fb and does nothing.  `SetOrientation(0)` in `EVT_INIT` runs
after the shim has attached the main framebuffer and produces the same
end-state orientation (portrait) without the early-NULL-fb problem.

Below the top bar is a **search** row.  Tapping it opens the
firmware's native on-screen keyboard; pressing Enter (or the keyboard's
OK button) filters the visible books by title or author.

The main area is a 3×2 grid of thumbnails.  Each tile shows the book
cover (blurhash placeholder or cached PNG while the real cover
downloads), the book title, the author, and a corner badge:

* `v` — the book is downloaded locally (`/mnt/ext1/system/bin/<id>.<ext>` exists)
* `R` — the book is remote-only (it lives in the API server)

The bottom bar is the pager: `n / total` and `<` / `>` buttons when
there is more than one page of books.

Tapping a thumbnail resolves the configured `open-with` app for the
file's extension, fetches the file from the API (if not already on
device), and launches the chosen app via `NewTaskEx()` with the
downloaded path as the first argument.  In the API server config
(`api/config/server.json`) the `open_with` table maps extensions to
ordered candidate apps, e.g. `epub: [eink-reader, bookshelf]`.

### Languages

The UI auto-detects language from the firmware's `LANG` environment
variable and translates all of the visible strings.  Supported today:
English, German, French, Italian.  Add a new language by extending the
`g_i18n` table near the top of `bookshelf.c`.

### Downloading and opening books

1. User taps a tile.
2. The app POSTs `{id, ext}` to `/api/v1/open-with`.  The server
   returns `{app, url, ext}` for the first available reader in
   `open_with[<ext>]`.
3. The app `QuickDownload`s the file from the resolved URL (with the
   bearer token appended as `?access_token=`) and saves it to
   `/mnt/ext1/system/bin/<id>.<ext>`.
4. The app calls `NewTaskEx(app.app, args=[path], ...)` to launch the
   reader.

### Offline persistence (library store + cover cache)

The SQLite database `bookshelf_lib.db` next to the config file
(on-device: next to the binary; in the emulator:
`/tmp/bookshelf_lib.db`) is the single source of truth for the
library, using the firmware's own `libsqlite3.so.0` from
`/ebrmain/lib`.  There is no in-memory master list: a 100k-book
library never fits in device RAM, so every consumer pages through
the store instead —

- sync writes one bounded batch of rows per `/sync/delta` round
  inside a transaction (rollback journal, crash-safe);
- the grid/list renders from a materialised `view` table that the
  store rebuilds in SQL (filter/sort/group/collapse all happen in
  the query plan), paged `LIMIT`/`OFFSET` into page-sized row
  buffers;
- series cards are collapsed in SQL (one card per multi-member
  series, representative = highest volume);
- cover PNGs are cached under `covers/<id>.png` and blitted through
  a small LRU of decoded bitmaps, never one per book.

On boot the app opens the store, re-probes every book's on-device
file to resync its `downloaded` flag (files can vanish or appear
while the app is away), and builds the grid from the store *before*
attempting the auto-sync, so with the API unreachable the shelf
still shows the cached library and covers blit from the cache
instead of the network (log line `cover_tick cache hit id=…`).

A pre-sqlite `bookshelf_lib.json` store is imported once on first
boot and renamed to `bookshelf_lib.json.migrated`.
