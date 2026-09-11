//! The prompt: textarea + mode (normal / `!` shell / `/` command) +
//! autocomplete popup + history ring.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::textarea::{AtomKind, Textarea};
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    Normal,
    Shell,
    Command,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub label: String,
    pub description: String,
    /// Text inserted in place of the current token.
    pub insert: String,
    /// `@path` completions attach a File atom instead of raw text.
    pub file: bool,
    pub agent: bool,
}

#[derive(Default)]
pub struct Autocomplete {
    pub items: Vec<Completion>,
    pub cursor: usize,
    pub token_start: usize,
    pub query: String,
    pub visible: bool,
    pub request_id: u64,
}

pub struct Prompt {
    pub textarea: Textarea,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub stash: Option<String>,
    pub autocomplete: Autocomplete,
    pub max_height: u16,
    scroll: u16,
}

impl Prompt {
    pub fn new(history: Vec<String>, max_height: u16) -> Prompt {
        let mut textarea = Textarea::default();
        textarea.placeholder = "Ask anything… (@ files, / commands, ! shell)".into();
        Prompt {
            textarea,
            history,
            history_idx: None,
            stash: None,
            autocomplete: Autocomplete::default(),
            max_height: max_height.max(1),
            scroll: 0,
        }
    }

    pub fn mode(&self) -> PromptMode {
        let t = self.textarea.text();
        if t.starts_with('!') {
            PromptMode::Shell
        } else if t.starts_with('/') && !t.contains('\n') {
            PromptMode::Command
        } else {
            PromptMode::Normal
        }
    }

    pub fn push_history(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
            if self.history.len() > 200 {
                self.history.remove(0);
            }
        }
        self.history_idx = None;
    }

    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let idx = match self.history_idx {
            None => {
                self.stash = Some(self.textarea.text());
                self.history.len() - 1
            }
            Some(0) => return true,
            Some(i) => i - 1,
        };
        self.history_idx = Some(idx);
        self.textarea.set_text(&self.history[idx].clone());
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(i) = self.history_idx else { return false };
        if i + 1 < self.history.len() {
            self.history_idx = Some(i + 1);
            self.textarea.set_text(&self.history[i + 1].clone());
        } else {
            self.history_idx = None;
            let s = self.stash.take().unwrap_or_default();
            self.textarea.set_text(&s);
        }
        true
    }

    /// Height in rows the prompt wants at `width` (excluding border).
    pub fn height(&self, width: u16) -> u16 {
        let (lines, _) = self.textarea.layout(
            width.saturating_sub(4) as usize,
            Style::default(),
            Style::default(),
            Style::default(),
            false,
        );
        (lines.len() as u16).clamp(1, self.max_height)
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme, focused: bool, hint: &str) {
        let mode = self.mode();
        let border_color = match mode {
            PromptMode::Shell => theme.color("warning"),
            PromptMode::Command => theme.color("secondary"),
            PromptMode::Normal => {
                if focused {
                    theme.color("borderActive")
                } else {
                    theme.color("border")
                }
            }
        };
        let title = match mode {
            PromptMode::Shell => " shell ",
            PromptMode::Command => " command ",
            PromptMode::Normal => "",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                title,
                Style::default().fg(border_color).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::from(Span::styled(hint.to_string(), theme.muted())).right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let text_w = inner.width.saturating_sub(2) as usize;
        let (mut lines, (cy, cx)) = self.textarea.layout(
            text_w,
            theme.text(),
            theme.bg("backgroundElement").fg(theme.color("primary")),
            theme.text().add_modifier(Modifier::REVERSED),
            focused,
        );
        if self.textarea.is_empty() {
            lines = vec![Line::from(Span::styled(
                self.textarea.placeholder.clone(),
                theme.muted(),
            ))];
        }
        let h = inner.height.max(1);
        if cy < self.scroll {
            self.scroll = cy;
        } else if cy >= self.scroll + h {
            self.scroll = cy + 1 - h;
        }
        let prefix = Span::styled(
            "› ",
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        );
        let shown: Vec<Line<'static>> = lines
            .into_iter()
            .skip(self.scroll as usize)
            .take(h as usize)
            .enumerate()
            .map(|(i, l)| {
                let mut spans = vec![if i == 0 && self.scroll == 0 {
                    prefix.clone()
                } else {
                    Span::raw("  ")
                }];
                spans.extend(l.spans);
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(shown), inner);
        if focused {
            f.set_cursor_position((inner.x + 2 + cx, inner.y + cy - self.scroll));
        }
    }

    /// Popup above the prompt.
    pub fn render_autocomplete(&self, f: &mut Frame, prompt_area: Rect, theme: &Theme) {
        let ac = &self.autocomplete;
        if !ac.visible || ac.items.is_empty() {
            return;
        }
        let n = ac.items.len().min(8) as u16;
        let width = prompt_area.width.min(70);
        let y = prompt_area.y.saturating_sub(n + 2);
        let rect = Rect {
            x: prompt_area.x,
            y,
            width,
            height: n + 2,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("border"))
            .style(theme.bg("backgroundPanel"));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let start = ac.cursor.saturating_sub(n as usize - 1);
        let lines: Vec<Line<'static>> = ac
            .items
            .iter()
            .enumerate()
            .skip(start)
            .take(n as usize)
            .map(|(i, c)| {
                let sel = i == ac.cursor;
                let base = if sel {
                    theme.bg("backgroundElement")
                } else {
                    Style::default()
                };
                let label_w = inner.width as usize / 2;
                let mut label = c.label.clone();
                if label.chars().count() > label_w {
                    label = format!(
                        "…{}",
                        label
                            .chars()
                            .rev()
                            .take(label_w - 1)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<String>()
                    );
                }
                let pad = label_w.saturating_sub(label.chars().count());
                let mut desc = c.description.clone();
                let dw = inner.width as usize - label_w - 3;
                if desc.chars().count() > dw {
                    desc = desc.chars().take(dw.saturating_sub(1)).collect::<String>() + "…";
                }
                Line::from(vec![
                    Span::styled(if sel { "▶ " } else { "  " }, base.fg(theme.color("primary"))),
                    Span::styled(
                        label,
                        if sel {
                            base.fg(theme.color("primary")).add_modifier(Modifier::BOLD)
                        } else {
                            base.fg(theme.color("text"))
                        },
                    ),
                    Span::styled(" ".repeat(pad + 1), base),
                    Span::styled(desc, base.fg(theme.color("textMuted"))),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), inner);
    }

    /// Apply the highlighted completion to the textarea.
    pub fn accept_completion(&mut self) -> bool {
        let Some(c) = self.autocomplete.items.get(self.autocomplete.cursor).cloned() else {
            return false;
        };
        let start = self.autocomplete.token_start;
        if c.file {
            self.textarea.replace_range(start, "");
            self.textarea.insert_atom(
                AtomKind::File {
                    path: c.insert.clone(),
                },
                format!("@{}", c.insert),
            );
        } else {
            self.textarea.replace_range(start, &c.insert);
            if !c.insert.ends_with(' ') {
                self.textarea.insert_char(' ');
            }
        }
        self.autocomplete.visible = false;
        self.autocomplete.items.clear();
        true
    }
}
