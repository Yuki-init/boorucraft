//! The Tags screen: the central tag list with how many files use each tag, and changes that reach
//! every collection (open ones now, closed ones when next opened): create, describe, rename,
//! merge, delete; namespace colours; pool names; and the review of unknown tags (used in files
//! but not in the list).

use super::*;

const ROW: f32 = 22.0;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagsTab {
    #[default]
    Tags,
    Namespaces,
    Pools,
    Unknown,
}

/// A change that needs a "are you sure?" first.
enum Confirm {
    Merge { from: String, into: String },
    Delete(String),
    DeletePool(String),
    RemoveUnknown(String),
}

#[derive(Default)]
pub(crate) struct TagsView {
    pub tab: TagsTab,
    filter: String,
    by_count: bool,
    selected: Option<String>,
    rename: String,
    description: String,
    merge_into: String,
    new_tag: String,
    new_ns: String,
    new_ns_color: [u8; 3],
    /// Colour picked but not saved yet (saved when the mouse button is let go).
    ns_color_edit: Option<(String, [u8; 3])>,
    pool: Option<String>,
    pool_rename: String,
    unknown: Option<String>,
    map_to: String,
    confirm: Option<Confirm>,
}

impl TagsView {
    /// Selects a tag and fills the editor with its current values.
    pub(crate) fn select(&mut self, tags: &boorucraft_store::TagList, tag: &str) {
        self.selected = Some(tag.to_string());
        self.rename = tag.to_string();
        self.description = tags.tags().find(|(t, _)| *t == tag).map(|(_, i)| i.description.clone()).unwrap_or_default();
        self.merge_into.clear();
    }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn grey(text: impl Into<String>) -> RichText {
    RichText::new(text).color(Color32::from_gray(150))
}

impl App {
    pub(crate) fn tags_screen(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Tags");
            ui.separator();
            let unknown = self.lib.unknown_tags().len();
            let unknown_label = if unknown > 0 { format!("Unknown ({unknown})") } else { "Unknown".to_string() };
            for (tab, name) in
                [(TagsTab::Tags, "Tags"), (TagsTab::Namespaces, "Namespaces"), (TagsTab::Pools, "Pools"), (TagsTab::Unknown, unknown_label.as_str())]
            {
                if ui.selectable_label(self.tags_view.tab == tab, name).clicked() {
                    self.tags_view.tab = tab;
                }
            }
        });
        if self.lib.tags.is_read_only() {
            ui.label(RichText::new("The tag list is open in another BooruCraft window, so it can't be changed here.").color(Color32::from_rgb(235, 170, 110)));
        }
        ui.label(RichText::new("Changes reach every collection: open ones now, others the next time they are opened.").small().color(Color32::from_gray(140)));
        ui.add_space(4.0);
        match self.tags_view.tab {
            TagsTab::Tags => self.tags_tab(ui),
            TagsTab::Namespaces => self.namespaces_tab(ui),
            TagsTab::Pools => self.pools_tab(ui),
            TagsTab::Unknown => self.unknown_tab(ui),
        }
        self.tags_confirm_dialog(ui.ctx());
    }

    fn tags_tab(&mut self, ui: &mut egui::Ui) {
        let counts: HashMap<String, u64> = self.lib.usage().iter().cloned().collect();
        let needle = self.tags_view.filter.trim().to_lowercase().replace(char::is_whitespace, "_");
        let mut rows: Vec<(String, u64, String)> = self
            .lib
            .tags
            .tags()
            .filter(|(t, info)| needle.is_empty() || t.contains(&needle) || info.description.to_lowercase().contains(&needle))
            .map(|(t, info)| (t.to_string(), counts.get(t).copied().unwrap_or(0), info.description.clone()))
            .collect();
        if self.tags_view.by_count {
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        let mut select = None;
        let mut create = None;
        ui.columns(2, |cols| {
            let [left, right] = cols else { return };
            left.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.tags_view.filter).hint_text("Filter by name or description").desired_width(200.0));
                ui.label(grey("Sort:"));
                if ui.selectable_label(!self.tags_view.by_count, "name").clicked() {
                    self.tags_view.by_count = false;
                }
                if ui.selectable_label(self.tags_view.by_count, "use").clicked() {
                    self.tags_view.by_count = true;
                }
            });
            left.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut self.tags_view.new_tag).hint_text("New tag, e.g. artist:name").desired_width(200.0));
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let ok = !self.tags_view.new_tag.trim().is_empty() && !self.lib.tags.is_read_only();
                if (ui.add_enabled(ok, egui::Button::new("Create")).clicked() || enter) && ok {
                    create = Some(self.tags_view.new_tag.clone());
                }
            });
            left.label(grey(format!("{} of {} tags", rows.len(), self.lib.tags.len())).small());
            let tags = &self.lib.tags;
            let selected = self.tags_view.selected.clone();
            egui::ScrollArea::vertical().id_salt("tag-rows").auto_shrink([false, false]).show_rows(left, ROW, rows.len(), |ui, range| {
                for (tag, count, desc) in rows.get(range).unwrap_or_default() {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW);
                        let color = tags.namespace_of(tag).and_then(|ns| namespace_color(tags, ns)).unwrap_or(Color32::from_gray(225));
                        let on = selected.as_deref() == Some(tag.as_str());
                        if ui.selectable_label(on, RichText::new(tag).color(color)).clicked() {
                            select = Some(tag.clone());
                        }
                        ui.label(RichText::new(count.to_string()).small().color(Color32::from_gray(120)));
                        if !desc.is_empty() {
                            ui.add(egui::Label::new(RichText::new(desc).small().color(Color32::from_gray(140))).truncate());
                        }
                    });
                }
            });
            self.tag_editor(right, &counts);
        });
        if let Some(name) = create
            && let Some(t) = self.lib.create_tag(&name)
        {
            self.tags_view.new_tag.clear();
            self.tags_view.select(&self.lib.tags, &t);
        }
        if let Some(t) = select {
            self.tags_view.select(&self.lib.tags, &t);
        }
    }

    /// The selected tag: what it is, and what can be done with it.
    fn tag_editor(&mut self, ui: &mut egui::Ui, counts: &HashMap<String, u64>) {
        let Some(tag) = self.tags_view.selected.clone().filter(|t| self.lib.tags.contains(t)) else {
            ui.label(grey("Pick a tag on the left to describe, rename, merge or delete it."));
            return;
        };
        let writable = !self.lib.tags.is_read_only();
        let count = counts.get(tag.as_str()).copied().unwrap_or(0);
        let color = self.lib.tags.namespace_of(&tag).and_then(|ns| namespace_color(&self.lib.tags, ns)).unwrap_or(Color32::from_gray(230));
        ui.label(RichText::new(&tag).size(18.0).strong().color(color));
        ui.horizontal(|ui| {
            ui.label(grey(format!("Used by {count} file{} in the open collections", if count == 1 { "" } else { "s" })));
            if count > 0 && ui.small_button("Show them").clicked() {
                self.query = tag.clone();
                self.screen = Screen::Grid;
            }
        });
        ui.add_space(8.0);

        ui.label(RichText::new("Description").strong());
        ui.add(egui::TextEdit::multiline(&mut self.tags_view.description).desired_rows(2).desired_width(f32::INFINITY));
        let current = self.lib.tags.tags().find(|(t, _)| *t == tag).map(|(_, i)| i.description.clone()).unwrap_or_default();
        if ui.add_enabled(writable && self.tags_view.description.trim() != current, egui::Button::new("Save description")).clicked() {
            let text = self.tags_view.description.clone();
            self.lib.describe_tag(&tag, &text);
        }
        ui.add_space(8.0);

        ui.label(RichText::new("Rename").strong());
        let mut rename = false;
        ui.horizontal(|ui| {
            let resp = ui.add(egui::TextEdit::singleline(&mut self.tags_view.rename).desired_width(220.0));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let ok = writable && !self.tags_view.rename.trim().is_empty() && self.tags_view.rename.trim() != tag;
            let clicked = ui.add_enabled(ok, egui::Button::new("Rename")).on_hover_text("In every file and sidecar that has it").clicked();
            rename = ok && (clicked || enter);
        });
        if rename {
            let to = self.tags_view.rename.clone();
            if let Some(new) = self.lib.rename_tag(&tag, &to) {
                self.lib.notify(format!("Renamed {tag} to {new}"));
                self.tags_view.select(&self.lib.tags, &new);
                self.searched = None;
            }
        }
        ui.add_space(8.0);

        ui.label(RichText::new("Merge into another tag").strong());
        ui.label(grey("Files with this tag get the other one instead, and this tag is removed.").small());
        let mut merge = false;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.tags_view.merge_into).hint_text("tag to keep").desired_width(220.0));
            let ok = writable && self.lib.tags.resolve(&self.tags_view.merge_into).is_ok_and(|t| t != tag);
            merge = ui.add_enabled(ok, egui::Button::new("Merge…")).clicked();
        });
        if !self.tags_view.merge_into.trim().is_empty() && self.lib.tags.resolve(&self.tags_view.merge_into).is_err() {
            ui.horizontal_wrapped(|ui| {
                let suggestions: Vec<String> =
                    self.lib.tags.suggest(&self.tags_view.merge_into, 6).into_iter().filter(|t| *t != tag).map(str::to_string).collect();
                for s in suggestions {
                    if ui.small_button(&s).clicked() {
                        self.tags_view.merge_into = s;
                    }
                }
            });
        }
        if merge && let Ok(into) = self.lib.tags.resolve(&self.tags_view.merge_into) {
            self.tags_view.confirm = Some(Confirm::Merge { from: tag.clone(), into });
        }
        ui.add_space(12.0);
        if ui.add_enabled(writable, egui::Button::new(RichText::new("Delete tag…").color(Color32::from_rgb(240, 140, 130)))).clicked() {
            self.tags_view.confirm = Some(Confirm::Delete(tag.clone()));
        }
    }

    fn namespaces_tab(&mut self, ui: &mut egui::Ui) {
        let writable = !self.lib.tags.is_read_only();
        ui.label(grey("A namespace groups tags written `name:tag` (artist:foo) and gives them a colour.").small());
        ui.add_space(4.0);
        let namespaces: Vec<(String, String)> = self.lib.tags.namespaces().map(|(n, i)| (n.to_string(), i.color.clone())).collect();
        let mut remove = None;
        egui::Grid::new("namespaces").striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
            for (name, color) in &namespaces {
                let mut rgb = match &self.tags_view.ns_color_edit {
                    Some((n, c)) if n == name => *c,
                    _ => parse_color(color).map_or([200, 200, 200], |c| [c.r(), c.g(), c.b()]),
                };
                ui.add_enabled_ui(writable, |ui| {
                    if egui::widgets::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                        self.tags_view.ns_color_edit = Some((name.clone(), rgb));
                    }
                });
                let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                ui.label(RichText::new(format!("{name}:")).strong().color(c));
                let n = self.lib.tags.tags().filter(|(t, _)| self.lib.tags.namespace_of(t) == Some(name.as_str())).count();
                ui.label(grey(format!("{n} tag{}", if n == 1 { "" } else { "s" })));
                if ui
                    .add_enabled(writable, egui::Button::new("Remove").small())
                    .on_hover_text("Tags keep their names; they just lose the grouping and colour")
                    .clicked()
                {
                    remove = Some(name.clone());
                }
                ui.end_row();
            }
        });
        // Save a picked colour once the mouse button is let go, not on every step of a drag.
        if let Some((name, rgb)) = self.tags_view.ns_color_edit.clone()
            && ui.input(|i| !i.pointer.any_down())
        {
            self.tags_view.ns_color_edit = None;
            self.lib.set_namespace_color(&name, &hex(rgb));
        }
        if let Some(name) = remove {
            self.lib.remove_namespace(&name);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.tags_view.new_ns).hint_text("new namespace, e.g. location").desired_width(200.0));
            egui::widgets::color_picker::color_edit_button_srgb(ui, &mut self.tags_view.new_ns_color);
            if ui.add_enabled(writable && !self.tags_view.new_ns.trim().is_empty(), egui::Button::new("Add")).clicked() {
                let (name, color) = (self.tags_view.new_ns.clone(), hex(self.tags_view.new_ns_color));
                if self.lib.add_namespace(&name, &color).is_some() {
                    self.tags_view.new_ns.clear();
                }
            }
        });
    }

    fn pools_tab(&mut self, ui: &mut egui::Ui) {
        let writable = !self.lib.tags.is_read_only();
        let counts: HashMap<String, usize> = self.lib.pool_counts().iter().cloned().collect();
        let pools: Vec<String> = self.lib.tags.pools().map(|(p, _)| p.to_string()).collect();
        if pools.is_empty() {
            ui.label(grey("No pools yet. Select files in the grid and use “Add to pool…” in the panel on the right."));
            return;
        }
        let mut pick = None;
        ui.columns(2, |cols| {
            let [left, right] = cols else { return };
            egui::ScrollArea::vertical().id_salt("pool-rows").auto_shrink([false, false]).show(left, |ui| {
                for p in &pools {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.tags_view.pool.as_deref() == Some(p.as_str()), RichText::new(p).color(POOL_COLOR)).clicked() {
                            pick = Some(p.clone());
                        }
                        ui.label(RichText::new(counts.get(p).copied().unwrap_or(0).to_string()).small().color(Color32::from_gray(120)));
                    });
                }
            });
            let Some(pool) = self.tags_view.pool.clone().filter(|p| pools.contains(p)) else {
                right.label(grey("Pick a pool to rename or delete it."));
                return;
            };
            right.label(RichText::new(&pool).size(18.0).strong().color(POOL_COLOR));
            if right.small_button("Show it").clicked() {
                self.query = format!("pool:{pool}");
                self.screen = Screen::Grid;
            }
            right.add_space(8.0);
            let mut rename = false;
            right.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.tags_view.pool_rename).desired_width(220.0));
                let ok = writable && !self.tags_view.pool_rename.trim().is_empty() && self.tags_view.pool_rename.trim() != pool;
                rename = ui.add_enabled(ok, egui::Button::new("Rename")).clicked();
            });
            if rename {
                let to = self.tags_view.pool_rename.clone();
                if let Some(new) = self.lib.rename_pool(&pool, &to) {
                    self.tags_view.pool = Some(new.clone());
                    self.tags_view.pool_rename = new;
                    self.searched = None;
                }
            }
            right.add_space(12.0);
            if right.add_enabled(writable, egui::Button::new(RichText::new("Delete pool…").color(Color32::from_rgb(240, 140, 130)))).clicked() {
                self.tags_view.confirm = Some(Confirm::DeletePool(pool.clone()));
            }
        });
        if let Some(p) = pick {
            self.tags_view.pool_rename = p.clone();
            self.tags_view.pool = Some(p);
        }
    }

    /// Tags found in files that aren't in the list: add each, map it to a listed tag, or remove it.
    fn unknown_tab(&mut self, ui: &mut egui::Ui) {
        let unknown = self.lib.unknown_tags();
        ui.label(
            grey("Tags found in files but not in the tag list: from a folder copied from another computer, or added by another app in a sidecar. Until you decide, they are kept and searchable, shown in grey.")
                .small(),
        );
        if unknown.is_empty() {
            ui.add_space(8.0);
            ui.label(grey("Every tag in the open collections is in the tag list."));
            return;
        }
        let writable = !self.lib.tags.is_read_only();
        if ui
            .add_enabled(writable, egui::Button::new(format!("Add all {} to the tag list", unknown.len())))
            .on_hover_text("Keep them all as they are")
            .clicked()
        {
            for (t, _) in &unknown {
                self.lib.create_tag(t);
            }
            self.lib.notify(format!("Added {} tags to the tag list", unknown.len()));
            return;
        }
        ui.add_space(4.0);
        let mut pick = None;
        ui.columns(2, |cols| {
            let [left, right] = cols else { return };
            egui::ScrollArea::vertical().id_salt("unknown-rows").auto_shrink([false, false]).show_rows(left, ROW, unknown.len(), |ui, range| {
                for (tag, n) in unknown.get(range).unwrap_or_default() {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW);
                        let on = self.tags_view.unknown.as_deref() == Some(tag.as_str());
                        if ui.selectable_label(on, RichText::new(tag).color(Color32::from_gray(150))).clicked() {
                            pick = Some(tag.clone());
                        }
                        ui.label(RichText::new(n.to_string()).small().color(Color32::from_gray(120)));
                    });
                }
            });
            let Some((tag, n)) = self.tags_view.unknown.as_ref().and_then(|t| unknown.iter().find(|(u, _)| u == t)).cloned() else {
                right.label(grey("Pick a tag on the left."));
                return;
            };
            right.label(RichText::new(&tag).size(18.0).strong().color(Color32::from_gray(190)));
            right.horizontal(|ui| {
                ui.label(grey(format!("On {n} file{} in the open collections", if n == 1 { "" } else { "s" })));
                if ui.small_button("Show them").clicked() {
                    self.query = tag.clone();
                    self.screen = Screen::Grid;
                }
            });
            right.add_space(8.0);
            if right.add_enabled(writable, egui::Button::new("Add to the tag list")).on_hover_text("Keep it as it is").clicked()
                && self.lib.create_tag(&tag).is_some()
            {
                self.lib.notify(format!("Added {tag} to the tag list"));
            }
            right.add_space(8.0);
            right.label(RichText::new("Or use a listed tag instead").strong());
            let mut map = false;
            right.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.tags_view.map_to).hint_text("listed tag").desired_width(220.0));
                let ok = self.lib.tags.resolve(&self.tags_view.map_to).is_ok();
                map = ui.add_enabled(ok, egui::Button::new("Use it")).on_hover_text("These files get that tag instead").clicked();
            });
            let needle = if self.tags_view.map_to.trim().is_empty() { tag.clone() } else { self.tags_view.map_to.clone() };
            if self.lib.tags.resolve(&self.tags_view.map_to).is_err() {
                let suggestions: Vec<String> = self.lib.tags.suggest(&needle, 6).into_iter().map(str::to_string).collect();
                if !suggestions.is_empty() {
                    right.horizontal_wrapped(|ui| {
                        ui.label(grey("Similar:").small());
                        for s in suggestions {
                            if ui.small_button(&s).clicked() {
                                self.tags_view.map_to = s;
                            }
                        }
                    });
                }
            }
            if map {
                let to = self.tags_view.map_to.clone();
                let changed = self.lib.replace_unknown_tag(&tag, Some(&to));
                self.lib.notify(format!("{changed} file(s) now have {to} instead of {tag}"));
                self.tags_view.map_to.clear();
                self.searched = None;
            }
            right.add_space(12.0);
            if right.button(RichText::new("Remove from the files…").color(Color32::from_rgb(240, 140, 130))).clicked() {
                self.tags_view.confirm = Some(Confirm::RemoveUnknown(tag.clone()));
            }
        });
        if let Some(t) = pick {
            self.tags_view.unknown = Some(t);
            self.tags_view.map_to.clear();
        }
    }

    fn tags_confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.tags_view.confirm else { return };
        let count = |t: &str| self.lib.usage().iter().find(|(u, _)| u == t).map_or(0, |(_, n)| *n);
        let (title, body, action) = match confirm {
            Confirm::Merge { from, into } => (
                format!("Merge {from} into {into}?"),
                format!("{} open file(s) with {from} get {into} instead, and {from} is removed from the tag list.", count(from)),
                "Merge",
            ),
            Confirm::Delete(t) => (format!("Delete {t}?"), format!("It is removed from the tag list and from {} open file(s).", count(t)), "Delete"),
            Confirm::DeletePool(p) => {
                (format!("Delete the pool {p}?"), "The files stay; they just leave the pool and lose their page numbers.".to_string(), "Delete")
            }
            Confirm::RemoveUnknown(t) => {
                (format!("Remove {t}?"), format!("It is taken off {} file(s) in the open collections and their sidecars.", count(t)), "Remove")
            }
        };
        let mut answer: Option<bool> = None;
        let resp = egui::Modal::new(egui::Id::unique("confirm-tags")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading(title);
            ui.add_space(4.0);
            ui.label(body);
            ui.label(
                RichText::new("Collections that aren't open are changed too, the next time they are opened. Sidecars follow.")
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new(action).color(Color32::from_rgb(240, 140, 130))).clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if resp.should_close() && answer.is_none() {
            answer = Some(false);
        }
        let Some(yes) = answer else { return };
        let Some(confirm) = self.tags_view.confirm.take() else { return };
        if !yes {
            return;
        }
        match confirm {
            Confirm::Merge { from, into } => {
                if self.lib.merge_tag(&from, &into).is_some() {
                    self.lib.notify(format!("Merged {from} into {into}"));
                    self.tags_view.select(&self.lib.tags, &into);
                }
            }
            Confirm::Delete(t) => {
                if self.lib.delete_tag(&t).is_some() {
                    self.lib.notify(format!("Deleted {t}"));
                    self.tags_view.selected = None;
                }
            }
            Confirm::DeletePool(p) => {
                if self.lib.delete_pool(&p).is_some() {
                    self.lib.notify(format!("Deleted the pool {p}"));
                    self.tags_view.pool = None;
                }
            }
            Confirm::RemoveUnknown(t) => {
                let n = self.lib.replace_unknown_tag(&t, None);
                self.lib.notify(format!("Removed {t} from {n} file(s)"));
            }
        }
        self.searched = None;
    }
}
