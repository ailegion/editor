mod acp;
mod chat;
mod code_editor;

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

struct Tab {
    path: Option<PathBuf>,
    content: code_editor::Buffer,
    search: code_editor::search::SearchState,
    dirty: bool,
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
}

impl std::fmt::Display for EditAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            EditAction::Undo => "Undo (Cmd+Z)",
            EditAction::Redo => "Redo (Cmd+Shift+Z)",
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
    TabSelected(usize),
    TabClosed(usize),

    Tree(DirectoryTreeEvent),
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

    AiToggle,
    AiModeSelected(AiMode),
    Chat(chat::Message),
    Acp(acp::Message),
    Tick,
}

struct State {
    root: Option<PathBuf>,
    tabs: Vec<Tab>,
    active_tab: usize,
    focus: Focus,

    tree: Option<DirectoryTree>,
    renaming: Option<(PathBuf, String)>,
    creating: Option<(PathBuf, bool, String)>,

    app_theme: iced::Theme,
    highlighter: code_editor::Highlighter,
    zoom: f32,

    panes: pane_grid::State<PaneKind>,

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
        let ai_visible = load_ai_visible();
        let app_theme = load_app_theme();
        let zoom = load_zoom();
        let main_pane = pane_grid::Configuration::Pane(PaneKind::Main);
        let main_and_ai = if ai_visible {
            pane_grid::Configuration::Split {
                axis: pane_grid::Axis::Vertical,
                ratio: 0.75,
                a: Box::new(main_pane),
                b: Box::new(pane_grid::Configuration::Pane(PaneKind::Ai)),
            }
        } else {
            main_pane
        };
        let panes = pane_grid::State::with_configuration(pane_grid::Configuration::Split {
            axis: pane_grid::Axis::Vertical,
            ratio: 0.2,
            a: Box::new(pane_grid::Configuration::Pane(PaneKind::Sidebar)),
            b: Box::new(main_and_ai),
        });
        Self {
            root,
            tabs: Vec::new(),
            active_tab: 0,
            focus: Focus::Editor,
            tree,
            renaming: None,
            creating: None,
            app_theme,
            highlighter: code_editor::Highlighter::new(),
            zoom,
            panes,
            ai_visible,
            ai_mode: AiMode::default(),
            chat: chat::ChatState::default(),
            acp: acp::AcpState::default(),
        }
    }

    fn root_or_cwd(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(|| PathBuf::from("."))
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

    fn active_path(&self) -> Option<PathBuf> {
        self.tabs.get(self.active_tab).and_then(|t| t.path.clone())
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
        let mut content = code_editor::Buffer::new(&text, code_editor::metrics_for_zoom(self.zoom));
        content.highlight(&self.highlighter, &extension, &self.app_theme);
        self.tabs.push(Tab {
            path: Some(path),
            content,
            search: code_editor::search::SearchState::default(),
            dirty: false,
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
                    keyboard::Key::Character("f") => {
                        if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                            code_editor::search::update(
                                &mut tab.search,
                                &mut tab.content,
                                code_editor::search::Message::Toggle,
                            );
                        }
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
                        state.panes.resize(split, 0.75);
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
            }
        }
        Message::AiModeSelected(mode) => state.ai_mode = mode,
        Message::Chat(msg) => chat::update(&mut state.chat, msg),
        Message::Acp(msg) => {
            let cwd = state.root_or_cwd();
            acp::update(&mut state.acp, msg, cwd);
        }
        Message::Tick => {
            state.chat.poll();
            state.acp.poll();
            if state.acp.take_finished() {
                state.reload_open_tabs();
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
            PaneKind::Sidebar => container(view_tree(state)).padding(8).into(),
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

    column![top_bar, panes, status_bar].into()
}

fn view_ai_sidebar(state: &State) -> Element<'_, Message> {
    let mode_row = row![
        button(text("HTTP")).on_press(Message::AiModeSelected(AiMode::Http)),
        button(text("Claude Code")).on_press(Message::AiModeSelected(AiMode::Acp)),
    ]
    .spacing(4);

    let panel: Element<'_, Message> = match state.ai_mode {
        AiMode::Http => chat::view(&state.chat).map(Message::Chat),
        AiMode::Acp => acp::view(&state.acp, state.root_or_cwd()).map(Message::Acp),
    };

    column![mode_row, panel].height(Length::Fill).into()
}

/// Transparent-background button style, so a title + close button pair placed inside a
/// pill-shaped `container` (see `view_editor`'s tab strip) reads as one merged tab rather
/// than two separate button-shaped elements.
fn flat_button_style(theme: &iced::Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
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
    let status = state
        .active_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "No file open".to_string());
    row![
        text(status).width(Length::Fill),
        button(text("AI")).on_press(Message::AiToggle),
    ]
    .padding(4)
    .into()
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
            ]
            .width(Length::Fixed(200.0)),
        )
        .padding(4)
        .style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            iced::widget::container::Style {
                background: Some(palette.background.weak.color.into()),
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
    Subscription::batch([keys, tick])
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

fn config_path(name: &str) -> Option<PathBuf> {
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
    iced::application(State::new, update, view)
        .title("editor")
        .theme(|state: &State| state.app_theme.clone())
        .subscription(subscription)
        .font(iced_swdir_tree::LUCIDE_FONT_BYTES)
        .run()
}
