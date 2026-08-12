"""
api/storage/suggest.py — server-side suggestion term generation.

The bookshelf search completes terms while the user types in the
system keyboard, offline, against a local SQLite index.  The device
never re-implements Unicode folding: this module computes every
matchable term at delta-serve time from the book's title, author
names and series title, and the wire carries the finished term list.

Term scheme (user-specified, fixed): for each field, every individual
word AND every word-aligned suffix phrase (words i..end).  So the
title "Harry Potter Order of the Phoenix" yields::

    harry, potter, order, of, the, phoenix,
    harry potter order of the phoenix, potter order of the phoenix,
    order of the phoenix, of the phoenix, the phoenix

Typing "harry po" matches the phrase term by plain prefix.  There is
no stopword removal: a tapped suggestion is committed verbatim as a
substring query against the raw title/author/series text, so terms
must contain every word that query needs.  Terms longer than the
device's query buffer (80 chars) are dropped server-side; the list is
capped at 96 terms per book.
"""

from __future__ import annotations

import re
import unicodedata
from collections.abc import Sequence

# Device constants this module must stay in sync with (app/bookshelf.h):
#   MAX_QUERY_LEN  80  — a tapped term is copied into the query buffer
#   SUGGEST_MAX_TERMS 96 — an upper bound the device accepts; the wire
#                          and the device-side term index are far
#                          cheaper with a tighter cap (a 100k first
#                          sync ships ~16 terms/book on average; long
#                          titles hit the cap and dominate the payload
#                          and the device's per-round inserts).
_TERM_MAX = 79  # 80 - NUL
_TERM_CAP = 24

_NON_ALNUM = re.compile(r"[^0-9a-z]+")


def _fold(text: str) -> str:
    """NFKD + casefold + strip combining marks.  The C app only
    ASCII-lowercases typed queries, so folded ASCII terms match the
    dominant input; non-ASCII input may miss folded terms (accepted)."""
    folded = unicodedata.normalize("NFKD", text).casefold()
    return "".join(ch for ch in folded if unicodedata.combining(ch) == 0)


def _field_terms(text: str) -> list[str]:
    """Terms for one field: words then word-aligned suffix phrases.

    Tokens are kept only when len >= 2 ("J.K." drops j/k; "of" stays).
    """
    tokens = [t for t in _NON_ALNUM.split(_fold(text)) if len(t) >= 2]
    terms = list(tokens)
    for i in range(len(tokens)):
        terms.append(" ".join(tokens[i:]))
    return terms


def suggest_terms(title: str, authors: Sequence[str], series: str | None) -> list[str]:
    """Deduplicated, capped suggestion terms for one book.

    Fields contribute in order: title, each author, series.  `None` or
    empty fields contribute nothing.  Output is deterministic (input
    order, first occurrence wins) and the device relies only on term
    text, never on ordering semantics beyond "first is not special".
    """
    out: list[str] = []
    seen: set[str] = set()
    for field in (title, *authors, series):
        if not field:
            continue
        for term in _field_terms(field):
            if len(term) > _TERM_MAX:
                continue
            if term in seen:
                continue
            seen.add(term)
            out.append(term)
            if len(out) >= _TERM_CAP:
                return out
    return out


def search_text(title: str, authors: Sequence[str], series: str | None) -> str:
    """Folded, space-joined search blob for one book.

    The device answers searches with ``LIKE '%q%'`` against the raw
    title/author/series text.  Suggestions are folded server-side, so
    a folded term like "songgong" (from "sŏnggong") never matches the
    raw text — tapping such a suggestion would find nothing.  This
    blob is the folded search target the device matches against; the
    wire carries it per added book (delta ``searchText`` key).
    """
    parts = [s for s in (title, *authors, series) if s]
    return " ".join(_fold(s) for s in parts)
