# EinkHome Rust GUI toolkit (eh_ui)

A portable GUI toolkit for the EinkHome PocketBook app (and future Kobo /
Kindle / Android), following KOReader's device-abstraction pattern: the UI is
fully platform-independent and drives a thin framebuffer contract.

## Crates

| Crate | Role |
|---|---|
| `eh_hal` | The platform contract: `Framebuffer` (canvas + region refresh with e-ink waveform mode), `Screen` (with native-panel reservation), `InputEvent`, `RefreshMode`, `Rect`. `#![no_std]`. |
| `eh_render` | Software rasteriser (grayscale8 / RGB24 / RGBA32): fills, lines, glyph strings via `fontdue`, nearest-neighbour image blit. KOReader-`blitbuffer` analog. |
| `eh_layout` | Responsive layout over `taffy` (CSS flex/grid). `Breakpoint::{Narrow, Std, Wide}` is resolved once per frame from screen width — the app's `eh_view_cols()` thresholds as data, not inline `if`s. |
| `eh_shell` | Retained widget layer: a `Widget` draws AND hit-tests itself from the same taffy-computed rect, so draw/hit cannot drift (the core fix for the C app's `eh_grid.c`/`eh_input.c` duplication). `Screen` owns tree + dirty regions + one `present()` entry point. |
| `eh_backend_linuxfb` | Direct `/dev/fb0` backend: mmap + FB ioctls + e-ink update. The KOReader route for real PB/Kobo/Kindle/reMarkable hardware. |
| `eh_backend_inkview` | PocketBook backend over libinkview: draws into `GetCanvas()` (physical fb on device; observable task-SHM in pbemu) and preserves the native status panel (refreshes clamp above `content_bottom`). |
| `eh_backend_sdl` | Host dev/visual-verification backend (RGBA canvas → streaming texture). |
| `eh_pb` | PocketBook app facade: `extern "C"` `eh_pb_init`/`eh_pb_on_event`/`eh_pb_panel_height`, linkable with the C shim at `sdk/pb-demo/main.c` via `build_armel.sh`. |
| `eh_demo` | A portable cover-grid shelf exercising the breakpoint layout; the same screen runs on SDL (host) and the device backends. |

## Build

```sh
cargo build            # host, all libs
cargo test             # layout/breakpoint tests
cargo run --features sdl -p eh_demo --bin eh_demo_sdl   # host demo (SDL)
SDL_VIDEODRIVER=dummy EH_DUMP=/tmp/f.ppm cargo run --features sdl \
    -p eh_demo --bin eh_demo_sdl                       # headless frame dump
cargo build --release --target armv7-unknown-linux-gnueabi   -p eh_pb # armel
cargo build --release --target armv7-unknown-linux-gnueabihf -p eh_pb # armhf
```

The device staticlibs (`libeh_pb.a`) cross-compile to real ARM EABI5 for both
ABIs (confirmed by inspecting the embedded objects).

## Linking a device `.app`

`sdk/pb-demo/main.c` + `build_armel.sh` link the toolkit into a runnable guest
ELF. **Requirement:** build std from source against the pinned glibc via
`cargo-zigbuild` + a nightly with `rust-src` (the prebuilt `armv7-unknown-linux-gnueabi`
std references `stat64`/`open64`/`statx` absent from the firmware glibc 2.23);
the `.cargo/config.toml` here enables `-Z build-std`. `cargo-zigbuild` writes
the `.2.23`-built staticlib to `target/armv7-unknown-linux-gnueabi/release/`:

```sh
# one-time: rustup toolchain install nightly --component rust-src
cargo +nightly zigbuild --release --target armv7-unknown-linux-gnueabi.2.23 -p eh_pb
# then:
PBEMU_FIRMWARE_DIR="$(pwd)/pbemu/U633_6.8.2817" \
  LINK_INPUTS="eh_ui/target/armv7-unknown-linux-gnueabi/release/libeh_pb.a" \
  sdk/build_armel.sh sdk/pb-demo/main.c --output build/pb-demo.app
```

Std built this way is clean — `nm -u` shows none of the newer-glibc syscall refs.
`eh_lib` (the extraction crate) is `#![no_std]` so it never needs this; the GUI
toolkit uses `std`.

## Native status bar

`Screen::content_height` is the boundary: on PocketBook
`[content_height, height)` is the firmware type-1 panel strip. `eh_backend_inkview`
clamps every refresh above it so the native clock/battery/wifi bar survives;
when `PanelHeight()==0` (live device) the app draws its own strip via
`eh_demo::draw_self_panel` — the portable `eh_draw_system_strip`.

## Verification status (2026-08-18)

- Host: renders a correct 3-column responsive shelf frame (headless PPM dump),
  verified via pixel analysis; `cargo test` green, zero warnings.
- Cross-compile: `eh_pb` staticlibs build for armel + armhf, embedded objects
  confirmed ARM EABI5.
- Emulator (pbemu): the Rust demo **boots, registers as an inkview task,
  draws through `GetCanvas()`, and issues `FullUpdate` + `PartialUpdate`**
  (confirmed in the shim's `/var/log/repaint.log`). Same code path works
  identically on a real PocketBook (the loader bug below is glibc-wide, not
  pbemu-specific).

### Root cause found + fixed: `realloc` interposition
The demo originally crashed at `OpenTheme` with
`ld.so: dl-minimal.c:137: realloc: Assertion 'ptr == alloc_last_block' failed!`
everywhere (pbemu AND real hardware). Root cause, via `LD_DEBUG=bindings`: the
shared-lib-only link (libc passed by full path, no `libc_nonshared`) leaves
`realloc`/`malloc`/`free`/`calloc` **unversioned**, so they bind to
`ld-linux.so.3`'s dl-minimal bootstrap allocator during dl-open — which can
only reallocate its tail block. Fix: strong `malloc`/`realloc`/`free`/`calloc`
forwarders in `sdk/pb-demo/main.c` that call the firmware's real
`__libc_malloc`/`__libc_realloc`/`__libc_free` (matching what a normal `-lc`
link's libc_nonshared does). Also added the `stat64`-family shims (the
firmware's shared-only link omits glibc's `libc_nonshared` macro aliases).

Observability note: pbemu's fake `/dev/fb0` is a private memfd invisible to
`frame_dump`; the inkview-task SysV SHM is the observer path. On a real
device the canvas is the physical framebuffer, so this is emulator-only.