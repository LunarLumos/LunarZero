//! Shared, dependency-light types for LunarZero: identifiers, the
//! session/message/part model, bus events, configuration, permission rules,
//! and the `EngineApi` contract.

pub mod api;
pub mod config;
pub mod event;
pub mod ids;
pub mod permission;
pub mod session;

pub use api::{ApiError, ApiResult, EngineApi};
pub use event::Event;
