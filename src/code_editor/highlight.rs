//! Syntect-driven syntax highlighting; colors come from the current theme's `syntax`.

use cosmic_text::{Attrs, Color as CosmicColor};
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter as ThemeHighlighter, Style, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// The parser's and the styler's state at a line boundary. Highlighting can resume from it,
/// and when two passes reach the same boundary in the same state, the lines after it come
/// out identical -- which is what lets an edit re-highlight only the lines around it.
#[derive(Clone, PartialEq)]
pub struct LineState {
    parse: ParseState,
    style: HighlightState,
}

/// The text attributes for a syntect style: its foreground colour.
pub fn attrs_for(style: Style) -> Attrs<'static> {
    let fg = style.foreground;
    Attrs::new().color(CosmicColor::rgba(fg.r, fg.g, fg.b, fg.a))
}

/// Loads syntax definitions once and turns source text into `cosmic_text` rich-text spans,
/// so `Buffer` can feed them straight into `Buffer::set_rich_text`.
pub struct Highlighter {
    syntax_set: SyntaxSet,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: two_face::syntax::extra_no_newlines(),
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

    /// Highlights `text`, returning a flat sequence of `(chunk, attrs)` spans covering the
    /// whole document (line breaks embedded as `"\n"` chunks), ready for
    /// `cosmic_text::Buffer::set_rich_text`. Tests use it to check the line-by-line
    /// colouring the editor does against this older way of applying colours.
    #[cfg(test)]
    pub fn highlight(
        &self,
        text: &str,
        extension: &str,
        theme: &Theme,
    ) -> Vec<(String, Attrs<'static>)> {
        self.highlight_lines(text.split('\n'), extension, theme)
    }

    /// [`Self::highlight`] for text already split into lines (none containing `'\n'`).
    #[cfg(test)]
    pub fn highlight_lines<'a>(
        &self,
        lines: impl Iterator<Item = &'a str>,
        extension: &str,
        theme: &Theme,
    ) -> Vec<(String, Attrs<'static>)> {
        let styles = ThemeHighlighter::new(theme);
        let mut state = self.start(extension, theme);

        let lines: Vec<&str> = lines.collect();
        let last = lines.len().saturating_sub(1);

        let mut spans = Vec::new();
        for (i, line) in lines.into_iter().enumerate() {
            for (style, piece) in self.highlight_line(&mut state, &styles, line) {
                spans.push((piece.to_string(), attrs_for(style)));
            }
            if i != last {
                spans.push(("\n".to_string(), Attrs::new()));
            }
        }
        spans
    }

    /// The state a document starts in. This is what `syntect::easy::HighlightLines::new`
    /// sets up.
    pub fn start(&self, extension: &str, theme: &Theme) -> LineState {
        let styles = ThemeHighlighter::new(theme);
        LineState {
            parse: ParseState::new(self.syntax_for(extension)),
            style: HighlightState::new(&styles, ScopeStack::new()),
        }
    }

    /// Highlights one line (no line break in it) from `state`, leaving `state` at the line's
    /// end. `styles` is `syntect::highlighting::Highlighter::new` of the theme `state` was
    /// started with. This is what `syntect::easy::HighlightLines::highlight_line` does; like
    /// it, a line the parser rejects yields nothing.
    pub fn highlight_line<'t>(&self, state: &mut LineState, styles: &ThemeHighlighter<'_>, line: &'t str) -> Vec<(Style, &'t str)> {
        let Ok(ops) = state.parse.parse_line(line, &self.syntax_set) else {
            return Vec::new();
        };
        HighlightIterator::new(&mut state.style, &ops, line, styles).collect()
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
            for theme in [crate::theme::EditorTheme::default_dark(), crate::theme::EditorTheme::default_light()] {
                let spans = highlighter.highlight(source, extension, &theme.syntax);
                assert_eq!(spans.iter().map(|(text, _)| text.as_str()).collect::<String>(), source);
                let colors: std::collections::HashSet<_> = spans.iter().filter_map(|(_, attrs)| attrs.color_opt).collect();
                assert!(colors.len() > 2, "{extension} should color different token types");
            }
        }
    }
}
