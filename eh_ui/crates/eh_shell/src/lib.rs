//! eh_shell — the portable widget/screen layer.
//!
//! This is the piece that makes the C UI easier to maintain: instead of
//! hand-drawing coordinates in one file and hand-computing hit targets in
//! another (the `eh_grid.c` / `eh_input.c` drift in the current app), a
//! [`Widget`] here is a self-contained thing that **draws itself and
//! hit-tests itself** from the same stored geometry.
//!
//! Layout is delegated to `eh_layout` (taffy, CSS flex/grid ⇒ real
//! breakpoints/reflow); rendering to `eh_render`.  A [`Screen`] owns the
//! widget tree, the dirty-region accumulator, and the current [`Breakpoint`]
//! — the shell is fully platform-independent and drives any [`Framebuffer`].

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use eh_hal::{Framebuffer, InputEvent, KeyCode, Rect, RefreshMode};
use eh_layout::taffy::NodeId;
use eh_layout::{Breakpoint, Layout};
use eh_render::{draw_text, Font, Glyph, Surface};

/// The bold UI face (the inkview DEFAULTFONTB stand-in: grid/list titles,
/// menu rows, launcher labels — C loads DEFAULTFONTB for all of these).
pub fn bold_font() -> &'static Font {
    static BOLD: std::sync::LazyLock<Font> = std::sync::LazyLock::new(|| {
        Font::from_bytes(include_bytes!("../../../fonts/DejaVuSans-Bold.ttf"))
            .expect("embed bold font")
    });
    &BOLD
}

/// Greyscale palette values (identical to the inkview colour constants the C
/// app uses: BLACK/DGRAY/LGRAY/WHITE).
pub const GRAY_BLACK: u8 = 0x00;
pub const GRAY_DGRAY: u8 = 0x55;
pub const GRAY_LGRAY: u8 = 0xaa;
pub const GRAY_WHITE: u8 = 0xff;

/// A widget: draws within its allotted rect and answers hit-tests against
/// the same geometry.  The shell keeps one geometry source (the taffy
/// layout), so draw and hit cannot drift.
pub trait Widget {
    /// Draw into `ctx` clipped to `rect` (already position-translated).
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect);
    /// Report rectangles this widget covered in the last draw, for the shell
    /// to flush only the changed regions to the panel.
    fn dirty(&self, out: &mut Vec<Rect>);
    /// Hit-test `(x, y)` in widget-local coords → true if hit.
    fn hit(&self, _x: i32, _y: i32) -> bool {
        false
    }
    fn on_tap(&mut self, _x: i32, _y: i32) -> bool {
        false
    }
    /// Optional resize hook (called once per frame with the computed rect).
    fn layout(&mut self, _rect: Rect) {}
}

/// Drawing context handed to widgets for one pass.
pub struct DrawCtx<'a> {
    pub surf: &'a mut Surface<'a>,
    pub font: &'a Font,
    /// Bold face for titles/menu rows/labels (C DEFAULTFONTB).
    pub bold: &'static Font,
    pub glyph: &'a mut Glyph,
    pub dirty: &'a mut Vec<Rect>,
}

impl<'a> DrawCtx<'a> {
    pub fn fill(&mut self, rect: Rect, gray: u8) {
        self.surf.fill_gray(rect, gray);
        self.push(rect);
    }
    pub fn outline(&mut self, r: Rect, thick: u32, gray: u8) {
        self.surf.rect_outline(r, thick, gray);
        self.push(r);
    }
    pub fn hline(&mut self, x: u32, y: u32, len: u32, thick: u32, gray: u8) {
        self.surf.hline(x, y, len, thick, gray);
        self.push(Rect {
            x,
            y,
            w: len,
            h: thick,
        });
    }
    pub fn vline(&mut self, x: u32, y: u32, len: u32, thick: u32, gray: u8) {
        self.surf.vline(x, y, len, thick, gray);
        self.push(Rect {
            x,
            y,
            w: thick,
            h: len,
        });
    }
    /// 2D Bresenham line (the C app's `DrawLine`), tracking the bounding
    /// box for dirty regions (over-approximated by the line thickness).
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thick: u32, gray: u8) {
        self.surf.line(x0, y0, x1, y1, thick, gray);
        let t = thick as i32;
        let x = x0.min(x1).max(0);
        let y = y0.min(y1).max(0);
        let w = (x0 - x1).abs() + t;
        let h = (y0 - y1).abs() + t;
        self.push(Rect {
            x: x as u32,
            y: y as u32,
            w: w as u32,
            h: h as u32,
        });
    }
    pub fn text(&mut self, x: i32, baseline: i32, size: f32, s: &str, gray: u8) {
        let w = draw_text(self.surf, self.font, size, s, x, baseline, gray, self.glyph) as i32;
        self.push(Rect::from_xy(x, baseline - size as i32, w, size as i32));
    }
    pub fn text_center(&mut self, cx: i32, baseline: i32, size: f32, s: &str, gray: u8) {
        let w = self.font.width(s, size) as i32;
        self.text(cx - w / 2, baseline, size, s, gray);
    }

    /// [`Self::text`] with an explicit face (the bold title font).
    pub fn text_with(&mut self, font: &Font, x: i32, baseline: i32, size: f32, s: &str, gray: u8) {
        let w = draw_text(self.surf, font, size, s, x, baseline, gray, self.glyph) as i32;
        self.push(Rect::from_xy(x, baseline - size as i32, w, size as i32));
    }

    /// Centre `s` on `cx`, truncating (whole glyphs + `…`) so it never exceeds
    /// `max_w` px AND stays within `[cx - max_w/2, cx + max_w/2]` — the C app's
    /// `utf8_fit_width` analog for cover captions that must stay in their cell.
    pub fn text_center_fit(
        &mut self,
        cx: i32,
        baseline: i32,
        size: f32,
        s: &str,
        max_w: i32,
        gray: u8,
    ) {
        let full = self.font.width(s, size);
        if full as i32 <= max_w {
            // Fits whole; centre normally but never bleed off the left edge.
            let x = (cx - (full as i32) / 2).max(0);
            self.text(x, baseline, size, s, gray);
            return;
        }
        // Ellipsis; cut chars until we fit inside [cx-half, cx+half].
        let ell = self.font.width("…", size);
        let half = max_w / 2;
        let budget = (half * 2) as f32 - ell;
        let chars: Vec<char> = s.chars().collect();
        let mut cut_len = chars.len();
        while self.font.width(&s[..byte_len(s, cut_len)], size) > budget && cut_len > 0 {
            cut_len -= 1;
        }
        let shown = format!("{}…", &s[..byte_len(s, cut_len)]);
        let w2 = self.font.width(&shown, size) as i32;
        let x = cx - w2 / 2;
        self.text(x, baseline, size, &shown, gray);
    }
    pub fn blit(&mut self, img: &[u8], w: u32, h: u32, fmt: eh_hal::PixelFormat, at: Rect) {
        let used = self.surf.blit_image(img, w, h, fmt, at);
        self.push(used);
    }
    pub fn push(&mut self, r: Rect) {
        if !r.is_empty() {
            self.dirty.push(r);
        }
    }
}

/// Solid-colour fill leaf; optionally tappable.
pub struct Fill {
    pub color: u8,
    pub rect: Option<Rect>,
    pub on_tap: Option<fn(&mut Self, i32, i32)>,
    pub tag: u32,
}
impl Fill {
    pub fn new(color: u8) -> Self {
        Self {
            color,
            rect: None,
            on_tap: None,
            tag: 0,
        }
    }
}
impl Widget for Fill {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, self.color);
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            if !r.is_empty() {
                out.push(r);
            }
        }
    }
    fn layout(&mut self, rect: Rect) {
        self.rect = Some(rect);
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if rect_contains(r, x, y))
    }
    fn on_tap(&mut self, x: i32, y: i32) -> bool {
        if self.on_tap.is_some() && self.hit(x, y) {
            let f = self.on_tap.unwrap();
            (f)(self, x, y);
            true
        } else {
            false
        }
    }
}

/// Text label leaf.
pub struct Label {
    pub text: String,
    pub size: f32,
    pub gray: u8,
    pub centered: bool,
    pub baseline: Option<i32>,
    pub rect: Option<Rect>,
}
impl Label {
    pub fn new(text: impl Into<String>, size: f32) -> Self {
        Self {
            text: text.into(),
            size,
            gray: GRAY_BLACK,
            centered: false,
            baseline: None,
            rect: None,
        }
    }
}
impl Widget for Label {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        let baseline = if self.centered {
            // Basic vertical centre: place baseline at ~40% height (ascender).
            rect.y as i32 + rect.h as i32 / 2 + (self.size * 0.30) as i32
        } else {
            rect.y as i32 + self.size as i32
        };
        self.baseline = Some(baseline);
        self.rect = Some(rect);
        if self.centered {
            ctx.text_center(
                rect.x as i32 + rect.w as i32 / 2,
                baseline,
                self.size,
                &self.text,
                self.gray,
            );
        } else {
            ctx.text(rect.x as i32, baseline, self.size, &self.text, self.gray);
        }
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let (Some(r), Some(b)) = (self.rect, self.baseline) {
            let w = (self.text.len() * (self.size as usize) / 2).min(r.w as usize) as i32;
            out.push(Rect::from_xy(
                r.x as i32,
                b - self.size as i32,
                w,
                self.size as i32,
            ));
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if rect_contains(r, x, y))
    }
}

/// A layout group: a container that holds geometry but draws nothing.
/// Used by the shell's nested layout (top-bar / grid / pager bands).
pub struct Group {
    pub rect: Option<Rect>,
}

impl Default for Group {
    fn default() -> Self {
        Self::new()
    }
}

impl Group {
    pub fn new() -> Self {
        Self { rect: None }
    }
}
impl Widget for Group {
    fn draw(&mut self, _ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
    }
    fn dirty(&self, _out: &mut Vec<Rect>) {}
    fn layout(&mut self, rect: Rect) {
        self.rect = Some(rect);
    }
    fn hit(&self, _x: i32, _y: i32) -> bool {
        false
    }
}

/// A cover tile: optional image + title/author lines.
pub struct Cover {
    pub img: Option<alloc::vec::Vec<u8>>,
    pub img_w: u32,
    pub img_h: u32,
    pub title: String,
    pub author: String,
    pub title_size: f32,
    pub author_size: f32,
    pub rect: Option<Rect>,
}
impl Cover {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            img: None,
            img_w: 0,
            img_h: 0,
            title: title.into(),
            author: String::new(),
            title_size: 22.0,
            author_size: 18.0,
            rect: None,
        }
    }
    pub fn set_image(&mut self, data: Vec<u8>, w: u32, h: u32) {
        self.img = Some(data);
        self.img_w = w;
        self.img_h = h;
    }
    /// The centred 2:3 cover card inside `rect` (port of C eh_cover_rect:
    /// 4px border, 52px caption band below).
    pub fn cover_card(rect: Rect) -> Rect {
        const THUMB_BORDER: u32 = 4;
        const TEXT_AREA: u32 = 52;
        let inner_w = rect.w.saturating_sub(2 * THUMB_BORDER) as i32;
        let inner_h = rect.h.saturating_sub(2 * THUMB_BORDER) as i32;
        let mut ch0 = inner_h - TEXT_AREA as i32;
        let mut cw0 = ch0 * 2 / 3;
        if cw0 > inner_w {
            cw0 = inner_w;
            ch0 = cw0 * 3 / 2;
        }
        if ch0 > inner_h {
            ch0 = inner_h;
        }
        if ch0 < 8 {
            ch0 = 8;
        }
        let cx = rect.x as i32 + THUMB_BORDER as i32 + (inner_w - cw0) / 2;
        let cy = rect.y as i32 + THUMB_BORDER as i32;
        Rect {
            x: cx.max(0) as u32,
            y: cy.max(0) as u32,
            w: cw0.max(0) as u32,
            h: ch0.max(0) as u32,
        }
    }
}
impl Widget for Cover {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        // Cover card (C eh_cover_rect): a centred 2:3 portrait inside the
        // tile minus the 4px thumb border, with EH_TEXT_AREA (52px)
        // reserved below for the caption lines.  Covers stretch to fill
        // this card exactly (C StretchBitmap); with no art yet C draws a
        // 1px BLACK outline of the same card.
        let card = Self::cover_card(rect);
        if let Some(img) = &self.img {
            ctx.surf.blit_image_stretch(
                img,
                self.img_w,
                self.img_h,
                eh_hal::PixelFormat::Rgb24,
                card,
            );
            ctx.push(card);
        } else {
            ctx.outline(card, 1, GRAY_BLACK);
        }
        // Caption (C draw_thumbnail_text grid slot): bold title flush-LEFT
        // at the tile edge, 6px under the card; author 24px lower in DGRAY
        // — both truncated to the tile width, never centred.
        let cap_y = card.y as i32 + card.h as i32 + 6;
        let tx = rect.x as i32 + 4;
        let max_px = (rect.w.saturating_sub(8)) as f32;
        let mut fitted = String::new();
        eh_render::fit_width(ctx.bold, self.title_size, &self.title, max_px, &mut fitted);
        if fitted.is_empty() {
            fitted.push_str(&self.title[..1.min(self.title.chars().count())]);
        }
        let baseline1 = cap_y + self.title_size as i32;
        draw_text(
            ctx.surf,
            ctx.bold,
            self.title_size,
            &fitted,
            tx,
            baseline1,
            GRAY_BLACK,
            ctx.glyph,
        );
        ctx.push(rect);
        let text_h = rect.h.saturating_sub(card.y - rect.y + card.h) as i32;
        if !self.author.is_empty() && text_h >= (self.title_size + self.author_size) as i32 {
            let mut afitted = String::new();
            eh_render::fit_width(
                ctx.font,
                self.author_size,
                &self.author,
                max_px,
                &mut afitted,
            );
            draw_text(
                ctx.surf,
                ctx.font,
                self.author_size,
                &afitted,
                tx,
                cap_y + 24 + self.author_size as i32,
                GRAY_DGRAY,
                ctx.glyph,
            );
        }
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if rect_contains(r, x, y))
    }
}

fn rect_contains(r: Rect, x: i32, y: i32) -> bool {
    (x as u32) >= r.x && (x as u32) < r.x + r.w && (y as u32) >= r.y && (y as u32) < r.y + r.h
}

/// Byte length of the first `n` chars of `s` (for slicing on a boundary).
fn byte_len(s: &str, n: usize) -> usize {
    s.chars()
        .take(n)
        .map(|c| c.len_utf8())
        .sum::<usize>()
        .min(s.len())
}

/// A screen: the widget tree + layout engine + dirty tracking driving one
/// framebuffer.  This is the platform-independent heart of the app.
pub struct Screen<B: Framebuffer> {
    fb: B,
    font: &'static Font,
    glyph: Glyph,
    layout: Layout,
    /// taffy node id for each widget (parallel to `widgets`).
    nodes: Vec<NodeId>,
    /// The top-level nodes that are direct children of the layout root (the
    /// chrome bands + containers); nested `add_to` children are NOT here.
    root_nodes: Vec<NodeId>,
    dirty: Vec<Rect>,
    pub breakpoint: Breakpoint,
    pub widgets: Vec<Box<dyn Widget>>,
    /// Rows owned by the app above the (possibly firmware) status strip.
    pub content_h: u32,
    /// Pages whose widgets may not cover the whole content band
    /// (Search, browser) set this so present() pre-fills white (C
    /// FillArea-per-page discipline).
    pub bg_fill: bool,
}

impl<B: Framebuffer> Screen<B> {
    pub fn new(fb: B, font: &'static Font) -> Self {
        let screen = fb.screen();
        Self {
            fb,
            font,
            glyph: Glyph::new(),
            layout: Layout::new(),
            nodes: Vec::new(),
            root_nodes: Vec::new(),
            dirty: Vec::new(),
            bg_fill: false,
            breakpoint: Breakpoint::from_width(screen.width),
            widgets: Vec::new(),
            content_h: screen.content_height(),
        }
    }

    pub fn framebuffer(&self) -> &B {
        &self.fb
    }
    pub fn framebuffer_mut(&mut self) -> &mut B {
        &mut self.fb
    }
    /// Consume the screen, returning the framebuffer (for the app to rebuild
    /// a different screen from the same canvas on navigation).
    pub fn into_framebuffer(self) -> B {
        self.fb
    }
    /// The content height this screen lays out against (the screen height
    /// minus any firmware panel band).
    pub fn content_height(&self) -> u32 {
        self.content_h
    }

    /// Access the underlying layout engine (configure root wrapping, etc.).
    pub fn layout_mut(&mut self) -> &mut Layout {
        &mut self.layout
    }
    /// Screen-absolute rect of the widget at `idx` (the same taffy geometry
    /// `present` draws with) — so app-level hit-testing shares one source
    /// of truth with the paint path (the C app's draw/hit geometry parity).
    /// Valid after the first `present`.
    pub fn widget_rect(&self, idx: usize) -> Rect {
        match self.nodes.get(idx) {
            Some(&n) => self.clamp(self.layout.rect(n)).0,
            None => Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
        }
    }

    /// Push a widget, giving it a taffy node with the given style.  The app
    /// calls this once when building its screen.  `present()` places the
    /// widget at the taffy-computed rect, so draw and hit share one source.
    pub fn add(&mut self, w: Box<dyn Widget>) -> usize {
        let node = self.layout.leaf(eh_layout::Style::DEFAULT);
        self.nodes.push(node);
        self.root_nodes.push(node);
        self.widgets.push(w);
        self.widgets.len() - 1
    }

    /// Push a widget with an explicit taffy style (flexbox/grid layout).
    pub fn add_styled(&mut self, w: Box<dyn Widget>, style: eh_layout::Style) -> usize {
        let node = self.layout.leaf(style);
        self.nodes.push(node);
        self.root_nodes.push(node);
        self.widgets.push(w);
        self.widgets.len() - 1
    }

    /// Create a layout container (a taffy node with children + a style) that
    /// is itself one of the root's children.  Returns its index; later
    /// [`add_to`](Self::add_to) places widgets inside it so the screen can
    /// express a nested layout (e.g. top-bar / grid-container / pager).
    /// The container has no Widget of its own — it only groups geometry.
    pub fn add_container(&mut self, style: eh_layout::Style) -> usize {
        let node = self.layout.node(style, &[]);
        self.nodes.push(node);
        self.root_nodes.push(node);
        self.widgets.push(Box::new(Group::new()));
        self.nodes.len() - 1
    }

    /// Add a widget inside `container_idx` (a node created by
    /// [`add_container`](Self::add_container)).  The container becomes that
    /// node's parent in taffy.  Returns the widget's index.
    pub fn add_to(
        &mut self,
        container_idx: usize,
        w: Box<dyn Widget>,
        style: eh_layout::Style,
    ) -> usize {
        let parent = self.nodes[container_idx];
        let node = self.layout.leaf(style);
        self.layout.tree_mut().add_child(parent, node).ok();
        self.nodes.push(node);
        self.widgets.push(w);
        self.widgets.len() - 1
    }

    /// Re-style an existing widget's taffy node (e.g. to show/hide a
    /// breakpoint-specific container by collapsing it to zero size).
    pub fn set_style(&mut self, idx: usize, style: eh_layout::Style) {
        if let Some(&node) = self.nodes.get(idx) {
            self.layout.tree_mut().set_style(node, style).ok();
        }
    }

    /// Handle one input event: dispatch taps to widgets (front-to-back,
    /// matching the visual z-order).
    pub fn on_event(&mut self, ev: &InputEvent) {
        if let InputEvent::PointerUp { x, y } = ev {
            for w in self.widgets.iter_mut().rev() {
                if w.on_tap(*x, *y) {
                    break;
                }
            }
        } else if let InputEvent::KeyDown { key: KeyCode::Back } = ev {
            let _ = KeyCode::Back; // default: app wires a back callback
        }
    }

    /// Compute layout and draw all widgets into the framebuffer surface,
    /// accumulating dirty regions.  No panel update: `present` paints and
    /// then flushes; the app layer paints overlays between the two halves
    /// so one frame costs exactly ONE panel update.
    pub fn paint(&mut self) {
        let screen = self.fb.screen();
        let w = screen.width;
        let h = self.content_h;
        let stride = self.fb.stride();

        let root_children: Vec<_> = self.root_nodes.to_vec();
        self.layout.set_root_children(&root_children);
        self.layout.compute(w as f32, h as f32);

        self.dirty.clear();

        // C parity: pages whose widgets don't cover the whole content
        // band (Search, folder browser) must paint the WHOLE area first,
        // or stale pixels survive in the uncovered band — visible on
        // e-ink and in the SDL buffer alike.  Such pages set `bg_fill`;
        // the shelf (whose tiles tile the entire band) skips the fill —
        // through qemu it costs ~90ms per frame and starves the sync.
        let content = Rect { x: 0, y: 0, w, h };
        let fmt = self.fb.format();
        if self.bg_fill {
            let mut surf = Surface::new(self.fb.surface_mut(), w, h, stride, fmt);
            surf.fill_gray(content, GRAY_WHITE);
        }

        for i in 0..self.widgets.len() {
            let rect = self.layout.rect(self.nodes[i]);
            let rect = self.clamp(rect).0;
            if rect.is_empty() {
                continue; // collapsed (breakpoint-hidden) widget
            }
            let widget = &mut self.widgets[i];
            {
                let mut surf = Surface::new(self.fb.surface_mut(), w, h, stride, fmt);
                let mut ctx = DrawCtx {
                    surf: &mut surf,
                    font: self.font,
                    bold: bold_font(),
                    glyph: &mut self.glyph,
                    dirty: &mut self.dirty,
                };
                widget.draw(&mut ctx, rect);
                widget.layout(rect);
            }
        }
    }

    /// Take the dirty regions [`paint`](Self::paint) accumulated, leaving
    /// it empty (the app merges them with its overlay regions before its
    /// own flush).
    pub fn drain_dirty(&mut self) -> Vec<Rect> {
        core::mem::take(&mut self.dirty)
    }

    /// Compute layout, draw all widgets, then flush with `mode`.  One call
    /// per frame.
    pub fn present(&mut self, mode: RefreshMode) {
        self.paint();
        if !self.dirty.is_empty() {
            let content = Rect {
                x: 0,
                y: 0,
                w: self.fb.screen().width,
                h: self.content_h,
            };
            self.fb.refresh(content, mode);
        }
        self.dirty.clear();
    }

    /// Flush the regions accumulated since the last present (used by the
    /// app layer after drawing overlays onto the canvas without a full
    /// widget repaint).
    pub fn flush(&mut self, mode: RefreshMode) {
        if self.dirty.is_empty() {
            return;
        }
        let mut r = self.dirty[0];
        for d in &self.dirty[1..] {
            let x0 = r.x.min(d.x);
            let y0 = r.y.min(d.y);
            let x1 = (r.x + r.w).max(d.x + d.w);
            let y1 = (r.y + r.h).max(d.y + d.h);
            r = Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            };
        }
        self.fb.refresh(r, mode);
        self.dirty.clear();
    }

    fn clamp(&self, r: Rect) -> (Rect, bool) {
        let limit = Rect {
            x: 0,
            y: 0,
            w: self.fb.screen().width,
            h: self.content_h,
        };
        let clipped = r.intersect(&limit);
        (clipped, clipped == r)
    }

    /// Full refresh helper (page flips / big changes).
    pub fn redraw_full(&mut self) {
        self.present(RefreshMode::Full);
    }
    /// Partial refresh helper (small updates).
    pub fn redraw_partial(&mut self) {
        self.present(RefreshMode::Partial);
    }
}

// ── modal dim + scroll buttons + word wrap (C eh_screen.c / eh_popups.c)
//
// Shared drawing helpers for the overlay layer.  They live in the shell
// (not the app) so every overlay draws the same dim and the same corner
// buttons — the C app keeps them in eh_screen.c for the same reason.

/// The modal backdrop: an LGRAY every-other-line hatch (C eh_dim_content).
/// The hatch keeps the dimmed shelf readable behind a sheet in a way a
/// solid fill never is on e-ink.  `y0` is where the dim starts: popups
/// keep the top bar undimmed (its icons — the spinning sync glyph among
/// them — stay fully visible), full-screen overlays dim from the very top.
pub fn dim_hatch(surf: &mut Surface, y0: u32, y1: u32) {
    let w = surf.width();
    let mut y = y0;
    while y < y1 {
        surf.hline(0, y, w, 1, GRAY_LGRAY);
        y += 2;
    }
}

/// Corner scroll-button geometry (C EH_SCROLL_BTN_*).
pub const SCROLL_BTN_W: u32 = 150;
pub const SCROLL_BTN_H: u32 = 96;

/// The two bottom-corner scroll buttons (C eh_draw_scroll_buttons_at):
/// left = up/older, right = down/newer; a disabled direction greys its
/// border and chevron.  `y0` is the buttons' top edge.
pub fn draw_scroll_buttons_at(surf: &mut Surface, y0: u32, up_ok: bool, down_ok: bool) {
    if !up_ok && !down_ok {
        return;
    }
    let w = surf.width();
    // Left button: an up chevron.
    surf.fill_gray(
        Rect {
            x: 0,
            y: y0,
            w: SCROLL_BTN_W,
            h: SCROLL_BTN_H,
        },
        GRAY_WHITE,
    );
    surf.rect_outline(
        Rect {
            x: 0,
            y: y0,
            w: SCROLL_BTN_W,
            h: SCROLL_BTN_H,
        },
        2,
        if up_ok { GRAY_BLACK } else { GRAY_LGRAY },
    );
    let col = if up_ok { GRAY_BLACK } else { GRAY_LGRAY };
    let cx = (SCROLL_BTN_W / 2) as i32;
    let cy = (y0 + SCROLL_BTN_H / 2) as i32;
    surf.line(cx - 24, cy + 14, cx, cy - 14, 2, col);
    surf.line(cx + 24, cy + 14, cx, cy - 14, 2, col);
    // Right button: a down chevron.
    let x2 = w - SCROLL_BTN_W;
    surf.fill_gray(
        Rect {
            x: x2,
            y: y0,
            w: SCROLL_BTN_W,
            h: SCROLL_BTN_H,
        },
        GRAY_WHITE,
    );
    surf.rect_outline(
        Rect {
            x: x2,
            y: y0,
            w: SCROLL_BTN_W,
            h: SCROLL_BTN_H,
        },
        2,
        if down_ok { GRAY_BLACK } else { GRAY_LGRAY },
    );
    let col = if down_ok { GRAY_BLACK } else { GRAY_LGRAY };
    let cx = (x2 + SCROLL_BTN_W / 2) as i32;
    surf.line(cx - 24, cy - 14, cx, cy + 14, 2, col);
    surf.line(cx + 24, cy - 14, cx, cy + 14, 2, col);
}

/// Scroll buttons on the content area's bottom edge (C
/// eh_draw_scroll_buttons: y0 = eh_content_bottom() - EH_SCROLL_BTN_H).
pub fn draw_scroll_buttons(surf: &mut Surface, content_bottom: u32, up_ok: bool, down_ok: bool) {
    draw_scroll_buttons_at(
        surf,
        content_bottom.saturating_sub(SCROLL_BTN_H),
        up_ok,
        down_ok,
    );
}

/// Scroll buttons on the content area's bottom edge (C
/// Hit-test the corner scroll buttons (C eh_hit_scroll_button_at):
/// -1 = up/older (left), +1 = down/newer (right), 0 = miss.  `w` is the
/// surface width the buttons were drawn on.
pub fn hit_scroll_button_at(x: i32, y: i32, y0: u32, w: u32) -> i32 {
    if (y as u32) < y0 || (y as u32) >= y0 + SCROLL_BTN_H {
        return 0;
    }
    if x >= 0 && (x as u32) < SCROLL_BTN_W {
        return -1;
    }
    if x >= (w - SCROLL_BTN_W) as i32 && (x as u32) < w {
        return 1;
    }
    0
}

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
fn wrap_line(
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
    #[test]
    fn scroll_hit_left_right_and_miss() {
        let y0 = 500u32;
        assert_eq!(hit_scroll_button_at(10, y0 as i32 + 40, y0, 1000), -1);
        assert_eq!(hit_scroll_button_at(999, y0 as i32 + 40, y0, 1000), 1);
        // Between the two corner boxes: miss.
        assert_eq!(hit_scroll_button_at(500, y0 as i32 + 40, y0, 1000), 0);
    }

    #[test]
    fn scroll_hit_respects_the_band_edges() {
        let y0 = 500u32;
        assert_eq!(hit_scroll_button_at(10, y0 as i32 - 1, y0, 1000), 0);
        assert_eq!(hit_scroll_button_at(10, y0 as i32, y0, 1000), -1);
        assert_eq!(
            hit_scroll_button_at(10, (y0 + SCROLL_BTN_H) as i32, y0, 1000),
            0,
            "y+h exclusive"
        );
        assert_eq!(hit_scroll_button_at(-1, y0 as i32 + 40, y0, 1000), 0);
    }

    #[test]
    fn scroll_hit_right_edge_is_inclusive_of_last_pixel() {
        let w = 1000u32;
        let y0 = 500u32;
        assert_eq!(
            hit_scroll_button_at((w - SCROLL_BTN_W) as i32, y0 as i32 + 1, y0, w),
            1
        );
        assert_eq!(hit_scroll_button_at(w as i32, y0 as i32 + 1, y0, w), 0);
    }
}
