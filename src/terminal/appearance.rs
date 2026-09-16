use iced::{Color, Font};
use serde_json::Value;
use std::sync::OnceLock;

pub(super) struct Appearance {
    pub font: Font,
    pub font_size: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub cursor: Option<Color>,
    pub ansi: [Option<Color>; 16],
}

pub(super) fn get() -> &'static Appearance {
    static APPEARANCE: OnceLock<Appearance> = OnceLock::new();
    APPEARANCE.get_or_init(|| {
        let settings = windows_terminal_settings().unwrap_or(Value::Null);
        let (name, size, foreground, background, cursor, ansi) = settings_values(&settings);
        // Iced font handles require static names; this allocation happens once per app.
        let font = name.map_or(Font::MONOSPACE, |name| {
            Font::with_name(Box::leak(name.into_boxed_str()))
        });
        let mut font_system = iced::advanced::graphics::text::font_system()
            .write()
            .expect("font system");
        let fonts = font_system.raw();
        let family = match font.family {
            iced::font::Family::Name(name) => cosmic_text::Family::Name(name),
            _ => cosmic_text::Family::Monospace,
        };
        let cell_height = (size * 1.3).ceil();
        let mut buffer =
            cosmic_text::Buffer::new(fonts, cosmic_text::Metrics::new(size, cell_height));
        buffer.set_wrap(fonts, cosmic_text::Wrap::None);
        buffer.set_text(
            fonts,
            "M",
            &cosmic_text::Attrs::new().family(family),
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(fonts, false);
        let cell_width = buffer
            .layout_runs()
            .next()
            .map(|run| run.line_w)
            .filter(|w| w.is_finite() && *w > 0.0)
            .unwrap_or(size * 0.6);
        Appearance {
            font,
            font_size: size,
            cell_width,
            cell_height,
            foreground,
            background,
            cursor,
            ansi,
        }
    })
}

type SettingsValues = (
    Option<String>,
    f32,
    Option<Color>,
    Option<Color>,
    Option<Color>,
    [Option<Color>; 16],
);

fn settings_values(settings: &Value) -> SettingsValues {
    let defaults = &settings["profiles"]["defaults"];
    let profile = settings["profiles"]["list"]
        .as_array()
        .and_then(|profiles| {
            profiles.iter().find(|profile| {
                settings["defaultProfile"]
                    .as_str()
                    .is_some_and(|id| profile["guid"].as_str() == Some(id))
            })
        })
        .unwrap_or(&Value::Null);
    let font_value = |key| {
        profile["font"]
            .get(key)
            .or_else(|| defaults["font"].get(key))
    };
    let name = font_value("face")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Windows Terminal's font size is in points; Iced uses logical pixels.
    let size = font_value("size")
        .and_then(Value::as_f64)
        .unwrap_or(if cfg!(windows) { 12.0 } else { 10.5 }) as f32
        * 96.0
        / 72.0;
    let size = if size.is_finite() {
        size.clamp(8.0, 48.0)
    } else {
        16.0
    };
    let scheme_name = profile
        .get("colorScheme")
        .or_else(|| defaults.get("colorScheme"));
    let scheme_name =
        scheme_name.and_then(|value| value.as_str().or_else(|| value["dark"].as_str()));
    let scheme = settings["schemes"]
        .as_array()
        .and_then(|schemes| {
            schemes
                .iter()
                .find(|scheme| scheme["name"].as_str() == scheme_name)
        })
        .unwrap_or(&Value::Null);
    let color = |key| {
        profile
            .get(key)
            .or_else(|| defaults.get(key))
            .or_else(|| scheme.get(key))
            .and_then(Value::as_str)
            .and_then(parse_color)
    };
    let keys = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "purple",
        "cyan",
        "white",
        "brightBlack",
        "brightRed",
        "brightGreen",
        "brightYellow",
        "brightBlue",
        "brightPurple",
        "brightCyan",
        "brightWhite",
    ];
    (
        name,
        size,
        color("foreground"),
        color("background"),
        color("cursorColor"),
        keys.map(|key| scheme[key].as_str().and_then(parse_color)),
    )
}

fn parse_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let rgb = u32::from_str_radix(hex, 16).ok()?;
    Some(Color::from_rgb8(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ))
}

fn windows_terminal_settings() -> Option<Value> {
    #[cfg(windows)]
    {
        let root = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
        for relative in [
            "Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json",
            "Microsoft/Windows Terminal/settings.json",
            "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
        ] {
            if let Ok(text) = std::fs::read_to_string(root.join(relative)) {
                if let Ok(settings) = serde_json::from_str(&text) {
                    return Some(settings);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_overrides_inherit_font_and_scheme_defaults() {
        let settings = serde_json::json!({
            "defaultProfile": "pwsh", "profiles": {
                "defaults": {"font": {"face": "JetBrainsMono Nerd Font", "size": 12}, "colorScheme": "Custom"},
                "list": [{"guid": "pwsh", "font": {"size": 15}, "background": "#010203"}]
            },
            "schemes": [{"name": "Custom", "foreground": "#AABBCC", "background": "#112233", "red": "#FF0000"}]
        });
        let (name, size, fg, bg, _, ansi) = settings_values(&settings);
        assert_eq!(name.as_deref(), Some("JetBrainsMono Nerd Font"));
        assert_eq!(size, 20.0);
        assert_eq!(fg, parse_color("#AABBCC"));
        assert_eq!(bg, parse_color("#010203"));
        assert_eq!(ansi[1], parse_color("#FF0000"));
    }
}
