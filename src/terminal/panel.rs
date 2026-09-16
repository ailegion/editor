use super::Terminal;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    name: String,
    path: PathBuf,
}

fn add_profile(profiles: &mut Vec<Profile>, name: &str, path: PathBuf) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !path
            .metadata()
            .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
        {
            return;
        }
    }
    if path.is_file() && !profiles.iter().any(|profile| profile.path == path) {
        profiles.push(Profile {
            name: name.into(),
            path,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::time::{Duration, Instant};

    #[test]
    fn tabs_keep_independent_output_and_route_delayed_messages_by_id() {
        let mut panel = Panel::default();
        // In-memory sessions make switching/closing and delayed clipboard routing deterministic.
        for id in [1, 2] {
            let mut terminal = Terminal::default();
            terminal.parser.process(format!("session {id}").as_bytes());
            panel.tabs.push(Tab {
                id,
                name: "Test".into(),
                terminal,
            });
        }
        panel.active = Some(1);
        let cwd = Path::new(".");
        let _ = panel.update(Message::Select(2), cwd);
        assert!(!panel.tabs[0].terminal.focused);
        assert!(panel.tabs[1].terminal.focused);
        assert_eq!(
            panel.tabs[0].terminal.parser.screen().contents(),
            "session 1"
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        panel.tabs[0].terminal.input = Some(tx);
        let _ = panel.update(
            Message::Session(1, super::super::Message::Pasted(Some("first".into()))),
            cwd,
        );
        assert_eq!(rx.try_recv().unwrap(), b"first");
        let _ = panel.update(Message::Close(1), cwd);
        assert_eq!(panel.active, Some(2));
        // A clipboard response arriving after its tab closes must not hit the next tab.
        let _ = panel.update(
            Message::Session(1, super::super::Message::Pasted(Some("stale".into()))),
            cwd,
        );
        assert_eq!(
            panel.tabs[0].terminal.parser.screen().contents(),
            "session 2"
        );
        let _ = panel.update(Message::Close(2), cwd);
        assert!(panel.tabs.is_empty());
        assert_eq!(panel.active, None);
    }

    #[cfg(windows)]
    #[test]
    fn detected_powershell_profiles_accept_commands() {
        let profiles = discover_profiles();
        assert!(profiles.iter().any(|p| p.name == "CMD"));
        for profile in profiles
            .into_iter()
            .filter(|p| p.name.contains("PowerShell"))
        {
            let mut panel = Panel::default();
            panel.open(Some(profile.clone()), &std::env::current_dir().unwrap());
            let terminal = &mut panel.tabs[0].terminal;
            assert!(
                terminal.child.is_some(),
                "{}: {}",
                profile.name,
                terminal.status
            );
            let deadline = Instant::now() + Duration::from_secs(20);
            // User profiles can replace the prompt entirely; wait for output,
            // then let the shell consume buffered input once initialization ends.
            while terminal.parser.screen().contents().is_empty() {
                terminal.poll();
                assert!(
                    Instant::now() < deadline,
                    "{} did not reach prompt: {}",
                    profile.name,
                    terminal.parser.screen().contents()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            terminal.write(b"Write-Output ('profile-' + 'ready')\r".to_vec());
            loop {
                terminal.poll();
                let output = terminal.parser.screen().contents();
                if output.contains("profile-ready") {
                    break;
                }
                assert!(Instant::now() < deadline, "{}: {output}", profile.name);
                std::thread::sleep(Duration::from_millis(20));
            }
            terminal.write(b"exit\r".to_vec());
            while terminal.child.is_some() {
                terminal.poll();
                assert!(Instant::now() < deadline, "{} did not exit", profile.name);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn find_program(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|path| path.is_file())
    })
}

fn discover_profiles() -> Vec<Profile> {
    let mut profiles = Vec::new();
    #[cfg(windows)]
    {
        for (name, exe) in [
            ("PowerShell", "pwsh.exe"),
            ("Windows PowerShell", "powershell.exe"),
            ("CMD", "cmd.exe"),
        ] {
            if let Some(path) = find_program(exe) {
                add_profile(&mut profiles, name, path);
            }
        }
        if let Some(root) = std::env::var_os("SystemRoot") {
            let root = PathBuf::from(root);
            add_profile(
                &mut profiles,
                "Windows PowerShell",
                root.join("System32/WindowsPowerShell/v1.0/powershell.exe"),
            );
            add_profile(&mut profiles, "CMD", root.join("System32/cmd.exe"));
        }
        for root in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(root) {
                let root = PathBuf::from(root);
                add_profile(
                    &mut profiles,
                    "PowerShell",
                    root.join("PowerShell/7/pwsh.exe"),
                );
                add_profile(&mut profiles, "Git Bash", root.join("Git/bin/bash.exe"));
                add_profile(
                    &mut profiles,
                    "Git Bash",
                    root.join("Programs/Git/bin/bash.exe"),
                );
            }
        }
        // Prefer modern PowerShell over the legacy COMSPEC default (CMD).
        profiles.sort_by_key(|p| match p.name.as_str() {
            "PowerShell" => 0,
            "Windows PowerShell" => 1,
            "CMD" => 2,
            _ => 3,
        });
    }
    #[cfg(unix)]
    {
        if let Some(shell) = std::env::var_os("SHELL") {
            let path = PathBuf::from(shell);
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            add_profile(&mut profiles, &name, path);
        }
        if let Ok(shells) = std::fs::read_to_string("/etc/shells") {
            for line in shells
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with('/'))
            {
                let path = PathBuf::from(line);
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                add_profile(&mut profiles, &name, path);
            }
        }
        for name in ["zsh", "bash", "fish", "sh", "pwsh"] {
            if let Some(path) = find_program(name) {
                add_profile(&mut profiles, name, path);
            }
        }
    }
    profiles
}

struct Tab {
    id: u64,
    name: String,
    terminal: Terminal,
}

#[derive(Debug, Clone)]
pub enum Message {
    ToggleMenu,
    DismissMenu,
    New(usize),
    Select(u64),
    Close(u64),
    Session(u64, super::Message),
    Hide,
}

pub struct Panel {
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
    profiles: Vec<Profile>,
    menu_open: bool,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            profiles: discover_profiles(),
            menu_open: false,
        }
    }
}

impl Panel {
    fn open(&mut self, profile: Option<Profile>, cwd: &Path) {
        self.set_focused(false);
        let id = self.next_id;
        self.next_id += 1;
        let mut terminal = Terminal::default();
        terminal.shell = profile.as_ref().map(|p| p.path.clone());
        terminal.start(cwd);
        self.tabs.push(Tab {
            id,
            name: profile.map_or_else(|| "Shell".into(), |p| p.name),
            terminal,
        });
        self.active = Some(id);
        self.menu_open = false;
    }

    pub fn ensure_started(&mut self, cwd: &Path) {
        if self.tabs.is_empty() {
            let saved = crate::config_path("terminal_shell")
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(PathBuf::from);
            let profile = self
                .profiles
                .iter()
                .find(|p| Some(&p.path) == saved.as_ref())
                .or_else(|| self.profiles.first())
                .cloned();
            self.open(profile, cwd);
        }
    }

    pub fn focused(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| Some(tab.id) == self.active && tab.terminal.focused)
    }

    pub fn set_focused(&mut self, focused: bool) {
        for tab in &mut self.tabs {
            tab.terminal.focused = focused && Some(tab.id) == self.active;
        }
        if !focused {
            self.menu_open = false;
        }
    }

    pub fn shutdown(&mut self) {
        self.tabs.clear();
        self.active = None;
    }

    pub fn poll(&mut self) {
        for tab in &mut self.tabs {
            tab.terminal.poll();
        }
    }

    pub fn update(&mut self, message: Message, cwd: &Path) -> Task<Message> {
        match message {
            Message::ToggleMenu => self.menu_open = !self.menu_open,
            Message::DismissMenu => self.menu_open = false,
            Message::New(index) => {
                if let Some(profile) = self.profiles.get(index).cloned() {
                    if let Some(path) = crate::config_path("terminal_shell") {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(path, profile.path.to_string_lossy().as_bytes());
                    }
                    self.open(Some(profile), cwd);
                    return iced::widget::operation::focus("terminal-canvas");
                }
            }
            Message::Select(id) => {
                if self.tabs.iter().any(|tab| tab.id == id) {
                    self.active = Some(id);
                    self.menu_open = false;
                    self.set_focused(true);
                    return iced::widget::operation::focus("terminal-canvas");
                }
            }
            Message::Close(id) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                    self.tabs.remove(index);
                    if self.active == Some(id) {
                        self.active = self
                            .tabs
                            .get(index.min(self.tabs.len().saturating_sub(1)))
                            .map(|tab| tab.id);
                        self.set_focused(true);
                    }
                }
            }
            Message::Session(id, message) => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    if matches!(message, super::Message::Restart) {
                        tab.terminal.start(cwd);
                    } else {
                        return tab
                            .terminal
                            .update(message)
                            .map(move |msg| Message::Session(id, msg));
                    }
                }
            }
            Message::Hide => {}
        }
        Task::none()
    }

    pub fn view(&self, enabled: bool) -> Element<'_, Message> {
        use lucide_icons::Icon;
        let mut tabs = row![].spacing(1).padding([2, 4]);
        for tab in &self.tabs {
            let selected = Some(tab.id) == self.active;
            let glyph: char = Icon::Terminal.into();
            let title = row![
                button(
                    row![
                        text(glyph).font(iced::Font::with_name("lucide")).size(14),
                        text(format!("{} {}", tab.name, tab.id)).size(13),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center)
                )
                .padding([4, 8])
                .on_press(Message::Select(tab.id))
                .style(move |theme, status| crate::tab_button_style(theme, status, selected)),
                crate::icon_control(
                    Icon::X,
                    "Close terminal (ends session)",
                    Some(Message::Close(tab.id)),
                    false
                ),
            ]
            .spacing(2)
            .align_y(iced::Alignment::Center);
            let underline = container(Space::new().width(Length::Fill).height(2)).style(
                move |theme: &iced::Theme| container::Style {
                    background: Some(
                        if selected {
                            theme.extended_palette().primary.base.color
                        } else {
                            iced::Color::TRANSPARENT
                        }
                        .into(),
                    ),
                    ..Default::default()
                },
            );
            let tab_content = container(column![title, underline].spacing(2)).style(
                move |theme: &iced::Theme| container::Style {
                    background: selected
                        .then(|| theme.extended_palette().background.weak.color.into()),
                    ..Default::default()
                },
            );
            tabs = tabs.push(
                iced::widget::mouse_area(tab_content).on_middle_press(Message::Close(tab.id)),
            );
        }
        let mut menu = column![text("New terminal").size(12)].spacing(2);
        for (index, profile) in self.profiles.iter().enumerate() {
            menu = menu.push(
                button(text(&profile.name).size(13))
                    .padding([6, 10])
                    .style(crate::flat_button_style)
                    .on_press(Message::New(index))
                    .width(Length::Fill),
            );
        }
        if self.profiles.is_empty() {
            menu = menu.push(text("No installed shells found").size(12));
        }
        let new_terminal = iced_aw::DropDown::new(
            crate::icon_control(
                Icon::Plus,
                "New terminal — choose shell",
                Some(Message::ToggleMenu),
                self.menu_open,
            ),
            container(scrollable(menu).height(Length::Shrink))
                .padding(6)
                .max_height(240)
                .style(container::rounded_box),
            self.menu_open,
        )
        .width(220)
        .on_dismiss(Message::DismissMenu);
        let header = row![
            scrollable(tabs)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default()
                ))
                .width(Length::Shrink),
            new_terminal,
            Space::new().width(Length::Fill),
            crate::icon_control(
                Icon::X,
                "Hide terminal (Ctrl+`)",
                Some(Message::Hide),
                false
            ),
        ]
        .spacing(4)
        .padding([0, 4])
        .align_y(iced::Alignment::Center);
        let mut content = column![
            container(header).style(crate::chrome_style),
            iced::widget::rule::horizontal(1),
        ];
        if let Some(tab) = self.tabs.iter().find(|tab| Some(tab.id) == self.active) {
            let id = tab.id;
            content = content.push(
                super::view(&tab.terminal, enabled && !self.menu_open)
                    .map(move |msg| Message::Session(id, msg)),
            );
        } else {
            content = content.push(
                container(text("Press + to open a terminal"))
                    .padding(16)
                    .height(Length::Fill),
            );
        }
        content.height(Length::Fill).into()
    }
}
