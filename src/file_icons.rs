//! Filename-aware tree icons. Text badges distinguish related source languages.
use std::path::Path;
use iced::Color;
use iced_swdir_tree::{IconRole, IconSpec, IconTheme, LucideTheme};

#[derive(Debug)]
pub struct FileIcons;

fn badge(path: &Path) -> Option<(&'static str, Color)> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    let (label, color) = match name.as_str() {
        "dockerfile" | "containerfile" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yaml" | "compose.yml" => ("▣", 0x429bd6),
        "package.json" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lock" | "bun.lockb" => ("⬡", 0xc97965),
        ".gitignore" | ".gitattributes" | ".gitmodules" => ("git", 0xd77b62),
        "license" | "licence" => ("©", 0xc6a663),
        _ if name == ".env" || name.starts_with(".env.") => ("⚙", 0xc6a663),
        _ if name.starts_with("dockerfile.") => ("▣", 0x429bd6),
        _ => match ext.as_str() {
            "ts" | "mts" | "cts" => ("TS", 0x429bd6),
            "tsx" => ("TSX", 0x65b9cf),
            "js" | "mjs" | "cjs" => ("JS", 0xc6a663),
            "jsx" => ("JSX", 0x65b9cf),
            "json" | "jsonc" | "json5" => ("{}", 0xc6a663),
            "yaml" | "yml" => ("Y", 0xc97985),
            "toml" | "ini" | "conf" => ("⚙", 0x929ca6),
            "rs" => ("Rs", 0xc98a68),
            "py" => ("Py", 0x729fca),
            "go" => ("Go", 0x65b9cf),
            "html" | "htm" | "xml" | "svg" => ("<>", 0xc98a68),
            "css" | "scss" | "sass" | "less" => ("#", 0xa88ac9),
            "md" | "mdx" | "txt" => ("≡", 0x729fca),
            "sh" | "bash" | "zsh" | "fish" => (">_", 0x8faf78),
            "sql" | "db" | "sqlite" => ("DB", 0xc6a663),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" => ("▧", 0xa88ac9),
            "lock" => ("⊟", 0x929ca6),
            _ => return None,
        },
    };
    Some((label, Color::from_rgb8((color >> 16) as u8, (color >> 8) as u8, color as u8)))
}

impl IconTheme for FileIcons {
    fn glyph(&self, role: IconRole) -> IconSpec { LucideTheme.glyph(role) }
    fn file_glyph(&self, path: &Path) -> IconSpec {
        let Some((label, _)) = badge(path) else { return self.glyph(IconRole::File); };
        use lucide_icons::Icon;
        let icon = match label {
            "▣" => Some(Icon::Container),
            "⬡" => Some(Icon::Package),
            "git" => Some(Icon::GitBranch),
            "⚙" => Some(Icon::Settings),
            "▧" => Some(Icon::Image),
            "≡" | "©" => Some(Icon::FileText),
            "DB" => Some(Icon::Database),
            "⊟" => Some(Icon::Lock),
            _ => None,
        };
        match icon {
            Some(icon) => {
                let glyph: char = icon.into();
                IconSpec::new(glyph.to_string()).with_size(14.0).with_font(iced::Font::with_name("lucide"))
            }
            None => IconSpec::new(label).with_size(11.0).with_font(iced::Font::MONOSPACE),
        }
    }
    fn file_color(&self, path: &Path) -> Option<Color> { badge(path).map(|(_, color)| color) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognizes_extensions_and_special_filenames() {
        for (path, label) in [("app.TSX", "TSX"), ("index.ts", "TS"), ("package.json", "⬡"), ("data.json", "{}"), (".env.local", "⚙"), ("Dockerfile", "▣"), ("docker-compose.yml", "▣")] {
            assert_eq!(badge(Path::new(path)).unwrap().0, label);
        }
        assert!(badge(Path::new("unknown.xyz")).is_none());
    }
}
