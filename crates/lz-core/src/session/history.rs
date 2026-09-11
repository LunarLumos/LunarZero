//! History helpers: compaction-aware filtering, "latest" lookups, and the
//! projection of stored messages/parts into neutral LLM messages
//!.

use lz_schema::session::*;

use crate::llm::types::*;

pub const SYNTHETIC_ATTACHMENT_PROMPT: &str = "Attached media from tool result:";

fn is_after(info: &Message, other: Option<&Message>) -> bool {
    match other {
        None => true,
        Some(o) => {
            if info.created() != o.created() {
                info.created() > o.created()
            } else {
                info.id() > o.id()
            }
        }
    }
}

pub struct Latest<'a> {
    pub user: Option<&'a UserMessage>,
    pub assistant: Option<&'a AssistantMessage>,
    pub finished: Option<&'a AssistantMessage>,
    /// Pending `compaction` / `subtask` parts newer than the last finished turn.
    pub tasks: Vec<&'a Part>,
}

pub fn latest(msgs: &[MessageWithParts]) -> Latest<'_> {
    let mut user: Option<&Message> = None;
    let mut assistant: Option<&Message> = None;
    let mut finished: Option<&Message> = None;
    for m in msgs {
        match &m.info {
            Message::User(_) if is_after(&m.info, user) => user = Some(&m.info),
            Message::Assistant(a) => {
                if is_after(&m.info, assistant) {
                    assistant = Some(&m.info);
                }
                if a.finish.is_some() && is_after(&m.info, finished) {
                    finished = Some(&m.info);
                }
            }
            _ => {}
        }
    }
    let tasks = msgs
        .iter()
        .filter(|m| finished.is_none_or(|f| is_after(&m.info, Some(f))))
        .flat_map(|m| {
            m.parts
                .iter()
                .filter(|p| matches!(p.kind, PartKind::Compaction { .. } | PartKind::Subtask { .. }))
        })
        .collect();
    Latest {
        user: user.and_then(Message::as_user),
        assistant: assistant.and_then(Message::as_assistant),
        finished: finished.and_then(Message::as_assistant),
        tasks,
    }
}

fn compaction_part(m: &MessageWithParts) -> Option<(&bool, &Option<String>)> {
    m.parts.iter().find_map(|p| match &p.kind {
        PartKind::Compaction {
            auto, tail_start_id, ..
        } => Some((auto, tail_start_id)),
        _ => None,
    })
}

/// Keep only history after the last completed compaction, re-ordered for the
/// model as `[compaction-user, summary, ...retained tail, later messages]`.
pub fn filter_compacted(msgs: Vec<MessageWithParts>) -> Vec<MessageWithParts> {
    let mut result: Vec<MessageWithParts> = Vec::new();
    let mut completed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut retain: Option<String> = None;
    for m in msgs.into_iter().rev() {
        let id = m.info.id().to_string();
        let is_user = matches!(m.info, Message::User(_));
        let comp = compaction_part(&m).map(|(_, tail)| tail.clone());
        let summary_parent = match &m.info {
            Message::Assistant(a) if a.summary == Some(true) && a.finish.is_some() && a.error.is_none() => {
                Some(a.parent_id.clone())
            }
            _ => None,
        };
        result.push(m);
        if let Some(r) = &retain {
            if &id == r {
                break;
            }
            continue;
        }
        if is_user && completed.contains(&id) {
            let Some(tail) = comp else { continue };
            let Some(tail) = tail else { break };
            if id == tail {
                break;
            }
            retain = Some(tail);
            continue;
        }
        if let Some(p) = summary_parent {
            completed.insert(p);
        }
    }
    result.reverse();

    let compaction_index = result.iter().rposition(|m| {
        matches!(m.info, Message::User(_)) && compaction_part(m).is_some_and(|(_, tail)| tail.is_some())
    });
    let Some(ci) = compaction_index else { return result };
    let tail_id = compaction_part(&result[ci]).and_then(|(_, t)| t.clone());
    let compaction_id = result[ci].info.id().to_string();
    let summary_index = result.iter().enumerate().position(|(i, m)| {
        i > ci && matches!(&m.info, Message::Assistant(a) if a.summary == Some(true) && a.parent_id == compaction_id)
    });
    let tail_index = tail_id.and_then(|t| result.iter().position(|m| m.info.id() == t));
    match (tail_index, summary_index) {
        (Some(ti), Some(si)) if ti < ci && si > ci => {
            let mut out = Vec::with_capacity(result.len());
            let mut it: Vec<Option<MessageWithParts>> = result.into_iter().map(Some).collect();
            for slot in &mut it[ci..=si] {
                out.push(slot.take().unwrap());
            }
            for slot in &mut it[ti..ci] {
                out.push(slot.take().unwrap());
            }
            for item in it.into_iter().skip(si + 1).flatten() {
                out.push(item);
            }
            out
        }
        _ => result,
    }
}

fn data_url_payload(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.split(';').next().unwrap_or("").to_string();
    Some((mime, data.to_string()))
}

fn is_media(mime: &str) -> bool {
    mime.starts_with("image/") || mime == "application/pdf"
}

fn truncate_tool_output(s: &str, max: Option<usize>) -> String {
    match max {
        Some(n) if s.len() > n => {
            let cut = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
            format!("{}\n[output truncated]", &s[..cut])
        }
        _ => s.to_string(),
    }
}

pub struct ToModelOptions {
    pub current_model: (String, String),
    /// Providers that accept media inside tool results (OpenAI-compatible: no).
    pub media_in_tool_results: bool,
    pub tool_output_max_chars: Option<usize>,
}

/// Project stored history into neutral LLM messages.
pub fn to_llm_messages(msgs: &[MessageWithParts], opts: &ToModelOptions) -> Vec<LlmMessage> {
    let mut out: Vec<LlmMessage> = Vec::new();
    for m in msgs {
        if m.parts.is_empty() {
            continue;
        }
        match &m.info {
            Message::User(_) => {
                let mut content = Vec::new();
                for p in &m.parts {
                    match &p.kind {
                        PartKind::Text { text, ignored, .. } if !ignored && !text.is_empty() => {
                            content.push(ContentPart::Text { text: text.clone() });
                        }
                        PartKind::File {
                            mime, url, filename, ..
                        } if mime != "text/plain" && mime != "application/x-directory" => {
                            if let Some((_, data)) = data_url_payload(url) {
                                content.push(ContentPart::Media {
                                    mime: mime.clone(),
                                    data,
                                    filename: filename.clone(),
                                });
                            }
                        }
                        PartKind::Compaction { .. } => content.push(ContentPart::Text {
                            text: "What did we do so far?".into(),
                        }),
                        PartKind::Subtask { .. } => content.push(ContentPart::Text {
                            text: "The following tool was executed by the user".into(),
                        }),
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    out.push(LlmMessage::User { content });
                }
            }
            Message::Assistant(a) => {
                let different_model = (a.provider_id.as_str(), a.model_id.as_str())
                    != (opts.current_model.0.as_str(), opts.current_model.1.as_str());
                // skip errored turns unless it was an abort with real content
                if let Some(err) = &a.error {
                    let has_content = m
                        .parts
                        .iter()
                        .any(|p| !matches!(p.kind, PartKind::StepStart { .. } | PartKind::Reasoning { .. }));
                    if !(matches!(err, MessageError::Aborted { .. }) && has_content) {
                        continue;
                    }
                }
                // split into steps on step-start; each step = assistant msg (+ tool msg)
                let mut assistant: Vec<ContentPart> = Vec::new();
                let mut results: Vec<ContentPart> = Vec::new();
                let mut media: Vec<(String, String, Option<String>)> = Vec::new();
                let flush = |assistant: &mut Vec<ContentPart>,
                             results: &mut Vec<ContentPart>,
                             out: &mut Vec<LlmMessage>| {
                    if !assistant.is_empty() {
                        out.push(LlmMessage::Assistant {
                            content: std::mem::take(assistant),
                        });
                    }
                    if !results.is_empty() {
                        out.push(LlmMessage::Tool {
                            content: std::mem::take(results),
                        });
                    }
                };
                for p in &m.parts {
                    match &p.kind {
                        PartKind::StepStart { .. } => flush(&mut assistant, &mut results, &mut out),
                        PartKind::Text { text, .. } => {
                            if !text.is_empty() {
                                assistant.push(ContentPart::Text { text: text.clone() });
                            }
                        }
                        PartKind::Reasoning { text, metadata, .. } => {
                            if different_model {
                                if !text.trim().is_empty() {
                                    assistant.push(ContentPart::Text { text: text.clone() });
                                }
                            } else {
                                assistant.push(ContentPart::Reasoning {
                                    text: text.clone(),
                                    metadata: metadata.clone(),
                                });
                            }
                        }
                        PartKind::Tool {
                            call_id, tool, state, ..
                        } => {
                            assistant.push(ContentPart::ToolCall {
                                id: call_id.clone(),
                                name: tool.clone(),
                                input: state.input().clone(),
                            });
                            let result = match state {
                                ToolState::Completed {
                                    output,
                                    time,
                                    attachments,
                                    ..
                                } => {
                                    let text = if time.compacted.is_some() {
                                        "[Old tool result content cleared]".to_string()
                                    } else {
                                        truncate_tool_output(output, opts.tool_output_max_chars)
                                    };
                                    let mut items = vec![ToolContentItem::Text { text: text.clone() }];
                                    let mut had_inline = false;
                                    if time.compacted.is_none() {
                                        for att in attachments.iter().flatten() {
                                            if !is_media(&att.mime) {
                                                continue;
                                            }
                                            let Some((_, data)) = data_url_payload(&att.url) else {
                                                continue;
                                            };
                                            if opts.media_in_tool_results {
                                                items.push(ToolContentItem::File {
                                                    mime: att.mime.clone(),
                                                    data,
                                                    name: att.filename.clone(),
                                                });
                                                had_inline = true;
                                            } else {
                                                media.push((att.mime.clone(), data, att.filename.clone()));
                                            }
                                        }
                                    }
                                    if had_inline {
                                        ToolResultContent::Content { items }
                                    } else {
                                        ToolResultContent::Text { text }
                                    }
                                }
                                ToolState::Error { error, metadata, .. } => {
                                    let interrupted_output = metadata
                                        .as_ref()
                                        .filter(|m| m["interrupted"] == true)
                                        .and_then(|m| m["output"].as_str().map(str::to_string));
                                    match interrupted_output {
                                        Some(o) => ToolResultContent::Text { text: o },
                                        None => ToolResultContent::Error { text: error.clone() },
                                    }
                                }
                                ToolState::Pending { .. } | ToolState::Running { .. } => {
                                    ToolResultContent::Error {
                                        text: "[Tool execution was interrupted]".into(),
                                    }
                                }
                            };
                            results.push(ContentPart::ToolResult {
                                id: call_id.clone(),
                                name: tool.clone(),
                                result,
                            });
                        }
                        _ => {}
                    }
                }
                flush(&mut assistant, &mut results, &mut out);
                if !media.is_empty() {
                    let mut content = vec![ContentPart::Text {
                        text: SYNTHETIC_ATTACHMENT_PROMPT.into(),
                    }];
                    for (mime, data, filename) in media {
                        content.push(ContentPart::Media { mime, data, filename });
                    }
                    out.push(LlmMessage::User { content });
                }
            }
        }
    }
    out
}

/// Rough token estimate (chars / 4).
pub fn estimate_tokens(messages: &[LlmMessage]) -> u64 {
    let s = serde_json::to_string(messages).unwrap_or_default();
    (s.len() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, created: u64) -> MessageWithParts {
        MessageWithParts {
            info: Message::User(UserMessage {
                id: id.into(),
                session_id: "s".into(),
                time: UserTime { created },
                format: None,
                summary: None,
                agent: "build".into(),
                model: ModelRef {
                    provider_id: "p".into(),
                    model_id: "m".into(),
                    variant: None,
                },
                system: None,
                tools: None,
            }),
            parts: vec![Part {
                id: format!("prt_{id}"),
                session_id: "s".into(),
                message_id: id.into(),
                kind: PartKind::Text {
                    text: "hi".into(),
                    synthetic: false,
                    ignored: false,
                    time: None,
                    metadata: None,
                },
            }],
        }
    }

    fn assistant(id: &str, parent: &str, created: u64, finish: Option<&str>) -> MessageWithParts {
        MessageWithParts {
            info: Message::Assistant(AssistantMessage {
                id: id.into(),
                session_id: "s".into(),
                time: AssistantTime {
                    created,
                    completed: None,
                },
                error: None,
                parent_id: parent.into(),
                model_id: "m".into(),
                provider_id: "p".into(),
                mode: "build".into(),
                agent: "build".into(),
                path: MessagePath {
                    cwd: "/".into(),
                    root: "/".into(),
                },
                summary: None,
                cost: 0.0,
                tokens: Tokens::default(),
                structured: None,
                variant: None,
                finish: finish.map(str::to_string),
            }),
            parts: vec![Part {
                id: format!("prt_{id}"),
                session_id: "s".into(),
                message_id: id.into(),
                kind: PartKind::Text {
                    text: "yo".into(),
                    synthetic: false,
                    ignored: false,
                    time: None,
                    metadata: None,
                },
            }],
        }
    }

    #[test]
    fn latest_picks_newest() {
        let msgs = vec![
            user("msg_1", 1),
            assistant("msg_2", "msg_1", 2, Some("stop")),
            user("msg_3", 3),
        ];
        let l = latest(&msgs);
        assert_eq!(l.user.unwrap().id, "msg_3");
        assert_eq!(l.assistant.unwrap().id, "msg_2");
        assert_eq!(l.finished.unwrap().id, "msg_2");
    }

    #[test]
    fn projects_to_llm_messages() {
        let msgs = vec![user("msg_1", 1), assistant("msg_2", "msg_1", 2, Some("stop"))];
        let out = to_llm_messages(
            &msgs,
            &ToModelOptions {
                current_model: ("p".into(), "m".into()),
                media_in_tool_results: false,
                tool_output_max_chars: None,
            },
        );
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[1], LlmMessage::Assistant { content } if content.len() == 1));
    }
}
