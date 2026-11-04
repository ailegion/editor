//! Most-recently-opened project folders, persisted across sessions and listed on the welcome
//! screen and in File > Open Recent.

use std::path::{Path, PathBuf};

pub const MAX_ENTRIES: usize = 10;

/// Where the list is saved; `None` (tests) keeps it in memory only.
pub fn path() -> Option<PathBuf> {
    crate::config_path("recent_projects")
}

/// The saved list, most recent first, without folders that no longer exist.
pub fn load(file: Option<&Path>) -> Vec<PathBuf> {
    let Some(text) = file.and_then(|file| std::fs::read_to_string(file).ok()) else { return Vec::new() };
    text.lines().map(PathBuf::from).filter(|path| path.is_dir()).take(MAX_ENTRIES).collect()
}

/// Moves `opened` to the front of `recent` (inserting it if new), caps the length, and saves.
pub fn record(recent: &mut Vec<PathBuf>, opened: PathBuf, file: Option<&Path>) {
    recent.retain(|path| !crate::workspaces::same_folder(path, &opened));
    recent.insert(0, opened);
    recent.truncate(MAX_ENTRIES);
    let Some(file) = file else { return };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let text = recent.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n");
    let _ = std::fs::write(file, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates_or_missing_folders() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config").join("recent_projects");
        let folders: Vec<PathBuf> = (0..12).map(|i| dir.path().join(format!("p{i}"))).collect();
        for folder in &folders { std::fs::create_dir(folder).unwrap(); }
        let mut recent = Vec::new();
        for folder in &folders { record(&mut recent, folder.clone(), Some(&file)); }
        assert_eq!(recent.len(), MAX_ENTRIES);
        assert_eq!(recent[0], folders[11]);
        record(&mut recent, folders[5].clone(), Some(&file));
        assert_eq!(recent[0], folders[5]);
        assert_eq!(recent.iter().filter(|path| **path == folders[5]).count(), 1);
        assert_eq!(load(Some(&file)), recent);
        std::fs::remove_dir(&folders[5]).unwrap();
        assert!(!load(Some(&file)).contains(&folders[5]), "a deleted folder is not offered");
        assert!(load(None).is_empty());
        // Without a file nothing is written, but the list still updates.
        let mut memory = Vec::new();
        record(&mut memory, folders[0].clone(), None);
        assert_eq!(memory, [folders[0].clone()]);
    }
}
