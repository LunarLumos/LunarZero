//! Synthetic reminders injected into the last user message: plan-mode
//! instructions for the `plan` agent and a "you're back in build" note after
//! switching from plan to build.

use std::sync::Arc;

use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;

use super::system::{PROMPT_BUILD_SWITCH, PROMPT_PLAN};
use crate::agent::Agent;
use crate::engine::Engine;

fn synthetic(session_id: &str, message_id: &str, text: &str) -> Part {
    Part {
        id: ids::ascending(Prefix::Part),
        session_id: session_id.into(),
        message_id: message_id.into(),
        kind: PartKind::Text {
            text: text.into(),
            synthetic: true,
            ignored: false,
            time: None,
            metadata: None,
        },
    }
}

pub async fn apply(
    _engine: &Engine,
    mut messages: Vec<MessageWithParts>,
    agent: &Arc<Agent>,
    _session: &SessionInfo,
) -> Vec<MessageWithParts> {
    let was_plan = messages
        .iter()
        .any(|m| matches!(&m.info, Message::Assistant(a) if a.agent == "plan"));
    let Some(user) = messages
        .iter_mut()
        .rev()
        .find(|m| matches!(m.info, Message::User(_)))
    else {
        return messages;
    };
    let (sid, mid) = (user.info.session_id().to_string(), user.info.id().to_string());
    if agent.name == "plan" {
        user.parts.push(synthetic(&sid, &mid, PROMPT_PLAN));
    }
    if was_plan && agent.name == "build" {
        user.parts.push(synthetic(&sid, &mid, PROMPT_BUILD_SWITCH));
    }
    messages
}
