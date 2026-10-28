//! Install the registry-selected Claude adapter and run its published entry through Node.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use agent_client_protocol::{AcpAgent, AcpAgentConfig};

const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
const ENTRY: &str = "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js";
const INSTALL: &str = "install";
const INSTALLED_RELEASE: &str = "installed-release";
const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";

#[derive(Clone, Debug, serde::Deserialize)]
struct Release {
    version: String,
    distribution: Distribution,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Distribution { npx: Npx }

#[derive(Clone, Debug, serde::Deserialize)]
struct Npx {
    package: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

impl Release {
    fn package_spec(&self) -> Result<String, String> {
        semver::Version::parse(&self.version).map_err(|e| format!("Invalid Claude registry version: {e}"))?;
        let version = self.distribution.npx.package.strip_prefix(&format!("{PACKAGE}@"))
            .ok_or("Claude registry contains an unexpected package")?;
        semver::Version::parse(version).map_err(|e| format!("Invalid Claude package version: {e}"))?;
        // Match Zed's bounded_npm_package_spec, preserving npm release-age restrictions.
        Ok(format!("{PACKAGE}@0.0.0 - {version}"))
    }
}

fn parse_registry(body: &str) -> Result<Release, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let entry = value["agents"].as_array().and_then(|agents| agents.iter().find(|agent| agent["id"] == "claude-acp"))
        .ok_or("Claude was not found in the ACP registry")?;
    let release: Release = serde_json::from_value(entry.clone()).map_err(|e| e.to_string())?;
    release.package_spec()?;
    Ok(release)
}

static CACHE: std::sync::Mutex<Option<(std::time::Instant, Release)>> = std::sync::Mutex::new(None);

/// The last registry entry seen, without going online; online only when none was ever saved.
fn saved_release(root: &Path) -> Result<Release, String> {
    let saved = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(_, release)| release.clone());
    let saved = saved.or_else(|| std::fs::read_to_string(root.join("registry.json")).ok().and_then(|body| parse_registry(&body).ok()));
    match saved { Some(release) => Ok(release), None => registry_release(root) }
}

fn registry_release(root: &Path) -> Result<Release, String> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((checked, release)) = &*cache {
        if checked.elapsed() < std::time::Duration::from_secs(3600) { return Ok(release.clone()); }
    }
    let cached_body = std::fs::read_to_string(root.join("registry.json")).ok();
    let result: Result<Release, String> = (|| {
        let client: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(30))).build().into();
        let mut response = client.get(REGISTRY_URL).call().map_err(|e| e.to_string())?;
        let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
        let release = parse_registry(&body)?;
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        file.persist(root.join("registry.json")).map_err(|e| e.to_string())?;
        Ok(release)
    })();
    match result {
        Ok(release) => { *cache = Some((std::time::Instant::now(), release.clone())); Ok(release) }
        Err(error) => cache.as_ref().map(|(_, release)| release.clone())
            .or_else(|| cached_body.as_deref().and_then(|body| parse_registry(body).ok()))
            .ok_or_else(|| format!("Could not check Claude ACP updates: {error}")),
    }
}

/// `check` is set by the AI panel: it looks for a new registry version and a broken install
/// and fixes either before starting. Other callers start the installed version as it is.
pub fn agent(path: &OsStr, check: bool, mut progress: impl FnMut(&str)) -> Result<AcpAgent, String> {
    let node = crate::agent_launch::find_in(if cfg!(windows) { "node.exe" } else { "node" }, path)
        .ok_or(crate::agent_launch::NODE_MISSING)?;
    if check { progress("Checking…"); }
    let root = crate::config_path("agents/claude").ok_or("Could not locate the editor's agent cache")?;
    let release = if check { registry_release(&root)? } else { saved_release(&root)? };
    let package = release.package_spec()?;
    let entry = ensure_at(&root, &release.version, check, &mut progress, |destination| {
        let npm = crate::agent_launch::find_in(if cfg!(windows) { "npm.cmd" } else { "npm" }, path)
            .ok_or("npm was not found. Install Node.js with npm, then restart the editor.")?;
        let cli = if cfg!(windows) {
            npm.parent().ok_or("npm has no parent directory")?.join("node_modules/npm/bin/npm-cli.js")
        } else { std::fs::canonicalize(npm).map_err(|e| e.to_string())? };
        let mut command = std::process::Command::new(&node);
        command.arg(cli)
            .args(["install", "--prefix"]).arg(destination)
            .args(["--no-audit", "--no-fund", "--save-exact", &package])
            .env("PATH", path).stdin(std::process::Stdio::null());
        // node.exe is a console program; without this the windowed editor opens a terminal for it.
        crate::updater::hide_console(&mut command);
        let output = command.output().map_err(|e| format!("Could not install Claude ACP: {e}"))?;
        if !output.status.success() {
            return Err(format!("Could not install Claude ACP ({}): {}", output.status, String::from_utf8_lossy(&output.stderr)));
        }
        Ok(())
    })?;
    let agent = configured_agent(&node, &entry, path);
    let mut config = agent.into_config().args(release.distribution.npx.args);
    for (name, value) in release.distribution.npx.env { config = config.env(name, value); }
    progress("Connecting…");
    Ok(AcpAgent::new(config))
}

fn configured_agent(node: &Path, entry: &Path, path: &OsStr) -> AcpAgent {
    AcpAgent::new(AcpAgentConfig::new(node).arg(entry.to_string_lossy()).env("PATH", path.to_string_lossy()))
}

/// One install folder updated in place, like Zed. `release` is the registry version the
/// folder was last installed for; a different one, a missing entry or a half-extracted
/// package makes `check` install again before Claude starts.
fn ensure_at(root: &Path, release: &str, check: bool, progress: &mut impl FnMut(&str), mut install: impl FnMut(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(root.join("install.lock")).map_err(|e| e.to_string())?;
    progress("Waiting for installation…");
    fs2::FileExt::lock_exclusive(&lock).map_err(|e| e.to_string())?;
    let installed = root.join(INSTALL);
    let entry = installed.join(ENTRY);
    let modules = installed.join("node_modules");
    let missing = !entry.is_file();
    if !check && !missing { return Ok(entry); }
    let broken = missing || has_incomplete_package(&modules);
    let recorded = std::fs::read_to_string(root.join(INSTALLED_RELEASE)).ok();
    if !broken && recorded.as_deref() == Some(release) {
        remove_old_installs(root);
        return Ok(entry);
    }
    progress(if broken && installed.exists() { "Reinstalling…" } else { "Installing…" });
    if broken && modules.exists() { std::fs::remove_dir_all(&modules).map_err(|e| e.to_string())?; }
    std::fs::create_dir_all(&installed).map_err(|e| e.to_string())?;
    install(&installed)?;
    // npm can leave packages half-extracted; Zed clears node_modules and installs once more.
    if has_incomplete_package(&modules) {
        std::fs::remove_dir_all(&modules).map_err(|e| e.to_string())?;
        install(&installed)?;
    }
    if !entry.is_file() || has_incomplete_package(&modules) {
        return Err("Claude ACP install is incomplete. Try again.".into());
    }
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    file.write_all(release.as_bytes()).map_err(|e| e.to_string())?;
    file.persist(root.join(INSTALLED_RELEASE)).map_err(|e| e.to_string())?;
    remove_old_installs(root);
    Ok(entry)
}

/// A top-level package folder, scoped or not, without its `package.json`.
fn has_incomplete_package(modules: &Path) -> bool {
    let folders = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir).map(|entries| entries.filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir() && !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path()).collect()).unwrap_or_default()
    };
    folders(modules).into_iter().any(|folder| {
        if folder.file_name().is_some_and(|name| name.to_string_lossy().starts_with('@')) {
            folders(&folder).iter().any(|package| !package.join("package.json").is_file())
        } else { !folder.join("package.json").is_file() }
    })
}

/// Version folders and staging folders left by the earlier one-folder-per-version layout.
/// A folder Windows will not delete yet is tried again after the next install.
fn remove_old_installs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && (semver::Version::parse(&name).is_ok() || name.starts_with(".tmp")) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(version: &str) -> String {
        serde_json::json!({"agents":[{"id":"unrelated"},{"id":"claude-acp", "version":version,
            "distribution":{"npx":{"package":format!("{PACKAGE}@{version}"), "args":["--example"], "env":{"EXAMPLE":"literal value"}}}}]}).to_string()
    }

    #[test]
    fn registry_updates_change_the_ceiling_without_an_editor_release() {
        for version in ["0.98.0", "0.99.1", "1.0.0"] {
            let release = parse_registry(&registry(version)).unwrap();
            assert_eq!(release.package_spec().unwrap(), format!("{PACKAGE}@0.0.0 - {version}"));
            assert_eq!(release.distribution.npx.args, ["--example"]);
            assert_eq!(release.distribution.npx.env["EXAMPLE"], "literal value");
        }
    }

    #[test]
    fn invalid_registry_cannot_select_an_unrelated_package_or_path() {
        assert!(parse_registry("not JSON").is_err());
        assert!(parse_registry(r#"{"agents":[]}"#).is_err());
        assert!(parse_registry(&registry("../outside")).is_err());
        let wrong_package = registry("1.0.0").replace(PACKAGE, "unrelated-package");
        assert!(parse_registry(&wrong_package).is_err());
    }

    /// What npm leaves for the adapter: its entry and `package.json`, plus one dependency.
    fn fake_install(destination: &Path) -> Result<(), String> {
        let entry = destination.join(ENTRY);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(entry, "synthetic JavaScript; no executable permission required").unwrap();
        let package = destination.join("node_modules/@agentclientprotocol/claude-agent-acp");
        std::fs::write(package.join("package.json"), r#"{"version":"0.0.0"}"#).unwrap();
        std::fs::create_dir_all(destination.join("node_modules/dependency")).unwrap();
        std::fs::write(destination.join("node_modules/dependency/package.json"), "{}").unwrap();
        Ok(())
    }

    fn statuses(root: &Path, release: &str, check: bool, install: impl FnMut(&Path) -> Result<(), String>) -> (Result<PathBuf, String>, Vec<String>) {
        let mut progress = Vec::new();
        let result = ensure_at(root, release, check, &mut |status| progress.push(status.to_owned()), install);
        (result, progress)
    }

    #[test]
    fn panel_installs_into_one_folder_and_reuses_it_until_a_new_registry_version() {
        let root = tempfile::tempdir().unwrap();
        let (entry, progress) = statuses(root.path(), "0.98.0", true, fake_install);
        assert_eq!(entry.unwrap(), root.path().join(INSTALL).join(ENTRY));
        assert!(progress.contains(&"Installing…".to_owned()));
        // Same registry version and a complete install: no npm run at all.
        let (again, progress) = statuses(root.path(), "0.98.0", true, |_| panic!("must reuse install"));
        assert_eq!(again.unwrap(), root.path().join(INSTALL).join(ENTRY));
        assert!(!progress.iter().any(|status| status == "Installing…" || status == "Reinstalling…"));
        // A new registry version installs in place, into the same folder.
        let mut installs = 0;
        let (updated, progress) = statuses(root.path(), "0.99.0", true, |destination| { installs += 1; fake_install(destination) });
        assert_eq!(updated.unwrap(), root.path().join(INSTALL).join(ENTRY));
        assert_eq!(installs, 1);
        assert!(progress.contains(&"Installing…".to_owned()));
        assert_eq!(std::fs::read_to_string(root.path().join(INSTALLED_RELEASE)).unwrap(), "0.99.0");
    }

    #[test]
    fn panel_reinstalls_a_broken_install_in_the_same_start() {
        for broken in ["entry", "dependency"] {
            let root = tempfile::tempdir().unwrap();
            statuses(root.path(), "0.98.0", true, fake_install).0.unwrap();
            let modules = root.path().join(INSTALL).join("node_modules");
            if broken == "entry" { std::fs::remove_file(root.path().join(INSTALL).join(ENTRY)).unwrap(); }
            else { std::fs::remove_file(modules.join("dependency/package.json")).unwrap(); }
            std::fs::write(modules.join("leftover"), "cleared before reinstalling").unwrap();
            let (entry, progress) = statuses(root.path(), "0.98.0", true, fake_install);
            assert!(entry.unwrap().is_file(), "{broken}");
            assert!(progress.contains(&"Reinstalling…".to_owned()), "{broken}");
            assert!(!modules.join("leftover").exists(), "{broken}");
            assert!(!has_incomplete_package(&modules), "{broken}");
        }
    }

    #[test]
    fn half_extracted_npm_install_is_cleared_and_installed_once_more() {
        let root = tempfile::tempdir().unwrap();
        let mut installs = 0;
        let (entry, _) = statuses(root.path(), "0.98.0", true, |destination| {
            installs += 1;
            fake_install(destination)?;
            if installs == 1 { std::fs::create_dir_all(destination.join("node_modules/@scope/half-extracted")).unwrap(); }
            Ok(())
        });
        assert!(entry.unwrap().is_file());
        assert_eq!(installs, 2);
        // Still incomplete after the second try: an error, never a broken start.
        let root = tempfile::tempdir().unwrap();
        let (result, _) = statuses(root.path(), "0.98.0", true, |destination| {
            fake_install(destination)?;
            std::fs::create_dir_all(destination.join("node_modules/half-extracted")).unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert!(!root.path().join(INSTALLED_RELEASE).exists());
    }

    #[test]
    fn one_off_starts_the_installed_version_without_checking() {
        let root = tempfile::tempdir().unwrap();
        // Nothing installed yet: a one-off still has to install to answer at all.
        let (entry, _) = statuses(root.path(), "0.98.0", false, fake_install);
        let entry = entry.unwrap();
        // A new registry version or a half-extracted dependency is left for the panel's check.
        std::fs::remove_file(root.path().join(INSTALL).join("node_modules/dependency/package.json")).unwrap();
        let (again, progress) = statuses(root.path(), "0.99.0", false, |_| panic!("one-off must not install"));
        assert_eq!(again.unwrap(), entry);
        assert!(!progress.iter().any(|status| status == "Checking…" || status == "Installing…" || status == "Reinstalling…"));
    }

    #[test]
    fn old_version_folders_are_removed_and_other_files_kept() {
        let root = tempfile::tempdir().unwrap();
        for old in ["0.97.0", "0.98.1", ".tmpStaging"] {
            fake_install(&root.path().join(old)).unwrap();
        }
        std::fs::write(root.path().join("registry.json"), "kept").unwrap();
        std::fs::create_dir(root.path().join("not-a-version")).unwrap();
        statuses(root.path(), "0.99.0", true, fake_install).0.unwrap();
        for old in ["0.97.0", "0.98.1", ".tmpStaging"] { assert!(!root.path().join(old).exists(), "{old}"); }
        for kept in [INSTALL, INSTALLED_RELEASE, "install.lock", "registry.json", "not-a-version"] {
            assert!(root.path().join(kept).exists(), "{kept}");
        }
    }

    #[test]
    fn node_runs_the_entry_directly_with_literal_paths() {
        let node = PathBuf::from("node directory").join("node executable");
        let entry = PathBuf::from("cache directory").join("adapter & literal.js");
        let config = configured_agent(&node, &entry, OsStr::new("synthetic path")).into_config();
        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["command"], node.to_string_lossy().as_ref());
        assert_eq!(value["args"], serde_json::json!([entry.to_string_lossy()]));
        assert_eq!(value["env"]["PATH"], "synthetic path");
    }

    #[test]
    fn failed_or_incomplete_install_is_retryable() {
        for incomplete in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let result = ensure_at(root.path(), "0.98.0", true, &mut |_| {}, |_| if incomplete { Ok(()) } else { Err("synthetic npm error".into()) });
            assert!(result.is_err());
            assert!(!root.path().join(INSTALLED_RELEASE).exists());
            let (entry, progress) = statuses(root.path(), "0.98.0", true, fake_install);
            assert!(entry.unwrap().is_file());
            assert!(progress.contains(&"Reinstalling…".to_owned()));
        }
    }

    #[test]
    fn simultaneous_sessions_share_one_complete_install() {
        let root = tempfile::tempdir().unwrap();
        let installs = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let attempts: Vec<_> = (0..2).map(|_| scope.spawn(|| {
                ensure_at(root.path(), "0.98.0", true, &mut |_| {}, |destination| {
                    installs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    fake_install(destination)
                }).unwrap()
            })).collect();
            for attempt in attempts { assert!(attempt.join().unwrap().is_file()); }
        });
        assert_eq!(installs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
