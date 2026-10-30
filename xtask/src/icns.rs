//! `cargo xtask icns <out.icns> <in.png>…`: pack PNG images into a macOS `.icns`, so the icon
//! can be built on any system (Apple's `iconutil` only runs on macOS).
//!
//! An `.icns` is the magic `icns` and the file length, then one entry per image: a four-letter
//! type that says its size, the entry length, and the PNG as is (macOS 10.7 and later).

use std::path::Path;

use crate::ico::png_size;

/// The entry types for a square PNG of `size` pixels. Retina types (`@2x`) reuse the image of
/// twice the size, so one 32 px PNG serves both 32×32 and 16×16@2x.
fn types_for(size: u32) -> &'static [&'static [u8; 4]] {
    match size {
        16 => &[b"icp4"],
        32 => &[b"icp5", b"ic11"],
        64 => &[b"ic12"],
        128 => &[b"ic07"],
        256 => &[b"ic08", b"ic13"],
        512 => &[b"ic09", b"ic14"],
        1024 => &[b"ic10"],
        _ => &[],
    }
}

/// An `.icns` holding `pngs`, each square and 16, 32, 64, 128, 256, 512 or 1024 px.
pub fn pack(pngs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if pngs.is_empty() {
        return Err("need at least one PNG".into());
    }
    let mut body = Vec::new();
    for png in pngs {
        let (w, h) = png_size(png)?;
        let types = types_for(w);
        if w != h || types.is_empty() {
            return Err(format!("icns images must be square and 16, 32, 64, 128, 256, 512 or 1024 px, got {w}x{h}"));
        }
        let len = u32::try_from(png.len() + 8).map_err(|_| "PNG too large for an icns entry".to_string())?;
        for ty in types {
            body.extend_from_slice(&ty[..]);
            body.extend_from_slice(&len.to_be_bytes());
            body.extend_from_slice(png);
        }
    }
    let total = u32::try_from(body.len() + 8).map_err(|_| "icns too large".to_string())?;
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let [out, inputs @ ..] = args else {
        return Err("usage: cargo xtask icns <out.icns> <in.png>…".into());
    };
    let pngs = inputs.iter().map(|p| std::fs::read(p).map_err(|e| format!("read {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let icns = pack(&pngs)?;
    std::fs::write(Path::new(out), &icns).map_err(|e| format!("write {out}: {e}"))?;
    println!("{out}: {} images, {} bytes", pngs.len(), icns.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_png(size: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 1, 2, 3, 4]);
        v
    }

    #[test]
    fn packs_entries_with_lengths() {
        let a = fake_png(16);
        let b = fake_png(32);
        let icns = pack(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize, icns.len());
        // 16 px: one entry; 32 px: two (32×32 and 16×16@2x).
        assert_eq!(&icns[8..12], b"icp4");
        assert_eq!(u32::from_be_bytes(icns[12..16].try_into().unwrap()) as usize, a.len() + 8);
        let second = 8 + 8 + a.len();
        assert_eq!(&icns[second..second + 4], b"icp5");
        let third = second + 8 + b.len();
        assert_eq!(&icns[third..third + 4], b"ic11");
        assert_eq!(icns.len(), 8 + (8 + a.len()) + 2 * (8 + b.len()));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(pack(&[]).is_err());
        assert!(pack(&[fake_png(48)]).is_err());
        assert!(pack(&[b"GIF89a".to_vec()]).is_err());
    }

    #[test]
    fn committed_icon_is_valid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/app-icon/boorucraft.icns");
        let Ok(icns) = std::fs::read(&path) else { return };
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize, icns.len());
    }
}
