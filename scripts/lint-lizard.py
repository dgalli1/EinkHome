#!/usr/bin/env python3
"""Complexity + duplicate-code gate over the EinkHome C app.

Uses `lizard` to:
  * fail CI when a NEW function exceeds a cyclomatic-complexity ceiling
    (GATE_CCN), so an unmaintainable function cannot land; functions that
    already exceeded it at the time this gate was introduced are recorded
    in BASELINE and allowed to stay (they form the refactor backlog);
  * report the sub-ceiling complexity backlog + duplicate blocks as
    warnings (CI-green, debt surfaced).

lizard needs no compile DB, so this runs standalone.

Usage:
    scripts/lint-lizard.py [--gate-ccn 28] [--ccn 14] [--warn-only]
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# CI fails when a function NOT in BASELINE exceeds this.
GATE_CCN = 28
# Everything at/above this is printed as the refactor backlog.
BACKLOG_CCN = 14
# Path (relative to browser.py's dir) -> file of "path:function" baseline.
BASELINE_FILE = REPO_ROOT / "ci" / "lizard-baseline.txt"

# Functions that already exceeded GATE_CCN when the gate landed.  They are
# the debt to refactor down; they stay CI-green but show in the backlog.
def _load_baseline() -> set[tuple[str, str]]:
    if not BASELINE_FILE.is_file():
        return set()
    items = set()
    for raw in BASELINE_FILE.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if ":" in line:
            path, _, name = line.partition(":")
            items.add((path.strip(), name.strip()))
    return items


def _collect_c_sources() -> list[str]:
    srcs = []
    for d in ("core", "data", "ui", "action", "platform"):
        for p in sorted((REPO_ROOT / "app" / d).glob("*.c")):
            srcs.append(str(p))
    return srcs


# lint: disable=line-too-long
_ROW_RE = re.compile(
    r"^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(.+?)@(\d+)-(\d+)@(.+)$"
)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--gate-ccn", type=int, default=GATE_CCN)
    ap.add_argument("--ccn", type=int, default=BACKLOG_CCN)
    ap.add_argument("--warn-only", action="store_true",
                    help="report but never fail CI")
    args = ap.parse_args()

    srcs = _collect_c_sources()
    if not srcs:
        print("lint-lizard: no C sources found")
        return 2

    baseline = _load_baseline()

    cmd = ["lizard", "-Eduplicate", *srcs, "-C", str(args.gate_ccn)]
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    out = proc.stdout or proc.stderr or ""

    rows = []
    seen: set[tuple] = set()
    for line in out.splitlines():
        m = _ROW_RE.match(line)
        if not m:
            continue
        nloc, ccn, _tok, _param, _len, name, _l0, _l1, file_ = m.groups()
        ccn_i = int(ccn)
        fpath = file_.replace(str(REPO_ROOT), "").lstrip("/")
        key = (ccn_i, name, fpath)
        if key in seen:
            continue
        seen.add(key)
        rows.append((ccn_i, int(nloc), name, fpath))

    # Backlog (default all >= BACKLOG_CCN).
    backlog = sorted((r for r in rows if r[0] >= args.ccn), reverse=True)
    if backlog:
        print(f"Cyclomatic-complexity backlog (>= {args.ccn}):")
        for ccn_i, nloc, name, fpath in backlog:
            print(f"  CCN {ccn_i:3d}  NLOC {nloc:3d}  {name}  @ {fpath}")
        print()

    # Duplicate-code report (lizard prints a Duplicates section when the
    # -Eduplicate extension is enabled).
    in_dup = False
    for line in out.splitlines():
        if "Duplicates" in line:
            in_dup = True
            print(line)
            continue
        # Print the block list and the unique/duplicate rates.
        if in_dup and line.strip() and (line.startswith("Duplicate") or "rate:" in line
                                        or line.startswith("-")):
            print(line)

    # New offenders above the cap (not in the baseline).
    new_offenders = [
        r for r in rows
        if r[0] > args.gate_ccn and (r[3], r[2]) not in baseline
    ]
    if new_offenders:
        print(f"\nCI gate: {len(new_offenders)} NEW function(s) exceed "
              f"CCN {args.gate_ccn}:")
        for ccn_i, nloc, name, fpath in sorted(new_offenders, reverse=True):
            tag = "" if (fpath, name) in baseline else "  <-- NEW"
            print(f"  CCN {ccn_i:3d}  NLOC {nloc:3d}  {name}  @ {fpath}{tag}")
    if args.warn_only:
        return 0
    return 1 if new_offenders else 0


if __name__ == "__main__":
    sys.exit(main())
