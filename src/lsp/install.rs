//! Downloads a language server from its project's latest GitHub release into the editor's
//! managed directory. Blocking; run it on a worker thread.
//!
//! The release API reports a SHA-256 digest per asset, which the download is checked against,
//! so no hash table has to ship with the editor. Archives are unpacked with the same path
//! and size limits the updater applies to its own releases.
use super::registry::Server;
use serde::Deserialize;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 512 * 1024 * 1024;

#[derive(Deserialize)]
struct Release { tag_name: String, assets: Vec<Asset> }
#[derive(Deserialize)]
struct Asset { name: String, browser_download_url: String, digest: Option<String> }

/// Installs `server` into its managed directory and returns the path of its binary.
pub fn install(server: &'static Server) -> Result<PathBuf, String> {
    let root = server.managed_dir().ok_or("No home directory")?;
    install_to(server, &root)
}

/// Installs `server` under `root/<release tag>/`.
pub fn install_to(server: &'static Server, root: &Path) -> Result<PathBuf, String> {
    let platform = crate::updater::platform().ok_or("No prebuilt language servers for this platform")?;
    let url = format!("https://api.github.com/repos/{}/releases/latest", server.repo);
    let release: Release = serde_json::from_slice(&get(&url)?).map_err(|e| format!("unexpected release data: {e}"))?;
    let wanted = server.asset_for(platform, &release.tag_name).ok_or("No build for this platform")?;
    let asset = release.assets.into_iter().find(|asset| asset.name == wanted)
        .ok_or_else(|| format!("{} {} has no {wanted}", server.name, release.tag_name))?;

    let destination = root.join(&release.tag_name);
    let _ = fs::remove_dir_all(&destination);
    fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
    let download = root.join(format!("{}.download", asset.name));
    let bytes = get(&asset.browser_download_url)?;
    fs::write(&download, &bytes).map_err(|e| e.to_string())?;
    if let Some(expected) = asset.digest.as_deref().and_then(|digest| digest.strip_prefix("sha256:")) {
        if crate::updater::protocol::hash_file(&download)? != expected {
            let _ = fs::remove_file(&download);
            return Err(format!("{} download failed integrity verification", asset.name));
        }
    }

    let binary = destination.join(server.binary_file());
    let result = unpack(&download, &asset.name, &destination, &binary);
    let _ = fs::remove_file(&download);
    result?;
    crate::updater::executable(&binary)?;
    Ok(binary)
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut response = crate::updater::agent().get(url).header("User-Agent", "ailegion-editor").call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut bytes = Vec::new();
    response.body_mut().as_reader().take(MAX_DOWNLOAD + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_DOWNLOAD { return Err("download is too large".into()); }
    Ok(bytes)
}

/// Puts the server binary at `binary`, whatever shape the release ships it in: a bare
/// executable, a gzipped one, or a zip / tar.gz archive containing it somewhere.
fn unpack(download: &Path, name: &str, destination: &Path, binary: &Path) -> Result<(), String> {
    let file = File::open(download).map_err(|e| e.to_string())?;
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let mut budget = Budget::default();
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
            if entry.is_dir() || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) { continue; }
            let path = PathBuf::from(entry.name());
            budget.write(destination, &path, entry.size(), &mut entry)?;
        }
    } else if name.ends_with(".tar.gz") {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut budget = Budget::default();
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            if !entry.header().entry_type().is_file() { continue; }
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            let size = entry.size();
            budget.write(destination, &path, size, &mut entry)?;
        }
    } else if name.ends_with(".gz") {
        let mut reader = flate2::read::GzDecoder::new(file).take(MAX_EXTRACTED + 1);
        let mut out = File::create(binary).map_err(|e| e.to_string())?;
        if std::io::copy(&mut reader, &mut out).map_err(|e| e.to_string())? > MAX_EXTRACTED {
            return Err("extracted server is too large".into());
        }
        return Ok(());
    } else {
        fs::copy(download, binary).map_err(|e| e.to_string())?;
        return Ok(());
    }
    // Archives nest the binary under a versioned folder; move it up to the known location.
    let wanted = binary.file_name().ok_or("invalid binary name")?.to_os_string();
    let found = find_file(destination, &wanted).ok_or_else(|| format!("{} not found in {name}", wanted.to_string_lossy()))?;
    if found != binary { fs::rename(&found, binary).map_err(|e| e.to_string())?; }
    Ok(())
}

#[derive(Default)]
struct Budget { count: usize, total: u64 }

impl Budget {
    fn write(&mut self, destination: &Path, path: &Path, size: u64, input: &mut dyn Read) -> Result<(), String> {
        crate::updater::safe_relative(path)?;
        self.count += 1;
        self.total = self.total.checked_add(size).ok_or("archive is too large")?;
        if self.count > 2_000 || self.total > MAX_EXTRACTED { return Err("archive exceeds limits".into()); }
        let target = destination.join(path);
        fs::create_dir_all(target.parent().ok_or("invalid archive path")?).map_err(|e| e.to_string())?;
        let mut file = File::create(&target).map_err(|e| e.to_string())?;
        if std::io::copy(&mut input.take(size + 1), &mut file).map_err(|e| e.to_string())? != size {
            return Err("invalid archive entry size".into());
        }
        Ok(())
    }
}

fn find_file(dir: &Path, name: &std::ffi::OsStr) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) { return Some(found); }
        } else if path.file_name() == Some(name) {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn archives_yield_the_binary_at_the_expected_path() {
        let root = std::env::temp_dir().join(format!("editor-lsp-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let tar_path = root.join("x.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(File::create(&tar_path).unwrap(), flate2::Compression::fast());
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "ruff-0.1/ruff.exe", &b"hello"[..]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let out = root.join("tar-out");
        fs::create_dir_all(&out).unwrap();
        unpack(&tar_path, "x.tar.gz", &out, &out.join("ruff.exe")).unwrap();
        assert_eq!(fs::read(out.join("ruff.exe")).unwrap(), b"hello");

        let gz_path = root.join("x.gz");
        let mut gz = flate2::write::GzEncoder::new(File::create(&gz_path).unwrap(), flate2::Compression::fast());
        gz.write_all(b"binary").unwrap();
        gz.finish().unwrap();
        let bin = root.join("rust-analyzer");
        unpack(&gz_path, "x.gz", &root, &bin).unwrap();
        assert_eq!(fs::read(&bin).unwrap(), b"binary");

        let raw = root.join("biome-win32-x64.exe");
        fs::write(&raw, b"raw").unwrap();
        unpack(&raw, "biome-win32-x64.exe", &root, &root.join("biome.exe")).unwrap();
        assert_eq!(fs::read(root.join("biome.exe")).unwrap(), b"raw");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Downloads a real release, so it's opt-in: `cargo test -- --ignored downloads`.
    #[test]
    #[ignore]
    fn downloads_verifies_and_runs_a_real_server() {
        let root = std::env::temp_dir().join(format!("editor-lsp-download-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let server = super::super::registry::by_id("just-lsp").unwrap();
        let binary = install_to(server, &root).unwrap();
        assert!(binary.is_file());
        let output = std::process::Command::new(&binary).arg("--version").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        fs::remove_dir_all(&root).unwrap();
    }
}
