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
    /// `two_face`'s extra themes, for app themes that have a same-named syntax theme.
    extra_themes: two_face::theme::EmbeddedLazyThemeSet,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: two_face::syntax::extra_no_newlines(),
            theme_set: ThemeSet::load_defaults(),
            extra_themes: two_face::theme::extra(),
        }
    }

    fn syntax_for(&self, extension: &str) -> &SyntaxReference {
        let normalized = extension.to_ascii_lowercase();
        let extension = match normalized.as_str() {
            "mts" | "cts" => "ts",
            "mjs" | "cjs" => "js",
            other => other,
        };
        self.syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
    }

    /// Human-readable language name (e.g. "Rust", "Go", "JavaScript") for the status bar,
    /// taken straight from `syntect`'s syntax definitions.
    pub fn language_name(&self, extension: &str) -> &str {
        &self.syntax_for(extension).name
    }

    /// Uses the same-named syntax theme when `two_face` has one (like VS Code/Zed, where a
    /// theme carries its own syntax colors); otherwise falls back by background brightness.
    fn theme_for(&self, app_theme: &iced::Theme) -> &Theme {
        use two_face::theme::EmbeddedThemeName as Name;
        let matching = match app_theme {
            iced::Theme::Dracula => Some(Name::Dracula),
            iced::Theme::Nord => Some(Name::Nord),
            iced::Theme::SolarizedLight => Some(Name::SolarizedLight),
            iced::Theme::SolarizedDark => Some(Name::SolarizedDark),
            iced::Theme::GruvboxLight => Some(Name::GruvboxLight),
            iced::Theme::GruvboxDark => Some(Name::GruvboxDark),
            _ => None,
        };
        if let Some(name) = matching {
            return self.extra_themes.get(name);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_typescript_and_related_extensions() {
        let highlighter = Highlighter::new();
        for extension in ["ts", "mts", "cts", "TS"] {
            assert_eq!(highlighter.language_name(extension), "TypeScript");
        }
        for extension in ["tsx", "jsx", "js", "mjs", "cjs", "json", "yaml", "yml", "toml", "rs"] {
            assert_ne!(highlighter.language_name(extension), "Plain Text", "{extension}");
        }
        assert_eq!(highlighter.language_name("unknown-extension"), "Plain Text");
    }

    #[test]
    fn typescript_and_tsx_get_colors_without_losing_text() {
        let highlighter = Highlighter::new();
        for (extension, source) in [
            ("ts", "interface User { name: string }\nconst user: User = { name: \"Ada\" };\n"),
            ("tsx", "export const View = () => <div title=\"hello\">{42}</div>;\n"),
        ] {
            for theme in iced::Theme::ALL {
                let spans = highlighter.highlight(source, extension, theme);
                assert_eq!(spans.iter().map(|(text, _)| text.as_str()).collect::<String>(), source);
                let colors: std::collections::HashSet<_> = spans.iter().filter_map(|(_, attrs)| attrs.color_opt).collect();
                assert!(colors.len() > 2, "{extension} should color different token types");
            }
        }
    }
}
