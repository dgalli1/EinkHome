//! Pixel-width word wrap shared with the C app (C log_wrap_word /
//! log_wrap_line / lic_wrap_rows): the viewers wrap text with the SAME
//! fontdue metrics the old renderer used, then hand the visible rows to
//! Slint as strings.

use eh_render::Font;

/// A wrapped display row: a byte span `[start, end)` of the wrapped text
/// (never modified), or a blank paragraph-gap row (`blank`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapRow {
    pub start: usize,
    pub end: usize,
    pub blank: bool,
}

/// Greedy pixel-width word wrap of ONE line into `out`, at most `cap`
/// total rows (C log_wrap_word/log_wrap_line).  `base` is the line's
/// byte offset in the wrapped text — emitted spans index the WHOLE text,
/// not the line slice.  Space runs collapse; a word only breaks the row
/// when the current row already has content and `cur_w + word_w + 6`
/// would overflow `max_w` (the C fudge factor).  A single word wider
/// than `max_w` gets its own overflowing row — the C app does the same
/// rather than splitting words.
pub(crate) fn wrap_line(
    font: &Font,
    size: f32,
    line: &str,
    base: usize,
    max_w: f32,
    out: &mut Vec<WrapRow>,
    cap: usize,
) {
    let b = line.as_bytes();
    // Scan for the space byte directly: ' ' (0x20) can never occur
    // inside a multi-byte UTF-8 sequence, so byte indices stay on char
    // boundaries.
    let mut row_start: Option<usize> = None;
    let mut row_end = 0usize;
    let mut ws = 0usize;
    while ws < line.len() && out.len() < cap {
        let mut we = ws;
        while we < line.len() && b[we] != b' ' {
            we += 1;
        }
        if we == ws {
            ws += 1; // collapse space runs
            continue;
        }
        let word_w = font.width(&line[ws..we], size);
        let cur_w = match row_start {
            Some(s) => font.width(&line[s..row_end], size),
            None => 0.0,
        };
        if row_start.is_some() && cur_w + word_w + 6.0 > max_w {
            out.push(WrapRow {
                start: base + row_start.unwrap(),
                end: base + row_end,
                blank: false,
            });
            row_start = None;
            if out.len() >= cap {
                return; // no room on a fresh row
            }
        }
        if row_start.is_none() {
            row_start = Some(ws);
        }
        row_end = we;
        if we < line.len() {
            row_end += 1; // the separating space
        }
        ws = we;
    }
    if out.len() < cap {
        if let Some(s) = row_start {
            // Finalise the trailing partial row.
            out.push(WrapRow {
                start: base + s,
                end: base + row_end,
                blank: false,
            });
        }
    }
}

/// Greedy word wrap of `text` into rows no wider than `max_w` px, oldest
/// line first (C lic_wrap_rows).  Blank source lines become dedicated
/// gap rows so paragraph shape survives.  At most `cap` rows.
pub fn wrap_rows_forward(
    font: &Font,
    size: f32,
    text: &str,
    max_w: f32,
    cap: usize,
) -> Vec<WrapRow> {
    let mut rows = Vec::new();
    let mut base = 0usize;
    for line in text.split('\n') {
        if rows.len() >= cap {
            break;
        }
        if line.is_empty() {
            rows.push(WrapRow {
                start: base,
                end: base,
                blank: true,
            });
        } else {
            wrap_line(font, size, line, base, max_w, &mut rows, cap);
        }
        base += line.len() + 1; // the LF the split consumed
    }
    rows
}

/// Greedy word wrap of a log tail into at most `cap` rows, anchored on
/// the NEWEST content (C log_wrap_rows_last): lines are walked backward
/// from the last one and the resulting rows are returned oldest → newest
/// (row 0 = oldest kept, the last row = the current log tail).  A
/// forward wrap of a big log would fill the cap-bounded row set with the
/// OLDEST rows and never wrap the newest lines, so an open viewer would
/// show stale content instead of the tail.
pub fn wrap_rows_last(font: &Font, size: f32, text: &str, max_w: f32, cap: usize) -> Vec<WrapRow> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop(); // a trailing LF opens no line of its own
    }
    let mut kept: Vec<WrapRow> = Vec::new();
    let mut base = text.len();
    for line in lines.iter().rev() {
        base -= line.len();
        if kept.len() >= cap {
            break;
        }
        let before = kept.len();
        wrap_line(font, size, line, base, max_w, &mut kept, cap);
        kept[before..].reverse(); // this line's rows newest-first
        base = base.saturating_sub(1); // the LF before this line
    }
    kept.reverse(); // the kept set oldest → newest
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> &'static Font {
        static FONT: std::sync::LazyLock<Font> = std::sync::LazyLock::new(|| {
            Font::from_bytes(include_bytes!("../../../fonts/DejaVuSans.ttf")).expect("bundled font")
        });
        &FONT
    }

    fn spans<'a>(text: &'a str, rows: &[WrapRow]) -> Vec<&'a str> {
        rows.iter().map(|r| &text[r.start..r.end]).collect()
    }

    #[test]
    fn wrap_breaks_rows_at_pixel_width() {
        let f = font();
        let text = "aaa bbb ccc";
        // A width that only fits two of the three words (6px fudge).
        let two_words = f.width("aaa bbb", 20.0);
        let rows = wrap_rows_forward(f, 20.0, text, two_words - 1.0, 64);
        // The trailing separator space rides in the row span and the +6
        // fudge counts it too (C behaviour) — so "bbb" overflows here.
        assert_eq!(spans(text, &rows), vec!["aaa ", "bbb ", "ccc"]);
    }

    #[test]
    fn wrap_keeps_paragraph_gaps_and_collapses_spaces() {
        let f = font();
        let text = "hello  world\n\nnext para";
        let rows = wrap_rows_forward(f, 20.0, text, 10_000.0, 64);
        // Spans keep the source bytes (the collapsed run stays inside the
        // slice, as in C) — only the ROW BREAKS matter.
        assert_eq!(spans(text, &rows), vec!["hello  world", "", "next para"]);
        assert!(rows[1].blank, "blank source line becomes a gap row");
    }

    #[test]
    fn wrap_rows_last_pins_the_tail() {
        let f = font();
        let text = "l1\nl2\nl3\nl4\nl5";
        // Cap of 3 keeps the NEWEST three rows, oldest-first.
        let rows = wrap_rows_last(f, 20.0, text, 10_000.0, 3);
        assert_eq!(spans(text, &rows), vec!["l3", "l4", "l5"]);
        // A long final line wraps into several rows; the tail row set is
        // still the newest content, in order.
        let long = "a b c d e f";
        let one_word = f.width("a", 20.0) + 6.0;
        let rows = wrap_rows_last(f, 20.0, long, one_word, 3);
        // Cap hit mid-line keeps the line's OLDEST rows (C log_wrap_line
        // wraps forward and stops at the cap) — the tail-pinning promise
        // holds across LINES.
        assert_eq!(spans(long, &rows), vec!["a ", "b ", "c "]);
    }

    #[test]
    fn wrap_respects_cap() {
        let f = font();
        let rows = wrap_rows_forward(f, 20.0, "a\nb\nc\nd", 10_000.0, 2);
        assert_eq!(rows.len(), 2);
    }

    // The tri-state corner hit test: -1 = up/older (left), +1 =
    // down/newer (right), 0 = miss.  Every scrollable overlay pages by
    // this result — a sign flip or dead zone scrolls the wrong way.
}
