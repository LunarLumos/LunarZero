//! Curated MCP servers and skills that can be installed by alias
//! (`lz mcp install github`, `lz skill install pdf`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendedMcp {
    pub alias: String,
    pub source: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub description: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendedSkill {
    pub alias: String,
    pub source: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommended {
    pub mcp: Vec<RecommendedMcp>,
    pub skills: Vec<RecommendedSkill>,
}

static EMBEDDED: &str = include_str!("../../../assets/recommended.json");

pub fn catalog() -> &'static Recommended {
    static R: std::sync::LazyLock<Recommended> =
        std::sync::LazyLock::new(|| serde_json::from_str(EMBEDDED).expect("embedded recommended catalog"));
    &R
}

pub fn mcp(alias: &str) -> Option<&'static RecommendedMcp> {
    catalog().mcp.iter().find(|m| m.alias == alias)
}

pub fn skill(alias: &str) -> Option<&'static RecommendedSkill> {
    catalog().skills.iter().find(|s| s.alias == alias)
}
