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
        c.set_overlay(match self.overlay {
            Overlay::None => crate::ui::EhOverlay::None,
            Overlay::More => crate::ui::EhOverlay::More,
            Overlay::Settings => crate::ui::EhOverlay::Settings,
            Overlay::Launcher => crate::ui::EhOverlay::Launcher,
            Overlay::Source => crate::ui::EhOverlay::Source,
            Overlay::Download => crate::ui::EhOverlay::Download,
            Overlay::Sync => crate::ui::EhOverlay::Sync,
            Overlay::Context => crate::ui::EhOverlay::Context,
            Overlay::GroupChooser => crate::ui::EhOverlay::GroupChooser,
            Overlay::SortChooser => crate::ui::EhOverlay::SortChooser,
            Overlay::LogViewer => crate::ui::EhOverlay::LogViewer,
            Overlay::Licenses => crate::ui::EhOverlay::Licenses,
            Overlay::LicenseDetail => crate::ui::EhOverlay::LicenseDetail,
        });
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
        c.set_page(self.page as i32);
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
            self.fb
                .as_ref()
                .and_then(|fb| fb.battery_level())
                .unwrap_or(0) as i32,
        );
        c.set_frontlight(
            self.fb
                .as_ref()
                .map(|fb| fb.frontlight_on())
                .unwrap_or(false),
        );
    }
    /// The top-bar title for the active body (browser path / drilled
    /// group / query / Search).
    fn ui_title(&self) -> String {
        if let Some(p) = &self.dl_picker {
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
        if ov != Overlay::None {
            self.sync_overlay();
            self.sync_overlay_pages();
        }

        let w = self.screen_width();
        // One render → one flush.  Plain pages deep-clean the whole
        // content band with a Full waveform; overlay frames flush only
        // the renderer's dirty region with ONE Partial update (the
        // flicker contract: never a bare-base flash between the two).
        let full = ov == Overlay::None && overlay_switched;
        let region = self.render_ui(full);
        if ov == Overlay::None {
            let band = Rect {
                x: 0,
                y: 0,
                w,
                h: self.content_bottom,
            };
            self.fb().refresh(band, eh_hal::RefreshMode::Full);
        } else if let Some(u) = region {
            self.fb().refresh(u, eh_hal::RefreshMode::Partial);
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
        let fb = self.fb.as_ref().expect("fb");
        let scr = fb.screen();
        let (content_bottom, self_panel, win_h) = if fb.needs_self_panel() {
            (scr.height.saturating_sub(106), 106, scr.height)
        } else {
            (scr.content_height(), 0, scr.content_height())
        };
        self.content_bottom = content_bottom;
        self.self_panel = self_panel;
        self.last_panel_min = -1;
        // The window is the canvas (see App::new): full panel when the
        // app draws its own strip, content band when the firmware does.
        self.ui.set_size(scr.width, win_h);
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

/// The clock's current minute (the self-panel strip's redraw cadence).
fn panel_minute() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 60)
        .unwrap_or(0)
}

impl<B: Framebuffer> App<B> {
    /// Push the active overlay's model into the Slint tree (called from
    /// present when an overlay is up).  Pure display data only — the
    /// state lives in the App fields.
    pub(crate) fn sync_overlay(&mut self) {
        use crate::widgets::chooser::{group_display_key, sort_display_key};
        // App-side probes first (cover_warm_active needs &mut for the net
        // probe; the property sets below hold an immutable borrow).
        let warm_drained = !self.cover_warm_active();
        let book_count = self.store.count().unwrap_or(0);
        let c = self.ui.comp();
        let w = self.screen_width();
        c.set_hatch(self.ui.hatch());
        match self.overlay {
            Overlay::More => {
                let labels: Vec<slint::SharedString> = crate::menu::label_keys()
                    .iter()
                    .map(|(_, key)| crate::i18n::tr(key).to_string().into())
                    .collect();
                c.set_more_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
                c.set_more_group_value(
                    crate::i18n::tr(group_display_key(self.group))
                        .to_string()
                        .into(),
                );
                c.set_more_sort_value(
                    crate::i18n::tr(sort_display_key(self.sort))
                        .to_string()
                        .into(),
                );
            }
            Overlay::Source => {
                c.set_source_title(crate::i18n::tr("source.title").to_string().into());
                let labels: Vec<slint::SharedString> =
                    [Source::Kavita, Source::Local, Source::Folder]
                        .iter()
                        .map(|s| crate::i18n::tr(s.ui_label_key()).to_string().into())
                        .collect();
                c.set_source_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
                c.set_source_selected(self.source as i32);
            }
            Overlay::GroupChooser | Overlay::SortChooser => {
                let (title, current, labels, selected) = match self.overlay {
                    Overlay::GroupChooser => {
                        let offer = self.group_offer();
                        let selected = offer
                            .iter()
                            .position(|g| *g == self.group)
                            .map(|i| i as i32)
                            .unwrap_or(-1);
                        let labels: Vec<slint::SharedString> = offer
                            .iter()
                            .map(|g| {
                                crate::i18n::tr(crate::widgets::chooser::GROUP_KEYS[*g as usize])
                                    .to_string()
                                    .into()
                            })
                            .collect();
                        (
                            crate::i18n::tr("action.group_by").to_string(),
                            crate::i18n::tr(group_display_key(self.group)).to_string(),
                            labels,
                            selected,
                        )
                    }
                    _ => {
                        let labels: Vec<slint::SharedString> = crate::widgets::chooser::SORT_KEYS
                            .iter()
                            .map(|k| crate::i18n::tr(k).to_string().into())
                            .collect();
                        (
                            crate::i18n::tr("action.sort_by").to_string(),
                            crate::i18n::tr(sort_display_key(self.sort)).to_string(),
                            labels,
                            self.sort as i32,
                        )
                    }
                };
                let rows = labels.len() as i32;
                c.set_chooser_title(title.into());
                c.set_chooser_current(current.into());
                c.set_chooser_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
                c.set_chooser_rows(rows);
                c.set_chooser_selected(selected);
            }
            Overlay::Context => {
                let labels: Vec<slint::SharedString> = self
                    .context
                    .items
                    .iter()
                    .map(|a| crate::i18n::tr(a.label_key()).to_string().into())
                    .collect();
                let rows = labels.len() as i32;
                c.set_context_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
                c.set_context_rows(rows);
            }
            Overlay::Download => {
                c.set_dl_title(crate::i18n::tr("dl.in_progress").to_string().into());
                let status = match self.dl.sheet_status(self.downloader.pending) {
                    crate::downloads::SheetStatus::Tally { done, failed } => format!(
                        "{}, {}",
                        crate::i18n::trn("dl.complete", &[done as i64]),
                        crate::i18n::trn("dl.failed_count", &[failed as i64])
                    ),
                    crate::downloads::SheetStatus::Remaining { count } => {
                        crate::i18n::trn("dl.remaining", &[count as i64]).to_string()
                    }
                };
                c.set_dl_status(status.into());
            }
            Overlay::Sync => {
                c.set_sync_title(crate::i18n::tr("action.sync").to_string().into());
                let p = &self.sync_popup;
                let (line, subline) = match p.stage {
                    crate::widgets::sync_popup::SyncStage::Meta => (
                        crate::i18n::tr("sync.meta").to_string(),
                        crate::i18n::trn("sync.batch", &[p.round as i64]).to_string(),
                    ),
                    crate::widgets::sync_popup::SyncStage::Scan => (
                        crate::i18n::tr("sync.scan").to_string(),
                        crate::i18n::trn("sync.books", &[p.scanned as i64]).to_string(),
                    ),
                    crate::widgets::sync_popup::SyncStage::Covers => (
                        crate::i18n::tr("sync.covers").to_string(),
                        if p.covers_total > 0 {
                            crate::i18n::trn(
                                "sync.cover_count",
                                &[p.covers_done as i64, p.covers_total as i64],
                            )
                            .to_string()
                        } else {
                            crate::i18n::tr("sync.covers").to_string()
                        },
                    ),
                    crate::widgets::sync_popup::SyncStage::Fail => {
                        (crate::i18n::tr("status.fail").to_string(), p.error.clone())
                    }
                    crate::widgets::sync_popup::SyncStage::Done => (
                        crate::i18n::tr("sync.done").to_string(),
                        crate::i18n::trn("sync.books", &[book_count]).to_string(),
                    ),
                };
                c.set_sync_line(line.into());
                c.set_sync_subline(subline.into());
                let show_bar =
                    p.stage == crate::widgets::sync_popup::SyncStage::Covers && p.covers_total > 0;
                let covers = (p.covers_done, p.covers_total);
                c.set_sync_show_bar(show_bar);
                if show_bar {
                    c.set_sync_bar_fill(covers.0 as f32 / covers.1.max(1) as f32);
                    // Striped overlay over the unfilled part while the warm
                    // pass still runs (C draw_sync_popup_sheet's diagonals).
                    let stripes = if !warm_drained {
                        let pw = (w * 3 / 4).saturating_sub(48);
                        let fill = (covers.0 * pw.saturating_sub(2)) / covers.1;
                        let mut path = String::new();
                        let mut sx = fill + 1;
                        while sx + 3 < pw.saturating_sub(1) {
                            path.push_str(&format!("M {sx} 1 L {} 11 ", sx + 2));
                            sx += 6;
                        }
                        path
                    } else {
                        String::new()
                    };
                    c.set_sync_stripes(stripes.into());
                }
            }
            _ => {}
        }
    }
}

impl<B: Framebuffer> App<B> {
    /// Viewer/launcher/settings page models (the full-screen overlays).
    pub(crate) fn sync_overlay_pages(&mut self) {
        // The launcher's layout pass mutates the app — run it before the
        // component borrow like the other &mut probes.
        let launcher_model = if self.overlay == Overlay::Launcher {
            crate::launcher::layout(self);
            let (top, body_h) = crate::launcher::body_rects(self);
            let (scroll, max_scroll) = crate::launcher::scroll_of(self);
            Some((
                self.launcher_items
                    .iter()
                    .zip(self.launcher_rects.iter())
                    .map(|(it, r)| (it.clone(), *r))
                    .collect::<Vec<_>>(),
                top,
                body_h,
                scroll,
                max_scroll,
            ))
        } else {
            None
        };
        let c = self.ui.comp();
        match self.overlay {
            Overlay::Settings => {
                c.set_settings_title(crate::i18n::tr("settings.title").to_string().into());
                let reader_val = self.reader_label();
                let sysapp_val = if crate::sysapp::detect() {
                    crate::i18n::tr("settings.sysapp_on").to_string()
                } else {
                    crate::i18n::tr("settings.sysapp_off").to_string()
                };
                let dl = self.config.downloads_dir.clone().unwrap_or_default();
                let labels: Vec<slint::SharedString> = [
                    crate::i18n::tr("settings.api_host"),
                    crate::i18n::tr("settings.api_key"),
                    crate::i18n::tr("settings.reader"),
                    crate::i18n::tr("settings.dl_dir"),
                    crate::i18n::tr("settings.system_app"),
                ]
                .iter()
                .map(|s| s.to_string().into())
                .collect();
                c.set_settings_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
                c.set_settings_api_host(self.config.api_url.clone().into());
                c.set_settings_api_key(self.config.api_token.clone().into());
                c.set_settings_reader(reader_val.into());
                c.set_settings_dl_dir(dl.into());
                c.set_settings_sysapp(sysapp_val.into());
                c.set_settings_editing(match self.kb_editing {
                    Some(KbField::ApiHost) => 0,
                    Some(KbField::ApiKey) => 1,
                    _ => -1,
                });
                c.set_settings_save(crate::i18n::tr("settings.save").to_string().into());
                c.set_settings_logs(crate::i18n::tr("settings.logs").to_string().into());
                c.set_settings_licenses(crate::i18n::tr("settings.licenses").to_string().into());
            }
            Overlay::LogViewer => {
                c.set_viewer_title(crate::i18n::tr("log.title").to_string().into());
                let w = self.screen_width();
                let shown = crate::viewer::log_path().display().to_string();
                let mut fitted = String::new();
                eh_render::fit_width(
                    crate::shelf::shelf_font(),
                    20.0,
                    &shown,
                    (w.saturating_sub(64)) as f32,
                    &mut fitted,
                );
                c.set_viewer_path(fitted.into());
                match crate::viewer::log_rows(w, self.content_bottom) {
                    None => {
                        c.set_viewer_empty(true);
                        c.set_viewer_empty_text(crate::i18n::tr("log.empty").to_string().into());
                        c.set_viewer_rows(slint::ModelRc::new(slint::VecModel::from(Vec::<
                            slint::SharedString,
                        >::new(
                        ))));
                        c.set_viewer_up(false);
                        c.set_viewer_down(false);
                    }
                    Some((text, rows)) => {
                        c.set_viewer_empty(false);
                        let rows_vis = crate::viewer::log_rows_vis(self.content_bottom);
                        let nrows = rows.len();
                        let max_first = nrows.saturating_sub(rows_vis);
                        let first = if self.log_scroll < 0 {
                            max_first
                        } else {
                            let f = (self.log_scroll as usize).min(max_first);
                            self.log_scroll = f as i32;
                            f
                        };
                        let vis: Vec<slint::SharedString> = (0..rows_vis)
                            .take(rows.len().saturating_sub(first))
                            .map(|i| {
                                let r = &rows[first + i];
                                text[r.start..r.end].to_string().into()
                            })
                            .collect();
                        c.set_viewer_rows(slint::ModelRc::new(slint::VecModel::from(vis)));
                        c.set_viewer_up(first > 0);
                        c.set_viewer_down(first < max_first);
                    }
                }
            }
            Overlay::Licenses => {
                c.set_viewer_title(crate::i18n::tr("licenses.title").to_string().into());
                let h = self.content_bottom;
                let btn_y = h.saturating_sub(8 + crate::appui::SCROLL_BTN_H);
                let body_h = btn_y.saturating_sub(crate::viewer::LIC_LIST_TOP + 8);
                let rows_vis = ((body_h / crate::viewer::LIC_LIST_H).max(1)) as usize;
                let max_first = crate::viewer::LICENSES.len().saturating_sub(rows_vis);
                let first = {
                    let f = (self.lic_scroll.max(0) as usize).min(max_first);
                    self.lic_scroll = f as i32;
                    f
                };
                let mut names = Vec::new();
                let mut kinds = Vec::new();
                for i in 0..rows_vis {
                    let idx = first + i;
                    if idx >= crate::viewer::LICENSES.len() {
                        break;
                    }
                    let lic = &crate::viewer::LICENSES[idx];
                    names.push(lic.name.to_string().into());
                    kinds.push(lic.kind.to_string().into());
                }
                c.set_lic_names(slint::ModelRc::new(slint::VecModel::from(names)));
                c.set_lic_kinds(slint::ModelRc::new(slint::VecModel::from(kinds)));
                c.set_viewer_up(first > 0);
                c.set_viewer_down(first < max_first);
            }
            Overlay::LicenseDetail => {
                let w = self.screen_width();
                let sel = self
                    .license_selected
                    .map(|i| i.min(crate::viewer::LICENSES.len() - 1))
                    .unwrap_or(0);
                let lic = &crate::viewer::LICENSES[sel];
                c.set_viewer_title(lic.name.to_string().into());
                let band = format!("{}  \u{b7}  {}", lic.kind, lic.note);
                let mut fitted = String::new();
                eh_render::fit_width(
                    crate::shelf::shelf_font(),
                    20.0,
                    &band,
                    (w.saturating_sub(64)) as f32,
                    &mut fitted,
                );
                c.set_lic_attribution(fitted.into());
                let h = self.content_bottom;
                let btn_y = h.saturating_sub(8 + crate::appui::SCROLL_BTN_H);
                let body_h = btn_y
                    .saturating_sub(crate::viewer::LOG_BODY_TOP + 8)
                    .max(crate::viewer::LOG_ROW_H);
                let rows_vis = (body_h / crate::viewer::LOG_ROW_H).max(1) as usize;
                let rows = crate::wrap::wrap_rows_forward(
                    crate::shelf::shelf_font(),
                    crate::viewer::LOG_FONT,
                    lic.text,
                    (w.saturating_sub(48)) as f32,
                    512,
                );
                let max_first = rows.len().saturating_sub(rows_vis);
                let first = {
                    let f = (self.lic_scroll.max(0) as usize).min(max_first);
                    self.lic_scroll = f as i32;
                    f
                };
                let vis: Vec<slint::SharedString> = (0..rows_vis)
                    .take(rows.len().saturating_sub(first))
                    .filter_map(|i| {
                        let r = &rows[first + i];
                        if r.blank {
                            None
                        } else {
                            Some(lic.text[r.start..r.end].to_string().into())
                        }
                    })
                    .collect();
                c.set_viewer_rows(slint::ModelRc::new(slint::VecModel::from(vis)));
                c.set_viewer_up(first > 0);
                c.set_viewer_down(first < max_first);
            }
            Overlay::Launcher => {
                let Some((items_snapshot, _top, body_h, scroll, max_scroll)) = launcher_model
                else {
                    return;
                };
                c.set_viewer_title(crate::i18n::tr("launcher.title").to_string().into());
                c.set_launcher_empty(crate::i18n::tr("launcher.empty").to_string().into());
                let mut entries = Vec::with_capacity(items_snapshot.len());
                for (it, r) in items_snapshot.iter() {
                    let (text, label2, letter) = if it.group {
                        (it.text.clone(), String::new(), String::new())
                    } else {
                        let (a, b) = crate::launcher::split_label(&it.text, r.w as i32 - 8);
                        let letter = it.text.chars().next().map(String::from).unwrap_or_default();
                        (a, b, letter)
                    };
                    let icon = match &it.art {
                        Some((rgb, iw, ih)) => slint::Image::from_rgb8(slint::SharedPixelBuffer::<
                            slint::Rgb8Pixel,
                        >::clone_from_slice(
                            rgb, *iw, *ih
                        )),
                        None => slint::Image::default(),
                    };
                    entries.push(crate::ui::LauncherEntry {
                        is_group: it.group,
                        text: text.into(),
                        letter: letter.into(),
                        label2: label2.into(),
                        icon,
                        has_icon: it.art.is_some(),
                        x: r.x as i32,
                        // layout y; the body adds its top and the scroll
                        y: r.y as i32,
                        w: r.w as i32,
                        h: r.h as i32,
                    });
                }
                c.set_launcher_items(slint::ModelRc::new(slint::VecModel::from(entries)));
                c.set_launcher_scroll(0); // offset baked into the entries
                c.set_launcher_body_h(body_h as i32);
                c.set_launcher_up(scroll > 0);
                c.set_launcher_down(scroll < max_scroll);
            }
            _ => {}
        }
    }
}
