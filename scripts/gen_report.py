#!/usr/bin/env python3
"""Playwright-style static HTML report generator for the EinkHome e2e suite.

Reads build/report/results.json (produced by the pytest collector plugin,
see docs/playwright-report-spec.md for the data contract), copies the
per-test screenshot PNGs into build/report/screenshots/<test>/ and the
firmware logs into build/report/logs/, then emits a single self-contained
build/report/index.html (inline CSS/JS, zero external assets) that renders
a dark, Playwright-style report with hash routing, status filters, live
search, expandable tests (steps tree + screenshot filmstrip + image
viewer) and a speedboard of the slowest tests.

Pure stdlib. Usage:
    scripts/gen_report.py [--results build/report/results.json]
                          [--out build/report]
                          [--firmware-dir pbemu/U634k3_6.10.2544]
    scripts/gen_report.py --serve [--port 8901]
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
import time
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path

DEFAULT_RESULTS = Path("build/report/results.json")
DEFAULT_OUT = Path("build/report")
DEFAULT_FIRMWARE = Path("pbemu/U634k3_6.10.2544")
DEFAULT_PORT = 8901

LOG_SOURCES = (
    (".live/var/log/system.log", "system.log"),
    (".live/var/log/monitor.log", "monitor.log"),
    (".live/var/log/informer.log", "informer.log"),
    (".live/tmp/bookshelf.log", "bookshelf.log"),
    (".live/mnt/ext1/system/bin/bookshelf.log", "bookshelf.log"),
)

API_LOG_DEFAULT = Path("build/pbemu-api-test.log")
SLICE_MARGIN_S = 1.0  # seconds added on each side of a test's time window
DAY_S = 86400

_TS_RE = re.compile(r"^\[(\d{2}):(\d{2}):(\d{2})\]\s?")
_LOG_OPEN_MARKER = "--- bookshelf.app log opened"


def _safe_dir_name(name: str) -> str:
    """Mirror of the collector's per-test directory sanitization
    (tests/support/bookshelf/report.py) so log dirs match screenshot dirs."""
    return "".join(c if c.isalnum() or c in "._-" else "_" for c in name)


def _find_log_source(fw: Path, name: str) -> Path | None:
    """First existing LOG_SOURCES candidate for *name* (e.g. bookshelf.log)."""
    for rel, log_name in LOG_SOURCES:
        if log_name == name and (fw / rel).is_file():
            return fw / rel
    return None


def _api_log_path(out_dir: Path) -> Path:
    """The pbemu API server log: build/pbemu-api-test.log (next to --out if set)."""
    candidates = [API_LOG_DEFAULT, out_dir.parent / "pbemu-api-test.log"]
    return next((p for p in candidates if p.is_file()), candidates[0])


def _sod(line: str) -> int | None:
    """Seconds-of-day of a leading [HH:MM:SS] stamp, or None when absent."""
    m = _TS_RE.match(line)
    if not m:
        return None
    h, mi, s = (int(g) for g in m.groups())
    return h * 3600 + mi * 60 + s


def _epoch_sod(ts: float) -> int:
    """Local seconds-of-day of epoch *ts* (log stamps are local time)."""
    lt = time.localtime(ts)
    return lt.tm_hour * 3600 + lt.tm_min * 60 + lt.tm_sec


def _sod_fmt(sod: int | None) -> str:
    """Format seconds-of-day as HH:MM:SS, or '?' when unknown."""
    if sod is None:
        return "??:??:??"
    return f"{sod // 3600:02d}:{(sod % 3600) // 60:02d}:{sod % 60:02d}"


def _window_header(title: str, started: float, finished: float) -> str:
    """Header for a time-window slice (margins included)."""
    return (
        f"# scoped to {title} "
        f"[{time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(started - SLICE_MARGIN_S))} .. "
        f"{time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(finished + SLICE_MARGIN_S))}]"
    )


def _slice_bookshelf_by_ordinals(
    src: Path, open_start: int, open_end: int
) -> list[str] | None:
    """Lines of the bookshelf log for invocations [open_start, open_end).

    The ordinals are the marker indices recorded by the suite fixtures
    (each ``--- bookshelf.app log opened`` line is one invocation), so
    the per-test scopes are exactly disjoint — no neighbor bleed and no
    clock-skew ambiguity.  Returns None when the log lacks the markers
    (old format) or the ordinals are out of range (log rotated).
    """
    try:
        text = src.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    lines = text.splitlines()
    marker_idx = [
        i for i, line in enumerate(lines) if _LOG_OPEN_MARKER in line
    ]
    if not marker_idx:
        return None
    if open_start < 0 or open_start >= len(marker_idx):
        return None
    start = marker_idx[open_start]
    end = marker_idx[open_end] if open_end < len(marker_idx) else len(lines)
    return lines[start:end]


def _slice_lines(src: Path, start_sod: int, end_sod: int) -> list[str] | None:
    """Lines of *src* whose [HH:MM:SS] stamp falls in [start_sod, end_sod].

    Seconds-of-day wrap at midnight (end_sod < start_sod): the window is
    [start_sod, 86400) union [0, end_sod). Untimestamped lines in a
    timestamped file are dropped (outside any window by construction).
    Returns None when the file has no timestamped lines at all (old
    format), letting the caller fall back to a full copy.
    """
    try:
        text = src.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    lines = text.splitlines()
    sods = [_sod(line) for line in lines]
    if not any(s is not None for s in sods):
        return None
    wrapped = end_sod < start_sod
    if wrapped:
        end_sod += DAY_S
    out = []
    for line, sod in zip(lines, sods, strict=True):
        if sod is None:
            continue
        if wrapped and sod < start_sod:
            sod += DAY_S
        if start_sod <= sod <= end_sod:
            out.append(line)
    return out


def _write_scoped_logs(test: dict, bookshelf_src, api_src, out_dir) -> list[str] | None:
    """Slice bookshelf.log + api.log to this test's window into logs/<test>/.

    bookshelf.log is cut at invocation-marker boundaries (disjoint per
    test); api.log by the time window.  Returns destination-relative
    paths (bookshelf.log first, then api.log), or None when the test
    lacks a usable started_at/finished_at — the generator then falls
    back to the shared full logs for that test.
    """
    started = test.get("started_at")
    finished = test.get("finished_at")
    if not isinstance(started, (int, float)) or not isinstance(finished, (int, float)):
        return None
    test_dir = _safe_dir_name(test.get("title", "test"))
    dest_dir = out_dir / "logs" / test_dir
    dest_dir.mkdir(parents=True, exist_ok=True)
    title = test.get("title", "")

    def _write(name: str, src: Path | None, header: str, lines: list[str] | None) -> str | None:
        if src is None or not src.is_file():
            return None
        dest = dest_dir / name
        if lines is None:
            # Old format (no timestamps): keep the full file so it still renders.
            text = src.read_text(encoding="utf-8", errors="replace")
            dest.write_text(header + "\n" + text, encoding="utf-8")
        else:
            dest.write_text(header + "\n" + "\n".join(lines) + "\n", encoding="utf-8")
        return f"logs/{test_dir}/{name}"

    rel = []

    # bookshelf.log: exact invocation-ordinal slice (disjoint per test);
    # falls back to the time window when ordinals are missing.
    open_start = test.get("log_open_start")
    open_end = test.get("log_open_end")
    start_sod = _epoch_sod(started - SLICE_MARGIN_S)
    end_sod = _epoch_sod(finished + SLICE_MARGIN_S)
    if isinstance(open_start, int) and isinstance(open_end, int):
        ordinal_lines = _slice_bookshelf_by_ordinals(bookshelf_src, open_start, open_end)
    else:
        ordinal_lines = None
    if ordinal_lines is not None:
        first_sod = _sod(ordinal_lines[0])
        last_sod = _sod(ordinal_lines[-1])
        header = f"# scoped to {title} [invocations {open_start}..{open_end - 1}, {_sod_fmt(first_sod)} .. {_sod_fmt(last_sod)}]"
        rel.append(_write("bookshelf.log", bookshelf_src, header, ordinal_lines))
    else:
        header = _window_header(title, started, finished)
        rel.append(_write("bookshelf.log", bookshelf_src, header,
                          _slice_lines(bookshelf_src, start_sod, end_sod)))

    # api.log: time-window slice (no invocation markers in that file).
    header = _window_header(title, started, finished)
    rel.append(_write("api.log", api_src, header, _slice_lines(api_src, start_sod, end_sod)))

    return [r for r in rel if r] or None

HTML_TEMPLATE = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>EinkHome e2e report</title>
<style>
:root{
  --bg:#0d1117; --panel:#161b22; --panel2:#1c2128; --border:#30363d;
  --text:#e6edf3; --muted:#8b949e; --accent:#58a6ff;
  --green:#3fb950; --red:#f85149; --amber:#d29922;
}
*{box-sizing:border-box}
html,body{margin:0;padding:0}
body{background:var(--bg);color:var(--text);font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif;-webkit-font-smoothing:antialiased}
.toolbar{position:sticky;top:0;z-index:50;background:rgba(13,17,23,.92);backdrop-filter:blur(8px);border-bottom:1px solid var(--border)}
.toolbar-inner{max-width:1280px;margin:0 auto;padding:12px 16px;display:flex;align-items:center;gap:14px;flex-wrap:wrap}
.search-wrap{position:relative;flex:1;min-width:200px}
.search-wrap .mag{position:absolute;left:9px;top:50%;transform:translateY(-50%);color:var(--muted);pointer-events:none}
#search{padding:6px 10px 6px 32px;background:var(--panel);border:1px solid var(--border);border-radius:6px;color:var(--text);width:100%;font-size:13px}
#search::placeholder{color:var(--muted)}
#search:focus{outline:none;border-color:var(--accent)}
.chips{display:flex;gap:6px;align-items:center;flex-wrap:wrap}
.chip{display:inline-flex;align-items:center;gap:6px;padding:4px 11px;border:1px solid var(--border);border-radius:999px;color:var(--muted);text-decoration:none;font-size:12px;background:transparent;cursor:pointer;user-select:none}
.chip:hover{border-color:var(--muted);color:var(--text)}
.chip.active{background:var(--panel2);border-color:var(--accent);color:var(--text)}
.glyph{font-size:13px;line-height:1;font-weight:700}
.glyph.pass{color:var(--green)} .glyph.fail{color:var(--red)} .glyph.skip{color:var(--muted)}
.badge{background:#21262d;border-radius:999px;padding:0 7px;font-size:11px;color:var(--muted);white-space:nowrap}
.toolbar-right{margin-left:auto;display:flex;gap:12px;align-items:center}
.link{color:var(--accent);text-decoration:none;font-size:13px;display:inline-flex;gap:5px;align-items:center}
.link:hover{text-decoration:underline}
.meta{max-width:1280px;margin:0 auto;padding:10px 16px;color:var(--muted);font-size:12px;display:flex;gap:18px;flex-wrap:wrap;border-bottom:1px solid var(--border)}
.meta b{color:var(--text);font-weight:600}
main{max-width:1280px;margin:0 auto;padding:18px 16px 60px}
.empty{color:var(--muted);text-align:center;padding:48px 0}
/* groups */
.group{margin-bottom:10px;border:1px solid var(--border);border-radius:8px;overflow:hidden;background:var(--panel)}
.group-head{display:flex;align-items:center;gap:8px;width:100%;padding:10px 14px;background:transparent;border:none;color:var(--text);cursor:pointer;font-size:13px;font-weight:600;text-align:left;font-family:inherit}
.group-head:hover{background:var(--panel2)}
.chevron{color:var(--muted);font-size:10px;transition:transform .15s;display:inline-block}
.group.collapsed .chevron{transform:rotate(-90deg)}
.group-body .test-row{border-top:1px solid var(--border)}
/* rows */
.test-row{display:block;width:100%;padding:9px 14px;background:transparent;border:none;color:var(--text);cursor:pointer;text-align:left;font-family:inherit}
.test-row:hover{background:var(--panel2)}
.test-row-main{display:flex;align-items:center;gap:11px}
.row-title{font-weight:600;font-size:14px}
.row-sub{color:var(--muted);font-size:12px}
.row-right{margin-left:auto;display:flex;align-items:center;gap:12px;flex-shrink:0}
.duration{color:var(--muted);font-size:12px;white-space:nowrap}
.shots{color:var(--muted);font-size:12px;display:inline-flex;gap:4px;align-items:center;white-space:nowrap}
.shots svg{vertical-align:-2px}
/* detail */
.back{display:inline-flex;margin-bottom:14px}
.test-detail{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:20px 22px}
.detail-head{display:flex;align-items:baseline;gap:12px;flex-wrap:wrap}
.detail-title{font-size:18px;font-weight:600;margin:0}
.tabs{display:flex;gap:6px;margin:18px 0 0;flex-wrap:wrap}
.tab{padding:5px 13px;border:1px solid var(--border);border-radius:6px;background:transparent;color:var(--muted);cursor:pointer;font-size:12px;display:inline-flex;gap:6px;align-items:center;font-family:inherit}
.tab:hover{border-color:var(--muted);color:var(--text)}
.tab.active{background:var(--panel2);color:var(--text);border-color:var(--accent)}
.section{margin-top:22px}
.section-head{display:flex;align-items:center;gap:8px;font-size:14px;font-weight:600;user-select:none;margin-bottom:10px}
.section-head .chevron{font-size:9px}
pre.error{background:#161b22;border:1px solid var(--border);border-left:3px solid var(--red);border-radius:6px;padding:12px 14px;overflow:auto;color:#f0f6fc;font:12px/1.6 ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;white-space:pre-wrap;word-break:break-word;margin:0}
.step{display:flex;align-items:center;gap:11px;padding:6px 8px;border-radius:6px;border-bottom:1px solid var(--border)}
.step:last-child{border-bottom:none}
.step:hover{background:var(--panel2)}
.step .idx{color:var(--muted);font-size:11px;width:26px;flex-shrink:0;text-align:right;font-variant-numeric:tabular-nums}
.step-thumb{width:64px;height:48px;object-fit:cover;border:1px solid var(--border);border-radius:4px;background:#000;cursor:zoom-in;flex-shrink:0}
.step-label{font-size:13px}
.step-ms{color:var(--muted);font-size:12px;margin-left:auto;white-space:nowrap;font-variant-numeric:tabular-nums}
.filmstrip{display:flex;gap:10px;overflow-x:auto;padding-bottom:8px}
.filmstrip img{height:120px;border:1px solid var(--border);border-radius:6px;cursor:zoom-in;background:#000;flex-shrink:0}
.logs{display:flex;gap:6px;flex-wrap:wrap}
.logs a{padding:5px 12px;border:1px solid var(--border);border-radius:6px;background:transparent;color:var(--accent);text-decoration:none;font-size:12px;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
.logs a:hover{border-color:var(--accent)}
/* speedboard */
.speedboard{margin-top:6px}
.speed-row{display:flex;align-items:center;gap:14px;padding:9px 4px;border-bottom:1px solid var(--border)}
.speed-row:last-child{border-bottom:none}
.speed-bar{height:14px;background:var(--accent);border-radius:3px;opacity:.8;flex-shrink:0;min-width:2px}
.speed-title{font-size:13px;font-weight:600;color:var(--text);text-decoration:none}
.speed-title:hover{color:var(--accent)}
.speed-row .row-sub{flex-shrink:0}
/* image viewer */
.viewer{position:fixed;inset:0;background:rgba(1,4,9,.88);display:none;z-index:100;align-items:center;justify-content:center;flex-direction:column}
.viewer.open{display:flex}
.viewer img{max-width:92vw;max-height:80vh;border:1px solid var(--border);border-radius:4px;background:#000;box-shadow:0 8px 40px rgba(0,0,0,.6)}
.viewer-cap{color:var(--muted);font-size:12px;margin-top:10px;max-width:92vw;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.viewer-nav{position:fixed;top:50%;transform:translateY(-50%);background:var(--panel2);border:1px solid var(--border);color:var(--text);width:46px;height:46px;border-radius:50%;cursor:pointer;font-size:20px;display:flex;align-items:center;justify-content:center}
.viewer-nav:hover{border-color:var(--accent);color:var(--accent)}
.viewer-nav.prev{left:18px} .viewer-nav.next{right:18px}
.viewer-close{position:fixed;top:14px;right:18px;background:transparent;border:none;color:var(--muted);font-size:26px;cursor:pointer;line-height:1}
.viewer-close:hover{color:var(--text)}
</style>
</head>
<body>
<header class="toolbar">
  <div class="toolbar-inner">
    <div class="search-wrap">
      <svg class="mag" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5 14 14"/></svg>
      <input id="search" type="text" placeholder="Search tests" autocomplete="off" spellcheck="false">
    </div>
    <nav class="chips" id="chips"></nav>
    <div class="toolbar-right">
      <a class="link" id="speedboard-link" href="#?speedboard">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><circle cx="8" cy="8" r="6.2"/><path d="M8 4.5V8l2.4 1.6"/></svg>
        Speedboard
      </a>
    </div>
  </div>
</header>
<div class="meta" id="meta"></div>
<main id="app"></main>

<div class="viewer" id="viewer" role="dialog" aria-modal="true">
  <button class="viewer-close" id="viewer-close" aria-label="Close">&times;</button>
  <button class="viewer-nav prev" id="viewer-prev" aria-label="Previous">&#8249;</button>
  <img id="viewer-img" alt="screenshot">
  <div class="viewer-cap" id="viewer-cap"></div>
  <button class="viewer-nav next" id="viewer-next" aria-label="Next">&#8250;</button>
</div>

<script>
'use strict';
const DATA = __DATA_JSON__;
const TESTS = DATA.tests || [];
const EXIST = new Set();
for (const t of TESTS) for (const s of (t._shots || [])) EXIST.add(s);

const STATUSES = ['passed', 'failed', 'skipped'];
const ICONS = {passed: '\u2713', failed: '\u2717', skipped: '\u2298'};

const $ = (sel, root) => (root || document).querySelector(sel);

function esc(v) {
  return String(v == null ? '' : v).replace(/[&<>"']/g, c => (
    {'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'}[c]
  ));
}

function glyph(st) { return '<span class="glyph ' + esc(st) + '">' + (ICONS[st] || '?') + '</span>'; }

const FILM_ICON = '<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true"><rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M4 2.5v11M12 2.5v11M1.5 6h2.5M1.5 10h2.5M12 6h2.5M12 10h2.5"/></svg>';

function fmtDur(s) {
  if (s == null || isNaN(s)) return '';
  if (s < 1) return Math.max(1, Math.round(s * 1000)) + 'ms';
  if (s < 60) return s.toFixed(1) + 's';
  const m = Math.floor(s / 60);
  return m + 'm ' + (s - m * 60).toFixed(1) + 's';
}
function fmtMs(ms) {
  if (ms == null || isNaN(ms)) return '';
  if (ms < 1000) return Math.round(ms) + 'ms';
  if (ms < 60000) return (ms / 1000).toFixed(1) + 's';
  const m = Math.floor(ms / 60000);
  return m + 'm ' + ((ms - m * 60000) / 1000).toFixed(1) + 's';
}

function setHash(params) {
  const usp = new URLSearchParams();
  for (const k of Object.keys(params)) {
    const v = params[k];
    if (v != null && v !== '') usp.set(k, v);
  }
  const h = '#?' + usp.toString();
  if (location.hash !== h) location.hash = h;
  else render();
}
function parseHash() {
  const h = location.hash.replace(/^#\??/, '');
  const usp = new URLSearchParams(h);
  const q = usp.get('q') || '';
  let status = null;
  const m = q.match(/^s:([^&|]+(?:\|[^&|]+)*)$/);
  if (m) status = m[1].split('|');
  return {q: q, status: status, speedboard: usp.has('speedboard'), testId: usp.get('testId')};
}
function testHref(id) { return '#?testId=' + encodeURIComponent(id); }

let searchTerm = '';
let collapsedFiles = new Set();
let activeAttempt = 0;
let currentList = [];
let currentIdx = 0;

function counts() {
  const c = {all: TESTS.length, passed: 0, failed: 0, skipped: 0};
  for (const t of TESTS) if (c[t.status] != null) c[t.status]++;
  return c;
}
function filtered() {
  const st = parseHash().status;
  const q = searchTerm.trim().toLowerCase();
  return TESTS.filter(t => {
    if (st && !st.includes(t.status)) return false;
    if (q && t.title.toLowerCase().indexOf(q) === -1) return false;
    return true;
  });
}

function renderChips() {
  const c = counts();
  const st = parseHash().status || [];
  const chip = (key, label, icon, count, href, active) =>
    '<a class="chip' + (active ? ' active' : '') + '" href="' + href + '">' +
    (icon ? '<span class="glyph ' + key + '">' + icon + '</span>' : '') +
    label + '<span class="badge">' + count + '</span></a>';
  let html = chip('all', 'All', '', c.all, '#?', st.length === 0);
  for (const s of STATUSES) {
    html += chip(s, s.charAt(0).toUpperCase() + s.slice(1), ICONS[s], c[s],
                 '#?q=s:' + s, st.indexOf(s) !== -1);
  }
  $('#chips').innerHTML = html;
}

function renderMeta() {
  const parts = [];
  if (DATA.generated_at) parts.push('Generated: <b>' + esc(DATA.generated_at) + '</b>');
  if (DATA.total_time_s != null) parts.push('Total time: <b>' + fmtDur(DATA.total_time_s) + '</b>');
  if (DATA.firmware) parts.push('Firmware: <b>' + esc(DATA.firmware) + '</b>');
  if (DATA.commit) parts.push('Commit: <b>' + esc(DATA.commit) + '</b>');
  $('#meta').innerHTML = parts.join('<span style="opacity:.45">\u00b7</span>');
}

function renderOverview(app) {
  const list = filtered();
  const groups = new Map();
  for (const t of list) {
    const key = t.file || '(unknown)';
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(t);
  }
  let html = '';
  if (!list.length) {
    html = '<div class="empty">No tests match the current filters.</div>';
  } else {
    for (const [file, tests] of groups) {
      const collapsed = collapsedFiles.has(file);
      html += '<section class="group' + (collapsed ? ' collapsed' : '') + '">' +
        '<button class="group-head" data-file="' + esc(file) + '">' +
        '<span class="chevron">\u25bc</span><span>' + esc(file) + '</span>' +
        '<span class="badge">' + tests.length + '</span></button><div class="group-body">';
      for (const t of tests) html += testRow(t);
      html += '</div></section>';
    }
  }
  app.innerHTML = html;
  app.querySelectorAll('.group-head').forEach(btn => {
    btn.addEventListener('click', () => {
      const file = btn.dataset.file;
      if (collapsedFiles.has(file)) collapsedFiles.delete(file);
      else collapsedFiles.add(file);
      btn.closest('.group').classList.toggle('collapsed');
    });
  });
  app.querySelectorAll('.test-row').forEach(row => {
    row.addEventListener('click', () => {
      activeAttempt = 0;
      setHash({q: parseHash().q, testId: row.dataset.id});
    });
  });
}

function testRow(t) {
  const shots = (t._shots || []).length;
  const film = shots ? '<span class="shots" title="' + shots + ' screenshot' + (shots === 1 ? '' : 's') + '">' + FILM_ICON + shots + '</span>' : '';
  return '<button class="test-row" data-id="' + esc(t.id) + '">' +
    '<div class="test-row-main">' + glyph(t.status) +
    '<div><div class="row-title">' + esc(t.title) + '</div>' +
    '<div class="row-sub">' + esc(t.file) + ':' + esc(t.line) + '</div></div>' +
    '<div class="row-right">' + film + '<span class="duration">' + fmtDur(t.duration_s) + '</span></div>' +
    '</div></button>';
}

function renderDetail(app, id) {
  const t = TESTS.find(x => x.id === id);
  if (!t) { renderOverview(app); return; }
  const attempts = (t.attempts && t.attempts.length) ? t.attempts :
    [{status: t.status, duration_s: t.duration_s, steps: []}];
  const active = activeAttempt >= attempts.length ? 0 : activeAttempt;
  const att = attempts[active];
  const q = parseHash().q;
  const backHref = q ? '#?q=' + encodeURIComponent(q) : '#?';
  let html = '<a class="link back" href="' + backHref + '">\u2190 All tests</a>';
  html += '<div class="test-detail">';
  html += '<div class="detail-head">' + glyph(t.status) +
    '<h2 class="detail-title">' + esc(t.title) + '</h2>' +
    '<span class="row-sub">' + esc(t.file) + ':' + esc(t.line) + '</span>' +
    '<span class="duration" style="margin-left:auto">' + fmtDur(att.duration_s != null ? att.duration_s : t.duration_s) + '</span></div>';
  if (attempts.length > 1) {
    html += '<div class="tabs">';
    attempts.forEach((a, i) => {
      const label = i === 0 ? 'Run 1' : 'Retry #' + (i + 1);
      html += '<button class="tab' + (i === active ? ' active' : '') + '" data-att="' + i + '">' +
        glyph(a.status) + label + ' \u00b7 ' + fmtDur(a.duration_s) + '</button>';
    });
    html += '</div>';
  }
  const err = att.error || t.error;
  if (t.status === 'failed' && err) {
    html += '<div class="section"><div class="section-head">Errors <span class="chevron">\u25bc</span></div>' +
      '<pre class="error">' + esc(err) + '</pre></div>';
  }
  const steps = att.steps || [];
  if (steps.length) {
    html += '<div class="section"><div class="section-head">Test Steps <span class="badge">' + steps.length + '</span> <span class="chevron">\u25bc</span></div>';
    steps.forEach((s, i) => {
      const hasPng = s.png && EXIST.has(s.png);
      html += '<div class="step"><span class="idx">' + i + '</span>';
      if (hasPng) html += '<img class="step-thumb" src="' + esc(s.png) + '" alt="' + esc(s.label) + '" loading="lazy" data-png="' + esc(s.png) + '">';
      html += '<span class="step-label">' + esc(s.label) + '</span>' +
        '<span class="step-ms">' + fmtMs(s.ms) + '</span></div>';
    });
    html += '</div>';
  }
  const shots = t._shots || [];
  if (shots.length) {
    html += '<div class="section"><div class="section-head">Screenshots <span class="badge">' + shots.length + '</span> <span class="chevron">\u25bc</span></div>' +
      '<div class="filmstrip">';
    for (const p of shots) {
      html += '<img src="' + esc(p) + '" alt="' + esc(p) + '" loading="lazy" data-png="' + esc(p) + '">';
    }
    html += '</div></div>';
  }
  const sharedLogs = (DATA.logs || []).map(n => 'logs/' + n);
  const logs = (t._logs && t._logs.length) ? t._logs : sharedLogs;
  if (logs.length) {
    html += '<div class="section"><div class="section-head">Logs <span class="chevron">\u25bc</span></div><div class="logs">';
    for (const l of logs) html += '<a href="' + esc(l) + '" target="_blank" rel="noopener">' + esc(l) + '</a>';
    html += '</div></div>';
  }
  html += '</div>';
  app.innerHTML = html;
  app.querySelectorAll('.tab').forEach(tab => {
    tab.addEventListener('click', () => {
      activeAttempt = +tab.dataset.att;
      renderDetail(app, id);
    });
  });
  app.querySelectorAll('[data-png]').forEach(img => {
    img.addEventListener('click', () => {
      const idx = shots.indexOf(img.dataset.png);
      openViewer(shots, idx === -1 ? 0 : idx);
    });
  });
}

function renderSpeedboard(app) {
  const list = TESTS.slice().sort((a, b) => (b.duration_s || 0) - (a.duration_s || 0));
  let html = '<div class="section-head">Slowest Tests</div>';
  if (!list.length) html += '<div class="empty">No tests.</div>';
  else {
    const max = Math.max.apply(null, list.map(t => t.duration_s || 0).concat([1]));
    html += '<div class="speedboard">';
    for (const t of list) {
      const pct = (t.duration_s || 0) / max;
      html += '<div class="speed-row">' +
        '<span class="speed-bar" style="width:' + Math.max(2, Math.round(pct * 160)) + 'px"></span>' +
        '<a class="speed-title" href="' + testHref(t.id) + '">' + esc(t.title) + '</a>' +
        '<span class="row-sub">' + esc(t.file) + ':' + esc(t.line) + '</span>' +
        '<span class="duration" style="margin-left:auto">' + fmtDur(t.duration_s) + '</span></div>';
    }
    html += '</div>';
  }
  app.innerHTML = html;
}

function render() {
  renderChips();
  const {speedboard, testId} = parseHash();
  const app = $('#app');
  if (testId) renderDetail(app, testId);
  else if (speedboard) renderSpeedboard(app);
  else renderOverview(app);
}

function viewerOpen() { return $('#viewer').classList.contains('open'); }
function updateViewer() {
  $('#viewer-img').src = currentList[currentIdx];
  $('#viewer-cap').textContent = (currentIdx + 1) + ' / ' + currentList.length + ' \u2014 ' + currentList[currentIdx];
  $('#viewer-prev').style.visibility = currentIdx > 0 ? 'visible' : 'hidden';
  $('#viewer-next').style.visibility = currentIdx < currentList.length - 1 ? 'visible' : 'hidden';
}
function openViewer(list, idx) {
  if (!list || !list.length) return;
  currentList = list;
  currentIdx = Math.max(0, Math.min(idx, list.length - 1));
  updateViewer();
  $('#viewer').classList.add('open');
  document.body.style.overflow = 'hidden';
}
function closeViewer() {
  $('#viewer').classList.remove('open');
  document.body.style.overflow = '';
}

$('#viewer-close').addEventListener('click', closeViewer);
$('#viewer').addEventListener('click', e => { if (e.target === $('#viewer')) closeViewer(); });
$('#viewer-prev').addEventListener('click', () => { if (currentIdx > 0) { currentIdx--; updateViewer(); } });
$('#viewer-next').addEventListener('click', () => { if (currentIdx < currentList.length - 1) { currentIdx++; updateViewer(); } });

const searchInput = $('#search');
searchInput.addEventListener('input', () => { searchTerm = searchInput.value; render(); });
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') closeViewer();
  if (e.key === '/' && document.activeElement !== searchInput && !viewerOpen()) {
    e.preventDefault();
    searchInput.focus();
  }
});
window.addEventListener('hashchange', render);
renderMeta();
render();
</script>
</body>
</html>
"""


def copy_screenshots(data, out_dir):
    """Copy each test's PNGs from <out>/../screenshots into out/screenshots/<test>/.

    Order comes from the recorder's index.txt when present, otherwise from a
    sorted glob of *.png. Returns a dict test_id -> list of destination-relative
    png paths (only files that actually got copied).
    """
    candidates = [out_dir.parent / "screenshots", out_dir.resolve().parent / "screenshots"]
    source_root = next((p for p in candidates if p.is_dir()), candidates[0])
    shots_by_test = {}
    for t in data.get("tests", []):
        test_dir = None
        for att in t.get("attempts") or []:
            for step in att.get("steps") or []:
                png = step.get("png")
                if png:
                    parts = Path(png).parts
                    if len(parts) >= 2 and parts[0] == "screenshots":
                        test_dir = parts[1]
                        break
            if test_dir:
                break
        if not test_dir:
            test_dir = t.get("title", "test")
        src_dir = source_root / test_dir
        names = []
        index_file = src_dir / "index.txt"
        if index_file.is_file():
            for line in index_file.read_text(encoding="utf-8", errors="replace").splitlines():
                tok = line.strip().split()
                if tok:
                    names.append(tok[0])
        if src_dir.is_dir():
            for p in sorted(src_dir.glob("*.png")):
                if p.name not in names:
                    names.append(p.name)
        shots = []
        for name in names:
            src = src_dir / name
            if not src.is_file():
                continue
            dest = out_dir / "screenshots" / test_dir / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dest)
            shots.append(f"screenshots/{test_dir}/{name}")
        shots_by_test[t.get("id", test_dir)] = shots
    return shots_by_test


def copy_logs(firmware_dir, out_dir):
    """Best-effort copy of firmware logs into out/logs/. Returns list of copied names."""
    fw = Path(firmware_dir)
    logs = []
    for rel, name in LOG_SOURCES:
        if name in logs:
            continue
        src = fw / rel
        if src.is_file():
            dest = out_dir / "logs" / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dest)
            logs.append(name)
    return logs


def generate(results_path, out_dir, firmware_dir):
    if not results_path.is_file():
        print(f"error: results file not found: {results_path}", file=sys.stderr)
        print("  run the e2e suite first, or point --results at an existing results.json", file=sys.stderr)
        return 1

    with results_path.open(encoding="utf-8") as fh:
        data = json.load(fh)

    out_dir.mkdir(parents=True, exist_ok=True)

    fw = Path(firmware_dir)
    bookshelf_src = _find_log_source(fw, "bookshelf.log")
    api_src = _api_log_path(out_dir)

    shots_by_test = copy_screenshots(data, out_dir)
    logs = copy_logs(firmware_dir, out_dir)

    for t in data.get("tests", []):
        t["_shots"] = shots_by_test.get(t.get("id", ""), [])
        scoped = _write_scoped_logs(t, bookshelf_src, api_src, out_dir)
        t["_logs"] = (scoped or []) + [f"logs/{name}" for name in logs]

    payload = {
        "generated_at": data.get("generated_at", ""),
        "total_time_s": data.get("total_time_s", 0),
        "firmware": data.get("firmware", ""),
        "commit": data.get("commit", ""),
        "tests": data.get("tests", []),
        "logs": logs,
    }
    data_json = json.dumps(payload, ensure_ascii=True)
    data_json = data_json.replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")

    html = HTML_TEMPLATE.replace("__DATA_JSON__", data_json)
    index = out_dir / "index.html"
    index.write_text(html, encoding="utf-8")

    n_tests = len(payload["tests"])
    n_shots = sum(len(v) for v in shots_by_test.values())
    print(f"wrote {index}")
    print(f"  {n_tests} tests, {n_shots} screenshots copied, {len(logs)} log file(s) copied")
    return 0


def serve(out_dir, port):
    class QuietHandler(SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(out_dir), **kwargs)

        def log_message(self, fmt, *args):  # keep the console clean
            pass

    host, port = "127.0.0.1", port
    httpd = HTTPServer((host, port), QuietHandler)
    print(f"Serving report at http://{host}:{port}/  (Ctrl+C to stop)")
    print(f"  directory: {out_dir.resolve()}")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped")
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="gen_report.py",
        description="Generate (and optionally serve) the Playwright-style HTML e2e report.")
    parser.add_argument("--results", default=str(DEFAULT_RESULTS),
                        help=f"path to results.json (default: {DEFAULT_RESULTS})")
    parser.add_argument("--out", default=str(DEFAULT_OUT),
                        help=f"output directory (default: {DEFAULT_OUT})")
    parser.add_argument("--firmware-dir", default=str(DEFAULT_FIRMWARE),
                        help=f"firmware dir for log copying (default: {DEFAULT_FIRMWARE})")
    parser.add_argument("--serve", action="store_true",
                        help="generate, then serve the report directory over HTTP")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT,
                        help=f"port for --serve (default: {DEFAULT_PORT})")
    args = parser.parse_args(argv)

    results_path = Path(args.results)
    out_dir = Path(args.out)
    rc = generate(results_path, out_dir, args.firmware_dir)
    if rc:
        return rc
    if args.serve:
        return serve(out_dir, args.port)
    return 0


if __name__ == "__main__":
    sys.exit(main())
