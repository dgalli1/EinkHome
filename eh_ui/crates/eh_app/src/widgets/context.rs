//! The long-press context menu (C eh_draw_context): a centred white sheet
//! with the action rows.  Geometry matches the harness's context_geom
//! (sheet centred on the FULL screen; title band 72 + n*96 + 24 rows).

use eh_hal::{Framebuffer, Rect};

/// A long-press action row (C eh_context_action).
#[derive(Clone, Copy, PartialEq)]
pub enum ContextAction {
    Open,
    Download,
    Delete,
    DownloadAll,
    DeleteAll,
}

pub fn draw_context_menu<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut crate::app::App<B>,
    dirty: &mut Vec<Rect>,
) {
    use eh_shell::{GRAY_BLACK, GRAY_LGRAY, GRAY_WHITE};
    let h = surf.height();
    let n = app.context.items.len().max(1);
    // Dim over the FULL screen; sheet centred on the full screen too.
    let sh = super::sheet::open_sheet(surf, dirty, h, 0, h, h, (72 + n * 96 + 24) as u32, false);
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    surf.hline(sh.px + 24, sh.py + 72, sh.pw - 48, 2, GRAY_LGRAY);
    app.context.rects.clear();
    for (i, act) in app.context.items.iter().enumerate() {
        let iy = sh.py + 72 + (i as u32) * 96;
        let label: &str = match act {
            ContextAction::Open => crate::i18n::tr("ctx.open"),
            ContextAction::Download => crate::i18n::tr("ctx.download"),
            ContextAction::Delete => crate::i18n::tr("ctx.delete"),
            ContextAction::DownloadAll => crate::i18n::tr("ctx.download_all"),
            ContextAction::DeleteAll => crate::i18n::tr("ctx.delete_series"),
        };
        surf.fill_gray(
            Rect {
                x: sh.px + 12,
                y: iy,
                w: sh.pw - 24,
                h: 84,
            },
            GRAY_WHITE,
        );
        surf.rect_outline(
            Rect {
                x: sh.px + 12,
                y: iy,
                w: sh.pw - 24,
                h: 84,
            },
            1,
            GRAY_BLACK,
        );
        eh_render::draw_text(
            surf,
            font,
            28.0,
            label,
            (sh.px + 32) as i32,
            (iy + 30) as i32,
            GRAY_BLACK,
            &mut g,
        );
        app.context.rects.push(Rect {
            x: sh.px + 12,
            y: iy,
            w: sh.pw - 24,
            h: 84,
        });
    }
}
