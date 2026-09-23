//! Branch and working-tree status from a single `git status --porcelain=v2 --branch -z`.

use std::path::Path;

use super::cli::{self, Access};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Branch {
    /// Branch name, or `None` when `HEAD` is detached.
    pub name: Option<String>,
    /// Abbreviated commit `HEAD` points to; `None` before the first commit.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

impl Branch {
    pub fn unborn(&self) -> bool { self.oid.is_none() }

    pub fn label(&self) -> String {
        match (&self.name, &self.oid) {
            (Some(name), _) => name.clone(),
            (None, Some(oid)) => format!("Detached at {oid}"),
            (None, None) => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChangedFile {
    /// Path relative to the directory status ran in.
    pub path: String,
    /// Two-character `XY` code as in short status: `"M "`, `" D"`, `"??"`, `"UU"`, ...
    pub status: String,
    /// Source path of a staged rename or copy.
    pub original: Option<String>,
}

impl ChangedFile {
    pub fn staged(&self) -> bool { self.status.as_bytes()[0] != b' ' && self.status != "??" }
    pub fn unstaged(&self) -> bool { self.status.as_bytes()[1] != b' ' }
    pub fn conflicted(&self) -> bool {
        matches!(self.status.as_str(), "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub branch: Branch,
    pub files: Vec<ChangedFile>,
}

pub fn load(cwd: &Path) -> Result<Status, String> {
    let output = cli::run(
        cwd,
        &["-c", "status.relativePaths=true", "status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
        None,
        Access::Read,
    )?;
    Ok(parse(&String::from_utf8_lossy(&output)))
}

fn parse(output: &str) -> Status {
    let mut status = Status::default();
    let mut records = output.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => status.branch.oid = Some(value.chars().take(7).collect()),
                "branch.head" if value != "(detached)" => status.branch.name = Some(value.to_owned()),
                "branch.upstream" => status.branch.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') { status.branch.ahead = n.parse().unwrap_or(0); }
                        if let Some(n) = part.strip_prefix('-') { status.branch.behind = n.parse().unwrap_or(0); }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = record.as_bytes()[0];
        // Fields before the path: ordinary 8, rename/copy 9, unmerged 10.
        let fields = match kind {
            b'1' => 8,
            b'2' => 9,
            b'u' => 10,
            b'?' => {
                status.files.push(ChangedFile { path: record[2..].to_owned(), status: "??".into(), original: None });
                continue;
            }
            _ => continue, // `!` ignored entries are not requested.
        };
        let mut parts = record.splitn(fields + 1, ' ');
        let xy = parts.nth(1).unwrap_or("..").replace('.', " ");
        let Some(path) = parts.nth(fields - 2) else { continue };
        // With -z the rename source follows as its own record.
        let original = if kind == b'2' { records.next().map(str::to_owned) } else { None };
        status.files.push(ChangedFile { path: path.to_owned(), status: xy, original });
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_renames_conflicts_and_untracked() {
        let output = [
            "# branch.oid 0123456789abcdef0123456789abcdef01234567",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 aaaaaaa aaaaaaa src/a file.rs",
            "2 R. N... 100644 100644 100644 bbbbbbb bbbbbbb R100 new name.txt",
            "old name.txt",
            "u UU N... 100644 100644 100644 100644 c1 c2 c3 conflict.rs",
            "? new [1].txt",
            "",
        ].join("\0");
        let status = parse(&output);
        assert_eq!(status.branch, Branch {
            name: Some("main".into()), oid: Some("0123456".into()), upstream: Some("origin/main".into()), ahead: 2, behind: 1,
        });
        assert_eq!(status.files, vec![
            ChangedFile { path: "src/a file.rs".into(), status: " M".into(), original: None },
            ChangedFile { path: "new name.txt".into(), status: "R ".into(), original: Some("old name.txt".into()) },
            ChangedFile { path: "conflict.rs".into(), status: "UU".into(), original: None },
            ChangedFile { path: "new [1].txt".into(), status: "??".into(), original: None },
        ]);
        assert!(status.files[2].conflicted() && status.files[2].staged() && status.files[2].unstaged());
        assert!(!status.files[3].staged() && status.files[3].unstaged());
    }

    #[test]
    fn unborn_and_detached_heads() {
        let unborn = parse("# branch.oid (initial)\0# branch.head main\0");
        assert!(unborn.branch.unborn());
        assert_eq!(unborn.branch.label(), "main");
        let detached = parse("# branch.oid 0123456789\0# branch.head (detached)\0");
        assert_eq!(detached.branch.label(), "Detached at 0123456");
    }
}
