//! `@path` mentions in the AI composer: file suggestions while one is typed, and the files
//! mentioned in a message, which are attached when it is sent. Paths are project-relative with
//! `/` separators; paths containing spaces are written `@"like this"`.
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

const MAX_SUGGESTIONS: usize = 6;

/// The mention being typed at the end of `text`, without its `@`.
pub fn typing(text: &str) -> Option<&str> {
    let word = text.rsplit(char::is_whitespace).next()?;
    word.strip_prefix('@').map(|query| query.trim_start_matches('"'))
}

/// `text` with the mention being typed replaced by `path`, ready for more typing.
pub fn complete(text: &str, path: &str) -> String {
    let start = text.rfind(char::is_whitespace).map_or(0, |index| index + text[index..].chars().next().map_or(1, char::len_utf8));
    let mention = if path.contains(char::is_whitespace) { format!("@\"{path}\"") } else { format!("@{path}") };
    format!("{}{mention} ", &text[..start])
}

/// Paths mentioned in `text`, in order and without repeats.
pub fn mentioned(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let preceded_by_space = rest[..at].chars().next_back().is_none_or(char::is_whitespace);
        rest = &rest[at + 1..];
        if !preceded_by_space { continue; }
        let path = match rest.strip_prefix('"') {
            Some(quoted) => match quoted.find('"') {
                Some(end) => { rest = &quoted[end + 1..]; &quoted[..end] }
                None => continue,
            },
            None => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let word = &rest[..end];
                rest = &rest[end..];
                // Sentence punctuation after a mention isn't part of the path.
                word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\''])
            }
        };
        if !path.is_empty() && !paths.iter().any(|known| known == path) { paths.push(path.to_string()); }
    }
    paths
}

/// The best matches for `query` among `files`: closest first, shorter paths winning ties.
pub fn suggest(files: &[String], query: &str) -> Vec<String> {
    if query.is_empty() { return Vec::new(); }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, &String)> = files.iter()
        .filter_map(|path| matcher.fuzzy_match(path, query).map(|score| (score, path)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    scored.into_iter().take(MAX_SUGGESTIONS).map(|(_, path)| path.clone()).collect()
}

/// `files` under `root` as mention paths.
pub fn relative_paths(root: &std::path::Path, files: &[std::path::PathBuf]) -> Vec<String> {
    let mut paths: Vec<String> = files.iter()
        .filter_map(|path| path.strip_prefix(root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_completes_the_mention_being_typed() {
        assert_eq!(typing("look at @src/ma"), Some("src/ma"));
        assert_eq!(typing("@"), Some(""));
        assert_eq!(typing("look at @src/main.rs "), None, "a finished mention");
        assert_eq!(typing("mail me@example.com"), None);
        assert_eq!(complete("look at @src/ma", "src/main.rs"), "look at @src/main.rs ");
        assert_eq!(complete("@re", "docs/read me.md"), "@\"docs/read me.md\" ");
        assert_eq!(complete("é @x", "a.rs"), "é @a.rs ");
    }

    #[test]
    fn finds_every_mentioned_path_once() {
        let text = "Compare @src/a.rs with @\"docs/read me.md\", then @src/a.rs. Email me@host.com (see @b.rs)";
        assert_eq!(mentioned(text), ["src/a.rs", "docs/read me.md", "b.rs"]);
        assert_eq!(mentioned("@\"unterminated"), Vec::<String>::new());
        assert!(mentioned("no mentions @ here").is_empty());
    }

    #[test]
    fn suggestions_rank_closest_and_shortest_first() {
        let files: Vec<String> = ["src/main.rs", "src/git/main_view.rs", "README.md", "src/mainframe/deep/nested/main.rs"]
            .map(String::from).to_vec();
        let found = suggest(&files, "main");
        assert_eq!(found[0], "src/main.rs");
        assert!(!found.contains(&"README.md".to_string()));
        assert!(suggest(&files, "").is_empty());
        let many: Vec<String> = (0..20).map(|i| format!("file{i}.rs")).collect();
        assert_eq!(suggest(&many, "file").len(), MAX_SUGGESTIONS);
    }

    #[test]
    fn paths_are_relative_with_forward_slashes() {
        let root = std::path::Path::new("project");
        let files = [root.join("src").join("b.rs"), root.join("a.rs"), std::path::PathBuf::from("elsewhere/c.rs")];
        assert_eq!(relative_paths(root, &files), ["a.rs", "src/b.rs"]);
    }
}
