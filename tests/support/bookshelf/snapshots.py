"""Framebuffer snapshot recording for the bookshelf e2e suite.

Every interactive action captures the emulator framebuffer as a PNG
(Playwright-style), so a run can be inspected visually from the CI
artifacts: screenshots land under ``build/screenshots/<test>/`` with
zero-padded, action-labelled names plus an ``index.txt``.

The framebuffer is dumped with the host-side ``frame_dump`` probe
(``--ppm`` into the guest-visible .live/tmp) and converted to PNG with
a small stdlib encoder — no image library or container round-trip
needed.  e-ink frames are mostly white, so the PNGs stay tiny
(~10-30 KB).

Capture is best-effort: a failing dump never fails the test, it just
records the problem in the index.
"""

from __future__ import annotations

import struct
import time
import zlib
from pathlib import Path

# The recorder that captured the *current* test's screenshots.  Fixtures call
# begin() once per test, so this tracks the live test's recorder; the report
# plugin harvests steps from it at teardown.
ACTIVE: SnapshotRecorder | None = None

_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def _png_chunk(tag: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + tag
        + payload
        + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
    )


def ppm_to_png(ppm: bytes) -> bytes:
    """Convert a P6 PPM blob to PNG bytes (RGB, 8-bit)."""
    if not ppm.startswith(b"P6"):
        raise ValueError("not a P6 PPM")
    pos = 2
    tokens: list[int] = []
    while len(tokens) < 3:
        while pos < len(ppm) and ppm[pos:pos + 1].isspace():
            pos += 1
        start = pos
        while pos < len(ppm) and not ppm[pos:pos + 1].isspace():
            pos += 1
        if pos == start:
            raise ValueError("malformed PPM header")
        tokens.append(int(ppm[start:pos]))
    width, height, _maxval = tokens[:3]
    pos += 1  # single whitespace between maxval and the pixel data
    pixels = ppm[pos:]
    row_size = width * 3
    if len(pixels) < row_size * height:
        raise ValueError("truncated PPM pixel data")

    raw = b"".join(
        b"\x00" + pixels[y * row_size:(y + 1) * row_size] for y in range(height)
    )
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        _PNG_SIGNATURE
        + _png_chunk(b"IHDR", ihdr)
        + _png_chunk(b"IDAT", zlib.compress(raw, 6))
        + _png_chunk(b"IEND", b"")
    )


def pgm_to_png(pgm: bytes) -> bytes:
    """Convert a P5 PGM (8-bit grayscale) blob to RGB PNG bytes.

    Grayscale devices (depth=8 framebuffer) dump PGM; the result is
    expanded to the same RGB PNG shape the report pipeline expects.
    """
    if not pgm.startswith(b"P5"):
        raise ValueError("not a P5 PGM")
    pos = 2
    tokens: list[int] = []
    while len(tokens) < 3:
        while pos < len(pgm) and pgm[pos:pos + 1].isspace():
            pos += 1
        start = pos
        while pos < len(pgm) and not pgm[pos:pos + 1].isspace():
            pos += 1
        if pos == start:
            raise ValueError("malformed PGM header")
        tokens.append(int(pgm[start:pos]))
    width, height, _maxval = tokens[:3]
    pos += 1  # single whitespace between maxval and the pixel data
    pixels = pgm[pos:]
    row_size = width
    if len(pixels) < row_size * height:
        raise ValueError("truncated PGM pixel data")

    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter: none
        for x in range(width):
            g = pixels[y * row_size + x]
            raw += bytes((g, g, g))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        _PNG_SIGNATURE
        + _png_chunk(b"IHDR", ihdr)
        + _png_chunk(b"IDAT", zlib.compress(bytes(raw), 6))
        + _png_chunk(b"IEND", b"")
    )


class SnapshotRecorder:
    """Per-test screenshot collector bound to an emulator."""

    def __init__(self, root: Path) -> None:
        self._root = root
        self._dir: Path | None = None
        self._seq = 0
        self._index: list[str] = []
        self._t0 = 0.0
        self._entries: list[dict] = []

    def begin(self, test_name: str) -> None:
        """Start a fresh capture sequence for one test."""
        global ACTIVE
        safe = "".join(c if c.isalnum() or c in "._-" else "_" for c in test_name)
        self._dir = self._root / safe
        self._dir.mkdir(parents=True, exist_ok=True)
        self._seq = 0
        self._index = []
        self._t0 = time.monotonic()
        self._entries = []
        ACTIVE = self

    @property
    def active(self) -> bool:
        return self._dir is not None

    def peek_name(self, label: str) -> str:
        """Next filename without consuming the sequence slot."""
        safe = "".join(
            c if c.isalnum() or c in "._-" else "_" for c in label
        ) or "frame"
        return f"{self._seq:03d}-{safe}"

    def finish_capture(self, name: str, label: str, raw: Path) -> Path | None:
        """Convert the probe's PPM/PGM dump at *raw* into the named PNG.

        Never raises: capture problems are recorded in the index and
        the suite continues.
        """
        if self._dir is None:
            return None
        self._seq += 1
        try:
            if not raw.exists():
                raise FileNotFoundError(f"probe output missing: {raw}")
            data = raw.read_bytes()
            if data.startswith(b"P5"):
                png = pgm_to_png(data)
            else:
                png = ppm_to_png(data)
            out = self._dir / f"{name}.png"
            out.write_bytes(png)
            raw.unlink(missing_ok=True)
            self._index.append(f"{name}.png  {label}")
            self._entries.append(
                {
                    "label": label,
                    "png": f"{name}.png",
                    "ms": int((time.monotonic() - self._t0) * 1000),
                }
            )
            return out
        except Exception as exc:  # noqa: BLE001
            self._index.append(f"{name}.png  {label}  (capture failed: {exc})")
            return None

    def entries(self) -> list[dict]:
        """Copy of the per-test capture list (label, png, elapsed ms)."""
        return list(self._entries)

    def write_index(self) -> None:
        """Write index.txt listing the captured frames in order."""
        if self._dir is not None and self._index:
            (self._dir / "index.txt").write_text(
                "\n".join(self._index) + "\n", encoding="utf-8"
            )
