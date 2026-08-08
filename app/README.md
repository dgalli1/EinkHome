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
push the binary + config and install it as the home screen:

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
* scps the binary + config into `/mnt/ext1/system/bin/` as
  **`bookshelf.app`** / `bookshelf.cfg` — the home-task override path
  (see below),
* clears the on-device log and kills any stale `bookshelf.app` so the
  next launch starts clean.

### Home-task override (auto-launch on boot)

The firmware's launcher, `monitor.app`, resolves the home/startup app
by checking `/mnt/ext1/system/bin/bookshelf.app` **before** the
read-only `/ebrmain/bin/bookshelf.app` (verified in the launcher's
disassembly at `0x33b48`–`0x33b74`). `/mnt/ext1` is the writable user
partition, so dropping our binary there makes **our bookshelf the home
task**, registered under the app name `bookshelf.app`: pressing the
Home button anywhere brings OUR bookshelf to the foreground (taskmgr's
`main_menu` action), not the stock UI.  The binary is installed
directly — no wrapper script — because a wrapper's `exec` would
register the home task as the wrapper, which breaks the reader's
book-open handshake (the built-in reader shows an hourglass and
closes).  If the binary is ever missing, the launcher falls back to the
stock `/ebrmain/bin/bookshelf.app` on its own.

**Auxiliary firmware services.**  The stock boot starts the resident
background services (`taskmgr`, `reader_controller`, `control_panel_mgr`,
`explorer`, `update_desktop_data`) as part of the home task's init
handshake; a fresh boot with our binary as the home task does not
trigger that startup, which leaves the control-panel Task Manager
button without a target and makes OpenBook's `reader_controller` poll
time out.  Our bookshelf therefore launches those services itself from
its deferred init (`launch_aux_services()`), using `NewTaskEx` with
`TASK_BACKGROUND` flags — the same approach the emulator's shim uses for
`reader_controller` (`ensure_reader_controller_task()`).  Once the
services are resident (they relaunch on every bookshelf start), the
Task Manager button and the book-open chain behave like stock.

To remove everything and restore the stock boot path:

```bash
bookshelf/uninstall-device.sh <device-ip>
# then reboot the device
```

Or do the install by hand:

```bash
scp build/bookshelf.app        root@<device-ip>:/mnt/ext1/system/bin/bookshelf.app
scp build/bookshelf.cfg        root@<device-ip>:/mnt/ext1/system/bin/bookshelf.cfg
ssh root@<device-ip> 'chmod +x /mnt/ext1/system/bin/bookshelf.app'
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

The binary syncs metadata on startup — deferred to a one-shot timer so
`EVT_INIT` returns immediately (a blocking network sync inside init
delays the firmware's main-menu task binding on the real device, which
breaks the control-panel Task Manager button and the
`reader_controller`-based book-open chain).  The local store is opened
and the grid built from it *before* the network sync, so an unreachable
API still shows the cached library, and the shelf refreshes once the
deferred sync settles.  Subsequent syncs are triggered via **⋯** →
**Sync**.

## UI

The on-screen layout has four regions stacked vertically in the
guest's logical space, which spans `[0, ScreenHeight() - panel_h)` —
the firmware draws the system status bar (day + 24h time + down-arrow
+ lightbulb + battery) into the TOP band `[0, panel_h)` of the
framebuffer and offsets all app drawing below it (draws past the
logical bottom wrap around to the top, so every surface must stay
inside the logical space):

```
+--------------------------------------------------+ <- 0
|  Fr 23:05      ⌄      (sync) wifi 💡 63% 🔋     |   system status bar (firmware, TOP)
+--------------------------------------------------+ <- panel_h (~106)
|  [⌂]                    [Q] [⬇] []            |   TOP_BAR_H (128 px)
+--------------------------------------------------+
|                                                  |
|   [book]    [book]    [book]                     |
|   [book]    [book]    [book]                     |   3×2 grid of thumbnails
|                                                  |
+--------------------------------------------------+
|              <  1 / 9  >                          |   PAGER_H (96)
+--------------------------------------------------+ <- ScreenHeight - panel_h (logical bottom)
```

The top bar style matches the firmware's standard `sudoku.app`
header:

* **Left** — a 96×96 house outline (home button).  While the Search
  page or a drilled-in series is showing it acts as "back"; on the
  plain library shelf it is a no-op (the app is the home-screen
  replacement, so closing it there would drop the user to the stock UI
  and, behind the boot wrapper, never come back).
* **Center** — a title while a drilled-in series (series name), the
  Search page ("Search"), or an active search query (the query text,
  truncated) is showing; the plain library shelf carries no title.
* **Right** — a 96×96 hit area with three black hamburger lines.
  Tapping opens the in-app "More" menu (sort, view, sync).
* **Left of the menu button** — a 96×96 sync hit area with two black
  arc arrows that rotate while a sync or download is in flight.
  Tapping it runs a library sync.
* **Left of the sync button** — a 96×96 magnifying-glass icon.
  Tapping it opens the Search page (input row + previous search
  terms); the old full-width search row was dropped to reclaim the
  vertical space for the shelf.

The "More" menu is a right-anchored 75%-width panel with the
following items, in order: **Sync**, **Title A–Z**, **By author**,
**By series**, **Recent**, **Grid**, **List**, **Download all**,
**Settings**, **Applications**.  (A "System menu" entry existed while
the firmware bar was unreliable; tapping the top status strip now
opens the firmware control panel directly, so the entry was dropped.)

### How the firmware system status bar works

The firmware draws the system status bar at the TOP of the screen
("Fr 23:05 + down-arrow + sync/wifi + lightbulb + battery", rows
`[0, PanelHeight())` of the physical screen), with all app content
below it.  The mechanism (from `libinkview` disassembly + device
screenshots, U633 6.8.2817):

* The guest's logical drawing space stays the full `ScreenHeight()`
  (1448 rows on the U633) and drawing is NOT shifted: app content
  occupies SHM rows `[0, ScreenHeight()-PanelHeight())` and
  `iv_update_panel()` blits the bar at
  `y = ScreenHeight()-PanelHeight()` — the BOTTOM rows of the task
  framebuffer.
* `OpenScreen()` stores `GetThemeInt("panel_position", 0)` in the
  library state (offset `0x160`).  When it is 1, `SetPanelType(type)`
  (bit 3 of `type` clear) sets the per-task
  `iv_fbinfo::fb_y_offset = PanelHeight()` (offset `0x8c` in the task
  SHM header) — the **wrapped scanout origin**.  The display pipeline
  starts scanout `fb_y_offset` rows into the framebuffer and wraps:
  the bottom rows (the bar) appear at the top of the screen and app
  row 0 lands at physical row `PanelHeight()`.  (The wrap is also why
  drawing "too far down" makes content appear at the top, e.g. while
  scrolling the launcher.)
* Pointer input arrives in physical screen coordinates and the guest
  subtracts `fb_y_offset`, so the app sees logical coordinates.

Registration happens in `main()` BEFORE `InkViewMain()`, in the stock
order (doing it inside `EVT_INIT` corrupts the per-task fbinfo on the
live device — `ScreenHeight()` collapses to the panel height and the
layout fights the bar for the same rows):

```c
InitInkview(0x4110);
IvSetAppCapability(1);      /* weak-linked; absent from the SDK lib */
SetOrientation(0);
SetDefaultOrientation(-1);  /* weak-linked; absent from the SDK lib */
SetPanelType(1);            /* the stock bookshelf's literal value */
```

In `EVT_INIT` we then reserve the band and force a first paint:

```c
g_state.panel_h = PanelHeight();  /* height reserved at the TOP */
DrawPanel(NULL, "Bookshelf", NULL, -1);
stamp_panel();   /* iv_update_panel(0) or our self-drawn fallback */
Repaint();
```

The pbemu emulator replicates the display side of this contract in the
PC viewer (and `frame_dump`): both read `fb_y_offset` from the task SHM
header (via the informer snapshot) and present SHM row `r` at screen
row `(r + fb_y_offset) % height`, exactly like the device's scanout.
The shim only supplies the theme answer the device resolves natively
(`GetThemeString("panel_position") = "1"`); `SetPanelType()` itself is
NOT interposed.

On a live device where the panel painter never activates for this task
(`PanelHeight()` returns 0 at `EVT_INIT`), the app falls back to a
**self-drawn** strip of the same height (`SELF_PANEL_H`) in the same
top band: day + 24h time on the left, frontlight bulb + battery
outline on the right, drawn by `draw_system_strip()`.  `stamp_panel()`
picks the right painter; `g_state.panel_h` is forced to `SELF_PANEL_H`
so the top bar never sits flush against the top edge.  Tapping the
strip (either painter) opens the firmware control panel, the same
gesture as the stock UI.

We deliberately **do not** call `SetPanelType(PANEL_DISABLED)` or
`iv_fullscreen()`.  Without those the system panel stays visible —
swipe down from the top to access Wi-Fi / Bluetooth / Sync.

`IvSetAppCapability` and `SetDefaultOrientation` are exported by the
firmware's `libinkview` but absent from this SDK vintage's bundled
lib, so both are declared `__attribute__((weak))` and guarded with a
NULL check; the firmware's `SetOrientation()`/`SetDefaultOrientation()`
are NULL-fb-safe before registration (they log and return while
`hw_getframebuffer()` is still NULL), which is exactly why the stock
app can call them in `main()`.

Search lives on its own sub-page, opened from the magnifying-glass
icon in the top bar.  The page shows a search input row at the top;
tapping it opens the firmware's native on-screen keyboard, and
pressing Enter (or the keyboard's OK button) filters the visible books
by title or author and returns to the shelf.  Below the input row the
previously committed search terms are listed (newest first, capped at
20, persisted in the library database); tapping a term re-runs that
search immediately.  While a search is active the query is shown as
the top-bar title, and the search page's input row is prefilled with
it so the filter can be edited or cleared.

The main area is a 3×2 grid of thumbnails.  Each tile shows the book
cover (cached PNG while the real cover downloads, hatch placeholder
otherwise), the book title, the author, and a corner badge:

* `v` — the book is downloaded locally (`/mnt/ext1/system/bin/<id>.<ext>` exists)
* `R` — the book is remote-only (it lives in the API server)

The bottom bar is the pager: `n / total` and `<` / `>` buttons when
there is more than one page of books.

Tapping a book that is not yet on device shows the download-progress
popup (a centred sheet with the queue/batch progress bar); when the
file lands, the reader opens automatically.  Already-downloaded books
open immediately.  The download popup is also shown by **Download all**
(More menu) and the context-menu **Download**.  While any download is
active the popup is modal — downloads never run in the background —
and a tap or Back closes it only once the queue has drained.  The
opening itself goes through `OpenBook()` — the firmware's canonical
book-open path, which routes the book to `reader_controller` and the
default reader for the extension.  An explicitly selected third-party
reader (KOReader) is still launched via `NewTaskEx()` with the book
path as its argument.
(The API server's `open_with` table in `api/config/server.json` still
maps extensions to candidate apps, but the device validates the resolved
app exists and falls back to `OpenBook()` — the stock firmware has no
`pdfviewer.app`, so the old `NewTaskEx("/ebrmain/bin/pdfviewer.app", …)`
failed silently on PDFs.)


**Applications** opens the in-app launcher: a scrollable column of the
firmware's app groups (read from `apps_db.json` / `view.json`) plus any
user-installed apps.  Drag vertically to scroll; a group heading always
keeps its own row, so headings never clip the previous group's last row.
Tapping a tile launches the app via `NewTaskEx()`.

### Languages

### Downloading, opening, and deleting books

1. User taps a tile.
2. If the file is not on device, the download-progress popup opens and
   the file fetch runs on a worker thread (`QuickDownload` with the
   bearer token appended as `?access_token=`), saving to
   `/mnt/ext1/system/bin/<id>.<ext>` — the event loop stays responsive
   the whole time, and the popup is modal until the queue drains (no
   background downloads).  When the queue drains the reader
   opens automatically.
3. Standard reader (or Auto): the app calls
   `OpenBook(path, NULL, 1)` — the firmware's canonical book-open
   path; `reader_controller` picks the right reader for the extension
   and brings it to the foreground with the book registered.
4. Explicit third-party reader (KOReader): the app calls
   `NewTaskEx(app, args=[app, path], ...)` with the stock launch
   flags (0x25).
5. Long-pressing a tile opens the context menu: a book offers
   **Open** (same as a single tap), **Download**, **Delete**; a series
   card offers **Download all** / **Delete series**.

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
  a small LRU of decoded bitmaps, never one per book.  On a colour
  display (`device_display_colormask() != 0`, e.g. the PocketBook
  Color 633) covers decode as RGB24 — the same choice the stock
  bookshelf.app makes — and are written straight into the libinkview
  canvas (RGB byte order, nearest-neighbour scale), because libinkview's
  own 8-bit draw pipeline flattens 24-bit bitmaps to greyscale.  The
  QPA bridge that eink-reader uses writes the canvas the same way,
  which is how it renders colour thumbnails.  Greyscale displays keep
  the 8-bit decode and the normal StretchBitmap path.  (The colour
  signal is `device_display_colormask()`, not the framebuffer ioctl —
  the ioctl reports 8bpp on some Colour hardware.)
On boot the app opens the store, re-probes every book's on-device
file to resync its `downloaded` flag (files can vanish or appear
while the app is away), and builds the grid from the store *before*
attempting the auto-sync, so with the API unreachable the shelf
still shows the cached library and covers blit from the cache
instead of the network (log line `cover_tick cache hit id=…`).

A pre-sqlite `bookshelf_lib.json` store is imported once on first
boot and renamed to `bookshelf_lib.json.migrated`.
