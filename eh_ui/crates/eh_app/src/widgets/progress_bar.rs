//! The reading-progress bar (C eh_grid.c draw_progress_bar): a thin white
//! track with a black outline and a black fill proportional to the percent
//! read, drawn inside a cover's bottom edge — grid tiles and list thumbs
//! share it.  The two geometry helpers are pure so they carry contract
//! tests without a surface.

use eh_hal::Rect;
use eh_shell::{DrawCtx, GRAY_BLACK, GRAY_WHITE};

/// Bar height by cover width (C draw_progress_bar): 10px on covers ≥150px
/// wide, 6px on small thumbs.
pub fn progress_bar_h(width: i32) -> i32 {
    if width >= 150 {
        10
    } else {
        6
    }
}

/// Inner fill width for `pct` (C: fill = cw*pct/100, drawn only once it
/// leaves a ≥1px white margin on each side).
pub fn progress_fill_w(width: i32, pct: i32) -> i32 {
    width * pct.clamp(0, 100) / 100
}

/// Reading-progress bar inside the bottom edge of a cover (port of C
/// eh_grid.c draw_progress_bar): a thin white track with black outline and
/// a black fill proportional to the percent read (0..100).
pub fn draw_progress_bar(ctx: &mut DrawCtx, x: i32, y: i32, w: i32, h: i32, pct: i32) {
    if w <= 0 || h <= 0 || x < 0 || y < 0 {
        return;
    }
    let bar_h = progress_bar_h(w).min(h);
    let by = y + h - bar_h;
    let track = Rect {
        x: x as u32,
        y: by as u32,
        w: w as u32,
        h: bar_h as u32,
    };
    ctx.fill(track, GRAY_WHITE);
    ctx.outline(track, 1, GRAY_BLACK);
    let fill = progress_fill_w(w, pct);
    if fill >= 2 && bar_h >= 3 {
        ctx.fill(
            Rect {
                x: x as u32 + 1,
                y: by as u32 + 1,
                w: (fill - 2) as u32,
                h: (bar_h - 2) as u32,
            },
            GRAY_BLACK,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_height_switches_at_150px() {
        // C draw_progress_bar: 10px on wide covers, 6px on small thumbs.
        assert_eq!(progress_bar_h(150), 10);
        assert_eq!(progress_bar_h(400), 10);
        assert_eq!(progress_bar_h(149), 6);
        assert_eq!(progress_bar_h(85), 6);
    }

    #[test]
    fn fill_width_is_proportional_and_clamped() {
        // C: fill = cw * pct / 100 (integer division floors).
        assert_eq!(progress_fill_w(300, 50), 150);
        assert_eq!(progress_fill_w(300, 33), 99);
        assert_eq!(progress_fill_w(85, 100), 85);
        assert_eq!(progress_fill_w(85, 0), 0);
        // Out-of-range percents clamp before scaling (C clamps first).
        assert_eq!(progress_fill_w(300, -20), 0);
        assert_eq!(progress_fill_w(300, 140), 300);
    }

    #[test]
    fn fill_never_leaves_the_track() {
        for w in [1i32, 6, 85, 149, 150, 280, 420] {
            let f = progress_fill_w(w, 100);
            assert!(f <= w);
            // The drawn inner fill keeps a ≥1px white margin per side.
            if f >= 2 {
                assert!(f - 2 <= w - 2);
            }
        }
    }
}
