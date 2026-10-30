//! BooruCraft desktop app.
//!
//! ```text
//! boorucraft [--collection DIR]... [--query Q] [--select N]... [--view N]
//!            [--focus search|tags] [--duplicates|--tags] [--slideshow] [--fullscreen] [--screenshot OUT.png [--after SECONDS] [--during-scan]] [--size WxH]
//! ```
//!
//! `--collection` opens folders for this session only (they aren't remembered).

// Release builds on Windows are GUI programs: no console window opens with the app.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use boorucraft_ui_egui::{App, Options};

/// Reverse-DNS app id: the Linux `.desktop` file, the hicolor icon name and the Wayland app id
/// (`packaging/env.sh` has the same one).
const APP_ID: &str = "io.github.yuki_init.boorucraft";

const USAGE: &str = "usage: boorucraft [--version] [--collection DIR]... [--query Q] [--select N]... [--view N] [--focus search|tags] [--duplicates|--tags] [--slideshow] [--fullscreen] [--screenshot OUT.png [--after SECONDS] [--during-scan]] [--size WxH]";

struct Args {
    options: Options,
    size: [f32; 2],
}

fn parse_args() -> Result<Args, String> {
    let mut options = Options { screenshot_after: Duration::from_millis(1500), ..Options::default() };
    let mut size = [1280.0, 800.0];
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--collection" => options.collections.get_or_insert_with(Vec::new).push(PathBuf::from(value("--collection")?)),
            "--query" => options.query = Some(value("--query")?),
            "--focus" => {
                let f = value("--focus")?;
                if f != "search" && f != "tags" {
                    return Err("--focus takes search or tags".into());
                }
                options.focus = Some(f);
            }
            "--select" => options.select.push(value("--select")?.parse().map_err(|_| "--select needs a number")?),
            "--duplicates" => options.screen = Some(boorucraft_ui_egui::Screen::Duplicates),
            "--tags" => options.screen = Some(boorucraft_ui_egui::Screen::Tags),
            "--slideshow" => options.slideshow = true,
            "--fullscreen" => options.fullscreen = true,
            "--view" => options.view = Some(value("--view")?.parse().map_err(|_| "--view needs a number")?),
            "--screenshot" => options.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--during-scan" => options.screenshot_during_scan = true,
            "--after" => {
                let s: f64 = value("--after")?.parse().map_err(|_| "--after needs seconds")?;
                options.screenshot_after = Duration::from_secs_f64(s.clamp(0.0, 600.0));
            }
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size looks like 1280x800")?;
                let w: f32 = w.parse().map_err(|_| "bad --size width")?;
                let h: f32 = h.parse().map_err(|_| "bad --size height")?;
                size = [w.clamp(320.0, 8192.0), h.clamp(240.0, 8192.0)];
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("boorucraft {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok(Args { options, size })
}

/// The window, Dock, taskbar and app-switcher icon (`assets/app-icon/`, made by
/// `packaging/icons.sh`). macOS gets the tile inside Apple's transparent margin so it matches the
/// size of other Dock icons; Windows and Linux get the full-bleed tile.
fn app_icon() -> Option<eframe::egui::IconData> {
    #[cfg(target_os = "macos")]
    let png: &[u8] = include_bytes!("../../../assets/app-icon/boorucraft-macos-512.png");
    #[cfg(not(target_os = "macos"))]
    let png: &[u8] = include_bytes!("../../../assets/app-icon/hicolor/256x256/apps/io.github.yuki_init.boorucraft.png");
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(eframe::egui::IconData { rgba: rgba.into_raw(), width, height })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("boorucraft: {e}");
            return ExitCode::from(2);
        }
    };
    let native = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("BooruCraft")
            .with_app_id(APP_ID)
            .with_inner_size(args.size)
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    let native = eframe::NativeOptions {
        viewport: match app_icon() {
            Some(icon) => native.viewport.with_icon(icon),
            None => native.viewport,
        },
        ..Default::default()
    };
    let options = args.options;
    match eframe::run_native("BooruCraft", native, Box::new(move |cc| Ok(Box::new(App::new(cc, options))))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("boorucraft: {e}");
            ExitCode::FAILURE
        }
    }
}
