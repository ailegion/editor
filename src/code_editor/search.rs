//! Find/replace against our own buffer, with our own styled UI.

use cosmic_text::Cursor;
use iced::widget::{button, row, text, text_input};
use iced::{Element, Length};
use regex::{Regex, RegexBuilder};

use super::buffer::Buffer;

#[derive(Default)]
pub struct SearchState {
    pub visible: bool,
    pub query: String,
    pub replace: String,
    pub regex_mode: bool,
    matches: Vec<(Cursor, Cursor)>,
    current: usize,
    /// Set when `regex_mode` is on and `query` fails to compile, so `view` can say so
    /// instead of the misleading "No results" (which normally means "compiled fine, matched
    /// nothing").
    regex_error: bool,
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
    ToggleRegex,
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
        Message::ToggleRegex => {
            state.regex_mode = !state.regex_mode;
            refresh(state, buffer);
        }
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
                let replacement = replacement_for(state, buffer, start, end);
                buffer.replace_range(start, end, &replacement);
                refresh(state, buffer);
            }
        }
        Message::ReplaceAll => {
            // Replace back-to-front so earlier matches' byte offsets stay valid as later
            // ones (which come after them in the document) are rewritten.
            for (start, end) in state.matches.clone().into_iter().rev() {
                let replacement = replacement_for(state, buffer, start, end);
                buffer.replace_range(start, end, &replacement);
            }
            refresh(state, buffer);
        }
    }
}

fn refresh(state: &mut SearchState, buffer: &mut Buffer) {
    let (matches, regex_error) = find_matches(buffer, &state.query, state.regex_mode);
    state.matches = matches;
    state.regex_error = regex_error;
    state.current = 0;
    select_current(state, buffer);
}

fn select_current(state: &SearchState, buffer: &mut Buffer) {
    if let Some((start, end)) = state.current_match() {
        buffer.select_range(start, end);
    }
}

fn compiled_regex(query: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(query).case_insensitive(true).build()
}

/// The text to substitute in for the match at `start..end`: the replace field verbatim in
/// plain-text mode, or -- in regex mode -- that field expanded against the match's capture
/// groups (`$1`, `$name`, ...), by re-running the query against just this match's own span
/// (cheap, and exactly reproduces the original match since it's the same regex over the same
/// text). Anchors like `^`/`$` that depend on absolute position within the line are the one
/// case this re-match can disagree with the original -- an accepted, rare edge case.
fn replacement_for(state: &SearchState, buffer: &Buffer, start: Cursor, end: Cursor) -> String {
    if !state.regex_mode {
        return state.replace.clone();
    }
    let Ok(re) = compiled_regex(&state.query) else {
        return state.replace.clone();
    };
    let Some(line) = buffer.inner.lines.get(start.line) else {
        return state.replace.clone();
    };
    let text = line.text();
    let Some(caps) = re.captures(&text[start.index..end.index]) else {
        return state.replace.clone();
    };
    let mut expanded = String::new();
    caps.expand(&state.replace, &mut expanded);
    expanded
}

/// Line-by-line search (no multi-line/`\n`-spanning matches, in either mode -- keeps the
/// match model a flat list of same-line `(Cursor, Cursor)` spans). Plain-text mode is a
/// case-insensitive substring search; regex mode compiles `query` (case-insensitive) and
/// runs it against each line. Returns `(matches, regex_compile_failed)`.
fn find_matches(buffer: &Buffer, query: &str, regex_mode: bool) -> (Vec<(Cursor, Cursor)>, bool) {
    if query.is_empty() {
        return (Vec::new(), false);
    }

    if regex_mode {
        return match compiled_regex(query) {
            Ok(re) => {
                let mut matches = Vec::new();
                for (line_i, line) in buffer.inner.lines.iter().enumerate() {
                    for m in re.find_iter(line.text()) {
                        matches.push((Cursor::new(line_i, m.start()), Cursor::new(line_i, m.end())));
                    }
                }
                (matches, false)
            }
            Err(_) => (Vec::new(), true),
        };
    }

    // Matching is done by lowercasing each line and comparing byte offsets directly against
    // the original -- fine for ASCII source text, but non-ASCII characters whose lowercase
    // form has a different byte length would throw off the reported match range.
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
    (matches, false)
}

pub fn view(state: &SearchState) -> Element<'_, Message> {
    let match_label = if state.query.is_empty() {
        String::new()
    } else if state.regex_error {
        "Invalid regex".to_string()
    } else if state.matches.is_empty() {
        "No results".to_string()
    } else {
        format!("{}/{}", state.current + 1, state.matches.len())
    };

    let regex_toggle = button(text(".*").size(13))
        .padding([4, 8])
        .style(move |theme: &iced::Theme, status| {
            use iced::widget::button::{Status, Style};
            let palette = theme.extended_palette();
            let base = Style {
                text_color: palette.background.base.text,
                border: iced::Border::default().rounded(4.0),
                ..Style::default()
            };
            if state.regex_mode {
                return base.with_background(palette.primary.weak.color);
            }
            match status {
                Status::Active | Status::Disabled => base.with_background(iced::Color::TRANSPARENT),
                Status::Hovered => base.with_background(palette.background.weak.color),
                Status::Pressed => base.with_background(palette.background.strong.color),
            }
        })
        .on_press(Message::ToggleRegex);

    row![
        text_input("Find", &state.query)
            .on_input(Message::QueryChanged)
            .width(Length::FillPortion(2)),
        regex_toggle,
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
