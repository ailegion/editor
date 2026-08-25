//! Bracket-pair matching: given a cursor position, find the bracket it's sitting next to
//! (if any) and its partner, accounting for nesting. Used both for auto-close-on-type
//! (`input.rs`) and the highlight-the-matching-bracket indicator (`buffer.rs`'s `sync`,
//! rendered in `render.rs`).

use cosmic_text::{Buffer as CosmicBuffer, Cursor};

const PAIRS: &[(char, char)] = &[('(', ')'), ('[', ']'), ('{', '}')];

pub fn matching_closer(open: char) -> Option<char> {
    PAIRS.iter().find(|(o, _)| *o == open).map(|(_, c)| *c)
}

pub fn matching_opener(close: char) -> Option<char> {
    PAIRS.iter().find(|(_, c)| *c == close).map(|(o, _)| *o)
}

/// The character (and its buffer position) immediately after `(line, index)`, stepping to
/// the next line at a line's end (a line break is a boundary, not a character of its own).
pub fn next_char(buffer: &CosmicBuffer, mut line: usize, index: usize) -> Option<(usize, usize, char)> {
    let mut index = index;
    loop {
        let text = buffer.lines.get(line)?.text();
        if index < text.len() {
            let ch = text[index..].chars().next()?;
            return Some((line, index, ch));
        }
        line += 1;
        index = 0;
        if line >= buffer.lines.len() {
            return None;
        }
    }
}

/// The character (and its buffer position) immediately before `(line, index)`.
pub fn prev_char(buffer: &CosmicBuffer, mut line: usize, mut index: usize) -> Option<(usize, usize, char)> {
    loop {
        if index == 0 {
            if line == 0 {
                return None;
            }
            line -= 1;
            index = buffer.lines.get(line)?.text().len();
            continue;
        }
        let text = buffer.lines.get(line)?.text();
        let prev_index = text[..index].char_indices().next_back()?.0;
        let ch = text[prev_index..].chars().next()?;
        return Some((line, prev_index, ch));
    }
}

/// If the cursor sits immediately before an opener or immediately after a closer, finds the
/// matching bracket (accounting for nesting) and returns both positions as
/// `(line, start_index, end_index)` pairs -- `[cursor-side, partner-side]`.
pub fn find_match(buffer: &CosmicBuffer, cursor: Cursor) -> Option<[(usize, usize, usize); 2]> {
    if let Some((line, index, ch)) = next_char(buffer, cursor.line, cursor.index) {
        if let Some(closer) = matching_closer(ch) {
            if let Some((ml, mi)) = scan_forward(buffer, line, index + ch.len_utf8(), ch, closer) {
                return Some([(line, index, index + ch.len_utf8()), (ml, mi, mi + closer.len_utf8())]);
            }
        }
    }
    if let Some((line, index, ch)) = prev_char(buffer, cursor.line, cursor.index) {
        if let Some(opener) = matching_opener(ch) {
            if let Some((ml, mi)) = scan_backward(buffer, line, index, opener, ch) {
                return Some([(line, index, index + ch.len_utf8()), (ml, mi, mi + opener.len_utf8())]);
            }
        }
    }
    None
}

/// Walks forward from just past an opener, tracking nesting depth, until the matching
/// closer (depth back to zero) is found.
fn scan_forward(buffer: &CosmicBuffer, line: usize, index: usize, open: char, close: char) -> Option<(usize, usize)> {
    let mut depth = 1i32;
    let mut pos = (line, index);
    loop {
        let (l, i, ch) = next_char(buffer, pos.0, pos.1)?;
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some((l, i));
            }
        }
        pos = (l, i + ch.len_utf8());
    }
}

/// Walks backward from a closer's own position, tracking nesting depth, until the matching
/// opener (depth back to zero) is found.
fn scan_backward(buffer: &CosmicBuffer, line: usize, index: usize, open: char, close: char) -> Option<(usize, usize)> {
    let mut depth = 1i32;
    let mut pos = (line, index);
    loop {
        let (l, i, ch) = prev_char(buffer, pos.0, pos.1)?;
        if ch == close {
            depth += 1;
        } else if ch == open {
            depth -= 1;
            if depth == 0 {
                return Some((l, i));
            }
        }
        pos = (l, i);
    }
}
