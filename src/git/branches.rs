//! Branch picker data: the repository's branches and switching to or creating one. Both go
//! through the real `git` ([`cli`]) so hooks such as `post-checkout` run.

use std::path::Path;

use super::cli::{self, Access};

#[derive(Debug, Clone, PartialEq)]
pub struct Branch {
    /// `main`, or `origin/main` for a remote-tracking branch.
    pub name: String,
    pub remote: bool,
    pub author: String,
    /// Unix seconds of the tip commit; 0 when unknown.
    pub date: u64,
    pub subject: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Local,
    Remote,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::All, Filter::Local, Filter::Remote];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All Branches",
            Filter::Local => "Local Branches",
            Filter::Remote => "Remote Branches",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Check out an existing local branch.
    Switch(String),
    /// Create a local branch tracking this remote one and check it out.
    Track(String),
    /// Create a branch at the current commit and check it out.
    Create(String),
}

/// Fields separated by NUL, which `for-each-ref` writes for `%00`.
const FORMAT: &str = "--format=%(refname)%00%(symref)%00%(authorname)%00%(committerdate:unix)%00%(subject)";

/// Local branches then remote ones, each most recently committed first.
pub fn load(cwd: &Path) -> Result<Vec<Branch>, String> {
    let output = cli::run(cwd, &["for-each-ref", "--sort=-committerdate", FORMAT, "refs/heads", "refs/remotes"], None, Access::Read)?;
    Ok(parse(&String::from_utf8_lossy(&output)))
}

fn parse(output: &str) -> Vec<Branch> {
    let mut branches: Vec<Branch> = output.lines().filter_map(|line| {
        let mut fields = line.split('\0');
        let (refname, symref, author, date) = (fields.next()?, fields.next()?, fields.next()?, fields.next()?);
        // `origin/HEAD` only names the remote's default branch, which is listed itself.
        if !symref.is_empty() { return None; }
        // Full ref names, not `:short`, which turns ambiguous names into `heads/x`.
        let (name, remote) = match refname.strip_prefix("refs/heads/") {
            Some(name) => (name, false),
            None => (refname.strip_prefix("refs/remotes/")?, true),
        };
        Some(Branch {
            name: name.to_owned(),
            remote,
            author: author.to_owned(),
            date: date.parse().unwrap_or(0),
            subject: fields.next().unwrap_or_default().to_owned(),
        })
    }).collect();
    // Stable, so each group keeps git's newest-first order.
    branches.sort_by_key(|branch| branch.remote);
    branches
}

/// Branches matching `query` (case-insensitive substring) and `filter`.
pub fn visible<'a>(branches: &'a [Branch], query: &str, filter: Filter) -> Vec<&'a Branch> {
    let query = query.trim().to_lowercase();
    branches.iter()
        .filter(|branch| match filter {
            Filter::All => true,
            Filter::Local => !branch.remote,
            Filter::Remote => branch.remote,
        })
        .filter(|branch| branch.name.to_lowercase().contains(&query))
        .collect()
}

/// The name to offer for a new branch: the typed text, unless a local branch has it already.
/// Git itself decides whether the name is valid.
pub fn new_name<'a>(branches: &[Branch], query: &'a str) -> Option<&'a str> {
    let name = query.trim();
    (!name.is_empty() && !branches.iter().any(|branch| !branch.remote && branch.name == name)).then_some(name)
}

pub fn run(cwd: &Path, action: &Action) -> Result<(), String> {
    let args = match action {
        Action::Switch(name) => ["switch", name.as_str()].to_vec(),
        Action::Track(remote) => ["switch", "--track", remote.as_str()].to_vec(),
        Action::Create(name) => ["switch", "-c", name.as_str()].to_vec(),
    };
    cli::run(cwd, &args, None, Access::Write).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        String::from_utf8(cli::run(root, args, None, Access::Write).unwrap()).unwrap()
    }

    fn current(root: &Path) -> String {
        git(root, &["branch", "--show-current"]).trim().to_owned()
    }

    fn commit(root: &Path, file: &str, message: &str) {
        std::fs::write(root.join(file), message).unwrap();
        git(root, &["add", "--", file]);
        git(root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-q", "-m", message]);
    }

    fn branch(name: &str, remote: bool) -> Branch {
        Branch { name: name.into(), remote, author: String::new(), date: 0, subject: String::new() }
    }

    #[test]
    fn output_is_parsed_locals_first_without_remote_head() {
        let output = [
            "refs/remotes/origin/test/e2e\0\0Ann\x001700000000\0fix: e2e",
            "refs/heads/dev\0\0Bob\x001690000000\0Add picker",
            "refs/remotes/origin/HEAD\0refs/remotes/origin/main\0Ann\x001600000000\0x",
            "refs/heads/heads/odd\0\0Bob\0\0",
        ].join("\n");
        let branches = parse(&output);
        assert_eq!(branches, [
            Branch { name: "dev".into(), remote: false, author: "Bob".into(), date: 1_690_000_000, subject: "Add picker".into() },
            Branch { name: "heads/odd".into(), remote: false, author: "Bob".into(), date: 0, subject: String::new() },
            Branch { name: "origin/test/e2e".into(), remote: true, author: "Ann".into(), date: 1_700_000_000, subject: "fix: e2e".into() },
        ]);
        assert!(parse("").is_empty());
    }

    #[test]
    fn search_filter_and_create_offer() {
        let branches = [branch("dev", false), branch("test/one", false), branch("origin/test/update-e2e", true), branch("origin/dev", true)];
        let names = |query, filter| visible(&branches, query, filter).iter().map(|b| b.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("TEST/", Filter::All), ["test/one", "origin/test/update-e2e"]);
        assert_eq!(names("test/", Filter::Local), ["test/one"]);
        assert_eq!(names("test/", Filter::Remote), ["origin/test/update-e2e"]);
        assert_eq!(names("", Filter::Remote), ["origin/test/update-e2e", "origin/dev"]);
        assert_eq!(new_name(&branches, " test/ "), Some("test/"));
        assert_eq!(new_name(&branches, "dev"), None, "a local branch has that name");
        assert_eq!(new_name(&branches, "origin/dev"), Some("origin/dev"), "only a remote one does");
        assert_eq!(new_name(&branches, "  "), None);
    }

    #[test]
    fn branches_are_listed_created_switched_and_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        // Before the first commit there is nothing to list, but a branch can still be started.
        assert!(load(root).unwrap().is_empty());
        run(root, &Action::Create("first".into())).unwrap();
        assert_eq!(current(root), "first");
        commit(root, "a.txt", "Initial");

        run(root, &Action::Create("feature/x".into())).unwrap();
        assert_eq!(current(root), "feature/x");
        commit(root, "b.txt", "Feature work");
        let err = run(root, &Action::Create("first".into())).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        for invalid in ["bad name", "-bad", "a..b"] {
            assert!(run(root, &Action::Create(invalid.into())).is_err(), "{invalid}");
        }
        assert_eq!(current(root), "feature/x");

        run(root, &Action::Switch("first".into())).unwrap();
        assert_eq!(current(root), "first");
        assert!(!root.join("b.txt").exists(), "the checkout matches the branch");

        // A remote-tracking branch, set up locally so no network is involved.
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare"]);
        git(root, &["remote", "add", "origin", &remote.path().to_string_lossy()]);
        git(root, &["push", "-q", "origin", "feature/x"]);
        git(root, &["branch", "-q", "-D", "feature/x"]);

        let branches = load(root).unwrap();
        let listed: Vec<_> = branches.iter().map(|b| (b.name.as_str(), b.remote, b.subject.as_str(), b.author.as_str())).collect();
        assert_eq!(listed, [("first", false, "Initial", "Test"), ("origin/feature/x", true, "Feature work", "Test")]);
        assert!(branches.iter().all(|b| b.date > 0));

        run(root, &Action::Track("origin/feature/x".into())).unwrap();
        assert_eq!(current(root), "feature/x");
        assert_eq!(git(root, &["rev-parse", "--abbrev-ref", "feature/x@{upstream}"]).trim(), "origin/feature/x");
        assert!(root.join("b.txt").exists());
    }
}
