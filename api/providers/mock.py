"""
providers/mock.py — a stand-in provider for offline development.

Reads the same `U633_6.8.2817/.live/mnt/ext1/books` directory the
firmware stages user books into and exposes every file as a single
fake "book". Useful for:
  - running the API server without a real Kavita instance
  - running the in-emulator app without internet
  - CI / smoke tests

Synthetic scale mode: ``count`` in the provider config generates that
many books (with stable, derived metadata and no files on disk) on top
of whatever the books dir holds.  Used to exercise the device app and
the delta protocol at 100k entries without materialising a library.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
from collections.abc import Iterator
from contextlib import suppress
from typing import Any

from storage.ledger import fingerprint_blob
from storage.placeholder import PLACEHOLDER_PNG

from .base import (
    AuthorInfo,
    BookMeta,
    LibraryInfo,
    Provider,
    SeriesInfo,
)

# Deterministic vocabulary for synthetic titles/authors.  Indexing by
# (i mod len) keeps every synthetic book reproducible across runs.
_SYN_SERIES = ("Orbit", "Quartz", "Lumen", "Fathom", "Cinder", "Vale")
_SYN_AUTHORS = (
    "Ada Quill",
    "Bram Hallow",
    "Cora Voss",
    "Dane Pryce",
    "Edda Marn",
    "Finn Ocker",
)
_SYN_FMTS = ("epub", "epub", "epub", "pdf", "fb2")
_SYN_GENRES = ("Fiction", "Fantasy", "Sci-Fi", "Mystery", "History", "Poetry")
# Fixed epoch for synthetic timestamps so the whole library has a stable
# added_at ordering (i-based) independent of the server's wall clock.
_SYN_EPOCH = 1_700_000_000.0
# TTL (seconds) for the cached books-dir scan used by health() and
# list_libraries() — long enough to avoid a full os.listdir + per-file
# os.stat on every request, short enough that dir changes surface fast.
_SCAN_TTL = 1.5


# The only record fields the mock provider reads (see _corpus_meta):
# id (required), title, authors, series, added_at, ol_key (optional).
# The compact fingerprint index below stores (id, fp, added_at) only —
# the fp is the sha1 the ledger uses for change detection, so catalogue
# walks compare against the ledger without re-parsing the file.


def _corpus_fp(i: int, rec: dict[str, Any]) -> tuple[str, str, str]:
    """(id, fp, added_at) for corpus record #i.  Derives EXACTLY the
    fields _corpus_meta derives (title fallback, authors list, series,
    series_id = ol_ser_+sha1 prefix, added_at fallback, format, size,
    file_name = "") so the fingerprint always matches
    ledger.fingerprint(_corpus_meta(i, rec)).  Keep the two in sync."""
    book_id = rec["id"]
    fmt = _SYN_FMTS[i % len(_SYN_FMTS)]
    series_name: str | None = rec.get("series") or None
    series_id: str | None = None
    if series_name:
        series_id = "ol_ser_" + hashlib.sha1(series_name.encode()).hexdigest()[:12]
    ts = rec.get("added_at") or _iso(_SYN_EPOCH + i)
    return (
        book_id,
        fingerprint_blob(
            rec.get("title") or f"Untitled {i}",
            list(rec.get("authors") or []),
            series_name,
            series_id,
            None,  # corpus metas carry no series_index
            fmt,
            None,  # corpus metas carry no file_name
            10_000 + (i % 900_000),
            ts,
        ),
        ts,
    )


class _Corpus:
    """Lazy reader over a JSONL corpus: line offsets in RAM, records
    parsed on demand.  A compact fingerprint index holds (id, fp,
    added_at) as three parallel string lists so catalogue walks (and
    the ledger's fingerprint walk) never re-json.loads the file.  An
    empty path yields an empty corpus (graceful when the configured
    file is missing on a fresh clone)."""

    __slots__ = ("_fps", "_fps_key", "_len", "_offsets", "_path")

    def __init__(self, path: str) -> None:
        self._path = path
        offs: list[int] = []
        if path:
            try:
                with open(path, encoding="utf-8") as f:
                    while True:
                        offs.append(f.tell())
                        if not f.readline():
                            offs.pop()
                            break
            except OSError as exc:
                sys.stderr.write(f"mock: cannot load corpus {path}: {exc}\n")
                offs = []
        self._offsets = offs
        self._len = len(offs)
        # Compact fingerprint index: three parallel lists (ids, fps,
        # added_ats — all strings), keyed by (path, mtime-ns, size) so
        # an edited corpus file invalidates it.  None until first
        # build; a stale or unstat-able file drops it back to None and
        # the streaming fallback applies.
        self._fps: tuple[list[str], list[str], list[str]] | None = None
        self._fps_key: tuple[str, int, int] | None = None

    def _fps_valid(self) -> bool:
        """True when the fingerprint index exists and the corpus file
        is unchanged since it was built (mtime-ns + size key).  An
        OSError while statting (file gone, …) drops the index."""
        if self._fps is None:
            return False
        try:
            st = os.stat(self._path)
        except OSError:
            self._fps = None
            self._fps_key = None
            return False
        if (self._path, st.st_mtime_ns, st.st_size) != self._fps_key:
            self._fps = None
            self._fps_key = None
            return False
        return True

    def build_fps(self) -> None:
        """One streaming pass over the JSONL building (ids, fps,
        added_ats).  Only one parsed dict is ever in flight: each
        record's derived strings are appended and the dict dropped, so
        the transient peak stays ~1 dict and the retained index is
        lists of strings, never a list of dicts."""
        if not self._path:
            return
        try:
            st = os.stat(self._path)
            ids: list[str] = []
            fps: list[str] = []
            added_ats: list[str] = []
            with open(self._path, encoding="utf-8") as f:
                i = 0
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        rec = json.loads(line)
                    except ValueError:
                        continue
                    bid, fp, ts = _corpus_fp(i, rec)
                    ids.append(bid)
                    fps.append(fp)
                    added_ats.append(ts)
                    i += 1
        except OSError:
            return  # keep the index unset; the streaming fallback applies
        self._fps = (ids, fps, added_ats)
        self._fps_key = (self._path, st.st_mtime_ns, st.st_size)

    def fps(self) -> tuple[list[str], list[str], list[str]] | None:
        """Guarantee the fingerprint index (building it if stale) and
        return (ids, fps, added_ats), or None when the corpus file
        cannot be read — the caller then falls back to a streaming
        pass."""
        if not self._fps_valid():
            self.build_fps()
        return self._fps

    def __len__(self) -> int:
        return self._len

    def __bool__(self) -> bool:
        return self._len > 0

    def __iter__(self) -> Iterator[dict[str, Any]]:
        if not self._path:
            return
        with open(self._path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    yield json.loads(line)
                except ValueError:
                    continue

    def __getitem__(self, i: int) -> dict[str, Any]:
        """Random access: open + seek per read so concurrent request
        threads never share a seek cursor."""
        with open(self._path, encoding="utf-8") as f:
            f.seek(self._offsets[i])
            line = f.readline()
        try:
            return json.loads(line)
        except ValueError:
            return {}


class MockProvider(Provider):
    name = "mock"

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        # Default to the pbemu books dir
        self.books_dir = cfg.get(
            "books_dir",
            os.path.join(
                os.path.dirname(os.path.dirname(os.path.dirname(__file__))),
                "..",
                "U633_6.8.2817",
                ".live",
                "mnt",
                "ext1",
                "books",
            ),
        )
        self.library_name = cfg.get("library_name", "pbemu demo library")
        # Number of synthetic books layered over the books-dir scan
        # (0 = off).  Synthetic ids/metadata are fully deterministic so
        # a given config always describes the same library.
        try:
            self.synthetic_count = int(cfg.get("count") or 0)
        except (TypeError, ValueError):
            self.synthetic_count = 0
        # Books per synthetic series.  Every `synthetic_series_size`-th
        # book joins a series (so series collapse has something to chew
        # on); the remainder stay standalone.
        try:
            self.synthetic_series_size = int(cfg.get("series_size") or 5)
        except (TypeError, ValueError):
            self.synthetic_series_size = 5
        self.synthetic_series_size = max(2, self.synthetic_series_size)
        # Realistic corpus: a JSONL file (see scripts/build_ol_corpus.py)
        # of {id, title, authors[], series?, added_at?} records served in
        # place of the synthetic layer.  Selected via the `corpus` config
        # key or the PBEMU_MOCK_CORPUS env var; when the corpus is
        # shorter than `count`, the remainder is padded with synthetic
        # books so the advertised count always holds.
        corpus_path = os.environ.get("PBEMU_MOCK_CORPUS") or cfg.get("corpus") or ""
        # Lazy corpus: 100k Open Library records as Python dicts cost
        # ~100 MB resident (measured) — far over the server's memory
        # budget.  _Corpus keeps per-line byte offsets (~1 MB) plus a
        # compact fingerprint index of (id, fp, added_at) — three
        # string lists, est. ~22-25 MB at 100k records — so the 30s
        # ledger refresh compares against the ledger without
        # re-json.loads-ing or re-deriving BookMeta on every walk.
        self.corpus = _Corpus(corpus_path)
        # id -> corpus index, built on first random-access lookup
        # (get_book / open_file).  The delta walk never needs it.
        self._corpus_index: dict[str, int] | None = None
        # (path, mtime-ns, size) key of the corpus file the id index was
        # built against; an edit after first build invalidates the map so
        # get_book / open_file never resolve stale ids -> wrong records.
        # Mirrors _Corpus._fps_key.
        self._corpus_index_key: tuple[str, int, int] | None = None
        # Stable in-memory book id cache
        self._id_cache: dict[str, str] = {}
        # Short-TTL cache for _scan() (health, library counts)
        self._scan_cache: list[dict[str, Any]] | None = None
        self._scan_cache_ts: float = 0.0
        # Cloud-delete tombstones: ids removed via DELETE /books/{id}.
        # They vanish from every listing/walk/get, which makes the next
        # sync delta report them as removed (the fingerprint walk stops
        # yielding them).
        self._deleted: set[str] = set()

    # --- helpers -----------------------------------------------------------

    def _book_id(self, abs_path: str) -> str:
        cached = self._id_cache.get(abs_path)
        if cached:
            return cached
        s = "mock_" + hashlib.sha1(abs_path.encode("utf-8")).hexdigest()[:16]
        self._id_cache[abs_path] = s
        return s

    def _scan(self) -> list[dict[str, Any]]:
        if not os.path.isdir(self.books_dir):
            return []
        out: list[dict[str, Any]] = []
        try:
            entries = os.listdir(self.books_dir)
        except OSError:
            return []
        for path in sorted(entries):
            full = os.path.join(self.books_dir, path)
            if not os.path.isfile(full):
                continue
            if not path.lower().endswith(
                (".epub", ".pdf", ".fb2", ".djvu", ".txt", ".cbz", ".cbr")
            ):
                continue
            try:
                st = os.stat(full)
            except OSError:
                continue
            ext = path.rsplit(".", 1)[-1].lower() if "." in path else "epub"
            out.append(
                {
                    "abs": full,
                    "name": path,
                    "ext": ext,
                    "size": st.st_size,
                    "mtime": st.st_mtime,
                }
            )
        return out

    def _scan_cached(self) -> list[dict[str, Any]]:
        """self._scan() with a short TTL so per-request calls (health,
        library counts) don't re-run a full os.listdir + per-file
        os.stat on every hit.  The window is small enough that a
        genuinely changed books dir is reflected within a second or
        two."""
        now = time.monotonic()
        if self._scan_cache is not None and now - self._scan_cache_ts < _SCAN_TTL:
            return self._scan_cache
        entries = self._scan()
        self._scan_cache = entries
        self._scan_cache_ts = now
        return entries

    # --- synthetic books ----------------------------------------------------

    def _syn_id(self, i: int) -> str:
        return f"syn_{i:07d}"

    def _syn_index(self, book_id: str) -> int | None:
        """Reverse-map a synthetic id back onto its sequence number."""
        if not book_id.startswith("syn_"):
            return None
        try:
            i = int(book_id[4:])
        except ValueError:
            return None
        return i if 0 <= i < self.synthetic_count else None

    def _corpus_key(self) -> tuple[str, int, int] | None:
        """(path, mtime-ns, size) of the corpus file, or None when it
        cannot be stat'ted (empty path, file gone, …)."""
        path = self.corpus._path
        if not path:
            return None
        try:
            st = os.stat(path)
        except OSError:
            return None
        return (path, st.st_mtime_ns, st.st_size)

    def _corpus_id_index(self) -> dict[str, int]:
        # Rebuild the index whenever the corpus file's (path, mtime-ns,
        # size) key changed since the last build — the same key _Corpus
        # uses for its fingerprint index — so an edit to the corpus file
        # after the first get_book/open_file can't resolve stale ids to
        # the wrong records.
        if self._corpus_index is not None and (
            self._corpus_key() == self._corpus_index_key
        ):
            return self._corpus_index
        index = self.corpus.fps()
        if index is not None:
            # Build from the fingerprint index's ids (the build
            # re-runs when the corpus file changed).
            ids, _fps, _added_ats = index
            self._corpus_index = {ids[i]: i for i in range(len(ids))}
        else:
            # Corpus file unreadable: fall back to a streaming pass
            # (identical to the pre-cache behaviour).
            self._corpus_index = {rec["id"]: i for i, rec in enumerate(self.corpus)}
        self._corpus_index_key = self._corpus_key()
        return self._corpus_index

    def _corpus(self, i: int) -> BookMeta:
        """Metadata for corpus entry #i (real Open Library data)."""
        return self._corpus_meta(i, self.corpus[i])

    def _corpus_meta(self, i: int, rec: dict[str, Any]) -> BookMeta:
        book_id = rec["id"]
        fmt = _SYN_FMTS[i % len(_SYN_FMTS)]
        series_name: str | None = rec.get("series") or None
        series_id: str | None = None
        series_index: float | None = None
        if series_name:
            series_id = "ol_ser_" + hashlib.sha1(series_name.encode()).hexdigest()[:12]
        ts = rec.get("added_at") or _iso(_SYN_EPOCH + i)
        return BookMeta(
            id=book_id,
            title=rec.get("title") or f"Untitled {i}",
            authors=list(rec.get("authors") or []),
            series=series_name,
            series_id=series_id,
            series_index=series_index,
            genre=rec.get("genre") or _SYN_GENRES[i % len(_SYN_GENRES)],
            summary=f"Open Library work {rec.get('ol_key') or book_id}",
            language=None,
            file_format=fmt,
            file_size=10_000 + (i % 900_000),
            page_count=0,
            cover_url=f"/api/v1/books/{book_id}/cover",
            download_url=f"/api/v1/books/{book_id}/file",
            added_at=ts,
            updated_at=ts,
            remote_only=True,
            extra={"index": i, "ol_key": rec.get("ol_key")},
        )

    def _synthetic(self, i: int) -> BookMeta:
        """Metadata for synthetic book #i — pure arithmetic, O(1)."""
        book_id = self._syn_id(i)
        fmt = _SYN_FMTS[i % len(_SYN_FMTS)]
        author = _SYN_AUTHORS[i % len(_SYN_AUTHORS)]
        series_name: str | None = None
        series_id: str | None = None
        series_index: float | None = None
        if i % self.synthetic_series_size != 0:
            # Members 1..size-1 of each block join the block's series.
            block = i // self.synthetic_series_size
            name = f"{_SYN_SERIES[block % len(_SYN_SERIES)]} {block:04d}"
            series_name = name
            series_id = "syn_ser_" + hashlib.sha1(name.encode()).hexdigest()[:12]
            series_index = float(i % self.synthetic_series_size)
        title = f"Synthetic Book {i:07d}"
        ts = _iso(_SYN_EPOCH + i)
        return BookMeta(
            id=book_id,
            title=title,
            authors=[author],
            series=series_name,
            series_id=series_id,
            series_index=series_index,
            genre=_SYN_GENRES[i % len(_SYN_GENRES)],
            summary=f"Synthetic mock book #{i}",
            language=None,
            file_format=fmt,
            file_size=10_000 + (i % 900_000),
            page_count=0,
            cover_url=f"/api/v1/books/{book_id}/cover",
            download_url=f"/api/v1/books/{book_id}/file",
            added_at=ts,
            updated_at=ts,
            remote_only=True,
            extra={"synthetic": True, "index": i},
        )

    def _all(self, series_id: str | None, search: str | None, since: str | None):
        """Yield every live BookMeta (dir books first, then synthetic),
        applying the cheap provider-side filters.  Generator so callers
        slice without materialising the whole library."""
        for entry in self._scan():
            meta = self._book_from_path(entry)
            if meta.id in self._deleted:
                continue
            if series_id and meta.series_id != series_id:
                continue
            if search and search.lower() not in meta.title.lower():
                continue
            if since and meta.updated_at and meta.updated_at <= since:
                continue
            yield meta
        if search:
            q = search.lower()
        else:
            q = None
        if self.corpus:
            used = 0
            limit = min(self.synthetic_count, len(self.corpus))
            for i, rec in enumerate(self.corpus):
                if i >= limit:
                    break
                meta = self._corpus_meta(i, rec)
                if meta.id in self._deleted:
                    continue
                if series_id and meta.series_id != series_id:
                    continue
                if q is not None and q not in meta.title.lower():
                    continue
                if since and meta.updated_at and meta.updated_at <= since:
                    continue
                yield meta
                used = i + 1
            # Pad a short corpus with synthetic books so the advertised
            # count always holds.
            for i in range(used, self.synthetic_count):
                meta = self._synthetic(i)
                if meta.id in self._deleted:
                    continue
                if series_id and meta.series_id != series_id:
                    continue
                if q is not None and q not in meta.title.lower():
                    continue
                if since and meta.updated_at and meta.updated_at <= since:
                    continue
                yield meta
            return
        for i in range(self.synthetic_count):
            meta = self._synthetic(i)
            if meta.id in self._deleted:
                continue
            if series_id and meta.series_id != series_id:
                continue
            if q is not None and q not in meta.title.lower():
                continue
            if since and meta.updated_at and meta.updated_at <= since:
                continue
            yield meta

    @staticmethod
    def _series_from_stem(
        stem: str,
    ) -> tuple[str | None, str | None, float | None]:
        """(series name, id, index) for a file stem, using the mock
        series convention: "Series Name - 03" → series="Series Name",
        series_index=3, series_id=stable hash.  Plain "book_NNN" names
        stay standalone (series=None) so existing tests are unaffected.
        Shared by _book_from_path and the fingerprint walker so both
        derive identical series fields."""
        dash_pos = stem.rfind(" - ")
        if dash_pos > 0:
            tail = stem[dash_pos + 3 :].strip()
            if tail.isdigit():
                series_name = stem[:dash_pos].replace("_", " ").strip()
                return (
                    series_name,
                    "mock_ser_" + hashlib.sha1(series_name.encode()).hexdigest()[:12],
                    float(tail),
                )
        return None, None, None

    def _book_from_path(self, entry: dict[str, Any]) -> BookMeta:
        book_id = self._book_id(entry["abs"])
        ext = entry["ext"]
        stem = os.path.splitext(entry["name"])[0]
        title = stem.replace("_", " ").strip() or entry["name"]
        series_name, series_id, series_index = self._series_from_stem(stem)

        return BookMeta(
            id=book_id,
            title=title,
            authors=["pbemu mock library"],
            series=series_name,
            series_id=series_id,
            series_index=series_index,
            summary=f"Mock book from {entry['abs']}",
            language=None,
            file_format=ext,
            file_name=entry["name"],
            file_size=entry["size"],
            page_count=0,
            cover_url=f"/api/v1/books/{book_id}/cover",
            download_url=f"/api/v1/books/{book_id}/file",
            added_at=_iso(entry["mtime"]),
            updated_at=_iso(entry["mtime"]),
            remote_only=True,
            extra={"abs_path": entry["abs"]},
        )

    # --- Provider interface -----------------------------------------------

    def health(self) -> dict[str, Any]:
        total = len(self._scan_cached()) + self.synthetic_count
        layer = (
            f"{min(self.synthetic_count, len(self.corpus))} corpus"
            if self.corpus
            else f"{self.synthetic_count} synthetic"
        )
        return {
            "ok": True,
            "detail": f"mock: {total} books ({layer})",
        }

    def list_libraries(self) -> list[LibraryInfo]:
        return [
            LibraryInfo(
                id="mock_lib",
                name=self.library_name,
                book_count=len(self._scan_cached()) + self.synthetic_count,
                kind="library",
            )
        ]

    def list_series(self, library_id: str) -> list[SeriesInfo]:
        seen: dict[str, SeriesInfo] = {}
        for meta in self._all(None, None, None):
            if meta.series_id and meta.series_id not in seen:
                seen[meta.series_id] = SeriesInfo(
                    id=meta.series_id,
                    name=meta.series or "Unknown",
                    library_id=library_id,
                    book_count=0,
                )
            if meta.series_id and meta.series_id in seen:
                seen[meta.series_id].book_count += 1
        return list(seen.values())

    def list_authors(self, library_id: str | None = None) -> list[AuthorInfo]:
        return []

    def list_books(
        self,
        *,
        mode: str = "all",
        library_id: str | None = None,
        series_id: str | None = None,
        author_id: str | None = None,
        search: str | None = None,
        limit: int = 500,
        offset: int = 0,
        since: str | None = None,
    ) -> list[BookMeta]:
        if mode == "all" and not series_id and not search and not since:
            # Unfiltered: slice by index instead of re-streaming + re-
            # json.loads-ing the whole corpus from line 0 for every page.
            # Catalogue order = dir books, then corpus records, then
            # synthetic padding (mirrors _all / walk_fingerprints), so
            # the [offset:offset+limit] window maps onto the three layers
            # via _scan(), _Corpus.__getitem__ random access, and pure
            # synthetic arithmetic — no re-parse.
            out: list[BookMeta] = []
            scanned = self._scan()
            n_dir = len(scanned)
            n_corpus = min(self.synthetic_count, len(self.corpus)) if self.corpus else 0
            start = offset
            end = offset + limit
            # dir layer: [0, n_dir)
            if start < n_dir:
                for entry in scanned[start : min(end, n_dir)]:
                    meta = self._book_from_path(entry)
                    if meta.id in self._deleted:
                        continue
                    out.append(meta)
            # corpus layer: [n_dir, n_dir + n_corpus)
            c_start = n_dir
            c_end = n_dir + n_corpus
            if start < c_end:
                lo = max(start - c_start, 0)
                hi = min(end - c_start, n_corpus)
                for i in range(lo, hi):
                    meta = self._corpus(i)
                    if meta.id in self._deleted:
                        continue
                    out.append(meta)
            # synthetic padding layer: [n_dir + n_corpus, n_dir +
            # synthetic_count)
            s_start = n_dir + n_corpus
            if end > s_start:
                lo = max(start - s_start, 0)
                hi = min(end - s_start, self.synthetic_count)
                for i in range(lo, hi):
                    meta = self._synthetic(i)
                    if meta.id in self._deleted:
                        continue
                    out.append(meta)
            return out
        out = []
        skipped = 0
        for meta in self._all(series_id, search, since):
            if skipped < offset:
                skipped += 1
                continue
            out.append(meta)
            if len(out) >= limit:
                break
        return out

    def walk_books(
        self, *, mode: str = "all", chunk_size: int = 500
    ) -> Iterator[list[BookMeta]]:
        """Single-pass catalogue walk.

        The default ``list_books`` offset paging re-scans the catalogue
        from index 0 for every page, which is quadratic at 100k synthetic
        books; walking ``_all`` once keeps the cover warm-up and the
        ledger refresh linear.  Filtered modes are rare and cheap, so
        those fall back to the base implementation.
        """
        if mode != "all":
            yield from super().walk_books(mode=mode, chunk_size=chunk_size)
            return
        chunk: list[BookMeta] = []
        for meta in self._all(None, None, None):
            chunk.append(meta)
            if len(chunk) >= chunk_size:
                yield chunk
                chunk = []
        if chunk:
            yield chunk

    # --- fingerprint walk --------------------------------------------------

    def walk_fingerprints(self) -> Iterator[tuple[str, str, str]]:
        """Yield (id, fp, added_at) for every live book in catalogue
        order (dir books, then corpus, then synthetic padding) without
        materialising BookMeta objects.  Corpus records come from the
        compact fingerprint index (one streaming build, then plain list
        walks); synthetic books are pure arithmetic; dir books
        re-derive exactly the fields _book_from_path would.  The
        ledger's refresh uses this to diff a steady state against its
        stored fingerprints without parsing the corpus — the same
        output shape and values as walk_books' metas, so a provider
        that implements it can skip BookMeta construction entirely."""
        for entry in self._scan():
            fid = self._book_id(entry["abs"])
            if fid in self._deleted:
                continue
            yield self._dir_fp(entry)
        if self.corpus:
            # Catalogue = dir books + the first `count` corpus records
            # + synthetic padding (mirrors _all).
            limit = min(self.synthetic_count, len(self.corpus))
            for id, fp, ts in self._corpus_fps(limit):
                if id in self._deleted:
                    continue
                yield (id, fp, ts)
            for i in range(limit, self.synthetic_count):
                id, fp, ts = self._syn_fp(i)
                if id in self._deleted:
                    continue
                yield (id, fp, ts)
        else:
            for i in range(self.synthetic_count):
                id, fp, ts = self._syn_fp(i)
                if id in self._deleted:
                    continue
                yield (id, fp, ts)

    def _corpus_fps(self, limit: int) -> Iterator[tuple[str, str, str]]:
        """(id, fp, added_at) for the first ``limit`` corpus records in
        file order, from the fingerprint index (built if stale).  If
        the corpus file cannot be read, falls back to a streaming pass
        with the same derivation and output shape."""
        index = self.corpus.fps()
        if index is not None:
            ids, fps, added_ats = index
            for i in range(limit):
                yield ids[i], fps[i], added_ats[i]
            return
        for i, rec in enumerate(self.corpus):
            if i >= limit:
                break
            yield _corpus_fp(i, rec)

    def _dir_fp(self, entry: dict[str, Any]) -> tuple[str, str, str]:
        """(id, fp, added_at) for a books-dir entry — derives exactly
        the fields _book_from_path derives (title, authors, series via
        the dash convention, format, file_name, size, added_at)."""
        book_id = self._book_id(entry["abs"])
        stem = os.path.splitext(entry["name"])[0]
        title = stem.replace("_", " ").strip() or entry["name"]
        series_name, series_id, series_index = self._series_from_stem(stem)
        ts = _iso(entry["mtime"])
        return (
            book_id,
            fingerprint_blob(
                title,
                ["pbemu mock library"],
                series_name,
                series_id,
                series_index,
                entry["ext"],
                entry["name"],
                entry["size"],
                ts,
            ),
            ts,
        )

    def _syn_fp(self, i: int) -> tuple[str, str, str]:
        """(id, fp, added_at) for synthetic book #i — pure arithmetic,
        deriving exactly the fields _synthetic derives."""
        fmt = _SYN_FMTS[i % len(_SYN_FMTS)]
        author = _SYN_AUTHORS[i % len(_SYN_AUTHORS)]
        series_name: str | None = None
        series_id: str | None = None
        series_index: float | None = None
        if i % self.synthetic_series_size != 0:
            # Members 1..size-1 of each block join the block's series.
            block = i // self.synthetic_series_size
            name = f"{_SYN_SERIES[block % len(_SYN_SERIES)]} {block:04d}"
            series_name = name
            series_id = "syn_ser_" + hashlib.sha1(name.encode()).hexdigest()[:12]
            series_index = float(i % self.synthetic_series_size)
        ts = _iso(_SYN_EPOCH + i)
        return (
            self._syn_id(i),
            fingerprint_blob(
                f"Synthetic Book {i:07d}",
                [author],
                series_name,
                series_id,
                series_index,
                fmt,
                None,  # synthetic metas carry no file_name
                10_000 + (i % 900_000),
                ts,
            ),
            ts,
        )

    def delete_book(self, book_id: str) -> bool:
        if self.get_book(book_id) is None:
            return False
        self._deleted.add(book_id)
        return True

    def get_book(self, book_id: str) -> BookMeta | None:
        if book_id in self._deleted:
            return None
        idx = self._syn_index(book_id)
        if idx is not None:
            return self._synthetic(idx)
        if self.corpus:
            ci = self._corpus_id_index().get(book_id)
            if ci is not None:
                return self._corpus(ci)
        for entry in self._scan():
            if self._book_id(entry["abs"]) == book_id:
                return self._book_from_path(entry)
        return None

    def get_cover(self, book_id: str) -> bytes | None:
        # No real covers in mock mode — return the 1x1 placeholder.
        return PLACEHOLDER_PNG

    def open_file(self, book_id: str) -> tuple[str, Iterator[bytes]] | None:
        if book_id in self._deleted:
            return None
        idx = self._syn_index(book_id)
        if idx is not None:
            meta = self._synthetic(idx)
            return f"{meta.title}.{meta.file_format}", _synthetic_bytes(idx)
        if self.corpus:
            ci = self._corpus_id_index().get(book_id)
            if ci is not None:
                meta = self._corpus(ci)
                return f"{meta.title}.{meta.file_format}", _synthetic_bytes(ci)
        for entry in self._scan():
            if self._book_id(entry["abs"]) == book_id:
                return entry["name"], _file_iter(entry["abs"])
        return None


def _iso(t: float) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t))


def _file_iter(path: str) -> Iterator[bytes]:
    try:
        f = open(path, "rb")  # noqa: SIM115
    except OSError:
        return
    try:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            yield chunk
    finally:
        with suppress(OSError):
            f.close()


def _synthetic_bytes(i: int) -> Iterator[bytes]:
    """Tiny deterministic payload for synthetic book downloads — big
    enough to look like a file, small enough to stream instantly.
    ``PBEMU_MOCK_DL_DELAY_MS`` (test hook) sleeps before the first chunk
    to simulate a slow link so UI-responsiveness tests can run."""
    delay_ms = float(os.environ.get("PBEMU_MOCK_DL_DELAY_MS", "0") or 0)
    if delay_ms > 0:
        time.sleep(delay_ms / 1000.0)
    header = f"SYNTHETIC BOOK #{i}\n".encode()
    yield header
    yield b"\x00" * max(0, 4096 - len(header))
