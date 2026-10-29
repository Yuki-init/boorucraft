//! BooruCraft's desktop UI: search bar, collections, thumbnail grid, viewer and tag editor.
//!
//! The UI is thin: it reads [`library::Library`] and changes it through its methods, which use
//! the same store and index functions as the command-line tool.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod fonts;
pub mod library;
pub mod selection;
pub mod thumbnails;
pub mod viewer;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boorucraft_core::{Rating, normalize_tag};
use boorucraft_thumbs::ThumbCache;
use egui::{Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use library::{CollState, Config, Dirs, Hit, Library};
use selection::Selection;
use thumbnails::{Thumb, Thumbnails};
use viewer::Viewer;

const SEARCH_ID: &str = "search";
const TAG_INPUT_ID: &str = "tag-input";
const GAP: f32 = 6.0;
const ACCENT: Color32 = Color32::from_rgb(90, 150, 240);

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
    /// Tags typed that aren't in the tag list, waiting for "Create".
    unknown_typed: Vec<String>,
    source_edit: Option<(Hit, String)>,
    focus: Option<&'static str>,
    options: Options,
    startup_done: bool,
    ready_at: Option<Instant>,
    screenshot_requested: bool,
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
            unknown_typed: Vec::new(),
            source_edit: None,
            focus: None,
            options,
            startup_done: false,
            ready_at: None,
            screenshot_requested: false,
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
            let n = self.lib.hits.len();
            match key {
                Key::Slash => self.focus = Some(SEARCH_ID),
                Key::T => self.focus = Some(TAG_INPUT_ID),
                Key::Num1 => self.rate(Some(Rating::Safe)),
                Key::Num2 => self.rate(Some(Rating::Questionable)),
                Key::Num3 => self.rate(Some(Rating::Explicit)),
                Key::Num0 => self.rate(None),
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
            let right_width = 330.0;
            let width = (ui.available_width() - right_width).max(160.0);
            let bad = self.lib.query_error.is_some();
            let edit = egui::TextEdit::singleline(&mut self.query)
                .id(egui::Id::unique(SEARCH_ID))
                .hint_text("Search: cat -dog ~sky ~sea cat* rating:s   (press / to focus)")
                .desired_width(width)
                .font(FontId::proportional(15.0));
            let frame_color = if bad { Color32::from_rgb(220, 90, 80) } else { Color32::TRANSPARENT };
            let resp = egui::Frame::NONE.stroke(Stroke::new(1.0, frame_color)).corner_radius(CornerRadius::same(4)).show(ui, |ui| ui.add(edit)).inner;
            if self.focus == Some(SEARCH_ID) {
                resp.request_focus();
                self.focus = None;
            }
            ui.label(RichText::new(count).color(Color32::from_gray(170)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
                        .add_enabled(!self.lib.is_scanning(), egui::Button::new("Close").small())
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
        let cell = self.tile + GAP;
        let cols = ((ui.available_width() + GAP) / cell).floor().max(1.0) as usize;
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
                        let resp = draw_tile(ui, &self.lib, &mut self.thumbs, hit, self.tile, self.sel.items.contains(&i), self.sel.cursor == Some(i));
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
        });
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
        let (_, info) = self.lib.tags.namespaces().find(|(n, _)| *n == ns)?;
        parse_color(&info.color)
    }

    fn tag_input(&mut self, ui: &mut egui::Ui, targets: &[Hit]) {
        let edit = egui::TextEdit::singleline(&mut self.tag_input)
            .id(egui::Id::unique(TAG_INPUT_ID))
            .hint_text("Add tags (space-separated, -tag removes)")
            .desired_width(f32::INFINITY);
        let resp = ui.add(edit);
        if self.focus == Some(TAG_INPUT_ID) {
            resp.request_focus();
            self.focus = None;
        }
        if resp.changed() {
            self.unknown_typed.clear();
        }
        let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if submitted {
            self.apply_tag_input(targets);
            resp.request_focus();
        }
        // Suggestions for the word being typed.
        let last = self.tag_input.split_whitespace().last().filter(|_| !self.tag_input.ends_with(' ')).unwrap_or("").trim_start_matches('-').to_string();
        if !last.is_empty() {
            let suggestions: Vec<String> = self.lib.tags.suggest(&last, 8).into_iter().map(str::to_string).collect();
            let mut picked = None;
            ui.horizontal_wrapped(|ui| {
                for s in &suggestions {
                    if ui.small_button(s).clicked() {
                        picked = Some(s.clone());
                    }
                }
            });
            if let Some(s) = picked {
                let mut words: Vec<String> = self.tag_input.split_whitespace().map(str::to_string).collect();
                let negate = words.last().is_some_and(|w| w.starts_with('-'));
                words.pop();
                words.push(if negate { format!("-{s}") } else { s });
                self.tag_input = words.join(" ") + " ";
                self.focus = Some(TAG_INPUT_ID);
            }
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
        if self.lib.is_scanning() {
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
        self.thumbs.begin_frame(&ctx);
        self.handle_keys(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.viewer.is_none() {
            egui::Panel::left("collections").resizable(true).default_size(210.0).show(ui, |ui| self.collections_panel(ui));
        }
        egui::Panel::right("tags").resizable(true).default_size(300.0).min_size(220.0).max_size(480.0).show(ui, |ui| self.tag_panel(ui));
        egui::CentralPanel::default_margins().show(ui, |ui| {
            if self.viewer.is_some() {
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

        self.startup(&ctx);
        if let Some(wait) = self.lib.save_due() {
            ctx.request_repaint_after(wait);
        }
        if self.lib.is_scanning() {
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
}
