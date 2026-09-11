//! LunarZero terminal UI (ratatui). Depends only on `lz-schema` and talks to
//! the engine through the `EngineApi` trait.

pub mod app;
pub mod clipboard;
pub mod dialog;
pub mod editor;
pub mod event;
pub mod keymap;
pub mod kv;
pub mod store;
pub mod terminal;
pub mod theme;
pub mod widgets;

pub use app::TuiOptions;

use std::sync::Arc;

use lz_schema::api::EngineApi;

/// Run the TUI until the user exits.
pub async fn run(api: Arc<dyn EngineApi>, opts: TuiOptions) -> anyhow::Result<()> {
    event::run(api, opts).await
}
