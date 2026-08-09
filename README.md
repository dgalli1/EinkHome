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
app/                # the app sources (bs_*.c, bookshelf.h, sqlite3.h)
scripts/            # run.sh / run-visible.sh (emulator),
                    # install-device.sh (real device), install-koreader.sh
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

The cross-compile glue (`sdk/build_armel.sh` in the submodule) is the
only pbemu file EinkHome reaches into; it supports building apps from
outside the pbemu tree via `PBEMU_EXTRA_MOUNTS` and
`PBEMU_APP_INCLUDE_DIR` (see the Makefile).

## Build

```sh
make                       # → build/bookshelf.app
```

## Run in the emulator

```sh
./scripts/run-visible.sh   # build + stage + start, WITH the Wayland viewer
./scripts/run.sh           # headless variant (screenshots)
pbemu/pbemu stop           # stop the emulator
```

## Install on a real device

```sh
./scripts/install-device.sh <device-ip> [api-url]
```

## Tests

```sh
cd pbemu && ./setup-venv.sh && cd ..
pbemu/pbemu test U633_6.8.2817 -- tests/test_bookshelf.py -k offline
```

## Why the binary is still called bookshelf.app

The firmware selects its home task by the exact binary name
`bookshelf.app` (checked before the stock `/ebrmain/bin/bookshelf.app`
when an override exists at `/mnt/ext1/system/bin/bookshelf.app`).
Renaming the binary would boot into the stock PocketBook bookshelf
instead of EinkHome, so the on-device and in-emulator artifacts keep
the historic name while the app's display identity is EinkHome.
