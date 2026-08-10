//! "Find in Project": recursive text search across every file under the open folder.
//!
//! Search runs on `Submit` (Enter), not live-as-you-type -- walking + reading every file in
//! a large repo on every keystroke would jank the UI, and this app has no background-thread
//! plumbing for it yet (unlike `acp.rs`, which does exactly that for the AI agent). If this
//! ever feels too slow to use, that's the place to add: move `search()` onto a thread and
//! stream results back like `acp::AcpState` does.

use std::path::{Path, PathBuf};

use iced::widget::{button, column, scrollable, text, text_input};
use iced::{Element, Length};

/// Directories skipped during the walk. Not `.gitignore`-aware -- just the usual noisy,
/// huge, or binary-heavy directories that would otherwise dominate both the walk time and
/// the results list.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".idea",
    ".vscode",
];

const MAX_RESULTS: usize = 500;

pub struct SearchResult {
    pub path: PathBuf,
    pub line: usize,
    pub preview: String,
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    Submit,
    ResultClicked(usize),
}

/// Applies `message`. Returns `Some((path, line))` when a result was clicked, for the
/// caller to open and jump to; `root` is `None` when no folder is open, in which case
/// `Submit` is a no-op (there's nothing to walk).
pub fn update(state: &mut SearchState, message: Message, root: Option<&Path>) -> Option<(PathBuf, usize)> {
    match message {
        Message::QueryChanged(query) => {
            state.query = query;
            None
        }
        Message::Submit => {
            state.results = root.map(|root| search(root, &state.query)).unwrap_or_default();
            None
        }
        Message::ResultClicked(i) => state.results.get(i).map(|r| (r.path.clone(), r.line)),
    }
}

/// Case-insensitive substring search over every non-ignored file under `root`, capped at
/// `MAX_RESULTS` matches. Files that aren't valid UTF-8 (most binaries) are silently
/// skipped, same as `std::fs::read_to_string` failing on them.
fn search(root: &Path, query: &str) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();

    let mut files = Vec::new();
    walk(root, &mut files);

    let mut results = Vec::new();
    'files: for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&query_lower) {
                results.push(SearchResult {
                    path: path.clone(),
                    line: i + 1,
                    preview: line.trim().chars().take(200).collect(),
                });
                if results.len() >= MAX_RESULTS {
                    break 'files;
                }
            }
        }
    }
    results
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

pub fn view<'a>(state: &'a SearchState, root: Option<&Path>) -> Element<'a, Message> {
    if root.is_none() {
        return column![
            text_input("Find in project", &state.query),
            text("Open a folder (File > Open Folder) to search its files."),
        ]
        .spacing(6)
        .padding(6)
        .into();
    }

    let input = text_input("Find in project", &state.query)
        .on_input(Message::QueryChanged)
        .on_submit(Message::Submit);

    let summary = if state.query.trim().is_empty() {
        String::new()
    } else if state.results.len() >= MAX_RESULTS {
        format!("{MAX_RESULTS}+ results (showing first {MAX_RESULTS})")
    } else {
        format!("{} result(s)", state.results.len())
    };

    let mut results_col = column![].spacing(2);
    for (i, result) in state.results.iter().enumerate() {
        let relative = root
            .and_then(|root| result.path.strip_prefix(root).ok())
            .unwrap_or(&result.path);
        let label = format!("{}:{}", relative.display(), result.line);
        results_col = results_col.push(
            button(column![text(label).size(13), text(result.preview.clone()).size(12)].spacing(1))
                .width(Length::Fill)
                .padding(4)
                .style(crate::flat_button_style)
                .on_press(Message::ResultClicked(i)),
        );
    }

    column![
        input,
        text(summary).size(12),
        scrollable(results_col).height(Length::Fill),
    ]
    .spacing(6)
    .padding(6)
    .into()
}
