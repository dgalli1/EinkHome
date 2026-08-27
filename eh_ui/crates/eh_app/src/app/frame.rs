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
            self.app_kb = fb.needs_app_keyboard();
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
            Overlay::Detail => crate::ui::EhOverlay::Detail,
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
        c.set_picker_mode(self.dl_picker.is_some());
        c.set_picker_select_label(crate::i18n::tr("settings.dl_use").to_string().into());
        c.set_grid_mode(self.view_mode == ViewMode::Grid);
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
        let (icon, icon_inv) = self.ui.source_images(self.source);
        c.set_source_icon(icon);
        c.set_source_icon_inv(icon_inv);
        // search page + the app-side on-screen keyboard (SDL hosts):
        // visible while any firmware-keyboard edit is open, showing the
        // live buffer.  The buffer read must precede the comp borrow
        // (fb() needs &mut self).
        let kb_open = self.search_kb || self.kb_editing.is_some();
        let kb_text = if kb_open {
            self.fb().live_keyboard_text().unwrap_or_default()
        } else {
            String::new()
        };
        let c = self.ui.comp();
        c.set_query(self.query.clone().into());
        c.set_search_placeholder(crate::i18n::tr("search.ph").to_string().into());
        c.set_search_kb(self.search_kb);
        c.set_osk_visible(self.app_kb && kb_open);
        c.set_kb_title(
            match self.kb_editing {
                Some(KbField::ApiHost) => crate::i18n::tr("settings.api_host"),
                Some(KbField::ApiKey) => crate::i18n::tr("settings.api_key"),
                _ => crate::i18n::tr("tab.search"),
            }
            .to_string()
            .into(),
        );
        c.set_kb_text(kb_text.into());
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
            // Slint-internal dirt without App state (a TouchArea press on
            // a chrome button): draw it and flush just the changed region
            // as a Partial — never a full-band flash for a press.
            else if let Some(r) = self.render_ui(false) {
                self.fb().refresh(r, eh_hal::RefreshMode::Partial);
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
        let fb = self.fb.as_mut().expect("fb");
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
            Overlay::Detail => {
                let Some(book) = self.detail_book.clone() else {
                    return;
                };
                c.set_detail_title(book.title.clone().into());
                // cover: cache first (degenerate placeholder entries are
                // unlinked by the loader), then the local extraction path
                let art = crate::cover::load_valid_rgb(&self.covers_dir, &book.id).or_else(|| {
                    let book_ref = &book;
                    self.local_cover_art(book_ref)
                });
                match &art {
                    Some((rgb, w, h)) => {
                        c.set_detail_cover(slint::Image::from_rgb8(slint::SharedPixelBuffer::<
                            slint::Rgb8Pixel,
                        >::clone_from_slice(
                            rgb, *w, *h
                        )));
                        c.set_detail_has_cover(true);
                    }
                    None => {
                        c.set_detail_cover(slint::Image::default());
                        c.set_detail_has_cover(false);
                    }
                }
                // metadata rows: every field the store carries
                let pct = crate::progress::percent(&self.progress, &book.local_path);
                let series = if book.series.is_empty() {
                    String::new()
                } else if book.series_idx > 0.0 {
                    format!("{} #{:0>2}", book.series, book.series_idx.round() as i64)
                } else {
                    book.series.clone()
                };
                let year = crate::store::year_of(book.added_at).unwrap_or_default();
                let added = crate::store::ymd_of(book.added_at)
                    .map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
                    .unwrap_or_else(|| "–".to_string());
                let size = fmt_size(book.size);
                let fmt = if book.ext.is_empty() {
                    "–".to_string()
                } else {
                    format!("{} · {}", book.ext.to_uppercase(), size)
                };
                let progress = if pct > 0 {
                    format!("{pct} %")
                } else {
                    "–".to_string()
                };
                let source = match book.source.as_str() {
                    "local" => crate::i18n::tr("source.local").to_string(),
                    "folder" => crate::i18n::tr("source.folder").to_string(),
                    _ => crate::i18n::tr("source.kavita").to_string(),
                };
                let downloaded = if book.downloaded {
                    crate::i18n::tr("detail.yes").to_string()
                } else {
                    crate::i18n::tr("detail.no").to_string()
                };
                let path = if book.local_path.is_empty() {
                    "–".to_string()
                } else {
                    book.local_path.clone()
                };
                let rows: Vec<crate::ui::DetailRow> = [
                    ("detail.author", book.author.clone()),
                    ("detail.series", series),
                    ("detail.year", year),
                    (
                        "detail.genre",
                        if book.genre.is_empty() {
                            "–".into()
                        } else {
                            book.genre
                        },
                    ),
                    (
                        "detail.format",
                        if book.ext.is_empty() {
                            "–".into()
                        } else {
                            fmt
                        },
                    ),
                    ("detail.added", added),
                    ("detail.progress", progress),
                    ("detail.source", source),
                    ("detail.downloaded", downloaded),
                    ("detail.path", path),
                ]
                .iter()
                .map(|(k, v)| crate::ui::DetailRow {
                    label: crate::i18n::tr(k).to_string().into(),
                    value: v.into(),
                })
                .collect();
                c.set_detail_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
            }
            Overlay::Settings => {
                c.set_settings_title(crate::i18n::tr("settings.title").to_string().into());
                let reader_val = self.reader_label();
                let sysapp_on = crate::sysapp::detect();
                let sysapp_val = if sysapp_on {
                    crate::i18n::tr("settings.sysapp_on").to_string()
                } else {
                    crate::i18n::tr("settings.sysapp_off").to_string()
                };
                let dl = self.config.downloads_dir.clone().unwrap_or_default();
                // The Local row shows the EFFECTIVE base: the configured
                // folder, else the storage-root default.
                let local_dir = self
                    .config
                    .local_dir
                    .clone()
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(crate::local::browse_root);
                // (label key, value) per card, in display order; the
                // System-app card only exists where the platform supports
                // a home-task override.
                let mut rows: Vec<(&str, String)> = vec![
                    ("settings.api_host", self.config.api_url.clone()),
                    ("settings.api_key", self.config.api_token.clone()),
                    ("settings.reader", reader_val),
                    ("settings.dl_dir", dl),
                    ("settings.local_dir", local_dir),
                ];
                let sysapp_supported = crate::sysapp::platform_supported();
                if sysapp_supported {
                    rows.push(("settings.system_app", sysapp_val));
                }
                let labels: Vec<slint::SharedString> = rows
                    .iter()
                    .map(|(k, _)| crate::i18n::tr(k).to_string().into())
                    .collect();
                let sysapp_idx = if sysapp_supported {
                    (labels.len() - 1) as i32
                } else {
                    -1
                };
                let values: Vec<slint::SharedString> =
                    rows.into_iter().map(|(_, v)| v.into()).collect();
                let n_rows = labels.len() as i32;
                c.set_settings_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
                c.set_settings_values(slint::ModelRc::new(slint::VecModel::from(values)));
                c.set_settings_card_rows(n_rows);
                c.set_settings_sysapp_idx(sysapp_idx);
                c.set_settings_sysapp_on(sysapp_on);
                c.set_settings_editing(match self.kb_editing {
                    Some(KbField::ApiHost) => 0,
                    Some(KbField::ApiKey) => 1,
                    _ => -1,
                });
                c.set_settings_save(crate::i18n::tr("settings.save").to_string().into());
                c.set_settings_logs(crate::i18n::tr("settings.logs").to_string().into());
                c.set_settings_licenses(crate::i18n::tr("settings.licenses").to_string().into());
                c.set_settings_reset(crate::i18n::tr("settings.reset_db").to_string().into());
            }
            Overlay::LogViewer => {
                c.set_viewer_title(crate::i18n::tr("log.title").to_string().into());
                let w = self.screen_width();
                let shown = crate::viewer::log_path().display().to_string();
                let mut fitted = String::new();
                eh_render::fit_width(
                    crate::shelf::shelf_font(),
                    crate::viewer::LOG_FONT,
                    &shown,
                    (w.saturating_sub(32)) as f32,
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
                    crate::viewer::LOG_FONT,
                    &band,
                    (w.saturating_sub(32)) as f32,
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
                    (w.saturating_sub(32)) as f32,
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
                // the entries carry raw layout y; the body subtracts the
                // scroll (launcher.slint: y: it.y - root.scroll)
                c.set_launcher_scroll(scroll);
                c.set_launcher_body_h(body_h as i32);
                c.set_launcher_up(scroll > 0);
                c.set_launcher_down(scroll < max_scroll);
            }
            _ => {}
        }
    }
}

/// Human-readable file size for the Detail page (C eh_draw_details' size).
fn fmt_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{b} B")
    }
}

#[cfg(test)]
mod self_panel_tests {
    use super::*;
    use crate::app::tests::FakeKb;

    /// The self-drawn status strip renders through the Slint tree: with
    /// needs_self_panel the window covers the full panel and the strip
    /// band below the content carries the clock text (dark pixels) after
    /// the first present.
    #[test]
    fn self_panel_strip_stamps_clock_into_the_band() {
        let dir = std::env::temp_dir().join(format!("eh_slint_panel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fb = FakeKb::with_panel(1072, 1448, true);
        let cfg = Config {
            api_url: "http://mock.invalid".into(),
            ..Default::default()
        };
        let mut app = App::new(fb, cfg, None, &dir);
        app.present();

        let px = app.fb().surface_mut().to_vec();
        let w = 1072usize;
        let band_y0 = (1448 - 106) as usize;
        // The band: white background, black top rule, dark clock text.
        let band = &px[band_y0 * w..(band_y0 + 106) * w];
        let dark = band.iter().filter(|p| **p < 80).count();
        assert!(
            dark > 200,
            "strip band must carry the clock glyph ink (dark={dark})"
        );
        // The top rule of the strip: a dark 2px line across the band's top.
        let rule = &px[band_y0 * w..band_y0 * w + 2 * w];
        assert!(
            rule.iter().filter(|p| **p < 80).count() > w * 9 / 10,
            "strip top rule missing"
        );
        // Above the band (content bottom edge): not part of the strip.
        app.rebuild_view();
        app.present();
    }

    /// The corner scroll buttons must carry chevron ink: commit db14552
    /// rewired the icon pushes and dropped set_chevron/set_chevron_down,
    /// leaving the ScrollButtons (launcher grid + both viewers) with an
    /// empty image — a blank bordered box.  With the launcher up, the
    /// bottom-corner button bands must hold dark glyph pixels (the grey
    /// disabled border is 0xaa, so only the chevron counts as dark).
    #[test]
    fn launcher_scroll_buttons_stamp_chevron_ink() {
        let dir = std::env::temp_dir().join(format!("eh_slint_scroll_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fb = FakeKb::with_panel(1072, 1448, false);
        let cfg = Config {
            api_url: "http://mock.invalid".into(),
            ..Default::default()
        };
        let mut app = App::new(fb, cfg, None, &dir);
        app.set_overlay(crate::app::Overlay::Launcher);
        app.present();

        let px = app.fb().surface_mut().to_vec();
        let w = 1072usize;
        // Each button is 150x96 at the window bottom; the 48x48 chevron
        // is centred, so sample the inner region clear of the 2px border.
        let band = |x0: usize| -> usize {
            (1360..1444)
                .map(|y| &px[y * w + x0..y * w + x0 + 146])
                .map(|row| row.iter().filter(|p| **p < 80).count())
                .sum()
        };
        let left = band(2);
        let right = band(1072 - 150 + 2);
        assert!(left > 50, "up-chevron ink missing (dark={left})");
        assert!(right > 50, "down-chevron ink missing (dark={right})");
    }
}
