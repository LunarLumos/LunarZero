//! Target for malformed tool calls: echoes the error back to the model.

use std::borrow::Cow;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult};

pub struct InvalidTool;

#[async_trait]
impl Tool for InvalidTool {
    fn id(&self) -> &'static str {
        "invalid"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed("Do not use this tool. It is only used internally to report malformed tool calls.")
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": { "tool": { "type": "string" }, "error": { "type": "string" } }, "required": ["tool", "error"] })
    }
    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let tool = args["tool"].as_str().unwrap_or("unknown");
        let error = args["error"].as_str().unwrap_or("invalid arguments");
        Err(ToolError::Invalid(format!(
            "The arguments provided to the {tool} tool are invalid: {error}"
        )))
    }
}
