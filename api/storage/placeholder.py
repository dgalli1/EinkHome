"""
storage/placeholder.py — the shared 1x1 grey RGB placeholder PNG.

The bytes form a *valid* PNG (correct zlib LEN/NLEN): Pillow and the
firmware's LoadPNGStretch both reject a corrupt stream.  Used as the
fallback cover for books without one and as the mock provider's
"cover".  Defined once here so the API server, the mock provider and
the cover cache all reference the same bytes.
"""

from __future__ import annotations

# 1x1 grey RGB PNG: PNG signature + IHDR/IDAT/IEND.
PLACEHOLDER_PNG = bytes.fromhex(
    "89504e470d0a1a0a"  # PNG signature
    "0000000d4948445200000001000000010802000000907753"
    "de0000000c49444154789c636868680000030401814bd3d2"
    "100000000049454e44ae426082"
)
