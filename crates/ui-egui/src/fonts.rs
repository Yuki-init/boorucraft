//! The UI's fonts: Inter and JetBrains Mono, embedded (OFL-1.1, see `ATTRIBUTION.md`), plus a
//! CJK font from the system if one is installed, so Japanese, Chinese and Korean file names and
//! tags show. egui's built-in fonts are switched off.

use std::path::{Path, PathBuf};
use std::sync::Arc;

const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

/// Where distributions put CJK fonts. Searched in order; the first readable file wins.
const CJK_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    "C:\\Windows\\Fonts\\YuGothR.ttc",
    "C:\\Windows\\Fonts\\msgothic.ttc",
];

/// Biggest font file we load (Noto Sans CJK is about 20 MiB).
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

fn find_cjk_font() -> Option<(PathBuf, Vec<u8>)> {
    let read = |p: &Path| -> Option<Vec<u8>> {
        let len = std::fs::metadata(p).ok()?.len();
        if len > MAX_FONT_BYTES {
            return None;
        }
        std::fs::read(p).ok()
    };
    CJK_CANDIDATES.iter().map(PathBuf::from).find_map(|p| read(&p).map(|b| (p, b)))
}

/// Installs the fonts. Returns the CJK font's path, if one was found.
pub fn install(ctx: &egui::Context) -> Option<PathBuf> {
    let mut fonts = egui::FontDefinitions::empty();
    fonts.font_data.insert("inter".into(), Arc::new(egui::FontData::from_static(INTER)));
    fonts.font_data.insert("jetbrains-mono".into(), Arc::new(egui::FontData::from_static(JETBRAINS_MONO)));
    let mut proportional = vec!["inter".to_string(), "jetbrains-mono".to_string()];
    let mut monospace = vec!["jetbrains-mono".to_string(), "inter".to_string()];
    let cjk = find_cjk_font();
    let found = cjk.as_ref().map(|(p, _)| p.clone());
    if let Some((_, bytes)) = cjk {
        fonts.font_data.insert("cjk".into(), Arc::new(egui::FontData::from_owned(bytes)));
        proportional.push("cjk".into());
        monospace.push("cjk".into());
    }
    fonts.families.insert(egui::FontFamily::Proportional, proportional);
    fonts.families.insert(egui::FontFamily::Monospace, monospace);
    ctx.set_fonts(fonts);
    found
}
