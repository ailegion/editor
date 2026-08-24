//! Find/replace against our own buffer, with our own styled UI.

use cosmic_text::Cursor;
use iced::widget::{button, row, text, text_input};
use iced::{Element, Length};

use super::buffer::Buffer;

#[derive(Default)]
pub struct SearchState {
    pub visible: bool,
    pub query: String,
    pub replace: String,
    matches: Vec<(Cursor, Cursor)>,
    current: usize,
}

impl SearchState {
    fn current_match(&self) -> Option<(Cursor, Cursor)> {
        self.matches.get(self.current).copied()
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Toggle,
    Close,
    QueryChanged(String),
    ReplaceChanged(String),
    Next,
    Previous,
    ReplaceOne,
    ReplaceAll,
}

pub fn update(state: &mut SearchState, buffer: &mut Buffer, message: Message) {
    match message {
        Message::Toggle => {
            state.visible = !state.visible;
            if state.visible {
                refresh(state, buffer);
            }
        }
        Message::Close => state.visible = false,
        Message::QueryChanged(query) => {
            state.query = query;
            refresh(state, buffer);
        }
        Message::ReplaceChanged(replace) => state.replace = replace,
        Message::Next => {
            if !state.matches.is_empty() {
                state.current = (state.current + 1) % state.matches.len();
                select_current(state, buffer);
            }
        }
        Message::Previous => {
            if !state.matches.is_empty() {
                state.current = (state.current + state.matches.len() - 1) % state.matches.len();
                select_current(state, buffer);
            }
        }
        Message::ReplaceOne => {
            if let Some((start, end)) = state.current_match() {
                buffer.replace_range(start, end, &state.replace);
                refresh(state, buffer);
            }
        }
        Message::ReplaceAll => {
            // Replace back-to-front so earlier matches' byte offsets stay valid as later
            // ones (which come after them in the document) are rewritten.
            for (start, end) in state.matches.clone().into_iter().rev() {
                buffer.replace_range(start, end, &state.replace);
            }
            refresh(state, buffer);
        }
    }
}

fn refresh(state: &mut SearchState, buffer: &mut Buffer) {
    state.matches = find_matches(buffer, &state.query);
    state.current = 0;
    select_current(state, buffer);
}

fn select_current(state: &SearchState, buffer: &mut Buffer) {
    if let Some((start, end)) = state.current_match() {
        buffer.select_range(start, end);
    }
}

/// Case-insensitive substring search, line by line. Matching is done by lowercasing each
/// line and comparing byte offsets directly against the original -- fine for ASCII source
/// text, but non-ASCII characters whose lowercase form has a different byte length would
/// throw off the reported match range.
fn find_matches(buffer: &Buffer, query: &str) -> Vec<(Cursor, Cursor)> {
    if query.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();

    let mut matches = Vec::new();
    for (line_i, line) in buffer.inner.lines.iter().enumerate() {
        let text = line.text();
        let text_lower = text.to_lowercase();
        let mut search_from = 0;
        while let Some(found) = text_lower[search_from..].find(&query_lower) {
            let start = search_from + found;
            let end = start + query.len();
            matches.push((Cursor::new(line_i, start), Cursor::new(line_i, end)));
            search_from = end.max(start + 1);
            if search_from > text.len() {
                break;
            }
        }
    }
    matches
}

pub fn view(state: &SearchState) -> Element<'_, Message> {
    let match_label = if state.query.is_empty() {
        String::new()
    } else if state.matches.is_empty() {
        "No results".to_string()
    } else {
        format!("{}/{}", state.current + 1, state.matches.len())
    };

    row![
        text_input("Find", &state.query)
            .on_input(Message::QueryChanged)
            .width(Length::FillPortion(2)),
        text(match_label),
        button(text("<")).on_press(Message::Previous),
        button(text(">")).on_press(Message::Next),
        text_input("Replace", &state.replace)
            .on_input(Message::ReplaceChanged)
            .width(Length::FillPortion(2)),
        button(text("Replace")).on_press(Message::ReplaceOne),
        button(text("Replace All")).on_press(Message::ReplaceAll),
        button(text("x")).on_press(Message::Close),
    ]
    .spacing(4)
    .padding(4)
    .into()
}
