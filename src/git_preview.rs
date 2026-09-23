//! Read-only, on-disk Git changes. Keep index and working-tree patches separate.
//! Versions are read and diffed in-process; see [`crate::git::repo`].
use iced::widget::{column, container, responsive, rich_text, row, scrollable, span, text};
use iced::{Element, Length};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};

use crate::git::repo::{Repo, Worktree};

pub struct Preview {
    pub root: PathBuf,
    pub path: String,
    pub result: Option<Result<String, String>>,
}

pub async fn load(repo: Option<Arc<Repo>>, root: PathBuf, path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || match repo {
        Some(repo) => load_sync(&repo, &root, &path),
        None => Err("This folder is not inside a Git repository.".into()),
    })
    .await.map_err(|err| err.to_string())?
}

/// `path` is relative to the project `root`, as reported by status.
fn load_sync(repo: &Repo, root: &Path, path: &str) -> Result<String, String> {
    let file = Path::new(path);
    if repo.is_conflicted(file)? {
        return Ok("Unmerged file\nResolve the conflict markers in the editor, then stage the file to mark it resolved.".into());
    }
    let head = repo.head_version(file)?;
    let index = repo.index_version(file)?;
    let mut patch = String::new();
    section(&mut patch, "Staged changes", head.as_deref(), index.as_deref());
    match repo.worktree_version(file)? {
        Worktree::NeedsDriver => {
            // Only git can run filter drivers such as Git LFS, so let it produce this patch.
            let args = ["--literal-pathspecs", "diff", "--no-ext-diff", "--no-color", "--", path];
            let unstaged = crate::git::cli::run(root, &args, None, crate::git::cli::Access::Read)?;
            if !unstaged.is_empty() { let _ = write!(patch, "Unstaged changes\n{}", String::from_utf8_lossy(&unstaged)); }
        }
        Worktree::Content(content) if index.is_none() => {
            patch.push_str("New file (untracked)\n");
            push_diff(&mut patch, &[], &content);
        }
        Worktree::Content(content) => section(&mut patch, "Unstaged changes", index.as_deref(), Some(&content)),
        Worktree::Missing => section(&mut patch, "Unstaged changes", index.as_deref(), None),
    }
    if patch.is_empty() { patch.push_str("No changes on disk for this file."); }
    Ok(patch)
}

/// Appends a titled patch from `old` to `new`, where `None` means the file does not exist there.
fn section(patch: &mut String, title: &str, old: Option<&[u8]>, new: Option<&[u8]>) {
    if old == new { return; }
    patch.push_str(title);
    patch.push('\n');
    match (old, new) {
        (None, _) => patch.push_str("New file\n"),
        (_, None) => patch.push_str("Deleted file\n"),
        _ => {}
    }
    push_diff(patch, old.unwrap_or_default(), new.unwrap_or_default());
}

fn push_diff(patch: &mut String, old: &[u8], new: &[u8]) {
    let is_binary = |data: &[u8]| data[..data.len().min(8000)].contains(&0);
    if is_binary(old) || is_binary(new) {
        patch.push_str("Binary file — no text preview available\n");
        return;
    }
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);
    if let Ok(text) = UnifiedDiff::new(&diff, &input, Hunks(String::new()), ContextSize::symmetrical(3)).consume() {
        patch.push_str(&text);
    }
}

/// Renders hunks as unified diff text, marking lines that lack a final newline like git does.
struct Hunks(String);

impl ConsumeHunk for Hunks {
    type Out = String;

    fn consume_hunk(&mut self, header: HunkHeader, lines: &[(DiffLineKind, &[u8])]) -> std::io::Result<()> {
        // git's format: a length of one is implied, `-3` rather than `-3,1`.
        let range = |start: u32, len: u32| if len == 1 { start.to_string() } else { format!("{start},{len}") };
        let _ = writeln!(self.0, "@@ -{} +{} @@", range(header.before_hunk_start, header.before_hunk_len), range(header.after_hunk_start, header.after_hunk_len));
        for (kind, line) in lines {
            let line = String::from_utf8_lossy(line);
            let (content, terminated) = match line.strip_suffix('\n') {
                Some(content) => (content, true),
                None => (&*line, false),
            };
            self.0.push(kind.to_prefix());
            self.0.push_str(content.strip_suffix('\r').unwrap_or(content));
            self.0.push('\n');
            if !terminated { self.0.push_str("\\ No newline at end of file\n"); }
        }
        Ok(())
    }

    fn finish(self) -> String { self.0 }
}

#[derive(Debug, PartialEq)]
enum DiffRow {
    Header(String),
    Lines { old: Option<(usize, String)>, new: Option<(usize, String)>, changed: bool },
}

fn split_diff(patch: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut old_line, mut new_line) = (0usize, 0usize);
    let mut in_hunk = false;
    let mut previous_end = 1usize;
    let mut removed = Vec::new();
    let mut added = Vec::new();
    fn flush(rows: &mut Vec<DiffRow>, removed: &mut Vec<(usize, String)>, added: &mut Vec<(usize, String)>) {
        let mut old = std::mem::take(removed).into_iter();
        let mut new = std::mem::take(added).into_iter();
        loop {
            let (old, new) = (old.next(), new.next());
            if old.is_none() && new.is_none() { break; }
            rows.push(DiffRow::Lines { old, new, changed: true });
        }
    }
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("@@ -") {
            flush(&mut rows, &mut removed, &mut added);
            if let Some((old, rest)) = header.split_once(" +") {
                old_line = old.split(',').next().unwrap_or("0").parse().unwrap_or(0);
                new_line = rest.split([',', ' ']).next().unwrap_or("0").parse().unwrap_or(0);
                in_hunk = true;
            }
            let skipped = old_line.saturating_sub(previous_end);
            rows.push(DiffRow::Header(if skipped > 0 {
                format!("⋯ {skipped} unchanged lines")
            } else { "Changes".to_owned() }));
        } else if in_hunk && line.starts_with('-') {
            removed.push((old_line, line[1..].to_owned()));
            old_line += 1;
            previous_end = old_line;
        } else if in_hunk && line.starts_with('+') {
            added.push((new_line, line[1..].to_owned()));
            new_line += 1;
        } else if in_hunk && line.starts_with(' ') {
            flush(&mut rows, &mut removed, &mut added);
            rows.push(DiffRow::Lines {
                old: Some((old_line, line[1..].to_owned())),
                new: Some((new_line, line[1..].to_owned())), changed: false,
            });
            old_line += 1;
            new_line += 1;
            previous_end = old_line;
        } else if line.starts_with("\\ No newline") {
            // This annotation does not consume a source line or break a replacement pair.
        } else {
            flush(&mut rows, &mut removed, &mut added);
            in_hunk = false;
            if line.starts_with("diff --git") {
                previous_end = 1;
            }
            if !line.is_empty() && !["diff --git", "index ", "--- ", "+++ "].iter().any(|prefix| line.starts_with(prefix)) {
                rows.push(DiffRow::Header(line.to_owned()));
            }
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

pub fn summary(preview: &Preview) -> String {
    let Some(Ok(patch)) = &preview.result else { return String::new(); };
    let (mut added, mut removed) = (0, 0);
    for row in split_diff(patch) {
        if let DiffRow::Lines { old, new, changed: true } = row {
            added += usize::from(new.is_some());
            removed += usize::from(old.is_some());
        }
    }
    format!("+{added}  −{removed}")
}

/// Highlight the smallest differing middle after removing shared prefix/suffix.
fn changed_range(content: &str, other: &str) -> std::ops::Range<usize> {
    let prefix = content.chars().zip(other.chars()).take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8()).sum::<usize>();
    let suffix = content[prefix..].chars().rev().zip(other[prefix..].chars().rev())
        .take_while(|(a, b)| a == b).map(|(ch, _)| ch.len_utf8()).sum::<usize>();
    prefix..content.len() - suffix
}

fn cell<Message: 'static>(line: &Option<(usize, String)>, other: &Option<(usize, String)>, kind: u8, width: f32, theme: &iced::Theme) -> Element<'static, Message> {
    let (number, content) = line.as_ref()
        .map(|(number, content)| (number.to_string(), content.replace('\t', "    ")))
        .unwrap_or_default();
    let range = if kind == 0 { 0..0 } else {
        other.as_ref().map(|(_, other)| changed_range(&content, &other.replace('\t', "    "))).unwrap_or(0..content.len())
    };
    let palette = theme.extended_palette();
    let accent = if kind == 1 { palette.success.base.color } else { palette.danger.base.color };
    let highlight = iced::Color { a: 0.35, ..accent };
    let code: iced::widget::text::Rich<'_, (), Message> = rich_text(vec![
        span(content[..range.start].to_owned()),
        span(content[range.clone()].to_owned()).background(highlight),
        span(content[range.end..].to_owned()),
    ]).font(iced::Font::MONOSPACE).size(13).wrapping(iced::widget::text::Wrapping::None);
    container(row![
        container(text(number).font(iced::Font::MONOSPACE).size(12)
            .style(iced::widget::text::secondary)).width(42),
        code,
    ].spacing(8))
    .padding([2, 8]).width(width).height(22).clip(true)
    .style(move |theme: &iced::Theme| {
        let palette = theme.extended_palette();
        let base = palette.background.base;
        let accent = match kind {
            1 => palette.success.base.color,
            2 => palette.danger.base.color,
            _ => base.color,
        };
        let tint = iced::Color {
            r: base.color.r * 0.88 + accent.r * 0.12,
            g: base.color.g * 0.88 + accent.g * 0.12,
            b: base.color.b * 0.88 + accent.b * 0.12,
            a: 1.0,
        };
        iced::widget::container::Style {
            background: Some(tint.into()), text_color: Some(base.text),
            ..Default::default()
        }
    }).into()
}

pub fn view<'a, Message: 'static>(preview: &'a Preview, theme: &'a iced::Theme) -> Element<'a, Message> {
    let patch = match &preview.result {
        None => return container(text("Loading diff…")).padding(16).into(),
        Some(Err(err)) => return container(text(err).style(iced::widget::text::danger)).padding(16).into(),
        Some(Ok(patch)) => patch,
    };
    let rows = split_diff(patch);
    responsive(move |size| {
        // Both axes scroll together. Explicit finite widths avoid Fill inside the
        // horizontal scrollable's unbounded content layout.
        let longest = rows.iter().filter_map(|row| match row {
            DiffRow::Lines { old, new, .. } => Some(old.iter().chain(new.iter())
                .map(|(_, text)| text.replace('\t', "    ").chars().count()).max().unwrap_or(0)),
            _ => None,
        }).max().unwrap_or(0);
        let viewport_half = ((size.width - 16.0) / 2.0).max(80.0);
        let content_width = viewport_half.max(longest as f32 * 8.0 + 84.0);
        let mut left = column![container(text("Before").size(13)).padding(8).height(34)];
        let mut right = column![container(text("After").size(13)).padding(8).height(34)];
        for entry in &rows {
            match entry {
                DiffRow::Header(label) => {
                    let header = |label: String| container(text(label).size(12)
                        .wrapping(iced::widget::text::Wrapping::None))
                        .padding([6, 8]).width(content_width).height(30).clip(true)
                        .style(iced::widget::container::rounded_box);
                    left = left.push(header(label.clone()));
                    right = right.push(header(String::new()));
                }
                DiffRow::Lines { old, new, changed } => {
                    left = left.push(cell(old, new, if *changed && old.is_some() { 2 } else { 0 }, content_width, theme));
                    right = right.push(cell(new, old, if *changed && new.is_some() { 1 } else { 0 }, content_width, theme));
                }
            }
        }
        // One vertical scroll keeps corresponding rows aligned. Each half can
        // scroll long lines horizontally without pushing the other offscreen.
        let content_height = 34.0 + rows.iter().map(|entry| match entry {
            DiffRow::Header(_) => 30.0, DiffRow::Lines { .. } => 22.0,
        }).sum::<f32>();
        let halves = row![
            scrollable(left.width(content_width))
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default()))
                .width(viewport_half),
            container(iced::widget::rule::vertical(1)).height(content_height),
            scrollable(right.width(content_width))
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default()))
                .width(viewport_half),
        ];
        scrollable(halves).width(Length::Fill).height(Length::Fill).into()
    }).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn character_highlights_respect_unicode_and_insertions() {
        assert_eq!(changed_range("news_intelx", "news_intel"), 10..11);
        assert_eq!(changed_range("é猫x", "é犬x"), 2..5);
        assert_eq!(changed_range("same", "same"), 4..4);
        assert_eq!(changed_range("", "new"), 0..0);
    }

    #[test]
    fn hides_patch_metadata_and_labels_omitted_context() {
        let rows = split_diff("Unstaged changes\ndiff --git a/f b/f\nindex abc..def 100644\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n a\n@@ -5 +5 @@\n-b\n+c\n");
        let headers: Vec<_> = rows.iter().filter_map(|row| match row {
            DiffRow::Header(label) => Some(label.as_str()), _ => None,
        }).collect();
        assert_eq!(headers, ["Unstaged changes", "Changes", "⋯ 3 unchanged lines"]);
    }

    #[test]
    fn aligns_replacements_and_preserves_line_numbers() {
        let rows = split_diff("@@ -4,3 +4,4 @@\n same\n-old\n+new\n+extra\n end\n");
        assert_eq!(rows[2], DiffRow::Lines {
            old: Some((5, "old".into())), new: Some((5, "new".into())), changed: true,
        });
        assert_eq!(rows[3], DiffRow::Lines {
            old: None, new: Some((6, "extra".into())), changed: true,
        });
        assert_eq!(rows[4], DiffRow::Lines {
            old: Some((6, "end".into())), new: Some((7, "end".into())), changed: false,
        });
    }

    #[test]
    fn aligns_deletions_and_resets_between_hunks() {
        let rows = split_diff("@@ -1,2 +0,0 @@\n-a\n-b\n@@ -10 +8 @@\n-x\n\\ No newline at end of file\n+y\n");
        assert_eq!(rows[2], DiffRow::Lines {
            old: Some((2, "b".into())), new: None, changed: true,
        });
        assert_eq!(rows[4], DiffRow::Lines {
            old: Some((10, "x".into())), new: Some((8, "y".into())), changed: true,
        });
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = crate::git::cli::run(root, args, None, crate::git::cli::Access::Write).unwrap();
        String::from_utf8(output).unwrap()
    }

    /// The hunks git itself prints for the same comparison, for checking the in-process diff.
    fn git_hunks(root: &Path, args: &[&str]) -> String {
        git(root, args).lines().skip_while(|line| !line.starts_with("@@")).map(|line| {
            // git appends the enclosing function after the second `@@`; the preview omits it.
            match line.strip_prefix("@@ ").and_then(|rest| rest.split_once(" @@")) {
                Some((ranges, _)) => format!("@@ {ranges} @@\n"),
                None => format!("{line}\n"),
            }
        }).collect()
    }

    fn open(root: &Path) -> Repo { Repo::discover(root).unwrap() }

    #[test]
    fn previews_staged_unstaged_new_and_deleted_files() {
        let root = std::env::temp_dir().join(format!("editor-preview-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init"]);
        git(&root, &["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("file.txt"), "original\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "initial"]);
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        git(&root, &["add", "."]);
        std::fs::write(root.join("file.txt"), "unstaged\n").unwrap();
        let patch = load_sync(&open(&root), &root, "file.txt").unwrap();
        assert!(patch.contains("Staged changes\n") && patch.contains("Unstaged changes\n"));
        assert!(patch.contains("+staged") && patch.contains("+unstaged") && patch.contains("-original"));
        std::fs::write(root.join("new [1].txt"), "new\n").unwrap();
        assert!(load_sync(&open(&root), &root, "new [1].txt").unwrap().contains("+new"));
        std::fs::remove_file(root.join("file.txt")).unwrap();
        assert!(load_sync(&open(&root), &root, "file.txt").unwrap().contains("-staged"));
        std::fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        assert!(load_sync(&open(&root), &root, "binary").unwrap().contains("Binary file"));
        git(&root, &["reset", "--hard", "HEAD"]);
        git(&root, &["mv", "file.txt", "renamed file.txt"]);
        assert!(load_sync(&open(&root), &root, "renamed file.txt").unwrap().contains("Staged changes"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn in_process_hunks_match_git_diff() {
        let root = std::env::temp_dir().join(format!("editor-preview-parity-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        git(&root, &["init"]);
        git(&root, &["config", "core.autocrlf", "true"]);
        let original: String = (1..=40).map(|n| format!("line {n}\r\n")).collect();
        std::fs::write(root.join("sub/code.txt"), &original).unwrap();
        std::fs::write(root.join("tail.txt"), "a\nb\nc").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "initial"]);
        let edited = original.replace("line 3\r\n", "line three\r\n").replace("line 20\r\n", "").replace("line 38\r\n", "line 38\r\ninserted\r\n");
        std::fs::write(root.join("sub/code.txt"), edited).unwrap();
        std::fs::write(root.join("tail.txt"), "a\nB\nc\n").unwrap();
        for path in ["sub/code.txt", "tail.txt"] {
            let patch = load_sync(&open(&root), &root, path).unwrap();
            let ours = patch.strip_prefix("Unstaged changes\n").unwrap();
            assert_eq!(ours, git_hunks(&root, &["-c", "core.quotepath=false", "diff", "--no-color", "--", path]), "{path}");
        }
        // Opened from a subdirectory, paths are relative to that project root.
        let patch = load_sync(&open(&root.join("sub")), &root.join("sub"), "code.txt").unwrap();
        assert!(patch.contains("+line three"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
