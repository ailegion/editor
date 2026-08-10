//! Custom code editor widget built on `cosmic-text`, replacing the built-in
//! `iced::widget::text_editor` / `iced-code-editor` crate.
//!
//! Build order (see project notes): (1) buffer + rendering + blinking cursor,
//! (2) input handling, (3) syntax highlighting, (4) gutter, (5) search/replace,
//! (6) undo/redo, (7) wiring into `Tab`/`State`, (8) polish. (1)-(6) are implemented;
//! only app wiring and final polish remain.

mod buffer;
mod highlight;
pub mod input;
mod render;
pub mod search;
mod theme;

pub use buffer::Buffer;
pub use highlight::Highlighter;
pub use theme::{default_metrics, Style};

use iced::mouse;
use iced::widget::canvas::{self, Canvas};
use iced::{Element, Event, Length, Rectangle, Renderer, Theme};

/// Canvas-based `Program` that draws a [`Buffer`]'s contents and turns mouse clicks/drags
/// into `cosmic_text::Action`s published as `Message`s (mirroring `text_editor`'s
/// `on_action`). Keyboard input is handled separately via [`input::handle_key`], since a
/// `canvas` doesn't participate in iced's per-widget focus system -- see that function's
/// docs for why.
pub struct CodeEditor<'a, Message> {
    content: &'a Buffer,
    style: Style,
    on_action: Option<Box<dyn Fn(cosmic_text::Action) -> Message + 'a>>,
}

impl<'a, Message> CodeEditor<'a, Message> {
    pub fn new(content: &'a Buffer, theme: &Theme) -> Self {
        Self {
            content,
            style: Style::from_theme(theme),
            on_action: None,
        }
    }

    pub fn on_action(mut self, f: impl Fn(cosmic_text::Action) -> Message + 'a) -> Self {
        self.on_action = Some(Box::new(f));
        self
    }
}

#[derive(Default)]
pub struct State {
    dragging: bool,
    scroll: f32,
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
        render::draw(self.content, &mut frame, &self.style, state.scroll);
        vec![frame.into_geometry()]
    }

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        // Rendered text starts `gutter_width` pixels right of the canvas origin and `scroll`
        // pixels above wherever it's currently scrolled to (see `render.rs`), but
        // `cosmic_text`'s own coordinate space starts at (0, 0) -- so mouse positions need to
        // be un-offset here before becoming a `Click`/`Drag` action.
        let gutter_width = self.style.gutter_width(self.content.line_count());

        let Event::Mouse(mouse_event) = event else {
            return None;
        };

        match mouse_event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let on_action = self.on_action.as_ref()?;
                let position = cursor.position_in(bounds)?;
                state.dragging = true;
                Some(
                    canvas::Action::publish(on_action(cosmic_text::Action::Click {
                        x: (position.x - gutter_width) as i32,
                        y: (position.y + state.scroll) as i32,
                    }))
                    .and_capture(),
                )
            }
            mouse::Event::CursorMoved { .. } if state.dragging => {
                let on_action = self.on_action.as_ref()?;
                let position = cursor.position_in(bounds)?;
                Some(
                    canvas::Action::publish(on_action(cosmic_text::Action::Drag {
                        x: (position.x - gutter_width) as i32,
                        y: (position.y + state.scroll) as i32,
                    }))
                    .and_capture(),
                )
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
                let max_scroll = (self.content.line_count() as f32 * self.style.line_height
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
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }
}

pub fn code_editor<'a, Message>(
    content: &'a Buffer,
    theme: &Theme,
    on_action: impl Fn(cosmic_text::Action) -> Message + 'a,
) -> Element<'a, Message>
where
    Message: 'a,
{
    Canvas::new(CodeEditor::new(content, theme).on_action(on_action))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
