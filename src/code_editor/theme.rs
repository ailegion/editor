use iced::Color;

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 20.0;

pub const ZOOM_DEFAULT: f32 = 1.0;
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 2.5;
pub const ZOOM_STEP: f32 = 0.1;

/// `Metrics` for a [`super::Buffer`] at the given zoom level, matching [`Style`]'s font
/// size/line height (see `Style::new`) so the two stay in sync.
pub fn metrics_for_zoom(zoom: f32) -> cosmic_text::Metrics {
    cosmic_text::Metrics::new(FONT_SIZE * zoom, LINE_HEIGHT * zoom)
}

/// Visual style for [`super::CodeEditor`], from the current theme's editor colors.
pub struct Style {
    pub text_color: Color,
    pub background: Color,
    pub cursor_color: Color,
    pub selection_color: Color,
    pub bracket_match_color: Color,
    pub diff_added_color: Color,
    pub diff_modified_color: Color,
    pub diff_removed_color: Color,
    pub current_line_color: Color,
    pub gutter_background: Color,
    pub gutter_text_color: Color,
    pub font_size: f32,
    pub line_height: f32,
}

impl Style {
    pub fn new(colors: &crate::theme::EditorColors, zoom: f32) -> Self {
        Self {
            text_color: colors.foreground,
            background: colors.background,
            cursor_color: colors.cursor,
            selection_color: colors.selection,
            bracket_match_color: colors.bracket_match,
            diff_added_color: colors.gutter_added,
            diff_modified_color: colors.gutter_modified,
            diff_removed_color: colors.gutter_deleted,
            current_line_color: colors.line_highlight,
            gutter_background: colors.gutter_background,
            gutter_text_color: colors.line_number,
            font_size: FONT_SIZE * zoom,
            line_height: LINE_HEIGHT * zoom,
        }
    }

    /// Width in pixels of the line-number gutter, sized for `line_count`'s digit count.
    pub fn gutter_width(&self, line_count: usize) -> f32 {
        let digits = line_count.max(1).to_string().len().max(2);
        digits as f32 * self.font_size * 0.62 + 28.0
    }
}
