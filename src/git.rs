//! Source control sidebar: branch, changed files, staging and commits.
//!
//! Repository changes (stage, unstage, commit) run the real `git` through [`cli`], so hooks,
//! signing and filters apply. Status is one `git status` per settled file-system change while
//! the panel is visible ([`watch`]); file versions for diffs are read in-process ([`repo`]).

pub mod cli;
pub mod repo;
pub mod status;
mod watch;

use iced::widget::{button, column, container, row, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli::Access;
pub use status::ChangedFile;
pub use watch::Changes;

#[derive(Default)]
pub struct GitState {
    status: status::Status,
    commit_message: String,
    committing: bool,
    error: Option<String>,
    refreshing: bool,
    refresh_again: bool,
    /// The project root the repository was looked up for; `None` until the first lookup.
    project: Option<Option<PathBuf>>,
    repo: Option<Arc<repo::Repo>>,
    watch: Option<watch::Watch>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    OpenDiff(String),
    Stage(ChangedFile, bool),
    Staged(Result<(), String>),
    Refreshed(Result<status::Status, String>),
    CommitMessageChanged(String),
    Commit,
    Committed(Result<(), String>),
}

impl GitState {
    /// Follows the open project: finds its repository and starts watching it. Returns whether
    /// the project changed, so views derived from the previous repository can be reloaded.
    pub fn set_project(&mut self, root: Option<&Path>) -> bool {
        let root = root.map(Path::to_path_buf);
        if self.project.as_ref() == Some(&root) { return false; }
        self.project = Some(root.clone());
        let root = root.as_deref();
        self.repo = root.and_then(repo::Repo::discover).map(Arc::new);
        self.watch = self.repo.clone().map(watch::Watch::start);
        self.status = status::Status::default();
        self.error = None;
        true
    }

    pub fn repo(&self) -> Option<Arc<repo::Repo>> { self.repo.clone() }

    /// File-system changes that affect git, reported once they have settled.
    pub fn take_changes(&mut self) -> Option<Changes> {
        self.watch.as_ref()?.take_settled()
    }
}

pub fn update(state: &mut GitState, message: Message, cwd: PathBuf) -> Task<Message> {
    match message {
        Message::OpenDiff(_) => {},
        Message::Stage(file, stage) => {
            if state.committing { return Task::none(); }
            state.committing = true;
            let unborn = state.status.branch.unborn();
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || stage_file(&cwd, &file, stage, unborn))
                    .await.map_err(|err| err.to_string())?
            }, Message::Staged);
        }
        Message::Staged(result) => {
            state.committing = false;
            match result {
                Ok(()) => return refresh(state, cwd),
                Err(err) => state.error = Some(err),
            }
        }
        Message::Refresh => return refresh(state, cwd),
        Message::Refreshed(result) => {
            state.refreshing = false;
            match result {
                Ok(status) => {
                    state.status = status;
                    state.error = None;
                }
                Err(err) => state.error = Some(err),
            }
            if std::mem::take(&mut state.refresh_again) { return refresh(state, cwd); }
        }
        Message::CommitMessageChanged(text) => state.commit_message = text,
        Message::Commit => {
            if state.commit_message.trim().is_empty() || state.committing || !state.status.files.iter().any(ChangedFile::staged) {
                return Task::none();
            }
            state.committing = true;
            let message = state.commit_message.clone();
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || commit(&cwd, &message))
                    .await.map_err(|err| err.to_string())?
            }, Message::Committed);
        }
        Message::Committed(result) => {
            state.committing = false;
            match result {
                Ok(()) => {
                    state.commit_message.clear();
                    return refresh(state, cwd);
                }
                Err(err) => state.error = Some(err),
            }
        }
    }
    Task::none()
}

/// Loads status unless a load is already running, in which case one more follows it.
fn refresh(state: &mut GitState, cwd: PathBuf) -> Task<Message> {
    if state.refreshing {
        state.refresh_again = true;
        return Task::none();
    }
    state.refreshing = true;
    Task::perform(async move {
        tokio::task::spawn_blocking(move || status::load(&cwd)).await.map_err(|err| err.to_string())?
    }, Message::Refreshed)
}

pub fn view<'a>(state: &'a GitState, selected: Option<&str>) -> Element<'a, Message> {
    let header = row![
        text("SOURCE CONTROL").size(12),
        Space::new().width(Length::Fill),
        crate::icon_control(lucide_icons::Icon::RefreshCw, "Refresh source control", Some(Message::Refresh), false),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let branch_icon: char = lucide_icons::Icon::GitBranch.into();
    let label = state.status.branch.label();
    let mut branch = row![
        text(branch_icon).font(iced::Font::with_name("lucide")).size(14),
        text(if label.is_empty() { "No branch".to_string() } else { label }).size(13),
    ].spacing(8).align_y(iced::Alignment::Center);
    let (ahead, behind) = (state.status.branch.ahead, state.status.branch.behind);
    if ahead > 0 || behind > 0 {
        branch = branch.push(text(format!("↑{ahead} ↓{behind}")).size(12).style(iced::widget::text::secondary));
    }
    let mut files_col = column![].spacing(4);
    if state.status.files.is_empty() && state.error.is_none() {
        files_col = files_col.push(container(text("Working tree clean").size(13)).padding([16, 0]));
    }
    for (title, staged) in [("Staged changes", true), ("Unstaged changes", false)] {
    let files: Vec<_> = state.status.files.iter().filter(|file| if staged { file.staged() } else { file.unstaged() }).collect();
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
        let action = if file.conflicted() && !staged { "Mark resolved" } else if staged { "Unstage" } else { "Stage" };
        files_col = files_col.push(row![file_button,
            button(text(action).size(11))
                .style(crate::flat_button_style)
                .on_press_maybe((!state.committing).then(|| Message::Stage(file.clone(), !staged))),
        ].spacing(4).align_y(iced::Alignment::Center));
    }
    }
    let files_list = scrollable(files_col).height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(err) = &state.error {
        bottom = bottom.push(scrollable(text(err.clone()).size(12).font(iced::Font::MONOSPACE).style(iced::widget::text::danger)).height(Length::Shrink));
    }
    let can_commit = !state.committing && !state.commit_message.trim().is_empty() && state.status.files.iter().any(ChangedFile::staged);
    bottom = bottom.push(
        text_input("Describe your changes", &state.commit_message)
            .on_input(Message::CommitMessageChanged)
            .on_submit(Message::Commit),
    );
    bottom = bottom.push(
        button(text(if state.committing { "Working..." } else { "Commit Staged" }).size(13))
            .width(Length::Fill)
            .padding([8, 12])
            .on_press_maybe(can_commit.then_some(Message::Commit)),
    );
    let changes = row![
        text("Changes").size(13),
        Space::new().width(Length::Fill),
        text(state.status.files.len().to_string()).size(12).style(iced::widget::text::secondary),
    ];

    container(column![header, branch, bottom, iced::widget::rule::horizontal(1), changes, files_list].spacing(8))
        .padding(8)
        .height(Length::Fill)
        .into()
}

/// Commits the index with `git commit`, so commit hooks and signing run as configured.
fn commit(cwd: &Path, message: &str) -> Result<(), String> {
    cli::run(cwd, &["commit", "-F", "-"], Some(message.as_bytes()), Access::Write).map(|_| ())
}

fn stage_file(cwd: &Path, file: &ChangedFile, stage: bool, unborn: bool) -> Result<(), String> {
    // Literal pathspecs also support filenames containing Git wildcard characters.
    let mut args = if stage {
        vec!["--literal-pathspecs", "add", "-A", "--"]
    } else if unborn {
        // Nothing to restore from before the first commit; drop the paths from the index.
        vec!["--literal-pathspecs", "rm", "--cached", "-r", "-q", "--"]
    } else {
        vec!["--literal-pathspecs", "restore", "--staged", "--"]
    };
    args.push(&file.path);
    if let Some(original) = &file.original { args.push(original); }
    cli::run(cwd, &args, None, Access::Write).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(cli::run(root, args, None, Access::Write).unwrap()).unwrap()
    }

    fn file(path: &str, status: &str) -> ChangedFile {
        ChangedFile { path: path.into(), status: status.into(), original: None }
    }

    #[test]
    fn stage_and_unstage_preserve_worktree_and_other_files() {
        let root = std::env::temp_dir().join(format!("editor-staging-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init"]);
        std::fs::write(root.join("a [1].txt"), "initial\n").unwrap();
        std::fs::write(root.join("b.txt"), "other\n").unwrap();
        let file = file("a [1].txt", "??");
        stage_file(&root, &file, true, true).unwrap();
        assert_eq!(git(&root, &["diff", "--cached", "--name-only"]).trim(), "a [1].txt");
        stage_file(&root, &file, false, true).unwrap();
        assert!(git(&root, &["ls-files"]).is_empty());
        assert_eq!(std::fs::read_to_string(root.join(&file.path)).unwrap(), "initial\n");
        stage_file(&root, &file, true, true).unwrap();
        git(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "Initial"]);
        std::fs::write(root.join(&file.path), "updated\n").unwrap();
        stage_file(&root, &file, true, false).unwrap();
        stage_file(&root, &file, false, false).unwrap();
        assert!(git(&root, &["diff", "--cached"]).is_empty());
        assert_eq!(std::fs::read_to_string(root.join(&file.path)).unwrap(), "updated\n");
        std::fs::remove_file(root.join(&file.path)).unwrap();
        stage_file(&root, &file, true, false).unwrap();
        assert!(git(&root, &["diff", "--cached", "--summary"]).contains("delete mode"));
        stage_file(&root, &file, false, false).unwrap();
        assert!(!root.join(&file.path).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_runs_hooks_and_reports_their_output() {
        let root = std::env::temp_dir().join(format!("editor-commit-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init"]);
        git(&root, &["config", "user.name", "Test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        stage_file(&root, &file("a.txt", "??"), true, true).unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        let write_hook = |script: &str| {
            std::fs::write(&hook, script).unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        };
        write_hook("#!/bin/sh\necho 'lint failed: a.txt' >&2\nexit 1\n");
        let err = commit(&root, "Blocked").unwrap_err();
        assert!(err.contains("lint failed: a.txt"), "{err}");
        assert_eq!(git(&root, &["diff", "--cached", "--name-only"]).trim(), "a.txt");
        write_hook("#!/bin/sh\nexit 0\n");
        commit(&root, "Subject line\n\nBody with details.").unwrap();
        assert_eq!(git(&root, &["log", "-1", "--format=%B"]).trim(), "Subject line\n\nBody with details.");
        let status = status::load(&root).unwrap();
        assert_eq!(status.branch.label(), git(&root, &["branch", "--show-current"]).trim());
        assert!(status.files.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
