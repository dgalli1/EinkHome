"""
storage/cover_proc.py — turn a raw upstream cover into what the device needs.

The e-ink bookshelf shows the real cover, fetched as a small JPEG.  The
JPEG is derived here from the provider's raw (often multi-megabyte JPEG)
cover bytes, exactly once, then cached on disk.  Doing the heavy Pillow
work server-side keeps the ARM guest dumb: it just stretches a small
JPEG.  PNG and JPEG are the only formats the device's libinkview can
decode, and JPEG wins on both size (~5x smaller for photographs) and
decode speed, so the processed cover is server-side JPEG.

(BlurHash placeholders were removed entirely — the device is too slow to
usefully display them.)

`process()` is pure and synchronous; the warm-up loop in the server calls
it off the request path so a sync never blocks on 90+ image decodes.
"""

from __future__ import annotations

import io
import threading
from typing import Optional

# Cover JPEG is sized for the device's portrait cover box (~2:3).  240x360
# is comfortably above the on-screen box (~220x330 on the 6" panel) so the
# guest's stretch is a mild downscale, never an upscale, and the file
# stays tiny (a few KB of JPEG).
COVER_W = 240
COVER_H = 360

# Bound concurrent heavy Pillow decodes.  A multi-megapixel source
# transiently allocates ~3x its pixel count while decoding (a 20MP source
# -> ~60MB); several warm-up workers could otherwise do this at once and
# spike memory.  Only the decode+resize is gated, so callers still do the
# (cheap, lazy) file I/O in parallel.
DECODE_SEMAPHORE = threading.Semaphore(2)


def process(raw: bytes) -> Optional[bytes]:
    """Decode `raw` cover bytes -> resized_png_bytes.

    Returns None if the bytes cannot be decoded as an image (corrupt or
    unsupported format); the caller then falls back to a 1x1 placeholder.
    """
    try:
        from PIL import Image  # local import: Pillow is an optional dep
    except ImportError:
        return None

    # Backstop against decompression bombs: raising the ceiling to our
    # explicit limit makes PIL warn instead of aborting below it.  The
    # explicit check below rejects oversized images before any pixel
    # work (no full decode of a 100MP source).  20MP is far above any
    # legit cover (typically < 2MP) yet bounds the transient decode
    # allocation to ~60MB.
    Image.MAX_IMAGE_PIXELS = 20_000_000

    try:
        img = Image.open(io.BytesIO(raw))
        if img.width * img.height > 20_000_000:
            return None
    except Exception:
        return None

    # Heavy decode + resize + PNG encode, gated by a semaphore so
    # concurrent warm-up workers don't spike memory.  The lazy Image.open
    # and the cheap dims check above stay outside the gate.
    with DECODE_SEMAPHORE:
        try:
            img.load()
            rgb = img.convert("RGB")

            # Resized cover JPEG.  contain keeps the aspect ratio (letterbox)
            # so a tall cover is never squashed; the letterbox bars are filled
            # with the cover's own average colour so they read as part of the
            # image on a 1-bit panel rather than as a hard white frame.  JPEG
            # is ~5x smaller and faster to decode on the ARM server-side cache
            # and device than PNG for these photographic covers.  The device
            # decodes via libinkview's LoadJPEGToFormat (PNG and JPEG are the
            # only codecs libinkview ships — never emit WebP/AV1 here).
            cover = _fit(rgb, COVER_W, COVER_H)
            buf = io.BytesIO()
            cover.save(buf, format="JPEG", quality=85, optimize=True)
            return buf.getvalue()
        except Exception:
            return None


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
