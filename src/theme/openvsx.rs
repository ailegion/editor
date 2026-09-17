//! Finding and installing color themes from Open VSX (open-vsx.org), the open VS Code
//! extension registry. Installs go to the user themes folder, one folder per extension, in
//! the layout `ThemeRegistry` reads. Everything here blocks; run it off the UI thread.

use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use serde_json::Value;

const API: &str = "https://open-vsx.org/api";
const SEARCH_SIZE: &str = "30";
const MAX_DOWNLOAD: u64 = 50 * 1024 * 1024;
/// Cap on the total size of the files extracted from one package.
const MAX_EXTRACTED: u64 = 20 * 1024 * 1024;

/// A theme extension from a search.
#[derive(Debug, Clone)]
pub struct Extension {
    pub namespace: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub version: String,
    pub downloads: u64,
    pub license: Option<String>,
    /// Names of the color themes it contributes.
    pub themes: Vec<String>,
    download_url: String,
}

impl Extension {
    /// Install folder name, `publisher.name` like VS Code's extension ids.
    pub fn id(&self) -> String {
        format!("{}.{}", self.namespace, self.name)
    }
}

/// An extension in the user themes folder.
#[derive(Debug, Clone)]
pub struct Installed {
    /// Folder name.
    pub id: String,
    pub display_name: String,
    pub themes: Vec<String>,
}

/// Theme extensions matching `query` (most downloaded first when it's empty). Only those that
/// contribute color themes are kept -- Open VSX's "Themes" category also has icon themes.
pub fn search(query: &str) -> Result<Vec<Extension>, String> {
    let query = query.trim();
    let mut params = vec![("category", "Themes"), ("size", SEARCH_SIZE)];
    if query.is_empty() {
        params.extend([("sortBy", "downloadCount"), ("sortOrder", "desc")]);
    } else {
        params.push(("query", query));
    }
    let response = get_json(&format!("{API}/-/search"), &params)?;
    let found = response["extensions"].as_array().cloned().unwrap_or_default();
    // Manifests are what tell color themes apart; fetch them in parallel, keeping order.
    Ok(std::thread::scope(|scope| {
        let handles: Vec<_> = found.iter().map(|found| scope.spawn(move || with_manifest(found))).collect();
        handles.into_iter().filter_map(|handle| handle.join().ok().flatten()).collect()
    }))
}

fn with_manifest(found: &Value) -> Option<Extension> {
    if found["deprecated"].as_bool() == Some(true) {
        return None;
    }
    let namespace = found["namespace"].as_str()?;
    let name = found["name"].as_str()?;
    let version = found["version"].as_str()?;
    let download_url = found.pointer("/files/download")?.as_str()?;
    let manifest = get_json(&format!("{API}/{namespace}/{name}/{version}/file/package.json"), &[]).ok()?;
    let themes = theme_labels(&manifest);
    if themes.is_empty() {
        return None;
    }
    Some(Extension {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        display_name: found["displayName"].as_str().unwrap_or(name).to_owned(),
        description: found["description"].as_str().unwrap_or_default().to_owned(),
        version: version.to_owned(),
        downloads: found["downloadCount"].as_u64().unwrap_or(0),
        license: manifest["license"].as_str().map(str::to_owned),
        themes,
        download_url: download_url.to_owned(),
    })
}

/// Color theme names from an extension manifest (localized `%key%` labels fall back to the
/// file name; `ThemeRegistry` reads the file's own name for those once installed).
fn theme_labels(manifest: &Value) -> Vec<String> {
    let themes = manifest.pointer("/contributes/themes").and_then(Value::as_array);
    themes
        .into_iter()
        .flatten()
        .filter_map(|theme| {
            let label = theme.get("label").and_then(Value::as_str).filter(|label| !label.starts_with('%'));
            let stem = || Path::new(theme.get("path")?.as_str()?).file_stem().map(|stem| stem.to_string_lossy().into_owned());
            label.map(str::to_owned).or_else(stem)
        })
        .collect()
}

fn get_json(url: &str, params: &[(&str, &str)]) -> Result<Value, String> {
    let mut request = ureq::get(url);
    for (key, value) in params {
        request = request.query(key, value);
    }
    let mut response = request.call().map_err(|err| format!("Open VSX request failed: {err}"))?;
    let text = response.body_mut().read_to_string().map_err(|err| err.to_string())?;
    serde_json::from_str(&text).map_err(|err| format!("unexpected Open VSX response: {err}"))
}

/// Downloads `extension` into `themes_dir/<id>`, replacing an older copy. Returns the names
/// of the themes that load.
pub fn install(extension: &Extension, themes_dir: &Path) -> Result<Vec<String>, String> {
    let mut response = ureq::get(&extension.download_url)
        .call()
        .map_err(|err| format!("download failed: {err}"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|err| format!("download failed: {err}"))?;
    install_package(&bytes, themes_dir, &extension.id())
}

/// Unpacks a `.vsix` into `themes_dir/<id>` via a staging folder, so a failed install leaves
/// any existing copy untouched.
fn install_package(bytes: &[u8], themes_dir: &Path, id: &str) -> Result<Vec<String>, String> {
    let id = folder_name(id)?;
    fs::create_dir_all(themes_dir).map_err(|err| err.to_string())?;
    // Dot-prefixed so `ThemeRegistry` never lists a half-written install.
    let staging = themes_dir.join(format!(".installing-{id}"));
    let _ = fs::remove_dir_all(&staging);
    let names = extract(bytes, &staging).and_then(|()| {
        let names = super::loadable_themes(&staging);
        if names.is_empty() { Err("the package has no usable color themes".to_owned()) } else { Ok(names) }
    });
    let names = match names {
        Ok(names) => names,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }
    };
    let dest = themes_dir.join(id);
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(|err| format!("cannot replace the installed copy: {err}"))?;
    }
    fs::rename(&staging, &dest).map_err(|err| err.to_string())?;
    Ok(names)
}

/// Extracts the manifest, license/notice files and JSON files (themes and anything they
/// include) from the package's `extension/` folder. Scripts and other files are skipped: a
/// theme needs none of them.
fn extract(bytes: &[u8], staging: &Path) -> Result<(), String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|err| format!("not a valid package: {err}"))?;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|err| format!("not a valid package: {err}"))?;
        if !file.is_file() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..` components.
        let Some(path) = file.enclosed_name() else { continue };
        let Ok(relative) = path.strip_prefix("extension") else { continue };
        if !is_wanted(relative) {
            continue;
        }
        let mut contents = Vec::new();
        (&mut file)
            .take(MAX_EXTRACTED - total + 1)
            .read_to_end(&mut contents)
            .map_err(|err| format!("not a valid package: {err}"))?;
        total += contents.len() as u64;
        if total > MAX_EXTRACTED {
            return Err("the package is too large".to_owned());
        }
        let target = staging.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::write(&target, contents).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn is_wanted(path: &Path) -> bool {
    let name = path.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    name.ends_with(".json") || ["license", "licence", "notice", "thirdpartynotices"].iter().any(|prefix| name.starts_with(prefix))
}

/// `id` if it's a single plain folder name (no separators, `..`, or leading dot).
fn folder_name(id: &str) -> Result<&str, String> {
    let mut components = Path::new(id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) if !id.starts_with('.') => Ok(id),
        _ => Err(format!("invalid extension id \"{id}\"")),
    }
}

/// Extensions in `themes_dir`, sorted by display name.
pub fn installed(themes_dir: &Path) -> Vec<Installed> {
    let Ok(entries) = fs::read_dir(themes_dir) else { return Vec::new() };
    let mut installed: Vec<Installed> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if id.starts_with('.') || !entry.path().is_dir() {
                return None;
            }
            let manifest = super::read_json(&entry.path().join("package.json"))?;
            let display_name = manifest["displayName"]
                .as_str()
                .filter(|name| !name.starts_with('%'))
                .map_or_else(|| id.clone(), str::to_owned);
            let themes = super::scan_extension(&entry.path()).into_iter().map(|theme| theme.name).collect();
            Some(Installed { id, display_name, themes })
        })
        .collect();
    installed.sort_by_key(|extension| extension.display_name.to_lowercase());
    installed
}

pub fn uninstall(themes_dir: &Path, id: &str) -> Result<(), String> {
    let id = folder_name(id)?;
    fs::remove_dir_all(themes_dir.join(id)).map_err(|err| format!("cannot remove {id}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn vsix(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, contents) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("editor-openvsx-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    const MANIFEST: &str = r#"{ "displayName": "Test Themes", "contributes": { "themes": [
        { "label": "Test Dark", "uiTheme": "vs-dark", "path": "./themes/dark.json" } ] } }"#;
    const THEME: &str = r##"{ "name": "Test Dark", "include": "./base.json",
        "tokenColors": [ { "scope": "comment", "settings": { "foreground": "#00ff00" } } ] }"##;
    const BASE: &str = r##"{ "colors": { "editor.background": "#101010" } }"##;

    #[test]
    fn installs_only_theme_files_and_replaces_old_copies() {
        let dir = temp_dir("install");
        let package = vsix(&[
            ("extension.vsixmanifest", "<xml/>"),
            ("extension/package.json", MANIFEST),
            ("extension/themes/dark.json", THEME),
            ("extension/themes/base.json", BASE),
            ("extension/LICENSE.md", "MIT"),
            ("extension/out/extension.js", "evil()"),
            ("extension/../../escape.json", "{}"),
        ]);
        assert_eq!(install_package(&package, &dir, "pub.test").unwrap(), vec!["Test Dark"]);
        let copy = dir.join("pub.test");
        assert!(copy.join("themes/base.json").is_file());
        assert!(copy.join("LICENSE.md").is_file());
        assert!(!copy.join("out/extension.js").exists());
        assert!(!dir.join("escape.json").exists() && !std::env::temp_dir().join("escape.json").exists());

        // Reinstall drops files the new version no longer has.
        fs::write(copy.join("stale.json"), "{}").unwrap();
        install_package(&package, &dir, "pub.test").unwrap();
        assert!(!copy.join("stale.json").exists());

        let list = installed(&dir);
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].id.as_str(), list[0].display_name.as_str()), ("pub.test", "Test Themes"));
        assert_eq!(list[0].themes, vec!["Test Dark"]);

        assert!(uninstall(&dir, "../pub.test").is_err());
        uninstall(&dir, "pub.test").unwrap();
        assert!(installed(&dir).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_install_keeps_existing_copy() {
        let dir = temp_dir("failed");
        let good = vsix(&[("extension/package.json", MANIFEST), ("extension/themes/dark.json", THEME), ("extension/themes/base.json", BASE)]);
        install_package(&good, &dir, "pub.test").unwrap();

        let icons_only = vsix(&[("extension/package.json", r#"{ "contributes": { "iconThemes": [] } }"#)]);
        assert!(install_package(&icons_only, &dir, "pub.test").is_err());
        let broken_include = vsix(&[("extension/package.json", MANIFEST), ("extension/themes/dark.json", THEME)]);
        assert!(install_package(&broken_include, &dir, "pub.test").is_err());
        assert!(install_package(b"not a zip", &dir, "pub.test").is_err());
        assert!(install_package(&good, &dir, "../outside").is_err());

        assert!(dir.join("pub.test/themes/dark.json").is_file());
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(leftovers, vec!["pub.test"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Talks to open-vsx.org: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn searches_and_installs_from_open_vsx() {
        let results = search("tokyo night").unwrap();
        let extension = results.iter().find(|extension| extension.id() == "enkia.tokyo-night").expect("Tokyo Night listed");
        assert!(extension.themes.iter().any(|theme| theme == "Tokyo Night Storm"));
        assert_eq!(extension.license.as_deref(), Some("MIT"));
        assert!(search("").unwrap().len() > 5, "popular themes");

        let dir = temp_dir("live");
        let names = install(extension, &dir).unwrap();
        assert!(names.contains(&"Tokyo Night Light".to_owned()), "{names:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn theme_labels_skip_localized_keys() {
        let manifest = serde_json::json!({ "contributes": { "themes": [
            { "label": "Named", "path": "./a.json" },
            { "label": "%themeLabel%", "path": "./themes/b-color-theme.json" },
        ] } });
        assert_eq!(theme_labels(&manifest), vec!["Named", "b-color-theme"]);
        assert!(theme_labels(&serde_json::json!({ "contributes": { "iconThemes": [{}] } })).is_empty());
    }
}
