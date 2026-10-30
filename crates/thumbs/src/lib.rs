//! Decoding pictures in pure Rust, and the thumbnail cache.
//!
//! Decodable formats: JPEG, PNG, GIF, WebP, BMP, TIFF, ICO, QOI (`image` crate) and JPEG XL
//! (`jxl-oxide`). Videos get a thumbnail from a frame near the start, decoded by FilmCraft's
//! media sources, but play in the default app. Anything else (AVIF, HEIC, raw photos, videos
//! FilmCraft can't decode) gets a placeholder in the UI and opens in the default app.
//!
//! Thumbnails live in `<cache>/thumbs/ab/<hash>.jpg` (`.png` when the picture has transparency),
//! at most [`THUMB_SIZE`] pixels on the long side. Pictures are untrusted: sizes and allocations
//! are capped, and decoder panics are caught.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Duration;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, ImageFormat, ImageReader, Limits, RgbaImage};
use thiserror::Error;

pub use boorucraft_dedup::Hashes;

/// Long side of a thumbnail, in pixels.
pub const THUMB_SIZE: u32 = 320;
/// Largest picture side we decode.
pub const MAX_SIDE: u32 = 32_768;
/// Most memory a decoder may allocate.
pub const MAX_ALLOC: u64 = 1024 * 1024 * 1024;
/// Most frames kept for an animation.
pub const MAX_FRAMES: usize = 2000;

#[derive(Debug, Error)]
pub enum ThumbError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Decode(PathBuf, String),
    #[error("{0}: BooruCraft can't decode `{1}` files")]
    Unsupported(PathBuf, String),
    #[error("{0}: the decoder crashed on this file")]
    Crashed(PathBuf),
}

/// Whether BooruCraft can show this format itself (format names as stored in collection.json).
pub fn can_decode(format: &str) -> bool {
    matches!(format, "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "ico" | "qoi" | "jxl")
}

/// Video formats [`decode`] can take a frame from, through FilmCraft (if their codec is one it
/// decodes: H.264, HEVC, VP8, VP9, AV1, MPEG-2, ProRes and more).
pub const VIDEO_FORMATS: [&str; 11] = ["mp4", "mov", "mkv", "webm", "avi", "mpeg", "ts", "m2ts", "mts", "mxf", "3gp"];

/// Whether BooruCraft can make a thumbnail for this format: the pictures it decodes, and videos.
pub fn can_thumbnail(format: &str) -> bool {
    can_decode(format) || VIDEO_FORMATS.contains(&format)
}

/// Where in a video its thumbnail frame comes from: a little way in, past fades from black,
/// but not far, so little has to be decoded.
fn thumbnail_time(duration: f64) -> f64 {
    if duration.is_finite() && duration > 0.0 { (duration * 0.1).min(3.0) } else { 0.0 }
}

/// Opens a video as FilmCraft does: the container's index now, frames read on demand (never
/// the whole file).
pub fn open_video(path: &Path) -> Result<filmcraft_media::SharedSource, ThumbError> {
    let reader = filmcraft_media::reader::FileReader::open(path).map_err(|e| ThumbError::Io(path.to_path_buf(), e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    filmcraft_media::reader::open_reader_within(&name, std::sync::Arc::new(reader), &filmcraft_codecs::reader_openers(), &[], 0)
        .map_err(|e| ThumbError::Decode(path.to_path_buf(), e.to_string()))
}

/// A frame from near the start of a video, at about thumbnail size.
fn video_frame(path: &Path) -> Result<DynamicImage, ThumbError> {
    let err = |e: filmcraft_media::MediaError| ThumbError::Decode(path.to_path_buf(), e.to_string());
    let src = open_video(path)?;
    let info = src.info();
    let video = info.video.as_ref().ok_or_else(|| ThumbError::Decode(path.to_path_buf(), "no video stream".into()))?;
    let long = video.width.max(video.height).max(1) as f32;
    let scale = (2.0 * THUMB_SIZE as f32 / long).clamp(0.02, 1.0);
    let t = filmcraft_time::Tick::from_seconds_f64(thumbnail_time(info.duration.seconds()));
    let frame = match src.video_frame(filmcraft_media::FrameRequest { time: t, scale }) {
        Ok(f) => f,
        // A wrong duration or a damaged middle: try the first frame.
        Err(_) => src.video_frame(filmcraft_media::FrameRequest { time: filmcraft_time::Tick::ZERO, scale }).map_err(err)?,
    };
    if frame.width == 0 || frame.height == 0 || frame.width > MAX_SIDE || frame.height > MAX_SIDE {
        return Err(ThumbError::Decode(path.to_path_buf(), format!("bad frame size {}×{}", frame.width, frame.height)));
    }
    RgbaImage::from_raw(frame.width, frame.height, frame.to_rgba8())
        .map(DynamicImage::ImageRgba8)
        .ok_or_else(|| ThumbError::Decode(path.to_path_buf(), "frame doesn't match its size".into()))
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(MAX_SIDE);
    l.max_image_height = Some(MAX_SIDE);
    l.max_alloc = Some(MAX_ALLOC);
    l
}

fn image_format(format: &str) -> Option<ImageFormat> {
    Some(match format {
        "jpeg" => ImageFormat::Jpeg,
        "png" => ImageFormat::Png,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::WebP,
        "bmp" => ImageFormat::Bmp,
        "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        "qoi" => ImageFormat::Qoi,
        _ => return None,
    })
}

/// Runs a decoder, turning a panic into an error.
fn guarded<T>(path: &Path, f: impl FnOnce() -> Result<T, ThumbError>) -> Result<T, ThumbError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(ThumbError::Crashed(path.to_path_buf())))
}

/// Decodes the first frame of a picture, or a frame near the start of a video.
pub fn decode(path: &Path, format: &str) -> Result<DynamicImage, ThumbError> {
    if VIDEO_FORMATS.contains(&format) {
        return guarded(path, || video_frame(path));
    }
    guarded(path, || decode_inner(path, format))
}

fn decode_inner(path: &Path, format: &str) -> Result<DynamicImage, ThumbError> {
    let err = |e: image::ImageError| ThumbError::Decode(path.to_path_buf(), e.to_string());
    if format == "jxl" {
        let file = File::open(path).map_err(|e| ThumbError::Io(path.to_path_buf(), e))?;
        let mut decoder = jxl_oxide::integration::JxlDecoder::new(BufReader::new(file)).map_err(err)?;
        image::ImageDecoder::set_limits(&mut decoder, limits()).map_err(err)?;
        return DynamicImage::from_decoder(decoder).map_err(err);
    }
    let fmt = image_format(format).ok_or_else(|| ThumbError::Unsupported(path.to_path_buf(), format.to_string()))?;
    let file = File::open(path).map_err(|e| ThumbError::Io(path.to_path_buf(), e))?;
    let mut reader = ImageReader::with_format(BufReader::new(file), fmt);
    reader.limits(limits());
    reader.decode().map_err(err)
}

/// One frame of an animation and how long it shows.
pub struct Frame {
    pub image: RgbaImage,
    pub delay: Duration,
}

/// All frames of an animated GIF, PNG or WebP (at most [`MAX_FRAMES`]); one frame for anything
/// else.
pub fn decode_frames(path: &Path, format: &str) -> Result<Vec<Frame>, ThumbError> {
    guarded(path, || {
        let err = |e: image::ImageError| ThumbError::Decode(path.to_path_buf(), e.to_string());
        let open = || File::open(path).map(BufReader::new).map_err(|e| ThumbError::Io(path.to_path_buf(), e));
        let frames = match format {
            "gif" => {
                let mut d = GifDecoder::new(open()?).map_err(err)?;
                image::ImageDecoder::set_limits(&mut d, limits()).map_err(err)?;
                collect_frames(d.into_frames(), path)?
            }
            "webp" => {
                let mut d = WebPDecoder::new(open()?).map_err(err)?;
                image::ImageDecoder::set_limits(&mut d, limits()).map_err(err)?;
                if d.has_animation() { collect_frames(d.into_frames(), path)? } else { Vec::new() }
            }
            "png" => {
                let mut d = PngDecoder::new(open()?).map_err(err)?;
                image::ImageDecoder::set_limits(&mut d, limits()).map_err(err)?;
                if d.is_apng().map_err(err)? { collect_frames(d.apng().map_err(err)?.into_frames(), path)? } else { Vec::new() }
            }
            _ => Vec::new(),
        };
        if frames.is_empty() { Ok(vec![Frame { image: decode_inner(path, format)?.into_rgba8(), delay: Duration::ZERO }]) } else { Ok(frames) }
    })
}

fn collect_frames(frames: image::Frames, path: &Path) -> Result<Vec<Frame>, ThumbError> {
    let mut out = Vec::new();
    let mut bytes: u64 = 0;
    for f in frames.take(MAX_FRAMES) {
        let f = f.map_err(|e| ThumbError::Decode(path.to_path_buf(), e.to_string()))?;
        let (num, den) = f.delay().numer_denom_ms();
        let ms = num.checked_div(den).map_or(100, |ms| ms.max(20));
        let image = f.into_buffer();
        bytes = bytes.saturating_add(image.as_raw().len() as u64);
        out.push(Frame { image, delay: Duration::from_millis(u64::from(ms)) });
        if bytes > MAX_ALLOC {
            break;
        }
    }
    Ok(out)
}

/// The on-disk thumbnail cache.
#[derive(Debug, Clone)]
pub struct ThumbCache {
    dir: PathBuf,
}

impl ThumbCache {
    /// Thumbnails under `<cache_dir>/thumbs`.
    pub fn new(cache_dir: &Path) -> ThumbCache {
        ThumbCache { dir: cache_dir.join("thumbs") }
    }

    fn path(&self, hash: &str, ext: &str) -> Option<PathBuf> {
        // Hashes come from collection.json, which can be edited by hand.
        if hash.len() < 3 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.dir.join(&hash[..2]).join(format!("{hash}.{ext}")))
    }

    /// A cached thumbnail, if there is one and it decodes.
    pub fn load(&self, hash: &str) -> Option<RgbaImage> {
        ["jpg", "png"]
            .iter()
            .filter_map(|ext| self.path(hash, ext))
            .filter(|p| p.exists())
            .find_map(|p| decode(&p, if p.extension()? == "jpg" { "jpeg" } else { "png" }).ok().map(|i| i.into_rgba8()))
    }

    /// The thumbnail (cached or made now) and the perceptual hashes computed from it. Hashing
    /// the thumbnail rather than the full picture means a cached thumbnail needs no decoding.
    pub fn thumbnail_and_hashes(&self, hash: &str, source: &Path, format: &str) -> Result<(RgbaImage, Hashes), ThumbError> {
        let thumb = self.get_or_make(hash, source, format)?;
        let hashes = perceptual(&thumb).ok_or_else(|| ThumbError::Decode(source.to_path_buf(), "picture is empty".into()))?;
        Ok((thumb, hashes))
    }

    /// The cached thumbnail, or a new one made from `source` and stored.
    pub fn get_or_make(&self, hash: &str, source: &Path, format: &str) -> Result<RgbaImage, ThumbError> {
        if let Some(t) = self.load(hash) {
            return Ok(t);
        }
        let full = decode(source, format)?;
        let thumb = make_thumbnail(&full);
        // A failure to cache isn't a failure to show the thumbnail.
        let _ = self.store(hash, &thumb);
        Ok(thumb)
    }

    fn store(&self, hash: &str, thumb: &RgbaImage) -> Result<(), String> {
        let opaque = thumb.pixels().all(|p| p.0[3] == 255);
        let (ext, fmt) = if opaque { ("jpg", ImageFormat::Jpeg) } else { ("png", ImageFormat::Png) };
        let path = self.path(hash, ext).ok_or("bad hash")?;
        let dir = path.parent().ok_or("bad path")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = path.with_extension(format!("{ext}.tmp"));
        {
            let mut w = BufWriter::new(File::create(&tmp).map_err(|e| e.to_string())?);
            if opaque {
                DynamicImage::ImageRgba8(thumb.clone()).into_rgb8().write_to(&mut w, fmt).map_err(|e| e.to_string())?;
            } else {
                thumb.write_to(&mut w, fmt).map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}

/// dHash and pHash of a picture (best given a thumbnail: it is scaled down to 32×32 anyway).
/// Transparent parts count as mid-grey, so the colour hidden under them doesn't matter.
pub fn perceptual(img: &RgbaImage) -> Option<Hashes> {
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let gray = image::GrayImage::from_fn(img.width(), img.height(), |x, y| {
        let [r, g, b, a] = img.get_pixel(x, y).0;
        let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
        let alpha = f32::from(a) / 255.0;
        image::Luma([(luma * alpha + 128.0 * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8])
    });
    let grid = |w: usize, h: usize| -> Vec<f32> {
        image::imageops::resize(&gray, w as u32, h as u32, image::imageops::FilterType::Triangle).pixels().map(|p| f32::from(p.0[0])).collect()
    };
    let (dw, dh) = boorucraft_dedup::DHASH_SIZE;
    let n = boorucraft_dedup::PHASH_SIZE;
    Some(Hashes { dhash: boorucraft_dedup::dhash(&grid(dw, dh))?, phash: boorucraft_dedup::phash(&grid(n, n))? })
}

/// Scales a picture down to fit [`THUMB_SIZE`] (never up).
pub fn make_thumbnail(img: &DynamicImage) -> RgbaImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let scale = (THUMB_SIZE as f32 / w.max(h) as f32).min(1.0);
    let (tw, th) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
    image::imageops::thumbnail(&img.to_rgba8(), tw, th)
}

/// Scales a picture down so neither side exceeds `max_side` (GPU texture limit).
pub fn fit_within(img: RgbaImage, max_side: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w <= max_side && h <= max_side {
        return img;
    }
    let scale = max_side as f32 / w.max(h) as f32;
    image::imageops::resize(&img, ((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1), image::imageops::FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32, alpha: u8) {
        let img = RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, alpha]));
        img.save_with_format(path, ImageFormat::Png).unwrap();
    }

    #[test]
    fn thumbnails_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.png");
        write_png(&src, 1000, 500, 255);
        let cache = ThumbCache::new(&dir.path().join("cache"));
        let hash = "ab".repeat(32);
        let t = cache.get_or_make(&hash, &src, "png").unwrap();
        assert_eq!(t.dimensions(), (THUMB_SIZE, THUMB_SIZE / 2));
        assert!(dir.path().join("cache/thumbs/ab").join(format!("{hash}.jpg")).exists(), "opaque → jpg");
        std::fs::remove_file(&src).unwrap();
        assert!(cache.get_or_make(&hash, &src, "png").is_ok(), "served from the cache");

        let src2 = dir.path().join("b.png");
        write_png(&src2, 10, 10, 100);
        let h2 = "cd".repeat(32);
        assert_eq!(cache.get_or_make(&h2, &src2, "png").unwrap().dimensions(), (10, 10), "never scaled up");
        assert!(dir.path().join("cache/thumbs/cd").join(format!("{h2}.png")).exists(), "transparent → png");
        assert!(cache.load("../../etc").is_none());
    }

    #[test]
    fn resized_copies_have_close_hashes() {
        let pattern = |w: u32, h: u32| {
            RgbaImage::from_fn(w, h, |x, y| {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let v = if (fx - 0.3).powi(2) + (fy - 0.5).powi(2) < 0.04 { 230.0 } else { fx * 120.0 + fy * 60.0 };
                image::Rgba([v as u8, (v * 0.8) as u8, 90, 255])
            })
        };
        let a = perceptual(&make_thumbnail(&DynamicImage::ImageRgba8(pattern(1200, 800)))).unwrap();
        let b = perceptual(&make_thumbnail(&DynamicImage::ImageRgba8(pattern(450, 300)))).unwrap();
        let other = perceptual(&image::imageops::flip_horizontal(&pattern(450, 300))).unwrap();
        let d = |x: u64, y: u64| boorucraft_dedup::distance(x, y);
        assert!(d(a.dhash, b.dhash) <= 4 && d(a.phash, b.phash) <= 4, "{a:?} {b:?}");
        assert!(d(a.phash, other.phash) > 8, "a different picture");
        // Fully transparent pixels hash the same whatever colour they hide.
        let clear = |c: u8| RgbaImage::from_pixel(8, 8, image::Rgba([c, c, c, 0]));
        assert_eq!(perceptual(&clear(0)), perceptual(&clear(255)));
        assert_eq!(perceptual(&RgbaImage::new(0, 0)), None);
    }

    /// A Matroska file with one H.264 frame of `mbs_w`×`mbs_h` macroblocks, each sent as raw
    /// samples (I_PCM) so the decoded picture is known exactly: luma `luma(mb)`, grey chroma.
    fn h264_mkv(path: &Path, mbs_w: u32, mbs_h: u32, luma: impl Fn(u32) -> u8) {
        use craft_media::bitstream::{BitWriter, escape_rbsp};
        use craft_media::matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
        let nal = |header: u8, w: BitWriter| {
            let mut v = vec![header];
            v.extend(escape_rbsp(&w.finish()));
            v
        };
        let mut w = BitWriter::new();
        w.write_bits(66, 8); // Baseline
        w.write_bits(0, 8);
        w.write_bits(30, 8);
        w.write_ue(0); // sps id
        w.write_ue(0); // log2_max_frame_num - 4
        w.write_ue(0); // pic_order_cnt_type
        w.write_ue(0); // log2_max_pic_order_cnt_lsb - 4
        w.write_ue(1); // max_num_ref_frames
        w.write_bit(false);
        w.write_ue(mbs_w - 1);
        w.write_ue(mbs_h - 1);
        w.write_bit(true); // frame_mbs_only
        w.write_bit(true); // direct_8x8_inference
        w.write_bit(false); // cropping
        w.write_bit(false); // vui
        w.rbsp_trailing();
        let sps = nal(0x67, w);
        let mut w = BitWriter::new();
        w.write_ue(0); // pps id
        w.write_ue(0); // sps id
        w.write_bit(false); // CAVLC
        w.write_bit(false);
        w.write_ue(0); // slice groups
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bits(0, 2);
        w.write_se(0);
        w.write_se(0);
        w.write_se(0);
        w.write_bit(false); // deblocking_filter_control_present
        w.write_bit(false);
        w.write_bit(false);
        w.rbsp_trailing();
        let pps = nal(0x68, w);
        let mut w = BitWriter::new();
        w.write_ue(0); // first_mb_in_slice
        w.write_ue(7); // I slice
        w.write_ue(0); // pps id
        w.write_bits(0, 4); // frame_num
        w.write_ue(0); // idr_pic_id
        w.write_bits(0, 4); // pic_order_cnt_lsb
        w.write_bit(false); // no_output_of_prior_pics
        w.write_bit(false); // long_term_reference
        w.write_se(0); // slice_qp_delta
        for mb in 0..mbs_w * mbs_h {
            w.write_ue(25); // I_PCM
            w.align_zero();
            w.write_bytes(&[luma(mb); 256]);
            w.write_bytes(&[128; 128]);
        }
        w.rbsp_trailing();
        let slice = nal(0x65, w);
        let mut avcc = vec![1, 66, 0, 30, 0xff, 0xe1];
        avcc.extend((sps.len() as u16).to_be_bytes());
        avcc.extend(&sps);
        avcc.push(1);
        avcc.extend((pps.len() as u16).to_be_bytes());
        avcc.extend(&pps);
        let mut sample = (slice.len() as u32).to_be_bytes().to_vec();
        sample.extend(&slice);
        let mut track = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
        track.codec_private = avcc;
        track.video_size = Some((mbs_w * 16, mbs_h * 16));
        let file = std::fs::File::create(path).unwrap();
        let mut mkv = MkvWriter::new(file, vec![track], MuxOptions::default()).unwrap();
        mkv.write_frame(0, 0, true, &sample, Some(1_000_000_000)).unwrap();
        mkv.finish().unwrap();
    }

    #[test]
    fn videos_get_a_thumbnail_from_a_frame() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("clip.mkv");
        // 3×2 macroblocks; only the top-left one is white.
        h264_mkv(&src, 3, 2, |mb| if mb == 0 { 235 } else { 16 });
        let img = decode(&src, "mkv").unwrap().into_rgba8();
        assert_eq!(img.dimensions(), (48, 32));
        assert_eq!(img.get_pixel(2, 2).0, [255, 255, 255, 255], "top-left: white");
        assert_eq!(img.get_pixel(40, 25).0, [0, 0, 0, 255], "elsewhere: black");
        let cache = ThumbCache::new(&dir.path().join("cache"));
        assert_eq!(cache.get_or_make(&"ef".repeat(32), &src, "mkv").unwrap().dimensions(), (48, 32));
        assert!(can_thumbnail("mkv") && can_thumbnail("mp4") && can_thumbnail("avi") && can_thumbnail("png") && !can_thumbnail("wmv"));
        assert!(!can_decode("mkv"), "the viewer still opens videos in the default app");

        // Damaged or empty videos are errors, not crashes.
        let bad = dir.path().join("bad.mp4");
        std::fs::write(&bad, b"\0\0\0\x18ftypisom nonsense").unwrap();
        assert!(matches!(decode(&bad, "mp4"), Err(ThumbError::Decode(..))));
        let mut cut = std::fs::read(&src).unwrap();
        cut.truncate(cut.len() / 2);
        std::fs::write(&bad, &cut).unwrap();
        assert!(matches!(decode(&bad, "mkv"), Err(ThumbError::Decode(..) | ThumbError::Crashed(..))));
    }

    #[test]
    fn thumbnail_time_stays_near_the_start() {
        assert_eq!(thumbnail_time(0.0), 0.0);
        assert_eq!(thumbnail_time(f64::NAN), 0.0);
        assert_eq!(thumbnail_time(10.0), 1.0);
        assert_eq!(thumbnail_time(3600.0), 3.0);
    }

    #[test]
    fn bad_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.png");
        std::fs::write(&p, b"\x89PNG\r\n\x1a\nnonsense").unwrap();
        assert!(matches!(decode(&p, "png"), Err(ThumbError::Decode(..))));
        assert!(matches!(decode(&p, "heic"), Err(ThumbError::Unsupported(..))));
        assert!(matches!(decode(&dir.path().join("none"), "png"), Err(ThumbError::Io(..))));
        assert!(!can_decode("heic") && !can_decode("dng") && can_decode("jxl"));
    }

    #[test]
    fn frames_of_a_still_picture() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        write_png(&p, 4, 4, 255);
        let f = decode_frames(&p, "png").unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(fit_within(f.into_iter().next().unwrap().image, 2).dimensions(), (2, 2));
    }

    #[test]
    fn animated_gif_frames() {
        use image::codecs::gif::GifEncoder;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.gif");
        {
            let mut enc = GifEncoder::new(File::create(&p).unwrap());
            let frames = (0..3).map(|i| {
                image::Frame::from_parts(RgbaImage::from_pixel(4, 4, image::Rgba([i * 80, 0, 0, 255])), 0, 0, image::Delay::from_numer_denom_ms(50, 1))
            });
            enc.encode_frames(frames).unwrap();
        }
        let f = decode_frames(&p, "gif").unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].delay, Duration::from_millis(50));
    }
}
