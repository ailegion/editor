//! Most-recently-opened files, persisted across sessions and used to seed Quick Open's
//! results when its query is empty (mirroring most editors' Cmd+P "recent files first"
//! behavior).

use std::path::PathBuf;

const MAX_ENTRIES: usize = 20;

fn path() -> Option<PathBuf> {
    crate::config_path("recent_files")
}

pub fn load() -> Vec<PathBuf> {
    let Some(path) = path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().map(PathBuf::from).filter(|p| p.is_file()).take(MAX_ENTRIES).collect()
}

/// Moves `opened` to the front of `recent` (inserting it if new), dedups, caps length, and
/// persists.
pub fn record(recent: &mut Vec<PathBuf>, opened: PathBuf) {
    recent.retain(|p| p != &opened);
    recent.insert(0, opened);
    recent.truncate(MAX_ENTRIES);
    save(recent);
}

fn save(recent: &[PathBuf]) {
    let Some(path) = path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = recent.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    let _ = std::fs::write(path, text);
}
