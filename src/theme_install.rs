//! "Install Theme..." / "Uninstall Theme..." panel: browse color themes on Open VSX and
//! install them into the user themes folder, or remove installed ones.

use std::time::Duration;

use iced::widget::{button, column, container, mouse_area, row, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};

use crate::theme::openvsx::{self, Extension, Installed};

/// Pause after typing before searching, so each keystroke doesn't hit Open VSX.
const SEARCH_DELAY: Duration = Duration::from_millis(350);

fn query_input_id() -> iced::widget::Id {
    iced::widget::Id::new("theme-install-query")
}

fn results_id() -> iced::widget::Id {
    iced::widget::Id::new("theme-install-results")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Install,
    Uninstall,
}

#[derive(Default)]
enum Status {
    #[default]
    Idle,
    Busy(String),
    Error(String),
}

#[derive(Default)]
pub struct ThemeInstallState {
    pub visible: bool,
    mode: Mode,
    query: String,
    results: Vec<Extension>,
    installed: Vec<Installed>,
    selected: usize,
    status: Status,
    /// Bumped whenever a search is superseded, so late results are dropped.
    generation: u64,
}

#[derive(Debug, Clone)]
pub enum Message {
    Open(Mode),
    Close,
    QueryChanged(String),
    SearchDue(u64),
    Searched(u64, Result<Vec<Extension>, String>),
    MoveUp,
    MoveDown,
    Confirm,
    Clicked(usize),
    Installed(Result<Vec<String>, String>),
}

/// Changes the app has to react to.
pub enum Event {
    /// Theme names that were installed.
    Installed(Vec<String>),
    /// Display name of the removed extension.
    Uninstalled(String),
}

impl ThemeInstallState {
    fn filtered_installed(&self) -> Vec<&Installed> {
        let query = self.query.trim().to_lowercase();
        self.installed
            .iter()
            .filter(|extension| {
                query.is_empty()
                    || extension.display_name.to_lowercase().contains(&query)
                    || extension.themes.iter().any(|theme| theme.to_lowercase().contains(&query))
            })
            .collect()
    }

    fn len(&self) -> usize {
        match self.mode {
            Mode::Install => self.results.len(),
            Mode::Uninstall => self.filtered_installed().len(),
        }
    }

    fn search(&mut self) -> Task<Message> {
        self.generation += 1;
        let generation = self.generation;
        let query = self.query.clone();
        self.status = Status::Busy(if query.trim().is_empty() { "Loading popular themes...".into() } else { "Searching...".into() });
        Task::perform(
            async move { tokio::task::spawn_blocking(move || openvsx::search(&query)).await.map_err(|err| err.to_string()).and_then(|result| result) },
            move |result| Message::Searched(generation, result),
        )
    }
}

pub fn update(state: &mut ThemeInstallState, message: Message) -> (Task<Message>, Option<Event>) {
    match message {
        Message::Open(mode) => {
            *state = ThemeInstallState { visible: true, mode, generation: state.generation, ..ThemeInstallState::default() };
            let task = match mode {
                Mode::Install => state.search(),
                Mode::Uninstall => {
                    state.installed = crate::theme::user_themes_dir().map(|dir| openvsx::installed(&dir)).unwrap_or_default();
                    Task::none()
                }
            };
            (Task::batch([task, iced::widget::operation::focus(query_input_id())]), None)
        }
        Message::Close => {
            state.visible = false;
            state.generation += 1;
            (Task::none(), None)
        }
        Message::QueryChanged(query) => {
            state.query = query;
            state.selected = 0;
            if state.mode == Mode::Uninstall {
                return (Task::none(), None);
            }
            state.generation += 1;
            let generation = state.generation;
            (Task::perform(tokio::time::sleep(SEARCH_DELAY), move |()| Message::SearchDue(generation)), None)
        }
        Message::SearchDue(generation) => {
            let task = if generation == state.generation && state.visible { state.search() } else { Task::none() };
            (task, None)
        }
        Message::Searched(generation, result) => {
            if generation == state.generation {
                match result {
                    Ok(results) => {
                        state.results = results;
                        state.selected = 0;
                        state.status = Status::Idle;
                    }
                    Err(err) => state.status = Status::Error(err),
                }
            }
            (Task::none(), None)
        }
        Message::MoveDown | Message::MoveUp => {
            let len = state.len();
            if len > 0 {
                let step = if matches!(message, Message::MoveDown) { 1 } else { len - 1 };
                state.selected = (state.selected + step) % len;
            }
            let ratio = if len > 1 { state.selected as f32 / (len - 1) as f32 } else { 0.0 };
            let scroll = iced::widget::operation::snap_to(results_id(), iced::widget::scrollable::RelativeOffset { x: 0.0, y: ratio });
            (scroll, None)
        }
        Message::Confirm => {
            let index = state.selected;
            act(state, index)
        }
        Message::Clicked(index) => act(state, index),
        Message::Installed(result) => match result {
            Ok(names) => {
                state.visible = false;
                state.status = Status::Idle;
                (Task::none(), Some(Event::Installed(names)))
            }
            Err(err) => {
                state.status = Status::Error(format!("Install failed: {err}"));
                (Task::none(), None)
            }
        },
    }
}

/// Installs or uninstalls the item at `index`.
fn act(state: &mut ThemeInstallState, index: usize) -> (Task<Message>, Option<Event>) {
    if matches!(state.status, Status::Busy(_)) {
        return (Task::none(), None);
    }
    let Some(dir) = crate::theme::user_themes_dir() else {
        state.status = Status::Error("No config folder to install themes into".into());
        return (Task::none(), None);
    };
    match state.mode {
        Mode::Install => {
            let Some(extension) = state.results.get(index).cloned() else { return (Task::none(), None) };
            state.selected = index;
            state.status = Status::Busy(format!("Installing {}...", extension.display_name));
            let task = Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || openvsx::install(&extension, &dir))
                        .await
                        .map_err(|err| err.to_string())
                        .and_then(|result| result)
                },
                Message::Installed,
            );
            (task, None)
        }
        Mode::Uninstall => {
            let Some(extension) = state.filtered_installed().get(index).map(|extension| (*extension).clone()) else {
                return (Task::none(), None);
            };
            match openvsx::uninstall(&dir, &extension.id) {
                Ok(()) => {
                    state.installed.retain(|installed| installed.id != extension.id);
                    state.selected = 0;
                    state.status = Status::Idle;
                    (Task::none(), Some(Event::Uninstalled(extension.display_name)))
                }
                Err(err) => {
                    state.status = Status::Error(err);
                    (Task::none(), None)
                }
            }
        }
    }
}

pub fn view(state: &ThemeInstallState) -> Element<'_, Message> {
    let (title, placeholder, action) = match state.mode {
        Mode::Install => ("Install Theme", "Search color themes on Open VSX...", "Install"),
        Mode::Uninstall => ("Uninstall Theme", "Filter installed themes...", "Uninstall"),
    };
    let input = text_input(placeholder, &state.query)
        .id(query_input_id())
        .on_input(Message::QueryChanged)
        .on_submit(Message::Confirm)
        .padding(8)
        .width(Length::Fill);

    let rows: Vec<(String, String, String)> = match state.mode {
        Mode::Install => state
            .results
            .iter()
            .map(|extension| {
                let license = extension.license.as_deref().unwrap_or("no license stated");
                (
                    format!("{}  v{}", extension.display_name, extension.version),
                    format!("{} · {} · {} downloads · {license}", extension.namespace, extension.themes.join(", "), compact(extension.downloads)),
                    extension.description.clone(),
                )
            })
            .collect(),
        Mode::Uninstall => state
            .filtered_installed()
            .into_iter()
            .map(|extension| (extension.display_name.clone(), extension.themes.join(", "), extension.id.clone()))
            .collect(),
    };

    let mut list = column![].spacing(2);
    if rows.is_empty() && matches!(state.status, Status::Idle) {
        let empty = match state.mode {
            Mode::Install => "No color themes found",
            Mode::Uninstall => "No installed themes",
        };
        list = list.push(text(empty).size(13));
    }
    for (index, (name, details, extra)) in rows.into_iter().enumerate() {
        let selected = index == state.selected;
        list = list.push(
            button(column![text(name).size(13), text(details).size(11), text(extra).size(11).style(iced::widget::text::secondary)].spacing(1))
                .width(Length::Fill)
                .padding(6)
                .style(move |theme, status| crate::quick_open::result_style(theme, status, selected))
                .on_press(Message::Clicked(index)),
        );
    }

    let status: Element<'_, Message> = match &state.status {
        Status::Idle => Space::new().height(Length::Fixed(0.0)).into(),
        Status::Busy(message) => text(message.clone()).size(12).into(),
        Status::Error(message) => text(message.clone()).size(12).style(iced::widget::text::danger).into(),
    };
    let hint = row![
        text(format!("↑ ↓ Navigate    Enter {action}    Esc Close")).size(11).style(iced::widget::text::secondary),
        Space::new().width(Length::Fill),
    ];
    let mut body = column![text(title).size(14), input, status, scrollable(list).id(results_id()).height(Length::Fixed(360.0)), hint].spacing(12);
    if state.mode == Mode::Install {
        body = body.push(
            text("Themes come from open-vsx.org and keep their own licenses; check a theme's license before redistributing it.")
                .size(11)
                .style(iced::widget::text::secondary),
        );
    }

    let panel = container(body).padding(16).width(Length::Fill).max_width(640).style(crate::overlay_style);
    let backdrop = container(Space::new().width(Length::Fill).height(Length::Fill)).style(|theme: &iced::Theme| container::Style {
        background: Some(iced::Color { a: 0.4, ..theme.extended_palette().background.base.color.inverse() }.into()),
        ..container::Style::default()
    });
    mouse_area(iced::widget::stack![
        backdrop,
        container(panel)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::Alignment::Center)
            .padding(iced::Padding { top: 80.0, ..iced::Padding::default() }),
    ])
    .on_press(Message::Close)
    .into()
}

/// 1234 -> "1.2K", 522214 -> "522K", 1500000 -> "1.5M".
fn compact(count: u64) -> String {
    let (value, suffix) = match count {
        0..1_000 => return count.to_string(),
        1_000..1_000_000 => (count as f64 / 1_000.0, "K"),
        _ => (count as f64 / 1_000_000.0, "M"),
    };
    if value < 10.0 { format!("{value:.1}{suffix}") } else { format!("{}{suffix}", value as u64) }
}

#[cfg(test)]
mod tests {
    use super::compact;

    #[test]
    fn compacts_download_counts() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_234), "1.2K");
        assert_eq!(compact(522_214), "522K");
        assert_eq!(compact(1_500_000), "1.5M");
        assert_eq!(compact(12_000_000), "12M");
    }
}
