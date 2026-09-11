//! `Engine`: composition root for one project directory. Owns config,
//! storage, bus, providers, tools, permissions and the session runner, and
//! implements `EngineApi` for in-process clients.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use lz_schema::api::*;
use lz_schema::config::Config;
use lz_schema::permission::Ruleset;
use lz_schema::session::*;
use lz_schema::{EngineApi, Event};
use serde_json::{Map, Value};

use crate::agent::Agents;
use crate::bus::Bus;
use crate::config::Loaded;
use crate::paths::Paths;
use crate::permission::Permissions;
use crate::project::Project;
use crate::provider::Registry;
use crate::provider::auth::AuthStore;
use crate::session::runner::{self, SessionRunner};
use crate::session::status::StatusTracker;
use crate::session::{SessionService, system};
use crate::storage::Storage;
use crate::tool::ToolRegistry;

pub struct EngineOptions {
    pub directory: PathBuf,
    pub auto_approve: bool,
    /// Skip the network catalog refresh (fast startup / tests).
    pub offline: bool,
}

pub struct Engine {
    pub paths: Paths,
    pub project: Project,
    pub directory: PathBuf,
    pub storage: Storage,
    pub bus: Bus,
    pub sessions: SessionService,
    pub permissions: Arc<Permissions>,
    pub tools: ToolRegistry,
    pub runner: SessionRunner,
    pub status: StatusTracker,
    pub auth: AuthStore,
    pub questions: crate::question::Questions,
    pub snapshot: crate::snapshot::Snapshot,
    pub mcp: crate::mcp::McpManager,
    pub lsp: crate::lsp::LspManager,
    /// Free-pool router (usage ledger, cooldowns, `auto` resolution).
    pub router: crate::provider::router::Router,
    pub project_map: crate::project_map::Cache,
    config: ArcSwap<Config>,
    raw_config: ArcSwap<Map<String, Value>>,
    config_dirs: ArcSwap<Vec<PathBuf>>,
    registry: RwLock<Arc<Registry>>,
    agents: ArcSwap<Agents>,
    skills: ArcSwap<std::collections::BTreeMap<String, crate::skill::Skill>>,
    commands: ArcSwap<std::collections::BTreeMap<String, crate::command::Command>>,
    /// Nested AGENTS.md files already attached per assistant message.
    instruction_claims: dashmap::DashMap<String, std::collections::HashSet<PathBuf>>,
    pub started_at: std::time::Instant,
    weak_self: std::sync::Weak<Engine>,
}

impl Engine {
    pub async fn start(opts: EngineOptions) -> anyhow::Result<Arc<Self>> {
        let started_at = std::time::Instant::now();
        let paths = Paths::detect();
        paths.ensure()?;
        let directory = opts.directory.canonicalize().unwrap_or(opts.directory.clone());
        let project = crate::project::resolve(&directory);
        let loaded = crate::config::load(crate::config::LoadInput {
            paths: &paths,
            directory: &directory,
            worktree: &project.worktree,
        })?;
        let storage = Storage::open(&paths.db())?;
        let bus = Bus::new(Some(storage.clone()));
        {
            let (pid, wt, vcs) = (
                project.id.clone(),
                project.worktree.display().to_string(),
                project.vcs,
            );
            storage.with_blocking(move |c| crate::storage::repo::upsert_project(c, &pid, &wt, vcs))?;
        }
        let auth = AuthStore::new(&paths);
        let registry = crate::provider::load(&paths, &loaded.config, false).await;
        let permissions = Permissions::new(bus.clone(), storage.clone(), &project.id, opts.auto_approve);
        let agents = crate::agent::build(&loaded.raw, &paths, &project.worktree);
        let skills = crate::skill::discover(
            &paths,
            &loaded.config,
            &loaded.directories,
            &directory,
            &project.worktree,
        );
        let commands = crate::command::build(&loaded.raw, &project.worktree, &skills);
        let lsp_enabled = match &loaded.config.lsp {
            Some(lz_schema::config::LspConfig::Enabled(b)) => *b,
            Some(lz_schema::config::LspConfig::Servers(_)) => true,
            None => false,
        };
        let mcp_timeout = loaded
            .config
            .experimental
            .as_ref()
            .and_then(|e| e.mcp_timeout)
            .unwrap_or(30_000);
        let snapshot = crate::snapshot::Snapshot::new(
            &paths,
            &project.id,
            &project.worktree,
            project.vcs.is_some(),
            loaded.config.snapshot_enabled(),
        );
        let sessions = SessionService::new(
            storage.clone(),
            bus.clone(),
            &project.id,
            &directory.display().to_string(),
        );
        let tools = ToolRegistry::new(crate::tool::builtins::all());
        let _ = opts.offline;
        crate::tool::truncate::cleanup(&paths.tool_output());
        let Loaded {
            config,
            raw,
            directories,
            ..
        } = loaded;
        let quota_path = paths.state.join("quota.json");
        let engine = Arc::new_cyclic(|weak| Self {
            weak_self: weak.clone(),
            paths,
            project,
            directory,
            storage,
            bus,
            sessions,
            permissions,
            tools,
            runner: SessionRunner::default(),
            status: StatusTracker::default(),
            auth,
            questions: crate::question::Questions::default(),
            snapshot,
            mcp: crate::mcp::McpManager::new(std::time::Duration::from_millis(mcp_timeout)),
            lsp: crate::lsp::LspManager::new(lsp_enabled),
            router: crate::provider::router::Router::new(Some(quota_path)),
            project_map: crate::project_map::Cache::default(),
            config: ArcSwap::from_pointee(config),
            raw_config: ArcSwap::from_pointee(raw),
            config_dirs: ArcSwap::from_pointee(directories),
            registry: RwLock::new(Arc::new(registry)),
            agents: ArcSwap::from_pointee(agents),
            skills: ArcSwap::from_pointee(skills),
            commands: ArcSwap::from_pointee(commands),
            instruction_claims: dashmap::DashMap::new(),
            started_at,
        });
        tracing::info!(dir = %engine.directory.display(), project = engine.project.id, "engine started in {:?}", started_at.elapsed());
        if !opts.offline && engine.config().mcp.as_ref().is_some_and(|m| !m.is_empty()) {
            engine.reload_mcp().await;
        }
        if !opts.offline {
            engine.watch_config();
        }
        // skills.urls: install anything missing, in the background
        let urls = engine
            .config()
            .skills
            .as_ref()
            .and_then(|s| s.urls.clone())
            .unwrap_or_default();
        if !opts.offline && !urls.is_empty() {
            let e = engine.clone();
            tokio::spawn(async move {
                let results = crate::skill_install::sync_urls(&e.paths, &urls).await;
                let mut any = false;
                for r in results {
                    match r {
                        Ok(list) => {
                            any |= !list.is_empty();
                            for i in list {
                                tracing::info!(skill = i.name, "installed skill from skills.urls");
                            }
                        }
                        Err(err) => tracing::warn!("skills.urls: {err}"),
                    }
                }
                if any {
                    let _ = e.reload().await;
                }
            });
        }
        Ok(engine)
    }

    pub fn config(&self) -> Arc<Config> {
        self.config.load_full()
    }
    pub fn raw_config(&self) -> Arc<Map<String, Value>> {
        self.raw_config.load_full()
    }
    pub fn config_dirs(&self) -> Arc<Vec<PathBuf>> {
        self.config_dirs.load_full()
    }
    /// Resolve `auto/*` (or keep a concrete model). Returns the model to call
    /// plus routing info when failover applies (pool member or auto).
    pub fn route_model(
        &self,
        model: &crate::provider::Model,
        need: crate::provider::router::Need,
        session_id: &str,
    ) -> Result<
        (
            crate::provider::Model,
            Option<crate::session::processor::RouteInput>,
            String,
        ),
        String,
    > {
        use crate::provider::pool;
        let config = self.config();
        let pool = pool::config(&config);
        let sticky_minutes = pool.sticky_minutes.unwrap_or(30);
        let fallback = pool.fallback.unwrap_or(true);
        if pool::is_virtual(model) {
            let strategy = pool::strategy_of(model, &config);
            let registry = self.registry();
            let pick = self
                .router
                .pick(&registry, strategy, &need, session_id, &[], sticky_minutes)
                .ok_or_else(|| {
                    let connected: Vec<String> = registry
                        .connected()
                        .filter(|p| p.id != pool::PROVIDER && p.models.values().any(|m| m.pool.is_some()))
                        .map(|p| p.id.clone())
                        .collect();
                    if connected.is_empty() {
                        "no pool provider is connected — add a key with `lz auth login <provider>` (see `lz pool setup`)".to_string()
                    } else {
                        format!(
                            "every pool model is rate limited or cooling down ({}). Check `lz pool status`",
                            connected.join(", ")
                        )
                    }
                })?;
            tracing::info!(model = %format!("{}/{}", pick.model.provider_id, pick.model.id), reason = pick.reason, "auto routed");
            self.bus.publish(lz_schema::Event::ModelRouted {
                session_id: session_id.to_string(),
                message_id: String::new(),
                provider_id: pick.model.provider_id.clone(),
                model_id: pick.model.id.clone(),
                reason: format!("routed:{}", pick.reason),
            });
            let reason = pick.reason.clone();
            return Ok((
                pick.model,
                Some(crate::session::processor::RouteInput {
                    strategy,
                    need,
                    sticky_minutes,
                }),
                reason,
            ));
        }
        let pool_connected = self
            .registry()
            .providers
            .get(pool::PROVIDER)
            .is_some_and(|p| p.connected());
        // pool members fail over inside the pool; any other model is rescued by
        // the pool when it fails hard (pool.rescue, default on)
        if fallback && (model.pool.is_some() || (pool_connected && pool.rescue.unwrap_or(true))) {
            return Ok((
                model.clone(),
                Some(crate::session::processor::RouteInput {
                    strategy: pool::Strategy::Auto,
                    need,
                    sticky_minutes: 0,
                }),
                "chosen".into(),
            ));
        }
        Ok((model.clone(), None, "chosen".into()))
    }

    pub fn registry(&self) -> Arc<Registry> {
        self.registry.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn agents(&self) -> Arc<Agents> {
        self.agents.load_full()
    }
    pub fn skills(&self) -> Arc<std::collections::BTreeMap<String, crate::skill::Skill>> {
        self.skills.load_full()
    }
    pub fn commands(&self) -> Arc<std::collections::BTreeMap<String, crate::command::Command>> {
        self.commands.load_full()
    }

    /// Reload config + providers + agents (after `auth login`, config edits).
    pub async fn reload(&self) -> anyhow::Result<()> {
        let loaded = crate::config::load(crate::config::LoadInput {
            paths: &self.paths,
            directory: &self.directory,
            worktree: &self.project.worktree,
        })?;
        let registry = crate::provider::load(&self.paths, &loaded.config, false).await;
        let agents = crate::agent::build(&loaded.raw, &self.paths, &self.project.worktree);
        let skills = crate::skill::discover(
            &self.paths,
            &loaded.config,
            &loaded.directories,
            &self.directory,
            &self.project.worktree,
        );
        self.commands.store(Arc::new(crate::command::build(
            &loaded.raw,
            &self.project.worktree,
            &skills,
        )));
        self.skills.store(Arc::new(skills));
        self.config.store(Arc::new(loaded.config));
        self.raw_config.store(Arc::new(loaded.raw));
        self.config_dirs.store(Arc::new(loaded.directories));
        *self.registry.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(registry);
        self.agents.store(Arc::new(agents));
        self.bus.publish(Event::ConfigUpdated {});
        Ok(())
    }

    /// Connect configured MCP servers and register their tools/prompts.
    pub async fn reload_mcp(&self) {
        let entries = self.config().mcp.clone().unwrap_or_default();
        self.mcp.load(&entries, &self.directory, &self.bus).await;
        self.tools.set_extra(self.mcp.tools().await);
        // MCP prompts become slash commands
        let mut commands = (*self.commands()).clone();
        for (name, description, args, client, server) in self.mcp.prompts().await {
            if commands.contains_key(&name) {
                continue;
            }
            let arg_map: std::collections::BTreeMap<String, String> = args
                .iter()
                .enumerate()
                .map(|(i, a)| (a.clone(), format!("${}", i + 1)))
                .collect();
            let template = self
                .mcp
                .get_prompt(&client, &name, arg_map)
                .await
                .unwrap_or_default();
            commands.insert(
                name.clone(),
                crate::command::Command {
                    name,
                    description,
                    agent: None,
                    model: None,
                    template,
                    subtask: None,
                    source: "mcp",
                },
            );
            let _ = &server;
        }
        self.commands.store(Arc::new(commands));
    }

    /// Bring MCP servers in line with the current config without restarting
    /// the ones that did not change: new/changed → (re)connect, removed → drop.
    pub async fn sync_mcp(&self) {
        let wanted = self.config().mcp.clone().unwrap_or_default();
        let current = self.mcp.configs().await;
        let mut changed = false;
        for name in current.keys() {
            if !wanted.contains_key(name) {
                self.mcp.remove(name, &self.bus).await;
                changed = true;
            }
        }
        let mut to_connect = Vec::new();
        for (name, entry) in &wanted {
            match entry {
                lz_schema::config::McpEntry::Server(cfg) => {
                    let enabled = match cfg {
                        lz_schema::config::McpServerConfig::Local { enabled, .. }
                        | lz_schema::config::McpServerConfig::Remote { enabled, .. } => {
                            enabled.unwrap_or(true)
                        }
                    };
                    if current.get(name) != Some(cfg) {
                        self.mcp.register(name, cfg.clone()).await;
                        changed = true;
                        if enabled {
                            to_connect.push(name.clone());
                        }
                    }
                }
                lz_schema::config::McpEntry::Toggle { .. } => {
                    if current.contains_key(name) {
                        self.mcp.remove(name, &self.bus).await;
                        changed = true;
                    }
                }
            }
        }
        for name in to_connect {
            if let Err(e) = self.mcp.connect_one(&name, &self.directory, &self.bus).await {
                tracing::warn!(server = name, "mcp connect failed: {e}");
                self.bus.publish(Event::McpStatus {
                    name: name.clone(),
                    status: McpStatus::Failed { error: e },
                });
            }
        }
        if changed {
            self.tools.set_extra(self.mcp.tools().await);
        }
    }

    /// Watch the config files (project + global + tui.json) and hot-reload
    /// when any of them changes — edits from the portal, from `lz mcp
    /// install` in another process, or by hand all reach the running engine.
    pub fn watch_config(self: &Arc<Self>) {
        let engine = self.clone();
        tokio::spawn(async move {
            let candidates = |e: &Engine| -> Vec<PathBuf> {
                let mut v: Vec<PathBuf> = e
                    .paths
                    .global_config_dirs()
                    .into_iter()
                    .flat_map(|d| {
                        [
                            "lunarzero.json",
                            "lunarzero.jsonc",
                            "opencode.json",
                            "opencode.jsonc",
                            "config.json",
                        ]
                        .iter()
                        .map(move |n| d.join(n))
                    })
                    .collect();
                for dir in crate::config::ancestors(&e.directory, Some(&e.project.worktree)) {
                    for n in [
                        "lunarzero.json",
                        "lunarzero.jsonc",
                        "opencode.json",
                        "opencode.jsonc",
                    ] {
                        v.push(dir.join(n));
                    }
                }
                v.extend(e.config_dirs().iter().map(|d| d.join("config.json")));
                v.push(e.auth.path().to_path_buf());
                v.push(e.paths.legacy_auth());
                v
            };
            let stamp = |paths: &[PathBuf]| -> Vec<(PathBuf, Option<std::time::SystemTime>, u64)> {
                paths
                    .iter()
                    .map(|p| {
                        let m = std::fs::metadata(p).ok();
                        (
                            p.clone(),
                            m.as_ref().and_then(|m| m.modified().ok()),
                            m.map(|m| m.len()).unwrap_or(0),
                        )
                    })
                    .collect()
            };
            let mut last = stamp(&candidates(&engine));
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let now = stamp(&candidates(&engine));
                if now != last {
                    last = now;
                    tracing::info!("config changed on disk; reloading");
                    if let Err(e) = engine.reload().await {
                        tracing::warn!("config reload failed: {e}");
                        continue;
                    }
                    engine.sync_mcp().await;
                }
            }
        });
    }

    pub async fn refresh_catalog(&self) -> anyhow::Result<()> {
        let registry = crate::provider::load(&self.paths, &self.config(), true).await;
        *self.registry.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(registry);
        Ok(())
    }

    /// Resolve a possibly-relative path against the project directory.
    pub fn resolve_path(&self, p: &str) -> PathBuf {
        let expanded = crate::paths::expand_home(p, &self.paths.home);
        if expanded.is_absolute() {
            expanded
        } else {
            self.directory.join(expanded)
        }
    }

    pub async fn instructions(&self) -> Vec<String> {
        system::instructions(
            &self.paths,
            &self.config(),
            &self.directory,
            &self.project.worktree,
        )
        .await
    }

    /// Nested AGENTS.md/CLAUDE.md files between `file` and the project root that
    /// haven't been attached yet for this assistant message.
    pub async fn resolve_nested_instructions(
        &self,
        history: &[MessageWithParts],
        file: &Path,
        message_id: &str,
    ) -> Vec<(PathBuf, String)> {
        let system_paths: std::collections::HashSet<PathBuf> = system::instruction_paths(
            &self.paths,
            &self.config(),
            &self.directory,
            &self.project.worktree,
        )
        .into_iter()
        .collect();
        let already: std::collections::HashSet<String> = history
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter_map(|p| match &p.kind {
                PartKind::Tool {
                    tool,
                    state: ToolState::Completed { metadata, time, .. },
                    ..
                } if tool == "read" && time.compacted.is_none() => metadata["loaded"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                }),
                _ => None,
            })
            .flatten()
            .collect();
        let root = self.directory.clone();
        let mut out = Vec::new();
        let mut current = file.parent().map(Path::to_path_buf);
        while let Some(dir) = current {
            if !dir.starts_with(&root) || dir == root {
                break;
            }
            if let Some(found) = system::find_in(&dir) {
                let key = found.display().to_string();
                if found != file && !system_paths.contains(&found) && !already.contains(&key) {
                    let mut claims = self.instruction_claims.entry(message_id.to_string()).or_default();
                    if claims.insert(found.clone())
                        && let Ok(content) = std::fs::read_to_string(&found)
                        && !content.trim().is_empty()
                    {
                        out.push((
                            found.clone(),
                            format!("Instructions from: {}\n{content}", found.display()),
                        ));
                    }
                }
            }
            current = dir.parent().map(Path::to_path_buf);
        }
        out
    }

    pub fn clear_instruction_claims(&self, message_id: &str) {
        self.instruction_claims.remove(message_id);
    }

    // ───── hooks filled in by later milestones ─────
    pub async fn mcp_instructions(&self, ruleset: &Ruleset) -> Option<String> {
        let items: Vec<(String, String)> = self
            .mcp
            .instructions()
            .await
            .into_iter()
            .filter(|(_, _, tools)| {
                tools.is_empty()
                    || tools.iter().any(|t| {
                        crate::permission::evaluate(t, "*", &[ruleset]).action
                            != lz_schema::permission::Action::Deny
                    })
            })
            .map(|(name, text, _)| (name, text))
            .collect();
        if items.is_empty() {
            return None;
        }
        let mut lines = vec!["<mcp_instructions>".to_string()];
        for (name, text) in items {
            lines.push(format!("  <server name=\"{name}\">"));
            for l in text.lines() {
                lines.push(format!("    {l}"));
            }
            lines.push("  </server>".into());
        }
        lines.push("</mcp_instructions>".into());
        Some(lines.join("\n"))
    }
    /// Skills block for the system prompt. With `smart.skills` (default) only
    /// the skills relevant to the prompt are described (others by name), and
    /// a clearly matching skill is attached in full so no tool call is needed.
    pub async fn skills_prompt(&self, agent: &crate::agent::Agent, user_text: &str) -> Option<String> {
        self.skills_prompt_detailed(agent, user_text)
            .await
            .map(|(s, _, _)| s)
    }

    /// (prompt block, skills described, skill attached in full)
    pub async fn skills_prompt_detailed(
        &self,
        agent: &crate::agent::Agent,
        user_text: &str,
    ) -> Option<(String, Vec<String>, Option<String>)> {
        if crate::permission::evaluate("skill", "*", &[&agent.permission]).action
            == lz_schema::permission::Action::Deny
        {
            return None;
        }
        let skills = self.skills();
        let list = crate::skill::available(&skills, agent);
        if list.is_empty() {
            return None;
        }
        let smart = self.config().smart.clone().unwrap_or_default();
        if !smart.skills.unwrap_or(true) || user_text.trim().is_empty() {
            let names = list.iter().map(|s| s.name.clone()).collect();
            return Some((crate::skill::format(&list, false), names, None));
        }
        let ranked = crate::skill::rank(&list, user_text);
        let relevant: Vec<&crate::skill::Skill> = ranked
            .iter()
            .filter(|(s, _)| *s > 0.0)
            .take(5)
            .map(|(_, s)| *s)
            .collect();
        let mut out = String::new();
        let mut attached = None;
        if relevant.is_empty() {
            let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
            out.push_str(&format!(
                "Skills available via the skill tool: {}",
                names.join(", ")
            ));
        } else {
            out.push_str(&crate::skill::format(&relevant, false));
            let others: Vec<&str> = list
                .iter()
                .filter(|s| !relevant.iter().any(|r| r.name == s.name))
                .map(|s| s.name.as_str())
                .collect();
            if !others.is_empty() {
                out.push_str(&format!("\nOther skills: {}", others.join(", ")));
            }
            // attach a strong match so the model does not have to load it
            if smart.attach_skill.unwrap_or(true)
                && let Some((score, best)) = ranked.first()
                && *score >= 0.55
                && best.content.len() <= 6000
            {
                out.push_str(&format!(
                    "\n<skill name=\"{}\">\n{}\n</skill>",
                    best.name,
                    best.content.trim()
                ));
                attached = Some(best.name.clone());
            }
        }
        let described = relevant.iter().map(|s| s.name.clone()).collect();
        Some((out, described, attached))
    }
    pub async fn snapshot_track(&self) -> Option<String> {
        self.snapshot.track().await
    }
    pub async fn snapshot_patch(&self, hash: &str) -> Option<(String, Vec<String>)> {
        self.snapshot.patch(hash).await
    }
    pub async fn lsp_touch(&self, path: &Path) {
        if self.lsp.enabled {
            let e = self.self_arc();
            let p = path.to_path_buf();
            tokio::spawn(async move { e.lsp.touch(&p, &e.project.worktree).await });
        }
    }
    /// Run the configured formatter; returns the new content when it changed.
    pub async fn format_file(&self, _path: &Path) -> Option<String> {
        None
    }
    /// LSP diagnostics block for a just-edited file, if any errors.
    pub async fn lsp_diagnostics_after_edit(&self, path: &Path) -> Option<String> {
        let out = self
            .lsp
            .diagnostics_after_edit(path, &self.project.worktree)
            .await;
        if out.is_some() {
            self.bus.publish(Event::LspUpdated {});
        }
        out
    }

    pub async fn shutdown(&self) {
        let ids: Vec<String> = self.status.all().into_keys().collect();
        for id in ids {
            self.runner.abort(&id).await;
        }
        self.mcp.shutdown().await;
        self.lsp.shutdown().await;
    }

    fn err(e: impl std::fmt::Display) -> ApiError {
        ApiError::internal(e)
    }
}

fn storage_err(e: crate::storage::StorageError) -> ApiError {
    match e {
        crate::storage::StorageError::NotFound(m) => ApiError::NotFound { message: m },
        other => ApiError::internal(other),
    }
}

#[async_trait]
impl EngineApi for Engine {
    async fn config(&self) -> ApiResult<Config> {
        Ok((*self.config()).clone())
    }
    async fn providers(&self) -> ApiResult<ProvidersResponse> {
        Ok(self.registry().to_response(&self.config()))
    }
    async fn agents(&self) -> ApiResult<Vec<AgentInfo>> {
        Ok(self.agents().list().into_iter().map(|a| a.to_info()).collect())
    }
    async fn commands(&self) -> ApiResult<Vec<CommandInfo>> {
        Ok(self.commands().values().map(|c| c.to_info()).collect())
    }
    async fn skills(&self) -> ApiResult<Vec<SkillInfo>> {
        Ok(self.skills().values().map(|s| s.to_info()).collect())
    }
    async fn lsp_status(&self) -> ApiResult<Vec<LspStatus>> {
        Ok(self.lsp.status().await)
    }
    async fn mcp_status(&self) -> ApiResult<BTreeMap<String, McpStatus>> {
        Ok(self.mcp.status().await)
    }
    async fn path(&self) -> ApiResult<PathInfo> {
        Ok(PathInfo {
            cwd: self.directory.display().to_string(),
            root: self.project.worktree.display().to_string(),
            worktree: self.project.worktree.display().to_string(),
            directory: self.directory.display().to_string(),
            config: self.paths.config.display().to_string(),
            data: self.paths.data.display().to_string(),
            state: self.paths.state.display().to_string(),
        })
    }

    async fn list_sessions(&self, q: SessionQuery) -> ApiResult<Vec<SessionInfo>> {
        self.sessions
            .list(q.search, q.limit, q.roots)
            .await
            .map_err(storage_err)
    }
    async fn session_status(&self) -> ApiResult<BTreeMap<SessionId, SessionStatus>> {
        Ok(self.status.all())
    }
    async fn get_session(&self, id: &str) -> ApiResult<SessionInfo> {
        self.sessions.get(id).await.map_err(storage_err)
    }
    async fn create_session(&self, opts: CreateSession) -> ApiResult<SessionInfo> {
        self.sessions
            .create(opts.parent_id, opts.title, opts.agent, opts.permission)
            .await
            .map_err(storage_err)
    }
    async fn update_session(&self, id: &str, patch: SessionPatch) -> ApiResult<SessionInfo> {
        self.sessions
            .modify(id, move |s| {
                if let Some(t) = patch.title {
                    s.title = t;
                }
                if let Some(a) = patch.archived {
                    s.time.archived = a;
                }
                if let Some(p) = patch.permission {
                    s.permission = Some(p);
                }
            })
            .await
            .map_err(storage_err)
    }
    async fn delete_session(&self, id: &str) -> ApiResult<()> {
        self.runner.abort(id).await;
        self.sessions.delete(id).await.map_err(storage_err)
    }
    async fn children(&self, id: &str) -> ApiResult<Vec<SessionInfo>> {
        self.sessions.children(id).await.map_err(storage_err)
    }
    async fn messages(&self, id: &str, q: MessagesQuery) -> ApiResult<Vec<MessageWithParts>> {
        self.sessions
            .messages(id, q.limit, q.before)
            .await
            .map_err(storage_err)
    }
    async fn message(&self, _id: &str, message_id: &str) -> ApiResult<MessageWithParts> {
        let info = self.sessions.get_message(message_id).await.map_err(storage_err)?;
        let parts = self.sessions.parts(message_id).await.map_err(storage_err)?;
        Ok(MessageWithParts { info, parts })
    }
    async fn todos(&self, id: &str) -> ApiResult<Vec<Todo>> {
        self.sessions.todos(id).await.map_err(storage_err)
    }
    async fn diff(&self, id: &str) -> ApiResult<Vec<FileDiff>> {
        // diff from the first snapshot of the session to now
        let msgs = self
            .sessions
            .messages(id, None, None)
            .await
            .map_err(storage_err)?;
        let first = msgs
            .iter()
            .flat_map(|m| m.parts.iter())
            .find_map(|p| match &p.kind {
                PartKind::StepStart { snapshot: Some(s) } => Some(s.clone()),
                _ => None,
            });
        match first {
            Some(s) => Ok(self.snapshot.diff(&s, None).await),
            None => Ok(Vec::new()),
        }
    }

    async fn prompt(&self, id: &str, req: PromptRequest) -> ApiResult<MessageWithParts> {
        let engine = self.self_arc();
        runner::prompt(engine.clone(), id, req)
            .await
            .map_err(|e| match e {
                runner::PromptError::Busy => ApiError::Busy,
                other => ApiError::invalid(other),
            })?;
        self.runner.wait(id).await;
        runner::last_assistant(self, id)
            .await
            .ok_or_else(|| ApiError::not_found("no assistant message"))
    }
    async fn prompt_async(&self, id: &str, req: PromptRequest) -> ApiResult<()> {
        runner::prompt(self.self_arc(), id, req)
            .await
            .map_err(|e| match e {
                runner::PromptError::Busy => ApiError::Busy,
                other => ApiError::invalid(other),
            })?;
        Ok(())
    }
    async fn command(&self, id: &str, req: CommandRequest) -> ApiResult<()> {
        crate::command::execute(
            self.self_arc(),
            crate::command::CommandInput {
                session_id: id.into(),
                command: req.command,
                arguments: req.arguments,
                agent: req.agent,
                model: req.model,
                variant: None,
                parts: Vec::new(),
            },
        )
        .await
        .map(|_| ())
        .map_err(ApiError::invalid)
    }
    async fn shell(&self, id: &str, req: ShellRequest) -> ApiResult<()> {
        if self.runner.is_running(id) {
            return Err(ApiError::Busy);
        }
        let engine = self.self_arc();
        let sid = id.to_string();
        let cancel = tokio_util::sync::CancellationToken::new();
        self.status.set(&self.bus, id, SessionStatus::Busy);
        tokio::spawn(async move {
            let r = crate::session::shell::run(
                engine.clone(),
                crate::session::shell::ShellInput {
                    session_id: sid.clone(),
                    command: req.command,
                    agent: req.agent,
                    model: req.model,
                },
                cancel,
            )
            .await;
            if let Err(e) = r {
                engine.bus.publish(Event::SessionError {
                    session_id: Some(sid.clone()),
                    error: MessageError::Unknown {
                        message: e.to_string(),
                        r#ref: None,
                    },
                });
            }
            engine.status.set(&engine.bus, &sid, SessionStatus::Idle);
        });
        Ok(())
    }
    async fn abort(&self, id: &str) -> ApiResult<()> {
        self.runner.abort(id).await;
        Ok(())
    }
    async fn resume(&self, id: &str, model: Option<ModelRef>) -> ApiResult<()> {
        if self.runner.is_running(id) {
            return Err(ApiError::Busy);
        }
        let msgs = self.sessions.messages(id, None, None).await.map_err(Self::err)?;
        let Some(last) = msgs.last() else {
            return Err(ApiError::invalid("nothing to resume in this session"));
        };
        // continue with the model the user has selected now (the failing one
        // may be the reason the turn stopped)
        if let Some(m) = model
            && let Some(user) = msgs.iter().rev().find_map(|x| match &x.info {
                Message::User(u) => Some(u.clone()),
                _ => None,
            })
            && (user.model.provider_id != m.provider_id || user.model.model_id != m.model_id)
        {
            let mut u = user;
            u.model = m;
            self.sessions
                .update_message(Message::User(u))
                .await
                .map_err(Self::err)?;
        }
        // clear the error on the last assistant step so the loop can pick up
        if let Message::Assistant(a) = &last.info
            && a.error.is_some()
        {
            let mut fixed = a.clone();
            fixed.error = None;
            self.sessions
                .update_message(Message::Assistant(fixed))
                .await
                .map_err(Self::err)?;
        }
        self.runner.ensure_running(self.self_arc(), id.to_string());
        Ok(())
    }
    async fn summarize(&self, id: &str, _model: Option<ModelRef>) -> ApiResult<()> {
        let msgs = self
            .sessions
            .messages(id, None, None)
            .await
            .map_err(storage_err)?;
        let last_user = msgs
            .iter()
            .rev()
            .find_map(|m| m.info.as_user().cloned())
            .ok_or_else(|| ApiError::invalid("session has no messages"))?;
        crate::session::compaction::create(self, id, &last_user, false, false)
            .await
            .map_err(storage_err)?;
        self.runner.ensure_running(self.self_arc(), id.to_string());
        Ok(())
    }
    async fn fork(&self, _id: &str, _at: Option<MessageId>) -> ApiResult<SessionInfo> {
        Err(ApiError::invalid("fork not implemented yet"))
    }
    async fn revert(&self, id: &str, message_id: &str, part_id: Option<PartId>) -> ApiResult<SessionInfo> {
        crate::session::revert::revert(self, id, message_id, part_id.as_deref())
            .await
            .map_err(ApiError::invalid)
    }
    async fn unrevert(&self, id: &str) -> ApiResult<SessionInfo> {
        crate::session::revert::unrevert(self, id)
            .await
            .map_err(ApiError::invalid)
    }
    async fn init(&self, id: &str, model: Option<ModelRef>) -> ApiResult<()> {
        self.command(
            id,
            CommandRequest {
                command: "init".into(),
                arguments: String::new(),
                agent: None,
                model,
            },
        )
        .await
    }

    async fn pending_permissions(&self) -> ApiResult<Vec<PermissionRequest>> {
        Ok(self.permissions.pending())
    }
    async fn reply_permission(&self, id: &str, reply: PermissionReplyRequest) -> ApiResult<()> {
        self.permissions
            .reply(id, reply.reply, reply.message)
            .await
            .map_err(ApiError::not_found)
    }
    async fn pending_questions(&self) -> ApiResult<Vec<QuestionRequest>> {
        Ok(self.questions.pending())
    }
    async fn reply_question(&self, id: &str, answers: Vec<Vec<String>>) -> ApiResult<()> {
        self.questions
            .reply(&self.bus, id, answers)
            .map_err(ApiError::not_found)
    }
    async fn reject_question(&self, id: &str) -> ApiResult<()> {
        self.questions.reject(&self.bus, id).map_err(ApiError::not_found)
    }

    async fn find_files(&self, query: &str, limit: usize) -> ApiResult<Vec<String>> {
        let root = self.directory.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || crate::files::find(&root, &query, limit))
            .await
            .map_err(Self::err)
    }
    async fn grep(&self, pattern: &str, limit: usize) -> ApiResult<Vec<GrepMatch>> {
        let root = self.directory.clone();
        let pattern = pattern.to_string();
        let hits = tokio::task::spawn_blocking(move || {
            crate::tool::builtins::search::grep_files(&root, &pattern, None, limit)
        })
        .await
        .map_err(Self::err)?
        .map_err(ApiError::invalid)?;
        Ok(hits
            .0
            .into_iter()
            .map(|h| GrepMatch {
                path: h.path.display().to_string(),
                line: h.line,
                text: h.text,
            })
            .collect())
    }
    async fn file_status(&self) -> ApiResult<Vec<FileStatus>> {
        Ok(crate::files::git_status(&self.project.worktree).await)
    }
    async fn read_file(&self, path: &str) -> ApiResult<String> {
        std::fs::read_to_string(self.resolve_path(path)).map_err(Self::err)
    }
    async fn set_auth(&self, provider: &str, auth: AuthInfo) -> ApiResult<()> {
        self.auth.set(provider, auth).map_err(Self::err)?;
        self.reload().await.map_err(Self::err)
    }
    async fn remove_auth(&self, provider: &str) -> ApiResult<()> {
        self.auth.remove(provider).map_err(Self::err)?;
        self.reload().await.map_err(Self::err)
    }
    async fn install_skill(&self, source: &str, global: bool) -> ApiResult<Vec<SkillInfo>> {
        let src = crate::skill_install::Source::parse(source).map_err(ApiError::invalid)?;
        let target = crate::skill_install::target_dir(&self.paths, &self.project.worktree, global);
        let installed = crate::skill_install::install(&src, &target)
            .await
            .map_err(ApiError::invalid)?;
        self.reload().await.map_err(Self::err)?;
        Ok(installed
            .into_iter()
            .map(|i| SkillInfo {
                name: i.name,
                description: i.description,
                location: i.path.display().to_string(),
            })
            .collect())
    }
    async fn remove_skill(&self, name: &str) -> ApiResult<()> {
        crate::skill_install::remove(&self.paths, &self.project.worktree, name)
            .map_err(ApiError::not_found)?;
        self.reload().await.map_err(Self::err)
    }
    async fn install_mcp(
        &self,
        source: &str,
        name: Option<String>,
        global: bool,
    ) -> ApiResult<McpInstallInfo> {
        let installed = crate::mcp_install::install(&self.paths, source, name)
            .await
            .map_err(ApiError::invalid)?;
        let path = crate::mcp_install::config_file(&self.paths, &self.directory, global);
        crate::mcp_install::register(&path, &installed).map_err(ApiError::invalid)?;
        self.reload().await.map_err(Self::err)?;
        // start only this server — the others keep running untouched
        let cfg = lz_schema::config::McpServerConfig::Local {
            command: installed.command.clone(),
            cwd: installed.cwd.clone(),
            environment: None,
            enabled: None,
            timeout: None,
        };
        self.mcp.register(&installed.name, cfg).await;
        let status = match self
            .mcp
            .connect_one(&installed.name, &self.directory, &self.bus)
            .await
        {
            Ok(()) => "connected".to_string(),
            Err(e) => format!("failed: {e}"),
        };
        self.tools.set_extra(self.mcp.tools().await);
        let prefix = format!("{}_", crate::mcp::sanitize(&installed.name));
        let tools: Vec<String> = self
            .mcp
            .tools()
            .await
            .iter()
            .filter(|t| t.id().starts_with(&prefix))
            .map(|t| t.id().to_string())
            .collect();
        Ok(McpInstallInfo {
            name: installed.name,
            command: installed.command,
            runtime: installed.runtime,
            config_path: path.display().to_string(),
            status,
            tools,
        })
    }
    async fn mcp_connect(&self, name: &str) -> ApiResult<()> {
        self.mcp
            .connect_one(name, &self.directory, &self.bus)
            .await
            .map_err(ApiError::invalid)?;
        self.tools.set_extra(self.mcp.tools().await);
        Ok(())
    }
    async fn mcp_disconnect(&self, name: &str) -> ApiResult<()> {
        self.mcp
            .disconnect_one(name, &self.bus)
            .await
            .map_err(ApiError::invalid)?;
        self.tools.set_extra(self.mcp.tools().await);
        Ok(())
    }

    fn subscribe(&self) -> BoxStream<'static, Event> {
        let rx = self.bus.subscribe();
        tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(|r| async move {
                match r {
                    Ok(ev) => Some((*ev).clone()),
                    Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                        tracing::warn!("event subscriber lagged by {n} events");
                        None
                    }
                }
            })
            .boxed()
    }
}

/// `Engine` is always constructed inside an `Arc`; tools and the runner need
/// an owned handle back to it.
impl Engine {
    pub fn self_arc(&self) -> Arc<Engine> {
        self.weak_self.upgrade().expect("engine dropped")
    }
}
