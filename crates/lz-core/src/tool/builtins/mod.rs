//! Built-in tools.

pub mod apply_patch;
pub mod bash;
pub mod edit;
pub mod external_directory;
pub mod invalid;
pub mod question;
pub mod read;
pub mod search;
pub mod skill;
pub mod symbol;
pub mod task;
pub mod todo;
pub mod web;

use std::sync::Arc;

use super::Tool;

/// Built-in tools in registration order.
pub fn all() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(invalid::InvalidTool),
        Arc::new(question::QuestionTool),
        Arc::new(bash::BashTool),
        Arc::new(read::ReadTool),
        Arc::new(search::GlobTool),
        Arc::new(search::GrepTool),
        Arc::new(symbol::SymbolTool),
        Arc::new(edit::EditTool),
        Arc::new(edit::WriteTool),
        Arc::new(task::TaskTool),
        Arc::new(web::WebFetchTool),
        Arc::new(todo::TodoWriteTool),
        Arc::new(skill::SkillTool),
        Arc::new(apply_patch::ApplyPatchTool),
    ]
}
