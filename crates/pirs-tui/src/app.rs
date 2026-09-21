//! The UI's state and every change to it. `App` reacts to keys from a driver
//! and to messages from the I/O tasks; it never blocks on the server, it
//! issues requests through [`Io`] and hears back as [`Msg`]s.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pirs_client::Client;
use pirs_protocol::{
    Content, Delta, Event, LoopCreateParams, LoopListParams, LoopMessageBody, LoopMessageEvent,
    LoopSelector, LoopState, LoopStatusEvent, Message, NotifyLevel, PromptWhen, Ref, ServerPath,
    UserContent,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::config::{Action, Config, ConfigFile, Loaded, Theme};
use crate::keys::{Code, Key};
use crate::model::{
    fences, Agent, Block, Builtin, CommandEntry, CommandSide, Entry, HookTarget, LoopKey, Mode,
    Msg, Notice, Page, PickAction, Picker, PromptAction, StoredConversation, UiAction,
};
use crate::process::{self, HookOutput};

/// How long a notice stays on the bottom line.
pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(5);

/// Lines of a tool result shown when collapsed.
pub(crate) const RESULT_PREVIEW_LINES: usize = 4;

/// The request side: every server connection and the channel results come
/// back on. Cheap to clone; every request runs in its own task.
#[derive(Clone)]
pub(crate) struct Io {
    servers: Arc<BTreeMap<String, Client>>,
    tx: mpsc::UnboundedSender<Msg>,
    cwd: PathBuf,
}

impl Io {
    pub(crate) fn new(
        servers: BTreeMap<String, Client>,
        tx: mpsc::UnboundedSender<Msg>,
        cwd: PathBuf,
    ) -> Io {
        Io {
            servers: Arc::new(servers),
            tx,
            cwd,
        }
    }

    pub(crate) fn server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    fn client(&self, server: &str) -> Option<Client> {
        self.servers.get(server).cloned()
    }

    /// Run a request whose only interesting outcome is failure.
    fn fire<F>(&self, what: String, future: F)
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(error) = future.await {
                let _ = tx.send(Msg::Failed { what, error });
            }
        });
    }

    pub(crate) fn subscribe_status(&self, server: &str) {
        let Some(client) = self.client(server) else {
            return;
        };
        self.fire(format!("subscribe * on {server}"), async move {
            client
                .subscribe_with_replay(LoopSelector::All, Some(vec!["loop.status".into()]), None)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    pub(crate) fn list(&self, server: &str, cwd: Option<ServerPath>) {
        let Some(client) = self.client(server) else {
            return;
        };
        let tx = self.tx.clone();
        let server = server.to_owned();
        tokio::spawn(async move {
            let result = client
                .loop_list(LoopListParams { cwd })
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Listed { server, result });
        });
    }

    pub(crate) fn attach(&self, key: LoopKey) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .loop_attach(&key.loop_id)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::Attached { key, result });
        });
    }

    pub(crate) fn subscribe(&self, key: LoopKey, since: u64) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        self.fire(format!("subscribe {}", key.loop_id), async move {
            client
                .subscribe_with_replay(LoopSelector::Loop(key.loop_id), None, Some(since))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    pub(crate) fn unsubscribe(&self, key: LoopKey) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        self.fire(format!("unsubscribe {}", key.loop_id), async move {
            client
                .unsubscribe(LoopSelector::Loop(key.loop_id))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    pub(crate) fn prompt(&self, key: LoopKey, text: String, when: PromptWhen) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        self.fire(format!("loop.prompt {}", key.loop_id), async move {
            client
                .loop_prompt(&key.loop_id, text, when)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    pub(crate) fn abort(&self, key: LoopKey) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        self.fire(format!("loop.abort {}", key.loop_id), async move {
            client
                .loop_abort(&key.loop_id)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    pub(crate) fn close(&self, key: LoopKey) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        self.fire(format!("loop.close {}", key.loop_id), async move {
            client
                .loop_close(&key.loop_id)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    pub(crate) fn create(&self, server: &str, params: LoopCreateParams) {
        let Some(client) = self.client(server) else {
            return;
        };
        let tx = self.tx.clone();
        let server = server.to_owned();
        tokio::spawn(async move {
            let result = client.loop_create(params).await.map_err(|e| e.to_string());
            let _ = tx.send(Msg::Created { server, result });
        });
    }

    pub(crate) fn read_file(&self, key: LoopKey, path: ServerPath) {
        let Some(client) = self.client(&key.server) else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = match client.fs_read(&key.loop_id, path.clone()).await {
                Ok(pirs_protocol::FsReadResult::Content { content }) => Ok(content),
                Ok(pirs_protocol::FsReadResult::Ref(r)) => {
                    // D-39: an explicit read is served in full; a server that
                    // still answers with a ref is asked once more for it.
                    match client.fs_read(&key.loop_id, r.path).await {
                        Ok(pirs_protocol::FsReadResult::Content { content }) => Ok(content),
                        Ok(pirs_protocol::FsReadResult::Ref(r)) => {
                            Err(format!("{} bytes, by reference only", r.bytes))
                        }
                        Err(e) => Err(e.to_string()),
                    }
                }
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Msg::FileRead { key, path, result });
        });
    }

    pub(crate) fn hook(&self, key: LoopKey, target: HookTarget, run: String, input: Value) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = process::run_hook(&run, &input).await;
            let _ = tx.send(Msg::Hook {
                key,
                target,
                result,
            });
        });
    }

    pub(crate) fn shell(&self, name: String, command: String) {
        let tx = self.tx.clone();
        let cwd = self.cwd.clone();
        tokio::spawn(async move {
            let result = process::run_shell(&command, &cwd).await;
            let _ = tx.send(Msg::Shell { name, result });
        });
    }

    pub(crate) fn editor(&self, prefix: String, path: String) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let text = match process::open_editor_pane(&prefix, &path).await {
                Ok(()) => format!("editor opened on {path}"),
                Err(e) => e,
            };
            let _ = tx.send(Msg::Notice(text));
        });
    }
}

/// The sidebar's rows, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarItem {
    Agent(usize),
    Conversation(usize),
}

/// The whole UI state.
pub(crate) struct App {
    pub config: Config,
    pub bindings: HashMap<Key, Action>,
    pub theme: Theme,
    config_file: ConfigFile,
    pub agents: Vec<Agent>,
    pub conversations: Vec<StoredConversation>,
    pub sidebar: usize,
    pub pages: Vec<Page>,
    pub page: usize,
    pub mode: Mode,
    pub input: String,
    pub notice: Option<Notice>,
    pub quit: bool,
    /// Servers whose connection dropped.
    pub lost: BTreeSet<String>,
    /// The TUI's own cwd, as the label it sends in `loop.list { cwd }`.
    pub cwd: String,
    /// Rows the body had at the last draw, for paging.
    pub body_height: usize,
    /// The row the visible page's body started at, and the largest start
    /// that still fills the body; the renderer reports both.
    pub body_view: (usize, usize),
    io: Io,
    /// The first `loop.list` answer selects the first agent.
    started: bool,
}

impl App {
    /// Build the state and issue the start-up requests: `subscribe *` for
    /// `loop.status`, then `loop.list { cwd }`, on every server.
    pub(crate) fn new(config_file: ConfigFile, io: Io, cwd: String) -> App {
        let mut app = App {
            config: Config::default(),
            bindings: Config::default()
                .keys
                .bindings()
                .expect("the default bindings parse"),
            theme: Theme::Dark,
            config_file,
            agents: Vec::new(),
            conversations: Vec::new(),
            sidebar: 0,
            pages: Vec::new(),
            page: 0,
            mode: Mode::Normal,
            input: String::new(),
            notice: None,
            quit: false,
            lost: BTreeSet::new(),
            cwd,
            body_height: 10,
            body_view: (0, 0),
            io: io.clone(),
            started: false,
        };
        // Temporarily take the file to read it; `config_file` is a field.
        let loaded = app.config_file.load();
        app.apply_loaded(loaded, false);
        for server in io.server_names() {
            io.subscribe_status(&server);
            io.list(&server, Some(ServerPath::from(app.cwd.clone())));
        }
        app
    }

    // ----- config ---------------------------------------------------------

    fn apply_loaded(&mut self, loaded: Loaded, announce: bool) {
        match loaded {
            Loaded::Config(config) => match config.keys.bindings() {
                Ok(bindings) => {
                    self.theme = config.theme;
                    self.bindings = bindings;
                    self.config = *config;
                    if announce {
                        self.notice(NotifyLevel::Info, "tui.toml reloaded");
                    }
                    // A `[[command]]` may take a built-in name, and then it
                    // wins (`commands`); say so once, so the built-in does
                    // not go missing silently.
                    let shadowed: Vec<String> = self
                        .config
                        .commands
                        .iter()
                        .map(|c| c.name.clone())
                        .filter(|name| Builtin::from_name(name).is_some())
                        .collect();
                    if !shadowed.is_empty() {
                        let names = shadowed
                            .iter()
                            .map(|n| format!("`{n}`"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let what = if shadowed.len() == 1 {
                            "the built-in"
                        } else {
                            "the built-ins"
                        };
                        self.notice(
                            NotifyLevel::Warning,
                            format!("command {names} overrides {what}"),
                        );
                    }
                }
                Err(e) => self.notice(NotifyLevel::Error, format!("tui.toml: {e}")),
            },
            Loaded::Error(e) => self.notice(NotifyLevel::Error, format!("tui.toml: {e}")),
        }
    }

    /// Re-read the config if the file changed (called once a second).
    pub(crate) fn poll_config(&mut self) {
        if let Some(loaded) = self.config_file.poll() {
            self.apply_loaded(loaded, true);
        }
    }

    fn reload_config(&mut self) {
        let loaded = self.config_file.load();
        self.apply_loaded(loaded, true);
    }

    // ----- lookups --------------------------------------------------------

    pub(crate) fn sidebar_items(&self) -> Vec<SidebarItem> {
        (0..self.agents.len())
            .map(SidebarItem::Agent)
            .chain((0..self.conversations.len()).map(SidebarItem::Conversation))
            .collect()
    }

    pub(crate) fn agent(&self, key: &LoopKey) -> Option<&Agent> {
        self.agents.iter().find(|a| &a.key == key)
    }

    pub(crate) fn agent_mut(&mut self, key: &LoopKey) -> Option<&mut Agent> {
        self.agents.iter_mut().find(|a| &a.key == key)
    }

    pub(crate) fn visible_page(&self) -> Option<&Page> {
        self.pages.get(self.page)
    }

    /// The loop whose agent page is showing, if one is.
    pub(crate) fn visible_agent_key(&self) -> Option<LoopKey> {
        match self.visible_page() {
            Some(Page::Agent { key, .. }) => Some(key.clone()),
            _ => None,
        }
    }

    /// The agent the input line talks to: the visible page's, else the
    /// sidebar cursor's, else the first.
    pub(crate) fn current_key(&self) -> Option<LoopKey> {
        if let Some(page) = self.visible_page() {
            return Some(page.key().clone());
        }
        match self.sidebar_items().get(self.sidebar) {
            Some(SidebarItem::Agent(i)) => self.agents.get(*i).map(|a| a.key.clone()),
            _ => self.agents.first().map(|a| a.key.clone()),
        }
    }

    pub(crate) fn current_agent(&self) -> Option<&Agent> {
        self.current_key().and_then(|k| self.agent(&k))
    }

    /// Whether the visible page is that agent's page; drawing it counts as
    /// viewing (S12).
    pub(crate) fn before_draw(&mut self) {
        if let Some(key) = self.visible_agent_key() {
            if let Some(agent) = self.agent_mut(&key) {
                agent.unviewed = false;
            }
        }
        if let Some(notice) = &self.notice {
            if notice.at.elapsed() > NOTICE_TTL {
                self.notice = None;
            }
        }
    }

    pub(crate) fn notice(&mut self, level: NotifyLevel, text: impl Into<String>) {
        self.notice = Some(Notice {
            at: Instant::now(),
            level,
            text: text.into(),
        });
    }

    /// The merged command list: the attach manifest's commands and the UI's
    /// own; a name on both sides is a conflict (D-32).
    pub(crate) fn commands(&self) -> Vec<CommandEntry> {
        let mut ui: BTreeMap<String, (String, UiAction)> = BTreeMap::new();
        for b in Builtin::ALL {
            ui.insert(
                b.name().to_owned(),
                (b.description().to_owned(), UiAction::Builtin(b)),
            );
        }
        for c in &self.config.commands {
            let action = match Builtin::from_name(&c.action) {
                Some(b) => UiAction::Builtin(b),
                None => UiAction::Shell(c.action.clone()),
            };
            ui.insert(c.name.clone(), (c.description.clone(), action));
        }
        let mut server: BTreeMap<String, String> = BTreeMap::new();
        if let Some(manifest) = self.current_agent().and_then(|a| a.manifest.as_ref()) {
            for c in &manifest.commands {
                server.insert(c.name.clone(), c.description.clone());
            }
        }
        let names: BTreeSet<&String> = ui.keys().chain(server.keys()).collect();
        names
            .into_iter()
            .map(|name| match (ui.get(name), server.get(name)) {
                (Some(_), Some(_)) => CommandEntry {
                    name: name.clone(),
                    description: "defined by both the server and the UI".to_owned(),
                    side: CommandSide::Conflict,
                },
                (Some((description, action)), None) => CommandEntry {
                    name: name.clone(),
                    description: description.clone(),
                    side: CommandSide::Ui(action.clone()),
                },
                (None, Some(description)) => CommandEntry {
                    name: name.clone(),
                    description: description.clone(),
                    side: CommandSide::Server,
                },
                (None, None) => unreachable!("names come from the two maps"),
            })
            .collect()
    }

    /// The command list narrowed by the first word typed.
    pub(crate) fn filtered_commands(&self, filter: &str) -> Vec<CommandEntry> {
        let word = filter.split_whitespace().next().unwrap_or("");
        self.commands()
            .into_iter()
            .filter(|c| c.name.starts_with(word))
            .collect()
    }

    // ----- pages and selection --------------------------------------------

    fn show_page(&mut self, index: usize) {
        self.page = index.min(self.pages.len().saturating_sub(1));
        if let Some(key) = self.visible_agent_key() {
            if let Some(i) = self.agents.iter().position(|a| a.key == key) {
                self.sidebar = i;
            }
            self.open_pending_picker(&key);
        }
    }

    /// Make an agent the selected one: show its page, attach if needed.
    fn select_agent(&mut self, key: LoopKey) {
        let index = match self.pages.iter().position(|p| p.is_agent(&key)) {
            Some(i) => i,
            None => {
                self.pages.push(Page::Agent {
                    key: key.clone(),
                    scroll: None,
                });
                self.pages.len() - 1
            }
        };
        self.show_page(index);
        self.ensure_attached(&key);
    }

    fn ensure_attached(&mut self, key: &LoopKey) {
        let io = self.io.clone();
        if let Some(agent) = self.agent_mut(key) {
            if !agent.subscribed && !agent.attaching {
                agent.attaching = true;
                io.attach(key.clone());
            }
        }
    }

    fn open_pending_picker(&mut self, key: &LoopKey) {
        if self.mode != Mode::Normal {
            return;
        }
        if let Some(picker) = self
            .agent_mut(key)
            .and_then(|a| a.pending_pickers.pop_front())
        {
            self.mode = Mode::Picker(picker);
        }
    }

    fn open_file(&mut self, key: LoopKey, path: ServerPath) {
        match self.pages.iter().position(|p| p.is_file(&key, &path)) {
            Some(i) => self.show_page(i),
            None => {
                self.pages.push(Page::File {
                    key: key.clone(),
                    path: path.clone(),
                    content: None,
                    scroll: 0,
                });
                let last = self.pages.len() - 1;
                self.show_page(last);
                self.io.read_file(key.clone(), path);
            }
        }
        self.ensure_attached(&key);
    }

    fn close_page(&mut self, index: usize) {
        if index >= self.pages.len() {
            return;
        }
        let page = self.pages.remove(index);
        let key = page.key().clone();
        if self.page >= self.pages.len() {
            self.page = self.pages.len().saturating_sub(1);
        }
        self.release_if_unused(&key);
        if !self.pages.is_empty() {
            self.show_page(self.page);
        }
    }

    /// Drop the loop's subscription once no page shows it.
    fn release_if_unused(&mut self, key: &LoopKey) {
        if self.pages.iter().any(|p| p.key() == key) {
            return;
        }
        let io = self.io.clone();
        if let Some(agent) = self.agent_mut(key) {
            if agent.subscribed {
                agent.subscribed = false;
                io.unsubscribe(key.clone());
            }
        }
    }

    fn remove_agent(&mut self, key: &LoopKey) {
        let mut i = 0;
        while i < self.pages.len() {
            if self.pages[i].key() == key {
                self.pages.remove(i);
            } else {
                i += 1;
            }
        }
        self.agents.retain(|a| &a.key != key);
        let picker_for_it = matches!(
            &self.mode,
            Mode::Picker(Picker {
                action: PickAction::SendPrompt(k) | PickAction::OpenFile(k),
                ..
            }) if k == key
        );
        if picker_for_it {
            self.mode = Mode::Normal;
        }
        self.page = self.page.min(self.pages.len().saturating_sub(1));
        self.sidebar = self
            .sidebar
            .min(self.sidebar_items().len().saturating_sub(1));
        if !self.pages.is_empty() {
            self.show_page(self.page);
        }
    }

    // ----- messages from I/O ----------------------------------------------

    pub(crate) fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Event { server, event } => self.handle_event(&server, event),
            Msg::ServerGone { server } => {
                self.lost.insert(server.clone());
                self.notice(
                    NotifyLevel::Error,
                    format!("connection to server `{server}` lost"),
                );
            }
            Msg::Listed { server, result } => match result {
                Ok(list) => self.merge_list(&server, list),
                Err(e) => self.notice(NotifyLevel::Error, format!("loop.list: {e}")),
            },
            Msg::Attached { key, result } => {
                let io = self.io.clone();
                match result {
                    Ok(attached) => {
                        if let Some(agent) = self.agent_mut(&key) {
                            agent.attaching = false;
                            agent.subscribed = true;
                            agent.info = attached.info;
                            agent.manifest = Some(attached.manifest);
                            io.subscribe(key, agent.tracker.since());
                        }
                    }
                    Err(e) => {
                        if let Some(agent) = self.agent_mut(&key) {
                            agent.attaching = false;
                        }
                        self.notice(NotifyLevel::Error, format!("loop.attach: {e}"));
                    }
                }
            }
            Msg::Created { server, result } => match result {
                Ok(info) => {
                    let key = LoopKey {
                        server: server.clone(),
                        loop_id: info.id.clone(),
                    };
                    match self.agent_mut(&key) {
                        Some(agent) => agent.info = info,
                        None => self.agents.push(Agent::new(&server, info)),
                    }
                    self.select_agent(key);
                }
                Err(e) => self.notice(NotifyLevel::Error, format!("loop.create: {e}")),
            },
            Msg::FileRead { key, path, result } => {
                for page in &mut self.pages {
                    if let Page::File {
                        key: k,
                        path: p,
                        content,
                        ..
                    } = page
                    {
                        if *k == key && *p == path {
                            *content = Some(result.clone());
                        }
                    }
                }
            }
            Msg::Hook {
                key,
                target,
                result,
            } => self.handle_hook(key, target, result),
            Msg::Shell { name, result } => match result {
                Ok(out) => {
                    let line = out.lines().next().unwrap_or("").to_owned();
                    self.notice(NotifyLevel::Info, format!("{name}: {line}"))
                }
                Err(e) => self.notice(NotifyLevel::Error, format!("{name}: {e}")),
            },
            Msg::Failed { what, error } => {
                self.notice(NotifyLevel::Error, format!("{what}: {error}"))
            }
            Msg::Notice(text) => self.notice(NotifyLevel::Info, text),
        }
    }

    fn merge_list(&mut self, server: &str, list: pirs_protocol::LoopListResult) {
        let listed: BTreeSet<String> = list.loops.iter().map(|l| l.id.clone()).collect();
        let gone: Vec<LoopKey> = self
            .agents
            .iter()
            .filter(|a| a.key.server == server && !listed.contains(&a.key.loop_id))
            .map(|a| a.key.clone())
            .collect();
        for key in gone {
            self.remove_agent(&key);
        }
        for info in list.loops {
            let key = LoopKey {
                server: server.to_owned(),
                loop_id: info.id.clone(),
            };
            match self.agent_mut(&key) {
                Some(agent) => {
                    // The list is fresher than what we had, unless a status
                    // event already moved past it.
                    if agent.last_status_seq == 0 {
                        agent.info = info;
                    } else {
                        agent.info.name = info.name;
                        agent.info.cwd = info.cwd;
                        agent.info.model = info.model;
                    }
                }
                None => self.agents.push(Agent::new(server, info)),
            }
        }
        self.conversations.retain(|c| c.server != server);
        self.conversations.extend(
            list.conversations
                .into_iter()
                .map(|info| StoredConversation {
                    server: server.to_owned(),
                    info,
                }),
        );
        self.sidebar = self
            .sidebar
            .min(self.sidebar_items().len().saturating_sub(1));
        if !self.started && !self.agents.is_empty() {
            self.started = true;
            let key = self.agents[0].key.clone();
            self.select_agent(key);
        }
    }

    fn handle_event(&mut self, server: &str, event: Event) {
        let key = LoopKey {
            server: server.to_owned(),
            loop_id: event.loop_id().to_owned(),
        };
        let event = match event {
            Event::LoopStatus(status) => return self.handle_status(key, status),
            other => other,
        };
        let Some(index) = self.agents.iter().position(|a| a.key == key) else {
            return;
        };
        if !self.agents[index].tracker.observe(&event) {
            return;
        }
        match event {
            Event::LoopStatus(_) => unreachable!("handled above"),
            Event::LoopMessage(message) => self.handle_message(key, message),
            Event::LoopTurnEnd(_) | Event::LoopRunEnd(_) => {
                // Their messages already arrived one by one as loop.message.
            }
            Event::UiStatus(e) => {
                let agent = &mut self.agents[index];
                if e.text.is_empty() {
                    agent.status.remove(&e.key);
                } else {
                    agent.status.insert(e.key, e.text);
                }
            }
            Event::UiWidget(e) => {
                let agent = &mut self.agents[index];
                if e.lines.is_empty() {
                    agent.widgets.remove(&e.key);
                } else {
                    agent.widgets.insert(e.key, e.lines);
                }
            }
            Event::UiNotify(e) => {
                let label = self.agents[index].label().to_owned();
                self.notice(e.level, format!("{label}: {}", e.text));
            }
            Event::FsChanged(e) => {
                let agent = &mut self.agents[index];
                if !agent.files.contains(&e.path) {
                    agent.files.push(e.path.clone());
                }
                if self.pages.iter().any(|p| p.is_file(&key, &e.path)) {
                    self.io.read_file(key, e.path);
                }
            }
        }
    }

    fn handle_status(&mut self, key: LoopKey, status: LoopStatusEvent) {
        let closed = status.detail.as_deref() == Some("closed");
        let Some(agent) = self.agent_mut(&key) else {
            if !closed {
                // A loop we do not know: created by someone else, or listed
                // before we were. `loop.list` tells us its name and cwd.
                self.io
                    .list(&key.server, Some(ServerPath::from(self.cwd.clone())));
            }
            return;
        };
        if status.seq <= agent.last_status_seq {
            return;
        }
        agent.last_status_seq = status.seq;
        if closed {
            self.remove_agent(&key);
            return;
        }
        let previous = agent.info.state;
        agent.info.state = status.state;
        agent.info.since = status.since;
        match status.state {
            LoopState::Working => {
                if previous == LoopState::Idle {
                    // A new run: the jump list is "files touched this run".
                    agent.files.clear();
                }
            }
            LoopState::Idle => {
                // Stopped is the server's fact; not looked at since is ours.
                // Drawing the page clears it (`before_draw`).
                agent.unviewed = true;
            }
        }
    }

    fn handle_message(&mut self, key: LoopKey, event: LoopMessageEvent) {
        let config = self.config.clone();
        let Some(agent) = self.agent_mut(&key) else {
            return;
        };
        let mut hooks: Vec<(HookTarget, String, Value)> = Vec::new();
        match event.body {
            LoopMessageBody::Delta { delta } => {
                let open = matches!(
                    agent.entries.last(),
                    Some(Entry::Assistant {
                        streaming: true,
                        ..
                    })
                );
                if !open {
                    agent.entries.push(Entry::Assistant {
                        blocks: Vec::new(),
                        streaming: true,
                    });
                }
                let Some(Entry::Assistant {
                    blocks: streaming, ..
                }) = agent.entries.last_mut()
                else {
                    unreachable!("an open assistant entry was just ensured");
                };
                let (index, text, thinking) = match delta {
                    Delta::Text { index, text } => (index, text, false),
                    Delta::Thinking { index, thinking } => (index, thinking, true),
                };
                while streaming.len() <= index {
                    streaming.push(Block::Text(String::new()));
                }
                match (&mut streaming[index], thinking) {
                    (Block::Text(s), false) | (Block::Thinking(s), true) => s.push_str(&text),
                    (block, true) => *block = Block::Thinking(text),
                    (block, false) => *block = Block::Text(text),
                }
            }
            LoopMessageBody::Message { message } => {
                // A complete message replaces whatever was streaming.
                if matches!(
                    agent.entries.last(),
                    Some(Entry::Assistant {
                        streaming: true,
                        ..
                    })
                ) && matches!(*message, Message::Assistant(_))
                {
                    agent.entries.pop();
                }
                match *message {
                    Message::System(s) => {
                        let n = match &s.content {
                            UserContent::Text(t) => t.chars().count(),
                            UserContent::Blocks(_) => s.content.plain_text().chars().count(),
                        };
                        agent
                            .entries
                            .push(Entry::System(format!("system prompt, {n} chars")));
                    }
                    Message::User(u) => agent.entries.push(Entry::User(u.content.plain_text())),
                    Message::Assistant(a) => {
                        let blocks: Vec<Block> = a
                            .content
                            .into_iter()
                            .filter_map(|c| match c {
                                Content::Text { text, .. } => Some(Block::Text(text)),
                                Content::Thinking {
                                    thinking, redacted, ..
                                } => Some(Block::Thinking(if redacted {
                                    "(redacted)".to_owned()
                                } else {
                                    thinking
                                })),
                                Content::ToolCall {
                                    id,
                                    name,
                                    arguments,
                                    ..
                                } => Some(Block::ToolCall {
                                    id,
                                    name,
                                    args: arguments,
                                }),
                                Content::Image { .. } => None,
                            })
                            .collect();
                        let entry_index = agent.entries.len();
                        for (bi, block) in blocks.iter().enumerate() {
                            match block {
                                Block::ToolCall { id, name, args } => {
                                    if let Some(hook) = config.render_for_tool(name) {
                                        hooks.push((
                                            HookTarget::Tool {
                                                call_id: id.clone(),
                                            },
                                            hook.run.clone(),
                                            json!({ "tool": name, "args": args, "id": id }),
                                        ));
                                    }
                                }
                                Block::Text(text) => {
                                    for (fi, fence) in fences(text).into_iter().enumerate() {
                                        if let Some(hook) = config.render_for_block(&fence.tag) {
                                            hooks.push((
                                                HookTarget::Block {
                                                    entry: entry_index,
                                                    block: bi,
                                                    fence: fi,
                                                },
                                                hook.run.clone(),
                                                json!({ "block": fence.tag, "text": fence.body }),
                                            ));
                                        }
                                    }
                                }
                                Block::Thinking(_) => {}
                            }
                        }
                        agent.entries.push(Entry::Assistant {
                            blocks,
                            streaming: false,
                        });
                    }
                    Message::ToolResult(t) => {
                        let mut lines = Vec::new();
                        for c in &t.content {
                            if let Content::Text { text, .. } = c {
                                lines.extend(text.lines().map(str::to_owned));
                            }
                        }
                        let refs = refs_of(t.details.as_ref());
                        // A by-reference payload is a file the server serves,
                        // so it joins the jump list (D-11).
                        for r in &refs {
                            if !agent.files.contains(&r.path) {
                                agent.files.push(r.path.clone());
                            }
                        }
                        agent.entries.push(Entry::ToolResult {
                            name: t.tool_name,
                            lines,
                            refs,
                            is_error: t.is_error,
                        });
                    }
                }
            }
        }
        for (target, run, input) in hooks {
            self.io.hook(key.clone(), target, run, input);
        }
    }

    fn handle_hook(
        &mut self,
        key: LoopKey,
        target: HookTarget,
        result: Result<HookOutput, String>,
    ) {
        match result {
            Ok(out) => {
                let show_now = self.current_key().as_ref() == Some(&key)
                    && matches!(self.visible_page(), Some(Page::Agent { .. }))
                    && self.mode == Mode::Normal;
                let Some(agent) = self.agent_mut(&key) else {
                    return;
                };
                // The lines are always used; the picker is a question, and
                // a question is only asked about a call no user message has
                // answered yet -- a replay re-runs every hook of every turn.
                let answered = agent.answered(&target);
                agent.rendered.insert(target.clone(), out.lines.clone());
                let mut open = None;
                if let Some(options) = out.options.filter(|o| !o.is_empty() && !answered) {
                    let picker = Picker {
                        title: out.lines,
                        options,
                        selected: 0,
                        action: PickAction::SendPrompt(key.clone()),
                    };
                    if show_now {
                        open = Some(picker);
                    } else {
                        agent.pending_pickers.push_back(picker);
                    }
                }
                if let Some(picker) = open {
                    self.mode = Mode::Picker(picker);
                }
            }
            Err(e) => {
                let what = match &target {
                    HookTarget::Tool { call_id } => format!("tool call {call_id}"),
                    HookTarget::Block { .. } => "fenced block".to_owned(),
                };
                self.notice(
                    NotifyLevel::Warning,
                    format!("render hook for {what} failed, default rendering: {e}"),
                );
            }
        }
    }

    // ----- keys -----------------------------------------------------------

    pub(crate) fn handle_text(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Normal => self.input.push_str(text),
            Mode::Command { filter, selected } => {
                filter.push_str(text);
                *selected = 0;
            }
            Mode::Prompt { value, .. } => value.push_str(text),
            Mode::Picker(_) => {}
        }
    }

    pub(crate) fn handle_key(&mut self, key: Key) {
        match self.mode.clone() {
            Mode::Normal => self.normal_key(key),
            Mode::Command { filter, selected } => self.command_key(key, filter, selected),
            Mode::Picker(picker) => self.picker_key(key, picker),
            Mode::Prompt {
                title,
                value,
                action,
            } => self.prompt_key(key, title, value, action),
        }
    }

    fn normal_key(&mut self, key: Key) {
        if let Some(&action) = self.bindings.get(&key) {
            // `/` opens the command list only at the start of a line, so it
            // can still be typed inside a prompt.
            if action != Action::Command || self.input.is_empty() {
                return self.act(action);
            }
        }
        if let Some(c) = key.typed_char() {
            if self.input.is_empty() {
                if let Some(n) = c.to_digit(10).filter(|n| *n > 0) {
                    return self.jump(n as usize);
                }
            }
            self.input.push(c);
            return;
        }
        match key.code {
            Code::Backspace if !key.ctrl && !key.alt => {
                self.input.pop();
            }
            Code::Char('u') if key.ctrl => self.input.clear(),
            _ => {}
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Quit => self.quit = true,
            Action::Send => self.send(PromptWhen::Now),
            Action::SendAfterTurn => self.send(PromptWhen::AfterTurn),
            Action::Abort => {
                let working = self
                    .current_agent()
                    .is_some_and(|a| a.state() == LoopState::Working);
                if working {
                    if let Some(key) = self.current_key() {
                        self.io.abort(key);
                    }
                } else if !self.input.is_empty() {
                    self.input.clear();
                }
            }
            Action::Command => {
                self.mode = Mode::Command {
                    filter: String::new(),
                    selected: 0,
                }
            }
            Action::NextPage => {
                if !self.pages.is_empty() {
                    let next = (self.page + 1) % self.pages.len();
                    self.show_page(next);
                }
            }
            Action::PrevPage => {
                if !self.pages.is_empty() {
                    let prev = (self.page + self.pages.len() - 1) % self.pages.len();
                    self.show_page(prev);
                }
            }
            Action::ClosePage => self.close_page(self.page),
            Action::SidebarUp => self.move_sidebar(-1),
            Action::SidebarDown => self.move_sidebar(1),
            Action::ScrollUp => self.scroll(-(self.body_height.max(2) as i64 / 2)),
            Action::ScrollDown => self.scroll(self.body_height.max(2) as i64 / 2),
            Action::Expand => {
                if let Some(key) = self.current_key() {
                    if let Some(agent) = self.agent_mut(&key) {
                        agent.expanded = !agent.expanded;
                    }
                }
            }
            Action::Edit => self.builtin(Builtin::Edit, ""),
            Action::OpenFile => self.open_file_picker(),
            Action::NewAgent => self.builtin(Builtin::New, ""),
            Action::Reload => self.reload_config(),
        }
    }

    fn move_sidebar(&mut self, delta: i64) {
        let items = self.sidebar_items();
        if items.is_empty() {
            return;
        }
        let next = (self.sidebar as i64 + delta).rem_euclid(items.len() as i64) as usize;
        self.sidebar = next;
        if let SidebarItem::Agent(i) = items[next] {
            let key = self.agents[i].key.clone();
            self.select_agent(key);
        }
    }

    fn scroll(&mut self, delta: i64) {
        let (start, max_start) = self.body_view;
        let next = (start as i64 + delta).clamp(0, max_start as i64) as usize;
        match self.pages.get_mut(self.page) {
            Some(Page::Agent { scroll, .. }) => {
                // At the end, follow the tail again.
                *scroll = if next >= max_start { None } else { Some(next) };
            }
            Some(Page::File { scroll, .. }) => *scroll = next,
            None => {}
        }
    }

    fn jump(&mut self, n: usize) {
        let Some(key) = self.current_key() else {
            return;
        };
        let path = self.agent(&key).and_then(|a| a.files.get(n - 1).cloned());
        match path {
            Some(path) => self.open_file(key, path),
            None => self.notice(NotifyLevel::Info, format!("no file {n} in the jump list")),
        }
    }

    fn open_file_picker(&mut self) {
        let Some(key) = self.current_key() else {
            return;
        };
        let files: Vec<String> = self
            .agent(&key)
            .map(|a| a.files.iter().map(|p| p.to_string()).collect())
            .unwrap_or_default();
        if files.is_empty() {
            self.notice(NotifyLevel::Info, "no files touched this run");
            return;
        }
        self.mode = Mode::Picker(Picker {
            title: vec!["files touched this run".to_owned()],
            options: files,
            selected: 0,
            action: PickAction::OpenFile(key),
        });
    }

    fn send(&mut self, when: PromptWhen) {
        let text = self.input.trim().to_owned();
        if text.is_empty() {
            // Enter on a stored conversation opens it.
            if let Some(SidebarItem::Conversation(i)) = self.sidebar_items().get(self.sidebar) {
                self.open_conversation(*i);
            }
            return;
        }
        match self.current_key() {
            Some(key) => {
                self.io.prompt(key, text, when);
                self.input.clear();
            }
            None => self.notice(NotifyLevel::Info, "no agent selected: /new starts one"),
        }
    }

    fn open_conversation(&mut self, index: usize) {
        let Some(conversation) = self.conversations.get(index).cloned() else {
            return;
        };
        self.io.create(
            &conversation.server,
            LoopCreateParams {
                cwd: conversation.info.cwd,
                model: None,
                name: conversation.info.name,
                session: Some(conversation.info.id),
            },
        );
    }

    fn command_key(&mut self, key: Key, mut filter: String, mut selected: usize) {
        let matches = self.filtered_commands(&filter);
        match key.code {
            Code::Esc => self.mode = Mode::Normal,
            Code::Enter => {
                self.mode = Mode::Normal;
                match matches.get(selected).cloned() {
                    Some(entry) => self.run_command(entry, &filter),
                    None => {
                        if !filter.trim().is_empty() {
                            self.notice(
                                NotifyLevel::Info,
                                format!("no command matches `{}`", filter.trim()),
                            );
                        }
                    }
                }
            }
            Code::Up => {
                selected = selected.saturating_sub(1);
                self.mode = Mode::Command { filter, selected };
            }
            Code::Down => {
                if selected + 1 < matches.len() {
                    selected += 1;
                }
                self.mode = Mode::Command { filter, selected };
            }
            Code::Backspace => {
                if filter.pop().is_none() {
                    self.mode = Mode::Normal;
                } else {
                    self.mode = Mode::Command {
                        filter,
                        selected: 0,
                    };
                }
            }
            _ => {
                if let Some(c) = key.typed_char() {
                    filter.push(c);
                    self.mode = Mode::Command {
                        filter,
                        selected: 0,
                    };
                } else {
                    self.mode = Mode::Command { filter, selected };
                }
            }
        }
    }

    fn run_command(&mut self, entry: CommandEntry, typed: &str) {
        let args = typed
            .strip_prefix(&entry.name)
            .map(str::trim)
            .unwrap_or("")
            .to_owned();
        match entry.side {
            CommandSide::Conflict => self.notice(
                NotifyLevel::Warning,
                format!(
                    "`/{}` is defined by both the server and the UI; rename one (D-32)",
                    entry.name
                ),
            ),
            CommandSide::Server => match self.current_key() {
                Some(key) => {
                    let text = if typed.starts_with(&entry.name) {
                        format!("/{}", typed.trim())
                    } else {
                        format!("/{}", entry.name)
                    };
                    self.io.prompt(key, text, PromptWhen::Now);
                }
                None => self.notice(NotifyLevel::Info, "no agent selected"),
            },
            CommandSide::Ui(UiAction::Builtin(b)) => self.builtin(b, &args),
            CommandSide::Ui(UiAction::Shell(command)) => self.io.shell(entry.name, command),
        }
    }

    fn builtin(&mut self, builtin: Builtin, args: &str) {
        match builtin {
            Builtin::New => {
                let server = self
                    .current_key()
                    .map(|k| k.server)
                    .or_else(|| self.io.server_names().into_iter().next());
                let Some(server) = server else {
                    return self.notice(NotifyLevel::Error, "no server");
                };
                if !args.is_empty() {
                    return self.io.create(
                        &server,
                        LoopCreateParams {
                            cwd: ServerPath::from(args),
                            model: None,
                            name: None,
                            session: None,
                        },
                    );
                }
                let default = self
                    .current_agent()
                    .map(|a| a.info.cwd.to_string())
                    .unwrap_or_else(|| self.cwd.clone());
                self.mode = Mode::Prompt {
                    title: "directory".to_owned(),
                    value: default,
                    action: PromptAction::NewAgent { server },
                };
            }
            Builtin::Close => match self.current_key() {
                Some(key) => self.io.close(key),
                None => self.notice(NotifyLevel::Info, "no agent selected"),
            },
            Builtin::Open => {
                if self.conversations.is_empty() {
                    return self.notice(NotifyLevel::Info, "no stored conversations here");
                }
                let options = self
                    .conversations
                    .iter()
                    .map(|c| format!("{}  {}", c.label(), c.info.cwd))
                    .collect();
                self.mode = Mode::Picker(Picker {
                    title: vec!["stored conversations".to_owned()],
                    options,
                    selected: 0,
                    action: PickAction::OpenConversation,
                });
            }
            Builtin::Edit => match self.visible_page() {
                Some(Page::File { key, path, .. }) => {
                    let prefix = self.config.editor_prefix(&key.server).to_owned();
                    self.io.editor(prefix, path.to_string());
                }
                _ => self.notice(NotifyLevel::Info, "edit: open a file page first"),
            },
            Builtin::Quit => self.quit = true,
            Builtin::Reload => self.reload_config(),
            Builtin::Theme => {
                self.theme = match args.trim() {
                    "dark" => Theme::Dark,
                    "light" => Theme::Light,
                    "" => match self.theme {
                        Theme::Dark => Theme::Light,
                        Theme::Light => Theme::Dark,
                    },
                    other => {
                        return self.notice(
                            NotifyLevel::Info,
                            format!("theme: `{other}` is not dark or light"),
                        )
                    }
                };
            }
        }
    }

    fn picker_key(&mut self, key: Key, mut picker: Picker) {
        match key.code {
            Code::Esc => self.mode = Mode::Normal,
            Code::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                self.mode = Mode::Picker(picker);
            }
            Code::Down => {
                if picker.selected + 1 < picker.options.len() {
                    picker.selected += 1;
                }
                self.mode = Mode::Picker(picker);
            }
            Code::Enter => {
                self.mode = Mode::Normal;
                let choice = picker.options.get(picker.selected).cloned();
                match (picker.action, choice) {
                    (PickAction::SendPrompt(key), Some(text)) => {
                        self.io.prompt(key.clone(), text, PromptWhen::Now);
                        self.open_pending_picker(&key);
                    }
                    (PickAction::OpenConversation, Some(_)) => {
                        self.open_conversation(picker.selected)
                    }
                    (PickAction::OpenFile(key), Some(_)) => {
                        let path = self
                            .agent(&key)
                            .and_then(|a| a.files.get(picker.selected).cloned());
                        if let Some(path) = path {
                            self.open_file(key, path);
                        }
                    }
                    (_, None) => {}
                }
            }
            _ => self.mode = Mode::Picker(picker),
        }
    }

    fn prompt_key(&mut self, key: Key, title: String, mut value: String, action: PromptAction) {
        match key.code {
            Code::Esc => self.mode = Mode::Normal,
            Code::Enter => {
                self.mode = Mode::Normal;
                let value = value.trim().to_owned();
                if value.is_empty() {
                    return;
                }
                match action {
                    PromptAction::NewAgent { server } => self.io.create(
                        &server,
                        LoopCreateParams {
                            cwd: ServerPath::from(value),
                            model: None,
                            name: None,
                            session: None,
                        },
                    ),
                }
            }
            Code::Backspace => {
                value.pop();
                self.mode = Mode::Prompt {
                    title,
                    value,
                    action,
                };
            }
            Code::Char('u') if key.ctrl => {
                self.mode = Mode::Prompt {
                    title,
                    value: String::new(),
                    action,
                };
            }
            _ => {
                if let Some(c) = key.typed_char() {
                    value.push(c);
                }
                self.mode = Mode::Prompt {
                    title,
                    value,
                    action,
                };
            }
        }
    }
}

/// The by-reference payloads a tool result's `details` names.
fn refs_of(details: Option<&Value>) -> Vec<Ref> {
    let Some(details) = details else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    if let Some(list) = details.get("refs").and_then(Value::as_array) {
        for item in list {
            if let Ok(r) = serde_json::from_value::<Ref>(item.clone()) {
                refs.push(r);
            }
        }
    }
    if refs.is_empty() {
        if let Some(r) = details
            .get("ref")
            .and_then(|r| serde_json::from_value::<Ref>(r.clone()).ok())
        {
            refs.push(r);
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_come_from_details() {
        let details = json!({
            "ref": { "ref": "/s/refs/a", "bytes": 10 },
            "refs": [{ "index": 0, "ref": "/s/refs/a", "bytes": 10 }, { "index": 2, "ref": "/s/refs/b", "bytes": 20 }]
        });
        let refs = refs_of(Some(&details));
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[1].path.as_str(), "/s/refs/b");
        let only_ref = json!({ "ref": { "ref": "/s/refs/c", "bytes": 3 } });
        assert_eq!(refs_of(Some(&only_ref)).len(), 1);
        assert!(refs_of(None).is_empty());
    }
}
