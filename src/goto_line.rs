//! Go to Line (Cmd+G): jump the active tab's cursor to a specific 1-indexed line number.
//!
//! Much simpler than `quick_open`/`command_palette` (no fuzzy list to filter or navigate),
//! but reuses the same overlay presentation (`stack!` + `mouse_area` backdrop, centered
//! panel, autofocused input) for visual consistency.

use iced::widget::{column, container, mouse_area, text, text_input, Space};
use iced::{Element, Length, Task};

pub fn query_input_id() -> iced::widget::Id {
    iced::widget::Id::new("goto-line-query")
}

#[derive(Default)]
pub struct GotoLineState {
    pub visible: bool,
    query: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
    QueryChanged(String),
    Confirm,
}

/// Opens the panel with an empty query, focusing it.
pub fn open(state: &mut GotoLineState) -> Task<Message> {
    state.visible = true;
    state.query.clear();
    iced::widget::operation::focus(query_input_id())
}

/// Applies `message`, returning the 1-indexed line number to jump to (if any) for the
/// caller to apply via `cosmic_text::Motion::GotoLine`.
pub fn update(state: &mut GotoLineState, message: Message) -> Option<usize> {
    match message {
        Message::Close => {
            state.visible = false;
            None
        }
        Message::QueryChanged(query) => {
            // Digits only -- there's no dedicated numeric-input widget here, so filter by
            // hand rather than validating/rejecting on submit.
            state.query = query.chars().filter(|c| c.is_ascii_digit()).collect();
            None
        }
        Message::Confirm => {
            let line = state.query.trim().parse::<usize>().ok().filter(|&n| n >= 1);
            if line.is_some() {
                state.visible = false;
            }
            line
        }
    }
}

pub fn view(state: &GotoLineState, line_count: usize) -> Element<'_, Message> {
    let input = text_input("Line number...", &state.query)
        .id(query_input_id())
        .on_input(Message::QueryChanged)
        .on_submit(Message::Confirm)
        .padding(8)
        .width(Length::Fill);

    let panel = container(
        column![input, text(format!("Go to line (1-{line_count})")).size(12)].spacing(8),
    )
    .padding(12)
    .width(Length::Fixed(320.0))
    .style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        iced::widget::container::Style {
            background: Some(palette.background.base.color.into()),
            border: iced::Border::default().rounded(8.0).color(palette.background.strong.color).width(1.0),
            ..iced::widget::container::Style::default()
        }
    });

    let backdrop = container(Space::new().width(Length::Fill).height(Length::Fill)).style(
        |theme: &iced::Theme| iced::widget::container::Style {
            background: Some(
                iced::Color { a: 0.4, ..theme.extended_palette().background.base.color.inverse() }.into(),
            ),
            ..iced::widget::container::Style::default()
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
