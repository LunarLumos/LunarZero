//! Modal dialogs: a generic fuzzy-select list, confirm, text input, and a
//! few static panels (help/status). The app owns a stack of these.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher, Utf32Str};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: String,
    pub category: String,
    /// Right-aligned hint (e.g. keybind, "★").
    pub hint: String,
    pub disabled: bool,
}

impl SelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        SelectItem {
            value: value.into(),
            label: label.into(),
            description: String::new(),
            category: String::new(),
            hint: String::new(),
            disabled: false,
        }
    }
    pub fn desc(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }
    pub fn cat(mut self, c: impl Into<String>) -> Self {
        self.category = c.into();
        self
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = h.into();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    Models,
    Providers,
    Agents,
    Variants,
    Sessions,
    Themes,
    Palette,
    Mcp,
    Skills,
    Commands,
    Fork,
    Export,
    Modes,
}

pub struct SelectDialog {
    pub kind: SelectKind,
    pub title: String,
    pub items: Vec<SelectItem>,
    pub filter: String,
    pub cursor: usize,
    /// Indices into `items` after filtering.
    pub visible: Vec<usize>,
    pub footer: String,
    matcher: Matcher,
}

impl SelectDialog {
    pub fn new(kind: SelectKind, title: impl Into<String>, items: Vec<SelectItem>) -> Self {
        let mut d = SelectDialog {
            kind,
            title: title.into(),
            items,
            filter: String::new(),
            cursor: 0,
            visible: Vec::new(),
            footer: String::new(),
            matcher: Matcher::new(NucleoConfig::DEFAULT),
        };
        d.refilter();
        d
    }
    pub fn with_footer(mut self, f: impl Into<String>) -> Self {
        self.footer = f.into();
        self
    }
    pub fn select_value(&mut self, value: &str) {
        if let Some(pos) = self.visible.iter().position(|&i| self.items[i].value == value) {
            self.cursor = pos;
        }
    }
    pub fn refilter(&mut self) {
        if self.filter.trim().is_empty() {
            self.visible = (0..self.items.len()).collect();
        } else {
            let pat = Pattern::parse(&self.filter, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let hay = format!("{} {} {}", it.label, it.description, it.category);
                    let s = pat.score(Utf32Str::new(&hay, &mut buf), &mut self.matcher)?;
                    Some((s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.visible = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
    }
    pub fn current(&self) -> Option<&SelectItem> {
        self.visible.get(self.cursor).map(|&i| &self.items[i])
    }
    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let n = self.visible.len() as isize;
        self.cursor = ((self.cursor as isize + delta).rem_euclid(n)) as usize;
    }
    pub fn type_char(&mut self, c: char) {
        self.filter.push(c);
        self.cursor = 0;
        self.refilter();
    }
    pub fn backspace(&mut self) {
        self.filter.pop();
        self.refilter();
    }
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.refilter();
    }

    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let w = area.width.clamp(30, 90).min(area.width);
        let max_h = area.height.saturating_sub(2).max(6);
        let list_needed = self.visible.len() as u16 + self.categories_count() as u16;
        let h = (list_needed + 5).clamp(6, max_h.min(28));
        let rect = centered(area, w, h);
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("borderActive"))
            .style(theme.bg("backgroundPanel"))
            .title(Span::styled(format!(" {} ", self.title), theme.bold("text")));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let [filter_area, _, list_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(if self.footer.is_empty() { 0 } else { 1 }),
        ])
        .areas(inner);
        let filter_line = Line::from(vec![
            Span::styled("› ", theme.fg("primary")),
            Span::styled(self.filter.clone(), theme.text()),
            Span::styled(
                if self.filter.is_empty() {
                    "type to filter…"
                } else {
                    ""
                },
                theme.muted(),
            ),
        ]);
        f.render_widget(Paragraph::new(filter_line), filter_area);
        f.set_cursor_position((
            filter_area.x + 2 + self.filter.chars().count() as u16,
            filter_area.y,
        ));

        // build rows with category headers
        let mut rows: Vec<(Line<'static>, Option<usize>)> = Vec::new();
        let mut last_cat: Option<&str> = None;
        for (pos, &i) in self.visible.iter().enumerate() {
            let it = &self.items[i];
            if !it.category.is_empty() && last_cat != Some(it.category.as_str()) && self.filter.is_empty() {
                rows.push((
                    Line::from(Span::styled(
                        format!(" {}", it.category),
                        theme.muted().add_modifier(Modifier::BOLD),
                    )),
                    None,
                ));
                last_cat = Some(it.category.as_str());
            }
            let selected = pos == self.cursor;
            let inner_w = list_area.width as usize;
            let hint_w = it.hint.chars().count();
            let label_w = it.label.chars().count();
            let mut desc = it.description.clone();
            let avail = inner_w.saturating_sub(4 + label_w + hint_w + 2);
            if desc.chars().count() > avail {
                desc = desc.chars().take(avail.saturating_sub(1)).collect::<String>()
                    + if avail > 0 { "…" } else { "" };
            }
            let pad = inner_w.saturating_sub(3 + label_w + 1 + desc.chars().count() + hint_w + 1);
            let base = if selected {
                theme.bg("backgroundElement").add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let label_style = if it.disabled {
                theme.muted()
            } else if selected {
                base.fg(theme.color("primary"))
            } else {
                theme.text()
            };
            let line = Line::from(vec![
                Span::styled(
                    if selected { " ▶ " } else { "   " },
                    base.fg(theme.color("primary")),
                ),
                Span::styled(it.label.clone(), label_style),
                Span::styled(" ", base),
                Span::styled(desc, base.fg(theme.color("textMuted"))),
                Span::styled(" ".repeat(pad), base),
                Span::styled(it.hint.clone(), base.fg(theme.color("textMuted"))),
                Span::styled(" ", base),
            ]);
            rows.push((line, Some(pos)));
        }
        if rows.is_empty() {
            rows.push((Line::from(Span::styled("  no matches", theme.muted())), None));
        }
        // keep the cursor row visible
        let cursor_row = rows
            .iter()
            .position(|(_, p)| *p == Some(self.cursor))
            .unwrap_or(0);
        let height = list_area.height as usize;
        let start = if cursor_row >= height {
            cursor_row + 1 - height
        } else {
            0
        };
        let lines: Vec<Line<'static>> = rows
            .into_iter()
            .skip(start)
            .take(height)
            .map(|(l, _)| l)
            .collect();
        f.render_widget(Paragraph::new(lines), list_area);
        if !self.footer.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(self.footer.clone(), theme.muted())))
                    .alignment(Alignment::Right),
                footer_area,
            );
        }
    }

    fn categories_count(&self) -> usize {
        if !self.filter.is_empty() {
            return 0;
        }
        let mut n = 0;
        let mut last: Option<&str> = None;
        for &i in &self.visible {
            let c = self.items[i].category.as_str();
            if !c.is_empty() && last != Some(c) {
                n += 1;
                last = Some(c);
            }
        }
        n
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    DeleteSession(String),
    Exit,
    Revert(String),
    McpDisconnect(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputAction {
    RenameSession(String),
    ProviderKey(String),
    RejectReason(String),
    QuestionCustom { question_id: String, index: usize },
    ExportPath(String),
}

pub struct InputDialog {
    pub title: String,
    pub prompt: String,
    pub value: String,
    pub masked: bool,
    pub action: InputAction,
}

pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub action: ConfirmAction,
    pub yes: bool,
}

pub struct TextDialog {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
}

pub enum Dialog {
    Select(SelectDialog),
    Confirm(ConfirmDialog),
    Input(InputDialog),
    Text(TextDialog),
}

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

impl ConfirmDialog {
    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let w = area.width.clamp(30, 70).min(area.width);
        let body = textwrap::wrap(&self.message, w.saturating_sub(4) as usize);
        let h = (body.len() as u16 + 5).min(area.height);
        let rect = centered(area, w, h);
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("borderActive"))
            .style(theme.bg("backgroundPanel"))
            .title(Span::styled(format!(" {} ", self.title), theme.bold("text")));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let mut lines: Vec<Line<'static>> = body
            .into_iter()
            .map(|l| Line::from(Span::styled(l.to_string(), theme.text())))
            .collect();
        lines.push(Line::from(""));
        let on = theme
            .bg("primary")
            .fg(theme.color("background"))
            .add_modifier(Modifier::BOLD);
        let off = theme.bg("backgroundElement").fg(theme.color("text"));
        lines.push(
            Line::from(vec![
                Span::styled("  Yes  ", if self.yes { on } else { off }),
                Span::raw("  "),
                Span::styled("  No  ", if self.yes { off } else { on }),
                Span::styled("   ←/→ tab · enter · esc", theme.muted()),
            ])
            .alignment(Alignment::Center),
        );
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

impl InputDialog {
    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let w = area.width.clamp(30, 80).min(area.width);
        let h = 6.min(area.height);
        let rect = centered(area, w, h);
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("borderActive"))
            .style(theme.bg("backgroundPanel"))
            .title(Span::styled(format!(" {} ", self.title), theme.bold("text")));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let shown: String = if self.masked {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        };
        let lines = vec![
            Line::from(Span::styled(self.prompt.clone(), theme.muted())),
            Line::from(vec![
                Span::styled("› ", theme.fg("primary")),
                Span::styled(shown.clone(), theme.text()),
            ]),
            Line::from(""),
            Line::from(Span::styled("enter to confirm · esc to cancel", theme.muted())),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        f.set_cursor_position((inner.x + 2 + shown.chars().count() as u16, inner.y + 1));
    }
}

impl TextDialog {
    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let w = area.width.clamp(40, 100).min(area.width);
        let h = (self.lines.len() as u16 + 3).clamp(5, area.height.saturating_sub(2).max(5));
        let rect = centered(area, w, h);
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.fg("borderActive"))
            .style(theme.bg("backgroundPanel"))
            .title(Span::styled(format!(" {} ", self.title), theme.bold("text")));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(
            Paragraph::new(self.lines.clone())
                .scroll((self.scroll, 0))
                .wrap(Wrap { trim: false }),
            inner,
        );
    }
}
