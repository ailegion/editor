//! Bracket-based folding, independent of the document's stored text.
use std::collections::BTreeMap;

pub fn ranges(text: &str) -> BTreeMap<usize, usize> {
    let mut ranges = BTreeMap::new();
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut block_comment = false;
    for (line, source) in text.lines().enumerate() {
        let mut chars = source.chars().peekable();
        while let Some(ch) = chars.next() {
            if block_comment {
                if ch == '*' && chars.peek() == Some(&'/') { chars.next(); block_comment = false; }
                continue;
            }
            if let Some(delimiter) = quote {
                if escaped { escaped = false; }
                else if ch == '\\' { escaped = true; }
                else if ch == delimiter { quote = None; }
                continue;
            }
            if ch == '/' && chars.peek() == Some(&'/') { break; }
            if ch == '/' && chars.peek() == Some(&'*') { chars.next(); block_comment = true; continue; }
            if matches!(ch, '"' | '\'' | '`') { quote = Some(ch); continue; }
            if matches!(ch, '{' | '[' | '(') { stack.push((ch, line)); }
            else if matches!(ch, '}' | ']' | ')') {
                let expected = match ch { '}' => '{', ']' => '[', _ => '(' };
                if let Some(&(open, start)) = stack.last() {
                    if open == expected {
                        stack.pop();
                        if line > start {
                            ranges.entry(start).and_modify(|end: &mut usize| *end = (*end).max(line)).or_insert(line);
                        }
                    }
                }
            }
        }
        // Single/double quoted strings normally end at the line boundary. Backticks
        // can span lines; this intentionally treats template contents as opaque.
        if quote != Some('`') && !escaped { quote = None; }
        escaped = false;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_nested_blocks_ignoring_strings_and_comments() {
        let text = "fn f() {\n  let x = [\n    \"}\", // ]\n    1, /* } */\n  ];\n}\n";
        assert_eq!(ranges(text), BTreeMap::from([(0, 5), (1, 4)]));
        assert!(ranges("let x = {};\n// {\n\"[\"\n").is_empty());
    }
}
