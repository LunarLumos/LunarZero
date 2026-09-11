//! Transient notifications in the top-right corner.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub until: Instant,
}

#[derive(Default)]
pub struct Toasts {
    pub items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>) {
        let text = text.into();
        let dur = match kind {
            ToastKind::Error => Duration::from_secs(8),
            _ => Duration::from_secs(4),
        };
        self.items.retain(|t| t.text != text);
        self.items.push(Toast {
            kind,
            text,
            until: Instant::now() + dur,
        });
        if self.items.len() > 4 {
            self.items.remove(0);
        }
    }
    pub fn tick(&mut self) -> bool {
        let before = self.items.len();
        let now = Instant::now();
        self.items.retain(|t| t.until > now);
        before != self.items.len()
    }
    /// Notifications stack upward from the bottom of `area` — the sidebar
    /// column, or a narrow strip at the right edge when there is no sidebar —
    /// so they never cover the transcript.
    pub fn render(&self, f: &mut Frame, area: Rect, theme: &Theme) {
        let mut bottom = area.y + area.height;
        for t in self.items.iter().rev() {
            let w = area.width.saturating_sub(2).max(10);
            let wrapped = textwrap::wrap(&t.text, w.saturating_sub(4) as usize);
            let h = wrapped.len() as u16 + 2;
            if bottom < area.y + h {
                break;
            }
            let y = bottom - h;
            bottom = y;
            let rect = Rect {
                x: area.x + 1,
                y,
                width: w,
                height: h,
            };
            let key = match t.kind {
                ToastKind::Info => "info",
                ToastKind::Success => "success",
                ToastKind::Warning => "warning",
                ToastKind::Error => "error",
            };
            f.render_widget(Clear, rect);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme.fg(key))
                .style(theme.bg("backgroundPanel"));
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let lines: Vec<Line<'static>> = wrapped
                .into_iter()
                .map(|l| {
                    Line::from(Span::styled(
                        l.to_string(),
                        theme.text().add_modifier(if t.kind == ToastKind::Error {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                    ))
                })
                .collect();
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}
