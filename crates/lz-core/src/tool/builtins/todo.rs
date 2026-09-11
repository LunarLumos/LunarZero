//! `todowrite` — replace the session's todo list.

use std::borrow::Cow;

use async_trait::async_trait;
use lz_schema::session::Todo;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

#[derive(Deserialize)]
struct Args {
    todos: Vec<Todo>,
}

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn id(&self) -> &'static str {
        "todowrite"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("todowrite"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Full list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Task" },
                            "status": { "type": "string", "description": "pending | in_progress | completed | cancelled" },
                            "priority": { "type": "string", "description": "high | medium | low" }
                        },
                        "required": ["content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        ctx.ask(
            "todowrite",
            vec!["*".into()],
            vec!["*".into()],
            Default::default(),
        )
        .await?;
        ctx.engine
            .sessions
            .set_todos(&ctx.session_id, args.todos.clone())
            .await
            .map_err(ToolError::other)?;
        let open = args.todos.iter().filter(|t| t.status != "completed").count();
        Ok(ToolResult {
            title: format!("{open} todos"),
            output: serde_json::to_string_pretty(&args.todos).unwrap_or_default(),
            metadata: json!({ "todos": args.todos }),
            attachments: Vec::new(),
        })
    }
}
