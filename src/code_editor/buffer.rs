use cosmic_text::{
    Attrs, Buffer as CosmicBuffer, Change, Cursor, Edit, Editor, FontSystem, Metrics, Selection,
    Shaping, Wrap,
};

use super::highlight::Highlighter;

/// Wraps a `cosmic_text::Buffer`, tracking cursor/selection state alongside it.
///
/// Editing goes through a transient `cosmic_text::Editor` (created on demand and dropped
/// immediately after) rather than storing one long-term, since `Editor<'buffer>` borrows the
/// buffer it wraps and a self-referential struct would be needed to hold both.
pub struct Buffer {
    pub inner: CosmicBuffer,
    pub font_system: FontSystem,
    pub cursor: Cursor,
    pub selection: Selection,
    cursor_pixel: Option<(i32, i32)>,
    /// `(line, x_start, x_end)` selection highlight rects, one per selected line.
    selection_pixels: Vec<(usize, f32, f32)>,
    undo_stack: Vec<Change>,
    redo_stack: Vec<Change>,
}

impl Buffer {
    pub fn new(text: &str, metrics: Metrics) -> Self {
        let mut font_system = FontSystem::new();
        let mut inner = CosmicBuffer::new(&mut font_system, metrics);
        // Code editors don't soft-wrap; this also keeps one `LayoutLine` per `BufferLine`,
        // which `render.rs` and `cursor_pixel` both assume.
        inner.set_wrap(&mut font_system, Wrap::None);
        inner.set_text(&mut font_system, text, &Attrs::new(), Shaping::Advanced, None);
        let mut buffer = Self {
            inner,
            font_system,
            cursor: Cursor::default(),
            selection: Selection::None,
            cursor_pixel: None,
            selection_pixels: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        };
        buffer.sync();
        buffer
    }

    pub fn set_size(&mut self, width: f32, height: f32) {
        self.inner.set_size(&mut self.font_system, Some(width), Some(height));
        self.sync();
    }

    /// Applies a new font size/line height (e.g. from a zoom change), re-shaping and
    /// re-syncing cached cursor/selection pixel positions to match.
    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.inner.set_metrics(&mut self.font_system, metrics);
        self.sync();
    }

    pub fn text(&self) -> String {
        self.inner
            .lines
            .iter()
            .map(|line| line.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.inner.lines.len()
    }

    /// 1-indexed (line, column) of the cursor, for the status bar. Column is a character
    /// count (not a byte offset), so it stays correct on lines with multi-byte characters.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let col = self
            .inner
            .lines
            .get(self.cursor.line)
            .map(|line| {
                let text = line.text();
                let index = self.cursor.index.min(text.len());
                text[..index].chars().count()
            })
            .unwrap_or(0);
        (self.cursor.line + 1, col + 1)
    }

    /// Selects the entire document, cursor ending at the very end. `perform` only reads
    /// `self.selection` back into the transient `Editor` at the start of each call, so setting
    /// it directly between the two motions (rather than via an `Action`) sticks -- matching
    /// how `input::handle_key` anchors shift-selections.
    pub fn select_all(&mut self) {
        self.perform(cosmic_text::Action::Motion(cosmic_text::Motion::BufferStart));
        self.selection = Selection::Normal(self.cursor);
        self.perform(cosmic_text::Action::Motion(cosmic_text::Motion::BufferEnd));
    }

    /// Pixel position of the top-left corner of the cursor, relative to the buffer origin.
    pub fn cursor_pixel(&self) -> Option<(i32, i32)> {
        self.cursor_pixel
    }

    /// `(line, x_start, x_end)` selection highlight rects, one per selected line, relative to
    /// the buffer origin.
    pub fn selection_pixels(&self) -> &[(usize, f32, f32)] {
        &self.selection_pixels
    }

    /// Applies a `cosmic_text::Action` (motion, insert, backspace, click, ...) to the buffer.
    pub fn perform(&mut self, action: cosmic_text::Action) {
        let mut editor = Editor::new(&mut self.inner);
        editor.set_cursor(self.cursor);
        editor.set_selection(self.selection);
        editor.start_change();
        editor.action(&mut self.font_system, action);
        let change = editor.finish_change();
        self.cursor = editor.cursor();
        self.selection = editor.selection();
        self.push_undo(change);
        self.sync();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// Number of edits recorded for undo. Grows by exactly one per user-visible edit (see
    /// `push_undo`), so comparing it before/after an operation tells the caller whether that
    /// operation actually changed the document -- used by `main.rs` to decide whether to mark
    /// a tab dirty and re-run syntax highlighting.
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) {
        let Some(mut change) = self.undo_stack.pop() else {
            return;
        };
        change.reverse();
        let mut editor = Editor::new(&mut self.inner);
        editor.apply_change(&change);
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        change.reverse(); // back to forward order, for redo
        self.redo_stack.push(change);
        self.sync();
    }

    pub fn redo(&mut self) {
        let Some(change) = self.redo_stack.pop() else {
            return;
        };
        let mut editor = Editor::new(&mut self.inner);
        editor.apply_change(&change);
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        self.undo_stack.push(change);
        self.sync();
    }

    /// Records a completed edit for undo, clearing the redo stack (its changes no longer
    /// apply cleanly once the document has diverged from where they were recorded).
    fn push_undo(&mut self, change: Option<Change>) {
        if let Some(change) = change {
            if !change.items.is_empty() {
                self.undo_stack.push(change);
                self.redo_stack.clear();
            }
        }
    }

    /// Re-derives syntax-highlight colors for the current text and re-applies them via
    /// `set_rich_text`. Cursor/selection (line, byte index) are unaffected since this only
    /// re-styles the existing text rather than editing it.
    pub fn highlight(&mut self, highlighter: &Highlighter, extension: &str, app_theme: &iced::Theme) {
        let text = self.text();
        let spans = highlighter.highlight(&text, extension, app_theme);
        self.inner.set_rich_text(
            &mut self.font_system,
            spans.iter().map(|(chunk, attrs)| (chunk.as_str(), attrs.clone())),
            &Attrs::new(),
            Shaping::Advanced,
            None,
        );
        self.sync();
    }

    /// Selects `start..end`, moving the cursor to `end` (matching how a mouse drag would
    /// leave things). Used by search/replace to highlight a match.
    pub fn select_range(&mut self, start: Cursor, end: Cursor) {
        self.cursor = end;
        self.selection = Selection::Normal(start);
        self.sync();
    }

    /// Replaces `start..end` with `replacement`, moving the cursor to just after it and
    /// clearing the selection.
    pub fn replace_range(&mut self, start: Cursor, end: Cursor, replacement: &str) {
        let mut editor = Editor::new(&mut self.inner);
        editor.start_change();
        editor.delete_range(start, end);
        let new_cursor = editor.insert_at(start, replacement, None);
        editor.set_cursor(new_cursor);
        let change = editor.finish_change();
        self.cursor = editor.cursor();
        self.selection = Selection::None;
        self.push_undo(change);
        self.sync();
    }

    fn sync(&mut self) {
        self.inner.shape_until_scroll(&mut self.font_system, false);

        let selection_bounds = {
            let mut editor = Editor::new(&mut self.inner);
            editor.set_cursor(self.cursor);
            editor.set_selection(self.selection);
            self.cursor_pixel = editor.cursor_position();
            editor.selection_bounds()
        };

        self.selection_pixels.clear();
        if let Some((start, end)) = selection_bounds {
            // Byte lengths must be read before `Editor::new` takes `&mut self.inner`.
            let line_lens: Vec<usize> = (start.line..=end.line)
                .map(|i| self.inner.lines.get(i).map_or(0, |l| l.text().len()))
                .collect();

            let mut editor = Editor::new(&mut self.inner);
            for (offset, line_i) in (start.line..=end.line).enumerate() {
                let line_len = line_lens[offset];
                let start_index = if line_i == start.line { start.index } else { 0 };
                let end_index = if line_i == end.line { end.index } else { line_len };

                editor.set_cursor(Cursor::new(line_i, start_index));
                let x0 = editor.cursor_position().map_or(0.0, |(x, _)| x as f32);
                editor.set_cursor(Cursor::new(line_i, end_index));
                let x1 = editor.cursor_position().map_or(x0, |(x, _)| x as f32);

                self.selection_pixels.push((line_i, x0, x1.max(x0)));
            }
        }
    }
}
