//! Configuration schema — accepts every key of opencode's `opencode.json`
//! (V1) so existing configs work unchanged. Key order inside `permission`
//! objects is preserved via `serde_json::Map` (preserve_order feature).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::permission::Action;

/// A permission entry: either a single action or `{pattern: action}` map.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum PermissionRuleConfig {
    Action(Action),
    Patterns(Map<String, Value>),
}

/// `permission` value: a bare action (`"ask"`) means `{"*": "ask"}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum PermissionConfig {
    Action(Action),
    Object(Map<String, Value>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    Subagent,
    Primary,
    All,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct AgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<AgentMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(rename = "maxSteps", default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionConfig>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModelLimit {
    pub context: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    pub output: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModelModalities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModelProviderHint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interleaved: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<ModelLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<ModelModalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ModelProviderHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whitelist: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacklist: Option<Vec<String>>,
    /// `apiKey`, `baseURL`, `headers`, `timeout`, `headerTimeout`, `chunkTimeout`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<String, ModelConfig>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct McpOAuthConfig {
    #[serde(rename = "clientId", default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(rename = "clientSecret", default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(rename = "callbackPort", default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    #[serde(rename = "redirectUri", default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum McpOAuth {
    Disabled(bool),
    Config(McpOAuthConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServerConfig {
    Local {
        command: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Remote {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth: Option<McpOAuth>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
}

/// `mcp.<name>`: full server config or just `{enabled: false}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum McpEntry {
    Server(McpServerConfig),
    Toggle { enabled: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum LspEntry {
    Disabled {
        disabled: bool,
    },
    Server {
        command: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extensions: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<Map<String, Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initialization: Option<Map<String, Value>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum LspConfig {
    Enabled(bool),
    Servers(Map<String, Value>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct FormatterEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum FormatterConfig {
    Enabled(bool),
    Entries(Map<String, Value>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct CommandConfig {
    pub template: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtask: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct SkillsConfig {
    /// Ship the built-in skills (debugging, testing, git-workflow, …). Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urls: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cors: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ImageAttachmentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resize: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_base64_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct AttachmentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageAttachmentConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ToolOutputConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

/// `smart`: choose skills, MCP servers and model strategy from the prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct SmartConfig {
    /// Describe only relevant skills in the prompt (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<bool>,
    /// Attach a clearly matching skill's content instead of waiting for a tool call (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_skill: Option<bool>,
    /// Send an MCP server's tools only when the prompt or session relates to it (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<bool>,
    /// MCP servers whose tools are always sent, regardless of relevance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_always: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ProjectMapConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
}

/// `pool`: the built-in router over free-tier providers (`lunar/auto`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct PoolConfig {
    /// Master switch for the `lunar` provider (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Default strategy for `lunar/auto`: `auto` | `fast` | `smart`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Fail over to another pool model when a pool model is rate limited (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<bool>,
    /// When a model outside the pool fails hard (quota, outage, bad key), continue
    /// on the best pool model instead of erroring (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rescue: Option<bool>,
    /// Keep a session on the same routed model for this long (default 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sticky_minutes: Option<u64>,
    /// `provider/model` or `provider/*` patterns to leave out of the pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Extra `provider/model` ids to treat as pool members (uses catalog limits when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct CompactionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_recent_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved: Option<u64>,
    /// Old tool outputs are dropped from the context once more than this many
    /// tokens of newer outputs exist (default 24000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_after_tokens: Option<u64>,
    /// …and only when at least this many tokens would be freed (default 6000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prune_min_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct WatcherConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ExperimentalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_paste_summary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_loop_on_deny: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_tools: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Top-level `lunarzero.json` / `opencode.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct Config {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(rename = "logLevel", default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Map<String, Value>>,
    /// Deprecated alias of `agent` (entries become `mode: "primary"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<BTreeMap<String, ProviderConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<BTreeMap<String, McpEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp: Option<LspConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatter: Option<FormatterConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<BTreeMap<String, CommandConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watcher: Option<WatcherConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<AttachmentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<ToolOutputConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolConfig>,
    /// Prompt-aware selection of skills, MCP servers and routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<SmartConfig>,
    /// Compact repository map in the system prompt (default on, ~1.5k chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_map: Option<ProjectMapConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<ExperimentalConfig>,
    /// Accepted for compatibility, ignored: `share`, `autoshare`, `autoupdate`,
    /// `plugin`, `references`, `reference`, `enterprise`, `layout`, `theme`, `keybinds`, …
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Config {
    pub fn tool_output_max_lines(&self) -> usize {
        self.tool_output
            .as_ref()
            .and_then(|t| t.max_lines)
            .unwrap_or(2000)
    }
    pub fn tool_output_max_bytes(&self) -> usize {
        self.tool_output
            .as_ref()
            .and_then(|t| t.max_bytes)
            .unwrap_or(50 * 1024)
    }
    pub fn subagent_depth(&self) -> u32 {
        self.subagent_depth.unwrap_or(1)
    }
    pub fn compaction_auto(&self) -> bool {
        self.compaction.as_ref().and_then(|c| c.auto).unwrap_or(true)
    }
    pub fn compaction_prune(&self) -> bool {
        self.compaction.as_ref().and_then(|c| c.prune).unwrap_or(true)
    }
    pub fn prune_after_tokens(&self) -> u64 {
        self.compaction
            .as_ref()
            .and_then(|c| c.prune_after_tokens)
            .unwrap_or(24_000)
    }
    pub fn prune_min_tokens(&self) -> u64 {
        self.compaction
            .as_ref()
            .and_then(|c| c.prune_min_tokens)
            .unwrap_or(6_000)
    }
    pub fn snapshot_enabled(&self) -> bool {
        self.snapshot.unwrap_or(true)
    }
}

// ───────────────────────────── tui.json ─────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct AttentionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct PromptConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct TuiConfig {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Action name → chord string(s), `"none"`/`false` to unbind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keybinds: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<AttentionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_speed: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_style_config() {
        let json = r#"{
          "$schema": "https://opencode.ai/config.json",
          "model": "openai/gpt-4o",
          "permission": { "bash": { "git *": "allow", "rm *": "deny" }, "edit": "ask" },
          "mcp": { "fs": { "type": "local", "command": ["npx", "server"] }, "off": { "enabled": false } },
          "lsp": true,
          "share": "manual",
          "agent": { "plan": { "model": "openai/gpt-4o-mini" } }
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("openai/gpt-4o"));
        match cfg.permission.unwrap() {
            PermissionConfig::Object(m) => {
                let keys: Vec<_> = m.keys().collect();
                assert_eq!(keys, vec!["bash", "edit"]);
            }
            _ => panic!(),
        }
        assert!(matches!(
            cfg.mcp.as_ref().unwrap()["off"],
            McpEntry::Toggle { enabled: false }
        ));
        assert!(cfg.extra.contains_key("share"));
    }
}
