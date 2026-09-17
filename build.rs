//! Copies the licence of the compiled-in Dark+/Light+ themes into `themes/`, which is what
//! gets shipped next to the executable (see `package.metadata.bundle.resources`).

use std::fs;
use std::path::Path;

const SOURCE: &str = "src/theme/default/LICENSE.txt";
const TARGET: &str = "themes/vscode.theme-defaults/LICENSE.txt";

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed={TARGET}");
    let license = fs::read(SOURCE).expect("reading the default themes' licence");
    // Only write on change so the target's mtime doesn't trigger a rebuild every time.
    if fs::read(TARGET).ok().as_deref() != Some(license.as_slice()) {
        fs::create_dir_all(Path::new(TARGET).parent().unwrap()).expect("creating the licence folder");
        fs::write(TARGET, license).expect("copying the default themes' licence");
    }
}
