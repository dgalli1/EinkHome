"""
storage/blurhash.py — pure-Python BlurHash *encoder* (no third-party deps).

BlurHash (https://blurha.sh) packs a tiny DCT of an image into a short
ASCII string.  The device decodes it into a soft, low-res colour field
and shows that *instantly* as a placeholder while the real cover PNG
streams in over the network — the e-ink equivalent of a skeleton screen,
but coloured like the actual cover.

We only need the *encoder* here (the device ships its own C decoder).
The algorithm is the public reference spec; this is a straight port so
the strings interoperate with any conforming decoder.

Kept dependency-free on purpose: the API server already pulls in Pillow
for resizing, but BlurHash itself is a few dozen lines of math and we do
not want a second PyPI dependency just for it.
"""

from __future__ import annotations

import math
from typing import Sequence

# The 83-character alphabet BlurHash encodes into (order matters).
_BASE83 = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~"


def _base83_encode(value: int, length: int) -> str:
    out = []
    for i in range(1, length + 1):
        digit = (value // (83 ** (length - i))) % 83
        out.append(_BASE83[digit])
    return "".join(out)


def _srgb_to_linear(value: int) -> float:
    v = value / 255.0
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def _linear_to_srgb(value: float) -> int:
    v = max(0.0, min(1.0, value))
    s = 12.92 * v if v <= 0.0031308 else 1.055 * (v ** (1.0 / 2.4)) - 0.055
    return int(round(s * 255.0))


def _sign_pow(value: float, exp: float) -> float:
    return (1.0 if value >= 0 else -1.0) * (abs(value) ** exp)


def _encode_dc(r: float, g: float, b: float) -> int:
    ir = _linear_to_srgb(r)
    ig = _linear_to_srgb(g)
    ib = _linear_to_srgb(b)
    return (ir << 16) + (ig << 8) + ib


def _encode_ac(value: float, maximum_value: float) -> int:
    q = max(0, min(18, int(math.floor(_sign_pow(value / maximum_value, 0.5) * 9 + 9.5))))
    return q


def encode(pixels: Sequence[Sequence[int]], width: int, height: int,
           comp_x: int = 4, comp_y: int = 3) -> str:
    """Encode an RGB pixel buffer to a BlurHash string.

    `pixels` is row-major: pixels[y*width + x] == (r, g, b) with each
    channel 0..255.  `comp_x`/`comp_y` are the number of DCT components
    in each axis (more = sharper placeholder, longer string).  4x3 is the
    usual sweet spot: a 28-char string that still captures the cover's
    colour layout.
    """
    if width <= 0 or height <= 0:
        raise ValueError("width/height must be positive")
    if not (1 <= comp_x <= 9 and 1 <= comp_y <= 9):
        raise ValueError("components must be in 1..9")

    factors: list[tuple[float, float, float]] = []
    for j in range(comp_y):
        for i in range(comp_x):
            norm = 1.0 if (i == 0 and j == 0) else 2.0
            r = g = b = 0.0
            for y in range(height):
                cos_y = math.cos(math.pi * j * y / height)
                for x in range(width):
                    basis = norm * math.cos(math.pi * i * x / width) * cos_y
                    px = pixels[y * width + x]
                    r += basis * _srgb_to_linear(px[0])
                    g += basis * _srgb_to_linear(px[1])
                    b += basis * _srgb_to_linear(px[2])
            denom = float(width * height)
            factors.append((r / denom, g / denom, b / denom))

    dc = factors[0]
    ac = factors[1:]

    if ac:
        max_ac = max(max(abs(c) for c in f) for f in ac)
    else:
        max_ac = 0.0
    quant_max_ac = 0 if max_ac == 0.0 else max(0, min(82, int(math.floor(max_ac * 166 - 0.5))))
    # The real maximum the decoder will reconstruct with (matches spec).
    real_max_ac = (quant_max_ac + 1) / 166.0

    out = _base83_encode((comp_x - 1) + (comp_y - 1) * 9, 1)
    out += _base83_encode(quant_max_ac, 1)
    out += _base83_encode(_encode_dc(*dc), 4)
    for f in ac:
        qr = _encode_ac(f[0], real_max_ac)
        qg = _encode_ac(f[1], real_max_ac)
        qb = _encode_ac(f[2], real_max_ac)
        out += _base83_encode(qr * 19 * 19 + qg * 19 + qb, 2)
    return out


# ---------------------------------------------------------------------------
# Decoder — NOT used in production (the device decodes in C).  Shipped only
# so the encoder can be round-trip tested without an external reference.
# ---------------------------------------------------------------------------


def _base83_decode(s: str) -> int:
    v = 0
    for ch in s:
        v = v * 83 + _BASE83.index(ch)
    return v


def _decode_dc(value: int) -> tuple[float, float, float]:
    r = _srgb_to_linear((value >> 16) & 255)
    g = _srgb_to_linear((value >> 8) & 255)
    b = _srgb_to_linear(value & 255)
    return r, g, b


def _decode_ac(value: int, maximum_value: float) -> tuple[float, float, float]:
    qr = value // (19 * 19)
    qg = (value // 19) % 19
    qb = value % 19
    return (
        _sign_pow((qr - 9.0) / 9.0, 2.0) * maximum_value,
        _sign_pow((qg - 9.0) / 9.0, 2.0) * maximum_value,
        _sign_pow((qb - 9.0) / 9.0, 2.0) * maximum_value,
    )


def decode(blurhash: str, width: int, height: int, punch: float = 1.0) -> list[tuple[int, int, int]]:
    """Decode a BlurHash string to a row-major RGB pixel list (test helper)."""
    size_flag = _base83_decode(blurhash[0])
    comp_x = (size_flag % 9) + 1
    comp_y = (size_flag // 9) + 1
    quant_max_ac = _base83_decode(blurhash[1])
    real_max_ac = (quant_max_ac + 1) / 166.0 * punch

    factors = [_decode_dc(_base83_decode(blurhash[2:6]))]
    pos = 6
    for _ in range(comp_x * comp_y - 1):
        factors.append(_decode_ac(_base83_decode(blurhash[pos:pos + 2]), real_max_ac))
        pos += 2

    pixels: list[tuple[int, int, int]] = []
    for y in range(height):
        for x in range(width):
            r = g = b = 0.0
            for j in range(comp_y):
                for i in range(comp_x):
                    basis = math.cos(math.pi * i * x / width) * math.cos(math.pi * j * y / height)
                    f = factors[i + j * comp_x]
                    r += f[0] * basis
                    g += f[1] * basis
                    b += f[2] * basis
            pixels.append((_linear_to_srgb(r), _linear_to_srgb(g), _linear_to_srgb(b)))
    return pixels
