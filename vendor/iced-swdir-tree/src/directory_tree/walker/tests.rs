//! Unit tests for the walker's normalization + filter layer.

use super::*;
use crate::directory_tree::config::DirectoryFilter;
use std::fs;
use swdir::scan_dir;

/// Minimal self-cleaning temp directory.
struct TmpDir(PathBuf);

impl TmpDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "iced-swdir-tree-test-{}-{}-{}",
            std::process::id(),
            nanos,
            tag
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("create tmpdir");
        Self(p)
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn filter_folders_only_drops_files() {
    let td = TmpDir::new("folders-only");
    fs::create_dir(td.0.join("sub")).unwrap();
    fs::write(td.0.join("file.txt"), b"").unwrap();
    let raw = scan_dir(&td.0).unwrap();
    let out: Vec<_> = normalize_entries(&raw)
        .into_iter()
        .filter(|e| e.passes(DirectoryFilter::FoldersOnly))
        .collect();
    assert_eq!(out.len(), 1);
    assert!(out[0].is_dir);
}

#[test]
fn filter_files_and_folders_drops_hidden() {
    let td = TmpDir::new("no-hidden");
    fs::write(td.0.join(".secret"), b"").unwrap();
    fs::write(td.0.join("visible.txt"), b"").unwrap();
    let raw = scan_dir(&td.0).unwrap();
    let out: Vec<_> = normalize_entries(&raw)
        .into_iter()
        .filter(|e| e.passes(DirectoryFilter::FilesAndFolders))
        .collect();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].path.file_name().unwrap(), "visible.txt");
}

#[test]
fn filter_all_includes_hidden() {
    let td = TmpDir::new("with-hidden");
    fs::write(td.0.join(".secret"), b"").unwrap();
    fs::write(td.0.join("visible.txt"), b"").unwrap();
    let raw = scan_dir(&td.0).unwrap();
    let out: Vec<_> = normalize_entries(&raw)
        .into_iter()
        .filter(|e| e.passes(DirectoryFilter::AllIncludingHidden))
        .collect();
    assert_eq!(out.len(), 2);
}

#[test]
fn folders_sort_before_files() {
    let td = TmpDir::new("sort");
    fs::create_dir(td.0.join("zebra")).unwrap();
    fs::write(td.0.join("alpha"), b"").unwrap();
    let raw = scan_dir(&td.0).unwrap();
    let out = normalize_entries(&raw);
    assert_eq!(out.len(), 2);
    assert!(out[0].is_dir);
    assert!(!out[1].is_dir);
}
