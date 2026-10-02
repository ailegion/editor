#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod about;
mod agent_launch;
mod ai_usage;
mod updater;
mod ai_approval;
mod ai_composer;
mod ai_context;
mod ai_diff;
mod ai_history;
mod ai_markdown;
mod ai_mention;
mod ai_oneshot;
mod ai_selectable;
mod inline_edit;
mod lsp;
mod acp;
mod terminal;
mod chat;
mod code_editor;
mod command_palette;
mod git;
mod file_icons;
mod git_diff;
mod titlebar;
mod completion;
mod git_graph;
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
use iced_aw::menu::{Item, Menu, MenuBar};
use iced_aw::menu_items;
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
    /// Last successfully loaded or saved text, normalized like the editor buffer.
    saved_text: Option<String>,
    line_ending: LineEnding,
    /// Diff gutter markers vs. the staged version, keyed by 0-indexed line number. Computed
    /// in-process from the buffer (see `Message::GitDiffLoaded`) when the tab opens, shortly
    /// after edits, and when the index changes.
    diff: std::collections::HashMap<usize, git_diff::LineStatus>,
    /// Latest diagnostics from the file's language server, sorted by line.
    diagnostics: Vec<lsp::Diagnostic>,
    blame: Vec<String>,
    /// Hover popup currently shown in the editor, if any.
    hover: Option<code_editor::Hover>,
    /// Rendered Markdown shown instead of the editor, while the preview is on.
    preview: Option<ai_markdown::Cache>,
}

impl Tab {
    fn is_markdown(&self) -> bool {
        self.path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
    }

    /// Re-parses the preview if the buffer changed since it was last rendered.
    fn sync_preview(&mut self) {
        if let Some(preview) = &mut self.preview {
            let text = self.content.text();
            preview.sync(std::iter::once(text.as_str()));
        }
    }

    fn preview_content(&self) -> Option<&iced::widget::markdown::Content> {
        self.preview.as_ref().and_then(|preview| preview.get(0))
    }

    fn refresh_dirty(&mut self) {
        self.dirty = self.saved_text.as_deref() != Some(self.content.text().as_str());
    }

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

/// Editor code sent to the AI panel.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AiAct {
    /// Explain the selection (or the file).
    Explain,
    /// Attach the selection (or the file) and start a refactoring request to finish typing.
    Refactor,
    /// Fix the diagnostic on the caret's line (or the file's first error).
    Fix,
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
    CheckUpdate,
    /// Help / app menu "Check for Updates…": like `CheckUpdate`, but reports the outcome.
    CheckUpdateFromMenu,
    RestartUpdate,
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
    About(about::Message),
    /// Open a new GitHub issue in the browser.
    FileBugReport,
    Git(git::Message),
    DiscardGitConfirmed(PathBuf, Vec<git::ChangedFile>),
    DiscardGitFinished(Vec<(PathBuf, String)>, git::Message),
    GitPanelToggle,
    GitPreviewLoaded(PathBuf, String, Result<String, String>),
    CloseGitPreview,
    GitPreviewScrolled(bool, f32),
    GitGraphOpen,
    GitGraph(git_graph::Message),
    GitGraphPatchLoaded(String, Result<String, String>),
    GitDiffLoaded(PathBuf, String, Vec<(usize, git_diff::LineStatus)>),
    GitBlameLoaded(PathBuf, String, Vec<String>),
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
    /// Switch the active Markdown tab between source and rendered preview.
    MarkdownPreviewToggle,
    MarkdownPreview(ai_markdown::Action),
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
    AiToggle,
    AiModeSelected(AiMode),
    TreeDragReleased,
    TreeDragCancel,
    /// Accept a completion: this item (clicked), or the selected one (Enter/Tab).
    CompletionAccept(Option<usize>),
    /// Move these files and folders into this folder (confirmed by the user).
    TreeMoveConfirmed(Vec<PathBuf>, PathBuf),
    AiAttach,
    AiFiles(Vec<PathBuf>),
    AiAttachmentsLoaded(AiMode, PathBuf, Vec<Result<ai_context::Attachment, String>>),
    AiReference,
    AiRemoveAttachment(usize),
    AiPaste,
    /// Clipboard text for the AI input, used when `AiPaste` found no image.
    AiPasteText(Option<String>),
    /// Ask the AI panel about the code in the editor.
    AiAct(AiAct),
    /// Cmd/Ctrl+K: start an inline AI edit of the selection (or the current line).
    InlineEditStart,
    InlineEditInput(String),
    InlineEditSubmit,
    InlineEditReady(u64, Result<String, String>),
    InlineEditAccept,
    InlineEditCancel,
    /// Install the language server with this id, or dismiss its prompt for the session.
    LspInstall(&'static str),
    LspDismiss(&'static str),
    /// Hover / Ctrl+click gestures from the editor canvas.
    EditorProbe(code_editor::Probe),
    /// Go to the definition of the symbol under the caret (F12).
    GoToDefinition,
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
    updater: updater::Updater,
    /// A menu-requested update check is running; its outcome is shown as a notice.
    announce_update_check: bool,
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
    about: about::AboutState,
    recent_files: Vec<PathBuf>,
    git: git::GitState,
    git_preview: Option<git_preview::Preview>,
    /// The commit graph, covering the editor area like `git_preview` (which opens over it).
    git_graph: Option<git_graph::Graph>,
    /// When the active tab's diff markers should be recomputed after typing pauses.
    gutter_due: Option<std::time::Instant>,
    lsp: lsp::Manager,
    /// A language server the open file needs but which isn't installed; shown as a toast.
    lsp_prompt: Option<&'static lsp::registry::Server>,
    /// The (line, byte index) the mouse is resting on, awaiting hover text, and the canvas
    /// pixel to anchor the popup at. A late answer for any other position is ignored.
    hover_request: Option<(usize, usize, iced::Point)>,
    /// The completion list showing, and the latest position completions were asked for
    /// (path, line, byte index), so answers to older requests are ignored.
    completion: Option<completion::Session>,
    completion_request: Option<(PathBuf, usize, usize)>,

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
    /// Project files for `@` mention suggestions, listed when a mention starts: (root, paths).
    mention_files: Option<(PathBuf, Vec<String>)>,
    /// Files matching the mention being typed in the AI input.
    mentions: Vec<String>,
    inline_edit: Option<inline_edit::InlineEdit>,
    /// Id of the latest inline edit request; answers to earlier ones are ignored.
    inline_generation: u64,
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
            .map(file_tree);
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
            updater: updater::Updater::default(),
            announce_update_check: false,
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
            about: about::AboutState::default(),
            recent_files: recent_files::load(),
            git: git::GitState::new(),
            git_preview: None,
            git_graph: None,
            gutter_due: None,
            lsp: lsp::Manager::default(),
            lsp_prompt: None,
            hover_request: None,
            completion: None,
            completion_request: None,
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
            mention_files: None,
            mentions: Vec::new(),
            inline_edit: None,
            inline_generation: 0,
            chat: chat::ChatState::new(),
            acp: acp::AcpState::claude(),
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
                if let Some(path) = &recovery.path { state.lsp.open(path, recovery.text.clone()); }
                let mut tab = Tab {
                    path: recovery.path.clone(), content, dirty: true,
                    saved_text: recovery.path.as_ref().and_then(|path| std::fs::read_to_string(path).ok())
                        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n")),
                    search: Default::default(), line_ending: LineEnding::detect(&recovery.text), diff: Default::default(),
                    diagnostics: Vec::new(), blame: Vec::new(), hover: None, preview: None,
                };
                tab.refresh_dirty();
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
        let repo = self.git.repo();
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
                    tab.blame.clear();
                    self.lsp.changed(&path);
                    tasks.push(load_diff_task(repo.clone(), self.root.as_deref(), path, text));
                }
                tab.saved_text = Some(tab.content.text());
            }
        }
        Task::batch(tasks)
    }

    /// Points open tabs at their new paths after files or folders moved; unsaved edits stay.
    fn paths_moved(&mut self, moves: &[(PathBuf, PathBuf)]) {
        for tab in &mut self.tabs {
            let Some(path) = &tab.path else { continue };
            if let Some(new_path) = moves.iter().find_map(|(from, to)| moved_path(path, from, to)) {
                tab.path = Some(new_path);
            }
        }
    }

    /// A diff or the commit graph covers the editor, so editing keys must not reach the hidden tab.
    fn editor_hidden(&self) -> bool {
        self.git_preview.is_some() || self.git_graph.is_some()
    }

    /// Uncovers the editor: closes the diff view and the commit graph.
    fn show_editor(&mut self) {
        self.git_preview = None;
        self.git_graph = None;
    }

    fn open_path(&mut self, path: PathBuf) -> Task<Message> {
        self.show_editor();
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path.as_path()))
        {
            self.active_tab = index;
            recent_files::record(&mut self.recent_files, path.clone());
            return Task::none();
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
        self.lsp.set_root(&self.root_or_cwd());
        self.lsp.open(&path, text.clone());
        let task = load_diff_task(self.git.repo(), self.root.as_deref(), path.clone(), text);
        self.tabs.push(Tab {
            path: Some(path),
            saved_text: Some(content.text()),
            content,
            search: code_editor::search::SearchState::default(),
            dirty: false,
            line_ending,
            diff: std::collections::HashMap::new(),
            diagnostics: Vec::new(), blame: Vec::new(),
            hover: None,
            preview: None,
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
        if let Some(path) = self.tabs[index].path.clone() {
            self.lsp.closed(&path);
            self.closed_tabs.push(path);
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab > index || self.active_tab >= self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1).min(self.tabs.len() - 1);
        }
    }

    fn save(&mut self) -> Task<Message> {
        if self.editor_hidden() { return Task::none(); }
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return Task::none();
        };
        let path = match &tab.path {
            Some(p) => Some(p.clone()),
            None => rfd::FileDialog::new().save_file(),
        };
        if let Some(path) = path {
            let text = tab.content.text();
            let result = std::fs::write(&path, &text);
            if result.is_ok() {
                let was_untitled = tab.path.is_none();
                tab.path = Some(path.clone());
                tab.saved_text = Some(text.clone());
                tab.dirty = false;
                if was_untitled { self.lsp.open(&path, text); } else { self.lsp.saved(&path, text); }
                // Saving changes neither the buffer nor the index, so the markers stay valid.
                self.notify(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()));
                return Task::none();
            }
            if let Err(err) = result { self.notify(format!("Save failed: {err}")); }
        }
        Task::none()
    }

    /// Routes a key press to the active tab's editor. Returns whether it was handled.
    fn handle_editor_key(&mut self, key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
        if self.editor_hidden() { return false; }
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return false;
        };
        let before = tab.content.undo_count();
        let handled = code_editor::input::handle_key(&mut tab.content, key, modifiers);
        self.mark_edited_if_changed(before);
        handled
    }

    /// Refreshes the active tab's saved-state comparison and syntax highlighting if `before` (an
    /// `undo_count()` taken just before some editor operation) no longer matches -- i.e. the
    /// operation actually changed the document, as opposed to just moving the cursor.
    fn mark_edited_if_changed(&mut self, before: usize) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if tab.content.undo_count() == before {
            return;
        }
        tab.refresh_dirty();
        tab.blame.clear();
        tab.hover = None;
        let extension = tab.extension();
        tab.content.highlight(&self.highlighter, &extension, &self.app_theme.syntax);
        if let Some(path) = &tab.path { self.lsp.changed(path); }
        self.gutter_due = Some(std::time::Instant::now() + GUTTER_DEBOUNCE);
    }

    /// Sends debounced edits to language servers and applies what they sent back.
    fn poll_lsp(&mut self) -> Task<Message> {
        for path in self.lsp.take_due() {
            if let Some(tab) = self.tabs.iter().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                let text = tab.content.text();
                self.lsp.change(&path, text);
            }
        }
        let mut tasks = Vec::new();
        for event in self.lsp.poll() {
            match event {
                lsp::Event::Diagnostics(path, diagnostics) => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                        tab.diagnostics = diagnostics;
                    }
                }
                lsp::Event::Notice(text) => self.notify(text),
                lsp::Event::Prompt(server) => self.lsp_prompt = Some(server),
                lsp::Event::Hover { path, line, column, lines } => {
                    if lines.is_empty() { continue; }
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                        let index = tab.content.inner.lines.get(line).map_or(0, |l| lsp::utf16_to_byte(l.text(), column));
                        if let Some((_, _, anchor)) = self.hover_request.filter(|(l, i, _)| (*l, *i) == (line, index)) {
                            tab.hover = Some(code_editor::Hover { line, index, anchor, lines });
                        }
                    }
                }
                lsp::Event::Completion { path, line, column, items, incomplete } => {
                    // Only the answer to the latest request, for the tab and line it was about.
                    let Some((_, _, request)) = self.completion_request.clone().filter(|(p, l, _)| *p == path && *l == line) else { continue };
                    let Some(tab) = self.tabs.get(self.active_tab).filter(|tab| tab.path.as_deref() == Some(path.as_path())) else { continue };
                    let Some(text) = tab.content.inner.lines.get(line).map(|l| l.text()) else { continue };
                    if text.get(..request).is_none() || lsp::byte_to_utf16(text, request) != column { continue; }
                    let word = completion::word_start(text, request);
                    let mut session = completion::Session::new(path, line, request, word, items, incomplete);
                    self.completion = session.refilter(&tab.content).then_some(session);
                }
                lsp::Event::Definition { path, line, column } => {
                    tasks.push(self.open_path(path.clone()));
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                        let index = tab.content.inner.lines.get(line).map_or(0, |l| lsp::utf16_to_byte(l.text(), column));
                        tab.content.goto(line, index);
                        tab.hover = None;
                        self.focus = Focus::Editor;
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    /// Asks the language server for completions at the active tab's cursor. `trigger` is the
    /// non-word character just typed, when that is what prompted it.
    fn request_completion(&mut self, trigger: Option<char>) {
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        let Some(path) = tab.path.clone() else { return };
        let cursor = tab.content.cursor;
        let Some(line) = tab.content.inner.lines.get(cursor.line) else { return };
        let column = lsp::byte_to_utf16(line.text(), cursor.index);
        let text = tab.content.text();
        self.completion_request = Some((path.clone(), cursor.line, cursor.index));
        self.lsp.completion(&path, cursor.line, column, text, trigger);
    }

    fn close_completion(&mut self) {
        self.completion = None;
        self.completion_request = None;
    }

    /// The completion list, if it belongs to the active tab and the cursor is still in its word.
    fn active_completion(&mut self) -> Option<&mut completion::Session> {
        let tab = self.tabs.get(self.active_tab)?;
        let valid = self.completion.as_ref().is_some_and(|session| tab.path.as_deref() == Some(session.path.as_path()) && session.at_cursor(&tab.content));
        if !valid { self.close_completion(); }
        self.completion.as_mut()
    }

    /// After the editor handled `key`: keeps the list in step with the typed word, and asks
    /// for completions while a word is typed or after a character the server completes after.
    fn after_editor_key(&mut self, key: &keyboard::Key, modifiers: keyboard::Modifiers) {
        let typed = match key.as_ref() {
            keyboard::Key::Character(c) if !modifiers.command() && !modifiers.alt() => c.chars().next(),
            keyboard::Key::Named(keyboard::key::Named::Space) => Some(' '),
            keyboard::Key::Named(keyboard::key::Named::Backspace) => None,
            _ => { self.close_completion(); return; }
        };
        if let Some(ch) = typed.filter(|ch| !completion::is_word_char(*ch)) {
            self.close_completion();
            self.request_completion(Some(ch));
            return;
        }
        // A word character, or Backspace (`typed` is `None`): narrow the list to the word.
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        let (open, incomplete) = match self.completion.as_mut() {
            Some(session) => (session.refilter(&tab.content), session.incomplete),
            None => (false, false),
        };
        let had_list = self.completion.is_some();
        if !open { self.completion = None; }
        // Typing a word with no list yet, or one the server marked incomplete (it may be
        // missing what matches now): ask again, keeping what shows meanwhile.
        if typed.is_some() && (!had_list || incomplete) { self.request_completion(None); }
    }

    /// UTF-16 column of byte `index` on `line` of the active tab, with the tab's path.
    fn lsp_position(&self, line: usize, index: usize) -> Option<(PathBuf, usize)> {
        let tab = self.tabs.get(self.active_tab)?;
        let column = lsp::byte_to_utf16(tab.content.inner.lines.get(line)?.text(), index);
        Some((tab.path.clone()?, column))
    }

    /// Reacts to repository changes and pending edits: markers follow the index and the
    /// buffer, and status reloads only while the source control panel is showing.
    fn poll_git(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.git.set_project(self.root.as_deref()) {
            tasks.push(self.reload_git_views());
            if self.sidebar_visible && self.sidebar_mode == SidebarMode::Git {
                tasks.push(Task::done(Message::Git(git::Message::Refresh)));
            }
        }
        if let Some(changes) = self.git.take_changes() {
            if changes.repository {
                tasks.push(self.reload_git_views());
                if let Some(graph) = &mut self.git_graph { tasks.push(graph.reload().map(Message::GitGraph)); }
            } else if let Some(preview) = self.git_preview.as_mut().filter(|preview| preview.live) {
                // Working-tree edits only change the unstaged half of an open preview.
                tasks.push(load_preview_task(self.git.repo(), preview.root.clone(), preview.path.clone()));
            }
            if self.sidebar_visible && self.sidebar_mode == SidebarMode::Git {
                let cwd = self.root_or_cwd();
                tasks.push(git::update(&mut self.git, git::Message::Refresh, cwd).map(Message::Git));
            }
        }
        if self.gutter_due.is_some_and(|due| std::time::Instant::now() >= due) {
            self.gutter_due = None;
            if let Some(tab) = self.tabs.get(self.active_tab) {
                if let Some(path) = tab.path.clone() {
                    tasks.push(load_diff_task(self.git.repo(), self.root.as_deref(), path, tab.content.text()));
                }
            }
        }
        Task::batch(tasks)
    }

    /// Reloads the open diff preview and every tab's markers from current repository state.
    fn reload_git_views(&mut self) -> Task<Message> {
        let mut tasks = vec![self.reload_all_diffs()];
        if let Some(preview) = self.git_preview.as_mut().filter(|preview| preview.live) {
            preview.result = None;
            tasks.push(load_preview_task(self.git.repo(), preview.root.clone(), preview.path.clone()));
        }
        Task::batch(tasks)
    }

    /// Recomputes the diff markers of every open tab, e.g. after the index changed.
    fn reload_all_diffs(&self) -> Task<Message> {
        let repo = self.git.repo();
        Task::batch(self.tabs.iter().filter_map(|tab| {
            let path = tab.path.clone()?;
            Some(load_diff_task(repo.clone(), self.root.as_deref(), path, tab.content.text()))
        }))
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

    fn ai_input(&self) -> String {
        match self.ai_mode {
            AiMode::Http => self.chat.input_text(),
            AiMode::Acp => self.acp.input_text(),
            AiMode::Codex => self.codex.input_text(),
        }
    }

    fn set_ai_input(&mut self, text: &str) {
        match self.ai_mode {
            AiMode::Http => self.chat.set_input(text),
            AiMode::Acp => self.acp.set_input(text),
            AiMode::Codex => self.codex.set_input(text),
        }
        self.refresh_mentions();
    }

    fn ai_attachments(&mut self) -> &mut Vec<ai_context::Attachment> {
        match self.ai_mode {
            AiMode::Http => &mut self.chat.attachments,
            AiMode::Acp => &mut self.acp.attachments,
            AiMode::Codex => &mut self.codex.attachments,
        }
    }

    /// Where one-off AI requests (commit messages, inline edits) go: the assistant selected
    /// in the AI panel.
    fn ai_backend(&self) -> Result<ai_oneshot::Backend, String> {
        match self.ai_mode {
            AiMode::Http => self.chat.connection()
                .map(|(base_url, api_key, model)| ai_oneshot::Backend::Http { base_url, api_key, model })
                .ok_or_else(|| "Choose a model in the AI panel's settings first".to_string()),
            AiMode::Acp => Ok(ai_oneshot::Backend::Agent { provider: self.acp.provider(), model: self.acp.preferred_model() }),
            AiMode::Codex => Ok(ai_oneshot::Backend::Agent { provider: self.codex.provider(), model: self.codex.preferred_model() }),
        }
    }

    /// Updates the `@` file suggestions for the AI input. The project is listed once per mention.
    fn refresh_mentions(&mut self) {
        let text = self.ai_input();
        let Some(query) = ai_mention::typing(&text) else {
            self.mentions.clear();
            self.mention_files = None;
            return;
        };
        let root = self.root_or_cwd();
        if self.mention_files.as_ref().is_none_or(|(listed, _)| *listed != root) {
            let files = self.root.as_deref().map(quick_open::walk).unwrap_or_default();
            self.mention_files = Some((root.clone(), ai_mention::relative_paths(&root, &files)));
        }
        self.mentions = self.mention_files.as_ref().map(|(_, files)| ai_mention::suggest(files, query)).unwrap_or_default();
    }

    /// Attaches the project files `@`-mentioned in the AI input that aren't attached yet.
    /// Mentions that aren't files in the project are left as plain text.
    fn attach_mentions(&mut self) {
        let Some(root) = self.root.clone() else { return };
        let Ok(canonical_root) = root.canonicalize() else { return };
        for mention in ai_mention::mentioned(&self.ai_input()) {
            if self.ai_attachments().iter().any(|attachment| attachment.name == mention) { continue; }
            let Some(path) = root.join(&mention).canonicalize().ok()
                .filter(|path| path.starts_with(&canonical_root) && path.is_file()) else { continue };
            if self.ai_attachments().len() >= 16 {
                self.notify("Maximum 16 attachments per message.");
                break;
            }
            match ai_context::Attachment::read(&path) {
                Ok(mut attachment) => {
                    attachment.name = mention;
                    self.ai_attachments().push(attachment);
                }
                Err(err) => self.notify(format!("Could not attach {mention}: {err}")),
            }
        }
        self.mentions.clear();
    }

    /// Puts AI-suggested code into the active tab at the caret, replacing any selection.
    fn insert_code(&mut self, code: &str) {
        if self.editor_hidden() || self.tabs.get(self.active_tab).is_none() {
            self.notify("Open a file tab to insert code");
            return;
        }
        let before = self.tabs[self.active_tab].content.undo_count();
        self.tabs[self.active_tab].content.replace_selection(&code.replace("\r\n", "\n"));
        self.focus = Focus::Editor;
        self.mark_edited_if_changed(before);
    }

    fn open_ai_diff(&mut self, diff: Option<ai_diff::FileDiff>) {
        match diff {
            Some(diff) => {
                self.git_preview = Some(diff.preview(&self.root_or_cwd()));
                self.focus = Focus::Editor;
            }
            None => self.notify("That change is no longer available"),
        }
    }

    /// Puts `prompt` in the AI panel with `attachment`, showing the panel, and sends it unless
    /// the user is meant to finish typing it.
    fn ask_ai(&mut self, prompt: &str, attachment: ai_context::Attachment, send: bool) -> Task<Message> {
        let mut task = Task::none();
        if !self.ai_visible { task = update(self, Message::AiToggle); }
        if self.ai_attachments().len() < 16 { self.ai_attachments().push(attachment); }
        else { self.notify("Maximum 16 attachments per message."); }
        self.set_ai_input(prompt);
        if send {
            let send = match self.ai_mode {
                AiMode::Http => Message::Chat(chat::Message::Send),
                AiMode::Acp => Message::Acp(acp::Message::Send),
                AiMode::Codex => Message::Codex(acp::Message::Send),
            };
            task = Task::batch([task, update(self, send)]);
        }
        task
    }

    /// The active tab's path relative to the project, for prompts.
    fn tab_label(&self, tab: &Tab) -> String {
        let root = self.root_or_cwd();
        tab.path.as_deref().map(|path| path.strip_prefix(&root).unwrap_or(path).display().to_string().replace('\\', "/"))
            .unwrap_or_else(|| "Untitled".into())
    }

    fn ai_act(&mut self, act: AiAct) -> Task<Message> {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            self.notify("Open a file first");
            return Task::none();
        };
        let bounds = tab.content.selection_bounds();
        let code = match bounds {
            Some((start, end)) => tab.content.text_between(start, end).unwrap_or_default(),
            None => tab.content.text(),
        };
        let tab = &self.tabs[self.active_tab];
        let name = self.tab_label(tab);
        let label = match bounds {
            Some((start, end)) => format!("{name} (lines {}-{})", start.line + 1, end.line + 1),
            None => name.clone(),
        };
        let snippet = ai_context::Attachment { name: label.clone(), content: code, mime: None };
        match act {
            AiAct::Explain => self.ask_ai(&format!("Explain what the code in {label} does and how it works."), snippet, true),
            AiAct::Refactor => self.ask_ai(&format!("Refactor the code in {label} to "), snippet, false),
            AiAct::Fix => {
                let line = tab.content.cursor.line;
                let diagnostic = tab.diagnostics.iter().find(|d| d.line == line)
                    .or_else(|| tab.diagnostics.iter().find(|d| d.severity == lsp::Severity::Error));
                let Some(diagnostic) = diagnostic else {
                    self.notify("No problem reported on this line");
                    return Task::none();
                };
                let (problem, message) = (diagnostic.line, diagnostic.message.clone());
                let kind = if diagnostic.severity == lsp::Severity::Error { "error" } else { "warning" };
                let text = tab.content.text();
                let lines: Vec<&str> = text.lines().collect();
                let first = problem.saturating_sub(15);
                let last = (diagnostic.end_line + 15).min(lines.len().saturating_sub(1));
                let numbered = (first..=last).filter_map(|n| lines.get(n).map(|code| format!("{:>5} | {code}", n + 1)))
                    .collect::<Vec<_>>().join("\n");
                let nearby = ai_context::Attachment { name: format!("{name} (lines {}-{})", first + 1, last + 1), content: numbered, mime: None };
                self.ask_ai(&format!("Fix this {kind} in {name} at line {}: {message}", problem + 1), nearby, true)
            }
        }
    }

    /// Cmd/Ctrl+K: asks for an instruction to rewrite the selection, or the caret's line.
    fn start_inline_edit(&mut self) -> Task<Message> {
        if self.editor_hidden() { return Task::none(); }
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            self.notify("Open a file first");
            return Task::none();
        };
        let (start, end) = tab.content.selection_bounds().unwrap_or_else(|| {
            let line = tab.content.cursor.line;
            let length = tab.content.inner.lines.get(line).map_or(0, |text| text.text().len());
            (cosmic_text::Cursor::new(line, 0), cosmic_text::Cursor::new(line, length))
        });
        let original = tab.content.text_between(start, end).unwrap_or_default();
        self.inline_edit = Some(inline_edit::InlineEdit {
            path: tab.path.clone(), start, end, original, instruction: String::new(), stage: inline_edit::Stage::Prompt,
        });
        iced::widget::operation::focus(inline_edit::INPUT_ID)
    }

    fn submit_inline_edit(&mut self) -> Task<Message> {
        let backend = match self.ai_backend() {
            Ok(backend) => backend,
            Err(err) => { self.notify(err); return Task::none(); }
        };
        let root = self.root_or_cwd();
        let Some(edit) = self.inline_edit.as_ref().filter(|edit| matches!(edit.stage, inline_edit::Stage::Prompt) && !edit.instruction.trim().is_empty()) else {
            return Task::none();
        };
        let Some(tab) = self.tabs.iter().find(|tab| tab.path == edit.path) else {
            self.inline_edit = None;
            return Task::none();
        };
        let last = tab.content.line_count().saturating_sub(1);
        let end_of_file = cosmic_text::Cursor::new(last, tab.content.inner.lines.get(last).map_or(0, |line| line.text().len()));
        let before = tab.content.text_between(cosmic_text::Cursor::new(0, 0), edit.start).unwrap_or_default();
        let after = tab.content.text_between(edit.end, end_of_file).unwrap_or_default();
        let language = self.highlighter.language_name(&tab.extension()).to_string();
        let prompt = inline_edit::prompt(&self.tab_label(tab), &language, &before, &edit.original, &after, &edit.instruction);
        self.inline_generation += 1;
        let id = self.inline_generation;
        if let Some(edit) = &mut self.inline_edit { edit.stage = inline_edit::Stage::Working(id); }
        Task::perform(ai_oneshot::ask(backend, root, prompt), move |result| Message::InlineEditReady(id, result))
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

/// `update`, then brings the visible Markdown preview up to date with whatever the message
/// changed (a reload from disk, an AI edit, switching tabs).
fn update_and_sync_preview(state: &mut State, message: Message) -> Task<Message> {
    let task = update(state, message);
    if let Some(tab) = state.tabs.get_mut(state.active_tab) {
        tab.sync_preview();
    }
    task
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    if matches!(&message, Message::TabClosed(_) | Message::CloseTabs(_, _) | Message::CloseTabConfirmed(_) | Message::CloseTabsConfirmed(_, _) | Message::TabSelected(_) | Message::FileAction(_) | Message::ReopenClosedTab)
        || matches!(&message, Message::KeyPressed(_, modifiers) if modifiers.command()) {
        state.last_session_write = std::time::Instant::now() - Duration::from_secs(1);
    }
    let mut task = Task::none();
    if state.editor_hidden() && matches!(&message,
        Message::EditorAction(_) | Message::EditAction(_) | Message::Search(_)
        | Message::FileAction(FileAction::Save)) {
        return Task::none();
    }
    // The previewed buffer is hidden; edits to it would go unseen.
    if state.tabs.get(state.active_tab).is_some_and(|tab| tab.preview.is_some()) && matches!(&message,
        Message::EditorAction(_) | Message::EditAction(_) | Message::EditorPasted(_) | Message::Search(_)
        | Message::InlineEditStart) {
        return Task::none();
    }
    match message {
        Message::MarkdownPreviewToggle => {
            if let Some(tab) = state.tabs.get_mut(state.active_tab).filter(|tab| tab.is_markdown()) {
                tab.preview = if tab.preview.is_some() { None } else { Some(Default::default()) };
                tab.hover = None;
            }
        }
        Message::MarkdownPreview(ai_markdown::Action::Copy(text)) => task = iced::clipboard::write(text),
        Message::MarkdownPreview(ai_markdown::Action::Link(url)) => ai_markdown::open_link(&url),
        Message::MarkdownPreview(ai_markdown::Action::Insert(_)) => {}
        Message::CheckUpdate => state.updater.check(),
        Message::CheckUpdateFromMenu => {
            if !state.updater.enabled {
                state.notify("Updates are only available in downloaded release builds");
            } else if state.updater.ready() {
                return update(state, Message::RestartUpdate);
            } else {
                state.updater.check();
                // The status bar shows progress; the outcome also gets a notice.
                state.announce_update_check = true;
            }
        }
        Message::About(msg) => task = about::update(&mut state.about, msg).map(Message::About),
        Message::FileBugReport => open_url("https://github.com/ailegion/editor/issues/new"),
        Message::RestartUpdate => {
            state.last_session_write = std::time::Instant::now() - Duration::from_secs(1);
            state.persist_session();
            let recovery: Vec<_> = state.tabs.iter().filter(|tab| tab.dirty).map(|tab| RecoveryBuffer {
                path: tab.path.clone(), text: tab.content.text(),
            }).collect();
            if recovery != state.saved_session.recovery {
                state.notify("Could not save recovery. Save your files before updating.");
                return Task::none();
            }
            match state.updater.restart() {
                Ok(()) => { state.terminal.shutdown(); return iced::exit(); }
                Err(error) => state.notify(error),
            }
        }
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
        Message::CompletionAccept(index) => {
            let Some(session) = state.completion.take() else { return Task::none() };
            state.completion_request = None;
            let Some(tab) = state.tabs.get_mut(state.active_tab).filter(|tab| tab.path.as_deref() == Some(session.path.as_path())) else { return Task::none() };
            let Some(edits) = index.or_else(|| session.selected_item()).and_then(|index| session.edits(index, &tab.content)) else { return Task::none() };
            let before = tab.content.undo_count();
            tab.content.replace_ranges(&edits, 0);
            state.focus = Focus::Editor;
            state.mark_edited_if_changed(before);
        }
        Message::EditorAction(action) => {
            state.focus = Focus::Editor;
            state.close_completion();
            let before = state
                .tabs
                .get(state.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                tab.content.perform(action);
                tab.hover = None;
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
                root: root.clone(), path: path.clone(), result: None, live: true,
            });
            task = load_preview_task(state.git.repo(), root, path);
        }
        Message::Git(git::Message::GenerateMessage) => match state.ai_backend() {
            Err(err) => state.notify(err),
            Ok(backend) => {
                let cwd = state.root_or_cwd();
                let _ = git::update(&mut state.git, git::Message::GenerateMessage, cwd.clone());
                task = Task::perform(async move {
                    let prompt = tokio::task::spawn_blocking({
                        let cwd = cwd.clone();
                        move || git::commit_message_prompt(&cwd)
                    }).await.map_err(|err| err.to_string())??;
                    ai_oneshot::ask(backend, cwd, prompt).await.map(|reply| git::clean_commit_message(&reply))
                }, |result| Message::Git(git::Message::MessageGenerated(result)));
            }
        },
        Message::GitPreviewLoaded(root, path, result) => {
            if let Some(preview) = &mut state.git_preview {
                if preview.root == root && preview.path == path {
                    preview.result = Some(result);
                }
            }
        }
        Message::CloseGitPreview => state.git_preview = None,
        Message::GitGraphOpen => {
            if state.git.repo().is_none() {
                state.notify("This folder is not inside a Git repository.");
            } else {
                let (graph, load) = git_graph::Graph::open(state.root_or_cwd());
                state.git_preview = None;
                state.git_graph = Some(graph);
                state.focus = Focus::Editor;
                task = load.map(Message::GitGraph);
            }
        }
        Message::GitGraph(git_graph::Message::Close) => state.git_graph = None,
        Message::GitGraph(git_graph::Message::OpenFile(hash, file)) => {
            if let Some(graph) = &state.git_graph {
                let (root, parent) = (graph.root.clone(), graph.parent_of(&hash));
                let label = format!("{} @ {}", file.path, &hash[..hash.len().min(7)]);
                state.git_preview = Some(git_preview::Preview { root: root.clone(), path: label.clone(), result: None, live: false });
                task = Task::perform(async move {
                    tokio::task::spawn_blocking(move || git_graph::file_patch(&root, &hash, parent.as_deref(), &file))
                        .await.map_err(|err| err.to_string())?
                }, move |result| Message::GitGraphPatchLoaded(label.clone(), result));
            }
        }
        Message::GitGraphPatchLoaded(label, result) => {
            if let Some(preview) = state.git_preview.as_mut().filter(|preview| !preview.live && preview.path == label) {
                preview.result = Some(result);
            }
        }
        Message::GitGraph(msg) => {
            if let Some(graph) = &mut state.git_graph {
                task = git_graph::update(graph, msg).map(Message::GitGraph);
            }
        }
        Message::GitPreviewScrolled(left, y) => {
            task = iced::widget::operation::scroll_to(
                if left { git_preview::RIGHT_SCROLL } else { git_preview::LEFT_SCROLL },
                scrollable::AbsoluteOffset { x: None, y: Some(y) },
            );
        }
        Message::Git(git::Message::ShowGraph) => {
            let cwd = state.root_or_cwd();
            let _ = git::update(&mut state.git, git::Message::ShowGraph, cwd);
            task = update(state, Message::GitGraphOpen);
        }
        Message::Git(git::Message::Discard(files)) => {
            task = ask(state.window_handle, "Discard changes", format!("Discard unstaged changes in {} file(s)? Staged changes are kept. Untracked files will be deleted. Unsaved editor changes in these files will also be discarded. This cannot be undone.", files.len()), Message::DiscardGitConfirmed(state.root_or_cwd(), files));
        }
        Message::DiscardGitConfirmed(cwd, files) => {
            if cwd != state.root_or_cwd() { return Task::none(); }
            let paths: Vec<_> = state.tabs.iter().filter_map(|tab| {
                let path = tab.path.as_ref()?;
                files.iter().any(|file| cwd.join(&file.path) == *path).then(|| (path.clone(), tab.content.text()))
            }).collect();
            task = git::update(&mut state.git, git::Message::DiscardConfirmed(files), cwd).map(move |msg| Message::DiscardGitFinished(paths.clone(), msg));
        }
        Message::DiscardGitFinished(paths, msg) => {
            if matches!(msg, git::Message::Discarded(Ok(()))) {
                for index in (0..state.tabs.len()).rev() {
                    let tab = &mut state.tabs[index];
                    if paths.iter().any(|(path, snapshot)| tab.path.as_ref() == Some(path) && tab.content.text() == *snapshot) {
                        if tab.path.as_ref().is_some_and(|p| !p.exists()) {
                            state.remove_tab(index);
                        } else { tab.dirty = false; }
                    }
                }
                task = Task::batch([state.reload_open_tabs(), state.reload_git_views()]);
                state.notify("Unstaged changes discarded");
            }
            if let git::Message::Discarded(Err(err)) = &msg { state.notify(format!("Git: {err}")); }
            let cwd = state.root_or_cwd();
            task = Task::batch([task, git::update(&mut state.git, msg, cwd).map(Message::Git)]);
        }
        Message::Git(msg) => {
            match &msg {
                git::Message::Committed(Ok(())) => state.notify("Commit created"),
                git::Message::Staged(Ok(())) => state.notify("Staging updated"),
                git::Message::Committed(Err(err)) | git::Message::Staged(Err(err)) | git::Message::Refreshed(Err(err)) => state.notify(format!("Git: {err}")),
                git::Message::MessageGenerated(Err(err)) => state.notify(format!("Could not write a commit message: {err}")),
                git::Message::BranchDone(Err(err)) | git::Message::BranchesLoaded(Err(err)) | git::Message::SyncDone(_, Err(err)) => state.notify(format!("Git: {err}")),
                git::Message::SyncDone(sync, Ok(())) => state.notify(sync.done()),
                _ => {},
            }
            let cwd = state.root_or_cwd();
            if matches!(msg, git::Message::Refresh) {
                task = state.reload_git_views();
            }
            // The checkout rewrote files: tabs without unsaved edits follow it, as after an AI edit.
            if matches!(msg, git::Message::BranchDone(Ok(()))) {
                state.notify("Switched branch");
                task = Task::batch([state.reload_open_tabs(), state.reload_git_views()]);
            }
            // A pull rewrites files too, and a failed one can leave conflict markers.
            if matches!(msg, git::Message::SyncDone(git::remote::Sync::Pull, _)) {
                task = Task::batch([state.reload_open_tabs(), state.reload_git_views()]);
            }
            task = Task::batch([task, git::update(&mut state.git, msg, cwd).map(Message::Git)]);
        }
        Message::GitPanelToggle => {
            if state.toggle_sidebar_mode(SidebarMode::Git) {
                let cwd = state.root_or_cwd();
                task = git::update(&mut state.git, git::Message::Refresh, cwd).map(Message::Git);
            }
        }
        Message::GitBlameLoaded(path, snapshot, blame) => {
            if let Some(tab) = state.tabs.iter_mut().find(|t| t.path.as_deref() == Some(path.as_path())) {
                if tab.content.text() == snapshot { tab.blame = blame; }
            }
        }
        Message::GitDiffLoaded(path, snapshot, diff) => {
            if let Some(tab) = state.tabs.iter_mut().find(|t| t.path.as_deref() == Some(path.as_path())) {
                if tab.content.text() == snapshot { tab.diff = diff.into_iter().collect(); }
            }
        }
        Message::TabSelected(i) => { state.show_editor(); state.active_tab = i; },
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
            state.lsp.shutdown_all();
            return iced::exit();
        }
        Message::LspInstall(id) => {
            state.lsp_prompt = None;
            if let Some(server) = lsp::registry::by_id(id) { state.lsp.install(server); }
        }
        Message::LspDismiss(id) => {
            state.lsp_prompt = None;
            if let Some(server) = lsp::registry::by_id(id) { state.lsp.dismiss(server); }
        }
        Message::EditorProbe(probe) => match probe {
            code_editor::Probe::Leave => {
                state.hover_request = None;
                if let Some(tab) = state.tabs.get_mut(state.active_tab) { tab.hover = None; }
            }
            code_editor::Probe::Hover { line, index, anchor } => {
                state.hover_request = Some((line, index, anchor));
                if let Some((path, column)) = state.lsp_position(line, index) { state.lsp.hover(&path, line, column); }
            }
            code_editor::Probe::Definition(line, index) => {
                if let Some((path, column)) = state.lsp_position(line, index) { state.lsp.definition(&path, line, column); }
            }
        },
        Message::GoToDefinition => {
            if let Some(cursor) = state.tabs.get(state.active_tab).map(|tab| tab.content.cursor) {
                if let Some((path, column)) = state.lsp_position(cursor.line, cursor.index) {
                    state.lsp.definition(&path, cursor.line, column);
                }
            }
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
                let mut tree = file_tree(root);
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
                    state.tree = Some(file_tree(path.clone()));
                    state.show_editor();
                    if let Some(tree) = &mut state.tree {
                        task = tree.update(DirectoryTreeEvent::Toggled(path.clone())).map(Message::Tree);
                    }
                    let changed = state.root.as_deref() != Some(path.as_path());
                    state.root = Some(path);
                    // Shells left in the previous project would run commands against it.
                    if changed {
                        let focused = state.terminal.focused();
                        state.terminal.shutdown();
                        if state.terminal_visible {
                            state.terminal.ensure_started(&state.root_or_cwd());
                            state.terminal.set_focused(focused);
                        }
                    }
                }
            }
            FileAction::CloseFolder => {
                state.show_editor();
                state.root = None;
                state.tree = None;
                state.terminal.shutdown();
                if state.terminal_visible { task = update(state, Message::TerminalToggle); }
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
            if let (Some(text), false) = (text, state.editor_hidden()) {
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

        // A drag in the tree ended over a folder; the tree leaves the move to us.
        Message::Tree(DirectoryTreeEvent::DragCompleted { sources, destination }) => {
            let name = destination.file_name().map_or_else(|| destination.display().to_string(), |name| name.to_string_lossy().into_owned());
            let what = match sources.as_slice() {
                [one] => one.file_name().map_or_else(|| "1 item".to_string(), |name| format!("\"{}\"", name.to_string_lossy())),
                many => format!("{} items", many.len()),
            };
            task = ask(state.window_handle, "Move", format!("Move {what} into \"{name}\"?"), Message::TreeMoveConfirmed(sources, destination));
        }
        Message::TreeMoveConfirmed(sources, destination) => {
            let outcome = move_entries(&sources, &destination);
            state.paths_moved(&outcome.moved);
            let mut notice = outcome.problems.clone();
            if !outcome.moved.is_empty() { notice.insert(0, format!("Moved {} item(s)", outcome.moved.len())); }
            if !notice.is_empty() { state.notify(notice.join("\n")); }
            let mut dirs: Vec<PathBuf> = outcome.moved.iter().filter_map(|(from, _)| from.parent().map(Path::to_path_buf)).collect();
            dirs.push(destination);
            dirs.sort();
            dirs.dedup();
            task = Task::batch(dirs.into_iter().map(refresh_dir_task));
        }
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
                        // Also tabs for files inside a renamed folder.
                        state.paths_moved(&[(old_path.clone(), new_path)]);
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
            // About is modal: Escape closes it and nothing reaches the panes behind it.
            if state.about.visible {
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    return update(state, Message::About(about::Message::Close));
                }
                return Task::none();
            }
            if modifiers.control() && key.as_ref() == keyboard::Key::Character("`") {
                return update(state, Message::TerminalToggle);
            }
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) && state.inline_edit.is_some()
                && !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible && !state.theme_install.visible {
                return update(state, Message::InlineEditCancel);
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
            if key == keyboard::Key::Named(keyboard::key::Named::F12) && state.focus == Focus::Editor {
                return update(state, Message::GoToDefinition);
            }
            // Completion keys come before the editor's own: Ctrl+Space (Ctrl on every platform)
            // asks for suggestions, and while the list shows it takes the arrows, Enter/Tab, Esc.
            let editing = state.focus == Focus::Editor && !state.editor_hidden() && !state.quick_open.visible
                && !state.command_palette.visible && !state.goto_line.visible && !state.theme_install.visible;
            if editing && modifiers.control() && key == keyboard::Key::Named(keyboard::key::Named::Space) {
                state.request_completion(None);
                return Task::none();
            }
            if editing && state.active_completion().is_some() {
                use keyboard::key::Named;
                let plain = !modifiers.shift() && !modifiers.command() && !modifiers.alt();
                match key.as_ref() {
                    keyboard::Key::Named(Named::ArrowDown) if plain => {
                        if let Some(session) = state.active_completion() { session.select(1); }
                        return Task::none();
                    }
                    keyboard::Key::Named(Named::ArrowUp) if plain => {
                        if let Some(session) = state.active_completion() { session.select(-1); }
                        return Task::none();
                    }
                    keyboard::Key::Named(Named::Enter | Named::Tab) if plain => return update(state, Message::CompletionAccept(None)),
                    keyboard::Key::Named(Named::Escape) => {
                        state.close_completion();
                        return Task::none();
                    }
                    _ => {}
                }
            }
            if modifiers.command() {
                match key.as_ref() {
                    keyboard::Key::Character(c) if modifiers.shift() && c.eq_ignore_ascii_case("t") => {
                        task = update(state, Message::ReopenClosedTab);
                    }
                    keyboard::Key::Character("q") => return update(state, Message::Exit),
                    keyboard::Key::Character("w") => {
                        // A diff opened from the graph closes back to the graph.
                        if state.git_preview.is_some() { state.git_preview = None; }
                        else if state.git_graph.is_some() { state.git_graph = None; }
                        else { task = state.close_tab(state.active_tab); }
                    }
                    keyboard::Key::Character(c) if c.len() == 1 && matches!(c.as_bytes()[0], b'1'..=b'9') => {
                        let index = (c.as_bytes()[0] - b'1') as usize;
                        if index < state.tabs.len() { state.active_tab = index; state.show_editor(); }
                    }
                    keyboard::Key::Character("s") => task = state.save(),
                    keyboard::Key::Character("k") if !state.editor_hidden() => task = update(state, Message::InlineEditStart),
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
                    keyboard::Key::Character("f") if !state.editor_hidden() => {
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
                    keyboard::Key::Character("g") if !state.editor_hidden() => {
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
                    keyboard::Key::Character("x") if !state.editor_hidden() => {
                        task = update(state, Message::EditAction(EditAction::Cut));
                    }
                    keyboard::Key::Character("c") if !state.editor_hidden() => {
                        task = update(state, Message::EditAction(EditAction::Copy));
                    }
                    keyboard::Key::Character("v") if !state.editor_hidden() => {
                        task = update(state, Message::EditAction(EditAction::Paste));
                    }
                    // Other command combos (undo/redo, ...) are the active editor's to handle.
                    _ => {
                        if state.handle_editor_key(&key, modifiers) { state.close_completion(); }
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
                } else if state.focus == Focus::Editor && state.handle_editor_key(&key, modifiers) {
                    state.after_editor_key(&key, modifiers);
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
        Message::AiModeSelected(mode) => {
            state.ai_mode = mode;
            state.refresh_mentions();
        }
        Message::Chat(chat::Message::Composer(action))
        | Message::Acp(acp::Message::Composer(action))
        | Message::Codex(acp::Message::Composer(action)) => {
            let next = match action {
                ai_composer::Action::Attach => Message::AiAttach,
                ai_composer::Action::Reference => Message::AiReference,
                ai_composer::Action::Remove(index) => Message::AiRemoveAttachment(index),
                ai_composer::Action::Paste => Message::AiPaste,
                ai_composer::Action::Mention(path) => {
                    let text = ai_mention::complete(&state.ai_input(), &path);
                    state.set_ai_input(&text);
                    return Task::none();
                }
                ai_composer::Action::Drop(paths) => {
                    if let Some(tree) = &mut state.tree {
                        let _ = tree.update(DirectoryTreeEvent::Drag(iced_swdir_tree::DragMsg::Cancelled));
                    }
                    Message::AiFiles(paths)
                }
            };
            return update(state, next);
        }
        Message::TreeDragReleased => {
            // Let the tree process its own release/click before clearing an
            // abandoned drag; raw subscriptions also see captured events.
            task = Task::done(Message::TreeDragCancel);
        }
        Message::TreeDragCancel => {
            if let Some(tree) = &mut state.tree {
                task = tree.update(DirectoryTreeEvent::Drag(iced_swdir_tree::DragMsg::Cancelled)).map(Message::Tree);
            }
        }
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
        Message::AiPaste => {
            let mode = state.ai_mode;
            let project = state.root_or_cwd();
            task = Task::perform(
                async { tokio::task::spawn_blocking(ai_context::Attachment::from_clipboard).await.unwrap_or_else(|e| Err(e.to_string())) },
                move |result| match result {
                    Ok(Some(image)) => Message::AiAttachmentsLoaded(mode, project.clone(), vec![Ok(image)]),
                    Ok(None) => Message::AiPasteText(None),
                    Err(error) => Message::AiAttachmentsLoaded(mode, project.clone(), vec![Err(error)]),
                },
            );
        }
        Message::AiPasteText(None) => task = iced::clipboard::read().map(|text| Message::AiPasteText(Some(text.unwrap_or_default()))),
        Message::AiPasteText(Some(text)) => {
            use iced::widget::text_editor;
            let paste = text_editor::Action::Edit(text_editor::Edit::Paste(std::sync::Arc::new(text)));
            let next = match state.ai_mode {
                AiMode::Http => Message::Chat(chat::Message::InputChanged(paste)),
                AiMode::Acp => Message::Acp(acp::Message::InputChanged(paste)),
                AiMode::Codex => Message::Codex(acp::Message::InputChanged(paste)),
            };
            return update(state, next);
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
        Message::AiAct(act) => task = state.ai_act(act),
        Message::InlineEditStart => task = state.start_inline_edit(),
        Message::InlineEditInput(text) => {
            if let Some(edit) = state.inline_edit.as_mut().filter(|edit| matches!(edit.stage, inline_edit::Stage::Prompt)) {
                edit.instruction = text;
            }
        }
        Message::InlineEditSubmit => task = state.submit_inline_edit(),
        Message::InlineEditReady(id, result) => {
            let root = state.root_or_cwd();
            let Some(edit) = state.inline_edit.as_mut().filter(|edit| matches!(edit.stage, inline_edit::Stage::Working(current) if current == id)) else {
                return Task::none();
            };
            match result {
                Ok(reply) => {
                    let replacement = inline_edit::replacement(&reply, &edit.original);
                    let name = edit.path.as_deref().and_then(Path::file_name).map_or_else(|| "Untitled".into(), |name| name.to_string_lossy().into_owned());
                    state.git_preview = Some(git_preview::Preview {
                        root, path: format!("{}{name}", inline_edit::PREVIEW_PREFIX),
                        result: Some(Ok(git_preview::text_patch(&edit.original, &replacement))), live: false,
                    });
                    edit.stage = inline_edit::Stage::Review(replacement);
                    state.focus = Focus::Editor;
                }
                Err(err) => {
                    edit.stage = inline_edit::Stage::Prompt;
                    state.notify(format!("AI edit failed: {err}"));
                }
            }
        }
        Message::InlineEditAccept => {
            if let Some(inline_edit::InlineEdit { path, start, end, original, stage: inline_edit::Stage::Review(replacement), .. }) = state.inline_edit.take() {
                state.git_preview = None;
                match state.tabs.iter().position(|tab| tab.path == path) {
                    Some(index) if state.tabs[index].content.text_between(start, end).as_deref() == Some(original.as_str()) => {
                        state.active_tab = index;
                        let before = state.tabs[index].content.undo_count();
                        state.tabs[index].content.replace_range(start, end, &replacement);
                        state.focus = Focus::Editor;
                        state.mark_edited_if_changed(before);
                    }
                    _ => state.notify("The code changed while the edit was being written; try again"),
                }
            }
        }
        Message::InlineEditCancel => {
            if state.inline_edit.take().is_some_and(|edit| matches!(edit.stage, inline_edit::Stage::Review(_))) {
                state.git_preview = None;
            }
        }
        Message::Acp(acp::Message::ViewDiff(entry, index)) => {
            let diff = state.acp.diff(entry, index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Codex(acp::Message::ViewDiff(entry, index)) => {
            let diff = state.codex.diff(entry, index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Chat(chat::Message::ViewDiff(index)) => {
            let diff = state.chat.diff(index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Chat(chat::Message::Markdown(ai_markdown::Action::Insert(code)))
        | Message::Acp(acp::Message::Markdown(ai_markdown::Action::Insert(code)))
        | Message::Codex(acp::Message::Markdown(ai_markdown::Action::Insert(code))) => state.insert_code(&code),
        Message::Codex(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, acp::Message::InputChanged(_) | acp::Message::UsePrompt(_));
            if matches!(msg, acp::Message::Send) { state.attach_mentions(); }
            task = acp::update(&mut state.codex, msg, cwd).map(Message::Codex);
            if input { state.refresh_mentions(); }
        }
        Message::Chat(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, chat::Message::InputChanged(_) | chat::Message::UsePrompt(_));
            if matches!(msg, chat::Message::Send) { state.attach_mentions(); }
            task = chat::update(&mut state.chat, msg, cwd).map(Message::Chat);
            if input { state.refresh_mentions(); }
        }
        Message::Acp(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, acp::Message::InputChanged(_) | acp::Message::UsePrompt(_));
            if matches!(msg, acp::Message::Send) { state.attach_mentions(); }
            task = acp::update(&mut state.acp, msg, cwd).map(Message::Acp);
            if input { state.refresh_mentions(); }
        }
        Message::Tick => {
            match about::native_menu_requested() {
                Some(about::NativeMenu::About) => task = about::update(&mut state.about, about::Message::Open).map(Message::About),
                Some(about::NativeMenu::CheckForUpdates) => task = update(state, Message::CheckUpdateFromMenu),
                None => {}
            }
            state.updater.poll();
            if state.announce_update_check && !state.updater.busy() {
                state.announce_update_check = false;
                let detail = state.updater.detail.clone();
                state.notify(detail);
            }
            state.terminal.poll();
            if state.notice.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(8)) { state.notice = None; }
            let cwd = state.root_or_cwd();
            state.lsp.set_root(&cwd);
            task = Task::batch([task, state.poll_lsp()]);
            state.chat.set_project(&cwd);
            state.acp.ensure_loaded(&cwd);
            state.codex.ensure_loaded(&cwd);
            // Only a Claude or Codex chat needs the shell PATH, so look it up once one is open.
            if state.ai_visible && matches!(state.ai_mode, AiMode::Acp | AiMode::Codex) { agent_launch::warm_up(); }
            // Connect as soon as the panel shows, so it names the model in use straight away.
            if state.ai_visible {
                match state.ai_mode {
                    AiMode::Acp => state.acp.connect_for_display(&cwd),
                    AiMode::Codex => state.codex.connect_for_display(&cwd),
                    _ => {}
                }
            }
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
            task = Task::batch([task, state.poll_git()]);
        }

        Message::WindowOpened(id) => {
            if !state.window_revealed {
                task = iced::window::raw_id::<Message>(id).map(move |raw| Message::PaintInitialWindow(id, raw));
            }
        }
        Message::PaintInitialWindow(id, raw) => {
            state.startup_window = Some(raw);
            state.window_handle = Some(raw);
            // Before the first paint, so the window never shows the system title bar.
            #[cfg(windows)]
            if let Err(err) = titlebar::install(raw) { state.notify(err); }
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
    // Closing or replacing the proposed edit's preview rejects it.
    let reviewing = state.git_preview.as_ref().is_some_and(|preview| !preview.live && preview.path.starts_with(inline_edit::PREVIEW_PREFIX));
    if !reviewing && state.inline_edit.as_ref().is_some_and(|edit| matches!(edit.stage, inline_edit::Stage::Review(_))) {
        state.inline_edit = None;
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
                    && !state.theme_install.visible && !state.about.visible).map(Message::Terminal),
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
    if let Some(server) = state.lsp_prompt {
        let prompt = container(column![
            text(format!("{} provides diagnostics for this file type but isn't installed.", server.name)).size(13),
            row![
                button(text("Install").size(12)).padding([6, 12]).on_press(Message::LspInstall(server.id)),
                button(text("Not now").size(12)).padding([6, 12]).style(flat_button_style).on_press(Message::LspDismiss(server.id)),
                text(format!("From github.com/{}", server.repo)).size(11).style(iced::widget::text::secondary),
            ].spacing(8).align_y(iced::Alignment::Center),
        ].spacing(10)).padding(12).max_width(440).style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            iced::widget::container::Style {
                background: Some(palette.background.weak.color.into()),
                border: iced::Border { color: palette.primary.weak.color, width: 1.0, radius: 6.0.into() },
                shadow: iced::Shadow { color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.25), offset: iced::Vector::new(0.0, 3.0), blur_radius: 12.0 },
                ..Default::default()
            }
        });
        base = iced::widget::stack![base, container(prompt).padding(16)
            .align_left(Length::Fill).align_bottom(Length::Fill)].into();
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
    } else if state.about.visible {
        iced::widget::stack![base, about::view(&state.about, &state.app_theme.iced).map(Message::About)].into()
    } else {
        base
    }
}

fn view_ai_sidebar(state: &State) -> Element<'_, Message> {
    let controls = match state.ai_mode {
        AiMode::Http => chat::conversation_controls(&state.chat).map(Message::Chat),
        AiMode::Codex => acp::conversation_controls(&state.codex).map(Message::Codex),
        AiMode::Acp => acp::conversation_controls(&state.acp).map(Message::Acp),
    };
    let mode_row = row![
        text("Assistant").size(14),
        iced::widget::pick_list([AiMode::Http, AiMode::Acp, AiMode::Codex], Some(state.ai_mode), Message::AiModeSelected).text_size(12),
        Space::new().width(Length::Fill),
        controls
    ].spacing(6).padding(8);
    let composer = ai_composer::Context {
        sources: state.tree.as_ref().map(|tree| tree.drag_sources()).unwrap_or_default(),
        can_reference: state.tabs.get(state.active_tab).is_some(),
        theme: &state.app_theme.iced,
        mentions: &state.mentions,
    };
    let panel: Element<'_, Message> = match state.ai_mode {
        AiMode::Http => chat::view(&state.chat, composer).map(Message::Chat),
        AiMode::Codex => acp::view(&state.codex, state.root_or_cwd(), composer).map(Message::Codex),
        AiMode::Acp => acp::view(&state.acp, state.root_or_cwd(), composer).map(Message::Acp),
    };

    column![mode_row, panel].height(Length::Fill).into()
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
        Command { label: "Git: Show Graph".to_string(), message: Message::GitGraphOpen },
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
        commands.push(Command { label: "Go to Definition (F12)".to_string(), message: Message::GoToDefinition });
        commands.push(Command { label: "AI: Edit Selection Inline (Cmd+K)".into(), message: Message::InlineEditStart });
        commands.push(Command { label: "AI: Explain Selection or File".into(), message: Message::AiAct(AiAct::Explain) });
        commands.push(Command { label: "AI: Refactor Selection or File".into(), message: Message::AiAct(AiAct::Refactor) });
        commands.push(Command { label: "AI: Fix Problem on This Line".into(), message: Message::AiAct(AiAct::Fix) });
    }
    commands.push(Command { label: "AI: Write Commit Message from Staged Changes".into(), message: Message::Git(git::Message::GenerateMessage) });

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

    let mut menus = menu_items!(
        (file_menu_button, menu_tpl(file_items)),
        (edit_menu_button, menu_tpl(edit_items)),
        (view_menu_button, menu_tpl(view_items)),
        (theme_menu_button, menu_tpl(theme_items))
    );
    let help_menu_button = menu_button("Help".to_string(), Message::Noop).width(Length::Shrink);
    let update_label = if state.updater.ready() { "Restart to Update" } else { "Check for Updates…" };
    let mut help_items = menu_items!(
        (menu_button_maybe(update_label.into(), (!state.updater.busy()).then_some(Message::CheckUpdateFromMenu))),
        (menu_button("File Bug Report".into(), Message::FileBugReport))
    );
    // macOS already has About in the native app menu.
    if !cfg!(target_os = "macos") {
        help_items.push(Item::new(menu_button("About".into(), Message::About(about::Message::Open))));
    }
    menus.push(Item::with_menu(help_menu_button, menu_tpl(help_items)));
    let mb = MenuBar::new(menus)
    .close_on_item_click_global(true)
    .close_on_background_click(true)
    .close_on_background_click_global(true);

    let project = state.root.as_ref().and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Editor".into());
    // On Windows this row is the title bar: its empty middle drags the window (see `titlebar`).
    let custom = titlebar::custom();
    let caption = titlebar::caption(row![
        Space::new().width(Length::Fill),
        text(project).size(12).style(iced::widget::text::secondary),
    ].align_y(iced::Alignment::Center).width(Length::Fill).height(if custom { Length::Fill } else { Length::Shrink }));
    let bar = container(row![
        mb,
        caption,
        icon_control(lucide_icons::Icon::Search, "Find a file", Some(Message::ToggleQuickOpen), false),
        icon_control(lucide_icons::Icon::Command, "Command palette", Some(Message::ToggleCommandPalette), false),
    ].spacing(4).align_y(iced::Alignment::Center).height(if custom { Length::Fill } else { Length::Shrink }))
        .padding(if custom { [0, 6] } else { [2, 6] }).width(Length::Fill);
    let bar = row![bar, titlebar::buttons(titlebar::maximized())]
        .align_y(iced::Alignment::Center)
        .height(if custom { Length::Fixed(titlebar::HEIGHT) } else { Length::Shrink });
    container(bar).style(chrome_style).into()
}

fn view_status_bar(state: &State) -> Element<'_, Message> {
    let mut bar = row![
        icon_control(lucide_icons::Icon::Folder, "Toggle files", Some(Message::SidebarToggle), state.sidebar_visible && state.sidebar_mode == SidebarMode::Tree),
        icon_control(lucide_icons::Icon::GitBranch, "Toggle source control", Some(Message::GitPanelToggle), state.sidebar_visible && state.sidebar_mode == SidebarMode::Git),
        icon_control(lucide_icons::Icon::Search, "Search project", Some(Message::ToggleProjectSearch), state.sidebar_visible && state.sidebar_mode == SidebarMode::ProjectSearch),
        Space::new().width(Length::Fill),
    ].spacing(6);

    if state.updater.enabled {
        let action = if state.updater.ready() { Message::RestartUpdate } else { Message::CheckUpdate };
        let mut control = button(text(&state.updater.label).size(12)).style(flat_button_style);
        if !state.updater.busy() { control = control.on_press(action); }
        bar = bar.push(iced::widget::tooltip(
            control, text(&state.updater.detail).size(12), iced::widget::tooltip::Position::Top,
        ));
    }

    if let Some(tab) = state.tabs.get(state.active_tab) {
        let (line, col) = tab.content.cursor_line_col();
        let language = state.highlighter.language_name(&tab.extension());
        bar = bar.push(text(format!("Ln {line}, Col {col}")).size(12));
        bar = bar.push(text(tab.line_ending.to_string()).size(12).style(iced::widget::text::secondary));
        bar = bar.push(text(language.to_string()).size(12).style(iced::widget::text::secondary));
        // The most severe diagnostic under the cursor, plus the file's error/warning totals.
        let errors = tab.diagnostics.iter().filter(|d| d.severity == lsp::Severity::Error).count();
        let warnings = tab.diagnostics.iter().filter(|d| d.severity == lsp::Severity::Warning).count();
        if errors + warnings > 0 {
            bar = bar.push(text(format!("✕ {errors}  ⚠ {warnings}")).size(12).style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                iced::widget::text::Style { color: Some(if errors > 0 { palette.danger.base.color } else { palette.background.base.text }) }
            }));
        }
        if let Some(diagnostic) = tab.diagnostics.iter().find(|d| d.line + 1 == line) {
            let message: String = diagnostic.message.lines().next().unwrap_or_default().chars().take(120).collect();
            let is_error = diagnostic.severity == lsp::Severity::Error;
            bar = bar.push(text(message).size(12).style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                iced::widget::text::Style { color: Some(if is_error { palette.danger.base.color } else { palette.background.base.text }) }
            }));
            bar = bar.push(icon_control(lucide_icons::Icon::Wand, "Fix with AI", Some(Message::AiAct(AiAct::Fix)), false));
        }
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
    match state.sidebar_mode {
        SidebarMode::Tree => view_tree(state),
        SidebarMode::ProjectSearch => project_search::view(&state.project_search, state.root.as_deref()).map(Message::ProjectSearch),
        SidebarMode::Git => git::view(&state.git, state.git_preview.as_ref().map(|preview| preview.path.as_str())).map(Message::Git),
    }
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
        let is_active = !state.editor_hidden() && i == state.active_tab;
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
        let mut header = row![
            text(preview.path.clone()).size(14),
            text(git_preview::summary(preview)).size(12).style(iced::widget::text::secondary),
            button("×").style(flat_button_style).on_press(Message::CloseGitPreview),
            Space::new().width(Length::Fill),
        ].spacing(12).padding(8).align_y(iced::Alignment::Center);
        if state.inline_edit.as_ref().is_some_and(|edit| matches!(edit.stage, inline_edit::Stage::Review(_))) {
            header = header
                .push(button(text("Accept").size(13)).padding([6, 14]).style(iced::widget::button::success).on_press(Message::InlineEditAccept))
                .push(button(text("Reject").size(13)).padding([6, 14]).style(flat_button_style).on_press(Message::InlineEditCancel));
        }
        let header = header.push(button(if state.ai_visible { "Hide AI panel" } else { "Show AI panel" }).style(flat_button_style).on_press(Message::AiToggle));
        return column![
            scrollable(tab_row).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())),
            header,

            git_preview::view(preview, &state.app_theme.iced, Message::GitPreviewScrolled),
        ].width(Length::Fill).height(Length::Fill).into();
    }
    if let Some(graph) = &state.git_graph {
        return column![
            scrollable(tab_row).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())),
            git_graph::view(graph).map(Message::GitGraph),
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
            crumbs = crumbs.push(Space::new().width(Length::Fill));
            if tab.is_markdown() {
                let previewing = tab.preview.is_some();
                crumbs = crumbs.push(icon_control(lucide_icons::Icon::Eye, if previewing { "Show source" } else { "Open preview" },
                    Some(Message::MarkdownPreviewToggle), previewing));
            }
            crumbs = crumbs.push(icon_control(lucide_icons::Icon::Folder, "Reveal in files", Some(Message::RevealPath(path.clone())), false));
            editor_column = editor_column.push(scrollable(crumbs.padding([0, 8]))
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::default())));
        }
        if let Some(content) = tab.preview_content() {
            let document = ai_markdown::view_document(content, &state.app_theme.iced, Message::MarkdownPreview);
            editor_column = editor_column.push(scrollable(container(container(document).max_width(860))
                .center_x(Length::Fill).padding([16, 24])).height(Length::Fill));
            return editor_frame(tab_row, editor_column);
        }
        if tab.search.visible {
            editor_column = editor_column.push(code_editor::search::view(&tab.search).map(Message::Search));
        }
        if let Some(edit) = state.inline_edit.as_ref().filter(|edit| edit.path == tab.path) {
            let working = matches!(edit.stage, inline_edit::Stage::Working(_));
            let sparkles: char = lucide_icons::Icon::Sparkles.into();
            let placeholder = if edit.original.is_empty() { "Describe the code to insert here…" } else { "Describe how to change the selected code…" };
            let action: Element<'_, Message> = if working {
                text("Writing…").size(12).style(iced::widget::text::secondary).into()
            } else {
                button(text("Generate").size(12)).padding([4, 10])
                    .on_press_maybe((!edit.instruction.trim().is_empty()).then_some(Message::InlineEditSubmit)).into()
            };
            editor_column = editor_column.push(container(row![
                text(sparkles).font(iced::Font::with_name("lucide")).size(14),
                text_input(placeholder, &edit.instruction).id(inline_edit::INPUT_ID).size(13).padding([4, 8])
                    .on_input_maybe((!working).then_some(Message::InlineEditInput)).on_submit(Message::InlineEditSubmit),
                action,
                icon_control(lucide_icons::Icon::X, "Cancel (Esc)", Some(Message::InlineEditCancel), false),
            ].spacing(8).align_y(iced::Alignment::Center)).padding([4, 8]).style(chrome_style));
        }
        let editor = code_editor::code_editor(
            &tab.content,
            &tab.diff,
            &tab.diagnostics,
            &tab.blame,
            &state.app_theme.editor,
            state.zoom,
            Message::EditorAction,
            Message::ToggleFold,
            Message::EditorProbe,
        );
        // Hover text as a real widget over the canvas; see `code_editor::Hover` for why.
        // The stack is always present: wrapping the canvas only while a popup shows would
        // change the widget tree's shape and reset the canvas state, scroll included.
        let mut layers = iced::widget::stack![editor];
        if let Some(hover) = &tab.hover {
                let mut lines = column![].spacing(2);
                for line in &hover.lines {
                    lines = lines.push(text(line.clone()).size(12).font(iced::Font::MONOSPACE)
                        .wrapping(iced::widget::text::Wrapping::None));
                }
                let popup = container(lines).padding([6, 10]).max_width(640).style(|theme: &iced::Theme| {
                    let palette = theme.extended_palette();
                    iced::widget::container::Style {
                        background: Some(palette.background.weak.color.into()),
                        border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 6.0.into() },
                        shadow: iced::Shadow { color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.3), offset: iced::Vector::new(0.0, 2.0), blur_radius: 8.0 },
                        ..Default::default()
                    }
                });
                let offset = iced::Padding { top: hover.anchor.y.max(0.0) + 2.0, left: hover.anchor.x.max(0.0), right: 0.0, bottom: 0.0 };
                layers = layers.push(container(popup).padding(offset));
        }
        // Completions below the cursor, drawn over the canvas like the hover popup.
        if let Some(session) = state.completion.as_ref()
            .filter(|session| tab.path.as_deref() == Some(session.path.as_path()) && session.at_cursor(&tab.content))
        {
            if let Some(anchor) = code_editor::caret_anchor(&tab.content, &state.app_theme.editor, state.zoom) {
                layers = layers.push(completion::view(session, anchor, |index| Message::CompletionAccept(Some(index))));
            }
        }
        let editor: Element<'_, Message> = layers.into();
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
                iced::widget::rule::horizontal(1),
                menu_button("Edit with AI… (Cmd+K)".into(), Message::InlineEditStart),
                menu_button(if has_selection { "Explain Selection with AI" } else { "Explain File with AI" }.into(), Message::AiAct(AiAct::Explain)),
                menu_button(if has_selection { "Refactor Selection with AI…" } else { "Refactor File with AI…" }.into(), Message::AiAct(AiAct::Refactor)),
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

    editor_frame(tab_row, editor_column)
}

fn editor_frame<'a>(tab_row: iced::widget::Row<'a, Message>, editor_column: iced::widget::Column<'a, Message>) -> Element<'a, Message> {
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
    let drag_cleanup = iced::event::listen_raw(|event, _, _| match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => Some(Message::TreeDragReleased),
        iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::TreeDragCancel),
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape), .. }) => Some(Message::TreeDragCancel),
        _ => None,
    });
    Subscription::batch([keys, tick, window_events, first_frame, drag_cleanup])
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

/// What moving files into a folder did: `(from, to)` for each move, and why others were skipped.
#[derive(Debug, Default, PartialEq)]
struct MoveOutcome {
    moved: Vec<(PathBuf, PathBuf)>,
    problems: Vec<String>,
}

/// Moves `sources` into `destination` by renaming, never overwriting. Items already there are
/// left alone, and so is anything inside another moved folder (it moves with that folder).
fn move_entries(sources: &[PathBuf], destination: &Path) -> MoveOutcome {
    let mut outcome = MoveOutcome::default();
    for source in sources {
        if sources.iter().any(|other| other != source && source.starts_with(other)) { continue; }
        if source.parent() == Some(destination) { continue; }
        let Some(name) = source.file_name() else { continue };
        let target = destination.join(name);
        let label = name.to_string_lossy();
        if std::fs::symlink_metadata(&target).is_ok() {
            outcome.problems.push(format!("{label} already exists there"));
            continue;
        }
        match std::fs::rename(source, &target) {
            Ok(()) => outcome.moved.push((source.clone(), target)),
            Err(err) => outcome.problems.push(format!("Could not move {label}: {err}")),
        }
    }
    outcome
}

/// Where `path` is after `from` moved to `to`: itself, or a file inside a moved folder.
fn moved_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    path.strip_prefix(from).ok().map(|rest| if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
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

/// The file tree for a project root: hidden entries shown, the user's exclude list applied.
fn file_tree(root: PathBuf) -> DirectoryTree {
    DirectoryTree::new(root)
        .with_filter(DirectoryFilter::AllIncludingHidden)
        .with_exclude(load_tree_exclude())
        .with_icon_theme(std::sync::Arc::new(file_icons::FileIcons))
}

const DEFAULT_TREE_EXCLUDE: &str = ".git\n.DS_Store\nThumbs.db\n";

/// Names the file tree never shows, one per line in `file_tree_exclude`.
fn load_tree_exclude() -> Vec<String> {
    match config_path("file_tree_exclude") {
        Some(path) => tree_exclude_at(&path),
        None => parse_tree_exclude(DEFAULT_TREE_EXCLUDE),
    }
}

/// Reads the exclude list at `path`, writing the defaults there first when the file does not
/// exist yet so users can find and edit it. A file that exists but can't be read is left alone.
fn tree_exclude_at(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_tree_exclude(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, DEFAULT_TREE_EXCLUDE);
            parse_tree_exclude(DEFAULT_TREE_EXCLUDE)
        }
        Err(_) => parse_tree_exclude(DEFAULT_TREE_EXCLUDE),
    }
}

fn parse_tree_exclude(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect()
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
/// Pause in typing before the active tab's diff markers are recomputed.
const GUTTER_DEBOUNCE: Duration = Duration::from_millis(150);

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

/// Computes diff markers for `text`, the buffer of `path`, against its staged version,
/// producing `Message::GitDiffLoaded`. A no-op outside the open project (`root`). A free
/// function rather than a `State` method so callers already holding a `&mut` borrow of part
/// of `state` (e.g. iterating `&mut self.tabs`) can still call it without a borrow conflict.
fn load_diff_task(repo: Option<std::sync::Arc<git::repo::Repo>>, root: Option<&Path>, path: PathBuf, text: String) -> Task<Message> {
    let Some(relative) = root.and_then(|root| path.strip_prefix(root).ok()).map(Path::to_path_buf) else {
        return Task::none();
    };
    let root = root.unwrap().to_owned();
    let blame_path = path.clone();
    let blame_text = text.clone();
    let blame_relative = relative.clone();
    let blame = Task::perform(async move {
        let snapshot = blame_text.clone();
        let result = tokio::task::spawn_blocking(move || git::blame::load(&root, &blame_relative, &blame_text)).await.unwrap_or_default();
        Message::GitBlameLoaded(blame_path, snapshot, result)
    }, |message| message);
    let snapshot = text.clone();
    Task::batch([blame, Task::perform(git_diff::diff_for_buffer(repo, relative, text), move |diff| {
        Message::GitDiffLoaded(path.clone(), snapshot.clone(), diff)
    })])
}

fn load_preview_task(repo: Option<std::sync::Arc<git::repo::Repo>>, root: PathBuf, path: String) -> Task<Message> {
    Task::perform(git_preview::load(repo, root.clone(), path.clone()), move |result| {
        Message::GitPreviewLoaded(root.clone(), path.clone(), result)
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

pub(crate) fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    // Release notes are remote content; hand only web links to the OS.
    if url.starts_with("https://") || url.starts_with("http://") {
        let _ = command.arg(url).spawn();
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
    if updater::run_helper() { return Ok(()); }
    let _update_guard = updater::running_guard();
    let icon = iced::window::icon::from_file_data(include_bytes!("../icon.png"), None).ok();
    iced::application(State::boot, update_and_sync_preview, view)
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
mod tree_move_tests {
    use super::*;

    #[test]
    fn dragged_items_move_into_the_folder_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let dest = root.join("dest");
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        for file in ["a.txt", "clash.txt", "src/lib.rs", "src/nested/x.rs", "dest/clash.txt", "dest/kept.txt"] {
            std::fs::write(root.join(file), file).unwrap();
        }
        let sources = ["a.txt", "src", "src/nested/x.rs", "clash.txt", "dest/kept.txt"].map(|path| root.join(path));
        let outcome = move_entries(&sources, &dest);
        assert_eq!(outcome.moved, [(root.join("a.txt"), dest.join("a.txt")), (root.join("src"), dest.join("src"))]);
        assert_eq!(outcome.problems, ["clash.txt already exists there"]);
        assert_eq!(std::fs::read_to_string(dest.join("src/nested/x.rs")).unwrap(), "src/nested/x.rs", "moved with its folder");
        assert_eq!(std::fs::read_to_string(dest.join("clash.txt")).unwrap(), "dest/clash.txt", "never overwritten");
        assert!(root.join("clash.txt").exists());
        assert!(dest.join("kept.txt").exists(), "already in the folder: left alone");
        assert!(!root.join("a.txt").exists() && !root.join("src").exists());

        let gone = move_entries(&[root.join("missing.txt")], &dest);
        assert!(gone.moved.is_empty() && gone.problems.len() == 1);
    }

    #[test]
    fn open_paths_follow_moved_files_and_folders() {
        let (from, to) = (Path::new("/p/src"), Path::new("/p/lib/src"));
        assert_eq!(moved_path(Path::new("/p/src"), from, to), Some(PathBuf::from("/p/lib/src")));
        assert_eq!(moved_path(Path::new("/p/src/a/b.rs"), from, to), Some(PathBuf::from("/p/lib/src/a/b.rs")));
        assert_eq!(moved_path(Path::new("/p/srcx/b.rs"), from, to), None, "a similar name is not inside");
        assert_eq!(moved_path(Path::new("/p/other.rs"), from, to), None);
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

#[cfg(test)]
mod saved_state_tests {
    use super::*;

    #[test]
    fn tree_exclude_file_is_created_once_then_read_as_edited() {
        let dir = std::env::temp_dir().join(format!("editor-tree-exclude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config").join("file_tree_exclude");

        // First run: the file is written with the defaults, which are used.
        assert_eq!(tree_exclude_at(&path), [".git", ".DS_Store", "Thumbs.db"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_TREE_EXCLUDE);

        // After that the user's edits win; blank lines and surrounding spaces are ignored.
        std::fs::write(&path, "node_modules\r\n\n  target  \n").unwrap();
        assert_eq!(tree_exclude_at(&path), ["node_modules", "target"]);

        // An emptied file means nothing is excluded, and is not refilled with the defaults.
        std::fs::write(&path, "").unwrap();
        assert!(tree_exclude_at(&path).is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markdown_preview_follows_the_buffer() {
        use iced::widget::markdown::Item;
        let tab_for = |path: Option<&str>, text: &str| Tab {
            path: path.map(PathBuf::from),
            content: code_editor::Buffer::new(text, code_editor::metrics_for_zoom(1.0)),
            saved_text: Some(text.into()),
            dirty: false,
            search: Default::default(),
            line_ending: LineEnding::Lf,
            diff: Default::default(),
            diagnostics: Vec::new(), blame: Vec::new(), hover: None, preview: None,
        };
        assert!(tab_for(Some("docs/README.md"), "").is_markdown());
        assert!(tab_for(Some("NOTES.Markdown"), "").is_markdown());
        assert!(!tab_for(Some("main.rs"), "").is_markdown());
        assert!(!tab_for(Some("md"), "").is_markdown());
        assert!(!tab_for(None, "").is_markdown());

        let mut tab = tab_for(Some("README.md"), "# Title");
        tab.sync_preview();
        assert!(tab.preview_content().is_none());
        tab.preview = Some(Default::default());
        tab.sync_preview();
        assert!(matches!(tab.preview_content().unwrap().items(), [Item::Heading(..)]));
        tab.content.goto(0, 0);
        tab.content.perform(cosmic_text::Action::Delete);
        tab.content.perform(cosmic_text::Action::Delete);
        tab.sync_preview();
        assert_eq!(tab.content.text(), "Title");
        assert!(matches!(tab.preview_content().unwrap().items(), [Item::Paragraph(..)]));
    }

    #[test]
    fn undo_and_redo_compare_text_with_the_latest_save() {
        let mut tab = Tab {
            path: None,
            content: code_editor::Buffer::new("x;", code_editor::metrics_for_zoom(1.0)),
            saved_text: Some("x;".into()),
            dirty: false,
            search: Default::default(),
            line_ending: LineEnding::Lf,
            diff: Default::default(),
            diagnostics: Vec::new(), blame: Vec::new(), hover: None, preview: None,
        };
        tab.content.goto(0, 2);
        tab.content.perform(cosmic_text::Action::Backspace);
        tab.refresh_dirty();
        assert!(tab.dirty);
        tab.content.undo();
        tab.refresh_dirty();
        assert!(!tab.dirty);
        tab.content.redo();
        tab.refresh_dirty();
        assert!(tab.dirty);
        tab.saved_text = Some(tab.content.text());
        tab.refresh_dirty();
        assert!(!tab.dirty);
        tab.content.undo();
        tab.refresh_dirty();
        assert!(tab.dirty);
        tab.content.redo();
        tab.refresh_dirty();
        assert!(!tab.dirty);
        // Matching saved text through a new edit must also clear the indicator.
        tab.content.perform(cosmic_text::Action::Insert(';'));
        tab.content.perform(cosmic_text::Action::Backspace);
        tab.refresh_dirty();
        assert!(!tab.dirty);
    }
}
