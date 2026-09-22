//! Copies the licence of the compiled-in Dark+/Light+ themes into `themes/`, which is what
//! gets shipped next to the executable (see `package.metadata.bundle.resources`).

use std::fs;
use std::path::Path;

const SOURCE: &str = "src/theme/default/LICENSE.txt";
const TARGET: &str = "themes/vscode.theme-defaults/LICENSE.txt";

fn main() {
    #[cfg(windows)]
    embed_windows_icon();

    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed={TARGET}");
    let license = fs::read(SOURCE).expect("reading the default themes' licence");
    // Only write on change so the target's mtime doesn't trigger a rebuild every time.
    if fs::read(TARGET).ok().as_deref() != Some(license.as_slice()) {
        fs::create_dir_all(Path::new(TARGET).parent().unwrap()).expect("creating the licence folder");
        fs::write(TARGET, license).expect("copying the default themes' licence");
    }
}

#[cfg(windows)]
fn embed_windows_icon() {
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::imageops::FilterType;

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=icon.png");
    let source = image::open("icon.png").expect("reading the application icon");
    let frames: Vec<_> = [16, 24, 32, 48, 64, 128, 256]
        .into_iter()
        .map(|size| {
            let pixels = source.resize_exact(size, size, FilterType::Lanczos3).to_rgba8();
            IcoFrame::as_png(pixels.as_raw(), size, size, image::ExtendedColorType::Rgba8)
                .expect("encoding a Windows icon frame")
        })
        .collect();
    let icon_path = Path::new(&std::env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("editor.ico");
    IcoEncoder::new(fs::File::create(&icon_path).expect("creating the Windows icon"))
        .encode_images(&frames)
        .expect("writing the Windows icon");
    winresource::WindowsResource::new()
        .set_icon(icon_path.to_str().expect("Windows icon path"))
        .compile()
        .expect("embedding the Windows executable icon");
}
