mod acp;
mod chat;
mod code_editor;
mod git;
mod project_search;
mod quick_open;

use iced::keyboard;
use iced::widget::{
    button, column, container, pane_grid, row, scrollable, text, text_input, PaneGrid, Space,
};
use iced::{Element, Length, Subscription, Task};
use iced_aw::context_menu::ContextMenu;
use iced_aw::menu::{Item, Menu};
use iced_aw::{menu_bar, menu_items};
use iced_swdir_tree::{DirectoryFilter, DirectoryTree, DirectoryTreeEvent};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    fn detect(text: &str) -> Self {
        if text.contains("\r\n") {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    }
}

impl std::fmt::Display for LineEnding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineEnding::Lf => write!(f, "LF"),
            LineEnding::Crlf => write!(f, "CRLF"),
        }
    }
}

struct Tab {
    path: Option<PathBuf>,
    content: code_editor::Buffer,
    search: code_editor::search::SearchState,
    dirty: bool,
    line_ending: LineEnding,
}

impl Tab {
    fn title(&self) -> String {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string());
        if self.dirty {
            format!("{name} *")
        } else {
            name
        }
    }

    fn extension(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Tree,
    Editor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SidebarMode {
    #[default]
    Tree,
    ProjectSearch,
    Git,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileAction {
    OpenFile,
    OpenFolder,
    CloseFolder,
    Save,
}

impl std::fmt::Display for FileAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            FileAction::OpenFile => "Open File (Cmd+O)",
            FileAction::OpenFolder => "Open Folder",
            FileAction::CloseFolder => "Close Folder",
            FileAction::Save => "Save (Cmd+S)",
        };
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditAction {
    Undo,
    Redo,
    SelectAll,
}

impl std::fmt::Display for EditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            EditAction::Undo => "Undo (Cmd+Z)",
            EditAction::Redo => "Redo (Cmd+Shift+Z)",
            EditAction::SelectAll => "Select All (Cmd+A)",
        };
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewAction {
    ZoomIn,
    ZoomOut,
    ZoomReset,
}

impl std::fmt::Display for ViewAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            ViewAction::ZoomIn => "Zoom In (Cmd+=)",
            ViewAction::ZoomOut => "Zoom Out (Cmd+-)",
            ViewAction::ZoomReset => "Reset Zoom (Cmd+0)",
        };
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum AiMode {
    #[default]
    Http,
    Acp,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PaneKind {
    Sidebar,
    Main,
    Ai,
}

#[derive(Debug, Clone)]
enum Message {
    EditorAction(cosmic_text::Action),
    Search(code_editor::search::Message),
    ProjectSearch(project_search::Message),
    ToggleProjectSearch,
    QuickOpen(quick_open::Message),
    ToggleQuickOpen,
    Git(git::Message),
    GitPanelToggle,
    TabSelected(usize),
    TabClosed(usize),

    Tree(DirectoryTreeEvent),
    RefreshTree,
    ContextNewFile,
    ContextNewFolder,
    ContextRename,
    ContextDelete,
    ContextCopyPath,
    ContextCopyRelativePath,
    ContextReveal,
    RenameInput(String),
    RenameSubmit,
    RenameCancel,
    CreateInput(String),
    CreateSubmit,
    CreateCancel,

    FileAction(FileAction),
    EditAction(EditAction),
    ViewAction(ViewAction),
    AppThemeSelected(iced::Theme),
    PaneResized(pane_grid::ResizeEvent),

    KeyPressed(keyboard::Key, keyboard::Modifiers),
    Noop,

    SidebarToggle,
    AiToggle,
    AiModeSelected(AiMode),
    Chat(chat::Message),
    Acp(acp::Message),
    Tick,

    WindowResized(iced::window::Id, iced::Size),
    WindowMoved(iced::Point),
    WindowMaximizedChecked(bool),
}

struct State {
    root: Option<PathBuf>,
    tabs: Vec<Tab>,
    active_tab: usize,
    focus: Focus,

    tree: Option<DirectoryTree>,
    renaming: Option<(PathBuf, String)>,
    creating: Option<(PathBuf, bool, String)>,
    sidebar_mode: SidebarMode,
    project_search: project_search::SearchState,
    quick_open: quick_open::QuickOpenState,
    git: git::GitState,

    app_theme: iced::Theme,
    highlighter: code_editor::Highlighter,
    zoom: f32,

    panes: pane_grid::State<PaneKind>,
    sidebar_split: Option<pane_grid::Split>,
    ai_split: Option<pane_grid::Split>,
    sidebar_visible: bool,

    /// The most recent size reported by a `WindowResized` event -- kept so the async
    /// `is_maximized` check triggered by that same event (see its handler) knows what to
    /// persist as "windowed size" once it resolves.
    last_known_size: iced::Size,

    ai_visible: bool,
    ai_mode: AiMode,
    chat: chat::ChatState,
    acp: acp::AcpState,
}

impl State {
    fn new() -> Self {
        let root = load_last_project();
        let tree = root
            .clone()
            .map(|p| DirectoryTree::new(p).with_filter(DirectoryFilter::FilesAndFolders));
        let sidebar_visible = load_sidebar_visible();
        let ai_visible = load_ai_visible();
        let app_theme = load_app_theme();
        let zoom = load_zoom();
        let sidebar_ratio = load_sidebar_ratio();
        let ai_ratio = load_ai_ratio();

        let (mut panes, first_pane) = if sidebar_visible {
            pane_grid::State::new(PaneKind::Sidebar)
        } else {
            pane_grid::State::new(PaneKind::Main)
        };

        let (main_pane, sidebar_split) = if sidebar_visible {
            let (main_pane, split) = panes
                .split(pane_grid::Axis::Vertical, first_pane, PaneKind::Main)
                .expect("splitting a freshly created single-pane state always succeeds");
            panes.resize(split, sidebar_ratio);
            (main_pane, Some(split))
        } else {
            (first_pane, None)
        };

        let ai_split = if ai_visible {
            let split = panes
                .split(pane_grid::Axis::Vertical, main_pane, PaneKind::Ai)
                .map(|(_, split)| split);
            if let Some(split) = split {
                panes.resize(split, ai_ratio);
            }
            split
        } else {
            None
        };

        Self {
            root,
            tabs: Vec::new(),
            active_tab: 0,
            focus: Focus::Editor,
            tree,
            renaming: None,
            creating: None,
            sidebar_mode: SidebarMode::default(),
            project_search: project_search::SearchState::default(),
            quick_open: quick_open::QuickOpenState::default(),
            git: git::GitState::default(),
            app_theme,
            highlighter: code_editor::Highlighter::new(),
            zoom,
            panes,
            sidebar_split,
            ai_split,
            sidebar_visible,
            last_known_size: load_window_size(),
            ai_visible,
            ai_mode: AiMode::default(),
            chat: chat::ChatState::default(),
            acp: acp::AcpState::default(),
        }
    }

    fn root_or_cwd(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(|| PathBuf::from("."))
    }

    /// Re-inserts the sidebar pane (to Main's left) if it isn't already showing, and marks it
    /// visible/persisted. Safe to call when the sidebar is already visible -- it's a no-op.
    fn show_sidebar(&mut self) {
        if self.sidebar_visible {
            return;
        }
        self.sidebar_visible = true;
        save_sidebar_visible(true);
        let main_pane = self.panes.iter().find(|(_, kind)| **kind == PaneKind::Main).map(|(pane, _)| *pane);
        if let Some(main_pane) = main_pane {
            if let Some((sidebar_pane, split)) =
                self.panes.split(pane_grid::Axis::Vertical, main_pane, PaneKind::Sidebar)
            {
                // `split` always inserts the new pane after the target, i.e. to Main's
                // right; swap them so the sidebar ends up on the left.
                self.panes.swap(main_pane, sidebar_pane);
                self.panes.resize(split, load_sidebar_ratio());
                self.sidebar_split = Some(split);
            }
        }
    }

    fn hide_sidebar(&mut self) {
        self.sidebar_visible = false;
        save_sidebar_visible(false);
        let sidebar_pane = self.panes.iter().find(|(_, kind)| **kind == PaneKind::Sidebar).map(|(pane, _)| *pane);
        if let Some(sidebar_pane) = sidebar_pane {
            self.panes.close(sidebar_pane);
        }
        self.sidebar_split = None;
    }

    /// Single entry point for every sidebar-mode button (tree/git/project-search): clicking
    /// the button for the panel that's already showing collapses the sidebar (matching the
    /// familiar "activity bar" pattern); clicking any other button switches to that panel,
    /// opening the sidebar first if it was closed. Returns `true` if `mode` ended up visible
    /// (as opposed to the sidebar collapsing), so callers can decide whether to e.g. refresh.
    fn toggle_sidebar_mode(&mut self, mode: SidebarMode) -> bool {
        if self.sidebar_visible && self.sidebar_mode == mode {
            self.hide_sidebar();
            false
        } else {
            self.sidebar_mode = mode;
            self.show_sidebar();
            true
        }
    }

    /// The path the context menu should act on: the tree's current selection, or the root.
    fn context_target_path(&self) -> Option<PathBuf> {
        self.tree
            .as_ref()
            .and_then(|t| t.selected_path())
            .map(Path::to_path_buf)
            .or_else(|| self.root.clone())
    }

    /// The directory a New File/Folder should be created in.
    fn context_target_dir(&self) -> Option<PathBuf> {
        let path = self.context_target_path()?;
        if path.is_dir() {
            Some(path)
        } else {
            Some(path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")))
        }
    }


    /// Reloads open, unmodified tabs from disk, in case the AI sidebar just edited them.
    fn reload_open_tabs(&mut self) {
        for tab in &mut self.tabs {
            if tab.dirty {
                continue;
            }
            let Some(path) = &tab.path else { continue };
            if let Ok(text) = std::fs::read_to_string(path) {
                if text != tab.content.text() {
                    let extension = tab.extension();
                    tab.content =
                        code_editor::Buffer::new(&text, code_editor::metrics_for_zoom(self.zoom));
                    tab.content.highlight(&self.highlighter, &extension, &self.app_theme);
                }
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path.as_path()))
        {
            self.active_tab = index;
            return;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_string();
        let line_ending = LineEnding::detect(&text);
        let mut content = code_editor::Buffer::new(&text, code_editor::metrics_for_zoom(self.zoom));
        content.highlight(&self.highlighter, &extension, &self.app_theme);
        self.tabs.push(Tab {
            path: Some(path),
            content,
            search: code_editor::search::SearchState::default(),
            dirty: false,
            line_ending,
        });
        self.active_tab = self.tabs.len() - 1;
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].dirty && !confirm("Unsaved changes", "Discard unsaved changes?") {
            return;
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab > index || self.active_tab >= self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1).min(self.tabs.len() - 1);
        }
    }

    fn save(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let path = match &tab.path {
            Some(p) => Some(p.clone()),
            None => rfd::FileDialog::new().save_file(),
        };
        if let Some(path) = path {
            if std::fs::write(&path, tab.content.text()).is_ok() {
                tab.path = Some(path);
                tab.dirty = false;
            }
        }
    }

    /// Routes a key press to the active tab's editor. Returns whether it was handled.
    fn handle_editor_key(&mut self, key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        let before = tab.content.undo_count();
        let handled = code_editor::input::handle_key(&mut tab.content, key, modifiers);
        self.mark_edited_if_changed(before);
        handled
    }

    /// Marks the active tab dirty and re-runs syntax highlighting if `before` (an
    /// `undo_count()` taken just before some editor operation) no longer matches -- i.e. the
    /// operation actually changed the document, as opposed to just moving the cursor.
    fn mark_edited_if_changed(&mut self, before: usize) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if tab.content.undo_count() == before {
            return;
        }
        tab.dirty = true;
        let extension = tab.extension();
        tab.content.highlight(&self.highlighter, &extension, &self.app_theme);
    }

    /// Applies `action` to `self.zoom`, then re-shapes every open tab's buffer at the new
    /// font size/line height and persists the level for next launch.
    fn apply_zoom(&mut self, action: ViewAction) {
        self.zoom = match action {
            ViewAction::ZoomIn => (self.zoom + code_editor::ZOOM_STEP).min(code_editor::ZOOM_MAX),
            ViewAction::ZoomOut => (self.zoom - code_editor::ZOOM_STEP).max(code_editor::ZOOM_MIN),
            ViewAction::ZoomReset => code_editor::ZOOM_DEFAULT,
        };
        let metrics = code_editor::metrics_for_zoom(self.zoom);
        for tab in &mut self.tabs {
            tab.content.set_metrics(metrics);
        }
        save_zoom(self.zoom);
    }

    fn delete_path(&mut self, path: &Path) {
        if !confirm(
            "Delete",
            &format!("Delete \"{}\"? This cannot be undone.", path.display()),
        ) {
            return;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if result.is_ok() {
            if let Some(index) = self.tabs.iter().position(|t| t.path.as_deref() == Some(path)) {
                self.close_tab(index);
            }
        }
    }
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    let mut task = Task::none();
    match message {
        Message::EditorAction(action) => {
            state.focus = Focus::Editor;
            let before = state
                .tabs
                .get(state.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                tab.content.perform(action);
            }
            state.mark_edited_if_changed(before);
        }
        Message::Search(msg) => {
            let before = state
                .tabs
                .get(state.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                code_editor::search::update(&mut tab.search, &mut tab.content, msg);
            }
            state.mark_edited_if_changed(before);
        }
        Message::ProjectSearch(msg) => {
            let root = state.root.clone();
            if let Some((path, line)) = project_search::update(&mut state.project_search, msg, root.as_deref()) {
                state.open_path(path);
                state.focus = Focus::Editor;
                if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                    tab.content
                        .perform(cosmic_text::Action::Motion(cosmic_text::Motion::GotoLine(
                            line.saturating_sub(1),
                        )));
                }
            }
        }
        Message::ToggleProjectSearch => {
            state.toggle_sidebar_mode(SidebarMode::ProjectSearch);
        }
        Message::QuickOpen(msg) => {
            let root = state.root.clone();
            let (t, opened) = quick_open::update(&mut state.quick_open, msg, root.as_deref());
            task = t.map(Message::QuickOpen);
            if let Some(path) = opened {
                state.open_path(path);
                state.focus = Focus::Editor;
            }
        }
        Message::ToggleQuickOpen => {
            let root = state.root.clone();
            let msg = if state.quick_open.visible {
                quick_open::Message::Close
            } else {
                quick_open::Message::Open
            };
            let (t, _) = quick_open::update(&mut state.quick_open, msg, root.as_deref());
            task = t.map(Message::QuickOpen);
        }
        Message::Git(msg) => {
            let cwd = state.root_or_cwd();
            task = git::update(&mut state.git, msg, cwd).map(Message::Git);
        }
        Message::GitPanelToggle => {
            if state.toggle_sidebar_mode(SidebarMode::Git) {
                let cwd = state.root_or_cwd();
                task = git::update(&mut state.git, git::Message::Refresh, cwd).map(Message::Git);
            }
        }
        Message::TabSelected(i) => state.active_tab = i,
        Message::TabClosed(i) => state.close_tab(i),

        Message::FileAction(action) => match action {
            FileAction::OpenFile => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    state.open_path(path);
                    state.focus = Focus::Editor;
                }
            }
            FileAction::OpenFolder => {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    save_last_project(&path);
                    state.tree = Some(
                        DirectoryTree::new(path.clone()).with_filter(DirectoryFilter::FilesAndFolders),
                    );
                    state.root = Some(path);
                }
            }
            FileAction::CloseFolder => {
                state.root = None;
                state.tree = None;
                if let Some(path) = last_project_path() {
                    let _ = std::fs::remove_file(path);
                }
            }
            FileAction::Save => state.save(),
        },

        Message::EditAction(action) => {
            let before = state
                .tabs
                .get(state.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                match action {
                    EditAction::Undo => tab.content.undo(),
                    EditAction::Redo => tab.content.redo(),
                    EditAction::SelectAll => tab.content.select_all(),
                }
            }
            state.mark_edited_if_changed(before);
        }

        Message::ViewAction(action) => state.apply_zoom(action),

        Message::Tree(event) => {
            state.focus = Focus::Tree;
            if let DirectoryTreeEvent::Selected(path, is_dir, _) = &event {
                if !*is_dir {
                    state.open_path(path.clone());
                }
            }
            if let Some(tree) = &mut state.tree {
                task = tree.update(event).map(Message::Tree);
            }
        }
        Message::RefreshTree => {
            if let Some(root) = state.root.clone() {
                task = refresh_dir_task(root);
            }
        }
        Message::ContextNewFile => {
            if let Some(dir) = state.context_target_dir() {
                state.creating = Some((dir, false, String::new()));
            }
        }
        Message::ContextNewFolder => {
            if let Some(dir) = state.context_target_dir() {
                state.creating = Some((dir, true, String::new()));
            }
        }
        Message::ContextRename => {
            if let Some(path) = state.context_target_path() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                state.renaming = Some((path, name));
            }
        }
        Message::ContextDelete => {
            if let Some(path) = state.context_target_path() {
                state.delete_path(&path);
                if let Some(parent) = path.parent() {
                    task = refresh_dir_task(parent.to_path_buf());
                }
            }
        }
        Message::ContextCopyPath => {
            if let Some(path) = state.context_target_path() {
                task = iced::clipboard::write(path.display().to_string());
            }
        }
        Message::ContextCopyRelativePath => {
            if let Some(path) = state.context_target_path() {
                let relative = state
                    .root
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
                    .map(Path::to_path_buf)
                    .unwrap_or(path);
                task = iced::clipboard::write(relative.display().to_string());
            }
        }
        Message::ContextReveal => {
            if let Some(path) = state.context_target_path() {
                reveal_in_file_manager(&path);
            }
        }
        Message::RenameInput(text) => {
            if let Some((_, buf)) = &mut state.renaming {
                *buf = text;
            }
        }
        Message::RenameSubmit => {
            if let Some((old_path, new_name)) = state.renaming.take() {
                if !new_name.is_empty() {
                    let new_path = old_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(&new_name);
                    if std::fs::rename(&old_path, &new_path).is_ok() {
                        if let Some(tab) = state
                            .tabs
                            .iter_mut()
                            .find(|t| t.path.as_deref() == Some(old_path.as_path()))
                        {
                            tab.path = Some(new_path);
                        }
                        if let Some(parent) = old_path.parent() {
                            task = refresh_dir_task(parent.to_path_buf());
                        }
                    }
                }
            }
        }
        Message::RenameCancel => state.renaming = None,
        Message::CreateInput(text) => {
            if let Some((_, _, buf)) = &mut state.creating {
                *buf = text;
            }
        }
        Message::CreateSubmit => {
            if let Some((dir, is_dir, name)) = state.creating.take() {
                if !name.is_empty() {
                    let target = dir.join(&name);
                    let result = if is_dir {
                        std::fs::create_dir(target)
                    } else {
                        std::fs::write(target, "")
                    };
                    if result.is_ok() {
                        task = refresh_dir_task(dir);
                    }
                }
            }
        }
        Message::CreateCancel => state.creating = None,

        Message::AppThemeSelected(theme) => {
            save_app_theme(&theme);
            state.app_theme = theme;
            for tab in &mut state.tabs {
                let extension = tab.extension();
                tab.content.highlight(&state.highlighter, &extension, &state.app_theme);
            }
        }
        Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
            state.panes.resize(split, ratio);
            if Some(split) == state.sidebar_split {
                save_sidebar_ratio(ratio);
            } else if Some(split) == state.ai_split {
                save_ai_ratio(ratio);
            }
        }

        Message::KeyPressed(key, modifiers) => {
            if modifiers.command() {
                match key.as_ref() {
                    keyboard::Key::Character("s") => state.save(),
                    keyboard::Key::Character("o") => {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            state.open_path(path);
                            state.focus = Focus::Editor;
                        }
                    }
                    // Checked before the plain "f" arm below: Shift+F usually reports as
                    // character "F", but match on either case defensively.
                    keyboard::Key::Character(c)
                        if modifiers.shift() && c.eq_ignore_ascii_case("f") =>
                    {
                        state.toggle_sidebar_mode(SidebarMode::ProjectSearch);
                    }
                    keyboard::Key::Character("f") => {
                        if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                            code_editor::search::update(
                                &mut tab.search,
                                &mut tab.content,
                                code_editor::search::Message::Toggle,
                            );
                        }
                    }
                    keyboard::Key::Character("p") => {
                        let root = state.root.clone();
                        let msg = if state.quick_open.visible {
                            quick_open::Message::Close
                        } else {
                            quick_open::Message::Open
                        };
                        let (t, _) = quick_open::update(&mut state.quick_open, msg, root.as_deref());
                        task = t.map(Message::QuickOpen);
                    }
                    // "+" covers Shift+= on layouts where that's how a plus sign is typed.
                    keyboard::Key::Character("=") | keyboard::Key::Character("+") => {
                        state.apply_zoom(ViewAction::ZoomIn);
                    }
                    keyboard::Key::Character("-") => {
                        state.apply_zoom(ViewAction::ZoomOut);
                    }
                    keyboard::Key::Character("0") => {
                        state.apply_zoom(ViewAction::ZoomReset);
                    }
                    // Other command combos (undo/redo, ...) are the active editor's to handle.
                    _ => {
                        state.handle_editor_key(&key, modifiers);
                    }
                }
            } else if state.quick_open.visible {
                // Single-line `text_input` captures Enter (via `on_submit`) and character keys
                // itself, so only the keys it doesn't bind reach this global handler: arrow
                // navigation and Escape-to-close.
                let msg = match key.as_ref() {
                    keyboard::Key::Named(keyboard::key::Named::Escape) => Some(quick_open::Message::Close),
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(quick_open::Message::MoveDown),
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(quick_open::Message::MoveUp),
                    _ => None,
                };
                if let Some(msg) = msg {
                    let root = state.root.clone();
                    let (t, opened) = quick_open::update(&mut state.quick_open, msg, root.as_deref());
                    task = t.map(Message::QuickOpen);
                    if let Some(path) = opened {
                        state.open_path(path);
                        state.focus = Focus::Editor;
                    }
                }
            } else if key.as_ref() == keyboard::Key::Named(keyboard::key::Named::Escape)
                && state.sidebar_mode == SidebarMode::ProjectSearch
            {
                // The global key subscription fires regardless of which widget has iced's own
                // focus (see `input::handle_key`'s docs on why this app doesn't rely on that
                // system), so this closes the panel even while typing in its search box.
                state.sidebar_mode = SidebarMode::Tree;
            } else {
                // No per-widget focus system here (see `input::handle_key`'s docs), so route
                // by `state.focus`: the tree when it's the last thing the user interacted
                // with, the active tab's editor otherwise.
                let tree_event = if state.focus == Focus::Tree {
                    state.tree.as_ref().and_then(|tree| tree.handle_key(&key, modifiers))
                } else {
                    None
                };

                if let Some(event) = tree_event {
                    if let DirectoryTreeEvent::Selected(path, is_dir, _) = &event {
                        if !*is_dir {
                            state.open_path(path.clone());
                        }
                    }
                    if let Some(tree) = &mut state.tree {
                        task = tree.update(event).map(Message::Tree);
                    }
                } else if state.focus == Focus::Editor {
                    state.handle_editor_key(&key, modifiers);
                }
            }
        }
        Message::Noop => {}

        Message::SidebarToggle => {
            state.toggle_sidebar_mode(SidebarMode::Tree);
        }

        Message::AiToggle => {
            state.ai_visible = !state.ai_visible;
            save_ai_visible(state.ai_visible);
            if state.ai_visible {
                let main_pane = state
                    .panes
                    .iter()
                    .find(|(_, kind)| **kind == PaneKind::Main)
                    .map(|(pane, _)| *pane);
                if let Some(main_pane) = main_pane {
                    if let Some((_, split)) =
                        state.panes.split(pane_grid::Axis::Vertical, main_pane, PaneKind::Ai)
                    {
                        state.panes.resize(split, load_ai_ratio());
                        state.ai_split = Some(split);
                    }
                }
            } else {
                let ai_pane = state
                    .panes
                    .iter()
                    .find(|(_, kind)| **kind == PaneKind::Ai)
                    .map(|(pane, _)| *pane);
                if let Some(ai_pane) = ai_pane {
                    state.panes.close(ai_pane);
                }
                state.ai_split = None;
            }
        }
        Message::AiModeSelected(mode) => state.ai_mode = mode,
        Message::Chat(msg) => {
            let cwd = state.root_or_cwd();
            task = chat::update(&mut state.chat, msg, cwd).map(Message::Chat);
        }
        Message::Acp(msg) => {
            let cwd = state.root_or_cwd();
            task = acp::update(&mut state.acp, msg, cwd).map(Message::Acp);
        }
        Message::Tick => {
            state.chat.poll();
            state.acp.poll();
            if state.acp.take_finished() {
                state.reload_open_tabs();
            }
            if state.chat.take_files_changed() {
                state.reload_open_tabs();
                if let Some(root) = state.root.clone() {
                    task = refresh_dir_task(root);
                }
            }
        }

        Message::WindowResized(id, size) => {
            state.last_known_size = size;
            task = iced::window::is_maximized(id).map(Message::WindowMaximizedChecked);
        }
        Message::WindowMoved(position) => save_window_position(position),
        Message::WindowMaximizedChecked(maximized) => {
            save_window_maximized(maximized);
            if !maximized {
                save_window_size(state.last_known_size);
            }
        }
    }
    task
}

fn view(state: &State) -> Element<'_, Message> {
    let top_bar = view_top_bar(state);
    let status_bar = view_status_bar(state);

    let panes = PaneGrid::new(&state.panes, |_id, kind, _is_maximized| {
        let content: Element<'_, Message> = match kind {
            PaneKind::Sidebar => container(view_sidebar(state))
                .padding(8)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|theme: &iced::Theme| iced::widget::container::Style {
                    background: Some(theme.extended_palette().background.weak.color.into()),
                    ..iced::widget::container::Style::default()
                })
                .into(),
            PaneKind::Main => view_editor(state),
            PaneKind::Ai => container(view_ai_sidebar(state))
                .height(Length::Fill)
                .padding(8)
                .into(),
        };
        pane_grid::Content::new(content)
    })
    .on_resize(10, Message::PaneResized)
    .height(Length::Fill);

    let base: Element<'_, Message> = column![top_bar, panes, status_bar].into();

    if state.quick_open.visible {
        iced::widget::stack![
            base,
            quick_open::view(&state.quick_open, state.root.as_deref()).map(Message::QuickOpen),
        ]
        .into()
    } else {
        base
    }
}

fn view_ai_sidebar(state: &State) -> Element<'_, Message> {
    let mode_tab = |label: &'static str, mode: AiMode| {
        let is_active = state.ai_mode == mode;
        let underline = container(Space::new().width(Length::Fill).height(2)).style(
            move |theme: &iced::Theme| iced::widget::container::Style {
                background: Some(
                    if is_active {
                        theme.extended_palette().primary.base.color
                    } else {
                        iced::Color::TRANSPARENT
                    }
                    .into(),
                ),
                ..iced::widget::container::Style::default()
            },
        );
        column![
            button(text(label).wrapping(iced::widget::text::Wrapping::None))
                .padding([4, 4])
                .width(Length::Shrink)
                .style(move |theme, status| tab_button_style(theme, status, is_active))
                .on_press(Message::AiModeSelected(mode)),
            underline,
        ]
        .spacing(2)
        .width(Length::Shrink)
    };

    let mode_row = row![mode_tab("HTTP", AiMode::Http), mode_tab("Claude Code", AiMode::Acp)]
        .spacing(2)
        .padding([4, 4]);

    let panel: Element<'_, Message> = match state.ai_mode {
        AiMode::Http => chat::view(&state.chat).map(Message::Chat),
        AiMode::Acp => acp::view(&state.acp, state.root_or_cwd()).map(Message::Acp),
    };

    column![mode_row, panel].height(Length::Fill).into()
}

/// Transparent-background button style, so a title + close button pair placed inside a
/// pill-shaped `container` (see `view_editor`'s tab strip) reads as one merged tab rather
/// than two separate button-shaped elements.
pub(crate) fn flat_button_style(theme: &iced::Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
    use iced::widget::button::{Status, Style};

    let palette = theme.extended_palette();
    let base = Style {
        text_color: palette.background.base.text,
        border: iced::Border::default().rounded(4.0),
        ..Style::default()
    };
    match status {
        Status::Active | Status::Disabled => base.with_background(iced::Color::TRANSPARENT),
        Status::Hovered => base.with_background(palette.background.strong.color),
        Status::Pressed => base.with_background(palette.primary.strong.color),
    }
}

/// Text-only tab title style: no background at rest (even when active -- the underline in
/// `view_editor` carries that signal), a subtle highlight on hover, dimmer text for inactive
/// tabs so the active one reads clearly without needing a button-like fill.
fn tab_button_style(
    theme: &iced::Theme,
    status: iced::widget::button::Status,
    is_active: bool,
) -> iced::widget::button::Style {
    use iced::widget::button::{Status, Style};

    let palette = theme.extended_palette();
    let text_color = if is_active {
        palette.background.base.text
    } else {
        iced::Color {
            a: palette.background.base.text.a * 0.6,
            ..palette.background.base.text
        }
    };
    let base = Style {
        text_color,
        border: iced::Border::default().rounded(4.0),
        ..Style::default()
    };
    match status {
        Status::Active | Status::Disabled => base.with_background(iced::Color::TRANSPARENT),
        Status::Hovered => base.with_background(palette.background.weak.color),
        Status::Pressed => base.with_background(palette.background.strong.color),
    }
}

fn menu_button<'a>(label: String, msg: Message) -> iced::widget::button::Button<'a, Message> {
    menu_button_maybe(label, Some(msg))
}

/// Like `menu_button`, but `None` leaves the button with no `on_press`, which iced renders
/// as disabled (greyed out, non-interactive) -- used for Undo/Redo when there's nothing to
/// undo/redo.
fn menu_button_maybe<'a>(label: String, msg: Option<Message>) -> iced::widget::button::Button<'a, Message> {
    button(text(label))
        .width(Length::Fill)
        .padding([4, 8])
        .style(|theme: &iced::Theme, status| {
            use iced::widget::button::{Status, Style};

            let palette = theme.extended_palette();
            let base = Style {
                text_color: palette.background.base.text,
                border: iced::Border::default().rounded(6.0),
                ..Style::default()
            };
            match status {
                Status::Active | Status::Disabled => base.with_background(iced::Color::TRANSPARENT),
                Status::Hovered => base.with_background(palette.primary.weak.color),
                Status::Pressed => base.with_background(palette.primary.strong.color),
            }
        })
        .on_press_maybe(msg)
}

fn view_top_bar(state: &State) -> Element<'_, Message> {
    let menu_tpl = |items| Menu::new(items).width(200.0).offset(4.0).spacing(2.0);

    let file_menu_button = menu_button("File".to_string(), Message::Noop).width(Length::Shrink);
    let file_items = menu_items!(
        (menu_button(
            "Open File (Cmd+O)".to_string(),
            Message::FileAction(FileAction::OpenFile)
        )),
        (menu_button(
            "Open Folder".to_string(),
            Message::FileAction(FileAction::OpenFolder)
        )),
        (menu_button(
            "Close Folder".to_string(),
            Message::FileAction(FileAction::CloseFolder)
        )),
        (menu_button(
            "Save (Cmd+S)".to_string(),
            Message::FileAction(FileAction::Save)
        )),
    );

    let active_content = state.tabs.get(state.active_tab).map(|t| &t.content);
    let can_undo = active_content.is_some_and(|c| c.can_undo());
    let can_redo = active_content.is_some_and(|c| c.can_redo());

    let has_active_tab = state.tabs.get(state.active_tab).is_some();
    let edit_menu_button = menu_button("Edit".to_string(), Message::Noop).width(Length::Shrink);
    let edit_items = menu_items!(
        (menu_button_maybe(
            EditAction::Undo.to_string(),
            can_undo.then_some(Message::EditAction(EditAction::Undo)),
        )),
        (menu_button_maybe(
            EditAction::Redo.to_string(),
            can_redo.then_some(Message::EditAction(EditAction::Redo)),
        )),
        (menu_button_maybe(
            EditAction::SelectAll.to_string(),
            has_active_tab.then_some(Message::EditAction(EditAction::SelectAll)),
        )),
        (menu_button_maybe(
            "Find (Cmd+F)".to_string(),
            has_active_tab.then_some(Message::Search(code_editor::search::Message::Toggle)),
        )),
        (menu_button(
            "Find in Project (Cmd+Shift+F)".to_string(),
            Message::ToggleProjectSearch
        )),
        (menu_button(
            "Quick Open (Cmd+P)".to_string(),
            Message::ToggleQuickOpen
        )),
    );

    let view_menu_button = menu_button("View".to_string(), Message::Noop).width(Length::Shrink);
    let view_items = menu_items!(
        (menu_button(
            ViewAction::ZoomIn.to_string(),
            Message::ViewAction(ViewAction::ZoomIn)
        )),
        (menu_button(
            ViewAction::ZoomOut.to_string(),
            Message::ViewAction(ViewAction::ZoomOut)
        )),
        (menu_button(
            ViewAction::ZoomReset.to_string(),
            Message::ViewAction(ViewAction::ZoomReset)
        )),
    );

    let theme_menu_button = menu_button("Theme".to_string(), Message::Noop).width(Length::Shrink);
    let theme_items: Vec<_> = iced::Theme::ALL
        .iter()
        .map(|t| Item::new(menu_button(t.to_string(), Message::AppThemeSelected(t.clone()))))
        .collect();

    let mb = menu_bar!(
        (file_menu_button, menu_tpl(file_items)),
        (edit_menu_button, menu_tpl(edit_items)),
        (view_menu_button, menu_tpl(view_items)),
        (theme_menu_button, menu_tpl(theme_items))
    )
    .close_on_background_click(true)
    .close_on_background_click_global(true);

    row![mb].padding(4).into()
}

fn view_status_bar(state: &State) -> Element<'_, Message> {
    let folder_icon: char = lucide_icons::Icon::Folder.into();
    let git_icon: char = lucide_icons::Icon::GitBranch.into();
    let find_icon: char = lucide_icons::Icon::Search.into();
    let mut bar = row![
        button(text(folder_icon).font(iced::Font::with_name("lucide")).size(12))
            .padding([2, 6])
            .on_press(Message::SidebarToggle),
        button(text(git_icon).font(iced::Font::with_name("lucide")).size(12))
            .padding([2, 6])
            .on_press(Message::GitPanelToggle),
        button(text(find_icon).font(iced::Font::with_name("lucide")).size(12))
            .padding([2, 6])
            .on_press(Message::ToggleProjectSearch),
        Space::new().width(Length::Fill),
    ]
    .spacing(12);

    if let Some(tab) = state.tabs.get(state.active_tab) {
        let (line, col) = tab.content.cursor_line_col();
        let language = state.highlighter.language_name(&tab.extension());
        bar = bar.push(text(format!("Ln {line}, Col {col}")));
        bar = bar.push(text(tab.line_ending.to_string()));
        bar = bar.push(text(language.to_string()));
    }

    let ai_icon: char = lucide_icons::Icon::Sparkles.into();
    bar.push(
        button(text(ai_icon).font(iced::Font::with_name("lucide")).size(12))
            .padding([2, 6])
            .on_press(Message::AiToggle),
    )
    .align_y(iced::Alignment::Center)
        .padding(4)
        .into()
}

fn view_sidebar(state: &State) -> Element<'_, Message> {
    match state.sidebar_mode {
        SidebarMode::Tree => view_tree(state),
        SidebarMode::ProjectSearch => {
            project_search::view(&state.project_search, state.root.as_deref()).map(Message::ProjectSearch)
        }
        SidebarMode::Git => git::view(&state.git).map(Message::Git),
    }
}

fn view_tree(state: &State) -> Element<'_, Message> {
    let Some(tree) = &state.tree else {
        return text("No folder open (File > Open Folder)").into();
    };

    let content = ContextMenu::new(tree.view(Message::Tree), || {
        container(
            column![
                menu_button("New File".to_string(), Message::ContextNewFile),
                menu_button("New Folder".to_string(), Message::ContextNewFolder),
                menu_button("Rename".to_string(), Message::ContextRename),
                menu_button("Delete".to_string(), Message::ContextDelete),
                menu_button("Copy Path".to_string(), Message::ContextCopyPath),
                menu_button("Copy Relative Path".to_string(), Message::ContextCopyRelativePath),
                menu_button(reveal_label().to_string(), Message::ContextReveal),
                menu_button("Refresh".to_string(), Message::RefreshTree),
            ]
            .width(Length::Fixed(200.0)),
        )
        .padding(4)
        .style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            iced::widget::container::Style {
                background: Some(palette.background.base.color.into()),
                border: iced::Border::default().rounded(6.0),
                ..iced::widget::container::Style::default()
            }
        })
        .into()
    });

    let mut col = column![Element::from(content)].spacing(6);

    if let Some((_, is_dir, buf)) = &state.creating {
        let hint = if *is_dir { "new folder name" } else { "new file name" };
        col = col.push(row![
            text_input(hint, buf)
                .on_input(Message::CreateInput)
                .on_submit(Message::CreateSubmit),
            button(text("x")).on_press(Message::CreateCancel),
        ]);
    }

    if let Some((_, buf)) = &state.renaming {
        col = col.push(row![
            text_input("new name", buf)
                .on_input(Message::RenameInput)
                .on_submit(Message::RenameSubmit),
            button(text("x")).on_press(Message::RenameCancel),
        ]);
    }

    col.into()
}

fn view_editor(state: &State) -> Element<'_, Message> {
    let mut tab_row = row![].spacing(2).padding([4, 4]);
    for (i, tab) in state.tabs.iter().enumerate() {
        let is_active = i == state.active_tab;
        let tab_title = row![
            button(text(tab.title()))
                .padding([4, 4])
                .style(move |theme, status| tab_button_style(theme, status, is_active))
                .on_press(Message::TabSelected(i)),
            button(text("x").size(12))
                .padding([2, 6])
                .style(flat_button_style)
                .on_press(Message::TabClosed(i)),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);

        // A thin underline (rather than a filled pill) marks the active tab, so tabs read
        // as flat text labels instead of buttons.
        let underline = container(Space::new().width(Length::Fill).height(2))
            .style(move |theme: &iced::Theme| iced::widget::container::Style {
                background: Some(
                    if is_active {
                        theme.extended_palette().primary.base.color
                    } else {
                        iced::Color::TRANSPARENT
                    }
                    .into(),
                ),
                ..iced::widget::container::Style::default()
            });

        tab_row = tab_row.push(column![tab_title, underline].spacing(2));
    }

    let mut editor_column = column![];

    if let Some(tab) = state.tabs.get(state.active_tab) {
        if tab.search.visible {
            editor_column = editor_column.push(code_editor::search::view(&tab.search).map(Message::Search));
        }
        editor_column = editor_column
            .push(code_editor::code_editor(
                &tab.content,
                &state.app_theme,
                state.zoom,
                Message::EditorAction,
            ));
    } else {
        editor_column = editor_column.push(text("No file open"));
    }

    let tab_strip = scrollable(tab_row)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default(),
        ))
        .width(Length::Fill);

    column![tab_strip, editor_column.height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let keys = keyboard::listen().map(|event| match event {
        keyboard::Event::KeyPressed {
            key, modifiers, ..
        } => Message::KeyPressed(key, modifiers),
        _ => Message::Noop,
    });
    let tick = iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick);
    let window_events = iced::window::events().map(|(id, event)| match event {
        iced::window::Event::Resized(size) => Message::WindowResized(id, size),
        iced::window::Event::Moved(position) => Message::WindowMoved(position),
        _ => Message::Noop,
    });
    Subscription::batch([keys, tick, window_events])
}

/// `HOME` is unset on native Windows launches outside Git Bash/pwsh7, so use
/// `USERPROFILE` there instead.
pub(crate) fn home_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

pub(crate) fn config_path(name: &str) -> Option<PathBuf> {
    Some(home_dir()?.join(".config").join("editor").join(name))
}

fn last_project_path() -> Option<PathBuf> {
    config_path("last_project")
}

fn save_last_project(root: &Path) {
    let Some(path) = last_project_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, root.to_string_lossy().as_bytes());
}

fn load_last_project() -> Option<PathBuf> {
    let text = std::fs::read_to_string(last_project_path()?).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

fn save_app_theme(theme: &iced::Theme) {
    let Some(path) = config_path("app_theme") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, theme.to_string());
}

fn load_app_theme() -> iced::Theme {
    config_path("app_theme")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| {
            let name = text.trim();
            iced::Theme::ALL.iter().find(|t| t.to_string() == name).cloned()
        })
        .unwrap_or(iced::Theme::Dark)
}

fn save_zoom(zoom: f32) {
    let Some(path) = config_path("zoom") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, zoom.to_string());
}

fn load_zoom() -> f32 {
    config_path("zoom")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.trim().parse::<f32>().ok())
        .map(|zoom| zoom.clamp(code_editor::ZOOM_MIN, code_editor::ZOOM_MAX))
        .unwrap_or(code_editor::ZOOM_DEFAULT)
}

fn save_sidebar_ratio(ratio: f32) {
    let Some(path) = config_path("sidebar_ratio") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, ratio.to_string());
}

fn load_sidebar_ratio() -> f32 {
    config_path("sidebar_ratio")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.trim().parse::<f32>().ok())
        .map(|ratio| ratio.clamp(0.05, 0.95))
        .unwrap_or(0.2)
}

fn save_ai_ratio(ratio: f32) {
    let Some(path) = config_path("ai_ratio") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, ratio.to_string());
}

/// Default is wider than the pane grid's original 0.75 (main:ai) -- the AI panel was too
/// cramped out of the box.
fn load_ai_ratio() -> f32 {
    config_path("ai_ratio")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.trim().parse::<f32>().ok())
        .map(|ratio| ratio.clamp(0.05, 0.95))
        .unwrap_or(0.65)
}

fn save_window_size(size: iced::Size) {
    let Some(path) = config_path("window_size") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{},{}", size.width, size.height));
}

fn load_window_size() -> iced::Size {
    config_path("window_size")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| {
            let (w, h) = text.trim().split_once(',')?;
            Some(iced::Size::new(w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or(iced::Size::new(1280.0, 800.0))
}

fn save_window_position(position: iced::Point) {
    let Some(path) = config_path("window_position") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{},{}", position.x, position.y));
}

fn load_window_position() -> iced::window::Position {
    config_path("window_position")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| {
            let (x, y) = text.trim().split_once(',')?;
            Some(iced::window::Position::Specific(iced::Point::new(
                x.parse().ok()?,
                y.parse().ok()?,
            )))
        })
        .unwrap_or_default()
}

fn save_window_maximized(maximized: bool) {
    let Some(path) = config_path("window_maximized") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if maximized { "true" } else { "false" });
}

fn load_window_maximized() -> bool {
    config_path("window_maximized")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim() == "true")
        .unwrap_or(false)
}

fn save_ai_visible(visible: bool) {
    let Some(path) = config_path("ai_visible") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if visible { "true" } else { "false" });
}

fn load_ai_visible() -> bool {
    config_path("ai_visible")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim() == "true")
        .unwrap_or(false)
}

fn save_sidebar_visible(visible: bool) {
    let Some(path) = config_path("sidebar_visible") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, if visible { "true" } else { "false" });
}

fn load_sidebar_visible() -> bool {
    config_path("sidebar_visible")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim() == "true")
        .unwrap_or(true)
}

/// Forces DirectoryTree to rescan `dir` without disturbing the rest of the tree's expand
/// state: a collapse+re-expand round trip is the widget's documented cache-invalidation
/// trick, and it leaves `dir`'s own displayed expand state unchanged either way.
fn refresh_dir_task(dir: PathBuf) -> Task<Message> {
    Task::batch([
        Task::done(Message::Tree(DirectoryTreeEvent::Toggled(dir.clone()))),
        Task::done(Message::Tree(DirectoryTreeEvent::Toggled(dir))),
    ])
}

fn confirm(title: &str, description: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(target_os = "windows") {
        "Show in Explorer"
    } else {
        "Open Containing Folder"
    }
}

fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        let _ = std::process::Command::new("explorer").arg(arg).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

pub fn main() -> iced::Result {
    let icon = iced::window::icon::from_file_data(include_bytes!("../icon.png"), None).ok();
    iced::application(State::new, update, view)
        .title("editor")
        .theme(|state: &State| state.app_theme.clone())
        .subscription(subscription)
        .font(iced_swdir_tree::LUCIDE_FONT_BYTES)
        .window(iced::window::Settings {
            icon,
            size: load_window_size(),
            position: load_window_position(),
            maximized: load_window_maximized(),
            ..Default::default()
        })
        .run()
}
