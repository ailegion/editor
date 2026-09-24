//! Which language server handles which file, and where its binary lives.
//!
//! Servers are external programs, never linked in. Lookup order is the editor's own managed
//! directory (`~/.config/editor/lsp/<id>/<tag>/`, filled by [`super::install`]) and then
//! `PATH`, so a user's own installation, e.g. rustup's rust-analyzer, wins when present.
use std::path::{Path, PathBuf};

pub struct Server {
    pub id: &'static str,
    pub name: &'static str,
    /// Executable name without the Windows `.exe`.
    pub binary: &'static str,
    pub args: &'static [&'static str],
    pub languages: &'static [&'static str],
    /// GitHub `owner/repo` whose latest release provides the binary.
    pub repo: &'static str,
    /// Release asset per platform, in [`PLATFORMS`] order; `{tag}` is the release tag.
    pub assets: [&'static str; 4],
}

pub const PLATFORMS: [&str; 4] = ["windows-x86_64", "linux-x86_64", "macos-aarch64", "macos-x86_64"];

pub const SERVERS: [Server; 5] = [
    Server {
        id: "rust-analyzer", name: "rust-analyzer", binary: "rust-analyzer", args: &[],
        languages: &["rust"], repo: "rust-lang/rust-analyzer",
        assets: ["rust-analyzer-x86_64-pc-windows-msvc.zip", "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            "rust-analyzer-aarch64-apple-darwin.gz", "rust-analyzer-x86_64-apple-darwin.gz"],
    },
    Server {
        id: "ruff", name: "Ruff", binary: "ruff", args: &["server"],
        languages: &["python"], repo: "astral-sh/ruff",
        assets: ["ruff-x86_64-pc-windows-msvc.zip", "ruff-x86_64-unknown-linux-gnu.tar.gz",
            "ruff-aarch64-apple-darwin.tar.gz", "ruff-x86_64-apple-darwin.tar.gz"],
    },
    Server {
        id: "biome", name: "Biome", binary: "biome", args: &["lsp-proxy"],
        languages: &["javascript", "javascriptreact", "typescript", "typescriptreact", "json", "jsonc", "css"],
        repo: "biomejs/biome",
        assets: ["biome-win32-x64.exe", "biome-linux-x64", "biome-darwin-arm64", "biome-darwin-x64"],
    },
    Server {
        id: "just-lsp", name: "just-lsp", binary: "just-lsp", args: &[],
        languages: &["just"], repo: "terror/just-lsp",
        assets: ["just-lsp-{tag}-x86_64-pc-windows-msvc.zip", "just-lsp-{tag}-x86_64-unknown-linux-gnu.tar.gz",
            "just-lsp-{tag}-aarch64-apple-darwin.tar.gz", "just-lsp-{tag}-x86_64-apple-darwin.tar.gz"],
    },
    Server {
        id: "emmylua", name: "EmmyLua", binary: "emmylua_ls", args: &[],
        languages: &["lua"], repo: "EmmyLuaLs/emmylua-analyzer-rust",
        assets: ["emmylua_ls-win32-x64.zip", "emmylua_ls-linux-x64.tar.gz",
            "emmylua_ls-darwin-arm64.tar.gz", "emmylua_ls-darwin-x64.tar.gz"],
    },
];

/// LSP language identifier for `path`, if a server here covers it.
pub fn language_for(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    if name.eq_ignore_ascii_case("justfile") || name.eq_ignore_ascii_case(".justfile") {
        return Some("just");
    }
    Some(match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "json" => "json",
        "jsonc" => "jsonc",
        "css" => "css",
        "just" => "just",
        "lua" => "lua",
        _ => return None,
    })
}

pub fn server_for(language: &str) -> Option<&'static Server> {
    SERVERS.iter().find(|server| server.languages.contains(&language))
}

pub fn by_id(id: &str) -> Option<&'static Server> {
    SERVERS.iter().find(|server| server.id == id)
}

impl Server {
    pub fn binary_file(&self) -> String {
        if cfg!(windows) { format!("{}.exe", self.binary) } else { self.binary.to_string() }
    }

    /// Where installed copies live; one subdirectory per release tag.
    pub fn managed_dir(&self) -> Option<PathBuf> {
        Some(crate::config_path("lsp")?.join(self.id))
    }

    pub fn asset_for(&self, platform: &str, tag: &str) -> Option<String> {
        let index = PLATFORMS.iter().position(|p| *p == platform)?;
        Some(self.assets[index].replace("{tag}", tag))
    }

    /// The binary to run: the newest managed install, else the first hit on `PATH`.
    pub fn locate(&self) -> Option<PathBuf> {
        let file = self.binary_file();
        if let Some(entries) = self.managed_dir().and_then(|dir| std::fs::read_dir(dir).ok()) {
            let mut tags: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.join(&file).is_file())
                .collect();
            tags.sort();
            if let Some(newest) = tags.pop() { return Some(newest.join(&file)); }
        }
        std::env::var_os("PATH")?
            .to_str()
            .and_then(|path| std::env::split_paths(path).map(|dir| dir.join(&file)).find(|candidate| candidate.is_file()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_map_to_servers_and_assets_follow_the_platform() {
        assert_eq!(language_for(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_for(Path::new("app/View.tsx")), Some("typescriptreact"));
        assert_eq!(language_for(Path::new("Justfile")), Some("just"));
        assert_eq!(language_for(Path::new("notes.txt")), None);
        assert_eq!(server_for("python").unwrap().id, "ruff");
        assert_eq!(server_for("typescript").unwrap().id, "biome");
        let just = by_id("just-lsp").unwrap();
        assert_eq!(just.asset_for("linux-x86_64", "0.9.0").as_deref(), Some("just-lsp-0.9.0-x86_64-unknown-linux-gnu.tar.gz"));
        assert_eq!(just.asset_for("freebsd", "0.9.0"), None);
        for server in &SERVERS {
            assert_eq!(server.languages.iter().filter_map(|l| server_for(l)).filter(|s| s.id != server.id).count(), 0, "{} overlaps another server", server.id);
        }
    }
}
