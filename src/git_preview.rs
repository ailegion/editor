//! Read-only, on-disk Git changes. Keep index and working-tree patches separate.
use iced::widget::{column, container, responsive, row, scrollable, text};
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
    let (mut old_line, mut new_line) = (0, 0);
    let mut in_hunk = false;
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
            rows.push(DiffRow::Header(line.to_owned()));
        } else if in_hunk && line.starts_with('-') {
            removed.push((old_line, line[1..].to_owned()));
            old_line += 1;
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
        } else if line.starts_with("\\ No newline") {
            // This annotation does not consume a source line or break a replacement pair.
        } else {
            flush(&mut rows, &mut removed, &mut added);
            in_hunk = false;
            if !line.is_empty() { rows.push(DiffRow::Header(line.to_owned())); }
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

fn cell<Message: 'static>(line: &Option<(usize, String)>, kind: u8, width: f32) -> Element<'static, Message> {
    let (number, content) = line.as_ref()
        .map(|(number, content)| (number.to_string(), content.replace('\t', "    ")))
        .unwrap_or_default();
    container(row![
        text(number).font(iced::Font::MONOSPACE).size(12).width(52),
        text(content).font(iced::Font::MONOSPACE).size(13)
            .wrapping(iced::widget::text::Wrapping::None),
    ].spacing(8))
    .padding([2, 8]).width(width).height(22).clip(true)
    .style(move |theme: &iced::Theme| {
        let palette = theme.extended_palette();
        let pair = match kind {
            1 => palette.success.weak,
            2 => palette.danger.weak,
            _ => palette.background.base,
        };
        iced::widget::container::Style {
            background: Some(pair.color.into()), text_color: Some(pair.text),
            ..Default::default()
        }
    }).into()
}

pub fn view<Message: 'static>(preview: &Preview) -> Element<'_, Message> {
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
        let mut left = column![container(text("Previous • removed").size(13)).padding(8).height(34)];
        let mut right = column![container(text("Updated • added").size(13)).padding(8).height(34)];
        for entry in &rows {
            match entry {
                DiffRow::Header(label) => {
                    let header = || container(text(label.clone()).size(12)
                        .wrapping(iced::widget::text::Wrapping::None))
                        .padding([6, 8]).width(content_width).height(30).clip(true)
                        .style(iced::widget::container::rounded_box);
                    left = left.push(header());
                    right = right.push(header());
                }
                DiffRow::Lines { old, new, changed } => {
                    left = left.push(cell(old, if *changed && old.is_some() { 2 } else { 0 }, content_width));
                    right = right.push(cell(new, if *changed && new.is_some() { 1 } else { 0 }, content_width));
                }
            }
        }
        // One vertical scroll keeps corresponding rows aligned. Each half can
        // scroll long lines horizontally without pushing the other offscreen.
        let halves = row![
            scrollable(left.width(content_width))
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default()))
                .width(viewport_half),
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
