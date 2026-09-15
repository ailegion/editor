//! Read-only, on-disk Git changes. Keep index and working-tree patches separate.
use iced::widget::{column, container, scrollable, text};
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

pub fn view<Message: 'static>(preview: &Preview) -> Element<'_, Message> {
    let patch = match &preview.result {
        None => return container(text("Loading diff…")).padding(16).into(),
        Some(Err(err)) => return container(text(err).style(iced::widget::text::danger)).padding(16).into(),
        Some(Ok(patch)) => patch,
    };
    // Horizontal scrolling gives the content unbounded width. Keep rows sized to
    // their text: Fill here produces infinite bounds and the renderer clips them.
    let mut lines = column![].spacing(0).width(Length::Shrink);
    for line in patch.lines() {
        let kind = if line.starts_with('+') && !line.starts_with("+++") { 1 }
            else if line.starts_with('-') && !line.starts_with("---") { 2 }
            else if line.starts_with("@@") { 3 } else { 0 };
        lines = lines.push(container(text(line).font(iced::Font::MONOSPACE).size(13)
            .wrapping(iced::widget::text::Wrapping::None))
            .padding([2, 12]).width(Length::Shrink)
            .style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let pair = match kind {
                    1 => palette.success.weak,
                    2 => palette.danger.weak,
                    3 => palette.primary.weak,
                    _ => palette.background.base,
                };
                iced::widget::container::Style {
                    background: Some(pair.color.into()), text_color: Some(pair.text),
                    ..Default::default()
                }
            }));
    }
    scrollable(lines).direction(scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::default(), horizontal: scrollable::Scrollbar::default(),
    }).width(Length::Fill).height(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::*;
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
