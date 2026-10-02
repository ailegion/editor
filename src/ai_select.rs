//! Rich text that can be selected with the mouse and copied, for rendered Markdown.
//!
//! iced's `rich_text` has no selection. This is the same kind of widget -- styled spans laid
//! out as one paragraph, links that can be clicked -- with a selection that runs across as
//! many of them as the mouse is dragged over: every block of a document shares one
//! [`Selection`], and each draws and copies the part of it that falls on its own text.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::mouse::click::{Click, Kind};
use iced::advanced::text::{self, Paragraph as _, Span};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{layout, mouse, renderer, Clipboard, Layout, Renderer as _, Shell, Widget};
use iced::{keyboard, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Vector};

/// Where a block sits in reading order: the document it belongs to, then its place in it.
pub type Id = (u32, u32);

/// A place in a block's text: the line, and the byte within that line.
type Place = (usize, usize);

/// The selection shared by every block of the documents shown together (the replies of a
/// conversation, or a previewed file).
#[derive(Clone, Default)]
pub struct Selection(Arc<Mutex<Shared>>);

impl Selection {
    /// Drops the selection, e.g. when the text it was made in is replaced.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Selects from the start of block `from` to byte `to` of the first line of block `until`.
    #[cfg(test)]
    pub fn select(&self, from: Id, until: Id, to: usize) {
        self.lock().select((from, (0, 0)), (until, (0, to)));
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.lock().ends().is_none()
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Default)]
struct Shared {
    /// Where the selection started and where it currently ends, in either order.
    anchor: Option<(Id, Place)>,
    head: Option<(Id, Place)>,
    /// The mouse button is down after a press on a block, so moving extends the selection.
    dragging: bool,
    /// The selection was started by the mouse press still being delivered. Every block sees
    /// that press, in no particular order, and those it missed must not clear what the one it
    /// hit just began.
    fresh: bool,
    /// What each block last contributed to a copy, with the separator that goes before it.
    copied: BTreeMap<Id, (&'static str, String)>,
}

impl Shared {
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// A press landed on a block at `at`, or (`None`) somewhere that is not this block.
    fn press(&mut self, at: Option<(Id, Place)>) {
        match at {
            Some(at) => {
                self.select(at, at);
                self.dragging = true;
            }
            None if !self.fresh => self.clear(),
            None => {}
        }
    }

    /// Selects `from..to` outright, as a double or triple click does.
    fn select(&mut self, from: (Id, Place), to: (Id, Place)) {
        self.anchor = Some(from);
        self.head = Some(to);
        self.dragging = false;
        self.fresh = true;
        self.copied.clear();
    }

    fn release(&mut self) {
        self.dragging = false;
        self.fresh = false;
    }

    /// The mouse moved during a drag and `at` is the nearest place to it in block `id`.
    /// A block takes the end of the selection when the mouse is `inside` it; the block that
    /// already holds the end keeps following a mouse that has left it, so the selection
    /// reaches the block's edge. Returns whether the selection changed.
    fn drag(&mut self, id: Id, inside: bool, at: Place) -> bool {
        let holds_end = self.head.is_some_and(|(head, _)| head == id);
        if !self.dragging || !(inside || holds_end) || self.head == Some((id, at)) {
            return false;
        }
        self.head = Some((id, at));
        self.copied.clear();
        true
    }

    /// Both ends in reading order; `None` when nothing is selected.
    fn ends(&self) -> Option<((Id, Place), (Id, Place))> {
        let (anchor, head) = (self.anchor?, self.head?);
        (anchor != head).then(|| if anchor <= head { (anchor, head) } else { (head, anchor) })
    }

    /// The part of block `id` that is selected, given the place its text ends at.
    fn range(&self, id: Id, last: Place) -> Option<(Place, Place)> {
        let (start, end) = self.ends()?;
        if id < start.0 || id > end.0 {
            return None;
        }
        let from = if id == start.0 { start.1 } else { (0, 0) };
        let to = if id == end.0 { end.1.min(last) } else { last };
        (from < to).then_some((from, to))
    }

    /// Records block `id`'s selected text and returns everything copied so far, in reading
    /// order. Each selected block does this for the same key press, so the last one to run
    /// leaves the whole selection on the clipboard.
    fn copy(&mut self, id: Id, separator: &'static str, text: String) -> String {
        self.copied.insert(id, (separator, text));
        let mut all = String::new();
        for (index, (separator, text)) in self.copied.values().enumerate() {
            if index > 0 { all.push_str(separator); }
            all.push_str(text);
        }
        all
    }
}

/// The text from `from` to `to` across `lines`.
fn slice(lines: &[&str], from: Place, to: Place) -> String {
    let mut text = String::new();
    for line in from.0..=to.0.min(lines.len().saturating_sub(1)) {
        let Some(content) = lines.get(line) else { break };
        let start = if line == from.0 { from.1.min(content.len()) } else { 0 };
        let end = if line == to.0 { to.1.min(content.len()) } else { content.len() };
        if line > from.0 { text.push('\n'); }
        text.push_str(content.get(start..end).unwrap_or(""));
    }
    text
}

/// The bytes of the word around `index` in `line`: a run of letters, digits and
/// underscores, or the single character there when it is none of those.
fn word_at(line: &str, index: usize) -> (usize, usize) {
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let Some((at, ch)) = line.char_indices().rev().find(|(at, _)| *at <= index) else { return (0, 0) };
    if !is_word(ch) {
        return (at, at + ch.len_utf8());
    }
    let start = line[..at].char_indices().rev().take_while(|(_, ch)| is_word(*ch)).last().map_or(at, |(start, _)| start);
    let end = at + line[at..].char_indices().find(|(_, ch)| !is_word(*ch)).map_or(line.len() - at, |(end, _)| end);
    (start, end)
}

/// One block of selectable rich text.
pub struct SelectableText<'a, Message> {
    spans: Arc<[Span<'static, String>]>,
    size: Pixels,
    font: Option<Font>,
    id: Id,
    separator: &'static str,
    selection: Selection,
    on_link: Box<dyn Fn(String) -> Message + 'a>,
}

impl<'a, Message> SelectableText<'a, Message> {
    /// `id` places the block among the others sharing `selection`; `on_link` is the message
    /// for a clicked link.
    pub fn new(spans: Arc<[Span<'static, String>]>, size: impl Into<Pixels>, id: Id, selection: Selection, on_link: impl Fn(String) -> Message + 'a) -> Self {
        Self { spans, size: size.into(), font: None, id, separator: "\n\n", selection, on_link: Box::new(on_link) }
    }

    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    /// What goes between the previous block's text and this one's when both are copied.
    pub fn separator(mut self, separator: &'static str) -> Self {
        self.separator = separator;
        self
    }
}

struct State {
    spans: Arc<[Span<'static, String>]>,
    paragraph: Paragraph,
    hovered_link: Option<usize>,
    pressed_link: Option<usize>,
    /// Previous left click, so the next press can be recognized as a double or triple click.
    last_click: Option<Click>,
}

impl State {
    fn lines(&self) -> Vec<&str> {
        self.paragraph.buffer().lines.iter().map(|line| line.text()).collect()
    }

    /// Where the text ends.
    fn last(&self) -> Place {
        let lines = &self.paragraph.buffer().lines;
        (lines.len().saturating_sub(1), lines.last().map_or(0, |line| line.text().len()))
    }

    /// The place nearest `point` (relative to the block's top left).
    fn hit(&self, point: Point) -> Place {
        self.paragraph.buffer().hit(point.x, point.y).map_or((0, 0), |cursor| (cursor.line, cursor.index))
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for SelectableText<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            spans: Arc::new([]),
            paragraph: Paragraph::default(),
            hovered_link: None,
            pressed_link: None,
            last_click: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size { width: Length::Shrink, height: Length::Shrink }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        use text::Renderer as _;
        let state = tree.state.downcast_mut::<State>();
        let font = self.font.unwrap_or_else(|| renderer.default_font());
        layout::sized(limits, Length::Shrink, Length::Shrink, |limits| {
            let bounds = limits.max();
            let unchanged = Arc::ptr_eq(&state.spans, &self.spans) || state.spans == self.spans;
            let shape = !unchanged || match state.paragraph.compare(text::Text {
                content: (),
                bounds,
                size: self.size,
                line_height: text::LineHeight::default(),
                font,
                align_x: text::Alignment::Default,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::default(),
            }) {
                text::Difference::None => false,
                text::Difference::Bounds => {
                    state.paragraph.resize(bounds);
                    false
                }
                text::Difference::Shape => true,
            };
            if shape {
                state.paragraph = Paragraph::with_spans(text::Text {
                    content: self.spans.as_ref(),
                    bounds,
                    size: self.size,
                    line_height: text::LineHeight::default(),
                    font,
                    align_x: text::Alignment::Default,
                    align_y: iced::alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::default(),
                });
                state.spans = self.spans.clone();
            }
            state.paragraph.min_bounds()
        })
    }

    fn draw(&self, tree: &Tree, renderer: &mut iced::Renderer, theme: &iced::Theme, defaults: &renderer::Style, layout: Layout<'_>, _cursor: mouse::Cursor, viewport: &Rectangle) {
        let bounds = layout.bounds();
        if !bounds.intersects(viewport) {
            return;
        }
        let state = tree.state.downcast_ref::<State>();
        let translation = bounds.position() - Point::ORIGIN;

        // Span backgrounds (inline code) and lines (links under the mouse, strikethrough),
        // as `rich_text` draws them.
        for (index, span) in self.spans.iter().enumerate() {
            let hovered = Some(index) == state.hovered_link;
            if span.highlight.is_none() && !span.underline && !span.strikethrough && !hovered {
                continue;
            }
            let regions = state.paragraph.span_bounds(index);
            if let Some(highlight) = span.highlight {
                for region in &regions {
                    let region = Rectangle::new(
                        region.position() - Vector::new(span.padding.left, span.padding.top),
                        region.size() + Size::new(span.padding.x(), span.padding.y()),
                    );
                    renderer.fill_quad(
                        renderer::Quad { bounds: region + translation, border: highlight.border, ..Default::default() },
                        highlight.background,
                    );
                }
            }
            if span.underline || span.strikethrough || hovered {
                let size = span.size.unwrap_or(self.size);
                let line_height = span.line_height.unwrap_or_default().to_absolute(size);
                let color = span.color.unwrap_or(defaults.text_color);
                let baseline = translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);
                for region in &regions {
                    let mut line = |offset: f32| renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(region.position() + baseline - Vector::new(0.0, offset), Size::new(region.width, 1.0)),
                            ..Default::default()
                        },
                        color,
                    );
                    if span.underline || hovered { line(size.0 * 0.08); }
                    if span.strikethrough { line(size.0 / 2.0); }
                }
            }
        }

        // The selected part of this block, one rectangle per line of laid-out text.
        if let Some((from, to)) = self.selection.lock().range(self.id, state.last()) {
            let start = cosmic_text::Cursor::new(from.0, from.1);
            let end = cosmic_text::Cursor::new(to.0, to.1);
            let color = theme.extended_palette().primary.weak.color;
            for run in state.paragraph.buffer().layout_runs() {
                if let Some((x, width)) = run.highlight(start, end) {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(Point::new(x, run.line_top), Size::new(width, run.line_height)) + translation,
                            ..Default::default()
                        },
                        color,
                    );
                }
            }
        }

        iced::advanced::widget::text::draw(renderer, defaults, bounds, &state.paragraph, Default::default(), viewport);
    }

    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout<'_>, cursor: mouse::Cursor, _renderer: &iced::Renderer, clipboard: &mut dyn Clipboard, shell: &mut Shell<'_, Message>, _viewport: &Rectangle) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let inside = cursor.position_in(bounds);

        let hovered = inside.and_then(|point| state.paragraph.hit_span(point)).filter(|span| self.spans.get(*span).is_some_and(|span| span.link.is_some()));
        if hovered != state.hovered_link {
            state.hovered_link = hovered;
            shell.request_redraw();
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let mut shared = self.selection.lock();
                let had = shared.ends().is_some();
                let Some(point) = inside else {
                    shared.press(None);
                    if had && shared.ends().is_none() { shell.request_redraw(); }
                    return;
                };
                let click = Click::new(point + (bounds.position() - Point::ORIGIN), mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                state.pressed_link = hovered;
                let at = state.hit(point);
                match click.kind() {
                    Kind::Single => shared.press(Some((self.id, at))),
                    Kind::Double => {
                        let lines = state.lines();
                        let (start, end) = word_at(lines.get(at.0).copied().unwrap_or(""), at.1);
                        shared.select((self.id, (at.0, start)), (self.id, (at.0, end)));
                    }
                    Kind::Triple => shared.select((self.id, (0, 0)), (self.id, state.last())),
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(position) = cursor.position() else { return };
                // Outside the block the nearest place still counts: past an edge is that edge.
                let at = state.hit(position - (bounds.position() - Point::ORIGIN));
                if self.selection.lock().drag(self.id, inside.is_some(), at) {
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let mut shared = self.selection.lock();
                shared.release();
                // A link is followed by a plain click on it, not by a drag that selected text.
                let pressed = state.pressed_link.take();
                if pressed.is_some() && pressed == hovered && shared.ends().is_none() {
                    if let Some(link) = pressed.and_then(|span| self.spans.get(span)).and_then(|span| span.link.clone()) {
                        shell.publish((self.on_link)(link));
                    }
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                if modifiers.command() && !modifiers.alt() && key.as_ref() == keyboard::Key::Character("c") =>
            {
                let mut shared = self.selection.lock();
                if let Some((from, to)) = shared.range(self.id, state.last()) {
                    let text = shared.copy(self.id, self.separator, slice(&state.lines(), from, to));
                    clipboard.write(iced::advanced::clipboard::Kind::Standard, text);
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, _viewport: &Rectangle, _renderer: &iced::Renderer) -> mouse::Interaction {
        if tree.state.downcast_ref::<State>().hovered_link.is_some() {
            mouse::Interaction::Pointer
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Message: 'a> From<SelectableText<'a, Message>> for Element<'a, Message> {
    fn from(text: SelectableText<'a, Message>) -> Self {
        Element::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Id = (0, 0);
    const B: Id = (0, 1);
    const C: Id = (1, 0);

    #[test]
    fn a_drag_selects_from_the_press_to_wherever_the_mouse_is() {
        let mut shared = Shared::default();
        // Moving without a press selects nothing.
        assert!(!shared.drag(A, true, (0, 3)));
        shared.press(Some((A, (0, 2))));
        assert_eq!(shared.ends(), None, "a press alone selects nothing");
        assert!(shared.drag(A, true, (0, 6)));
        assert_eq!(shared.range(A, (0, 10)), Some(((0, 2), (0, 6))));
        assert!(!shared.drag(A, true, (0, 6)), "the same place again is not a change");
        // Into the next block, which was not under the mouse before.
        assert!(shared.drag(B, true, (1, 4)));
        assert_eq!(shared.range(A, (0, 10)), Some(((0, 2), (0, 10))), "the first block to its end");
        assert_eq!(shared.range(B, (2, 7)), Some(((0, 0), (1, 4))), "the second from its start");
        assert_eq!(shared.range(C, (0, 5)), None);
        // Blocks the mouse is not in don't take the end; the one holding it follows the
        // mouse to its own edge.
        assert!(!shared.drag(A, false, (0, 10)));
        assert!(shared.drag(B, false, (2, 7)));
        // Dragging back above the press selects backwards.
        assert!(shared.drag(A, true, (0, 0)));
        assert_eq!(shared.range(A, (0, 10)), Some(((0, 0), (0, 2))));
        assert_eq!(shared.range(B, (2, 7)), None);
        shared.release();
        assert!(!shared.drag(A, true, (0, 9)), "released: moving no longer selects");
    }

    #[test]
    fn blocks_between_the_ends_are_selected_whole_across_documents() {
        let mut shared = Shared::default();
        shared.press(Some((A, (0, 4))));
        shared.drag(C, true, (0, 3));
        assert_eq!(shared.range(B, (3, 9)), Some(((0, 0), (3, 9))));
        assert_eq!(shared.range(C, (0, 5)), Some(((0, 0), (0, 3))));
        assert_eq!(shared.range((2, 0), (0, 5)), None);
        // An empty block has nothing to select, and an end past a block's text (it shrank
        // since) is clamped to it.
        assert_eq!(shared.range(B, (0, 0)), None);
        assert_eq!(shared.range(C, (0, 2)), Some(((0, 0), (0, 2))));
    }

    #[test]
    fn a_press_elsewhere_clears_whatever_order_blocks_hear_of_it() {
        let selected = || {
            let mut shared = Shared::default();
            shared.press(Some((A, (0, 1))));
            shared.drag(A, true, (0, 5));
            shared.release();
            shared
        };
        // A press that lands on no block clears.
        let mut shared = selected();
        shared.press(None);
        shared.press(None);
        assert_eq!(shared.ends(), None);
        // A press on block B: A hears of it as "not on me" before or after B does, and
        // either way B's new selection survives.
        for b_first in [true, false] {
            let mut shared = selected();
            if b_first { shared.press(Some((B, (0, 3)))); }
            shared.press(None);
            if !b_first { shared.press(Some((B, (0, 3)))); }
            shared.press(None);
            assert!(shared.drag(B, true, (0, 8)));
            assert_eq!(shared.range(B, (0, 9)), Some(((0, 3), (0, 8))));
            assert_eq!(shared.range(A, (0, 9)), None);
        }
    }

    #[test]
    fn double_and_triple_clicks_select_outright() {
        let mut shared = Shared::default();
        shared.select((A, (0, 4)), (A, (0, 9)));
        assert_eq!(shared.range(A, (0, 20)), Some(((0, 4), (0, 9))));
        // The other blocks hear of the same press and must leave it alone.
        shared.press(None);
        assert_eq!(shared.range(A, (0, 20)), Some(((0, 4), (0, 9))));
        // It is not a drag: moving the mouse afterwards doesn't stretch it.
        assert!(!shared.drag(A, true, (0, 15)));
    }

    #[test]
    fn copying_gathers_every_selected_block_in_reading_order() {
        let mut shared = Shared::default();
        shared.press(Some((A, (0, 0))));
        shared.drag(C, true, (0, 4));
        // Blocks answer the key press in any order; the last answer has everything.
        assert_eq!(shared.copy(C, "\n\n", "next".into()), "next");
        assert_eq!(shared.copy(A, "\n\n", "First paragraph".into()), "First paragraph\n\nnext");
        assert_eq!(shared.copy(B, "\n", "code line".into()), "First paragraph\ncode line\n\nnext");
        // A new selection starts a new copy.
        shared.drag(C, true, (0, 2));
        assert_eq!(shared.copy(C, "\n\n", "ne".into()), "ne");
        shared.clear();
        assert_eq!(shared.ends(), None);
    }

    #[test]
    fn text_is_cut_out_by_line_and_byte() {
        let lines = ["first line", "second", "", "last one"];
        assert_eq!(slice(&lines, (0, 6), (0, 10)), "line");
        assert_eq!(slice(&lines, (0, 6), (1, 3)), "line\nsec");
        assert_eq!(slice(&lines, (0, 0), (3, 8)), "first line\nsecond\n\nlast one");
        assert_eq!(slice(&lines, (1, 6), (3, 0)), "\n\n");
        // Places past the text are clamped.
        assert_eq!(slice(&lines, (3, 5), (9, 99)), "one");
        assert_eq!(slice(&[], (0, 0), (0, 5)), "");
    }

    #[test]
    fn words_are_runs_of_letters_digits_and_underscores() {
        let line = "let total_2 = naïve(x);";
        assert_eq!(word_at(line, 0), (0, 3));
        assert_eq!(word_at(line, 2), (0, 3));
        assert_eq!(word_at(line, 6), (4, 11));
        assert_eq!(word_at(line, 3), (3, 4), "a space is its own selection");
        assert_eq!(&line[word_at(line, 16).0..word_at(line, 16).1], "naïve");
        assert_eq!(&line[word_at(line, 20).0..word_at(line, 20).1], "(");
        // At the very end, the last word; in an empty line, nothing.
        assert_eq!(word_at("the end", 7), (4, 7));
        assert_eq!(word_at("", 0), (0, 0));
    }
}
