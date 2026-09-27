//! A file edit made or proposed by an AI panel, shown as a diff for review.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// `None` when the edit creates the file.
    pub old: Option<String>,
    pub new: String,
}

impl FileDiff {
    pub fn patch(&self) -> String {
        crate::git_preview::text_patch(self.old.as_deref().unwrap_or_default(), &self.new)
    }

    /// Lines added and removed.
    pub fn stats(&self) -> (usize, usize) {
        let patch = self.patch();
        let count = |prefix: char| patch.lines().filter(|line| line.starts_with(prefix)).count();
        (count('+'), count('-'))
    }

    pub fn file_name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }

    /// The path, then the hunks: what an approval card shows instead of the raw file text.
    pub fn review_text(&self) -> String {
        let (added, removed) = self.stats();
        let kind = if self.old.is_none() { "new file, " } else { "" };
        format!("{} ({kind}+{added} −{removed})\n{}", self.path, self.patch())
    }

    /// The diff for the preview pane, labelled relative to `root` when inside it.
    pub fn preview(&self, root: &std::path::Path) -> crate::git_preview::Preview {
        let path = std::path::Path::new(&self.path);
        let label = path.strip_prefix(root).unwrap_or(path).display().to_string();
        crate::git_preview::Preview { root: root.to_path_buf(), path: label, result: Some(Ok(self.patch())), live: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_show_hunks_and_counts() {
        let edit = FileDiff { path: "src/lib.rs".into(), old: Some("a\nb\nc\n".into()), new: "a\nB\nc\nd\n".into() };
        assert_eq!(edit.stats(), (2, 1));
        assert_eq!(edit.file_name(), "lib.rs");
        let review = edit.review_text();
        assert!(review.starts_with("src/lib.rs (+2 −1)\n@@"), "{review}");
        assert!(review.contains("-b\n+B\n") && review.contains("+d\n"));
        let created = FileDiff { path: "C:\\p\\new.txt".into(), old: None, new: "hi\n".into() };
        assert_eq!(created.file_name(), "new.txt");
        assert!(created.review_text().contains("(new file, +1 −0)"));
        let preview = FileDiff { path: std::path::Path::new("root").join("x.rs").display().to_string(), ..created }.preview(std::path::Path::new("root"));
        assert_eq!(preview.path, "x.rs");
        assert!(!preview.live);
    }
}
