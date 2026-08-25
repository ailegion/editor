//! Command Palette (Cmd+Shift+P): a searchable list of app-level actions, filtered with the
//! same fuzzy matcher as `quick_open`. Reuses the same overlay/backdrop presentation, too.
//!
//! Unlike `quick_open` (which walks the filesystem on open), the command list here is built
//! by the caller (`main.rs`, via `command_list`) from state it already has -- gating e.g.
//! Undo/Redo on whether there's anything to undo/redo, exactly like the existing Edit menu
//! does. `open` takes that list and snapshots it into `PaletteState` for the duration the
//! palette stays open, so filtering as the user types is just an in-memory fuzzy match, no
//! rebuilding.

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use iced::widget::{button, column, container, mouse_area, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};

/// One selectable action: a label shown in the palette, and the app-level `Message` it
/// dispatches (via a direct recursive call into `main::update`, see `main.rs`'s
/// `Message::CommandPalette` handler) when picked.
pub struct Command {
    pub label: String,
    pub message: crate::Message,
}

pub fn query_input_id() -> iced::widget::Id {
    iced::widget::Id::new("command-palette-query")
}

fn results_id() -> iced::widget::Id {
    iced::widget::Id::new("command-palette-results")
}

/// Scrolls the results list so the selected row is (proportionally) in view -- see
/// `quick_open::scroll_to_selected` for the same trick and its caveats.
fn scroll_to_selected(state: &PaletteState) -> Task<Message> {
    if state.results.len() <= 1 {
        return Task::none();
    }
    let ratio = state.selected as f32 / (state.results.len() - 1) as f32;
    iced::widget::operation::snap_to(results_id(), iced::widget::scrollable::RelativeOffset { x: 0.0, y: ratio })
}

#[derive(Default)]
pub struct PaletteState {
    pub visible: bool,
    query: String,
    commands: Vec<Command>,
    results: Vec<usize>,
    selected: usize,
}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
    QueryChanged(String),
    MoveUp,
    MoveDown,
    Confirm,
    ResultClicked(usize),
}

impl PaletteState {
    fn refresh(&mut self) {
        let matcher = SkimMatcherV2::default();
        if self.query.trim().is_empty() {
            self.results = (0..self.commands.len()).collect();
        } else {
            let mut scored: Vec<(i64, usize)> = self
                .commands
                .iter()
                .enumerate()
                .filter_map(|(i, cmd)| matcher.fuzzy_match(&cmd.label, &self.query).map(|score| (score, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.results = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = 0;
    }
}

/// Opens the palette with a fresh snapshot of `commands`, focusing the query box.
pub fn open(state: &mut PaletteState, commands: Vec<Command>) -> Task<Message> {
    state.visible = true;
    state.query.clear();
    state.commands = commands;
    state.refresh();
    iced::widget::operation::focus(query_input_id())
}

/// Applies `message`, returning a `Task` (e.g. to scroll the selection into view) plus the
/// picked command's `Message` (if any) for the caller to dispatch.
pub fn update(state: &mut PaletteState, message: Message) -> (Task<Message>, Option<crate::Message>) {
    match message {
        Message::Close => {
            state.visible = false;
            (Task::none(), None)
        }
        Message::QueryChanged(query) => {
            state.query = query;
            state.refresh();
            (Task::none(), None)
        }
        Message::MoveDown => {
            if !state.results.is_empty() {
                state.selected = (state.selected + 1) % state.results.len();
            }
            (scroll_to_selected(state), None)
        }
        Message::MoveUp => {
            if !state.results.is_empty() {
                state.selected = (state.selected + state.results.len() - 1) % state.results.len();
            }
            (scroll_to_selected(state), None)
        }
        Message::Confirm => {
            let picked = state
                .results
                .get(state.selected)
                .and_then(|&i| state.commands.get(i))
                .map(|cmd| cmd.message.clone());
            if picked.is_some() {
                state.visible = false;
            }
            (Task::none(), picked)
        }
        Message::ResultClicked(i) => {
            let picked = state.results.get(i).and_then(|&idx| state.commands.get(idx)).map(|cmd| cmd.message.clone());
            state.visible = false;
            (Task::none(), picked)
        }
    }
}

pub fn view(state: &PaletteState) -> Element<'_, Message> {
    let input = text_input("Type a command...", &state.query)
        .id(query_input_id())
        .on_input(Message::QueryChanged)
        .on_submit(Message::Confirm)
        .padding(8)
        .width(Length::Fill);

    let mut results_col = column![].spacing(2);
    if state.results.is_empty() {
        results_col = results_col.push(text("No matching commands").size(13));
    }
    for (row, &i) in state.results.iter().enumerate() {
        let Some(cmd) = state.commands.get(i) else { continue };
        let is_selected = row == state.selected;
        results_col = results_col.push(
            button(text(cmd.label.clone()).size(13))
                .width(Length::Fill)
                .padding(6)
                .style(move |theme, status| result_style(theme, status, is_selected))
                .on_press(Message::ResultClicked(row)),
        );
    }

    let panel = container(
        column![input, scrollable(results_col).id(results_id()).height(Length::Fixed(320.0))].spacing(8),
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
