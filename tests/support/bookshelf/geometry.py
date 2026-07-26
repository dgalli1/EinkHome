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
PAGER_H = 96
BOTTOM_RESERVED = 80
COLS = 3
ROWS = 2
PAGESIZE = COLS * ROWS  # 6
CELL_MAX_H = 600
CELL_MAX_W = 420
CELL_MIN_H = 280
CELL_MIN_W = 280

# ── More overlay item indices (right-anchored panel) ───────────────────
MORE_SYNC = 0
MORE_TITLE_AZ = 1
MORE_TITLE_ZA = 2
MORE_AUTHOR = 3
MORE_SERIES = 4
MORE_RECENT = 5
MORE_GRID = 6
MORE_LIST = 7
MORE_SETTINGS = 8
MORE_SYSTEM = 9

# ── Settings overlay layout (full-screen page) ────────────────────────
SETTINGS_ROW_H = 120
SETTINGS_BTN_H = 96
SETTINGS_ROW1_Y = 112

# ── Menu / group overlay item indices (left-anchored panel) ────────────
MENU_ALL = 0
MENU_BY_AUTHOR = 1
MENU_BY_SERIES = 2
MENU_BY_RECENT = 3

__all__ = [
    "BOTTOM_RESERVED",
    "CELL_MAX_H",
    "CELL_MAX_W",
    "CELL_MIN_H",
    "CELL_MIN_W",
    "COLS",
    "BookshelfGeometry",
    "MENU_ALL",
    "MENU_BY_AUTHOR",
    "MENU_BY_RECENT",
    "MENU_BY_SERIES",
    "MORE_AUTHOR",
    "MORE_SETTINGS",
    "MORE_SYSTEM",
    "MORE_GRID",
    "MORE_LIST",
    "MORE_RECENT",
    "MORE_SERIES",
    "MORE_SYNC",
    "MORE_TITLE_AZ",
    "MORE_TITLE_ZA",
    "PAGER_H",
    "PAGESIZE",
    "ROWS",
    "SEARCH_ROW_H",
    "TOP_BAR_H",
    "SETTINGS_BTN_H",
    "SETTINGS_ROW1_Y",
    "SETTINGS_ROW_H",
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

    # ── top bar ────────────────────────────────────────────────────────

    def home_button_center(self) -> tuple[int, int]:
        """Centre of the 96×96 home (house) button, left side of top bar."""
        return (8 + 48, self.panel_h + TOP_BAR_H // 2)

    def menu_button_center(self) -> tuple[int, int]:
        """Centre of the 96×96 hamburger-circle button, right side of top bar."""
        return (self.screen_w - 8 - 48, self.panel_h + TOP_BAR_H // 2)

    # ── search row ─────────────────────────────────────────────────────

    def search_box_center(self) -> tuple[int, int]:
        """Centre of the search text box."""
        row_top = self.panel_h + TOP_BAR_H
        return (self.screen_w // 2, row_top + SEARCH_ROW_H // 2)

    # ── book grid ──────────────────────────────────────────────────────

    def _grid_params(self) -> tuple[int, int, int]:
        """Return ``(grid_top, cell_w, cell_h)`` matching ``grid_geom()``."""
        top = self.panel_h + TOP_BAR_H + SEARCH_ROW_H
        bot = self.screen_h - PAGER_H - BOTTOM_RESERVED
        avail_h = bot - top - 8
        avail_w = self.screen_w - 16
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
        """Top edge of the pager row."""
        return self.screen_h - PAGER_H - BOTTOM_RESERVED

    def pager_prev_center(self) -> tuple[int, int]:
        """Centre of the 96×64 prev-page button."""
        return (12 + 48, self.pager_y() + PAGER_H // 2)

    def pager_next_center(self) -> tuple[int, int]:
        """Centre of the 96×64 next-page button."""
        return (self.screen_w - 12 - 48, self.pager_y() + PAGER_H // 2)

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

    # ── Menu / group overlay (left-anchored, 75 % width) ──────────────

    def menu_overlay_right(self) -> int:
        """X coordinate just past the Menu panel's right edge."""
        return self.screen_w * 3 // 4

    def menu_item_center(self, index: int) -> tuple[int, int]:
        """Centre of Menu-overlay item *index* (y0=96, item_h=88)."""
        pw = self.screen_w * 3 // 4
        return (pw // 2, 96 + index * 88 + 44)

    # ── Settings overlay (full-screen page) ───────────────────────────

    def settings_row_center(self, row: int) -> tuple[int, int]:
        """Centre of settings row *row* (0=API host, 1=API key, 2=reader)."""
        y = SETTINGS_ROW1_Y + row * SETTINGS_ROW_H
        return (self.screen_w // 2, y + (SETTINGS_ROW_H - 12) // 2)

    def settings_save_center(self) -> tuple[int, int]:
        """Centre of the Save & apply button."""
        y = SETTINGS_ROW1_Y + 3 * SETTINGS_ROW_H + 24
        return (self.screen_w // 2, y + (SETTINGS_BTN_H - 12) // 2)

    def settings_back_center(self) -> tuple[int, int]:
        """Centre of the Back button."""
        y = SETTINGS_ROW1_Y + 3 * SETTINGS_ROW_H + 24 + SETTINGS_BTN_H
        return (self.screen_w // 2, y + (SETTINGS_BTN_H - 12) // 2)

    def outside_menu_overlay(self) -> tuple[int, int]:
        """A point guaranteed to be right of the left-anchored Menu panel."""
        return (self.screen_w - 4, self.screen_h // 2)
