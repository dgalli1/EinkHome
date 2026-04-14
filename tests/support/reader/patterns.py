"""App-name constants and compiled regex pattern groups for reader log matching."""

from __future__ import annotations

import re

# ---------------------------------------------------------------------------
# App name constants
# ---------------------------------------------------------------------------

BOOK_PATH = "/mnt/ext1/books/bgipc.epub"
BOOK_TITLE = "Beej's Guide to Interprocess Communication"
HOME_APP = "bookshelf.app"
EXPLORER_APP = "explorer.app"
READER_APP = "/ebrmain/bin/eink-reader.app"
READER_DB_NAME = "eink-reader.app"
CONTROL_PANEL_APP = "control_panel_mgr.app"
BOOK_INFO_APP = "book_info.app"
HOME_SURFACE_APPS: tuple[str, ...] = (HOME_APP, EXPLORER_APP, CONTROL_PANEL_APP)
EXIT_CONTROL_APPS: tuple[str, ...] = HOME_SURFACE_APPS + (BOOK_INFO_APP,)

DEFAULT_CRASH_MARKERS: tuple[str, ...] = (
    "Exec format error",
    "wrong configuration: missing shared temp framebuffer",
    "core dumped",
    "Segmentation fault",
    "task_died",
    "exited with signal",
    "exec: not found",
)

# ---------------------------------------------------------------------------
# Regex pattern groups used to classify monitor.log activity
# ---------------------------------------------------------------------------

OPEN_BOOK_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"PERF_TESTING: Book opened \[(?P<path>[^\]]+)\]"),
    re.compile(r"BookReady\((?P<path>[^)]+)\)"),
    re.compile(r"set_subtask_info: .* book='(?P<path>[^']+)'"),
    re.compile(r"file://(?P<path>/mnt/ext1/\S+)"),
)

OPEN_ACTIVITY_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("BookReady", re.compile(r"BookReady\(")),
    ("PERF_TESTING: Book opened", re.compile(r"PERF_TESTING: Book opened \[")),
    ("set_subtask_info book", re.compile(r"set_subtask_info: .* book='[^']+'")),
    (
        "reader app launch requested",
        re.compile(r"Starting app: /ebrmain/bin/eink-reader\.app"),
    ),
    (
        "reader task started",
        re.compile(r"Starting task - /ebrmain/bin/eink-reader\.app"),
    ),
    (
        "reader file URL",
        re.compile(r"file:///mnt/ext1/\S+"),
    ),
    (
        "reader foregrounded",
        re.compile(r"ReaderController::onForeground\(\) (?:begin|end)"),
    ),
    (
        "switch to reader",
        re.compile(
            r"switch tasks .*eink-cache-reader\.app\(\d+\)"
            r" -> .*(?:/)?eink-reader\.app\(\d+\)"
        ),
    ),
)

MENU_ACTIVITY_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("kMenu action", re.compile(r"action\.action_ \[kMenu\]")),
    ("QML current page changed", re.compile(r"qml: \*\*\* onCurrent_pageChanged")),
    (
        "hidden reader task registered",
        re.compile(
            r'register_task "(?:/ebrmain/bin/)?eink-reader\.app":'
            r" pid = \d+; flags = 25"
        ),
    ),
    (
        "hidden reader task started",
        re.compile(r"Starting task - /ebrmain/bin/eink-reader\.app"),
    ),
    (
        "reader app launch requested",
        re.compile(r"Starting app: /ebrmain/bin/eink-reader\.app"),
    ),
)

PAGE_ACTIVITY_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "page repaint",
        re.compile(
            r"PERF_TESTING END: ScreenManagerObj::paint time = \[[^\]]+\],"
            r" pid = \[\d+\], filename = \[[^\]]+\]"
        ),
    ),
    ("QML current page changed", re.compile(r"qml: \*\*\* onCurrent_pageChanged")),
    ("page render", re.compile(r"Page render time\s+\d+\s+ms")),
)

EXIT_ACTIVITY_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "reader to bookshelf switch",
        re.compile(
            r"switch tasks .*(?:/)?eink-reader\.app\(\d+\)"
            r" -> bookshelf\.app\(\d+\)"
        ),
    ),
    ("explorer launched", re.compile(r'register_task "explorer\.app"')),
    (
        "explorer foreground parameters",
        re.compile(r"set_task_parameters: \d+ appname='explorer\.app'"),
    ),
)
