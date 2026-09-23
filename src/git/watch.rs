//! File-system watching that tells the UI when git state may have changed, so status is
//! refreshed on real changes instead of on every UI action. Events are coalesced until the
//! file system has been quiet for [`SETTLE`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher as _};

use super::repo::Repo;

const SETTLE: Duration = Duration::from_millis(300);

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Changes {
    /// A tracked or untracked (not ignored) working-tree file changed.
    pub worktree: bool,
    /// The index, `HEAD` or a ref changed: staged versions and the branch may differ.
    pub repository: bool,
}

#[derive(Default)]
struct Pending {
    changes: Changes,
    last: Option<Instant>,
}

pub struct Watch {
    /// Filled in by a background thread: registering a large tree can take a while (inotify
    /// adds one watch per directory). Dropping the `Watch` drops the watcher.
    _watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    pending: Arc<Mutex<Pending>>,
}

impl Watch {
    pub fn start(repo: Arc<Repo>) -> Watch {
        let pending = Arc::new(Mutex::new(Pending::default()));
        let holder = Arc::new(Mutex::new(None));
        let (sink, slot) = (pending.clone(), holder.clone());
        std::thread::spawn(move || {
            if let Some(watcher) = Self::watch(&repo, sink) { *slot.lock().unwrap() = Some(watcher); }
        });
        Watch { _watcher: holder, pending }
    }

    fn watch(repo: &Repo, sink: Arc<Mutex<Pending>>) -> Option<notify::RecommendedWatcher> {
        let mut filter = Filter::new(repo);
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) { return; }
            let mut changes = Changes::default();
            for path in &event.paths {
                match filter.classify(path) {
                    Some(Kind::Repository) => changes.repository = true,
                    Some(Kind::Worktree) => changes.worktree = true,
                    None => {}
                }
            }
            if changes.repository || changes.worktree {
                let mut pending = sink.lock().unwrap();
                pending.changes.repository |= changes.repository;
                pending.changes.worktree |= changes.worktree;
                pending.last = Some(Instant::now());
            }
        }).ok()?;
        watcher.watch(repo.workdir(), RecursiveMode::Recursive).ok()?;
        for dir in [repo.git_dir(), repo.common_dir()] {
            if !dir.starts_with(repo.workdir()) {
                let _ = watcher.watch(&dir, RecursiveMode::Recursive);
            }
        }
        Some(watcher)
    }

    /// The changes seen since the last call, once events have stopped arriving for a moment.
    pub fn take_settled(&self) -> Option<Changes> {
        let mut pending = self.pending.lock().unwrap();
        if pending.last?.elapsed() < SETTLE { return None; }
        pending.last = None;
        Some(std::mem::take(&mut pending.changes))
    }
}

enum Kind {
    Repository,
    Worktree,
}

/// Decides which events matter: git's own state files, and working-tree paths git does not ignore.
struct Filter {
    repo: gix::Repository,
    /// Each directory as opened and, if different, fully resolved: some platforms report
    /// events by resolved path (macOS FSEvents turns `/var/...` into `/private/var/...`).
    workdirs: Vec<PathBuf>,
    git_dirs: Vec<PathBuf>,
    excludes: Option<gix::worktree::Stack>,
}

fn with_resolved(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut all = Vec::new();
    for path in paths {
        let resolved = super::repo::dunce_canonicalize(&path);
        all.push(path);
        all.extend(resolved.filter(|resolved| !all.contains(resolved)));
    }
    all
}

impl Filter {
    fn new(repo: &Repo) -> Filter {
        Filter {
            repo: repo.thread_local(),
            workdirs: with_resolved([repo.workdir().to_path_buf()]),
            git_dirs: with_resolved([repo.git_dir(), repo.common_dir()]),
            excludes: None,
        }
    }

    fn classify(&mut self, path: &Path) -> Option<Kind> {
        if let Some(inside) = self.git_dirs.iter().find_map(|dir| path.strip_prefix(dir).ok()) {
            let first = inside.components().next()?.as_os_str().to_str()?;
            return matches!(first, "index" | "HEAD" | "packed-refs" | "refs" | "MERGE_HEAD" | "REBASE_HEAD" | "CHERRY_PICK_HEAD")
                .then_some(Kind::Repository);
        }
        let rela = self.workdirs.iter().find_map(|dir| path.strip_prefix(dir).ok())?.to_path_buf();
        let rela = rela.as_path();
        if rela.file_name().is_some_and(|name| name == ".gitignore") {
            self.excludes = None; // Reload ignore rules on next use.
        }
        (!self.is_ignored(rela)).then_some(Kind::Worktree)
    }

    /// Whether `rela` or one of its parent directories is excluded by git's ignore rules.
    fn is_ignored(&mut self, rela: &Path) -> bool {
        if self.excludes.is_none() {
            let Ok(index) = self.repo.index_or_empty() else { return false };
            let source = gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped;
            self.excludes = self.repo.excludes(&index, None, source).ok().map(|stack| stack.detach());
        }
        let Some(excludes) = &mut self.excludes else { return false };
        let mut prefix = PathBuf::new();
        let mut components = rela.components().peekable();
        while let Some(component) = components.next() {
            prefix.push(component);
            let mode = components.peek().is_some().then_some(gix::index::entry::Mode::DIR);
            if excludes.at_path(prefix.as_path(), mode, &self.repo.objects).is_ok_and(|platform| platform.is_excluded()) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        super::super::cli::run(root, args, None, super::super::cli::Access::Write).unwrap();
    }

    /// Waits up to `timeout` for the next settled batch of changes.
    fn next_changes(watch: &Watch, timeout: Duration) -> Option<Changes> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(changes) = watch.take_settled() { return Some(changes); }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn repo_with_ignored_target(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("editor-{name}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        git(&root, &["init"]);
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        root
    }

    /// Event paths may be the resolved form of the opened directory (macOS reports
    /// `/private/var/...` for `/var/...`; Windows may expand 8.3 short names).
    #[test]
    fn classifies_paths_given_as_opened_or_resolved() {
        let root = repo_with_ignored_target("watch-paths");
        // Open the project through a link so the opened and resolved paths always differ.
        let link = root.with_file_name(format!("editor-watch-link-{}", std::process::id()));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, &link).unwrap();
        #[cfg(windows)]
        assert!(std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&root).output().unwrap().status.success());
        let mut filter = Filter::new(&Repo::discover(&link).unwrap());
        let resolved = super::super::repo::dunce_canonicalize(&root).unwrap();
        for base in [&link, &resolved] {
            assert!(matches!(filter.classify(&base.join("main.rs")), Some(Kind::Worktree)), "{}", base.display());
            assert!(filter.classify(&base.join("target/debug/build.o")).is_none(), "{}", base.display());
            assert!(matches!(filter.classify(&base.join(".git/index")), Some(Kind::Repository)), "{}", base.display());
            assert!(filter.classify(&base.join(".git/objects/ab/cdef")).is_none(), "{}", base.display());
        }
        // Removes the link itself, not the directory it points to.
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_worktree_and_index_changes_but_not_ignored_files() {
        let root = repo_with_ignored_target("watch-events");
        let watch = Watch::start(Arc::new(Repo::discover(&root).unwrap()));

        // Registration runs on a background thread and some backends (FSEvents) start
        // delivering shortly after; write until an event arrives so the checks below are live.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut attempt = 0;
        loop {
            attempt += 1;
            std::fs::write(root.join("main.rs"), format!("// {attempt}\n")).unwrap();
            if next_changes(&watch, Duration::from_secs(1)).is_some_and(|changes| changes.worktree) { break; }
            assert!(Instant::now() < deadline, "no file-system events were delivered");
        }
        // Drop events from any earlier write still in flight.
        std::thread::sleep(Duration::from_secs(1));
        watch.take_settled();

        std::fs::write(root.join("target/debug/build.o"), "object").unwrap();
        assert_eq!(next_changes(&watch, Duration::from_secs(2)), None, "ignored build output must not trigger a refresh");

        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        assert_eq!(next_changes(&watch, Duration::from_secs(10)).map(|c| c.worktree), Some(true));

        git(&root, &["add", "main.rs"]);
        assert_eq!(next_changes(&watch, Duration::from_secs(10)).map(|c| c.repository), Some(true));
        drop(watch);
        std::fs::remove_dir_all(root).unwrap();
    }
}
