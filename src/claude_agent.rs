//! Install the registry-selected Claude adapter and run its published entry through Node.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use agent_client_protocol::{AcpAgent, AcpAgentConfig};

const PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
const ENTRY: &str = "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js";
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

fn registry_release(root: &Path) -> Result<Release, String> {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Release)>> = std::sync::Mutex::new(None);
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

pub fn agent(path: &OsStr, mut progress: impl FnMut(&str)) -> Result<AcpAgent, String> {
    let node = crate::agent_launch::find_in(if cfg!(windows) { "node.exe" } else { "node" }, path)
        .ok_or(crate::agent_launch::NODE_MISSING)?;
    progress("Checking for updates…");
    let root = crate::config_path("agents/claude").ok_or("Could not locate the editor's agent cache")?;
    let release = registry_release(&root)?;
    let package = release.package_spec()?;
    let entry = ensure_at(&root, &release.version, true, &mut progress, |destination| {
        let npm = crate::agent_launch::find_in(if cfg!(windows) { "npm.cmd" } else { "npm" }, path)
            .ok_or("npm was not found. Install Node.js with npm, then restart the editor.")?;
        let cli = if cfg!(windows) {
            npm.parent().ok_or("npm has no parent directory")?.join("node_modules/npm/bin/npm-cli.js")
        } else { std::fs::canonicalize(npm).map_err(|e| e.to_string())? };
        let output = std::process::Command::new(&node).arg(cli)
            .args(["install", "--prefix"]).arg(destination)
            .args(["--no-audit", "--no-fund", "--save-exact", &package])
            .env("PATH", path).stdin(std::process::Stdio::null())
            .output().map_err(|e| format!("Could not install Claude ACP: {e}"))?;
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

fn ensure_at(root: &Path, directory: &str, refresh: bool, progress: &mut impl FnMut(&str), install: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(root.join("install.lock")).map_err(|e| e.to_string())?;
    progress("Waiting for installation…");
    fs2::FileExt::lock_exclusive(&lock).map_err(|e| e.to_string())?;
    let installed = root.join(directory);
    let ready = installed.join(ENTRY).is_file();
    if ready && !refresh { return Ok(installed.join(ENTRY)); }
    let updating = ready || std::fs::read_dir(root).map_err(|e| e.to_string())?
        .filter_map(Result::ok).any(|entry| entry.path().join(ENTRY).is_file());
    progress(if updating { "Updating…" } else { "Installing…" });
    let staging = tempfile::tempdir_in(root).map_err(|e| e.to_string())?;
    let payload = staging.path().join("payload");
    std::fs::create_dir(&payload).map_err(|e| e.to_string())?;
    install(&payload)?;
    if !payload.join(ENTRY).is_file() { return Err("Claude ACP install is missing its entry point. Try again.".into()); }
    // Publish under the resolved version, preserving files used by existing sessions.
    let installed = if refresh {
        let manifest = std::fs::read_to_string(payload.join("node_modules/@agentclientprotocol/claude-agent-acp/package.json"))
            .map_err(|e| format!("Could not read installed Claude version: {e}"))?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest).map_err(|e| e.to_string())?;
        let version = manifest["version"].as_str().ok_or("Claude package has no version")?;
        semver::Version::parse(version).map_err(|e| e.to_string())?;
        let installed = root.join(version);
        if installed.join(ENTRY).is_file() { return Ok(installed.join(ENTRY)); }
        installed
    } else { installed };
    if installed.exists() { std::fs::remove_dir_all(&installed).map_err(|e| e.to_string())?; }
    std::fs::rename(&payload, &installed).map_err(|e| e.to_string())?;
    Ok(installed.join(ENTRY))
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

    fn install_version(destination: &Path, version: &str) -> Result<(), String> {
        fake_install(destination)?;
        std::fs::write(destination.join("node_modules/@agentclientprotocol/claude-agent-acp/package.json"),
            serde_json::json!({"version":version}).to_string()).unwrap();
        Ok(())
    }

    #[test]
    fn registry_install_checks_again_and_preserves_running_versions() {
        let root = tempfile::tempdir().unwrap();
        let mut progress = Vec::new();
        let first = ensure_at(root.path(), "0.98.0", true, &mut |status| progress.push(status.to_owned()),
            |destination| install_version(destination, "0.97.0")).unwrap();
        assert!(progress.iter().any(|status| status == "Installing…"));
        assert_eq!(first, root.path().join("0.97.0").join(ENTRY));
        progress.clear();
        let second = ensure_at(root.path(), "0.99.0", true, &mut |status| progress.push(status.to_owned()),
            |destination| install_version(destination, "0.99.0")).unwrap();
        assert!(progress.iter().any(|status| status == "Updating…"));
        assert_ne!(first, second);
        assert!(first.is_file() && second.is_file());
        // npm can resolve a newer eligible release even with an unchanged ceiling.
        let third = ensure_at(root.path(), "0.99.0", true, &mut |_| {},
            |destination| install_version(destination, "0.99.0")).unwrap();
        assert_eq!(third, second);
        assert!(ensure_at(root.path(), "0.99.0", true, &mut |_| {}, |_| Err("offline".into())).is_err());
        assert!(first.is_file() && second.is_file());
    }

    fn fake_install(destination: &Path) -> Result<(), String> {
        let entry = destination.join(ENTRY);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(entry, "synthetic JavaScript; no executable permission required").unwrap();
        Ok(())
    }

    #[test]
    fn installs_once_and_reuses_the_pinned_entry_without_executing_a_shim() {
        let root = tempfile::tempdir().unwrap();
        let entry = ensure_at(root.path(), "test-release", false, &mut |_| {}, fake_install).unwrap();
        assert_eq!(entry, root.path().join("test-release").join(ENTRY));
        assert_eq!(ensure_at(root.path(), "test-release", false, &mut |_| {}, |_| panic!("must reuse install")).unwrap(), entry);
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
    fn failed_or_incomplete_install_is_retryable_and_never_published() {
        for incomplete in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let result = ensure_at(root.path(), "test-release", false, &mut |_| {}, |_| if incomplete { Ok(()) } else { Err("synthetic npm error".into()) });
            assert!(result.is_err());
            assert!(!root.path().join("test-release").exists());
            assert!(ensure_at(root.path(), "test-release", false, &mut |_| {}, fake_install).unwrap().is_file());
        }
    }

    #[test]
    fn incomplete_cache_is_replaced_only_after_successful_install() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("test-release");
        std::fs::create_dir(&installed).unwrap();
        std::fs::write(installed.join("incomplete"), "preserve until ready").unwrap();
        assert!(ensure_at(root.path(), "test-release", false, &mut |_| {}, |_| Err("offline".into())).is_err());
        assert!(installed.join("incomplete").exists());
        assert!(ensure_at(root.path(), "test-release", false, &mut |_| {}, fake_install).unwrap().is_file());
        assert!(!installed.join("incomplete").exists());
    }

    #[test]
    fn simultaneous_sessions_share_one_complete_install() {
        let root = tempfile::tempdir().unwrap();
        let installs = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let attempts: Vec<_> = (0..2).map(|_| scope.spawn(|| {
                ensure_at(root.path(), "test-release", false, &mut |_| {}, |destination| {
                    installs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    fake_install(destination)
                }).unwrap()
            })).collect();
            for attempt in attempts { assert!(attempt.join().unwrap().is_file()); }
        });
        assert_eq!(installs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
