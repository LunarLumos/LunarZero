//! Right-hand sidebar: context usage, changed files, LSP/MCP status, todos.

use lz_schema::api::McpStatus;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::messages::fmt_tokens;
use crate::store::Store;
use crate::theme::Theme;

pub fn render(f: &mut Frame, area: Rect, store: &Store, session_id: &str, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.fg("border"));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let h = |s: &str| {
        Line::from(Span::styled(
            s.to_string(),
            theme.muted().add_modifier(Modifier::BOLD),
        ))
    };
    if let Some(s) = store.sessions.get(session_id) {
        lines.push(Line::from(Span::styled(s.title.clone(), theme.bold("text"))));
        lines.push(Line::from(""));
        lines.push(h("CONTEXT"));
        let (total, cost) = store.usage(session_id);
        let limit = s
            .model
            .as_ref()
            .and_then(|m| store.model_info(&m.provider_id, &m.id))
            .map(|m| m.limit.context)
            .unwrap_or(0.0);
        let pct = if limit > 0.0 {
            (total / limit * 100.0).min(999.0)
        } else {
            0.0
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} tokens", fmt_tokens(total)), theme.text()),
            Span::styled(
                if limit > 0.0 {
                    format!(" · {pct:.0}%")
                } else {
                    String::new()
                },
                theme.muted(),
            ),
        ]));
        lines.push(Line::from(Span::styled(format!("${cost:.4}"), theme.muted())));
        lines.push(Line::from(""));
    }
    // what the current step is built from: model + why, skills, MCP servers
    if let Some(step) = store.step.get(session_id) {
        lines.push(h("THIS TURN"));
        lines.push(Line::from(vec![
            Span::styled(step.model.clone(), theme.text()),
            Span::styled(format!(" · {}", step.reason), theme.muted()),
        ]));
        lines.push(Line::from(Span::styled(
            format!("~{} tokens sent", fmt_tokens(step.tokens as f64)),
            theme.muted(),
        )));
        if let Some(a) = &step.attached_skill {
            lines.push(Line::from(vec![
                Span::styled("skill ", theme.muted()),
                Span::styled(a.clone(), theme.fg("accent")),
                Span::styled(" attached", theme.muted()),
            ]));
        }
        if !step.skills.is_empty() {
            let others: Vec<&str> = step
                .skills
                .iter()
                .filter(|s| Some(*s) != step.attached_skill.as_ref())
                .map(String::as_str)
                .collect();
            if !others.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("skills ", theme.muted()),
                    Span::styled(others.join(", "), theme.text()),
                ]));
            }
        }
        if !step.mcp_loaded.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("mcp ", theme.muted()),
                Span::styled(step.mcp_loaded.join(", "), theme.fg("success")),
            ]));
        }
        if !step.mcp_skipped.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("mcp idle ", theme.muted()),
                Span::styled(step.mcp_skipped.join(", "), theme.muted()),
            ]));
        }
        lines.push(Line::from(""));
    }
    if let Some(diffs) = store.diffs.get(session_id)
        && !diffs.is_empty()
    {
        lines.push(h("CHANGES"));
        for d in diffs.iter().take(12) {
            let file = d.file.clone().unwrap_or_default();
            let name = file.rsplit('/').next().unwrap_or(&file).to_string();
            lines.push(Line::from(vec![
                Span::styled(name, theme.text()),
                Span::styled(format!(" +{}", d.additions), theme.fg("diffAdded")),
                Span::styled(format!(" -{}", d.deletions), theme.fg("diffRemoved")),
            ]));
        }
        if diffs.len() > 12 {
            lines.push(Line::from(Span::styled(
                format!("… {} more", diffs.len() - 12),
                theme.muted(),
            )));
        }
        lines.push(Line::from(""));
    }
    if !store.lsp.is_empty() {
        lines.push(h("LSP"));
        for l in &store.lsp {
            let (g, st) = if l.status == "connected" {
                ("●", theme.fg("success"))
            } else {
                ("●", theme.fg("error"))
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{g} "), st),
                Span::styled(l.name.clone(), theme.text()),
            ]));
        }
        lines.push(Line::from(""));
    }
    if !store.mcp.is_empty() {
        lines.push(h("MCP"));
        for (name, st) in &store.mcp {
            let (g, style) = match st {
                McpStatus::Connected => ("●", theme.fg("success")),
                McpStatus::Disabled => ("○", theme.muted()),
                McpStatus::Failed { .. } => ("●", theme.fg("error")),
                McpStatus::NeedsAuth => ("●", theme.fg("warning")),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{g} "), style),
                Span::styled(name.clone(), theme.text()),
            ]));
        }
        lines.push(Line::from(""));
    }
    if let Some(todos) = store.todos.get(session_id)
        && !todos.is_empty()
    {
        lines.push(h("PLAN"));
        for t in todos {
            let (g, style) = match t.status.as_str() {
                "completed" => ("☑", theme.muted().add_modifier(Modifier::CROSSED_OUT)),
                "in_progress" => ("◐", theme.fg("warning")),
                "cancelled" => ("☒", theme.muted()),
                _ => ("☐", theme.text()),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{g} "), style),
                Span::styled(t.content.clone(), style),
            ]));
        }
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    );
}
