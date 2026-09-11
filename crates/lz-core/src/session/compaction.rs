//! Context compaction: summarize old history with the hidden `compaction`
//! agent, keep a recent tail, and prune old tool outputs.

use std::sync::Arc;

use lz_schema::Event;
use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;
use tokio_util::sync::CancellationToken;

use super::processor::{self, ProcessInput, StepOutcome};
use crate::engine::Engine;
use crate::llm::types::*;
use crate::provider::Model;
use crate::storage::now_ms;

const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;
const PRUNE_PROTECTED_TOOLS: &[&str] = &["skill"];
const MIN_PRESERVE_RECENT_TOKENS: u64 = 2_000;
const MAX_PRESERVE_RECENT_TOKENS: u64 = 15_000;

const SUMMARY_TEMPLATE: &str = r#"Output exactly the Markdown structure shown inside <template> and keep the section order unchanged. Do not include the <template> tags in your response.
<template>
## Objective
- [one or two brief sentences describing what the user is trying to accomplish]

## Important Details
- [constraints/preferences, decisions and why, important facts/assumptions, exact context needed to continue, or "(none)"]

## Work State
### Completed
- [finished work, verified facts, or changes made; otherwise "(none)"]

### Active
- [current work, partial changes, or investigation state; otherwise "(none)"]

### Blocked
- [blockers, failing commands, or unknowns; otherwise "(none)"]

## Next Move
1. [immediate concrete action, or "(none)"]
2. [next action if known, or "(none)"]

## Relevant Files
- [file or directory path: why it matters, or "(none)"]
</template>

Rules:
- Keep every section, even when empty.
- Use terse bullets, not prose paragraphs.
- Preserve exact file paths, symbols, commands, error strings, URLs, and identifiers when known.
- Do not mention the summary process or that context was compacted."#;

const SUMMARY_UPDATE_INSTRUCTIONS: &str = r#"The <prior-summary> summarizes everything that happened before the <conversation>. Construct a new summary that combines both. The <prior-summary> is discarded after this: anything you do not carry into the new summary is lost.

When combining:
- Carry forward objectives, constraints, user directives, decisions, and parallel workstreams from the <prior-summary> even when the <conversation> does not mention them. Drop only what is finished and no longer needed.
- The <conversation> is more recent than the <prior-summary>. Where they conflict, the conversation wins: state the corrected fact and drop the old claim.
- Add new progress, decisions, constraints, and context from the conversation.
- Move completed work from "Active" to "Completed".
- If a blocker has been resolved, update the summary to reflect that while keeping any details still needed to continue the work.
- Update "Objective" and "Next Move" to reflect the current work state."#;

pub fn build_prompt(previous_summary: Option<&str>, context: &[String]) -> String {
    let conversation = format!(
        "Here is the conversation so far:\n\n<conversation>\n{}\n</conversation>",
        context.join("\n\n")
    );
    match previous_summary {
        None => [
            conversation,
            "Create a new anchored summary from the conversation history in the <conversation> tags above so another coding agent can continue the work.".into(),
            SUMMARY_TEMPLATE.into(),
        ]
        .join("\n\n"),
        Some(prev) => [
            conversation,
            format!("Here is the summary of the conversation before the <conversation> above:\n\n<prior-summary>\n{prev}\n</prior-summary>"),
            SUMMARY_UPDATE_INSTRUCTIONS.into(),
            SUMMARY_TEMPLATE.into(),
        ]
        .join("\n\n"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Stop,
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        s.to_string()
    } else {
        format!(
            "{}\n[truncated]",
            s.chars().take(TOOL_OUTPUT_MAX_CHARS).collect::<String>()
        )
    }
}

fn serialize(m: &MessageWithParts) -> String {
    match &m.info {
        Message::User(_) => {
            let text: Vec<&str> = m
                .parts
                .iter()
                .filter_map(|p| match &p.kind {
                    PartKind::Text {
                        text, ignored: false, ..
                    } if !text.is_empty() => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let mut lines = Vec::new();
            if !text.is_empty() {
                lines.push(format!("[User]: {}", text.join("\n")));
            }
            for p in &m.parts {
                if let PartKind::File { mime, filename, .. } = &p.kind {
                    lines.push(format!(
                        "[Attached {mime}: {}]",
                        filename.as_deref().unwrap_or("file")
                    ));
                }
            }
            lines.join("\n")
        }
        Message::Assistant(_) => m
            .parts
            .iter()
            .flat_map(|p| match &p.kind {
                PartKind::Text { text, .. } if !text.is_empty() => vec![format!("[Assistant]: {text}")],
                PartKind::Reasoning { text, .. } if !text.is_empty() => {
                    vec![format!("[Assistant reasoning]: {text}")]
                }
                PartKind::Tool { tool, state, .. } => {
                    let call = format!("[Assistant tool call]: {tool}({})", state.input());
                    match state {
                        ToolState::Completed {
                            output,
                            time,
                            attachments,
                            ..
                        } => {
                            let mut parts = vec![output.clone()];
                            for a in attachments.iter().flatten() {
                                parts.push(format!(
                                    "[Attached {}: {}]",
                                    a.mime,
                                    a.filename.as_deref().unwrap_or("file")
                                ));
                            }
                            let out = if time.compacted.is_some() {
                                "[Old tool result content cleared]".to_string()
                            } else {
                                truncate(&parts.join("\n"))
                            };
                            vec![call, format!("[Tool result]: {out}")]
                        }
                        ToolState::Error { error, .. } => vec![call, format!("[Tool error]: {error}")],
                        _ => vec![call],
                    }
                }
                _ => vec![],
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn summary_text(m: &MessageWithParts) -> Option<String> {
    let text: Vec<String> = m
        .parts
        .iter()
        .filter_map(|p| match &p.kind {
            PartKind::Text { text, .. } => Some(text.trim().to_string()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect();
    let joined = text.join("\n\n").trim().to_string();
    if joined.is_empty() { None } else { Some(joined) }
}

fn has_compaction_part(m: &MessageWithParts) -> bool {
    m.parts
        .iter()
        .any(|p| matches!(p.kind, PartKind::Compaction { .. }))
}

struct Completed {
    user_index: usize,
    assistant_index: usize,
    summary: Option<String>,
}

fn completed_compactions(msgs: &[MessageWithParts]) -> Vec<Completed> {
    let users: std::collections::HashMap<&str, usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m.info, Message::User(_)) && has_compaction_part(m))
        .map(|(i, m)| (m.info.id(), i))
        .collect();
    msgs.iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let Message::Assistant(a) = &m.info else {
                return None;
            };
            if a.summary != Some(true) || a.finish.is_none() || a.error.is_some() {
                return None;
            }
            let ui = *users.get(a.parent_id.as_str())?;
            Some(Completed {
                user_index: ui,
                assistant_index: i,
                summary: summary_text(m),
            })
        })
        .collect()
}

fn usable(config: &lz_schema::config::Config, model: &Model) -> u64 {
    let max_output = crate::provider::transform::max_output_tokens(model) as f64;
    let reserved = config
        .compaction
        .as_ref()
        .and_then(|c| c.reserved)
        .map(|r| r as f64)
        .unwrap_or_else(|| 20_000f64.min(max_output));
    let v = match model.limit.input {
        Some(i) if i > 0.0 => i - reserved,
        _ => model.limit.context - max_output.max(reserved),
    };
    v.max(0.0) as u64
}

fn estimate(msgs: &[MessageWithParts]) -> u64 {
    let s = serde_json::to_string(msgs).unwrap_or_default();
    (s.len() as u64).div_ceil(4)
}

struct Turn {
    start: usize,
    end: usize,
    id: String,
}

fn turns(msgs: &[MessageWithParts]) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        if !matches!(m.info, Message::User(_)) || has_compaction_part(m) {
            continue;
        }
        out.push(Turn {
            start: i,
            end: msgs.len(),
            id: m.info.id().to_string(),
        });
    }
    for i in 0..out.len().saturating_sub(1) {
        out[i].end = out[i + 1].start;
    }
    out
}

/// Split history into the part to summarize and the tail to keep verbatim.
fn select(
    msgs: &[MessageWithParts],
    config: &lz_schema::config::Config,
    model: &Model,
) -> (usize, Option<String>) {
    let limit = config.compaction.as_ref().and_then(|c| c.tail_turns);
    if limit == Some(0) {
        return (msgs.len(), None);
    }
    let budget = config
        .compaction
        .as_ref()
        .and_then(|c| c.preserve_recent_tokens)
        .unwrap_or_else(|| {
            (usable(config, model) / 4).clamp(MIN_PRESERVE_RECENT_TOKENS, MAX_PRESERVE_RECENT_TOKENS)
        });
    let all = turns(msgs);
    if all.is_empty() {
        return (msgs.len(), None);
    }
    let recent: &[Turn] = match limit {
        Some(l) => &all[all.len().saturating_sub(l as usize)..],
        None => &all,
    };
    let mut total = 0u64;
    let mut keep: Option<(usize, String)> = None;
    for turn in recent.iter().rev() {
        let size = estimate(&msgs[turn.start..turn.end]);
        if total + size <= budget {
            total += size;
            keep = Some((turn.start, turn.id.clone()));
            continue;
        }
        let remaining = budget.saturating_sub(total);
        if remaining > 0 && turn.end - turn.start > 1 {
            for start in turn.start + 1..turn.end {
                if estimate(&msgs[start..turn.end]) <= remaining {
                    keep = Some((start, msgs[start].info.id().to_string()));
                    break;
                }
            }
        }
        break;
    }
    match keep {
        Some((start, id)) if start > 0 => (start, Some(id)),
        _ => (msgs.len(), None),
    }
}

/// Append a user message carrying a `compaction` part; the loop picks it up.
pub async fn create(
    engine: &Engine,
    session_id: &str,
    last_user: &UserMessage,
    auto: bool,
    overflow: bool,
) -> Result<(), crate::storage::StorageError> {
    let msg = UserMessage {
        id: ids::ascending(Prefix::Message),
        session_id: session_id.into(),
        time: UserTime { created: now_ms() },
        format: None,
        summary: None,
        agent: last_user.agent.clone(),
        model: last_user.model.clone(),
        system: None,
        tools: None,
    };
    engine.sessions.update_message(Message::User(msg.clone())).await?;
    let part = engine.sessions.new_part(
        session_id,
        &msg.id,
        PartKind::Compaction {
            auto,
            overflow: Some(overflow),
            tail_start_id: None,
        },
    );
    engine.sessions.update_part(part).await?;
    Ok(())
}

pub async fn process(
    engine: Arc<Engine>,
    session_id: &str,
    parent: &UserMessage,
    messages: &[MessageWithParts],
    auto: bool,
    overflow: bool,
    cancel: CancellationToken,
) -> anyhow::Result<Outcome> {
    let parent_idx = messages
        .iter()
        .position(|m| m.info.id() == parent.id)
        .ok_or_else(|| anyhow::anyhow!("compaction parent not found"))?;
    let compaction_part = messages[parent_idx]
        .parts
        .iter()
        .find(|p| matches!(p.kind, PartKind::Compaction { .. }))
        .cloned();

    // on overflow, drop the last real user turn and replay it after summarizing
    let mut history: &[MessageWithParts] = messages;
    let mut replay: Option<&MessageWithParts> = None;
    if overflow {
        for i in (0..parent_idx).rev() {
            let m = &messages[i];
            if matches!(m.info, Message::User(_)) && !has_compaction_part(m) {
                replay = Some(m);
                history = &messages[..i];
                break;
            }
        }
        let has_content = replay.is_some()
            && history
                .iter()
                .any(|m| matches!(m.info, Message::User(_)) && !has_compaction_part(m));
        if !has_content {
            replay = None;
            history = messages;
        }
    }

    let agents = engine.agents();
    let agent = Arc::new(
        agents
            .get("compaction")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no compaction agent"))?,
    );
    let model = agent
        .model
        .as_ref()
        .and_then(|m| engine.registry().get(&m.provider_id, &m.model_id).cloned())
        .or_else(|| {
            engine
                .registry()
                .get(&parent.model.provider_id, &parent.model.model_id)
                .cloned()
        })
        .ok_or_else(|| anyhow::anyhow!("model not found"))?;
    let config = engine.config();
    let need = crate::provider::router::Need {
        tools: false,
        vision: false,
        tokens: history.iter().map(|m| m.parts.len() as u64 * 200).sum::<u64>() / 4,
        user_text: String::new(),
    };
    let (model, route) = engine
        .route_model(&model, need, &parent.session_id)
        .map_err(|e| anyhow::anyhow!(e))?;

    let history: &[MessageWithParts] =
        if compaction_part.is_some() && history.last().is_some_and(|m| m.info.id() == parent.id) {
            &history[..history.len() - 1]
        } else {
            history
        };
    let prior = completed_compactions(history);
    let hidden: std::collections::HashSet<usize> = prior
        .iter()
        .flat_map(|c| [c.user_index, c.assistant_index])
        .collect();
    let previous_summary = prior.last().and_then(|c| c.summary.clone());
    let visible: Vec<MessageWithParts> = history
        .iter()
        .enumerate()
        .filter(|(i, _)| !hidden.contains(i))
        .map(|(_, m)| m.clone())
        .collect();
    let (head_len, tail_start_id) = select(&visible, &config, &model);
    let conversation: String = visible[..head_len]
        .iter()
        .map(serialize)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let prompt = build_prompt(previous_summary.as_deref(), &[conversation]);

    let assistant = AssistantMessage {
        id: ids::ascending(Prefix::Message),
        session_id: session_id.into(),
        time: AssistantTime {
            created: now_ms(),
            completed: None,
        },
        error: None,
        parent_id: parent.id.clone(),
        model_id: model.id.clone(),
        provider_id: model.provider_id.clone(),
        mode: "compaction".into(),
        agent: "compaction".into(),
        path: MessagePath {
            cwd: engine.directory.display().to_string(),
            root: engine.project.worktree.display().to_string(),
        },
        summary: Some(true),
        cost: 0.0,
        tokens: Tokens::default(),
        structured: None,
        variant: parent.model.variant.clone(),
        finish: None,
    };
    engine
        .sessions
        .update_message(Message::Assistant(assistant.clone()))
        .await?;

    let result = processor::process(ProcessInput {
        engine: engine.clone(),
        session_id: session_id.into(),
        assistant,
        model: model.clone(),
        agent: agent.clone(),
        system: vec![SystemBlock {
            text: agent.prompt.clone().unwrap_or_default(),
        }],
        messages: vec![LlmMessage::User {
            content: vec![ContentPart::Text { text: prompt }],
        }],
        tools: Vec::new(),
        tool_choice: None,
        response_format: None,
        variant: None,
        cancel,
        history: Arc::new(Vec::new()),
        bypass_agent_check: false,
        route,
    })
    .await;

    let mut message = result.message;
    if result.outcome == StepOutcome::Compact {
        message.error = Some(MessageError::ContextOverflow {
            message: if replay.is_some() {
                "Conversation history too large to compact - exceeds model context limit".into()
            } else {
                "Session too large to compact - context exceeds model limit even after stripping media".into()
            },
            response_body: None,
        });
        message.finish = Some("error".into());
        engine
            .sessions
            .update_message(Message::Assistant(message))
            .await?;
        return Ok(Outcome::Stop);
    }

    if let (Some(mut part), Some(tail)) = (compaction_part, tail_start_id)
        && let PartKind::Compaction { tail_start_id, .. } = &mut part.kind
        && tail_start_id.as_deref() != Some(&tail)
    {
        *tail_start_id = Some(tail);
        engine.sessions.update_part(part).await?;
    }

    if result.outcome == StepOutcome::Continue && auto {
        if let Some(r) = replay {
            let Message::User(orig) = &r.info else {
                unreachable!()
            };
            let mut replay_msg = orig.clone();
            replay_msg.id = ids::ascending(Prefix::Message);
            replay_msg.time = UserTime { created: now_ms() };
            engine
                .sessions
                .update_message(Message::User(replay_msg.clone()))
                .await?;
            for p in &r.parts {
                if matches!(p.kind, PartKind::Compaction { .. }) {
                    continue;
                }
                let kind = match &p.kind {
                    PartKind::File { mime, filename, .. }
                        if mime.starts_with("image/") || mime == "application/pdf" =>
                    {
                        PartKind::Text {
                            text: format!("[Attached {mime}: {}]", filename.as_deref().unwrap_or("file")),
                            synthetic: false,
                            ignored: false,
                            time: None,
                            metadata: None,
                        }
                    }
                    other => other.clone(),
                };
                let np = engine.sessions.new_part(session_id, &replay_msg.id, kind);
                engine.sessions.update_part(np).await?;
            }
        } else {
            let cont = UserMessage {
                id: ids::ascending(Prefix::Message),
                session_id: session_id.into(),
                time: UserTime { created: now_ms() },
                format: None,
                summary: None,
                agent: parent.agent.clone(),
                model: parent.model.clone(),
                system: None,
                tools: None,
            };
            engine
                .sessions
                .update_message(Message::User(cont.clone()))
                .await?;
            let text = format!(
                "{}Continue if you have next steps, or stop and ask for clarification if you are unsure how to proceed.",
                if overflow {
                    "The previous request exceeded the provider's size limit due to large media attachments. The conversation was compacted and media files were removed from context. If the user was asking about attached images or files, explain that the attachments were too large to process and suggest they try again with smaller or fewer files.\n\n"
                } else {
                    ""
                }
            );
            let np = engine.sessions.new_part(
                session_id,
                &cont.id,
                PartKind::Text {
                    text,
                    synthetic: true,
                    ignored: false,
                    time: Some(PartTime {
                        start: now_ms(),
                        end: Some(now_ms()),
                    }),
                    metadata: Some(serde_json::json!({ "compaction_continue": true })),
                },
            );
            engine.sessions.update_part(np).await?;
        }
    }

    if message.error.is_some() {
        return Ok(Outcome::Stop);
    }
    if result.outcome == StepOutcome::Continue {
        engine.bus.publish(Event::SessionCompacted {
            session_id: session_id.into(),
        });
        return Ok(Outcome::Continue);
    }
    Ok(Outcome::Stop)
}

/// Clear old tool outputs once more than `prune_after_tokens` of newer
/// outputs exist and at least `prune_min_tokens` would be freed.
pub async fn prune(engine: &Engine, session_id: &str) -> anyhow::Result<()> {
    let config = engine.config();
    if !config.compaction_prune() {
        return Ok(());
    }
    let (protect, minimum) = (config.prune_after_tokens(), config.prune_min_tokens());
    let msgs = engine.sessions.messages(session_id, None, None).await?;
    let mut total = 0u64;
    let mut pruned = 0u64;
    let mut to_prune: Vec<Part> = Vec::new();
    let mut turns = 0;
    'outer: for m in msgs.iter().rev() {
        if matches!(m.info, Message::User(_)) {
            turns += 1;
        }
        if turns < 2 {
            continue;
        }
        if matches!(&m.info, Message::Assistant(a) if a.summary == Some(true)) {
            break;
        }
        for p in m.parts.iter().rev() {
            let PartKind::Tool {
                tool,
                state: ToolState::Completed { output, time, .. },
                ..
            } = &p.kind
            else {
                continue;
            };
            if PRUNE_PROTECTED_TOOLS.contains(&tool.as_str()) {
                continue;
            }
            if time.compacted.is_some() {
                break 'outer;
            }
            let est = (output.len() as u64).div_ceil(4);
            total += est;
            if total <= protect {
                continue;
            }
            pruned += est;
            to_prune.push(p.clone());
        }
    }
    if pruned > minimum {
        for mut p in to_prune {
            if let PartKind::Tool {
                state: ToolState::Completed { time, .. },
                ..
            } = &mut p.kind
            {
                time.compacted = Some(now_ms());
            }
            engine.sessions.update_part(p).await?;
        }
    }
    Ok(())
}
