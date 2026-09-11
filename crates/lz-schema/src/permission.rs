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
