//! Selectable, read-only message text for the AI panels.
//!
//! iced's `text` widget can't be selected, so messages are shown in a `text_editor` that
//! accepts only selection, caret movement and copy: every edit is dropped before it reaches
//! the content, and keys other than those bindings pass through to the rest of the app.
use iced::widget::text_editor::{self, Action, Binding, Content, KeyPress};
use iced::Element;

/// One `Content` per message, rebuilt whenever the message text changes.
#[derive(Default)]
pub struct Cache {
    items: Vec<(String, Content)>,
}

impl Cache {
    /// Mirrors `texts` so `get(i)` matches message `i`. Cheap when nothing changed: it
    /// compares strings and only reshapes messages whose text differs.
    pub fn sync<'a>(&mut self, texts: impl ExactSizeIterator<Item = &'a str>) {
        self.items.truncate(texts.len());
        for (i, text) in texts.enumerate() {
            match self.items.get_mut(i) {
                Some((cached, content)) if cached != text => {
                    *content = Content::with_text(text);
                    *cached = text.to_string();
                }
                Some(_) => {}
                None => self.items.push((text.to_string(), Content::with_text(text))),
            }
        }
    }

    pub fn get(&self, index: usize) -> Option<&Content> {
        self.items.get(index).map(|(_, content)| content)
    }

    /// Applies a selection or caret action; edits are ignored so the text stays as received.
    pub fn perform(&mut self, index: usize, action: Action) {
        if matches!(action, Action::Edit(_)) { return; }
        if let Some((_, content)) = self.items.get_mut(index) { content.perform(action); }
    }
}

/// The message text, selectable with the mouse and copyable with Cmd/Ctrl+C.
pub fn view<'a, M: Clone + 'a>(content: &'a Content, size: f32, on_action: impl Fn(Action) -> M + 'a) -> Element<'a, M> {
    iced::widget::text_editor(content)
        .size(size)
        .padding(0)
        .on_action(on_action)
        .key_binding(key_binding)
        .style(|theme: &iced::Theme, _status| {
            let palette = theme.extended_palette();
            text_editor::Style {
                background: iced::Color::TRANSPARENT.into(),
                border: iced::Border::default(),
                placeholder: palette.background.strong.color,
                value: palette.background.base.text,
                selection: palette.primary.weak.color,
            }
        })
        .into()
}

/// Only selection, movement and copy; anything else is left for the app's own shortcuts.
fn key_binding<M>(key_press: KeyPress) -> Option<Binding<M>> {
    match Binding::from_key_press(key_press)? {
        binding @ (Binding::Copy | Binding::SelectAll | Binding::SelectWord | Binding::SelectLine
            | Binding::Select(_) | Binding::Move(_) | Binding::Unfocus) => Some(binding),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_tracks_messages_and_drops_edits() {
        let mut cache = Cache::default();
        cache.sync(["hello", "world"].into_iter());
        assert_eq!(cache.get(1).unwrap().text(), "world");
        cache.perform(0, Action::Edit(text_editor::Edit::Insert('x')));
        cache.perform(0, Action::SelectAll);
        assert_eq!(cache.get(0).unwrap().text(), "hello");
        assert_eq!(cache.get(0).unwrap().selection().as_deref(), Some("hello"));
        cache.sync(["hello", "world!"].into_iter());
        assert_eq!(cache.get(1).unwrap().text(), "world!");
        assert_eq!(cache.get(0).unwrap().selection().as_deref(), Some("hello"), "unchanged text keeps its selection");
        cache.sync(["only"].into_iter());
        assert!(cache.get(1).is_none());
    }
}
