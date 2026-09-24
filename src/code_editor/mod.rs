//! Custom code editor widget built on `cosmic-text`, replacing the built-in
//! `iced::widget::text_editor` / `iced-code-editor` crate.
//!
//! Build order (see project notes): (1) buffer + rendering + blinking cursor,
//! (2) input handling, (3) syntax highlighting, (4) gutter, (5) search/replace,
//! (6) undo/redo, (7) wiring into `Tab`/`State`, (8) polish. (1)-(6) are implemented;
//! only app wiring and final polish remain.

mod brackets;
mod folding;
mod buffer;
mod highlight;
pub mod input;
mod render;
pub mod search;
mod theme;

pub use buffer::Buffer;
pub use highlight::Highlighter;
pub use theme::{metrics_for_zoom, Style, ZOOM_DEFAULT, ZOOM_MAX, ZOOM_MIN, ZOOM_STEP};

use std::collections::HashMap;

use iced::advanced::mouse::click::{Click, Kind as ClickKind};
use iced::mouse;
use iced::widget::canvas::{self, Canvas};
use iced::{Element, Event, Length, Rectangle, Renderer, Theme};

use crate::git_diff::LineStatus;
use crate::theme::EditorColors;

/// Canvas-based `Program` that draws a [`Buffer`]'s contents and turns mouse clicks/drags
/// into `cosmic_text::Action`s published as `Message`s (mirroring `text_editor`'s
/// `on_action`). Keyboard input is handled separately via [`input::handle_key`], since a
/// `canvas` doesn't participate in iced's per-widget focus system -- see that function's
/// docs for why.
pub struct CodeEditor<'a, Message> {
    content: &'a Buffer,
    diff: &'a HashMap<usize, LineStatus>,
    diagnostics: &'a [crate::lsp::Diagnostic],
    style: Style,
    on_fold: Option<Box<dyn Fn(usize) -> Message + 'a>>,
    on_action: Option<Box<dyn Fn(cosmic_text::Action) -> Message + 'a>>,
    on_probe: Option<Box<dyn Fn(Probe) -> Message + 'a>>,
}

/// Mouse gestures that ask the language server something about a buffer position
/// (`line`, byte `index`), published through `code_editor`'s `on_probe`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Probe {
    /// The mouse rested on this position long enough to want hover information. `anchor` is
    /// the canvas pixel just below the word, where a popup should go.
    Hover { line: usize, index: usize, anchor: iced::Point },
    /// The mouse moved off the hovered position (or scrolled); hide any hover popup.
    Leave,
    /// Ctrl/Cmd+click: go to the definition of what's here.
    Definition(usize, usize),
}

/// Hover text for (`line`, byte `index`), shown by the app as a widget stacked over the
/// editor at `anchor`. It can't be drawn inside the canvas: canvas text always paints above
/// every canvas shape, so a box drawn there would have the code showing through it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hover {
    pub line: usize,
    pub index: usize,
    pub anchor: iced::Point,
    pub lines: Vec<String>,
}

/// How long the mouse must rest on a word before hover information is requested.
const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

impl<'a, Message> CodeEditor<'a, Message> {
    pub fn new(
        content: &'a Buffer,
        diff: &'a HashMap<usize, LineStatus>,
        diagnostics: &'a [crate::lsp::Diagnostic],
        colors: &EditorColors,
        zoom: f32,
    ) -> Self {
        Self {
            content,
            diff,
            diagnostics,
            style: Style::new(colors, zoom),
            on_action: None,
            on_fold: None,
            on_probe: None,
        }
    }

    /// Buffer position (`line`, byte `index`) under `position`, when it's over actual text
    /// rather than the gutter, the scrollbars, or the empty space past a line's end.
    fn cell_at(&self, state: &State, bounds: Rectangle, position: iced::Point) -> Option<(usize, usize)> {
        let gutter_width = self.style.gutter_width(self.content.line_count());
        if position.x < gutter_width || position.x >= bounds.width - BAR || position.y >= bounds.height - BAR {
            return None;
        }
        let y = position.y + state.scroll;
        let row = (y / self.style.line_height).max(0.0) as usize;
        if row >= self.content.visible_count() { return None; }
        let line = self.content.source_line(row);
        let x = position.x - gutter_width + state.scroll_x;
        let hit = self.content.inner.hit(x, line as f32 * self.style.line_height + y.rem_euclid(self.style.line_height))?;
        let length = self.content.inner.lines.get(hit.line)?.text().len();
        (hit.line == line && hit.index < length).then_some((hit.line, hit.index))
    }

    /// Canvas pixel just below (`line`, byte `index`), for anchoring a popup.
    fn anchor_of(&self, state: &State, line: usize, index: usize) -> iced::Point {
        let gutter_width = self.style.gutter_width(self.content.line_count());
        let x = self.content.inner.lines.get(line)
            .and_then(|l| l.layout_opt().and_then(|lines| lines.first()).map(|layout| render::x_at(layout, index)))
            .unwrap_or(0.0);
        let row = self.content.visual_row(line).unwrap_or(0) as f32;
        iced::Point::new(gutter_width - state.scroll_x + x, (row + 1.0) * self.style.line_height - state.scroll)
    }

    pub fn on_action(mut self, f: impl Fn(cosmic_text::Action) -> Message + 'a) -> Self {
        self.on_action = Some(Box::new(f));
        self
    }

    /// If the cursor moved since `state.last_cursor` and now falls outside the visible
    /// window (vertically or horizontally), corrects `state.scroll`/`state.scroll_x` to
    /// bring it back into view and returns `true`. Always updates `state.last_cursor`.
    /// Returns `false` both when the cursor hasn't moved (so a manual scroll-away is left
    /// alone) and when it moved but is still visible.
    fn scroll_correction(&self, state: &mut State, viewport: iced::Size) -> bool {
        let current = self.content.cursor_pixel().and_then(|(x, _)| {
            self.content.visual_row(self.content.cursor.line).map(|row| (x, (row as f32 * self.style.line_height) as i32))
        });
        let moved = current != state.last_cursor;
        state.last_cursor = current;
        if !moved {
            return false;
        }

        let Some((x, y)) = current else { return false };
        let (x, y) = (x as f32, y as f32);
        let line_height = self.style.line_height;
        let mut changed = false;
        if y < state.scroll {
            state.scroll = y;
            changed = true;
        } else if y + line_height > state.scroll + viewport.height {
            state.scroll = y + line_height - viewport.height;
            changed = true;
        }
        // Keep a small margin so the cursor never sits flush against either edge.
        let text_width = (viewport.width - self.style.gutter_width(self.content.line_count())).max(0.0);
        let margin = H_SCROLL_PAD.min(text_width / 2.0);
        if x - margin < state.scroll_x {
            state.scroll_x = (x - margin).max(0.0);
            changed = true;
        } else if x + margin > state.scroll_x + text_width {
            state.scroll_x = x + margin - text_width;
            changed = true;
        }
        changed
    }

    fn max_scroll_x(&self, bounds: Rectangle) -> f32 {
        let text_width = bounds.width - self.style.gutter_width(self.content.line_count());
        (self.content.content_width() + H_SCROLL_PAD - text_width).max(0.0)
    }

    /// `(track_start, track_length, content_length)` along `axis`, all in canvas pixels.
    fn track(&self, bounds: Rectangle, axis: Axis) -> (f32, f32, f32) {
        match axis {
            Axis::X => {
                let gutter = self.style.gutter_width(self.content.line_count());
                (gutter, (bounds.width - gutter).max(0.0), self.content.content_width() + H_SCROLL_PAD)
            }
            Axis::Y => (0.0, bounds.height, self.content.visible_count() as f32 * self.style.line_height),
        }
    }

    /// Thumb rectangle for `axis` in canvas coordinates, or `None` when nothing overflows.
    fn thumb(&self, state: &State, bounds: Rectangle, axis: Axis) -> Option<Rectangle> {
        let (start, track, content) = self.track(bounds, axis);
        if content <= track || track <= 0.0 {
            return None;
        }
        let offset = match axis { Axis::X => state.scroll_x, Axis::Y => state.scroll };
        let len = (track * track / content).max(BAR * 2.0).min(track);
        let pos = (start + offset / content * track).min(start + track - len);
        Some(match axis {
            Axis::X => Rectangle::new(iced::Point::new(pos, bounds.height - BAR), iced::Size::new(len, BAR)),
            Axis::Y => Rectangle::new(iced::Point::new(bounds.width - BAR, pos), iced::Size::new(BAR, len)),
        })
    }

    /// Scrolls so the thumb's leading edge lands at `pos` along `axis`.
    fn scroll_to_thumb(&self, state: &mut State, bounds: Rectangle, axis: Axis, pos: f32) {
        let (start, track, content) = self.track(bounds, axis);
        let offset = ((pos - start) / track * content).clamp(0.0, (content - track).max(0.0));
        match axis {
            Axis::X => state.scroll_x = offset,
            Axis::Y => state.scroll = offset,
        }
    }
}

/// Extra pixels of horizontal scroll past the widest line, so its last character doesn't
/// sit flush against the canvas edge.
const H_SCROLL_PAD: f32 = 40.0;

/// Scrollbar thickness. The bars overlay the text along the right and bottom edges.
pub(super) const BAR: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis { X, Y }

#[derive(Default)]
pub struct State {
    dragging: bool,
    scroll: f32,
    scroll_x: f32,
    /// A scrollbar thumb being dragged, with the grab offset from its leading edge.
    thumb_drag: Option<(Axis, f32)>,
    /// Shift held, so a vertical wheel pans sideways (Windows doesn't do this conversion).
    shift: bool,
    /// Ctrl (Cmd on macOS) held, so a click asks for the definition instead of moving the caret.
    command: bool,
    /// Where the mouse currently rests, since when, and whether a hover was already sent.
    hover_cell: Option<(usize, usize)>,
    hover_since: Option<std::time::Instant>,
    hover_sent: bool,
    /// The cursor pixel position as of the last redraw check, so scroll-into-view only
    /// fires when the cursor itself moved -- not just because the user scrolled the
    /// viewport away from a stationary cursor to read elsewhere in the file.
    last_cursor: Option<(i32, i32)>,
    /// Previous left-click, so the next press can be recognized as a double/triple click.
    last_click: Option<Click>,
}

impl<'a, Message> canvas::Program<Message> for CodeEditor<'a, Message> {
    type State = State;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        render::draw(self.content, self.diff, self.diagnostics, &mut frame, &self.style, state.scroll, state.scroll_x);
        // Scrollbars: semi-transparent overlays, brighter while hovered or dragged.
        let local = Rectangle::with_size(bounds.size());
        let position = cursor.position_in(bounds);
        for axis in [Axis::X, Axis::Y] {
            let Some(thumb) = self.thumb(state, local, axis) else { continue };
            let over_track = position.is_some_and(|p| match axis {
                Axis::X => p.y >= local.height - BAR,
                Axis::Y => p.x >= local.width - BAR,
            });
            let active = over_track || state.thumb_drag.is_some_and(|(a, _)| a == axis);
            render::draw_thumb(&mut frame, thumb, &self.style, active);
        }
        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        // `scroll` lives here in the canvas's own widget state rather than on `Buffer`
        // (app-level state `main.rs` can see), so nothing outside this widget -- typing,
        // arrow keys, a search/goto-line jump -- can tell it to re-center the viewport
        // directly. Instead, treat every `RedrawRequested` (which fires at least once per
        // animation tick, see `render::draw`'s blinking cursor) as a chance to notice the
        // cursor drifted outside the visible window and correct `scroll` to bring it back in.
        if let Event::Window(iced::window::Event::RedrawRequested(_)) = event {
            let max_scroll = (self.content.visible_count() as f32 * self.style.line_height - bounds.height).max(0.0);
            state.scroll = state.scroll.min(max_scroll);
            state.scroll_x = state.scroll_x.min(self.max_scroll_x(bounds));
            if !state.hover_sent && state.hover_since.is_some_and(|since| since.elapsed() >= HOVER_DELAY) {
                if let (Some((line, index)), Some(on_probe)) = (state.hover_cell, self.on_probe.as_ref()) {
                    state.hover_sent = true;
                    let anchor = self.anchor_of(state, line, index);
                    return Some(canvas::Action::publish(on_probe(Probe::Hover { line, index, anchor })));
                }
            }
            return self.scroll_correction(state, bounds.size()).then(canvas::Action::request_redraw);
        }
        if let Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.shift = modifiers.shift();
            state.command = modifiers.command();
            return None;
        }

        // Rendered text starts `gutter_width` pixels right of the canvas origin and `scroll`
        // pixels above wherever it's currently scrolled to (see `render.rs`), but
        // `cosmic_text`'s own coordinate space starts at (0, 0) -- so mouse positions need to
        // be un-offset here before becoming a `Click`/`Drag` action.
        let gutter_width = self.style.gutter_width(self.content.line_count());
        let scroll = state.scroll;
        let scroll_x = state.scroll_x;
        let buffer_position = |position: iced::Point| {
            let y = position.y + scroll;
            let row = (y / self.style.line_height).max(0.0) as usize;
            (
                (position.x - gutter_width + scroll_x) as i32,
                (self.content.source_line(row) as f32 * self.style.line_height
                    + y.rem_euclid(self.style.line_height)) as i32,
            )
        };

        let Event::Mouse(mouse_event) = event else {
            return None;
        };

        // Scrollbar thumbs take precedence over the text underneath them.
        let local = Rectangle::with_size(bounds.size());
        match mouse_event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let position = cursor.position_in(bounds)?;
                for axis in [Axis::X, Axis::Y] {
                    let Some(thumb) = self.thumb(state, local, axis) else { continue };
                    let (along, lead, len, in_track) = match axis {
                        Axis::X => (position.x, thumb.x, thumb.width, position.y >= local.height - BAR),
                        Axis::Y => (position.y, thumb.y, thumb.height, position.x >= local.width - BAR),
                    };
                    if !in_track {
                        continue;
                    }
                    // On the thumb: grab where it was clicked. Off it: centre the thumb there.
                    let grab = if thumb.contains(position) { along - lead } else { len / 2.0 };
                    state.thumb_drag = Some((axis, grab));
                    self.scroll_to_thumb(state, local, axis, along - grab);
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                if state.command {
                    if let (Some((line, index)), Some(on_probe)) = (self.cell_at(state, local, position), self.on_probe.as_ref()) {
                        return Some(canvas::Action::publish(on_probe(Probe::Definition(line, index))).and_capture());
                    }
                }
                let on_action = self.on_action.as_ref()?;
                let row = ((position.y + state.scroll) / self.style.line_height).max(0.0) as usize;
                let line = self.content.source_line(row);
                if position.x >= gutter_width - 18.0 && position.x < gutter_width {
                    if self.content.folds.contains_key(&line) {
                        return self.on_fold.as_ref().map(|on_fold| canvas::Action::publish(on_fold(line)).and_capture());
                    }
                }
                let click = Click::new(position, mouse::Button::Left, state.last_click);
                state.last_click = Some(click);
                state.dragging = true;
                let (x, y) = buffer_position(position);
                let action = match click.kind() {
                    ClickKind::Single => cosmic_text::Action::Click { x, y },
                    ClickKind::Double => cosmic_text::Action::DoubleClick { x, y },
                    ClickKind::Triple => cosmic_text::Action::TripleClick { x, y },
                };
                Some(canvas::Action::publish(on_action(action)).and_capture())
            }
            mouse::Event::CursorMoved { .. } if state.thumb_drag.is_some() => {
                let (axis, grab) = state.thumb_drag?;
                // Use the raw position so the drag keeps tracking outside the canvas.
                let position = cursor.position()? - iced::Vector::new(bounds.x, bounds.y);
                let along = match axis { Axis::X => position.x, Axis::Y => position.y };
                self.scroll_to_thumb(state, local, axis, along - grab);
                Some(canvas::Action::request_redraw().and_capture())
            }
            mouse::Event::CursorMoved { .. } if state.dragging => {
                let on_action = self.on_action.as_ref()?;
                let position = cursor.position_in(bounds)?;
                let (x, y) = buffer_position(position);
                Some(canvas::Action::publish(on_action(cosmic_text::Action::Drag { x, y })).and_capture())
            }
            mouse::Event::CursorMoved { .. } => {
                // Track where the mouse rests; the hover request itself fires from the
                // redraw tick once it has stayed put for `HOVER_DELAY`.
                let cell = cursor.position_in(bounds).and_then(|position| self.cell_at(state, local, position));
                if cell != state.hover_cell {
                    let left = state.hover_sent;
                    state.hover_cell = cell;
                    state.hover_since = cell.map(|_| std::time::Instant::now());
                    state.hover_sent = false;
                    if left {
                        if let Some(on_probe) = self.on_probe.as_ref() {
                            return Some(canvas::Action::publish(on_probe(Probe::Leave)));
                        }
                    }
                }
                // Hover highlight on the bars needs a repaint.
                cursor.is_over(bounds).then(canvas::Action::request_redraw)
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                state.dragging = false;
                state.thumb_drag = None;
                None
            }
            mouse::Event::WheelScrolled { delta } if cursor.is_over(bounds) => {
                let (mut dx, mut dy) = match *delta {
                    mouse::ScrollDelta::Lines { x, y } => {
                        (x * self.style.line_height * 3.0, y * self.style.line_height * 3.0)
                    }
                    mouse::ScrollDelta::Pixels { x, y } => (x, y),
                };
                if state.shift && dx == 0.0 {
                    (dx, dy) = (dy, 0.0);
                }
                let max_scroll = (self.content.visible_count() as f32 * self.style.line_height
                    - bounds.height)
                    .max(0.0);
                state.scroll = (state.scroll - dy).clamp(0.0, max_scroll);
                state.scroll_x = (state.scroll_x - dx).clamp(0.0, self.max_scroll_x(bounds));
                // The text moved under a stationary mouse: whatever was hovered is gone.
                if std::mem::take(&mut state.hover_sent) {
                    state.hover_cell = None;
                    state.hover_since = None;
                    if let Some(on_probe) = self.on_probe.as_ref() {
                        return Some(canvas::Action::publish(on_probe(Probe::Leave)).and_capture());
                    }
                }
                Some(canvas::Action::request_redraw().and_capture())
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let Some(position) = cursor.position_in(bounds) else {
            return mouse::Interaction::default();
        };
        let local = Rectangle::with_size(bounds.size());
        let on_bar = state.thumb_drag.is_some()
            || (self.thumb(state, local, Axis::X).is_some() && position.y >= local.height - BAR)
            || (self.thumb(state, local, Axis::Y).is_some() && position.x >= local.width - BAR);
        if on_bar {
            return mouse::Interaction::default();
        }
        // Same hit area as the fold toggle in `update`.
        let gutter_width = self.style.gutter_width(self.content.line_count());
        let row = ((position.y + state.scroll) / self.style.line_height).max(0.0) as usize;
        if position.x >= gutter_width - 18.0
            && position.x < gutter_width
            && self.content.folds.contains_key(&self.content.source_line(row))
        {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::Text
        }
    }
}

pub fn code_editor<'a, Message>(
    content: &'a Buffer,
    diff: &'a HashMap<usize, LineStatus>,
    diagnostics: &'a [crate::lsp::Diagnostic],
    colors: &EditorColors,
    zoom: f32,
    on_action: impl Fn(cosmic_text::Action) -> Message + 'a,
    on_fold: impl Fn(usize) -> Message + 'a,
    on_probe: impl Fn(Probe) -> Message + 'a,
) -> Element<'a, Message>
where
    Message: 'a,
{
    let mut editor = CodeEditor::new(content, diff, diagnostics, colors, zoom).on_action(on_action);
    editor.on_fold = Some(Box::new(on_fold));
    editor.on_probe = Some(Box::new(on_probe));
    Canvas::new(editor)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
