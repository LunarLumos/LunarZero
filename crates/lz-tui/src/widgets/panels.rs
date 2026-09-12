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
    /// For `edit` diffs: which `@@` hunks are selected (all by default).
    pub hunks: Vec<bool>,
    /// Hunk the cursor is on (space toggles it).
    pub hunk_cursor: usize,
    /// Request the hunk state was built for.
    pub request_id: Option<String>,
}

impl Default for PermissionPanel {
    fn default() -> Self {
        PermissionPanel {
            choice: PermChoice::Once,
            fullscreen: false,
            scroll: 0,
            hunks: Vec::new(),
            hunk_cursor: 0,
            request_id: None,
        }
    }
}

/// Split a unified diff into its `@@` hunks (file headers dropped).
pub fn split_hunks(diff: &str) -> Vec<String> {
    let mut hunks: Vec<String> = Vec::new();
    for line in diff.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if line.starts_with("@@") {
            hunks.push(String::new());
        }
        if let Some(h) = hunks.last_mut() {
            h.push_str(line);
            h.push('\n');
        }
    }
    hunks
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

    /// (Re)build hunk selection state for a request.
    pub fn prepare(&mut self, req: &PermissionRequest) {
        if self.request_id.as_deref() == Some(req.id.as_str()) {
            return;
        }
        self.request_id = Some(req.id.clone());
        self.hunk_cursor = 0;
        self.hunks = if req.permission == "edit" {
            req.metadata
                .get("diff")
                .and_then(|d| d.as_str())
                .map(|d| vec![true; split_hunks(d).len()])
                .unwrap_or_default()
        } else {
            Vec::new()
        };
    }

    pub fn toggle_hunk(&mut self) {
        if let Some(h) = self.hunks.get_mut(self.hunk_cursor) {
            *h = !*h;
        }
    }

    pub fn next_hunk(&mut self) {
        if !self.hunks.is_empty() {
            self.hunk_cursor = (self.hunk_cursor + 1) % self.hunks.len();
        }
    }

    pub fn prev_hunk(&mut self) {
        if !self.hunks.is_empty() {
            self.hunk_cursor = (self.hunk_cursor + self.hunks.len() - 1) % self.hunks.len();
        }
    }

    /// Selected hunk indexes when the user left some out; `None` = everything.
    pub fn selected_hunks(&self) -> Option<Vec<usize>> {
        if self.hunks.len() < 2 || self.hunks.iter().all(|h| *h) {
            return None;
        }
        Some(
            self.hunks
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(i, _)| i)
                .collect(),
        )
    }

    pub fn body_lines(
        req: &PermissionRequest,
        width: usize,
        theme: &Theme,
        full: bool,
    ) -> Vec<Line<'static>> {
        Self::body_lines_with(req, width, theme, full, &[], usize::MAX)
    }

    pub fn body_lines_with(
        req: &PermissionRequest,
        width: usize,
        theme: &Theme,
        full: bool,
        hunks: &[bool],
        cursor: usize,
    ) -> Vec<Line<'static>> {
        let m = &req.metadata;
        let mut out = Vec::new();
        match req.permission.as_str() {
            "edit" | "write" | "apply_patch" => {
                if let Some(p) = m.get("filePath").or(m.get("filepath")).and_then(|v| v.as_str()) {
                    out.push(Line::from(Span::styled(p.to_string(), theme.bold("text"))));
                }
                if let Some(d) = m.get("diff").and_then(|v| v.as_str()) {
                    let parts = split_hunks(d);
                    if parts.len() >= 2 && hunks.len() == parts.len() {
                        // one checkbox per hunk; unchecked hunks render dimmed
                        let per_hunk = if full {
                            None
                        } else {
                            Some(24usize / parts.len().max(1) + 4)
                        };
                        for (i, h) in parts.iter().enumerate() {
                            let on = hunks[i];
                            let marker = format!(
                                "{} [{}] hunk {}/{}",
                                if i == cursor { "▶" } else { " " },
                                if on { "x" } else { " " },
                                i + 1,
                                parts.len()
                            );
                            out.push(Line::from(Span::styled(
                                marker,
                                if i == cursor {
                                    theme.bold("primary")
                                } else if on {
                                    theme.fg("success")
                                } else {
                                    theme.muted()
                                },
                            )));
                            let mut lines = diff::render(h, width, theme, per_hunk);
                            if !on {
                                for l in &mut lines {
                                    for sp in &mut l.spans {
                                        sp.style = theme.muted().add_modifier(Modifier::DIM);
                                    }
                                }
                            }
                            out.extend(lines);
                        }
                    } else {
                        out.extend(diff::render(d, width, theme, if full { None } else { Some(24) }));
                    }
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
            "install" => {
                out.push(Line::from(Span::styled(
                    "⚠ Third-party code the agent wants to install — you did not ask for this.",
                    theme.bold("error"),
                )));
                if let Some(src) = m.get("source").and_then(|v| v.as_str()) {
                    out.push(Line::from(vec![
                        Span::styled("source  ", theme.muted()),
                        Span::styled(src.to_string(), theme.text()),
                    ]));
                }
                if let Some(c) = m.get("command").and_then(|v| v.as_str()) {
                    out.push(Line::from(vec![
                        Span::styled("$ ", theme.fg("primary")),
                        Span::styled(c.to_string(), theme.text()),
                    ]));
                }
                out.push(Line::from(Span::styled(
                    "It will be cloned, built and run on this machine. Allow only if you trust the source.",
                    theme.muted(),
                )));
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
        self.prepare(req);
        let body = Self::body_lines_with(
            req,
            inner.width as usize,
            theme,
            full,
            &self.hunks,
            self.hunk_cursor,
        );
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
        let once_label = match self.selected_hunks() {
            Some(sel) => format!(" Apply {} of {} hunks ", sel.len(), self.hunks.len()),
            None => " Allow once ".to_string(),
        };
        let hint = if self.hunks.len() >= 2 {
            "   space toggle hunk · n/p next/prev · enter · A always · n/esc reject · r reason · shift+tab mode"
        } else {
            "   ←/→ tab · enter · a/y once · A always · n/esc reject · r reason · shift+tab mode · ctrl+f full"
        };
        let buttons = Line::from(vec![
            Span::styled(once_label, if self.choice == PermChoice::Once { on } else { off }),
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
            Span::styled(hint, theme.muted()),
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
