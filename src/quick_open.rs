//! "Quick Open" (Cmd+P): fuzzy file finder over every file under the open project root.
//!
//! Unlike `project_search` (which walks the disk on every query, since it also reads file
//! contents), this walks the file list once when the panel opens and fuzzy-filters that
//! in-memory list live as the user types -- cheap enough with no background-thread plumbing.

use std::path::{Path, PathBuf};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use iced::widget::{button, column, container, mouse_area, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};

const MAX_RESULTS: usize = 50;

/// Directories skipped during the walk, matching `project_search::IGNORED_DIRS`.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".idea",
    ".vscode",
];

pub fn query_input_id() -> iced::widget::Id {
    iced::widget::Id::new("quick-open-query")
}

#[derive(Default)]
pub struct QuickOpenState {
    pub visible: bool,
    query: String,
    files: Vec<PathBuf>,
    results: Vec<PathBuf>,
    selected: usize,
}

#[derive(Debug, Clone)]
pub enum Message {
    Open,
    Close,
    QueryChanged(String),
    MoveUp,
    MoveDown,
    Confirm,
    ResultClicked(usize),
}

impl QuickOpenState {
    fn refresh_results(&mut self) {
        let matcher = SkimMatcherV2::default();
        if self.query.trim().is_empty() {
            self.results = self.files.iter().take(MAX_RESULTS).cloned().collect();
        } else {
            let mut scored: Vec<(i64, &PathBuf)> = self
                .files
                .iter()
                .filter_map(|path| {
                    let label = path.to_string_lossy();
                    matcher.fuzzy_match(&label, &self.query).map(|score| (score, path))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.results = scored.into_iter().take(MAX_RESULTS).map(|(_, path)| path.clone()).collect();
        }
        self.selected = 0;
    }
}

/// Applies `message`. Returns the focus/close `Task` plus `Some(path)` when a file was
/// picked, for the caller to open.
pub fn update(
    state: &mut QuickOpenState,
    message: Message,
    root: Option<&Path>,
) -> (Task<Message>, Option<PathBuf>) {
    match message {
        Message::Open => {
            state.visible = true;
            state.query.clear();
            state.files = root.map(walk).unwrap_or_default();
            state.refresh_results();
            (iced::widget::operation::focus(query_input_id()), None)
        }
        Message::Close => {
            state.visible = false;
            (Task::none(), None)
        }
        Message::QueryChanged(query) => {
            state.query = query;
            state.refresh_results();
            (Task::none(), None)
        }
        Message::MoveDown => {
            if !state.results.is_empty() {
                state.selected = (state.selected + 1) % state.results.len();
            }
            (Task::none(), None)
        }
        Message::MoveUp => {
            if !state.results.is_empty() {
                state.selected = (state.selected + state.results.len() - 1) % state.results.len();
            }
            (Task::none(), None)
        }
        Message::Confirm => {
            let path = state.results.get(state.selected).cloned();
            if path.is_some() {
                state.visible = false;
            }
            (Task::none(), path)
        }
        Message::ResultClicked(i) => {
            let path = state.results.get(i).cloned();
            state.visible = false;
            (Task::none(), path)
        }
    }
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_into(root, &mut files);
    files
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
            walk_into(&path, out);
        } else {
            out.push(path);
        }
    }
}

pub fn view<'a>(state: &'a QuickOpenState, root: Option<&Path>) -> Element<'a, Message> {
    let input = text_input("Search files by name...", &state.query)
        .id(query_input_id())
        .on_input(Message::QueryChanged)
        .on_submit(Message::Confirm)
        .padding(8)
        .width(Length::Fill);

    let mut results_col = column![].spacing(2);
    if state.results.is_empty() && !state.query.trim().is_empty() {
        results_col = results_col.push(text("No matching files").size(13));
    }
    for (i, path) in state.results.iter().enumerate() {
        let relative = root.and_then(|root| path.strip_prefix(root).ok()).unwrap_or(path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_selected = i == state.selected;
        results_col = results_col.push(
            button(
                column![text(name).size(13), text(relative.display().to_string()).size(11)]
                    .spacing(1),
            )
            .width(Length::Fill)
            .padding(6)
            .style(move |theme, status| result_style(theme, status, is_selected))
            .on_press(Message::ResultClicked(i)),
        );
    }

    let panel = container(
        column![input, scrollable(results_col).height(Length::Fixed(320.0))].spacing(8),
    )
    .padding(12)
    .width(Length::Fixed(560.0))
    .style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.base.color.into()),
            border: iced::Border::default().rounded(8.0).color(palette.background.strong.color).width(1.0),
            ..container::Style::default()
        }
    });

    let backdrop = container(Space::new().width(Length::Fill).height(Length::Fill)).style(
        |theme: &iced::Theme| container::Style {
            background: Some(
                iced::Color { a: 0.4, ..theme.extended_palette().background.base.color.inverse() }.into(),
            ),
            ..container::Style::default()
        },
    );

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

fn result_style(theme: &iced::Theme, status: button::Status, selected: bool) -> button::Style {
    use button::{Status, Style};

    let palette = theme.extended_palette();
    let base = Style {
        text_color: palette.background.base.text,
        border: iced::Border::default().rounded(4.0),
        ..Style::default()
    };
    if selected {
        return base.with_background(palette.primary.weak.color);
    }
    match status {
        Status::Active | Status::Disabled => base.with_background(iced::Color::TRANSPARENT),
        Status::Hovered => base.with_background(palette.background.weak.color),
        Status::Pressed => base.with_background(palette.background.strong.color),
    }
}
