//! Windows only: embed the app icon and version info (VERSIONINFO) into `boorucraft.exe`, so it
//! shows in Explorer, the taskbar, the Start menu and Alt-Tab. Adapted from FilmCraft's.
//!
//! On every other target this does nothing (`winresource` is only a build-dependency on Windows
//! hosts). A missing resource compiler is a warning, unless `BOORUCRAFT_REQUIRE_WINRES=1` (set by
//! the release packaging) makes it an error.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/boorucraft.ico");
    println!("cargo:rerun-if-env-changed=BOORUCRAFT_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    windows::embed_resources();
}

#[cfg(windows)]
mod windows {
    pub fn embed_resources() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/app-icon/boorucraft.ico")
            .set("ProductName", "BooruCraft")
            .set("FileDescription", "BooruCraft picture organiser")
            .set("LegalCopyright", "Copyright (c) the BooruCraft contributors. MIT OR Apache-2.0.")
            .set("OriginalFilename", "boorucraft.exe")
            .set("InternalName", "boorucraft");
        if let Err(e) = res.compile() {
            if std::env::var_os("BOORUCRAFT_REQUIRE_WINRES").is_some() {
                #[allow(clippy::panic)]
                {
                    panic!("embedding Windows resources failed: {e}");
                }
            }
            println!("cargo:warning=boorucraft.exe built without icon/version resources: {e}");
        }
    }
}

/// Cross-compiling for Windows from macOS or Linux: `winresource` is not available on this host.
#[cfg(not(windows))]
mod windows {
    pub fn embed_resources() {
        println!("cargo:warning=boorucraft.exe built without icon/version resources (cross-compiled from a non-Windows host)");
    }
}
