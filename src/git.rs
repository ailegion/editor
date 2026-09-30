//! Source control sidebar: branch, changed files, staging and commits.
//!
//! Repository changes (stage, unstage, commit) run the real `git` through [`cli`], so hooks,
//! signing and filters apply. Status is one `git status` per settled file-system change while
//! the panel is visible ([`watch`]); file versions for diffs are read in-process ([`repo`]).

pub mod cli;
pub mod blame;
pub mod branches;
pub mod remote;
pub mod repo;
pub mod status;
mod watch;

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_editor, Space};
use iced::{Element, Length, Task};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cli::Access;
pub use status::ChangedFile;
pub use watch::Changes;

#[derive(Default)]
pub struct GitState {
    status: status::Status,
    /// Multiline: subject, blank line, body. Cmd/Ctrl+Enter commits.
    commit_message: text_editor::Content,
    committing: bool,
    /// An AI-written commit message is on its way.
    generating: bool,
    error: Option<String>,
    refreshing: bool,
    refresh_again: bool,
    /// The project root the repository was looked up for; `None` until the first lookup.
    project: Option<Option<PathBuf>>,
    repo: Option<Arc<repo::Repo>>,
    watch: Option<watch::Watch>,
    /// Show changed files as a directory tree rather than a flat list. Persisted.
    tree_view: bool,
    /// Directories collapsed in tree view, by repository-relative path.
    collapsed: HashSet<String>,
    picker: BranchPicker,
    /// The ⋯ menu with fetch, pull and push.
    sync_menu: bool,
}

/// The branch picker opened from the branch name, like Zed's.
#[derive(Default)]
struct BranchPicker {
    open: bool,
    query: String,
    filter: branches::Filter,
    filter_menu: bool,
    branches: Vec<branches::Branch>,
    loading: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Discard(Vec<ChangedFile>),
    DiscardConfirmed(Vec<ChangedFile>),
    Discarded(Result<(), String>),
    OpenDiff(String),
    /// Stage (`true`) or unstage (`false`) these files in one git call.
    Stage(Vec<ChangedFile>, bool),
    Staged(Result<(), String>),
    Refreshed(Result<status::Status, String>),
    CommitMessageEdited(text_editor::Action),
    /// Ask the AI assistant to write the message from the staged changes (handled by the app,
    /// which answers with `MessageGenerated`).
    GenerateMessage,
    MessageGenerated(Result<String, String>),
    Commit,
    Committed(Result<(), String>),
    ToggleTreeView,
    ToggleDir(String),
    ToggleBranches,
    CloseBranches,
    BranchesLoaded(Result<Vec<branches::Branch>, String>),
    BranchQueryChanged(String),
    ToggleBranchFilter,
    BranchFilterChosen(branches::Filter),
    /// Enter in the search box: the first listed branch, else create the typed name.
    BranchSubmit,
    Branch(branches::Action),
    /// Checked out a branch; the app reloads open files and diffs.
    BranchDone(Result<(), String>),
    ToggleSyncMenu,
    CloseSyncMenu,
    Sync(remote::Sync),
    /// A pull may have changed files even when it failed (conflicts); the app reloads them.
    SyncDone(remote::Sync, Result<(), String>),
}

fn branch_query_id() -> iced::widget::Id {
    iced::widget::Id::new("git-branch-query")
}

impl GitState {
    pub fn new() -> Self {
        let tree_view = crate::config_path("git_tree_view")
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or(true, |text| text.trim() != "false");
        Self { tree_view, ..Self::default() }
    }

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
        self.picker = BranchPicker { filter: self.picker.filter, ..BranchPicker::default() };
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
        Message::OpenDiff(_) | Message::Discard(_) => {},
        Message::DiscardConfirmed(files) => {
            if state.committing || files.is_empty() { return Task::none(); }
            state.committing = true;
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || discard_files(&cwd, &files)).await.map_err(|e| e.to_string())?
            }, Message::Discarded);
        }
        Message::Discarded(result) => {
            state.committing = false;
            match result {
                Ok(()) => return refresh(state, cwd),
                Err(err) => state.error = Some(err),
            }
        }
        Message::Stage(files, stage) => {
            if state.committing || files.is_empty() { return Task::none(); }
            state.committing = true;
            let unborn = state.status.branch.unborn();
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || stage_files(&cwd, &files, stage, unborn))
                    .await.map_err(|err| err.to_string())?
            }, Message::Staged);
        }
        Message::ToggleTreeView => {
            state.tree_view = !state.tree_view;
            if let Some(path) = crate::config_path("git_tree_view") {
                if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
                let _ = std::fs::write(path, if state.tree_view { "true" } else { "false" });
            }
        }
        Message::ToggleDir(path) => {
            if !state.collapsed.remove(&path) { state.collapsed.insert(path); }
        }
        Message::ToggleBranches => {
            if state.picker.open { state.picker.open = false; return Task::none(); }
            state.picker = BranchPicker { open: true, loading: true, filter: state.picker.filter, ..BranchPicker::default() };
            let load = Task::perform(async move {
                tokio::task::spawn_blocking(move || branches::load(&cwd)).await.map_err(|err| err.to_string())?
            }, Message::BranchesLoaded);
            return Task::batch([load, iced::widget::operation::focus(branch_query_id())]);
        }
        Message::CloseBranches => state.picker.open = false,
        Message::BranchesLoaded(result) => {
            state.picker.loading = false;
            match result {
                Ok(list) => state.picker.branches = list,
                Err(err) => state.error = Some(err),
            }
        }
        Message::BranchQueryChanged(query) => state.picker.query = query,
        Message::ToggleBranchFilter => state.picker.filter_menu = !state.picker.filter_menu,
        Message::BranchFilterChosen(filter) => {
            state.picker.filter = filter;
            state.picker.filter_menu = false;
        }
        Message::BranchSubmit => {
            if let Some(action) = submit_action(&state.picker) { return update(state, Message::Branch(action), cwd); }
        }
        Message::Branch(action) => {
            if state.committing { return Task::none(); }
            state.committing = true;
            state.picker.open = false;
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || branches::run(&cwd, &action)).await.map_err(|err| err.to_string())?
            }, Message::BranchDone);
        }
        Message::BranchDone(result) => {
            state.committing = false;
            match result {
                Ok(()) => return refresh(state, cwd),
                Err(err) => state.error = Some(err),
            }
        }
        Message::ToggleSyncMenu => state.sync_menu = !state.sync_menu,
        Message::CloseSyncMenu => state.sync_menu = false,
        Message::Sync(sync) => {
            state.sync_menu = false;
            if state.committing { return Task::none(); }
            state.committing = true;
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || remote::run(&cwd, sync)).await.map_err(|err| err.to_string())?
            }, move |result| Message::SyncDone(sync, result));
        }
        Message::SyncDone(_, result) => {
            state.committing = false;
            // Refresh either way: ahead/behind moved, or a failed pull left conflicts.
            if let Err(err) = result { state.error = Some(err); }
            return refresh(state, cwd);
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
        Message::CommitMessageEdited(action) => state.commit_message.perform(action),
        Message::GenerateMessage => state.generating = true,
        Message::MessageGenerated(result) => {
            state.generating = false;
            if let Ok(message) = result {
                state.commit_message = text_editor::Content::with_text(&message);
            }
        }
        Message::Commit => {
            if !can_commit(state) {
                return Task::none();
            }
            state.committing = true;
            let message = state.commit_message.text();
            return Task::perform(async move {
                tokio::task::spawn_blocking(move || commit(&cwd, &message))
                    .await.map_err(|err| err.to_string())?
            }, Message::Committed);
        }
        Message::Committed(result) => {
            state.committing = false;
            match result {
                Ok(()) => {
                    state.commit_message = text_editor::Content::new();
                    return refresh(state, cwd);
                }
                Err(err) => state.error = Some(err),
            }
        }
    }
    Task::none()
}

/// What Enter does: the first listed branch, else create the typed name; nothing while loading.
fn submit_action(picker: &BranchPicker) -> Option<branches::Action> {
    if picker.loading { return None; }
    match branches::visible(&picker.branches, &picker.query, picker.filter).first() {
        Some(branch) => Some(branch_action(branch)),
        None => branches::new_name(&picker.branches, &picker.query).map(|name| branches::Action::Create(name.to_owned())),
    }
}

fn branch_action(branch: &branches::Branch) -> branches::Action {
    if branch.remote { branches::Action::Track(branch.name.clone()) } else { branches::Action::Switch(branch.name.clone()) }
}

fn can_commit(state: &GitState) -> bool {
    !state.committing && !state.commit_message.text().trim().is_empty() && state.status.files.iter().any(ChangedFile::staged)
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
    let branch_icon: char = lucide_icons::Icon::GitBranch.into();
    let label = state.status.branch.label();
    let mut branch = row![
        text(branch_icon).font(iced::Font::with_name("lucide")).size(14),
        text(if label.is_empty() { "No branch".to_string() } else { label.clone() }).size(13),
    ].spacing(8).align_y(iced::Alignment::Center);
    let (ahead, behind) = (state.status.branch.ahead, state.status.branch.behind);
    if ahead > 0 || behind > 0 {
        branch = branch.push(text(format!("↑{ahead} ↓{behind}")).size(12).style(iced::widget::text::secondary));
    }
    let branch_button = button(branch.push(Space::new().width(Length::Fill)))
        .padding([2, 4]).width(Length::Fill).style(crate::flat_button_style)
        .on_press_maybe((!state.committing).then_some(Message::ToggleBranches));
    let branch = iced_aw::DropDown::new(branch_button, branch_picker(&state.picker, &label), state.picker.open)
        .alignment(iced_aw::drop_down::Alignment::Bottom).on_dismiss(Message::CloseBranches);
    let sync_items = remote::Sync::ALL.into_iter().fold(column![].spacing(1), |menu, sync| {
        menu.push(button(text(sync.label()).size(13)).width(Length::Fill).padding([4, 10]).style(crate::flat_button_style)
            .on_press_maybe((!state.committing).then_some(Message::Sync(sync))))
    });
    let sync_menu = iced_aw::DropDown::new(
        crate::icon_control(lucide_icons::Icon::Ellipsis, "Fetch, pull, push", Some(Message::ToggleSyncMenu), state.sync_menu),
        container(sync_items).padding(4).style(crate::overlay_style),
        state.sync_menu,
    ).width(140).alignment(iced_aw::drop_down::Alignment::Bottom).on_dismiss(Message::CloseSyncMenu);
    let header = row![
        branch,
        crate::icon_control(lucide_icons::Icon::RefreshCw, "Refresh source control", Some(Message::Refresh), false),
        sync_menu,
    ].spacing(4).align_y(iced::Alignment::Center);
    let mut files_col = column![].spacing(2);
    if state.status.files.is_empty() && state.error.is_none() {
        files_col = files_col.push(container(text("Working tree clean").size(13)).padding([16, 0]));
    }
    let lucide = iced::Font::with_name("lucide");
    for entry in rows(&state.status.files, state.tree_view, &state.collapsed) {
        let (files, depth): (Vec<&ChangedFile>, usize) = match &entry {
            Row::Dir { files, depth, .. } => (files.clone(), *depth),
            Row::File { file, depth } => (vec![file], *depth),
        };
        // Checked = staged. A minus glyph marks partial staging (some edits still unstaged,
        // or only some files under a directory); clicking it finishes staging.
        let (checked, partial) = stage_state(&files);
        let owned: Vec<ChangedFile> = files.iter().map(|file| (*file).clone()).collect();
        let all_staged = checked && !partial;
        let mut check = checkbox(checked).size(14)
            .on_toggle_maybe((!state.committing).then_some(move |_| Message::Stage(owned.clone(), !all_staged)));
        if partial {
            check = check.icon(checkbox::Icon {
                font: lucide, code_point: lucide_icons::Icon::Minus.into(), size: Some(iced::Pixels(11.0)),
                line_height: iced::widget::text::LineHeight::default(), shaping: iced::widget::text::Shaping::Basic,
            });
        }
        let indent = Space::new().width(Length::Fixed(depth as f32 * 14.0));
        let label: Element<'a, Message> = match &entry {
            Row::Dir { path, name, open, .. } => {
                let chevron: char = if *open { lucide_icons::Icon::ChevronDown } else { lucide_icons::Icon::ChevronRight }.into();
                let folder: char = if *open { lucide_icons::Icon::FolderOpen } else { lucide_icons::Icon::Folder }.into();
                button(row![
                    text(chevron).font(lucide).size(12),
                    text(folder).font(lucide).size(13).style(iced::widget::text::secondary),
                    text(name.clone()).size(13).width(Length::Fill),
                    text(files.len().to_string()).size(11).style(iced::widget::text::secondary),
                ].spacing(6).align_y(iced::Alignment::Center))
                .padding([4, 6]).width(Length::Fill).style(crate::flat_button_style)
                .on_press(Message::ToggleDir(path.clone())).into()
            }
            Row::File { file, .. } => {
                let file: &'a ChangedFile = file;
                let is_selected = selected == Some(file.path.as_str());
                let path = Path::new(&file.path);
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(&file.path);
                let parent = path.parent().and_then(|path| path.to_str()).unwrap_or("");
                let mut label = row![text(name).size(13)].spacing(6).align_y(iced::Alignment::Center);
                // The tree already shows the directory; the flat list needs it on the row.
                if !state.tree_view && !parent.is_empty() {
                    label = label.push(text(parent).size(11).style(iced::widget::text::secondary));
                }
                button(row![
                        label.width(Length::Fill),
                        // Checking a conflicted file's box runs `git add`, i.e. marks it resolved.
                        text(if file.conflicted() { "conflict" } else { file.status.trim() }).size(12).style(|theme: &iced::Theme| {
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
                    .padding([4, 6])
                    .width(Length::Fill)
                    .style(move |theme, status| {
                        let mut style = crate::flat_button_style(theme, status);
                        if is_selected {
                            style.background = Some(theme.extended_palette().primary.weak.color.into());
                            style.text_color = theme.extended_palette().primary.weak.text;
                        }
                        style
                    })
                    .on_press(Message::OpenDiff(file.path.clone())).into()
            }
        };
        let discard: Vec<_> = files.iter().filter(|file| file.unstaged() && !file.conflicted()).map(|file| (*file).clone()).collect();
        let discard = crate::icon_control(lucide_icons::Icon::Undo2, "Discard unstaged changes", (!state.committing && !discard.is_empty()).then_some(Message::Discard(discard)), false);
        files_col = files_col.push(row![indent, check, label, discard].spacing(4).align_y(iced::Alignment::Center));
    }
    let files_list = scrollable(files_col).height(Length::Fill);

    let mut bottom = column![].spacing(4);
    if let Some(err) = &state.error {
        bottom = bottom.push(scrollable(text(err.clone()).size(12).font(iced::Font::MONOSPACE).style(iced::widget::text::danger)).height(Length::Shrink));
    }
    let has_staged = state.status.files.iter().any(ChangedFile::staged);
    bottom = bottom.push(
        text_editor(&state.commit_message)
            .placeholder("Describe your changes (Cmd/Ctrl+Enter to commit)")
            .on_action(Message::CommitMessageEdited)
            .size(13)
            .min_height(60.0)
            .max_height(200.0)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
            .key_binding(|key_press| {
                use iced::keyboard::{key::Named, Key};
                if key_press.modifiers.command() && key_press.key == Key::Named(Named::Enter) {
                    Some(text_editor::Binding::Custom(Message::Commit))
                } else {
                    text_editor::Binding::from_key_press(key_press)
                }
            }),
    );
    // The AI button stays disabled while a message is being written; the box fills in when done.
    bottom = bottom.push(row![
        Space::new().width(Length::Fill),
        crate::icon_control(lucide_icons::Icon::Sparkles, "Write message with AI from staged changes",
            (has_staged && !state.generating && !state.committing).then_some(Message::GenerateMessage), false),
        button(text(if state.committing { "Working..." } else { "Commit" }).size(13))
            .padding([8, 12])
            .on_press_maybe(can_commit(state).then_some(Message::Commit)),
    ].spacing(6).align_y(iced::Alignment::Center));
    let to_stage: Vec<ChangedFile> = state.status.files.iter().filter(|file| file.unstaged() || !file.staged()).cloned().collect();
    let to_unstage: Vec<ChangedFile> = state.status.files.iter().filter(|file| file.staged()).cloned().collect();
    let to_discard: Vec<_> = state.status.files.iter().filter(|file| file.unstaged() && !file.conflicted()).cloned().collect();
    let staged_count = to_unstage.len();
    let changes = row![
        text("Changes").size(13),
        text(format!("{} staged of {}", staged_count, state.status.files.len())).size(12).style(iced::widget::text::secondary),
        Space::new().width(Length::Fill),
        crate::icon_control(lucide_icons::Icon::Plus, "Stage all", (!state.committing && !to_stage.is_empty()).then(|| Message::Stage(to_stage.clone(), true)), false),
        crate::icon_control(lucide_icons::Icon::Minus, "Unstage all", (!state.committing && !to_unstage.is_empty()).then(|| Message::Stage(to_unstage.clone(), false)), false),
        crate::icon_control(lucide_icons::Icon::Undo2, "Discard all unstaged changes", (!state.committing && !to_discard.is_empty()).then_some(Message::Discard(to_discard)), false),
        crate::icon_control(lucide_icons::Icon::ListTree, if state.tree_view { "Show as flat list" } else { "Show as tree" }, Some(Message::ToggleTreeView), state.tree_view),
    ].spacing(6).align_y(iced::Alignment::Center);

    container(column![header, bottom, iced::widget::rule::horizontal(1), changes, files_list].spacing(8))
        .padding(8)
        .height(Length::Fill)
        .into()
}

/// Search box, filter menu, the matching branches and a "Create Branch" row for the typed name.
fn branch_picker<'a>(picker: &'a BranchPicker, current: &str) -> Element<'a, Message> {
    let secondary = iced::widget::text::secondary;
    let search = row![
        iced::widget::text_input("Select branch…", &picker.query).id(branch_query_id()).size(13).padding([5, 8])
            .on_input(Message::BranchQueryChanged).on_submit(Message::BranchSubmit),
        crate::icon_control(lucide_icons::Icon::SlidersHorizontal, "Filter branches", Some(Message::ToggleBranchFilter),
            picker.filter_menu || picker.filter != branches::Filter::All),
    ].spacing(4).align_y(iced::Alignment::Center);
    let mut content = column![search].spacing(6);
    if picker.filter_menu {
        content = content.push(branches::Filter::ALL.into_iter().fold(column![].spacing(1), |menu, filter| {
            let label = if filter == picker.filter { format!("✓ {}", filter.label()) } else { filter.label().to_owned() };
            menu.push(button(text(label).size(12)).width(Length::Fill).padding([3, 8]).style(crate::flat_button_style)
                .on_press(Message::BranchFilterChosen(filter)))
        }));
    }

    let visible = branches::visible(&picker.branches, &picker.query, picker.filter);
    let now = crate::ai_history::now();
    let mut list = column![].spacing(2);
    for (remote, heading) in [(false, "Local Branches"), (true, "Remote Branches")] {
        let group: Vec<_> = visible.iter().filter(|branch| branch.remote == remote).collect();
        if group.is_empty() { continue; }
        list = list.push(container(text(heading).size(11).style(secondary)).padding([4, 4]));
        for branch in group {
            let name = if !branch.remote && branch.name == current { format!("✓ {}", branch.name) } else { branch.name.clone() };
            let details = branch_details(branch, now);
            list = list.push(button(column![text(name).size(13), text(details).size(11).style(secondary)].spacing(1))
                .width(Length::Fill).padding([4, 8]).style(crate::flat_button_style)
                .on_press(Message::Branch(branch_action(branch))));
        }
    }
    // Until the list is in, the typed name may be an existing branch.
    if picker.loading {
        list = list.push(text("Loading branches…").size(12).style(secondary));
    } else if let Some(name) = branches::new_name(&picker.branches, &picker.query) {
        let plus: char = lucide_icons::Icon::Plus.into();
        list = list.push(button(row![
            text(plus).font(iced::Font::with_name("lucide")).size(13),
            column![
                text(format!("Create Branch: \"{name}\"…")).size(13),
                text(format!("Based off {}", if current.is_empty() { "the current commit" } else { current })).size(11).style(secondary),
            ].spacing(1),
        ].spacing(8).align_y(iced::Alignment::Center))
            .width(Length::Fill).padding([4, 8]).style(crate::flat_button_style)
            .on_press(Message::Branch(branches::Action::Create(name.to_owned()))));
    } else if visible.is_empty() {
        list = list.push(text(if picker.branches.is_empty() { "No branches yet. Type a name to create one." } else { "No matching branches" }).size(12).style(secondary));
    }
    content = content.push(container(scrollable(list)).max_height(320));
    container(content).padding(6).width(Length::Fill).style(crate::overlay_style).into()
}

/// "Ann · 2w ago · fix: e2e", leaving out whatever git did not report.
fn branch_details(branch: &branches::Branch, now: u64) -> String {
    let age = match crate::ai_history::ago(now, branch.date) {
        age if age.is_empty() => age,
        age if age == "now" => "just now".to_owned(),
        age => format!("{age} ago"),
    };
    [branch.author.as_str(), &age, &branch.subject].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// Restore tracked files from the index, preserving staged changes. Untracked files are removed.
fn discard_files(cwd: &Path, files: &[ChangedFile]) -> Result<(), String> {
    for file in files {
        if file.conflicted() { return Err("Resolve conflicts before discarding changes".into()); }
        let path = Path::new(&file.path);
        if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err("Invalid repository path".into());
        }
        if file.status == "??" {
            // Never recursively remove directories (including nested repositories).
            std::fs::remove_file(cwd.join(path)).map_err(|e| e.to_string())?;
        } else {
            cli::run(cwd, &["--literal-pathspecs", "restore", "--worktree", "--", &file.path], None, Access::Write)?;
        }
    }
    Ok(())
}

/// One line of the changes list: a directory (tree view only) or a file.
enum Row<'a> {
    Dir { path: String, name: String, depth: usize, open: bool, files: Vec<&'a ChangedFile> },
    File { file: &'a ChangedFile, depth: usize },
}

#[cfg(test)]
impl Row<'_> {
    fn path(&self) -> &str {
        match self { Row::Dir { path, .. } => path, Row::File { file, .. } => &file.path }
    }
    fn depth(&self) -> usize {
        match self { Row::Dir { depth, .. } | Row::File { depth, .. } => *depth }
    }
}

/// Lays the changed files out as rows: sorted by path when flat, otherwise a directory tree
/// with directories first, single-child directory chains compacted (`src/git`) and
/// `collapsed` directories closed.
fn rows<'a>(files: &'a [ChangedFile], tree: bool, collapsed: &HashSet<String>) -> Vec<Row<'a>> {
    if !tree {
        let mut flat: Vec<&ChangedFile> = files.iter().collect();
        flat.sort_by(|a, b| a.path.cmp(&b.path));
        return flat.into_iter().map(|file| Row::File { file, depth: 0 }).collect();
    }
    #[derive(Default)]
    struct Dir<'a> { dirs: BTreeMap<String, Dir<'a>>, files: Vec<&'a ChangedFile> }
    impl<'a> Dir<'a> {
        fn all_files(&self) -> Vec<&'a ChangedFile> {
            let mut all = self.files.clone();
            for dir in self.dirs.values() { all.extend(dir.all_files()); }
            all
        }
    }
    let mut root = Dir::default();
    for file in files {
        let mut dir = &mut root;
        let mut parts = file.path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() { dir.files.push(file); break; }
            dir = dir.dirs.entry(part.to_string()).or_default();
        }
    }
    fn walk<'a>(dir: Dir<'a>, prefix: &str, depth: usize, collapsed: &HashSet<String>, out: &mut Vec<Row<'a>>) {
        for (mut name, mut sub) in dir.dirs {
            while sub.files.is_empty() && sub.dirs.len() == 1 {
                let (child_name, child) = sub.dirs.into_iter().next().expect("one child");
                name = format!("{name}/{child_name}");
                sub = child;
            }
            let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let open = !collapsed.contains(&path);
            out.push(Row::Dir { path: path.clone(), name, depth, open, files: sub.all_files() });
            if open { walk(sub, &path, depth + 1, collapsed, out); }
        }
        let mut files = dir.files;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        out.extend(files.into_iter().map(|file| Row::File { file, depth }));
    }
    let mut out = Vec::new();
    walk(root, "", 0, collapsed, &mut out);
    out
}

/// `(checked, partial)` for a checkbox covering `files`: checked once anything is staged,
/// partial while any of it still has unstaged changes.
fn stage_state(files: &[&ChangedFile]) -> (bool, bool) {
    let any_staged = files.iter().any(|file| file.staged());
    let all_staged = files.iter().all(|file| file.staged() && !file.unstaged());
    (any_staged, any_staged && !all_staged)
}

/// Longest staged diff sent to the AI; bigger ones are cut, with a note saying so.
const MAX_PROMPT_DIFF: usize = 60_000;

/// A prompt asking for a commit message for the staged changes, in the style of recent ones.
pub fn commit_message_prompt(cwd: &Path) -> Result<String, String> {
    let diff = cli::run(cwd, &["diff", "--cached", "--no-color", "--no-ext-diff"], None, Access::Read)?;
    let diff = String::from_utf8_lossy(&diff);
    if diff.trim().is_empty() { return Err("Stage some changes first".into()); }
    // An unborn branch has no history to match; that's fine.
    let recent = cli::run(cwd, &["log", "-8", "--format=%s"], None, Access::Read).unwrap_or_default();
    Ok(commit_prompt(&diff, &String::from_utf8_lossy(&recent)))
}

fn commit_prompt(diff: &str, recent_subjects: &str) -> String {
    let mut prompt = String::from(
        "Write a git commit message for the staged changes below.\n\
         Reply with only the message: a concise imperative subject line under 72 characters, \
         then, only if the change needs explaining, a blank line and a short body. \
         No quotes, no code fences, no commentary.\n",
    );
    let subjects: Vec<&str> = recent_subjects.lines().filter(|line| !line.trim().is_empty()).collect();
    if !subjects.is_empty() {
        prompt.push_str("\nMatch the style of this repository's recent subjects:\n");
        for subject in subjects { prompt.push_str(&format!("- {subject}\n")); }
    }
    let cut = diff.char_indices().nth(MAX_PROMPT_DIFF).map(|(index, _)| index);
    prompt.push_str("\nStaged diff:\n");
    prompt.push_str(&diff[..cut.unwrap_or(diff.len())]);
    if cut.is_some() { prompt.push_str("\n[diff truncated]\n"); }
    prompt
}

/// The message from an AI reply, without fences or wrapping quotes.
pub fn clean_commit_message(reply: &str) -> String {
    let message = crate::ai_oneshot::unfence(reply);
    let message = message.trim();
    let message = message.strip_prefix('"').and_then(|m| m.strip_suffix('"')).unwrap_or(message);
    message.trim().to_string()
}

/// Commits the index with `git commit`, so commit hooks and signing run as configured.
fn commit(cwd: &Path, message: &str) -> Result<(), String> {
    cli::run(cwd, &["commit", "-F", "-"], Some(message.as_bytes()), Access::Write).map(|_| ())
}

fn stage_files(cwd: &Path, files: &[ChangedFile], stage: bool, unborn: bool) -> Result<(), String> {
    // Literal pathspecs also support filenames containing Git wildcard characters.
    let mut args = if stage {
        vec!["--literal-pathspecs", "add", "-A", "--"]
    } else if unborn {
        // Nothing to restore from before the first commit; drop the paths from the index.
        vec!["--literal-pathspecs", "rm", "--cached", "-r", "-q", "--"]
    } else {
        vec!["--literal-pathspecs", "restore", "--staged", "--"]
    };
    for file in files {
        args.push(&file.path);
        if let Some(original) = &file.original { args.push(original); }
    }
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
    fn discard_preserves_index_restores_deletions_and_uses_literal_paths() {
        for autocrlf in [false, true] {
            check_discard(autocrlf);
        }
    }

    fn check_discard(autocrlf: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init"]);
        // Exercise both checkout styles independently of the host's Git configuration.
        git(root, &["config", "core.autocrlf", if autocrlf { "true" } else { "false" }]);
        git(root, &["config", "core.eol", "lf"]);
        let checkout_eol = if autocrlf { "\r\n" } else { "\n" };
        for name in ["a[1].txt", "a1.txt", "deleted.txt"] { std::fs::write(root.join(name), "original\n").unwrap(); }
        git(root, &["add", "."]);
        git(root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "Initial"]);
        std::fs::write(root.join("a[1].txt"), "staged\n").unwrap();
        git(root, &["--literal-pathspecs", "add", "--", "a[1].txt"]);
        std::fs::write(root.join("a[1].txt"), "unstaged\n").unwrap();
        std::fs::write(root.join("a1.txt"), "keep\n").unwrap();
        std::fs::remove_file(root.join("deleted.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        discard_files(root, &[file("a[1].txt", "MM"), file("deleted.txt", " D"), file("new.txt", "??")]).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a[1].txt")).unwrap(), format!("staged{checkout_eol}"));
        assert_eq!(std::fs::read_to_string(root.join("a1.txt")).unwrap(), "keep\n");
        assert_eq!(std::fs::read_to_string(root.join("deleted.txt")).unwrap(), format!("original{checkout_eol}"));
        assert!(!root.join("new.txt").exists());
        assert!(git(root, &["diff", "--cached"]).contains("+staged"));
        assert!(discard_files(root, &[file("../outside", "??")]).is_err());
        let blame = blame::load(root, Path::new("a1.txt"), "original\ninserted\n");
        assert!(blame[0].starts_with("Test, "));
        assert_eq!(blame[1], "Uncommitted changes");
    }

    #[test]
    fn stage_and_unstage_preserve_worktree_and_other_files() {
        let root = std::env::temp_dir().join(format!("editor-staging-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init"]);
        std::fs::write(root.join("a [1].txt"), "initial\n").unwrap();
        std::fs::write(root.join("b.txt"), "other\n").unwrap();
        let file = file("a [1].txt", "??");
        stage_files(&root, std::slice::from_ref(&file), true, true).unwrap();
        assert_eq!(git(&root, &["diff", "--cached", "--name-only"]).trim(), "a [1].txt");
        stage_files(&root, std::slice::from_ref(&file), false, true).unwrap();
        assert!(git(&root, &["ls-files"]).is_empty());
        assert_eq!(std::fs::read_to_string(root.join(&file.path)).unwrap(), "initial\n");
        stage_files(&root, std::slice::from_ref(&file), true, true).unwrap();
        git(&root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-m", "Initial"]);
        std::fs::write(root.join(&file.path), "updated\n").unwrap();
        stage_files(&root, std::slice::from_ref(&file), true, false).unwrap();
        stage_files(&root, std::slice::from_ref(&file), false, false).unwrap();
        assert!(git(&root, &["diff", "--cached"]).is_empty());
        assert_eq!(std::fs::read_to_string(root.join(&file.path)).unwrap(), "updated\n");
        std::fs::remove_file(root.join(&file.path)).unwrap();
        stage_files(&root, std::slice::from_ref(&file), true, false).unwrap();
        assert!(git(&root, &["diff", "--cached", "--summary"]).contains("delete mode"));
        stage_files(&root, std::slice::from_ref(&file), false, false).unwrap();
        assert!(!root.join(&file.path).exists());
        // Several files in one call, as the tree's directory checkboxes do.
        let many = [self::tests::file("b.txt", "??"), self::tests::file("a [1].txt", " D")];
        stage_files(&root, &many, true, false).unwrap();
        assert_eq!(git(&root, &["diff", "--cached", "--name-only"]).lines().count(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_message_accepts_multiple_lines() {
        use text_editor::{Action, Edit};
        let mut state = GitState::default();
        let cwd = PathBuf::from(".");
        for action in "Subject".chars().map(Edit::Insert).chain([Edit::Enter, Edit::Enter]).chain("Body".chars().map(Edit::Insert)) {
            let _ = update(&mut state, Message::CommitMessageEdited(Action::Edit(action)), cwd.clone());
        }
        assert_eq!(state.commit_message.text().trim_end(), "Subject\n\nBody");
        // Nothing staged: committing is refused without starting git.
        assert!(!can_commit(&state));
        let _ = update(&mut state, Message::Commit, cwd);
        assert!(!state.committing);
        state.status.files.push(file("a.txt", "M "));
        assert!(can_commit(&state));
        state.commit_message = text_editor::Content::with_text(" \n \n");
        assert!(!can_commit(&state));
    }

    #[test]
    fn commit_prompts_include_style_and_bounded_diffs_and_replies_are_cleaned() {
        let prompt = commit_prompt("+fn login() {}\n", "Fix crash on save\n\nAdd blame view\n");
        assert!(prompt.contains("- Fix crash on save\n- Add blame view\n"));
        assert!(prompt.ends_with("Staged diff:\n+fn login() {}\n"));
        assert!(!commit_prompt("+x\n", "").contains("recent subjects"));
        let huge = "é".repeat(MAX_PROMPT_DIFF + 10);
        let prompt = commit_prompt(&huge, "");
        assert!(prompt.ends_with("[diff truncated]\n"));
        assert_eq!(prompt.matches('é').count(), MAX_PROMPT_DIFF);

        assert_eq!(clean_commit_message("```\nAdd login\n\nChecks the token.\n```"), "Add login\n\nChecks the token.");
        assert_eq!(clean_commit_message("\"Add login\"\n"), "Add login");

        let mut state = GitState::default();
        let _ = update(&mut state, Message::GenerateMessage, PathBuf::from("."));
        assert!(state.generating);
        let _ = update(&mut state, Message::MessageGenerated(Err("offline".into())), PathBuf::from("."));
        assert!(!state.generating);
        assert!(state.commit_message.text().trim().is_empty(), "a failure keeps the draft");
        let _ = update(&mut state, Message::MessageGenerated(Ok("Add login\n\nBody".into())), PathBuf::from("."));
        assert_eq!(state.commit_message.text().trim_end(), "Add login\n\nBody");
    }

    #[test]
    fn tree_rows_nest_compact_and_collapse() {
        let files = [file("src/git/status.rs", "M "), file("src/main.rs", " M"), file("README.md", "??"), file("src/git/cli.rs", "MM")];
        let flat = rows(&files, false, &HashSet::new());
        assert_eq!(flat.iter().map(Row::path).collect::<Vec<_>>(), ["README.md", "src/git/cli.rs", "src/git/status.rs", "src/main.rs"]);
        let tree = rows(&files, true, &HashSet::new());
        let labels: Vec<_> = tree.iter().map(|row| (row.depth(), row.path())).collect();
        assert_eq!(labels, [(0, "src"), (1, "src/git"), (2, "src/git/cli.rs"), (2, "src/git/status.rs"), (1, "src/main.rs"), (0, "README.md")]);
        let Row::Dir { files: under_git, .. } = &tree[1] else { panic!() };
        assert_eq!(stage_state(under_git), (true, true)); // cli.rs still has unstaged edits
        let collapsed = rows(&files, true, &HashSet::from(["src/git".to_string()]));
        assert_eq!(collapsed.iter().map(Row::path).collect::<Vec<_>>(), ["src", "src/git", "src/main.rs", "README.md"]);
        assert_eq!(stage_state(&[&files[0]]), (true, false));
        assert_eq!(stage_state(&[&files[2]]), (false, false));
    }

    #[test]
    fn branch_picker_opens_submits_and_blocks_while_git_runs() {
        use branches::{Action, Branch, Filter};
        let branch = |name: &str, remote| Branch { name: name.into(), remote, author: "Ann".into(), date: 0, subject: String::new() };
        let mut state = GitState::default();
        let cwd = PathBuf::from(".");
        let _ = update(&mut state, Message::ToggleBranches, cwd.clone());
        assert!(state.picker.open && state.picker.loading);
        let _ = update(&mut state, Message::BranchQueryChanged("dev".into()), cwd.clone());
        assert_eq!(submit_action(&state.picker), None, "nothing is chosen before the list loads");
        let _ = update(&mut state, Message::BranchesLoaded(Ok(vec![branch("main", false), branch("origin/dev", true)])), cwd.clone());
        assert_eq!(submit_action(&state.picker), Some(Action::Track("origin/dev".into())));
        let _ = update(&mut state, Message::BranchFilterChosen(Filter::Local), cwd.clone());
        assert_eq!(submit_action(&state.picker), Some(Action::Create("dev".into())));
        let _ = update(&mut state, Message::BranchQueryChanged("MAI".into()), cwd.clone());
        assert_eq!(submit_action(&state.picker), Some(Action::Switch("main".into())));
        assert_eq!(branch_details(&branch("main", false), 100), "Ann");

        let _ = update(&mut state, Message::Branch(Action::Switch("main".into())), cwd.clone());
        assert!(state.committing && !state.picker.open);
        let _ = update(&mut state, Message::ToggleBranches, cwd.clone());
        let _ = update(&mut state, Message::Branch(Action::Create("x".into())), cwd.clone());
        assert!(state.committing, "a second git write waits for the first");
        let _ = update(&mut state, Message::BranchDone(Err("error: Your local changes would be overwritten".into())), cwd.clone());
        assert!(!state.committing);
        assert!(state.error.as_deref().unwrap().contains("overwritten"));
        let _ = update(&mut state, Message::CloseBranches, cwd);
        assert!(!state.picker.open);
    }

    #[test]
    fn sync_menu_runs_one_git_command_at_a_time_and_keeps_errors() {
        use remote::Sync;
        let mut state = GitState::default();
        let cwd = PathBuf::from(".");
        let _ = update(&mut state, Message::ToggleSyncMenu, cwd.clone());
        assert!(state.sync_menu);
        let _ = update(&mut state, Message::Sync(Sync::Pull), cwd.clone());
        assert!(state.committing && !state.sync_menu);
        let _ = update(&mut state, Message::ToggleSyncMenu, cwd.clone());
        let _ = update(&mut state, Message::Sync(Sync::Push), cwd.clone());
        assert!(state.committing && !state.sync_menu, "a second command waits for the first");
        let _ = update(&mut state, Message::SyncDone(Sync::Pull, Err("CONFLICT (content): Merge conflict in a.txt".into())), cwd.clone());
        assert!(!state.committing);
        assert!(state.error.as_deref().unwrap().contains("CONFLICT"));
        assert!(state.refreshing, "status reloads even after a failure");
        let _ = update(&mut state, Message::ToggleSyncMenu, cwd.clone());
        let _ = update(&mut state, Message::CloseSyncMenu, cwd);
        assert!(!state.sync_menu);
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
        stage_files(&root, &[file("a.txt", "??")], true, true).unwrap();
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
