//! Cmd/Ctrl+K: describe a change to the selected code, review the AI's rewrite as a diff, then
//! accept it into the buffer (one undo step) or reject it.
use cosmic_text::Cursor;
use std::path::PathBuf;

/// Lines of surrounding code sent for context on each side of the selection.
const CONTEXT_LINES: usize = 40;

pub struct InlineEdit {
    /// The tab being edited, by path (`None` for an untitled buffer).
    pub path: Option<PathBuf>,
    pub start: Cursor,
    pub end: Cursor,
    /// The text between `start` and `end` when the edit began; accepting checks it is unchanged.
    pub original: String,
    pub instruction: String,
    pub stage: Stage,
}

pub enum Stage {
    /// Waiting for the instruction.
    Prompt,
    /// The request with this id is running; answers for older ids are ignored.
    Working(u64),
    /// The proposed replacement, shown as a diff.
    Review(String),
}

pub const INPUT_ID: &str = "inline-edit";

/// Starts the diff preview's title while a proposed edit is under review.
pub const PREVIEW_PREFIX: &str = "Proposed AI edit · ";

/// The request for a rewrite of `original`, which sits between `before` and `after` in `file`.
pub fn prompt(file: &str, language: &str, before: &str, original: &str, after: &str, instruction: &str) -> String {
    let task = if original.is_empty() {
        "Write code to insert at the <cursor/> position, following the instruction."
    } else {
        "Rewrite the code inside <selection> following the instruction."
    };
    let tail = |text: &str| text.lines().rev().take(CONTEXT_LINES).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    let head = |text: &str| text.lines().take(CONTEXT_LINES).collect::<Vec<_>>().join("\n");
    let target = if original.is_empty() { "<cursor/>".to_string() } else { format!("<selection>\n{original}\n</selection>") };
    format!(
        "You are editing {file} ({language}). {task}\n\
         Reply with only the new code in a single fenced code block: no explanation. Match the \
         surrounding indentation and style, and do not repeat the context around it.\n\n\
         Instruction: {instruction}\n\n\
         {before}\n{target}\n{after}\n",
        before = tail(before),
        after = head(after),
    )
}

/// The code to put in place of `original`, from the AI's reply.
pub fn replacement(reply: &str, original: &str) -> String {
    let mut code = crate::ai_oneshot::unfence(reply);
    // A selection of whole lines usually ends in a newline that the reply's block drops.
    if original.ends_with('\n') && !code.ends_with('\n') { code.push('\n'); }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_carry_bounded_context_and_the_instruction() {
        let before: String = (1..=60).map(|n| format!("line {n}\n")).collect();
        let text = prompt("src/lib.rs", "Rust", &before, "fn a() {}", "after 1\nafter 2", "add docs");
        assert!(text.contains("You are editing src/lib.rs (Rust). Rewrite the code inside <selection>"));
        assert!(text.contains("Instruction: add docs"));
        assert!(text.contains("<selection>\nfn a() {}\n</selection>\nafter 1\nafter 2\n"));
        assert!(text.contains("line 21\n") && !text.contains("line 20\n"), "only the nearest {CONTEXT_LINES} lines before");
        let insert = prompt("a.py", "Python", "x = 1", "", "", "add a main guard");
        assert!(insert.contains("insert at the <cursor/> position") && insert.contains("x = 1\n<cursor/>\n"));
    }

    #[test]
    fn replies_become_the_replacement_code() {
        assert_eq!(replacement("```rust\n/// Docs.\nfn a() {}\n```", "fn a() {}"), "/// Docs.\nfn a() {}");
        assert_eq!(replacement("```\nb\n```", "a\n"), "b\n");
        assert_eq!(replacement("just code", ""), "just code");
    }
}
