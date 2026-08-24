"""Unit tests for the Content-Disposition filename parser.

The header comes from the upstream Kavita server — potentially
malicious — so the parser must always return a bare base name: no
traversal segments, no absolute paths, no backslash escapes to
Windows-style joiners.
"""

import os
import sys

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from providers.kavita import _filename_from_content_disposition as parse  # noqa: E402


def test_plain_filename():
    assert parse("attachment; filename=\"book.epub\"") == "book.epub"


def test_filename_star_utf8_form():
    assert (
        parse("attachment; filename*=UTF-8''%D1%80%D1%83.doc")
        == "\u0440\u0443.doc"  # cyrillic letters, spelled as escapes
    )


def test_traversal_segments_are_stripped():
    assert parse("filename=\"../../../etc/cron.d/pwn\"") == "pwn"
    assert parse("filename=..%2f..%2fetc%2fpasswd") == "passwd"
    assert parse("filename*=UTF-8''..%2F..%2Fevil.fb2") == "evil.fb2"


def test_absolute_and_backslash_paths_are_reduced():
    assert parse("filename=\"/etc/shadow\"") == "shadow"
    assert parse("filename=\"C:\\\\Users\\\\x\\\\a.pdf\"") == "a.pdf"


def test_empty_or_degenerate_values_return_none():
    assert parse("") is None
    assert parse("filename=\"\"") is None
    assert parse("filename=../..") is None  # reduces to nothing
    assert parse("inline") is None
