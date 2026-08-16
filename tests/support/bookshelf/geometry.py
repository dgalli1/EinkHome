"""Bookshelf UI geometry — coordinate calculations matching bookshelf.c layout.

All constants mirror the ``#define`` layout values in ``bookshelf/bookshelf.c``.
The ``BookshelfGeometry`` dataclass computes tap-target centres from the
framebuffer dimensions and the system panel height so tests never hard-code
pixel positions.
"""

from __future__ import annotations

from dataclasses import dataclass

# ── layout constants (must match bookshelf.c) ──────────────────────────
TOP_BAR_H = 128
SEARCH_ROW_H = 88
SEARCH_HISTORY_ROW_H = 96  # history-term rows on the Search sub-page
TAB_ROW_H = 0  # tab row removed; downloads via top-bar icon
PAGER_H = 96
BOTTOM_RESERVED = 0
COLS = 3
ROWS = 2
PAGESIZE = COLS * ROWS  # 6
CELL_MAX_H = 600
CELL_MAX_W = 420
CELL_MIN_H = 280
CELL_MIN_W = 280

# ── More overlay item indices (right-anchored drawer) ─────────────────
MORE_GROUP = 0
MORE_SORT = 1
MORE_DOWNLOAD_ALL = 2
MORE_SETTINGS = 3
MORE_APPS = 4

# ── Settings overlay layout (full-screen page) ────────────────────────
OVERLAY_HEADER_H = TOP_BAR_H  # overlay header == top bar height (Back chevron + title)
SETTINGS_ROW_H = 120
SETTINGS_BTN_H = 96
SETTINGS_ROW1_Y = 112

# ── Context (long-press) menu layout ──────────────────────────────────
CTX_ITEM_H = 96
CTX_TITLE_H = 72
CTX_PAD = 24
# ── Launcher overlay layout (must match bookshelf.c) ─────────────────
LAUNCHER_COLS = 3
LAUNCHER_GROUP_H = 64
LAUNCHER_CELL_H = 232
LAUNCHER_ICON_SZ = 120
LAUNCHER_MARGIN = 16

__all__ = [
    "BOTTOM_RESERVED",
    "CELL_MAX_H",
    "CELL_MAX_W",
    "CELL_MIN_H",
    "CELL_MIN_W",
    "COLS",
    "CTX_ITEM_H",
    "CTX_PAD",
    "CTX_TITLE_H",
    "LAUNCHER_CELL_H",
    "LAUNCHER_COLS",
    "LAUNCHER_GROUP_H",
    "LAUNCHER_ICON_SZ",
    "LAUNCHER_MARGIN",
    "MORE_APPS",
    "MORE_DOWNLOAD_ALL",
    "MORE_GROUP",
    "MORE_SETTINGS",
    "MORE_SORT",
    "OVERLAY_HEADER_H",
    "PAGER_H",
    "PAGESIZE",
    "SEARCH_HISTORY_ROW_H",
    "SEARCH_ROW_H",
    "SETTINGS_BTN_H",
    "SETTINGS_ROW1_Y",
    "SETTINGS_ROW_H",
    "TOP_BAR_H",
    "BookshelfGeometry",
]


@dataclass(frozen=True, slots=True)
class BookshelfGeometry:
    """Coordinate calculator for the bookshelf UI layout.

    Constructed once per test module from the live framebuffer dimensions
    and the ``panel_h`` value parsed from the bookshelf log.
    """

    screen_w: int
    screen_h: int
    panel_h: int

    def content_bottom(self) -> int:
        """Bottom edge of the app content area (top of the system panel)."""
        return self.screen_h - self.panel_h

    # ── top bar ────────────────────────────────────────────────────────

    def home_button_center(self) -> tuple[int, int]:
        """Centre of the 96x96 home (house) button, left side of top bar."""
        return (8 + 48, TOP_BAR_H // 2)

    def menu_button_center(self) -> tuple[int, int]:
        """Centre of the 96x96 More (hamburger) button, right side of top
        bar."""
        return (self.screen_w - 8 - 48, TOP_BAR_H // 2)

    # ── search (top-bar icon + Search sub-page) ───────────────────────

    def search_icon_center(self) -> tuple[int, int]:
        """Centre of the 96x96 magnifying-glass icon in the top bar,
        left of the layout-switch icon."""
        if self.screen_w == 0:
            return (0, 0)
        x = self.screen_w - 8 - 96 - 3 * 96 + 48
        return (x, TOP_BAR_H // 2)

    def layout_icon_center(self) -> tuple[int, int]:
        """Centre of the 96x96 layout-switch icon (grid/list), between
        the search and sync icons."""
        if self.screen_w == 0:
            return (0, 0)
        x = self.screen_w - 8 - 96 - 2 * 96 + 48
        return (x, TOP_BAR_H // 2)
    def search_input_center(self) -> tuple[int, int]:
        """Centre of the search text box on the Search sub-page input
        row (the row sits directly below the top bar)."""
        row_top = TOP_BAR_H
        return (self.screen_w // 2, row_top + SEARCH_ROW_H // 2)

    def search_history_center(self, index: int) -> tuple[int, int]:
        """Centre of history-term row *index* (0-based within the
        current page) on the Search sub-page."""
        row_top = TOP_BAR_H + SEARCH_ROW_H
        return (
            self.screen_w // 2,
            row_top + index * SEARCH_HISTORY_ROW_H + SEARCH_HISTORY_ROW_H // 2,
        )

    def suggestion_row_center(self, index: int) -> tuple[int, int]:
        """Centre of live suggestion row *index* in the band above the
        on-screen keyboard (same row layout as the history list)."""
        row_top = TOP_BAR_H + SEARCH_ROW_H
        return (
            self.screen_w // 2,
            row_top + index * SEARCH_HISTORY_ROW_H + SEARCH_HISTORY_ROW_H // 2,
        )

    # ── book grid ──────────────────────────────────────────────────────

    def _grid_params(self) -> tuple[int, int, int]:
        """Return ``(grid_top, cell_w, cell_h)`` matching ``grid_geom()``."""
        top = TOP_BAR_H + TAB_ROW_H
        bot = self.content_bottom() - PAGER_H
        avail_w = self.screen_w - 16
        avail_h = bot - top - 8
        cell_w = max(CELL_MIN_W, min(avail_w // COLS, CELL_MAX_W))
        cell_h = max(CELL_MIN_H, min(avail_h // ROWS, CELL_MAX_H))
        return top, cell_w, cell_h

    def book_tile_center(self, index: int) -> tuple[int, int]:
        """Centre of tile *index* (0-based within the current page)."""
        top, cell_w, cell_h = self._grid_params()
        row = index // COLS
        col = index % COLS
        tx = 8 + col * cell_w
        ty = top + 4 + row * cell_h
        tw = cell_w - 8
        th = cell_h - 6
        return (tx + tw // 2, ty + th // 2)

    # ── pager ──────────────────────────────────────────────────────────

    def pager_y(self) -> int:
        """Top edge of the pager row (directly above the bottom panel)."""
        return self.content_bottom() - PAGER_H

    def pager_prev_center(self) -> tuple[int, int]:
        """Centre of the 96x64 prev-page button."""
        return (12 + 48, self.pager_y() + PAGER_H // 2)

    def pager_next_center(self) -> tuple[int, int]:
        """Centre of the 96x64 next-page button."""
        return (self.screen_w - 12 - 48, self.pager_y() + PAGER_H // 2)

    def pager_first_center(self) -> tuple[int, int]:
        """Centre of the 96x64 first-page button (second from left)."""
        return (116 + 48, self.pager_y() + PAGER_H // 2)

    def pager_last_center(self) -> tuple[int, int]:
        """Centre of the 96x64 last-page button (second from right)."""
        return (self.screen_w - 212 + 48, self.pager_y() + PAGER_H // 2)

    # ── More overlay (right-anchored, 75 % width) ─────────────────────

    def more_overlay_left(self) -> int:
        """X coordinate of the More panel's left edge."""
        return self.screen_w - self.screen_w * 3 // 4

    def more_item_center(self, index: int) -> tuple[int, int]:
        """Centre of More-overlay item *index* (y0=96, item_h=88)."""
        pw = self.screen_w * 3 // 4
        px = self.screen_w - pw
        return (px + pw // 2, 96 + index * 88 + 44)

    def outside_more_overlay(self) -> tuple[int, int]:
        """A point guaranteed to be left of the right-anchored More panel."""
        return (4, self.screen_h // 2)

    # ── Settings overlay (full-screen page) ───────────────────────────

    def settings_row_center(self, row: int) -> tuple[int, int]:
        """Centre of settings row *row* (0=API host, 1=API key, 2=reader,
        3=download folder, 4=install as system app)."""
        y = SETTINGS_ROW1_Y + row * SETTINGS_ROW_H
        return (self.screen_w // 2, y + (SETTINGS_ROW_H - 12) // 2)

    def settings_sysapp_center(self) -> tuple[int, int]:
        """Centre of the Install-as-system-app toggle row (row 4)."""
        return self.settings_row_center(4)

    def settings_save_center(self) -> tuple[int, int]:
        """Centre of the Save & apply button."""
        y = SETTINGS_ROW1_Y + 5 * SETTINGS_ROW_H + 24
        return (self.screen_w // 2, y + (SETTINGS_BTN_H - 12) // 2)

    def settings_back_center(self) -> tuple[int, int]:
        """Centre of the header Back chevron (same as the search page's)."""
        return (8 + 48, TOP_BAR_H // 2)

    def settings_logs_center(self) -> tuple[int, int]:
        """Centre of the Show logs button (below Save)."""
        y = SETTINGS_ROW1_Y + 5 * SETTINGS_ROW_H + 24 + SETTINGS_BTN_H
        return (self.screen_w // 2, y + (SETTINGS_BTN_H - 12) // 2)

    def settings_licenses_center(self) -> tuple[int, int]:
        """Centre of the Licenses button (below Show logs)."""
        y = SETTINGS_ROW1_Y + 5 * SETTINGS_ROW_H + 24 + 2 * SETTINGS_BTN_H
        return (self.screen_w // 2, y + (SETTINGS_BTN_H - 12) // 2)

    # ── third-party licenses viewer (full-screen) ─────────────────────
    # The list body starts at the shared overlay header (the top bar's
    # own 96 px, BS_OVERLAY_HEADER_H) plus 16 — i.e. BS_LIC_LIST_TOP in
    # bs_core.h — with BS_LIC_LIST_H (110) per row.

    def licenses_list_row_center(self, index: int) -> tuple[int, int]:
        """Centre of license-list row *index* (0-based)."""
        y = 96 + 16 + index * 110
        return (self.screen_w // 2, y + (110 - 12) // 2)

    def licenses_back_center(self) -> tuple[int, int]:
        """Centre of the licenses viewer's Back chevron (same as settings)."""
        return (8 + 48, TOP_BAR_H // 2)

    # ── Log viewer (full-screen) ─────────────────────────────────────

    def log_back_center(self) -> tuple[int, int]:
        """Centre of the log viewer's Back chevron (same as the search
        page's)."""
        return (8 + 48, TOP_BAR_H // 2)

    # ── group / sort chooser sheets (centered source-chooser style) ────

    # App grid area starts just below the top bar (BS_TOP_BAR_H 96 +
    # BS_TOP_BAR_PAD 12); the group header band is the first 48 px.
    GROUP_GRID_TOP = 108
    GROUP_HEADER_H = 48

    def _chooser_py(self, n_rows: int) -> int:
        ph = 72 + n_rows * 96 + 24
        return (self.screen_h - ph) // 2

    def chooser_outside_point(self) -> tuple[int, int]:
        """A point guaranteed outside any centred chooser sheet."""
        return (self.screen_w - 4, self.screen_h // 2)

    def group_option_center(self, index: int, n_rows: int = 5) -> tuple[int, int]:
        """Centre of group-chooser option *index*: row 0 = All books, then
        Series / Author / Year / Genre (n_rows defaults to all five with
        the mock provider's data)."""
        py = self._chooser_py(n_rows)
        return (self.screen_w // 2, py + 84 + index * 96 + 48)

    def sort_option_center(self, index: int) -> tuple[int, int]:
        """Centre of sort-chooser option *index* (0..3: title/author/
        series/recent)."""
        py = self._chooser_py(4)
        return (self.screen_w // 2, py + 84 + index * 96 + 48)

    def group_header_center(self) -> tuple[int, int]:
        """Centre of the current page's dimension-group header band."""
        return (self.screen_w // 2, self.GROUP_GRID_TOP + self.GROUP_HEADER_H // 2)

    # ── top-bar right-corner buttons ──────────────────────────────────

    def sync_button_center(self) -> tuple[int, int]:
        """Centre of the 96x96 sync button, left of the More button.
        Runs a library sync."""
        return (self.screen_w - 152, TOP_BAR_H // 2)

    # ── context (long-press) menu ─────────────────────────────────────

    def context_item_center(self, item: int, n_items: int = 2) -> tuple[int, int]:
        """Centre of context-menu item *item* (0-based) in a sheet of
        *n_items* rows.  The app centres the sheet on the full logical
        screen (``context_geom``: ``py = (ScreenHeight() - ph) / 2``),
        not on the content area above the system strip."""
        pw = self.screen_w * 3 // 4
        ph = CTX_TITLE_H + n_items * CTX_ITEM_H + CTX_PAD
        px = (self.screen_w - pw) // 2
        py = (self.screen_h - ph) // 2
        iy = py + CTX_TITLE_H + item * CTX_ITEM_H
        return (px + pw // 2, iy + CTX_ITEM_H // 2)

    # ── firmware on-screen keyboard ───────────────────────────────────
    # The keyboard is drawn by the firmware in the guest's logical space.
    # The return key centre was measured at logical (963, 1269) and does
    # not move when the panel reserves fb_y_offset rows: the keyboard
    # keeps its bottom edge at ScreenHeight()-PanelHeight(), which the
    # scanout wrap presents at the physical window bottom either way.

    def keyboard_return_center(self) -> tuple[int, int]:
        """Centre of the on-screen keyboard's return/accept key."""
        return (963, 1269)

    # ── launcher overlay ──────────────────────────────────────────────

    def launcher_back_center(self) -> tuple[int, int]:
        """Centre of the launcher Back chevron (same as the search
        page's)."""
        return (8 + 48, TOP_BAR_H // 2)

    def launcher_app_center(self, index: int) -> tuple[int, int]:
        """Centre of launcher app cell *index* (0-based, row-major) at
        scroll 0: the launcher draws LAUNCHER_COLS columns of cells
        directly under the first group header."""
        cell_w = (self.screen_w - 2 * LAUNCHER_MARGIN) // LAUNCHER_COLS
        col = index % LAUNCHER_COLS
        row = index // LAUNCHER_COLS
        x = LAUNCHER_MARGIN + col * cell_w + cell_w // 2
        y = OVERLAY_HEADER_H + LAUNCHER_GROUP_H + row * LAUNCHER_CELL_H
        return (x, y + LAUNCHER_CELL_H // 2)

    def launcher_first_app_center(self) -> tuple[int, int]:
        """Centre of the first app cell (after the first header) at scroll 0."""
        return self.launcher_app_center(0)

    def launcher_body_center(self) -> tuple[int, int]:
        """A point in the middle of the launcher's scrollable body."""
        body_top = OVERLAY_HEADER_H
        return (self.screen_w // 2, (body_top + self.content_bottom()) // 2)
