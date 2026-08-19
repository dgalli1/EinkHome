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
    fn hit(&self, x: i32, y: i32) -> bool;
    /// Optional tap handler; return true if consumed.
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
        self.push(Rect { x, y, w: len, h: thick });
    }
    pub fn vline(&mut self, x: u32, y: u32, len: u32, thick: u32, gray: u8) {
        self.surf.vline(x, y, len, thick, gray);
        self.push(Rect { x, y, w: thick, h: len });
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
        self.push(Rect { x: x as u32, y: y as u32, w: w as u32, h: h as u32 });
    }
    pub fn text(&mut self, x: i32, baseline: i32, size: f32, s: &str, gray: u8) {
        let w = draw_text(self.surf, self.font, size, s, x, baseline, gray, self.glyph) as i32;
        self.push(Rect::from_xy(x, baseline - size as i32, w, size as i32));
    }
    pub fn text_center(&mut self, cx: i32, baseline: i32, size: f32, s: &str, gray: u8) {
        let w = self.font.width(s, size) as i32;
        self.text(cx - w / 2, baseline, size, s, gray);
    }

/// Centre `s` on `cx`, truncating (whole glyphs + `…`) so it never exceeds
/// `max_w` px AND stays within `[cx - max_w/2, cx + max_w/2]` — the C app's
/// `utf8_fit_width` analog for cover captions that must stay in their cell.
pub fn text_center_fit(&mut self, cx: i32, baseline: i32, size: f32, s: &str, max_w: i32, gray: u8) {
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
        while self.font.width(&s[..byte_len(&s, cut_len)], size) > budget && cut_len > 0 {
            cut_len -= 1;
        }
        let shown = format!("{}…", &s[..byte_len(&s, cut_len)]);
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
        Self { color, rect: None, on_tap: None, tag: 0 }
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
        Self { text: text.into(), size, gray: GRAY_BLACK, centered: false, baseline: None, rect: None }
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
            ctx.text_center(rect.x as i32 + rect.w as i32 / 2, baseline, self.size, &self.text, self.gray);
        } else {
            ctx.text(rect.x as i32, baseline, self.size, &self.text, self.gray);
        }
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let (Some(r), Some(b)) = (self.rect, self.baseline) {
            let w = (self.text.len() * (self.size as usize) / 2).min(r.w as usize) as i32;
            out.push(Rect::from_xy(r.x as i32, b - self.size as i32, w, self.size as i32));
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
}
impl Widget for Cover {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        // Cover image area: the top ~78% is a 2:3 letterboxed image; when no
        // image is loaded, draw a bordered placeholder card so the tile reads
        // like a cover even before real art arrives.
        let img_h = (rect.h as f32 * 0.78) as u32;
        let area = Rect { x: rect.x, y: rect.y, w: rect.w, h: img_h };
        if let Some(img) = &self.img {
            ctx.blit(img, self.img_w, self.img_h, eh_hal::PixelFormat::Rgb24, area);
        } else {
            // Placeholder: inset card with a border, centred on the tile.
            let border = 2u32;
            let cx = rect.x + border;
            let cy = rect.y + border;
            let cw = rect.w.saturating_sub(border * 2);
            let ch = img_h.saturating_sub(border * 2);
            if cw > 0 && ch > 0 {
                ctx.outline(Rect { x: cx, y: cy, w: cw, h: ch }, border, GRAY_LGRAY);
            }
        }
        // Title + author, centred horizontally + fitted to the tile width so
        // long titles never run past the cell edge.
        let text_h = rect.h - img_h;
        let ty = rect.y + img_h;
        let cx = rect.x as i32 + rect.w as i32 / 2;
        let max_w = rect.w.saturating_sub(4) as i32;
        let b1 = ty as i32 + self.title_size as i32;
        ctx.text_center_fit(cx, b1, self.title_size, &self.title, max_w, GRAY_BLACK);
        if !self.author.is_empty() && text_h >= (self.title_size + self.author_size) as u32 {
            ctx.text_center_fit(cx, b1 + self.author_size as i32, self.author_size, &self.author, max_w, GRAY_DGRAY);
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
    s.chars().take(n).map(|c| c.len_utf8()).sum::<usize>().min(s.len())
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
            None => Rect { x: 0, y: 0, w: 0, h: 0 },
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
    pub fn add_to(&mut self, container_idx: usize, w: Box<dyn Widget>, style: eh_layout::Style) -> usize {
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
    /// then flush dirty regions with `mode`.  One call per frame.
    pub fn present(&mut self, mode: RefreshMode) {
        let screen = self.fb.screen();
        let w = screen.width;
        let h = self.content_h;
        let stride = self.fb.stride();

        let root_children: Vec<_> = self.root_nodes.iter().copied().collect();
        self.layout.set_root_children(&root_children);
        self.layout.compute(w as f32, h as f32);

        self.dirty.clear();

        // E-ink semantics: the panel background is white, and every widget
        // fills its own rect white (top bar / tiles / pager cover the whole
        // content band), so no full-canvas pre-fill is needed — the C app
        // paints per-widget too.  A full-canvas fill here is ~4.6MB through
        // qemu on the emulator (~90ms), which delayed every redraw flush.
        let content = Rect { x: 0, y: 0, w, h };
        let fmt = self.fb.format();

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
                    glyph: &mut self.glyph,
                    dirty: &mut self.dirty,
                };
                widget.draw(&mut ctx, rect);
                widget.layout(rect);
            }
        }

        // Flush the union of drawn regions as one panel update.  (Production
        // coalesces per-region with the right waveform; the shell keeps it
        // simple and correct.)
        if !self.dirty.is_empty() {
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
            r = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
        }
        self.fb.refresh(r, mode);
        self.dirty.clear();
    }

    fn clamp(&self, r: Rect) -> (Rect, bool) {
        let limit = Rect { x: 0, y: 0, w: self.fb.screen().width, h: self.content_h };
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