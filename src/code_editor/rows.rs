//! Screen rows: which part of which buffer line each row of the editor shows.
//!
//! The text engine lays every buffer line out as one long row and never wraps. Everything
//! that needs a position on screen -- drawing, the mouse, scrolling, cursor movement -- asks
//! the [`RowMap`] instead of multiplying a line number by the line height. A line folded away
//! has no rows, a line too wide for the view (with word wrap on) has several, and any other
//! line has exactly one, which is all there is when word wrap is off.
//!
//! The functions here work on glyph extents only (see [`Extent`]), so they can be tested with
//! hand-written numbers instead of whatever fonts a machine has.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// A laid-out glyph cluster of an unwrapped line.
pub trait Extent {
    /// Byte range of the cluster in the line's text.
    fn bytes(&self) -> (usize, usize);
    /// Left edge and width on the unwrapped line.
    fn span(&self) -> (f32, f32);
    /// Part of right-to-left text.
    fn rtl(&self) -> bool { false }
}

impl Extent for cosmic_text::LayoutGlyph {
    fn bytes(&self) -> (usize, usize) { (self.start, self.end) }
    fn span(&self) -> (f32, f32) { (self.x, self.w) }
    fn rtl(&self) -> bool { self.level.is_rtl() }
}

/// One screen row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub line: usize,
    /// Bytes of the line shown on this row. Rows of a line cover its text once, in order; the
    /// last one ends at the line's length.
    pub start: usize,
    pub end: usize,
    /// The row's glyphs, as indices into the line's laid-out glyphs.
    pub glyphs: Range<usize>,
    /// x on the unwrapped line where this row begins.
    pub origin: f32,
    /// x on screen where this row begins: 0 for a line's first row, the line's indentation
    /// for the rows it wraps onto.
    pub indent: f32,
}

impl Row {
    /// Whether this is the first row of its line, the one that carries the line number.
    pub fn is_first(&self) -> bool { self.start == 0 }

    /// This row's glyphs out of its line's.
    pub fn slice<'a, G>(&self, glyphs: &'a [G]) -> &'a [G] {
        glyphs.get(self.glyphs.clone()).unwrap_or(&[])
    }

    /// Screen x (relative to the text origin) of byte `index`, clamped to this row.
    pub fn x_of<G: Extent>(&self, glyphs: &[G], index: usize) -> f32 {
        x_at(self.slice(glyphs), index, self.origin) - self.origin + self.indent
    }

    /// The text position at screen x on this row, and whether it is the end of a glyph (so
    /// at a wrap point it belongs to this row rather than to the start of the next).
    pub fn hit<G: Extent>(&self, glyphs: &[G], text: &str, x: f32) -> (usize, bool) {
        hit(self.slice(glyphs), text, x - self.indent + self.origin).unwrap_or((self.start, false))
    }

    /// Like [`Row::hit`], but only when `x` is over a glyph rather than beside the text.
    pub fn hit_glyph<G: Extent>(&self, glyphs: &[G], text: &str, x: f32) -> Option<usize> {
        let x = x - self.indent + self.origin;
        let over = self.slice(glyphs).iter().any(|glyph| {
            let (left, width) = glyph.span();
            x >= left && x <= left + width
        });
        over.then(|| hit(self.slice(glyphs), text, x)).flatten().map(|(index, _)| index)
    }
}

/// x of byte `index` among `glyphs` on the unwrapped line: the left edge of the glyph that
/// starts there, the right edge of the last glyph when past them all, `empty` when there are
/// none.
pub fn x_at<G: Extent>(glyphs: &[G], index: usize, empty: f32) -> f32 {
    for glyph in glyphs {
        let ((start, end), (x, w)) = (glyph.bytes(), glyph.span());
        if index < end {
            return if index <= start { x } else { x + w };
        }
    }
    glyphs.last().map_or(empty, |glyph| glyph.span().0 + glyph.span().1)
}

/// The text position at `x` (on the unwrapped line) among one row's `glyphs`, and whether it
/// is a glyph's end. `None` for a row without glyphs. Left of the row is its first position,
/// right of it its last; within a glyph the nearer side wins, per grapheme when a glyph
/// covers several.
pub fn hit<G: Extent>(glyphs: &[G], text: &str, x: f32) -> Option<(usize, bool)> {
    let first = glyphs.first()?;
    if !first.rtl() && x < first.span().0 {
        return Some((first.bytes().0, false));
    }
    for glyph in glyphs {
        let ((start, end), (left, width)) = (glyph.bytes(), glyph.span());
        if x < left || x > left + width {
            continue;
        }
        let cluster = text.get(start..end).unwrap_or("");
        let count = cluster.graphemes(true).count().max(1);
        let each = width / count as f32;
        for (i, (offset, grapheme)) in cluster.grapheme_indices(true).enumerate() {
            let grapheme_left = left + each * i as f32;
            if x >= grapheme_left && x <= grapheme_left + each {
                let right_half = x >= grapheme_left + each / 2.0;
                return Some(if right_half != glyph.rtl() {
                    (start + offset + grapheme.len(), true)
                } else {
                    (start + offset, false)
                });
            }
        }
        let right_half = x >= left + width / 2.0;
        return Some(if right_half != glyph.rtl() { (end, true) } else { (start, false) });
    }
    Some((glyphs.last()?.bytes().1, true))
}

/// Every screen row, top to bottom.
#[derive(Debug, Default)]
pub struct RowMap {
    rows: Vec<Row>,
    /// Index of each line's first row, `HIDDEN` for a line folded away.
    first: Vec<usize>,
}

const HIDDEN: usize = usize::MAX;

impl RowMap {
    pub fn clear(&mut self) {
        self.rows.clear();
        self.first.clear();
    }

    /// Adds the next line as hidden: it takes no rows.
    pub fn push_hidden(&mut self) {
        self.first.push(HIDDEN);
    }

    /// Adds the next line. `width` is the room for text when word wrap is on; a line wider
    /// than that is broken into rows, after a space where there is one and inside a word only
    /// when the word alone is too wide. Spaces never start a row: they hang past the edge.
    /// Rows after the first begin at the line's indentation, capped at half the width.
    pub fn push_line<G: Extent>(&mut self, glyphs: &[G], text: &str, width: Option<f32>) {
        let line = self.first.len();
        self.first.push(self.rows.len());
        let whole = Row { line, start: 0, end: text.len(), glyphs: 0..glyphs.len(), origin: 0.0, indent: 0.0 };
        let Some(width) = width else { return self.rows.push(whole) };
        let right = |glyph: &G| glyph.span().0 + glyph.span().1;
        // Right-to-left text is not wrapped: its glyphs don't run left to right in byte order.
        if glyphs.last().is_none_or(|last| right(last) <= width) || glyphs.iter().any(Extent::rtl) {
            return self.rows.push(whole);
        }
        let is_space = |glyph: &G| {
            let (start, end) = glyph.bytes();
            text.get(start..end).is_some_and(|cluster| cluster.chars().all(char::is_whitespace))
        };
        let indent = glyphs.iter().find(|glyph| !is_space(glyph)).map_or(0.0, |glyph| glyph.span().0).min(width / 2.0).max(0.0);
        let mut from = 0;
        while from < glyphs.len() {
            let first = from == 0;
            let origin = if first { 0.0 } else { glyphs[from].span().0 };
            let room = if first { width } else { width - indent };
            // The row takes glyphs until one that isn't a space no longer fits. It always
            // takes its first piece of text, so a row is never only the line's indentation
            // and a view narrower than a glyph still makes progress.
            let mut to = from;
            let mut after_space = None;
            let mut seen_text = false;
            while to < glyphs.len() {
                let space = is_space(&glyphs[to]);
                if !space && seen_text && right(&glyphs[to]) - origin > room {
                    break;
                }
                if space && seen_text { after_space = Some(to + 1); }
                seen_text |= !space;
                to += 1;
            }
            // Break after the last space that follows some text; inside the word otherwise.
            if to < glyphs.len() {
                to = after_space.unwrap_or(to);
            }
            self.rows.push(Row {
                line,
                start: if first { 0 } else { glyphs[from].bytes().0 },
                end: glyphs.get(to).map_or(text.len(), |glyph| glyph.bytes().0),
                glyphs: from..to,
                origin,
                indent: if first { 0.0 } else { indent },
            });
            from = to;
        }
    }

    /// Number of rows.
    pub fn len(&self) -> usize { self.rows.len() }

    pub fn get(&self, row: usize) -> Option<&Row> { self.rows.get(row) }

    /// The row indices `line` occupies; empty when it is folded away or unknown.
    pub fn of_line(&self, line: usize) -> Range<usize> {
        match self.first.get(line) {
            Some(&first) if first != HIDDEN => {
                first..first + self.rows[first..].partition_point(|row| row.line == line)
            }
            _ => 0..0,
        }
    }

    /// The first row of `line`, `None` when it is folded away.
    pub fn first_row(&self, line: usize) -> Option<usize> {
        self.first.get(line).copied().filter(|first| *first != HIDDEN)
    }

    /// The line shown on `row`; rows past the end give the last row's line.
    pub fn line_at(&self, row: usize) -> usize {
        self.rows.get(row).or(self.rows.last()).map_or(0, |row| row.line)
    }

    /// The row byte `index` of `line` is on. Where a line wraps, the same index is both the
    /// end of one row and the start of the next: `at_end` picks the former.
    pub fn locate(&self, line: usize, index: usize, at_end: bool) -> Option<usize> {
        let range = self.of_line(line);
        let rows = &self.rows[range.clone()];
        let after = rows.partition_point(|row| row.start <= index);
        let mut row = after.checked_sub(1)?;
        if at_end && row > 0 && rows[row].start == index {
            row -= 1;
        }
        Some(range.start + row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A glyph of a monospace font 10px wide: `(start, end, x, w)`.
    impl Extent for (usize, usize, f32, f32) {
        fn bytes(&self) -> (usize, usize) { (self.0, self.1) }
        fn span(&self) -> (f32, f32) { (self.2, self.3) }
    }

    fn glyphs(text: &str) -> Vec<(usize, usize, f32, f32)> {
        text.char_indices().enumerate().map(|(i, (at, ch))| (at, at + ch.len_utf8(), i as f32 * 10.0, 10.0)).collect()
    }

    fn map(lines: &[&str], width: Option<f32>) -> RowMap {
        let mut map = RowMap::default();
        for line in lines { map.push_line(&glyphs(line), line, width); }
        map
    }

    /// The text each row of the map shows.
    fn shown<'a>(map: &RowMap, lines: &[&'a str]) -> Vec<&'a str> {
        (0..map.len()).map(|row| { let row = map.get(row).unwrap(); &lines[row.line][row.start..row.end] }).collect()
    }

    #[test]
    fn without_wrap_every_line_is_one_row_however_long() {
        let lines = ["short", "", "a very long line that would never fit in any view at all"];
        let map = map(&lines, None);
        assert_eq!(shown(&map, &lines), lines);
        for line in 0..3 {
            assert_eq!(map.of_line(line), line..line + 1);
            assert_eq!(map.first_row(line), Some(line));
            assert_eq!(map.line_at(line), line);
            let row = map.get(line).unwrap();
            assert_eq!((row.origin, row.indent, row.is_first()), (0.0, 0.0, true));
        }
        assert_eq!(map.line_at(50), 2);
        assert_eq!(map.of_line(50), 0..0);
        assert_eq!(map.first_row(50), None);
        assert_eq!(RowMap::default().line_at(0), 0);
    }

    #[test]
    fn folded_lines_take_no_rows() {
        let mut map = RowMap::default();
        map.push_line(&glyphs("fn a() {"), "fn a() {", None);
        map.push_hidden();
        map.push_hidden();
        map.push_line(&glyphs("after"), "after", None);
        assert_eq!(map.len(), 2);
        assert_eq!((map.first_row(0), map.first_row(1), map.first_row(2), map.first_row(3)), (Some(0), None, None, Some(1)));
        assert_eq!(map.of_line(2), 0..0);
        assert_eq!(map.locate(1, 0, false), None);
        assert_eq!(map.line_at(1), 3);
        assert_eq!(map.get(1).unwrap().line, 3);
    }

    #[test]
    fn long_lines_break_after_spaces_and_cover_the_text_once() {
        // 100px of room: ten glyphs a row.
        let lines = ["one two three four five", "fits", "", "abcdefghijklmnopqrstuvwxyz"];
        let map = map(&lines, Some(100.0));
        assert_eq!(shown(&map, &lines), ["one two ", "three four ", "five", "fits", "", "abcdefghij", "klmnopqrst", "uvwxyz"]);
        assert_eq!(map.of_line(0), 0..3);
        assert_eq!(map.of_line(1), 3..4);
        assert_eq!(map.of_line(3), 5..8);
        assert_eq!(map.line_at(6), 3);
        // Each row knows where it starts on the unwrapped line, and which glyphs are its own.
        let second = map.get(1).unwrap();
        assert_eq!((second.origin, second.glyphs.clone(), second.is_first()), (80.0, 8..19, false));
        // "three four " is 11 glyphs: the space that follows a row's last word hangs past the
        // edge rather than starting the next row.
        assert_eq!(second.end - second.start, 11);
        for (line, text) in lines.iter().enumerate() {
            let rows: Vec<_> = map.of_line(line).map(|row| map.get(row).unwrap()).collect();
            assert_eq!(rows.first().unwrap().start, 0);
            assert_eq!(rows.last().unwrap().end, text.len());
            assert!(rows.windows(2).all(|pair| pair[0].end == pair[1].start && pair[0].glyphs.end == pair[1].glyphs.start));
        }
    }

    #[test]
    fn wrapped_rows_start_at_the_lines_indentation() {
        let lines = ["    let total = first + second;", "                        deep nesting here"];
        let map = map(&lines, Some(200.0));
        // "first" would end at 210px, so it moves down, where rows have 160px: the 200 less
        // the 40 of indentation.
        assert_eq!(shown(&map, &lines)[..2], ["    let total = ", "first + second;"]);
        assert_eq!((map.get(0).unwrap().indent, map.get(1).unwrap().indent), (0.0, 40.0));
        // 240px of indentation is capped at half the width, and the first row is never the
        // indentation alone: with no space after text to break at, it breaks inside the word.
        let deep: Vec<_> = map.of_line(1).map(|row| map.get(row).unwrap()).collect();
        assert_eq!(deep[0].start..deep[0].end, 0..25);
        assert!(deep[1..].iter().all(|row| row.indent == 100.0));
        assert_eq!(shown(&map, &lines)[2..], ["                        d", "eep ", "nesting ", "here"]);
    }

    #[test]
    fn a_view_narrower_than_a_glyph_still_shows_one_per_row() {
        let lines = ["abc"];
        for width in [5.0, 0.0, -20.0] {
            assert_eq!(shown(&map(&lines, Some(width)), &lines), ["a", "b", "c"]);
        }
    }

    #[test]
    fn positions_map_to_rows_and_back() {
        let lines = ["one two three four five"];
        let map = map(&lines, Some(100.0));
        let glyphs = glyphs(lines[0]);
        // Byte 8 is where the line first wraps: the end of row 0 and the start of row 1.
        assert_eq!(map.locate(0, 8, false), Some(1));
        assert_eq!(map.locate(0, 8, true), Some(0));
        assert_eq!(map.locate(0, 0, true), Some(0));
        assert_eq!(map.locate(0, 7, false), Some(0));
        assert_eq!(map.locate(0, 23, false), Some(2));
        assert_eq!(map.locate(0, 999, false), Some(2));
        let (first, second, last) = (map.get(0).unwrap(), map.get(1).unwrap(), map.get(2).unwrap());
        assert_eq!(first.x_of(&glyphs, 8), 80.0);
        assert_eq!(second.x_of(&glyphs, 8), 0.0);
        assert_eq!(second.x_of(&glyphs, 10), 20.0);
        assert_eq!(last.x_of(&glyphs, 23), 40.0);
        // Every position of every row comes back from a click at its x.
        for row in [first, second, last] {
            for index in row.start..=row.end {
                let at_end = index == row.end;
                assert_eq!(row.hit(&glyphs, lines[0], row.x_of(&glyphs, index)), (index, at_end || index > row.start), "index {index}");
            }
        }
        // Left and right of a row clamp to its ends; the nearer side of a glyph wins.
        assert_eq!(second.hit(&glyphs, lines[0], -30.0), (8, false));
        assert_eq!(second.hit(&glyphs, lines[0], 5000.0), (19, true));
        assert_eq!(second.hit(&glyphs, lines[0], 12.0), (9, false));
        assert_eq!(second.hit(&glyphs, lines[0], 18.0), (10, true));
        // Only a click on a glyph counts as being over the text.
        assert_eq!(second.hit_glyph(&glyphs, lines[0], 12.0), Some(9));
        assert_eq!(second.hit_glyph(&glyphs, lines[0], 5000.0), None);
        assert_eq!(last.hit_glyph(&glyphs, lines[0], 45.0), None);
    }

    #[test]
    fn indented_rows_offset_positions_by_the_indent() {
        let lines = ["    let total = first + second;"];
        let map = map(&lines, Some(200.0));
        let glyphs = glyphs(lines[0]);
        let second = map.get(1).unwrap();
        assert_eq!(second.start, 16);
        assert_eq!(second.x_of(&glyphs, 16), 40.0);
        assert_eq!(second.x_of(&glyphs, 18), 60.0);
        assert_eq!(second.hit(&glyphs, lines[0], 62.0), (18, false));
        assert_eq!(second.hit(&glyphs, lines[0], 10.0), (16, false));
    }

    #[test]
    fn multi_byte_text_and_empty_rows_have_sane_positions() {
        let text = "héllo wörld";
        let glyphs = glyphs(text);
        let mut map = RowMap::default();
        map.push_line(&glyphs, text, Some(60.0));
        map.push_line(&glyphs[..0], "", Some(60.0));
        assert_eq!(shown(&map, &[text, ""]), ["héllo ", "wörld", ""]);
        let second = map.get(1).unwrap();
        assert_eq!(second.x_of(&glyphs, text.len()), 50.0);
        let empty = map.get(2).unwrap();
        assert_eq!(empty.x_of(&glyphs[..0], 0), 0.0);
        assert_eq!(empty.hit(&glyphs[..0], "", 40.0), (0, false));
        assert_eq!(empty.hit_glyph(&glyphs[..0], "", 0.0), None);
        // A glyph covering two graphemes splits its width between them.
        let ligature = [(0usize, 2usize, 0.0f32, 20.0f32)];
        assert_eq!(hit(&ligature, "fi", 4.0), Some((0, false)));
        assert_eq!(hit(&ligature, "fi", 8.0), Some((1, true)));
        assert_eq!(hit(&ligature, "fi", 12.0), Some((1, false)));
        assert_eq!(hit(&ligature, "fi", 19.0), Some((2, true)));
    }
}
