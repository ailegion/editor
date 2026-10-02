//! Markdown rendering for AI replies, with Copy and Insert-at-cursor on each code block.
//! Rendered text can be selected with the mouse and copied; see `ai_select.rs`.
use std::cell::Cell;

use iced::widget::{column, container, markdown, row, scrollable, text, Space};
use iced::{Element, Length};

use crate::ai_select::{SelectableText, Selection};

#[derive(Debug, Clone)]
pub enum Action {
    Link(String),
    Copy(String),
    /// Insert the code into the active editor tab at the caret, replacing any selection.
    Insert(String),
}

/// One parsed document per message, kept in step with the message text. Streaming replies
/// only grow, so appended text is parsed incrementally instead of from the start.
#[derive(Default)]
pub struct Cache {
    items: Vec<(String, markdown::Content)>,
    /// The text selected across these documents, if any.
    selection: Selection,
}

impl Cache {
    pub fn sync<'a>(&mut self, texts: impl ExactSizeIterator<Item = &'a str>) {
        // A selection is places in the rendered text; it survives text being added after
        // it, but not documents going away or being replaced.
        if texts.len() < self.items.len() { self.selection.clear(); }
        self.items.truncate(texts.len());
        for (i, text) in texts.enumerate() {
            match self.items.get_mut(i) {
                Some((cached, _)) if cached == text => {}
                Some((cached, content)) => {
                    match text.strip_prefix(cached.as_str()) {
                        Some(appended) => content.push_str(appended),
                        None => {
                            *content = markdown::Content::parse(text);
                            self.selection.clear();
                        }
                    }
                    *cached = text.to_string();
                }
                None => self.items.push((text.to_string(), markdown::Content::parse(text))),
            }
        }
    }

    pub fn get(&self, index: usize) -> Option<&markdown::Content> {
        self.items.get(index).map(|(_, content)| content)
    }
}

/// Message `index` of `cache`, rendered; `None` when the cache has not caught up with it.
pub fn view<'a, M: 'a>(cache: &'a Cache, index: usize, theme: &iced::Theme, on_action: impl Fn(Action) -> M + 'a) -> Option<Element<'a, M>> {
    Some(render(cache.get(index)?, theme, 13, Viewer::new(cache, index, true), on_action))
}

/// A whole Markdown file, as in the editor's preview. Code blocks offer Copy only: the
/// document they would be inserted into is the one being previewed.
pub fn view_document<'a, M: 'a>(cache: &'a Cache, theme: &iced::Theme, on_action: impl Fn(Action) -> M + 'a) -> Option<Element<'a, M>> {
    Some(render(cache.get(0)?, theme, 15, Viewer::new(cache, 0, false), on_action))
}

fn render<'a, M: 'a>(content: &'a markdown::Content, theme: &iced::Theme, text_size: u32, viewer: Viewer, on_action: impl Fn(Action) -> M + 'a) -> Element<'a, M> {
    let palette = theme.extended_palette();
    let mut style = markdown::Style::from_palette(theme.palette());
    style.inline_code_highlight = markdown::Highlight {
        background: palette.background.strong.color.into(),
        border: iced::Border::default().rounded(3),
    };
    style.inline_code_color = palette.background.base.text;
    let settings = markdown::Settings::with_text_size(text_size, style);
    Element::from(markdown::view_with(content.items(), settings, &viewer)).map(on_action)
}

struct Viewer {
    /// Show "Insert at cursor" on code blocks.
    insert: bool,
    selection: Selection,
    /// Which of the cache's documents this is, and how many of its text blocks have been
    /// built so far: together they order every block for the selection.
    document: u32,
    blocks: Cell<u32>,
}

impl Viewer {
    fn new(cache: &Cache, document: usize, insert: bool) -> Self {
        Self { insert, selection: cache.selection.clone(), document: document as u32, blocks: Cell::new(0) }
    }

    /// The next block of selectable text, in reading order.
    fn text<'a>(&self, text: &markdown::Text, settings: markdown::Settings, size: iced::Pixels) -> SelectableText<'a, Action> {
        let block = self.blocks.replace(self.blocks.get() + 1);
        SelectableText::new(text.spans(settings.style), size, (self.document, block), self.selection.clone(), Action::Link)
    }
}

impl<'a> markdown::Viewer<'a, Action> for Viewer {
    fn on_link_click(url: markdown::Uri) -> Action {
        Action::Link(url)
    }

    // The three kinds of text follow iced's own `markdown::heading`, `paragraph` and
    // `code_block`, with selectable text in place of `rich_text`. Lists, quotes and tables
    // are iced's: they lay out their contents through these.

    fn heading(&self, settings: markdown::Settings, level: &'a markdown::HeadingLevel, text: &'a markdown::Text, index: usize) -> Element<'a, Action> {
        use markdown::HeadingLevel::*;
        let size = match level {
            H1 => settings.h1_size,
            H2 => settings.h2_size,
            H3 => settings.h3_size,
            H4 => settings.h4_size,
            H5 => settings.h5_size,
            H6 => settings.h6_size,
        };
        let above = if index > 0 { settings.text_size / 2.0 } else { iced::Pixels::ZERO };
        container(self.text(text, settings, size)).padding(iced::padding::top(above)).into()
    }

    fn paragraph(&self, settings: markdown::Settings, text: &markdown::Text) -> Element<'a, Action> {
        self.text(text, settings, settings.text_size).into()
    }

    fn code_block(&self, settings: markdown::Settings, language: Option<&'a str>, code: &'a str, lines: &'a [markdown::Text]) -> Element<'a, Action> {
        let mut header = row![
            text(language.filter(|language| !language.is_empty()).unwrap_or("code")).size(11).style(text::secondary),
            Space::new().width(Length::Fill),
            crate::icon_control(lucide_icons::Icon::Copy, "Copy code", Some(Action::Copy(code.to_owned())), false),
        ].spacing(2).align_y(iced::Alignment::Center);
        if self.insert {
            header = header.push(crate::icon_control(lucide_icons::Icon::ArrowDownToLine, "Insert at cursor", Some(Action::Insert(code.to_owned())), false));
        }
        // Lines of one block are a line break apart when copied; the block as a whole is a
        // blank line from what precedes it, like any other.
        let lines = column(lines.iter().enumerate().map(|(index, line)| {
            self.text(line, settings, settings.code_size)
                .font(settings.style.code_block_font)
                .separator(if index == 0 { "\n\n" } else { "\n" })
                .into()
        }));
        let block = container(
            scrollable(container(lines).padding(settings.code_size))
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default().width(settings.code_size / 2).scroller_width(settings.code_size / 2),
                )),
        ).width(Length::Fill).padding(settings.code_size / 4).style(container::dark);
        column![header, block].spacing(2).into()
    }
}

/// Opens web links in the default browser; anything else is ignored.
pub fn open_link(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) { return; }
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_blocks(content: &markdown::Content) -> Vec<String> {
        content.items().iter().filter_map(|item| match item {
            markdown::Item::CodeBlock { code, .. } => Some(code.clone()),
            _ => None,
        }).collect()
    }

    #[test]
    fn streamed_and_replaced_text_parse_like_the_whole_reply() {
        let reply = "Use this:\n\n```rust\nfn main() {}\n```\n\nDone.";
        let mut cache = Cache::default();
        let mut streamed = String::new();
        for piece in reply.split_inclusive(['\n', ' ']) {
            streamed.push_str(piece);
            cache.sync([streamed.as_str()].into_iter());
        }
        let whole = markdown::Content::parse(reply);
        assert_eq!(code_blocks(cache.get(0).unwrap()), code_blocks(&whole));
        assert_eq!(code_blocks(&whole), ["fn main() {}\n"]);
        assert_eq!(cache.get(0).unwrap().items().len(), whole.items().len());
        // A different text (e.g. after switching conversations) is parsed from scratch.
        cache.sync(["# Title", ""].into_iter());
        assert!(matches!(cache.get(0).unwrap().items(), [markdown::Item::Heading(..)]));
        assert!(cache.get(1).unwrap().items().is_empty());
        cache.sync(std::iter::empty());
        assert!(cache.get(0).is_none());
    }

    #[test]
    fn a_selection_outlives_streaming_but_not_replaced_or_removed_text() {
        let mut cache = Cache::default();
        cache.sync(["First reply.", "Second"].into_iter());
        cache.selection.select((0, 0), (1, 0), 3);
        // The reply being streamed grows, and a new one arrives: what was selected is still there.
        cache.sync(["First reply.", "Second reply, longer."].into_iter());
        cache.sync(["First reply.", "Second reply, longer.", "Third"].into_iter());
        assert!(!cache.selection.is_empty());
        // Text that is not a continuation is a different document.
        cache.sync(["Another conversation.", "Second reply, longer.", "Third"].into_iter());
        assert!(cache.selection.is_empty());
        cache.selection.select((0, 0), (1, 0), 3);
        cache.sync(["Another conversation."].into_iter());
        assert!(cache.selection.is_empty());
    }
}
