//! Minimal git integration for the sidebar: current branch, working-tree file status, and a
//! one-button "stage everything and commit". Shells out to the `git` CLI via
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
    Refreshed(Result<(String, Vec<ChangedFile>), String>),
    CommitMessageChanged(String),
    Commit,
    Committed(Result<(), String>),
}

pub fn update(state: &mut GitState, message: Message, cwd: PathBuf) -> Task<Message> {
    match message {
        Message::OpenDiff(_) => {},
        Message::Refresh => return Task::perform(refresh(cwd), Message::Refreshed),
        Message::Refreshed(Ok((branch, files))) => {
            state.branch = branch;
            state.files = files;
            state.error = None;
        }
        Message::Refreshed(Err(err)) => state.error = Some(err),
        Message::CommitMessageChanged(text) => state.commit_message = text,
        Message::Commit => {
            if state.commit_message.trim().is_empty() || state.committing || state.files.is_empty() {
                return Task::none();
            }
            state.committing = true;
            return Task::perform(commit_all(cwd, state.commit_message.clone()), Message::Committed);
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

pub fn view(state: &GitState) -> Element<'_, Message> {
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
    for file in &state.files {
        let path = Path::new(&file.path);
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(&file.path);
        let parent = path.parent().and_then(|path| path.to_str()).unwrap_or("");
        let mut label = column![text(name).size(13)].spacing(2);
        if !parent.is_empty() {
            label = label.push(text(parent).size(11).style(iced::widget::text::secondary));
        }
        files_col = files_col.push(
            button(row![
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
            .style(crate::flat_button_style)
            .on_press(Message::OpenDiff(file.path.clone())),
        );
    }
    let files_list = scrollable(files_col).height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(err) = &state.error {
        bottom = bottom.push(text(err.clone()).size(12).style(iced::widget::text::danger));
    }
    let can_commit = !state.committing && !state.commit_message.trim().is_empty() && !state.files.is_empty();
    bottom = bottom.push(
        text_input("Describe your changes", &state.commit_message)
            .on_input(Message::CommitMessageChanged)
            .on_submit(Message::Commit),
    );
    bottom = bottom.push(
        button(text(if state.committing { "Committing..." } else { "Stage All & Commit" }).size(13))
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
            files.push(ChangedFile { status: code.to_owned(), path: entry[3..].to_owned() });
            // In -z mode a rename/copy includes the original path as a second entry.
            if code.contains('R') || code.contains('C') { entries.next(); }
        }
        Ok((branch, files))
    })
    .await
    .map_err(|err| err.to_string())?
}

async fn commit_all(cwd: PathBuf, message: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        run_git(&cwd, &["add", "-A"])?;
        run_git(&cwd, &["commit", "-m", &message])?;
        Ok(())
    })
    .await
    .map_err(|err| err.to_string())?
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
