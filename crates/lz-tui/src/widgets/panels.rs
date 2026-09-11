//! Bottom panels that replace the prompt: permission request and question.

use lz_schema::session::{PermissionRequest, QuestionRequest};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::diff;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermChoice {
    Once,
    Always,
    Reject,
}

pub struct PermissionPanel {
    pub choice: PermChoice,
    pub fullscreen: bool,
    pub scroll: u16,
}

impl Default for PermissionPanel {
    fn default() -> Self {
        PermissionPanel {
            choice: PermChoice::Once,
            fullscreen: false,
            scroll: 0,
        }
    }
}

impl PermissionPanel {
    pub fn next(&mut self) {
        self.choice = match self.choice {
            PermChoice::Once => PermChoice::Always,
            PermChoice::Always => PermChoice::Reject,
            PermChoice::Reject => PermChoice::Once,
        };
    }
    pub fn prev(&mut self) {
        self.choice = match self.choice {
            PermChoice::Once => PermChoice::Reject,
            PermChoice::Always => PermChoice::Once,
            PermChoice::Reject => PermChoice::Always,
        };
    }

    pub fn body_lines(
        req: &PermissionRequest,
        width: usize,
        theme: &Theme,
        full: bool,
    ) -> Vec<Line<'static>> {
        let m = &req.metadata;
        let mut out = Vec::new();
        match req.permission.as_str() {
            "edit" | "write" | "apply_patch" => {
                if let Some(p) = m.get("filePath").or(m.get("filepath")).and_then(|v| v.as_str()) {
                    out.push(Line::from(Span::styled(p.to_string(), theme.bold("text"))));
                }
                if let Some(d) = m.get("diff").and_then(|v| v.as_str()) {
                    out.extend(diff::render(d, width, theme, if full { None } else { Some(24) }));
                } else if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
                    out.extend(
                        c.lines()
                            .take(if full { 2000 } else { 20 })
                            .map(|l| Line::from(Span::styled(l.to_string(), theme.text()))),
                    );
                }
            }
            "bash" => {
                if let Some(c) = m.get("command").and_then(|v| v.as_str()) {
                    for l in c.lines() {
                        out.push(Line::from(vec![
                            Span::styled("$ ", theme.fg("primary")),
                            Span::styled(l.to_string(), theme.text()),
                        ]));
                    }
                }
                if let Some(d) = m.get("description").and_then(|v| v.as_str()) {
                    out.push(Line::from(Span::styled(d.to_string(), theme.muted())));
                }
            }
            "external_directory" => {
                for p in &req.patterns {
                    out.push(Line::from(Span::styled(p.clone(), theme.text())));
                }
            }
            "doom_loop" => {
                out.push(Line::from(Span::styled(
                    "The model is repeating the same tool call. Continue?",
                    theme.fg("warning"),
                )));
            }
            _ => {
                for p in &req.patterns {
                    out.push(Line::from(Span::styled(p.clone(), theme.text())));
                }
                if !m.is_null() && m.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    let s = serde_json::to_string_pretty(m).unwrap_or_default();
                    out.extend(
                        s.lines()
                            .take(if full { 500 } else { 12 })
                            .map(|l| Line::from(Span::styled(l.to_string(), theme.muted()))),
                    );
                }
            }
        }
        out
    }

    /// Desired height (rows including border) for the compact view.
    pub fn height(&self, req: &PermissionRequest, width: u16, theme: &Theme, max: u16) -> u16 {
        let body = Self::body_lines(req, width.saturating_sub(4) as usize, theme, false).len() as u16;
        (body + 5).clamp(6, max)
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, req: &PermissionRequest, theme: &Theme, full: bool) {
        let title = format!(" Permission: {} ", req.permission);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("warning"))
            .title(Span::styled(title, theme.bold("warning")));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let body = Self::body_lines(req, inner.width as usize, theme, full);
        let body_h = inner.height.saturating_sub(2);
        let max_scroll = (body.len() as u16).saturating_sub(body_h);
        self.scroll = self.scroll.min(max_scroll);
        let body_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: body_h,
        };
        f.render_widget(
            Paragraph::new(body)
                .scroll((self.scroll, 0))
                .wrap(Wrap { trim: false }),
            body_area,
        );
        let on = theme
            .bg("primary")
            .fg(theme.color("background"))
            .add_modifier(Modifier::BOLD);
        let off = theme.bg("backgroundElement").fg(theme.color("text"));
        let always_label = if req.always.is_empty() {
            " Allow always ".to_string()
        } else {
            format!(" Always: {} ", req.always.join(", "))
        };
        let buttons = Line::from(vec![
            Span::styled(
                " Allow once ",
                if self.choice == PermChoice::Once { on } else { off },
            ),
            Span::raw(" "),
            Span::styled(
                always_label,
                if self.choice == PermChoice::Always {
                    on
                } else {
                    off
                },
            ),
            Span::raw(" "),
            Span::styled(
                " Reject ",
                if self.choice == PermChoice::Reject {
                    theme
                        .bg("error")
                        .fg(theme.color("background"))
                        .add_modifier(Modifier::BOLD)
                } else {
                    off
                },
            ),
            Span::styled(
                "   ←/→ tab · enter · a/y once · A always · n/esc reject · r reason · shift+tab mode · ctrl+f full",
                theme.muted(),
            ),
        ]);
        let btn_area = Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(buttons).alignment(Alignment::Left), btn_area);
    }
}

pub struct QuestionPanel {
    pub tab: usize,
    pub cursor: usize,
    /// Per-question selected option indexes.
    pub selected: Vec<Vec<usize>>,
    /// Per-question custom answers.
    pub custom: Vec<Option<String>>,
}

impl QuestionPanel {
    pub fn new(req: &QuestionRequest) -> Self {
        QuestionPanel {
            tab: 0,
            cursor: 0,
            selected: vec![Vec::new(); req.questions.len()],
            custom: vec![None; req.questions.len()],
        }
    }

    pub fn option_count(&self, req: &QuestionRequest) -> usize {
        let q = &req.questions[self.tab];
        q.options.len() + if q.custom.unwrap_or(true) { 1 } else { 0 }
    }

    pub fn is_custom_row(&self, req: &QuestionRequest) -> bool {
        self.cursor >= req.questions[self.tab].options.len()
    }

    pub fn toggle(&mut self, req: &QuestionRequest) {
        let q = &req.questions[self.tab];
        if self.is_custom_row(req) {
            return;
        }
        let multiple = q.multiple.unwrap_or(false);
        let sel = &mut self.selected[self.tab];
        if multiple {
            if let Some(p) = sel.iter().position(|&i| i == self.cursor) {
                sel.remove(p);
            } else {
                sel.push(self.cursor);
            }
        } else {
            sel.clear();
            sel.push(self.cursor);
            self.custom[self.tab] = None;
        }
    }

    pub fn answers(&self, req: &QuestionRequest) -> Vec<Vec<String>> {
        req.questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let mut v: Vec<String> = self.selected[i]
                    .iter()
                    .filter_map(|&j| q.options.get(j).map(|o| o.label.clone()))
                    .collect();
                if let Some(c) = &self.custom[i]
                    && !c.trim().is_empty()
                {
                    v.push(c.clone());
                }
                v
            })
            .collect()
    }

    pub fn height(&self, req: &QuestionRequest, max: u16) -> u16 {
        let q = &req.questions[self.tab];
        (q.options.len() as u16 * 2 + 6).clamp(6, max)
    }

    pub fn render(&self, f: &mut Frame, area: Rect, req: &QuestionRequest, theme: &Theme) {
        let q = &req.questions[self.tab];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("info"))
            .title(Span::styled(" Question ", theme.bold("info")));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let mut lines: Vec<Line<'static>> = Vec::new();
        if req.questions.len() > 1 {
            let tabs: Vec<Span<'static>> = req
                .questions
                .iter()
                .enumerate()
                .flat_map(|(i, qq)| {
                    let done = !self.selected[i].is_empty() || self.custom[i].is_some();
                    let st = if i == self.tab {
                        theme
                            .bg("backgroundElement")
                            .fg(theme.color("primary"))
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.muted()
                    };
                    vec![
                        Span::styled(format!(" {}{} ", qq.header, if done { " ✓" } else { "" }), st),
                        Span::raw(" "),
                    ]
                })
                .collect();
            lines.push(Line::from(tabs));
        }
        lines.push(Line::from(Span::styled(q.question.clone(), theme.bold("text"))));
        lines.push(Line::from(""));
        let multiple = q.multiple.unwrap_or(false);
        for (i, o) in q.options.iter().enumerate() {
            let sel = self.selected[self.tab].contains(&i);
            let glyph = match (multiple, sel) {
                (true, true) => "[x]",
                (true, false) => "[ ]",
                (false, true) => "(•)",
                (false, false) => "( )",
            };
            let cur = i == self.cursor;
            let base = if cur {
                theme.bg("backgroundElement")
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(if cur { "▶ " } else { "  " }, base.fg(theme.color("primary"))),
                Span::styled(format!("{glyph} "), base.fg(theme.color("primary"))),
                Span::styled(
                    o.label.clone(),
                    base.fg(theme.color("text")).add_modifier(if cur {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
            ]));
            if !o.description.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("      {}", o.description),
                    theme.muted(),
                )));
            }
        }
        if q.custom.unwrap_or(true) {
            let cur = self.cursor == q.options.len();
            let base = if cur {
                theme.bg("backgroundElement")
            } else {
                Style::default()
            };
            let custom = self.custom[self.tab]
                .clone()
                .unwrap_or_else(|| "Type your own answer…".into());
            lines.push(Line::from(vec![
                Span::styled(if cur { "▶ " } else { "  " }, base.fg(theme.color("primary"))),
                Span::styled("✎ ", base.fg(theme.color("primary"))),
                Span::styled(custom, base.fg(theme.color("textMuted"))),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "↑/↓ move · space toggle · ←/→ questions · enter submit · esc dismiss",
            theme.muted(),
        )));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}
