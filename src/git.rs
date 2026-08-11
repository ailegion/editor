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
    Refreshed(Result<(String, Vec<ChangedFile>), String>),
    CommitMessageChanged(String),
    Commit,
    Committed(Result<(), String>),
}

pub fn update(state: &mut GitState, message: Message, cwd: PathBuf) -> Task<Message> {
    match message {
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
        text(if state.branch.is_empty() { "(no branch)".to_string() } else { state.branch.clone() }),
        Space::new().width(Length::Fill),
        button(text(refresh_icon).font(iced::Font::with_name("lucide")).size(14))
            .padding([4, 8])
            .style(crate::flat_button_style)
            .on_press(Message::Refresh),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let mut files_col = column![].spacing(2);
    if state.files.is_empty() {
        files_col = files_col.push(text("No changes").size(12));
    }
    for file in &state.files {
        files_col = files_col.push(
            row![
                text(file.status.clone()).size(12).width(Length::Fixed(20.0)),
                text(file.path.clone()).size(13),
            ]
            .spacing(4),
        );
    }
    let files_list = scrollable(files_col).height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(err) = &state.error {
        bottom = bottom.push(text(err.clone()).size(12));
    }
    let can_commit = !state.committing && !state.commit_message.trim().is_empty() && !state.files.is_empty();
    bottom = bottom.push(
        text_input("Commit message", &state.commit_message)
            .on_input(Message::CommitMessageChanged)
            .on_submit(Message::Commit),
    );
    bottom = bottom.push(row![
        Space::new().width(Length::Fill),
        button(text(if state.committing { "Committing..." } else { "Commit" }))
            .on_press_maybe(can_commit.then_some(Message::Commit)),
    ]);

    container(column![header, files_list, bottom].spacing(8))
        .height(Length::Fill)
        .into()
}

async fn refresh(cwd: PathBuf) -> Result<(String, Vec<ChangedFile>), String> {
    tokio::task::spawn_blocking(move || {
        let branch = run_git(&cwd, &["branch", "--show-current"])?.trim().to_string();
        let status = run_git(&cwd, &["status", "--porcelain"])?;
        let files = status
            .lines()
            .filter(|line| line.len() > 3)
            .map(|line| ChangedFile {
                status: line[..2].to_string(),
                path: line[3..].to_string(),
            })
            .collect();
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
