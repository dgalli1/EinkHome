#!/usr/bin/env python3
"""Generate build/compile_commands.json for the EinkHome app.

clang-tidy (and most C static analysers) need a per-file compilation
database.  The app builds via a single ``cc`` invocation in
sdk/build_pc.sh (SDL) / sdk/build_armel.sh (PB), so it emits no compile
database of its own.  This script synthesises one, resolving each app
source to the exact flag set the real builds use, so the analysers see
what the compiler sees.

Design constraints:
  * The app's source list lives in ONE place: the `SOURCES` variable in
    the root Makefile (see the Makefile header comment).  This script
    parses it out of ``make -pn`` output rather than maintaining a
    second copy.
  * Both platform backends are covered: the SDL entries are host
    compilable (all dev deps present on the build host); the lone PB
    backend file (app/platform/bs_plat_pb.c) is emitted against the
    PocketBook SDK include dir, which lives in-repo.
  * cJSON.c is vendored third-party code; it is skipped so analysers
    don't drown in upstream findings.

Usage:
    scripts/gen-compile-commands.py [--output build/compile_commands.json]
        [--sdk-include sdk/pocketbook-sdk-b288/include]
"""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
MAKEFILE = REPO_ROOT / "Makefile"
OUT_DEFAULT = REPO_ROOT / "build" / "compile_commands.json"
SDK_INCLUDE_DEFAULT = REPO_ROOT / "sdk" / "pocketbook-sdk-b288" / "include"

APP_INCLUDES = (
    "app/core",
    "app/data",
    "app/ui",
    "app/action",
    "app/vendor",
    "app/platform",
)

PB_ONLY_FILE = "app/platform/bs_plat_pb.c"
EXCLUDED_VENDOR = {"app/vendor/cJSON.c"}


def make_sources() -> list[str]:
    """Return the app sources as listed in the Makefile's SOURCES variable.

    The Makefile writes ``SOURCES := core/bs_main.c ...`` (paths relative
    to ``app/``) and ``make -pn`` joins the backslash-continuations onto a
    single logical line.  Return the tokens from the first (real, ``:=``)
    definition, prefixed with ``app/``.
    """
    proc = subprocess.run(
        ["make", "-pn"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"error: make -pn failed: {proc.stderr.strip()}")
    for line in proc.stdout.splitlines():
        # Only the real definition carries ':='; the later sink redefinition
        # (bare "SOURCES:") has no value and is skipped.
        if not line.startswith("SOURCES :="):
            continue
        value = line.split(":=", 1)[1].strip()
        return [f"app/{t}" for t in value.split() if t]
    sys.exit("error: SOURCES := not found in make -pn output")


def _sdl_pkg_cflags() -> list[str]:
    proc = subprocess.run(
        ["pkg-config", "--cflags", "sdl2", "SDL2_ttf", "SDL2_image", "libcurl", "sqlite3", "zlib"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        print(f"warning: pkg-config SDL cflags failed: {proc.stderr.strip()}", file=sys.stderr)
        return []
    return shlex.split(proc.stdout)


def build_sources() -> list[str]:
    return sorted(set(make_sources()))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--output", default=str(OUT_DEFAULT))
    ap.add_argument("--sdk-include", default=str(SDK_INCLUDE_DEFAULT))
    args = ap.parse_args()

    out = Path(args.output)
    sdk_inc = Path(args.sdk_include)
    if not sdk_inc.is_dir():
        print(f"error: sdk include dir not found: {sdk_inc}", file=sys.stderr)
        return 2
    if not MAKEFILE.is_file():
        print(f"error: Makefile not found at {MAKEFILE}", file=sys.stderr)
        return 2

    srcs = [s for s in build_sources() if s not in EXCLUDED_VENDOR]
    sdl_incs = [str(REPO_ROOT / d) for d in APP_INCLUDES]
    sdl_cflags = _sdl_pkg_cflags()

    entries = []
    for src in sorted(srcs):
        if src == PB_ONLY_FILE:
            continue
        entries.append(
            {
                "directory": str(REPO_ROOT),
                "file": str(REPO_ROOT / src),
                "arguments": [
                    "clang",
                    *[f"-I{inc}" for inc in sdl_incs],
                    "-Wall",
                    "-Wextra",
                    "-O2",
                    "-g",
                    "-DBS_PLATFORM_SDL",
                    *sdl_cflags,
                    "-std=gnu11",
                    str(REPO_ROOT / src),
                    "-c",
                    "-o",
                    "/dev/null",
                ],
            }
        )

    # PB backend: parsed against the SDK headers, no BS_PLATFORM_SDL.
    entries.append(
        {
            "directory": str(REPO_ROOT),
            "file": str(REPO_ROOT / PB_ONLY_FILE),
            "arguments": [
                "clang",
                *[f"-I{inc}" for inc in sdl_incs],
                f"-I{sdk_inc}",
                "-Wall",
                "-Wextra",
                "-O2",
                "-g",
                "-std=gnu11",
                str(REPO_ROOT / PB_ONLY_FILE),
                "-c",
                "-o",
                "/dev/null",
            ],
        }
    )

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(entries, indent=2) + "\n")
    print(f"wrote {len(entries)} compile entries -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
