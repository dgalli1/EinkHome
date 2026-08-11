"""Unit tests for api/storage/suggest.suggest_terms.

Run with:
    python -m pytest api/tests/test_suggest.py -v
"""

# pylint: disable=missing-function-docstring
import os
import sys

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, REPO_ROOT)
sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

from storage.suggest import search_text, suggest_terms  # noqa: E402


def test_words_and_suffix_phrases():
    terms = suggest_terms(
        "Harry Potter Order of the Phoenix", ["Joanne Rowling"], None
    )
    for want in (
        "harry",
        "potter",
        "order",
        "of",
        "the",
        "phoenix",
        "rowling",
        "joanne",
        "harry potter order of the phoenix",
        "potter order of the phoenix",
        "order of the phoenix",
        "of the phoenix",
        "the phoenix",
        "joanne rowling",
    ):
        assert want in terms, f"missing term {want!r} in {terms}"


def test_no_stopword_removal():
    # No stopword *list* — "the"/"of"/"and" all stay as terms.  The
    # len>=2 token rule still drops single chars ("a").
    terms = suggest_terms("The Of A And", [], None)
    assert "the" in terms and "of" in terms and "and" in terms
    assert "a" not in terms
    assert "the of a and" not in terms  # "a" never enters a phrase
    assert "of and" in terms


def test_no_one_char_tokens_no_mid_phrases():
    terms = suggest_terms("Harry Potter Order of the Phoenix", ["J. K. Rowling"], None)
    for bad in ("j", "k", "potter order", "of the"):
        assert bad not in terms, f"unexpected term {bad!r} in {terms}"
    assert "rowling" in terms  # the word itself is a term


def test_unicode_folding():
    terms = suggest_terms("Émile Zola — Au Bonheur", ["André Gide"], None)
    for want in (
        "emile",
        "zola",
        "au",
        "bonheur",
        "andre",
        "gide",
        "emile zola au bonheur",
        "zola au bonheur",
        "andre gide",
    ):
        assert want in terms, f"missing folded term {want!r} in {terms}"


def test_series_field_included():
    terms = suggest_terms("Book 1", [], "The Great Series")
    assert "the great series" in terms and "series" in terms


def test_empty_and_none_inputs():
    assert suggest_terms("", [], None) == []
    assert suggest_terms("   ", [], "") == []
    assert suggest_terms(None, [], None) == []  # type: ignore[arg-type]


def test_dedupe_preserves_first_occurrence():
    # tokens: the, the, book → words the, the, book + phrases
    # "the the book", "the book", "book"; first occurrence wins.
    terms = suggest_terms("The The Book", [], None)
    assert terms == ["the", "book", "the the book", "the book"]


def test_length_cap_drops_only_long_terms():
    word = "abcdefgh"
    title = " ".join([word] * 10)  # full phrase is 89 chars > 79
    terms = suggest_terms(title, [], None)
    assert all(len(t) <= 79 for t in terms), [t for t in terms if len(t) > 79]
    # Words and short suffix phrases survive; the 80-char phrase is dropped.
    assert word in terms
    assert len([t for t in terms if t == word]) == 1
    assert " ".join([word] * 9) not in terms  # 80 chars — dropped


def test_term_cap_twenty_four():
    # 3 fields x 20 words = 120 terms (words + suffix phrases, all
    # <= 79 chars: 20*3 + 19 = 79 exactly for the longest phrase).
    # Dedupe collapses each field's last-word phrase into its word
    # (3 fields x 39 = 117 distinct), then the cap keeps 24: title
    # 39 -> capped mid-phrases, then author words a00..a04.
    title = " ".join(f"t{i:02d}" for i in range(20))
    author = " ".join(f"a{i:02d}" for i in range(20))
    series = " ".join(f"s{i:02d}" for i in range(20))
    terms = suggest_terms(title, [author], series)
    assert len(terms) == 24
    assert terms[0] == "t00"
    assert terms[19] == "t19"
    assert terms[20] == " ".join(f"t{i:02d}" for i in range(20))
    assert terms[23] == " ".join(f"t{i:02d}" for i in range(3, 20))  # 4th title phrase
    assert "a00" not in terms  # the cap never reached the author


def test_search_text_folds_diacritics():
    # A "songgong" suggestion from "sŏnggong" must match the search
    # blob: this is the "suggestion found, no results" bug.
    blob = search_text("Han-Il pijŭnisŭ sŏnggong pigyŏl", ["Sŭng-il Chŏng"], None)
    assert "songgong" in blob
    assert "sŭng-il chŏng" not in blob
    assert "sung-il chong" in blob
    assert blob == "han-il pijunisu songgong pigyol sung-il chong"


def test_search_text_fields_and_empty():
    assert search_text("Title", [], None) == "title"
    assert search_text("Title", ["Author One"], "Series X") == (
        "title author one series x"
    )
    assert search_text("", [], None) == ""
    assert search_text(None, [], None) == ""  # type: ignore[arg-type]
