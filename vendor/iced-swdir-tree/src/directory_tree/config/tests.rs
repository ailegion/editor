//! Unit tests for [`super::DirectoryFilter`].

use super::*;

#[test]
fn default_is_files_and_folders() {
    assert_eq!(DirectoryFilter::default(), DirectoryFilter::FilesAndFolders);
}

#[test]
fn filter_predicates() {
    assert!(DirectoryFilter::FoldersOnly.skips_hidden());
    assert!(DirectoryFilter::FoldersOnly.skips_files());

    assert!(DirectoryFilter::FilesAndFolders.skips_hidden());
    assert!(!DirectoryFilter::FilesAndFolders.skips_files());

    assert!(!DirectoryFilter::AllIncludingHidden.skips_hidden());
    assert!(!DirectoryFilter::AllIncludingHidden.skips_files());
}
