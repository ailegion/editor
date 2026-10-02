use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use cosmic_text::LayoutGlyph;
use iced::widget::canvas::{Frame, Path, Stroke, Text};
use iced::{Color, Point, Size};

use super::buffer::Buffer;
use super::theme::Style;
use crate::git_diff::LineStatus;

/// Draws the buffer's text and a blinking cursor into `frame`, scrolled up by `scroll` pixels
/// and left by `scroll_x` pixels. The gutter is painted after the text so anything scrolled
/// under it is hidden.
///
/// Everything is placed by screen row (see `rows.rs`): a line that wraps takes several, and
/// what belongs to the line as a whole -- its number, its fold arrow -- goes on its first.
///
/// The blink phase is derived from wall-clock time rather than stored per-widget state, so it
/// stays in sync across redraws without needing its own subscription: the app already redraws
/// on a timer (see `Message::Tick` in `main.rs`), which is enough to animate it.
pub fn draw(
    buffer: &Buffer,
    diff: &HashMap<usize, LineStatus>,
    diagnostics: &[crate::lsp::Diagnostic],
    blame: &[String],
    frame: &mut Frame,
    style: &Style,
    scroll: f32,
    scroll_x: f32,
) {
    frame.fill_rectangle(Point::ORIGIN, frame.size(), style.background);

    let gutter_width = style.gutter_width(buffer.line_count());
    // Where buffer x = 0 lands on the canvas.
    let text_x = gutter_width - scroll_x;
    let line_height = style.line_height;
    let viewport_height = frame.size().height;

    // Rows fully outside the viewport would still cost a shaped-text draw call each; skip
    // them since `Frame` clipping alone wouldn't save that work.
    let is_visible = |y: f32| y + line_height >= 0.0 && y <= viewport_height;
    let y_of = |row: usize| row as f32 * line_height - scroll;
    let row_count = buffer.visible_count();
    let first_row = ((scroll / line_height).floor().max(0.0) as usize).min(row_count);
    let last_row = (((scroll + viewport_height) / line_height).ceil().max(0.0) as usize).min(row_count);

    // The current line, across every row it wraps onto.
    let cursor_rows = buffer.rows_of(buffer.cursor.line);
    if !cursor_rows.is_empty() {
        let (top, bottom) = (y_of(cursor_rows.start), y_of(cursor_rows.end));
        if bottom >= 0.0 && top <= viewport_height {
            frame.fill_rectangle(
                Point::new(gutter_width, top),
                Size::new((frame.size().width - gutter_width).max(0.0), bottom - top),
                style.current_line_color,
            );
        }
    }

    for &(row, x0, x1) in buffer.selection_pixels() {
        let y = y_of(row);
        if !is_visible(y) {
            continue;
        }
        let width = (x1 - x0).max(4.0);
        frame.fill_rectangle(
            Point::new(text_x + x0, y),
            Size::new(width, line_height),
            style.selection_color,
        );
    }

    for index in first_row..last_row {
        let Some(row) = buffer.row(index) else { continue; };
        let Some(line) = buffer.inner.lines.get(row.line) else { continue; };
        let y = y_of(index);
        let text = line.text();
        let glyphs = row.slice(buffer.glyphs(row.line));
        // Where the unwrapped line's x = 0 lands for this row.
        let row_x = text_x - row.origin + row.indent;
        // Use shaped glyph positions, so guides follow actual spaces and tabs.
        if row.is_first() {
            let mut indent = 0usize;
            for (offset, ch) in text.char_indices() {
                if ch != ' ' && ch != '\t' { break; }
                if indent > 0 && indent % 2 == 0 {
                    if let Some(glyph) = glyphs.iter().find(|glyph| glyph.start == offset) {
                        frame.fill_rectangle(
                            Point::new(row_x + glyph.x, y), Size::new(1.0, line_height),
                            iced::Color { a: 0.14, ..style.text_color },
                        );
                    }
                }
                indent += if ch == '\t' { 4 } else { 1 };
            }
        }
        if line.layout_opt().is_some() {
            draw_glyph_runs(frame, glyphs, text, y, row_x, gutter_width, style);
        } else if row.is_first() {
            fill_run(frame, text, 0, text.len(), text_x, style.text_color, y, style.font_size);
        }
        // What follows a line's text goes after its last row.
        if row.end < text.len() {
            continue;
        }
        let end_x = text_x + row.x_of(buffer.glyphs(row.line), text.len());
        if row.line == buffer.cursor.line && !buffer.collapsed.contains(&row.line) {
            if let Some(annotation) = blame.get(row.line) {
                let x = end_x + style.font_size * 3.0;
                if x >= gutter_width {
                    frame.fill_text(Text {
                        content: format!("⑂ {annotation}"),
                        position: Point::new(x, y),
                        color: iced::Color { a: 0.65, ..style.gutter_text_color },
                        size: iced::Pixels(style.font_size),
                        font: iced::Font::MONOSPACE,
                        ..Text::default()
                    });
                }
            }
        }
        if let Some(end) = buffer.folds.get(&row.line) {
            if buffer.collapsed.contains(&row.line) {
                frame.fill_text(Text {
                    content: format!("  ⋯ {} lines", end - row.line),
                    position: Point::new(end_x, y),
                    color: style.gutter_text_color, size: iced::Pixels(style.font_size), ..Default::default()
                });
            }
        }
    }

    // Wavy underline beneath each diagnostic's range, one stretch per row it runs over.
    // Columns arrive as UTF-16 units and are mapped onto the shaped glyphs, so they land
    // correctly on non-ASCII lines too.
    for diagnostic in diagnostics {
        let Some(line) = buffer.inner.lines.get(diagnostic.line) else { continue; };
        let text = line.text();
        let start = crate::lsp::utf16_to_byte(text, diagnostic.start);
        let end = if diagnostic.end_line > diagnostic.line { text.len() } else { crate::lsp::utf16_to_byte(text, diagnostic.end) };
        let glyphs = buffer.glyphs(diagnostic.line);
        let color = diagnostic_color(diagnostic.severity, style);
        let mut underline = |index: usize, from: usize, to: usize| {
            let Some(row) = buffer.row(index) else { return; };
            let y = y_of(index);
            if !is_visible(y) { return; }
            let x0 = row.x_of(glyphs, from);
            let x1 = row.x_of(glyphs, to).max(x0 + style.font_size * 0.6);
            if x1 < scroll_x { return; }
            let baseline = y + line_height - 2.5;
            let step = 3.0;
            let mut x = (text_x + x0).max(gutter_width);
            let right = text_x + x1;
            let wave = Path::new(|path| {
                path.move_to(Point::new(x, baseline));
                let mut up = true;
                while x < right {
                    x = (x + step).min(right);
                    path.line_to(Point::new(x, if up { baseline - 2.0 } else { baseline }));
                    up = !up;
                }
            });
            frame.stroke(&wave, Stroke::default().with_color(color).with_width(1.0));
        };
        let mut drawn = false;
        for index in buffer.rows_of(diagnostic.line) {
            let Some(row) = buffer.row(index) else { continue; };
            let (from, to) = (start.max(row.start), end.min(row.end));
            if from < to {
                underline(index, from, to);
                drawn = true;
            }
        }
        // A range that covers no text still marks the spot it points at.
        if !drawn {
            if let Some((index, _)) = buffer.locate(diagnostic.line, start) {
                underline(index, start, start);
            }
        }
    }

    for &(row, x0, x1) in buffer.matched_brackets() {
        let y = y_of(row);
        if !is_visible(y) || x0 < scroll_x {
            continue;
        }
        let width = (x1 - x0).max(4.0);
        frame.stroke_rectangle(
            Point::new(text_x + x0, y + 1.0),
            Size::new(width, line_height - 2.0),
            Stroke::default().with_color(style.bracket_match_color).with_width(1.0),
        );
    }

    if blink_on() {
        if let Some((row, x)) = buffer.caret() {
            let y = y_of(row);
            if is_visible(y) && x >= scroll_x {
                frame.fill_rectangle(
                    Point::new(text_x + x, y),
                    Size::new(1.5, line_height),
                    style.cursor_color,
                );
            }
        }
    }

    // Gutter goes on top of the text so lines scrolled left disappear under it.
    frame.fill_rectangle(
        Point::ORIGIN,
        Size::new(gutter_width, viewport_height),
        style.gutter_background,
    );

    // A thin bar at the gutter's left edge for added/modified lines, down every row of the
    // line; a small notch for a pure deletion (the line no longer exists, so there's nothing
    // to bar -- just mark the new-file line it now borders, per
    // `git_diff::LineStatus::Removed`'s doc comment).
    for (&line_i, status) in diff {
        let rows = buffer.rows_of(line_i);
        if rows.is_empty() { continue; }
        let (y, height) = (y_of(rows.start), rows.len() as f32 * line_height);
        if y + height < 0.0 || y > viewport_height {
            continue;
        }
        match status {
            LineStatus::Added => {
                frame.fill_rectangle(Point::new(0.0, y), Size::new(3.0, height), style.diff_added_color);
            }
            LineStatus::Modified => {
                frame.fill_rectangle(Point::new(0.0, y), Size::new(3.0, height), style.diff_modified_color);
            }
            LineStatus::Removed => {
                frame.fill_rectangle(Point::new(0.0, y - 2.0), Size::new(6.0, 4.0), style.diff_removed_color);
            }
        }
    }

    // Gutter dot per diagnostic line, beside the diff bar; the most severe one wins.
    let mut marked = std::collections::HashSet::new();
    for diagnostic in diagnostics {
        if !marked.insert(diagnostic.line) { continue; }
        let Some(row) = buffer.visual_row(diagnostic.line) else { continue; };
        let y = y_of(row);
        if !is_visible(y) { continue; }
        frame.fill_rectangle(
            Point::new(5.0, y + line_height / 2.0 - 2.0), Size::new(4.0, 4.0),
            diagnostic_color(diagnostic.severity, style),
        );
    }

    // The line number and fold arrow sit on a line's first row.
    for index in first_row..last_row {
        let Some(row) = buffer.row(index).filter(|row| row.is_first()) else { continue; };
        let y = y_of(index);
        draw_line_number(frame, row.line + 1, y, gutter_width, style);
        if buffer.folds.contains_key(&row.line) {
            frame.fill_text(Text {
                content: if buffer.collapsed.contains(&row.line) { "▸" } else { "▾" }.into(),
                position: Point::new(gutter_width - 16.0, y),
                color: style.gutter_text_color, size: iced::Pixels(style.font_size), ..Default::default()
            });
        }
    }

}

/// Draws a scrollbar thumb (see `CodeEditor::thumb`), semi-transparent so the text under it
/// stays readable, and brighter while `active` (hovered or dragged).
pub fn draw_thumb(frame: &mut Frame, thumb: iced::Rectangle, style: &Style, active: bool) {
    let inset = 2.0;
    let path = Path::rounded_rectangle(
        Point::new(thumb.x + inset, thumb.y + inset),
        Size::new(thumb.width - inset * 2.0, thumb.height - inset * 2.0),
        ((super::BAR - inset * 2.0) / 2.0).into(),
    );
    frame.fill(&path, iced::Color { a: if active { 0.5 } else { 0.25 }, ..style.text_color });
}

fn diagnostic_color(severity: crate::lsp::Severity, style: &Style) -> Color {
    use crate::lsp::Severity;
    match severity {
        Severity::Error => Color::from_rgb(0.93, 0.33, 0.31),
        Severity::Warning => Color::from_rgb(0.88, 0.68, 0.20),
        Severity::Information => Color::from_rgb(0.36, 0.60, 0.90),
        Severity::Hint => Color { a: 0.5, ..style.text_color },
    }
}

fn draw_line_number(frame: &mut Frame, number: usize, y: f32, gutter_width: f32, style: &Style) {
    frame.fill_text(Text {
        content: number.to_string(),
        position: Point::new(gutter_width - 20.0, y),
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
///
/// `glyphs` are one screen row's, and `x_offset` is where that row puts the line's x = 0.
fn draw_glyph_runs(
    frame: &mut Frame,
    glyphs: &[LayoutGlyph],
    text: &str,
    y: f32,
    x_offset: f32,
    clip_left: f32,
    style: &Style,
) {
    for glyph in glyphs {
        if glyph.end <= glyph.start {
            continue;
        }
        // Canvas text paints above every canvas shape, gutter included, so glyphs scrolled
        // under the gutter must be skipped rather than covered.
        if x_offset + glyph.x < clip_left {
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
