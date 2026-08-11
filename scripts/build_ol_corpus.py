#!/usr/bin/env python3
"""Build a realistic mock-book corpus from Open Library data dumps.

Streams ``ol_dump_works_latest.txt.gz`` (titles + author keys) and
``ol_dump_authors_latest.txt.gz`` (author names) and writes a JSONL
corpus the mock provider serves via ``PBEMU_MOCK_CORPUS``:

    {"id": "ol_OL123W", "title": "...", "authors": ["..."],
     "series": "..." | null, "added_at": "ISO" | null}

Only the works-dump prefix needed for ``--count`` valid works is
downloaded (the stream aborts once enough works are kept); the authors
dump is read in full because the needed keys are scattered.

Usage:
    python3 scripts/build_ol_corpus.py --count 100000
"""

from __future__ import annotations

import argparse
import gzip
import json
import sys
import urllib.request
from pathlib import Path

WORKS_URL = "https://openlibrary.org/data/ol_dump_works_latest.txt.gz"
AUTHORS_URL = "https://openlibrary.org/data/ol_dump_authors_latest.txt.gz"
OTHER_URL = "https://openlibrary.org/data/ol_dump_other_latest.txt.gz"
_UA = "einkhome-mock-corpus-builder/1.0 (dev tool; abort-early stream)"
_REPO_ROOT = Path(__file__).resolve().parent.parent


def _stream_lines(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": _UA})
    resp = urllib.request.urlopen(req, timeout=120)
    f = gzip.GzipFile(fileobj=resp)
    try:
        for line in f:
            yield line
    finally:
        resp.close()


def _work_author_keys(rec: dict) -> list[str]:
    """Author keys of a work record, in record order."""
    keys: list[str] = []
    for a in rec.get("authors") or []:
        if not isinstance(a, dict):
            continue
        author = a.get("author")
        if isinstance(author, dict) and author.get("key"):
            keys.append(author["key"])
        elif a.get("key"):
            keys.append(a["key"])
    return keys


def _series_name(rec: dict) -> str | None:
    """Inline series name when the work record carries one (most
    records only reference /series/ keys; those are resolved via the
    other dump's /type/series records)."""
    for s in rec.get("series") or []:
        if not isinstance(s, dict):
            continue
        sub = s.get("series")
        if isinstance(sub, dict):
            name = sub.get("title") or sub.get("name") or ""
            if name:
                return name
    return None


def _series_keys(rec: dict) -> list[str]:
    """Series keys of a work record: the dump nests them as
    {"series": {"key": "/series/..."}, "position": "N"}."""
    keys: list[str] = []
    for s in rec.get("series") or []:
        if not isinstance(s, dict):
            continue
        sub = s.get("series")
        if isinstance(sub, dict) and sub.get("key", "").startswith("/series/"):
            keys.append(sub["key"])
    return keys


def _created_iso(rec: dict) -> str | None:
    created = rec.get("created")
    if isinstance(created, dict) and created.get("value"):
        return created["value"]
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--count", type=int, default=100_000,
                    help="corpus size (default 100000)")
    ap.add_argument("--keep", type=int, default=130_000,
                    help="works to keep in the first pass before resolving "
                         "authors (buffer for unresolvable names)")
    ap.add_argument("--max-lines", type=int, default=0,
                    help="stop the works stream after N lines (0 = keep "
                         "reading until --keep works are found)")
    ap.add_argument("--stride", type=int, default=1,
                    help="keep every N-th valid work — samples a wider "
                         "slice of the dump instead of only the lowest OL "
                         "keys (pair with --max-lines)")
    ap.add_argument("--out", default=str(_REPO_ROOT / ".cover-cache" / "mock_books.jsonl"),
                    help="output JSONL path (default: <repo>/.cover-cache/mock_books.jsonl)")
    args = ap.parse_args()

    if args.keep <= args.count:
        ap.error(
            f"--keep ({args.keep}) must be greater than --count ({args.count}): "
            "the first pass buffers extra works so unresolvable author names "
            "can be dropped without falling short of the requested count"
        )

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    works: list[dict] = []
    work_series_keys: list[list[str]] = []
    author_keys: set[str] = set()
    series_keys: set[str] = set()
    n_lines = 0
    valid_seen = 0
    print(f"streaming works dump (keeping {args.keep} valid works)...",
          file=sys.stderr)
    for line in _stream_lines(WORKS_URL):
        n_lines += 1
        if args.max_lines and n_lines > args.max_lines:
            break
        parts = line.rstrip(b"\n").split(b"\t")
        if len(parts) < 5 or parts[0] != b"/type/work":
            continue
        try:
            rec = json.loads(parts[4])
        except ValueError:
            continue
        title = (rec.get("title") or "").strip()
        if len(title) < 2:
            continue
        keys = _work_author_keys(rec)
        if not keys:
            continue
        valid_seen += 1
        if args.stride > 1 and valid_seen % args.stride != 0:
            continue
        works.append(rec)
        work_series_keys.append(_series_keys(rec))
        series_keys.update(work_series_keys[-1])
        author_keys.update(keys)
        if len(works) % 20_000 == 0:
            print(f"  {len(works)} works kept ({n_lines} lines read)",
                  file=sys.stderr)
        if len(works) >= args.keep:
            break
    print(f"works kept: {len(works)} from {n_lines} lines; "
          f"{len(author_keys)} author keys", file=sys.stderr)

    names: dict[str, str] = {}
    n_lines = 0
    print("streaming authors dump (full read)...", file=sys.stderr)
    for line in _stream_lines(AUTHORS_URL):
        n_lines += 1
        parts = line.rstrip(b"\n").split(b"\t")
        if len(parts) < 5 or parts[0] != b"/type/author":
            continue
        key = parts[1].decode("utf-8", "replace")
        if key not in author_keys:
            continue
        try:
            rec = json.loads(parts[4])
        except ValueError:
            continue
        name = (rec.get("name") or "").strip()
        if name:
            names[key] = name
        if len(names) % 20_000 == 0:
            print(f"  {len(names)} authors resolved ({n_lines} lines read)",
                  file=sys.stderr)
    print(f"authors resolved: {len(names)} of {len(author_keys)}",
          file=sys.stderr)

    series_names: dict[str, str] = {}
    n_lines = 0
    if series_keys:
        print("streaming other dump (series records)...", file=sys.stderr)
        for line in _stream_lines(OTHER_URL):
            n_lines += 1
            parts = line.rstrip(b"\n").split(b"\t")
            if len(parts) < 5 or parts[0] != b"/type/series":
                continue
            key = parts[1].decode("utf-8", "replace")
            if key not in series_keys:
                continue
            try:
                rec = json.loads(parts[4])
            except ValueError:
                continue
            name = (rec.get("name") or "").strip()
            if name:
                series_names[key] = name
        print(f"series resolved: {len(series_names)} of {len(series_keys)}",
              file=sys.stderr)

    out: list[dict] = []
    dropped = 0
    for rec, skeys in zip(works, work_series_keys):
        seen: set[str] = set()
        authors: list[str] = []
        for k in _work_author_keys(rec):
            name = names.get(k)
            if name and name not in seen:
                seen.add(name)
                authors.append(name)
        if not authors:
            dropped += 1
            continue
        series = _series_name(rec)
        if series is None:
            for k in skeys:
                if k in series_names:
                    series = series_names[k]
                    break
        key = rec.get("key") or ""
        out.append(
            {
                "id": "ol_" + key.replace("/works/", ""),
                "ol_key": key,
                "title": (rec.get("title") or "").strip(),
                "authors": authors,
                "series": series,
                "added_at": _created_iso(rec),
            }
        )
        if len(out) >= args.count:
            break
    print(f"dropped (no resolvable author): {dropped}", file=sys.stderr)

    if len(out) < args.count:
        print(
            f"ERROR: only {len(out)} of {args.count} requested entries could be "
            f"built ({dropped} works dropped for unresolvable author names); "
            "raise --keep (or lower --count) and re-run",
            file=sys.stderr,
        )
        return 1

    with open(out_path, "w", encoding="utf-8") as f:
        for rec in out:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(f"wrote {len(out)} entries -> {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
