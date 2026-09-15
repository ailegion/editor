//! Per-file diff gutter markers (added/modified/removed lines vs. git HEAD), computed by
//! shelling out to `git diff -U0` -- same approach as `git.rs`'s status/commit -- and parsed
//! from the unified-diff hunk headers. Zero context lines (`-U0`) means every hunk boundary
//! maps directly to a contiguous run of changed lines, nothing to trim.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    Added,
    Modified,
    /// Attached to the new-file line immediately after which content was removed (or line 0
    /// if the removal was at the very start of the file) -- there's no "this line" to mark
    /// for a pure deletion, since the deleted lines no longer exist in the new file.
    Removed,
}

/// Runs `git diff -U0` for `path` (which must be inside `cwd`, the project root) and returns
/// new-file line numbers (0-indexed) paired with their diff status. Returns an empty list on
/// any failure (not a git repo, `git` missing, file untracked with no diff, ...) -- the
/// gutter just shows nothing rather than an error, matching how a brand new/clean file has
/// nothing to mark anyway.
pub async fn diff_for_file(cwd: PathBuf, path: PathBuf) -> Vec<(usize, LineStatus)> {
    tokio::task::spawn_blocking(move || {
        let Ok(relative) = path.strip_prefix(&cwd) else {
            return Vec::new();
        };
        let Ok(output) = std::process::Command::new("git")
            .args(["diff", "--no-color", "-U0", "--"])
            .arg(relative)
            .current_dir(&cwd)
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        parse_hunks(&String::from_utf8_lossy(&output.stdout))
    })
    .await
    .unwrap_or_default()
}

fn parse_hunks(diff: &str) -> Vec<(usize, LineStatus)> {
    let mut result = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ -") else { continue };
        let Some(header_end) = rest.find(" @@") else { continue };
        let Some((old_part, new_part)) = rest[..header_end].split_once(" +") else { continue };
        let (_, old_count) = parse_range(old_part);
        let (new_start, new_count) = parse_range(new_part);

        if new_count == 0 {
            // A zero-length new-range hunk header already points at the new-file line right
            // after the deletion (1-indexed start of an empty range == that many lines *in*
            // from a 0-indexed position) -- unlike the add/modify branch below, no
            // 1-indexed-to-0-indexed shift is needed here.
            result.push((new_start, LineStatus::Removed));
        } else {
            let status = if old_count == 0 { LineStatus::Added } else { LineStatus::Modified };
            for line_i in new_start..new_start + new_count {
                result.push((line_i.saturating_sub(1), status));
            }
        }
    }
    result
}

/// Parses a `start[,count]` hunk range; `count` defaults to 1 when omitted (unified diff
/// format elides it for single-line ranges).
fn parse_range(part: &str) -> (usize, usize) {
    match part.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(0)),
        None => (part.parse().unwrap_or(0), 1),
    }
}
