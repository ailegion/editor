//! App themes, read from VS Code color theme files so a theme sets the UI, editor and syntax
//! colors together. Dark+ and Light+ are compiled in (they are also the fallbacks); other
//! themes come from `themes/` folders laid out like VS Code extensions.

mod jsonc;
pub mod openvsx;
mod vscode;

use std::path::{Path, PathBuf};

use iced::Color;
use serde_json::Value;
use syntect::highlighting::{Theme as SyntaxTheme, ThemeItem, ThemeSettings};

pub const DEFAULT_DARK: &str = "Dark+";
pub const DEFAULT_LIGHT: &str = "Light+";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dark,
    Light,
}

impl Kind {
    /// From an extension manifest's `uiTheme` or a theme file's `type`.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "vs-dark" | "hc-black" | "dark" | "hc" | "hcDark" => Some(Kind::Dark),
            "vs" | "hc-light" | "light" | "hcLight" => Some(Kind::Light),
            _ => None,
        }
    }
}

/// Colors for the code editor canvas (see `code_editor::Style`).
#[derive(Debug, Clone)]
pub struct EditorColors {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub selection: Color,
    pub line_highlight: Color,
    pub gutter_background: Color,
    pub line_number: Color,
    pub bracket_match: Color,
    pub gutter_added: Color,
    pub gutter_modified: Color,
    pub gutter_deleted: Color,
}

/// A loaded theme.
#[derive(Debug, Clone)]
pub struct EditorTheme {
    pub name: String,
    pub kind: Kind,
    /// For iced widgets, built from the theme's colors.
    pub iced: iced::Theme,
    pub editor: EditorColors,
    pub syntax: SyntaxTheme,
}

impl EditorTheme {
    pub fn default_dark() -> Self {
        Self::embedded(DEFAULT_DARK, Kind::Dark, "dark_plus.json")
    }

    pub fn default_light() -> Self {
        Self::embedded(DEFAULT_LIGHT, Kind::Light, "light_plus.json")
    }

    pub fn default_for(kind: Kind) -> Self {
        match kind {
            Kind::Dark => Self::default_dark(),
            Kind::Light => Self::default_light(),
        }
    }

    fn embedded(name: &str, kind: Kind, file: &str) -> Self {
        let read = |path: &Path| embedded_file(path).map(str::to_owned);
        // Covered by `default_themes_match_vscode`.
        let file = vscode::parse(Path::new(file), &read).expect("compiled-in theme parses");
        Self::build(name.to_owned(), kind, file, Vec::new)
    }

    /// `fallback_scopes` supplies syntax rules for themes that define none.
    fn build(name: String, kind: Kind, file: vscode::ThemeFile, fallback_scopes: impl FnOnce() -> Vec<ThemeItem>) -> Self {
        // A theme's own value for the first key that has one, else VS Code's default for the
        // first key that has one.
        let lookup = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| file.colors.get(*key).copied())
                .or_else(|| keys.iter().find_map(|key| default_color(key, kind)))
                .map(to_iced)
                .unwrap_or(Color::BLACK)
        };
        let base = if kind == Kind::Dark { Color::BLACK } else { Color::WHITE };
        let background = blend(lookup(&["editor.background"]), base);
        let foreground = file
            .colors
            .get("editor.foreground")
            .or(file.token_foreground.as_ref())
            .copied()
            .map(to_iced)
            .unwrap_or_else(|| lookup(&["editor.foreground"]));
        let opaque = |color: Color| blend(color, background);

        let editor = EditorColors {
            background,
            foreground,
            cursor: lookup(&["editorCursor.foreground"]),
            selection: lookup(&["editor.selectionBackground"]),
            line_highlight: lookup(&["editor.lineHighlightBackground", "editor.lineHighlightBorder"]),
            gutter_background: opaque(lookup(&["editorGutter.background", "editor.background"])),
            line_number: lookup(&["editorLineNumber.foreground"]),
            bracket_match: lookup(&["editorBracketMatch.border"]),
            gutter_added: lookup(&["editorGutter.addedBackground"]),
            gutter_modified: lookup(&["editorGutter.modifiedBackground"]),
            gutter_deleted: lookup(&["editorGutter.deletedBackground", "editorError.foreground"]),
        };
        let palette = iced::theme::Palette {
            background,
            text: opaque(foreground),
            primary: opaque(lookup(&["button.background", "focusBorder"])),
            success: opaque(lookup(&["terminal.ansiGreen"])),
            warning: opaque(lookup(&["editorWarning.foreground", "terminal.ansiYellow"])),
            danger: opaque(lookup(&["editorError.foreground", "terminal.ansiRed"])),
        };
        let scopes = if file.token_colors.is_empty() { fallback_scopes() } else { file.token_colors };
        let syntax = SyntaxTheme {
            name: Some(name.clone()),
            author: None,
            settings: ThemeSettings {
                foreground: Some(to_syntect(foreground)),
                background: Some(to_syntect(background)),
                ..ThemeSettings::default()
            },
            scopes,
        };
        Self { iced: iced::Theme::custom(name.clone(), palette), name, kind, editor, syntax }
    }
}

/// VS Code's built-in defaults (from its color registry) for the keys used above, applied
/// when a theme doesn't set them -- many themes, including Dark+, rely on these.
fn default_color(key: &str, kind: Kind) -> Option<[u8; 4]> {
    let (dark, light) = match key {
        "editor.background" => ("#1E1E1E", "#FFFFFF"),
        "editor.foreground" => ("#BBBBBB", "#333333"),
        "editorCursor.foreground" => ("#AEAFAD", "#000000"),
        "editor.selectionBackground" => ("#264F78", "#ADD6FF"),
        "editor.lineHighlightBorder" => ("#282828", "#EEEEEE"),
        "editorLineNumber.foreground" => ("#858585", "#237893"),
        "editorBracketMatch.border" => ("#888888", "#B9B9B9"),
        "editorGutter.addedBackground" => ("#487E02", "#48985D"),
        "editorGutter.modifiedBackground" => ("#1B81A8", "#2090D3"),
        "editorError.foreground" => ("#F14C4C", "#E51400"),
        "editorWarning.foreground" => ("#CCA700", "#BF8803"),
        "button.background" => ("#0E639C", "#007ACC"),
        "terminal.ansiGreen" => ("#0DBC79", "#107C10"),
        _ => return None,
    };
    vscode::parse_color(if kind == Kind::Dark { dark } else { light })
}

fn embedded_file(path: &Path) -> Option<&'static str> {
    Some(match path.file_name()?.to_str()? {
        "dark_plus.json" => include_str!("default/dark_plus.json"),
        "dark_vs.json" => include_str!("default/dark_vs.json"),
        "light_plus.json" => include_str!("default/light_plus.json"),
        "light_vs.json" => include_str!("default/light_vs.json"),
        _ => return None,
    })
}

fn to_iced([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(r, g, b, f32::from(a) / 255.0)
}

fn to_syntect(color: Color) -> syntect::highlighting::Color {
    let [r, g, b, a] = color.into_rgba8();
    syntect::highlighting::Color { r, g, b, a }
}

/// `top` composited over opaque `bottom`.
fn blend(top: Color, bottom: Color) -> Color {
    let mix = |t: f32, b: f32| t * top.a + b * (1.0 - top.a);
    Color::from_rgb(mix(top.r, bottom.r), mix(top.g, bottom.g), mix(top.b, bottom.b))
}

/// Names saved before themes came from files (iced's built-in themes) that have a bundled
/// equivalent under a different name.
pub fn migrate_name(name: &str) -> &str {
    match name {
        "Dark" => DEFAULT_DARK,
        "Light" => DEFAULT_LIGHT,
        "Dracula" => "Dracula Theme",
        "Gruvbox Dark" => "Gruvbox Dark Medium",
        "Gruvbox Light" => "Gruvbox Light Medium",
        other => other,
    }
}

#[derive(Debug, Clone)]
enum Source {
    Embedded,
    File(PathBuf),
}

#[derive(Debug, Clone)]
struct ThemeEntry {
    name: String,
    /// From the extension manifest; `None` for loose theme files.
    kind: Option<Kind>,
    source: Source,
}

/// The themes available to pick. Only names and locations are read up front; a theme file is
/// parsed when it's selected.
pub struct ThemeRegistry {
    entries: Vec<ThemeEntry>,
}

impl ThemeRegistry {
    pub fn load() -> Self {
        Self::from_dirs(&theme_dirs())
    }

    /// `dirs` in load order; themes from a folder marked `true` replace same-named file
    /// themes found earlier (the compiled-in defaults can't be replaced).
    fn from_dirs(dirs: &[(PathBuf, bool)]) -> Self {
        let mut entries = vec![
            ThemeEntry { name: DEFAULT_DARK.into(), kind: Some(Kind::Dark), source: Source::Embedded },
            ThemeEntry { name: DEFAULT_LIGHT.into(), kind: Some(Kind::Light), source: Source::Embedded },
        ];
        for (dir, overrides) in dirs {
            for entry in scan(dir) {
                match entries.iter().position(|existing| existing.name == entry.name) {
                    None => entries.push(entry),
                    Some(i) if *overrides && !matches!(entries[i].source, Source::Embedded) => entries[i] = entry,
                    Some(_) => {}
                }
            }
        }
        entries.sort_by_key(|entry| entry.name.to_lowercase());
        Self { entries }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    pub fn load_theme(&self, name: &str) -> Result<EditorTheme, String> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| format!("no theme named \"{name}\""))?;
        let path = match &entry.source {
            Source::Embedded => return Ok(EditorTheme::default_for(entry.kind.unwrap_or(Kind::Dark))),
            Source::File(path) => path,
        };
        let file = vscode::parse(path, &|path| std::fs::read_to_string(path).ok())?;
        // The manifest's `uiTheme` is what VS Code goes by (e.g. Tokyo Night Light's file says
        // "dark"); loose files fall back to their `type`, then to the background brightness.
        let kind = entry
            .kind
            .or_else(|| file.kind.as_deref().and_then(Kind::parse))
            .unwrap_or_else(|| match file.colors.get("editor.background") {
                Some([r, g, b, _]) if u32::from(*r) + u32::from(*g) + u32::from(*b) > 382 => Kind::Light,
                _ => Kind::Dark,
            });
        Ok(EditorTheme::build(entry.name.clone(), kind, file, || EditorTheme::default_for(kind).syntax.scopes))
    }
}

/// Theme folders in load order; `true` marks the user folder.
fn theme_dirs() -> Vec<(PathBuf, bool)> {
    let mut dirs = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push((exe_dir.join("themes"), false));
        // macOS app bundles keep resources in Contents/Resources.
        dirs.push((exe_dir.join("../Resources/themes"), false));
    }
    if cfg!(debug_assertions) {
        // `cargo run` starts the binary from target/, away from the bundled theme files.
        dirs.push((Path::new(env!("CARGO_MANIFEST_DIR")).join("themes"), false));
    }
    if let Some(user) = user_themes_dir() {
        dirs.push((user, true));
    }
    dirs
}

/// Where installed themes go.
pub fn user_themes_dir() -> Option<PathBuf> {
    crate::config_path("themes")
}

/// Names of the themes in an extension folder that load.
fn loadable_themes(dir: &Path) -> Vec<String> {
    let registry = ThemeRegistry { entries: scan_extension(dir) };
    registry
        .names()
        .filter(|name| registry.load_theme(name).is_ok())
        .map(str::to_owned)
        .collect()
}

/// Extension folders (`<dir>/<extension>/package.json`) and loose theme files (`<dir>/*.json`).
fn scan(dir: &Path) -> Vec<ThemeEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = read_dir.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    paths.sort();
    let mut entries = Vec::new();
    for path in paths {
        // Dot folders are in-progress installs (see `openvsx::install`).
        if path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.')) {
            continue;
        }
        if path.is_dir() {
            entries.extend(scan_extension(&path));
        } else if path.extension().is_some_and(|ext| ext == "json") {
            if let Some(name) = theme_name(&path) {
                entries.push(ThemeEntry { name, kind: None, source: Source::File(path) });
            }
        }
    }
    entries
}

fn scan_extension(dir: &Path) -> Vec<ThemeEntry> {
    let Some(manifest) = read_json(&dir.join("package.json")) else { return Vec::new() };
    let themes = manifest.pointer("/contributes/themes").and_then(Value::as_array);
    themes
        .into_iter()
        .flatten()
        .filter_map(|theme| {
            let path = dir.join(theme.get("path")?.as_str()?);
            // `%key%` labels are localization keys, not names.
            let label = theme.get("label").and_then(Value::as_str).filter(|label| !label.starts_with('%'));
            let name = label.map(str::to_owned).or_else(|| theme_name(&path))?;
            let kind = theme.get("uiTheme").and_then(Value::as_str).and_then(Kind::parse);
            Some(ThemeEntry { name, kind, source: Source::File(path) })
        })
        .collect()
}

/// The file's `name`, else its file stem; `None` if it isn't readable JSON.
fn theme_name(path: &Path) -> Option<String> {
    let json = read_json(path)?;
    let name = json.get("name").and_then(Value::as_str).map(str::to_owned);
    name.or_else(|| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&jsonc::strip(&std::fs::read_to_string(path).ok()?)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled() -> ThemeRegistry {
        ThemeRegistry::from_dirs(&[(Path::new(env!("CARGO_MANIFEST_DIR")).join("themes"), false)])
    }

    fn hex(color: Color) -> String {
        let [r, g, b, a] = color.into_rgba8();
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }

    #[test]
    fn default_themes_match_vscode() {
        let dark = EditorTheme::default_dark();
        assert_eq!(dark.kind, Kind::Dark);
        assert_eq!(hex(dark.editor.background), "#1E1E1EFF");
        assert_eq!(hex(dark.editor.foreground), "#D4D4D4FF");
        assert_eq!(hex(dark.editor.cursor), "#AEAFADFF");
        assert!(!dark.syntax.scopes.is_empty());

        let light = EditorTheme::default_light();
        assert_eq!(light.kind, Kind::Light);
        assert_eq!(hex(light.editor.background), "#FFFFFFFF");
        assert!(!light.syntax.scopes.is_empty());
    }

    #[test]
    fn every_default_color_parses() {
        let keys = [
            "editor.background", "editor.foreground", "editorCursor.foreground", "editor.selectionBackground",
            "editor.lineHighlightBorder", "editorLineNumber.foreground", "editorBracketMatch.border",
            "editorGutter.addedBackground", "editorGutter.modifiedBackground", "editorError.foreground",
            "editorWarning.foreground", "button.background", "terminal.ansiGreen",
        ];
        for key in keys {
            for kind in [Kind::Dark, Kind::Light] {
                assert!(default_color(key, kind).is_some(), "{key} {kind:?}");
            }
        }
    }

    #[test]
    fn all_bundled_themes_load_and_highlight() {
        let registry = bundled();
        let expected = [
            ("Dark+", Kind::Dark), ("Light+", Kind::Light),
            ("Dracula Theme", Kind::Dark), ("Dracula Theme Soft", Kind::Dark),
            ("Catppuccin Latte", Kind::Light), ("Catppuccin Frappé", Kind::Dark),
            ("Catppuccin Macchiato", Kind::Dark), ("Catppuccin Mocha", Kind::Dark),
            ("Tokyo Night", Kind::Dark), ("Tokyo Night Storm", Kind::Dark), ("Tokyo Night Light", Kind::Light),
            ("Nord", Kind::Dark), ("Solarized Dark", Kind::Dark), ("Solarized Light", Kind::Light),
            ("Gruvbox Dark Hard", Kind::Dark), ("Gruvbox Dark Medium", Kind::Dark), ("Gruvbox Dark Soft", Kind::Dark),
            ("Gruvbox Light Hard", Kind::Light), ("Gruvbox Light Medium", Kind::Light), ("Gruvbox Light Soft", Kind::Light),
        ];
        let names: Vec<&str> = registry.names().collect();
        assert_eq!(names.len(), expected.len(), "{names:?}");
        let highlighter = crate::code_editor::Highlighter::new();
        let source = "fn main() {\n    let answer: u32 = 42; // comment\n    println!(\"{answer}\");\n}\n";
        for (name, kind) in expected {
            let theme = registry.load_theme(name).unwrap_or_else(|err| panic!("{name}: {err}"));
            assert_eq!(theme.kind, kind, "{name}");
            let spans = highlighter.highlight(source, "rs", &theme.syntax);
            let colors: std::collections::HashSet<_> = spans.iter().filter_map(|(_, attrs)| attrs.color_opt).collect();
            assert!(colors.len() > 3, "{name} should color different token types");
        }
    }

    #[test]
    fn old_iced_theme_names_resolve_to_bundled_themes() {
        let registry = bundled();
        let unbundled = ["Kanagawa Wave", "Kanagawa Dragon", "Kanagawa Lotus", "Moonfly", "Nightfly", "Oxocarbon", "Ferra"];
        for theme in iced::Theme::ALL {
            let name = theme.to_string();
            if unbundled.contains(&name.as_str()) {
                continue;
            }
            assert!(registry.load_theme(migrate_name(&name)).is_ok(), "{name}");
        }
    }

    #[test]
    fn user_themes_replace_bundled_ones_but_not_defaults() {
        let dir = std::env::temp_dir().join(format!("editor-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("nord.json"), r##"{ "name": "Nord", "colors": { "editor.background": "#FAFAFA" } }"##).unwrap();
        std::fs::write(dir.join("fake-dark.json"), r##"{ "name": "Dark+", "colors": { "editor.background": "#FF0000" } }"##).unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        let registry = ThemeRegistry::from_dirs(&[
            (Path::new(env!("CARGO_MANIFEST_DIR")).join("themes"), false),
            (dir.clone(), true),
        ]);
        let nord = registry.load_theme("Nord").unwrap();
        // Loose file without `type`: kind comes from the background.
        assert_eq!(nord.kind, Kind::Light);
        assert_eq!(hex(nord.editor.background), "#FAFAFAFF");
        // Token colors fall back to Light+'s.
        assert_eq!(nord.syntax.scopes, EditorTheme::default_light().syntax.scopes);
        assert_eq!(hex(registry.load_theme("Dark+").unwrap().editor.background), "#1E1E1EFF");
        assert!(registry.names().all(|name| name != "broken"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
