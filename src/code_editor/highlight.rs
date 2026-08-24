//! Syntect-driven syntax highlighting, theme derived from `iced::Theme`.

use cosmic_text::{Attrs, Color as CosmicColor};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Loads syntax/theme definitions once and turns source text into `cosmic_text` rich-text
/// spans, so `Buffer` can feed them straight into `Buffer::set_rich_text`.
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_nonewlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    fn syntax_for(&self, extension: &str) -> &SyntaxReference {
        self.syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
    }

    /// Human-readable language name (e.g. "Rust", "Go", "JavaScript") for the status bar,
    /// taken straight from `syntect`'s syntax definitions.
    pub fn language_name(&self, extension: &str) -> &str {
        &self.syntax_for(extension).name
    }

    /// `syntect`'s bundled themes have no 1:1 mapping to the app's 22 named `iced::Theme`s,
    /// so (matching `highlighter_theme_for` in main.rs) this maps by background brightness
    /// instead of guessing per-name.
    fn theme_for(&self, app_theme: &iced::Theme) -> &Theme {
        let bg = app_theme.palette().background;
        let brightness = bg.r + bg.g + bg.b;
        let name = if brightness < 1.5 {
            "base16-ocean.dark"
        } else {
            "InspiredGitHub"
        };
        &self.theme_set.themes[name]
    }

    /// Highlights `text`, returning a flat sequence of `(chunk, attrs)` spans covering the
    /// whole document (line breaks embedded as `"\n"` chunks), ready for
    /// `cosmic_text::Buffer::set_rich_text`.
    pub fn highlight(
        &self,
        text: &str,
        extension: &str,
        app_theme: &iced::Theme,
    ) -> Vec<(String, Attrs<'static>)> {
        let syntax = self.syntax_for(extension);
        let theme = self.theme_for(app_theme);
        let mut highlighter = HighlightLines::new(syntax, theme);

        let lines: Vec<&str> = text.split('\n').collect();
        let last = lines.len().saturating_sub(1);

        let mut spans = Vec::new();
        for (i, line) in lines.into_iter().enumerate() {
            let ranges = highlighter
                .highlight_line(line, &self.syntax_set)
                .unwrap_or_default();
            for (style, piece) in ranges {
                let fg = style.foreground;
                let attrs = Attrs::new().color(CosmicColor::rgba(fg.r, fg.g, fg.b, fg.a));
                spans.push((piece.to_string(), attrs));
            }
            if i != last {
                spans.push(("\n".to_string(), Attrs::new()));
            }
        }
        spans
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}
