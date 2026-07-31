mod acp;
mod chat;

use iced::highlighter;
use iced::keyboard;
use iced::widget::{button, column, container, row, scrollable, text, text_editor, text_input};
use iced::{Alignment, Element, Length, Subscription, Task};
use iced_aw::menu::{Item, Menu};
use iced_aw::{menu_bar, menu_items};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Tab {
    path: Option<PathBuf>,
    content: text_editor::Content,
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

#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum AiMode {
    #[default]
    Http,
    Acp,
}

#[derive(Debug, Clone)]
enum Message {
    EditorAction(text_editor::Action),
    TabSelected(usize),
    TabClosed(usize),

    TreeEntryClicked(PathBuf),
    TreeMenuToggle(PathBuf),
    NewFile(PathBuf),
    NewFolder(PathBuf),
    RenameStart(PathBuf),
    DeleteEntry(PathBuf),
    RenameInput(String),
    RenameSubmit,
    RenameCancel,
    CreateInput(String),
    CreateSubmit,
    CreateCancel,

    FileAction(FileAction),
    AppThemeSelected(iced::Theme),

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

    expanded: HashSet<PathBuf>,
    tree_menu_open: Option<PathBuf>,
    renaming: Option<(PathBuf, String)>,
    creating: Option<(PathBuf, bool, String)>,

    app_theme: iced::Theme,

    ai_visible: bool,
    ai_mode: AiMode,
    chat: chat::ChatState,
    acp: acp::AcpState,
}

impl State {
    fn new() -> Self {
        Self {
            root: load_last_project(),
            tabs: Vec::new(),
            active_tab: 0,
            expanded: HashSet::new(),
            tree_menu_open: None,
            renaming: None,
            creating: None,
            app_theme: iced::Theme::Dark,
            ai_visible: load_ai_visible(),
            ai_mode: AiMode::default(),
            chat: chat::ChatState::default(),
            acp: acp::AcpState::default(),
        }
    }

    fn root_or_cwd(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(|| PathBuf::from("."))
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
                    tab.content = text_editor::Content::with_text(&text);
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
        self.tabs.push(Tab {
            path: Some(path),
            content: text_editor::Content::with_text(&text),
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
    match message {
        Message::EditorAction(action) => {
            if let Some(tab) = state.tabs.get_mut(state.active_tab) {
                if action.is_edit() {
                    tab.dirty = true;
                }
                tab.content.perform(action);
            }
        }
        Message::TabSelected(i) => state.active_tab = i,
        Message::TabClosed(i) => state.close_tab(i),

        Message::FileAction(action) => match action {
            FileAction::OpenFile => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    state.open_path(path);
                }
            }
            FileAction::OpenFolder => {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    save_last_project(&path);
                    state.root = Some(path);
                }
            }
            FileAction::CloseFolder => {
                state.root = None;
                if let Some(path) = last_project_path() {
                    let _ = std::fs::remove_file(path);
                }
            }
            FileAction::Save => state.save(),
        },

        Message::TreeEntryClicked(path) => {
            if path.is_dir() {
                if !state.expanded.remove(&path) {
                    state.expanded.insert(path);
                }
            } else {
                state.open_path(path);
            }
        }
        Message::TreeMenuToggle(path) => {
            state.tree_menu_open = if state.tree_menu_open.as_deref() == Some(path.as_path()) {
                None
            } else {
                Some(path)
            };
        }
        Message::NewFile(dir) => {
            state.creating = Some((dir.clone(), false, String::new()));
            state.expanded.insert(dir);
            state.tree_menu_open = None;
        }
        Message::NewFolder(dir) => {
            state.creating = Some((dir.clone(), true, String::new()));
            state.expanded.insert(dir);
            state.tree_menu_open = None;
        }
        Message::RenameStart(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            state.renaming = Some((path, name));
            state.tree_menu_open = None;
        }
        Message::DeleteEntry(path) => {
            state.delete_path(&path);
            state.tree_menu_open = None;
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
                    if is_dir {
                        let _ = std::fs::create_dir(target);
                    } else {
                        let _ = std::fs::write(target, "");
                    }
                }
            }
        }
        Message::CreateCancel => state.creating = None,

        Message::AppThemeSelected(theme) => state.app_theme = theme,

        Message::KeyPressed(key, modifiers) => {
            if modifiers.command() {
                match key.as_ref() {
                    keyboard::Key::Character("s") => state.save(),
                    keyboard::Key::Character("o") => {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            state.open_path(path);
                        }
                    }
                    _ => {}
                }
            }
        }
        Message::Noop => {}

        Message::AiToggle => {
            state.ai_visible = !state.ai_visible;
            save_ai_visible(state.ai_visible);
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
    Task::none()
}

fn view(state: &State) -> Element<'_, Message> {
    let top_bar = view_top_bar(state);
    let status_bar = view_status_bar(state);

    let sidebar = container(scrollable(view_tree(state)))
        .width(Length::Fixed(220.0))
        .height(Length::Fill)
        .padding(8);

    let editor = view_editor(state);

    let mut body = row![sidebar, editor].height(Length::Fill);
    if state.ai_visible {
        body = body.push(
            container(view_ai_sidebar(state))
                .width(Length::Fixed(320.0))
                .height(Length::Fill)
                .padding(8),
        );
    }

    column![top_bar, body, status_bar].into()
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

fn menu_button<'a>(label: String, msg: Message) -> iced::widget::button::Button<'a, Message> {
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
        .on_press(msg)
}

fn view_top_bar(_state: &State) -> Element<'_, Message> {
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

    let theme_menu_button = menu_button("Theme".to_string(), Message::Noop).width(Length::Shrink);
    let theme_items: Vec<_> = iced::Theme::ALL
        .iter()
        .map(|t| Item::new(menu_button(t.to_string(), Message::AppThemeSelected(t.clone()))))
        .collect();

    let mb = menu_bar!(
        (file_menu_button, menu_tpl(file_items)),
        (theme_menu_button, menu_tpl(theme_items))
    );

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
    let Some(root) = state.root.clone() else {
        return text("No folder open (File > Open Folder)").into();
    };
    render_dir_entry(state, root, true)
}

fn render_dir_entry(state: &State, path: PathBuf, is_root: bool) -> Element<'_, Message> {
    let name = if is_root {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    } else {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let is_expanded = is_root || state.expanded.contains(&path);
    let arrow = if is_expanded { "v" } else { ">" };

    let mut col = column![];

    let header = row![
        button(text(format!("{arrow} {name}")))
            .on_press(Message::TreeEntryClicked(path.clone()))
            .width(Length::Fill),
        button(text("...")).on_press(Message::TreeMenuToggle(path.clone())),
    ]
    .align_y(Alignment::Center);
    col = col.push(header);

    if state.tree_menu_open.as_deref() == Some(path.as_path()) {
        col = col.push(view_entry_menu(state, path.clone()));
    }

    if state.creating.as_ref().map(|(d, _, _)| d.as_path()) == Some(path.as_path()) {
        let (_, is_dir, buf) = state.creating.as_ref().unwrap();
        let hint = if *is_dir { "new folder name" } else { "new file name" };
        col = col.push(row![
            text_input(hint, buf)
                .on_input(Message::CreateInput)
                .on_submit(Message::CreateSubmit),
            button(text("x")).on_press(Message::CreateCancel),
        ]);
    }

    if is_expanded {
        if let Ok(read_dir) = std::fs::read_dir(&path) {
            let mut entries: Vec<PathBuf> = read_dir.flatten().map(|e| e.path()).collect();
            entries.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_owned())));
            for entry in entries {
                if state.renaming.as_ref().map(|(p, _)| p.as_path()) == Some(entry.as_path()) {
                    let (_, buf) = state.renaming.as_ref().unwrap();
                    col = col.push(
                        row![
                            text("  "),
                            text_input("", buf)
                                .on_input(Message::RenameInput)
                                .on_submit(Message::RenameSubmit),
                            button(text("x")).on_press(Message::RenameCancel),
                        ],
                    );
                    continue;
                }
                if entry.is_dir() {
                    col = col.push(
                        container(render_dir_entry(state, entry, false)).padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 12.0 }),
                    );
                } else {
                    let entry_name = entry
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let selected = state.active_path().as_deref() == Some(entry.as_path());
                    let label = if selected {
                        format!("* {entry_name}")
                    } else {
                        entry_name
                    };
                    col = col.push(container(
                        row![
                            button(text(label))
                                .on_press(Message::TreeEntryClicked(entry.clone()))
                                .width(Length::Fill),
                            button(text("...")).on_press(Message::TreeMenuToggle(entry.clone())),
                        ]
                        .align_y(Alignment::Center),
                    ).padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 12.0 }));
                    if state.tree_menu_open.as_deref() == Some(entry.as_path()) {
                        col = col.push(
                            container(view_entry_menu(state, entry.clone())).padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 12.0 }),
                        );
                    }
                }
            }
        }
    }

    col.into()
}

fn view_entry_menu(_state: &State, path: PathBuf) -> Element<'_, Message> {
    let is_dir = path.is_dir();
    let target_dir = if is_dir {
        path.clone()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    };
    row![
        button(text("New File")).on_press(Message::NewFile(target_dir.clone())),
        button(text("New Folder")).on_press(Message::NewFolder(target_dir)),
        button(text("Rename")).on_press(Message::RenameStart(path.clone())),
        button(text("Delete")).on_press(Message::DeleteEntry(path)),
    ]
    .spacing(4)
    .into()
}

fn view_editor(state: &State) -> Element<'_, Message> {
    let mut tab_row = row![].spacing(4).padding(4);
    for (i, tab) in state.tabs.iter().enumerate() {
        tab_row = tab_row.push(
            row![
                button(text(tab.title())).on_press(Message::TabSelected(i)),
                button(text("x")).on_press(Message::TabClosed(i)),
            ]
            .spacing(2),
        );
    }

    let editor: Element<'_, Message> = if let Some(tab) = state.tabs.get(state.active_tab) {
        let extension = tab
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("txt");
        text_editor(&tab.content)
            .on_action(Message::EditorAction)
            .highlight(extension, highlighter::Theme::SolarizedDark)
            .height(Length::Fill)
            .into()
    } else {
        text("No file open").into()
    };

    column![tab_row, editor].height(Length::Fill).into()
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

fn config_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("editor").join(name))
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

fn confirm(title: &str, description: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(description)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

pub fn main() -> iced::Result {
    iced::application(State::new, update, view)
        .title("editor")
        .theme(|state: &State| state.app_theme.clone())
        .subscription(subscription)
        .run()
}
