#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod about;
mod scrollbar;
mod agent_launch;
mod claude_agent;
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
mod ai_select;
mod ai_selectable;
mod inline_edit;
mod lsp;
mod acp;
mod grok_auth;
mod cursor_agent;
mod cursor_auth;
mod codex_auth;
mod claude_auth;
mod terminal;
mod chat;
mod code_editor;
mod command_palette;
mod git;
mod file_icons;
mod fonts;
mod git_diff;
mod titlebar;
mod completion;
mod git_graph;
mod git_preview;
mod image_preview;
mod goto_line;
mod project_search;
mod quick_open;
mod recent_files;
mod recent_projects;
mod theme;
mod theme_install;
mod workspaces;

use iced::keyboard;
use iced::widget::{
    button, column, container, pane_grid, row, text, text_input, PaneGrid, Space,
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
    /// The changes those markers mark, loaded with them.
    hunks: Vec<git_diff::Hunk>,
    /// The change whose old and new lines are shown after a click on its marker.
    diff_popup: Option<git_diff::Hunk>,
    /// Latest diagnostics from the file's language server, sorted by line.
    diagnostics: Vec<lsp::Diagnostic>,
    blame: Vec<String>,
    /// Hover popup currently shown in the editor, if any.
    hover: Option<code_editor::Hover>,
    /// Rendered Markdown shown instead of the editor, while the preview is on.
    preview: Option<ai_markdown::Cache>,
    image: Option<image_preview::Preview>,
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

    #[cfg(test)]
    fn preview_content(&self) -> Option<&iced::widget::markdown::Content> {
        self.preview.as_ref().and_then(|preview| preview.get(0))
    }

    fn refresh_dirty(&mut self) {
        if self.image.is_some() { self.dirty = false; return; }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
    ToggleWordWrap,
}

impl std::fmt::Display for ViewAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            ViewAction::ZoomIn => "Zoom In (Cmd+=)",
            ViewAction::ZoomOut => "Zoom Out (Cmd+-)",
            ViewAction::ZoomReset => "Reset Zoom (Cmd+0)",
            ViewAction::ToggleWordWrap => "Toggle Word Wrap (Alt+Z)",
        };
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
enum AiMode {
    #[default]
    Http,
    Acp,
    Codex,
    Grok,
    Cursor,
}

impl std::fmt::Display for AiMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { Self::Http => "Local / Ollama / API", Self::Acp => "Claude ACP", Self::Codex => "Codex ACP", Self::Grok => "Grok ACP", Self::Cursor => "Cursor ACP" })
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
    /// A file or folder dropped on the window outside the AI composer: a file opens in a tab,
    /// a folder opens as the project.
    PathDropped(PathBuf),
    DiscardGitConfirmed(PathBuf, Vec<git::ChangedFile>),
    DiscardGitFinished(Vec<(PathBuf, String)>, git::Message),
    GitPanelToggle,
    GitPreviewLoaded(PathBuf, String, Result<String, String>),
    CloseGitPreview,
    ImageLoaded(u64, Result<image_preview::Decoded, String>),
    GitPreviewScrolled(bool, f32),
    GitGraphOpen,
    GitGraph(git_graph::Message),
    GitGraphPatchLoaded(String, Result<String, String>),
    GitDiffLoaded(PathBuf, String, (Vec<(usize, git_diff::LineStatus)>, Vec<git_diff::Hunk>)),
    /// The diff marker on this line of the active tab was clicked: show its change's popup.
    DiffMarkerClicked(usize),
    DiffPopupClose,
    /// Put the popup's change back to the staged lines.
    DiffRestore,
    PassphraseInput(String),
    RememberPassphrase(bool),
    /// OK (`true`) or Cancel (`false`) on the passphrase dialog.
    PassphraseDone(bool),
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
    /// The editor widget's room for text and its height changed.
    EditorViewport(f32, f32),
    /// Go to the definition of the symbol under the caret (F12).
    GoToDefinition,
    Codex(acp::Message),
    Grok(acp::Message),
    Cursor(acp::Message),
    Chat(chat::Message),
    Acp(acp::Message),
    Tick,

    WindowResized(iced::window::Id, iced::Size),
    WindowFrameDrawn(iced::window::Id),
    WindowOpened(iced::window::Id),
    PaintInitialWindow(iced::window::Id, u64),
    WindowMoved(iced::Point),
    WindowMaximizedChecked(bool),

    /// `Message` for the workspace tab `WorkspaceId`, even if it is in the background by now:
    /// what an async job, dialog or timer started for a project reports back to that project.
    In(WorkspaceId, Box<Message>),
    /// The per-project half of `Tick`, sent to every loaded workspace tab.
    WorkspaceTick,
    WorkspaceNew,
    /// Next (`true`) or previous workspace tab, wrapping around.
    WorkspaceCycle(bool),
    WorkspaceClose(WorkspaceId),
    WorkspaceCloseOthers(WorkspaceId),
    WorkspaceCloseRight(WorkspaceId),
    /// Closes these tabs, unsaved edits and all (answered "Yes", or nothing was unsaved).
    WorkspacesCloseConfirmed(Vec<WorkspaceId>),
    WorkspaceCopyPath(WorkspaceId),
    WorkspaceReveal(WorkspaceId),
    /// Pressed on a workspace tab: selects it and may start dragging it.
    WorkspaceDragStart(WorkspaceId),
    /// The pointer moved over the tab strip while a tab may be dragged: its x from the start
    /// of the tabs, and the tabs' width.
    WorkspaceDragMove(f32, f32),
    /// Open this folder: switch to its tab, open it in an empty one, or ask.
    OpenFolderRequest(PathBuf),
    /// The answer to "replace the current project or open a new tab?"; `None` cancels.
    FolderPromptChoice(Option<FolderChoice>),
    /// Replace the project in this tab with the folder (answered "Yes" to losing unsaved edits).
    FolderReplaceConfirmed(WorkspaceId, PathBuf),
}

impl Message {
    /// Messages about the app or the tab strip rather than one project. They are handled
    /// where the user is, never inside a background tab.
    fn is_global(&self) -> bool {
        matches!(self,
            Message::CheckUpdate | Message::CheckUpdateFromMenu | Message::RestartUpdate | Message::About(_)
            | Message::FileBugReport | Message::ThemeInstall(_) | Message::CommandPalette(_) | Message::ToggleCommandPalette
            | Message::QuickOpen(_) | Message::ToggleQuickOpen | Message::GotoLine(_) | Message::ToggleGotoLine
            | Message::AppThemeSelected(_) | Message::ViewAction(_) | Message::DismissNotice | Message::Exit
            | Message::KeyPressed(..) | Message::Noop | Message::Tick | Message::PathDropped(_)
            | Message::WindowResized(..) | Message::WindowFrameDrawn(_) | Message::WindowOpened(_)
            | Message::PaintInitialWindow(..) | Message::WindowMoved(_) | Message::WindowMaximizedChecked(_)
            | Message::In(..) | Message::WorkspaceNew | Message::WorkspaceCycle(_)
            | Message::WorkspaceClose(_) | Message::WorkspaceCloseOthers(_) | Message::WorkspaceCloseRight(_)
            | Message::WorkspacesCloseConfirmed(_) | Message::WorkspaceCopyPath(_) | Message::WorkspaceReveal(_)
            | Message::WorkspaceDragStart(_) | Message::WorkspaceDragMove(..) | Message::OpenFolderRequest(_)
            | Message::FolderPromptChoice(_) | Message::FolderReplaceConfirmed(..))
    }
}

/// Addresses what `task` produces to workspace `id`, except app-wide messages.
fn tag(task: Task<Message>, id: WorkspaceId) -> Task<Message> {
    task.map(move |message| if message.is_global() { message } else { Message::In(id, Box::new(message)) })
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct RecoveryBuffer {
    path: Option<PathBuf>,
    text: String,
}

/// One workspace tab as saved in `session.json` (see `workspaces::SessionFile`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct EditorSession {
    root: Option<PathBuf>,
    tabs: Vec<PathBuf>,
    active: Option<PathBuf>,
    #[serde(default)]
    recovery: Vec<RecoveryBuffer>,
    /// When the active file is untitled (so `active` can't name it): which of the untitled
    /// drafts in `recovery` it is, counting from 0. Absent in older sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_untitled: Option<usize>,
    /// The tab's panels and their sizes. Absent in sessions saved before workspace tabs, which
    /// open with the saved defaults like before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<Layout>,
}

/// Which panels a workspace tab shows and how they are sized.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct Layout {
    sidebar_visible: bool,
    sidebar_mode: SidebarMode,
    ai_visible: bool,
    ai_mode: AiMode,
    terminal_visible: bool,
    sidebar_ratio: f32,
    ai_ratio: f32,
}

impl Layout {
    /// What a new tab starts with: the panels and sizes last saved, the file tree, the first AI
    /// assistant, no terminal. The same as startup before workspace tabs.
    fn saved_defaults() -> Self {
        Self {
            sidebar_visible: load_sidebar_visible(),
            sidebar_mode: SidebarMode::default(),
            ai_visible: load_ai_visible(),
            ai_mode: AiMode::default(),
            terminal_visible: false,
            sidebar_ratio: load_sidebar_ratio(),
            ai_ratio: load_ai_ratio(),
        }
    }
}

type WorkspaceId = u64;

/// Everything that belongs to one project: its files, panels, language servers, terminals,
/// source control and AI conversations. Each workspace tab has one.
struct Workspace {
    id: WorkspaceId,
    root: Option<PathBuf>,
    closed_tabs: Vec<PathBuf>,
    git_status: Option<(String, std::time::Instant)>,
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

    panes: pane_grid::State<PaneKind>,
    sidebar_split: Option<pane_grid::Split>,
    ai_split: Option<pane_grid::Split>,
    sidebar_visible: bool,
    /// The sidebar's and AI panel's share of the width, for showing them again.
    sidebar_ratio: f32,
    ai_ratio: f32,

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
    grok: acp::AcpState,
    cursor: acp::AcpState,
    terminal: terminal::panel::Panel,
    terminal_visible: bool,
    /// A terminal printed output while this tab was in the background; cleared on showing it.
    terminal_activity: bool,
}

impl Workspace {
    /// A workspace for `root` (none: a "New Tab") laid out as `layout`. The terminal starts
    /// hidden; `State::restore_layout` opens it when the layout says so.
    fn new(id: WorkspaceId, root: Option<PathBuf>, layout: Layout) -> Self {
        let tree = root.clone().map(file_tree);
        // No sidebar without a folder (see `State::sidebar_shown`); the setting is kept.
        let sidebar = layout.sidebar_visible && root.is_some();
        let (mut panes, first_pane) = if sidebar {
            pane_grid::State::new(PaneKind::Sidebar)
        } else {
            pane_grid::State::new(PaneKind::Main)
        };

        let (main_pane, sidebar_split) = if sidebar {
            let (main_pane, split) = panes
                .split(pane_grid::Axis::Vertical, first_pane, PaneKind::Main)
                .expect("splitting a freshly created single-pane state always succeeds");
            panes.resize(split, layout.sidebar_ratio);
            (main_pane, Some(split))
        } else {
            (first_pane, None)
        };

        let ai_split = if layout.ai_visible {
            let split = panes
                .split(pane_grid::Axis::Vertical, main_pane, PaneKind::Ai)
                .map(|(_, split)| split);
            if let Some(split) = split {
                panes.resize(split, layout.ai_ratio);
            }
            split
        } else {
            None
        };

        Self {
            id,
            root,
            closed_tabs: Vec::new(),
            git_status: None,
            reveal_generation: 0,
            reveal_queue: std::collections::VecDeque::new(),
            reveal_target: None,
            tabs: Vec::new(),
            active_tab: 0,
            focus: Focus::Editor,
            tree,
            renaming: None,
            creating: None,
            sidebar_mode: layout.sidebar_mode,
            project_search: project_search::SearchState::default(),
            git: git::GitState::new(),
            git_preview: None,
            git_graph: None,
            gutter_due: None,
            lsp: lsp::Manager::default(),
            lsp_prompt: None,
            hover_request: None,
            completion: None,
            completion_request: None,
            panes,
            sidebar_split,
            ai_split,
            sidebar_visible: layout.sidebar_visible,
            sidebar_ratio: layout.sidebar_ratio,
            ai_ratio: layout.ai_ratio,
            ai_visible: layout.ai_visible,
            ai_mode: layout.ai_mode,
            mention_files: None,
            mentions: Vec::new(),
            inline_edit: None,
            inline_generation: 0,
            chat: chat::ChatState::new(),
            acp: acp::AcpState::claude(),
            codex: acp::AcpState::codex(),
            grok: acp::AcpState::grok(),
            cursor: acp::AcpState::cursor(),
            terminal: terminal::panel::Panel::default(),
            terminal_visible: false,
            terminal_activity: false,
        }
    }

    fn layout(&self) -> Layout {
        Layout {
            sidebar_visible: self.sidebar_visible,
            sidebar_mode: self.sidebar_mode,
            ai_visible: self.ai_visible,
            ai_mode: self.ai_mode,
            terminal_visible: self.terminal_visible,
            sidebar_ratio: self.sidebar_ratio,
            ai_ratio: self.ai_ratio,
        }
    }

    /// What `session.json` keeps for this tab.
    fn session(&self) -> EditorSession {
        EditorSession {
            root: self.root.clone(),
            tabs: self.tabs.iter().filter_map(|tab| tab.path.clone()).collect(),
            active: self.tabs.get(self.active_tab).and_then(|tab| tab.path.clone()),
            recovery: self.recovery(),
            active_untitled: self.tabs.get(self.active_tab).filter(|tab| tab.path.is_none() && tab.dirty).map(|_| {
                self.tabs[..self.active_tab].iter().filter(|tab| tab.path.is_none() && tab.dirty).count()
            }),
            layout: Some(self.layout()),
        }
    }

    fn recovery(&self) -> Vec<RecoveryBuffer> {
        self.tabs.iter().filter(|tab| tab.dirty).map(|tab| RecoveryBuffer {
            path: tab.path.clone(), text: tab.content.text(),
        }).collect()
    }

    fn unsaved(&self) -> usize {
        self.tabs.iter().filter(|tab| tab.dirty).count()
    }

    /// An AI assistant in this tab is writing a reply.
    fn ai_busy(&self) -> bool {
        self.chat.busy() || [&self.acp, &self.codex, &self.grok, &self.cursor].iter().any(|panel| panel.busy())
    }

    /// Stops what the project runs and saves its AI conversations, before the tab goes away.
    /// Terminals and language servers also stop when dropped; this makes it explicit.
    fn shut_down(&mut self) {
        self.terminal.shutdown();
        self.lsp.shutdown_all();
        self.chat.close();
        for panel in [&mut self.acp, &mut self.codex, &mut self.grok, &mut self.cursor] {
            panel.close();
        }
    }
}

/// Where a workspace tab's state is: in `State::ws` (the one being worked on), set aside
/// while another is, or not loaded yet since startup (only its saved session is known).
enum Slot {
    Here,
    Parked(Box<Workspace>),
    Unloaded(EditorSession),
}

struct Entry {
    id: WorkspaceId,
    slot: Slot,
}

/// Where a folder the user picked should open, once they answered the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderChoice {
    Replace,
    NewTab,
}

/// What the app reads from disk at startup, gathered so tests can build a `State` without
/// touching the user's settings.
struct Settings {
    themes: theme::ThemeRegistry,
    app_theme: theme::EditorTheme,
    zoom: f32,
    word_wrap: bool,
    recent_files: Vec<PathBuf>,
    session_store: SessionStore,
    /// Where the recent projects list is saved; `None` keeps it in memory.
    recent_projects_path: Option<PathBuf>,
}

/// Where `session.json` is written. `Disk(None)` when there's no home folder: nothing is saved,
/// as before. `Memory` (tests) keeps it in `State::saved_session` only.
enum SessionStore {
    Disk(Option<PathBuf>),
    #[cfg_attr(not(test), allow(dead_code))]
    Memory,
}

impl Settings {
    fn load() -> Self {
        let themes = theme::ThemeRegistry::load();
        let app_theme = load_app_theme(&themes);
        Self {
            themes,
            app_theme,
            zoom: load_zoom(),
            word_wrap: load_word_wrap(),
            recent_files: recent_files::load(),
            session_store: SessionStore::Disk(config_path("session.json")),
            recent_projects_path: recent_projects::path(),
        }
    }
}

struct State {
    updater: updater::Updater,
    /// A menu-requested update check is running; its outcome is shown as a notice.
    announce_update_check: bool,
    saved_session: workspaces::SessionFile,
    last_session_write: std::time::Instant,
    session_store: SessionStore,
    notice: Option<(String, std::time::Instant)>,

    /// The workspace being worked on: the active tab's, except while a message for a
    /// background tab is handled (see `route`).
    ws: Box<Workspace>,
    /// Every workspace tab in strip order; the one at `current` is `Slot::Here`.
    workspaces: Vec<Entry>,
    /// The tab shown.
    active: usize,
    /// The tab whose workspace is in `ws`; equal to `active` outside `route`.
    current: usize,
    next_workspace_id: WorkspaceId,
    /// A folder waiting for the user to choose between replacing the current project and a new tab.
    folder_prompt: Option<PathBuf>,
    /// SSH key passphrase prompts from fetch, pull and push, the first one shown.
    passphrase_prompts: std::collections::VecDeque<git::askpass::Request>,
    passphrase: String,
    remember_passphrase: bool,
    /// The workspace tab being dragged, and whether the pointer has moved since the press.
    workspace_drag: Option<(WorkspaceId, bool)>,
    recent_projects: Vec<PathBuf>,
    recent_projects_path: Option<PathBuf>,

    quick_open: quick_open::QuickOpenState,
    theme_install: theme_install::ThemeInstallState,
    command_palette: command_palette::PaletteState,
    goto_line: goto_line::GotoLineState,
    about: about::AboutState,
    recent_files: Vec<PathBuf>,

    app_theme: theme::EditorTheme,
    themes: theme::ThemeRegistry,
    highlighter: code_editor::Highlighter,
    zoom: f32,
    /// Long lines continue on further rows instead of running off to the right.
    word_wrap: bool,

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
}

impl State {
    /// A state whose only workspace is `session`'s, not restored yet (see `boot`).
    fn with_settings(settings: Settings, first: &EditorSession) -> Self {
        let layout = first.layout.unwrap_or_else(Layout::saved_defaults);
        let recent_projects = recent_projects::load(settings.recent_projects_path.as_deref());
        Self {
            updater: updater::Updater::default(),
            announce_update_check: false,
            saved_session: workspaces::SessionFile { workspaces: Vec::new(), active_workspace: 0 },
            last_session_write: std::time::Instant::now() - Duration::from_secs(1),
            session_store: settings.session_store,
            notice: None,
            ws: Box::new(Workspace::new(1, first.root.clone(), layout)),
            workspaces: vec![Entry { id: 1, slot: Slot::Here }],
            active: 0,
            current: 0,
            next_workspace_id: 2,
            folder_prompt: None,
            passphrase_prompts: Default::default(),
            passphrase: String::new(),
            remember_passphrase: false,
            workspace_drag: None,
            recent_projects,
            recent_projects_path: settings.recent_projects_path,
            quick_open: quick_open::QuickOpenState::default(),
            theme_install: theme_install::ThemeInstallState::default(),
            command_palette: command_palette::PaletteState::default(),
            goto_line: goto_line::GotoLineState::default(),
            about: about::AboutState::default(),
            recent_files: settings.recent_files,
            app_theme: settings.app_theme,
            themes: settings.themes,
            highlighter: code_editor::Highlighter::new(),
            zoom: settings.zoom,
            word_wrap: settings.word_wrap,
            last_known_size: load_window_size(),
            window_revealed: !cfg!(windows),
            startup_maximized: load_window_maximized(),
            startup_window: None,
            window_handle: None,
            started_at: std::time::Instant::now(),
        }
    }

    fn boot() -> (Self, Task<Message>) {
        let settings = Settings::load();
        let bytes = match &settings.session_store {
            SessionStore::Disk(Some(path)) => std::fs::read(path).ok(),
            _ => None,
        };
        let session = workspaces::read_session(bytes.as_deref(), load_last_project());
        Self::start(settings, session)
    }

    /// Opens every saved workspace tab: the active one now, the others when first shown.
    fn start(settings: Settings, session: workspaces::SessionFile) -> (Self, Task<Message>) {
        let active = session.active_workspace;
        let mut state = Self::with_settings(settings, &session.workspaces[active]);
        state.workspaces.clear();
        for (index, saved) in session.workspaces.iter().enumerate() {
            let id = if index == active { state.ws.id } else { state.new_workspace_id() };
            let slot = if index == active { Slot::Here } else { Slot::Unloaded(saved.clone()) };
            state.workspaces.push(Entry { id, slot });
        }
        state.active = active;
        state.current = active;
        let task = state.restore(session.workspaces[active].clone());
        let task = tag(task, state.ws.id);
        state.persist_session();
        (state, task)
    }

    fn new_workspace_id(&mut self) -> WorkspaceId {
        let id = self.next_workspace_id;
        self.next_workspace_id += 1;
        id
    }

    /// Fills the workspace in `ws`, freshly created for `session`'s folder and layout, with
    /// the session's files and unsaved edits, and opens its terminal if it was open.
    fn restore(&mut self, session: EditorSession) -> Task<Message> {
        let mut tasks = Vec::new();
        if let (Some(tree), Some(root)) = (&mut self.ws.tree, &self.ws.root) {
            tasks.push(tree.update(DirectoryTreeEvent::Toggled(root.clone())).map(Message::Tree));
        }
        // Projects restored from the last session count as opened, so they stay findable
        // among the recent projects once their tab is closed.
        if let Some(root) = self.ws.root.clone() {
            recent_projects::record(&mut self.recent_projects, root, self.recent_projects_path.as_deref());
        }
        if session.root == self.ws.root {
            for path in &session.tabs {
                tasks.push(self.open_path(path.clone()));
            }
            for recovery in session.recovery {
                let extension = recovery.path.as_ref().and_then(|path| path.extension()).and_then(|ext| ext.to_str()).unwrap_or("txt");
                let mut content = code_editor::Buffer::highlighted(&recovery.text, code_editor::metrics_for_zoom(self.zoom), &self.highlighter, extension, &self.app_theme.syntax);
                content.set_wrap(self.word_wrap);
                if let Some(path) = &recovery.path { self.ws.lsp.open(path, recovery.text.clone()); }
                let mut tab = Tab {
                    path: recovery.path.clone(), content, dirty: true,
                    saved_text: recovery.path.as_ref().and_then(|path| std::fs::read_to_string(path).ok())
                        .map(|text| text.replace("\r\n", "\n").replace('\r', "\n")),
                    search: Default::default(), line_ending: LineEnding::detect(&recovery.text), diff: Default::default(), hunks: Vec::new(), diff_popup: None,
                    diagnostics: Vec::new(), blame: Vec::new(), hover: None, preview: None, image: None,
                };
                tab.refresh_dirty();
                // A file's edits go into its reopened tab. Untitled drafts have no path to
                // match by, so each gets its own tab.
                let reopened = recovery.path.as_ref().and_then(|_| self.ws.tabs.iter().position(|tab| tab.path == recovery.path));
                if let Some(index) = reopened {
                    self.ws.tabs[index] = tab;
                } else { self.ws.tabs.push(tab); }
                self.notify("Recovered unsaved edits");
            }
            self.ws.tabs.sort_by_key(|tab| session.tabs.iter().position(|path| tab.path.as_ref() == Some(path)).unwrap_or(usize::MAX));
            let active = match (&session.active, session.active_untitled) {
                // The untitled draft that was active, among the untitled tabs in saved order.
                (None, Some(nth)) => self.ws.tabs.iter().enumerate().filter(|(_, tab)| tab.path.is_none()).nth(nth).map(|(index, _)| index),
                _ => self.ws.tabs.iter().position(|tab| tab.path == session.active),
            };
            if let Some(index) = active {
                self.ws.active_tab = index;
            }
        }
        if session.layout.is_some_and(|layout| layout.terminal_visible) {
            // A fresh shell: the old one ended with the app.
            self.show_terminal(false);
        }
        Task::batch(tasks)
    }

    fn notify(&mut self, message: impl Into<String>) {
        let message = message.into();
        // A notice from a tab in the background says which project it is about.
        let message = if self.in_background() { format!("[{}] {message}", self.workspace_label(self.current)) } else { message };
        self.notice = Some((message, std::time::Instant::now()));
    }

    /// A message for a background tab is being handled: nothing may take focus or change
    /// what the user is looking at.
    fn in_background(&self) -> bool {
        self.current != self.active
    }

    fn workspace_root(&self, index: usize) -> Option<&Path> {
        match &self.workspaces.get(index)?.slot {
            Slot::Here => self.ws.root.as_deref(),
            Slot::Parked(workspace) => workspace.root.as_deref(),
            Slot::Unloaded(session) => session.root.as_deref(),
        }
    }

    fn workspace_roots(&self) -> Vec<Option<&Path>> {
        (0..self.workspaces.len()).map(|index| self.workspace_root(index)).collect()
    }

    fn workspace_label(&self, index: usize) -> String {
        workspaces::labels(&self.workspace_roots()).into_iter().nth(index).unwrap_or_default()
    }

    fn workspace_index(&self, id: WorkspaceId) -> Option<usize> {
        self.workspaces.iter().position(|entry| entry.id == id)
    }

    /// The loaded workspace of tab `index`, wherever it is.
    fn workspace(&self, index: usize) -> Option<&Workspace> {
        match &self.workspaces.get(index)?.slot {
            Slot::Here => Some(&self.ws),
            Slot::Parked(workspace) => Some(workspace),
            Slot::Unloaded(_) => None,
        }
    }

    /// Unsaved files in tab `index`, counting the unsaved edits of a tab not loaded yet.
    fn workspace_unsaved(&self, index: usize) -> usize {
        match self.workspaces.get(index).map(|entry| &entry.slot) {
            Some(Slot::Unloaded(session)) => session.recovery.len(),
            Some(_) => self.workspace(index).map_or(0, Workspace::unsaved),
            None => 0,
        }
    }

    /// Puts tab `index`'s workspace in `ws` (loading it first if needed) and sets the one
    /// that was there aside. Doesn't change which tab is shown.
    fn swap_to(&mut self, index: usize) -> Task<Message> {
        if index == self.current || index >= self.workspaces.len() { return Task::none(); }
        let mut task = Task::none();
        let incoming = match std::mem::replace(&mut self.workspaces[index].slot, Slot::Here) {
            Slot::Parked(workspace) => workspace,
            Slot::Unloaded(session) => {
                let layout = session.layout.unwrap_or_else(Layout::saved_defaults);
                let workspace = Box::new(Workspace::new(self.workspaces[index].id, session.root.clone(), layout));
                let outgoing = std::mem::replace(&mut self.ws, workspace);
                self.workspaces[self.current].slot = Slot::Parked(outgoing);
                self.current = index;
                task = tag(self.restore(session), self.ws.id);
                return task;
            }
            Slot::Here => unreachable!("only the current tab is here"),
        };
        let outgoing = std::mem::replace(&mut self.ws, incoming);
        self.workspaces[self.current].slot = Slot::Parked(outgoing);
        self.current = index;
        task
    }

    /// Adds a tab at the end of the strip and shows it: empty ("New Tab"), or with `root` open.
    fn new_workspace(&mut self, root: Option<PathBuf>) -> Task<Message> {
        let id = self.new_workspace_id();
        let workspace = Workspace::new(id, None, Layout::saved_defaults());
        self.workspaces.push(Entry { id, slot: Slot::Parked(Box::new(workspace)) });
        let mut task = self.activate_workspace(self.workspaces.len() - 1);
        if let Some(root) = root {
            task = Task::batch([task, tag(open_folder(self, root), id)]);
        }
        task
    }

    /// Replaces the shown tab's project with the folder at `path`, keeping the tab's place
    /// and layout. The old project is closed completely, unsaved edits included. The tab
    /// gets a new id, so replies still on their way to the old project are dropped.
    fn replace_workspace(&mut self, path: PathBuf) -> Task<Message> {
        let layout = self.ws.layout();
        let focused = self.ws.terminal.focused();
        let id = self.new_workspace_id();
        let mut old = std::mem::replace(&mut self.ws, Box::new(Workspace::new(id, None, Layout { terminal_visible: false, ..layout })));
        old.shut_down();
        self.workspaces[self.current].id = id;
        let task = open_folder(self, path);
        if layout.terminal_visible {
            self.show_terminal(focused);
        }
        tag(task, id)
    }

    /// Asks before closing tabs with unsaved edits, then closes them.
    fn request_close(&mut self, ids: Vec<WorkspaceId>) -> Task<Message> {
        if ids.is_empty() { return Task::none(); }
        let unsaved: usize = ids.iter().filter_map(|id| self.workspace_index(*id)).map(|index| self.workspace_unsaved(index)).sum();
        if unsaved == 0 {
            return update(self, Message::WorkspacesCloseConfirmed(ids));
        }
        let what = match ids.as_slice() {
            [id] => format!("\"{}\"", self.workspace_index(*id).map(|index| self.workspace_label(index)).unwrap_or_default()),
            many => format!("{} tabs", many.len()),
        };
        ask(self.window_handle, "Unsaved changes", format!("Discard unsaved changes in {unsaved} file(s) and close {what}?"), Message::WorkspacesCloseConfirmed(ids))
    }

    /// Closes tab `id`: its terminals, language servers, source control and AI sessions stop,
    /// and its unsaved edits are dropped. The last tab isn't removed but emptied, which shows
    /// the welcome screen with recent projects.
    fn close_workspace(&mut self, id: WorkspaceId) -> Task<Message> {
        let Some(index) = self.workspace_index(id) else { return Task::none() };
        if self.workspaces.len() == 1 {
            self.reset_workspace();
            return Task::none();
        }
        let mut task = Task::none();
        if index == self.active {
            // The tab to the right takes its place, or the one to the left at the end.
            let next = if index + 1 < self.workspaces.len() { index + 1 } else { index - 1 };
            task = self.activate_workspace(next);
        }
        let entry = self.workspaces.remove(index);
        if let Slot::Parked(mut workspace) = entry.slot { workspace.shut_down(); }
        if self.active > index { self.active -= 1; }
        if self.current > index { self.current -= 1; }
        task
    }

    /// Empties the shown tab: no folder, no files, no terminal, fresh AI and source control.
    fn reset_workspace(&mut self) {
        let layout = Layout { terminal_visible: false, ..self.ws.layout() };
        let id = self.new_workspace_id();
        let mut old = std::mem::replace(&mut self.ws, Box::new(Workspace::new(id, None, layout)));
        old.shut_down();
        self.workspaces[self.current].id = id;
    }

    /// Moves the tab at `from` to `to`, keeping the same tab shown.
    fn move_workspace(&mut self, from: usize, to: usize) {
        if from >= self.workspaces.len() || to >= self.workspaces.len() { return; }
        let (active, current) = (self.workspaces[self.active].id, self.workspaces[self.current].id);
        let entry = self.workspaces.remove(from);
        self.workspaces.insert(to, entry);
        self.active = self.workspace_index(active).unwrap_or(0);
        self.current = self.workspace_index(current).unwrap_or(0);
    }

    /// Runs `apply` on every loaded workspace, the one in `ws` included.
    fn for_each_loaded(&mut self, mut apply: impl FnMut(&mut Workspace)) {
        apply(&mut self.ws);
        for entry in &mut self.workspaces {
            if let Slot::Parked(workspace) = &mut entry.slot { apply(workspace); }
        }
    }

    /// Shows tab `index`, loading it the first time.
    fn activate_workspace(&mut self, index: usize) -> Task<Message> {
        if index >= self.workspaces.len() { return Task::none(); }
        // Project-bound overlays would act on the wrong project after the switch.
        if index != self.active {
            let _ = quick_open::update(&mut self.quick_open, quick_open::Message::Close, None, &[]);
            let _ = command_palette::update(&mut self.command_palette, command_palette::Message::Close);
            let _ = goto_line::update(&mut self.goto_line, goto_line::Message::Close);
        }
        self.active = index;
        let task = self.swap_to(index);
        self.ws.terminal_activity = false;
        if let Some(tab) = self.ws.tabs.get_mut(self.ws.active_tab) { tab.sync_preview(); }
        task
    }

    fn git_info(&mut self, message: impl Into<String>) {
        self.ws.git_status = Some((message.into(), std::time::Instant::now()));
    }

    fn notify_git(&mut self, message: &git::Message) {
        match git_notification(message) {
            Some(Ok(info)) => self.git_info(info),
            Some(Err(error)) => self.notify(error),
            None => {}
        }
    }

    /// Every workspace tab as `session.json` keeps it. Tabs not loaded since startup keep
    /// exactly what was read for them.
    fn session(&self) -> workspaces::SessionFile {
        workspaces::SessionFile {
            workspaces: self.workspaces.iter().map(|entry| match &entry.slot {
                Slot::Here => self.ws.session(),
                Slot::Parked(workspace) => workspace.session(),
                Slot::Unloaded(session) => session.clone(),
            }).collect(),
            active_workspace: self.active,
        }
    }

    /// The unsaved edits of every tab, as the session file must hold them before quitting.
    fn all_recovery(session: &workspaces::SessionFile) -> Vec<&RecoveryBuffer> {
        session.workspaces.iter().flat_map(|workspace| &workspace.recovery).collect()
    }

    fn persist_session(&mut self) {
        if self.last_session_write.elapsed() < Duration::from_millis(500) { return; }
        self.last_session_write = std::time::Instant::now();
        let session = self.session();
        if session == self.saved_session { return; }
        let path = match &self.session_store {
            SessionStore::Memory => {
                self.saved_session = session;
                return;
            }
            SessionStore::Disk(None) => return,
            SessionStore::Disk(Some(path)) => path.clone(),
        };
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(bytes) = serde_json::to_vec(&session) {
            match write_session(&path, &bytes) {
                Ok(()) => self.saved_session = session,
                Err(err) => self.notify(format!("Could not save recovery: {err}")),
            }
        }
    }

    /// Whether every tab's unsaved edits are in the saved session, so quitting loses nothing.
    fn recovery_saved(&mut self) -> bool {
        self.last_session_write = std::time::Instant::now() - Duration::from_secs(1);
        self.persist_session();
        let session = self.session();
        Self::all_recovery(&session) == Self::all_recovery(&self.saved_session)
    }

    /// Shows the terminal panel below the editor, starting a shell if none is open.
    fn show_terminal(&mut self, focus: bool) {
        self.ws.terminal_visible = true;
        self.ws.terminal.ensure_started(&self.root_or_cwd());
        self.ws.terminal.set_focused(focus);
        let main = self.ws.panes.iter().find(|(_, kind)| **kind == PaneKind::Main).map(|(pane, _)| *pane);
        if let Some(main) = main {
            if let Some((_, split)) = self.ws.panes.split(pane_grid::Axis::Horizontal, main, PaneKind::Terminal) {
                self.ws.panes.resize(split, 0.7);
            }
        }
    }

    fn root_or_cwd(&self) -> PathBuf {
        self.ws.root.clone().unwrap_or_else(|| PathBuf::from("."))
    }

    /// Re-inserts the sidebar pane (to Main's left) if it isn't already showing, and marks it
    /// visible/persisted. Safe to call when the sidebar is already visible -- it's a no-op.
    fn show_sidebar(&mut self) {
        if self.ws.sidebar_visible {
            return;
        }
        self.ws.sidebar_visible = true;
        save_sidebar_visible(true);
        self.sync_sidebar();
    }

    /// The sidebar shows when it is switched on and the tab has a folder: everything in it
    /// (files, project search, source control) needs one. Without a folder the welcome screen
    /// gets the whole width, and the on/off setting is kept for when a folder opens.
    fn sidebar_shown(&self) -> bool {
        self.ws.sidebar_visible && self.ws.root.is_some()
    }

    /// Adds or removes the sidebar pane so it matches `sidebar_shown`.
    fn sync_sidebar(&mut self) {
        let sidebar_pane = self.ws.panes.iter().find(|(_, kind)| **kind == PaneKind::Sidebar).map(|(pane, _)| *pane);
        match (self.sidebar_shown(), sidebar_pane) {
            (true, None) => {
                let main_pane = self.ws.panes.iter().find(|(_, kind)| **kind == PaneKind::Main).map(|(pane, _)| *pane);
                if let Some(main_pane) = main_pane {
                    if let Some((sidebar_pane, split)) =
                        self.ws.panes.split(pane_grid::Axis::Vertical, main_pane, PaneKind::Sidebar)
                    {
                        // `split` always inserts the new pane after the target, i.e. to Main's
                        // right; swap them so the sidebar ends up on the left.
                        self.ws.panes.swap(main_pane, sidebar_pane);
                        self.ws.panes.resize(split, self.ws.sidebar_ratio);
                        self.ws.sidebar_split = Some(split);
                    }
                }
            }
            (false, Some(sidebar_pane)) => {
                self.ws.panes.close(sidebar_pane);
                self.ws.sidebar_split = None;
            }
            _ => {}
        }
    }

    fn hide_sidebar(&mut self) {
        self.ws.sidebar_visible = false;
        save_sidebar_visible(false);
        let sidebar_pane = self.ws.panes.iter().find(|(_, kind)| **kind == PaneKind::Sidebar).map(|(pane, _)| *pane);
        if let Some(sidebar_pane) = sidebar_pane {
            self.ws.panes.close(sidebar_pane);
        }
        self.ws.sidebar_split = None;
    }

    /// Single entry point for every sidebar-mode button (tree/git/project-search): clicking
    /// the button for the panel that's already showing collapses the sidebar (matching the
    /// familiar "activity bar" pattern); clicking any other button switches to that panel,
    /// opening the sidebar first if it was closed. Returns `true` if `mode` ended up visible
    /// (as opposed to the sidebar collapsing), so callers can decide whether to e.g. refresh.
    /// Without a folder there is nothing to show, so nothing happens.
    fn toggle_sidebar_mode(&mut self, mode: SidebarMode) -> bool {
        if self.ws.root.is_none() {
            return false;
        }
        if self.ws.sidebar_visible && self.ws.sidebar_mode == mode {
            self.hide_sidebar();
            false
        } else {
            self.ws.sidebar_mode = mode;
            self.show_sidebar();
            true
        }
    }

    /// The path the context menu should act on: the tree's current selection, or the root.
    fn context_target_path(&self) -> Option<PathBuf> {
        self.ws.tree
            .as_ref()
            .and_then(|t| t.selected_path())
            .map(Path::to_path_buf)
            .or_else(|| self.ws.root.clone())
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
        let repo = self.ws.git.repo();
        for tab in &mut self.ws.tabs {
            if tab.dirty {
                continue;
            }
            let Some(path) = tab.path.clone() else { continue };
            if let Some(preview) = &mut tab.image {
                *preview = image_preview::Preview::new();
                tasks.push(load_image_task(path, preview.request));
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text != tab.content.text() {
                    let extension = tab.extension();
                    tab.content = code_editor::Buffer::highlighted(&text, code_editor::metrics_for_zoom(self.zoom), &self.highlighter, &extension, &self.app_theme.syntax);
                    tab.content.set_wrap(self.word_wrap);
                    tab.blame.clear();
                    self.ws.lsp.changed(&path);
                    tasks.push(load_diff_task(repo.clone(), self.ws.root.as_deref(), path, text));
                }
                tab.saved_text = Some(tab.content.text());
            }
        }
        Task::batch(tasks)
    }

    /// Points open tabs at their new paths after files or folders moved; unsaved edits stay.
    fn paths_moved(&mut self, moves: &[(PathBuf, PathBuf)]) {
        for tab in &mut self.ws.tabs {
            let Some(path) = &tab.path else { continue };
            if let Some(new_path) = moves.iter().find_map(|(from, to)| moved_path(path, from, to)) {
                tab.path = Some(new_path);
            }
        }
    }

    /// Read-only views must not receive text editing or saving actions.
    fn editor_hidden(&self) -> bool {
        self.ws.git_preview.is_some() || self.ws.git_graph.is_some()
            || self.ws.tabs.get(self.ws.active_tab).is_some_and(|tab| tab.image.is_some())
    }

    /// Uncovers the editor: closes the diff view and the commit graph.
    fn show_editor(&mut self) {
        self.ws.git_preview = None;
        self.ws.git_graph = None;
    }

    fn open_path(&mut self, path: PathBuf) -> Task<Message> {
        self.show_editor();
        if let Some(index) = self
            .ws.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path.as_path()))
        {
            self.ws.active_tab = index;
            recent_files::record(&mut self.recent_files, path.clone());
            return Task::none();
        }
        let image = image_preview::supported(&path).then(image_preview::Preview::new);
        let text = if image.is_some() { String::new() } else {
            match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => { self.notify(format!("Could not open {}: {err}", path.display())); return Task::none(); }
            }
        };
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("txt")
            .to_string();
        let line_ending = LineEnding::detect(&text);
        let mut content = code_editor::Buffer::highlighted(&text, code_editor::metrics_for_zoom(self.zoom), &self.highlighter, &extension, &self.app_theme.syntax);
        content.set_wrap(self.word_wrap);
        recent_files::record(&mut self.recent_files, path.clone());
        self.ws.lsp.set_root(&self.root_or_cwd());
        let task = if let Some(preview) = &image {
            load_image_task(path.clone(), preview.request)
        } else {
            self.ws.lsp.open(&path, text.clone());
            load_diff_task(self.ws.git.repo(), self.ws.root.as_deref(), path.clone(), text)
        };
        self.ws.tabs.push(Tab {
            path: Some(path),
            saved_text: Some(content.text()),
            content,
            search: code_editor::search::SearchState::default(),
            dirty: false,
            line_ending,
            diff: std::collections::HashMap::new(), hunks: Vec::new(), diff_popup: None,
            diagnostics: Vec::new(), blame: Vec::new(),
            hover: None,
            preview: None, image,
        });
        self.ws.active_tab = self.ws.tabs.len() - 1;
        task
    }

    fn close_tab(&mut self, index: usize) -> Task<Message> {
        if index >= self.ws.tabs.len() {
            return Task::none();
        }
        if self.ws.tabs[index].dirty {
            return ask(self.window_handle, "Unsaved changes", "Discard unsaved changes?".into(), Message::CloseTabConfirmed(index));
        }
        self.remove_tab(index);
        Task::none()
    }

    fn close_tabs(&mut self, anchor: usize, scope: TabCloseScope) -> Task<Message> {
        let indices = tabs_to_close(self.ws.tabs.len(), anchor, scope);
        let unsaved = indices.iter().filter(|&&index| self.ws.tabs[index].dirty).count();
        if unsaved > 0 {
            return ask(self.window_handle, "Unsaved changes", format!("Discard unsaved changes in {unsaved} tab(s) and close the selected tabs?"), Message::CloseTabsConfirmed(anchor, scope));
        }
        self.remove_tabs(anchor, scope);
        Task::none()
    }

    fn remove_tabs(&mut self, anchor: usize, scope: TabCloseScope) {
        // Descending indices keep the remaining targets and active tab stable.
        for index in tabs_to_close(self.ws.tabs.len(), anchor, scope) { self.remove_tab(index); }
    }

    fn remove_tab(&mut self, index: usize) {
        if let Some(path) = self.ws.tabs[index].path.clone() {
            self.ws.lsp.closed(&path);
            self.ws.closed_tabs.push(path);
        }
        self.ws.tabs.remove(index);
        if self.ws.tabs.is_empty() {
            self.ws.active_tab = 0;
        } else if self.ws.active_tab > index || self.ws.active_tab >= self.ws.tabs.len() {
            self.ws.active_tab = self.ws.active_tab.saturating_sub(1).min(self.ws.tabs.len() - 1);
        }
    }

    fn save(&mut self) -> Task<Message> {
        if self.editor_hidden() { return Task::none(); }
        let Some(tab) = self.ws.tabs.get_mut(self.ws.active_tab) else {
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
                if was_untitled { self.ws.lsp.open(&path, text); } else { self.ws.lsp.saved(&path, text); }
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
        let Some(tab) = self.ws.tabs.get_mut(self.ws.active_tab) else {
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
        let Some(tab) = self.ws.tabs.get_mut(self.ws.active_tab) else {
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
        if let Some(path) = &tab.path { self.ws.lsp.changed(path); }
        self.ws.gutter_due = Some(std::time::Instant::now() + GUTTER_DEBOUNCE);
    }

    /// Sends debounced edits to language servers and applies what they sent back.
    fn poll_lsp(&mut self) -> Task<Message> {
        for path in self.ws.lsp.take_due() {
            if let Some(tab) = self.ws.tabs.iter().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                let text = tab.content.text();
                self.ws.lsp.change(&path, text);
            }
        }
        let mut tasks = Vec::new();
        for event in self.ws.lsp.poll() {
            match event {
                lsp::Event::Diagnostics(path, diagnostics) => {
                    if let Some(tab) = self.ws.tabs.iter_mut().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                        tab.diagnostics = diagnostics;
                    }
                }
                lsp::Event::Notice(text) => self.notify(text),
                lsp::Event::Prompt(server) => self.ws.lsp_prompt = Some(server),
                // Installed from this tab: the other tabs' files that need it get it too.
                lsp::Event::Installed(server, binary) => {
                    for entry in &mut self.workspaces {
                        let Slot::Parked(workspace) = &mut entry.slot else { continue };
                        workspace.lsp.installed(server, binary.clone());
                        if workspace.lsp_prompt.is_some_and(|prompt| prompt.id == server.id) { workspace.lsp_prompt = None; }
                    }
                }
                lsp::Event::Hover { path, line, column, lines } => {
                    if lines.is_empty() { continue; }
                    if let Some(tab) = self.ws.tabs.iter_mut().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                        let index = tab.content.inner.lines.get(line).map_or(0, |l| lsp::utf16_to_byte(l.text(), column));
                        if let Some((_, _, anchor)) = self.ws.hover_request.filter(|(l, i, _)| (*l, *i) == (line, index)) {
                            tab.hover = Some(code_editor::Hover { line, index, anchor, lines });
                        }
                    }
                }
                lsp::Event::Completion { path, line, column, items, incomplete } => {
                    // Only the answer to the latest request, for the tab and line it was about.
                    let Some((_, _, request)) = self.ws.completion_request.clone().filter(|(p, l, _)| *p == path && *l == line) else { continue };
                    let Some(tab) = self.ws.tabs.get(self.ws.active_tab).filter(|tab| tab.path.as_deref() == Some(path.as_path())) else { continue };
                    let Some(text) = tab.content.inner.lines.get(line).map(|l| l.text()) else { continue };
                    if text.get(..request).is_none() || lsp::byte_to_utf16(text, request) != column { continue; }
                    let word = completion::word_start(text, request);
                    let mut session = completion::Session::new(path, line, request, word, items, incomplete);
                    self.ws.completion = session.refilter(&tab.content).then_some(session);
                }
                lsp::Event::Definition { path, line, column } => {
                    tasks.push(self.open_path(path.clone()));
                    if let Some(tab) = self.ws.tabs.iter_mut().find(|tab| tab.path.as_deref() == Some(path.as_path())) {
                        let index = tab.content.inner.lines.get(line).map_or(0, |l| lsp::utf16_to_byte(l.text(), column));
                        tab.content.goto(line, index);
                        tab.hover = None;
                        self.ws.focus = Focus::Editor;
                    }
                }
            }
        }
        Task::batch(tasks)
    }

    /// Asks the language server for completions at the active tab's cursor. `trigger` is the
    /// non-word character just typed, when that is what prompted it.
    fn request_completion(&mut self, trigger: Option<char>) {
        let Some(tab) = self.ws.tabs.get(self.ws.active_tab) else { return };
        let Some(path) = tab.path.clone() else { return };
        let cursor = tab.content.cursor;
        let Some(line) = tab.content.inner.lines.get(cursor.line) else { return };
        let column = lsp::byte_to_utf16(line.text(), cursor.index);
        let text = tab.content.text();
        self.ws.completion_request = Some((path.clone(), cursor.line, cursor.index));
        self.ws.lsp.completion(&path, cursor.line, column, text, trigger);
    }

    fn close_completion(&mut self) {
        self.ws.completion = None;
        self.ws.completion_request = None;
    }

    /// The completion list, if it belongs to the active tab and the cursor is still in its word.
    fn active_completion(&mut self) -> Option<&mut completion::Session> {
        let tab = self.ws.tabs.get(self.ws.active_tab)?;
        let valid = self.ws.completion.as_ref().is_some_and(|session| tab.path.as_deref() == Some(session.path.as_path()) && session.at_cursor(&tab.content));
        if !valid { self.close_completion(); }
        self.ws.completion.as_mut()
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
        let Some(tab) = self.ws.tabs.get(self.ws.active_tab) else { return };
        let (open, incomplete) = match self.ws.completion.as_mut() {
            Some(session) => (session.refilter(&tab.content), session.incomplete),
            None => (false, false),
        };
        let had_list = self.ws.completion.is_some();
        if !open { self.ws.completion = None; }
        // Typing a word with no list yet, or one the server marked incomplete (it may be
        // missing what matches now): ask again, keeping what shows meanwhile.
        if typed.is_some() && (!had_list || incomplete) { self.request_completion(None); }
    }

    /// UTF-16 column of byte `index` on `line` of the active tab, with the tab's path.
    fn lsp_position(&self, line: usize, index: usize) -> Option<(PathBuf, usize)> {
        let tab = self.ws.tabs.get(self.ws.active_tab)?;
        let column = lsp::byte_to_utf16(tab.content.inner.lines.get(line)?.text(), index);
        Some((tab.path.clone()?, column))
    }

    /// Reacts to repository changes and pending edits: markers follow the index and the
    /// buffer, and status reloads only while the source control panel is showing.
    fn poll_git(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.ws.git.set_project(self.ws.root.as_deref()) {
            tasks.push(self.reload_git_views());
            if self.ws.sidebar_visible && self.ws.sidebar_mode == SidebarMode::Git {
                tasks.push(Task::done(Message::Git(git::Message::Refresh)));
            }
        }
        if let Some(changes) = self.ws.git.take_changes() {
            if changes.repository {
                tasks.push(self.reload_git_views());
                if let Some(graph) = &mut self.ws.git_graph { tasks.push(graph.reload().map(Message::GitGraph)); }
            } else if let Some(preview) = self.ws.git_preview.as_mut().filter(|preview| preview.live) {
                // Working-tree edits only change the unstaged half of an open preview.
                tasks.push(load_preview_task(self.ws.git.repo(), preview.root.clone(), preview.path.clone()));
            }
            if self.ws.sidebar_visible && self.ws.sidebar_mode == SidebarMode::Git {
                let cwd = self.root_or_cwd();
                tasks.push(git::update(&mut self.ws.git, git::Message::Refresh, cwd).map(Message::Git));
            }
        }
        if self.ws.gutter_due.is_some_and(|due| std::time::Instant::now() >= due) {
            self.ws.gutter_due = None;
            if let Some(tab) = self.ws.tabs.get(self.ws.active_tab).filter(|tab| tab.image.is_none()) {
                if let Some(path) = tab.path.clone() {
                    tasks.push(load_diff_task(self.ws.git.repo(), self.ws.root.as_deref(), path, tab.content.text()));
                }
            }
        }
        Task::batch(tasks)
    }

    /// Reloads the open diff preview and every tab's markers from current repository state.
    fn reload_git_views(&mut self) -> Task<Message> {
        let mut tasks = vec![self.reload_all_diffs()];
        if let Some(preview) = self.ws.git_preview.as_mut().filter(|preview| preview.live) {
            preview.result = None;
            tasks.push(load_preview_task(self.ws.git.repo(), preview.root.clone(), preview.path.clone()));
        }
        Task::batch(tasks)
    }

    /// Recomputes the diff markers of every open tab, e.g. after the index changed.
    fn reload_all_diffs(&self) -> Task<Message> {
        let repo = self.ws.git.repo();
        Task::batch(self.ws.tabs.iter().filter_map(|tab| {
            if tab.image.is_some() { return None; }
            let path = tab.path.clone()?;
            Some(load_diff_task(repo.clone(), self.ws.root.as_deref(), path, tab.content.text()))
        }))
    }

    /// Applies `action` to `self.zoom`, then re-shapes every open tab's buffer at the new
    /// font size/line height and persists the level for next launch.
    /// Turns word wrap on or off for every open file, and for files opened from now on.
    fn toggle_word_wrap(&mut self) {
        self.word_wrap = !self.word_wrap;
        let wrap = self.word_wrap;
        self.for_each_loaded(|workspace| for tab in &mut workspace.tabs {
            tab.content.set_wrap(wrap);
        });
        save_word_wrap(self.word_wrap);
    }

    fn apply_zoom(&mut self, action: ViewAction) {
        self.zoom = match action {
            ViewAction::ZoomIn => (self.zoom + code_editor::ZOOM_STEP).min(code_editor::ZOOM_MAX),
            ViewAction::ZoomOut => (self.zoom - code_editor::ZOOM_STEP).max(code_editor::ZOOM_MIN),
            ViewAction::ZoomReset => code_editor::ZOOM_DEFAULT,
            ViewAction::ToggleWordWrap => return self.toggle_word_wrap(),
        };
        let metrics = code_editor::metrics_for_zoom(self.zoom);
        self.for_each_loaded(|workspace| for tab in &mut workspace.tabs {
            tab.content.set_metrics(metrics);
        });
        save_zoom(self.zoom);
    }

    fn ai_input(&self) -> String {
        match self.ws.ai_mode {
            AiMode::Http => self.ws.chat.input_text(),
            AiMode::Acp => self.ws.acp.input_text(),
            AiMode::Cursor => self.ws.cursor.input_text(),
            AiMode::Grok => self.ws.grok.input_text(),
            AiMode::Codex => self.ws.codex.input_text(),
        }
    }

    fn set_ai_input(&mut self, text: &str) {
        match self.ws.ai_mode {
            AiMode::Http => self.ws.chat.set_input(text),
            AiMode::Acp => self.ws.acp.set_input(text),
            AiMode::Cursor => self.ws.cursor.set_input(text),
            AiMode::Grok => self.ws.grok.set_input(text),
            AiMode::Codex => self.ws.codex.set_input(text),
        }
        self.refresh_mentions();
    }

    fn ai_attachments(&mut self) -> &mut Vec<ai_context::Attachment> {
        match self.ws.ai_mode {
            AiMode::Http => &mut self.ws.chat.attachments,
            AiMode::Acp => &mut self.ws.acp.attachments,
            AiMode::Cursor => &mut self.ws.cursor.attachments,
            AiMode::Grok => &mut self.ws.grok.attachments,
            AiMode::Codex => &mut self.ws.codex.attachments,
        }
    }

    /// Where one-off AI requests (commit messages, inline edits) go: the assistant selected
    /// in the AI panel.
    fn ai_backend(&self) -> Result<ai_oneshot::Backend, String> {
        match self.ws.ai_mode {
            AiMode::Http => self.ws.chat.connection()
                .map(|(base_url, api_key, model)| ai_oneshot::Backend::Http { base_url, api_key, model })
                .ok_or_else(|| "Choose a model in the AI panel's settings first".to_string()),
            AiMode::Acp => Ok(ai_oneshot::Backend::Agent { provider: self.ws.acp.provider(), model: self.ws.acp.preferred_model() }),
            AiMode::Cursor => Ok(ai_oneshot::Backend::Agent { provider: self.ws.cursor.provider(), model: self.ws.cursor.preferred_model() }),
            AiMode::Grok => Ok(ai_oneshot::Backend::Agent { provider: self.ws.grok.provider(), model: self.ws.grok.preferred_model() }),
            AiMode::Codex => Ok(ai_oneshot::Backend::Agent { provider: self.ws.codex.provider(), model: self.ws.codex.preferred_model() }),
        }
    }

    /// Updates the `@` file suggestions for the AI input. The project is listed once per mention.
    fn refresh_mentions(&mut self) {
        let text = self.ai_input();
        let Some(query) = ai_mention::typing(&text) else {
            self.ws.mentions.clear();
            self.ws.mention_files = None;
            return;
        };
        let root = self.root_or_cwd();
        if self.ws.mention_files.as_ref().is_none_or(|(listed, _)| *listed != root) {
            let files = self.ws.root.as_deref().map(quick_open::walk).unwrap_or_default();
            self.ws.mention_files = Some((root.clone(), ai_mention::relative_paths(&root, &files)));
        }
        self.ws.mentions = self.ws.mention_files.as_ref().map(|(_, files)| ai_mention::suggest(files, query)).unwrap_or_default();
    }

    /// Attaches the project files `@`-mentioned in the AI input that aren't attached yet.
    /// Mentions that aren't files in the project are left as plain text.
    fn attach_mentions(&mut self) {
        let Some(root) = self.ws.root.clone() else { return };
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
        self.ws.mentions.clear();
    }

    /// Puts AI-suggested code into the active tab at the caret, replacing any selection.
    fn insert_code(&mut self, code: &str) {
        if self.editor_hidden() || self.ws.tabs.get(self.ws.active_tab).is_none() {
            self.notify("Open a file tab to insert code");
            return;
        }
        let before = self.ws.tabs[self.ws.active_tab].content.undo_count();
        self.ws.tabs[self.ws.active_tab].content.replace_selection(&code.replace("\r\n", "\n"));
        self.ws.focus = Focus::Editor;
        self.mark_edited_if_changed(before);
    }

    fn open_ai_diff(&mut self, diff: Option<ai_diff::FileDiff>) {
        match diff {
            Some(diff) => {
                self.ws.git_preview = Some(diff.preview(&self.root_or_cwd()));
                self.ws.focus = Focus::Editor;
            }
            None => self.notify("That change is no longer available"),
        }
    }

    /// Puts `prompt` in the AI panel with `attachment`, showing the panel, and sends it unless
    /// the user is meant to finish typing it.
    fn ask_ai(&mut self, prompt: &str, attachment: ai_context::Attachment, send: bool) -> Task<Message> {
        let mut task = Task::none();
        if !self.ws.ai_visible { task = update(self, Message::AiToggle); }
        if self.ai_attachments().len() < 16 { self.ai_attachments().push(attachment); }
        else { self.notify("Maximum 16 attachments per message."); }
        self.set_ai_input(prompt);
        if send {
            let send = match self.ws.ai_mode {
                AiMode::Http => Message::Chat(chat::Message::Send),
                AiMode::Acp => Message::Acp(acp::Message::Send),
                AiMode::Cursor => Message::Cursor(acp::Message::Send),
                AiMode::Grok => Message::Grok(acp::Message::Send),
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
        if self.ws.tabs.get(self.ws.active_tab).is_some_and(|tab| tab.image.is_some()) { return Task::none(); }
        let Some(tab) = self.ws.tabs.get_mut(self.ws.active_tab) else {
            self.notify("Open a file first");
            return Task::none();
        };
        let bounds = tab.content.selection_bounds();
        let code = match bounds {
            Some((start, end)) => tab.content.text_between(start, end).unwrap_or_default(),
            None => tab.content.text(),
        };
        let tab = &self.ws.tabs[self.ws.active_tab];
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
        let Some(tab) = self.ws.tabs.get_mut(self.ws.active_tab) else {
            self.notify("Open a file first");
            return Task::none();
        };
        let (start, end) = tab.content.selection_bounds().unwrap_or_else(|| {
            let line = tab.content.cursor.line;
            let length = tab.content.inner.lines.get(line).map_or(0, |text| text.text().len());
            (cosmic_text::Cursor::new(line, 0), cosmic_text::Cursor::new(line, length))
        });
        let original = tab.content.text_between(start, end).unwrap_or_default();
        self.ws.inline_edit = Some(inline_edit::InlineEdit {
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
        let Some(edit) = self.ws.inline_edit.as_ref().filter(|edit| matches!(edit.stage, inline_edit::Stage::Prompt) && !edit.instruction.trim().is_empty()) else {
            return Task::none();
        };
        let Some(tab) = self.ws.tabs.iter().find(|tab| tab.path == edit.path) else {
            self.ws.inline_edit = None;
            return Task::none();
        };
        let last = tab.content.line_count().saturating_sub(1);
        let end_of_file = cosmic_text::Cursor::new(last, tab.content.inner.lines.get(last).map_or(0, |line| line.text().len()));
        let before = tab.content.text_between(cosmic_text::Cursor::new(0, 0), edit.start).unwrap_or_default();
        let after = tab.content.text_between(edit.end, end_of_file).unwrap_or_default();
        let language = self.highlighter.language_name(&tab.extension()).to_string();
        let prompt = inline_edit::prompt(&self.tab_label(tab), &language, &before, &edit.original, &after, &edit.instruction);
        self.ws.inline_generation += 1;
        let id = self.ws.inline_generation;
        if let Some(edit) = &mut self.ws.inline_edit { edit.stage = inline_edit::Stage::Working(id); }
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
            if let Some(index) = self.ws.tabs.iter().position(|t| t.path.as_deref() == Some(path)) {
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
    if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
        tab.sync_preview();
    }
    task
}

/// Routine Git feedback belongs in the status bar; failures keep the alert.
fn git_notification(message: &git::Message) -> Option<Result<String, String>> {
    match message {
        git::Message::Committed(Ok(())) => Some(Ok("Commit created".into())),
        git::Message::Staged(Ok(())) => Some(Ok("Staging updated".into())),
        git::Message::Discarded(Ok(())) => Some(Ok("Unstaged changes discarded".into())),
        git::Message::BranchDone(Ok(())) => Some(Ok("Switched branch".into())),
        git::Message::SyncDone(sync, Ok(())) => Some(Ok(sync.done().into())),
        git::Message::Committed(Err(error)) | git::Message::Staged(Err(error))
        | git::Message::Discarded(Err(error)) | git::Message::Refreshed(Err(error))
        | git::Message::BranchDone(Err(error)) | git::Message::BranchesLoaded(Err(error))
        | git::Message::SyncDone(_, Err(error)) => Some(Err(format!("Git: {error}"))),
        git::Message::MessageGenerated(Err(error)) => Some(Err(format!("Could not write a commit message: {error}"))),
        _ => None,
    }
}

/// Handles `message` for the workspace it belongs to: an addressed one (`Message::In`) for
/// its tab, wherever that is; anything else for the tab in `ws`. What the handling starts
/// reports back to that same tab.
fn update(state: &mut State, message: Message) -> Task<Message> {
    if let Message::In(id, message) = message {
        return route(state, id, *message);
    }
    let id = state.ws.id;
    tag(handle(state, message), id)
}

/// Handles `message` with workspace `id` in `ws`, then puts the shown tab's workspace back.
/// Messages for a tab that was closed meanwhile are dropped.
fn route(state: &mut State, id: WorkspaceId, message: Message) -> Task<Message> {
    if message.is_global() {
        return update(state, message);
    }
    let Some(index) = state.workspace_index(id) else { return Task::none() };
    if matches!(state.workspaces[index].slot, Slot::Unloaded(_)) { return Task::none(); }
    if index == state.current {
        return tag(handle(state, message), id);
    }
    let back = state.workspaces[state.current].id;
    let _ = state.swap_to(index);
    let task = tag(handle(state, message), id);
    if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) { tab.sync_preview(); }
    if let Some(back) = state.workspace_index(back) {
        let _ = state.swap_to(back);
    }
    task
}

fn handle(state: &mut State, message: Message) -> Task<Message> {
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
    if state.ws.tabs.get(state.ws.active_tab).is_some_and(|tab| tab.preview.is_some()) && matches!(&message,
        Message::EditorAction(_) | Message::EditAction(_) | Message::EditorPasted(_) | Message::Search(_)
        | Message::InlineEditStart) {
        return Task::none();
    }
    match message {
        Message::MarkdownPreviewToggle => {
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab).filter(|tab| tab.is_markdown()) {
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
            if !state.recovery_saved() {
                state.notify("Could not save recovery. Save your files before updating.");
                return Task::none();
            }
            match state.updater.restart() {
                Ok(()) => {
                    state.for_each_loaded(|workspace| workspace.terminal.shutdown());
                    return iced::exit();
                }
                Err(error) => state.notify(error),
            }
        }
        Message::TerminalToggle => {
            if !state.ws.terminal_visible {
                // A tab in the background opens it without taking focus from the user.
                let focus = !state.in_background();
                state.show_terminal(focus);
                if focus { task = iced::widget::operation::focus("terminal-canvas"); }
            } else {
                state.ws.terminal_visible = false;
                state.ws.terminal.set_focused(false);
                let pane = state.ws.panes.iter().find(|(_, kind)| **kind == PaneKind::Terminal).map(|(pane, _)| *pane);
                if let Some(pane) = pane { state.ws.panes.close(pane); }
            }
        }
        Message::Terminal(terminal::panel::Message::Hide) => return update(state, Message::TerminalToggle),
        Message::Terminal(message) => task = state.ws.terminal.update(message, &state.root_or_cwd()).map(Message::Terminal),
        Message::ToggleFold(line) => {
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) { tab.content.toggle_fold(line); }
        }
        Message::CompletionAccept(index) => {
            let Some(session) = state.ws.completion.take() else { return Task::none() };
            state.ws.completion_request = None;
            let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab).filter(|tab| tab.path.as_deref() == Some(session.path.as_path())) else { return Task::none() };
            let Some(edits) = index.or_else(|| session.selected_item()).and_then(|index| session.edits(index, &tab.content)) else { return Task::none() };
            let before = tab.content.undo_count();
            tab.content.replace_ranges(&edits, 0);
            state.ws.focus = Focus::Editor;
            state.mark_edited_if_changed(before);
        }
        Message::EditorAction(action) => {
            state.ws.focus = Focus::Editor;
            state.close_completion();
            let before = state
                .ws.tabs
                .get(state.ws.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
                tab.content.perform(action);
                tab.hover = None;
                tab.diff_popup = None;
            }
            state.mark_edited_if_changed(before);
        }
        Message::EditorViewport(width, height) => {
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) { tab.content.set_viewport(width, height); }
        }
        Message::Search(msg) => {
            let before = state
                .ws.tabs
                .get(state.ws.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
                code_editor::search::update(&mut tab.search, &mut tab.content, msg);
            }
            state.mark_edited_if_changed(before);
        }
        Message::ProjectSearch(msg) => {
            let root = state.ws.root.clone();
            let (search_task, opened) = project_search::update(&mut state.ws.project_search, msg, root.as_deref());
            task = search_task.map(Message::ProjectSearch);
            if let Some((path, line)) = opened {
                task = Task::batch([task, state.open_path(path)]);
                state.ws.focus = Focus::Editor;
                if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
                    tab.content.goto_line(line.saturating_sub(1));
                }
            }
        }
        Message::ToggleProjectSearch => {
            state.toggle_sidebar_mode(SidebarMode::ProjectSearch);
        }
        Message::QuickOpen(msg) => {
            let root = state.ws.root.clone();
            let (t, opened) = quick_open::update(&mut state.quick_open, msg, root.as_deref(), &state.recent_files);
            task = t.map(Message::QuickOpen);
            if let Some(path) = opened {
                task = Task::batch([task, state.open_path(path)]);
                state.ws.focus = Focus::Editor;
            }
        }
        Message::ToggleQuickOpen => {
            let root = state.ws.root.clone();
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
                if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
                    tab.content.goto_line(line.saturating_sub(1));
                }
                state.ws.focus = Focus::Editor;
            }
        }
        Message::ToggleGotoLine => {
            if state.goto_line.visible {
                let _ = goto_line::update(&mut state.goto_line, goto_line::Message::Close);
            } else if state.ws.tabs.get(state.ws.active_tab).is_some() {
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
            state.ws.focus = Focus::Editor;
            state.ws.git_preview = Some(git_preview::Preview {
                root: root.clone(), path: path.clone(), result: None, live: true,
            });
            task = load_preview_task(state.ws.git.repo(), root, path);
        }
        Message::Git(git::Message::GenerateMessage) => match state.ai_backend() {
            Err(err) => state.notify(err),
            Ok(backend) => {
                let cwd = state.root_or_cwd();
                let _ = git::update(&mut state.ws.git, git::Message::GenerateMessage, cwd.clone());
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
            if let Some(preview) = &mut state.ws.git_preview {
                if preview.root == root && preview.path == path {
                    preview.result = Some(result);
                }
            }
        }
        Message::CloseGitPreview => state.ws.git_preview = None,
        Message::ImageLoaded(request, result) => {
            if let Some(preview) = state.ws.tabs.iter_mut().filter_map(|tab| tab.image.as_mut()).find(|preview| preview.request == request) {
                preview.receive(request, result);
            }
        }
        Message::GitGraphOpen => {
            if state.ws.git.repo().is_none() {
                state.git_info("This folder is not inside a Git repository.");
            } else {
                let (graph, load) = git_graph::Graph::open(state.root_or_cwd());
                state.ws.git_preview = None;
                state.ws.git_graph = Some(graph);
                state.ws.focus = Focus::Editor;
                task = load.map(Message::GitGraph);
            }
        }
        Message::GitGraph(git_graph::Message::Close) => state.ws.git_graph = None,
        Message::GitGraph(git_graph::Message::OpenFile(hash, file)) => {
            if let Some(graph) = &state.ws.git_graph {
                let (root, parent) = (graph.root.clone(), graph.parent_of(&hash));
                let label = format!("{} @ {}", file.path, &hash[..hash.len().min(7)]);
                state.ws.git_preview = Some(git_preview::Preview { root: root.clone(), path: label.clone(), result: None, live: false });
                task = Task::perform(async move {
                    tokio::task::spawn_blocking(move || git_graph::file_patch(&root, &hash, parent.as_deref(), &file))
                        .await.map_err(|err| err.to_string())?
                }, move |result| Message::GitGraphPatchLoaded(label.clone(), result));
            }
        }
        Message::GitGraphPatchLoaded(label, result) => {
            if let Some(preview) = state.ws.git_preview.as_mut().filter(|preview| !preview.live && preview.path == label) {
                preview.result = Some(result);
            }
        }
        Message::GitGraph(msg) => {
            if let Some(graph) = &mut state.ws.git_graph {
                task = git_graph::update(graph, msg).map(Message::GitGraph);
            }
        }
        Message::GitPreviewScrolled(left, y) => {
            task = iced::widget::operation::scroll_to(
                if left { git_preview::RIGHT_SCROLL } else { git_preview::LEFT_SCROLL },
                iced::widget::scrollable::AbsoluteOffset { x: None, y: Some(y) },
            );
        }
        Message::Git(git::Message::ShowGraph) => {
            let cwd = state.root_or_cwd();
            let _ = git::update(&mut state.ws.git, git::Message::ShowGraph, cwd);
            task = update(state, Message::GitGraphOpen);
        }
        Message::Git(git::Message::Discard(files)) => {
            task = ask(state.window_handle, "Discard changes", format!("Discard unstaged changes in {} file(s)? Staged changes are kept. Untracked files will be deleted. Unsaved editor changes in these files will also be discarded. This cannot be undone.", files.len()), Message::DiscardGitConfirmed(state.root_or_cwd(), files));
        }
        Message::DiscardGitConfirmed(cwd, files) => {
            if cwd != state.root_or_cwd() { return Task::none(); }
            let paths: Vec<_> = state.ws.tabs.iter().filter_map(|tab| {
                let path = tab.path.as_ref()?;
                files.iter().any(|file| cwd.join(&file.path) == *path).then(|| (path.clone(), tab.content.text()))
            }).collect();
            task = git::update(&mut state.ws.git, git::Message::DiscardConfirmed(files), cwd).map(move |msg| Message::DiscardGitFinished(paths.clone(), msg));
        }
        Message::DiscardGitFinished(paths, msg) => {
            if matches!(msg, git::Message::Discarded(Ok(()))) {
                for index in (0..state.ws.tabs.len()).rev() {
                    let tab = &mut state.ws.tabs[index];
                    if paths.iter().any(|(path, snapshot)| tab.path.as_ref() == Some(path) && tab.content.text() == *snapshot) {
                        if tab.path.as_ref().is_some_and(|p| !p.exists()) {
                            state.remove_tab(index);
                        } else { tab.dirty = false; }
                    }
                }
                task = Task::batch([state.reload_open_tabs(), state.reload_git_views()]);
            }
            state.notify_git(&msg);
            let cwd = state.root_or_cwd();
            task = Task::batch([task, git::update(&mut state.ws.git, msg, cwd).map(Message::Git)]);
        }
        Message::Git(msg) => {
            state.notify_git(&msg);
            let cwd = state.root_or_cwd();
            if matches!(msg, git::Message::Refresh) {
                task = state.reload_git_views();
            }
            // The checkout rewrote files: tabs without unsaved edits follow it, as after an AI edit.
            if matches!(msg, git::Message::BranchDone(Ok(()))) {
                task = Task::batch([state.reload_open_tabs(), state.reload_git_views()]);
            }
            // A pull rewrites files too, and a failed one can leave conflict markers.
            if matches!(msg, git::Message::SyncDone(git::remote::Sync::Pull, _)) {
                task = Task::batch([state.reload_open_tabs(), state.reload_git_views()]);
            }
            task = Task::batch([task, git::update(&mut state.ws.git, msg, cwd).map(Message::Git)]);
        }
        Message::GitPanelToggle => {
            if state.toggle_sidebar_mode(SidebarMode::Git) {
                let cwd = state.root_or_cwd();
                task = git::update(&mut state.ws.git, git::Message::Refresh, cwd).map(Message::Git);
            }
        }
        Message::GitBlameLoaded(path, snapshot, blame) => {
            if let Some(tab) = state.ws.tabs.iter_mut().find(|t| t.path.as_deref() == Some(path.as_path())) {
                if tab.content.text() == snapshot { tab.blame = blame; }
            }
        }
        Message::GitDiffLoaded(path, snapshot, diff) => {
            if let Some(tab) = state.ws.tabs.iter_mut().find(|t| t.path.as_deref() == Some(path.as_path())) {
                if tab.content.text() == snapshot {
                    let (lines, hunks) = diff;
                    tab.diff = lines.into_iter().collect();
                    tab.hunks = hunks;
                    // A change that is gone, or was edited, has nothing left to show.
                    if tab.diff_popup.as_ref().is_some_and(|hunk| !tab.hunks.contains(hunk)) { tab.diff_popup = None; }
                }
            }
        }
        Message::DiffMarkerClicked(line) => {
            state.ws.focus = Focus::Editor;
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
                let hunk = tab.hunks.iter().find(|hunk| hunk.marks(line)).cloned();
                // A second click on the same marker closes the popup.
                tab.diff_popup = if hunk == tab.diff_popup { None } else { hunk };
                tab.hover = None;
            }
        }
        Message::DiffPopupClose => {
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) { tab.diff_popup = None; }
        }
        Message::DiffRestore => {
            state.ws.focus = Focus::Editor;
            let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) else { return Task::none() };
            let Some(hunk) = tab.diff_popup.take() else { return Task::none() };
            let before = tab.content.undo_count();
            git_diff::restore(&mut tab.content, &hunk);
            state.mark_edited_if_changed(before);
        }
        Message::PassphraseInput(passphrase) => state.passphrase = passphrase,
        Message::RememberPassphrase(remember) => state.remember_passphrase = remember,
        Message::PassphraseDone(ok) => {
            let passphrase = std::mem::take(&mut state.passphrase);
            let remember = std::mem::replace(&mut state.remember_passphrase, false);
            if let Some(request) = state.passphrase_prompts.pop_front() {
                git::askpass::answer(request.id, ok.then_some((passphrase, remember)));
            }
            if !state.passphrase_prompts.is_empty() { task = iced::widget::operation::focus(passphrase_input_id()); }
        }
        Message::TabSelected(i) => { state.show_editor(); state.ws.active_tab = i; },
        Message::TabClosed(i) => task = state.close_tab(i),
        Message::CloseTabs(anchor, scope) => task = state.close_tabs(anchor, scope),
        Message::CloseTabConfirmed(i) => {
            if i < state.ws.tabs.len() { state.remove_tab(i); }
        }
        Message::CloseTabsConfirmed(anchor, scope) => state.remove_tabs(anchor, scope),
        Message::DeleteConfirmed(path) => task = state.delete_confirmed(&path),
        Message::DismissNotice => state.notice = None,
        Message::Exit => {
            if !state.recovery_saved() {
                state.notify("Could not save recovery. Save your files before closing.");
                return Task::none();
            }
            state.for_each_loaded(|workspace| {
                workspace.terminal.shutdown();
                workspace.lsp.shutdown_all();
            });
            return iced::exit();
        }
        Message::LspInstall(id) => {
            state.ws.lsp_prompt = None;
            if let Some(server) = lsp::registry::by_id(id) { state.ws.lsp.install(server); }
        }
        Message::LspDismiss(id) => {
            state.ws.lsp_prompt = None;
            if let Some(server) = lsp::registry::by_id(id) { state.ws.lsp.dismiss(server); }
        }
        Message::EditorProbe(probe) => match probe {
            code_editor::Probe::Leave => {
                state.ws.hover_request = None;
                if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) { tab.hover = None; }
            }
            code_editor::Probe::Hover { line, index, anchor } => {
                state.ws.hover_request = Some((line, index, anchor));
                if let Some((path, column)) = state.lsp_position(line, index) { state.ws.lsp.hover(&path, line, column); }
            }
            code_editor::Probe::Definition(line, index) => {
                if let Some((path, column)) = state.lsp_position(line, index) { state.ws.lsp.definition(&path, line, column); }
            }
        },
        Message::GoToDefinition => {
            if let Some(cursor) = state.ws.tabs.get(state.ws.active_tab).map(|tab| tab.content.cursor) {
                if let Some((path, column)) = state.lsp_position(cursor.line, cursor.index) {
                    state.ws.lsp.definition(&path, cursor.line, column);
                }
            }
        }
        Message::ReopenClosedTab => {
            if let Some(path) = state.ws.closed_tabs.pop() { task = state.open_path(path); }
        }
        Message::RevealPath(path) => {
            if let Some(root) = state.ws.root.clone().filter(|root| path.starts_with(root)) {
                state.ws.sidebar_mode = SidebarMode::Tree;
                state.show_sidebar();
                let mut dirs: Vec<_> = path.ancestors().skip(1).take_while(|parent| parent.starts_with(&root)).map(Path::to_path_buf).collect();
                dirs.reverse();
                if path.is_dir() { dirs.push(path.clone()); }
                dirs.dedup();
                state.ws.reveal_queue = dirs.into();
                state.ws.reveal_target = Some(path);
                state.ws.reveal_generation += 1;
                let generation = state.ws.reveal_generation;
                let mut tree = file_tree(root);
                if let Some(dir) = state.ws.reveal_queue.pop_front() {
                    task = tree.update(DirectoryTreeEvent::Toggled(dir)).map(move |event| Message::RevealStep(generation, event));
                }
                state.ws.tree = Some(tree);
            } else { state.notify("This file is outside the open project"); }
        }
        Message::RevealStep(generation, event) => {
            if generation == state.ws.reveal_generation {
                if let Some(tree) = &mut state.ws.tree {
                    let loaded = matches!(&event, DirectoryTreeEvent::Loaded(_));
                    task = tree.update(event).map(move |event| Message::RevealStep(generation, event));
                    if loaded {
                        if let Some(dir) = state.ws.reveal_queue.pop_front() {
                            task = Task::batch([task, tree.update(DirectoryTreeEvent::Toggled(dir)).map(move |event| Message::RevealStep(generation, event))]);
                        } else if let Some(path) = state.ws.reveal_target.take() {
                            task = Task::batch([task, tree.update(DirectoryTreeEvent::Selected(path.clone(), path.is_dir(), iced_swdir_tree::SelectionMode::Replace)).map(Message::Tree)]);
                        }
                    }
                }
            }
        }

        Message::PathDropped(path) => {
            if path.is_dir() {
                return update(state, Message::OpenFolderRequest(path));
            } else {
                task = state.open_path(path);
                state.ws.focus = Focus::Editor;
            }
        }
        Message::FileAction(action) => match action {
            FileAction::OpenFile => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    task = state.open_path(path);
                    state.ws.focus = Focus::Editor;
                }
            }
            FileAction::OpenFolder => {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    return update(state, Message::OpenFolderRequest(path));
                }
            }
            // Closes the project's tab; the last tab is emptied instead (see `close_workspace`).
            FileAction::CloseFolder => return update(state, Message::WorkspaceClose(state.ws.id)),
            FileAction::Save => task = state.save(),
        },

        Message::OpenFolderRequest(path) => {
            if !path.is_dir() {
                state.recent_projects.retain(|recent| *recent != path);
                state.notify(format!("Folder not found: {}", path.display()));
                return Task::none();
            }
            match workspaces::folder_target(&state.workspace_roots(), state.active, &path) {
                workspaces::FolderTarget::Switch(index) => task = state.activate_workspace(index),
                workspaces::FolderTarget::Current => task = tag(open_folder(state, path), state.ws.id),
                workspaces::FolderTarget::Ask => state.folder_prompt = Some(path),
            }
        }
        Message::FolderPromptChoice(choice) => {
            let Some(path) = state.folder_prompt.take() else { return Task::none() };
            match choice {
                None => {}
                Some(FolderChoice::NewTab) => task = state.new_workspace(Some(path)),
                Some(FolderChoice::Replace) => {
                    let unsaved = state.ws.unsaved();
                    let confirmed = Message::FolderReplaceConfirmed(state.ws.id, path);
                    task = if unsaved > 0 {
                        let name = state.workspace_label(state.active);
                        ask(state.window_handle, "Unsaved changes", format!("Discard unsaved changes in {unsaved} file(s) of \"{name}\" and open the other folder in its place?"), confirmed)
                    } else {
                        update(state, confirmed)
                    };
                }
            }
        }
        Message::FolderReplaceConfirmed(id, path) => {
            if let Some(index) = state.workspace_index(id) {
                let switch = state.activate_workspace(index);
                task = Task::batch([switch, state.replace_workspace(path)]);
            }
        }
        // `update` unwraps these before calling here; kept for completeness.
        Message::In(id, message) => return route(state, id, *message),
        Message::WorkspaceNew => task = state.new_workspace(None),
        Message::WorkspaceCycle(forward) => {
            let index = workspaces::cycle(state.active, state.workspaces.len(), forward);
            task = state.activate_workspace(index);
        }
        Message::WorkspaceClose(id) => task = state.request_close(vec![id]),
        Message::WorkspaceCloseOthers(id) => {
            if let Some(index) = state.workspace_index(id) {
                let ids = workspaces::to_close(state.workspaces.len(), index, false).into_iter().map(|index| state.workspaces[index].id).collect();
                task = state.request_close(ids);
            }
        }
        Message::WorkspaceCloseRight(id) => {
            if let Some(index) = state.workspace_index(id) {
                let ids = workspaces::to_close(state.workspaces.len(), index, true).into_iter().map(|index| state.workspaces[index].id).collect();
                task = state.request_close(ids);
            }
        }
        Message::WorkspacesCloseConfirmed(ids) => {
            let mut tasks = Vec::new();
            for id in ids { tasks.push(state.close_workspace(id)); }
            task = Task::batch(tasks);
        }
        Message::WorkspaceCopyPath(id) => {
            if let Some(root) = state.workspace_index(id).and_then(|index| state.workspace_root(index)) {
                task = iced::clipboard::write(root.display().to_string());
            }
        }
        Message::WorkspaceReveal(id) => {
            if let Some(root) = state.workspace_index(id).and_then(|index| state.workspace_root(index)) {
                reveal_in_file_manager(root);
            }
        }
        Message::WorkspaceDragStart(id) => {
            if let Some(index) = state.workspace_index(id) { task = state.activate_workspace(index); }
            state.workspace_drag = Some((id, false));
        }
        Message::WorkspaceDragMove(x, width) => {
            if let Some((id, _)) = state.workspace_drag {
                if let Some(from) = state.workspace_index(id) {
                    let to = workspaces::drag_target(x, width, state.workspaces.len());
                    if to != from { state.move_workspace(from, to); }
                    state.workspace_drag = Some((id, true));
                }
            }
        }

        Message::EditAction(action) => {
            let before = state
                .ws.tabs
                .get(state.ws.active_tab)
                .map(|t| t.content.undo_count())
                .unwrap_or(0);
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
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
                    .ws.tabs
                    .get(state.ws.active_tab)
                    .map(|t| t.content.undo_count())
                    .unwrap_or(0);
                if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
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
            if state.ws.reveal_target.is_some() && matches!(&event, DirectoryTreeEvent::Loaded(_)) { return Task::none(); }
            if !matches!(&event, DirectoryTreeEvent::Loaded(_)) {
                state.ws.reveal_generation += 1;
                state.ws.reveal_target = None;
                state.ws.reveal_queue.clear();
            }
            state.ws.focus = Focus::Tree;
            let mut open_task = Task::none();
            if let DirectoryTreeEvent::Selected(path, is_dir, _) = &event {
                if !*is_dir {
                    open_task = state.open_path(path.clone());
                }
            }
            if let Some(tree) = &mut state.ws.tree {
                task = Task::batch([open_task, tree.update(event).map(Message::Tree)]);
            } else {
                task = open_task;
            }
        }
        Message::RefreshTree => {
            if let Some(root) = state.ws.root.clone() {
                task = refresh_dir_task(root);
            }
        }
        Message::ContextNewFile => {
            if let Some(dir) = state.context_target_dir() {
                state.ws.renaming = None;
                let expand = state.ws.tree.as_mut().map(|tree| tree.update(DirectoryTreeEvent::Expand(dir.clone())).map(Message::Tree)).unwrap_or_else(Task::none);
                state.ws.creating = Some((dir, false, String::new()));
                task = Task::batch([expand, iced::widget::operation::focus("tree-name")]);
            }
        }
        Message::ContextNewFolder => {
            if let Some(dir) = state.context_target_dir() {
                state.ws.renaming = None;
                let expand = state.ws.tree.as_mut().map(|tree| tree.update(DirectoryTreeEvent::Expand(dir.clone())).map(Message::Tree)).unwrap_or_else(Task::none);
                state.ws.creating = Some((dir, true, String::new()));
                task = Task::batch([expand, iced::widget::operation::focus("tree-name")]);
            }
        }
        Message::ContextRename => {
            if let Some(path) = state.context_target_path() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                state.ws.creating = None;
                state.ws.renaming = Some((path, name));
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
                    .ws.root
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
            if let Some((_, buf)) = &mut state.ws.renaming {
                *buf = text;
            }
        }
        Message::RenameSubmit => {
            if let Some((old_path, new_name)) = state.ws.renaming.clone() {
                if !new_name.is_empty() {
                    if !valid_entry_name(&new_name) {
                        state.notify("Enter a name without path separators");
                        return iced::widget::operation::focus("tree-name");
                    }
                    let new_path = old_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(&new_name);
                    if new_path == old_path { state.ws.renaming = None; return Task::none(); }
                    if std::fs::symlink_metadata(&new_path).is_ok() {
                        state.notify("That name already exists");
                        return iced::widget::operation::focus("tree-name");
                    }
                    let renamed = std::fs::rename(&old_path, &new_path);
                    if renamed.is_ok() {
                        state.ws.renaming = None;
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
        Message::RenameCancel => state.ws.renaming = None,
        Message::CreateInput(text) => {
            if let Some((_, _, buf)) = &mut state.ws.creating {
                *buf = text;
            }
        }
        Message::CreateSubmit => {
            if let Some((dir, is_dir, name)) = state.ws.creating.clone() {
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
                        state.ws.creating = None;
                        state.notify("Created successfully");
                        task = refresh_dir_task(dir);
                    } else if let Err(err) = result { state.notify(format!("Create failed: {err}")); }
                }
            }
        }
        Message::CreateCancel => state.ws.creating = None,

        Message::AppThemeSelected(name) => match state.themes.load_theme(&name) {
            Ok(theme) => {
                save_app_theme(&theme.name);
                state.app_theme = theme;
                let (highlighter, syntax) = (&state.highlighter, &state.app_theme.syntax);
                let rehighlight = |workspace: &mut Workspace| for tab in &mut workspace.tabs {
                    let extension = tab.extension();
                    tab.content.highlight(highlighter, &extension, syntax);
                };
                rehighlight(&mut state.ws);
                for entry in &mut state.workspaces {
                    if let Slot::Parked(workspace) = &mut entry.slot { rehighlight(workspace); }
                }
            }
            Err(err) => state.notify(format!("Theme failed to load: {err}")),
        },
        Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
            state.ws.panes.resize(split, ratio);
            if Some(split) == state.ws.sidebar_split {
                state.ws.sidebar_ratio = ratio;
                save_sidebar_ratio(ratio);
            } else if Some(split) == state.ws.ai_split {
                state.ws.ai_ratio = ratio;
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
            // The open-folder question is modal too: Escape cancels it.
            if state.folder_prompt.is_some() {
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    return update(state, Message::FolderPromptChoice(None));
                }
                return Task::none();
            }
            // So is the passphrase dialog: Escape cancels it.
            if !state.passphrase_prompts.is_empty() {
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    return update(state, Message::PassphraseDone(false));
                }
                return Task::none();
            }
            if let Some(forward) = workspace_cycle_key(&key, modifiers) {
                return update(state, Message::WorkspaceCycle(forward));
            }
            if modifiers.control() && key.as_ref() == keyboard::Key::Character("`") {
                return update(state, Message::TerminalToggle);
            }
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) && state.ws.inline_edit.is_some()
                && !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible && !state.theme_install.visible {
                return update(state, Message::InlineEditCancel);
            }
            if key == keyboard::Key::Named(keyboard::key::Named::Escape)
                && state.ws.tabs.get(state.ws.active_tab).is_some_and(|tab| tab.diff_popup.is_some())
                && !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible && !state.theme_install.visible {
                return update(state, Message::DiffPopupClose);
            }
            if state.ws.terminal_visible && state.ws.terminal.focused()
                && !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible
                && !state.theme_install.visible
                && key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                return Task::none();
            }
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) && (state.ws.creating.is_some() || state.ws.renaming.is_some()) {
                state.ws.creating = None;
                state.ws.renaming = None;
                return Task::none();
            }
            if key == keyboard::Key::Named(keyboard::key::Named::F12) && state.ws.focus == Focus::Editor {
                return update(state, Message::GoToDefinition);
            }
            // Completion keys come before the editor's own: Ctrl+Space (Ctrl on every platform)
            // asks for suggestions, and while the list shows it takes the arrows, Enter/Tab, Esc.
            let editing = state.ws.focus == Focus::Editor && !state.editor_hidden() && !state.quick_open.visible
                && !state.command_palette.visible && !state.goto_line.visible && !state.theme_install.visible;
            if editing && modifiers.alt() && !modifiers.command() && !modifiers.shift() && key.as_ref() == keyboard::Key::Character("z") {
                state.toggle_word_wrap();
                return Task::none();
            }
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
                    // Workspace tabs, as in a browser: Cmd/Ctrl+T opens one, +Shift+W closes it.
                    keyboard::Key::Character("t") => task = update(state, Message::WorkspaceNew),
                    keyboard::Key::Character(c) if modifiers.shift() && c.eq_ignore_ascii_case("w") => {
                        task = update(state, Message::WorkspaceClose(state.ws.id));
                    }
                    keyboard::Key::Character("q") => return update(state, Message::Exit),
                    keyboard::Key::Character("w") => {
                        // A diff opened from the graph closes back to the graph.
                        if state.ws.git_preview.is_some() { state.ws.git_preview = None; }
                        else if state.ws.git_graph.is_some() { state.ws.git_graph = None; }
                        else { task = state.close_tab(state.ws.active_tab); }
                    }
                    keyboard::Key::Character(c) if c.len() == 1 && matches!(c.as_bytes()[0], b'1'..=b'9') => {
                        let index = (c.as_bytes()[0] - b'1') as usize;
                        if index < state.ws.tabs.len() { state.ws.active_tab = index; state.show_editor(); }
                    }
                    keyboard::Key::Character("s") => task = state.save(),
                    keyboard::Key::Character("k") if !state.editor_hidden() => task = update(state, Message::InlineEditStart),
                    keyboard::Key::Character("o") => {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            task = state.open_path(path);
                            state.ws.focus = Focus::Editor;
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
                        if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
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
                        } else if state.ws.tabs.get(state.ws.active_tab).is_some() {
                            state.quick_open.visible = false;
                            state.theme_install.visible = false;
                            let _ = command_palette::update(&mut state.command_palette, command_palette::Message::Close);
                            task = goto_line::open(&mut state.goto_line).map(Message::GotoLine);
                        }
                    }
                    keyboard::Key::Character("p") => {
                        let root = state.ws.root.clone();
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
                    let root = state.ws.root.clone();
                    let (t, opened) = quick_open::update(&mut state.quick_open, msg, root.as_deref(), &state.recent_files);
                    task = t.map(Message::QuickOpen);
                    if let Some(path) = opened {
                        task = Task::batch([task, state.open_path(path)]);
                        state.ws.focus = Focus::Editor;
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
                && state.ws.sidebar_mode == SidebarMode::ProjectSearch
            {
                // The global key subscription fires regardless of which widget has iced's own
                // focus (see `input::handle_key`'s docs on why this app doesn't rely on that
                // system), so this closes the panel even while typing in its search box.
                state.ws.sidebar_mode = SidebarMode::Tree;
            } else {
                // No per-widget focus system here (see `input::handle_key`'s docs), so route
                // by `state.ws.focus`: the tree when it's the last thing the user interacted
                // with, the active tab's editor otherwise.
                let tree_event = if state.ws.focus == Focus::Tree {
                    state.ws.tree.as_ref().and_then(|tree| tree.handle_key(&key, modifiers))
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
                    if let Some(tree) = &mut state.ws.tree {
                        task = Task::batch([open_task, tree.update(event).map(Message::Tree)]);
                    } else {
                        task = open_task;
                    }
                } else if state.ws.focus == Focus::Editor && state.handle_editor_key(&key, modifiers) {
                    state.after_editor_key(&key, modifiers);
                }
            }
        }
        Message::Noop => {}

        Message::SidebarToggle => {
            state.toggle_sidebar_mode(SidebarMode::Tree);
        }

        Message::AiToggle => {
            state.ws.ai_visible = !state.ws.ai_visible;
            save_ai_visible(state.ws.ai_visible);
            if state.ws.ai_visible {
                let main_pane = state
                    .ws.panes
                    .iter()
                    .find(|(_, kind)| **kind == PaneKind::Main)
                    .map(|(pane, _)| *pane);
                if let Some(main_pane) = main_pane {
                    if let Some((_, split)) =
                        state.ws.panes.split(pane_grid::Axis::Vertical, main_pane, PaneKind::Ai)
                    {
                        state.ws.panes.resize(split, state.ws.ai_ratio);
                        state.ws.ai_split = Some(split);
                    }
                }
            } else {
                let ai_pane = state
                    .ws.panes
                    .iter()
                    .find(|(_, kind)| **kind == PaneKind::Ai)
                    .map(|(pane, _)| *pane);
                if let Some(ai_pane) = ai_pane {
                    state.ws.panes.close(ai_pane);
                }
                state.ws.ai_split = None;
            }
        }
        Message::AiModeSelected(mode) => {
            state.ws.ai_mode = mode;
            state.refresh_mentions();
        }
        Message::Chat(chat::Message::Composer(action))
        | Message::Acp(acp::Message::Composer(action))
        | Message::Cursor(acp::Message::Composer(action))
        | Message::Grok(acp::Message::Composer(action))
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
                    if let Some(tree) = &mut state.ws.tree {
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
            // The same release ends dragging a workspace tab, wherever the pointer is.
            state.workspace_drag = None;
        }
        Message::TreeDragCancel => {
            if let Some(tree) = &mut state.ws.tree {
                task = tree.update(DirectoryTreeEvent::Drag(iced_swdir_tree::DragMsg::Cancelled)).map(Message::Tree);
            }
        }
        Message::AiAttach => {
            task = Task::perform(async { rfd::AsyncFileDialog::new().pick_files().await.unwrap_or_default().into_iter().map(|f| f.path().to_path_buf()).collect() }, Message::AiFiles);
        }
        Message::AiFiles(paths) => {
            if state.ws.ai_visible {
                let mode = state.ws.ai_mode;
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
                        let attachments = match mode { AiMode::Http => &mut state.ws.chat.attachments, AiMode::Acp => &mut state.ws.acp.attachments, AiMode::Codex => &mut state.ws.codex.attachments, AiMode::Grok => &mut state.ws.grok.attachments, AiMode::Cursor => &mut state.ws.cursor.attachments };
                        if attachments.len() < 16 { attachments.push(file); }
                        else { state.notice = Some(("Maximum 16 attachments per message.".into(), std::time::Instant::now())); }
                    }
                    Err(error) => state.notice = Some((error, std::time::Instant::now())),
                }
            }
        }
        Message::AiRemoveAttachment(index) => {
            let attachments = match state.ws.ai_mode { AiMode::Http => &mut state.ws.chat.attachments, AiMode::Acp => &mut state.ws.acp.attachments, AiMode::Codex => &mut state.ws.codex.attachments, AiMode::Grok => &mut state.ws.grok.attachments, AiMode::Cursor => &mut state.ws.cursor.attachments };
            if index < attachments.len() { attachments.remove(index); }
        }
        Message::AiPaste => {
            let mode = state.ws.ai_mode;
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
            let next = match state.ws.ai_mode {
                AiMode::Http => Message::Chat(chat::Message::InputChanged(paste)),
                AiMode::Acp => Message::Acp(acp::Message::InputChanged(paste)),
                AiMode::Cursor => Message::Cursor(acp::Message::InputChanged(paste)),
                AiMode::Grok => Message::Grok(acp::Message::InputChanged(paste)),
                AiMode::Codex => Message::Codex(acp::Message::InputChanged(paste)),
            };
            return update(state, next);
        }
        Message::AiReference => {
            if let Some(tab) = state.ws.tabs.get_mut(state.ws.active_tab) {
                let selection = tab.content.copy_selection();
                let name = format!("{}{}", tab.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Untitled".into()), if selection.is_some() { " (selection)" } else { " (editor buffer)" });
                let content = selection.unwrap_or_else(|| tab.content.text());
                let attachments = match state.ws.ai_mode { AiMode::Http => &mut state.ws.chat.attachments, AiMode::Acp => &mut state.ws.acp.attachments, AiMode::Codex => &mut state.ws.codex.attachments, AiMode::Grok => &mut state.ws.grok.attachments, AiMode::Cursor => &mut state.ws.cursor.attachments };
                if content.len() <= ai_context::MAX_BYTES && attachments.len() < 16 { attachments.push(ai_context::Attachment { name, content, mime: None }); }
                else { state.notice = Some(("Attachment limit reached (16 files, 8 MiB each).".into(), std::time::Instant::now())); }
            }
        }
        Message::AiAct(act) => task = state.ai_act(act),
        Message::InlineEditStart => task = state.start_inline_edit(),
        Message::InlineEditInput(text) => {
            if let Some(edit) = state.ws.inline_edit.as_mut().filter(|edit| matches!(edit.stage, inline_edit::Stage::Prompt)) {
                edit.instruction = text;
            }
        }
        Message::InlineEditSubmit => task = state.submit_inline_edit(),
        Message::InlineEditReady(id, result) => {
            let root = state.root_or_cwd();
            let Some(edit) = state.ws.inline_edit.as_mut().filter(|edit| matches!(edit.stage, inline_edit::Stage::Working(current) if current == id)) else {
                return Task::none();
            };
            match result {
                Ok(reply) => {
                    let replacement = inline_edit::replacement(&reply, &edit.original);
                    let name = edit.path.as_deref().and_then(Path::file_name).map_or_else(|| "Untitled".into(), |name| name.to_string_lossy().into_owned());
                    state.ws.git_preview = Some(git_preview::Preview {
                        root, path: format!("{}{name}", inline_edit::PREVIEW_PREFIX),
                        result: Some(Ok(git_preview::text_patch(&edit.original, &replacement))), live: false,
                    });
                    edit.stage = inline_edit::Stage::Review(replacement);
                    state.ws.focus = Focus::Editor;
                }
                Err(err) => {
                    edit.stage = inline_edit::Stage::Prompt;
                    state.notify(format!("AI edit failed: {err}"));
                }
            }
        }
        Message::InlineEditAccept => {
            if let Some(inline_edit::InlineEdit { path, start, end, original, stage: inline_edit::Stage::Review(replacement), .. }) = state.ws.inline_edit.take() {
                state.ws.git_preview = None;
                match state.ws.tabs.iter().position(|tab| tab.path == path) {
                    Some(index) if state.ws.tabs[index].content.text_between(start, end).as_deref() == Some(original.as_str()) => {
                        state.ws.active_tab = index;
                        let before = state.ws.tabs[index].content.undo_count();
                        state.ws.tabs[index].content.replace_range(start, end, &replacement);
                        state.ws.focus = Focus::Editor;
                        state.mark_edited_if_changed(before);
                    }
                    _ => state.notify("The code changed while the edit was being written; try again"),
                }
            }
        }
        Message::InlineEditCancel => {
            if state.ws.inline_edit.take().is_some_and(|edit| matches!(edit.stage, inline_edit::Stage::Review(_))) {
                state.ws.git_preview = None;
            }
        }
        Message::Acp(acp::Message::ViewDiff(entry, index)) => {
            let diff = state.ws.acp.diff(entry, index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Cursor(acp::Message::ViewDiff(entry, index)) => {
            let diff = state.ws.cursor.diff(entry, index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Grok(acp::Message::ViewDiff(entry, index)) => {
            let diff = state.ws.grok.diff(entry, index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Codex(acp::Message::ViewDiff(entry, index)) => {
            let diff = state.ws.codex.diff(entry, index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Chat(chat::Message::ViewDiff(index)) => {
            let diff = state.ws.chat.diff(index).cloned();
            state.open_ai_diff(diff);
        }
        Message::Chat(chat::Message::Markdown(ai_markdown::Action::Insert(code)))
        | Message::Acp(acp::Message::Markdown(ai_markdown::Action::Insert(code)))
        | Message::Cursor(acp::Message::Markdown(ai_markdown::Action::Insert(code)))
        | Message::Grok(acp::Message::Markdown(ai_markdown::Action::Insert(code)))
        | Message::Codex(acp::Message::Markdown(ai_markdown::Action::Insert(code))) => state.insert_code(&code),
        // One autopilot for every panel and agent, saved until turned off.
        Message::Chat(chat::Message::Autopilot(on))
        | Message::Acp(acp::Message::Autopilot(on))
        | Message::Cursor(acp::Message::Autopilot(on))
        | Message::Grok(acp::Message::Autopilot(on))
        | Message::Codex(acp::Message::Autopilot(on)) => {
            ai_approval::save_autopilot(on);
            state.for_each_loaded(|workspace| {
                workspace.chat.set_autopilot(on);
                for panel in [&mut workspace.acp, &mut workspace.cursor, &mut workspace.grok, &mut workspace.codex] { panel.set_autopilot(on); }
            });
        }
        Message::Cursor(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, acp::Message::InputChanged(_) | acp::Message::UsePrompt(_));
            if matches!(msg, acp::Message::Send) { state.attach_mentions(); }
            task = acp::update(&mut state.ws.cursor, msg, cwd).map(Message::Cursor);
            if input { state.refresh_mentions(); }
        }
        Message::Grok(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, acp::Message::InputChanged(_) | acp::Message::UsePrompt(_));
            if matches!(msg, acp::Message::Send) { state.attach_mentions(); }
            task = acp::update(&mut state.ws.grok, msg, cwd).map(Message::Grok);
            if input { state.refresh_mentions(); }
        }
        Message::Codex(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, acp::Message::InputChanged(_) | acp::Message::UsePrompt(_));
            if matches!(msg, acp::Message::Send) { state.attach_mentions(); }
            task = acp::update(&mut state.ws.codex, msg, cwd).map(Message::Codex);
            if input { state.refresh_mentions(); }
        }
        Message::Chat(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, chat::Message::InputChanged(_) | chat::Message::UsePrompt(_));
            if matches!(msg, chat::Message::Send) { state.attach_mentions(); }
            task = chat::update(&mut state.ws.chat, msg, cwd).map(Message::Chat);
            if input { state.refresh_mentions(); }
        }
        Message::Acp(msg) => {
            let cwd = state.root_or_cwd();
            let input = matches!(msg, acp::Message::InputChanged(_) | acp::Message::UsePrompt(_));
            if matches!(msg, acp::Message::Send) { state.attach_mentions(); }
            task = acp::update(&mut state.ws.acp, msg, cwd).map(Message::Acp);
            if input { state.refresh_mentions(); }
        }
        Message::Tick => {
            match about::native_menu_requested() {
                Some(about::NativeMenu::About) => task = about::update(&mut state.about, about::Message::Open).map(Message::About),
                Some(about::NativeMenu::CheckForUpdates) => task = update(state, Message::CheckUpdateFromMenu),
                None => {}
            }
            state.updater.poll();
            let was_asking = !state.passphrase_prompts.is_empty();
            while let Some(request) = git::askpass::next_request() { state.passphrase_prompts.push_back(request); }
            if !was_asking && !state.passphrase_prompts.is_empty() {
                task = Task::batch([task, iced::widget::operation::focus(passphrase_input_id())]);
            }
            if state.announce_update_check && !state.updater.busy() {
                state.announce_update_check = false;
                let detail = state.updater.detail.clone();
                state.notify(detail);
            }
            if state.notice.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(8)) { state.notice = None; }
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
            // Every loaded project keeps running while its tab is in the background.
            let loaded: Vec<WorkspaceId> = state.workspaces.iter()
                .filter(|entry| !matches!(entry.slot, Slot::Unloaded(_))).map(|entry| entry.id).collect();
            for id in loaded {
                task = Task::batch([task, route(state, id, Message::WorkspaceTick)]);
            }
        }
        Message::WorkspaceTick => {
            if state.ws.terminal.poll() && state.in_background() { state.ws.terminal_activity = true; }
            if state.ws.git_status.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(8)) { state.ws.git_status = None; }
            let cwd = state.root_or_cwd();
            state.ws.lsp.set_root(&cwd);
            task = Task::batch([task, state.poll_lsp()]);
            state.ws.chat.set_project(&cwd);
            state.ws.acp.ensure_loaded(&cwd);
            state.ws.codex.ensure_loaded(&cwd);
            state.ws.grok.ensure_loaded(&cwd);
            state.ws.cursor.ensure_loaded(&cwd);
            // Only an ACP chat needs the shell PATH, so look it up once one is open.
            if state.ws.ai_visible && matches!(state.ws.ai_mode, AiMode::Acp | AiMode::Codex | AiMode::Grok | AiMode::Cursor) { agent_launch::warm_up(); }
            // Connect as soon as the panel shows, so it names the model in use straight away.
            if state.ws.ai_visible {
                match state.ws.ai_mode {
                    AiMode::Acp => state.ws.acp.connect_for_display(&cwd),
                    AiMode::Cursor => state.ws.cursor.connect_for_display(&cwd),
                    AiMode::Grok => state.ws.grok.connect_for_display(&cwd),
                    AiMode::Codex => state.ws.codex.connect_for_display(&cwd),
                    _ => {}
                }
            }
            state.ws.chat.poll();
            state.ws.acp.poll();
            if let Some(request) = state.ws.acp.take_terminal_login() {
                state.ws.terminal.open_claude_login(request, &cwd);
                if !state.ws.terminal_visible { task = Task::batch([task, update(state, Message::TerminalToggle)]); }
                // A background tab's login waits there; focus stays where the user is.
                if state.in_background() {
                    state.ws.terminal.set_focused(false);
                } else {
                    state.ws.terminal.set_focused(true);
                    task = Task::batch([task, iced::widget::operation::focus("terminal-canvas")]);
                }
            }
            state.ws.codex.poll();
            state.ws.grok.poll();
            state.ws.cursor.poll();
            let cursor_finished = state.ws.cursor.take_finished();
            let grok_finished = state.ws.grok.take_finished();
            let codex_finished = state.ws.codex.take_finished();
            let claude_finished = state.ws.acp.take_finished();
            let local_changed = state.ws.chat.take_files_changed();
            if cursor_finished || grok_finished || codex_finished || claude_finished || local_changed {
                let reload_task = state.reload_open_tabs();
                if let Some(root) = state.ws.root.clone() {
                    task = Task::batch([reload_task, refresh_dir_task(root)]);
                } else { task = reload_task; }
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
    let reviewing = state.ws.git_preview.as_ref().is_some_and(|preview| !preview.live && preview.path.starts_with(inline_edit::PREVIEW_PREFIX));
    if !reviewing && state.ws.inline_edit.as_ref().is_some_and(|edit| matches!(edit.stage, inline_edit::Stage::Review(_))) {
        state.ws.inline_edit = None;
    }
    state.persist_session();
    task
}

fn view(state: &State) -> Element<'_, Message> {
    let top_bar = view_top_bar(state);
    let status_bar = view_status_bar(state);

    let panes = PaneGrid::new(&state.ws.panes, |_id, kind, _is_maximized| {
        let content: Element<'_, Message> = match kind {
            PaneKind::Terminal => state.ws.terminal.view(
                !state.quick_open.visible && !state.command_palette.visible && !state.goto_line.visible
                    && !state.theme_install.visible && !state.about.visible).map(Message::Terminal),
            PaneKind::Sidebar => container(view_sidebar(state))
                .padding(1)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(move |theme| sidebar_panel_style(theme, state.ws.sidebar_mode))
                .into(),
            PaneKind::Main => view_editor(state),
            PaneKind::Ai => container(view_ai_sidebar(state))
                .padding(1)
                .height(Length::Fill)
                .style(workspace_panel_style)
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

    let base: Element<'_, Message> = column![top_bar, view_workspace_strip(state), tab_content_frame(panes), status_bar].into();
    let toast: Option<Element<'_, Message>> = state.notice.as_ref().map(|(message, _)| {
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
        container(toast).padding(16).align_right(Length::Fill).align_bottom(Length::Fill).into()
    });
    let prompt: Option<Element<'_, Message>> = state.ws.lsp_prompt.map(|server| {
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
        container(prompt).padding(16).align_left(Length::Fill).align_bottom(Length::Fill).into()
    });

    let overlay: Option<Element<'_, Message>> = if let Some(request) = state.passphrase_prompts.front() {
        Some(view_passphrase_prompt(state, request))
    } else if let Some(path) = &state.folder_prompt {
        Some(view_folder_prompt(state, path))
    } else if state.theme_install.visible {
        Some(theme_install::view(&state.theme_install).map(Message::ThemeInstall))
    } else if state.quick_open.visible {
        Some(quick_open::view(&state.quick_open, state.ws.root.as_deref()).map(Message::QuickOpen))
    } else if state.command_palette.visible {
        Some(command_palette::view(&state.command_palette).map(Message::CommandPalette))
    } else if state.goto_line.visible {
        let line_count = state.ws.tabs.get(state.ws.active_tab).map(|t| t.content.line_count()).unwrap_or(0);
        Some(goto_line::view(&state.goto_line, line_count).map(Message::GotoLine))
    } else if state.about.visible {
        Some(about::view(&state.about, &state.app_theme.iced).map(Message::About))
    } else {
        None
    };
    layered(base, toast, prompt, overlay)
}

/// The workspace tabs under the top bar: one per project, equal widths that shrink until
/// they'd get too narrow and then scroll, plus "+" for a new tab.
fn view_workspace_strip(state: &State) -> Element<'_, Message> {
    let count = state.workspaces.len();
    let labels = workspaces::labels(&state.workspace_roots());
    let dragging = state.workspace_drag.is_some();
    let strip = iced::widget::responsive(move |size| {
        // Room for the tabs: the strip minus the "+" button after them.
        let width = workspaces::tab_width(size.width - 34.0, count);
        let mut tabs = row![].spacing(1);
        for (index, entry) in state.workspaces.iter().enumerate() {
            tabs = tabs.push(view_workspace_tab(state, index, entry.id, labels[index].clone(), width));
        }
        // While a tab is dragged, the pointer's position over the tabs says where it goes.
        let mut tabs = iced::widget::mouse_area(tabs);
        if dragging { tabs = tabs.on_move(move |point| Message::WorkspaceDragMove(point.x, width)); }
        let new_tab = icon_control(lucide_icons::Icon::Plus, if cfg!(target_os = "macos") { "New tab (⌘T)" } else { "New tab (Ctrl+T)" },
            Some(Message::WorkspaceNew), false);
        crate::scrollbar::scrollable(row![tabs, new_tab].spacing(4).align_y(iced::Alignment::Center))
            .direction(iced::widget::scrollable::Direction::Horizontal(tab_rail()))
            .width(Length::Fill)
            .into()
    });
    let bar = container(strip).width(Length::Fill)
        .padding(iced::Padding { top: 2.0, right: 1.0, bottom: 0.0, left: 1.0 }).height(PANEL_HEADER_HEIGHT);
    connected_tab_strip(bar)
}

fn view_workspace_tab(state: &State, index: usize, id: WorkspaceId, label: String, width: f32) -> Element<'_, Message> {
    let is_active = index == state.active;
    let workspace = state.workspace(index);
    let root = state.workspace_root(index).map(Path::to_path_buf);
    let unsaved = state.workspace_unsaved(index) > 0;
    let busy = workspace.is_some_and(Workspace::ai_busy);
    let activity = !is_active && workspace.is_some_and(|workspace| workspace.terminal_activity);

    let glyph: char = if root.is_some() { lucide_icons::Icon::Folder } else { lucide_icons::Icon::File }.into();
    let mut title = row![
        text(glyph).font(iced::Font::with_name("lucide")).size(13).line_height(tab_line_height()),
        text(label).size(13).line_height(tab_line_height()).wrapping(iced::widget::text::Wrapping::None).width(Length::Fill),
    ].spacing(6).align_y(iced::Alignment::Center);
    if busy {
        title = title.push(iced::widget::tooltip(
            iced::widget::canvas(workspaces::Spinner).width(12).height(12),
            container(text("AI assistant is working").size(12)).padding([4, 8]).style(iced::widget::container::rounded_box),
            iced::widget::tooltip::Position::Bottom,
        ));
    }
    let dot = |color: fn(&iced::Theme) -> iced::Color, tip: &'static str| -> Element<'_, Message> {
        iced::widget::tooltip(
            text("●").size(10).style(move |theme: &iced::Theme| iced::widget::text::Style { color: Some(color(theme)) }),
            container(text(tip).size(12)).padding([4, 8]).style(iced::widget::container::rounded_box),
            iced::widget::tooltip::Position::Bottom,
        ).into()
    };
    if activity {
        title = title.push(dot(|theme| theme.extended_palette().primary.base.color, "New terminal output"));
    }
    if unsaved {
        title = title.push(dot(|theme| theme.extended_palette().background.base.text, "Unsaved changes"));
    }
    let close_label = if cfg!(target_os = "macos") { "Close tab (⌘⇧W)" } else { "Close tab (Ctrl+Shift+W)" };
    let tip = root.as_ref().map_or_else(|| "No folder open".to_string(), |root| root.display().to_string());
    // Pressing the name selects (and may drag) the tab; the close button sits beside that
    // area, so closing a background tab never selects it first.
    let name = iced::widget::tooltip(
        iced::widget::mouse_area(container(title).width(Length::Fill).height(Length::Fill).align_y(iced::Alignment::Center))
            .on_press(Message::WorkspaceDragStart(id))
            .on_middle_press(Message::WorkspaceClose(id))
            .interaction(iced::mouse::Interaction::Pointer),
        container(text(tip).size(12)).padding([6, 10]).style(overlay_style),
        iced::widget::tooltip::Position::Bottom,
    ).gap(8).delay(std::time::Duration::from_millis(600)).snap_within_viewport(true);
    let content = row![
        name,
        tab_close_control(close_label, Message::WorkspaceClose(id)),
    ].spacing(4).align_y(iced::Alignment::Center).padding([0, 4]).height(Length::Fill);

    let body = joined_tab(container(content)
        .width(width).height(TAB_HEIGHT).padding(1)
        .style(move |theme| tab_surface_style(theme, is_active)), is_active);
    let has_root = root.is_some();
    let count = state.workspaces.len();
    ContextMenu::new(body, move || {
        container(column![
            menu_button("Close Tab".into(), Message::WorkspaceClose(id)),
            menu_button_maybe("Close Other Tabs".into(), (count > 1).then_some(Message::WorkspaceCloseOthers(id))),
            menu_button_maybe("Close Tabs to the Right".into(), (index + 1 < count).then_some(Message::WorkspaceCloseRight(id))),
            iced::widget::rule::horizontal(1),
            menu_button_maybe("Copy Path".into(), has_root.then_some(Message::WorkspaceCopyPath(id))),
            menu_button_maybe(reveal_label().into(), has_root.then_some(Message::WorkspaceReveal(id))),
        ].spacing(2).width(220))
        .padding(6).style(iced::widget::container::rounded_box).into()
    }).into()
}

/// "Replace the current project or open a new tab?" for a folder opened while the shown tab
/// already has one.
fn view_folder_prompt<'a>(state: &'a State, path: &'a Path) -> Element<'a, Message> {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
    let current = state.workspace_label(state.active);
    let panel = container(column![
        text(format!("Open \"{name}\"")).size(16),
        text(path.display().to_string()).size(12).style(iced::widget::text::secondary),
        text(format!("\"{current}\" is open in this tab.")).size(13),
        row![
            button(text("Open in New Tab").size(13)).padding([6, 14]).on_press(Message::FolderPromptChoice(Some(FolderChoice::NewTab))),
            button(text("Replace Current").size(13)).padding([6, 14]).style(iced::widget::button::secondary)
                .on_press(Message::FolderPromptChoice(Some(FolderChoice::Replace))),
            Space::new().width(Length::Fill),
            secondary_button("Cancel").on_press(Message::FolderPromptChoice(None)),
        ].spacing(8).align_y(iced::Alignment::Center),
    ].spacing(12)).padding(16).width(Length::Fill).max_width(480).style(overlay_style);
    let backdrop = container(Space::new().width(Length::Fill).height(Length::Fill)).style(|theme: &iced::Theme| iced::widget::container::Style {
        background: Some(iced::Color { a: 0.4, ..theme.extended_palette().background.base.color.inverse() }.into()),
        ..Default::default()
    });
    iced::widget::mouse_area(iced::widget::stack![
        backdrop,
        container(iced::widget::opaque(panel)).width(Length::Fill).height(Length::Fill)
            .align_x(iced::Alignment::Center).padding(iced::Padding { top: 80.0, ..iced::Padding::default() }),
    ]).on_press(Message::FolderPromptChoice(None)).into()
}

fn passphrase_input_id() -> iced::widget::Id {
    iced::widget::Id::new("ssh-passphrase")
}

/// "Enter the passphrase for this SSH key", for a fetch, pull or push waiting on ssh.
fn view_passphrase_prompt<'a>(state: &'a State, request: &'a git::askpass::Request) -> Element<'a, Message> {
    let panel = container(column![
        text("SSH key passphrase").size(16),
        text(request.key.clone()).size(12).style(iced::widget::text::secondary),
        text_input("Passphrase", &state.passphrase).id(passphrase_input_id()).secure(true).size(13).padding(8)
            .on_input(Message::PassphraseInput).on_submit(Message::PassphraseDone(true)),
        iced::widget::checkbox(state.remember_passphrase).label("Remember for this session").size(14).text_size(12)
            .on_toggle(Message::RememberPassphrase),
        row![
            Space::new().width(Length::Fill),
            secondary_button("Cancel").on_press(Message::PassphraseDone(false)),
            button(text("OK").size(13)).padding([6, 14]).on_press(Message::PassphraseDone(true)),
        ].spacing(8).align_y(iced::Alignment::Center),
    ].spacing(12)).padding(16).width(Length::Fill).max_width(480).style(overlay_style);
    let backdrop = container(Space::new().width(Length::Fill).height(Length::Fill)).style(|theme: &iced::Theme| iced::widget::container::Style {
        background: Some(iced::Color { a: 0.4, ..theme.extended_palette().background.base.color.inverse() }.into()),
        ..Default::default()
    });
    // Clicks outside the panel do nothing: a stray click must not cancel the push.
    iced::widget::opaque(iced::widget::stack![
        backdrop,
        container(iced::widget::opaque(panel)).width(Length::Fill).height(Length::Fill)
            .align_x(iced::Alignment::Center).padding(iced::Padding { top: 80.0, ..iced::Padding::default() }),
    ])
}

/// A change's staged lines (removed, red) above its buffer lines (added, green), with Restore.
fn view_diff_popup(hunk: &git_diff::Hunk) -> Element<'_, Message> {
    let line = |prefix: &'static str, line: &str, removed: bool| -> Element<'_, Message> {
        container(text(format!("{prefix}{line}")).font(fonts::FONT).size(12).wrapping(iced::widget::text::Wrapping::None))
            .width(Length::Fill).padding([0, 6])
            .style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let color = if removed { palette.danger.base.color } else { palette.success.base.color };
                iced::widget::container::Style { background: Some(iced::Color { a: 0.18, ..color }.into()), ..Default::default() }
            })
            .into()
    };
    let mut lines = column![].spacing(0);
    for old in &hunk.old { lines = lines.push(line("- ", old, true)); }
    for new in &hunk.new { lines = lines.push(line("+ ", new, false)); }
    let header = row![
        button(text("Restore").size(12)).padding([3, 10]).on_press(Message::DiffRestore),
        Space::new().width(Length::Fill),
        icon_control(lucide_icons::Icon::X, "Close (Esc)", Some(Message::DiffPopupClose), false),
    ].spacing(8).align_y(iced::Alignment::Center);
    let popup = container(column![header, crate::scrollbar::scrollable(lines).height(Length::Shrink)].spacing(6))
        .padding(6).max_width(720).max_height(360).clip(true)
        .style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            iced::widget::container::Style {
                background: Some(palette.background.weak.color.into()),
                border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 6.0.into() },
                shadow: iced::Shadow { color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.3), offset: iced::Vector::new(0.0, 2.0), blur_radius: 8.0 },
                ..Default::default()
            }
        });
    iced::widget::opaque(popup)
}

/// Lays the toast, the language-server prompt and the open overlay over `base` as four layers
/// that are always present. iced matches widget state by position in the tree, so wrapping
/// `base` in a `stack` only while a layer was visible rebuilt the whole tree each time a toast
/// came or went (it auto-dismisses after 8 seconds), which dropped the focus of the commit
/// message and AI message boxes mid-typing.
fn layered<'a, M: 'a>(
    base: Element<'a, M>, toast: Option<Element<'a, M>>, prompt: Option<Element<'a, M>>, overlay: Option<Element<'a, M>>,
) -> Element<'a, M> {
    let slot = |layer: Option<Element<'a, M>>| layer.unwrap_or_else(|| Space::new().into());
    iced::widget::stack![base, slot(toast), slot(prompt), slot(overlay)].into()
}

/// iced keeps a text editor's focus in widget-tree state, which a view change that rebuilds the
/// editor's subtree silently drops. Reads and sets that state on a `Tree`, with no renderer.
#[cfg(test)]
pub(crate) mod tree_focus {
    use iced::advanced::text::highlighter::PlainText;
    use iced::advanced::widget::operation::Focusable;
    use iced::advanced::widget::{tree::Tag, Tree};
    use iced::widget::text_editor;
    use iced::Element;

    type EditorState = text_editor::State<PlainText>;

    fn editor(tree: &mut Tree) -> Option<&mut EditorState> {
        if tree.tag == Tag::of::<EditorState>() {
            return Some(tree.state.downcast_mut());
        }
        tree.children.iter_mut().find_map(editor)
    }

    /// Every node's tag and child count match: the state each widget will find is its own.
    fn same_shape(a: &Tree, b: &Tree) -> bool {
        a.tag == b.tag && a.children.len() == b.children.len()
            && a.children.iter().zip(&b.children).all(|(a, b)| same_shape(a, b))
    }

    /// Whether the first text editor in `before` is still focused once the view is `after`.
    /// Panics if the diffed tree no longer matches `after`, since a widget handed another
    /// widget's state panics at runtime.
    pub(crate) fn keeps_focus<M>(before: Element<'_, M>, after: Element<'_, M>) -> bool {
        let mut tree = Tree::new(&before);
        editor(&mut tree).expect("the view has a text editor").focus();
        tree.diff(&after);
        assert!(same_shape(&tree, &Tree::new(&after)), "the diffed widget tree does not match the new view");
        editor(&mut tree).is_some_and(|state| state.is_focused())
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    fn dropped(path: &str) -> iced::Event {
        iced::Event::Window(iced::window::Event::FileDropped(PathBuf::from(path)))
    }

    #[test]
    fn drops_open_in_the_editor_unless_a_widget_took_them() {
        use iced::event::Status;
        let window = iced::window::Id::unique();
        assert!(matches!(
            handle_raw_drop_event(dropped("notes.md"), Status::Ignored, window),
            Some(Message::PathDropped(path)) if path == PathBuf::from("notes.md")
        ));
        // The AI composer captures drops on itself; they must not also open a tab.
        assert!(handle_raw_drop_event(dropped("notes.md"), Status::Captured, window).is_none());
        let hovered = iced::Event::Window(iced::window::Event::FileHovered(PathBuf::from("notes.md")));
        assert!(handle_raw_drop_event(hovered, Status::Ignored, window).is_none());
    }
}

#[cfg(test)]
mod git_notification_tests {
    use super::*;

    #[test]
    fn successful_git_operations_use_status_messages() {
        let mut messages = vec![
            (git::Message::Committed(Ok(())), "Commit created"),
            (git::Message::Staged(Ok(())), "Staging updated"),
            (git::Message::Discarded(Ok(())), "Unstaged changes discarded"),
            (git::Message::BranchDone(Ok(())), "Switched branch"),
        ];
        for sync in git::remote::Sync::ALL { messages.push((git::Message::SyncDone(sync, Ok(())), sync.done())); }
        for (message, expected) in messages {
            assert_eq!(git_notification(&message), Some(Ok(expected.into())));
        }
        assert_eq!(git_notification(&git::Message::Refresh), None);
        assert_eq!(git_notification(&git::Message::Refreshed(Ok(Default::default()))), None);
        assert_eq!(git_notification(&git::Message::CommitMessageEdited(iced::widget::text_editor::Action::Edit(
            iced::widget::text_editor::Edit::Insert('a')))), None);
    }

    #[test]
    fn git_failures_keep_alerts() {
        let error = "operation failed".to_string();
        let mut messages = vec![
            git::Message::Committed(Err(error.clone())), git::Message::Staged(Err(error.clone())),
            git::Message::Discarded(Err(error.clone())), git::Message::Refreshed(Err(error.clone())),
            git::Message::BranchDone(Err(error.clone())), git::Message::BranchesLoaded(Err(error.clone())),
        ];
        for sync in git::remote::Sync::ALL { messages.push(git::Message::SyncDone(sync, Err(error.clone()))); }
        for message in messages { assert_eq!(git_notification(&message), Some(Err("Git: operation failed".into()))); }
        assert_eq!(git_notification(&git::Message::MessageGenerated(Err(error))), Some(Err("Could not write a commit message: operation failed".into())));
    }
}

#[cfg(test)]
mod layer_tests {
    use super::*;
    use iced::widget::text_editor;

    fn base(content: &text_editor::Content) -> Element<'_, Message> {
        iced::widget::column![text_editor(content)].into()
    }

    fn layer<'a>() -> Element<'a, Message> {
        container(text("layer")).into()
    }

    #[test]
    fn layers_coming_and_going_keep_the_editor_focused() {
        let content = text_editor::Content::new();
        let with = |toast: bool, prompt: bool, overlay: bool| {
            layered(base(&content), toast.then(layer), prompt.then(layer), overlay.then(layer))
        };
        for (toast, prompt, overlay) in [(true, false, false), (false, true, false), (false, false, true), (true, true, true)] {
            assert!(tree_focus::keeps_focus(with(false, false, false), with(toast, prompt, overlay)));
            assert!(tree_focus::keeps_focus(with(toast, prompt, overlay), with(false, false, false)));
        }
    }
}

fn view_ai_sidebar(state: &State) -> Element<'_, Message> {
    let controls = match state.ws.ai_mode {
        AiMode::Http => chat::conversation_controls(&state.ws.chat).map(Message::Chat),
        AiMode::Cursor => acp::conversation_controls(&state.ws.cursor).map(Message::Cursor),
        AiMode::Grok => acp::conversation_controls(&state.ws.grok).map(Message::Grok),
        AiMode::Codex => acp::conversation_controls(&state.ws.codex).map(Message::Codex),
        AiMode::Acp => acp::conversation_controls(&state.ws.acp).map(Message::Acp),
    };
    let mode_row = row![
        iced::widget::pick_list([AiMode::Http, AiMode::Acp, AiMode::Codex, AiMode::Grok, AiMode::Cursor], Some(state.ws.ai_mode), Message::AiModeSelected).text_size(12).padding([4, 8]),
        controls,
        Space::new().width(Length::Fill),
    ].spacing(8).align_y(iced::Alignment::Center);
    let composer = ai_composer::Context {
        sources: state.ws.tree.as_ref().map(|tree| tree.drag_sources()).unwrap_or_default(),
        can_reference: state.ws.tabs.get(state.ws.active_tab).is_some(),
        theme: &state.app_theme.iced,
        mentions: &state.ws.mentions,
    };
    let panel: Element<'_, Message> = match state.ws.ai_mode {
        AiMode::Http => chat::view(&state.ws.chat, composer).map(Message::Chat),
        AiMode::Cursor => acp::view(&state.ws.cursor, state.root_or_cwd(), composer).map(Message::Cursor),
        AiMode::Grok => acp::view(&state.ws.grok, state.root_or_cwd(), composer).map(Message::Grok),
        AiMode::Codex => acp::view(&state.ws.codex, state.root_or_cwd(), composer).map(Message::Codex),
        AiMode::Acp => acp::view(&state.ws.acp, state.root_or_cwd(), composer).map(Message::Acp),
    };

    column![panel_header(mode_row), panel].height(Length::Fill).into()
}

/// Shared 26px target; utility glyphs stay quieter than send/stop actions.
pub(crate) fn icon_control<'a, M: Clone + 'a>(
    icon: lucide_icons::Icon,
    label: &'a str,
    message: Option<M>,
    selected: bool,
) -> Element<'a, M> {
    let glyph: char = icon.into();
    let primary = matches!(icon, lucide_icons::Icon::SendHorizonal | lucide_icons::Icon::CircleStop);
    let control = button(container(text(glyph).font(iced::Font::with_name("lucide")).size(if primary { 14 } else { 13 }))
        .center_x(Length::Fill).center_y(Length::Fill))
        .width(26).height(26).padding(0)
        .style(move |theme, status| toolbar_button_style(theme, status, selected, primary))
        .on_press_maybe(message);
    iced::widget::tooltip(control, container(text(label).size(12)).padding([4, 8]).style(iced::widget::container::rounded_box),
        iced::widget::tooltip::Position::FollowCursor).into()
}

fn toolbar_button_style(theme: &iced::Theme, status: iced::widget::button::Status, selected: bool, primary: bool) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let mut style = flat_button_style(theme, status);
    if status != Status::Disabled {
        if selected {
            style.background = Some(theme.extended_palette().primary.weak.color.into());
            style.text_color = theme.extended_palette().primary.weak.text;
        } else if !primary && status == Status::Active {
            style.text_color.a *= 0.7;
        }
    }
    style
}

/// Compact text actions share a baseline and padding across panels and dialogs.
pub(crate) fn secondary_button<'a, M: 'a>(label: impl Into<String>) -> iced::widget::button::Button<'a, M> {
    button(text(label.into()).size(12)).padding([4, 8]).style(flat_button_style)
}

/// A quiet close glyph with the same comfortable target as toolbar controls.
pub(crate) fn tab_close_control<'a, M: Clone + 'a>(label: &'a str, message: M) -> Element<'a, M> {
    let control = button(container(text(char::from(lucide_icons::Icon::X))
        .font(iced::Font::with_name("lucide")).size(11).line_height(tab_line_height()))
        .center_x(Length::Fill).center_y(Length::Fill))
        .width(26).height(26).padding(0)
        .style(tab_close_style).on_press(message);
    iced::widget::tooltip(control,
        container(text(label).size(12)).padding([4, 8]).style(iced::widget::container::rounded_box),
        iced::widget::tooltip::Position::FollowCursor).into()
}

fn tab_close_style(theme: &iced::Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
    let mut style = flat_button_style(theme, status);
    if status == iced::widget::button::Status::Active {
        style.text_color.a *= 0.45;
    }
    style
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
        Status::Pressed => base.with_background(palette.background.strong.color),
    }
}

/// One surface for the entire tab, including the independently clickable close control.
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
        border: iced::Border {
            radius: iced::border::Radius { top_left: 7.0, top_right: 7.0, ..Default::default() },
            width: 1.0,
            color: if is_active {
                iced::Color { a: 0.45, ..palette.primary.base.color }
            } else {
                iced::Color { a: if matches!(status, Status::Hovered | Status::Pressed) { 0.25 } else { 0.10 }, ..palette.background.base.text }
            },
        },
        ..Style::default()
    };
    match status {
        Status::Disabled => Style { text_color: iced::Color { a: 0.35, ..base.text_color }, ..base }
            .with_background(iced::Color::TRANSPARENT),
        Status::Active => base.with_background(if is_active { palette.background.base.color } else { iced::Color::TRANSPARENT }),
        Status::Hovered => base.with_background(if is_active { palette.background.base.color } else { palette.background.strong.color }),
        Status::Pressed => base.with_background(if is_active { palette.background.base.color } else { palette.background.strong.color }),
    }
}

fn tab_line_height() -> iced::widget::text::LineHeight {
    iced::widget::text::LineHeight::Absolute(iced::Pixels(18.0))
}

// Tab geometry matches the approved connected-tab layout; only the rail is thinner.
const TAB_HEIGHT: f32 = 30.0;
const PANEL_HEADER_HEIGHT: f32 = 32.0;

const TAB_RAIL_WIDTH: f32 = 2.0;
const TAB_RAIL_MARGIN: f32 = 1.0;

fn tab_rail() -> iced::widget::scrollable::Scrollbar {
    iced::widget::scrollable::Scrollbar::new()
        .width(TAB_RAIL_WIDTH).scroller_width(TAB_RAIL_WIDTH).margin(TAB_RAIL_MARGIN)
}

pub(crate) fn panel_header<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content).width(Length::Fill).height(PANEL_HEADER_HEIGHT)
        .padding(iced::Padding { top: 0.0, right: 8.0, bottom: 0.0, left: 8.0 })
        .align_y(iced::Alignment::Center).into()
}

fn menu_bar_style(theme: &iced::Theme, status: iced_aw::style::Status) -> iced_aw::style::menu_bar::Style {
    let mut style = iced_aw::style::menu_bar::primary(theme, status);
    style.bar_background = iced::Color::TRANSPARENT.into();
    style.bar_border = iced::Border::default();
    style.bar_shadow = iced::Shadow::default();
    style
}

fn tab_context_menu_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let menu = menu_bar_style(theme, iced_aw::style::Status::Active);
    iced::widget::container::Style {
        background: Some(menu.menu_background),
        border: menu.menu_border,
        shadow: menu.menu_shadow,
        text_color: Some(theme.extended_palette().background.base.text),
        ..Default::default()
    }
}

fn tab_surface_style(theme: &iced::Theme, selected: bool) -> iced::widget::container::Style {
    let style = tab_button_style(theme, iced::widget::button::Status::Active, selected);
    iced::widget::container::Style {
        background: style.background,
        text_color: Some(style.text_color),
        border: style.border,
        ..Default::default()
    }
}

/// Cover only the selected tab's bottom stroke, preserving both side strokes.
fn joined_tab<'a, M: 'a>(content: impl Into<Element<'a, M>>, selected: bool) -> Element<'a, M> {
    let content = content.into();
    if !selected { return content; }
    iced::widget::stack![content, container(
        container(Space::new().width(Length::Fill).height(1))
            .style(|theme: &iced::Theme| iced::widget::container::Style {
                background: Some(theme.extended_palette().background.base.color.into()),
                ..Default::default()
            })
    ).padding(iced::Padding { left: 1.0, right: 1.0, ..Default::default() })
        .width(Length::Fill).height(Length::Fill).align_y(iced::Alignment::End)].into()
}

fn tab_frame_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let mut style = tab_surface_style(theme, true);
    style.border.radius = iced::border::Radius { bottom_left: 6.0, bottom_right: 6.0, ..Default::default() };
    style
}

/// The strip supplies the top edge; this frame supplies the sides and bottom.
fn tab_content_frame<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    iced::widget::stack![
        container(content).padding(1).width(Length::Fill).height(Length::Fill).style(tab_frame_style),
        container(container(Space::new().width(Length::Fill).height(1))
            .style(|theme: &iced::Theme| iced::widget::container::Style {
                background: Some(theme.extended_palette().background.base.color.into()),
                ..Default::default()
            }))
            .padding(iced::Padding { left: 1.0, right: 1.0, ..Default::default() })
            .width(Length::Fill)
    ].into()
}

fn tab_strip_style(theme: &iced::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(theme.extended_palette().background.base.color.into()),
        ..Default::default()
    }
}

fn workspace_panel_style(theme: &iced::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(theme.extended_palette().background.base.color.into()),
        border: tab_frame_style(theme).border.rounded(6),
        ..Default::default()
    }
}

fn sidebar_panel_style(theme: &iced::Theme, mode: SidebarMode) -> iced::widget::container::Style {
    match mode {
        SidebarMode::Tree | SidebarMode::ProjectSearch | SidebarMode::Git => workspace_panel_style(theme),
    }
}

fn connected_tab_strip<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    connected_tab_strip_with_style(content, chrome_style)
}

fn connected_tab_strip_with_style<'a, M: 'a>(
    content: impl Into<Element<'a, M>>,
    style: fn(&iced::Theme) -> iced::widget::container::Style,
) -> Element<'a, M> {
    iced::widget::stack![content.into()].push_under(
        container(container(Space::new().width(Length::Fill).height(1))
            .style(|theme: &iced::Theme| iced::widget::container::Style {
                background: Some(tab_frame_style(theme).border.color.into()),
                ..Default::default()
            })).width(Length::Fill).height(Length::Fill).align_y(iced::Alignment::End)
    ).push_under(container(Space::new().width(Length::Fill).height(Length::Fill)).style(style)).into()
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
        .style(flat_button_style)
        .on_press_maybe(msg)
}

/// Builds the command palette's action list from current state, gating Undo/Redo/Select
/// All/Find on whether there's an active tab -- exactly like the Edit menu (`view_top_bar`)
/// already does, so the two stay in sync.
fn command_list(state: &State) -> Vec<command_palette::Command> {
    use command_palette::Command;

    let active_content = state.ws.tabs.get(state.ws.active_tab).filter(|tab| tab.image.is_none()).map(|t| &t.content);
    let can_undo = active_content.is_some_and(|c| c.can_undo());
    let can_redo = active_content.is_some_and(|c| c.can_redo());
    let has_active_tab = state.ws.tabs.get(state.ws.active_tab).is_some();

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
        Command { label: ViewAction::ToggleWordWrap.to_string(), message: Message::ViewAction(ViewAction::ToggleWordWrap) },
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
    // Like the status bar's buttons, the sidebar's panels are only offered with a folder open.
    if state.ws.root.is_none() {
        commands.retain(|command| !matches!(command.message, Message::ToggleProjectSearch | Message::SidebarToggle | Message::GitPanelToggle));
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
    let mut file_items = file_items;
    let open_recent = if state.recent_projects.is_empty() {
        Item::new(menu_button_maybe("Open Recent".into(), None))
    } else {
        let roots = state.workspace_roots();
        let recent: Vec<_> = state.recent_projects.iter().map(|path| {
            let open = roots.iter().flatten().any(|root| workspaces::same_folder(root, path));
            let label = format!("{}{}", path.display(), if open { "  (open)" } else { "" });
            Item::new(menu_button(label, Message::OpenFolderRequest(path.clone())))
        }).collect();
        Item::with_menu(menu_button("Open Recent ›".into(), Message::Noop), Menu::new(recent).width(420.0).offset(4.0).spacing(2.0))
    };
    file_items.insert(2, open_recent);

    let active_content = state.ws.tabs.get(state.ws.active_tab).filter(|tab| tab.image.is_none()).map(|t| &t.content);
    let can_undo = active_content.is_some_and(|c| c.can_undo());
    let can_redo = active_content.is_some_and(|c| c.can_redo());

    let has_active_tab = state.ws.tabs.get(state.ws.active_tab).is_some();
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
        (menu_button_maybe(
            "Find in Project (Cmd+Shift+F)".to_string(),
            state.ws.root.is_some().then_some(Message::ToggleProjectSearch),
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
        (menu_button(
            ViewAction::ToggleWordWrap.to_string(),
            Message::ViewAction(ViewAction::ToggleWordWrap)
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
    let mb = MenuBar::new(menus).style(menu_bar_style)
    .close_on_item_click_global(true)
    .close_on_background_click(true)
    .close_on_background_click_global(true);

    let update: Element<'_, Message> = if state.updater.enabled {
        let action = if state.updater.ready() { Message::RestartUpdate } else { Message::CheckUpdate };
        let mut control = button(text(&state.updater.label).size(12)).style(flat_button_style);
        if !state.updater.busy() { control = control.on_press(action); }
        iced::widget::tooltip(
            control, text(&state.updater.detail).size(12), iced::widget::tooltip::Position::Bottom,
        ).into()
    } else { Space::new().width(0).into() };
    // Keep interactive controls outside the draggable caption region on Windows.
    let custom = titlebar::custom();
    let caption = titlebar::caption(Space::new().width(Length::Fill)
        .height(if custom { Length::Fill } else { Length::Shrink }));
    let bar = container(row![
        mb,
        caption,
        update,
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
    // The sidebar's panels need a folder: without one these are greyed out.
    let (shown, project) = (state.sidebar_shown(), state.ws.root.is_some());
    let mut bar = row![
        icon_control(lucide_icons::Icon::Folder, "Toggle files", project.then_some(Message::SidebarToggle), shown && state.ws.sidebar_mode == SidebarMode::Tree),
        icon_control(lucide_icons::Icon::GitBranch, "Toggle source control", project.then_some(Message::GitPanelToggle), shown && state.ws.sidebar_mode == SidebarMode::Git),
        icon_control(lucide_icons::Icon::Search, "Search project", project.then_some(Message::ToggleProjectSearch), shown && state.ws.sidebar_mode == SidebarMode::ProjectSearch),
        text(state.ws.git_status.as_ref().map_or("", |(message, _)| message.as_str()))
            .size(12).width(Length::Fill).wrapping(iced::widget::text::Wrapping::None)
            .style(iced::widget::text::secondary),
    ].spacing(6);


    if let Some(tab) = state.ws.tabs.get(state.ws.active_tab).filter(|tab| tab.image.is_none()) {
        let (line, col) = tab.content.cursor_line_col();
        let language = state.highlighter.language_name(&tab.extension());
        bar = bar.push(text(format!("Ln {line}, Col {col}")).size(12).style(metadata_text_style));
        bar = bar.push(text(tab.line_ending.to_string()).size(12).style(metadata_text_style));
        bar = bar.push(text(language.to_string()).size(12).style(metadata_text_style));
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

    let bar = bar.push(icon_control(lucide_icons::Icon::Terminal, "Toggle terminal (Ctrl+`)", Some(Message::TerminalToggle), state.ws.terminal_visible))
        .push(icon_control(lucide_icons::Icon::Sparkles, "Toggle AI panel", Some(Message::AiToggle), state.ws.ai_visible))
        .align_y(iced::Alignment::Center).padding([0, 6]);
    container(bar).width(Length::Fill).style(chrome_style).into()
}

/// Subtle structural lines keep attention on the editor rather than its chrome.
pub(crate) fn panel_divider_style(theme: &iced::Theme) -> iced::widget::rule::Style {
    let mut style = iced::widget::rule::default(theme);
    style.color.a *= 0.45;
    style
}

pub(crate) fn metadata_text_style(theme: &iced::Theme) -> iced::widget::text::Style {
    let color = theme.extended_palette().background.base.text;
    iced::widget::text::Style { color: Some(iced::Color { a: color.a * 0.72, ..color }) }
}

pub(crate) fn panel_heading<'a>(label: impl Into<String>) -> iced::widget::Text<'a> {
    text(label.into()).size(11).style(metadata_text_style)
}

/// Hover stays neutral; only keyboard/input focus receives the accent border.
pub(crate) fn search_input_style(theme: &iced::Theme, status: iced::widget::text_input::Status) -> iced::widget::text_input::Style {
    let mut style = iced::widget::text_input::default(theme, status);
    if status == iced::widget::text_input::Status::Hovered {
        let color = theme.extended_palette().background.base.text;
        style.border.color = iced::Color { a: color.a * 0.4, ..color };
    }
    style
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
    match state.ws.sidebar_mode {
        SidebarMode::Tree => view_tree(state),
        SidebarMode::ProjectSearch => project_search::view(&state.ws.project_search, state.ws.root.as_deref()).map(Message::ProjectSearch),
        SidebarMode::Git => git::view(&state.ws.git, state.ws.git_preview.as_ref().map(|preview| preview.path.as_str())).map(Message::Git),
    }
}

fn view_tree(state: &State) -> Element<'_, Message> {
    let Some(tree) = &state.ws.tree else {
        return container(column![
            text("Your workspace").size(16),
            text("Open a folder to browse files, search your project, and review changes.")
                .size(13).style(iced::widget::text::secondary),
            button(text("Open Folder").size(13)).padding([8, 12])
                .on_press(Message::FileAction(FileAction::OpenFolder)),
        ].spacing(12)).padding(16).into();
    };

    let entry = if let Some((dir, is_dir, buf)) = &state.ws.creating {
        let icon: char = if *is_dir { lucide_icons::Icon::Folder } else { lucide_icons::Icon::File }.into();
        Some((dir.as_path(), false, row![
            text(icon).font(iced::Font::with_name("lucide")).size(14),
            text_input(if *is_dir { "Folder name" } else { "File name" }, buf)
                .id("tree-name").size(13).padding([3, 5])
                .on_input(Message::CreateInput).on_submit(Message::CreateSubmit),
            icon_control(lucide_icons::Icon::X, "Cancel new item", Some(Message::CreateCancel), false),
        ].spacing(4).align_y(iced::Alignment::Center).into()))
    } else if let Some((path, buf)) = &state.ws.renaming {
        Some((path.as_path(), true, row![
            text_input("Name", buf).id("tree-name").size(13).padding([3, 5])
                .on_input(Message::RenameInput).on_submit(Message::RenameSubmit),
            icon_control(lucide_icons::Icon::X, "Cancel rename", Some(Message::RenameCancel), false),
        ].spacing(4).into()))
    } else { None };
    let refresh = icon_control(lucide_icons::Icon::RefreshCw, "Refresh files", Some(Message::RefreshTree), false);
    let content = ContextMenu::new(tree.view_with_entry_scrollbar_and_root_action(
        Message::Tree, entry, scrollbar::rail(), scrollbar::style, Some(refresh),
    ), || {
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

    container(content).padding([0, 8]).height(Length::Fill).into()
}

fn view_editor(state: &State) -> Element<'_, Message> {
    use iced_swdir_tree::IconTheme;
    let mut tab_row = row![].spacing(1).padding(iced::Padding { top: 2.0, right: 1.0, bottom: 0.0, left: 1.0 });
    for (i, tab) in state.ws.tabs.iter().enumerate() {
        let is_active = state.ws.git_preview.is_none() && state.ws.git_graph.is_none() && i == state.ws.active_tab;
        let path = tab.path.as_deref().unwrap_or_else(|| Path::new("Untitled"));
        let spec = file_icons::FileIcons.file_glyph(path);
        let color = file_icons::FileIcons.file_color(path);
        let icon = text(spec.glyph.into_owned())
            .font(spec.font.unwrap_or_default())
            .size(spec.size.unwrap_or(14.0)).line_height(tab_line_height())
            .style(move |theme: &iced::Theme| iced::widget::text::Style {
                color: Some(color.unwrap_or(theme.extended_palette().background.base.text)),
            });
        let tab_title = row![
            container(row![icon, text(tab.title()).size(13).line_height(tab_line_height())].spacing(6).align_y(iced::Alignment::Center))
                .padding([0, 8]),
            tab_close_control("Close tab", Message::TabClosed(i)),
        ]
        .spacing(2)
        .align_y(iced::Alignment::Center);

        let tab_content = joined_tab(button(tab_title).padding(1).height(TAB_HEIGHT)
            .on_press(Message::TabSelected(i))
            .style(move |theme, status| tab_button_style(theme, status, is_active)), is_active);
        let tab_content = iced::widget::mouse_area(tab_content)
            .on_middle_press(Message::TabClosed(i));
        tab_row = tab_row.push(ContextMenu::new(tab_content, move || {
            let count = state.ws.tabs.len();
            container(column![
                menu_button("Close Tab".into(), Message::TabClosed(i)),
                menu_button_maybe("Close Other Tabs".into(), (count > 1).then_some(Message::CloseTabs(i, TabCloseScope::Others))),
                menu_button_maybe("Close Tabs to the Left".into(), (i > 0).then_some(Message::CloseTabs(i, TabCloseScope::Left))),
                menu_button_maybe("Close Tabs to the Right".into(), (i + 1 < count).then_some(Message::CloseTabs(i, TabCloseScope::Right))),
                iced::widget::rule::horizontal(1),
                menu_button("Close All Tabs".into(), Message::CloseTabs(i, TabCloseScope::All)),
                menu_button_maybe("Reopen Closed Tab".into(), (!state.ws.closed_tabs.is_empty()).then_some(Message::ReopenClosedTab)),
            ].spacing(2).width(220))
            .padding(6).style(tab_context_menu_style).into()
        }));
    }

    if let Some(preview) = &state.ws.git_preview {
        let path = Path::new(&preview.path);
        let spec = file_icons::FileIcons.file_glyph(path);
        let color = file_icons::FileIcons.file_color(path);
        let icon = text(spec.glyph.into_owned())
            .font(spec.font.unwrap_or_default()).size(spec.size.unwrap_or(14.0)).line_height(tab_line_height())
            .style(move |theme: &iced::Theme| iced::widget::text::Style {
                color: Some(color.unwrap_or(theme.extended_palette().background.base.text)),
            });
        let title = row![
            container(row![icon, text(&preview.path).size(13).line_height(tab_line_height())].spacing(6).align_y(iced::Alignment::Center))
                .padding([0, 8]),
            tab_close_control("Close diff", Message::CloseGitPreview),
        ].spacing(2).align_y(iced::Alignment::Center);
        let tab = joined_tab(container(title).padding(1).height(TAB_HEIGHT)
            .style(|theme| tab_surface_style(theme, true)), true);
        let mut header = row![
            tab,
            text(git_preview::summary(preview)).size(12).style(iced::widget::text::secondary),
            Space::new().width(Length::Fill),
        ].spacing(8).padding(iced::Padding { top: 2.0, left: 1.0, right: 1.0, bottom: 0.0 }).align_y(iced::Alignment::Center);
        if state.ws.inline_edit.as_ref().is_some_and(|edit| matches!(edit.stage, inline_edit::Stage::Review(_))) {
            header = header
                .push(button(text("Accept").size(13)).padding([6, 14]).style(iced::widget::button::success).on_press(Message::InlineEditAccept))
                .push(secondary_button("Reject").on_press(Message::InlineEditCancel));
        }
        let header = header.push(secondary_button(if state.ws.ai_visible { "Hide AI panel" } else { "Show AI panel" }).on_press(Message::AiToggle));
        return editor_frame(tab_row, column![
            connected_tab_strip(header),
            tab_content_frame(git_preview::view(preview, &state.app_theme.iced, Message::GitPreviewScrolled)),
        ]);
    }
    if let Some(graph) = &state.ws.git_graph {
        return editor_frame(tab_row, column![git_graph::view(graph).map(Message::GitGraph)]);
    }
    let mut editor_column = column![];

    if let Some(tab) = state.ws.tabs.get(state.ws.active_tab) {
        if let Some(path) = &tab.path {
            let mut crumbs = row![].spacing(2).align_y(iced::Alignment::Center);
            let base = state.ws.root.as_deref().filter(|root| path.starts_with(root));
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
            editor_column = editor_column.push(crate::scrollbar::scrollable(crumbs.padding([0, 8]))
                .direction(iced::widget::scrollable::Direction::Horizontal(crate::scrollbar::rail())));
        }
        if let Some(preview) = &tab.image {
            return editor_frame(tab_row, editor_column.push(image_preview::view(preview)));
        }
        if let Some(document) = tab.preview.as_ref().and_then(|preview| ai_markdown::view_document(preview, &state.app_theme.iced, Message::MarkdownPreview)) {
            editor_column = editor_column.push(crate::scrollbar::scrollable(container(container(document).max_width(860))
                .center_x(Length::Fill).padding([16, 24])).height(Length::Fill));
            return editor_frame(tab_row, editor_column);
        }
        if tab.search.visible {
            editor_column = editor_column.push(code_editor::search::view(&tab.search).map(Message::Search));
        }
        if let Some(edit) = state.ws.inline_edit.as_ref().filter(|edit| edit.path == tab.path) {
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
            Message::DiffMarkerClicked,
            Message::EditorProbe,
            Message::EditorViewport,
        );
        // Hover text as a real widget over the canvas; see `code_editor::Hover` for why.
        // The stack is always present: wrapping the canvas only while a popup shows would
        // change the widget tree's shape and reset the canvas state, scroll included.
        let mut layers = iced::widget::stack![editor];
        if let Some(hover) = &tab.hover {
                let mut lines = column![].spacing(2);
                for line in &hover.lines {
                    lines = lines.push(text(line.clone()).size(12).font(fonts::FONT)
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
        // A clicked diff marker's change below its lines, drawn over the canvas like the hover popup.
        if let Some(hunk) = &tab.diff_popup {
            let anchor = if hunk.lines.is_empty() && hunk.lines.start < tab.content.line_count() {
                code_editor::line_anchor(&tab.content, &state.app_theme.editor, state.zoom, hunk.lines.start, false)
            } else {
                code_editor::line_anchor(&tab.content, &state.app_theme.editor, state.zoom, hunk.lines.end.max(1) - 1, true)
            };
            if let Some(anchor) = anchor {
                let offset = iced::Padding { top: anchor.y.max(0.0) + 2.0, left: anchor.x.max(0.0), right: 0.0, bottom: 0.0 };
                layers = layers.push(container(view_diff_popup(hunk)).padding(offset));
            }
        }
        // Completions below the cursor, drawn over the canvas like the hover popup.
        if let Some(session) = state.ws.completion.as_ref()
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
        let roots = state.workspace_roots();
        let mut recent = column![].spacing(2);
        for path in &state.recent_projects {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
            let open = roots.iter().flatten().any(|root| workspaces::same_folder(root, path));
            let location = path.parent().map(|parent| parent.display().to_string()).unwrap_or_default();
            recent = recent.push(button(row![
                text(name).size(14),
                text(location).size(12).style(iced::widget::text::secondary).wrapping(iced::widget::text::Wrapping::None),
                Space::new().width(Length::Fill),
                text(if open { "open" } else { "" }).size(11).style(iced::widget::text::secondary),
            ].spacing(8).align_y(iced::Alignment::Center))
                .width(Length::Fill).padding([6, 10]).style(flat_button_style)
                .on_press(Message::OpenFolderRequest(path.clone())));
        }
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
        let welcome = if state.recent_projects.is_empty() { welcome } else {
            welcome.push(Space::new().height(12)).push(text("Recent projects").size(13).style(iced::widget::text::secondary)).push(recent)
        };
        editor_column = editor_column.push(container(crate::scrollbar::scrollable(container(welcome).padding(32)))
            .center_x(Length::Fill).center_y(Length::Fill));
        // `view` keeps the outer workspace frame (`tab_content_frame(panes)`).
        // With no editor tabs, don't add a second frame around the welcome screen.
        return editor_column.height(Length::Fill).into();
    }

    editor_frame(tab_row, editor_column)
}

fn editor_frame<'a>(tab_row: iced::widget::Row<'a, Message>, editor_column: iced::widget::Column<'a, Message>) -> Element<'a, Message> {
    let tab_strip = crate::scrollbar::scrollable(tab_row)
        .direction(iced::widget::scrollable::Direction::Horizontal(
            tab_rail(),
        ))
        .width(Length::Fill);

    column![connected_tab_strip_with_style(tab_strip, tab_strip_style), tab_content_frame(editor_column.height(Length::Fill))]
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
    // Ctrl+Tab switches workspace tabs from anywhere, even a focused text box.
    let workspace_switch = !cfg!(target_os = "macos") && workspace_cycle_key(&modified_key, modifiers).is_some();
    if status == iced::event::Status::Ignored || is_escape || terminal_shortcut || workspace_switch {
        Some(Message::KeyPressed(modified_key, modifiers))
    } else {
        None
    }
}

/// The key switching workspace tabs, if `key` is one: `Some(true)` for the next tab,
/// `Some(false)` for the previous. Ctrl+Tab and Ctrl+Shift+Tab on Windows and Linux;
/// Cmd+Shift+] and Cmd+Shift+[ on macOS, where Control+Tab stays the editor's.
pub(crate) fn workspace_cycle_key(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<bool> {
    workspace_cycle_key_on(cfg!(target_os = "macos"), key, modifiers)
}

fn workspace_cycle_key_on(macos: bool, key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<bool> {
    use keyboard::Key;
    if macos {
        if !modifiers.logo() || !modifiers.shift() || modifiers.control() || modifiers.alt() { return None; }
        match key.as_ref() {
            Key::Character("]" | "}") => Some(true),
            Key::Character("[" | "{") => Some(false),
            _ => None,
        }
    } else {
        if !modifiers.control() || modifiers.alt() || modifiers.logo() { return None; }
        (key.as_ref() == Key::Named(keyboard::key::Named::Tab)).then_some(!modifiers.shift())
    }
}

/// Opens `path` as the project of the tab in `ws`: remembers it among the recent projects,
/// shows its tree and points the shells at it.
fn open_folder(state: &mut State, path: PathBuf) -> Task<Message> {
    let mut task = Task::none();
    recent_projects::record(&mut state.recent_projects, path.clone(), state.recent_projects_path.as_deref());
    state.ws.tree = Some(file_tree(path.clone()));
    state.show_editor();
    if let Some(tree) = &mut state.ws.tree {
        task = tree.update(DirectoryTreeEvent::Toggled(path.clone())).map(Message::Tree);
    }
    let changed = state.ws.root.as_deref() != Some(path.as_path());
    state.ws.root = Some(path);
    // The sidebar, hidden while the tab had no folder, comes back if it is switched on.
    state.sync_sidebar();
    // Shells left in the previous project would run commands against it.
    if changed {
        let focused = state.ws.terminal.focused();
        state.ws.terminal.shutdown();
        if state.ws.terminal_visible {
            state.ws.terminal.ensure_started(&state.root_or_cwd());
            state.ws.terminal.set_focused(focused);
        }
    }
    task
}

/// Files dropped on the window open in the editor, unless a widget took the drop first: the
/// AI composer captures drops on itself to attach them (`ai_composer::DropTarget`).
fn handle_raw_drop_event(
    event: iced::Event,
    status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(iced::window::Event::FileDropped(path)) if status == iced::event::Status::Ignored => {
            Some(Message::PathDropped(path))
        }
        _ => None,
    }
}

fn subscription(state: &State) -> Subscription<Message> {
    let keys = iced::event::listen_with(handle_raw_key_event);
    let drops = iced::event::listen_with(handle_raw_drop_event);
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
    Subscription::batch([keys, drops, tick, window_events, first_frame, drag_cleanup])
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

/// The project folder saved by versions before workspace tabs; only read to migrate their
/// session (see `workspaces::read_session`).
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

fn save_word_wrap(on: bool) {
    let Some(path) = config_path("word_wrap") else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, on.to_string());
}

/// Word wrap is off unless it was saved as on.
fn load_word_wrap() -> bool {
    config_path("word_wrap").and_then(|path| std::fs::read_to_string(path).ok()).is_some_and(|text| text.trim() == "true")
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
/// of `state` (e.g. iterating `&mut self.ws.tabs`) can still call it without a borrow conflict.
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
    Task::batch([blame, Task::perform(git_diff::diff_for_buffer(repo, relative, text), move |diff: (Vec<(usize, git_diff::LineStatus)>, Vec<git_diff::Hunk>)| {
        Message::GitDiffLoaded(path.clone(), snapshot.clone(), diff)
    })])
}

fn load_image_task(path: PathBuf, request: u64) -> Task<Message> {
    Task::perform(async move {
        tokio::task::spawn_blocking(move || image_preview::load(&path)).await.map_err(|error| error.to_string())?
    }, move |result| Message::ImageLoaded(request, result))
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
    // Started by ssh to ask for a key's passphrase: answers and exits.
    if git::askpass::run_helper() { return Ok(()); }
    if updater::run_helper() { return Ok(()); }
    let _update_guard = updater::running_guard();
    let icon = iced::window::icon::from_file_data(include_bytes!("../icon.png"), None).ok();
    iced::application(State::boot, update_and_sync_preview, view)
        .title("editor")
        .theme(|state: &State| state.app_theme.iced.clone())
        .subscription(subscription)
        .font(iced_swdir_tree::LUCIDE_FONT_BYTES)
        .font(fonts::FILES[0])
        .font(fonts::FILES[1])
        .font(fonts::FILES[2])
        .font(fonts::FILES[3])
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
mod workspace_tests {
    use super::*;
    use iced::futures::StreamExt;

    #[test]
    fn interaction_feedback_keeps_hover_neutral_and_focus_and_selection_clear() {
        use iced::widget::{button::Status, text_input};
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let palette = theme.extended_palette();
            for status in [Status::Hovered, Status::Pressed] {
                let button = flat_button_style(&theme, status);
                assert_eq!(button.background, Some(palette.background.strong.color.into()));
                assert_eq!(button.text_color, palette.background.base.text);
                let result = quick_open::result_style(&theme, status, false);
                assert_eq!(result.background, button.background);
            }
            for status in [Status::Active, Status::Hovered, Status::Pressed] {
                let selected = quick_open::result_style(&theme, status, true);
                assert_eq!(selected.background, Some(palette.primary.weak.color.into()));
                assert_eq!(selected.text_color, palette.primary.weak.text);
            }
            let disabled = quick_open::result_style(&theme, Status::Disabled, true);
            assert_eq!(disabled.background, Some(iced::Color::TRANSPARENT.into()));
            assert!(disabled.text_color.a < palette.background.base.text.a);
            let hovered = search_input_style(&theme, text_input::Status::Hovered);
            assert!(hovered.border.color.a < palette.background.base.text.a);
            for status in [text_input::Status::Active, text_input::Status::Disabled,
                text_input::Status::Focused { is_hovered: false },
                text_input::Status::Focused { is_hovered: true }] {
                assert_eq!(search_input_style(&theme, status), text_input::default(&theme, status));
            }
        }
    }

    #[test]
    fn panel_hierarchy_softens_chrome_without_changing_theme_colors_or_rule_geometry() {
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let original = iced::widget::rule::default(&theme);
            let divider = panel_divider_style(&theme);
            assert!(divider.color.a > 0.0 && divider.color.a < original.color.a);
            assert_eq!((divider.color.r, divider.color.g, divider.color.b),
                (original.color.r, original.color.g, original.color.b));
            assert_eq!(divider.radius, original.radius);
            assert_eq!(divider.snap, original.snap);
            assert!(matches!(divider.fill_mode, iced::widget::rule::FillMode::Full));
            let foreground = theme.extended_palette().background.base.text;
            let metadata = metadata_text_style(&theme).color.unwrap();
            assert!(metadata.a >= foreground.a * 0.7 && metadata.a < foreground.a);
            assert_eq!((metadata.r, metadata.g, metadata.b),
                (foreground.r, foreground.g, foreground.b));
        }
    }

    #[test]
    fn toolbar_utilities_are_quiet_without_dimming_primary_selected_or_disabled_states() {
        use iced::widget::button::Status;
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let palette = theme.extended_palette();
            let utility = toolbar_button_style(&theme, Status::Active, false, false);
            let primary = toolbar_button_style(&theme, Status::Active, false, true);
            assert!(utility.text_color.a < primary.text_color.a);
            assert_eq!(primary.text_color, palette.background.base.text);
            assert_eq!(utility.background, Some(iced::Color::TRANSPARENT.into()));
            for primary in [false, true] {
                for status in [Status::Active, Status::Hovered, Status::Pressed] {
                    let selected = toolbar_button_style(&theme, status, true, primary);
                    assert_eq!(selected.background, Some(palette.primary.weak.color.into()));
                    assert_eq!(selected.text_color, palette.primary.weak.text);
                }
                for selected in [false, true] {
                    let disabled = toolbar_button_style(&theme, Status::Disabled, selected, primary);
                    let expected = flat_button_style(&theme, Status::Disabled);
                    assert_eq!(disabled.text_color, expected.text_color);
                    assert_eq!(disabled.background, expected.background);
                }
            }
            for status in [Status::Hovered, Status::Pressed] {
                let utility = toolbar_button_style(&theme, status, false, false);
                let expected = flat_button_style(&theme, status);
                assert_eq!(utility.text_color, expected.text_color);
                assert_eq!(utility.background, expected.background);
            }
        }
    }

    #[test]
    fn tab_scrollbars_have_a_lane_clear_of_titles_and_the_join() {
        let top_padding = 2.0;
        let border = 1.0;
        let title_height = 26.0;
        let rail_width = TAB_RAIL_WIDTH;
        let rail_margin = TAB_RAIL_MARGIN;
        let title_bottom = top_padding + border + title_height;
        let thumb_top = PANEL_HEADER_HEIGHT - rail_margin - rail_width;
        let thumb_bottom = PANEL_HEADER_HEIGHT - rail_margin;
        assert!(title_bottom <= thumb_top, "the scrollbar must not overlap the title");
        assert!(thumb_bottom <= top_padding + TAB_HEIGHT - border, "the scrollbar must leave the bottom join clear");
        assert!(title_height + 2.0 * border <= TAB_HEIGHT);
        assert_eq!(TAB_HEIGHT, 30.0, "tabs must stay compact");
        assert_eq!(PANEL_HEADER_HEIGHT, 32.0);
        assert_eq!(top_padding + TAB_HEIGHT, PANEL_HEADER_HEIGHT);
        let header: Element<'_, Message> = panel_header(text("Files"));
        assert_eq!(header.as_widget().size().height, Length::Fixed(PANEL_HEADER_HEIGHT));
    }

    #[test]
    fn tab_context_menus_match_the_top_menu_popup_surface() {
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let popup = menu_bar_style(&theme, iced_aw::style::Status::Active);
            let context = tab_context_menu_style(&theme);
            assert_eq!(context.background, Some(popup.menu_background));
            assert_eq!(context.border, popup.menu_border);
            assert_eq!(context.shadow, popup.menu_shadow);
            assert_eq!(context.text_color, Some(theme.extended_palette().background.base.text));
        }
    }

    #[test]
    fn menu_bar_blends_with_chrome_without_changing_popup_surfaces() {
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            let status = iced_aw::style::Status::Active;
            let style = menu_bar_style(&theme, status);
            let original = iced_aw::style::menu_bar::primary(&theme, status);
            assert_eq!(style.bar_background, iced::Color::TRANSPARENT.into());
            assert_eq!(style.bar_border, iced::Border::default());
            assert_eq!(style.bar_shadow, iced::Shadow::default());
            assert_eq!(style.menu_background, original.menu_background);
            assert_eq!(style.menu_border, original.menu_border);
        }
    }

    #[test]
    fn inactive_tabs_remain_readable_and_workspace_tooltips_are_opaque() {
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            let inactive = tab_button_style(&theme, iced::widget::button::Status::Active, false);
            assert_eq!(inactive.text_color.a, theme.extended_palette().background.base.text.a * 0.6);
            let tip = overlay_style(&theme);
            assert_eq!(tip.background, Some(theme.extended_palette().background.base.color.into()));
            assert!(tip.border.width > 0.0);
        }
    }

    #[test]
    fn sidebar_and_ai_backgrounds_match_workspace() {
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let workspace = Some(theme.extended_palette().background.base.color.into());
            let ai = workspace_panel_style(&theme);
            let explorer = sidebar_panel_style(&theme, SidebarMode::Tree);
            assert_eq!(ai.background, workspace);
            assert_eq!(explorer.background, workspace);
            assert_eq!(ai.border, tab_frame_style(&theme).border.rounded(6));
            assert_eq!(explorer.border, ai.border);
            for mode in [SidebarMode::Git, SidebarMode::ProjectSearch] {
                assert_eq!(sidebar_panel_style(&theme, mode), ai);
            }
        }
    }

    #[test]
    fn git_and_search_panels_match_explorer_background_and_border() {
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let explorer = sidebar_panel_style(&theme, SidebarMode::Tree);
            for mode in [SidebarMode::Git, SidebarMode::ProjectSearch] {
                let panel = sidebar_panel_style(&theme, mode);
                assert_eq!(panel.background, explorer.background);
                assert_eq!(panel.border, explorer.border);
                assert_eq!(panel.border.width, 1.0);
                assert_eq!(panel.border.radius, iced::border::Radius::from(6.0));
            }
        }
    }

    #[test]
    fn explorer_and_ai_panel_borders_match_content_frame_color_and_width() {
        for theme in [iced::Theme::Light, iced::Theme::Dark] {
            let frame = tab_frame_style(&theme).border;
            for border in [workspace_panel_style(&theme).border, sidebar_panel_style(&theme, SidebarMode::Tree).border] {
                assert_eq!(border.color, frame.color);
                assert_eq!(border.width, frame.width);
                assert_eq!(border.width, 1.0);
                assert_eq!(border.radius, iced::border::Radius::from(6.0));
            }
        }
    }

    #[test]
    fn tab_strip_background_matches_the_workspace_in_each_theme() {
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            let strip = tab_strip_style(&theme);
            assert_eq!(strip.background, Some(theme.extended_palette().background.base.color.into()));
            assert_eq!(strip.background, tab_frame_style(&theme).background);
            assert_eq!(strip.border, iced::Border::default());
            let other_strips = chrome_style(&theme);
            assert_eq!(other_strips.background, Some(theme.extended_palette().background.weak.color.into()));
            assert_ne!(strip.background, other_strips.background);
        }
    }

    #[test]
    fn selected_tabs_open_into_a_matching_content_frame() {
        use iced::widget::button::Status;
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            let frame = tab_frame_style(&theme);
            for status in [Status::Active, Status::Hovered, Status::Pressed] {
                let tab = tab_button_style(&theme, status, true);
                assert_eq!(tab.background, frame.background);
                assert_eq!(tab.border.color, frame.border.color);
                assert_eq!(tab.border.width, frame.border.width);
            }
            assert_eq!(frame.border.radius.top_left, 0.0);
            assert_eq!(frame.border.radius.top_right, 0.0);
            assert_eq!(frame.border.radius.bottom_left, 6.0);
            assert_eq!(frame.border.radius.bottom_right, 6.0);
        }
        // Only the selected tab gets a layer covering its bottom border.
        for selected in [false, true] {
            let tab: Element<'_, Message> = joined_tab(button(text("tab")), selected);
            let tree = iced::advanced::widget::Tree::new(tab.as_widget());
            assert_eq!(tree.children.len(), if selected { 2 } else { 1 });
        }
    }

    #[test]
    fn workspace_and_diff_tab_surfaces_match_editor_and_terminal_tabs() {
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            for selected in [false, true] {
                let surface = tab_surface_style(&theme, selected);
                let button = tab_button_style(&theme, iced::widget::button::Status::Active, selected);
                assert_eq!(surface.border, button.border);
                assert_eq!(surface.background, button.background);
                assert_eq!(surface.text_color, Some(button.text_color));
            }
        }
    }

    #[test]
    fn tab_glyphs_and_labels_use_the_same_absolute_line_height() {
        // Relative line height gives the 11 px close glyph, 13 px label, and 14 px
        // file icon different line boxes even when their rows are centered.
        for font_size in [11.0, 13.0, 14.0] {
            assert_eq!(tab_line_height().to_absolute(iced::Pixels(font_size)), iced::Pixels(18.0));
        }
    }

    #[test]
    fn tabs_have_rounded_tops_and_theme_accented_selection_borders() {
        use iced::widget::button::Status;
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            let active = tab_button_style(&theme, Status::Active, true);
            let inactive = tab_button_style(&theme, Status::Active, false);
            assert_eq!(active.border.width, 1.0);
            assert_eq!(active.border.radius.top_left, 7.0);
            assert_eq!(active.border.radius.top_right, 7.0);
            assert_eq!(active.border.radius.bottom_left, 0.0);
            assert_eq!(active.border.radius.bottom_right, 0.0);
            assert_eq!(active.border.color, iced::Color { a: 0.45, ..theme.extended_palette().primary.base.color });
            assert!(active.border.color.a > inactive.border.color.a);
            assert!(tab_button_style(&theme, Status::Hovered, false).border.color.a > inactive.border.color.a);
        }
    }

    #[test]
    fn tab_surfaces_and_close_controls_keep_a_quiet_visual_hierarchy() {
        use iced::widget::button::Status;
        for theme in [iced::Theme::Dark, iced::Theme::Light] {
            let palette = theme.extended_palette();
            let active = tab_button_style(&theme, Status::Active, true);
            let inactive = tab_button_style(&theme, Status::Active, false);
            assert_eq!(active.background, Some(palette.background.base.color.into()));
            assert_eq!(inactive.background, Some(iced::Color::TRANSPARENT.into()));
            assert!(active.text_color.a > inactive.text_color.a);
            for selected in [false, true] {
                let hovered = tab_button_style(&theme, Status::Hovered, selected);
                assert_eq!(hovered.background, Some(if selected { palette.background.base.color } else { palette.background.strong.color }.into()));
                assert_eq!(hovered.border.radius, iced::border::Radius { top_left: 7.0, top_right: 7.0, ..Default::default() });
            }
            let resting = tab_close_style(&theme, Status::Active);
            let hovered = tab_close_style(&theme, Status::Hovered);
            assert!(resting.text_color.a < hovered.text_color.a);
            assert_eq!(hovered.text_color, palette.background.base.text);
            assert_eq!(resting.background, Some(iced::Color::TRANSPARENT.into()));
            assert_eq!(tab_close_style(&theme, Status::Disabled).text_color,
                flat_button_style(&theme, Status::Disabled).text_color);
        }
    }

    /// Settings that save nothing: the session and recent projects stay in memory.
    fn settings() -> Settings {
        Settings {
            themes: theme::ThemeRegistry::load(),
            app_theme: theme::EditorTheme::default_dark(),
            zoom: code_editor::ZOOM_DEFAULT,
            word_wrap: false,
            recent_files: Vec::new(),
            session_store: SessionStore::Memory,
            recent_projects_path: None,
        }
    }

    fn layout() -> Layout {
        Layout {
            sidebar_visible: true, sidebar_mode: SidebarMode::Tree, ai_visible: false, ai_mode: AiMode::Http,
            terminal_visible: false, sidebar_ratio: 0.2, ai_ratio: 0.65,
        }
    }

    /// A tab without a folder holding one unsaved file per draft, named after its text. The
    /// files don't exist and none is opened from disk, so nothing touches the user's files or
    /// recent files; `.txt` has no language server.
    fn tab(drafts: &[&str]) -> EditorSession {
        let path = |text: &str| std::env::temp_dir().join(format!("editor-workspace-test-{}-{text}.txt", std::process::id()));
        EditorSession {
            recovery: drafts.iter().map(|text| RecoveryBuffer { path: Some(path(text)), text: text.to_string() }).collect(),
            layout: Some(layout()),
            ..Default::default()
        }
    }

    fn state(tabs: Vec<EditorSession>, active: usize) -> State {
        State::start(settings(), workspaces::SessionFile { workspaces: tabs, active_workspace: active }).0
    }

    fn ids(state: &State) -> Vec<WorkspaceId> {
        state.workspaces.iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn explorer_has_one_root_row_with_refresh_and_no_duplicate_heading() {
        let root = tempfile::tempdir().unwrap();
        let mut state = state(vec![tab(&[])], 0);
        state.ws.tree = Some(DirectoryTree::new(root.path().to_path_buf()));
        let explorer = view_tree(&state);
        let tree = iced::advanced::widget::Tree::new(explorer.as_widget());
        // Containers delegate their widget tree: context menu -> scrollable -> rows.
        assert_eq!(tree.children.len(), 2, "context menu underlay and overlay only");
        let rows = &tree.children[0].children[0];
        assert_eq!(rows.children.len(), 1);
        assert_eq!(rows.children[0].children.len(), 2, "root folder and refresh control share one row");

        // The existing tree API continues to render its original root row.
        let plain = state.ws.tree.as_ref().unwrap().view_with_entry_and_scrollbar(
            Message::Tree, None, scrollbar::rail(), scrollbar::style,
        );
        let plain = iced::advanced::widget::Tree::new(plain.as_widget());
        assert_eq!(rows.children[0].children[0].tag, plain.children[0].children[0].tag);
    }

    #[test]
    fn welcome_screen_does_not_add_an_inner_frame_to_the_workspace_frame() {
        let state = state(vec![tab(&[])], 0);
        assert!(state.ws.tabs.is_empty());
        let welcome = view_editor(&state);
        let tree = iced::advanced::widget::Tree::new(welcome.as_widget());
        assert_eq!(tree.children.len(), 1, "only welcome content, without an inner tab frame");
    }

    #[test]
    fn the_passphrase_dialog_answers_one_prompt_at_a_time_and_forgets_what_was_typed() {
        let mut state = state(vec![tab(&["passphrase"])], 0);
        let request = |id: u64| git::askpass::Request { id, key: format!("/keys/{id}") };
        state.passphrase_prompts.extend([request(1), request(2)]);
        let _ = update(&mut state, Message::PassphraseInput("secret".into()));
        let _ = update(&mut state, Message::RememberPassphrase(true));
        assert_eq!(state.passphrase, "secret");
        let _ = update(&mut state, Message::PassphraseDone(true));
        assert_eq!(state.passphrase_prompts.front(), Some(&request(2)), "the next prompt is shown");
        assert_eq!(state.passphrase, "", "the passphrase is not kept in the dialog");
        assert!(!state.remember_passphrase, "remember is off by default for each prompt");
        // The dialog is modal: other keys do nothing, Escape cancels.
        let _ = update(&mut state, Message::KeyPressed(keyboard::Key::Character("p".into()), keyboard::Modifiers::COMMAND));
        assert!(!state.quick_open.visible);
        let _ = update(&mut state, Message::PassphraseInput("typed".into()));
        let _ = update(&mut state, Message::KeyPressed(keyboard::Key::Named(keyboard::key::Named::Escape), keyboard::Modifiers::default()));
        assert!(state.passphrase_prompts.is_empty());
        assert_eq!(state.passphrase, "");
    }

    #[test]
    fn a_clicked_diff_marker_shows_its_change_and_restore_puts_the_staged_lines_back() {
        let mut state = state(vec![tab(&["diff-popup"])], 0);
        let hunk = git_diff::Hunk { lines: 1..2, old: vec!["b".into()], new: vec!["B".into()] };
        {
            let tab = &mut state.ws.tabs[0];
            tab.content = code_editor::Buffer::new("a\nB\nc", code_editor::metrics_for_zoom(1.0));
            tab.saved_text = Some("a\nB\nc".into());
            tab.dirty = false;
            tab.diff = [(1, git_diff::LineStatus::Modified)].into();
            tab.hunks = vec![hunk.clone()];
        }
        let popup = |state: &State| state.ws.tabs[0].diff_popup.clone();
        let _ = update(&mut state, Message::DiffMarkerClicked(1));
        assert_eq!(popup(&state), Some(hunk.clone()));
        let _ = update(&mut state, Message::DiffMarkerClicked(1));
        assert_eq!(popup(&state), None, "a second click closes it");
        let _ = update(&mut state, Message::DiffMarkerClicked(1));
        let _ = update(&mut state, Message::KeyPressed(keyboard::Key::Named(keyboard::key::Named::Escape), keyboard::Modifiers::default()));
        assert_eq!(popup(&state), None, "Escape closes it");
        let _ = update(&mut state, Message::DiffMarkerClicked(1));
        let _ = update(&mut state, Message::EditorAction(cosmic_text::Action::Click { x: 0, y: 0 }));
        assert_eq!(popup(&state), None, "a click in the text closes it");

        let _ = update(&mut state, Message::DiffMarkerClicked(1));
        let _ = update(&mut state, Message::DiffRestore);
        assert_eq!(popup(&state), None);
        assert_eq!(state.ws.tabs[0].content.text(), "a\nb\nc");
        assert!(state.ws.tabs[0].dirty, "restored in the buffer, not saved");
        state.ws.tabs[0].content.undo();
        assert_eq!(state.ws.tabs[0].content.text(), "a\nB\nc");
    }

    fn loaded(state: &State, index: usize) -> bool {
        !matches!(state.workspaces[index].slot, Slot::Unloaded(_))
    }

    /// The texts of tab `index`'s files.
    fn texts(state: &State, index: usize) -> Vec<String> {
        state.workspace(index).map(|workspace| workspace.tabs.iter().map(|tab| tab.content.text()).collect()).unwrap_or_default()
    }

    async fn outputs(task: Task<Message>) -> Vec<Message> {
        let mut messages = Vec::new();
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(message) = action { messages.push(message); }
            }
        }
        messages
    }

    #[tokio::test]
    async fn what_a_tab_starts_reports_back_to_that_tab() {
        let messages = outputs(tag(Task::batch([Task::done(Message::RefreshTree), Task::done(Message::Tick)]), 7)).await;
        assert!(matches!(&messages[0], Message::In(7, message) if matches!(**message, Message::RefreshTree)));
        assert!(matches!(messages[1], Message::Tick), "app-wide messages are not addressed to a tab");
        // Already addressed messages keep their tab.
        let messages = outputs(tag(Task::done(Message::In(3, Box::new(Message::RefreshTree))), 7)).await;
        assert!(matches!(&messages[0], Message::In(3, _)));
    }

    #[test]
    fn only_the_shown_tab_loads_at_startup_and_others_on_first_show() {
        let mut state = state(vec![tab(&["a"]), tab(&["b1", "b2"]), tab(&[])], 1);
        assert_eq!(state.active, 1);
        assert!(!loaded(&state, 0) && loaded(&state, 1) && !loaded(&state, 2));
        assert_eq!(texts(&state, 1), ["b1", "b2"]);
        // Unsaved edits of a tab not loaded yet still count.
        assert_eq!(state.workspace_unsaved(0), 1);
        assert_eq!(state.workspace_unsaved(2), 0);

        let _ = update(&mut state, Message::WorkspaceCycle(true));
        assert_eq!((state.active, state.current), (2, 2));
        assert!(loaded(&state, 2) && matches!(state.workspaces[1].slot, Slot::Parked(_)));
        let _ = update(&mut state, Message::WorkspaceCycle(true));
        assert_eq!(state.active, 0, "wraps around");
        assert_eq!(texts(&state, 0), ["a"], "its unsaved edits came back on first show");
        let _ = update(&mut state, Message::WorkspaceCycle(false));
        assert_eq!(state.active, 2);
        assert_eq!(texts(&state, 1), ["b1", "b2"], "a tab in the background keeps its files");
    }

    #[test]
    fn replies_reach_their_own_tab_even_in_the_background() {
        let mut state = state(vec![tab(&["a1", "a2"]), tab(&["b1", "b2"])], 1);
        let _ = update(&mut state, Message::WorkspaceCycle(true));
        let [a, b] = ids(&state)[..] else { panic!() };
        assert_eq!(state.active, 0);
        // A reply for tab B (here: a confirmed close of its first file) while A is shown.
        let _ = update(&mut state, Message::In(b, Box::new(Message::CloseTabConfirmed(0))));
        assert_eq!(texts(&state, 1), ["b2"]);
        assert_eq!(texts(&state, 0), ["a1", "a2"], "the shown tab is untouched");
        assert_eq!((state.active, state.current, state.ws.id), (0, 0, a), "the shown tab is back in place");
        // A notice from the background says which tab it is about.
        let missing = std::env::temp_dir().join(format!("editor-missing-{}", std::process::id()));
        let _ = update(&mut state, Message::In(b, Box::new(Message::DeleteConfirmed(missing))));
        assert!(state.notice.as_ref().is_some_and(|(text, _)| text.starts_with("[New Tab] Delete failed")), "{:?}", state.notice);
        // Replies for a closed tab are dropped.
        let _ = update(&mut state, Message::WorkspacesCloseConfirmed(vec![b]));
        let _ = update(&mut state, Message::In(b, Box::new(Message::CloseTabConfirmed(0))));
        assert_eq!(texts(&state, 0), ["a1", "a2"]);
    }

    #[test]
    fn new_tabs_open_at_the_end_and_closing_shows_the_neighbour() {
        let mut state = state(vec![tab(&[])], 0);
        for _ in 0..3 { let _ = update(&mut state, Message::WorkspaceNew); }
        assert_eq!(state.workspaces.len(), 4);
        assert_eq!(state.active, 3, "a new tab is shown");
        let all = ids(&state);
        // Closing the shown tab shows the one to its right...
        let _ = update(&mut state, Message::WorkspaceCycle(true));
        let _ = update(&mut state, Message::WorkspaceCycle(true));
        assert_eq!(state.active, 1);
        let _ = update(&mut state, Message::WorkspaceClose(all[1]));
        assert_eq!(ids(&state), [all[0], all[2], all[3]]);
        assert_eq!(state.ws.id, all[2]);
        // ...or, at the end, the one to its left.
        let _ = update(&mut state, Message::WorkspaceCycle(true));
        let _ = update(&mut state, Message::WorkspaceClose(all[3]));
        assert_eq!(state.ws.id, all[2]);
        // Closing a tab in the background keeps the shown one.
        let _ = update(&mut state, Message::WorkspaceClose(all[0]));
        assert_eq!(ids(&state), [all[2]]);
        assert_eq!((state.active, state.current), (0, 0));
        // The last tab is emptied, not removed, under a new id so late replies are dropped.
        let _ = update(&mut state, Message::WorkspaceClose(all[2]));
        assert_eq!(state.workspaces.len(), 1);
        assert_ne!(state.ws.id, all[2]);
        assert!(state.ws.root.is_none() && state.ws.tabs.is_empty());
    }

    #[test]
    fn close_others_and_close_to_the_right() {
        let mut state = state(vec![tab(&[]), tab(&[]), tab(&[]), tab(&[])], 0);
        let all = ids(&state);
        let _ = update(&mut state, Message::WorkspaceCloseRight(all[1]));
        assert_eq!(ids(&state), [all[0], all[1]]);
        let _ = update(&mut state, Message::WorkspaceCloseOthers(all[1]));
        assert_eq!(ids(&state), [all[1]]);
        assert_eq!(state.ws.id, all[1]);
    }

    #[test]
    fn close_folder_with_several_tabs_closes_its_tab() {
        let mut state = state(vec![tab(&[]), tab(&[])], 0);
        let all = ids(&state);
        let _ = update(&mut state, Message::FileAction(FileAction::CloseFolder));
        assert_eq!(ids(&state), [all[1]]);
    }

    #[test]
    fn dragging_a_tab_moves_it_and_keeps_it_shown() {
        let mut state = state(vec![tab(&[]), tab(&[]), tab(&[])], 2);
        let all = ids(&state);
        let _ = update(&mut state, Message::WorkspaceDragStart(all[0]));
        assert_eq!(state.ws.id, all[0], "pressing a tab shows it");
        // Over the third tab's place (tabs 150 wide).
        let _ = update(&mut state, Message::WorkspaceDragMove(320.0, 150.0));
        assert_eq!(ids(&state), [all[1], all[2], all[0]]);
        assert_eq!((state.active, state.ws.id), (2, all[0]));
        let _ = update(&mut state, Message::WorkspaceDragMove(10.0, 150.0));
        assert_eq!(ids(&state), [all[0], all[1], all[2]]);
        // Releasing the mouse ends the drag; moving no longer moves tabs.
        let _ = update(&mut state, Message::TreeDragReleased);
        let _ = update(&mut state, Message::WorkspaceDragMove(320.0, 150.0));
        assert_eq!(ids(&state), [all[0], all[1], all[2]]);
    }

    #[test]
    fn the_session_keeps_every_tab_and_tabs_not_loaded_exactly_as_read() {
        let mut background = tab(&["draft"]);
        background.tabs = vec![PathBuf::from("never-opened.rs")];
        background.layout = Some(Layout { ai_visible: true, terminal_visible: true, ..layout() });
        let mut state = state(vec![tab(&["shown"]), background.clone()], 0);
        let session = state.session();
        assert_eq!(session.active_workspace, 0);
        assert_eq!(session.workspaces[1], background);
        assert_eq!(session.workspaces[0].recovery, tab(&["shown"]).recovery);
        assert_eq!(session.workspaces[0].layout, Some(layout()));
        assert!(state.recovery_saved(), "every tab's unsaved edits are in the saved session");
        assert_eq!(State::all_recovery(&state.saved_session).len(), 2);
        let _ = update(&mut state, Message::WorkspaceNew);
        assert_eq!(state.session().active_workspace, 2);
    }

    #[test]
    fn a_layout_is_restored_per_tab() {
        let wide = Layout { sidebar_visible: false, ai_visible: true, ai_mode: AiMode::Codex, sidebar_mode: SidebarMode::Git, ai_ratio: 0.5, ..layout() };
        let state = state(vec![EditorSession { layout: Some(wide), ..Default::default() }], 0);
        assert_eq!(state.ws.layout(), wide);
        assert!(state.ws.panes.iter().any(|(_, kind)| *kind == PaneKind::Ai));
        assert!(!state.ws.panes.iter().any(|(_, kind)| *kind == PaneKind::Sidebar));
    }

    fn sidebar_pane(state: &State) -> bool {
        state.ws.panes.iter().any(|(_, kind)| *kind == PaneKind::Sidebar)
    }

    #[test]
    fn the_sidebar_only_shows_with_a_folder_and_keeps_its_setting() {
        // Switched on (see `layout`), but the tab has no folder.
        let mut state = state(vec![tab(&[])], 0);
        assert!(!sidebar_pane(&state));
        assert!(state.ws.sidebar_visible && !state.sidebar_shown());
        // Its buttons, menu entries and Ctrl/Cmd+Shift+F do nothing without a folder.
        let find = Message::KeyPressed(keyboard::Key::Character("F".into()), keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT);
        for message in [Message::SidebarToggle, Message::ToggleProjectSearch, Message::GitPanelToggle, find] {
            let _ = update(&mut state, message);
            assert!(!sidebar_pane(&state));
            assert!(state.ws.sidebar_visible, "the setting is not switched off");
            assert_eq!(state.ws.sidebar_mode, SidebarMode::Tree);
        }
        assert!(!command_list(&state).iter().any(|command| matches!(command.message,
            Message::SidebarToggle | Message::ToggleProjectSearch | Message::GitPanelToggle)));
        // A folder opening in the tab brings it back as it was set.
        let dir = tempfile::tempdir().unwrap();
        state.ws.root = Some(dir.path().to_path_buf());
        state.sync_sidebar();
        assert!(sidebar_pane(&state) && state.sidebar_shown());
        assert!(command_list(&state).iter().any(|command| matches!(command.message, Message::SidebarToggle)));
        // Emptying the tab hides it again, still switched on.
        let id = state.ws.id;
        let _ = update(&mut state, Message::WorkspaceClose(id));
        assert!(!sidebar_pane(&state));
        assert!(state.ws.sidebar_visible);
        // Switched off, a folder doesn't bring it back.
        state.ws.sidebar_visible = false;
        state.ws.root = Some(dir.path().to_path_buf());
        state.sync_sidebar();
        assert!(!sidebar_pane(&state));
    }

    fn untitled(text: &str) -> RecoveryBuffer {
        RecoveryBuffer { path: None, text: text.into() }
    }

    #[test]
    fn every_untitled_draft_comes_back_as_its_own_tab() {
        let mut session = tab(&["file"]);
        session.recovery.insert(0, untitled("one"));
        session.recovery.push(untitled("two"));
        let state = state(vec![session.clone()], 0);
        assert_eq!(texts(&state, 0), ["one", "file", "two"]);
        assert_eq!(state.ws.tabs[1].path, session.recovery[1].path, "a file's draft is in its own tab");
        assert!(state.ws.tabs.iter().all(|tab| tab.dirty));
    }

    #[test]
    fn the_active_untitled_draft_is_active_again_after_a_restart() {
        let drafts = EditorSession { recovery: vec![untitled("a"), untitled("b")], layout: Some(layout()), ..Default::default() };
        let state_with = |active_untitled| state(vec![EditorSession { active_untitled, ..drafts.clone() }], 0);
        let restored = state_with(Some(1));
        assert_eq!(restored.ws.tabs[restored.ws.active_tab].content.text(), "b");
        // A session from before this was saved still opens on the first untitled draft.
        let old = state_with(None);
        assert_eq!(old.ws.tabs[old.ws.active_tab].content.text(), "a");
        // What is saved: the active draft's place among the untitled drafts, else nothing.
        let mut both = state_with(Some(1));
        assert_eq!(both.ws.session().active_untitled, Some(1));
        both.ws.active_tab = 0;
        assert_eq!(both.ws.session().active_untitled, Some(0));
        let mut with_file = tab(&["file"]);
        with_file.recovery.push(untitled("c"));
        let mut mixed = state(vec![with_file], 0);
        mixed.ws.active_tab = mixed.ws.tabs.iter().position(|tab| tab.path.is_some()).unwrap();
        assert_eq!(mixed.ws.session().active_untitled, None, "a file is named by its path instead");
    }

    #[test]
    fn workspace_keys_are_ctrl_tab_except_on_macos() {
        use keyboard::{key::Named, Key, Modifiers};
        let tab = Key::Named(Named::Tab);
        let (ctrl, shift, alt, cmd) = (Modifiers::CTRL, Modifiers::SHIFT, Modifiers::ALT, Modifiers::LOGO);
        assert_eq!(workspace_cycle_key_on(false, &tab, ctrl), Some(true));
        assert_eq!(workspace_cycle_key_on(false, &tab, ctrl | shift), Some(false));
        assert_eq!(workspace_cycle_key_on(false, &tab, Modifiers::empty()), None);
        assert_eq!(workspace_cycle_key_on(false, &tab, ctrl | alt), None);
        assert_eq!(workspace_cycle_key_on(false, &Key::Character("]".into()), cmd | shift), None);
        // macOS: Cmd+Shift+] / [ (reported as } / { with Shift); Control+Tab stays the editor's.
        assert_eq!(workspace_cycle_key_on(true, &Key::Character("}".into()), cmd | shift), Some(true));
        assert_eq!(workspace_cycle_key_on(true, &Key::Character("]".into()), cmd | shift), Some(true));
        assert_eq!(workspace_cycle_key_on(true, &Key::Character("{".into()), cmd | shift), Some(false));
        assert_eq!(workspace_cycle_key_on(true, &Key::Character("]".into()), cmd), None);
        assert_eq!(workspace_cycle_key_on(true, &tab, ctrl), None);
        assert_eq!(workspace_cycle_key_on(true, &tab, ctrl | shift), None);
    }

    #[test]
    fn ctrl_tab_gets_through_a_focused_text_box_except_on_macos() {
        let tab = keyboard::Key::Named(keyboard::key::Named::Tab);
        let press = |modifiers| iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: tab.clone(), modified_key: tab.clone(),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Tab),
            location: keyboard::Location::Standard, modifiers, text: None, repeat: false,
        });
        let window = iced::window::Id::unique();
        let captured = handle_raw_key_event(press(keyboard::Modifiers::CTRL), iced::event::Status::Captured, window);
        assert_eq!(captured.is_some(), !cfg!(target_os = "macos"));
        // Other captured keys still stay with the text box.
        assert!(handle_raw_key_event(press(keyboard::Modifiers::empty()), iced::event::Status::Captured, window).is_none());
    }

    #[test]
    fn mac_control_tab_still_indents_in_the_editor() {
        // On macOS the editor's Tab handling ignores Control, so Control+Tab indents; the
        // workspace keys don't take it there (see `workspace_cycle_key_on`). Unchanged here.
        let mut buffer = code_editor::Buffer::new("x", code_editor::metrics_for_zoom(1.0));
        let handled = code_editor::input::handle_key(&mut buffer, &keyboard::Key::Named(keyboard::key::Named::Tab), keyboard::Modifiers::CTRL);
        assert_eq!(handled, cfg!(target_os = "macos"));
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
            diff: Default::default(), hunks: Vec::new(), diff_popup: None,
            diagnostics: Vec::new(), blame: Vec::new(), hover: None, preview: None, image: None,
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
    fn image_tabs_never_become_dirty_text_buffers() {
        let mut tab = Tab {
            path: Some(PathBuf::from("photo.png")),
            content: code_editor::Buffer::new("", code_editor::metrics_for_zoom(1.0)),
            saved_text: Some(String::new()), dirty: false, search: Default::default(),
            line_ending: LineEnding::Lf, diff: Default::default(), hunks: Vec::new(), diff_popup: None, diagnostics: Vec::new(),
            blame: Vec::new(), hover: None, preview: None, image: Some(image_preview::Preview::new()),
        };
        tab.content.perform(cosmic_text::Action::Insert('x'));
        tab.refresh_dirty();
        assert!(!tab.dirty);
        assert_eq!(tab.title(), "photo.png");
        assert!(tab.image.is_some());
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
            diff: Default::default(), hunks: Vec::new(), diff_popup: None,
            diagnostics: Vec::new(), blame: Vec::new(), hover: None, preview: None, image: None,
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
