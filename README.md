# EinkHome

EinkHome — the PocketBook home-screen replacement app, formerly the
`bookshelf` app inside the [pbemu](https://github.com/dgalli1/pbemu)
emulator project.  It is a native application for PocketBook e-readers:
a library home screen that syncs book metadata from a server (Kavita),
renders cover grids, manages downloads, launches the built-in reader or
KOReader, and doubles as the application launcher.

The app is **Rust**.  The UI toolkit (`eh_ui` workspace) is
platform-independent; thin per-platform backends bind it to the
PocketBook firmware (libinkview), to SDL2 on the desktop, and to the
emulator's headless test harness.

## Repository layout

```
Makefile            # build entry points: bookshelf.app / .armhf.app /
                    # bookshelf.pc / bookshelf.test + lint + test
eh_ui/              # the Rust workspace:
  crates/eh_hal     #   platform contract: framebuffer, input, keyboard,
                    #   network seams (the KOReader device-abstraction analog)
  crates/eh_render  #   offline rasteriser (icon baking, TXT-cover art)
  crates/eh_app/src/ui  #   Slint markup + bridge (screens, input, painting)
  crates/eh_app     #   the app: shelf, search, downloads, sync, settings,
                    #   launcher, sysapp promote, store (SQLite), config
  crates/eh_backend_inkview  # PocketBook firmware backend (device/emulator)
  crates/eh_backend_sdl      # SDL2 desktop backend + the headless IPC
                             # control socket (tests/ drives it)
  crates/eh_backend_linuxfb  # raw-linux-fb backend (experimental)
  crates/eh_pb      # staticlib facade: libinkview boot + event pump → .app
  crates/eh_host    # host binary: SDL window + IPC control plane
                    # (→ build/bookshelf.test for the e2e suite)
  crates/eh_demo    # scripted demo bins (visual verification)
sdk/                # cross-compile wrappers (build_armel.sh / build_armhf.sh)
                    # + pb-demo/main.c (the libinkview task shim) + SDK
scripts/            # run.sh (headless emulator), run-visible-pb.sh
                    # (emulator+viewer), run-visible-sdl.sh (SDL desktop),
                    # setup.sh (bootstrap), install-device.sh /
                    # install-koreader-device.sh (real device),
                    # install-koreader.sh (emulator KOReader),
                    # uninstall-device.sh, test.sh, dev tools, legacy/
docs/               # ARCHITECTURE.md (crate layers, workers, test tiers)
                    # + playwright-report-spec.md
                    # Start with docs/ARCHITECTURE.md before diving in.
 api/                # mock Kavita REST API server (+ tests)
tests/              # e2e suites: emulator (bookshelf.py) and SDL
                    # (offline_sdl, cover_warm_sdl) + support framework
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
commit there, push it, and bump the submodule pointer here.

The cross-compile glue (`sdk/build_armel.sh`, in this repo) links the
Rust staticlib against the firmware rootfs staged in the pbemu
submodule via `PBEMU_FIRMWARE_DIR` (see the Makefile).

## Build

First-time setup needs two ARM Rust targets plus cargo-zigbuild and zig
(the shim's C and sqlite's bundled C cross-compile through zig, pinned
to the firmware's glibc):

```sh
rustup target add armv7-unknown-linux-gnueabi armv7-unknown-linux-gnueabihf
cargo install cargo-zigbuild          # distro zig works too
```

Then:

```sh
make                       # → build/bookshelf.app      (emulator + armel devices)
make armhf                 # → build/bookshelf.armhf.app (InkPad One)
make pc                    # → build/bookshelf.pc        (host SDL, visible window)
make test-host             # → build/bookshelf.test      (headless IPC host)
make lint                  # clippy + python lints
```

`bookshelf.app` is linked by `sdk/build_armel.sh`: the eh_pb staticlib +
`sdk/pb-demo/main.c` (a ~40-line libinkview task shim) against the
firmware's libc/libinkview.

## Run in the emulator

```sh
./scripts/run-visible-pb.sh   # build + stage + start, WITH the Wayland viewer
./scripts/run.sh              # headless variant (screenshots)
pbemu/pbemu stop              # stop the emulator
```

## Run as a desktop app (SDL)

```sh
./scripts/run-visible-sdl.sh  # make pc + start the API + open the SDL window
# or: make pc; ./build/bookshelf.pc
```

Set `EH_SOCKET=/path/to.sock` to expose the headless IPC control plane
(`tap x y`, `type TEXT`, `kb_commit`, `hash`, `shot PATH`, `state`,
`quit`) — the same newline protocol the e2e suite drives.

## Dev releases (CI)

Every push to `main`/`demo` runs the `dev-release` pipeline, which
compiles every executable for every target and publishes them to the
rolling [dev release](https://github.com/dgalli1/EinkHome/releases/tag/dev)
(prerelease, tag always points at the built commit):

| asset | target |
| --- | --- |
| `einkhome-<sha>.zip` | PocketBook: `bookshelf.app` (armel) + `bookshelf.armhf.app` (armhf) + `install.sh` |
| `bookshelf.pc` | desktop SDL (x86_64 linux) |
| `bookshelf.test` | headless SDL + e2e IPC (x86_64 linux) |
| `einkhome-dev.apk` | Android (arm64-v8a + x86_64) |

`SHA256SUMS` covers all assets.  Pull requests rehearse the builds but
never publish.

## Install on a real device

```sh
./scripts/install-device.sh <device-ip> [api-url]
./scripts/install-koreader-device.sh <device-ip> [version]   # push KOReader
./scripts/uninstall-device.sh <device-ip>                    # remove the custom bookshelf
```

Settings → "System app" promotes the running binary to the firmware's
home-task override path (`EH_SYSAPP_DIR` overrides it for testing);
toggling again removes it.

## Development tools

```sh
./scripts/setup.sh                    # one-time bootstrap: submodule, venv, pbdev
                                      # image, firmware, SDK, emulator artifacts
python3 scripts/build_ol_corpus.py    # build a mock-book corpus (JSONL) from Open
                                      # Library dumps (served by the mock provider
                                      # via PBEMU_MOCK_CORPUS)
python3 scripts/click_first_book.py   # open the first staged book in the emulator
```

`scripts/legacy/` holds superseded on-device helper scripts; they are
kept for reference and for uninstalling from devices that still carry
them.

## Tests

```sh
./scripts/test.sh            # rust unit tests + api unit tests + emulator e2e
./scripts/test.sh --pbemu    # ... plus the pbemu submodule's own suite
./scripts/test.sh -- -k offline   # pass pytest args through to the e2e suite
# or: make test / make test-rust (rust unit tests only)

# SDL (native PC) suites — fast, no emulator:
EH_TEST_BACKEND=sdl pbemu/.venv/bin/python -m pytest \
  tests/test_bookshelf.py tests/test_offline_sdl.py \
  tests/test_cover_warm_sdl.py tests/test_rust_app_sdl.py
```

The full local inventory, all runnable today:

| Tier | Suite | Needs |
|---|---|---|
| Rust unit | `make test-rust` (`cargo test --workspace`) | nothing |
| API unit + lint | `make lint-py` (ruff, mypy, pytest) | python3 |
| SDL e2e | `EH_TEST_BACKEND=sdl pytest tests/…` | SDL2 |
| Emulator e2e | `scripts/test-all-firmwares.sh --device <fw>` | podman + firmware |
| Scale (100k) | `PBEMU_SYS_TMPFS=1 … pytest tests/test_bookshelf_scale.py` | podman + firmware |

Plus a diagnostic capture: `tests/test_visual_capture.py` shoots every
UI page to `build/screenshots/visual/` for manual layout review (same
env as the e2e suite).

Run every pre-commit gate in one shot: `make verify`
(fmt + clippy + doc-check + unit tests + lint-py).


The emulator e2e suite needs podman, the staged firmware and staged
books in `pbemu/U633_6.8.2817/.live/mnt/ext1/books/` (stage with
`./scripts/stage-mock-books.sh pbemu/U633_6.8.2817`).  `test.sh`
resolves the firmware strictly inside `pbemu/`: when your staging
lives at the repository root instead (as a raw `U633_6.8.2817/`
directory next to `build/`), link it through —
`ln -s ../U633_6.8.2817 pbemu/U633_6.8.2817` — or export
`PB_TEST_FIRMWARE` / point the suite at the root copy yourself.
Scale-tier runs against the local emulator also want
`PBEMU_SYS_TMPFS=1` (hosted-runner /sys workaround, harmless
locally).

## Why the binary is still called bookshelf.app

The firmware selects its home task by the exact binary name
`bookshelf.app` (checked before the stock `/ebrmain/bin/bookshelf.app`
when an override exists at `/mnt/ext1/system/bin/bookshelf.app`).
Renaming the binary would boot into the stock PocketBook bookshelf
instead of EinkHome, so the on-device and in-emulator artifacts keep
the historic name while the app's display identity is EinkHome.
