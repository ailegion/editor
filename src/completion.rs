//! Code completion in the editor: the suggestion list under the cursor, narrowed as the word
//! grows, and the edits applied when one is accepted. Suggestions come from the language
//! server (`lsp::Manager::completion`); `main.rs` decides when to ask and routes the keys.

use std::path::PathBuf;

use cosmic_text::Cursor;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use iced::widget::{button, column, container, row, text, Space};
use iced::{Element, Length};

use crate::code_editor::Buffer;
use crate::lsp::{self, CompletionItem};

/// Rows shown at once; the list scrolls with the selection.
const VISIBLE: usize = 10;
const WIDTH: f32 = 460.0;

pub fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Byte index where the word ending at byte `index` of `text` starts.
pub fn word_start(text: &str, index: usize) -> usize {
    let index = index.min(text.len());
    text[..index].char_indices().rev().take_while(|(_, ch)| is_word_char(*ch)).last().map_or(index, |(start, _)| start)
}

/// The suggestions for one position, and which of them match what has been typed since.
pub struct Session {
    pub path: PathBuf,
    pub line: usize,
    /// Byte index the server was asked about, and where the word being completed starts.
    request: usize,
    word: usize,
    items: Vec<CompletionItem>,
    /// The server wants to be asked again as the word grows.
    pub incomplete: bool,
    /// Indices into `items` matching the typed word, best first.
    shown: Vec<usize>,
    selected: usize,
}

impl Session {
    pub fn new(path: PathBuf, line: usize, request: usize, word: usize, items: Vec<CompletionItem>, incomplete: bool) -> Self {
        Self { path, line, request, word, items, incomplete, shown: Vec::new(), selected: 0 }
    }

    /// Whether the cursor is still in the word this list is for.
    pub fn at_cursor(&self, buffer: &Buffer) -> bool {
        let cursor = buffer.cursor;
        cursor.line == self.line && cursor.index >= self.word
            && buffer.inner.lines.get(self.line)
                .and_then(|line| line.text().get(self.word..cursor.index))
                .is_some_and(|typed| typed.chars().all(is_word_char))
    }

    /// Narrows the list to what is typed now. `false` when it should close: the cursor left
    /// the word, or nothing matches.
    pub fn refilter(&mut self, buffer: &Buffer) -> bool {
        if !self.at_cursor(buffer) { return false; }
        let typed = &buffer.inner.lines[self.line].text()[self.word..buffer.cursor.index];
        self.shown = filter(&self.items, typed);
        self.selected = 0;
        !self.shown.is_empty()
    }

    /// Moves the selection by `delta` rows, wrapping around.
    pub fn select(&mut self, delta: isize) {
        let len = self.shown.len() as isize;
        if len > 0 { self.selected = (self.selected as isize + delta).rem_euclid(len) as usize; }
    }

    pub fn selected_item(&self) -> Option<usize> { self.shown.get(self.selected).copied() }

    /// Buffer edits that accept item `index`, the main replacement first, then edits elsewhere
    /// (imports). Positions are mapped from the request's text to the current one: the user
    /// may have typed more of the word since.
    pub fn edits(&self, index: usize, buffer: &Buffer) -> Option<Vec<(Cursor, Cursor, String)>> {
        let item = self.items.get(index)?;
        let cursor = buffer.cursor;
        let text_of = |line: usize| buffer.inner.lines.get(line).map(|line| line.text());
        let current = text_of(self.line)?;
        // Text up to the request position is unchanged; what was typed since sits after it.
        let request_column = lsp::byte_to_utf16(current, self.request);
        let to_cursor = |(line, column): (usize, usize)| -> Option<Cursor> {
            let text = text_of(line)?;
            if line == self.line && column > request_column {
                let rest = text.get(cursor.index..)?;
                return Some(Cursor::new(line, cursor.index + lsp::utf16_to_byte(rest, column - request_column)));
            }
            Some(Cursor::new(line, lsp::utf16_to_byte(text, column)))
        };
        let main = match &item.edit {
            // The server's range ends where it was asked; extend it over what was typed since.
            Some(edit) if edit.end == (self.line, request_column) => (to_cursor(edit.start)?, cursor, edit.text.clone()),
            Some(edit) => (to_cursor(edit.start)?, to_cursor(edit.end)?, edit.text.clone()),
            None => (Cursor::new(self.line, self.word), cursor, item.insert.clone()),
        };
        let mut edits = vec![main];
        for edit in &item.additional {
            edits.push((to_cursor(edit.start)?, to_cursor(edit.end)?, edit.text.clone()));
        }
        Some(edits)
    }
}

/// Items matching `typed` (fuzzy, like Quick Open), best match first and the server's order
/// among equals; all items in the server's order when nothing is typed yet.
pub fn filter(items: &[CompletionItem], typed: &str) -> Vec<usize> {
    if typed.is_empty() {
        let mut all: Vec<usize> = (0..items.len()).collect();
        all.sort_by(|a, b| items[*a].sort.cmp(&items[*b].sort));
        return all;
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, usize)> = items.iter().enumerate()
        .filter_map(|(index, item)| matcher.fuzzy_match(&item.filter, typed).map(|score| (score, index)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| items[a.1].sort.cmp(&items[b.1].sort)));
    scored.into_iter().map(|(_, index)| index).collect()
}

/// Short label for an LSP `CompletionItemKind`.
fn kind_label(kind: Option<u64>) -> &'static str {
    match kind {
        Some(2) => "method", Some(3) => "fn", Some(4) => "new", Some(5) => "field", Some(6) => "var",
        Some(7) => "class", Some(8) => "iface", Some(9) => "mod", Some(10) => "prop", Some(13) => "enum",
        Some(14) => "kw", Some(15) => "snip", Some(17) => "file", Some(19) => "dir", Some(20) => "variant",
        Some(21) => "const", Some(22) => "struct", Some(25) => "type",
        _ => "",
    }
}

/// The list as a popup whose top-left sits at `anchor` (canvas pixels below the cursor).
pub fn view<'a, M: Clone + 'a>(session: &'a Session, anchor: iced::Point, on_pick: impl Fn(usize) -> M) -> Element<'a, M> {
    let secondary = iced::widget::text::secondary;
    let start = session.selected.saturating_sub(VISIBLE - 1).min(session.shown.len().saturating_sub(VISIBLE));
    let mut rows = column![];
    for (offset, &index) in session.shown.iter().enumerate().skip(start).take(VISIBLE) {
        let item = &session.items[index];
        let selected = offset == session.selected;
        let line = row![
            text(kind_label(item.kind)).size(11).width(48).style(secondary),
            text(item.label.clone()).size(13).font(iced::Font::MONOSPACE).wrapping(iced::widget::text::Wrapping::None),
            Space::new().width(Length::Fill),
            text(item.detail.clone().unwrap_or_default()).size(11).style(secondary).wrapping(iced::widget::text::Wrapping::None),
        ].spacing(8).align_y(iced::Alignment::Center);
        rows = rows.push(button(container(line).clip(true)).width(Length::Fill).padding([3, 8])
            .style(move |theme: &iced::Theme, status| {
                let mut style = crate::flat_button_style(theme, status);
                if selected {
                    let palette = theme.extended_palette();
                    style.background = Some(palette.primary.weak.color.into());
                    style.text_color = palette.primary.weak.text;
                }
                style
            })
            .on_press(on_pick(index)));
    }
    let popup = container(rows).width(WIDTH).padding(3).style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 6.0.into() },
            shadow: iced::Shadow { color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.3), offset: iced::Vector::new(0.0, 2.0), blur_radius: 8.0 },
            ..Default::default()
        }
    });
    container(popup).padding(iced::Padding { top: anchor.y.max(0.0) + 2.0, left: anchor.x.max(0.0), right: 0.0, bottom: 0.0 }).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::TextEdit;
    use cosmic_text::Metrics;

    fn item(label: &str, sort: &str) -> CompletionItem {
        CompletionItem { label: label.into(), kind: None, detail: None, filter: label.into(), sort: sort.into(), edit: None, insert: label.into(), additional: Vec::new() }
    }

    fn buffer_at(text: &str, line: usize, index: usize) -> Buffer {
        let mut buffer = Buffer::new(text, Metrics::new(14.0, 20.0));
        buffer.goto(line, index);
        buffer
    }

    #[test]
    fn words_are_found_and_filtered_best_first() {
        assert_eq!(word_start("let vec_le", 10), 4);
        assert_eq!(word_start("a.", 2), 2, "nothing typed after a trigger character");
        assert_eq!(word_start("é_x", "é_x".len()), 0);
        let items = [item("push", "2"), item("pop", "1"), item("push_str", "3"), item("len", "0")];
        assert_eq!(filter(&items, ""), [3, 1, 0, 2], "server order before anything is typed");
        let pushes = filter(&items, "pu");
        assert_eq!(pushes.len(), 2);
        assert!(pushes.contains(&0) && pushes.contains(&2));
        assert_eq!(filter(&items, "zzz"), Vec::<usize>::new());
    }

    #[test]
    fn session_follows_typing_and_closes_when_the_cursor_leaves_the_word() {
        let items = vec![item("push", "1"), item("pop", "2"), item("len", "3")];
        let mut session = Session::new(PathBuf::from("a.rs"), 0, 2, 2, items, false);
        assert!(session.refilter(&buffer_at("v.p", 0, 3)));
        assert_eq!(session.shown.len(), 2);
        session.select(-1);
        assert_eq!(session.selected, 1, "wraps to the last row");
        session.select(1);
        assert_eq!(session.selected, 0);
        assert!(!session.refilter(&buffer_at("v.pq", 0, 4)), "nothing matches");
        assert!(!session.refilter(&buffer_at("v.p ", 0, 4)), "a space ends the word");
        assert!(!session.refilter(&buffer_at("v.p\nx", 1, 1)), "another line");
        assert!(!session.refilter(&buffer_at("v.p", 0, 1)), "before the word");
    }

    #[test]
    fn accepting_replaces_the_typed_word_and_adds_imports() {
        // Asked at `v.pu|` (UTF-16 column 4), then the user typed `s` before accepting.
        let mut push = item("push", "1");
        push.edit = Some(TextEdit { start: (1, 2), end: (1, 4), text: "push".into() });
        push.additional = vec![TextEdit { start: (0, 0), end: (0, 0), text: "use std::vec;\n".into() }];
        let session = Session::new(PathBuf::from("a.rs"), 1, 4, 2, vec![push, item("pop", "2")], false);
        let mut buffer = buffer_at("fn main() {}\nv.pus();", 1, 5);
        let edits = session.edits(0, &buffer).unwrap();
        assert_eq!(edits[0], (Cursor::new(1, 2), Cursor::new(1, 5), "push".into()), "covers what was typed after the request");
        buffer.replace_ranges(&edits, 0);
        assert_eq!(buffer.text(), "use std::vec;\nfn main() {}\nv.push();");
        assert_eq!((buffer.cursor.line, buffer.cursor.index), (2, 6), "after the completion, moved down by the import");
        buffer.undo();
        assert_eq!(buffer.text(), "fn main() {}\nv.pus();", "one undo step");

        // Without a server range, the word itself is replaced.
        let mut buffer = buffer_at("x = po", 0, 6);
        let session = Session::new(PathBuf::from("a.py"), 0, 6, 4, vec![item("pop", "1")], false);
        let edits = session.edits(0, &buffer).unwrap();
        buffer.replace_ranges(&edits, 0);
        assert_eq!(buffer.text(), "x = pop");
        assert_eq!(buffer.cursor.index, 7);
    }

    #[test]
    fn server_ranges_past_the_request_shift_with_what_was_typed() {
        // Replace-mode range: asked at `ab|cd` (column 2) for the whole word `abcd`; the user
        // then typed `x`, so the word is `abx|cd` and the range end moves one to the right.
        let mut word = item("abcdef", "1");
        word.edit = Some(TextEdit { start: (0, 0), end: (0, 4), text: "abcdef".into() });
        let session = Session::new(PathBuf::from("a.ts"), 0, 2, 0, vec![word], false);
        let buffer = buffer_at("abxcd", 0, 3);
        assert_eq!(session.edits(0, &buffer).unwrap()[0], (Cursor::new(0, 0), Cursor::new(0, 5), "abcdef".into()));
    }
}
