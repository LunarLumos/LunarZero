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
            return Ok((
                pick.model,
                Some(crate::session::processor::RouteInput {
                    strategy,
                    need,
                    sticky_minutes,
                }),
            ));
        }
        if model.pool.is_some() && fallback {
            return Ok((
                model.clone(),
                Some(crate::session::processor::RouteInput {
                    strategy: pool::Strategy::Auto,
                    need,
                    sticky_minutes: 0,
                }),
            ));
        }
        Ok((model.clone(), None))
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
    pub async fn skills_prompt(&self, agent: &crate::agent::Agent) -> Option<String> {
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
        Some(format!(
            "Skills provide specialized instructions and workflows for specific tasks.\nUse the skill tool to load a skill when a task matches its description.\n{}",
            crate::skill::format(&list, true)
        ))
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
