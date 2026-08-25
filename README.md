# EinkHome

EinkHome — the PocketBook home-screen replacement app, formerly the
`bookshelf` app inside the [pbemu](https://github.com/dgalli1/pbemu)
emulator project.  It is a native C application for PocketBook
e-readers: a library home screen that syncs book metadata from a
server (Kavita), renders cover grids, manages downloads, launches the
built-in reader or KOReader, and doubles as the application launcher.

The git history of every file in this repository was carried over
from pbemu; the original `bookshelf/` sources still exist there and
are not touched by this repository.

## Repository layout

```
Makefile            # the only source list; builds build/bookshelf.app
app/                # the app sources, split by layer:
  core/             #   boot/event loop, HTTP, config, i18n, worker, bs_core.h
  data/             #   store (SQLite), model/sync, local scan, metadata, progress
  ui/               #   drawing: grid, top bar, popups, overlays, browsers
  action/           #   downloads/context menu, input hit-testing, app launcher
  vendor/           #   cJSON, sqlite3.h
scripts/            # run.sh (headless), run-visible-pb.sh (emulator+viewer),
                    # run-visible-sdl.sh (native SDL desktop window), setup.sh
                    # (bootstrap), install-device.sh / install-koreader-device.sh
                    # (real device), install-koreader.sh (emulator KOReader),
                    # uninstall-device.sh, test.sh, build_ol_corpus.py +
                    # click_first_book.py (dev tools), legacy/ (superseded
                    # on-device helper scripts, kept for reference)
sdk/                # cross-compile wrapper + SDK bootstrap
tests/              # emulator integration test suite + support framework
pbemu/              # git submodule: the emulator project this app runs in
```

## The pbemu submodule

The pbemu submodule provides everything around the app — the
emulator (qemu-arm + shim), the staged firmware
(`pbemu/U633_6.8.2817`), and the developer CLI (`pbemu/pbemu`).
EinkHome contains the app itself, its mock Kavita REST API server
(`api/`), and the glue that builds, stages, and tests them against
the submodule.

```sh
git submodule update --init --recursive   # first checkout
(cd pbemu && ./setup-venv.sh)             # once: test/API venv
pbemu/pbemu install U633_6.8.2817         # once: stage the firmware
```

If something in the emulator misbehaves (shim, informer, viewer,
fake-fb, ...), the fix belongs in the submodule: edit inside `pbemu/`,
commit there, push it, and bump the submodule pointer here:

```sh
cd pbemu
# ...make the fix, test it...
git push origin <branch>
cd ..
git add pbemu && git commit -m "submodule: bump pbemu (…)"
```

The cross-compile glue (`sdk/build_armel.sh`, in this repo) builds the
app against the firmware rootfs staged in the pbemu submodule via
`PBEMU_FIRMWARE_DIR` (see the Makefile).

## Build

```sh
make                       # → build/bookshelf.app
```

## Run in the emulator

```sh
./scripts/run-visible-pb.sh   # build + stage + start, WITH the Wayland viewer
./scripts/run.sh              # headless variant (screenshots)
pbemu/pbemu stop              # stop the emulator
```

## Run as a desktop app (SDL)

The same app source also builds natively for the PC — a Wayland/X11
window rendered by SDL2 (the `bs_plat_sdl` backend behind the platform
seam).  Requires SDL2/SDL2_ttf/SDL2_image/libcurl dev packages.

```sh
./scripts/run-visible-sdl.sh  # make pc + start the API + open the SDL window
# or: make pc; ./build/bookshelf.pc
```

## Dev releases (CI)

Every push to `main`/`demo` runs the `dev-release` pipeline, which
compiles every executable for every target and publishes them to the
rolling [dev release](https://github.com/dgalli1/EinkHome/releases/tag/dev)
(prerelease, tag always points at the built commit):

| asset | target |
| --- | --- |
| `einkhome-<sha>.zip` | PocketBook: `bookshelf.app` (armel) + `bookshelf.armhf.app` (armhf) + `install.sh` |
| `bookshelf-linux-armv7` | Kobo / reMarkable 1 / Cervantes / generic Linux fb (runtime-detecting; built when the `eh_device` crate is on the branch) |
| `bookshelf.pc` | desktop SDL (x86_64 linux) |
| `bookshelf.test` | headless SDL + e2e IPC (x86_64 linux) |
| `einkhome-dev.apk` | Android (arm64-v8a + x86_64; built when the `eh_android` crate is on the branch) |

`SHA256SUMS` covers all assets.  Pull requests rehearse the builds but
never publish.

## Install on a real device

```sh
./scripts/install-device.sh <device-ip> [api-url]
./scripts/install-koreader-device.sh <device-ip> [version]   # push KOReader
./scripts/uninstall-device.sh <device-ip>                    # remove the custom bookshelf
```

## Development tools

```sh
./scripts/setup.sh                    # one-time bootstrap: submodule, venv, pbdev
                                      # image, firmware, SDK, emulator artifacts
python3 scripts/build_ol_corpus.py    # build a mock-book corpus (JSONL) from Open
                                      # Library dumps → .cover-cache/mock_books.jsonl
                                      # (served by the mock provider via PBEMU_MOCK_CORPUS)
python3 scripts/click_first_book.py   # open the first staged book in the emulator and
                                      # capture page-1/page-10 screenshots into screenshots/
```

`scripts/legacy/` holds superseded on-device helper scripts (e.g. the
`bookshelf-wrapper.sh` startup hook that older installs deployed to
`/mnt/ext1/system/bin/bookshelf.app`); they are kept for reference and
for uninstalling from devices that still carry them.

## Tests

```sh
./scripts/test.sh            # api unit tests + full emulator e2e suite
./scripts/test.sh --pbemu    # ... plus the pbemu submodule's own suite
./scripts/test.sh -- -k offline   # pass pytest args through to the e2e suite
# or: make test
```

The emulator e2e suite needs podman, the staged firmware
(`pbemu/pbemu install`) and staged books in
`pbemu/U633_6.8.2817/.live/mnt/ext1/books/`.

## Why the binary is still called bookshelf.app

The firmware selects its home task by the exact binary name
`bookshelf.app` (checked before the stock `/ebrmain/bin/bookshelf.app`
when an override exists at `/mnt/ext1/system/bin/bookshelf.app`).
Renaming the binary would boot into the stock PocketBook bookshelf
instead of EinkHome, so the on-device and in-emulator artifacts keep
the historic name while the app's display identity is EinkHome.
