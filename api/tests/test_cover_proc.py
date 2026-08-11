"""Unit tests for api/storage/cover_proc.process.

Pure byte-in/byte-out image processing: corrupt input -> None,
oversized input (decompression-bomb backstop) -> None, valid input ->
a 240x360 letterboxed PNG, and a graceful None when Pillow is absent.
Hermetic — no network; the oversized image is built at 5500x5500 in
1-bit mode so the test stays allocation-safe.
"""

from __future__ import annotations

import builtins
import io
import os
import sys

import pytest

pytest.importorskip("PIL")  # api/tests assume Pillow is installed

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from storage import cover_proc  # noqa: E402


def test_corrupt_bytes_return_none():
    assert cover_proc.process(b"this is definitely not an image") is None
    assert cover_proc.process(b"") is None
    assert cover_proc.process(b"\x89PNG\r\n\x1a\n" + b"\x00" * 64) is None


def test_oversized_image_rejected():
    """5500x5500 = 30.25MP exceeds the 30MP cap; process must return
    None before any full decode (no decompression bomb)."""
    from PIL import Image

    # 1-bit mode keeps the construction at ~30MB, acceptable for a test.
    img = Image.new("1", (5500, 5500), 0)
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    assert cover_proc.process(buf.getvalue()) is None


def test_valid_image_resized_to_cover_dims():
    from PIL import Image

    img = Image.new("RGB", (100, 200), (200, 30, 30))
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    out = cover_proc.process(buf.getvalue())
    assert out is not None
    assert out.startswith(b"\x89PNG\r\n\x1a\n")
    with Image.open(io.BytesIO(out)) as check:
        assert check.size == (240, 360)


def test_wide_image_letterboxed():
    """A 300x100 (wide) cover scales to 240x80 and is centred on the
    240x360 canvas; the top/bottom bars are the image's mean colour."""
    from PIL import Image

    img = Image.new("RGB", (300, 100))
    half = img.width // 2
    for x in range(half):
        for y in range(img.height):
            img.putpixel((x, y), (255, 0, 0))
    for x in range(half, img.width):
        for y in range(img.height):
            img.putpixel((x, y), (0, 0, 255))
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    out = cover_proc.process(buf.getvalue())
    assert out is not None
    with Image.open(io.BytesIO(out)) as check:
        assert check.size == (240, 360)
        rgb = check.convert("RGB")
        # Bars are uniform and identical top/bottom…
        assert rgb.getpixel((0, 0)) == rgb.getpixel((239, 0))
        assert rgb.getpixel((0, 0)) == rgb.getpixel((239, 359))
        # …and differ from the centred band, which holds the scaled
        # source: red on the left, blue on the right.
        assert rgb.getpixel((5, 180)) == (255, 0, 0)
        assert rgb.getpixel((234, 180)) == (0, 0, 255)
        assert rgb.getpixel((5, 180)) != rgb.getpixel((0, 0))


def test_process_without_pillow_returns_none(monkeypatch):
    """Pillow is an optional dependency; without it, process returns
    None instead of raising."""
    real_import = builtins.__import__

    def _block_pil(name, *args, **kwargs):
        if name == "PIL" or name.startswith("PIL."):
            raise ImportError("Pillow disabled for this test")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", _block_pil)
    assert cover_proc.process(b"\x89PNG\r\n\x1a\nplaceholder") is None


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
