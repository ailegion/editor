//! Blame the current buffer, including unsaved edits, on a background task.
use std::path::Path;
use super::cli::{self, Access};

pub fn load(root: &Path, relative: &Path, text: &str) -> Vec<String> {
    let Ok(output) = cli::run(root, &["--literal-pathspecs", "blame", "--line-porcelain", "--contents", "-", "--", &relative.to_string_lossy()], Some(text.as_bytes()), Access::Read) else { return Vec::new() };
    parse(&String::from_utf8_lossy(&output), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
}

fn parse(output: &str, now: u64) -> Vec<String> {
    let mut result = Vec::new();
    let mut author = String::new();
    let mut timestamp = 0;
    let mut uncommitted = false;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("author ") { author = value.to_owned(); }
        else if let Some(value) = line.strip_prefix("author-time ") { timestamp = value.parse().unwrap_or(now); }
        else if line.starts_with('\t') {
            result.push(if uncommitted { "Uncommitted changes".into() } else { format!("{author}, {}", age(now.saturating_sub(timestamp))) });
        } else if let Some(hash) = line.split_whitespace().next() {
            if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) { uncommitted = hash.bytes().all(|b| b == b'0'); }
        }
    }
    result
}

fn age(seconds: u64) -> String {
    for (unit, size) in [("year", 31_536_000), ("month", 2_592_000), ("week", 604_800), ("day", 86_400), ("hour", 3600), ("minute", 60)] {
        let count = seconds / size;
        if count > 0 { return format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" }); }
    }
    "just now".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_and_unsaved_lines() {
        let output = format!("{} 1 1 1\nauthor A Person\nauthor-time 0\n\tone\n{} 2 2 1\nauthor Not Committed Yet\nauthor-time 10\n\ttwo\n", "a".repeat(40), "0".repeat(40));
        assert_eq!(parse(&output, 1_209_600), ["A Person, 2 weeks ago", "Uncommitted changes"]);
    }
}
