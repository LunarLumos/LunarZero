//! `question` — ask the user one or more multiple-choice questions.

use std::borrow::Cow;

use async_trait::async_trait;
use lz_schema::session::{QuestionInfo, ToolRef};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

#[derive(Deserialize)]
struct Args {
    questions: Vec<QuestionInfo>,
}

pub struct QuestionTool;

#[async_trait]
impl Tool for QuestionTool {
    fn id(&self) -> &'static str {
        "question"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("question"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to ask",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "Complete question" },
                            "header": { "type": "string", "description": "Very short label (max 30 chars)" },
                            "options": {
                                "type": "array",
                                "description": "Available choices",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "Display text (1-5 words, concise)" },
                                        "description": { "type": "string", "description": "Explanation of choice" }
                                    },
                                    "required": ["label", "description"]
                                }
                            },
                            "multiple": { "type": "boolean", "description": "Allow selecting multiple choices" }
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let answers = ctx
            .engine
            .questions
            .ask(
                &ctx.engine.bus,
                &ctx.session_id,
                args.questions.clone(),
                Some(ToolRef {
                    message_id: ctx.message_id.clone(),
                    call_id: ctx.call_id.clone(),
                }),
            )
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let formatted = args
            .questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let a = answers
                    .get(i)
                    .filter(|a| !a.is_empty())
                    .map(|a| a.join(", "))
                    .unwrap_or_else(|| "Unanswered".into());
                format!("\"{}\"=\"{}\"", q.question, a)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let n = args.questions.len();
        Ok(ToolResult {
            title: format!("Asked {n} question{}", if n > 1 { "s" } else { "" }),
            output: format!("Answers from the user: {formatted}. Continue accordingly."),
            metadata: json!({ "answers": answers }),
            attachments: Vec::new(),
        })
    }
}
