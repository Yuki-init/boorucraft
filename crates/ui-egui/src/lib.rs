//! BooruCraft's desktop UI: search bar, collections, thumbnail grid, viewer and tag editor.
//!
//! The UI is thin: it reads [`library::Library`] and changes it through its methods, which use
//! the same store and index functions as the command-line tool.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod dupes_view;
pub mod fonts;
pub mod library;
pub mod selection;
pub mod suggest;
pub mod thumbnails;
pub mod viewer;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boorucraft_core::{Rating, normalize_tag};
use boorucraft_thumbs::ThumbCache;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use library::{CollState, Config, Dirs, Hit, Library, PoolMove};
use selection::Selection;
use thumbnails::{Thumb, Thumbnails};
use viewer::Viewer;

const SEARCH_ID: &str = "search";
const TAG_INPUT_ID: &str = "tag-input";
const POOL_INPUT_ID: &str = "pool-input";
const GAP: f32 = 6.0;
const ACCENT: Color32 = Color32::from_rgb(90, 150, 240);
/// Most tags listed in the sidebar.
const MAX_SIDEBAR_TAGS: usize = 300;

/// What the central area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Grid,
    Duplicates,
}

/// Start-up options (from the command line).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Open these collections instead of the remembered ones, without remembering them.
    pub collections: Option<Vec<PathBuf>>,
    pub query: Option<String>,
    /// Select these results once the scans are done.
    pub select: Vec<usize>,
    /// Open the viewer on this result once the scans are done.
    pub view: Option<usize>,
    /// Save a screenshot this long after the scans are done, then quit.
    pub screenshot: Option<PathBuf>,
    pub screenshot_after: Duration,
    /// Put the keyboard focus here at start-up: `search` or `tags`.
    pub focus: Option<String>,
    /// Start on the Duplicates screen.
    pub duplicates: bool,
}

pub struct App {
    lib: Library,
    thumbs: Thumbnails,
    query: String,
    searched: Option<String>,
    sel: Selection,
    viewer: Option<Viewer>,
    tile: f32,
    columns: usize,
    /// Scroll the grid so this result is visible.
    scroll_to: Option<usize>,
    /// Grid scroll offset and height from the last frame.
    grid_view: (f32, f32),
    tag_input: String,
    pool_input: String,
    /// Tags typed that aren't in the tag list, waiting for "Create".
    unknown_typed: Vec<String>,
    source_edit: Option<(Hit, String)>,
    focus: Option<&'static str>,
    options: Options,
    startup_done: bool,
    ready_at: Option<Instant>,
    screenshot_requested: bool,
    screen: Screen,
    /// The copy to keep in each duplicate group, by the group's first hash (default: the best).
    dupe_keep: HashMap<String, Hit>,
    confirm_trash: Option<dupes_view::TrashPlan>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, options: Options) -> App {
        let ctx = cc.egui_ctx.clone();
        let cjk = fonts::install(&ctx);
        if cjk.is_none() {
            eprintln!("boorucraft: no CJK font found; Japanese, Chinese and Korean text may not show");
        }
        ctx.set_visuals(egui::Visuals::dark());
        let dirs = Dirs::system();
        let config = Config::load(&dirs);
        let tile = config.tile_size.unwrap_or(160.0).clamp(80.0, 360.0);
        let waker_ctx = ctx.clone();
        let lib = Library::new(dirs.clone(), config, options.collections.clone(), Arc::new(move || waker_ctx.request_repaint()));
        let thumbs = Thumbnails::new(ThumbCache::new(&dirs.cache), ctx);
        let query = options.query.clone().unwrap_or_default();
        let screen = if options.duplicates { Screen::Duplicates } else { Screen::Grid };
        App {
            lib,
            thumbs,
            query,
            searched: None,
            sel: Selection::default(),
            viewer: None,
            tile,
            columns: 1,
            scroll_to: None,
            grid_view: (0.0, 0.0),
            tag_input: String::new(),
            pool_input: String::new(),
            unknown_typed: Vec::new(),
            source_edit: None,
            focus: None,
            options,
            startup_done: false,
            ready_at: None,
            screenshot_requested: false,
            screen,
            dupe_keep: HashMap::new(),
            confirm_trash: None,
        }
    }

    fn run_search(&mut self) {
        let before = self.lib.hits.clone();
        self.lib.search(&self.query);
        self.searched = Some(self.query.clone());
        if self.lib.hits != before {
            self.sel.clear();
            self.viewer = None;
        }
    }

    /// The files the tag editor and rating keys act on.
    fn targets(&self) -> Vec<Hit> {
        if let Some(v) = &self.viewer {
            return vec![v.hit.clone()];
        }
        self.sel.items.iter().filter_map(|&i| self.lib.hits.get(i).cloned()).collect()
    }

    fn open_result(&mut self, pos: usize, ctx: &egui::Context) {
        let Some(hit) = self.lib.hits.get(pos).cloned() else { return };
        let format = self.lib.entry(&hit).map(|e| e.format.clone()).unwrap_or_default();
        let path = self.lib.path_of(&hit).filter(|p| p.exists());
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        self.sel.only(pos);
        self.viewer = Some(Viewer::open(pos, hit, path, &format, max_side.max(1024), ctx));
    }

    fn open_external(&mut self, pos: usize) {
        let Some(hit) = self.lib.hits.get(pos).cloned() else { return };
        match self.lib.path_of(&hit) {
            Some(p) => {
                if let Err(e) = viewer::open_external(&p) {
                    self.lib.notify(e);
                }
            }
            None => self.lib.notify("This file is missing"),
        }
    }

    fn rate(&mut self, rating: Option<Rating>) {
        let targets = self.targets();
        if !targets.is_empty() {
            self.lib.set_rating(&targets, rating);
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let events: Vec<(Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, m) in events {
            if typing {
                if key == Key::Escape {
                    ctx.memory_mut(|mem| {
                        if let Some(id) = mem.focused() {
                            mem.surrender_focus(id);
                        }
                    });
                }
                continue;
            }
            if self.screen == Screen::Duplicates && self.viewer.is_none() {
                match key {
                    Key::Escape => self.screen = Screen::Grid,
                    Key::Slash => {
                        self.screen = Screen::Grid;
                        self.focus = Some(SEARCH_ID);
                    }
                    _ => {}
                }
                continue;
            }
            let n = self.lib.hits.len();
            match key {
                Key::Slash => self.focus = Some(SEARCH_ID),
                Key::T => self.focus = Some(TAG_INPUT_ID),
                Key::Num1 => self.rate(Some(Rating::Safe)),
                Key::Num2 => self.rate(Some(Rating::Questionable)),
                Key::Num3 => self.rate(Some(Rating::Explicit)),
                Key::Num0 => self.rate(None),
                Key::OpenBracket => self.move_in_pool(if m.shift { PoolMove::First } else { PoolMove::Earlier }),
                Key::CloseBracket => self.move_in_pool(if m.shift { PoolMove::Last } else { PoolMove::Later }),
                _ => {}
            }
            if let Some(v) = &mut self.viewer {
                let pos = v.pos;
                match key {
                    Key::Escape | Key::Backspace => self.viewer = None,
                    Key::ArrowRight | Key::Space if pos + 1 < n => self.open_result(pos + 1, ctx),
                    Key::ArrowLeft if pos > 0 => self.open_result(pos - 1, ctx),
                    Key::F => v.fit(),
                    Key::Z => v.actual_size(),
                    Key::Plus | Key::Equals => v.zoom_by(1.25, ctx.content_rect(), None),
                    Key::Minus => v.zoom_by(0.8, ctx.content_rect(), None),
                    Key::O | Key::Enter => self.open_external(pos),
                    _ => {}
                }
                if self.viewer.is_none() {
                    self.scroll_to = Some(pos);
                }
                continue;
            }
            let cols = self.columns.max(1) as isize;
            let mut moved = true;
            match key {
                Key::ArrowRight => self.sel.move_by(1, n, m.shift),
                Key::ArrowLeft => self.sel.move_by(-1, n, m.shift),
                Key::ArrowDown => self.sel.move_by(cols, n, m.shift),
                Key::ArrowUp => self.sel.move_by(-cols, n, m.shift),
                Key::PageDown => self.sel.move_by(cols * 4, n, m.shift),
                Key::PageUp => self.sel.move_by(-cols * 4, n, m.shift),
                Key::Home if n > 0 => self.sel.move_to(0, m.shift),
                Key::End if n > 0 => self.sel.move_to(n - 1, m.shift),
                Key::A if m.command => self.sel.select_all(n),
                _ => moved = false,
            }
            if moved {
                self.scroll_to = self.sel.cursor;
                continue;
            }
            match key {
                Key::Escape => self.sel.clear(),
                Key::Enter => {
                    if let Some(c) = self.sel.cursor {
                        self.activate(c, ctx);
                    }
                }
                Key::O => {
                    if let Some(c) = self.sel.cursor {
                        self.open_external(c);
                    }
                }
                _ => {}
            }
        }
    }

    /// Moves the selected files within the pool being viewed, keeping them selected.
    fn move_in_pool(&mut self, to: PoolMove) {
        let Some(pool) = self.lib.pool_view.clone() else { return };
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let cursor = self.sel.cursor.and_then(|c| self.lib.hits.get(c)).cloned();
        self.lib.move_in_pool(&pool, &targets, to);
        self.lib.search(&self.query);
        self.searched = Some(self.query.clone());
        // Follow the moved files.
        self.sel.clear();
        for (i, h) in self.lib.hits.iter().enumerate() {
            if targets.contains(h) {
                self.sel.click(i, true, false);
            }
        }
        if let Some(c) = cursor.and_then(|c| self.lib.hits.iter().position(|h| *h == c)) {
            self.sel.cursor = Some(c);
            self.scroll_to = Some(c);
        }
        if let Some(v) = &mut self.viewer
            && let Some(p) = self.lib.hits.iter().position(|h| *h == v.hit)
        {
            v.pos = p;
        }
    }

    /// Double-click or Enter: the viewer for pictures BooruCraft can show, else the default app.
    fn activate(&mut self, pos: usize, ctx: &egui::Context) {
        let decodable = self.lib.hits.get(pos).and_then(|h| self.lib.entry(h)).is_some_and(|e| boorucraft_thumbs::can_decode(&e.format));
        if decodable {
            self.open_result(pos, ctx);
        } else {
            self.sel.only(pos);
            self.open_external(pos);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let count = format!("{} results", self.lib.hits.len());
            let right_width = 450.0;
            let width = (ui.available_width() - right_width).max(160.0);
            let bad = self.lib.query_error.is_some();
            let err_span = self.lib.query_error.as_ref().map(|(_, s)| s.clone());
            let text_color = ui.visuals().text_color();
            let mut layouter = move |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                let job = highlight_query(buf.as_str(), err_span.as_ref(), text_color);
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let edit = egui::TextEdit::singleline(&mut self.query)
                .hint_text("Search: cat -dog ~sky ~sea cat* rating:s width:>1920 order:date   (press / to focus)")
                .desired_width(width)
                .font(FontId::proportional(15.0))
                .layouter(&mut layouter);
            let frame_color = if bad { Color32::from_rgb(220, 90, 80) } else { Color32::TRANSPARENT };
            let (tags, usage) = (&self.lib.tags, self.lib.usage());
            egui::Frame::NONE.stroke(Stroke::new(1.0, frame_color)).corner_radius(CornerRadius::same(4)).show(ui, |ui| {
                egui::CompletionPopup::new(egui::Id::unique(SEARCH_ID))
                    .align(egui::RectAlign::BOTTOM_START)
                    .max_height(340.0)
                    .show(ui, edit, |q| suggest::for_search(q.word, usage, tags).into_iter().map(|s| popup_item(tags, s)).collect())
            });
            if self.focus == Some(SEARCH_ID) {
                focus_at_end(ui.ctx(), egui::Id::unique(SEARCH_ID), &self.query);
                self.focus = None;
            }
            ui.label(RichText::new(count).color(Color32::from_gray(170)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let groups = self.lib.dupe_groups.len();
                let on = self.screen == Screen::Duplicates;
                let label = if groups > 0 { format!("Duplicates ({groups})") } else { "Duplicates".to_string() };
                if ui.selectable_label(on, label).on_hover_text("Review near-duplicate pictures").clicked() {
                    self.screen = if on { Screen::Grid } else { Screen::Duplicates };
                    self.viewer = None;
                }
                if ui.button("Add folder…").on_hover_text("Open a folder as a collection").clicked()
                    && let Some(dir) = rfd::FileDialog::new().set_title("Choose a picture folder").pick_folder()
                {
                    self.lib.open_collection(&dir, true);
                }
                let mut tile = self.tile;
                if ui.add(egui::Slider::new(&mut tile, 80.0..=360.0).show_value(false)).on_hover_text("Thumbnail size").changed() {
                    self.tile = tile;
                }
                if ui.ctx().input(|i| i.pointer.any_released()) && self.lib.config.tile_size != Some(self.tile) {
                    self.lib.set_tile_size(self.tile);
                }
                ui.label("Size");
            });
        });
        if let Some((msg, span)) = &self.lib.query_error {
            let term = self.query.get(span.clone()).unwrap_or("");
            ui.label(RichText::new(format!("`{term}`: {msg}")).color(Color32::from_rgb(230, 120, 110)));
        }
    }

    fn collections_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Collections");
        ui.add_space(4.0);
        if self.lib.colls.is_empty() {
            ui.label(RichText::new("No folders yet. Use “Add folder…” to open one.").color(Color32::from_gray(150)));
        }
        let mut rescan = None;
        let mut close = None;
        for (i, oc) in self.lib.colls.iter().enumerate() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&oc.name).strong()).on_hover_text(oc.root.display().to_string());
                let status = match &oc.state {
                    CollState::Scanning => "Scanning…".to_string(),
                    CollState::Failed(e) => format!("Error: {e}"),
                    CollState::Ready { c, .. } => {
                        let mut s = format!("{} files", c.len());
                        if c.is_read_only() {
                            s.push_str(", read-only");
                        }
                        if oc.dirty_since.is_some() {
                            s.push_str(", saving…");
                        }
                        if oc.save_error.is_some() {
                            s.push_str(", not saved, will retry");
                        }
                        s
                    }
                };
                ui.label(RichText::new(status).small().color(Color32::from_gray(160)));
                if let Some(r) = &oc.last_report {
                    ui.label(RichText::new(r).small().color(Color32::from_gray(120)));
                }
                ui.horizontal(|ui| {
                    let ready = matches!(oc.state, CollState::Ready { .. });
                    if ui.add_enabled(ready && !oc.read_only(), egui::Button::new("Rescan").small()).clicked() {
                        rescan = Some(i);
                    }
                    if ui
                        .add_enabled(!self.lib.is_busy(), egui::Button::new("Close").small())
                        .on_hover_text("Stop showing this folder (nothing is deleted)")
                        .clicked()
                    {
                        close = Some(i);
                    }
                });
            });
        }
        if let Some(i) = rescan {
            self.lib.start_scan(i);
        }
        if let Some(i) = close {
            self.lib.close_collection(i);
            self.searched = None;
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let sel = self.sel.len();
            if sel > 0 {
                ui.label(format!("{sel} selected"));
                ui.separator();
            }
            if self.lib.is_scanning() {
                ui.spinner();
                ui.label("Scanning…");
                ui.separator();
            } else if let Some((done, total)) = self.lib.hash_progress() {
                ui.spinner();
                ui.label(format!("Looking for duplicates: {done} / {total}"));
                ui.separator();
            }
            let loading = self.thumbs.loading();
            if loading > 0 {
                ui.label(RichText::new(format!("{loading} thumbnails loading")).color(Color32::from_gray(150)));
                ui.separator();
            }
            if self.lib.has_unsaved() {
                ui.label(RichText::new("Unsaved changes").color(Color32::from_gray(150)));
                ui.separator();
            }
            let recent = self.lib.messages.iter().rev().find(|(_, at)| at.elapsed() < Duration::from_secs(8));
            if let Some((msg, _)) = recent {
                ui.label(RichText::new(msg).color(Color32::from_rgb(240, 190, 110)));
            }
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let n = self.lib.hits.len();
        if n == 0 {
            ui.centered_and_justified(|ui| {
                let msg = if self.lib.colls.is_empty() {
                    "Add a folder to start."
                } else if self.lib.is_scanning() {
                    "Scanning…"
                } else {
                    "Nothing matches this search."
                };
                ui.label(RichText::new(msg).size(16.0).color(Color32::from_gray(150)));
            });
            return;
        }
        let cols = ((ui.available_width() + GAP) / (self.tile + GAP)).floor().max(1.0) as usize;
        // Stretch the tiles a little so the row fills the width.
        let tile = ((ui.available_width() + GAP) / cols as f32 - GAP).max(self.tile);
        let cell = tile + GAP;
        self.columns = cols;
        let rows = n.div_ceil(cols);
        let mut area = egui::ScrollArea::vertical().id_salt("grid").auto_shrink([false, false]);
        if let Some(target) = self.scroll_to.take() {
            let top = (target / cols) as f32 * cell;
            let (offset, height) = self.grid_view;
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if top + cell > offset + height {
                area = area.vertical_scroll_offset((top + cell - height).max(0.0));
            }
        }
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        let mut activated: Option<usize> = None;
        let out = area.show_rows(ui, cell, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(GAP, GAP);
                    for col in 0..cols {
                        let i = row * cols + col;
                        let Some(hit) = self.lib.hits.get(i) else { break };
                        let page = self.lib.pool_view.as_deref().and_then(|p| self.lib.entry(hit)?.pools.get(p).copied());
                        let resp = draw_tile(ui, &self.lib, &mut self.thumbs, hit, tile, self.sel.items.contains(&i), self.sel.cursor == Some(i));
                        if let Some(n) = page {
                            corner_badge(ui.painter(), resp.rect, format!("#{n}"));
                        }
                        if resp.double_clicked() {
                            activated = Some(i);
                        } else if resp.clicked() {
                            clicked = Some((i, ui.input(|inp| inp.modifiers)));
                        }
                    }
                });
            }
        });
        self.grid_view = (out.state.clamped_offset().y, out.inner_rect.height());
        if let Some((i, m)) = clicked {
            self.sel.click(i, m.command, m.shift);
        }
        if let Some(i) = activated {
            self.activate(i, &ctx);
        }
    }

    fn tag_panel(&mut self, ui: &mut egui::Ui) {
        let targets = self.targets();
        if targets.is_empty() {
            ui.label(RichText::new("Select files to see and edit their tags.").color(Color32::from_gray(150)));
            return;
        }
        egui::ScrollArea::vertical().id_salt("tag-panel").auto_shrink([false, false]).show(ui, |ui| {
            self.file_info(ui, &targets);
            ui.separator();
            self.rating_row(ui, &targets);
            ui.separator();
            self.tag_list(ui, &targets);
            ui.add_space(6.0);
            self.tag_input(ui, &targets);
            if targets.len() == 1 {
                ui.separator();
                self.source_row(ui, &targets[0]);
            }
            ui.separator();
            self.pool_section(ui, &targets);
            ui.separator();
            self.family_section(ui, &targets);
        });
    }

    /// The pools of the selected files: remove, add (with autocomplete), create, and reorder
    /// while a pool is being viewed.
    fn pool_section(&mut self, ui: &mut egui::Ui, targets: &[Hit]) {
        ui.label(RichText::new("Pools").strong()).on_hover_text("Named, ordered groups: comic pages, a photo series…");
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for e in targets.iter().filter_map(|h| self.lib.entry(h)) {
            for p in e.pools.keys() {
                *counts.entry(p.clone()).or_default() += 1;
            }
        }
        let mut remove = None;
        let mut show = None;
        if !counts.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (pool, n) in &counts {
                    let page = match targets {
                        [hit] => self.lib.entry(hit).and_then(|e| e.pools.get(pool)).map(|p| format!(" #{p}")).unwrap_or_default(),
                        _ => format!(" ({n})"),
                    };
                    if tag_chip(ui, &format!("{pool}{page}"), POOL_COLOR, "Click × to take the file(s) out of the pool") {
                        remove = Some(pool.clone());
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                for pool in counts.keys() {
                    if ui.small_button(format!("View {pool}")).clicked() {
                        show = Some(pool.clone());
                    }
                }
            });
        }
        let edit = egui::TextEdit::singleline(&mut self.pool_input).hint_text("Add to pool…").desired_width(f32::INFINITY);
        let tags = &self.lib.tags;
        let out = egui::CompletionPopup::new(egui::Id::unique(POOL_INPUT_ID))
            .align(egui::RectAlign::BOTTOM_START)
            .max_height(240.0)
            .show(ui, edit, |q| suggest::for_pool_input(q.word, tags).into_iter().map(|s| popup_item(tags, s)).collect());
        let resp = out.text_edit.response.response;
        let submitted = out.accepted.is_none() && resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let typed = self.pool_input.trim().to_string();
        let known = boorucraft_store::TagList::normalize_pool(&typed).ok().filter(|p| self.lib.tags.has_pool(p));
        if submitted && let Some(pool) = &known {
            let n = self.lib.add_to_pool(targets, pool);
            self.lib.notify(format!("Added {n} file{} to {pool}", if n == 1 { "" } else { "s" }));
            self.pool_input.clear();
        }
        if !typed.is_empty() && known.is_none() && ui.button(format!("Create pool “{typed}” and add")).clicked() {
            if let Some(pool) = self.lib.create_pool(&typed) {
                self.lib.add_to_pool(targets, &pool);
            }
            self.pool_input.clear();
        }
        if let Some(pool) = self.lib.pool_view.clone()
            && targets.iter().any(|h| self.lib.entry(h).is_some_and(|e| e.pools.contains_key(&pool)))
        {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Move").color(Color32::from_gray(170)));
                for (label, key, to) in [
                    ("First", "Shift+[", PoolMove::First),
                    ("Earlier", "[", PoolMove::Earlier),
                    ("Later", "]", PoolMove::Later),
                    ("Last", "Shift+]", PoolMove::Last),
                ] {
                    if ui.small_button(label).on_hover_text(format!("Key {key}")).clicked() {
                        self.move_in_pool(to);
                    }
                }
            });
        }
        if let Some(pool) = remove {
            self.lib.remove_from_pool(targets, &pool);
        }
        if let Some(pool) = show {
            self.query = format!("pool:{pool}");
            self.viewer = None;
        }
    }

    /// Parent and children: show the family, unlink, or link a selection as versions.
    fn family_section(&mut self, ui: &mut egui::Ui, targets: &[Hit]) {
        ui.label(RichText::new("Versions").strong()).on_hover_text("Link edits, crops and alternates of a picture to it (parent and children)");
        let file_name = |lib: &Library, hash: &str| -> String {
            lib.find(hash)
                .and_then(|(_, e)| e.paths.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()))
                .unwrap_or_else(|| format!("{}… (not open)", hash.get(..12).unwrap_or(hash)))
        };
        let mut show: Option<String> = None;
        let mut unlink: Option<Vec<Hit>> = None;
        let mut link: Option<(Hit, Vec<Hit>)> = None;
        if let [hit] = targets {
            let parent = self.lib.entry(hit).and_then(|e| e.parent.clone());
            let kids = self.lib.children(&hit.hash);
            if let Some(p) = &parent {
                ui.horizontal(|ui| {
                    ui.label("Parent:");
                    if ui.link(file_name(&self.lib, p)).on_hover_text("Show the family").clicked() {
                        show = Some(p.clone());
                    }
                    if ui.small_button("Unlink").on_hover_text("This file is no longer a version of it").clicked() {
                        unlink = Some(vec![hit.clone()]);
                    }
                });
            }
            if !kids.is_empty() {
                let text = format!("{} version{} of this file", kids.len(), if kids.len() == 1 { "" } else { "s" });
                if ui.link(text).on_hover_text("Show the family").clicked() {
                    show = Some(hit.hash.clone());
                }
            }
            if parent.is_none() && kids.is_empty() {
                ui.label(RichText::new("Select it with other files to link them as versions.").small().color(Color32::from_gray(150)));
            }
        } else {
            let main = self.sel.cursor.and_then(|c| self.lib.hits.get(c)).filter(|h| targets.contains(h)).or(targets.first()).cloned();
            if let Some(main) = main {
                let others: Vec<Hit> = targets.iter().filter(|h| **h != main).cloned().collect();
                let rest = if others.len() == 1 { "the other file".to_string() } else { format!("the other {} files", others.len()) };
                let text = format!("Make “{}” the parent of {rest}", file_name(&self.lib, &main.hash));
                if ui.button(text).on_hover_text("The file with the keyboard cursor becomes the parent").clicked() {
                    link = Some((main, others));
                }
            }
            if targets.iter().any(|h| self.lib.entry(h).is_some_and(|e| e.parent.is_some())) && ui.button("Unlink them from their parents").clicked() {
                unlink = Some(targets.to_vec());
            }
        }
        if let Some(hash) = show {
            self.query = format!("parent:{}", hash.get(..16).unwrap_or(&hash));
            self.viewer = None;
        }
        if let Some(hits) = unlink {
            self.lib.set_parent(&hits, None);
        }
        if let Some((main, others)) = link {
            let n = self.lib.set_parent(&others, Some(&main));
            if n > 0 {
                self.lib.notify(format!("Linked {n} file{} as versions", if n == 1 { "" } else { "s" }));
            }
        }
    }

    fn file_info(&self, ui: &mut egui::Ui, targets: &[Hit]) {
        let [hit] = targets else {
            ui.heading(format!("{} files", targets.len()));
            return;
        };
        let Some(e) = self.lib.entry(hit) else { return };
        let name = e.paths.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default();
        ui.label(RichText::new(name).strong());
        let mut facts = vec![e.format.to_uppercase()];
        if let (Some(w), Some(h)) = (e.width, e.height) {
            facts.push(format!("{w}×{h}"));
        }
        facts.push(human_size(e.size));
        if e.animated {
            facts.push("animated".into());
        }
        ui.label(RichText::new(facts.join(" · ")).color(Color32::from_gray(170)));
        if let Some(t) = e.taken {
            ui.label(RichText::new(format!("Taken {}", t.format("%Y-%m-%d %H:%M"))).color(Color32::from_gray(150)));
        }
        if e.missing {
            ui.label(RichText::new("Missing: not found in the folder").color(Color32::from_rgb(230, 120, 110)));
        }
        if let Some(oc) = self.lib.colls.get(hit.coll) {
            for p in &e.paths {
                ui.label(RichText::new(format!("{}/{p}", oc.name)).small().color(Color32::from_gray(130)));
            }
        }
        ui.horizontal(|ui| {
            let path = self.lib.path_of(hit);
            if ui.small_button("Open in default app").clicked()
                && let Some(p) = &path
                && let Err(e) = viewer::open_external(p)
            {
                eprintln!("boorucraft: {e}");
            }
            if ui.small_button("Show in folder").clicked()
                && let Some(p) = &path
                && let Err(e) = viewer::show_in_folder(p)
            {
                eprintln!("boorucraft: {e}");
            }
        });
    }

    fn rating_row(&mut self, ui: &mut egui::Ui, targets: &[Hit]) {
        let ratings: Vec<Option<Rating>> = targets.iter().filter_map(|h| self.lib.entry(h)).map(|e| e.rating).collect();
        let common = ratings.first().copied().filter(|r| ratings.iter().all(|x| x == r));
        ui.horizontal(|ui| {
            ui.label("Rating");
            for (label, r, key) in [
                ("Safe", Some(Rating::Safe), "1"),
                ("Questionable", Some(Rating::Questionable), "2"),
                ("Explicit", Some(Rating::Explicit), "3"),
                ("None", None, "0"),
            ] {
                if ui.selectable_label(common == Some(r), label).on_hover_text(format!("Key {key}")).clicked() {
                    self.lib.set_rating(targets, r);
                }
            }
        });
    }

    fn tag_list(&mut self, ui: &mut egui::Ui, targets: &[Hit]) {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for e in targets.iter().filter_map(|h| self.lib.entry(h)) {
            for t in &e.tags {
                *counts.entry(t.clone()).or_default() += 1;
            }
        }
        if counts.is_empty() {
            ui.label(RichText::new("No tags yet. Type below to add some (press T).").color(Color32::from_gray(150)));
            return;
        }
        // Namespaced tags first, grouped by namespace, then the rest.
        let mut sorted: Vec<(Option<String>, String, usize)> =
            counts.into_iter().map(|(t, n)| (self.lib.tags.namespace_of(&t).map(str::to_string), t, n)).collect();
        sorted.sort_by(|a, b| (a.0.is_none(), &a.0, &a.1).cmp(&(b.0.is_none(), &b.0, &b.1)));
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (ns, tag, n) in &sorted {
                let known = self.lib.tags.contains(tag);
                let color =
                    if !known { Color32::from_gray(120) } else { ns.as_deref().and_then(|ns| self.namespace_color(ns)).unwrap_or(Color32::from_gray(225)) };
                let text = if targets.len() > 1 { format!("{tag} ({n})") } else { tag.clone() };
                if tag_chip(ui, &text, color, if known { "Click × to remove" } else { "Not in the tag list. Click × to remove" }) {
                    remove = Some(tag.clone());
                }
            }
        });
        if let Some(t) = remove {
            self.lib.edit_tags(targets, &[], &[t]);
        }
    }

    fn namespace_color(&self, ns: &str) -> Option<Color32> {
        namespace_color(&self.lib.tags, ns)
    }

    /// Left panel, below the collections: the tags of the current results.
    fn results_sidebar(&mut self, ui: &mut egui::Ui) {
        let mut toggle: Option<String> = None;
        let pools = self.lib.pool_counts().to_vec();
        if !pools.is_empty() {
            ui.label(RichText::new("Pools").strong()).on_hover_text("Click: show the pool in its order (click again to go back)");
            for (pool, n) in &pools {
                let term = format!("pool:{pool}");
                if sidebar_link(ui, pool, *n as u64, query_has(&self.query, &term), POOL_COLOR) {
                    toggle = Some(term);
                }
            }
            ui.add_space(6.0);
        }
        if self.lib.hits.is_empty() {
            if let Some(term) = toggle {
                self.query = toggle_term(&self.query, &term);
            }
            return;
        }
        ui.label(RichText::new("Ratings").strong());
        let [s, q, e, none] = self.lib.result_ratings;
        for (label, term, n) in [("Safe", "rating:s", s), ("Questionable", "rating:q", q), ("Explicit", "rating:e", e), ("Unrated", "-rating:s,q,e", none)] {
            if n > 0 && sidebar_link(ui, label, n, query_has(&self.query, term), Color32::from_gray(210)) {
                toggle = Some(term.to_string());
            }
        }
        ui.add_space(6.0);
        let shown = self.lib.result_tags.len().min(MAX_SIDEBAR_TAGS);
        ui.label(RichText::new(format!("Tags in these results ({})", self.lib.result_tags.len())).strong())
            .on_hover_text("Click: add to the search (click again to remove)\nCtrl+click: exclude");
        // Namespaced groups first (by namespace), then plain tags; each by count.
        let mut groups: BTreeMap<Option<&str>, Vec<(&str, u64)>> = BTreeMap::new();
        for (tag, n) in self.lib.result_tags.iter().take(shown) {
            groups.entry(self.lib.tags.namespace_of(tag)).or_default().push((tag.as_str(), *n));
        }
        let ordered = groups.iter().filter(|(ns, _)| ns.is_some()).chain(groups.iter().filter(|(ns, _)| ns.is_none()));
        egui::ScrollArea::vertical().id_salt("result-tags").auto_shrink([false, false]).show(ui, |ui| {
            for (ns, list) in ordered {
                let color = ns.and_then(|ns| namespace_color(&self.lib.tags, ns)).unwrap_or(Color32::from_gray(215));
                if let Some(ns) = ns {
                    ui.add_space(4.0);
                    ui.label(RichText::new(*ns).small().color(color));
                }
                for (tag, n) in list {
                    let known = self.lib.tags.contains(tag);
                    let shown_name = match ns {
                        Some(ns) => tag.strip_prefix(*ns).and_then(|t| t.strip_prefix(':')).unwrap_or(tag),
                        None => tag,
                    };
                    let c = if known { color } else { Color32::from_gray(120) };
                    let active = query_has(&self.query, tag) || query_has(&self.query, &format!("-{tag}"));
                    if sidebar_link(ui, shown_name, *n, active, c) {
                        let exclude = ui.input(|i| i.modifiers.command);
                        toggle = Some(if exclude && !active { format!("-{tag}") } else { (*tag).to_string() });
                    }
                }
            }
            if self.lib.result_tags.len() > shown {
                ui.label(RichText::new(format!("…and {} more", self.lib.result_tags.len() - shown)).small().color(Color32::from_gray(130)));
            }
        });
        if let Some(term) = toggle {
            self.query = toggle_term(&self.query, &term);
        }
    }

    fn tag_input(&mut self, ui: &mut egui::Ui, targets: &[Hit]) {
        let edit = egui::TextEdit::singleline(&mut self.tag_input).hint_text("Add tags (space-separated, -tag removes)").desired_width(f32::INFINITY);
        let (tags, usage) = (&self.lib.tags, self.lib.usage());
        let out = egui::CompletionPopup::new(egui::Id::unique(TAG_INPUT_ID))
            .align(egui::RectAlign::BOTTOM_START)
            .max_height(300.0)
            .show(ui, edit, |q| suggest::for_tag_input(q.word, usage, tags).into_iter().map(|s| popup_item(tags, s)).collect());
        let resp = out.text_edit.response.response;
        if self.focus == Some(TAG_INPUT_ID) {
            focus_at_end(ui.ctx(), egui::Id::unique(TAG_INPUT_ID), &self.tag_input);
            self.focus = None;
        }
        if resp.changed() {
            self.unknown_typed.clear();
        }
        let submitted = out.accepted.is_none() && resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if submitted {
            self.apply_tag_input(targets);
            resp.request_focus();
        }
        if !self.unknown_typed.is_empty() {
            ui.label(RichText::new(format!("Not in the tag list: {}", self.unknown_typed.join(" "))).color(Color32::from_rgb(240, 190, 110)));
            if ui.button(format!("Create {} and add", if self.unknown_typed.len() == 1 { "it" } else { "them" })).clicked() {
                let created: Vec<String> = self.unknown_typed.clone().iter().filter_map(|t| self.lib.create_tag(t)).collect();
                self.lib.edit_tags(targets, &created, &[]);
                self.unknown_typed.clear();
                self.tag_input.clear();
            }
        }
    }

    fn apply_tag_input(&mut self, targets: &[Hit]) {
        let mut add = Vec::new();
        let mut remove = Vec::new();
        let mut unknown = Vec::new();
        let mut leftover = Vec::new();
        for word in self.tag_input.split_whitespace() {
            let (neg, w) = match word.strip_prefix('-') {
                Some(w) => (true, w),
                None => (false, word),
            };
            let Ok(t) = normalize_tag(w) else {
                leftover.push(word.to_string());
                continue;
            };
            if neg {
                remove.push(t);
            } else if self.lib.tags.contains(&t) {
                add.push(t);
            } else {
                unknown.push(t.clone());
                leftover.push(t);
            }
        }
        if !add.is_empty() || !remove.is_empty() {
            self.lib.edit_tags(targets, &add, &remove);
        }
        self.tag_input = if leftover.is_empty() { String::new() } else { leftover.join(" ") };
        self.unknown_typed = unknown;
    }

    fn source_row(&mut self, ui: &mut egui::Ui, hit: &Hit) {
        if self.source_edit.as_ref().is_none_or(|(h, _)| h != hit) {
            let current = self.lib.entry(hit).and_then(|e| e.source.clone()).unwrap_or_default();
            self.source_edit = Some((hit.clone(), current));
        }
        let Some((_, text)) = &mut self.source_edit else { return };
        ui.label("Source");
        let resp = ui.add(egui::TextEdit::singleline(text).hint_text("https://…").desired_width(f32::INFINITY));
        if resp.lost_focus() {
            let value = text.clone();
            self.lib.set_source(hit, Some(value));
        }
    }

    fn viewer_bar(&mut self, ui: &mut egui::Ui) {
        let Some(v) = &mut self.viewer else { return };
        let total = self.lib.hits.len();
        let pos = v.pos;
        let mut close = false;
        let mut go = None;
        let mut external = false;
        ui.horizontal(|ui| {
            if ui.button("← Back").on_hover_text("Esc").clicked() {
                close = true;
            }
            if ui.add_enabled(pos > 0, egui::Button::new("‹ Prev")).clicked() {
                go = Some(pos - 1);
            }
            ui.label(format!("{} / {total}", pos + 1));
            if ui.add_enabled(pos + 1 < total, egui::Button::new("Next ›")).clicked() {
                go = Some(pos + 1);
            }
            ui.separator();
            if ui.button("Fit").on_hover_text("F").clicked() {
                v.fit();
            }
            if ui.button("1:1").on_hover_text("Z").clicked() {
                v.actual_size();
            }
            if ui.button("Open in default app").on_hover_text("O").clicked() {
                external = true;
            }
        });
        if close {
            self.viewer = None;
            self.scroll_to = Some(pos);
        }
        if let Some(p) = go {
            let ctx = ui.ctx().clone();
            self.open_result(p, &ctx);
        }
        if external {
            self.open_external(pos);
        }
    }

    /// Applies `--select` / `--view` and takes `--screenshot` once scanning is done.
    fn startup(&mut self, ctx: &egui::Context) {
        if self.lib.is_busy() {
            return;
        }
        if !self.startup_done {
            self.startup_done = true;
            self.run_search();
            for &i in &self.options.select.clone() {
                self.sel.click(i, true, false);
            }
            if let Some(v) = self.options.view {
                self.open_result(v, ctx);
            }
            self.focus = match self.options.focus.as_deref() {
                Some("search") => Some(SEARCH_ID),
                Some("tags") => Some(TAG_INPUT_ID),
                _ => None,
            };
            self.ready_at = Some(Instant::now());
        }
        let Some(path) = self.options.screenshot.clone() else { return };
        let Some(ready) = self.ready_at else { return };
        if self.screenshot_requested {
            return;
        }
        if ready.elapsed() < self.options.screenshot_after {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        self.screenshot_requested = true;
        let ctx2 = ctx.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
            let saved = image::RgbaImage::from_raw(w as u32, h as u32, rgba).map(|img| img.save_with_format(&path, image::ImageFormat::Png));
            match saved {
                Some(Ok(())) => eprintln!("boorucraft: saved {}", path.display()),
                Some(Err(e)) => eprintln!("boorucraft: screenshot {}: {e}", path.display()),
                None => eprintln!("boorucraft: screenshot has a bad size"),
            }
            ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            ctx2.request_repaint();
        })));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.lib.poll() {
            self.searched = None;
        }
        if self.searched.as_deref() != Some(self.query.as_str()) {
            self.run_search();
        }
        self.sel.clamp(self.lib.hits.len());
        self.lib.refresh_usage();
        self.lib.refresh_dupes();
        self.thumbs.begin_frame(&ctx);
        self.handle_keys(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.viewer.is_none() {
            egui::Panel::left("collections").resizable(true).default_size(220.0).show(ui, |ui| {
                self.collections_panel(ui);
                ui.separator();
                self.results_sidebar(ui);
            });
        }
        let dupes = self.screen == Screen::Duplicates && self.viewer.is_none();
        if !dupes {
            egui::Panel::right("tags").resizable(true).default_size(300.0).min_size(220.0).max_size(480.0).show(ui, |ui| self.tag_panel(ui));
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            if dupes {
                self.duplicates_screen(ui);
            } else if self.viewer.is_some() {
                self.viewer_bar(ui);
                let view = ui.available_rect_before_wrap();
                let format = self.viewer.as_ref().and_then(|v| self.lib.entry(&v.hit)).map(|e| e.format.clone()).unwrap_or_default();
                let back = self.viewer.as_mut().is_some_and(|v| v.show(ui, view, &format));
                if back {
                    let pos = self.viewer.as_ref().map(|v| v.pos);
                    self.viewer = None;
                    self.scroll_to = pos;
                }
            } else {
                self.grid(ui);
            }
        });

        self.confirm_trash_dialog(&ctx);
        self.startup(&ctx);
        if let Some(wait) = self.lib.save_due() {
            ctx.request_repaint_after(wait);
        }
        if self.lib.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn on_exit(&mut self) {
        self.lib.flush_all();
    }
}

/// Draws one grid tile; returns its response.
fn draw_tile(ui: &mut egui::Ui, lib: &Library, thumbs: &mut Thumbnails, hit: &Hit, size: f32, selected: bool, cursor: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(6);
    painter.rect_filled(rect, radius, Color32::from_gray(32));
    let Some(entry) = lib.entry(hit) else { return resp };
    let inner = rect.shrink(4.0);
    if boorucraft_thumbs::can_decode(&entry.format) && !entry.missing {
        match thumbs.get(&hit.hash, lib.path_of(hit), &entry.format) {
            Thumb::Ready(tex) => {
                let [w, h] = tex.size();
                let scale = (inner.width() / w.max(1) as f32).min(inner.height() / h.max(1) as f32);
                let r = Rect::from_center_size(inner.center(), vec2(w as f32, h as f32) * scale);
                painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            Thumb::Loading => {
                painter.rect_filled(inner, radius, Color32::from_gray(38));
            }
            Thumb::Failed(_) => draw_placeholder(&painter, inner, &entry.format, 4.0),
        }
    } else {
        draw_placeholder(&painter, inner, &entry.format, 4.0);
    }
    if entry.missing {
        painter.rect_filled(rect, radius, Color32::from_black_alpha(150));
        painter.text(rect.center(), Align2::CENTER_CENTER, "missing", FontId::proportional(13.0), Color32::from_rgb(230, 120, 110));
    }
    if entry.animated {
        badge(&painter, rect, if entry.format == "gif" { "GIF" } else { "ANIM" }.to_string());
    }
    if selected {
        painter.rect_filled(rect, radius, Color32::from_rgba_unmultiplied(90, 150, 240, 40));
        painter.rect_stroke(rect, radius, Stroke::new(3.0, ACCENT), StrokeKind::Inside);
    } else if cursor {
        painter.rect_stroke(rect, radius, Stroke::new(1.5, ACCENT), StrokeKind::Inside);
    } else if resp.hovered() {
        painter.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_gray(110)), StrokeKind::Inside);
    }
    let name = entry.paths.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default();
    resp.on_hover_ui(|ui| {
        ui.label(RichText::new(&name).strong());
        let mut facts = vec![entry.format.to_uppercase()];
        if let (Some(w), Some(h)) = (entry.width, entry.height) {
            facts.push(format!("{w}×{h}"));
        }
        facts.push(human_size(entry.size));
        ui.label(facts.join(" · "));
        if !entry.tags.is_empty() {
            let shown: Vec<&str> = entry.tags.iter().take(12).map(String::as_str).collect();
            let more = entry.tags.len().saturating_sub(shown.len());
            ui.label(if more > 0 { format!("{} +{more}", shown.join(" ")) } else { shown.join(" ") });
        }
    })
}

/// A label in the bottom-left corner of a tile (pool page numbers).
fn corner_badge(painter: &egui::Painter, rect: Rect, text: String) {
    let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), Color32::WHITE);
    let size = galley.size() + vec2(8.0, 4.0);
    let r = Rect::from_min_size(pos2(rect.left() + 6.0, rect.bottom() - 6.0 - size.y), size);
    painter.rect_filled(r, CornerRadius::same(3), Color32::from_rgba_unmultiplied(20, 90, 85, 220));
    painter.galley(r.min + vec2(4.0, 2.0), galley, Color32::WHITE);
}

fn badge(painter: &egui::Painter, rect: Rect, text: String) {
    let galley = painter.layout_no_wrap(text, FontId::proportional(10.0), Color32::WHITE);
    let r = Rect::from_min_size(rect.left_top() + vec2(6.0, 6.0), galley.size() + vec2(8.0, 4.0));
    painter.rect_filled(r, CornerRadius::same(3), Color32::from_black_alpha(170));
    painter.galley(r.min + vec2(4.0, 2.0), galley, Color32::WHITE);
}

/// Video formats (drawn with a play symbol).
pub fn is_video(format: &str) -> bool {
    matches!(format, "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "flv" | "mpeg" | "mpg" | "ts" | "3gp")
}

/// A code-drawn stand-in for files BooruCraft can't show: a play symbol for videos, a page for
/// anything else, and the format name.
pub(crate) fn draw_placeholder(painter: &egui::Painter, rect: Rect, format: &str, radius: f32) {
    painter.rect_filled(rect, CornerRadius::same(radius as u8), Color32::from_gray(44));
    let c = rect.center() - vec2(0.0, rect.height() * 0.08);
    let s = rect.width().min(rect.height()) * 0.18;
    let stroke = Stroke::new(2.0, Color32::from_gray(150));
    if is_video(format) {
        let pts = vec![c + vec2(-s * 0.6, -s), c + vec2(s, 0.0), c + vec2(-s * 0.6, s)];
        painter.add(egui::Shape::convex_polygon(pts, Color32::from_gray(150), Stroke::NONE));
    } else {
        let page = Rect::from_center_size(c, vec2(s * 1.5, s * 1.9));
        painter.rect_stroke(page, CornerRadius::same(3), stroke, StrokeKind::Middle);
        for k in 1..4 {
            let y = page.top() + page.height() * k as f32 / 4.0;
            painter.line_segment([pos2(page.left() + s * 0.3, y), pos2(page.right() - s * 0.3, y)], Stroke::new(1.5, Color32::from_gray(110)));
        }
    }
    let label = if format.is_empty() { "?".to_string() } else { format.to_uppercase() };
    painter.text(
        pos2(rect.center().x, rect.bottom() - rect.height() * 0.2),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional((rect.height() * 0.11).clamp(10.0, 22.0)),
        Color32::from_gray(190),
    );
}

/// A tag drawn as one rounded chip with a × at the end, so wrapped rows wrap whole chips.
/// Returns true when the × was clicked.
fn tag_chip(ui: &mut egui::Ui, text: &str, color: Color32, hover: &str) -> bool {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font.clone(), color);
    let x_width = 14.0;
    let pad = vec2(6.0, 2.0);
    let size = vec2(galley.size().x + x_width + pad.x * 2.0 + 2.0, galley.size().y + pad.y * 2.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let x_rect = Rect::from_min_max(pos2(rect.right() - pad.x - x_width, rect.top()), rect.max);
    let over_x = resp.hover_pos().is_some_and(|p| x_rect.contains(p));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), Color32::from_gray(if resp.hovered() { 50 } else { 40 }));
        painter.galley(rect.min + pad, galley, color);
        let x_color = if over_x { Color32::from_rgb(230, 120, 110) } else { Color32::from_gray(150) };
        painter.text(x_rect.center(), Align2::CENTER_CENTER, "×", font, x_color);
    }
    let resp = resp.on_hover_text(hover);
    resp.clicked() && resp.interact_pointer_pos().is_some_and(|p| x_rect.contains(p))
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Focuses a text box with the cursor after its text (where typing continues).
fn focus_at_end(ctx: &egui::Context, id: egui::Id, text: &str) {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
    state.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

fn namespace_color(tags: &boorucraft_store::TagList, ns: &str) -> Option<Color32> {
    let (_, info) = tags.namespaces().find(|(n, _)| *n == ns)?;
    parse_color(&info.color)
}

/// One autocomplete row: the name in its namespace colour, the count or example on the right.
fn popup_item(tags: &boorucraft_store::TagList, s: suggest::Suggestion) -> egui::Suggestion {
    let color = match &s.tag {
        Some(t) => tags.namespace_of(t).and_then(|ns| namespace_color(tags, ns)).unwrap_or(Color32::from_gray(225)),
        None => META_COLOR,
    };
    egui::Suggestion::new(s.insert).content(RichText::new(s.label).color(color)).description(s.detail)
}

/// A clickable "name count" line in the sidebar; returns true when clicked.
fn sidebar_link(ui: &mut egui::Ui, name: &str, count: u64, active: bool, color: Color32) -> bool {
    let text = RichText::new(name).color(color);
    let text = if active { text.strong().underline() } else { text };
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        clicked = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
        ui.label(RichText::new(count.to_string()).small().color(Color32::from_gray(120)));
    });
    clicked
}

/// Is `term` one of the query's terms (case-insensitive)?
fn query_has(query: &str, term: &str) -> bool {
    query.split_whitespace().any(|t| t.eq_ignore_ascii_case(term))
}

/// Removes `term` (or its `-` form) from the query if it's there, otherwise appends it.
fn toggle_term(query: &str, term: &str) -> String {
    let bare = term.trim_start_matches(['-', '~']);
    let words: Vec<&str> = query.split_whitespace().collect();
    let present = |w: &&str| w.trim_start_matches(['-', '~']).eq_ignore_ascii_case(bare);
    if words.iter().any(present) {
        words.into_iter().filter(|w| !present(w)).collect::<Vec<_>>().join(" ")
    } else if words.is_empty() {
        term.to_string()
    } else {
        format!("{} {term}", words.join(" "))
    }
}

const META_COLOR: Color32 = Color32::from_rgb(200, 160, 240);
const POOL_COLOR: Color32 = Color32::from_rgb(120, 200, 190);

/// Colours the search text: `-` terms red, `~` terms blue, metatags purple, and the term with
/// an error underlined.
fn highlight_query(text: &str, error: Option<&std::ops::Range<usize>>, base: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = FontId::proportional(15.0);
    // Term ranges (whitespace outside quotes splits).
    let mut terms: Vec<std::ops::Range<usize>> = Vec::new();
    let (mut start, mut quoted) = (None, false);
    for (i, c) in text.char_indices() {
        if c == '"' {
            quoted = !quoted;
        }
        match (c.is_whitespace() && !quoted, start) {
            (true, Some(s)) => {
                terms.push(s..i);
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        terms.push(s..text.len());
    }
    let color_of = |at: usize| -> Color32 {
        let Some(r) = terms.iter().find(|r| r.contains(&at)) else { return base };
        let t = text.get(r.clone()).unwrap_or("");
        let body = t.trim_start_matches(['-', '~']);
        let is_meta = body.split_once(':').is_some_and(|(n, _)| boorucraft_core::METATAGS.contains(&n.to_ascii_lowercase().as_str()));
        if is_meta {
            META_COLOR
        } else if t.starts_with('-') {
            Color32::from_rgb(235, 140, 130)
        } else if t.starts_with('~') {
            Color32::from_rgb(140, 185, 245)
        } else {
            base
        }
    };
    let error = error.filter(|r| r.start < r.end && text.is_char_boundary(r.start) && r.end <= text.len() && text.is_char_boundary(r.end));
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for r in &terms {
        cuts.extend([r.start, r.end]);
    }
    if let Some(r) = error {
        cuts.extend([r.start, r.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let Some(piece) = text.get(a..b) else { continue };
        let bad = error.is_some_and(|r| r.contains(&a));
        let mut format = egui::TextFormat::simple(font.clone(), if bad { Color32::from_rgb(240, 120, 110) } else { color_of(a) });
        if bad {
            format.underline = Stroke::new(1.5, Color32::from_rgb(240, 90, 80));
        }
        job.append(piece, 0.0, format);
    }
    job
}

/// `#rrggbb` → colour.
fn parse_color(s: &str) -> Option<Color32> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_sizes() {
        assert_eq!(parse_color("#d04040"), Some(Color32::from_rgb(0xd0, 0x40, 0x40)));
        assert_eq!(parse_color("d04040"), None);
        assert_eq!(parse_color("#zzz"), None);
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn sidebar_clicks_toggle_terms() {
        assert_eq!(toggle_term("", "cat"), "cat");
        assert_eq!(toggle_term("dog", "cat"), "dog cat");
        assert_eq!(toggle_term("dog  cat", "cat"), "dog");
        assert_eq!(toggle_term("dog -cat", "cat"), "dog", "removes the excluded form too");
        assert_eq!(toggle_term("dog", "-cat"), "dog -cat");
        assert_eq!(toggle_term("rating:s cat", "rating:s"), "cat");
        assert!(query_has("a -Cat b", "-cat"));
        assert!(!query_has("cats", "cat"));
    }

    #[test]
    fn highlighting_keeps_the_text_and_marks_the_error() {
        let text = "cat -dog ~sky width:>big path:\"a b\" é";
        let job = highlight_query(text, Some(&(14..24)), Color32::WHITE);
        assert_eq!(job.text, text, "every byte is kept");
        let underlined: String =
            job.sections.iter().filter(|s| s.format.underline.width > 0.0).map(|s| &text[s.byte_range.start.0..s.byte_range.end.0]).collect();
        assert_eq!(underlined, "width:>big");
        let color_at = |at: usize| job.sections.iter().find(|s| (s.byte_range.start.0..s.byte_range.end.0).contains(&at)).map(|s| s.format.color);
        assert_eq!(color_at(0), Some(Color32::WHITE));
        assert_eq!(color_at(4), Some(Color32::from_rgb(235, 140, 130)));
        assert_eq!(color_at(9), Some(Color32::from_rgb(140, 185, 245)));
        assert_eq!(color_at(25), Some(META_COLOR), "quoted value stays one term");
        // A stale error span past the end or inside a character is ignored, not a crash.
        assert_eq!(highlight_query("é", Some(&(1..9)), Color32::WHITE).text, "é");
        assert_eq!(highlight_query("", None, Color32::WHITE).text, "");
    }
}
