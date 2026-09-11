//! Multi-line prompt editor: a `String` buffer with a char cursor, word/line
//! motions, undo/redo, wrapping for display, and inline "atoms" (paste
//! summaries and attachments) that render as chips and expand on submit.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq)]
pub enum AtomKind {
    /// Large paste collapsed to `[Pasted ~N lines]`; `full` is inserted on submit.
    Paste { full: String },
    /// An image or file attachment to send as a `file` part.
    Attachment {
        path: String,
        mime: String,
        data_url: Option<String>,
    },
    /// `@path` mention (kept as text; recorded so parts can be attached).
    File { path: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Atom {
    pub id: usize,
    pub kind: AtomKind,
    pub label: String,
}

/// Private-use char that marks an atom position in the buffer.
pub const ATOM_MARK: char = '\u{E000}';

#[derive(Debug, Clone, Default)]
pub struct Textarea {
    text: Vec<char>,
    cursor: usize,
    atoms: Vec<Atom>,
    next_atom: usize,
    undo: Vec<(Vec<char>, usize)>,
    redo: Vec<(Vec<char>, usize)>,
    pub placeholder: String,
}

impl Textarea {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn len(&self) -> usize {
        self.text.len()
    }
    pub fn atoms(&self) -> &[Atom] {
        &self.atoms
    }

    fn snapshot(&mut self) {
        self.undo.push((self.text.clone(), self.cursor));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn set_text(&mut self, s: &str) {
        self.snapshot();
        self.text = s.chars().collect();
        self.cursor = self.text.len();
        self.atoms.clear();
    }

    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.snapshot();
        }
        self.text.clear();
        self.cursor = 0;
        self.atoms.clear();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.snapshot();
        for c in s.chars() {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if c == ' ' || c == '\n' {
            self.snapshot();
        }
        self.text.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn insert_atom(&mut self, kind: AtomKind, label: String) {
        self.snapshot();
        let id = self.next_atom;
        self.next_atom += 1;
        self.atoms.push(Atom { id, kind, label });
        // marker followed by the atom index as a private-use offset
        self.text.insert(self.cursor, ATOM_MARK);
        self.text.insert(
            self.cursor + 1,
            char::from_u32(0xE100 + id as u32).unwrap_or(ATOM_MARK),
        );
        self.cursor += 2;
        self.text.insert(self.cursor, ' ');
        self.cursor += 1;
    }

    fn atom_at(&self, i: usize) -> Option<&Atom> {
        if self.text.get(i) == Some(&ATOM_MARK) {
            let id = (*self.text.get(i + 1)? as u32).checked_sub(0xE100)? as usize;
            return self.atoms.iter().find(|a| a.id == id);
        }
        None
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.snapshot();
        // deleting an atom removes both marker chars
        if self.cursor >= 2 && self.text.get(self.cursor - 2) == Some(&ATOM_MARK) {
            self.text.drain(self.cursor - 2..self.cursor);
            self.cursor -= 2;
            return;
        }
        self.cursor -= 1;
        self.text.remove(self.cursor);
        self.prune_atoms();
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        self.snapshot();
        if self.text.get(self.cursor) == Some(&ATOM_MARK) {
            self.text
                .drain(self.cursor..(self.cursor + 2).min(self.text.len()));
        } else {
            self.text.remove(self.cursor);
        }
        self.prune_atoms();
    }

    fn prune_atoms(&mut self) {
        let present: Vec<usize> = (0..self.text.len())
            .filter_map(|i| self.atom_at(i).map(|a| a.id))
            .collect();
        self.atoms.retain(|a| present.contains(&a.id));
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            if self.cursor > 0 && self.text.get(self.cursor - 1) == Some(&ATOM_MARK) {
                self.cursor -= 1;
            }
        }
    }
    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            if self.text.get(self.cursor) == Some(&ATOM_MARK) {
                self.cursor += 1;
            }
            self.cursor += 1;
        }
    }
    fn line_bounds(&self, pos: usize) -> (usize, usize) {
        let start = self.text[..pos]
            .iter()
            .rposition(|&c| c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.text[pos..]
            .iter()
            .position(|&c| c == '\n')
            .map(|i| pos + i)
            .unwrap_or(self.text.len());
        (start, end)
    }
    pub fn line_home(&mut self) {
        self.cursor = self.line_bounds(self.cursor).0;
    }
    pub fn line_end(&mut self) {
        self.cursor = self.line_bounds(self.cursor).1;
    }
    pub fn buffer_home(&mut self) {
        self.cursor = 0;
    }
    pub fn buffer_end(&mut self) {
        self.cursor = self.text.len();
    }
    /// Move up a logical line; returns false when already on the first line.
    pub fn move_up(&mut self) -> bool {
        let (start, _) = self.line_bounds(self.cursor);
        if start == 0 {
            return false;
        }
        let col = self.cursor - start;
        let (pstart, pend) = self.line_bounds(start - 1);
        self.cursor = (pstart + col).min(pend);
        true
    }
    pub fn move_down(&mut self) -> bool {
        let (start, end) = self.line_bounds(self.cursor);
        if end >= self.text.len() {
            return false;
        }
        let col = self.cursor - start;
        let (nstart, nend) = self.line_bounds(end + 1);
        self.cursor = (nstart + col).min(nend);
        true
    }
    pub fn on_first_line(&self) -> bool {
        self.line_bounds(self.cursor).0 == 0
    }
    pub fn on_last_line(&self) -> bool {
        self.line_bounds(self.cursor).1 >= self.text.len()
    }
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    pub fn word_backward(&mut self) {
        let mut i = self.cursor;
        while i > 0 && !Self::is_word(self.text[i - 1]) {
            i -= 1;
        }
        while i > 0 && Self::is_word(self.text[i - 1]) {
            i -= 1;
        }
        self.cursor = i;
    }
    pub fn word_forward(&mut self) {
        let mut i = self.cursor;
        while i < self.text.len() && !Self::is_word(self.text[i]) {
            i += 1;
        }
        while i < self.text.len() && Self::is_word(self.text[i]) {
            i += 1;
        }
        self.cursor = i;
    }
    pub fn delete_word_backward(&mut self) {
        let end = self.cursor;
        self.word_backward();
        if self.cursor < end {
            self.snapshot();
            self.text.drain(self.cursor..end);
            self.prune_atoms();
        }
    }
    pub fn delete_word_forward(&mut self) {
        let start = self.cursor;
        self.word_forward();
        let end = self.cursor;
        self.cursor = start;
        if end > start {
            self.snapshot();
            self.text.drain(start..end);
            self.prune_atoms();
        }
    }
    pub fn delete_to_line_end(&mut self) {
        let (_, end) = self.line_bounds(self.cursor);
        if end > self.cursor {
            self.snapshot();
            self.text.drain(self.cursor..end);
            self.prune_atoms();
        }
    }
    pub fn delete_to_line_start(&mut self) {
        let (start, _) = self.line_bounds(self.cursor);
        if self.cursor > start {
            self.snapshot();
            self.text.drain(start..self.cursor);
            self.cursor = start;
            self.prune_atoms();
        }
    }
    pub fn undo_edit(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.redo.push((self.text.clone(), self.cursor));
            self.text = t;
            self.cursor = c.min(self.text.len());
            self.prune_atoms();
        }
    }
    pub fn redo_edit(&mut self) {
        if let Some((t, c)) = self.redo.pop() {
            self.undo.push((self.text.clone(), self.cursor));
            self.text = t;
            self.cursor = c.min(self.text.len());
        }
    }

    /// The word currently being typed before the cursor (for autocomplete).
    pub fn current_token(&self) -> (usize, String) {
        let mut i = self.cursor;
        while i > 0 && !self.text[i - 1].is_whitespace() && self.text[i - 1] != ATOM_MARK {
            i -= 1;
        }
        (i, self.text[i..self.cursor].iter().collect())
    }
    /// Replace chars `[start, cursor)` with `s`.
    pub fn replace_range(&mut self, start: usize, s: &str) {
        self.snapshot();
        self.text.drain(start..self.cursor);
        self.cursor = start;
        for c in s.chars() {
            self.text.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    /// Final text with paste atoms expanded and other atoms removed.
    pub fn expanded(&self) -> String {
        let mut out = String::new();
        let mut i = 0;
        while i < self.text.len() {
            if let Some(atom) = self.atom_at(i) {
                if let AtomKind::Paste { full } = &atom.kind {
                    out.push_str(full);
                }
                if let AtomKind::File { path } = &atom.kind {
                    out.push('@');
                    out.push_str(path);
                }
                i += 2;
                continue;
            }
            out.push(self.text[i]);
            i += 1;
        }
        out
    }

    /// Visual lines (wrapped) plus the cursor position as (row, col).
    pub fn layout(
        &self,
        width: usize,
        style: Style,
        atom_style: Style,
        cursor_style: Style,
        show_cursor: bool,
    ) -> (Vec<Line<'static>>, (u16, u16)) {
        let width = width.max(1);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut col = 0usize;
        let mut cursor_pos = (0u16, 0u16);
        let mut i = 0;
        let mut cur_run = String::new();
        let flush_run = |spans: &mut Vec<Span<'static>>, run: &mut String, st: Style| {
            if !run.is_empty() {
                spans.push(Span::styled(std::mem::take(run), st));
            }
        };
        while i <= self.text.len() {
            if i == self.cursor {
                cursor_pos = (lines.len() as u16, col as u16);
                if show_cursor {
                    flush_run(&mut spans, &mut cur_run, style);
                    let ch = if i < self.text.len() && self.text[i] != '\n' && self.text[i] != ATOM_MARK {
                        self.text[i]
                    } else {
                        ' '
                    };
                    spans.push(Span::styled(ch.to_string(), cursor_style));
                    if i < self.text.len() && self.text[i] != '\n' && self.text[i] != ATOM_MARK {
                        col += 1;
                        i += 1;
                        if col >= width {
                            lines.push(Line::from(std::mem::take(&mut spans)));
                            col = 0;
                        }
                        continue;
                    }
                }
            }
            if i == self.text.len() {
                break;
            }
            let c = self.text[i];
            if c == '\n' {
                flush_run(&mut spans, &mut cur_run, style);
                lines.push(Line::from(std::mem::take(&mut spans)));
                col = 0;
                i += 1;
                continue;
            }
            if let Some(atom) = self.atom_at(i) {
                flush_run(&mut spans, &mut cur_run, style);
                let label = format!(" {} ", atom.label);
                let w = label.width();
                if col + w > width && col > 0 {
                    lines.push(Line::from(std::mem::take(&mut spans)));
                    col = 0;
                }
                spans.push(Span::styled(label, atom_style));
                col += w;
                i += 2;
                continue;
            }
            if col >= width {
                flush_run(&mut spans, &mut cur_run, style);
                lines.push(Line::from(std::mem::take(&mut spans)));
                col = 0;
            }
            cur_run.push(c);
            col += unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
            i += 1;
        }
        flush_run(&mut spans, &mut cur_run, style);
        lines.push(Line::from(spans));
        (lines, cursor_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_and_atoms() {
        let mut t = Textarea::default();
        t.insert_str("hello world");
        t.word_backward();
        t.delete_word_forward();
        assert_eq!(t.text(), "hello ");
        t.insert_atom(AtomKind::Paste { full: "BIG".into() }, "[Pasted]".into());
        t.insert_str("end");
        assert_eq!(t.expanded(), "hello BIG end");
        t.undo_edit();
        assert_eq!(t.expanded(), "hello BIG ");
        let (lines, cursor) = t.layout(80, Style::default(), Style::default(), Style::default(), true);
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor.0, 0);
    }

    #[test]
    fn multiline_motion() {
        let mut t = Textarea::default();
        t.insert_str("ab\ncdef\ngh");
        assert!(t.on_last_line());
        t.move_up();
        assert_eq!(t.cursor(), 5);
        t.line_home();
        assert_eq!(t.cursor(), 3);
        t.delete_to_line_end();
        assert_eq!(t.text(), "ab\n\ngh");
    }
}
