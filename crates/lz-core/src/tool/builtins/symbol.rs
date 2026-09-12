//! `symbol` — where a function/type/class is defined and which files use it,
//! from the tree-sitter index. Cheaper and more precise than grep for names.

use std::borrow::Cow;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

#[derive(Deserialize)]
struct Args {
    name: String,
}

pub struct SymbolTool;

#[async_trait]
impl Tool for SymbolTool {
    fn id(&self) -> &'static str {
        "symbol"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(
            "Find where a function, type, class, method or constant is defined (file:line + signature) and which files reference it. Rust, Python, JS/TS, Go. Use before grep when you know the name.",
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Exact symbol name (case-insensitive fallback)" }
            },
            "required": ["name"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let name = args.name.trim().to_string();
        if name.is_empty() {
            return Err(ToolError::Invalid("name is required".into()));
        }
        let index = ctx.engine.index.clone();
        let (defs, refs) = tokio::task::spawn_blocking({
            let name = name.clone();
            move || {
                index.refresh();
                (index.definitions(&name), index.references(&name))
            }
        })
        .await
        .map_err(ToolError::other)?;
        let mut out = String::new();
        if defs.is_empty() {
            // the language server knows languages the index doesn't (and macros, generated code)
            if let Some(l) = ctx.engine.lsp.lookup(&name, &ctx.engine.project.worktree).await {
                out.push_str(&format!("Definitions of `{name}` (via {}):\n", l.server));
                for (file, line, kind) in &l.definitions {
                    out.push_str(&format!("  {kind} {file}:{line}\n"));
                }
                if !l.references.is_empty() {
                    out.push_str(&format!("Referenced in {} file(s):\n", l.references.len()));
                    for f in l.references.iter().take(30) {
                        out.push_str(&format!("  {f}\n"));
                    }
                }
                return Ok(ToolResult {
                    title: format!(
                        "{name} — {} def, {} refs (lsp)",
                        l.definitions.len(),
                        l.references.len()
                    ),
                    output: out,
                    metadata: json!({ "definitions": l.definitions, "references": l.references, "source": "lsp" }),
                    attachments: Vec::new(),
                });
            }
            out.push_str(&format!(
                "No definition of `{name}` in the index (Rust, Python, JS/TS, Go, Java, C/C++, Ruby) or from a language server. Try grep.\n"
            ));
        } else {
            out.push_str(&format!("Definitions of `{name}`:\n"));
            for d in &defs {
                out.push_str(&format!(
                    "  {} {}:{}-{}  {}\n",
                    d.kind, d.file, d.line, d.end_line, d.signature
                ));
            }
        }
        if !refs.is_empty() {
            out.push_str(&format!("Referenced in {} file(s):\n", refs.len()));
            for f in refs.iter().take(30) {
                out.push_str(&format!("  {f}\n"));
            }
            if refs.len() > 30 {
                out.push_str(&format!("  … {} more\n", refs.len() - 30));
            }
        }
        Ok(ToolResult {
            title: format!("{name} — {} def, {} refs", defs.len(), refs.len()),
            output: out,
            metadata: json!({ "definitions": defs, "references": refs }),
            attachments: Vec::new(),
        })
    }
}
