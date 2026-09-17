mod ai_context;
mod acp;
mod terminal;
mod chat;
mod code_editor;
mod command_palette;
mod git;
mod file_icons;
mod git_diff;
mod git_preview;
mod goto_line;
mod project_search;
mod quick_open;
mod recent_files;
mod theme;
mod theme_install;

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
    /// Diff gutter markers vs. git HEAD, keyed by 0-indexed line number. Loaded
    /// asynchronously (see `Message::GitDiffLoaded`) whenever the tab is opened or saved, so
    /// it reflects on-disk content, same as the git sidebar panel.
    diff: std::collections::HashMap<usize, git_diff::LineStatus>,
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
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl std::fmt::Display for EditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            EditAction::Undo => "Undo (Cmd+Z)",
            EditAction::Redo => "Redo (Cmd+Shift+Z)",
            EditAction::Cut => "Cut (Cmd+X)",
            EditAction::Copy => "Copy (Cmd+C)",
            EditAction::Paste => "Paste (Cmd+V)",
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
    Codex,
}

impl std::fmt::Display for AiMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { Self::Http => "Local / Ollama / API", Self::Acp => "Claude ACP", Self::Codex => "Codex ACP" })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PaneKind {
    Terminal,
    Sidebar,
    Main,
    Ai,
}

#[derive(Debug, Clone, Copy)]
enum TabCloseScope { Others, Left, Right, All }

fn tabs_to_close(count: usize, anchor: usize, scope: TabCloseScope) -> Vec<usize> {
    if anchor >= count { return Vec::new(); }
    (0..count).rev().filter(|&index| match scope {
        TabCloseScope::Others => index != anchor,
        TabCloseScope::Left => index < anchor,
        TabCloseScope::Right => index > anchor,
        TabCloseScope::All => true,
    }).collect()
}

#[derive(Debug, Clone)]
enum Message {
    TerminalToggle,
    Terminal(terminal::panel::Message),
    EditorAction(cosmic_text::Action),
    ToggleFold(usize),
    Search(code_editor::search::Message),
    ProjectSearch(project_search::Message),
    ToggleProjectSearch,
    QuickOpen(quick_open::Message),
    ToggleQuickOpen,
    ThemeInstall(theme_install::Message),
    CommandPalette(command_palette::Message),
    ToggleCommandPalette,
    GotoLine(goto_line::Message),
    ToggleGotoLine,
    Git(git::Message),
    GitPanelToggle,
    GitPreviewLoaded(PathBuf, String, Result<String, String>),
    CloseGitPreview,
    GitDiffLoaded(PathBuf, Vec<(usize, git_diff::LineStatus)>),
    TabSelected(usize),
    TabClosed(usize),
    CloseTabs(usize, TabCloseScope),
    /// "Yes" answers from `ask` dialogs.
    CloseTabConfirmed(usize),
    CloseTabsConfirmed(usize, TabCloseScope),
    DeleteConfirmed(PathBuf),
    ReopenClosedTab,
    RevealPath(PathBuf),
    RevealStep(u64, DirectoryTreeEvent),
    DismissNotice,
    Exit,

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
    /// Clipboard contents read for `EditAction::Paste`.
    EditorPasted(Option<String>),
    ViewAction(ViewAction),
    /// A theme name from `State::themes`.
    AppThemeSelected(String),
    PaneResized(pane_grid::ResizeEvent),

    KeyPressed(keyboard::Key, keyboard::Modifiers),
    Noop,

    SidebarToggle,
    SidebarSelected(SidebarMode),
    AiToggle,
    AiModeSelected(AiMode),
    AiAttach,
    AiFiles(Vec<PathBuf>),
    AiAttachmentsLoaded(AiMode, PathBuf, Vec<Result<ai_context::Attachment, String>>),
    AiReference,
    AiRemoveAttachment(usize),
    Codex(acp::Message),
    Chat(chat::Message),
    Acp(acp::Message),
    Tick,

    WindowResized(iced::window::Id, iced::Size),
    WindowFrameDrawn(iced::window::Id),
    WindowOpened(iced::window::Id),
    PaintInitialWindow(iced::window::Id, u64),
    WindowMoved(iced::Point),
    WindowMaximizedChecked(bool),
}

#[derive(PartialEq, serde::Serialize, serde::Deserialize)]
struct RecoveryBuffer {
    path: Option<PathBuf>,
    text: String,
}

#[derive(Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct EditorSession {
    root: Option<PathBuf>,
    tabs: Vec<PathBuf>,
    active: Option<PathBuf>,
    #[serde(default)]
    recovery: Vec<RecoveryBuffer>,
}

struct State {
    root: Option<PathBuf>,
    saved_session: EditorSession,
    last_session_write: std::time::Instant,
    closed_tabs: Vec<PathBuf>,
    notice: Option<(String, std::time::Instant)>,
    reveal_generation: u64,
    reveal_queue: std::collections::VecDeque<PathBuf>,
    reveal_target: Option<PathBuf>,
    tabs: Vec<Tab>,
    active_tab: usize,
    focus: Focus,

    tree: Option<DirectoryTree>,
    renaming: Option<(PathBuf, String)>,
    creating: Option<(PathBuf, bool, String)>,
    sidebar_mode: SidebarMode,
    project_search: project_search::SearchState,
    quick_open: quick_open::QuickOpenState,
    theme_install: theme_install::ThemeInstallState,
    command_palette: command_palette::PaletteState,
    goto_line: goto_line::GotoLineState,
    recent_files: Vec<PathBuf>,
    git: git::GitState,
    git_preview: Option<git_preview::Preview>,

    app_theme: theme::EditorTheme,
    themes: theme::ThemeRegistry,
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
    window_revealed: bool,
    startup_maximized: bool,
    startup_window: Option<u64>,
    /// The main window's native handle (HWND on Windows), used to parent native dialogs so
    /// they open in front of the app.
    window_handle: Option<u64>,
    started_at: std::time::Instant,

    ai_visible: bool,
    ai_mode: AiMode,
    chat: chat::ChatState,
    acp: acp::AcpState,
    codex: acp::AcpState,
    terminal: terminal::panel::Panel,
    terminal_visible: bool,
}

impl State {
    fn new() -> Self {
        let root = load_last_project();
        let tree = root
            .clone()
            .map(|p| DirectoryTree::new(p).with_filter(DirectoryFilter::FilesAndFolders).with_icon_theme(std::sync::Arc::new(file_icons::FileIcons)));
        let sidebar_visible = load_sidebar_visible();
        let ai_visible = load_ai_visible();
        let themes = theme::ThemeRegistry::load();
        let app_theme = load_app_theme(&themes);
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
            saved_session: EditorSession::default(),
            last_session_write: std::time::Instant::now() - Duration::from_secs(1),
            closed_tabs: Vec::new(),
            notice: None,
            reveal_generation: 0,
            reveal_queue: std::collections::VecDeque::new(),
            reveal_target: None,
            tabs: Vec::new(),
            active_tab: 0,
            focus: Focus::Editor,
            tree,
            renaming: None,
            creating: None,
            sidebar_mode: SidebarMode::default(),
            project_search: project_search::SearchState::default(),
            quick_open: quick_open::QuickOpenState::default(),
            theme_install: theme_install::ThemeInstallState::default(),
            command_palette: command_palette::PaletteState::default(),
            goto_line: goto_line::GotoLineState::default(),
            recent_files: recent_files::load(),
            git: git::GitState::default(),
            git_preview: None,
            app_theme,
            themes,
            highlighter: code_editor::Highlighter::new(),
            zoom,
            panes,
            sidebar_split,
            ai_split,
            sidebar_visible,
            last_known_size: load_window_size(),
            window_revealed: !cfg!(windows),
            startup_maximized: load_window_maximized(),
            startup_window: None,
            window_handle: None,
            started_at: std::time::Instant::now(),
            ai_visible,
            ai_mode: AiMode::default(),
            chat: chat::ChatState::default(),
            acp: acp::AcpState::default(),
            codex: acp::AcpState::codex(),
            terminal: terminal::panel::Panel::default(),
            terminal_visible: false,
        }
    }

    fn boot() -> (Self, Task<Message>) {
        let mut state = Self::new();
        let session: EditorSession = config_path("session.json")
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut tasks = Vec::new();
        if let (Some(tree), Some(root)) = (&mut state.tree, &state.root) {
            tasks.push(tree.update(DirectoryTreeEvent::Toggled(root.clone())).map(Message::Tree));
        }
        if session.root == state.root {
            for path in &session.tabs {
                tasks.push(state.open_path(path.clone()));
            }
            for recovery in session.recovery {
                let mut content = code_editor::Buffer::new(&recovery.text, code_editor::metrics_for_zoom(state.zoom));
                let extension = recovery.path.as_ref().and_then(|path| path.extension()).and_then(|ext| ext.to_str()).unwrap_or("txt");
                content.highlight(&state.highlighter, extension, &state.app_theme.syntax);
                let tab = Tab {
                    path: recovery.path.clone(), content, dirty: true,
                    search: Default::default(), line_ending: LineEnding::detect(&recovery.text), diff: Default::default(),
                };
                if let Some(index) = state.tabs.iter().position(|tab| tab.path == recovery.path) {
                    state.tabs[index] = tab;
                } else { state.tabs.push(tab); }
                state.notify("Recovered unsaved edits");
            }
            state.tabs.sort_by_key(|tab| session.tabs.iter().position(|path| tab.path.as_ref() == Some(path)).unwrap_or(usize::MAX));
            if let Some(index) = state.tabs.iter().position(|tab| tab.path == session.active) {
                state.active_tab = index;
            }
        }
        state.persist_session();
        (state, Task::batch(tasks))
    }

    fn notify(&mut self, message: impl Into<String>) {
        self.notice = Some((message.into(), std::time::Instant::now()));
    }

    fn persist_session(&mut self) {
        if self.last_session_write.elapsed() < Duration::from_millis(500) { return; }
        self.last_session_write = std::time::Instant::now();
        let session = EditorSession {
            root: self.root.clone(),
            tabs: self.tabs.iter().filter_map(|tab| tab.path.clone()).collect(),
            active: self.tabs.get(self.active_tab).and_then(|tab| tab.path.clone()),
            recovery: self.tabs.iter().filter(|tab| tab.dirty).map(|tab| RecoveryBuffer {
                path: tab.path.clone(), text: tab.content.text(),
            }).collect(),
        };
        if session == self.saved_session { return; }
        if let Some(path) = config_path("session.json") {
            if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
            if let Ok(bytes) = serde_json::to_vec(&session) {
                match write_session(&path, &bytes) {
                    Ok(()) => self.saved_session = session,
                    Err(err) => self.notify(format!("Could not save recovery: {err}")),
                }
            }
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
    fn reload_open_tabs(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        for tab in &mut self.tabs {
            if tab.dirty {
                continue;
            }
            let Some(path) = tab.path.clone() else { continue };
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text != tab.content.text() {
                    let extension = tab.extension();
                    tab.content =
                        code_editor::Buffer::new(&text, code_editor::metrics_for_zoom(self.zoom));
                    tab.content.highlight(&self.highlighter, &extension, &self.app_theme.syntax);
                    tasks.push(load_diff_task(self.root.clone(), path));
                }
            }
        }
        Task::batch(tasks)
    }

    fn open_path(&mut self, path: PathBuf) -> Task<Message> {
        self.git_preview = None;
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path.as_path()))
        {
            self.active_tab = index;
            recent_files::record(&mut self.recent_files, path.clone());
            return load_diff_task(self.root.clone(), path);
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => { self.notify(format!("Could not open {}: {err}", path.display())); return Task::none(); }
        };
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_string();
        let line_ending = LineEnding::detect(&text);
        let mut content = code_editor::Buffer::new(&text, code_editor::metrics_for_zoom(self.zoom));
        content.highlight(&self.highlighter, &extension, &self.app_theme.syntax);
        recent_files::record(&mut self.recent_files, path.clone());
        let task = load_diff_task(self.root.clone(), path.clone());
        self.tabs.push(Tab {
            path: Some(path),
            content,
            search: code_editor::search::SearchState::default(),
            dirty: false,
            line_ending,
            diff: std::collections::HashMap::new(),
        });
        self.active_tab = self.tabs.len() - 1;
        task
    }

    fn close_tab(&mut self, index: usize) -> Task<Message> {
        if index >= self.tabs.len() {
            return Task::none();
        }
        if self.tabs[index].dirty {
            return ask(self.window_handle, "Unsaved changes", "Discard unsaved changes?".into(), Message::CloseTabConfirmed(index));
        }
        self.remove_tab(index);
        Task::none()
    }

    fn close_tabs(&mut self, anchor: usize, scope: TabCloseScope) -> Task<Message> {
        let indices = tabs_to_close(self.tabs.len(), anchor, scope);
        let unsaved = indices.iter().filter(|&&index| self.tabs[index].dirty).count();
        if unsaved > 0 {
            return ask(self.window_handle, "Unsaved changes", format!("Discard unsaved changes in {unsaved} tab(s) and close the selected tabs?"), Message::CloseTabsConfirmed(anchor, scope));
        }
        self.remove_tabs(anchor, scope);
        Task::none()
    }

    fn remove_tabs(&mut self, anchor: usize, scope: TabCloseScope) {
        // Descending indices keep the remaining targets and active tab stable.
        for index in tabs_to_close(self.tabs.len(), anchor, scope) { self.remove_tab(index); }
    }

    fn remove_tab(&mut self, index: usize) {
        if let Some(path) = self.tabs[index].path.clone() { self.closed_tabs.push(path); }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab > index || self.active_tab >= self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1).min(self.tabs.len() - 1);
        }
    }

    fn save(&mut self) -> Task<Message> {
        if self.git_preview.is_some() { return Task::none(); }
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Task::none();
        };
        let path = match &tab.path {
            Some(p) => Some(p.clone()),
            None => rfd::FileDialog::new().save_file(),
        };
        if let Some(path) = path {
            let result = std::fs::write(&path, tab.content.text());
            if result.is_ok() {
                tab.path = Some(path.clone());
                tab.dirty = false;
                self.notify(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()));
                return load_diff_task(self.root.clone(), path);
            }
            if let Err(err) = result { self.notify(format!("Save failed: {err}")); }
        }
        Task::none()
    }

    /// Routes a key press to the active tab's editor. Returns whether it was handled.
    fn handle_editor_key(&mut self, key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
        if self.git_preview.is_some() { return false; }
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
        tab.content.highlight(&self.highlighter, &extension, &self.app_theme.syntax);
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

    fn delete_path(&self, path: PathBuf) -> Task<Message> {
        let description = format!("Delete \"{}\"? This cannot be undone.", path.display());
        ask(self.window_handle, "Delete", description, Message::DeleteConfirmed(path))
    }

    fn delete_confirmed(&mut self, path: &Path) -> Task<Message> {
        let mut task = Task::none();
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if result.is_ok() {
            self.notify("Deleted successfully");
            if let Some(index) = self.tabs.iter().position(|t| t.path.as_deref() == Some(path)) {
                task = self.close_tab(index);
            }
        } else if let Err(err) = result { self.notify(format!("Delete failed: {err}")); }
        if let Some(parent) = path.parent() {
            task = Task::batch([task, refresh_dir_task(parent.to_path_buf())]);
        }
        task
    }
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    if matches!(&message, Message::TabClosed(_) | Message::CloseTabs(_, _) | Message::CloseTabConfirmed(_) | Message::CloseTabsConfirmed(_, _) | Message::TabSelected(_) | Message::FileAction(_) | Message::ReopenClosedTab)
        || matches!(&message, Message::KeyPressed(_, modifiers) if modifiers.command()) {
        state.last_session_write = std::time::Instant::now() - Duration::from_secs(1);
    }
    let mut task = Task::none();
    if state.git_preview.is_some() && matches!(&message,
        Message::EditorAction(_) | Message::EditAction(_) | Message::Search(_)
        | Message::FileAction(FileAction::Save)) {
        return Task::none();
    }
    match message {
        Message::TerminalToggle => {
            state.terminal_visible = !state.terminal_visible;
            if state.terminal_visible {
                state.terminal.ensure_started(&state.root_or_cwd());
                state.terminal.set_focused(true);
                task = iced::widget::operation::focus("terminal-canvas");
                let main = state.panes.iter().find(|(_, kind)| **kind == PaneKind::Main).map(|(pane, _)| *pane);
                if let Some(main) = main {
                    if let Some((_, split)) = state.panes.split(pane_grid::Axis::Horizontal, main, PaneKind::Terminal) {
                        state.panes.resize(split, 0.7);
                    }
                }

            } else {
                state.terminal.set_focused(false);
                let pane = state.panes.iter().find(|(_, kind)| **kind == PaneKind::Terminal).map(|(pane, _)| *pane);
                if let Some(pane) = pane { state.panes.close(pane); }
            }
        }
        Message::Terminal(terminal::panel::Message::Hide) => return update(state, Message::TerminalToggle),
        Message::Terminal(message) => task = state.terminal.update(message, &state.root_or_cwd()).map(Message::Terminal),
        Message::ToggleFold(line) => {
            if let Some(tab) = state.tabs.get_mut(state.active_tab) { tab.content.toggle_fold(line); }
        }
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
            let (search_task, opened) = project_search::update(&mut state.project_search, msg, root.as_deref());
            task = search_task.map(Message::ProjectSearch);
            if let Some((path, line)) = opened {
                task = Task::batch([task, state.open_path(path)]);
                state.focus = Focus::Editor;
                if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                    tab.content.goto_line(line.saturating_sub(1));
                }
            }
        }
        Message::ToggleProjectSearch => {
            state.toggle_sidebar_mode(SidebarMode::ProjectSearch);
        }
        Message::QuickOpen(msg) => {
            let root = state.root.clone();
            let (t, opened) = quick_open::update(&mut state.quick_open, msg, root.as_deref(), &state.recent_files);
            task = t.map(Message::QuickOpen);
            if let Some(path) = opened {
                task = Task::batch([task, state.open_path(path)]);
                state.focus = Focus::Editor;
            }
        }
        Message::ToggleQuickOpen => {
            let root = state.root.clone();
            let msg = if state.quick_open.visible {
                quick_open::Message::Close
            } else {
                let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
                state.theme_install.visible = false;
                quick_open::Message::Open
            };
            let (t, _) = quick_open::update(&mut state.quick_open, msg, root.as_deref(), &state.recent_files);
            task = t.map(Message::QuickOpen);
        }
        Message::CommandPalette(msg) => {
            let (t, picked) = command_palette::update(&mut state.command_palette, msg);
            task = t.map(Message::CommandPalette);
            if let Some(picked) = picked {
                task = Task::batch([task, update(state, picked)]);
            }
        }
        Message::ToggleCommandPalette => {
            if state.command_palette.visible {
                let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
            } else {
                state.quick_open.visible = false;
                state.theme_install.visible = false;
                let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
                let commands = command_list(state);
                task = command_palette::open(&mut state.command_palette, commands).map(Message::CommandPalette);
            }
        }
        Message::GotoLine(msg) => {
            if let Some(line) = goto_line::update(&mut state.goto_line, msg) {
                if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                    tab.content.goto_line(line.saturating_sub(1));
                }
                state.focus = Focus::Editor;
            }
        }
        Message::ToggleGotoLine => {
            if state.goto_line.visible {
                let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
            } else if state.tabs.get(state.active_tab).is_some() {
                state.quick_open.visible = false;
                state.theme_install.visible = false;
                let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                task = goto_line::open(&mut state.goto_line).map(Message::GotoLine);
            }
        }
        Message::ThemeInstall(msg) => {
            if matches!(msg, theme_install::Message::Open(_)) {
                state.quick_open.visible = false;
                let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
            }
            let (t, event) = theme_install::update(&mut state.theme_install, msg);
            task = t.map(Message::ThemeInstall);
            let next_theme = match event {
                Some(theme_install::Event::Installed(names)) => {
                    state.themes = theme::ThemeRegistry::load();
                    state.notify(format!("Installed {}", names.join(", ")));
                    names.into_iter().next()
                }
                Some(theme_install::Event::Uninstalled(name)) => {
                    state.themes = theme::ThemeRegistry::load();
                    state.notify(format!("Uninstalled {name}"));
                    // Reload the current theme (a bundled copy may remain) or fall back.
                    let current = &state.app_theme.name;
                    Some(if state.themes.names().any(|name| name == current) {
                        current.clone()
                    } else {
                        match state.app_theme.kind {
                            theme::Kind::Dark => theme::DEFAULT_DARK.to_string(),
                            theme::Kind::Light => theme::DEFAULT_LIGHT.to_string(),
                        }
                    })
                }
                None => None,
            };
            if let Some(name) = next_theme {
                task = Task::batch([task, update(state, Message::AppThemeSelected(name))]);
            }
        }
        Message::Git(git::Message::OpenDiff(path)) => {
            let root = state.root_or_cwd();
            state.focus = Focus::Editor;
            state.git_preview = Some(git_preview::Preview {
                root: root.clone(), path: path.clone(), result: None,
            });
            task = Task::perform(git_preview::load(root.clone(), path.clone()), move |result| {
                Message::GitPreviewLoaded(root.clone(), path.clone(), result)
            });
        }
        Message::GitPreviewLoaded(root, path, result) => {
            if let Some(preview) = &mut state.git_preview {
                if preview.root == root && preview.path == path {
                    preview.result = Some(result);
                }
            }
        }
        Message::CloseGitPreview => state.git_preview = None,
        Message::Git(msg) => {
            match &msg {
                git::Message::Committed(Ok(())) => state.notify("Commit created"),
                git::Message::Staged(Ok(())) => state.notify("Staging updated"),
                git::Message::Committed(Err(err)) | git::Message::Staged(Err(err)) | git::Message::Refreshed(Err(err)) => state.notify(format!("Git: {err}")),
                _ => {},
            }
            let cwd = state.root_or_cwd();
            let is_refresh = matches!(msg, git::Message::Refresh | git::Message::Staged(Ok(())) | git::Message::Committed(Ok(())));
            if is_refresh {
                if let Some(preview) = &mut state.git_preview {
                    let root = preview.root.clone();
                    let path = preview.path.clone();
                    preview.result = None;
                    task = Task::perform(git_preview::load(root.clone(), path.clone()), move |result| Message::GitPreviewLoaded(root.clone(), path.clone(), result));
                }
            }
            task = Task::batch([task, git::update(&mut state.git, msg, cwd).map(Message::Git)]);
            if is_refresh {
                if let Some(path) = state.tabs.get(state.active_tab).and_then(|t| t.path.clone()) {
                    task = Task::batch([task, load_diff_task(state.root.clone(), path)]);
                }
            }
        }
        Message::GitPanelToggle => {
            if state.toggle_sidebar_mode(SidebarMode::Git) {
                let cwd = state.root_or_cwd();
                task = git::update(&mut state.git, git::Message::Refresh, cwd).map(Message::Git);
                if let Some(path) = state.tabs.get(state.active_tab).and_then(|t| t.path.clone()) {
                    task = Task::batch([task, load_diff_task(state.root.clone(), path)]);
                }
            }
        }
        Message::GitDiffLoaded(path, diff) => {
            if let Some(tab) = state.tabs.iter_mut().find(|t| t.path.as_deref() == Some(path.as_path())) {
                tab.diff = diff.into_iter().collect();
            }
        }
        Message::TabSelected(i) => { state.git_preview = None; state.active_tab = i; },
        Message::TabClosed(i) => task = state.close_tab(i),
        Message::CloseTabs(anchor, scope) => task = state.close_tabs(anchor, scope),
        Message::CloseTabConfirmed(i) => {
            if i < state.tabs.len() { state.remove_tab(i); }
        }
        Message::CloseTabsConfirmed(anchor, scope) => state.remove_tabs(anchor, scope),
        Message::DeleteConfirmed(path) => task = state.delete_confirmed(&path),
        Message::DismissNotice => state.notice = None,
        Message::Exit => {
            state.last_session_write = std::time::Instant::now() - Duration::from_secs(1);
            state.persist_session();
            let recovery: Vec<_> = state.tabs.iter().filter(|tab| tab.dirty).map(|tab| RecoveryBuffer {
                path: tab.path.clone(), text: tab.content.text(),
            }).collect();
            if recovery != state.saved_session.recovery {
                state.notify("Could not save recovery. Save your files before closing.");
                return Task::none();
            }
            state.terminal.shutdown();
            return iced::exit();
        }
        Message::ReopenClosedTab => {
            if let Some(path) = state.closed_tabs.pop() { task = state.open_path(path); }
        }
        Message::RevealPath(path) => {
            if let Some(root) = state.root.clone().filter(|root| path.starts_with(root)) {
                state.sidebar_mode = SidebarMode::Tree;
                state.show_sidebar();
                let mut dirs: Vec<_> = path.ancestors().skip(1).take_while(|parent| parent.starts_with(&root)).map(Path::to_path_buf).collect();
                dirs.reverse();
                if path.is_dir() { dirs.push(path.clone()); }
                dirs.dedup();
                state.reveal_queue = dirs.into();
                state.reveal_target = Some(path);
                state.reveal_generation += 1;
                let generation = state.reveal_generation;
                let mut tree = DirectoryTree::new(root).with_filter(DirectoryFilter::FilesAndFolders).with_icon_theme(std::sync::Arc::new(file_icons::FileIcons));
                if let Some(dir) = state.reveal_queue.pop_front() {
                    task = tree.update(DirectoryTreeEvent::Toggled(dir)).map(move |event| Message::RevealStep(generation, event));
                }
                state.tree = Some(tree);
            } else { state.notify("This file is outside the open project"); }
        }
        Message::RevealStep(generation, event) => {
            if generation == state.reveal_generation {
                if let Some(tree) = &mut state.tree {
                    let loaded = matches!(&event, DirectoryTreeEvent::Loaded(_));
                    task = tree.update(event).map(move |event| Message::RevealStep(generation, event));
                    if loaded {
                        if let Some(dir) = state.reveal_queue.pop_front() {
                            task = Task::batch([task, tree.update(DirectoryTreeEvent::Toggled(dir)).map(move |event| Message::RevealStep(generation, event))]);
                        } else if let Some(path) = state.reveal_target.take() {
                            task = Task::batch([task, tree.update(DirectoryTreeEvent::Selected(path.clone(), path.is_dir(), iced_swdir_tree::SelectionMode::Replace)).map(Message::Tree)]);
                        }
                    }
                }
            }
        }

        Message::FileAction(action) => match action {
            FileAction::OpenFile => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    task = state.open_path(path);
                    state.focus = Focus::Editor;
                }
            }
            FileAction::OpenFolder => {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    save_last_project(&path);
                    state.tree = Some(
                        DirectoryTree::new(path.clone()).with_filter(DirectoryFilter::FilesAndFolders).with_icon_theme(std::sync::Arc::new(file_icons::FileIcons)),
                    );
                    state.git_preview = None;
                    if let Some(tree) = &mut state.tree {
                        task = tree.update(DirectoryTreeEvent::Toggled(path.clone())).map(Message::Tree);
                    }
                    state.root = Some(path);
                }
            }
            FileAction::CloseFolder => {
                state.git_preview = None;
                state.root = None;
                state.tree = None;
                if let Some(path) = last_project_path() {
                    let _ = std::fs::remove_file(path);
                }
            }
            FileAction::Save => task = state.save(),
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
                    EditAction::Cut => {
                        if let Some(text) = tab.content.cut_selection() {
                            task = iced::clipboard::write(text);
                        }
                    }
                    EditAction::Copy => {
                        if let Some(text) = tab.content.copy_selection() {
                            task = iced::clipboard::write(text);
                        }
                    }
                    EditAction::Paste => task = iced::clipboard::read().map(Message::EditorPasted),
                    EditAction::SelectAll => tab.content.select_all(),
                }
            }
            state.mark_edited_if_changed(before);
        }

        Message::EditorPasted(text) => {
            if let (Some(text), None) = (text, &state.git_preview) {
                let before = state
                    .tabs
                    .get(state.active_tab)
                    .map(|t| t.content.undo_count())
                    .unwrap_or(0);
                if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                    // Clipboard text from other Windows apps uses CRLF; the buffer uses LF.
                    tab.content.replace_selection(&text.replace("\r\n", "\n"));
                }
                state.mark_edited_if_changed(before);
            }
        }

        Message::ViewAction(action) => state.apply_zoom(action),

        Message::Tree(event) => {
            if state.reveal_target.is_some() && matches!(&event, DirectoryTreeEvent::Loaded(_)) { return Task::none(); }
            if !matches!(&event, DirectoryTreeEvent::Loaded(_)) {
                state.reveal_generation += 1;
                state.reveal_target = None;
                state.reveal_queue.clear();
            }
            state.focus = Focus::Tree;
            let mut open_task = Task::none();
            if let DirectoryTreeEvent::Selected(path, is_dir, _) = &event {
                if !*is_dir {
                    open_task = state.open_path(path.clone());
                }
            }
            if let Some(tree) = &mut state.tree {
                task = Task::batch([open_task, tree.update(event).map(Message::Tree)]);
            } else {
                task = open_task;
            }
        }
        Message::RefreshTree => {
            if let Some(root) = state.root.clone() {
                task = refresh_dir_task(root);
            }
        }
        Message::ContextNewFile => {
            if let Some(dir) = state.context_target_dir() {
                state.renaming = None;
                let expand = state.tree.as_mut().map(|tree| tree.update(DirectoryTreeEvent::Expand(dir.clone())).map(Message::Tree)).unwrap_or_else(Task::none);
                state.creating = Some((dir, false, String::new()));
                task = Task::batch([expand, iced::widget::operation::focus("tree-name")]);
            }
        }
        Message::ContextNewFolder => {
            if let Some(dir) = state.context_target_dir() {
                state.renaming = None;
                let expand = state.tree.as_mut().map(|tree| tree.update(DirectoryTreeEvent::Expand(dir.clone())).map(Message::Tree)).unwrap_or_else(Task::none);
                state.creating = Some((dir, true, String::new()));
                task = Task::batch([expand, iced::widget::operation::focus("tree-name")]);
            }
        }
        Message::ContextRename => {
            if let Some(path) = state.context_target_path() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                state.creating = None;
                state.renaming = Some((path, name));
                task = iced::widget::operation::focus("tree-name");
            }
        }
        Message::ContextDelete => {
            if let Some(path) = state.context_target_path() {
                task = state.delete_path(path);
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
            if let Some((old_path, new_name)) = state.renaming.clone() {
                if !new_name.is_empty() {
                    if !valid_entry_name(&new_name) {
                        state.notify("Enter a name without path separators");
                        return iced::widget::operation::focus("tree-name");
                    }
                    let new_path = old_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(&new_name);
                    if new_path == old_path { state.renaming = None; return Task::none(); }
                    if std::fs::symlink_metadata(&new_path).is_ok() {
                        state.notify("That name already exists");
                        return iced::widget::operation::focus("tree-name");
                    }
                    let renamed = std::fs::rename(&old_path, &new_path);
                    if renamed.is_ok() {
                        state.renaming = None;
                        state.notify("Renamed successfully");
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
                    } else if let Err(err) = renamed { state.notify(format!("Rename failed: {err}")); }
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
            if let Some((dir, is_dir, name)) = state.creating.clone() {
                if !name.is_empty() {
                    if !valid_entry_name(&name) {
                        state.notify("Enter a file or folder name without path separators");
                        return iced::widget::operation::focus("tree-name");
                    }
                    let target = dir.join(&name);
                    let result = if is_dir {
                        std::fs::create_dir(target)
                    } else {
                        std::fs::OpenOptions::new().write(true).create_new(true).open(target).map(|_| ())
                    };
                    if result.is_ok() {
                        state.creating = None;
                        state.notify("Created successfully");
                        task = refresh_dir_task(dir);
                    } else if let Err(err) = result { state.notify(format!("Create failed: {err}")); }
                }
            }
        }
        Message::CreateCancel => state.creating = None,

        Message::AppThemeSelected(name) => match state.themes.load_theme(&name) {
            Ok(theme) => {
                save_app_theme(&theme.name);
                state.app_theme = theme;
                for tab in &mut state.tabs {
                    let extension = tab.extension();
                    tab.content.highlight(&state.highlighter, &extension, &state.app_theme.syntax);
                }
            }
            Err(err) => state.notify(format!("Theme failed to load: {err}")),
        },
        Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
            state.panes.resize(split, ratio);
            if Some(split) == state.sidebar_split {
                save_sidebar_ratio(ratio);
            } else if Some(split) == state.ai_split {
                save_ai_ratio(ratio);
            }
        }

        Message::KeyPressed(key, modifiers) => {
            if modifiers.control() && key.as_ref() == keyboard::Key::Character("`") {
                return update(state, Message::TerminalToggle);
            }
            if state.terminal_visible && state.terminal.focused()
                && !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible
                && !state.theme_install.visible
                && key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                return Task::none();
            }
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) && (state.creating.is_some() || state.renaming.is_some()) {
                state.creating = None;
                state.renaming = None;
                return Task::none();
            }
            if modifiers.command() {
                match key.as_ref() {
                    keyboard::Key::Character(c) if modifiers.shift() && c.eq_ignore_ascii_case("t") => {
                        task = update(state, Message::ReopenClosedTab);
                    }
                    keyboard::Key::Character("q") => return update(state, Message::Exit),
                    keyboard::Key::Character("w") => {
                        if state.git_preview.is_some() { state.git_preview = None; }
                        else { task = state.close_tab(state.active_tab); }
                    }
                    keyboard::Key::Character(c) if c.len() == 1 && matches!(c.as_bytes()[0], b'1'..=b'9') => {
                        let index = (c.as_bytes()[0] - b'1') as usize;
                        if index < state.tabs.len() { state.active_tab = index; state.git_preview = None; }
                    }
                    keyboard::Key::Character("s") => task = state.save(),
                    keyboard::Key::Character("o") => {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            task = state.open_path(path);
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
                    keyboard::Key::Character("f") if state.git_preview.is_none() => {
                        if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                            code_editor::search::update(
                                &mut tab.search,
                                &mut tab.content,
                                code_editor::search::Message::Toggle,
                            );
                        }
                    }
                    // Checked before the plain "p" arm below: Shift+P usually reports as
                    // character "P", but match on either case defensively.
                    keyboard::Key::Character(c)
                        if modifiers.shift() && c.eq_ignore_ascii_case("p") =>
                    {
                        if state.command_palette.visible {
                            let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                        } else {
                            state.quick_open.visible = false;
                            state.theme_install.visible = false;
                            let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
                            let commands = command_list(state);
                            task = command_palette::open(&mut state.command_palette, commands)
                                .map(Message::CommandPalette);
                        }
                    }
                    keyboard::Key::Character("g") if state.git_preview.is_none() => {
                        if state.goto_line.visible {
                            let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
                        } else if state.tabs.get(state.active_tab).is_some() {
                            state.quick_open.visible = false;
                            state.theme_install.visible = false;
                            let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                            task = goto_line::open(&mut state.goto_line).map(Message::GotoLine);
                        }
                    }
                    keyboard::Key::Character("p") => {
                        let root = state.root.clone();
                        let msg = if state.quick_open.visible {
                            quick_open::Message::Close
                        } else {
                            let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                            let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
                            state.theme_install.visible = false;
                            quick_open::Message::Open
                        };
                        let (t, _) = quick_open::update(&mut state.quick_open, msg, root.as_deref(), &state.recent_files);
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
                    // Clipboard access is a `Task`, which `input::handle_key` can't return.
                    keyboard::Key::Character("x") if state.git_preview.is_none() => {
                        task = update(state, Message::EditAction(EditAction::Cut));
                    }
                    keyboard::Key::Character("c") if state.git_preview.is_none() => {
                        task = update(state, Message::EditAction(EditAction::Copy));
                    }
                    keyboard::Key::Character("v") if state.git_preview.is_none() => {
                        task = update(state, Message::EditAction(EditAction::Paste));
                    }
                    // Other command combos (undo/redo, ...) are the active editor's to handle.
                    _ => {
                        state.handle_editor_key(&key, modifiers);
                    }
                }
            } else if state.theme_install.visible {
                // Same as quick open below: the text input handles Enter and typing.
                let msg = match key.as_ref() {
                    keyboard::Key::Named(keyboard::key::Named::Escape) => Some(theme_install::Message::Close),
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(theme_install::Message::MoveDown),
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(theme_install::Message::MoveUp),
                    _ => None,
                };
                if let Some(msg) = msg {
                    task = update(state, Message::ThemeInstall(msg));
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
                    let (t, opened) = quick_open::update(&mut state.quick_open, msg, root.as_deref(), &state.recent_files);
                    task = t.map(Message::QuickOpen);
                    if let Some(path) = opened {
                        task = Task::batch([task, state.open_path(path)]);
                        state.focus = Focus::Editor;
                    }
                }
            } else if state.command_palette.visible {
                // Same reasoning as the quick-open branch above: only unbound keys land here.
                let msg = match key.as_ref() {
                    keyboard::Key::Named(keyboard::key::Named::Escape) => Some(command_palette::Message::Close),
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(command_palette::Message::MoveDown),
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(command_palette::Message::MoveUp),
                    _ => None,
                };
                if let Some(msg) = msg {
                    let (t, picked) = command_palette::update(&mut state.command_palette, msg);
                    task = t.map(Message::CommandPalette);
                    if let Some(picked) = picked {
                        task = Task::batch([task, update(state, picked)]);
                    }
                }
            } else if state.goto_line.visible {
                if key.as_ref() == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
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
                    let mut open_task = Task::none();
                    if let DirectoryTreeEvent::Selected(path, is_dir, _) = &event {
                        if !*is_dir {
                            open_task = state.open_path(path.clone());
                        }
                    }
                    if let Some(tree) = &mut state.tree {
                        task = Task::batch([open_task, tree.update(event).map(Message::Tree)]);
                    } else {
                        task = open_task;
                    }
                } else if state.focus == Focus::Editor {
                    state.handle_editor_key(&key, modifiers);
                }
            }
        }
        Message::Noop => {}

        Message::SidebarSelected(mode) => {
            if state.sidebar_mode != mode || !state.sidebar_visible {
                return update(state, match mode {
                    SidebarMode::Tree => Message::SidebarToggle,
                    SidebarMode::ProjectSearch => Message::ToggleProjectSearch,
                    SidebarMode::Git => Message::GitPanelToggle,
                });
            }
        }
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
        Message::AiAttach => {
            task = Task::perform(async { rfd::AsyncFileDialog::new().pick_files().await.unwrap_or_default().into_iter().map(|f| f.path().to_path_buf()).collect() }, Message::AiFiles);
        }
        Message::AiFiles(paths) => {
            if state.ai_visible {
                let mode = state.ai_mode;
                let project = state.root_or_cwd();
                task = Task::perform(async move {
                    tokio::task::spawn_blocking(move || paths.iter().take(16).map(|p| ai_context::Attachment::read(p)).collect()).await.unwrap_or_else(|e| vec![Err(e.to_string())])
                }, move |files| Message::AiAttachmentsLoaded(mode, project.clone(), files));
            }
        }
        Message::AiAttachmentsLoaded(mode, project, files) => {
            if project != state.root_or_cwd() { return Task::none(); }
            for file in files {
                match file {
                    Ok(file) => {
                        let attachments = match mode { AiMode::Http => &mut state.chat.attachments, AiMode::Acp => &mut state.acp.attachments, AiMode::Codex => &mut state.codex.attachments };
                        if attachments.len() < 16 { attachments.push(file); }
                        else { state.notice = Some(("Maximum 16 attachments per message.".into(), std::time::Instant::now())); }
                    }
                    Err(error) => state.notice = Some((error, std::time::Instant::now())),
                }
            }
        }
        Message::AiRemoveAttachment(index) => {
            let attachments = match state.ai_mode { AiMode::Http => &mut state.chat.attachments, AiMode::Acp => &mut state.acp.attachments, AiMode::Codex => &mut state.codex.attachments };
            if index < attachments.len() { attachments.remove(index); }
        }
        Message::AiReference => {
            if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                let selection = tab.content.copy_selection();
                let name = format!("{}{}", tab.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Untitled".into()), if selection.is_some() { " (selection)" } else { " (editor buffer)" });
                let content = selection.unwrap_or_else(|| tab.content.text());
                let attachments = match state.ai_mode { AiMode::Http => &mut state.chat.attachments, AiMode::Acp => &mut state.acp.attachments, AiMode::Codex => &mut state.codex.attachments };
                if content.len() <= ai_context::MAX_BYTES && attachments.len() < 16 { attachments.push(ai_context::Attachment { name, content, mime: None }); }
                else { state.notice = Some(("Attachment limit reached (16 files, 8 MiB each).".into(), std::time::Instant::now())); }
            }
        }
        Message::Codex(msg) => {
            let cwd = state.root_or_cwd();
            task = acp::update(&mut state.codex, msg, cwd).map(Message::Codex);
        }
        Message::Chat(msg) => {
            let cwd = state.root_or_cwd();
            task = chat::update(&mut state.chat, msg, cwd).map(Message::Chat);
        }
        Message::Acp(msg) => {
            let cwd = state.root_or_cwd();
            task = acp::update(&mut state.acp, msg, cwd).map(Message::Acp);
        }
        Message::Tick => {
            state.terminal.poll();
            if state.notice.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(8)) { state.notice = None; }
            let cwd = state.root_or_cwd();
            state.chat.set_project(&cwd);
            state.acp.ensure_loaded(&cwd);
            state.codex.ensure_loaded(&cwd);
            state.chat.poll();
            state.acp.poll();
            state.codex.poll();
            let codex_finished = state.codex.take_finished();
            let claude_finished = state.acp.take_finished();
            let local_changed = state.chat.take_files_changed();
            if codex_finished || claude_finished || local_changed {
                let reload_task = state.reload_open_tabs();
                if let Some(root) = state.root.clone() {
                    task = Task::batch([reload_task, refresh_dir_task(root)]);
                } else { task = reload_task; }
            }
            if !state.window_revealed && state.started_at.elapsed() > Duration::from_secs(2) {
                // Safety net in case the frame after `PaintInitialWindow` never arrives: a
                // brief flicker beats an invisible app.
                state.window_revealed = true;
                if let Some(raw) = state.startup_window.take() {
                    cloak_startup_window(raw, false);
                }
                let reveal = iced::window::latest()
                    .and_then(|id| iced::window::set_mode(id, iced::window::Mode::Windowed));
                task = Task::batch([task, reveal]);
            }
        }

        Message::WindowOpened(id) => {
            if !state.window_revealed {
                task = iced::window::raw_id::<Message>(id).map(move |raw| Message::PaintInitialWindow(id, raw));
            }
        }
        Message::PaintInitialWindow(id, raw) => {
            state.startup_window = Some(raw);
            state.window_handle = Some(raw);
            if !paint_hidden_window(raw, state.startup_maximized) {
                // Do not leave the app invisible if the native paint request fails.
                cloak_startup_window(raw, false);
                state.window_revealed = true;
                task = iced::window::set_mode(id, iced::window::Mode::Windowed);
            }
        }
        Message::WindowFrameDrawn(id) => {
            // A frame drawn before `PaintInitialWindow` ran predates the cloak/maximize (slow
            // first launches can deliver it that early), so ignore it -- the WM_PAINT that
            // `PaintInitialWindow` posts produces another frame to reveal on.
            if !state.window_revealed {
                if let Some(raw) = state.startup_window.take() {
                    state.window_revealed = true;
                    // Redraw subscriptions are delivered after the renderer submits
                    // the frame, so Windows never shows an unpainted client area.
                    cloak_startup_window(raw, false);
                    task = iced::window::set_mode(id, iced::window::Mode::Windowed);
                }
            }
        }
        Message::WindowResized(id, size) => {
            state.last_known_size = size;
            if state.window_revealed {
                task = iced::window::is_maximized(id).map(Message::WindowMaximizedChecked);
            }
        }
        Message::WindowMoved(position) => save_window_position(position),
        Message::WindowMaximizedChecked(maximized) => {
            save_window_maximized(maximized);
            if !maximized {
                save_window_size(state.last_known_size);
            }
        }
    }
    state.persist_session();
    task
}

fn view(state: &State) -> Element<'_, Message> {
    let top_bar = view_top_bar(state);
    let status_bar = view_status_bar(state);

    let panes = PaneGrid::new(&state.panes, |_id, kind, _is_maximized| {
        let content: Element<'_, Message> = match kind {
            PaneKind::Terminal => state.terminal.view(
                !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible
                    && !state.theme_install.visible).map(Message::Terminal),
            PaneKind::Sidebar => container(view_sidebar(state))
                .padding(0)
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
                .style(chrome_style)
                .into(),
        };
        pane_grid::Content::new(content)
    })
    .spacing(2)
    .style(|theme: &iced::Theme| {
        let mut style = pane_grid::default(theme);
        style.hovered_split.color = theme.extended_palette().primary.base.color;
        style.hovered_split.width = 3.0;
        style.picked_split = style.hovered_split;
        style
    })
    .on_resize(10, Message::PaneResized)
    .height(Length::Fill);

    let mut base: Element<'_, Message> = column![top_bar, panes, status_bar].into();
    if let Some((message, _)) = &state.notice {
        let toast = container(row![text(message).size(13),
            button("×").style(flat_button_style).on_press(Message::DismissNotice),
        ].spacing(12).align_y(iced::Alignment::Center))
            .padding(12).max_width(440).style(|theme: &iced::Theme| {
                let palette = theme.extended_palette();
                iced::widget::container::Style {
                    background: Some(palette.background.weak.color.into()),
                    border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 6.0.into() },
                    shadow: iced::Shadow { color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.25), offset: iced::Vector::new(0.0, 3.0), blur_radius: 12.0 },
                    ..Default::default()
                }
            });
        base = iced::widget::stack![base, container(toast).padding(16)
            .align_right(Length::Fill).align_bottom(Length::Fill)].into();
    }

    if state.theme_install.visible {
        iced::widget::stack![base, theme_install::view(&state.theme_install).map(Message::ThemeInstall)].into()
    } else if state.quick_open.visible {
        iced::widget::stack![
            base,
            quick_open::view(&state.quick_open, state.root.as_deref()).map(Message::QuickOpen),
        ]
        .into()
    } else if state.command_palette.visible {
        iced::widget::stack![
            base,
            command_palette::view(&state.command_palette).map(Message::CommandPalette),
        ]
        .into()
    } else if state.goto_line.visible {
        let line_count = state.tabs.get(state.active_tab).map(|t| t.content.line_count()).unwrap_or(0);
        iced::widget::stack![
            base,
            goto_line::view(&state.goto_line, line_count).map(Message::GotoLine),
        ]
        .into()
    } else {
        base
    }
}

fn view_ai_sidebar(state: &State) -> Element<'_, Message> {
    let mode_row = row![
        text("Assistant").size(14),
        iced::widget::pick_list([AiMode::Http, AiMode::Acp, AiMode::Codex], Some(state.ai_mode), Message::AiModeSelected).text_size(12),
        Space::new().width(Length::Fill),
        icon_control(lucide_icons::Icon::X, "Close AI panel", Some(Message::AiToggle), false)
    ].spacing(6).padding(8);
    let attachments = match state.ai_mode { AiMode::Http => &state.chat.attachments, AiMode::Acp => &state.acp.attachments, AiMode::Codex => &state.codex.attachments };
    let mut context = column![row![
        button("Attach files").style(flat_button_style).on_press(Message::AiAttach),
        button("Reference code").style(flat_button_style).on_press_maybe(state.tabs.get(state.active_tab).map(|_| Message::AiReference)),
    ].spacing(4), text("Drop images or files · Reference selection or active file").size(11)].spacing(4);
    for (i, attachment) in attachments.iter().enumerate() {
        context = context.push(row![text(attachment.name.clone()).size(11).width(Length::Fill), button("Remove").style(flat_button_style).on_press(Message::AiRemoveAttachment(i))].spacing(4));
    }
    let panel: Element<'_, Message> = match state.ai_mode {
        AiMode::Http => chat::view(&state.chat).map(Message::Chat),
        AiMode::Codex => acp::view(&state.codex, state.root_or_cwd()).map(Message::Codex),
        AiMode::Acp => acp::view(&state.acp, state.root_or_cwd()).map(Message::Acp),
    };

    column![mode_row, container(scrollable(context).height(Length::Shrink)).max_height(160).padding(8), panel].height(Length::Fill).into()
}

/// Shared 26px toolbar target with a 14px glyph and a discoverable label.
pub(crate) fn icon_control<'a, M: Clone + 'a>(
    icon: lucide_icons::Icon,
    label: &'a str,
    message: Option<M>,
    selected: bool,
) -> Element<'a, M> {
    let glyph: char = icon.into();
    let control = button(container(text(glyph).font(iced::Font::with_name("lucide")).size(14))
        .center_x(Length::Fill).center_y(Length::Fill))
        .width(26).height(26).padding(0)
        .style(move |theme: &iced::Theme, status| {
            let mut style = flat_button_style(theme, status);
            if selected && status != iced::widget::button::Status::Disabled {
                style.background = Some(theme.extended_palette().primary.weak.color.into());
                style.text_color = theme.extended_palette().primary.weak.text;
            }
            style
        }).on_press_maybe(message);
    iced::widget::tooltip(control, container(text(label).size(12)).padding([4, 8]).style(iced::widget::container::rounded_box),
        iced::widget::tooltip::Position::FollowCursor).into()
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
        Status::Disabled => Style { text_color: iced::Color { a: 0.35, ..base.text_color }, ..base }
            .with_background(iced::Color::TRANSPARENT),
        Status::Active => base.with_background(iced::Color::TRANSPARENT),
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
        Status::Disabled => Style { text_color: iced::Color { a: 0.35, ..base.text_color }, ..base }
            .with_background(iced::Color::TRANSPARENT),
        Status::Active => base.with_background(iced::Color::TRANSPARENT),
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
    button(text(label).size(13))
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
                Status::Disabled => Style { text_color: iced::Color { a: 0.35, ..base.text_color }, ..base }
            .with_background(iced::Color::TRANSPARENT),
        Status::Active => base.with_background(iced::Color::TRANSPARENT),
                Status::Hovered => base.with_background(palette.primary.weak.color),
                Status::Pressed => base.with_background(palette.primary.strong.color),
            }
        })
        .on_press_maybe(msg)
}

/// Builds the command palette's action list from current state, gating Undo/Redo/Select
/// All/Find on whether there's an active tab -- exactly like the Edit menu (`view_top_bar`)
/// already does, so the two stay in sync.
fn command_list(state: &State) -> Vec<command_palette::Command> {
    use command_palette::Command;

    let active_content = state.tabs.get(state.active_tab).map(|t| &t.content);
    let can_undo = active_content.is_some_and(|c| c.can_undo());
    let can_redo = active_content.is_some_and(|c| c.can_redo());
    let has_active_tab = state.tabs.get(state.active_tab).is_some();

    let mut commands = vec![
        Command { label: "Reopen Closed Tab (Cmd+Shift+T)".into(), message: Message::ReopenClosedTab },
        Command { label: FileAction::OpenFile.to_string(), message: Message::FileAction(FileAction::OpenFile) },
        Command { label: FileAction::OpenFolder.to_string(), message: Message::FileAction(FileAction::OpenFolder) },
        Command { label: FileAction::CloseFolder.to_string(), message: Message::FileAction(FileAction::CloseFolder) },
        Command { label: FileAction::Save.to_string(), message: Message::FileAction(FileAction::Save) },
        Command { label: "Quick Open (Cmd+P)".to_string(), message: Message::ToggleQuickOpen },
        Command { label: "Find in Project (Cmd+Shift+F)".to_string(), message: Message::ToggleProjectSearch },
        Command { label: ViewAction::ZoomIn.to_string(), message: Message::ViewAction(ViewAction::ZoomIn) },
        Command { label: ViewAction::ZoomOut.to_string(), message: Message::ViewAction(ViewAction::ZoomOut) },
        Command { label: ViewAction::ZoomReset.to_string(), message: Message::ViewAction(ViewAction::ZoomReset) },
        Command { label: "Toggle Sidebar".to_string(), message: Message::SidebarToggle },
        Command { label: "Toggle Git Panel".to_string(), message: Message::GitPanelToggle },
        Command { label: "Toggle Terminal (Ctrl+`)".into(), message: Message::TerminalToggle },
        Command { label: "Toggle AI Panel".to_string(), message: Message::AiToggle },
        Command { label: "Refresh File Tree".to_string(), message: Message::RefreshTree },
        Command { label: "Refresh Git Status".to_string(), message: Message::Git(git::Message::Refresh) },
    ];

    if can_undo {
        commands.push(Command { label: EditAction::Undo.to_string(), message: Message::EditAction(EditAction::Undo) });
    }
    if can_redo {
        commands.push(Command { label: EditAction::Redo.to_string(), message: Message::EditAction(EditAction::Redo) });
    }
    if has_active_tab {
        for action in [EditAction::Cut, EditAction::Copy, EditAction::Paste] {
            commands.push(Command { label: action.to_string(), message: Message::EditAction(action) });
        }
        commands.push(Command {
            label: EditAction::SelectAll.to_string(),
            message: Message::EditAction(EditAction::SelectAll),
        });
        commands.push(Command {
            label: "Find (Cmd+F)".to_string(),
            message: Message::Search(code_editor::search::Message::Toggle),
        });
        commands.push(Command { label: "Go to Line (Cmd+G)".to_string(), message: Message::ToggleGotoLine });
    }

    for (label, mode) in [("Install Theme...", theme_install::Mode::Install), ("Uninstall Theme...", theme_install::Mode::Uninstall)] {
        commands.push(Command { label: label.to_string(), message: Message::ThemeInstall(theme_install::Message::Open(mode)) });
    }
    for name in state.themes.names() {
        commands.push(Command { label: format!("Theme: {name}"), message: Message::AppThemeSelected(name.to_string()) });
    }

    commands
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
            EditAction::Cut.to_string(),
            has_active_tab.then_some(Message::EditAction(EditAction::Cut)),
        )),
        (menu_button_maybe(
            EditAction::Copy.to_string(),
            has_active_tab.then_some(Message::EditAction(EditAction::Copy)),
        )),
        (menu_button_maybe(
            EditAction::Paste.to_string(),
            has_active_tab.then_some(Message::EditAction(EditAction::Paste)),
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
        (menu_button(
            "Command Palette (Cmd+Shift+P)".to_string(),
            Message::ToggleCommandPalette
        )),
        (menu_button_maybe(
            "Go to Line (Cmd+G)".to_string(),
            has_active_tab.then_some(Message::ToggleGotoLine),
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
        (menu_button("Toggle Terminal (Ctrl+`)".into(), Message::TerminalToggle)),
    );

    let theme_menu_button = menu_button("Theme".to_string(), Message::Noop).width(Length::Shrink);
    let mut theme_items = vec![
        Item::new(menu_button("Install Theme...".into(), Message::ThemeInstall(theme_install::Message::Open(theme_install::Mode::Install)))),
        Item::new(menu_button("Uninstall Theme...".into(), Message::ThemeInstall(theme_install::Message::Open(theme_install::Mode::Uninstall)))),
    ];
    theme_items.extend(
        state
            .themes
            .names()
            .map(|name| Item::new(menu_button(name.to_string(), Message::AppThemeSelected(name.to_string())))),
    );

    let mb = menu_bar!(
        (file_menu_button, menu_tpl(file_items)),
        (edit_menu_button, menu_tpl(edit_items)),
        (view_menu_button, menu_tpl(view_items)),
        (theme_menu_button, menu_tpl(theme_items))
    )
    .close_on_item_click_global(true)
    .close_on_background_click(true)
    .close_on_background_click_global(true);

    let project = state.root.as_ref().and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Editor".into());
    container(row![
        mb,
        Space::new().width(Length::Fill),
        text(project).size(12).style(iced::widget::text::secondary),
        icon_control(lucide_icons::Icon::Search, "Find a file", Some(Message::ToggleQuickOpen), false),
        icon_control(lucide_icons::Icon::Command, "Command palette", Some(Message::ToggleCommandPalette), false),
    ].spacing(4).align_y(iced::Alignment::Center))
        .padding([2, 6]).style(chrome_style).into()
}

fn view_status_bar(state: &State) -> Element<'_, Message> {
    let mut bar = row![
        icon_control(lucide_icons::Icon::Folder, "Toggle files", Some(Message::SidebarToggle), state.sidebar_visible && state.sidebar_mode == SidebarMode::Tree),
        icon_control(lucide_icons::Icon::GitBranch, "Toggle source control", Some(Message::GitPanelToggle), state.sidebar_visible && state.sidebar_mode == SidebarMode::Git),
        icon_control(lucide_icons::Icon::Search, "Search project", Some(Message::ToggleProjectSearch), state.sidebar_visible && state.sidebar_mode == SidebarMode::ProjectSearch),
        Space::new().width(Length::Fill),
    ].spacing(6);

    if let Some(tab) = state.tabs.get(state.active_tab) {
        let (line, col) = tab.content.cursor_line_col();
        let language = state.highlighter.language_name(&tab.extension());
        bar = bar.push(text(format!("Ln {line}, Col {col}")).size(12));
        bar = bar.push(text(tab.line_ending.to_string()).size(12).style(iced::widget::text::secondary));
        bar = bar.push(text(language.to_string()).size(12).style(iced::widget::text::secondary));
    }

    let bar = bar.push(icon_control(lucide_icons::Icon::Terminal, "Toggle terminal (Ctrl+`)", Some(Message::TerminalToggle), state.terminal_visible))
        .push(icon_control(lucide_icons::Icon::Sparkles, "Toggle AI panel", Some(Message::AiToggle), state.ai_visible))
        .align_y(iced::Alignment::Center).padding([0, 6]);
    container(bar).width(Length::Fill).style(chrome_style).into()
}

/// A shared, theme-aware surface for application chrome.
fn chrome_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let palette = theme.extended_palette();
    iced::widget::container::Style {
        background: Some(palette.background.weak.color.into()),
        ..Default::default()
    }
}

pub(crate) fn overlay_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let palette = theme.extended_palette();
    iced::widget::container::Style {
        background: Some(palette.background.base.color.into()),
        border: iced::Border::default().rounded(12.0).color(palette.background.strong.color).width(1.0),
        shadow: iced::Shadow {
            color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.25),
            offset: iced::Vector::new(0.0, 8.0), blur_radius: 24.0,
        },
        ..Default::default()
    }
}

fn view_sidebar(state: &State) -> Element<'_, Message> {
    let nav = [
        ("Files", lucide_icons::Icon::Folder, SidebarMode::Tree),
        ("Search", lucide_icons::Icon::Search, SidebarMode::ProjectSearch),
        ("Source control", lucide_icons::Icon::GitBranch, SidebarMode::Git),
    ].into_iter().fold(row![].spacing(2), |nav, (label, icon, mode)| {
        nav.push(icon_control(icon, label, Some(Message::SidebarSelected(mode)), state.sidebar_mode == mode))
    });
    let content = match state.sidebar_mode {
        SidebarMode::Tree => view_tree(state),
        SidebarMode::ProjectSearch => project_search::view(&state.project_search, state.root.as_deref()).map(Message::ProjectSearch),
        SidebarMode::Git => git::view(&state.git, state.git_preview.as_ref().map(|preview| preview.path.as_str())).map(Message::Git),
    };
    column![container(nav).padding([3, 6]), iced::widget::rule::horizontal(1), content]
        .height(Length::Fill).into()
}

fn view_tree(state: &State) -> Element<'_, Message> {
    let Some(tree) = &state.tree else {
        return container(column![
            text("Your workspace").size(16),
            text("Open a folder to browse files, search your project, and review changes.")
                .size(13).style(iced::widget::text::secondary),
            button(text("Open Folder").size(13)).padding([8, 12])
                .on_press(Message::FileAction(FileAction::OpenFolder)),
        ].spacing(12)).padding(16).into();
    };

    let entry = if let Some((dir, is_dir, buf)) = &state.creating {
        let icon: char = if *is_dir { lucide_icons::Icon::Folder } else { lucide_icons::Icon::File }.into();
        Some((dir.as_path(), false, row![
            text(icon).font(iced::Font::with_name("lucide")).size(14),
            text_input(if *is_dir { "Folder name" } else { "File name" }, buf)
                .id("tree-name").size(13).padding([3, 5])
                .on_input(Message::CreateInput).on_submit(Message::CreateSubmit),
            button("×").style(flat_button_style).on_press(Message::CreateCancel),
        ].spacing(4).align_y(iced::Alignment::Center).into()))
    } else if let Some((path, buf)) = &state.renaming {
        Some((path.as_path(), true, row![
            text_input("Name", buf).id("tree-name").size(13).padding([3, 5])
                .on_input(Message::RenameInput).on_submit(Message::RenameSubmit),
            button("×").style(flat_button_style).on_press(Message::RenameCancel),
        ].spacing(4).into()))
    } else { None };
    let content = ContextMenu::new(tree.view_with_entry(Message::Tree, entry), || {
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

    let project = state.root.as_ref().and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Files".into());
    column![
        row![text(project).size(12), Space::new().width(Length::Fill),
            icon_control(lucide_icons::Icon::RefreshCw, "Refresh files", Some(Message::RefreshTree), false),
        ].padding([2, 8]).align_y(iced::Alignment::Center),
        container(content).padding([0, 6]).height(Length::Fill),
    ].height(Length::Fill).into()
}

fn view_editor(state: &State) -> Element<'_, Message> {
    use iced_swdir_tree::IconTheme;
    let mut tab_row = row![].spacing(1).padding([2, 4]);
    for (i, tab) in state.tabs.iter().enumerate() {
        let is_active = state.git_preview.is_none() && i == state.active_tab;
        let path = tab.path.as_deref().unwrap_or_else(|| Path::new("Untitled"));
        let spec = file_icons::FileIcons.file_glyph(path);
        let color = file_icons::FileIcons.file_color(path);
        let icon = text(spec.glyph.into_owned())
            .font(spec.font.unwrap_or_default())
            .size(spec.size.unwrap_or(14.0))
            .style(move |theme: &iced::Theme| iced::widget::text::Style {
                color: Some(color.unwrap_or(theme.extended_palette().background.base.text)),
            });
        let tab_title = row![
            button(row![icon, text(tab.title()).size(13)].spacing(6).align_y(iced::Alignment::Center))
                .padding([4, 8])
                .style(move |theme, status| tab_button_style(theme, status, is_active))
                .on_press(Message::TabSelected(i)),
            icon_control(lucide_icons::Icon::X, "Close tab", Some(Message::TabClosed(i)), false),
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

        let tab_content = container(column![tab_title, underline].spacing(2))
            .style(move |theme: &iced::Theme| iced::widget::container::Style {
                background: is_active.then(|| theme.extended_palette().background.weak.color.into()),
                ..Default::default()
            });
        let tab_content = iced::widget::mouse_area(tab_content)
            .on_middle_press(Message::TabClosed(i));
        tab_row = tab_row.push(ContextMenu::new(tab_content, move || {
            let count = state.tabs.len();
            container(column![
                menu_button("Close Tab".into(), Message::TabClosed(i)),
                menu_button_maybe("Close Other Tabs".into(), (count > 1).then_some(Message::CloseTabs(i, TabCloseScope::Others))),
                menu_button_maybe("Close Tabs to the Left".into(), (i > 0).then_some(Message::CloseTabs(i, TabCloseScope::Left))),
                menu_button_maybe("Close Tabs to the Right".into(), (i + 1 < count).then_some(Message::CloseTabs(i, TabCloseScope::Right))),
                iced::widget::rule::horizontal(1),
                menu_button("Close All Tabs".into(), Message::CloseTabs(i, TabCloseScope::All)),
                menu_button_maybe("Reopen Closed Tab".into(), (!state.closed_tabs.is_empty()).then_some(Message::ReopenClosedTab)),
            ].spacing(2).width(220))
            .padding(6).style(iced::widget::container::rounded_box).into()
        }));
    }

    if let Some(preview) = &state.git_preview {
        let header = row![
            text(preview.path.clone()).size(14),
            text(git_preview::summary(preview)).size(12).style(iced::widget::text::secondary),
            button("×").style(flat_button_style).on_press(Message::CloseGitPreview),
            Space::new().width(Length::Fill),
            button(if state.ai_visible { "Hide AI panel" } else { "Show AI panel" }).style(flat_button_style).on_press(Message::AiToggle),
        ].spacing(12).padding(8).align_y(iced::Alignment::Center);
        return column![
            scrollable(tab_row).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())),
            header,

            git_preview::view(preview, &state.app_theme.iced),
        ].width(Length::Fill).height(Length::Fill).into();
    }
    let mut editor_column = column![];

    if let Some(tab) = state.tabs.get(state.active_tab) {
        if let Some(path) = &tab.path {
            let mut crumbs = row![].spacing(2).align_y(iced::Alignment::Center);
            let base = state.root.as_deref().filter(|root| path.starts_with(root));
            let relative = base.and_then(|root| path.strip_prefix(root).ok()).unwrap_or(path);
            let mut current = base.map(Path::to_path_buf).unwrap_or_default();
            for (index, part) in relative.components().enumerate() {
                if index > 0 {
                    crumbs = crumbs.push(text("›").size(12).style(iced::widget::text::secondary));
                }
                current.push(part);
                crumbs = crumbs.push(button(text(part.as_os_str().to_string_lossy().to_string()).size(12))
                    .style(flat_button_style).on_press(Message::RevealPath(current.clone())));
            }
            crumbs = crumbs.push(Space::new().width(Length::Fill)).push(icon_control(lucide_icons::Icon::Folder, "Reveal in files", Some(Message::RevealPath(path.clone())), false));
            editor_column = editor_column.push(scrollable(crumbs.padding([0, 8]))
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())));
        }
        if tab.search.visible {
            editor_column = editor_column.push(code_editor::search::view(&tab.search).map(Message::Search));
        }
        let editor = code_editor::code_editor(
            &tab.content,
            &tab.diff,
            &state.app_theme.editor,
            state.zoom,
            Message::EditorAction,
            Message::ToggleFold,
        );
        let can_undo = tab.content.can_undo();
        let can_redo = tab.content.can_redo();
        let has_selection = tab.content.has_selection();
        editor_column = editor_column.push(ContextMenu::new(editor, move || {
            let item = |action: EditAction, enabled: bool| {
                menu_button_maybe(action.to_string(), enabled.then_some(Message::EditAction(action)))
            };
            container(column![
                item(EditAction::Undo, can_undo),
                item(EditAction::Redo, can_redo),
                iced::widget::rule::horizontal(1),
                item(EditAction::Cut, has_selection),
                item(EditAction::Copy, has_selection),
                item(EditAction::Paste, true),
                iced::widget::rule::horizontal(1),
                item(EditAction::SelectAll, true),
            ].spacing(2).width(220))
            .padding(6).style(iced::widget::container::rounded_box).into()
        }));
    } else {
        let shortcut = |label: &'static str, keys: &'static str, message: Message| {
            button(row![text(label).size(14), Space::new().width(Length::Fill),
                text(keys).size(12).style(iced::widget::text::secondary)]
                .align_y(iced::Alignment::Center))
                .width(Length::Fill).padding([6, 10]).style(flat_button_style).on_press(message)
        };
        let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
        let welcome = column![
            text("A little space to build.").size(24),
            text("Open your project and make something useful.").size(14).style(iced::widget::text::secondary),
            Space::new().height(12),
            button(text("Open Folder").size(14)).padding([10, 18])
                .on_press(Message::FileAction(FileAction::OpenFolder)),
            Space::new().height(12),
            shortcut("Open a file", if cmd == "⌘" { "⌘ O" } else { "Ctrl O" }, Message::FileAction(FileAction::OpenFile)),
            shortcut("Find a file", if cmd == "⌘" { "⌘ P" } else { "Ctrl P" }, Message::ToggleQuickOpen),
            shortcut("Run a command", if cmd == "⌘" { "⌘ Shift P" } else { "Ctrl Shift P" }, Message::ToggleCommandPalette),
            shortcut("Ask your AI assistant", "", Message::AiToggle),
        ].spacing(8).max_width(420);
        editor_column = editor_column.push(container(scrollable(container(welcome).padding(32)))
            .center_x(Length::Fill).center_y(Length::Fill));
    }

    let tab_strip = scrollable(tab_row)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default(),
        ))
        .width(Length::Fill);

    column![container(tab_strip).style(chrome_style), iced::widget::rule::horizontal(1), editor_column.height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Like `iced::keyboard::listen()`, but lets Escape through even when a focused `text_input`
/// captured it. `text_input` handles Escape internally by blurring itself first (see its
/// `Status::Captured` branch for `Named::Escape`), and `keyboard::listen()` only forwards
/// *ignored* events -- so without this, closing one of this app's overlays (quick open,
/// command palette, ...) via Escape while the search box has focus takes two presses: one
/// that just blurs the input, and a second one that finally reaches us. Every other key
/// keeps the normal ignored-only behavior, so typing in text inputs still doesn't also
/// trigger the tree/editor's global key routing.
fn handle_raw_key_event(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    // `key` is the *unmodified* key (iced_core::keyboard::Event's own doc comment: "The key
    // pressed"); `modified_key` is "the key pressed with all keyboard modifiers applied,
    // except Ctrl" -- i.e. the one that actually reflects Shift (Shift+`[` -> `{`, not `[`).
    // Using `key` here silently drops Shift for every character key.
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { modified_key, modifiers, .. }) = event else {
        return None;
    };
    let is_escape = modified_key == keyboard::Key::Named(keyboard::key::Named::Escape);
    let terminal_shortcut = modifiers.control() && modified_key.as_ref() == keyboard::Key::Character("`");
    if status == iced::event::Status::Ignored || is_escape || terminal_shortcut {
        Some(Message::KeyPressed(modified_key, modifiers))
    } else {
        None
    }
}

fn subscription(state: &State) -> Subscription<Message> {
    let keys = iced::event::listen_with(handle_raw_key_event);
    let tick = iced::time::every(Duration::from_millis(50)).map(|_| Message::Tick);
    let window_events = iced::window::events().map(|(id, event)| match event {
        iced::window::Event::FileDropped(path) => Message::AiFiles(vec![path]),
        iced::window::Event::CloseRequested => Message::Exit,
        iced::window::Event::Opened { .. } => Message::WindowOpened(id),
        iced::window::Event::Resized(size) => Message::WindowResized(id, size),
        iced::window::Event::Moved(position) => Message::WindowMoved(position),
        _ => Message::Noop,
    });
    let first_frame = if state.window_revealed {
        Subscription::none()
    } else {
        // `window::events` filters redraws; listen raw only until the first frame.
        iced::event::listen_raw(|event, _, id| match event {
            iced::Event::Window(iced::window::Event::RedrawRequested(_)) => Some(Message::WindowFrameDrawn(id)),
            _ => None,
        })
    };
    Subscription::batch([keys, tick, window_events, first_frame])
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

fn valid_entry_name(name: &str) -> bool {
    !name.trim().is_empty() && name != "." && name != ".."
        && !name.contains(['/', '\\', '\0'])
}

fn write_session(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let pending = path.with_extension("pending");
    let mut file = std::fs::File::create(&pending)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(pending, path)
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

fn save_app_theme(name: &str) {
    let Some(path) = config_path("app_theme") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, name);
}

fn load_app_theme(themes: &theme::ThemeRegistry) -> theme::EditorTheme {
    let Some(saved) = config_path("app_theme").and_then(|path| std::fs::read_to_string(path).ok()) else {
        return theme::EditorTheme::default_dark();
    };
    let saved = saved.trim();
    if let Ok(theme) = themes.load_theme(theme::migrate_name(saved)) {
        return theme;
    }
    // A theme that's gone, or an old iced built-in without a bundled equivalent: keep at
    // least its brightness.
    let light = iced::Theme::ALL
        .iter()
        .find(|theme| theme.to_string() == saved)
        .is_some_and(|theme| !theme.extended_palette().is_dark);
    if light { theme::EditorTheme::default_light() } else { theme::EditorTheme::default_dark() }
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

const MIN_WINDOW_SIZE: iced::Size = iced::Size::new(800.0, 600.0);

fn valid_window_size(size: iced::Size) -> bool {
    size.width.is_finite() && size.height.is_finite()
        && size.width >= MIN_WINDOW_SIZE.width && size.height >= MIN_WINDOW_SIZE.height
}

fn save_window_size(size: iced::Size) {
    // Minimized windows can report zero dimensions; preserve the last usable size.
    if !valid_window_size(size) { return; }
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
        .filter(|size| valid_window_size(*size))
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

/// Rescan the directory's contents while preserving expansion state.
fn refresh_dir_task(dir: PathBuf) -> Task<Message> {
    Task::done(Message::Tree(DirectoryTreeEvent::Refresh(dir)))
}

/// Kicks off an async `git diff` for `path` against `root` (the open project folder),
/// producing `Message::GitDiffLoaded` once it resolves. A no-op when there's no open project
/// (nothing to diff against). A free function rather than a `State` method so callers already
/// holding a `&mut` borrow of part of `state` (e.g. iterating `&mut self.tabs`) can still call
/// it without a borrow conflict.
fn load_diff_task(root: Option<PathBuf>, path: PathBuf) -> Task<Message> {
    let Some(root) = root else {
        return Task::none();
    };
    let for_message = path.clone();
    Task::perform(git_diff::diff_for_file(root, path), move |diff| {
        Message::GitDiffLoaded(for_message.clone(), diff)
    })
}

/// Asks a Yes/No question and produces `on_yes` if confirmed. On Windows the dialog runs on
/// a background thread: shown from inside `update` it blocked the event loop and stayed
/// unpainted (invisible until Alt was pressed).
fn ask(parent: Option<u64>, title: &str, description: String, on_yes: Message) -> Task<Message> {
    #[cfg(windows)]
    {
        let title = title.to_string();
        // `spawn_blocking` inside the future so it runs on Iced's tokio runtime.
        Task::perform(
            async move { tokio::task::spawn_blocking(move || confirm(parent, &title, &description)).await },
            move |answer| if matches!(answer, Ok(true)) { on_yes } else { Message::Noop },
        )
    }
    #[cfg(not(windows))]
    {
        if confirm(parent, title, &description) { Task::done(on_yes) } else { Task::none() }
    }
}

/// Same call rfd makes on Windows, plus MB_SETFOREGROUND (which rfd can't pass), since
/// `ask` shows it from a background thread.
#[cfg(windows)]
fn confirm(parent: Option<u64>, title: &str, description: &str) -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(window: *mut std::ffi::c_void, text: *const u16, caption: *const u16, flags: u32) -> i32;
    }
    const MB_YESNO: u32 = 0x4;
    const MB_ICONWARNING: u32 = 0x30;
    const MB_SETFOREGROUND: u32 = 0x1_0000;
    const IDYES: i32 = 6;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(description), wide(title));
    let owner = parent.map_or(std::ptr::null_mut(), |raw| raw as usize as *mut std::ffi::c_void);
    // SAFETY: `owner` is Iced's live main window or null; both strings are
    // NUL-terminated UTF-16 buffers that outlive the call.
    unsafe {
        MessageBoxW(owner, text.as_ptr(), caption.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_SETFOREGROUND) == IDYES
    }
}

#[cfg(not(windows))]
fn confirm(parent: Option<u64>, title: &str, description: &str) -> bool {
    let _ = parent; // Only set on Windows (see `State::window_handle`).
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

fn cloak_startup_window(raw: u64, cloak: bool) {
    #[cfg(windows)]
    {
        #[link(name = "dwmapi")]
        unsafe extern "system" {
            fn DwmSetWindowAttribute(window: *mut std::ffi::c_void, attribute: u32, value: *const i32, size: u32) -> i32;
        }
        let value = i32::from(cloak);
        // SAFETY: live HWND from Iced and a valid BOOL for DWMWA_CLOAK.
        unsafe { DwmSetWindowAttribute(raw as usize as *mut _, 13, &value, 4); }
    }
    #[cfg(not(windows))]
    { let _ = (raw, cloak); }
}

fn paint_hidden_window(raw: u64, maximized: bool) -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn PostMessageW(window: *mut std::ffi::c_void, message: u32, wparam: usize, lparam: isize) -> i32;
            fn ShowWindow(window: *mut std::ffi::c_void, command: i32) -> i32;
        }
        if maximized {
            // SW_MAXIMIZE also shows the HWND. Cloak it until the first frame at
            // its final maximized size, avoiding both a blank flash and animation.
            cloak_startup_window(raw, true);
            // SAFETY: raw is Iced's live HWND; 3 is SW_MAXIMIZE.
            unsafe { ShowWindow(raw as usize as *mut _, 3); }
        }
        // Windows does not invalidate hidden windows. Explicitly queue WM_PAINT
        // so winit draws the first frame before we make the HWND visible.
        // SAFETY: raw is the HWND obtained from Iced's live window; no pointers
        // are dereferenced or passed in the message payload.
        unsafe { PostMessageW(raw as usize as *mut _, 0x000F, 0, 0) != 0 }
    }
    #[cfg(not(windows))]
    { let _ = (raw, maximized); false }
}

pub fn main() -> iced::Result {
    let icon = iced::window::icon::from_file_data(include_bytes!("../icon.png"), None).ok();
    iced::application(State::boot, update, view)
        .title("editor")
        .theme(|state: &State| state.app_theme.iced.clone())
        .subscription(subscription)
        .font(iced_swdir_tree::LUCIDE_FONT_BYTES)
        .window(iced::window::Settings {
            icon,
            size: load_window_size(),
            min_size: Some(MIN_WINDOW_SIZE),
            visible: !cfg!(windows),
            position: load_window_position(),
            maximized: !cfg!(windows) && load_window_maximized(),
            exit_on_close_request: false,
            ..Default::default()
        })
        .run()
}

#[cfg(test)]
mod session_tests {
    use super::*;
    #[test]
    fn window_size_rejects_minimized_and_invalid_dimensions() {
        assert!(valid_window_size(MIN_WINDOW_SIZE));
        assert!(valid_window_size(iced::Size::new(1280.0, 800.0)));
        for size in [
            iced::Size::ZERO,
            iced::Size::new(100.0, 800.0),
            iced::Size::new(1280.0, 100.0),
            iced::Size::new(f32::NAN, 800.0),
            iced::Size::new(1280.0, f32::INFINITY),
        ] {
            assert!(!valid_window_size(size));
        }
    }

    #[test]
    fn recovery_roundtrips_and_accepts_old_sessions() {
        let legacy: EditorSession = serde_json::from_str(r#"{"root":null,"tabs":[],"active":null}"#).unwrap();
        assert!(legacy.recovery.is_empty());
        let session = EditorSession {
            recovery: vec![RecoveryBuffer { path: Some(PathBuf::from("missing.txt")), text: "unsaved é\n".into() }],
            ..Default::default()
        };
        let path = std::env::temp_dir().join(format!("editor-session-{}.json", std::process::id()));
        write_session(&path, &serde_json::to_vec(&legacy).unwrap()).unwrap();
        write_session(&path, &serde_json::to_vec(&session).unwrap()).unwrap();
        let restored: EditorSession = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(restored == session);
        assert!(!path.with_extension("pending").exists());
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod tree_refresh_tests {
    use super::*;
    use iced::futures::StreamExt;

    async fn apply(tree: &mut DirectoryTree, event: DirectoryTreeEvent) {
        let mut tasks = vec![tree.update(event)];
        while let Some(task) = tasks.pop() {
            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Output(event) = action { tasks.push(tree.update(event)); }
                }
            }
        }
    }

    #[tokio::test]
    async fn refresh_discovers_create_rename_delete_and_preserves_expansion() {
        let root = std::env::temp_dir().join(format!("editor-tree-refresh-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/keep.ts"), "").unwrap();
        let mut tree = DirectoryTree::new(root.clone());
        apply(&mut tree, DirectoryTreeEvent::Toggled(root.clone())).await;
        apply(&mut tree, DirectoryTreeEvent::Toggled(root.join("nested"))).await;
        std::fs::write(root.join("created.ts"), "").unwrap();
        std::fs::create_dir(root.join("created-folder")).unwrap();
        apply(&mut tree, DirectoryTreeEvent::Refresh(root.clone())).await;
        let node = iced_swdir_tree::__testing::root(&tree);
        assert!(node.children.iter().any(|child| child.path == root.join("created.ts")));
        assert!(node.children.iter().any(|child| child.path == root.join("created-folder")));
        let nested = node.children.iter().find(|child| child.path == root.join("nested")).unwrap();
        assert!(nested.is_expanded && nested.is_loaded && nested.children.len() == 1);
        std::fs::rename(root.join("created.ts"), root.join("renamed.ts")).unwrap();
        apply(&mut tree, DirectoryTreeEvent::Refresh(root.clone())).await;
        let node = iced_swdir_tree::__testing::root(&tree);
        assert!(!node.children.iter().any(|child| child.path == root.join("created.ts")));
        assert!(node.children.iter().any(|child| child.path == root.join("renamed.ts")));
        std::fs::remove_file(root.join("renamed.ts")).unwrap();
        apply(&mut tree, DirectoryTreeEvent::Refresh(root.clone())).await;
        assert!(!iced_swdir_tree::__testing::root(&tree).children.iter().any(|child| child.path == root.join("renamed.ts")));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tab_close_tests {
    use super::*;
    #[test]
    fn bulk_close_targets_the_clicked_tab_and_removes_in_reverse_order() {
        assert_eq!(tabs_to_close(5, 2, TabCloseScope::Left), vec![1, 0]);
        assert_eq!(tabs_to_close(5, 2, TabCloseScope::Right), vec![4, 3]);
        assert_eq!(tabs_to_close(5, 2, TabCloseScope::Others), vec![4, 3, 1, 0]);
        assert_eq!(tabs_to_close(5, 2, TabCloseScope::All), vec![4, 3, 2, 1, 0]);
        assert!(tabs_to_close(1, 0, TabCloseScope::Others).is_empty());
        assert!(tabs_to_close(3, 0, TabCloseScope::Left).is_empty());
        assert!(tabs_to_close(3, 2, TabCloseScope::Right).is_empty());
        assert!(tabs_to_close(0, 0, TabCloseScope::All).is_empty());
        let mut tabs = vec!["a", "b", "c", "d", "e"];
        for index in tabs_to_close(tabs.len(), 2, TabCloseScope::Others) { tabs.remove(index); }
        assert_eq!(tabs, ["c"]);
    }
}
