//! The full-size viewer: fit or zoom, pan, animation, and opening files in the default app.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Vec2, pos2, vec2};

use crate::library::Hit;

type Decoded = Result<Vec<boorucraft_thumbs::Frame>, String>;

/// Decodes a picture on a thread; the result arrives on the receiver.
fn start_load(path: PathBuf, format: &str, max_side: u32, ctx: &egui::Context) -> Result<Receiver<Decoded>, String> {
    let (tx, rx) = channel();
    let (format, ctx) = (format.to_string(), ctx.clone());
    std::thread::Builder::new()
        .name("viewer".into())
        .spawn(move || {
            let result = boorucraft_thumbs::decode_frames(&path, &format)
                .map(|frames| {
                    frames.into_iter().map(|f| boorucraft_thumbs::Frame { image: boorucraft_thumbs::fit_within(f.image, max_side), delay: f.delay }).collect()
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
            ctx.request_repaint();
        })
        .map_err(|e| e.to_string())?;
    Ok(rx)
}

/// A picture decoding ahead of time (the next one in the viewer or slideshow), handed to
/// [`Viewer::open`] when the user gets there.
pub struct Preload {
    pub pos: usize,
    hash: String,
    path: PathBuf,
    rx: Receiver<Decoded>,
}

impl Preload {
    /// Starts decoding, or returns `None` for files the viewer doesn't decode itself.
    pub fn start(pos: usize, hash: &str, path: PathBuf, format: &str, max_side: u32, ctx: &egui::Context) -> Option<Preload> {
        if !boorucraft_thumbs::can_decode(format) {
            return None;
        }
        let rx = start_load(path.clone(), format, max_side, ctx).ok()?;
        Some(Preload { pos, hash: hash.to_string(), path, rx })
    }

    pub fn is_for(&self, hash: &str) -> bool {
        self.hash == hash
    }
}

/// Where the viewer will most likely go next from `pos` of `n`: on in the direction it last
/// moved, or back to the start in a looping slideshow (`slideshow`: whether it loops).
pub fn next_pos(pos: usize, n: usize, backwards: bool, slideshow: Option<bool>) -> Option<usize> {
    if backwards {
        return pos.checked_sub(1);
    }
    if pos.saturating_add(1) < n {
        Some(pos + 1)
    } else if slideshow == Some(true) && n > 1 {
        Some(0)
    } else {
        None
    }
}

pub struct LoadedFrame {
    pub texture: egui::TextureHandle,
    pub delay: Duration,
}

enum Content {
    Loading(Receiver<Decoded>),
    Ready {
        frames: Vec<LoadedFrame>,
        size: Vec2,
    },
    /// BooruCraft can't decode this format: offer the default app.
    External,
    Failed(String),
}

pub struct Viewer {
    /// Position in the result list.
    pub pos: usize,
    pub hit: Hit,
    content: Content,
    frame: usize,
    frame_shown: Instant,
    /// `None`: fit to the window (never enlarged). `Some(z)`: z screen pixels per image pixel.
    zoom: Option<f32>,
    /// Offset of the image centre from the view centre.
    pan: Vec2,
}

impl Viewer {
    /// Opens a file. Decodable pictures load on a thread; `max_side` is the GPU texture limit.
    /// `early` is a load already started for this file by [`Preload::start`], used if it matches.
    pub fn open(pos: usize, hit: Hit, path: Option<PathBuf>, format: &str, max_side: u32, ctx: &egui::Context, early: Option<Preload>) -> Viewer {
        let content = match path {
            None => Content::Failed("this file is missing".into()),
            Some(_) if !boorucraft_thumbs::can_decode(format) => Content::External,
            Some(path) => match early.filter(|p| p.hash == hit.hash && p.path == path) {
                Some(p) => Content::Loading(p.rx),
                None => match start_load(path, format, max_side, ctx) {
                    Ok(rx) => Content::Loading(rx),
                    Err(e) => Content::Failed(e),
                },
            },
        };
        Viewer { pos, hit, content, frame: 0, frame_shown: Instant::now(), zoom: None, pan: Vec2::ZERO }
    }

    /// Loaded, or known not to load (a slideshow can time it).
    pub fn is_ready(&self) -> bool {
        !matches!(self.content, Content::Loading(_))
    }

    /// One run through an animation (zero for a still picture).
    pub fn animation_length(&self) -> Duration {
        match &self.content {
            Content::Ready { frames, .. } if frames.len() > 1 => frames.iter().map(|f| f.delay).sum(),
            _ => Duration::ZERO,
        }
    }

    pub fn is_external(&self) -> bool {
        matches!(self.content, Content::External)
    }

    /// Size of the picture as loaded, if it is.
    pub fn loaded_size(&self) -> Option<Vec2> {
        match &self.content {
            Content::Ready { size, .. } => Some(*size),
            _ => None,
        }
    }

    pub fn fit(&mut self) {
        self.zoom = None;
        self.pan = Vec2::ZERO;
    }

    pub fn actual_size(&mut self) {
        self.zoom = Some(1.0);
        self.pan = Vec2::ZERO;
    }

    pub fn zoom_by(&mut self, factor: f32, view: Rect, around: Option<Pos2>) {
        let Some(size) = self.loaded_size() else { return };
        let current = self.zoom.unwrap_or_else(|| fit_scale(size, view.size()));
        let new = (current * factor).clamp(0.02, 64.0);
        // Keep the point under the cursor in place.
        if let Some(p) = around {
            let centre = view.center() + self.pan;
            let rel = p - centre;
            self.pan += rel - rel * (new / current);
        }
        self.zoom = Some(new);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Content::Loading(rx) = &self.content else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.content = match result {
            Ok(frames) => {
                let size = frames.first().map_or(Vec2::ZERO, |f| vec2(f.image.width() as f32, f.image.height() as f32));
                let frames = frames
                    .into_iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let color = egui::ColorImage::from_rgba_unmultiplied([f.image.width() as usize, f.image.height() as usize], f.image.as_raw());
                        LoadedFrame { texture: ctx.load_texture(format!("view-{}-{i}", self.hit.hash), color, egui::TextureOptions::LINEAR), delay: f.delay }
                    })
                    .collect();
                Content::Ready { frames, size }
            }
            Err(e) => Content::Failed(e),
        };
        self.frame = 0;
        self.frame_shown = Instant::now();
    }

    /// Draws the picture into `view`. Returns true if the user double-clicked (to go back).
    pub fn show(&mut self, ui: &mut egui::Ui, view: Rect, format: &str) -> bool {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let painter = ui.painter_at(view);
        painter.rect_filled(view, 0.0, Color32::from_gray(18));
        let response = ui.interact(view, ui.scope_id().with("viewer"), Sense::click_and_drag());
        if let Some(size) = self.loaded_size() {
            if response.dragged() {
                self.pan += response.drag_delta();
                if self.zoom.is_none() {
                    self.zoom = Some(fit_scale(size, view.size()));
                }
            }
            let scroll = ui.input(|i| if response.hovered() { i.smooth_scroll_delta.y } else { 0.0 });
            if scroll != 0.0 {
                self.zoom_by((scroll / 200.0).exp(), view, response.hover_pos());
            }
        }
        match &self.content {
            Content::Loading(_) => {
                painter.text(view.center(), Align2::CENTER_CENTER, "Loading…", FontId::proportional(18.0), Color32::from_gray(160));
            }
            Content::Failed(e) => {
                painter.text(
                    view.center(),
                    Align2::CENTER_CENTER,
                    format!("Can't show this file: {e}"),
                    FontId::proportional(16.0),
                    Color32::from_rgb(230, 120, 110),
                );
            }
            Content::External => {
                crate::draw_placeholder(&painter, Rect::from_center_size(view.center() - vec2(0.0, 30.0), vec2(220.0, 220.0)), format, 0.0);
                painter.text(
                    view.center() + vec2(0.0, 110.0),
                    Align2::CENTER_CENTER,
                    "BooruCraft can't show this format. Press O or Enter to open it in the default app.",
                    FontId::proportional(15.0),
                    Color32::from_gray(170),
                );
            }
            Content::Ready { frames, size } => {
                let size = *size;
                let scale = self.zoom.unwrap_or_else(|| fit_scale(size, view.size()));
                let rect = Rect::from_center_size(view.center() + self.pan, size * scale);
                if let Some(f) = frames.get(self.frame) {
                    painter.image(f.texture.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                if frames.len() > 1 {
                    let delay = frames.get(self.frame).map_or(Duration::from_millis(100), |f| f.delay);
                    let shown = self.frame_shown.elapsed();
                    if shown >= delay {
                        self.frame = (self.frame + 1) % frames.len();
                        self.frame_shown = Instant::now();
                        ctx.request_repaint_after(frames.get(self.frame).map_or(delay, |f| f.delay));
                    } else {
                        ctx.request_repaint_after(delay - shown);
                    }
                }
            }
        }
        response.double_clicked()
    }
}

/// Scale that fits `size` inside `view`, never enlarging.
fn fit_scale(size: Vec2, view: Vec2) -> f32 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return 1.0;
    }
    (view.x / size.x).min(view.y / size.y).min(1.0)
}

/// Opens a file in the system's default app.
pub fn open_external(path: &Path) -> Result<(), String> {
    open::that_detached(path).map_err(|e| format!("couldn't open {}: {e}", path.display()))
}

/// Opens the folder containing a file in the file manager.
pub fn show_in_folder(path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    open::that_detached(dir).map_err(|e| format!("couldn't open {}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preloads_in_the_direction_of_travel() {
        assert_eq!(next_pos(3, 10, false, None), Some(4));
        assert_eq!(next_pos(3, 10, true, None), Some(2));
        assert_eq!(next_pos(0, 10, true, None), None);
        assert_eq!(next_pos(9, 10, false, None), None);
        assert_eq!(next_pos(9, 10, false, Some(false)), None);
        assert_eq!(next_pos(9, 10, false, Some(true)), Some(0), "a looping slideshow wraps");
        assert_eq!(next_pos(0, 1, false, Some(true)), None, "nothing else to show");
        assert_eq!(next_pos(usize::MAX, 10, false, None), None);
    }

    #[test]
    fn fit_never_enlarges() {
        assert_eq!(fit_scale(vec2(100.0, 50.0), vec2(1000.0, 1000.0)), 1.0);
        assert_eq!(fit_scale(vec2(2000.0, 1000.0), vec2(1000.0, 1000.0)), 0.5);
        assert_eq!(fit_scale(vec2(1000.0, 4000.0), vec2(1000.0, 1000.0)), 0.25);
        assert_eq!(fit_scale(Vec2::ZERO, vec2(10.0, 10.0)), 1.0);
    }
}
