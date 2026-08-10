use iced::Color;

pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 20.0;

pub const ZOOM_DEFAULT: f32 = 1.0;
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 2.5;
pub const ZOOM_STEP: f32 = 0.1;

/// `Metrics` for a [`super::Buffer`] at the given zoom level, matching [`Style`]'s font
/// size/line height (see `Style::from_theme`) so the two stay in sync.
pub fn metrics_for_zoom(zoom: f32) -> cosmic_text::Metrics {
    cosmic_text::Metrics::new(FONT_SIZE * zoom, LINE_HEIGHT * zoom)
}

/// Visual style for [`super::CodeEditor`], derived from the app-wide [`iced::Theme`].
pub struct Style {
    pub text_color: Color,
    pub background: Color,
    pub cursor_color: Color,
    pub selection_color: Color,
    pub current_line_color: Color,
    pub gutter_background: Color,
    pub gutter_text_color: Color,
    pub font_size: f32,
    pub line_height: f32,
}

impl Style {
    pub fn from_theme(theme: &iced::Theme, zoom: f32) -> Self {
        let palette = theme.extended_palette();
        Self {
            text_color: palette.background.base.text,
            background: palette.background.base.color,
            cursor_color: palette.primary.base.color,
            selection_color: palette.primary.weak.color,
            current_line_color: palette.background.weak.color,
            gutter_background: palette.background.weak.color,
            gutter_text_color: palette.background.strong.text,
            font_size: FONT_SIZE * zoom,
            line_height: LINE_HEIGHT * zoom,
        }
    }

    /// Width in pixels of the line-number gutter, sized for `line_count`'s digit count.
    pub fn gutter_width(&self, line_count: usize) -> f32 {
        let digits = line_count.max(1).to_string().len().max(2);
        digits as f32 * self.font_size * 0.62 + 16.0
    }
}
