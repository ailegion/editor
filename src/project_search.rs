//! "Find in Project": recursive text search across every file under the open folder.
//!
//! Searches run off the UI thread after a short typing delay, or immediately on Enter.

use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, row, rich_text, span, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};

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

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: PathBuf,
    pub line: usize,
    pub preview: String,
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<SearchResult>,
    generation: u64,
    searching: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    Submit,
    Run(u64),
    Finished(u64, PathBuf, Result<Vec<SearchResult>, String>),
    ResultClicked(usize),
}

/// Returns background search work and an optional clicked result to open.
/// No scan is started without an open project; outdated completions are ignored.
pub fn update(state: &mut SearchState, message: Message, root: Option<&Path>) -> (Task<Message>, Option<(PathBuf, usize)>) {
    match message {
        Message::QueryChanged(query) => {
            state.query = query;
            state.results.clear();
            state.error = None;
            state.generation += 1;
            state.searching = root.is_some() && !state.query.trim().is_empty();
            if state.searching {
                let generation = state.generation;
                return (Task::perform(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    generation
                }, Message::Run), None);
            }
        }
        Message::Submit => {
            state.generation += 1;
            return update(state, Message::Run(state.generation), root);
        }
        Message::Run(generation) if generation == state.generation => {
            let Some(root) = root else { state.searching = false; return (Task::none(), None); };
            if state.query.trim().is_empty() { state.searching = false; state.results.clear(); return (Task::none(), None); }
            let root = root.to_path_buf();
            let target = root.clone();
            let query = state.query.clone();
            state.searching = true;
            state.error = None;
            return (Task::perform(async move {
                tokio::task::spawn_blocking(move || {
                    std::fs::read_dir(&target).map_err(|err| format!("Cannot search {}: {err}", target.display()))?;
                    Ok(search(&target, &query))
                }).await.map_err(|err| err.to_string())?
            }, move |result| Message::Finished(generation, root.clone(), result)), None);
        }
        Message::Finished(generation, searched_root, result) if generation == state.generation && root == Some(searched_root.as_path()) => {
            state.searching = false;
            match result {
                Ok(results) => { state.results = results; state.error = None; }
                Err(err) => { state.results.clear(); state.error = Some(err); }
            }
        }
        Message::ResultClicked(i) => return (Task::none(), state.results.get(i).map(|r| (r.path.clone(), r.line))),
        _ => {},
    }
    (Task::none(), None)
}

/// Case-insensitive substring search over every non-ignored file under `root`, capped at
/// `MAX_RESULTS` matches. Files that aren't valid UTF-8 (most binaries) are silently
/// skipped, same as `std::fs::read_to_string` failing on them.
pub(crate) fn search(root: &Path, query: &str) -> Vec<SearchResult> {
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
        let Ok(kind) = entry.file_type() else { continue; };
        if kind.is_dir() {
            if IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
            walk(&path, out);
        } else if kind.is_file() {
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

    let summary = if let Some(error) = &state.error {
        error.clone()
    } else if state.searching {
        "Searching…".to_owned()
    } else if state.query.trim().is_empty() {
        String::new()
    } else if state.results.len() >= MAX_RESULTS {
        format!("{MAX_RESULTS}+ results (showing first {MAX_RESULTS})")
    } else {
        format!("{} matches", state.results.len())
    };

    let mut results_col = column![].spacing(10);
    let mut groups = std::collections::BTreeMap::<&Path, Vec<(usize, &SearchResult)>>::new();
    for (index, result) in state.results.iter().enumerate() {
        groups.entry(&result.path).or_default().push((index, result));
    }
    let matcher = regex::RegexBuilder::new(&regex::escape(&state.query)).case_insensitive(true).build().ok();
    for (path, results) in groups {
        let relative = root.and_then(|root| path.strip_prefix(root).ok()).unwrap_or(path);
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let parent = relative.parent().filter(|parent| !parent.as_os_str().is_empty());
        let mut title = column![row![
            text(name).size(13), Space::new().width(Length::Fill),
            text(results.len().to_string()).size(11).style(iced::widget::text::secondary),
        ]].spacing(2);
        if let Some(parent) = parent {
            title = title.push(text(parent.display().to_string()).size(11).style(iced::widget::text::secondary));
        }
        let mut group = column![container(title).padding([6, 8])].spacing(1);
        for (index, result) in results {
            let mut pieces = Vec::new();
            let mut offset = 0;
            if let Some(matcher) = &matcher {
                for matched in matcher.find_iter(&result.preview) {
                    pieces.push(span(result.preview[offset..matched.start()].to_owned()));
                    pieces.push(span(result.preview[matched.range()].to_owned())
                        .background(iced::Color::from_rgba(0.8, 0.65, 0.15, 0.25)));
                    offset = matched.end();
                }
            }
            pieces.push(span(result.preview[offset..].to_owned()));
            let preview: iced::widget::text::Rich<'_, (), Message> = rich_text(pieces);
            group = group.push(button(row![
                text(result.line.to_string()).size(11).width(32).style(iced::widget::text::secondary),
                preview.size(12).font(iced::Font::MONOSPACE),
            ].spacing(6)).width(Length::Fill).padding([5, 8])
                .style(crate::flat_button_style).on_press(Message::ResultClicked(index)));
        }
        results_col = results_col.push(container(group).width(Length::Fill).style(iced::widget::container::rounded_box));
    }
    if state.query.is_empty() {
        results_col = results_col.push(text("Search across files in this project.").size(12).style(iced::widget::text::secondary));
    }

    column![
        text("SEARCH").size(12),
        input,
        text(summary).size(12).style(iced::widget::text::secondary),
        scrollable(results_col).height(Length::Fill),
    ]
    .spacing(6)
    .padding(6)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::StreamExt;

    async fn finish(state: &mut SearchState, task: Task<Message>, root: &Path) {
        let mut tasks = vec![task];
        while let Some(task) = tasks.pop() {
            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Output(message) = action {
                        tasks.push(update(state, message, Some(root)).0);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn typing_searches_nested_files_and_discards_old_results() {
        let root = std::env::temp_dir().join(format!("editor-search-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("src/app.ts"), "const Example = 42;\n").unwrap();
        std::fs::write(root.join("node_modules/skip.ts"), "Example").unwrap();
        let mut state = SearchState::default();
        let (task, _) = update(&mut state, Message::QueryChanged("example".into()), Some(&root));
        assert!(state.searching);
        finish(&mut state, task, &root).await;
        assert_eq!(state.results.len(), 1);
        assert_eq!(state.results[0].path, root.join("src/app.ts"));
        assert_eq!(state.results[0].line, 1);
        let old = state.generation;
        let _ = update(&mut state, Message::QueryChanged("".into()), Some(&root));
        let _ = update(&mut state, Message::Finished(old, root.clone(), Ok(search(&root, "example"))), Some(&root));
        assert!(state.results.is_empty());
        assert!(!state.searching);
        std::fs::remove_dir_all(root).unwrap();
    }
}
