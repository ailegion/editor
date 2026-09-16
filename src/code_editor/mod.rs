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

/// Canvas-based `Program` that draws a [`Buffer`]'s contents and turns mouse clicks/drags
/// into `cosmic_text::Action`s published as `Message`s (mirroring `text_editor`'s
/// `on_action`). Keyboard input is handled separately via [`input::handle_key`], since a
/// `canvas` doesn't participate in iced's per-widget focus system -- see that function's
/// docs for why.
pub struct CodeEditor<'a, Message> {
    content: &'a Buffer,
    diff: &'a HashMap<usize, LineStatus>,
    style: Style,
    on_fold: Option<Box<dyn Fn(usize) -> Message + 'a>>,
    on_action: Option<Box<dyn Fn(cosmic_text::Action) -> Message + 'a>>,
}

impl<'a, Message> CodeEditor<'a, Message> {
    pub fn new(
        content: &'a Buffer,
        diff: &'a HashMap<usize, LineStatus>,
        theme: &Theme,
        zoom: f32,
    ) -> Self {
        Self {
            content,
            diff,
            style: Style::from_theme(theme, zoom),
            on_action: None,
            on_fold: None,
        }
    }

    pub fn on_action(mut self, f: impl Fn(cosmic_text::Action) -> Message + 'a) -> Self {
        self.on_action = Some(Box::new(f));
        self
    }

    /// If the cursor moved since `state.last_cursor` and now falls outside
    /// `[scroll, scroll + viewport_height)`, returns the corrected scroll offset to bring it
    /// back into view. Always updates `state.last_cursor`. Returns `None` both when the
    /// cursor hasn't moved (so a manual scroll-away is left alone) and when it moved but is
    /// still visible.
    fn scroll_correction(&self, state: &mut State, viewport_height: f32) -> Option<f32> {
        let current = self.content.cursor_pixel().and_then(|(x, _)| {
            self.content.visual_row(self.content.cursor.line).map(|row| (x, (row as f32 * self.style.line_height) as i32))
        });
        let moved = current != state.last_cursor;
        state.last_cursor = current;
        if !moved {
            return None;
        }

        let (_, y) = current?;
        let y = y as f32;
        let line_height = self.style.line_height;
        if y < state.scroll {
            Some(y)
        } else if y + line_height > state.scroll + viewport_height {
            Some(y + line_height - viewport_height)
        } else {
            None
        }
    }
}

#[derive(Default)]
pub struct State {
    dragging: bool,
    scroll: f32,
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
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        render::draw(self.content, self.diff, &mut frame, &self.style, state.scroll);
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
            return self.scroll_correction(state, bounds.height).map(|scroll| {
                state.scroll = scroll;
                canvas::Action::request_redraw()
            });
        }

        // Rendered text starts `gutter_width` pixels right of the canvas origin and `scroll`
        // pixels above wherever it's currently scrolled to (see `render.rs`), but
        // `cosmic_text`'s own coordinate space starts at (0, 0) -- so mouse positions need to
        // be un-offset here before becoming a `Click`/`Drag` action.
        let gutter_width = self.style.gutter_width(self.content.line_count());
        let scroll = state.scroll;
        let buffer_position = |position: iced::Point| {
            let y = position.y + scroll;
            let row = (y / self.style.line_height).max(0.0) as usize;
            (
                (position.x - gutter_width) as i32,
                (self.content.source_line(row) as f32 * self.style.line_height
                    + y.rem_euclid(self.style.line_height)) as i32,
            )
        };

        let Event::Mouse(mouse_event) = event else {
            return None;
        };

        match mouse_event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let on_action = self.on_action.as_ref()?;
                let position = cursor.position_in(bounds)?;
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
            mouse::Event::CursorMoved { .. } if state.dragging => {
                let on_action = self.on_action.as_ref()?;
                let position = cursor.position_in(bounds)?;
                let (x, y) = buffer_position(position);
                Some(canvas::Action::publish(on_action(cosmic_text::Action::Drag { x, y })).and_capture())
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                state.dragging = false;
                None
            }
            mouse::Event::WheelScrolled { delta } if cursor.is_over(bounds) => {
                let dy = match *delta {
                    mouse::ScrollDelta::Lines { y, .. } => y * self.style.line_height * 3.0,
                    mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                let max_scroll = (self.content.visible_count() as f32 * self.style.line_height
                    - bounds.height)
                    .max(0.0);
                state.scroll = (state.scroll - dy).clamp(0.0, max_scroll);
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
    theme: &Theme,
    zoom: f32,
    on_action: impl Fn(cosmic_text::Action) -> Message + 'a,
    on_fold: impl Fn(usize) -> Message + 'a,
) -> Element<'a, Message>
where
    Message: 'a,
{
    let mut editor = CodeEditor::new(content, diff, theme, zoom).on_action(on_action);
    editor.on_fold = Some(Box::new(on_fold));
    Canvas::new(editor)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
