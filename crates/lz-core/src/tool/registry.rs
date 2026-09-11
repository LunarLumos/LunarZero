//! Which tools exist, in which order, and which ones a given agent + model may
//! use.

use std::sync::Arc;

use lz_schema::permission::{Action, Ruleset};

use super::Tool;
use crate::llm::ToolDef;
use crate::permission;
use crate::provider::Model;

pub struct ToolRegistry {
    builtin: Vec<Arc<dyn Tool>>,
    /// Extra tools (MCP, custom) appended after builtins.
    extra: std::sync::RwLock<Vec<Arc<dyn Tool>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// A tool visible to the model for one turn.
pub struct ResolvedTool {
    pub tool: Arc<dyn Tool>,
    pub def: ToolDef,
}

impl ToolRegistry {
    pub fn new(builtin: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            builtin,
            extra: std::sync::RwLock::new(Vec::new()),
        }
    }

    pub fn set_extra(&self, tools: Vec<Arc<dyn Tool>>) {
        *self.extra.write().unwrap_or_else(|e| e.into_inner()) = tools;
    }

    pub fn all(&self) -> Vec<Arc<dyn Tool>> {
        let mut v = self.builtin.clone();
        v.extend(
            self.extra
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned(),
        );
        v
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.all().into_iter().find(|t| t.id() == id)
    }

    /// Tools for this turn: filtered by model family, denied permissions, and
    /// agent restrictions.
    pub fn resolve(
        &self,
        model: &Model,
        ruleset: &Ruleset,
        tools_enabled: bool,
        agents: &crate::agent::Agents,
        agent: &crate::agent::Agent,
    ) -> Vec<ResolvedTool> {
        if !tools_enabled || !model.tool_call {
            return Vec::new();
        }
        let use_patch = model.id.contains("gpt-") && !model.id.contains("oss") && !model.id.contains("gpt-4");
        let mut out = Vec::new();
        for tool in self.all() {
            let id = tool.id();
            if id == "apply_patch" && !use_patch {
                continue;
            }
            if (id == "edit" || id == "write") && use_patch {
                continue;
            }
            // permission key: write/edit/apply_patch → edit
            let key = match id {
                "write" | "apply_patch" => "edit",
                other => other,
            };
            if permission::evaluate(key, "*", &[ruleset]).action == Action::Deny {
                continue;
            }
            // `invalid` only exists to receive malformed calls; the model never needs to see it
            if id == "invalid" {
                continue;
            }
            let schema = crate::provider::transform::compact_schema(
                &crate::provider::transform::sanitize_schema(&tool.parameters()),
            );
            let description = if id == "task" {
                super::builtins::task::describe(agents, agent)
            } else {
                tool.description().into_owned()
            };
            out.push(ResolvedTool {
                def: ToolDef {
                    name: id.to_string(),
                    description,
                    input_schema: schema,
                },
                tool,
            });
        }
        out
    }
}
