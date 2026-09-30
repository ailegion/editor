//! Fetch, pull and push with the real `git` ([`cli`]), so the user's configuration (pull
//! strategy, credential helpers, hooks) decides everything; nothing is forced.

use std::path::Path;

use super::cli::{self, Access};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    Fetch,
    Pull,
    Push,
}

impl Sync {
    pub const ALL: [Sync; 3] = [Sync::Fetch, Sync::Pull, Sync::Push];

    pub fn label(self) -> &'static str {
        match self {
            Sync::Fetch => "Fetch",
            Sync::Pull => "Pull",
            Sync::Push => "Push",
        }
    }

    pub fn done(self) -> &'static str {
        match self {
            Sync::Fetch => "Fetched",
            Sync::Pull => "Pulled",
            Sync::Push => "Pushed",
        }
    }
}

pub fn run(cwd: &Path, sync: Sync) -> Result<(), String> {
    match sync {
        Sync::Fetch => cli::run(cwd, &["fetch"], None, Access::Write).map(|_| ()),
        Sync::Pull => cli::run(cwd, &["pull"], None, Access::Write).map(|_| ()),
        Sync::Push => push(cwd),
    }
}

/// `git push`; a branch's first push, when there is exactly one remote, publishes it there
/// and sets it as upstream (`push -u`), like VS Code's Publish Branch.
fn push(cwd: &Path) -> Result<(), String> {
    let has_upstream = cli::run(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"], None, Access::Read).is_ok();
    if !has_upstream {
        let branch = String::from_utf8_lossy(&cli::run(cwd, &["branch", "--show-current"], None, Access::Read)?).trim().to_owned();
        let remotes = String::from_utf8_lossy(&cli::run(cwd, &["remote"], None, Access::Read)?).into_owned();
        if let ([remote], false) = (remotes.lines().collect::<Vec<_>>().as_slice(), branch.is_empty()) {
            return cli::run(cwd, &["push", "-u", remote, &branch], None, Access::Write).map(|_| ());
        }
    }
    cli::run(cwd, &["push"], None, Access::Write).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(cli::run(root, args, None, Access::Write).unwrap()).unwrap().trim().to_owned()
    }

    fn commit(root: &Path, file: &str) {
        std::fs::write(root.join(file), file).unwrap();
        git(root, &["add", "--", file]);
        git(root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-q", "-m", file]);
    }

    #[test]
    fn fetch_pull_and_push_against_a_local_remote() {
        // Bare repositories on disk stand in for servers, so no network is involved.
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare"]);
        let remote_path = remote.path().to_string_lossy().into_owned();
        let a = tempfile::tempdir().unwrap();
        let a = a.path();
        git(a, &["init", "-q"]);
        git(a, &["switch", "-q", "-c", "work"]);
        commit(a, "one.txt");
        git(a, &["remote", "add", "origin", &remote_path]);

        // First push of a branch with one remote: published and tracked.
        run(a, Sync::Push).unwrap();
        assert_eq!(git(a, &["rev-parse", "--abbrev-ref", "work@{upstream}"]), "origin/work");
        assert_eq!(git(remote.path(), &["rev-parse", "work"]), git(a, &["rev-parse", "HEAD"]));

        let b_dir = tempfile::tempdir().unwrap();
        let b = b_dir.path().join("clone");
        git(a, &["clone", "-q", "-b", "work", &remote_path, &b.to_string_lossy()]);

        // Later pushes are plain `git push` to the upstream.
        commit(a, "two.txt");
        run(a, Sync::Push).unwrap();
        assert_eq!(git(remote.path(), &["rev-parse", "work"]), git(a, &["rev-parse", "HEAD"]));

        run(&b, Sync::Fetch).unwrap();
        assert_eq!(git(&b, &["rev-parse", "origin/work"]), git(a, &["rev-parse", "HEAD"]));
        assert!(!b.join("two.txt").exists(), "fetch leaves the working tree alone");
        run(&b, Sync::Pull).unwrap();
        assert!(b.join("two.txt").exists());
        assert_eq!(git(&b, &["rev-parse", "HEAD"]), git(a, &["rev-parse", "HEAD"]));

        // With two remotes there is no obvious target: git's own error, nothing guessed.
        git(&b, &["remote", "add", "second", &remote_path]);
        git(&b, &["switch", "-q", "-c", "unpublished"]);
        // Outcomes only: git words its messages in the user's language.
        assert!(run(&b, Sync::Push).is_err());
        assert!(cli::run(&b, &["rev-parse", "unpublished@{upstream}"], None, Access::Read).is_err());
        assert!(cli::run(remote.path(), &["rev-parse", "--verify", "unpublished"], None, Access::Read).is_err());
        // Pull on a branch without upstream reports git's message too.
        assert!(run(&b, Sync::Pull).is_err());
    }
}
