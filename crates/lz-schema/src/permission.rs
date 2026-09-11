//! Permission rules. Ordered list; evaluation is "last matching rule wins".

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Rule {
    pub permission: String,
    pub pattern: String,
    pub action: Action,
}

impl Rule {
    pub fn new(permission: impl Into<String>, pattern: impl Into<String>, action: Action) -> Self {
        Self {
            permission: permission.into(),
            pattern: pattern.into(),
            action,
        }
    }
}

pub type Ruleset = Vec<Rule>;

/// How much the agent may do without asking — switched live from the UI
/// (`shift+tab` in the TUI). Layered over the agent's own rules; the
/// session's explicit rules still win.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// Every edit and command outside the allow-list asks first.
    #[default]
    Manual,
    /// File edits, writes and patches go through; commands still ask.
    AcceptEdits,
    /// Nothing asks. Same as `--auto`.
    Auto,
    /// Read-only planning: the `plan` agent, no edits.
    Plan,
}

impl PermissionMode {
    pub const ALL: [PermissionMode; 4] = [
        PermissionMode::Manual,
        PermissionMode::AcceptEdits,
        PermissionMode::Auto,
        PermissionMode::Plan,
    ];

    pub fn id(self) -> &'static str {
        match self {
            PermissionMode::Manual => "manual",
            PermissionMode::AcceptEdits => "accept-edits",
            PermissionMode::Auto => "auto",
            PermissionMode::Plan => "plan",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Manual => "manual",
            PermissionMode::AcceptEdits => "accept edits",
            PermissionMode::Auto => "auto",
            PermissionMode::Plan => "plan",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            PermissionMode::Manual => "ask before edits and commands",
            PermissionMode::AcceptEdits => "edits go through, commands still ask",
            PermissionMode::Auto => "never ask (like --auto)",
            PermissionMode::Plan => "read-only: research and plan, no edits",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "manual" | "ask" | "default" | "normal" => Some(PermissionMode::Manual),
            "accept-edits" | "accept" | "edits" | "acceptedits" | "accept edits" => {
                Some(PermissionMode::AcceptEdits)
            }
            "auto" | "yolo" | "bypass" | "all" => Some(PermissionMode::Auto),
            "plan" => Some(PermissionMode::Plan),
            _ => None,
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// Rules layered between the agent's and the user's own config (which is
    /// re-applied after them, so an explicit allow-list still wins).
    pub fn rules(self) -> Ruleset {
        match self {
            PermissionMode::Manual => vec![
                Rule::new("edit", "*", Action::Ask),
                Rule::new("bash", "*", Action::Ask),
            ],
            PermissionMode::AcceptEdits => vec![
                Rule::new("edit", "*", Action::Allow),
                Rule::new("bash", "*", Action::Ask),
            ],
            PermissionMode::Auto => vec![Rule::new("*", "*", Action::Allow)],
            PermissionMode::Plan => vec![
                Rule::new("edit", "*", Action::Deny),
                Rule::new("bash", "*", Action::Ask),
            ],
        }
    }

    /// Auto skips every prompt, so user `ask` rules are not re-applied on top.
    pub fn keeps_user_rules(self) -> bool {
        !matches!(self, PermissionMode::Auto)
    }

    /// Whether a pending request of this permission is covered by the mode.
    pub fn covers(self, permission: &str) -> bool {
        match self {
            PermissionMode::Auto => true,
            PermissionMode::AcceptEdits => permission == "edit",
            _ => false,
        }
    }

    /// The agent this mode pins the session to, if any.
    pub fn agent(self) -> Option<&'static str> {
        matches!(self, PermissionMode::Plan).then_some("plan")
    }
}
