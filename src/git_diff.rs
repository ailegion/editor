//! Per-file diff gutter markers: the open buffer compared with the file's staged version, the
//! same comparison as `git diff`. Computed in-process from the cached blob, so markers follow
//! unsaved edits and opening or editing a tab never starts `git`.

use std::path::PathBuf;
use std::sync::Arc;

use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    Added,
    Modified,
    /// Attached to the new-file line immediately after which content was removed (or line 0
    /// if the removal was at the very start of the file) -- there's no "this line" to mark
    /// for a pure deletion, since the deleted lines no longer exist in the new file.
    Removed,
}

/// Markers for `text`, the buffer of `path` (relative to the project root). Empty when the
/// project is not a repository or the file is untracked, matching a clean file.
pub async fn diff_for_buffer(repo: Option<Arc<crate::git::repo::Repo>>, path: PathBuf, text: String) -> Vec<(usize, LineStatus)> {
    let Some(repo) = repo else { return Vec::new() };
    tokio::task::spawn_blocking(move || match repo.buffer_base(&path) {
        Ok(Some(base)) => line_changes(&base, &text),
        _ => Vec::new(),
    })
    .await
    .unwrap_or_default()
}

/// 0-indexed buffer lines paired with their status relative to `base`. Line endings are
/// compared by content, as the buffer holds lines without their terminators.
fn line_changes(base: &[u8], text: &str) -> Vec<(usize, LineStatus)> {
    if base[..base.len().min(8000)].contains(&0) { return Vec::new(); }
    let normalize = |text: &str| {
        let mut text = text.replace("\r\n", "\n");
        if !text.ends_with('\n') { text.push('\n'); }
        text
    };
    let (before, after) = (normalize(&String::from_utf8_lossy(base)), normalize(text));
    let input = InternedInput::new(before.as_bytes(), after.as_bytes());
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    let mut result = Vec::new();
    for hunk in diff.hunks() {
        if hunk.after.is_empty() {
            result.push((hunk.after.start as usize, LineStatus::Removed));
        } else {
            let status = if hunk.before.is_empty() { LineStatus::Added } else { LineStatus::Modified };
            result.extend(hunk.after.map(|line| (line as usize, status)));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_added_modified_and_removed_lines() {
        let base = b"one\ntwo\nthree\nfour\n";
        assert_eq!(line_changes(base, "one\ntwo\nthree\nfour"), vec![]);
        assert_eq!(line_changes(base, "zero\none\ntwo\nthree\nfour"), vec![(0, LineStatus::Added)]);
        assert_eq!(line_changes(base, "one\nTWO\nthree\nfour"), vec![(1, LineStatus::Modified)]);
        assert_eq!(line_changes(base, "one\nfour"), vec![(1, LineStatus::Removed)]);
        assert_eq!(line_changes(base, "two\nthree\nfour"), vec![(0, LineStatus::Removed)]);
    }

    #[test]
    fn line_endings_and_binary_files_do_not_produce_markers() {
        assert!(line_changes(b"a\r\nb\r\n", "a\nb").is_empty());
        assert!(line_changes(b"a\0b", "changed").is_empty());
    }
}
