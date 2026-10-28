//! Signed, portable GitHub release updates. All network and filesystem work runs off the UI thread.
pub mod protocol;

use fs2::FileExt;
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

const RELEASES: &str = "https://api.github.com/repos/ailegion/editor/releases/latest";
const RELEASE_TAGS: &str = "https://api.github.com/repos/ailegion/editor/releases/tags";
const DOWNLOADS: &str = "https://github.com/ailegion/editor/releases/download";
const MARKER: &str = "editor-managed-installation-v1";
const MAX_EXTRACTED: u64 = 1024 * 1024 * 1024;

pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        _ => None,
    }
}

fn install_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let parent = exe.parent().ok_or("Missing installation directory")?;
    let root = if cfg!(target_os = "macos") {
        parent
            .parent()
            .and_then(Path::parent)
            .ok_or("Updates require an installed .app bundle")?
    } else {
        parent
    };
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let marker = if cfg!(target_os = "macos") {
        root.join("Contents/Resources/UPDATE_INSTALLATION")
    } else {
        root.join("UPDATE_INSTALLATION")
    };
    if fs::read_to_string(marker).unwrap_or_default().trim() != MARKER {
        return Err("Automatic updates require a downloaded release installation".into());
    }
    Ok(root)
}

fn lock_file(root: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(root)?)
        .map_err(|e| format!("Installation is not writable: {e}"))
}

/// Where earlier versions kept the lock: beside the installation.
fn sibling_lock(root: &Path) -> Result<PathBuf, String> {
    let parent = root.parent().ok_or("Invalid installation path")?;
    let name = root.file_name().ok_or("Invalid installation name")?.to_string_lossy();
    Ok(parent.join(format!(".{name}.update-lock")))
}

/// Inside the installation; a macOS bundle can't be written to without breaking its
/// signature, so there it stays beside the bundle.
fn lock_path(root: &Path) -> Result<PathBuf, String> {
    if cfg!(target_os = "macos") { sibling_lock(root) } else { Ok(root.join(".update-lock")) }
}

/// The one folder updates are staged in, on the installation's filesystem so the swap is
/// renames: `.update` inside the installation, or `.editor-update` beside a macOS bundle.
fn staging_dir(root: &Path) -> Result<PathBuf, String> {
    if cfg!(target_os = "macos") {
        Ok(root.parent().ok_or("Invalid installation path")?.join(".editor-update"))
    } else {
        Ok(root.join(".update"))
    }
}

/// Folders earlier versions staged each update in, one per update, beside the installation.
const LEGACY_PREFIX: &str = ".editor-update-";
/// The previous version, kept until the next update replaces it.
const BACKUP: &str = "backup";
/// A backup made by an update that is not yet settled into [`BACKUP`].
const NEW_BACKUP: &str = "backup-new";

/// Makes the latest backup the only one.
fn settle_backup(folder: &Path) {
    let fresh = folder.join(NEW_BACKUP);
    if !fresh.exists() { return; }
    let backup = folder.join(BACKUP);
    if backup.exists() && fs::remove_dir_all(&backup).is_err() { return; }
    let _ = fs::rename(fresh, backup);
}

/// Leaves only the backup in `folder`. Anything still in use, like the helper that just
/// installed an update, is removed by a later run.
fn clear_staging(folder: &Path) -> Result<(), String> {
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    settle_backup(folder);
    for entry in fs::read_dir(folder).map_err(|e| e.to_string())?.filter_map(Result::ok) {
        let name = entry.file_name();
        if name == BACKUP || name == NEW_BACKUP { continue; }
        let path = entry.path();
        let _ = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    }
    Ok(())
}

/// Removes what earlier versions left beside the installation: one staging folder per
/// update and, outside macOS, the lock file. The newest backup among them moves into the
/// staging folder when it has none, so the previous version stays recoverable.
fn remove_legacy(root: &Path, folder: &Path) {
    let Some(parent) = root.parent() else { return };
    let Ok(entries) = fs::read_dir(parent) else { return };
    let mut legacy: Vec<(std::time::SystemTime, PathBuf)> = entries.filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(LEGACY_PREFIX) && entry.path().is_dir())
        .map(|entry| (entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), entry.path()))
        .collect();
    legacy.sort();
    if !folder.join(BACKUP).exists() {
        if let Some((_, newest)) = legacy.iter().rev().find(|(_, path)| path.join(BACKUP).is_dir()) {
            let _ = fs::create_dir_all(folder);
            let _ = fs::rename(newest.join(BACKUP), folder.join(BACKUP));
        }
    }
    for (_, path) in legacy { let _ = fs::remove_dir_all(path); }
    if !cfg!(target_os = "macos") {
        if let Ok(old) = sibling_lock(root) { let _ = fs::remove_file(old); }
    }
}

/// Run before every update check, so an installed update's leftovers go on the next start.
fn tidy(root: &Path) {
    let Ok(folder) = staging_dir(root) else { return };
    remove_legacy(root, &folder);
    if folder.exists() { let _ = clear_staging(&folder); }
}

/// Keep a shared lock for the lifetime of each running editor. The helper waits for all instances.
pub fn running_guard() -> Option<File> {
    let file = lock_file(&install_root().ok()?).ok()?;
    FileExt::lock_shared(&file).ok()?;
    Some(file)
}

enum Event {
    Progress(u8),
    Finished(Result<Option<Prepared>, String>),
}
struct Prepared {
    folder: PathBuf,
    root: PathBuf,
    version: String,
}

pub struct Updater {
    receiver: Option<Receiver<Event>>,
    prepared: Option<Prepared>,
    last_check: Option<Instant>,
    pub label: String,
    pub detail: String,
    pub enabled: bool,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            receiver: None,
            prepared: None,
            last_check: None,
            label: "Check for updates".into(),
            detail: format!("editor {}", env!("CARGO_PKG_VERSION")),
            enabled: !cfg!(debug_assertions) && install_root().is_ok() && platform().is_some(),
        }
    }
}

impl Updater {
    pub fn ready(&self) -> bool {
        self.prepared.is_some()
    }
    pub fn busy(&self) -> bool {
        self.receiver.is_some()
    }
    pub fn check(&mut self) {
        if !self.enabled || self.busy() || self.ready() {
            return;
        }
        self.last_check = Some(Instant::now());
        self.label = "Checking for updates…".into();
        let (send, receive) = mpsc::channel();
        self.receiver = Some(receive);
        std::thread::spawn(move || {
            let result = prepare(&send);
            let _ = send.send(Event::Finished(result));
        });
    }
    pub fn poll(&mut self) {
        if self.enabled
            && self
                .last_check
                .is_none_or(|at| at.elapsed() > Duration::from_secs(6 * 3600))
        {
            self.check();
        }
        loop {
            let event = match self.receiver.as_ref().map(Receiver::try_recv) {
                Some(Ok(event)) => event,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.receiver = None;
                    self.label = "Retry update".into();
                    self.detail = "Update worker stopped unexpectedly".into();
                    break;
                }
                _ => break,
            };
            match event {
                Event::Progress(percent) => self.label = format!("Downloading update… {percent}%"),
                Event::Finished(result) => {
                    self.receiver = None;
                    match result {
                        Ok(Some(prepared)) => {
                            self.detail = format!(
                                "Version {} is ready. Unsaved edits will be recovered after restart.",
                                prepared.version
                            );
                            self.label = "Restart to update".into();
                            self.prepared = Some(prepared);
                        }
                        Ok(None) => {
                            self.label = "Check for updates".into();
                            self.detail = "You have the latest available stable update".into();
                        }
                        Err(error) => {
                            self.label = "Retry update".into();
                            self.detail = error;
                        }
                    }
                    break;
                }
            }
        }
    }
    pub fn restart(&mut self) -> Result<(), String> {
        let prepared = self.prepared.as_ref().ok_or("No update is ready")?;
        let helper = prepared.folder.join(if cfg!(windows) {
            "updater.exe"
        } else {
            "updater"
        });
        fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &helper)
            .map_err(|e| e.to_string())?;
        executable(&helper)?;
        let mut command = Command::new(&helper);
        command
            .arg("--apply-update")
            .arg(&prepared.folder)
            .arg(&prepared.root)
            .current_dir(&prepared.folder)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut command);
        command
            .spawn()
            .map_err(|e| format!("Could not start updater: {e}"))?;
        // The helper now owns the staging folder; its backup stays until the next update.
        self.prepared = None;
        Ok(())
    }
}

pub(crate) fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into()
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent()
        .get(url)
        .header("User-Agent", "ailegion-editor-updater")
        .call()
        .map_err(|e| format!("Update check failed: {e}"))?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
}

#[derive(Deserialize)]
struct Notes {
    body: Option<String>,
}

/// Markdown notes of the GitHub release for this build's version. `None` if it has no release.
pub fn release_notes() -> Result<Option<String>, String> {
    let url = format!("{RELEASE_TAGS}/v{}", env!("CARGO_PKG_VERSION"));
    let mut response = match agent().get(&url).header("User-Agent", "ailegion-editor-updater").call() {
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        result => result.map_err(|e| format!("Could not load release notes: {e}"))?,
    };
    let bytes = response
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let notes: Notes = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(notes.body.filter(|body| !body.trim().is_empty()))
}

fn newer_version(tag: &str, current: &str) -> Result<Option<String>, String> {
    let version = semver::Version::parse(tag.strip_prefix('v').ok_or("Invalid release tag")?)
        .map_err(|e| e.to_string())?;
    let current = semver::Version::parse(current).map_err(|e| e.to_string())?;
    Ok((version.pre.is_empty() && version > current).then(|| version.to_string()))
}

fn prepare(send: &Sender<Event>) -> Result<Option<Prepared>, String> {
    if let Ok(root) = install_root() { tidy(&root); }
    let release: Release =
        serde_json::from_slice(&get(RELEASES)?).map_err(|e| e.to_string())?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(version) = newer_version(&release.tag_name, env!("CARGO_PKG_VERSION"))? else {
        return Ok(None);
    };
    let platform = platform().ok_or("Unsupported update platform")?;
    let name = format!("update-{platform}.json");
    if !release.assets.iter().any(|asset| asset.name == name) {
        return Err("A newer release exists but does not include signed automatic updates".into());
    }
    let base = format!("{DOWNLOADS}/v{version}");
    let bytes = get(&format!("{base}/{name}"))?;
    let signature = get(&format!("{base}/{name}.sig"))?;
    let manifest = protocol::verify(&bytes, &signature, protocol::PUBLIC_KEY)?;
    protocol::validate(&manifest, &version, platform)?;
    let root = install_root()?;
    // Same filesystem allows atomic renames. Never stage updates inside user projects.
    let folder = staging_dir(&root)?;
    clear_staging(&folder).map_err(|e| format!("Cannot stage update in {}: {e}", folder.display()))?;
    fs::write(folder.join("manifest.json"), bytes).map_err(|e| e.to_string())?;
    fs::write(folder.join("manifest.sig"), signature).map_err(|e| e.to_string())?;
    let archive = folder.join("package");
    let mut response = agent()
        .get(&format!("{base}/{}", manifest.archive))
        .header("User-Agent", "ailegion-editor-updater")
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader = response.body_mut().as_reader();
    let mut output = File::create(&archive).map_err(|e| e.to_string())?;
    let mut buffer = [0; 64 * 1024];
    let mut size = 0u64;
    let mut percent = 101;
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > manifest.size {
            return Err("Update exceeds signed download size".into());
        }
        output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        let next = (size * 100 / manifest.size) as u8;
        if next != percent {
            let _ = send.send(Event::Progress(next));
            percent = next;
        }
    }
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    verify_archive(&archive, &manifest)?;
    // Extraction is deferred to the helper, which rechecks the signature/hash before applying.
    Ok(Some(Prepared {
        folder,
        root,
        version,
    }))
}

fn verify_archive(path: &Path, manifest: &protocol::Manifest) -> Result<(), String> {
    if fs::metadata(path).map_err(|e| e.to_string())?.len() != manifest.size
        || protocol::hash_file(path)? != manifest.sha256
    {
        return Err("Update archive failed integrity verification".into());
    }
    Ok(())
}

pub(crate) fn safe_relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path.to_string_lossy().contains(['\\', ':'])
    {
        return Err("Unsafe path in update archive".into());
    }
    Ok(())
}

fn extract(archive: &Path, destination: &Path, platform: &str) -> Result<(), String> {
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    let mut count = 0usize;
    let mut write_entry =
        |path: &Path, directory: bool, size: u64, input: &mut dyn Read| -> Result<(), String> {
            safe_relative(path)?;
            count += 1;
            total = total.checked_add(size).ok_or("Update is too large")?;
            if count > 30_000 || total > MAX_EXTRACTED {
                return Err("Extracted update exceeds limits".into());
            }
            let path = destination.join(path);
            if directory {
                fs::create_dir_all(path).map_err(|e| e.to_string())?;
            } else {
                fs::create_dir_all(path.parent().ok_or("Invalid archive path")?)
                    .map_err(|e| e.to_string())?;
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| e.to_string())?;
                let copied = std::io::copy(&mut input.take(size + 1), &mut file)
                    .map_err(|e| e.to_string())?;
                if copied != size {
                    return Err("Invalid archive entry size".into());
                }
                file.sync_all().map_err(|e| e.to_string())?;
            }
            Ok(())
        };
    if platform.starts_with("linux") {
        let file = File::open(archive).map_err(|e| e.to_string())?;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                return Err("Links and special files are not allowed in updates".into());
            }
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            write_entry(&path, kind.is_dir(), entry.size(), &mut entry)?;
        }
    } else {
        let mut zip = zip::ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
            if entry.name().starts_with("__MACOSX/") {
                continue;
            }
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err("Symlinks are not allowed in updates".into());
            }
            let path = PathBuf::from(entry.name().trim_end_matches('/'));
            write_entry(&path, entry.is_dir(), entry.size(), &mut entry)?;
        }
    }
    Ok(())
}

pub(crate) fn executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn executable_in(root: &Path) -> PathBuf {
    root.join(if cfg!(target_os = "macos") {
        "Contents/MacOS/editor"
    } else if cfg!(windows) {
        "editor.exe"
    } else {
        "editor"
    })
}

fn launch(root: &Path) -> Result<(), String> {
    let mut command = Command::new(executable_in(root));
    command
        .current_dir(root.parent().ok_or("Invalid installation path")?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    command
        .spawn()
        .map_err(|e| format!("Could not restart editor: {e}"))?;
    Ok(())
}

/// Returns true only in the copied helper process, before the GUI is initialized.
pub fn run_helper() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != "--apply-update") {
        return false;
    }
    let result = if args.len() == 4 {
        apply(Path::new(&args[2]), Path::new(&args[3]))
    } else {
        Err("Invalid updater arguments".into())
    };
    if let Err(error) = result {
        if args.len() == 4 {
            let _ = launch(Path::new(&args[3]));
        }
        rfd::MessageDialog::new().set_title("Editor update failed")
            .set_description(format!("{error}\n\nThe previous version, if backed up, is in {}.",
                if cfg!(target_os = "macos") { "the .editor-update folder beside the app" } else { "the .update folder inside the installation" }))
            .set_level(rfd::MessageLevel::Error).show();
    }
    true
}

fn apply(folder: &Path, root: &Path) -> Result<(), String> {
    let folder = folder.canonicalize().map_err(|e| e.to_string())?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if staging_dir(&root)?.canonicalize().ok().as_deref() != Some(folder.as_path())
        || std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|e| e.to_string())?
            .parent()
            != Some(folder.as_path())
    {
        return Err("Invalid updater staging location".into());
    }
    let platform = platform().ok_or("Unsupported update platform")?;
    let manifest = protocol::verify(
        &fs::read(folder.join("manifest.json")).map_err(|e| e.to_string())?,
        &fs::read(folder.join("manifest.sig")).map_err(|e| e.to_string())?,
        protocol::PUBLIC_KEY,
    )?;
    protocol::validate(&manifest, &manifest.version, platform)?;
    if newer_version(&format!("v{}", manifest.version), env!("CARGO_PKG_VERSION"))?.is_none() {
        return Err("Refusing to install an older or equal version".into());
    }
    verify_archive(&folder.join("package"), &manifest)?;
    let unpacked = folder.join("unpacked");
    if unpacked.exists() { fs::remove_dir_all(&unpacked).map_err(|e| e.to_string())?; }
    extract(&folder.join("package"), &unpacked, platform)?;
    let payload = if cfg!(target_os = "macos") {
        unpacked.join("editor.app")
    } else {
        unpacked.join(format!("editor-{}-{platform}", manifest.version))
    };
    if !executable_in(&payload).is_file() {
        return Err("Update is missing its executable".into());
    }
    executable(&executable_in(&payload))?;
    let resources = if cfg!(target_os = "macos") {
        payload.join("Contents/Resources")
    } else {
        payload.clone()
    };
    for file in [
        "LICENSE.md",
        "NOTICE",
        "UPDATE_INSTALLATION",
        "THIRD_PARTY_LICENSES.html",
        "SYNTAX_AND_THEME_LICENSES.md",
        "themes/vscode.theme-defaults/LICENSE.txt",
    ] {
        if !resources.join(file).is_file() {
            return Err(format!("Update is missing {file}"));
        }
    }
    if fs::read_to_string(resources.join("UPDATE_INSTALLATION"))
        .map_err(|e| e.to_string())?
        .trim()
        != MARKER
    {
        return Err("Invalid update installation marker".into());
    }
    #[cfg(target_os = "macos")]
    if !Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&payload)
        .status()
        .map_err(|e| e.to_string())?
        .success()
    {
        return Err("Updated macOS bundle failed code signature verification".into());
    }
    let lock = lock_file(&root)?;
    let start = Instant::now();
    while FileExt::try_lock_exclusive(&lock).is_err() {
        if start.elapsed() > Duration::from_secs(90) {
            return Err("Close all editor windows before installing the update".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // The previous backup stays until this update has launched, then this one replaces it.
    let backup = folder.join(NEW_BACKUP);
    if backup.exists() { fs::remove_dir_all(&backup).map_err(|e| e.to_string())?; }
    let result = replace(&root, &payload, &backup, cfg!(target_os = "macos"));
    FileExt::unlock(&lock).map_err(|e| e.to_string())?;
    match result {
        Ok(changed) => {
            if let Err(error) = launch(&root) {
                FileExt::lock_exclusive(&lock).map_err(|e| e.to_string())?;
                restore(
                    &root,
                    &payload,
                    &backup,
                    &changed,
                    cfg!(target_os = "macos"),
                )?;
                FileExt::unlock(&lock).map_err(|e| e.to_string())?;
                let _ = fs::remove_dir(&backup);
                return Err(error);
            }
            settle_backup(&folder);
            Ok(())
        }
        Err(error) => {
            // Empty after a rollback. If the rollback failed it holds the previous version, so it stays.
            let _ = fs::remove_dir(&backup);
            Err(error)
        }
    }
}

const MANAGED: &[&str] = &[
    "editor.exe",
    "editor",
    "themes",
    "licenses",
    "LICENSE.md",
    "NOTICE",
    "README.md",
    "INSTALL.md",
    "THIRD_PARTY_LICENSES.html",
    "SYNTAX_AND_THEME_LICENSES.md",
    "UPDATE_INSTALLATION",
];

fn move_file(from: impl AsRef<Path>, to: impl AsRef<Path>) -> std::io::Result<()> {
    // Windows can retain the executable mapping briefly after the running lock drops.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::rename(from.as_ref(), to.as_ref()) {
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(5 | 32 | 33))
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

/// Tracks each rename so an ordinary installation error can be rolled back without deleting user files.
fn replace(
    root: &Path,
    payload: &Path,
    backup: &Path,
    bundle: bool,
) -> Result<Vec<(String, bool)>, String> {
    if bundle {
        move_file(root, backup).map_err(|e| e.to_string())?;
        if let Err(error) = move_file(payload, root) {
            move_file(backup, root).map_err(|e| format!("{error}; rollback failed: {e}"))?;
            return Err(error.to_string());
        }
        return Ok(Vec::new());
    }
    fs::create_dir(backup).map_err(|e| e.to_string())?;
    let mut changed = Vec::new();
    let result = (|| {
        for entry in fs::read_dir(payload).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .to_str()
                .ok_or("Invalid update filename")?
                .to_string();
            if !MANAGED.contains(&name.as_str()) {
                return Err(format!("Unexpected update file: {name}"));
            }
            let destination = root.join(&name);
            let existed = destination.try_exists().map_err(|e| e.to_string())?;
            if existed {
                move_file(&destination, backup.join(&name)).map_err(|e| e.to_string())?;
            }
            changed.push((name.clone(), existed));
            move_file(entry.path(), destination).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        restore(root, payload, backup, &changed, false)?;
        return Err(error);
    }
    Ok(changed)
}

fn restore(
    root: &Path,
    payload: &Path,
    backup: &Path,
    changed: &[(String, bool)],
    bundle: bool,
) -> Result<(), String> {
    if bundle {
        move_file(root, payload).map_err(|e| e.to_string())?;
        move_file(backup, root).map_err(|e| e.to_string())?;
    } else {
        for (name, existed) in changed.iter().rev() {
            if root.join(name).exists() {
                move_file(root.join(name), payload.join(name))
                    .map_err(|e| format!("Rollback failed: {e}"))?;
            }
            if *existed {
                move_file(backup.join(name), root.join(name))
                    .map_err(|e| format!("Rollback failed: {e}"))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloads_and_verifies_64_byte_signatures() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use ring::{rand::SystemRandom, signature::{Ed25519KeyPair, KeyPair}};
        use std::net::TcpListener;

        let key = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap().as_ref(),
        ).unwrap();
        let manifest = br#"{"version":"0.1.1","platform":"windows-x86_64","archive":"editor-0.1.1-windows-x86_64.zip","size":100,"sha256":"abc"}"#;
        let signature = key.sign(manifest).as_ref().to_vec();
        assert_eq!(signature.len(), 64);
        let served = signature.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/update-windows-x86_64.json.sig", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for chunked in [false, true] {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                if chunked {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n40\r\n").unwrap();
                    socket.write_all(&served).unwrap();
                    socket.write_all(b"\r\n0\r\n\r\n").unwrap();
                } else {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n").unwrap();
                    socket.write_all(&served).unwrap();
                }
            }
        });
        for _ in 0..2 {
            let downloaded = get(&url).unwrap();
            assert_eq!(downloaded, signature);
            assert!(protocol::verify(manifest, &downloaded, &STANDARD.encode(key.public_key().as_ref())).is_ok());
        }
        server.join().unwrap();
    }

    #[test]
    fn versions_do_not_downgrade_or_offer_prereleases() {
        assert_eq!(
            newer_version("v1.10.0", "1.9.0").unwrap(),
            Some("1.10.0".into())
        );
        assert!(newer_version("v1.0.0", "1.0.0").unwrap().is_none());
        assert!(newer_version("v0.9.0", "1.0.0").unwrap().is_none());
        assert!(newer_version("v2.0.0-beta.1", "1.0.0").unwrap().is_none());
    }
    #[test]
    fn archive_paths_cannot_escape_staging() {
        for path in [
            "../editor",
            "/editor",
            "C:/editor",
            "themes/../../editor",
            "themes\\escape",
        ] {
            assert!(safe_relative(Path::new(path)).is_err(), "{path}");
        }
        assert!(safe_relative(Path::new("editor-1.0.0/themes/test.json")).is_ok());
    }
    #[test]
    fn replacing_and_restoring_preserves_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let payload = dir.path().join("new");
        let backup = dir.path().join("backup");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&payload).unwrap();
        fs::write(root.join("editor.exe"), "old").unwrap();
        fs::write(root.join("my-work.txt"), "work").unwrap();
        fs::write(payload.join("editor.exe"), "new").unwrap();
        fs::write(payload.join("NOTICE"), "notice").unwrap();
        let changed = replace(&root, &payload, &backup, false).unwrap();
        assert_eq!(fs::read_to_string(root.join("editor.exe")).unwrap(), "new");
        restore(&root, &payload, &backup, &changed, false).unwrap();
        assert_eq!(fs::read_to_string(root.join("editor.exe")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(root.join("my-work.txt")).unwrap(),
            "work"
        );
        assert!(!root.join("NOTICE").exists());
    }

    #[test]
    fn zip_extraction_rejects_traversal_and_preserves_bytes() {
        use zip::write::SimpleFileOptions;
        for name in ["editor-1.0.0-windows-x86_64/editor.exe", "../outside"] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("archive.zip");
            let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(b"executable").unwrap();
            zip.finish().unwrap();
            let out = dir.path().join("out");
            let result = extract(&archive, &out, "windows-x86_64");
            if name.starts_with("..") {
                assert!(result.is_err());
                assert!(!dir.path().join("outside").exists());
            } else {
                result.unwrap();
                assert_eq!(fs::read(out.join(name)).unwrap(), b"executable");
            }
        }
    }

    #[test]
    fn tar_extraction_accepts_files_but_rejects_symlinks() {
        for link in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("package.tar.gz");
            let gz = flate2::write::GzEncoder::new(
                File::create(&archive).unwrap(),
                flate2::Compression::default(),
            );
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            if link {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_link_name("../../outside").unwrap();
                header.set_cksum();
                tar.append_data(&mut header, "editor-1.0.0-linux-x86_64/editor", &[][..])
                    .unwrap();
            } else {
                header.set_size(4);
                header.set_cksum();
                tar.append_data(
                    &mut header,
                    "editor-1.0.0-linux-x86_64/editor",
                    &b"test"[..],
                )
                .unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
            let result = extract(&archive, &dir.path().join("out"), "linux-x86_64");
            assert_eq!(result.is_err(), link);
        }
    }

    #[test]
    fn corrupted_download_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("package");
        fs::write(&archive, b"signed archive").unwrap();
        let manifest = protocol::Manifest {
            version: "1.0.0".into(),
            platform: "windows-x86_64".into(),
            archive: "editor-1.0.0-windows-x86_64.zip".into(),
            size: 14,
            sha256: protocol::hash_file(&archive).unwrap(),
        };
        verify_archive(&archive, &manifest).unwrap();
        fs::write(&archive, b"unsafe archive").unwrap();
        assert!(verify_archive(&archive, &manifest).is_err());
    }

    #[test]
    fn bundle_swap_rolls_back_when_replacement_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("editor.app");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("old"), "original").unwrap();
        assert!(
            replace(
                &root,
                &dir.path().join("missing"),
                &dir.path().join("backup"),
                true
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(root.join("old")).unwrap(), "original");
    }

    #[test]
    fn running_editor_lock_blocks_installation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("editor");
        fs::create_dir(&root).unwrap();
        let running = lock_file(&root).unwrap();
        FileExt::lock_shared(&running).unwrap();
        let installing = lock_file(&root).unwrap();
        assert!(FileExt::try_lock_exclusive(&installing).is_err());
        drop(running);
        FileExt::try_lock_exclusive(&installing).unwrap();
    }

    #[test]
    fn updates_stage_in_one_folder_inside_the_installation_except_macos_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("editor");
        fs::create_dir(&root).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(staging_dir(&root).unwrap(), dir.path().join(".editor-update"));
            assert_eq!(lock_path(&root).unwrap(), dir.path().join(".editor.update-lock"));
        } else {
            assert_eq!(staging_dir(&root).unwrap(), root.join(".update"));
            assert_eq!(lock_path(&root).unwrap(), root.join(".update-lock"));
        }
        // The swap only moves the editor's own files, never the staging folder or the lock.
        assert!(!MANAGED.contains(&".update") && !MANAGED.contains(&".update-lock"));
    }

    #[test]
    fn staging_is_cleared_except_the_one_latest_backup() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join(".update");
        fs::create_dir_all(folder.join("backup")).unwrap();
        fs::write(folder.join("backup").join("editor.exe"), "previous").unwrap();
        fs::create_dir_all(folder.join("unpacked").join("editor-1.0.0")).unwrap();
        for leftover in ["package", "manifest.json", "manifest.sig", "updater.exe"] {
            fs::write(folder.join(leftover), "leftover").unwrap();
        }
        clear_staging(&folder).unwrap();
        let names: Vec<_> = fs::read_dir(&folder).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, ["backup"]);
        assert_eq!(fs::read_to_string(folder.join("backup").join("editor.exe")).unwrap(), "previous");
        // A newer backup replaces the older one: one previous version, never more.
        fs::create_dir_all(folder.join(NEW_BACKUP)).unwrap();
        fs::write(folder.join(NEW_BACKUP).join("editor.exe"), "latest").unwrap();
        clear_staging(&folder).unwrap();
        let names: Vec<_> = fs::read_dir(&folder).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, ["backup"]);
        assert_eq!(fs::read_to_string(folder.join("backup").join("editor.exe")).unwrap(), "latest");
        // A missing folder is created.
        let fresh = dir.path().join("fresh");
        clear_staging(&fresh).unwrap();
        assert!(fresh.is_dir());
    }

    #[test]
    fn folders_left_beside_the_installation_are_removed_keeping_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("editor");
        fs::create_dir(&root).unwrap();
        let folder = staging_dir(&root).unwrap();
        let with_backup = dir.path().join(".editor-update-abc123");
        fs::create_dir_all(with_backup.join("backup")).unwrap();
        fs::write(with_backup.join("backup").join("editor.exe"), "previous").unwrap();
        fs::write(with_backup.join("package"), "zip").unwrap();
        fs::create_dir_all(dir.path().join(".editor-update-def456").join("unpacked")).unwrap();
        fs::write(dir.path().join(".editor.update-lock"), "").unwrap();
        // Things that are not the updater's are left alone.
        fs::create_dir(dir.path().join("other-app")).unwrap();
        fs::write(dir.path().join(".editor-update-notes.txt"), "a file, not a folder").unwrap();
        tidy(&root);
        assert!(!with_backup.exists() && !dir.path().join(".editor-update-def456").exists());
        assert_eq!(fs::read_to_string(folder.join("backup").join("editor.exe")).unwrap(), "previous");
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 1, "only the backup is kept");
        assert_eq!(dir.path().join(".editor.update-lock").exists(), cfg!(target_os = "macos"), "macOS still uses this lock");
        assert!(dir.path().join("other-app").is_dir() && dir.path().join(".editor-update-notes.txt").is_file());
    }
}
