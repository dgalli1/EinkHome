//! Frame presentation & chrome: dirty-region flush, theme resources,
//! relayout and the self-drawn status strip (C eh_screen.c / eh_plat
//! panel stamping).  Split out of app.rs by concern; fields stay
//! visible here because this is a child of the defining module.
use super::*;

impl<B: Framebuffer> App<B> {
    // ── screen access ─────────────────────────────────────────────────

    /// Refresh the framebuffer caches from the live screen; call after
    /// building/moving the screen so overlay draws (which run while the
    /// screen is take()n) can use the cached values.
    pub(crate) fn sync_fb_cache(&mut self) {
        if let Some(s) = self.screen.as_mut() {
            let fb = s.framebuffer();
            self.fb_screen_w = fb.screen().width;
            self.fb_profile = fb.device_profile();
            self.fb_net_active = fb.net_active();
        }
    }
    /// Screen width safe to call from overlay draws (screen may be
    /// take()n during present).
    pub fn screen_width(&self) -> u32 {
        self.screen
            .as_ref()
            .map(|s| s.framebuffer().screen().width)
            .unwrap_or(self.fb_screen_w)
    }

    /// Device profile safe to call from overlay draws.
    pub(crate) fn device_profile(&mut self) -> eh_hal::DeviceProfile {
        self.sync_fb_cache();
        self.fb_profile
    }

    /// Theme-resource lookup safe to call from overlay draws: resolves
    /// through the framebuffer when it is alive, else replays the cache.
    pub fn theme_resource(&mut self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        if self.screen.is_some() {
            self.sync_fb_cache();
            let t = self
                .screen
                .as_mut()
                .unwrap()
                .framebuffer()
                .theme_resource(name);
            self.theme_cache.insert(name.to_string(), t.clone());
            t
        } else {
            self.theme_cache.get(name).cloned().flatten()
        }
    }
    /// Firmware-loader lookup (C LoadPNG fallback).  Deliberately does NOT
    /// consult theme_cache: a failed theme_resource() call caches None for
    /// the same name, which would shadow this lookup.
    pub(crate) fn load_png(&mut self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        if self.screen.is_some() {
            let t = self.screen.as_mut().unwrap().framebuffer().load_png(name);
            self.theme_cache.insert(name.to_string(), t.clone());
            t
        } else {
            self.theme_cache.get(name).cloned().flatten()
        }
    }
    pub fn screen(&mut self) -> &mut Screen<B> {
        self.screen.as_mut().expect("screen built")
    }

    /// Present the current frame: the screen, then the active overlay on
    /// top of the canvas.  The overlay + the self status strip flush only
    /// their own regions (partial update — the e-ink discipline).
    pub fn present(&mut self) {
        let _t0 = std::time::Instant::now();
        self.drain_keyboard();
        // Complete any worker downloads (may auto-open the reader when a
        // single-book batch drains) before we take the screen.
        self.drain_downloads();
        let ov = self.overlay;
        let changed = self.dirty || ov != self.last_overlay;
        self.dirty = false;
        self.last_overlay = ov;
        if !changed {
            // Unchanged frame: nothing to repaint (the emulator's full
            // redraw is ~1s, so skipping keeps event processing prompt —
            // and on e-ink it is the correct discipline).  Only the
            // self-panel minute rollover still needs the stamp.
            if self.self_panel > 0 {
                let min = panel_minute();
                if min != self.last_panel_min {
                    self.last_panel_min = min;
                    if let Some(s) = self.screen.as_mut() {
                        stamp_self_panel(s.framebuffer_mut(), self.content_bottom, self.self_panel);
                    }
                }
            }
            return;
        }
        let mut s = self.screen.take().expect("screen present");
        if ov == Overlay::None {
            // Plain page frame: one full-waveform flush (page flips /
            // big changes deep-clean the panel).
            s.redraw_full();
        } else {
            // Overlay frame: exactly ONE panel update per input.  The old
            // flow flushed the repainted base page first and the overlay
            // second, so every input (launcher drag-scroll, settings taps)
            // flashed the bare bookshelf for a frame — SDL presented both
            // updates back-to-back and an e-ink FullUpdate blacks the
            // panel before settling.  Paint the base into the canvas
            // silently, draw the overlay over it, then flush their merged
            // dirty union once.
            s.paint();
            let scr = s.framebuffer().screen();
            let fmt = s.framebuffer().format();
            let stride = s.framebuffer().stride();
            let mut dirty: Vec<Rect> = s.drain_dirty();
            {
                let fb = s.framebuffer_mut();
                let mut surf =
                    eh_render::Surface::new(fb.surface_mut(), scr.width, scr.height, stride, fmt);
                match ov {
                    Overlay::More => crate::menu::draw(&mut surf, self, &mut dirty),
                    Overlay::Settings => crate::settings::draw(&mut surf, self, &mut dirty),
                    Overlay::Launcher => crate::launcher::draw(&mut surf, self, &mut dirty),
                    Overlay::Source => crate::source::draw(&mut surf, self, &mut dirty),
                    Overlay::Download => {
                        crate::widgets::download::draw_download_popup(&mut surf, self, &mut dirty)
                    }
                    Overlay::Sync => {
                        crate::widgets::sync_popup::draw_sync_popup(&mut surf, self, &mut dirty)
                    }
                    Overlay::Context => {
                        crate::widgets::context::draw_context_menu(&mut surf, self, &mut dirty)
                    }
                    Overlay::GroupChooser => crate::widgets::chooser::draw_chooser_sheet(
                        &mut surf,
                        self,
                        &mut dirty,
                        ChooserKind::Group,
                    ),
                    Overlay::SortChooser => crate::widgets::chooser::draw_chooser_sheet(
                        &mut surf,
                        self,
                        &mut dirty,
                        ChooserKind::Sort,
                    ),
                    Overlay::LogViewer => {
                        crate::viewer::draw_log_viewer(&mut surf, self, &mut dirty)
                    }
                    Overlay::Licenses => crate::viewer::draw_licenses(&mut surf, self, &mut dirty),
                    Overlay::LicenseDetail => {
                        crate::viewer::draw_license_detail(&mut surf, self, &mut dirty)
                    }
                    Overlay::None => {}
                }
            }
            if let Some(u) = union_rects(&dirty) {
                s.framebuffer_mut().refresh(u, eh_hal::RefreshMode::Partial);
            }
        }
        // The self-drawn status strip lives below the content area (the
        // firmware owns the band otherwise).  Re-stamp on the first
        // present and whenever the clock's minute rolls over.
        if self.self_panel > 0 {
            let min = panel_minute();
            if min != self.last_panel_min {
                self.last_panel_min = min;
                stamp_self_panel(s.framebuffer_mut(), self.content_bottom, self.self_panel);
            }
        }
        self.screen = Some(s);
    }

    // ── navigation / input ────────────────────────────────────────────

    /// Route one input event (keyboard commits first, then taps through the
    /// overlay or the shelf; Back closes overlays).  State-only: the caller
    /// presents afterwards (the C tap handlers draw + flush themselves).
    /// Re-derive the layout geometry from the framebuffer after a live
    /// resolution switch (C sdl_set_resolution's EVT_REPAINT: the app
    /// relayouts against the new ScreenWidth/Height), then rebuild the
    /// current page.
    pub fn relayout(&mut self) {
        let s = self.screen().framebuffer().screen();
        if self.screen().framebuffer().needs_self_panel() {
            self.content_bottom = s.height.saturating_sub(106);
            self.self_panel = 106;
        } else {
            self.content_bottom = s.content_height();
            self.self_panel = 0;
        }
        self.refresh_shelf();
        self.dirty = true;
    }
}

/// "Weekday HH:MM" for the self-drawn status strip (real local time).
fn clock_label() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    // 1970-01-01 was a Thursday.
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days.rem_euclid(7)) as usize];
    format!("{wd} {h:02}:{m:02}")
}

fn union_rects(dirty: &[Rect]) -> Option<Rect> {
    let mut u = *dirty.first()?;
    for d in &dirty[1..] {
        let x0 = u.x.min(d.x);
        let y0 = u.y.min(d.y);
        let x1 = (u.x + u.w).max(d.x + d.w);
        let y1 = (u.y + u.h).max(d.y + d.h);
        u = Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        };
    }
    Some(u)
}

/// The clock's current minute (the self-panel strip's redraw cadence).
fn panel_minute() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 60)
        .unwrap_or(0)
}

/// Stamp the self-owned status strip (C `eh_plat_stamp_panel`): a white
/// band with the real clock + battery glyph, flushed as a band-only
/// partial update (the e-ink discipline — never a full refresh).
fn stamp_self_panel<B: Framebuffer>(fb: &mut B, y0: u32, panel: u32) {
    // Platform probes first (they read the device, not pixels — the
    // surface borrow below takes fb exclusively).
    let battery = fb.battery_level();
    let frontlight = fb.frontlight_on();
    let s = fb.screen();
    let h = panel as i32;
    let fmt = fb.format();
    let stride = fb.stride();
    let mut surf = eh_render::Surface::new(fb.surface_mut(), s.width, s.height, stride, fmt);
    let font = shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    surf.fill_gray(
        Rect {
            x: 0,
            y: y0,
            w: s.width,
            h: panel,
        },
        GRAY_WHITE,
    );
    surf.hline(0, y0, s.width, 2, GRAY_BLACK);
    let top = y0 as i32 + h / 2;
    let clock = clock_label();
    eh_render::draw_text(
        &mut surf,
        font,
        40.0,
        &clock,
        24,
        top - 12,
        GRAY_BLACK,
        &mut glyph,
    );
    // Frontlight bulb (C eh_draw_system_strip: circle with short rays),
    // drawn only when the light is actually on.
    if frontlight {
        let lx = s.width as i32 - 176;
        let ly = y0 as i32 + h / 2;
        surf.circle_outline(lx, ly, 12, 2, GRAY_BLACK);
        for a in 0..8u32 {
            let ang = a as f64 * core::f64::consts::PI / 4.0 + core::f64::consts::PI / 8.0;
            surf.line(
                lx + (16.0 * ang.cos()) as i32,
                ly + (16.0 * ang.sin()) as i32,
                lx + (22.0 * ang.cos()) as i32,
                ly + (22.0 * ang.sin()) as i32,
                2,
                GRAY_BLACK,
            );
        }
    }

    // Battery: outline + nub + fill proportional to charge (the C app's
    // shape; an unknown level draws empty, like the C lvl<0 clamp).
    let bw = 84u32;
    let bh = 40u32;
    let bx = s.width.saturating_sub(116);
    let by = y0 + (panel.saturating_sub(bh)) / 2;
    surf.rect_outline(
        Rect {
            x: bx,
            y: by,
            w: bw,
            h: bh,
        },
        3,
        GRAY_BLACK,
    );
    surf.fill_gray(
        Rect {
            x: bx + bw + 1,
            y: by + bh / 2 - 7,
            w: 6,
            h: 14,
        },
        GRAY_BLACK,
    );
    let lvl = battery.unwrap_or(0) as u32;
    let fw = (bw - 8) * lvl.min(100) / 100;
    if fw > 0 {
        surf.fill_gray(
            Rect {
                x: bx + 4,
                y: by + 4,
                w: fw,
                h: bh - 8,
            },
            GRAY_BLACK,
        );
    }
    fb.refresh(
        Rect {
            x: 0,
            y: y0,
            w: s.width,
            h: panel,
        },
        eh_hal::RefreshMode::Partial,
    );
}
