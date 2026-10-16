//! Automatically cache Cursor's official, self-contained ACP distribution.
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

// Release and platform layout published by the ACP registry's cursor/agent.json.
const RELEASE: &str = "2026.10.14-0f9edde";
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED: u64 = 2 * 1024 * 1024 * 1024;

fn platform(os: &str, arch: &str) -> Result<(&'static str, &'static str, bool), String> {
    let os = match os { "windows" => "windows", "macos" => "darwin", "linux" => "linux", _ => return Err(format!("Cursor does not support {os}")) };
    let arch = match arch { "x86_64" => "x64", "aarch64" => "arm64", _ => return Err(format!("Cursor does not support {arch}")) };
    Ok((os, arch, os == "windows"))
}

pub fn ensure() -> Result<PathBuf, String> {
    let root = crate::config_path("agents/cursor").ok_or("Could not locate the editor's agent cache")?;
    ensure_at(&root, std::env::consts::OS, std::env::consts::ARCH, |url, archive| {
        let mut response = crate::updater::agent().get(url).call().map_err(|e| format!("Could not download Cursor: {e}"))?;
        let mut file = File::create(archive).map_err(|e| e.to_string())?;
        let size = std::io::copy(&mut response.body_mut().as_reader().take(MAX_DOWNLOAD + 1), &mut file).map_err(|e| e.to_string())?;
        if size > MAX_DOWNLOAD { return Err("Cursor download exceeds the size limit".into()); }
        Ok(())
    })
}

fn ready(package: &Path, windows: bool) -> bool {
    package.join(if windows { "node.exe" } else { "node" }).is_file() && package.join("index.js").is_file()
}

fn ensure_at(root: &Path, os: &str, arch: &str, download: impl FnOnce(&str, &Path) -> Result<(), String>) -> Result<PathBuf, String> {
    let (os, arch, windows) = platform(os, arch)?;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    // Both panels and one-off requests can start together, even in separate editor windows.
    let lock = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.join("install.lock")).map_err(|e| e.to_string())?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|e| e.to_string())?;
    let installed = root.join(format!("{RELEASE}-{os}-{arch}"));
    let package = installed.join("dist-package");
    if ready(&package, windows) { return Ok(package); }
    let staging = tempfile::tempdir_in(root).map_err(|e| e.to_string())?;
    let suffix = if windows { "zip" } else { "tar.gz" };
    let archive = staging.path().join(format!("download.{suffix}"));
    download(&format!("https://downloads.cursor.com/lab/{RELEASE}/{os}/{arch}/agent-cli-package.{suffix}"), &archive)?;
    let payload = staging.path().join("payload");
    fs::create_dir(&payload).map_err(|e| e.to_string())?;
    extract(&archive, &payload, windows)?;
    if !ready(&payload.join("dist-package"), windows) { return Err("Cursor download is missing its bundled runtime or entry point".into()); }
    if installed.exists() { fs::remove_dir_all(&installed).map_err(|e| e.to_string())?; }
    fs::rename(&payload, &installed).map_err(|e| e.to_string())?;
    Ok(package)
}

fn extract(archive: &Path, destination: &Path, windows: bool) -> Result<(), String> {
    let mut total = 0u64;
    if windows {
        let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
            total = total.checked_add(entry.size()).ok_or("Cursor archive is too large")?;
            if total > MAX_EXTRACTED { return Err("Cursor archive is too large".into()); }
            let path = entry.enclosed_name().ok_or("Unsafe path in Cursor archive")?;
            if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) { return Err("Links are not supported in Cursor archives".into()); }
            let target = destination.join(path);
            if entry.is_dir() { fs::create_dir_all(target).map_err(|e| e.to_string())?; }
            else {
                if let Some(parent) = target.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
                std::io::copy(&mut entry, &mut File::create(target).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            }
        }
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(File::open(archive).map_err(|e| e.to_string())?));
        for entry in archive.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() { return Err("Links are not supported in Cursor archives".into()); }
            total = total.checked_add(entry.size()).ok_or("Cursor archive is too large")?;
            if total > MAX_EXTRACTED { return Err("Cursor archive is too large".into()); }
            if !entry.unpack_in(destination).map_err(|e| e.to_string())? { return Err("Unsafe path in Cursor archive".into()); }
        }
    }
    Ok(())
}

pub fn args(package: &Path, windows: bool, path: &str) -> Vec<String> {
    // Invoke the bundled runtime directly: no shell scripts, global Node, or PATH shims.
    vec![format!("PATH={path}"), package.join(if windows { "node.exe" } else { "node" }).display().to_string(), package.join("index.js").display().to_string(), "acp".into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(path: &Path, windows: bool, complete: bool) {
        let files = if complete { vec![(if windows { "dist-package/node.exe" } else { "dist-package/node" }, "runtime"), ("dist-package/index.js", "entry point")] }
            else { vec![("dist-package/index.js", "entry point")] };
        if windows {
            let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
            for (name, content) in files {
                zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
                zip.write_all(content.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        } else {
            let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default()));
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64); header.set_mode(0o755); header.set_cksum();
                tar.append_data(&mut header, name, content.as_bytes()).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
    }

    #[test]
    fn downloads_once_and_launches_bundled_runtime_for_all_six_platforms() {
        for os in ["windows", "linux", "macos"] {
            for arch in ["x86_64", "aarch64"] {
                let root = tempfile::tempdir().unwrap();
                let windows = os == "windows";
                let package = ensure_at(root.path(), os, arch, |url, path| {
                    let (target_os, target_arch, _) = platform(os, arch).unwrap();
                    assert!(url.starts_with("https://downloads.cursor.com/"));
                    assert!(url.contains(&format!("/{target_os}/{target_arch}/")));
                    archive(path, windows, true); Ok(())
                }).unwrap();
                assert_eq!(ensure_at(root.path(), os, arch, |_, _| panic!("must reuse the cached package")).unwrap(), package);
                let launch = args(&package, windows, "test-path");
                assert_eq!(launch[0], "PATH=test-path");
                assert_eq!(fs::read_to_string(&launch[1]).unwrap(), "runtime");
                assert_eq!(fs::read_to_string(&launch[2]).unwrap(), "entry point");
                assert_eq!(launch[3], "acp");
                #[cfg(unix)]
                if !windows {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(fs::metadata(&launch[1]).unwrap().permissions().mode() & 0o777, 0o755);
                }
            }
        }
    }

    #[test]
    fn failed_or_incomplete_downloads_can_be_retried() {
        for os in ["windows", "linux"] {
            let root = tempfile::tempdir().unwrap();
            assert!(ensure_at(root.path(), os, "x86_64", |_, _| Err("offline".into())).is_err());
            assert!(ensure_at(root.path(), os, "x86_64", |_, path| { archive(path, os == "windows", false); Ok(()) }).is_err());
            assert!(ensure_at(root.path(), os, "x86_64", |_, path| { archive(path, os == "windows", true); Ok(()) }).is_ok());
        }
        assert!(platform("unknown", "x86_64").is_err());
        assert!(platform("linux", "unknown").is_err());
    }

    #[test]
    fn archive_paths_and_links_cannot_escape_staging() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("payload");
        fs::create_dir(&destination).unwrap();
        let zip_path = root.path().join("unsafe.zip");
        let mut zip = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        zip.start_file("../escaped", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"bad").unwrap();
        zip.finish().unwrap();
        assert!(extract(&zip_path, &destination, true).is_err());
        assert!(!root.path().join("escaped").exists());
        let tar_path = root.path().join("unsafe.tar.gz");
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(File::create(&tar_path).unwrap(), flate2::Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0); header.set_mode(0o777); header.set_link_name("../escaped").unwrap(); header.set_cksum();
        tar.append_data(&mut header, "link", &[][..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        assert!(extract(&tar_path, &destination, false).is_err());
        assert!(!destination.join("link").exists());
    }
}
