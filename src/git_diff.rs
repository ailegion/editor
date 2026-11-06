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

/// One change between the staged version and the buffer, as the gutter marks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The buffer lines the change covers; empty, at the line after it, for a pure removal.
    pub lines: std::ops::Range<usize>,
    /// The staged lines it replaced, without line endings.
    pub old: Vec<String>,
    /// The buffer's lines in `lines` when the diff was computed.
    pub new: Vec<String>,
}

impl Hunk {
    /// Whether the gutter marker of this change is on `line`.
    pub fn marks(&self, line: usize) -> bool {
        if self.lines.is_empty() { line == self.lines.start } else { self.lines.contains(&line) }
    }
}

/// Markers and changes for `text`, the buffer of `path` (relative to the project root). Empty
/// when the project is not a repository or the file is untracked, matching a clean file.
pub async fn diff_for_buffer(repo: Option<Arc<crate::git::repo::Repo>>, path: PathBuf, text: String) -> (Vec<(usize, LineStatus)>, Vec<Hunk>) {
    let Some(repo) = repo else { return Default::default() };
    tokio::task::spawn_blocking(move || match repo.buffer_base(&path) {
        Ok(Some(base)) => changes(&base, &text),
        _ => Default::default(),
    })
    .await
    .unwrap_or_default()
}

/// 0-indexed buffer lines paired with their status relative to `base`. Line endings are
/// compared by content, as the buffer holds lines without their terminators.
#[cfg(test)]
fn line_changes(base: &[u8], text: &str) -> Vec<(usize, LineStatus)> {
    changes(base, text).0
}

/// [`line_changes`] together with the changes they mark.
fn changes(base: &[u8], text: &str) -> (Vec<(usize, LineStatus)>, Vec<Hunk>) {
    if base[..base.len().min(8000)].contains(&0) { return Default::default(); }
    let normalize = |text: &str| {
        let mut text = text.replace("\r\n", "\n");
        if !text.ends_with('\n') { text.push('\n'); }
        text
    };
    let (before, after) = (normalize(&String::from_utf8_lossy(base)), normalize(text));
    let input = InternedInput::new(before.as_bytes(), after.as_bytes());
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    // The diff's tokens are these lines, each with its line break.
    let split = |text: &str| text.split_inclusive('\n').map(|line| line.strip_suffix('\n').unwrap_or(line).to_string()).collect::<Vec<_>>();
    let (old_lines, new_lines) = (split(&before), split(&after));
    let mut result = Vec::new();
    let mut hunks = Vec::new();
    for hunk in diff.hunks() {
        let lines = hunk.after.start as usize..hunk.after.end as usize;
        hunks.push(Hunk {
            lines: lines.clone(),
            old: old_lines[hunk.before.start as usize..hunk.before.end as usize].to_vec(),
            new: new_lines[lines].to_vec(),
        });
        if hunk.after.is_empty() {
            result.push((hunk.after.start as usize, LineStatus::Removed));
        } else {
            let status = if hunk.before.is_empty() { LineStatus::Added } else { LineStatus::Modified };
            result.extend(hunk.after.map(|line| (line as usize, status)));
        }
    }
    (result, hunks)
}

/// Puts `hunk`'s staged lines back in place of its buffer lines, as one undoable edit. Does
/// nothing and returns `false` when the buffer no longer holds the lines the change was
/// computed for.
pub fn restore(buffer: &mut crate::code_editor::Buffer, hunk: &Hunk) -> bool {
    use cosmic_text::Cursor;
    let count = buffer.line_count();
    let (start, end) = (hunk.lines.start, hunk.lines.end);
    let line = |index: usize| buffer.inner.lines[index].text().to_string();
    if end > count || (start == end && start > count) || (start..end).map(line).ne(hunk.new.iter().cloned()) {
        return false;
    }
    let len = |index: usize| buffer.inner.lines[index].text().len();
    let old = hunk.old.join("\n");
    let (from, to, text) = if start == end {
        // A removal: the staged lines go back before the line now after them.
        if start < count {
            (Cursor::new(start, 0), Cursor::new(start, 0), format!("{old}\n"))
        } else {
            let last = count - 1;
            (Cursor::new(last, len(last)), Cursor::new(last, len(last)), format!("\n{old}"))
        }
    } else if !hunk.old.is_empty() {
        (Cursor::new(start, 0), Cursor::new(end - 1, len(end - 1)), old)
    } else if end < count {
        // An addition: its lines go, line breaks included.
        (Cursor::new(start, 0), Cursor::new(end, 0), String::new())
    } else if start > 0 {
        (Cursor::new(start - 1, len(start - 1)), Cursor::new(end - 1, len(end - 1)), String::new())
    } else {
        (Cursor::new(0, 0), Cursor::new(end - 1, len(end - 1)), String::new())
    };
    buffer.replace_range(from, to, &text);
    true
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
    fn hunks_hold_the_staged_and_the_buffer_lines() {
        let base = b"one\ntwo\nthree\nfour\n";
        let hunk = |lines: std::ops::Range<usize>, old: &[&str], new: &[&str]| Hunk {
            lines, old: old.iter().map(|s| s.to_string()).collect(), new: new.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(changes(base, "one\ntwo\nthree\nfour").1, vec![]);
        assert_eq!(changes(base, "zero\none\ntwo\nthree\nfour").1, vec![hunk(0..1, &[], &["zero"])]);
        assert_eq!(changes(base, "one\nTWO\nthree\nfour").1, vec![hunk(1..2, &["two"], &["TWO"])]);
        assert_eq!(changes(base, "one\nfour").1, vec![hunk(1..1, &["two", "three"], &[])]);
        assert_eq!(changes(b"one\r\ntwo\r\n", "one\nTWO").1, vec![hunk(1..2, &["two"], &["TWO"])]);
        let removed = hunk(1..1, &["two"], &[]);
        assert!(removed.marks(1) && !removed.marks(0) && !removed.marks(2));
        let modified = hunk(1..3, &["x"], &["a", "b"]);
        assert!(modified.marks(1) && modified.marks(2) && !modified.marks(3) && !modified.marks(0));
    }

    /// Restoring every change, one at a time as from the gutter, gives back the staged text,
    /// and each restore is a single undo step.
    #[test]
    fn restoring_every_change_gives_back_the_staged_text() {
        let normalize = |text: &str| {
            let mut text = text.replace("\r\n", "\n");
            if !text.ends_with('\n') { text.push('\n'); }
            text
        };
        let cases: &[(&str, &str)] = &[
            ("one\ntwo\nthree\nfour\n", "zero\none\ntwo\nthree\nfour"),
            ("one\ntwo\nthree\nfour\n", "one\nTWO\nthree\nfour"),
            ("one\ntwo\nthree\nfour\n", "one\nfour"),
            ("one\ntwo\nthree\nfour\n", "two\nthree\nfour"),
            ("one\ntwo\nthree\nfour\n", "one\ntwo\n"),
            ("one\ntwo\nthree\nfour", "one\ntwo"),
            ("one\ntwo\n", "one\ntwo\nthree\n"),
            ("one\ntwo", "one\ntwo\nthree"),
            ("", "new\nfile\n"),
            ("one\ntwo\n", ""),
            ("one\r\ntwo\r\nthree\r\n", "one\nTWO\nthree\n"),
            ("a\nb\nc\nd\ne\nf\n", "a\nB\nc\nd\nE\nf\nG\n"),
            ("a\nb\nc\n", "x\nb\ny\nz\n"),
        ];
        for (base, text) in cases {
            let mut buffer = crate::code_editor::Buffer::new(text, crate::code_editor::metrics_for_zoom(1.0));
            for _ in 0..10 {
                let Some(hunk) = changes(base.as_bytes(), &buffer.text()).1.into_iter().next() else { break };
                let before = buffer.text();
                let undo = buffer.undo_count();
                assert!(restore(&mut buffer, &hunk), "{base:?} / {text:?}: {hunk:?}");
                assert_eq!(buffer.undo_count(), undo + 1);
                let after = buffer.text();
                buffer.undo();
                assert_eq!(buffer.text(), before, "undo brings the change back");
                buffer.redo();
                assert_eq!(buffer.text(), after);
            }
            assert_eq!(normalize(&buffer.text()), normalize(base), "{base:?} / {text:?}");
            assert!(changes(base.as_bytes(), &buffer.text()).1.is_empty());
        }
    }

    #[test]
    fn a_change_edited_since_it_was_computed_is_not_restored() {
        let hunk = changes(b"one\ntwo\nthree\n", "one\nTWO\nthree\n").1.remove(0);
        let mut buffer = crate::code_editor::Buffer::new("one\nTWO!\nthree\n", crate::code_editor::metrics_for_zoom(1.0));
        assert!(!restore(&mut buffer, &hunk));
        assert_eq!(buffer.text(), "one\nTWO!\nthree\n");
        assert!(!buffer.can_undo());
        let mut short = crate::code_editor::Buffer::new("one", crate::code_editor::metrics_for_zoom(1.0));
        assert!(!restore(&mut short, &hunk));
        assert_eq!(short.text(), "one");
    }

    #[test]
    fn line_endings_and_binary_files_do_not_produce_markers() {
        assert!(line_changes(b"a\r\nb\r\n", "a\nb").is_empty());
        assert!(line_changes(b"a\0b", "changed").is_empty());
    }
}
