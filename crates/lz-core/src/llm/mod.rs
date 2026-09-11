//! Provider-neutral LLM client: neutral types, wire protocols, SSE transport.

pub mod client;
pub mod protocol;
pub mod protocols;
pub mod sse;
pub mod types;

pub use client::{Endpoint, LlmClient};
pub use types::*;
