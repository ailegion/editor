//! VS Code theme files are JSONC: JSON plus `//` / `/* */` comments and trailing commas.
//! `strip` reduces that to plain JSON for `serde_json`.

pub fn strip(input: &str) -> String {
    remove_trailing_commas(&remove_comments(input))
}

fn remove_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.next_if(|&next| next != '\n').is_some() {}
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if prev == '*' && next == '/' {
                        break;
                    }
                    prev = next;
                }
                // Keep the tokens on either side of the comment apart.
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Drops commas whose next non-whitespace character closes an array/object. Expects
/// comment-free input.
fn remove_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        let closes = || chars[i + 1..].iter().find(|next| !next.is_whitespace()).is_some_and(|next| matches!(next, ']' | '}'));
        if c == ',' && closes() {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip;
    use serde_json::{json, Value};

    fn parse(input: &str) -> Value {
        serde_json::from_str(&strip(input)).unwrap()
    }

    #[test]
    fn removes_comments_and_trailing_commas() {
        let input = r#"{
            // line comment
            "a": 1, /* block
            comment */ "b": [1, 2,],
            "c": { "d": true, },
        }"#;
        assert_eq!(parse(input), json!({"a": 1, "b": [1, 2], "c": {"d": true}}));
    }

    #[test]
    fn leaves_string_contents_alone() {
        let input = r#"{"url": "https://example.com/*x*/", "q": "say \"hi\", // not a comment", "t": ",]"}"#;
        assert_eq!(
            parse(input),
            json!({"url": "https://example.com/*x*/", "q": "say \"hi\", // not a comment", "t": ",]"})
        );
    }

    #[test]
    fn comment_after_value_on_same_line() {
        assert_eq!(parse("[\"a\", // note\n \"b\" // last\n]"), json!(["a", "b"]));
    }
}
