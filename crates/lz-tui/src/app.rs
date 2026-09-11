//! The application model: state, message handling (`update`) and drawing
//! (`view`). Async work is spawned onto tokio and reports back via `Msg`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use lz_schema::Event;
use lz_schema::api::*;
use lz_schema::config::TuiConfig;
use lz_schema::session::*;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc::UnboundedSender;

use crate::dialog::*;
use crate::keymap::{Chord, Keymap};
use crate::kv::Kv;
use crate::store::Store;
use crate::theme::{self, Mode, Theme};
use crate::widgets::messages::{RenderCache, RenderOpts, fmt_tokens};
use crate::widgets::panels::{PermChoice, PermissionPanel, QuestionPanel};
use crate::widgets::prompt::{Completion, Prompt, PromptMode};
use crate::widgets::textarea::AtomKind;
use crate::widgets::toast::{ToastKind, Toasts};
use crate::widgets::{sidebar, syntax};

pub struct TuiOptions {
    pub session: Option<String>,
    pub continue_last: bool,
    pub fork: bool,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub tui: TuiConfig,
    pub theme_dirs: Vec<PathBuf>,
    pub kv_path: PathBuf,
    pub version: String,
    /// URL of the in-process web portal, if it started.
    pub web_url: Option<String>,
}

pub struct Bootstrap {
    pub config: lz_schema::config::Config,
    pub providers: ProvidersResponse,
    pub agents: Vec<AgentInfo>,
    pub commands: Vec<CommandInfo>,
    pub skills: Vec<SkillInfo>,
    pub lsp: Vec<LspStatus>,
    pub mcp: BTreeMap<String, McpStatus>,
    pub path: PathInfo,
    pub sessions: Vec<SessionInfo>,
    pub status: BTreeMap<String, SessionStatus>,
    pub permissions: Vec<PermissionRequest>,
    pub questions: Vec<QuestionRequest>,
}

pub enum Submit {
    Prompt(PromptRequest),
    Shell(ShellRequest),
    Command(CommandRequest),
}

#[allow(clippy::large_enum_variant)]
pub enum Msg {
    Term(TermEvent),
    Engine(Vec<Event>),
    Booted(Box<Bootstrap>),
    Sessions(Vec<SessionInfo>),
    Messages(String, Vec<MessageWithParts>),
    SessionReady(SessionInfo, Option<Submit>),
    Files(u64, Vec<String>),
    Diff(String, Vec<FileDiff>),
    Todos(String, Vec<Todo>),
    Providers(ProvidersResponse),
    Refresh,
    Error(String),
    Toast(ToastKind, String),
    Tick,
    EditorDone(Option<String>),
    Exported(String),
}

pub enum Suspend {
    Editor(String),
    Stop,
}

pub struct App {
    pub api: Arc<dyn EngineApi>,
    pub tx: UnboundedSender<Msg>,
    pub store: Store,
    pub theme: Theme,
    pub theme_name: String,
    pub mode: Mode,
    pub theme_dirs: Vec<PathBuf>,
    pub system_colors: (Option<theme::Rgba>, Option<theme::Rgba>),
    theme_backup: Option<(String, Theme)>,
    pub keymap: Keymap,
    leader: Option<Instant>,
    leader_timeout: Duration,
    pub kv: Kv,
    pub session: Option<String>,
    pub prompt: Prompt,
    perm: PermissionPanel,
    question: Option<(String, QuestionPanel)>,
    pub dialogs: Vec<Dialog>,
    pub toasts: Toasts,
    cache: RenderCache,
    scroll: usize,
    follow: bool,
    last_total_lines: usize,
    last_list_height: usize,
    pub show_thinking: bool,
    pub show_details: bool,
    pub sidebar: bool,
    pub tips: bool,
    pub model: Option<ModelRef>,
    pub agent: String,
    pub variant: Option<String>,
    recent_models: Vec<String>,
    favorite_models: Vec<String>,
    pub focused: bool,
    pub quit: bool,
    pub suspend: Option<Suspend>,
    pub dirty: bool,
    file_req: u64,
    initial_prompt: Option<String>,
    initial_model: Option<String>,
    initial_agent: Option<String>,
    opts_session: Option<String>,
    opts_continue: bool,
    opts_fork: bool,
    version: String,
    web_url: Option<String>,
    tick: u64,
    attention: bool,
    booted: bool,
    scroll_speed: u16,
    pub area: Rect,
    /// Right-hand column used for notifications (sidebar when shown).
    notify_area: Option<Rect>,
}

const SLASH: &[(&str, &str)] = &[
    ("new", "Start a new session"),
    ("sessions", "Switch session"),
    ("models", "Choose a model"),
    ("agents", "Choose an agent"),
    ("variants", "Choose a model variant"),
    ("mcps", "Toggle MCP servers"),
    ("themes", "Choose a theme"),
    ("connect", "Connect a provider (API key)"),
    ("skills", "Browse skills"),
    ("help", "Show keybinds"),
    ("status", "Show engine status"),
    ("compact", "Compact the session context"),
    ("undo", "Undo the last turn (restore files)"),
    ("redo", "Redo an undone turn"),
    ("fork", "Fork this session"),
    ("rename", "Rename this session"),
    ("export", "Export the session to markdown"),
    ("copy", "Copy the last assistant message"),
    ("editor", "Compose in $EDITOR"),
    ("init", "Create/update AGENTS.md"),
    ("details", "Toggle tool details"),
    ("thinking", "Toggle thinking blocks"),
    ("web", "Open the web portal (keys, pool, settings, chat)"),
    (
        "install",
        "Install a skill (or --mcp server) from GitHub / npm: / pypi:",
    ),
    ("exit", "Exit"),
];

impl App {
    pub fn new(
        api: Arc<dyn EngineApi>,
        tx: UnboundedSender<Msg>,
        opts: TuiOptions,
        system_colors: (Option<theme::Rgba>, Option<theme::Rgba>),
    ) -> App {
        let kv = Kv::open(opts.kv_path.clone());
        let mode = crate::terminal::detect_mode(system_colors.0);
        let theme_name = opts
            .tui
            .theme
            .clone()
            .or_else(|| kv.get_str("theme").map(str::to_string))
            .unwrap_or_else(|| theme::DEFAULT_THEME.into());
        let dirs: Vec<&std::path::Path> = opts.theme_dirs.iter().map(|p| p.as_path()).collect();
        let theme = theme::load(&theme_name, mode, &dirs, system_colors).unwrap_or_else(|_| {
            theme::load(theme::DEFAULT_THEME, mode, &[], system_colors).expect("embedded theme")
        });
        let keymap = Keymap::new(
            opts.tui.keybinds.as_ref(),
            opts.tui.leader_timeout.unwrap_or(2000),
        );
        let prompt = Prompt::new(
            kv.get_list("history"),
            opts.tui.prompt.as_ref().and_then(|p| p.max_height).unwrap_or(10),
        );
        let attention = opts
            .tui
            .attention
            .as_ref()
            .and_then(|a| a.enabled)
            .unwrap_or(true);
        App {
            api,
            tx,
            store: Store::default(),
            theme,
            theme_name,
            mode,
            theme_dirs: opts.theme_dirs,
            system_colors,
            theme_backup: None,
            leader_timeout: Duration::from_millis(opts.tui.leader_timeout.unwrap_or(2000)),
            keymap,
            leader: None,
            show_thinking: kv.get_bool("show_thinking", false),
            show_details: kv.get_bool("show_details", false),
            sidebar: kv.get_bool("sidebar", true),
            tips: kv.get_bool("tips", true),
            recent_models: kv.get_list("recent_models"),
            favorite_models: kv.get_list("favorite_models"),
            kv,
            session: None,
            prompt,
            perm: PermissionPanel::default(),
            question: None,
            dialogs: Vec::new(),
            toasts: Toasts::default(),
            cache: RenderCache::default(),
            scroll: 0,
            follow: true,
            last_total_lines: 0,
            last_list_height: 0,
            model: None,
            agent: String::new(),
            variant: None,
            focused: true,
            quit: false,
            suspend: None,
            dirty: true,
            file_req: 0,
            initial_prompt: opts.prompt,
            initial_model: opts.model,
            initial_agent: opts.agent,
            opts_session: opts.session,
            opts_continue: opts.continue_last,
            opts_fork: opts.fork,
            version: opts.version,
            web_url: opts.web_url,
            tick: 0,
            attention,
            booted: false,
            scroll_speed: opts.tui.scroll_speed.unwrap_or(3).max(1),
            area: Rect::default(),
            notify_area: None,
        }
    }

    // ───────────────────────────── async helpers ─────────────────────────────

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = Result<Option<Msg>, ApiError>> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match fut.await {
                Ok(Some(m)) => {
                    let _ = tx.send(m);
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                }
            }
        });
    }

    pub fn bootstrap(&self) {
        let api = self.api.clone();
        self.spawn(async move {
            let (config, providers, agents, commands, skills, lsp, mcp, path) = tokio::try_join!(
                api.config(),
                api.providers(),
                api.agents(),
                api.commands(),
                api.skills(),
                api.lsp_status(),
                api.mcp_status(),
                api.path()
            )?;
            let (sessions, status, permissions, questions) = tokio::try_join!(
                api.list_sessions(SessionQuery {
                    roots: true,
                    limit: Some(200),
                    ..Default::default()
                }),
                api.session_status(),
                api.pending_permissions(),
                api.pending_questions()
            )?;
            Ok(Some(Msg::Booted(Box::new(Bootstrap {
                config,
                providers,
                agents,
                commands,
                skills,
                lsp,
                mcp,
                path,
                sessions,
                status,
                permissions,
                questions,
            }))))
        });
    }

    fn refresh_meta(&self) {
        let api = self.api.clone();
        let tx = self.tx.clone();
        self.spawn(async move {
            let (providers, agents, commands, skills, lsp, mcp) = tokio::try_join!(
                api.providers(),
                api.agents(),
                api.commands(),
                api.skills(),
                api.lsp_status(),
                api.mcp_status()
            )?;
            let _ = tx.send(Msg::Providers(providers));
            let _ = tx.send(Msg::Booted(Box::new(Bootstrap {
                config: api.config().await?,
                providers: ProvidersResponse::default(),
                agents,
                commands,
                skills,
                lsp,
                mcp,
                path: api.path().await?,
                sessions: Vec::new(),
                status: BTreeMap::new(),
                permissions: Vec::new(),
                questions: Vec::new(),
            })));
            Ok(Some(Msg::Refresh))
        });
    }

    fn load_session(&mut self, id: &str) {
        let api = self.api.clone();
        let id_s = id.to_string();
        self.spawn(async move {
            let msgs = api.messages(&id_s, MessagesQuery::default()).await?;
            Ok(Some(Msg::Messages(id_s, msgs)))
        });
        let api = self.api.clone();
        let id_s = id.to_string();
        self.spawn(async move {
            let d = api.diff(&id_s).await?;
            Ok(Some(Msg::Diff(id_s, d)))
        });
        let api = self.api.clone();
        let id_s = id.to_string();
        self.spawn(async move {
            let t = api.todos(&id_s).await?;
            Ok(Some(Msg::Todos(id_s, t)))
        });
    }

    pub fn open_session(&mut self, id: &str) {
        if self.session.as_deref() != Some(id) {
            self.session = Some(id.to_string());
            self.cache.clear();
            self.scroll = 0;
            self.follow = true;
            self.question = None;
            self.perm = PermissionPanel::default();
        }
        if !self.store.loaded.contains(id) {
            self.load_session(id);
        }
        if let Some(s) = self.store.sessions.get(id) {
            if let Some(m) = &s.model {
                self.model = Some(ModelRef {
                    provider_id: m.provider_id.clone(),
                    model_id: m.id.clone(),
                    variant: m.variant.clone(),
                });
            }
            if let Some(a) = &s.agent
                && self.store.agents.iter().any(|x| &x.name == a)
            {
                self.agent = a.clone();
            }
            crate::terminal::set_title(&format!("LZ | {}", s.title));
        }
        self.dirty = true;
    }

    fn new_session_and(&mut self, submit: Option<Submit>) {
        let api = self.api.clone();
        let agent = Some(self.agent.clone()).filter(|a| !a.is_empty());
        self.spawn(async move {
            let s = api
                .create_session(CreateSession {
                    agent,
                    ..Default::default()
                })
                .await?;
            Ok(Some(Msg::SessionReady(s, submit)))
        });
    }

    fn send(&mut self, submit: Submit) {
        let Some(id) = self.session.clone() else {
            self.new_session_and(Some(submit));
            return;
        };
        let api = self.api.clone();
        self.spawn(async move {
            match submit {
                Submit::Prompt(req) => api.prompt_async(&id, req).await?,
                Submit::Shell(req) => api.shell(&id, req).await?,
                Submit::Command(req) => api.command(&id, req).await?,
            }
            Ok(None)
        });
        self.follow = true;
    }

    fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(kind, text);
        self.dirty = true;
    }

    // ───────────────────────────── update ─────────────────────────────

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Tick => {
                self.tick += 1;
                if self.toasts.tick() {
                    self.dirty = true;
                }
                if let Some(t) = self.leader
                    && t.elapsed() > self.leader_timeout
                {
                    self.leader = None;
                    self.dirty = true;
                }
                if self
                    .session
                    .as_ref()
                    .map(|s| self.store.is_busy(s))
                    .unwrap_or(false)
                    || self.leader.is_some()
                {
                    self.dirty = true;
                }
                return;
            }
            Msg::Term(ev) => self.on_term(ev),
            Msg::Engine(events) => {
                for e in events {
                    self.on_event(e);
                }
            }
            Msg::Booted(b) => self.on_booted(*b),
            Msg::Providers(p) => self.store.providers = p,
            Msg::Refresh => self.ensure_model(),
            Msg::Sessions(list) => {
                for s in list {
                    self.store.sessions.insert(s.id.clone(), s);
                }
                if let Some(Dialog::Select(d)) = self.dialogs.last_mut()
                    && d.kind == SelectKind::Sessions
                {
                    let items = Self::session_items(&self.store);
                    d.items = items;
                    d.refilter();
                }
            }
            Msg::Messages(id, msgs) => {
                self.store.load_messages(&id, msgs);
                self.cache.clear();
                self.follow = true;
            }
            Msg::SessionReady(info, submit) => {
                let id = info.id.clone();
                self.store.sessions.insert(id.clone(), info);
                self.store.loaded.insert(id.clone());
                self.open_session(&id);
                if let Some(s) = submit {
                    self.send(s);
                }
            }
            Msg::Files(req, files) => {
                if req == self.prompt.autocomplete.request_id {
                    let q = self.prompt.autocomplete.query.clone();
                    let mut items: Vec<Completion> = self.agent_completions(&q);
                    items.extend(files.into_iter().map(|f| Completion {
                        label: format!("@{f}"),
                        description: "file".into(),
                        insert: f,
                        file: true,
                        agent: false,
                    }));
                    self.prompt.autocomplete.items = items;
                    self.prompt.autocomplete.cursor = 0;
                    self.prompt.autocomplete.visible = !self.prompt.autocomplete.items.is_empty();
                }
            }
            Msg::Diff(id, d) => {
                self.store.diffs.insert(id, d);
            }
            Msg::Todos(id, t) => {
                self.store.todos.insert(id, t);
            }
            Msg::Error(e) => self.toast(ToastKind::Error, e),
            Msg::Toast(k, t) => self.toast(k, t),
            Msg::EditorDone(text) => {
                if let Some(t) = text {
                    self.prompt.textarea.set_text(&t);
                }
            }
            Msg::Exported(path) => self.toast(ToastKind::Success, format!("Exported to {path}")),
        }
        self.dirty = true;
    }

    fn on_booted(&mut self, b: Bootstrap) {
        let first = !self.booted;
        self.store.config = b.config;
        if !b.providers.providers.is_empty() || first {
            self.store.providers = b.providers;
        }
        self.store.agents = b.agents;
        self.store.commands = b.commands;
        self.store.skills = b.skills;
        self.store.lsp = b.lsp;
        self.store.mcp = b.mcp;
        self.store.path = Some(b.path);
        for s in b.sessions {
            self.store.sessions.insert(s.id.clone(), s);
        }
        for (k, v) in b.status {
            self.store.status.insert(k, v);
        }
        for p in b.permissions {
            if !self.store.permissions.iter().any(|x| x.id == p.id) {
                self.store.permissions.push(p);
            }
        }
        for q in b.questions {
            if !self.store.questions.iter().any(|x| x.id == q.id) {
                self.store.questions.push(q);
            }
        }
        if !first {
            return;
        }
        self.booted = true;
        // agent
        let default_agent = self
            .initial_agent
            .take()
            .or_else(|| self.store.config.default_agent.clone())
            .unwrap_or_else(|| "build".into());
        self.agent = if self.store.agents.iter().any(|a| a.name == default_agent) {
            default_agent
        } else {
            self.primary_agents().first().cloned().unwrap_or_default()
        };
        self.ensure_model();
        // session
        let target = if let Some(s) = self.opts_session.take() {
            Some(s)
        } else if self.opts_continue {
            let cwd = self
                .store
                .path
                .as_ref()
                .map(|p| p.directory.clone())
                .unwrap_or_default();
            self.store
                .sessions
                .values()
                .filter(|s| s.parent_id.is_none() && (cwd.is_empty() || s.directory == cwd))
                .max_by_key(|s| s.time.updated)
                .map(|s| s.id.clone())
        } else {
            None
        };
        if let Some(id) = target {
            if self.opts_fork {
                let api = self.api.clone();
                self.spawn(async move {
                    let s = api.fork(&id, None).await?;
                    Ok(Some(Msg::SessionReady(s, None)))
                });
            } else if self.store.sessions.contains_key(&id) {
                self.open_session(&id);
            } else {
                let api = self.api.clone();
                self.spawn(async move {
                    let s = api.get_session(&id).await?;
                    Ok(Some(Msg::SessionReady(s, None)))
                });
            }
        }
        if let Some(p) = self.initial_prompt.take() {
            self.prompt.textarea.set_text(&p);
            self.submit();
        }
        if self.store.providers.providers.iter().all(|p| !p.connected) {
            self.toast(
                ToastKind::Warning,
                "No provider connected — use /connect or set OPENAI_API_KEY",
            );
        }
    }

    fn ensure_model(&mut self) {
        if let Some(spec) = self.initial_model.take() {
            if let Some(m) = self.resolve_model_spec(&spec) {
                self.model = Some(m);
            } else {
                self.toast(ToastKind::Error, format!("unknown model: {spec}"));
            }
        }
        let valid = self
            .model
            .as_ref()
            .map(|m| self.store.model_info(&m.provider_id, &m.model_id).is_some())
            .unwrap_or(false);
        if valid {
            return;
        }
        let candidates: Vec<String> = self
            .recent_models
            .clone()
            .into_iter()
            .chain(self.store.config.model.clone())
            .collect();
        for spec in candidates {
            if let Some(m) = self.resolve_model_spec(&spec)
                && self
                    .store
                    .providers
                    .providers
                    .iter()
                    .any(|p| p.id == m.provider_id && p.connected)
            {
                self.model = Some(m);
                return;
            }
        }
        // first connected provider's default model
        let pick = self
            .store
            .providers
            .providers
            .iter()
            .find(|p| p.connected)
            .and_then(|p| {
                let id = self
                    .store
                    .providers
                    .default
                    .get(&p.id)
                    .cloned()
                    .or_else(|| p.models.first().map(|m| m.id.clone()))?;
                Some(ModelRef {
                    provider_id: p.id.clone(),
                    model_id: id,
                    variant: None,
                })
            });
        self.model = pick;
    }

    fn resolve_model_spec(&self, spec: &str) -> Option<ModelRef> {
        let (p, m) = spec.split_once('/')?;
        self.store.model_info(p, m).map(|mi| ModelRef {
            provider_id: mi.provider_id.clone(),
            model_id: mi.id.clone(),
            variant: None,
        })
    }

    fn primary_agents(&self) -> Vec<String> {
        self.store
            .agents
            .iter()
            .filter(|a| !a.hidden && !matches!(a.mode, lz_schema::config::AgentMode::Subagent))
            .map(|a| a.name.clone())
            .collect()
    }

    fn on_event(&mut self, e: Event) {
        let sid = e.session_id().map(str::to_string);
        match &e {
            Event::PermissionAsked(req) => {
                if Some(&req.session_id) == self.session.as_ref() {
                    self.perm = PermissionPanel::default();
                }
                self.attention("Permission required", &req.permission);
            }
            Event::QuestionAsked(req) => {
                self.attention(
                    "Question",
                    req.questions.first().map(|q| q.question.as_str()).unwrap_or(""),
                );
            }
            Event::SessionStatus { session_id, status } => {
                if Some(session_id) == self.session.as_ref()
                    && matches!(status, SessionStatus::Idle)
                    && self.store.is_busy(session_id)
                {
                    self.attention("Session idle", "Agent finished");
                }
            }
            Event::SessionError { error, .. } => {
                if !matches!(error, MessageError::Aborted { .. }) {
                    self.toasts.push(ToastKind::Error, error.summary(140));
                }
            }
            Event::SessionUpdated { info, .. } => {
                if Some(&info.id) == self.session.as_ref() {
                    crate::terminal::set_title(&format!("LZ | {}", info.title));
                }
            }
            Event::ModelRouted {
                session_id,
                provider_id,
                model_id,
                reason,
                ..
            } => {
                if Some(session_id) == self.session.as_ref() && !reason.starts_with("routed:") {
                    self.toasts.push(
                        ToastKind::Warning,
                        format!("Switched to {provider_id}/{model_id} ({reason})"),
                    );
                }
            }
            Event::ConfigUpdated {} | Event::LspUpdated {} => self.refresh_meta(),
            Event::McpStatus { .. } => {}
            _ => {}
        }
        self.store.apply(e);
        if let Some(sid) = sid
            && Some(&sid) == self.session.as_ref()
        {
            let touched: Vec<String> = self.store.touched.drain().collect();
            self.cache.invalidate(touched);
        }
        self.store.touched.clear();
    }

    fn attention(&mut self, title: &str, body: &str) {
        if self.attention && !self.focused {
            crate::terminal::notify(title, body, true);
        }
    }

    // ───────────────────────────── terminal input ─────────────────────────────

    fn on_term(&mut self, ev: TermEvent) {
        match ev {
            TermEvent::Key(k) => {
                if matches!(k.kind, KeyEventKind::Release) {
                    return;
                }
                self.on_key(k);
            }
            TermEvent::Paste(text) => self.on_paste(text),
            TermEvent::FocusGained => self.focused = true,
            TermEvent::FocusLost => self.focused = false,
            TermEvent::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => self.scroll_by(-(self.scroll_speed as isize)),
                MouseEventKind::ScrollDown => self.scroll_by(self.scroll_speed as isize),
                _ => {}
            },
            TermEvent::Resize(_, _) => {}
        }
    }

    fn on_paste(&mut self, text: String) {
        if self.dialogs.last().is_some() {
            if let Some(Dialog::Input(d)) = self.dialogs.last_mut() {
                d.value.push_str(text.trim_end_matches('\n'));
            } else if let Some(Dialog::Select(d)) = self.dialogs.last_mut() {
                d.filter.push_str(text.trim());
                d.refilter();
            }
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines = text.lines().count();
        if lines >= 3 || text.chars().count() > 150 {
            self.prompt.textarea.insert_atom(
                AtomKind::Paste { full: text.clone() },
                format!("[Pasted ~{} lines]", lines.max(1)),
            );
        } else {
            self.prompt.textarea.insert_str(&text);
        }
        self.update_autocomplete();
    }

    fn on_key(&mut self, k: KeyEvent) {
        let mut chord = Chord::from_event(&k);
        if self.leader.take().is_some() {
            chord.leader = true;
        } else if self.keymap.is_leader(&chord) && self.dialogs.is_empty() {
            self.leader = Some(Instant::now());
            return;
        }
        let actions: Vec<String> = self
            .keymap
            .actions_for(&chord)
            .into_iter()
            .map(str::to_string)
            .collect();
        let has = |a: &str| actions.iter().any(|x| x == a);

        // dialogs first
        if !self.dialogs.is_empty() {
            self.on_dialog_key(&k, &actions);
            return;
        }
        // bottom panels
        let sid = self.session.clone();
        if let Some(sid) = &sid {
            if let Some(req) = self.store.permission_for(sid).cloned() {
                self.on_permission_key(&k, &actions, &req);
                return;
            }
            if let Some(req) = self.store.question_for(sid).cloned() {
                self.on_question_key(&k, &req);
                return;
            }
        }
        // autocomplete
        if self.prompt.autocomplete.visible {
            if has("autocomplete_hide") {
                self.prompt.autocomplete.visible = false;
                return;
            }
            if has("autocomplete_prev")
                && !matches!(k.code, KeyCode::Up if self.prompt.textarea.text().contains('\n'))
            {
                let n = self.prompt.autocomplete.items.len();
                if n > 0 {
                    self.prompt.autocomplete.cursor = (self.prompt.autocomplete.cursor + n - 1) % n;
                }
                return;
            }
            if has("autocomplete_next")
                && !matches!(k.code, KeyCode::Down if self.prompt.textarea.text().contains('\n'))
            {
                let n = self.prompt.autocomplete.items.len();
                if n > 0 {
                    self.prompt.autocomplete.cursor = (self.prompt.autocomplete.cursor + 1) % n;
                }
                return;
            }
            if (has("autocomplete_select") || has("autocomplete_complete")) && self.prompt.accept_completion()
            {
                if self.prompt.mode() == PromptMode::Command && has("autocomplete_select") {
                    self.submit();
                }
                return;
            }
        }
        // global actions
        for a in &actions {
            if self.global_action(a, &chord) {
                return;
            }
        }
        // prompt editing
        for a in &actions {
            if self.prompt_action(a) {
                self.update_autocomplete();
                return;
            }
        }
        if let KeyCode::Char(c) = k.code
            && !k
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            self.prompt.textarea.insert_char(c);
            self.update_autocomplete();
        }
    }

    fn global_action(&mut self, a: &str, _chord: &Chord) -> bool {
        let busy = self
            .session
            .as_ref()
            .map(|s| self.store.is_busy(s))
            .unwrap_or(false);
        match a {
            "app_exit" => {
                if a == "app_exit" && self.prompt.textarea.is_empty() {
                    self.quit = true;
                    return true;
                }
                false
            }
            "command_list" => {
                self.open_palette();
                true
            }
            "help_show" => {
                self.open_help();
                true
            }
            "editor_open" => {
                self.suspend = Some(Suspend::Editor(self.prompt.textarea.text()));
                true
            }
            "theme_list" => {
                self.open_themes();
                true
            }
            "sidebar_toggle" => {
                self.sidebar = !self.sidebar;
                self.kv.set("sidebar", self.sidebar.into());
                true
            }
            "status_view" => {
                self.open_status();
                true
            }
            "session_export" => {
                self.export();
                true
            }
            "session_new" => {
                self.session = None;
                self.cache.clear();
                self.question = None;
                crate::terminal::set_title("LZ");
                true
            }
            "session_list" => {
                self.open_sessions();
                true
            }
            "session_rename" => {
                if let Some(id) = self.session.clone() {
                    let title = self
                        .store
                        .sessions
                        .get(&id)
                        .map(|s| s.title.clone())
                        .unwrap_or_default();
                    self.dialogs.push(Dialog::Input(InputDialog {
                        title: "Rename session".into(),
                        prompt: "New title".into(),
                        value: title,
                        masked: false,
                        action: InputAction::RenameSession(id),
                    }));
                    return true;
                }
                false
            }
            "session_interrupt" => {
                if busy {
                    if let Some(id) = self.session.clone() {
                        let api = self.api.clone();
                        self.spawn(async move {
                            api.abort(&id).await?;
                            Ok(None)
                        });
                    }
                    return true;
                }
                if self.leader.is_some() {
                    self.leader = None;
                    return true;
                }
                false
            }
            "session_compact" => {
                if let Some(id) = self.session.clone() {
                    let api = self.api.clone();
                    let model = self.model.clone();
                    self.spawn(async move {
                        api.summarize(&id, model).await?;
                        Ok(Some(Msg::Toast(ToastKind::Info, "Compacting…".into())))
                    });
                    return true;
                }
                false
            }
            "session_parent" => {
                if self.prompt.textarea.is_empty()
                    && let Some(parent) = self
                        .session
                        .as_ref()
                        .and_then(|id| self.store.sessions.get(id))
                        .and_then(|s| s.parent_id.clone())
                {
                    self.open_session(&parent);
                    return true;
                }
                false
            }
            "model_list" => {
                self.open_models();
                true
            }
            "model_cycle_recent" | "model_cycle_recent_reverse" => {
                if self.recent_models.len() < 2 {
                    return true;
                }
                let cur = self
                    .model
                    .as_ref()
                    .map(|m| format!("{}/{}", m.provider_id, m.model_id))
                    .unwrap_or_default();
                let pos = self.recent_models.iter().position(|m| *m == cur).unwrap_or(0);
                let n = self.recent_models.len();
                let next = if a == "model_cycle_recent" {
                    (pos + 1) % n
                } else {
                    (pos + n - 1) % n
                };
                if let Some(m) = self.resolve_model_spec(&self.recent_models[next].clone()) {
                    self.set_model(m);
                }
                true
            }
            "agent_list" => {
                self.open_agents();
                true
            }
            "agent_cycle" | "agent_cycle_reverse" => {
                let list = self.primary_agents();
                if list.is_empty() {
                    return true;
                }
                let pos = list.iter().position(|x| *x == self.agent).unwrap_or(0);
                let n = list.len();
                let next = if a == "agent_cycle" {
                    (pos + 1) % n
                } else {
                    (pos + n - 1) % n
                };
                self.agent = list[next].clone();
                true
            }
            "variant_cycle" => {
                let variants = self
                    .model
                    .as_ref()
                    .and_then(|m| self.store.model_info(&m.provider_id, &m.model_id))
                    .map(|mi| mi.variants.clone())
                    .unwrap_or_default();
                if variants.is_empty() {
                    self.toast(ToastKind::Info, "This model has no variants");
                    return true;
                }
                let pos = self
                    .variant
                    .as_ref()
                    .and_then(|v| variants.iter().position(|x| x == v));
                self.variant = match pos {
                    None => Some(variants[0].clone()),
                    Some(i) if i + 1 < variants.len() => Some(variants[i + 1].clone()),
                    Some(_) => None,
                };
                true
            }
            "messages_page_up" => {
                self.scroll_by(-(self.last_list_height as isize).max(1));
                true
            }
            "messages_page_down" => {
                self.scroll_by((self.last_list_height as isize).max(1));
                true
            }
            "messages_half_page_up" => {
                self.scroll_by(-((self.last_list_height / 2) as isize).max(1));
                true
            }
            "messages_half_page_down" => {
                self.scroll_by(((self.last_list_height / 2) as isize).max(1));
                true
            }
            "messages_line_up" => {
                self.scroll_by(-1);
                true
            }
            "messages_line_down" => {
                self.scroll_by(1);
                true
            }
            "messages_first" => {
                if self.prompt.textarea.is_empty() || a == "messages_first" && _chord.ctrl {
                    self.scroll = 0;
                    self.follow = false;
                    return true;
                }
                false
            }
            "messages_last" => {
                if self.prompt.textarea.is_empty() || _chord.ctrl {
                    self.follow = true;
                    return true;
                }
                false
            }
            "messages_copy" => {
                self.copy_last();
                true
            }
            "messages_undo" => {
                self.undo();
                true
            }
            "messages_redo" => {
                self.redo();
                true
            }
            "tool_details" => {
                self.toggle_details();
                true
            }
            "display_thinking" => {
                self.toggle_thinking();
                true
            }
            "terminal_suspend" => {
                self.suspend = Some(Suspend::Stop);
                true
            }
            "tips_toggle" => {
                if self.session.is_none() {
                    self.tips = !self.tips;
                    self.kv.set("tips", self.tips.into());
                    return true;
                }
                false
            }
            "which_key_toggle" => {
                self.open_help();
                true
            }
            "mcp_list" => {
                self.open_mcps();
                true
            }
            "provider_connect" => {
                self.open_providers();
                true
            }
            "input_paste" => {
                if let Some((mime, url)) = crate::clipboard::paste_image() {
                    self.prompt.textarea.insert_atom(
                        AtomKind::Attachment {
                            path: "clipboard.png".into(),
                            mime,
                            data_url: Some(url),
                        },
                        "[image]".into(),
                    );
                } else if let Some(t) = crate::clipboard::paste_text() {
                    self.on_paste(t);
                }
                true
            }
            _ => false,
        }
    }

    fn prompt_action(&mut self, a: &str) -> bool {
        let ta = &mut self.prompt.textarea;
        match a {
            "input_submit" => {
                self.submit();
                true
            }
            "input_newline" => {
                ta.insert_char('\n');
                true
            }
            "input_clear" => {
                if ta.is_empty() {
                    return false;
                }
                ta.clear();
                true
            }
            "input_move_left" => {
                ta.move_left();
                true
            }
            "input_move_right" => {
                ta.move_right();
                true
            }
            "input_move_up" | "history_previous" => {
                if a == "input_move_up" && !ta.on_first_line() {
                    ta.move_up();
                    return true;
                }
                if a == "history_previous"
                    && (ta.is_empty() || self.prompt.history_idx.is_some() || ta.on_first_line())
                {
                    return self.prompt.history_prev();
                }
                false
            }
            "input_move_down" | "history_next" => {
                if a == "input_move_down" && !ta.on_last_line() {
                    ta.move_down();
                    return true;
                }
                if a == "history_next" && self.prompt.history_idx.is_some() {
                    return self.prompt.history_next();
                }
                false
            }
            "input_line_home" => {
                ta.line_home();
                true
            }
            "input_line_end" => {
                ta.line_end();
                true
            }
            "input_buffer_home" => {
                ta.buffer_home();
                true
            }
            "input_buffer_end" => {
                ta.buffer_end();
                true
            }
            "input_delete_to_line_end" => {
                ta.delete_to_line_end();
                true
            }
            "input_delete_to_line_start" => {
                ta.delete_to_line_start();
                true
            }
            "input_backspace" => {
                ta.backspace();
                true
            }
            "input_delete" => {
                if ta.is_empty() {
                    return false;
                }
                ta.delete();
                true
            }
            "input_undo" => {
                ta.undo_edit();
                true
            }
            "input_redo" => {
                ta.redo_edit();
                true
            }
            "input_word_forward" => {
                ta.word_forward();
                true
            }
            "input_word_backward" => {
                ta.word_backward();
                true
            }
            "input_delete_word_forward" => {
                ta.delete_word_forward();
                true
            }
            "input_delete_word_backward" => {
                ta.delete_word_backward();
                true
            }
            _ => false,
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.last_total_lines.saturating_sub(self.last_list_height);
        let cur = if self.follow { max } else { self.scroll };
        let next = (cur as isize + delta).clamp(0, max as isize) as usize;
        self.scroll = next;
        self.follow = next >= max;
        self.dirty = true;
    }

    // ───────────────────────────── autocomplete ─────────────────────────────

    fn agent_completions(&self, q: &str) -> Vec<Completion> {
        let q = q.trim_start_matches('@').to_lowercase();
        self.store
            .agents
            .iter()
            .filter(|a| !a.hidden && !matches!(a.mode, lz_schema::config::AgentMode::Primary))
            .filter(|a| a.name.to_lowercase().contains(&q))
            .map(|a| Completion {
                label: format!("@{}", a.name),
                description: a.description.clone().unwrap_or_else(|| "agent".into()),
                insert: format!("@{}", a.name),
                file: false,
                agent: true,
            })
            .collect()
    }

    fn slash_completions(&self, q: &str) -> Vec<Completion> {
        let q = q.trim_start_matches('/').to_lowercase();
        let mut items: Vec<Completion> = SLASH
            .iter()
            .map(|(n, d)| Completion {
                label: format!("/{n}"),
                description: d.to_string(),
                insert: format!("/{n}"),
                file: false,
                agent: false,
            })
            .collect();
        for c in &self.store.commands {
            if SLASH.iter().any(|(n, _)| *n == c.name) {
                continue;
            }
            items.push(Completion {
                label: format!("/{}", c.name),
                description: c.description.clone().unwrap_or_else(|| c.source.clone()),
                insert: format!("/{}", c.name),
                file: false,
                agent: false,
            });
        }
        if q.is_empty() {
            return items;
        }
        let mut scored: Vec<(i32, Completion)> = items
            .into_iter()
            .filter_map(|c| {
                let name = c.label.trim_start_matches('/').to_lowercase();
                let score = if name == q {
                    3
                } else if name.starts_with(&q) {
                    2
                } else if name.contains(&q) {
                    1
                } else {
                    return None;
                };
                Some((score, c))
            })
            .collect();
        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    fn update_autocomplete(&mut self) {
        let (start, token) = self.prompt.textarea.current_token();
        let ac = &mut self.prompt.autocomplete;
        if token.starts_with('/') && start == 0 && !self.prompt.textarea.text().contains(' ') {
            let items = self.slash_completions(&token);
            let ac = &mut self.prompt.autocomplete;
            ac.token_start = start;
            ac.query = token;
            ac.items = items;
            ac.cursor = 0;
            ac.visible = !ac.items.is_empty();
            return;
        }
        if token.starts_with('@') {
            self.file_req += 1;
            let req = self.file_req;
            ac.request_id = req;
            ac.token_start = start;
            ac.query = token.clone();
            let agents = self.agent_completions(&token);
            let ac = &mut self.prompt.autocomplete;
            ac.items = agents;
            ac.cursor = 0;
            ac.visible = true;
            let api = self.api.clone();
            let q = token.strip_prefix('@').unwrap_or(&token).to_string();
            self.spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let files = api.find_files(&q, 20).await?;
                Ok(Some(Msg::Files(req, files)))
            });
            return;
        }
        ac.visible = false;
        ac.items.clear();
    }

    // ───────────────────────────── submit ─────────────────────────────

    pub fn submit(&mut self) {
        let raw = self.prompt.textarea.text();
        let text = self.prompt.textarea.expanded();
        let atoms: Vec<AtomKind> = self
            .prompt
            .textarea
            .atoms()
            .iter()
            .map(|a| a.kind.clone())
            .collect();
        if text.trim().is_empty() && atoms.is_empty() {
            return;
        }
        self.prompt.autocomplete.visible = false;
        match self.prompt.mode() {
            PromptMode::Shell => {
                let cmd = text.trim_start_matches('!').trim().to_string();
                if cmd.is_empty() {
                    return;
                }
                self.finish_input(&raw);
                self.send(Submit::Shell(ShellRequest {
                    command: cmd,
                    agent: Some(self.agent.clone()),
                    model: self.model.clone(),
                }));
            }
            PromptMode::Command => {
                let body = text.trim_start_matches('/');
                let (name, args) = body
                    .split_once(char::is_whitespace)
                    .map(|(n, a)| (n.to_string(), a.trim().to_string()))
                    .unwrap_or((body.trim().to_string(), String::new()));
                self.finish_input(&raw);
                if !self.run_slash(&name, &args) {
                    if self.store.commands.iter().any(|c| c.name == name) {
                        self.send(Submit::Command(CommandRequest {
                            command: name,
                            arguments: args,
                            agent: Some(self.agent.clone()),
                            model: self.model.clone(),
                        }));
                    } else {
                        self.toast(ToastKind::Error, format!("Unknown command: /{name}"));
                    }
                }
            }
            PromptMode::Normal => {
                if self.model.is_none() {
                    self.toast(ToastKind::Error, "No model selected — use /models or /connect");
                    return;
                }
                let parts = self.build_parts(&text, &atoms);
                self.finish_input(&raw);
                let req = PromptRequest {
                    model: self.model.clone(),
                    agent: Some(self.agent.clone()),
                    variant: self.variant.clone(),
                    parts,
                    ..Default::default()
                };
                self.remember_model();
                self.send(Submit::Prompt(req));
            }
        }
    }

    fn finish_input(&mut self, raw: &str) {
        self.prompt.push_history(raw);
        let h = self.prompt.history.clone();
        self.kv.set_list("history", &h);
        self.prompt.textarea.clear();
    }

    fn build_parts(&self, text: &str, atoms: &[AtomKind]) -> Vec<PartInput> {
        let root = self
            .store
            .path
            .as_ref()
            .map(|p| p.directory.clone())
            .unwrap_or_default();
        let mut parts = vec![PartInput::Text {
            id: None,
            text: text.to_string(),
            synthetic: false,
            ignored: false,
        }];
        let mut seen = std::collections::HashSet::new();
        for a in atoms {
            match a {
                AtomKind::Attachment { path, mime, data_url } => {
                    let url = data_url.clone().unwrap_or_else(|| format!("file://{path}"));
                    parts.push(PartInput::File {
                        id: None,
                        mime: mime.clone(),
                        filename: Some(path.clone()),
                        url,
                        source: None,
                    });
                }
                AtomKind::File { path } => {
                    if seen.insert(path.clone()) {
                        parts.push(self.file_part(&root, path, text));
                    }
                }
                AtomKind::Paste { .. } => {}
            }
        }
        // plain `@path` / `@agent` mentions typed by hand
        for tok in text.split_whitespace() {
            let Some(name) = tok.strip_prefix('@') else {
                continue;
            };
            let name = name.trim_end_matches([',', '.', ';', ':', ')', '!', '?']);
            if name.is_empty() || seen.contains(name) {
                continue;
            }
            if self.store.agents.iter().any(|a| a.name == name && !a.hidden) {
                seen.insert(name.to_string());
                parts.push(PartInput::Agent {
                    id: None,
                    name: name.to_string(),
                    source: None,
                });
            } else if std::path::Path::new(&root).join(name).exists() {
                seen.insert(name.to_string());
                parts.push(self.file_part(&root, name, text));
            }
        }
        parts
    }

    fn file_part(&self, root: &str, path: &str, text: &str) -> PartInput {
        let abs = if std::path::Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            std::path::Path::new(root).join(path)
        };
        let start = text.find(&format!("@{path}")).unwrap_or(0) as f64;
        let mime = if abs.is_dir() {
            "application/x-directory".to_string()
        } else {
            "text/plain".to_string()
        };
        PartInput::File {
            id: None,
            mime,
            filename: abs.file_name().map(|n| n.to_string_lossy().to_string()),
            url: format!("file://{}", abs.display()),
            source: Some(FilePartSource::File {
                text: SourceText {
                    value: format!("@{path}"),
                    start,
                    end: start + path.len() as f64 + 1.0,
                },
                path: abs.display().to_string(),
            }),
        }
    }

    fn remember_model(&mut self) {
        let Some(m) = &self.model else { return };
        let key = format!("{}/{}", m.provider_id, m.model_id);
        self.recent_models.retain(|x| *x != key);
        self.recent_models.insert(0, key);
        self.recent_models.truncate(10);
        let r = self.recent_models.clone();
        self.kv.set_list("recent_models", &r);
    }

    fn set_model(&mut self, m: ModelRef) {
        self.variant = None;
        self.model = Some(m);
        self.remember_model();
    }

    fn run_slash(&mut self, name: &str, args: &str) -> bool {
        match name {
            "new" => {
                self.global_action(
                    "session_new",
                    &Chord::parse("none").unwrap_or(Chord::from_event(&KeyEvent::new(
                        KeyCode::Null,
                        KeyModifiers::NONE,
                    ))),
                );
            }
            "sessions" => self.open_sessions(),
            "models" => self.open_models(),
            "agents" => self.open_agents(),
            "variants" => self.open_variants(),
            "mcps" => self.open_mcps(),
            "themes" => self.open_themes(),
            "connect" => self.open_providers(),
            "skills" => self.open_skills(),
            "help" => self.open_help(),
            "status" => self.open_status(),
            "compact" => {
                self.global_action(
                    "session_compact",
                    &Chord::from_event(&KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)),
                );
            }
            "undo" => self.undo(),
            "redo" => self.redo(),
            "fork" => {
                if let Some(id) = self.session.clone() {
                    let api = self.api.clone();
                    self.spawn(async move {
                        let s = api.fork(&id, None).await?;
                        Ok(Some(Msg::SessionReady(s, None)))
                    });
                }
            }
            "rename" => {
                if let Some(id) = self.session.clone() {
                    if args.is_empty() {
                        self.global_action(
                            "session_rename",
                            &Chord::from_event(&KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)),
                        );
                    } else {
                        self.rename(id, args.to_string());
                    }
                }
            }
            "export" => self.export(),
            "copy" => self.copy_last(),
            "editor" => self.suspend = Some(Suspend::Editor(String::new())),
            "init" => {
                if let Some(id) = self.session.clone() {
                    let api = self.api.clone();
                    let model = self.model.clone();
                    self.spawn(async move {
                        api.init(&id, model).await?;
                        Ok(None)
                    });
                } else {
                    let api = self.api.clone();
                    let model = self.model.clone();
                    let agent = Some(self.agent.clone());
                    self.spawn(async move {
                        let s = api
                            .create_session(CreateSession {
                                agent,
                                ..Default::default()
                            })
                            .await?;
                        api.init(&s.id, model).await?;
                        Ok(Some(Msg::SessionReady(s, None)))
                    });
                }
            }
            "details" => self.toggle_details(),
            "thinking" => self.toggle_thinking(),
            "install" => {
                if args.trim().is_empty() {
                    self.toast(
                        ToastKind::Info,
                        "Usage: /install <owner/repo | GitHub URL> [--project]   ·   /install --mcp <source | npm:pkg | pypi:pkg> [--global]",
                    );
                } else if args.contains("--mcp") {
                    let global = args.contains("--global");
                    let source = args
                        .replace("--mcp", "")
                        .replace("--global", "")
                        .trim()
                        .to_string();
                    let api = self.api.clone();
                    self.toast(
                        ToastKind::Info,
                        format!("Installing MCP server from {source}… (this can take a minute)"),
                    );
                    self.spawn(async move {
                        let r = api.install_mcp(&source, None, global).await?;
                        Ok(Some(Msg::Toast(
                            if r.status == "connected" {
                                ToastKind::Success
                            } else {
                                ToastKind::Warning
                            },
                            format!("MCP `{}`: {} ({} tools)", r.name, r.status, r.tools.len()),
                        )))
                    });
                    self.refresh_meta();
                } else {
                    let project = args.contains("--project");
                    let source = args.replace("--project", "").trim().to_string();
                    let api = self.api.clone();
                    self.toast(ToastKind::Info, format!("Installing skill from {source}…"));
                    self.spawn(async move {
                        let list = api.install_skill(&source, !project).await?;
                        let names: Vec<String> = list.into_iter().map(|s| s.name).collect();
                        Ok(Some(Msg::Toast(
                            ToastKind::Success,
                            format!(
                                "Installed skill{}: {}",
                                if names.len() == 1 { "" } else { "s" },
                                names.join(", ")
                            ),
                        )))
                    });
                    self.refresh_meta();
                }
            }
            "web" => match self.web_url.clone() {
                Some(url) => {
                    crate::clipboard::copy(&url);
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener)
                        .arg(&url)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                    self.toast(ToastKind::Success, format!("Opened {url} (copied to clipboard)"));
                }
                None => self.toast(
                    ToastKind::Info,
                    "Web portal is off — run `lz web` or set tui.json web.enabled",
                ),
            },
            "exit" | "quit" | "q" => self.quit = true,
            _ => return false,
        }
        true
    }

    fn toggle_details(&mut self) {
        self.show_details = !self.show_details;
        self.kv.set("show_details", self.show_details.into());
        self.cache.clear();
    }
    fn toggle_thinking(&mut self) {
        self.show_thinking = !self.show_thinking;
        self.kv.set("show_thinking", self.show_thinking.into());
        self.cache.clear();
    }

    fn rename(&mut self, id: String, title: String) {
        let api = self.api.clone();
        self.spawn(async move {
            api.update_session(
                &id,
                SessionPatch {
                    title: Some(title),
                    ..Default::default()
                },
            )
            .await?;
            Ok(None)
        });
    }

    fn last_user_message(&self) -> Option<String> {
        let sid = self.session.as_ref()?;
        self.store
            .messages_of(sid)
            .iter()
            .rev()
            .find(|m| matches!(m, Message::User(_)))
            .map(|m| m.id().to_string())
    }

    fn undo(&mut self) {
        let Some(id) = self.session.clone() else { return };
        let Some(mid) = self.last_user_message() else {
            self.toast(ToastKind::Info, "Nothing to undo");
            return;
        };
        let api = self.api.clone();
        self.spawn(async move {
            api.revert(&id, &mid, None).await?;
            Ok(Some(Msg::Toast(ToastKind::Success, "Reverted last turn".into())))
        });
    }

    fn redo(&mut self) {
        let Some(id) = self.session.clone() else { return };
        let api = self.api.clone();
        self.spawn(async move {
            api.unrevert(&id).await?;
            Ok(Some(Msg::Toast(ToastKind::Success, "Restored".into())))
        });
    }

    fn last_assistant_text(&self) -> Option<String> {
        let sid = self.session.as_ref()?;
        for m in self.store.messages_of(sid).iter().rev() {
            if let Message::Assistant(a) = m {
                let text: Vec<String> = self
                    .store
                    .parts_of(&a.id)
                    .iter()
                    .filter_map(|p| {
                        if let PartKind::Text { text, .. } = &p.kind {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !text.is_empty() {
                    return Some(text.join("\n"));
                }
            }
        }
        None
    }

    fn copy_last(&mut self) {
        match self.last_assistant_text() {
            Some(t) => {
                crate::clipboard::copy(&t);
                self.toast(ToastKind::Success, "Copied last message");
            }
            None => self.toast(ToastKind::Info, "Nothing to copy"),
        }
    }

    fn export_markdown(&self) -> Option<String> {
        let sid = self.session.as_ref()?;
        let s = self.store.sessions.get(sid)?;
        let mut out = format!("# {}\n\n", s.title);
        for m in self.store.messages_of(sid) {
            let parts = self.store.parts_of(m.id());
            match m {
                Message::User(_) => {
                    let text: Vec<String> = parts
                        .iter()
                        .filter_map(|p| {
                            if let PartKind::Text {
                                text,
                                synthetic: false,
                                ..
                            } = &p.kind
                            {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !text.is_empty() {
                        out.push_str("## User\n\n");
                        out.push_str(&text.join("\n"));
                        out.push_str("\n\n");
                    }
                }
                Message::Assistant(a) => {
                    out.push_str(&format!("## Assistant ({})\n\n", a.model_id));
                    for p in parts {
                        match &p.kind {
                            PartKind::Text { text, .. } => {
                                out.push_str(text);
                                out.push_str("\n\n");
                            }
                            PartKind::Tool { tool, state, .. } => {
                                out.push_str(&format!(
                                    "**{tool}** `{}`\n\n",
                                    serde_json::to_string(state.input()).unwrap_or_default()
                                ));
                                if let ToolState::Completed { output, .. } = state {
                                    out.push_str("```\n");
                                    out.push_str(output.trim_end());
                                    out.push_str("\n```\n\n");
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Some(out)
    }

    fn export(&mut self) {
        let Some(md) = self.export_markdown() else {
            self.toast(ToastKind::Info, "No session to export");
            return;
        };
        let slug = self
            .session
            .as_ref()
            .and_then(|s| self.store.sessions.get(s))
            .map(|s| s.slug.clone())
            .unwrap_or_else(|| "session".into());
        let path = std::path::Path::new(
            &self
                .store
                .path
                .as_ref()
                .map(|p| p.directory.clone())
                .unwrap_or_else(|| ".".into()),
        )
        .join(format!("{slug}.md"));
        match std::fs::write(&path, md) {
            Ok(()) => self.toast(ToastKind::Success, format!("Exported to {}", path.display())),
            Err(e) => self.toast(ToastKind::Error, format!("Export failed: {e}")),
        }
    }

    // ───────────────────────────── dialogs ─────────────────────────────

    fn session_items(store: &Store) -> Vec<SelectItem> {
        let mut list: Vec<&SessionInfo> = store
            .sessions
            .values()
            .filter(|s| s.parent_id.is_none())
            .collect();
        list.sort_by_key(|s| std::cmp::Reverse(s.time.updated));
        list.into_iter()
            .take(200)
            .map(|s| {
                let busy = store.is_busy(&s.id);
                SelectItem::new(s.id.clone(), s.title.clone())
                    .desc(ago(s.time.updated))
                    .cat(
                        if s.directory == store.path.as_ref().map(|p| p.directory.as_str()).unwrap_or("") {
                            "This project"
                        } else {
                            "Other projects"
                        },
                    )
                    .hint(if busy { "● busy" } else { "" })
            })
            .collect()
    }

    fn open_sessions(&mut self) {
        let items = Self::session_items(&self.store);
        let mut d = SelectDialog::new(SelectKind::Sessions, "Sessions", items)
            .with_footer("enter open · ctrl+r rename · ctrl+d delete");
        if let Some(id) = &self.session {
            d.select_value(id);
        }
        self.dialogs.push(Dialog::Select(d));
        let api = self.api.clone();
        self.spawn(async move {
            Ok(Some(Msg::Sessions(
                api.list_sessions(SessionQuery {
                    roots: true,
                    limit: Some(200),
                    ..Default::default()
                })
                .await?,
            )))
        });
    }

    fn open_models(&mut self) {
        let mut items = Vec::new();
        let cur = self
            .model
            .as_ref()
            .map(|m| format!("{}/{}", m.provider_id, m.model_id));
        for key in &self.favorite_models {
            if let Some((p, m)) = key.split_once('/')
                && let Some(mi) = self.store.model_info(p, m)
            {
                items.push(
                    SelectItem::new(key.clone(), mi.name.clone())
                        .desc(p)
                        .cat("Favorites")
                        .hint("★"),
                );
            }
        }
        for key in &self.recent_models {
            if self.favorite_models.contains(key) {
                continue;
            }
            if let Some((p, m)) = key.split_once('/')
                && let Some(mi) = self.store.model_info(p, m)
            {
                items.push(
                    SelectItem::new(key.clone(), mi.name.clone())
                        .desc(p)
                        .cat("Recent"),
                );
            }
        }
        for p in &self.store.providers.providers {
            if !p.connected {
                continue;
            }
            for m in &p.models {
                let key = format!("{}/{}", p.id, m.id);
                let mut hint = String::new();
                if m.reasoning {
                    hint.push_str("reasoning ");
                }
                if self.favorite_models.contains(&key) {
                    hint.push('★');
                }
                items.push(
                    SelectItem::new(key, m.name.clone())
                        .desc(format!("{}k ctx", (m.limit.context / 1000.0) as u64))
                        .cat(p.name.clone())
                        .hint(hint.trim()),
                );
            }
        }
        if items.is_empty() {
            self.toast(ToastKind::Warning, "No connected providers — use /connect");
            return;
        }
        let mut d = SelectDialog::new(SelectKind::Models, "Models", items)
            .with_footer("enter select · ctrl+f favorite · ctrl+a providers");
        if let Some(c) = cur {
            d.select_value(&c);
        }
        self.dialogs.push(Dialog::Select(d));
    }

    fn open_providers(&mut self) {
        let mut items: Vec<SelectItem> = self
            .store
            .providers
            .providers
            .iter()
            .filter(|p| !p.local || p.connected)
            .map(|p| {
                let desc = if p.connected {
                    format!("connected via {}", p.source)
                } else if let Some(url) = &p.signup {
                    format!("get a key: {url}")
                } else if let Some(env) = p.env.first() {
                    format!("env {env}")
                } else {
                    String::new()
                };
                let cat = if p.connected {
                    "Connected"
                } else if p.free {
                    "Free tier — get a key in a minute"
                } else {
                    "Other providers"
                };
                SelectItem::new(p.id.clone(), p.name.clone())
                    .desc(desc)
                    .cat(cat)
                    .hint(if p.connected {
                        "●"
                    } else if p.free {
                        "free"
                    } else {
                        ""
                    })
            })
            .collect();
        items.sort_by_key(|i| {
            (
                i.category != "Connected",
                i.category != "Free tier — get a key in a minute",
                i.label.clone(),
            )
        });
        self.dialogs.push(Dialog::Select(
            SelectDialog::new(SelectKind::Providers, "Connect provider", items)
                .with_footer("enter to add an API key"),
        ));
    }

    fn open_agents(&mut self) {
        let items: Vec<SelectItem> = self
            .store
            .agents
            .iter()
            .filter(|a| !a.hidden)
            .map(|a| {
                SelectItem::new(a.name.clone(), a.name.clone())
                    .desc(a.description.clone().unwrap_or_default())
                    .cat(match a.mode {
                        lz_schema::config::AgentMode::Subagent => "Subagents",
                        _ => "Primary",
                    })
            })
            .collect();
        let mut d = SelectDialog::new(SelectKind::Agents, "Agents", items);
        d.select_value(&self.agent.clone());
        self.dialogs.push(Dialog::Select(d));
    }

    fn open_variants(&mut self) {
        let variants = self
            .model
            .as_ref()
            .and_then(|m| self.store.model_info(&m.provider_id, &m.model_id))
            .map(|mi| mi.variants.clone())
            .unwrap_or_default();
        if variants.is_empty() {
            self.toast(ToastKind::Info, "This model has no variants");
            return;
        }
        let mut items = vec![SelectItem::new("", "default")];
        items.extend(variants.into_iter().map(|v| SelectItem::new(v.clone(), v)));
        let mut d = SelectDialog::new(SelectKind::Variants, "Variants", items);
        d.select_value(self.variant.as_deref().unwrap_or(""));
        self.dialogs.push(Dialog::Select(d));
    }

    fn open_mcps(&mut self) {
        if self.store.mcp.is_empty() {
            self.toast(ToastKind::Info, "No MCP servers configured");
            return;
        }
        let items: Vec<SelectItem> = self
            .store
            .mcp
            .iter()
            .map(|(n, s)| {
                let (desc, hint) = match s {
                    McpStatus::Connected => ("connected".to_string(), "●"),
                    McpStatus::Disabled => ("disabled".to_string(), "○"),
                    McpStatus::Failed { error } => (format!("failed: {error}"), "✗"),
                    McpStatus::NeedsAuth => ("needs auth".to_string(), "!"),
                };
                SelectItem::new(n.clone(), n.clone()).desc(desc).hint(hint)
            })
            .collect();
        self.dialogs.push(Dialog::Select(
            SelectDialog::new(SelectKind::Mcp, "MCP servers", items).with_footer("enter toggle"),
        ));
    }

    fn open_skills(&mut self) {
        if self.store.skills.is_empty() {
            self.toast(ToastKind::Info, "No skills found");
            return;
        }
        let items: Vec<SelectItem> = self
            .store
            .skills
            .iter()
            .map(|s| SelectItem::new(s.name.clone(), s.name.clone()).desc(s.description.clone()))
            .collect();
        self.dialogs.push(Dialog::Select(
            SelectDialog::new(SelectKind::Skills, "Skills", items).with_footer("enter to insert /skill"),
        ));
    }

    fn open_themes(&mut self) {
        let dirs: Vec<&std::path::Path> = self.theme_dirs.iter().map(|p| p.as_path()).collect();
        let names = theme::available(&dirs);
        let items: Vec<SelectItem> = names.into_iter().map(|n| SelectItem::new(n.clone(), n)).collect();
        self.theme_backup = Some((self.theme_name.clone(), self.theme.clone()));
        let mut d =
            SelectDialog::new(SelectKind::Themes, "Themes", items).with_footer("↑/↓ preview · enter apply");
        d.select_value(&self.theme_name.clone());
        self.dialogs.push(Dialog::Select(d));
    }

    fn open_palette(&mut self) {
        let mut items: Vec<SelectItem> = SLASH
            .iter()
            .map(|(n, d)| {
                SelectItem::new(format!("/{n}"), format!("/{n}"))
                    .desc(*d)
                    .cat("Commands")
            })
            .collect();
        for c in &self.store.commands {
            if SLASH.iter().any(|(n, _)| *n == c.name) {
                continue;
            }
            items.push(
                SelectItem::new(format!("/{}", c.name), format!("/{}", c.name))
                    .desc(c.description.clone().unwrap_or_default())
                    .cat("Custom commands"),
            );
        }
        let mut keys: Vec<SelectItem> = self
            .keymap
            .bindings()
            .iter()
            .filter(|b| {
                !b.action.starts_with("input_")
                    && !b.action.starts_with("dialog_")
                    && !b.action.starts_with("autocomplete_")
                    && !b.chords.is_empty()
            })
            .map(|b| {
                SelectItem::new(format!("action:{}", b.action), b.description.clone())
                    .cat("Actions")
                    .hint(self.keymap.label(&b.action))
            })
            .collect();
        items.append(&mut keys);
        self.dialogs.push(Dialog::Select(SelectDialog::new(
            SelectKind::Palette,
            "Commands",
            items,
        )));
    }

    fn open_help(&mut self) {
        let theme = &self.theme;
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled("Keybinds", theme.bold("primary"))));
        for b in self.keymap.bindings() {
            if b.chords.is_empty() {
                continue;
            }
            let label = self.keymap.label(&b.action);
            lines.push(Line::from(vec![
                Span::styled(format!("{label:<28}"), theme.fg("accent")),
                Span::styled(b.description.clone(), theme.text()),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Slash commands", theme.bold("primary"))));
        for (n, d) in SLASH {
            lines.push(Line::from(vec![
                Span::styled(format!("/{n:<27}"), theme.fg("accent")),
                Span::styled(d.to_string(), theme.text()),
            ]));
        }
        for c in &self.store.commands {
            lines.push(Line::from(vec![
                Span::styled(format!("/{:<27}", c.name), theme.fg("accent")),
                Span::styled(
                    c.description.clone().unwrap_or_else(|| c.source.clone()),
                    theme.text(),
                ),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("↑/↓ scroll · esc close", theme.muted())));
        self.dialogs.push(Dialog::Text(TextDialog {
            title: "Help".into(),
            lines,
            scroll: 0,
        }));
    }

    fn open_status(&mut self) {
        let theme = &self.theme;
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled("LunarZero ", theme.bold("primary")),
            Span::styled(format!("v{}", self.version), theme.muted()),
        ]));
        if let Some(p) = &self.store.path {
            lines.push(Line::from(vec![
                Span::styled("Directory  ", theme.muted()),
                Span::styled(p.directory.clone(), theme.text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Worktree   ", theme.muted()),
                Span::styled(p.worktree.clone(), theme.text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Config     ", theme.muted()),
                Span::styled(p.config.clone(), theme.text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Data       ", theme.muted()),
                Span::styled(p.data.clone(), theme.text()),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Providers", theme.bold("primary"))));
        for p in &self.store.providers.providers {
            if p.connected {
                lines.push(Line::from(vec![
                    Span::styled("● ", theme.fg("success")),
                    Span::styled(format!("{} ", p.name), theme.text()),
                    Span::styled(
                        format!("({} models, via {})", p.models.len(), p.source),
                        theme.muted(),
                    ),
                ]));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("LSP", theme.bold("primary"))));
        if self.store.lsp.is_empty() {
            lines.push(Line::from(Span::styled(
                "none running (enable with \"lsp\": true)",
                theme.muted(),
            )));
        }
        for l in &self.store.lsp {
            lines.push(Line::from(vec![
                Span::styled(
                    if l.status == "connected" { "● " } else { "✗ " },
                    if l.status == "connected" {
                        theme.fg("success")
                    } else {
                        theme.fg("error")
                    },
                ),
                Span::styled(format!("{} ", l.name), theme.text()),
                Span::styled(l.root.clone(), theme.muted()),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("MCP", theme.bold("primary"))));
        if self.store.mcp.is_empty() {
            lines.push(Line::from(Span::styled("none configured", theme.muted())));
        }
        for (n, s) in &self.store.mcp {
            let (g, st, d) = match s {
                McpStatus::Connected => ("● ", theme.fg("success"), String::new()),
                McpStatus::Disabled => ("○ ", theme.muted(), "disabled".into()),
                McpStatus::Failed { error } => ("✗ ", theme.fg("error"), error.clone()),
                McpStatus::NeedsAuth => ("! ", theme.fg("warning"), "needs auth".into()),
            };
            lines.push(Line::from(vec![
                Span::styled(g, st),
                Span::styled(format!("{n} "), theme.text()),
                Span::styled(d, theme.muted()),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Theme      ", theme.muted()),
            Span::styled(
                format!(
                    "{} ({})",
                    self.theme_name,
                    if self.mode == Mode::Dark { "dark" } else { "light" }
                ),
                theme.text(),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Skills     ", theme.muted()),
            Span::styled(self.store.skills.len().to_string(), theme.text()),
        ]));
        self.dialogs.push(Dialog::Text(TextDialog {
            title: "Status".into(),
            lines,
            scroll: 0,
        }));
    }

    fn on_dialog_key(&mut self, k: &KeyEvent, actions: &[String]) {
        let has = |a: &str| actions.iter().any(|x| x == a);
        let Some(dialog) = self.dialogs.last_mut() else {
            return;
        };
        match dialog {
            Dialog::Select(d) => {
                let kind = d.kind;
                if has("dialog_close") {
                    self.dialogs.pop();
                    if kind == SelectKind::Themes
                        && let Some((name, t)) = self.theme_backup.take()
                    {
                        self.theme_name = name;
                        self.theme = t;
                        self.cache.clear();
                    }
                    return;
                }
                if has("dialog_select_prev") || matches!(k.code, KeyCode::Up) {
                    d.move_by(-1);
                    self.preview_theme();
                    return;
                }
                if has("dialog_select_next") || matches!(k.code, KeyCode::Down) {
                    d.move_by(1);
                    self.preview_theme();
                    return;
                }
                if matches!(k.code, KeyCode::PageUp) {
                    d.move_by(-10);
                    self.preview_theme();
                    return;
                }
                if matches!(k.code, KeyCode::PageDown) {
                    d.move_by(10);
                    self.preview_theme();
                    return;
                }
                // dialog-specific chords
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                if kind == SelectKind::Models && ctrl && matches!(k.code, KeyCode::Char('f')) {
                    if let Some(item) = d.current() {
                        let v = item.value.clone();
                        if let Some(p) = self.favorite_models.iter().position(|x| *x == v) {
                            self.favorite_models.remove(p);
                        } else {
                            self.favorite_models.push(v);
                        }
                        let f = self.favorite_models.clone();
                        self.kv.set_list("favorite_models", &f);
                        self.dialogs.pop();
                        self.open_models();
                    }
                    return;
                }
                if kind == SelectKind::Models && ctrl && matches!(k.code, KeyCode::Char('a')) {
                    self.dialogs.pop();
                    self.open_providers();
                    return;
                }
                if kind == SelectKind::Sessions && ctrl && matches!(k.code, KeyCode::Char('d')) {
                    if let Some(item) = d.current() {
                        let id = item.value.clone();
                        let title = item.label.clone();
                        self.dialogs.push(Dialog::Confirm(ConfirmDialog {
                            title: "Delete session".into(),
                            message: format!("Delete \"{title}\"? This cannot be undone."),
                            action: ConfirmAction::DeleteSession(id),
                            yes: false,
                        }));
                    }
                    return;
                }
                if kind == SelectKind::Sessions && ctrl && matches!(k.code, KeyCode::Char('r')) {
                    if let Some(item) = d.current() {
                        let id = item.value.clone();
                        let title = item.label.clone();
                        self.dialogs.push(Dialog::Input(InputDialog {
                            title: "Rename session".into(),
                            prompt: "New title".into(),
                            value: title,
                            masked: false,
                            action: InputAction::RenameSession(id),
                        }));
                    }
                    return;
                }
                if has("dialog_select_submit") {
                    let Some(item) = d.current().cloned() else { return };
                    self.dialogs.pop();
                    self.on_select(kind, item);
                    return;
                }
                match k.code {
                    KeyCode::Backspace => d.backspace(),
                    KeyCode::Char('u') if ctrl => d.clear_filter(),
                    KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => d.type_char(c),
                    _ => {}
                }
                if kind == SelectKind::Themes {
                    self.preview_theme();
                }
            }
            Dialog::Confirm(d) => match k.code {
                KeyCode::Esc => {
                    self.dialogs.pop();
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::Char('h') | KeyCode::Char('l') => {
                    d.yes = !d.yes
                }
                KeyCode::Char('y') => {
                    let action = d.action.clone();
                    self.dialogs.pop();
                    self.on_confirm(action);
                }
                KeyCode::Char('n') => {
                    self.dialogs.pop();
                }
                KeyCode::Enter => {
                    let yes = d.yes;
                    let action = d.action.clone();
                    self.dialogs.pop();
                    if yes {
                        self.on_confirm(action);
                    }
                }
                _ => {}
            },
            Dialog::Input(d) => match k.code {
                KeyCode::Esc => {
                    self.dialogs.pop();
                }
                KeyCode::Enter => {
                    let value = d.value.clone();
                    let action = d.action.clone();
                    self.dialogs.pop();
                    self.on_input(action, value);
                }
                KeyCode::Backspace => {
                    d.value.pop();
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => d.value.clear(),
                KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    d.value.push(c)
                }
                _ => {}
            },
            Dialog::Text(d) => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    self.dialogs.pop();
                }
                KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    d.scroll = d
                        .scroll
                        .saturating_add(1)
                        .min(d.lines.len().saturating_sub(3) as u16)
                }
                KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(10),
                KeyCode::PageDown => {
                    d.scroll = d
                        .scroll
                        .saturating_add(10)
                        .min(d.lines.len().saturating_sub(3) as u16)
                }
                _ => {}
            },
        }
    }

    fn preview_theme(&mut self) {
        let Some(Dialog::Select(d)) = self.dialogs.last() else {
            return;
        };
        if d.kind != SelectKind::Themes {
            return;
        }
        let Some(name) = d.current().map(|i| i.value.clone()) else {
            return;
        };
        if name == self.theme_name {
            return;
        }
        let dirs: Vec<&std::path::Path> = self.theme_dirs.iter().map(|p| p.as_path()).collect();
        if let Ok(t) = theme::load(&name, self.mode, &dirs, self.system_colors) {
            self.theme = t;
            self.theme_name = name;
            self.cache.clear();
        }
    }

    fn on_select(&mut self, kind: SelectKind, item: SelectItem) {
        match kind {
            SelectKind::Sessions => self.open_session(&item.value),
            SelectKind::Models => {
                if let Some(m) = self.resolve_model_spec(&item.value) {
                    self.set_model(m);
                }
            }
            SelectKind::Providers => {
                let signup = self
                    .store
                    .providers
                    .providers
                    .iter()
                    .find(|p| p.id == item.value)
                    .and_then(|p| p.signup.clone());
                if let Some(url) = &signup {
                    // open the signup page so the key is one paste away
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener)
                        .arg(url)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                }
                self.dialogs.push(Dialog::Input(InputDialog {
                    title: format!("Connect {}", item.label),
                    prompt: match signup {
                        Some(url) => format!("Paste your API key (signup page opened: {url})"),
                        None => "Paste your API key".into(),
                    },
                    value: String::new(),
                    masked: true,
                    action: InputAction::ProviderKey(item.value),
                }));
            }
            SelectKind::Agents => {
                let is_primary = self.store.agents.iter().any(|a| {
                    a.name == item.value && !matches!(a.mode, lz_schema::config::AgentMode::Subagent)
                });
                if is_primary {
                    self.agent = item.value;
                } else {
                    self.prompt.textarea.insert_str(&format!("@{} ", item.value));
                }
            }
            SelectKind::Variants => {
                self.variant = if item.value.is_empty() {
                    None
                } else {
                    Some(item.value)
                }
            }
            SelectKind::Themes => {
                self.theme_backup = None;
                self.kv.set("theme", self.theme_name.clone().into());
                self.toast(ToastKind::Success, format!("Theme: {}", self.theme_name));
            }
            SelectKind::Mcp => {
                let name = item.value;
                let connected = matches!(self.store.mcp.get(&name), Some(McpStatus::Connected));
                let api = self.api.clone();
                self.spawn(async move {
                    if connected {
                        api.mcp_disconnect(&name).await?
                    } else {
                        api.mcp_connect(&name).await?
                    }
                    Ok(None)
                });
            }
            SelectKind::Skills => {
                self.prompt.textarea.set_text(&format!("/{} ", item.value));
            }
            SelectKind::Commands => {
                self.prompt.textarea.set_text(&format!("/{} ", item.value));
            }
            SelectKind::Palette => {
                if let Some(action) = item.value.strip_prefix("action:") {
                    let action = action.to_string();
                    if !self.global_action(
                        &action,
                        &Chord::from_event(&KeyEvent::new(KeyCode::Null, KeyModifiers::NONE)),
                    ) {
                        let _ = self.prompt_action(&action);
                    }
                } else {
                    let name = item.value.trim_start_matches('/').to_string();
                    if !self.run_slash(&name, "") {
                        self.prompt.textarea.set_text(&format!("/{name} "));
                    }
                }
            }
            SelectKind::Fork | SelectKind::Export => {}
        }
    }

    fn on_confirm(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteSession(id) => {
                let api = self.api.clone();
                if self.session.as_deref() == Some(&id) {
                    self.session = None;
                }
                self.spawn(async move {
                    api.delete_session(&id).await?;
                    Ok(Some(Msg::Toast(ToastKind::Success, "Session deleted".into())))
                });
                if matches!(self.dialogs.last(), Some(Dialog::Select(d)) if d.kind == SelectKind::Sessions) {
                    self.dialogs.pop();
                    self.open_sessions();
                }
            }
            ConfirmAction::Exit => self.quit = true,
            ConfirmAction::Revert(_) => self.undo(),
            ConfirmAction::McpDisconnect(name) => {
                let api = self.api.clone();
                self.spawn(async move {
                    api.mcp_disconnect(&name).await?;
                    Ok(None)
                });
            }
        }
    }

    fn on_input(&mut self, action: InputAction, value: String) {
        match action {
            InputAction::RenameSession(id) => {
                if !value.trim().is_empty() {
                    self.rename(id, value.trim().to_string());
                }
            }
            InputAction::ProviderKey(provider) => {
                if value.trim().is_empty() {
                    return;
                }
                let api = self.api.clone();
                let p = provider.clone();
                self.spawn(async move {
                    api.set_auth(
                        &p,
                        AuthInfo::Api {
                            key: value.trim().to_string(),
                            metadata: None,
                        },
                    )
                    .await?;
                    Ok(Some(Msg::Toast(ToastKind::Success, format!("Connected {p}"))))
                });
                self.refresh_meta();
            }
            InputAction::RejectReason(id) => {
                let api = self.api.clone();
                self.spawn(async move {
                    api.reply_permission(
                        &id,
                        PermissionReplyRequest {
                            reply: PermissionReply::Reject,
                            message: Some(value).filter(|s| !s.trim().is_empty()),
                        },
                    )
                    .await?;
                    Ok(None)
                });
            }
            InputAction::QuestionCustom { question_id, index } => {
                if let Some((qid, panel)) = &mut self.question
                    && *qid == question_id
                    && index < panel.custom.len()
                {
                    panel.custom[index] = Some(value);
                    if let Some(req) = self.store.questions.iter().find(|q| q.id == question_id)
                        && !req.questions[index].multiple.unwrap_or(false)
                    {
                        panel.selected[index].clear();
                    }
                }
            }
            InputAction::ExportPath(_) => {}
        }
    }

    // ───────────────────────────── permission / question keys ─────────────────────────────

    fn reply_permission(&mut self, id: String, reply: PermissionReply) {
        let api = self.api.clone();
        self.spawn(async move {
            api.reply_permission(&id, PermissionReplyRequest { reply, message: None })
                .await?;
            Ok(None)
        });
        self.perm = PermissionPanel::default();
    }

    fn on_permission_key(&mut self, k: &KeyEvent, actions: &[String], req: &PermissionRequest) {
        let has = |a: &str| actions.iter().any(|x| x == a);
        if has("permission_fullscreen") {
            self.perm.fullscreen = !self.perm.fullscreen;
            return;
        }
        match k.code {
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => self.perm.prev(),
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => self.perm.next(),
            KeyCode::Up | KeyCode::Char('k') => self.perm.scroll = self.perm.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.perm.scroll = self.perm.scroll.saturating_add(1),
            KeyCode::PageUp => self.perm.scroll = self.perm.scroll.saturating_sub(10),
            KeyCode::PageDown => self.perm.scroll = self.perm.scroll.saturating_add(10),
            KeyCode::Char('a') | KeyCode::Char('y') => {
                self.reply_permission(req.id.clone(), PermissionReply::Once)
            }
            KeyCode::Char('A') | KeyCode::Char('Y') => {
                self.reply_permission(req.id.clone(), PermissionReply::Always)
            }
            KeyCode::Char('n') | KeyCode::Char('d') | KeyCode::Esc => {
                self.reply_permission(req.id.clone(), PermissionReply::Reject)
            }
            KeyCode::Char('r') => {
                self.dialogs.push(Dialog::Input(InputDialog {
                    title: "Reject with feedback".into(),
                    prompt: "Tell the agent what to do instead".into(),
                    value: String::new(),
                    masked: false,
                    action: InputAction::RejectReason(req.id.clone()),
                }));
            }
            KeyCode::Enter => {
                let reply = match self.perm.choice {
                    PermChoice::Once => PermissionReply::Once,
                    PermChoice::Always => PermissionReply::Always,
                    PermChoice::Reject => PermissionReply::Reject,
                };
                self.reply_permission(req.id.clone(), reply);
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            _ => {}
        }
    }

    fn on_question_key(&mut self, k: &KeyEvent, req: &QuestionRequest) {
        if self
            .question
            .as_ref()
            .map(|(id, _)| id != &req.id)
            .unwrap_or(true)
        {
            self.question = Some((req.id.clone(), QuestionPanel::new(req)));
        }
        let Some((_, panel)) = &mut self.question else {
            return;
        };
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                panel.cursor = panel.cursor.checked_sub(1).unwrap_or(panel.option_count(req) - 1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                panel.cursor = (panel.cursor + 1) % panel.option_count(req).max(1)
            }
            KeyCode::Left | KeyCode::BackTab => {
                panel.tab = panel.tab.checked_sub(1).unwrap_or(req.questions.len() - 1);
                panel.cursor = 0;
            }
            KeyCode::Right | KeyCode::Tab => {
                panel.tab = (panel.tab + 1) % req.questions.len();
                panel.cursor = 0;
            }
            KeyCode::Char(' ') => {
                if panel.is_custom_row(req) {
                    let (tab, id) = (panel.tab, req.id.clone());
                    let existing = panel.custom[tab].clone().unwrap_or_default();
                    self.dialogs.push(Dialog::Input(InputDialog {
                        title: req.questions[tab].header.clone(),
                        prompt: req.questions[tab].question.clone(),
                        value: existing,
                        masked: false,
                        action: InputAction::QuestionCustom {
                            question_id: id,
                            index: tab,
                        },
                    }));
                } else {
                    panel.toggle(req);
                }
            }
            KeyCode::Enter => {
                if panel.is_custom_row(req) && panel.custom[panel.tab].is_none() {
                    let (tab, id) = (panel.tab, req.id.clone());
                    self.dialogs.push(Dialog::Input(InputDialog {
                        title: req.questions[tab].header.clone(),
                        prompt: req.questions[tab].question.clone(),
                        value: String::new(),
                        masked: false,
                        action: InputAction::QuestionCustom {
                            question_id: id,
                            index: tab,
                        },
                    }));
                    return;
                }
                if !panel.is_custom_row(req) && panel.selected[panel.tab].is_empty() {
                    panel.toggle(req);
                }
                let unanswered = (0..req.questions.len())
                    .find(|&i| panel.selected[i].is_empty() && panel.custom[i].is_none());
                if let Some(i) = unanswered {
                    panel.tab = i;
                    panel.cursor = 0;
                    return;
                }
                let answers = panel.answers(req);
                let api = self.api.clone();
                let id = req.id.clone();
                self.spawn(async move {
                    api.reply_question(&id, answers).await?;
                    Ok(None)
                });
                self.question = None;
            }
            KeyCode::Esc => {
                let api = self.api.clone();
                let id = req.id.clone();
                self.spawn(async move {
                    api.reject_question(&id).await?;
                    Ok(None)
                });
                self.question = None;
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            _ => {}
        }
    }

    // ───────────────────────────── view ─────────────────────────────

    pub fn view(&mut self, f: &mut Frame) {
        let area = f.area();
        self.area = area;
        f.render_widget(
            ratatui::widgets::Block::default().style(self.theme.bg("background")),
            area,
        );
        let [body, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        match self.session.clone() {
            Some(sid) => self.view_session(f, body, &sid),
            None => self.view_home(f, body),
        }
        self.view_footer(f, footer);
        if let Some(d) = self.dialogs.last() {
            let theme = &self.theme;
            match d {
                Dialog::Select(d) => d.render(f, area, theme),
                Dialog::Confirm(d) => d.render(f, area, theme),
                Dialog::Input(d) => d.render(f, area, theme),
                Dialog::Text(d) => d.render(f, area, theme),
            }
        }
        // notifications live in the right column, never over the transcript
        let strip = self.notify_area.unwrap_or(Rect {
            x: area.x + area.width.saturating_sub(42),
            y: area.y,
            width: 42.min(area.width),
            height: area.height.saturating_sub(4),
        });
        self.toasts.render(f, strip, &self.theme);
        if self.leader.is_some() {
            let hint = "leader… (n new · l sessions · m models · a agents · t themes · b sidebar · c compact · u undo · r redo · e editor · x export · ? help · q quit)";
            let w = (hint.chars().count() as u16 + 2).min(area.width);
            let rect = Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(2),
                width: w,
                height: 1,
            };
            f.render_widget(ratatui::widgets::Clear, rect);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {hint} "),
                    self.theme.bg("backgroundElement").fg(self.theme.color("primary")),
                ))),
                rect,
            );
        }
    }

    fn bottom_height(&mut self, sid: &str, width: u16, max: u16) -> u16 {
        if let Some(req) = self.store.permission_for(sid) {
            if self.perm.fullscreen {
                return max;
            }
            return self.perm.height(req, width, &self.theme, max.min(20));
        }
        if let Some(req) = self.store.question_for(sid) {
            let panel = match &self.question {
                Some((id, p)) if id == &req.id => p.height(req, max.min(20)),
                _ => QuestionPanel::new(req).height(req, max.min(20)),
            };
            return panel;
        }
        self.prompt.height(width) + 2
    }

    fn view_session(&mut self, f: &mut Frame, area: Rect, sid: &str) {
        let show_sidebar = self.sidebar && area.width >= 110;
        let [main, side] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(if show_sidebar { 36 } else { 0 }),
        ])
        .areas(area);
        self.notify_area = if show_sidebar {
            // below the sidebar's own content: bottom half of the column
            Some(Rect {
                x: side.x,
                y: side.y + side.height / 2,
                width: side.width,
                height: side.height - side.height / 2,
            })
        } else {
            None
        };
        let bottom_h = self.bottom_height(sid, main.width, area.height.saturating_sub(3));
        let [list_area, bottom] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(bottom_h)]).areas(main);
        self.view_messages(f, list_area, sid);
        if show_sidebar {
            sidebar::render(f, side, &self.store, sid, &self.theme);
        }
        if let Some(req) = self.store.permission_for(sid).cloned() {
            let full = self.perm.fullscreen;
            self.perm.render(f, bottom, &req, &self.theme, full);
        } else if let Some(req) = self.store.question_for(sid).cloned() {
            if self
                .question
                .as_ref()
                .map(|(id, _)| id != &req.id)
                .unwrap_or(true)
            {
                self.question = Some((req.id.clone(), QuestionPanel::new(&req)));
            }
            if let Some((_, p)) = &self.question {
                p.render(f, bottom, &req, &self.theme);
            }
        } else {
            let hint = self.prompt_hint();
            let focused = self.dialogs.is_empty();
            self.prompt.render(f, bottom, &self.theme, focused, &hint);
            self.prompt.render_autocomplete(f, bottom, &self.theme);
        }
    }

    fn prompt_hint(&self) -> String {
        let submit = self.keymap.label("input_submit");
        let nl = self
            .keymap
            .chords_for("input_newline")
            .first()
            .map(|c| c.label())
            .unwrap_or_else(|| "shift+enter".into());
        format!(" {submit} send · {nl} newline · ctrl+p commands ")
    }

    fn view_messages(&mut self, f: &mut Frame, area: Rect, sid: &str) {
        let width = area.width.saturating_sub(2) as usize;
        let opts = RenderOpts {
            show_thinking: self.show_thinking,
            show_details: self.show_details,
            width,
        };
        let blocks = self.cache.render_session(&self.store, sid, &self.theme, &opts);
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                lines.push(Line::from(""));
            }
            lines.extend(b.lines.iter().cloned());
        }
        // status line at the bottom
        match self.store.status_of(sid) {
            SessionStatus::Busy => {
                let frames = ["◐", "◓", "◑", "◒"];
                let g = frames[(self.tick / 2) as usize % frames.len()];
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(format!("{g} "), self.theme.fg("primary")),
                    Span::styled("Working…", self.theme.muted()),
                    Span::styled(
                        format!("  {} to interrupt", self.keymap.label("session_interrupt")),
                        self.theme.muted(),
                    ),
                ]));
            }
            SessionStatus::Retry {
                attempt,
                message,
                next,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let secs = next.saturating_sub(now) / 1000;
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("↻ ", self.theme.fg("warning")),
                    Span::styled(
                        if message.starts_with("waiting") {
                            format!("{message} — {secs}s left")
                        } else {
                            format!("Retry {attempt} in {secs}s: {message}")
                        },
                        self.theme.fg("warning"),
                    ),
                ]));
            }
            SessionStatus::Idle => {
                if !self.store.loaded.contains(sid) && lines.is_empty() {
                    lines.push(Line::from(Span::styled("Loading…", self.theme.muted())));
                }
            }
        }
        if let Some(s) = self.store.sessions.get(sid)
            && let Some(r) = &s.revert
        {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("⟲ ", self.theme.fg("warning")),
                Span::styled(
                    format!(
                        "Reverted to {} — /redo to restore, or send a message to continue",
                        r.message_id
                    ),
                    self.theme.fg("warning"),
                ),
            ]));
        }
        let total = lines.len();
        let height = area.height as usize;
        self.last_total_lines = total;
        self.last_list_height = height;
        let max = total.saturating_sub(height);
        if self.follow {
            self.scroll = max;
        } else {
            self.scroll = self.scroll.min(max);
        }
        let visible: Vec<Line<'static>> = lines.into_iter().skip(self.scroll).take(height).collect();
        let inner = Rect {
            x: area.x + 1,
            y: area.y,
            width: area.width.saturating_sub(2),
            height: area.height,
        };
        f.render_widget(Paragraph::new(visible), inner);
        if !self.follow && max > 0 {
            let pct = (self.scroll as f64 / max as f64 * 100.0) as u64;
            let label = format!(" {pct}% ↓ ");
            let rect = Rect {
                x: area.x + area.width.saturating_sub(label.len() as u16 + 1),
                y: area.y + area.height.saturating_sub(1),
                width: label.len() as u16,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    label,
                    self.theme
                        .bg("backgroundElement")
                        .fg(self.theme.color("textMuted")),
                ))),
                rect,
            );
        }
    }

    fn view_home(&mut self, f: &mut Frame, area: Rect) {
        self.notify_area = None;
        let prompt_h = self.prompt.height(area.width.min(100)) + 2;
        let logo: Vec<&str> = vec![
            "██╗     ██╗   ██╗███╗   ██╗ █████╗ ██████╗ ",
            "██║     ██║   ██║████╗  ██║██╔══██╗██╔══██╗",
            "██║     ██║   ██║██╔██╗ ██║███████║██████╔╝",
            "██║     ██║   ██║██║╚██╗██║██╔══██║██╔══██╗",
            "███████╗╚██████╔╝██║ ╚████║██║  ██║██║  ██║",
            "╚══════╝ ╚═════╝ ╚═╝  ╚═══╝╚═╝  ╚═╝╚═╝  ╚═╝",
            "                   Z E R O                 ",
        ];
        let connected = self.booted && self.store.providers.providers.iter().any(|p| p.connected);
        let portal = self
            .web_url
            .as_deref()
            .map(|u| u.split("/?").next().unwrap_or(u).to_string())
            .unwrap_or_else(|| "lz web".into());
        let onboarding = [
            "No model connected yet — pick any one of these:".to_string(),
            "  /connect      paste a free API key (Groq, Cerebras, Google AI Studio, OpenRouter… a minute each, no card)".to_string(),
            format!("  {portal}   the portal's API keys tab has the signup links"),
            "  local         start Ollama or LM Studio — it is picked up automatically".to_string(),
            "  terminal      lz setup   (guided)".to_string(),
        ];
        let tips: Vec<String> = if !connected && self.booted {
            onboarding.to_vec()
        } else {
            vec![
                "Type a message and press enter to start a session".into(),
                "Use @ to mention files, / for commands, ! to run shell".into(),
                "ctrl+x then n/l/m/a: new · sessions · models · agents".into(),
                "ctrl+p opens the command palette".into(),
            ]
        };
        let show_tips = self.tips || !connected;
        let content_h = logo.len() as u16 + 2 + prompt_h + if show_tips { tips.len() as u16 + 1 } else { 0 };
        let top = area.y + area.height.saturating_sub(content_h) / 2;
        let w = area.width.min(100);
        let x = area.x + (area.width - w) / 2;
        let mut y = top;
        for (i, l) in logo.iter().enumerate() {
            let style = if i == logo.len() - 1 {
                self.theme.muted()
            } else {
                self.theme.fg("primary")
            };
            let rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(l.to_string(), style))).alignment(Alignment::Center),
                rect,
            );
            y += 1;
        }
        y += 1;
        let prompt_area = Rect {
            x,
            y,
            width: w,
            height: prompt_h.min(area.height.saturating_sub(y - area.y)),
        };
        let hint = self.prompt_hint();
        let focused = self.dialogs.is_empty();
        self.prompt.render(f, prompt_area, &self.theme, focused, &hint);
        y += prompt_h + 1;
        if show_tips {
            let left_align = !connected;
            for (i, t) in tips.iter().enumerate() {
                if y >= area.y + area.height {
                    break;
                }
                let style = if left_align && i == 0 {
                    self.theme.bold("warning")
                } else {
                    self.theme.muted()
                };
                let rect = Rect {
                    x: if left_align { x } else { area.x },
                    y,
                    width: if left_align { w } else { area.width },
                    height: 1,
                };
                let mut para = Paragraph::new(Line::from(Span::styled(t.to_string(), style)));
                if !left_align {
                    para = para.alignment(Alignment::Center);
                }
                f.render_widget(para, rect);
                y += 1;
            }
        }
        self.prompt.render_autocomplete(f, prompt_area, &self.theme);
    }

    fn view_footer(&mut self, f: &mut Frame, area: Rect) {
        let theme = &self.theme;
        let model = self.model.as_ref().map(|m| {
            let name = self
                .store
                .model_info(&m.provider_id, &m.model_id)
                .map(|mi| mi.name.clone())
                .unwrap_or_else(|| m.model_id.clone());
            match &self.variant {
                Some(v) => format!("{name} ({v})"),
                None => name,
            }
        });
        let mut left = vec![
            Span::styled(
                format!(" {} ", self.agent),
                theme
                    .bg("primary")
                    .fg(theme.color("background"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ];
        match model {
            Some(m) => left.push(Span::styled(m, theme.text())),
            None => left.push(Span::styled("no model", theme.fg("error"))),
        }
        if self.model.as_ref().is_some_and(|m| m.provider_id == "lunar")
            && let Some((p, m, _)) = self.session.as_ref().and_then(|s| self.store.routed.get(s))
        {
            left.push(Span::styled(format!(" → {p}/{m}"), theme.muted()));
        }
        if let Some(sid) = &self.session
            && let Some(s) = self.store.sessions.get(sid)
        {
            let (total, cost) = self.store.usage(sid);
            if total > 0.0 {
                let limit = s
                    .model
                    .as_ref()
                    .and_then(|m| self.store.model_info(&m.provider_id, &m.id))
                    .map(|mi| mi.limit.context)
                    .unwrap_or(0.0);
                let pct = if limit > 0.0 {
                    format!(" ({:.0}%)", total / limit * 100.0)
                } else {
                    String::new()
                };
                left.push(Span::styled(
                    format!("  ·  {}{pct}  ·  ${cost:.3}", fmt_tokens(total)),
                    theme.muted(),
                ));
            }
        }
        let dir = self
            .store
            .path
            .as_ref()
            .map(|p| p.directory.clone())
            .unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = if !home.is_empty() && dir.starts_with(&home) {
            format!("~{}", &dir[home.len()..])
        } else {
            dir
        };
        let right = match &self.web_url {
            Some(url) => {
                let shown = url.split("/?").next().unwrap_or(url).to_string();
                Span::styled(format!("⌂ {shown}  (/web opens it) "), theme.fg("accent"))
            }
            None => Span::styled(format!("{dir} "), theme.muted()),
        };
        let left_line = Line::from(left);
        let lw = left_line.width() as u16;
        f.render_widget(
            Paragraph::new(left_line),
            Rect {
                x: area.x,
                y: area.y,
                width: lw.min(area.width),
                height: 1,
            },
        );
        let rw = right.width() as u16;
        if lw + rw + 2 < area.width {
            f.render_widget(
                Paragraph::new(Line::from(right)).alignment(Alignment::Right),
                Rect {
                    x: area.x + lw + 1,
                    y: area.y,
                    width: area.width - lw - 1,
                    height: 1,
                },
            );
        }
        let _ = syntax::lang_token("");
    }
}

fn ago(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let d = now.saturating_sub(ts) / 1000;
    if d < 60 {
        "just now".into()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}
