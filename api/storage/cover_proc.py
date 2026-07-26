"""
storage/cover_proc.py — turn a raw upstream cover into what the device needs.

The e-ink bookshelf shows two things per tile:

  1. a *blurhash* placeholder, drawn instantly from a short ASCII string
     carried in the sync metadata (no network round trip), and
  2. the real cover, fetched as a small PNG a moment later.

Both are derived here from the provider's raw (often multi-megabyte JPEG)
cover bytes, exactly once, then cached on disk.  Doing the heavy Pillow
work server-side keeps the ARM guest dumb: it just decodes a string and
stretches a small PNG.

`process()` is pure and synchronous; the warm-up loop in the server calls
it off the request path so a sync never blocks on 90+ image decodes.
"""

from __future__ import annotations

import io
from typing import Optional

from storage import blurhash as _bh

# Cover PNG is sized for the device's portrait cover box (~2:3).  240x360
# is comfortably above the on-screen box (~220x330 on the 6" panel) so the
# guest's stretch is a mild downscale, never an upscale, and the file
# stays tiny (a few KB of PNG).
COVER_W = 240
COVER_H = 360

# Resolution the blurhash DCT is sampled at.  32x32 is plenty to capture a
# cover's colour layout and keeps the encode a fraction of a millisecond.
_HASH_W = 32
_HASH_H = 32

# DCT components -> placeholder sharpness / string length.  4x3 = 28 chars.
_COMP_X = 4
_COMP_Y = 3


def process(raw: bytes) -> Optional[tuple[bytes, str]]:
    """Decode `raw` cover bytes -> (resized_png_bytes, blurhash).

    Returns None if the bytes cannot be decoded as an image (corrupt or
    unsupported format); the caller then falls back to a 1x1 placeholder
    and an empty blurhash.
    """
    try:
        from PIL import Image  # local import: Pillow is an optional dep
    except ImportError:
        return None

    try:
        img = Image.open(io.BytesIO(raw))
        img.load()
    except Exception:
        return None

    rgb = img.convert("RGB")

    # BlurHash from a small downscale (fast, and a soft source suits the
    # low-frequency DCT anyway).
    try:
        small = rgb.resize((_HASH_W, _HASH_H), Image.Resampling.BILINEAR)
        pixels = list(small.getdata())
        bhash = _bh.encode(pixels, _HASH_W, _HASH_H, _COMP_X, _COMP_Y)
    except Exception:
        bhash = ""

    # Resized cover PNG.  contain=True keeps the aspect ratio (letterbox)
    # so a tall cover is never squashed; the letterbox bars are filled with
    # the cover's own average colour so they read as part of the image on a
    # 1-bit panel rather than as a hard white frame.
    try:
        cover = _fit(rgb, COVER_W, COVER_H)
        buf = io.BytesIO()
        cover.save(buf, format="PNG", optimize=True)
        png = buf.getvalue()
    except Exception:
        return None

    return png, bhash


def _fit(img, target_w: int, target_h: int):
    """Letterbox `img` into target_w x target_h, bars = mean colour."""
    from PIL import Image

    src_w, src_h = img.size
    scale = min(target_w / src_w, target_h / src_h)
    new_w = max(1, int(round(src_w * scale)))
    new_h = max(1, int(round(src_h * scale)))
    resized = img.resize((new_w, new_h), Image.Resampling.LANCZOS)

    # Average colour for the padding bars (quantised down to one value).
    thumb = img.resize((1, 1), Image.Resampling.BILINEAR).getpixel((0, 0))
    bg = Image.new("RGB", (target_w, target_h), thumb)
    bg.paste(resized, ((target_w - new_w) // 2, (target_h - new_h) // 2))
    return bg
