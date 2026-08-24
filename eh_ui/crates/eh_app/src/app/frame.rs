//! Frame presentation & chrome: dirty-region flush, theme resources,
//! relayout, the property sync into the Slint tree and the self-drawn
//! status strip (C eh_screen.c / eh_plat panel stamping).  Split out of
//! app.rs by concern.
//!
//! Presenting renders through the Slint software renderer straight into
//! the framebuffer: plain pages flush the whole content band with a Full
//! waveform (page flips deep-clean the panel), overlays flush only their
//! painted region with ONE Partial update — the e-ink discipline the
//! `overlay_frame_flushes_once` contract pins.
use super::*;

impl<B: Framebuffer> App<B> {
    // ── framebuffer access ─────────────────────────────────────────────

    /// Refresh the framebuffer caches from the live fb; call after the
    /// fb is (re)bound so overlay draws can use the cached values.
    pub(crate) fn sync_fb_cache(&mut self) {
        if let Some(fb) = self.fb.as_mut() {
            self.fb_screen_w = fb.screen().width;
            self.fb_profile = fb.device_profile();
            self.fb_net_active = fb.net_active();
        }
    }
    /// Screen width safe to call from overlay draws.
    pub fn screen_width(&self) -> u32 {
        self.fb
            .as_ref()
            .map(|fb| fb.screen().width)
            .unwrap_or(self.fb_screen_w)
    }

    /// The live framebuffer (panics when absent — same contract as the
    /// old `screen()`).
    pub fn fb(&mut self) -> &mut B {
        self.fb.as_mut().expect("framebuffer bound")
    }

    /// Device profile safe to call from overlay draws.
    pub(crate) fn device_profile(&mut self) -> eh_hal::DeviceProfile {
        self.sync_fb_cache();
        self.fb_profile
    }

    /// Theme-resource lookup: resolves through the framebuffer, else
    /// replays the cache.
    pub fn theme_resource(&mut self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        if let Some(fb) = self.fb.as_mut() {
            let t = fb.theme_resource(name);
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
        if let Some(fb) = self.fb.as_mut() {
            let t = fb.load_png(name);
            self.theme_cache.insert(name.to_string(), t.clone());
            t
        } else {
            self.theme_cache.get(name).cloned().flatten()
        }
    }

    // ── property sync ──────────────────────────────────────────────────

    /// Push the chrome + page-independent state into the Slint tree.
    /// Per-page models (entries / browse rows / history) are synced by
    /// `refresh_shelf`; this covers everything present() needs every frame.
    pub(crate) fn sync_ui(&mut self) {
        let c = self.ui.comp();
        c.set_content_bottom(self.content_bottom as f32);
        c.set_has_self_panel(self.self_panel > 0);
        c.set_tab(match self.tab {
            Tab::Library => crate::ui::EhTab::Library,
            Tab::Search => crate::ui::EhTab::Search,
        });
        let back = match self.tab {
            Tab::Search => true,
            Tab::Library => self.drill > 0 && !self.browse_active(),
        };
        c.set_back(back);
        c.set_browse_mode(self.browse_active());
        let slabel = match self.source {
            Source::Local => crate::i18n::tr("source.local").to_string(),
            Source::Folder => crate::i18n::tr("source.folder").to_string(),
            Source::Kavita => crate::i18n::tr("source.kavita").to_string(),
        };
        c.set_source_label(slabel.into());
        c.set_pages(self.pages.max(1) as i32);
        c.set_syncing(self.syncing);
        c.set_sync_angle(self.sync_angle);
        let title = self.ui_title();
        c.set_top_title(title.into());
        let icon = self.ui.source_image(self.source);
        c.set_source_icon(icon);
        // search page
        c.set_query(self.query.clone().into());
        c.set_search_placeholder(crate::i18n::tr("search.ph").to_string().into());
        c.set_search_kb(self.search_kb);
        // self panel
        c.set_clock(clock_label().into());
        c.set_battery_level(
            self.fb.as_ref().and_then(|fb| fb.battery_level()).unwrap_or(0) as i32,
        );
        c.set_frontlight(self.fb.as_ref().map(|fb| fb.frontlight_on()).unwrap_or(false));
    }
    /// The top-bar title for the active body (browser path / drilled
    /// group / query / Search).
    fn ui_title(&self) -> String {
        if self.dl_picker.is_some() {
            let p = self.dl_picker.as_ref().unwrap();
            return crate::local::browser::Browser::user_display(&p.path, &p.root).to_string();
        }
        if self.browse_active() {
            return crate::local::browser::Browser::user_display(
                &self.browser.path,
                &self.browser.root,
            )
            .to_string();
        }
        if self.tab == Tab::Search {
            return crate::i18n::tr("tab.search").to_string();
        }
        self.top_title().to_string()
    }

    /// True when the folder browser (or the download-dir picker) owns the
    /// shelf body.
    pub(crate) fn browse_active(&self) -> bool {
        self.dl_picker.is_some() || (self.source == Source::Folder && self.browser.open)
    }

    /// Present the current frame: render the Slint tree into the canvas,
    /// then flush.  Plain pages get one Full content-band refresh;
    /// overlays draw over a silently repainted base and flush their
    /// merged dirty region ONCE (Partial).
    pub fn present(&mut self) {
        let _t0 = std::time::Instant::now();
        self.drain_keyboard();
        // Complete any worker downloads (may auto-open the reader when a
        // single-book batch drains) before rendering.
        self.drain_downloads();
        let ov = self.overlay;
        let changed = self.dirty || ov != self.last_overlay;
        self.dirty = false;
        let overlay_switched = ov != self.last_overlay;
        self.last_overlay = ov;

        // The self strip re-stamps on the first present and whenever the
        // clock's minute rolls over.
        let stamp = self.self_panel > 0 && {
            let min = panel_minute();
            if min != self.last_panel_min {
                self.last_panel_min = min;
                true
            } else {
                false
            }
        };

        if !changed {
            // Unchanged frame: nothing to repaint (the emulator's full
            // redraw is ~1s, so skipping keeps event processing prompt —
            // and on e-ink it is the correct discipline).  Only the self
            // strip's minute rollover still re-stamps (band-only flush).
            if stamp {
                self.sync_ui();
                if let Some(r) = self.render_ui(false) {
                    let band = Rect {
                        x: 0,
                        y: self.content_bottom,
                        w: self.screen_width(),
                        h: self.self_panel,
                    };
                    let cl = r.intersect(&band);
                    if !cl.is_empty() {
                        self.fb().refresh(cl, eh_hal::RefreshMode::Partial);
                    }
                }
            }
            return;
        }

        self.sync_ui();

        let w = self.screen_width();
        if ov == Overlay::None {
            let full = overlay_switched; // an overlay just closed: clean slate
            let region = self.render_ui(full);
            let band = Rect { x: 0, y: 0, w, h: self.content_bottom };
            let _ = region;
            self.fb().refresh(band, eh_hal::RefreshMode::Full);
        } else {
            // Overlay frame: exactly ONE panel update per input.  Repaint
            // the base silently (full — the canvas may still carry the
            // previous overlay), draw the overlay onto the canvas, then
            // flush their merged dirty union once.
            let _ = self.render_ui(true);
            let mut dirty: Vec<Rect> = Vec::new();
            {
                let mut fb = self.fb.take().expect("fb");
                let scr = fb.screen();
                let fmt = fb.format();
                let stride = fb.stride();
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
                self.fb = Some(fb);
            }
            if let Some(u) = union_rects(&dirty) {
                self.fb().refresh(u, eh_hal::RefreshMode::Partial);
            }
        }

        // The self strip (painted by the render above — sync_ui refreshed
        // the clock text) flushes as its own band-only partial update.
        if stamp {
            let band = Rect {
                x: 0,
                y: self.content_bottom,
                w,
                h: self.self_panel,
            };
            self.fb().refresh(band, eh_hal::RefreshMode::Partial);
        }
    }

    /// Render the Slint tree into the canvas.  `full` forces a whole-
    /// window repaint (overlay open/close: the canvas carries stale
    /// overlay pixels the incremental buffer would keep).
    fn render_ui(&mut self, full: bool) -> Option<eh_hal::Rect> {
        let fb = self.fb.as_mut().expect("framebuffer bound");
        self.ui.render_full(fb, full)
    }

    // ── navigation / input ────────────────────────────────────────────

    /// Re-derive the layout geometry from the framebuffer after a live
    /// resolution switch (C sdl_set_resolution's EVT_REPAINT), then
    /// rebuild the current page.
    pub fn relayout(&mut self) {
        self.sync_fb_cache();
        let scr = self.fb.as_ref().expect("fb").screen();
        let (content_bottom, self_panel) = if self.fb.as_ref().expect("fb").needs_self_panel() {
            (scr.height.saturating_sub(106), 106)
        } else {
            (scr.content_height(), 0)
        };
        self.content_bottom = content_bottom;
        self.self_panel = self_panel;
        self.last_panel_min = -1;
        self.ui.set_size(scr.width, scr.height);
        self.refresh_shelf();
    }
}

/// "Weekday HH:MM" for the self-drawn status strip (real local time).
pub(crate) fn clock_label() -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    // The overlay flush region is the bounding box of the dirty rects:
    // too small clips paint (stale pixels), too big wastes an e-ink
    // waveform.  `None` must mean "nothing to flush".
    #[test]
    fn union_rects_empty_is_none() {
        assert_eq!(union_rects(&[]), None);
    }

    #[test]
    fn union_rects_single_rect_is_identity() {
        let r = Rect {
            x: 5,
            y: 7,
            w: 11,
            h: 13,
        };
        assert_eq!(union_rects(&[r]), Some(r));
    }

    #[test]
    fn union_rects_is_the_bounding_box() {
        let u = union_rects(&[
            Rect {
                x: 10,
                y: 20,
                w: 30,
                h: 40,
            },
            Rect {
                x: 0,
                y: 50,
                w: 100,
                h: 5,
            },
        ]);
        assert_eq!(
            u,
            Some(Rect {
                x: 0,
                y: 20,
                w: 100,
                h: 40
            })
        );
    }

    #[test]
    fn union_rects_nested_and_disjoint_members() {
        // A contained rect must not grow the box; order is irrelevant.
        let outer = Rect {
            x: 0,
            y: 0,
            w: 50,
            h: 50,
        };
        let inner = Rect {
            x: 10,
            y: 10,
            w: 5,
            h: 5,
        };
        assert_eq!(union_rects(&[inner, outer]), Some(outer));
        assert_eq!(union_rects(&[outer, inner]), union_rects(&[inner, outer]));
    }
}
