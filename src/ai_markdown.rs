//! Markdown rendering for AI replies, with Copy and Insert-at-cursor on each code block.
use iced::widget::{column, markdown, row, text, Space};
use iced::{Element, Length};

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
}

impl Cache {
    pub fn sync<'a>(&mut self, texts: impl ExactSizeIterator<Item = &'a str>) {
        self.items.truncate(texts.len());
        for (i, text) in texts.enumerate() {
            match self.items.get_mut(i) {
                Some((cached, _)) if cached == text => {}
                Some((cached, content)) => {
                    match text.strip_prefix(cached.as_str()) {
                        Some(appended) => content.push_str(appended),
                        None => *content = markdown::Content::parse(text),
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

pub fn view<'a, M: 'a>(content: &'a markdown::Content, theme: &iced::Theme, on_action: impl Fn(Action) -> M + 'a) -> Element<'a, M> {
    render(content, theme, 13, Viewer { insert: true }, on_action)
}

/// A whole Markdown file, as in the editor's preview. Code blocks offer Copy only: the
/// document they would be inserted into is the one being previewed.
pub fn view_document<'a, M: 'a>(content: &'a markdown::Content, theme: &iced::Theme, on_action: impl Fn(Action) -> M + 'a) -> Element<'a, M> {
    render(content, theme, 15, Viewer { insert: false }, on_action)
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
}

impl<'a> markdown::Viewer<'a, Action> for Viewer {
    fn on_link_click(url: markdown::Uri) -> Action {
        Action::Link(url)
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
        column![header, markdown::code_block(settings, lines, Action::Link)].spacing(2).into()
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
}
