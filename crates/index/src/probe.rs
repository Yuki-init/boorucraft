//! Reading one file: content hashes and metadata from its headers.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use boorucraft_store::{Kind, to_hex};
use chrono::{DateTime, NaiveDateTime, Utc};
use md5::Md5;
use sha2::{Digest, Sha256};

/// Bytes kept from the start of each file for format sniffing.
const HEADER_BYTES: usize = 64 * 1024;

/// Extensions included by default (lowercase).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "webp", "bmp", "tif", "tiff", "jxl", "avif", "heic", "heif", "ico", "qoi", "tga", "psd", "dng", "cr2",
    "cr3", "nef", "arw", "orf", "rw2", "raf",
];
/// Camera raw formats; opened in the system's default app.
pub const RAW_EXTENSIONS: &[&str] = &["dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf"];
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "3gp"];

pub struct Hashes {
    pub sha256: String,
    pub md5: String,
    pub header: Vec<u8>,
}

/// Hashes a file in one pass, keeping its first bytes.
pub fn hash_file(path: &Path) -> std::io::Result<Hashes> {
    let mut file = File::open(path)?;
    let mut sha = Sha256::new();
    let mut md5 = Md5::new();
    let mut header = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        let Some(chunk) = buf.get(..n) else { break };
        if chunk.is_empty() {
            break;
        }
        if header.len() < HEADER_BYTES {
            let want = (HEADER_BYTES - header.len()).min(chunk.len());
            header.extend_from_slice(chunk.get(..want).unwrap_or_default());
        }
        sha.update(chunk);
        md5.update(chunk);
    }
    Ok(Hashes { sha256: to_hex(&sha.finalize()), md5: to_hex(&md5.finalize()), header })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub kind: Kind,
    pub format: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub animated: bool,
    pub taken: Option<DateTime<Utc>>,
}

/// Format, size, animation and EXIF date. Files are untrusted: any parser trouble just leaves a
/// field empty, and a panic inside a parser is caught.
pub fn probe(path: &Path, header: &[u8], extension: &str) -> Meta {
    std::panic::catch_unwind(|| probe_inner(path, header, extension)).unwrap_or_else(|_| fallback(extension))
}

fn fallback(extension: &str) -> Meta {
    let kind = if VIDEO_EXTENSIONS.contains(&extension) {
        Kind::Video
    } else if IMAGE_EXTENSIONS.contains(&extension) {
        Kind::Image
    } else {
        Kind::Other
    };
    let format = match extension {
        "jpg" | "jpe" | "jfif" => "jpeg",
        "tif" => "tiff",
        "m4v" => "mp4",
        "mpg" => "mpeg",
        e => e,
    };
    Meta { kind, format: format.to_string(), ..Meta::default() }
}

fn probe_inner(path: &Path, header: &[u8], extension: &str) -> Meta {
    let Ok(image_type) = imagesize::image_type(header) else { return fallback(extension) };
    let mut meta = Meta { kind: Kind::Image, format: format_name(&image_type), ..Meta::default() };
    if RAW_EXTENSIONS.contains(&extension) {
        // Raw photos are TIFF-like containers whose first image is a small preview, so the
        // header size would be wrong: keep the raw format and leave the size unknown.
        meta.format = extension.to_string();
        meta.taken = exif_taken(path);
        return meta;
    }
    if let Ok(size) = imagesize::size(path) {
        meta.width = u32::try_from(size.width).ok();
        meta.height = u32::try_from(size.height).ok();
    }
    meta.animated = is_animated(&meta.format, header);
    meta.taken = exif_taken(path);
    meta
}

fn format_name(t: &imagesize::ImageType) -> String {
    use imagesize::ImageType as T;
    match t {
        T::Jpeg => "jpeg".into(),
        T::Png => "png".into(),
        T::Gif => "gif".into(),
        T::Webp => "webp".into(),
        T::Bmp => "bmp".into(),
        T::Tiff => "tiff".into(),
        T::Jxl => "jxl".into(),
        T::Ico => "ico".into(),
        T::Qoi => "qoi".into(),
        T::Heif(imagesize::Compression::Av1) => "avif".into(),
        T::Heif(_) => "heic".into(),
        // Anything else: the variant's name, e.g. `Psd` → `psd`.
        other => format!("{other:?}").chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase(),
    }
}

/// Animated GIF (looping extension or a second frame), APNG (`acTL` chunk) or animated WebP
/// (`VP8X` animation flag), judged from the first 64 KiB.
fn is_animated(format: &str, header: &[u8]) -> bool {
    match format {
        "gif" => contains(header, b"NETSCAPE2.0") || header.iter().filter(|&&b| b == 0x2c).count() > 1 && contains(header, b"\x21\xf9\x04"),
        "png" => png_has_actl(header),
        "webp" => header.get(12..16) == Some(b"VP8X") && header.get(20).is_some_and(|f| f & 0x02 != 0),
        _ => false,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Walks PNG chunks until `IDAT`; true if an `acTL` comes first.
fn png_has_actl(header: &[u8]) -> bool {
    let mut pos = 8usize;
    while let (Some(len), Some(kind)) = (header.get(pos..pos + 4), header.get(pos + 4..pos + 8)) {
        if kind == b"acTL" {
            return true;
        }
        if kind == b"IDAT" {
            return false;
        }
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        pos = match pos.checked_add(12).and_then(|p| p.checked_add(len)) {
            Some(p) => p,
            None => return false,
        };
    }
    false
}

/// `DateTimeOriginal` from EXIF, read as UTC (EXIF dates carry no time zone).
fn exif_taken(path: &Path) -> Option<DateTime<Utc>> {
    let file = File::open(path).ok()?;
    let exif = exif::Reader::new().read_from_container(&mut BufReader::new(file)).ok()?;
    let field = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY).or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))?;
    let exif::Value::Ascii(values) = &field.value else { return None };
    let text = std::str::from_utf8(values.first()?).ok()?;
    let naive = NaiveDateTime::parse_from_str(text.trim(), "%Y:%m:%d %H:%M:%S").ok()?;
    Some(naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_animation_flag() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0; 13 + 4]);
        let still = [png.clone(), vec![0, 0, 0, 0], b"IDAT".to_vec(), vec![0; 4]].concat();
        let animated = [png.clone(), vec![0, 0, 0, 8], b"acTL".to_vec(), vec![0; 12]].concat();
        assert!(!png_has_actl(&still));
        assert!(png_has_actl(&animated));
        let hostile = [png, vec![0xff, 0xff, 0xff, 0xff], b"zzzz".to_vec()].concat();
        assert!(!png_has_actl(&hostile));
    }

    #[test]
    fn unknown_content_falls_back_to_the_extension() {
        let m = probe(Path::new("/nonexistent"), b"not an image", "mp4");
        assert_eq!((m.kind, m.format.as_str()), (Kind::Video, "mp4"));
        let m = probe(Path::new("/nonexistent"), b"", "jpg");
        assert_eq!((m.kind, m.format.as_str()), (Kind::Image, "jpeg"));
    }
}
