//! Read-only, on-disk Git changes. Keep index and working-tree patches separate.
use iced::widget::{column, container, responsive, rich_text, row, scrollable, span, text};
use iced::{Element, Length};
use std::path::{Path, PathBuf};

pub struct Preview {
    pub root: PathBuf,
    pub path: String,
    pub result: Option<Result<String, String>>,
}

pub async fn load(root: PathBuf, path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || load_sync(&root, &path))
        .await.map_err(|err| err.to_string())?
}

fn command(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .current_dir(root).args(args).output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn load_sync(root: &Path, path: &str) -> Result<String, String> {
    // Literal pathspecs also support filenames containing Git wildcard characters.
    let staged = command(root, &["--literal-pathspecs", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--cached", "--", path])?;
    let unstaged = command(root, &["--literal-pathspecs", "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--", path])?;
    let mut patch = String::new();
    if !staged.is_empty() { patch.push_str(&format!("Staged changes\n{staged}\n")); }
    if !unstaged.is_empty() { patch.push_str(&format!("Unstaged changes\n{unstaged}")); }
    if patch.is_empty() {
        let untracked = command(root, &["--literal-pathspecs", "ls-files", "--others", "--exclude-standard", "--", path])?;
        if !untracked.is_empty() {
            let bytes = std::fs::read(root.join(path)).map_err(|err| err.to_string())?;
            patch.push_str("New file (untracked)\n");
            if bytes.contains(&0) {
                patch.push_str("Binary file — no text preview available\n");
            } else {
                let content = String::from_utf8_lossy(&bytes);
                patch.push_str(&format!("@@ -0,0 +1,{} @@\n", content.lines().count()));
                for line in content.lines() { patch.push('+'); patch.push_str(line); patch.push('\n'); }
            }
        } else {
            patch.push_str("No changes on disk for this file.");
        }
    }
    Ok(patch)
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

    #[test]
    fn previews_staged_unstaged_new_and_deleted_files() {
        let root = std::env::temp_dir().join(format!("editor-preview-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        command(&root, &["init"]).unwrap();
        std::fs::write(root.join("file.txt"), "original\n").unwrap();
        command(&root, &["add", "."]).unwrap();
        command(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "initial"]).unwrap();
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        command(&root, &["add", "."]).unwrap();
        std::fs::write(root.join("file.txt"), "unstaged\n").unwrap();
        let patch = load_sync(&root, "file.txt").unwrap();
        assert!(patch.contains("Staged changes\n") && patch.contains("Unstaged changes\n"));
        assert!(patch.contains("+staged") && patch.contains("+unstaged") && patch.contains("-original"));
        std::fs::write(root.join("new [1].txt"), "new\n").unwrap();
        assert!(load_sync(&root, "new [1].txt").unwrap().contains("+new"));
        std::fs::remove_file(root.join("file.txt")).unwrap();
        assert!(load_sync(&root, "file.txt").unwrap().contains("-staged"));
        std::fs::write(root.join("binary"), [0, 1, 2]).unwrap();
        assert!(load_sync(&root, "binary").unwrap().contains("Binary file"));
        command(&root, &["reset", "--hard", "HEAD"]).unwrap();
        command(&root, &["mv", "file.txt", "renamed file.txt"]).unwrap();
        assert!(load_sync(&root, "renamed file.txt").unwrap().contains("Staged changes"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
