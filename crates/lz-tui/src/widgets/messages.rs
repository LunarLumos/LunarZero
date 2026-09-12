//! Renders session messages into lines, caching per part so streaming only
//! re-renders the part that changed.

use std::collections::HashMap;

use lz_schema::session::*;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{diff, markdown};
use crate::store::Store;
use crate::theme::Theme;

#[derive(Default)]
pub struct RenderCache {
    entries: HashMap<String, (u64, usize, Vec<Line<'static>>)>,
}

pub struct RenderOpts {
    pub show_thinking: bool,
    pub show_details: bool,
    pub width: usize,
}

fn hash(s: &str) -> u64 {
    xxhash_rust::xxh3::xxh3_64(s.as_bytes())
}

fn wrap_plain(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for raw in text.lines() {
        if raw.is_empty() {
            out.push(Line::from(""));
            continue;
        }
        for w in textwrap::wrap(raw, width.max(10)) {
            out.push(Line::from(Span::styled(w.to_string(), style)));
        }
    }
    if out.is_empty() {
        out.push(Line::from(""));
    }
    out
}

fn short(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn tail_lines(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// One rendered block in the transcript.
pub struct Block {
    pub key: String,
    pub lines: Vec<Line<'static>>,
}

impl RenderCache {
    pub fn invalidate(&mut self, keys: impl IntoIterator<Item = String>) {
        for k in keys {
            self.entries.remove(&k);
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn cached(
        &mut self,
        key: &str,
        content_hash: u64,
        width: usize,
        build: impl FnOnce() -> Vec<Line<'static>>,
    ) -> Vec<Line<'static>> {
        if let Some((h, w, lines)) = self.entries.get(key)
            && *h == content_hash
            && *w == width
        {
            return lines.clone();
        }
        let lines = build();
        self.entries
            .insert(key.to_string(), (content_hash, width, lines.clone()));
        lines
    }

    /// Render the whole session as blocks (one per message part group).
    pub fn render_session(
        &mut self,
        store: &Store,
        session_id: &str,
        theme: &Theme,
        opts: &RenderOpts,
    ) -> Vec<Block> {
        let mut blocks = Vec::new();
        let width = opts.width.max(20);
        for msg in store.messages_of(session_id) {
            let parts = store.parts_of(msg.id());
            match msg {
                Message::User(u) => {
                    let visible: Vec<&Part> = parts
                        .iter()
                        .filter(|p| !matches!(&p.kind, PartKind::Text { synthetic: true, .. }))
                        .collect();
                    if visible.is_empty() {
                        // the runner sent the model back to its open plan items
                        let marker = parts.iter().find_map(|p| match &p.kind {
                            PartKind::Text {
                                metadata: Some(md), ..
                            } if md["plan_continue"] == true => {
                                Some("↻ plan still has open items — continuing".to_string())
                            }
                            PartKind::Text {
                                metadata: Some(md), ..
                            } if md["background_result"].is_object() => {
                                let d = md["background_result"]["description"].as_str().unwrap_or("task");
                                Some(format!("↻ background task finished: {d}"))
                            }
                            PartKind::Text {
                                metadata: Some(md), ..
                            } if md["heal"].is_object() => {
                                let h = &md["heal"];
                                let cmd = h["command"].as_str().unwrap_or("command");
                                let cmd: String = cmd.chars().take(40).collect();
                                Some(format!(
                                    "↻ `{cmd}` exited {} — repairing (round {})",
                                    h["exit"].as_i64().unwrap_or(1),
                                    h["round"].as_u64().unwrap_or(1)
                                ))
                            }
                            _ => None,
                        });
                        if let Some(text) = marker {
                            blocks.push(Block {
                                key: format!("continue-{}", u.id),
                                lines: vec![Line::from(Span::styled(text, theme.muted()))],
                            });
                        }
                        continue;
                    }
                    let content: String = visible
                        .iter()
                        .map(|p| match &p.kind {
                            PartKind::Text { text, .. } => text.clone(),
                            PartKind::File { filename, mime, .. } => format!(
                                "[{}: {}]",
                                if mime.starts_with("image/") {
                                    "image"
                                } else {
                                    "file"
                                },
                                filename.clone().unwrap_or_default()
                            ),
                            PartKind::Agent { name, .. } => format!("@{name}"),
                            PartKind::Subtask {
                                agent, description, ..
                            } => format!("@{agent} {description}"),
                            PartKind::Compaction { .. } => String::new(),
                            _ => String::new(),
                        })
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let is_compaction = parts
                        .iter()
                        .any(|p| matches!(p.kind, PartKind::Compaction { .. }));
                    if is_compaction {
                        blocks.push(Block {
                            key: format!("compaction-{}", u.id),
                            lines: vec![Line::from(Span::styled("── context compacted ──", theme.muted()))],
                        });
                        continue;
                    }
                    if content.trim().is_empty() {
                        continue;
                    }
                    let key = format!("user-{}", u.id);
                    let lines = self.cached(&key, hash(&content), width, || {
                        let body = wrap_plain(
                            &content,
                            width.saturating_sub(2),
                            theme.text().add_modifier(Modifier::BOLD),
                        );
                        body.into_iter()
                            .map(|l| {
                                let mut spans = vec![Span::styled("▌ ", theme.fg("primary"))];
                                spans.extend(l.spans);
                                Line::from(spans)
                            })
                            .collect()
                    });
                    blocks.push(Block { key, lines });
                }
                Message::Assistant(a) => {
                    for p in parts {
                        let key = p.id.clone();
                        match &p.kind {
                            PartKind::Text { text, .. } => {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                let lines = self
                                    .cached(&key, hash(text), width, || markdown::render(text, width, theme));
                                blocks.push(Block { key, lines });
                            }
                            PartKind::Reasoning { text, time, .. } => {
                                if text.trim().is_empty() {
                                    continue;
                                }
                                let h = hash(text)
                                    ^ (opts.show_thinking as u64)
                                    ^ ((time.end.is_some() as u64) << 1);
                                let lines = self.cached(&key, h, width, || {
                                    if opts.show_thinking {
                                        let mut out = vec![Line::from(Span::styled(
                                            "∴ Thinking",
                                            theme.muted().add_modifier(Modifier::ITALIC),
                                        ))];
                                        out.extend(
                                            wrap_plain(
                                                text,
                                                width.saturating_sub(2),
                                                theme.thinking().add_modifier(Modifier::ITALIC),
                                            )
                                            .into_iter()
                                            .map(|l| {
                                                let mut spans = vec![Span::styled("  ", Style::default())];
                                                spans.extend(l.spans);
                                                Line::from(spans)
                                            }),
                                        );
                                        out
                                    } else {
                                        let n = text.lines().count();
                                        let label = if time.end.is_none() {
                                            "∴ Thinking…".to_string()
                                        } else {
                                            format!("∴ Thought for {n} lines")
                                        };
                                        vec![Line::from(Span::styled(
                                            label,
                                            theme.muted().add_modifier(Modifier::ITALIC),
                                        ))]
                                    }
                                });
                                blocks.push(Block { key, lines });
                            }
                            PartKind::Tool { tool, state, .. } => {
                                let h = hash(&serde_json::to_string(state).unwrap_or_default())
                                    ^ (opts.show_details as u64);
                                let lines = self.cached(&key, h, width, || {
                                    render_tool(tool, state, theme, width, opts.show_details)
                                });
                                blocks.push(Block { key, lines });
                            }
                            PartKind::StepFinish { cost, tokens, .. } if opts.show_details => {
                                let line = format!(
                                    "┄ {} in · {} out{} · ${:.4}",
                                    fmt_tokens(tokens.input + tokens.cache.read + tokens.cache.write),
                                    fmt_tokens(tokens.output),
                                    if tokens.reasoning > 0.0 {
                                        format!(" · {} reasoning", fmt_tokens(tokens.reasoning))
                                    } else {
                                        String::new()
                                    },
                                    cost
                                );
                                blocks.push(Block {
                                    key,
                                    lines: vec![Line::from(Span::styled(line, theme.muted()))],
                                });
                            }
                            PartKind::Retry { attempt, error, .. } => {
                                blocks.push(Block {
                                    key,
                                    lines: vec![Line::from(Span::styled(
                                        format!("↻ retry {attempt}/5 · {}", error.summary(90)),
                                        theme.muted(),
                                    ))],
                                });
                            }
                            _ => {}
                        }
                    }
                    if let Some(err) = &a.error {
                        if !matches!(err, MessageError::Aborted { .. }) {
                            let key = format!("error-{}", a.id);
                            let msg = if opts.show_details {
                                err.message()
                            } else {
                                err.summary(160)
                            };
                            let lines =
                                self.cached(&key, hash(&msg) ^ (opts.show_details as u64), width, || {
                                    let mut out = vec![Line::from(vec![
                                        Span::styled(format!("✗ {}", error_name(err)), theme.bold("error")),
                                        Span::styled(
                                            if opts.show_details {
                                                ""
                                            } else {
                                                "  (/details for the full response)"
                                            },
                                            theme.muted(),
                                        ),
                                    ])];
                                    out.extend(
                                        wrap_plain(&msg, width.saturating_sub(2), theme.fg("error"))
                                            .into_iter()
                                            .map(|l| {
                                                let mut spans = vec![Span::styled("  ", Style::default())];
                                                spans.extend(l.spans);
                                                Line::from(spans)
                                            }),
                                    );
                                    out
                                });
                            blocks.push(Block { key, lines });
                        } else {
                            blocks.push(Block {
                                key: format!("error-{}", a.id),
                                lines: vec![Line::from(Span::styled("⏹ interrupted", theme.muted()))],
                            });
                        }
                    }
                }
            }
        }
        blocks
    }
}

fn error_name(e: &MessageError) -> &'static str {
    match e {
        MessageError::ProviderAuth { .. } => "Authentication error",
        MessageError::Unknown { .. } => "Error",
        MessageError::OutputLength {} => "Output length exceeded",
        MessageError::Aborted { .. } => "Aborted",
        MessageError::StructuredOutput { .. } => "Structured output error",
        MessageError::ContextOverflow { .. } => "Context overflow",
        MessageError::ContentFilter { .. } => "Content filter",
        MessageError::Api(_) => "API error",
    }
}

pub fn fmt_tokens(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 1000.0 {
        format!("{:.1}k", n / 1000.0)
    } else {
        format!("{}", n as u64)
    }
}

fn status_glyph(state: &ToolState, theme: &Theme) -> Span<'static> {
    match state {
        ToolState::Pending { .. } | ToolState::Running { .. } => Span::styled("◐ ", theme.fg("warning")),
        ToolState::Completed { .. } => Span::styled("✓ ", theme.fg("success")),
        ToolState::Error { .. } => Span::styled("✗ ", theme.fg("error")),
    }
}

fn render_tool(
    tool: &str,
    state: &ToolState,
    theme: &Theme,
    width: usize,
    details: bool,
) -> Vec<Line<'static>> {
    let input = state.input();
    let title_style = theme.text().add_modifier(Modifier::BOLD);
    let mut out: Vec<Line<'static>> = Vec::new();
    let body_width = width.saturating_sub(2);
    let indent = |lines: Vec<Line<'static>>| -> Vec<Line<'static>> {
        lines
            .into_iter()
            .map(|l| {
                let mut spans = vec![Span::styled("  ", Style::default())];
                spans.extend(l.spans);
                Line::from(spans)
            })
            .collect()
    };
    let head = |label: String, out: &mut Vec<Line<'static>>| {
        out.push(Line::from(vec![
            status_glyph(state, theme),
            Span::styled(label, title_style),
        ]));
    };
    let output = match state {
        ToolState::Completed { output, .. } => Some(output.as_str()),
        ToolState::Running {
            metadata: Some(m), ..
        } => m.get("output").and_then(|v| v.as_str()),
        _ => None,
    };
    match tool {
        "bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            head(
                format!("$ {}", short(cmd, body_width.saturating_sub(4))),
                &mut out,
            );
            if let Some(o) = output {
                let n = if details { 60 } else { 15 };
                let lines: Vec<Line<'static>> = tail_lines(o.trim_end(), n)
                    .into_iter()
                    .map(|l| Line::from(Span::styled(short(l, body_width), theme.muted())))
                    .collect();
                out.extend(indent(lines));
            }
        }
        "read" => {
            let path = input["filePath"].as_str().unwrap_or("");
            let range = match (input["offset"].as_u64(), input["limit"].as_u64()) {
                (Some(o), Some(l)) => format!(" [{o}-{}]", o + l),
                (Some(o), None) => format!(" [{o}+]"),
                _ => String::new(),
            };
            head(format!("Read {path}{range}"), &mut out);
        }
        "write" => {
            let path = input["filePath"].as_str().unwrap_or("");
            let content = input["content"].as_str().unwrap_or("");
            head(format!("Write {path}"), &mut out);
            if details {
                out.extend(indent(
                    super::syntax::highlight(content, path.rsplit('.').next().unwrap_or(""), theme)
                        .into_iter()
                        .take(80)
                        .collect(),
                ));
            } else {
                out.push(Line::from(Span::styled(
                    format!("  {} lines", content.lines().count()),
                    theme.muted(),
                )));
            }
        }
        "edit" | "apply_patch" => {
            let path = input["filePath"].as_str().unwrap_or("patch");
            head(format!("Edit {path}"), &mut out);
            let patch = match state {
                ToolState::Completed { metadata, .. } => metadata.get("diff").and_then(|d| d.as_str()),
                ToolState::Running {
                    metadata: Some(m), ..
                } => m.get("diff").and_then(|d| d.as_str()),
                _ => None,
            };
            if let Some(p) = patch {
                out.extend(indent(diff::render(
                    p,
                    body_width,
                    theme,
                    if details { None } else { Some(40) },
                )));
            } else if let ToolState::Running { .. } | ToolState::Pending { .. } = state {
                let old = input["oldString"].as_str().unwrap_or("");
                let new = input["newString"].as_str().unwrap_or("");
                if !old.is_empty() || !new.is_empty() {
                    let d = similar::TextDiff::from_lines(old, new);
                    let patch = d.unified_diff().context_radius(2).header(path, path).to_string();
                    out.extend(indent(diff::render(&patch, body_width, theme, Some(40))));
                }
            }
        }
        "glob" => {
            head(
                format!(
                    "Glob {}{}",
                    input["pattern"].as_str().unwrap_or(""),
                    input["path"]
                        .as_str()
                        .map(|p| format!(" in {p}"))
                        .unwrap_or_default()
                ),
                &mut out,
            );
            if let ToolState::Completed { metadata, .. } = state {
                out.push(Line::from(Span::styled(
                    format!("  {} files", metadata["count"].as_u64().unwrap_or(0)),
                    theme.muted(),
                )));
            }
        }
        "grep" => {
            head(
                format!(
                    "Grep /{}/{}",
                    input["pattern"].as_str().unwrap_or(""),
                    input["include"]
                        .as_str()
                        .map(|p| format!(" ({p})"))
                        .unwrap_or_default()
                ),
                &mut out,
            );
            if let ToolState::Completed { metadata, .. } = state {
                out.push(Line::from(Span::styled(
                    format!("  {} matches", metadata["matches"].as_u64().unwrap_or(0)),
                    theme.muted(),
                )));
            }
        }
        "task" => {
            head(
                format!(
                    "Task @{}: {}",
                    input["subagent_type"].as_str().unwrap_or("?"),
                    input["description"].as_str().unwrap_or("")
                ),
                &mut out,
            );
            if let Some(o) = output {
                let body = o.trim().trim_start_matches(|c| c != '\n').trim();
                let lines: Vec<Line<'static>> = wrap_plain(&short(body, 400), body_width, theme.muted())
                    .into_iter()
                    .take(6)
                    .collect();
                out.extend(indent(lines));
            }
        }
        "todowrite" => {
            head("Todos".into(), &mut out);
            if let Some(todos) = input["todos"].as_array() {
                for t in todos {
                    let glyph = match t["status"].as_str().unwrap_or("") {
                        "completed" => "☑",
                        "in_progress" => "◐",
                        "cancelled" => "☒",
                        _ => "☐",
                    };
                    out.push(Line::from(Span::styled(
                        format!("  {glyph} {}", t["content"].as_str().unwrap_or("")),
                        theme.text(),
                    )));
                }
            }
        }
        "webfetch" => head(format!("Fetch {}", input["url"].as_str().unwrap_or("")), &mut out),
        "question" => {
            head("Question".into(), &mut out);
            if let Some(qs) = input["questions"].as_array() {
                for q in qs {
                    out.push(Line::from(Span::styled(
                        format!("  ? {}", q["question"].as_str().unwrap_or("")),
                        theme.text(),
                    )));
                }
            }
            if let Some(o) = output {
                out.extend(indent(wrap_plain(o, body_width, theme.muted())));
            }
        }
        "skill" => head(
            format!("Skill {}", input["name"].as_str().unwrap_or("")),
            &mut out,
        ),
        "invalid" => head("Invalid tool call".into(), &mut out),
        _ => {
            head(tool.to_string(), &mut out);
            if details {
                out.extend(indent(wrap_plain(
                    &serde_json::to_string_pretty(input).unwrap_or_default(),
                    body_width,
                    theme.muted(),
                )));
            }
        }
    }
    if let ToolState::Error { error, .. } = state {
        out.extend(indent(wrap_plain(
            &short(error, 300),
            body_width,
            theme.fg("error"),
        )));
    }
    if let ToolState::Completed { output, .. } = state
        && let Some(i) = output.find("<diagnostics")
    {
        let block = &output[i..];
        let n = block.lines().filter(|l| l.starts_with("ERROR")).count();
        if n > 0 {
            out.push(Line::from(Span::styled(
                format!("  ⚠ {n} LSP error{}", if n == 1 { "" } else { "s" }),
                theme.fg("warning"),
            )));
        }
    }
    out
}
