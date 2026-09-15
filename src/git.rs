//! Minimal git integration for the sidebar: current branch, working-tree file status, and a
//! per-file staging and commits of the index. Shells out to the `git` CLI via
//! `std::process::Command` rather than a git library, the same approach `main.rs` already uses
//! for "reveal in file manager".

use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    /// Raw two-character `git status --porcelain` code, e.g. `"M "`, `"??"`, `"A "`.
    pub status: String,
    pub original: Option<String>,
}

impl ChangedFile {
    fn staged(&self) -> bool { self.status.as_bytes()[0] != b' ' && self.status != "??" }
    fn unstaged(&self) -> bool { self.status.as_bytes()[1] != b' ' }
}

#[derive(Default)]
pub struct GitState {
    branch: String,
    files: Vec<ChangedFile>,
    commit_message: String,
    committing: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    OpenDiff(String),
    Stage(ChangedFile, bool),
    Staged(Result<(), String>),
    Refreshed(Result<(String, Vec<ChangedFile>), String>),
    CommitMessageChanged(String),
    Commit,
    Committed(Result<(), String>),
}

pub fn update(state: &mut GitState, message: Message, cwd: PathBuf) -> Task<Message> {
    match message {
        Message::OpenDiff(_) => {},
        Message::Stage(file, stage) => {
            if state.committing { return Task::none(); }
            state.committing = true;
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || stage_file(&cwd, &file, stage))
                    .await.map_err(|err| err.to_string())?
            }, Message::Staged);
        }
        Message::Staged(result) => {
            state.committing = false;
            match result {
                Ok(()) => return Task::perform(refresh(cwd), Message::Refreshed),
                Err(err) => state.error = Some(err),
            }
        }
        Message::Refresh => return Task::perform(refresh(cwd), Message::Refreshed),
        Message::Refreshed(Ok((branch, files))) => {
            state.branch = branch;
            state.files = files;
            state.error = None;
        }
        Message::Refreshed(Err(err)) => state.error = Some(err),
        Message::CommitMessageChanged(text) => state.commit_message = text,
        Message::Commit => {
            if state.commit_message.trim().is_empty() || state.committing || !state.files.iter().any(ChangedFile::staged) {
                return Task::none();
            }
            state.committing = true;
            return Task::perform(commit_staged(cwd, state.commit_message.clone()), Message::Committed);
        }
        Message::Committed(result) => {
            state.committing = false;
            match result {
                Ok(()) => {
                    state.commit_message.clear();
                    return Task::perform(refresh(cwd), Message::Refreshed);
                }
                Err(err) => state.error = Some(err),
            }
        }
    }
    Task::none()
}

pub fn view<'a>(state: &'a GitState, selected: Option<&str>) -> Element<'a, Message> {
    let refresh_icon: char = lucide_icons::Icon::RefreshCw.into();
    let header = row![
        text("SOURCE CONTROL").size(12),
        Space::new().width(Length::Fill),
        button(text(refresh_icon).font(iced::Font::with_name("lucide")).size(14))
            .padding([4, 8])
            .style(crate::flat_button_style)
            .on_press(Message::Refresh),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let branch_icon: char = lucide_icons::Icon::GitBranch.into();
    let branch = row![
        text(branch_icon).font(iced::Font::with_name("lucide")).size(14),
        text(if state.branch.is_empty() { "No branch" } else { &state.branch }).size(13),
    ].spacing(8).align_y(iced::Alignment::Center);
    let mut files_col = column![].spacing(4);
    if state.files.is_empty() && state.error.is_none() {
        files_col = files_col.push(container(text("Working tree clean").size(13)).padding([16, 0]));
    }
    for (title, staged) in [("Staged changes", true), ("Unstaged changes", false)] {
    let files: Vec<_> = state.files.iter().filter(|file| if staged { file.staged() } else { file.unstaged() }).collect();
    files_col = files_col.push(text(format!("{title} ({})", files.len())).size(12));
    for file in files {
        let is_selected = selected == Some(file.path.as_str());
        let path = Path::new(&file.path);
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(&file.path);
        let parent = path.parent().and_then(|path| path.to_str()).unwrap_or("");
        let mut label = column![text(name).size(13)].spacing(2);
        if !parent.is_empty() {
            label = label.push(text(parent).size(11).style(iced::widget::text::secondary));
        }
        let file_button = button(row![
                label.width(Length::Fill),
                text(file.status.trim()).size(12).style(|theme: &iced::Theme| {
                    let palette = theme.extended_palette();
                    let color = if file.status.contains('U') || file.status.contains('D') {
                        palette.danger.base.color
                    } else if file.status.contains('A') || file.status == "??" {
                        palette.success.base.color
                    } else {
                        palette.primary.base.color
                    };
                    iced::widget::text::Style { color: Some(color) }
                }),
            ].spacing(8).align_y(iced::Alignment::Center))
            .padding([6, 8])
            .width(Length::Fill)
            .style(move |theme, status| {
                let mut style = crate::flat_button_style(theme, status);
                if is_selected {
                    style.background = Some(theme.extended_palette().primary.weak.color.into());
                    style.text_color = theme.extended_palette().primary.weak.text;
                }
                style
            })
            .on_press(Message::OpenDiff(file.path.clone()));
        files_col = files_col.push(row![file_button,
            button(text(if staged { "Unstage" } else { "Stage" }).size(11))
                .style(crate::flat_button_style)
                .on_press_maybe((!state.committing).then(|| Message::Stage(file.clone(), !staged))),
        ].spacing(4).align_y(iced::Alignment::Center));
    }
    }
    let files_list = scrollable(files_col).height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(err) = &state.error {
        bottom = bottom.push(text(err.clone()).size(12).style(iced::widget::text::danger));
    }
    let can_commit = !state.committing && !state.commit_message.trim().is_empty() && state.files.iter().any(ChangedFile::staged);
    bottom = bottom.push(
        text_input("Describe your changes", &state.commit_message)
            .on_input(Message::CommitMessageChanged)
            .on_submit(Message::Commit),
    );
    bottom = bottom.push(
        button(text(if state.committing { "Committing..." } else { "Commit Staged" }).size(13))
            .width(Length::Fill)
            .padding([8, 12])
            .on_press_maybe(can_commit.then_some(Message::Commit)),
    );
    let changes = row![
        text("Changes").size(13),
        Space::new().width(Length::Fill),
        text(state.files.len().to_string()).size(12).style(iced::widget::text::secondary),
    ];

    container(column![header, branch, bottom, iced::widget::rule::horizontal(1), changes, files_list].spacing(12))
        .padding(12)
        .height(Length::Fill)
        .into()
}

async fn refresh(cwd: PathBuf) -> Result<(String, Vec<ChangedFile>), String> {
    tokio::task::spawn_blocking(move || {
        let branch = run_git(&cwd, &["branch", "--show-current"])?.trim().to_string();
        let status = run_git(&cwd, &["status", "--porcelain", "-z", "--untracked-files=all"])?;
        let mut files = Vec::new();
        let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
        while let Some(entry) = entries.next() {
            if entry.len() < 4 { continue; }
            let code = &entry[..2];
            let original = if code.contains('R') || code.contains('C') { entries.next().map(str::to_owned) } else { None };
            files.push(ChangedFile { status: code.to_owned(), path: entry[3..].to_owned(), original });
        }
        Ok((branch, files))
    })
    .await
    .map_err(|err| err.to_string())?
}

async fn commit_staged(cwd: PathBuf, message: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        run_git(&cwd, &["commit", "-m", &message])?;
        Ok(())
    })
    .await
    .map_err(|err| err.to_string())?
}

fn stage_file(cwd: &Path, file: &ChangedFile, stage: bool) -> Result<(), String> {
    let has_head = run_git(cwd, &["rev-parse", "--verify", "HEAD"]).is_ok();
    let mut args = if stage {
        vec!["--literal-pathspecs", "add", "-A", "--"]
    } else if has_head {
        vec!["--literal-pathspecs", "reset", "HEAD", "--"]
    } else {
        vec!["--literal-pathspecs", "rm", "--cached", "-f", "--"]
    };
    args.push(&file.path);
    if let Some(original) = &file.original { args.push(original); }
    run_git(cwd, &args).map(|_| ())
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_and_unstage_preserve_worktree_and_other_files() {
        let root = std::env::temp_dir().join(format!("editor-staging-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init"]).unwrap();
        std::fs::write(root.join("a [1].txt"), "initial\n").unwrap();
        std::fs::write(root.join("b.txt"), "other\n").unwrap();
        let file = ChangedFile { path: "a [1].txt".into(), status: "??".into(), original: None };
        stage_file(&root, &file, true).unwrap();
        assert_eq!(run_git(&root, &["diff", "--cached", "--name-only"]).unwrap().trim(), "a [1].txt");
        stage_file(&root, &file, false).unwrap();
        assert!(run_git(&root, &["ls-files"]).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(root.join(&file.path)).unwrap(), "initial\n");
        stage_file(&root, &file, true).unwrap();
        run_git(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "Initial"]).unwrap();
        std::fs::write(root.join(&file.path), "updated\n").unwrap();
        stage_file(&root, &file, true).unwrap();
        stage_file(&root, &file, false).unwrap();
        assert!(run_git(&root, &["diff", "--cached"]).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(root.join(&file.path)).unwrap(), "updated\n");
        std::fs::remove_file(root.join(&file.path)).unwrap();
        stage_file(&root, &file, true).unwrap();
        assert!(run_git(&root, &["diff", "--cached", "--summary"]).unwrap().contains("delete mode"));
        stage_file(&root, &file, false).unwrap();
        assert!(!root.join(&file.path).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
