//! Reads VS Code color theme files: `colors` (UI colors by key) and `tokenColors` (TextMate
//! scope rules, which `syntect` understands natively).

use std::collections::HashMap;
use std::path::{Component, Path};
use std::str::FromStr;

use serde_json::Value;
use syntect::highlighting::{ScopeSelectors, StyleModifier, ThemeItem};

/// Includes deeper than this are treated as a cycle.
const MAX_INCLUDE_DEPTH: usize = 8;

/// A theme file with its `include` chain merged in (included file first, then overrides).
#[derive(Default)]
pub struct ThemeFile {
    pub name: Option<String>,
    /// The file's own `"type"`. Some published themes get this wrong, so the extension
    /// manifest's `uiTheme` takes precedence (see `ThemeRegistry::load`).
    pub kind: Option<String>,
    pub colors: HashMap<String, [u8; 4]>,
    pub token_colors: Vec<ThemeItem>,
    /// `foreground` of a scope-less `tokenColors` entry (the legacy global setting).
    pub token_foreground: Option<[u8; 4]>,
}

/// Parses the theme at `path`. `read` loads a file by path (the same kind of path as
/// `path`); `include`s are resolved relative to the including file.
pub fn parse(path: &Path, read: &dyn Fn(&Path) -> Option<String>) -> Result<ThemeFile, String> {
    parse_at_depth(path, read, 0)
}

fn parse_at_depth(path: &Path, read: &dyn Fn(&Path) -> Option<String>, depth: usize) -> Result<ThemeFile, String> {
    let text = read(path).ok_or_else(|| format!("cannot read {}", path.display()))?;
    let value: Value = serde_json::from_str(&super::jsonc::strip(&text))
        .map_err(|err| format!("{}: {err}", path.display()))?;

    let mut theme = match value.get("include").and_then(Value::as_str) {
        Some(_) if depth >= MAX_INCLUDE_DEPTH => return Err(format!("{}: include chain too deep", path.display())),
        Some(include) => {
            let mut included = path.parent().unwrap_or(Path::new("")).to_path_buf();
            for part in Path::new(include).components() {
                match part {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        included.pop();
                    }
                    other => included.push(other),
                }
            }
            parse_at_depth(&included, read, depth + 1)?
        }
        None => ThemeFile::default(),
    };

    if let Some(name) = value.get("name").and_then(Value::as_str) {
        theme.name = Some(name.to_owned());
    }
    if let Some(kind) = value.get("type").and_then(Value::as_str) {
        theme.kind = Some(kind.to_owned());
    }
    if let Some(colors) = value.get("colors").and_then(Value::as_object) {
        for (key, color) in colors {
            match color.as_str().and_then(parse_color) {
                Some(color) => {
                    theme.colors.insert(key.clone(), color);
                }
                // `null` (or anything unparseable) resets the key to its default.
                None => {
                    theme.colors.remove(key);
                }
            }
        }
    }
    for token in value.get("tokenColors").and_then(Value::as_array).into_iter().flatten() {
        let foreground = token
            .pointer("/settings/foreground")
            .and_then(Value::as_str)
            .and_then(parse_color);
        let scopes = match token.get("scope") {
            Some(Value::String(scope)) => scope.clone(),
            Some(Value::Array(scopes)) => scopes.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
            _ => {
                if foreground.is_some() {
                    theme.token_foreground = foreground;
                }
                continue;
            }
        };
        // Only foreground is used for now (no font styles, and VS Code ignores token backgrounds).
        let Some(foreground) = foreground else { continue };
        // Keep the selectors syntect can parse rather than dropping the whole rule.
        let selectors: Vec<&str> = scopes
            .split(',')
            .map(str::trim)
            .filter(|selector| !selector.is_empty() && ScopeSelectors::from_str(selector).is_ok())
            .collect();
        if selectors.is_empty() {
            continue;
        }
        let Ok(scope) = ScopeSelectors::from_str(&selectors.join(",")) else { continue };
        let [r, g, b, a] = foreground;
        theme.token_colors.push(ThemeItem {
            scope,
            style: StyleModifier {
                foreground: Some(syntect::highlighting::Color { r, g, b, a }),
                background: None,
                font_style: None,
            },
        });
    }
    Ok(theme)
}

/// `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA` as RGBA bytes.
pub fn parse_color(value: &str) -> Option<[u8; 4]> {
    let hex = value.trim().strip_prefix('#')?;
    let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok();
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        3 | 4 => {
            let alpha = if hex.len() == 4 { digit(3)? * 17 } else { 255 };
            Some([digit(0)? * 17, digit(1)? * 17, digit(2)? * 17, alpha])
        }
        6 | 8 => {
            let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
            Some([byte(0)?, byte(2)?, byte(4)?, alpha])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn files(entries: &[(&str, &str)]) -> impl Fn(&Path) -> Option<String> {
        let map: HashMap<PathBuf, String> =
            entries.iter().map(|(path, text)| (PathBuf::from(path), text.to_string())).collect();
        move |path: &Path| map.get(path).cloned()
    }

    #[test]
    fn parses_colors_in_all_hex_forms() {
        assert_eq!(parse_color("#abc"), Some([0xaa, 0xbb, 0xcc, 255]));
        assert_eq!(parse_color("#abcd"), Some([0xaa, 0xbb, 0xcc, 0xdd]));
        assert_eq!(parse_color("#A1B2C3"), Some([0xa1, 0xb2, 0xc3, 255]));
        assert_eq!(parse_color("#A1B2C380"), Some([0xa1, 0xb2, 0xc3, 0x80]));
        for bad in ["", "abc", "#ab", "#abcde", "#ggg", "red"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn include_is_merged_with_overrides() {
        let read = files(&[
            ("t/base.json", r##"{ "name": "Base", "colors": { "a": "#111111", "b": "#222222", "c": "#333333" },
                "tokenColors": [ { "scope": "comment", "settings": { "foreground": "#010101" } } ] }"##),
            ("t/top.json", r##"{
                // JSONC like real theme files
                "name": "Top", "include": "./base.json",
                "colors": { "b": "#bbbbbb", "c": null, },
                "tokenColors": [
                    { "settings": { "foreground": "#fefefe" } },
                    { "scope": ["string", "keyword.control"], "settings": { "foreground": "#020202", "fontStyle": "bold" } },
                    { "scope": "constant", "settings": { "fontStyle": "italic" } },
                ],
            }"##),
        ]);
        let theme = parse(Path::new("t/top.json"), &read).unwrap();
        assert_eq!(theme.name.as_deref(), Some("Top"));
        assert_eq!(theme.colors.get("a"), Some(&[0x11, 0x11, 0x11, 255]));
        assert_eq!(theme.colors.get("b"), Some(&[0xbb, 0xbb, 0xbb, 255]));
        assert_eq!(theme.colors.get("c"), None);
        assert_eq!(theme.token_foreground, Some([0xfe, 0xfe, 0xfe, 255]));
        // Included rules first; the italic-only rule has no foreground and is skipped.
        assert_eq!(theme.token_colors.len(), 2);
        assert_eq!(theme.token_colors[1].scope.selectors.len(), 2);
    }

    #[test]
    fn reports_missing_include_and_cycles() {
        let missing = files(&[("a.json", r#"{ "include": "./nope.json" }"#)]);
        assert!(parse(Path::new("a.json"), &missing).is_err());
        let cycle = files(&[("a.json", r#"{ "include": "./a.json" }"#)]);
        assert!(parse(Path::new("a.json"), &cycle).is_err());
    }
}
