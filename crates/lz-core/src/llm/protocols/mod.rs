pub mod anthropic_messages;
pub mod openai_chat;

use std::sync::Arc;

use super::protocol::Protocol;

/// Resolve a protocol implementation by id.
pub fn by_id(id: &str) -> Option<Arc<dyn Protocol>> {
    match id {
        "openai-chat" | "openai-compatible-chat" => Some(Arc::new(openai_chat::OpenAiChat)),
        "anthropic-messages" => Some(Arc::new(anthropic_messages::AnthropicMessages)),
        _ => None,
    }
}
