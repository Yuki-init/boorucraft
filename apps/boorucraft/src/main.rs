//! BooruCraft desktop app.
//!
//! ```text
//! boorucraft [--collection DIR]... [--query Q] [--select N]... [--view N]
//!            [--focus search|tags] [--duplicates] [--screenshot OUT.png [--after SECONDS]] [--size WxH]
//! ```
//!
//! `--collection` opens folders for this session only (they aren't remembered).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use boorucraft_ui_egui::{App, Options};

const USAGE: &str = "usage: boorucraft [--collection DIR]... [--query Q] [--select N]... [--view N] [--focus search|tags] [--duplicates] [--screenshot OUT.png [--after SECONDS]] [--size WxH]";

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
            "--duplicates" => options.duplicates = true,
            "--view" => options.view = Some(value("--view")?.parse().map_err(|_| "--view needs a number")?),
            "--screenshot" => options.screenshot = Some(PathBuf::from(value("--screenshot")?)),
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
            other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
        }
    }
    Ok(Args { options, size })
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
            .with_app_id("boorucraft")
            .with_inner_size(args.size)
            .with_min_inner_size([640.0, 400.0]),
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
