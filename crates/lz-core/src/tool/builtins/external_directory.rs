//! Ask before touching paths outside the project worktree.

use std::path::Path;

use crate::tool::{ToolCtx, ToolError};

pub fn contains(worktree: &Path, directory: &Path, target: &Path) -> bool {
    target.starts_with(worktree) || target.starts_with(directory)
}

pub async fn assert(ctx: &ToolCtx, target: &Path, is_directory: bool) -> Result<bool, ToolError> {
    if contains(ctx.worktree(), ctx.directory(), target) {
        return Ok(false);
    }
    let dir = if is_directory {
        target.to_path_buf()
    } else {
        target.parent().unwrap_or(target).to_path_buf()
    };
    let glob = format!("{}/*", dir.display()).replace('\\', "/");
    ctx.ask(
        "external_directory",
        vec![glob.clone()],
        vec![glob],
        serde_json::json!({ "filepath": target.display().to_string(), "parentDir": dir.display().to_string() })
            .as_object()
            .cloned()
            .unwrap_or_default(),
    )
    .await?;
    Ok(true)
}
