use cosmic_text::LayoutLine;
use iced::widget::canvas::{Frame, Text};
use iced::{Color, Point, Size};
use std::time::{SystemTime, UNIX_EPOCH};

use super::buffer::Buffer;
use super::theme::Style;

/// Draws the buffer's text and a blinking cursor into `frame`, scrolled up by `scroll` pixels.
///
/// The blink phase is derived from wall-clock time rather than stored per-widget state, so it
/// stays in sync across redraws without needing its own subscription: the app already redraws
/// on a timer (see `Message::Tick` in `main.rs`), which is enough to animate it.
pub fn draw(buffer: &Buffer, frame: &mut Frame, style: &Style, scroll: f32) {
    frame.fill_rectangle(Point::ORIGIN, frame.size(), style.background);

    let gutter_width = style.gutter_width(buffer.line_count());
    let line_height = style.line_height;
    let viewport_height = frame.size().height;

    // Lines fully outside the viewport would still cost a shaped-text draw call each; skip
    // them since `Frame` clipping alone wouldn't save that work.
    let is_visible = |y: f32| y + line_height >= 0.0 && y <= viewport_height;

    if let Some(cursor_line) = current_line_y(buffer, scroll) {
        if is_visible(cursor_line) {
            frame.fill_rectangle(
                Point::new(gutter_width, cursor_line),
                Size::new((frame.size().width - gutter_width).max(0.0), line_height),
                style.current_line_color,
            );
        }
    }

    for &(line_i, x0, x1) in buffer.selection_pixels() {
        let y = line_i as f32 * line_height - scroll;
        if !is_visible(y) {
            continue;
        }
        let width = (x1 - x0).max(4.0);
        frame.fill_rectangle(
            Point::new(gutter_width + x0, y),
            Size::new(width, line_height),
            style.selection_color,
        );
    }

    frame.fill_rectangle(
        Point::ORIGIN,
        Size::new(gutter_width, viewport_height),
        style.gutter_background,
    );

    for (i, line) in buffer.inner.lines.iter().enumerate() {
        let y = i as f32 * line_height - scroll;
        if !is_visible(y) {
            continue;
        }
        let text = line.text();
        match line.layout_opt().and_then(|lines| lines.first()) {
            Some(layout_line) => draw_glyph_runs(frame, layout_line, text, y, gutter_width, style),
            None => fill_run(
                frame,
                text,
                0,
                text.len(),
                gutter_width,
                style.text_color,
                y,
                style.font_size,
            ),
        }
        draw_line_number(frame, i + 1, y, gutter_width, style);
    }

    if blink_on() {
        if let Some((x, y)) = buffer.cursor_pixel() {
            let y = y as f32 - scroll;
            if is_visible(y) {
                frame.fill_rectangle(
                    Point::new(gutter_width + x as f32, y),
                    Size::new(1.5, line_height),
                    style.cursor_color,
                );
            }
        }
    }
}

fn current_line_y(buffer: &Buffer, scroll: f32) -> Option<f32> {
    buffer
        .cursor_pixel()
        .map(|(_, y)| y as f32 - scroll)
}

fn draw_line_number(frame: &mut Frame, number: usize, y: f32, gutter_width: f32, style: &Style) {
    frame.fill_text(Text {
        content: number.to_string(),
        position: Point::new(gutter_width - 10.0, y),
        color: style.gutter_text_color,
        size: iced::Pixels(style.font_size),
        align_x: iced::advanced::text::Alignment::Right,
        ..Text::default()
    });
}

/// Draws each glyph cluster individually at cosmic-text's own `glyph.x`, rather than batching
/// same-colored runs into one `fill_text` call over a re-sliced substring.
///
/// That batching was tried first and is the more obvious approach, but it's wrong: iced
/// reshapes whatever string it's given from scratch, and a tab's advance width depends on
/// which column it starts at. A multi-glyph run beginning mid-line (after some already-drawn
/// text) doesn't carry that column context, so a tab inside it measures differently than it
/// did when cosmic-text originally laid out the *whole* line -- every glyph after it then
/// lands off cosmic-text's computed `glyph.x`, producing visible overlap with the next run.
/// Drawing one cluster per glyph sidesteps the mismatch entirely: nothing gets reshaped, we
/// just place each already-shaped cluster where cosmic-text says it goes.
fn draw_glyph_runs(
    frame: &mut Frame,
    layout_line: &LayoutLine,
    text: &str,
    y: f32,
    x_offset: f32,
    style: &Style,
) {
    for glyph in &layout_line.glyphs {
        if glyph.end <= glyph.start {
            continue;
        }
        let piece = &text[glyph.start..glyph.end];
        // Whitespace has no visible glyph; drawing it anyway risks a tofu/notdef box for
        // characters (tabs especially) the fallback font has no glyph for.
        if piece.trim().is_empty() {
            continue;
        }
        let color = glyph
            .color_opt
            .map(cosmic_color_to_iced)
            .unwrap_or(style.text_color);
        frame.fill_text(Text {
            content: piece.to_string(),
            position: Point::new(x_offset + glyph.x, y),
            color,
            size: iced::Pixels(style.font_size),
            ..Text::default()
        });
    }
}

fn fill_run(
    frame: &mut Frame,
    text: &str,
    start: usize,
    end: usize,
    x: f32,
    color: Color,
    y: f32,
    font_size: f32,
) {
    if end <= start {
        return;
    }
    frame.fill_text(Text {
        content: text[start..end].to_string(),
        position: Point::new(x, y),
        color,
        size: iced::Pixels(font_size),
        ..Text::default()
    });
}

fn cosmic_color_to_iced(color: cosmic_text::Color) -> Color {
    let [r, g, b, a] = color.as_rgba();
    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

fn blink_on() -> bool {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (elapsed.as_millis() / 500) % 2 == 0
}
