//! The Duplicates screen: groups of near-duplicates side by side, the copy to keep, and what to
//! do with the others. Nothing is deleted without a confirmation, and then only to the system
//! trash.

use super::*;

/// Most groups drawn at once (the rest show after these are dealt with).
const MAX_GROUPS: usize = 200;
const CARD: f32 = 180.0;
/// Most bits the similarity slider allows.
const MAX_SLIDER: u32 = 12;

/// "Keep one, trash the others", waiting for the user to confirm.
pub(crate) struct TrashPlan {
    pub keep: Hit,
    pub others: Vec<Hit>,
    /// Copy the others' tags, rating and source to the kept file first.
    pub merge: bool,
}

enum Action {
    Choose(String, Hit),
    Trash(Hit, Vec<Hit>),
    Merge(Hit, Vec<Hit>),
    NotDupes(Vec<Hit>),
    Show(Vec<Hit>),
    Open(Hit),
}

impl App {
    pub(crate) fn duplicates_screen(&mut self, ui: &mut egui::Ui) {
        let total = self.lib.dupe_groups.len();
        let mut action = None;
        ui.horizontal(|ui| {
            ui.heading("Duplicates");
            ui.label(RichText::new(format!("{total} group{}", if total == 1 { "" } else { "s" })).color(Color32::from_gray(170)));
            ui.separator();
            let mut t = self.lib.dupe_threshold();
            let slider = egui::Slider::new(&mut t, 0..=MAX_SLIDER).text("bits apart at most");
            if ui.add(slider).on_hover_text("Lower: only near-identical copies. Higher: also edits and crops, but more false matches.").changed() {
                self.lib.set_dupe_threshold(t);
            }
            if let Some((done, all)) = self.lib.hash_progress() {
                ui.separator();
                ui.spinner();
                ui.label(format!("Checking pictures: {done} / {all}"));
            }
        });
        ui.label(
            RichText::new(
                "Click the picture to keep. “Keep this one” moves the others to the system trash (after asking), and can copy their tags to it first.",
            )
            .small()
            .color(Color32::from_gray(150)),
        );
        ui.add_space(4.0);
        if total == 0 {
            ui.centered_and_justified(|ui| {
                let msg = if self.lib.hash_progress().is_some() { "Still checking pictures…" } else { "No near-duplicates in the open collections." };
                ui.label(RichText::new(msg).size(16.0).color(Color32::from_gray(150)));
            });
            return;
        }
        let groups: Vec<Vec<Hit>> = self.lib.dupe_groups.iter().take(MAX_GROUPS).cloned().collect();
        egui::ScrollArea::vertical().id_salt("dupes").auto_shrink([false, false]).show(ui, |ui| {
            for (gi, group) in groups.iter().enumerate() {
                let Some(first) = group.first() else { continue };
                let key = first.hash.clone();
                let keep = self.dupe_keep.get(&key).filter(|h| group.contains(h)).cloned().unwrap_or_else(|| first.clone());
                let others: Vec<Hit> = group.iter().filter(|h| **h != keep).cloned().collect();
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(format!("Group {} · {} files", gi + 1, group.len())).strong());
                    let best_px = group.iter().filter_map(|h| self.lib.entry(h)).map(pixels).max().unwrap_or(0);
                    let best_size = group.iter().filter_map(|h| self.lib.entry(h)).map(|e| e.size).max().unwrap_or(0);
                    ui.horizontal_wrapped(|ui| {
                        for hit in group {
                            if let Some(a) = self.dupe_card(ui, hit, *hit == keep, best_px, best_size) {
                                action = Some(match a {
                                    CardClick::Choose => Action::Choose(key.clone(), hit.clone()),
                                    CardClick::Open => Action::Open(hit.clone()),
                                });
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let n = others.len();
                        if ui
                            .button(format!("Keep this one, trash the other {}…", if n == 1 { "file".to_string() } else { format!("{n} files") }))
                            .on_hover_text("Asks first. Files go to the system trash with their sidecars.")
                            .clicked()
                        {
                            action = Some(Action::Trash(keep.clone(), others.clone()));
                        }
                        if ui.button("Copy tags to this one").on_hover_text("Adds the others' tags (and their rating and source, if it has none)").clicked() {
                            action = Some(Action::Merge(keep.clone(), others.clone()));
                        }
                        if ui.button("Not duplicates").on_hover_text("Never group these again (saved in collection.json)").clicked() {
                            action = Some(Action::NotDupes(group.clone()));
                        }
                        if ui.button("Show in grid").clicked() {
                            action = Some(Action::Show(group.clone()));
                        }
                    });
                });
                ui.add_space(6.0);
            }
            if total > MAX_GROUPS {
                ui.label(
                    RichText::new(format!("…and {} more groups; they show here as these are dealt with.", total - MAX_GROUPS)).color(Color32::from_gray(150)),
                );
            }
        });
        match action {
            Some(Action::Choose(key, hit)) => {
                self.dupe_keep.insert(key, hit);
            }
            Some(Action::Trash(keep, others)) => self.confirm_trash = Some(TrashPlan { keep, others, merge: true }),
            Some(Action::Merge(keep, others)) => {
                if self.lib.merge_into(&keep, &others) {
                    self.lib.notify("Tags copied");
                } else {
                    self.lib.notify("It already has all their tags");
                }
            }
            Some(Action::NotDupes(group)) => self.lib.mark_not_duplicates(&group),
            Some(Action::Show(group)) => {
                self.query = group.iter().map(|h| format!("~hash:{}", h.hash.get(..16).unwrap_or(&h.hash))).collect::<Vec<_>>().join(" ");
                self.screen = Screen::Grid;
            }
            Some(Action::Open(hit)) => match self.lib.path_of(&hit) {
                Some(p) => {
                    if let Err(e) = viewer::open_external(&p) {
                        self.lib.notify(e);
                    }
                }
                None => self.lib.notify("This file is missing"),
            },
            None => {}
        }
    }

    /// One picture of a group with its facts; the biggest resolution and file are bold.
    fn dupe_card(&mut self, ui: &mut egui::Ui, hit: &Hit, keep: bool, best_px: u64, best_size: u64) -> Option<CardClick> {
        let mut out = None;
        ui.vertical(|ui| {
            ui.set_width(CARD);
            let resp = draw_tile(ui, &self.lib, &mut self.thumbs, hit, CARD, keep, false)
                .on_hover_text("Click: keep this one · Double-click: open in the default app");
            if resp.double_clicked() {
                out = Some(CardClick::Open);
            } else if resp.clicked() {
                out = Some(CardClick::Choose);
            }
            let Some(e) = self.lib.entry(hit) else { return };
            let grey = Color32::from_gray(165);
            if keep {
                ui.label(RichText::new("Keep").strong().color(ACCENT));
            } else {
                ui.label(RichText::new("Trash").color(Color32::from_gray(130)));
            }
            let res = match (e.width, e.height) {
                (Some(w), Some(h)) => format!("{w}×{h}"),
                _ => "size unknown".into(),
            };
            let res = RichText::new(res).color(grey);
            ui.label(if pixels(e) == best_px { res.strong().color(Color32::from_gray(225)) } else { res });
            let size = RichText::new(format!("{} · {}", human_size(e.size), e.format.to_uppercase())).color(grey);
            ui.label(if e.size == best_size { size.strong().color(Color32::from_gray(225)) } else { size });
            let coll = self.lib.colls.get(hit.coll).map_or("", |c| c.name.as_str());
            for p in e.paths.iter().take(3) {
                ui.add(egui::Label::new(RichText::new(format!("{coll}/{p}")).small().color(Color32::from_gray(135))).truncate());
            }
            let mut facts = vec![format!("{} tag{}", e.tags.len(), if e.tags.len() == 1 { "" } else { "s" })];
            if let Some(r) = e.rating {
                facts.push(format!("rating {r}"));
            }
            if e.source.is_some() {
                facts.push("source".into());
            }
            ui.label(RichText::new(facts.join(" · ")).small().color(Color32::from_gray(135)));
        });
        out
    }

    /// The "move to trash?" dialog. Shown over everything until answered.
    pub(crate) fn confirm_trash_dialog(&mut self, ctx: &egui::Context) {
        let Some(plan) = &mut self.confirm_trash else { return };
        let mut answer: Option<bool> = None;
        let lib = &self.lib;
        let name = |h: &Hit| {
            let coll = lib.colls.get(h.coll).map_or("", |c| c.name.as_str());
            let path = lib.entry(h).and_then(|e| e.paths.first().cloned()).unwrap_or_else(|| h.hash.clone());
            format!("{coll}/{path}")
        };
        let resp = egui::Modal::new(egui::Id::unique("confirm-trash")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            let n = plan.others.len();
            ui.heading(format!("Move {n} file{} to the trash?", if n == 1 { "" } else { "s" }));
            ui.add_space(4.0);
            for h in &plan.others {
                let copies = lib.entry(h).map_or(0, |e| e.paths.len());
                let extra = if copies > 1 { format!("  (and {} more cop{})", copies - 1, if copies == 2 { "y" } else { "ies" }) } else { String::new() };
                ui.label(RichText::new(format!("• {}{extra}", name(h))).color(Color32::from_rgb(235, 150, 140)));
            }
            ui.add_space(4.0);
            ui.label(format!("Keeping {}", name(&plan.keep)));
            ui.checkbox(&mut plan.merge, "Copy their tags, rating and source to the kept picture first");
            ui.label(
                RichText::new("Their XMP sidecars go to the trash with them. Files can be restored from the system trash.")
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Move to trash").color(Color32::from_rgb(240, 140, 130))).clicked() {
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
        let Some(plan) = self.confirm_trash.take() else { return };
        if yes {
            if plan.merge {
                self.lib.merge_into(&plan.keep, &plan.others);
            }
            let n = self.lib.trash_files(&plan.others);
            self.lib.notify(format!("Moved {n} file{} to the trash", if n == 1 { "" } else { "s" }));
            self.searched = None;
        }
    }
}

enum CardClick {
    Choose,
    Open,
}

fn pixels(e: &boorucraft_store::FileEntry) -> u64 {
    u64::from(e.width.unwrap_or(0)) * u64::from(e.height.unwrap_or(0))
}
