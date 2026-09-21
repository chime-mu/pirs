//! What the UI knows: agents keyed by `(server, loop)`, their conversations
//! as the page draws them, pages, modes, and the messages that flow between
//! the drivers, the I/O tasks and the engine.

use std::collections::{BTreeMap, HashMap, VecDeque};

use pirs_client::SeqTracker;
use pirs_protocol::{
    ConversationInfo, Event, LoopAttachResult, LoopInfo, LoopListResult, LoopState, Manifest,
    NotifyLevel, Ref, ServerPath,
};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::keys::Key;
use crate::process::HookOutput;

/// A loop on a server: what identifies an agent across every configured
/// server, because two servers can hand out the same loop id (S18, S21).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LoopKey {
    pub server: String,
    pub loop_id: String,
}

/// One block of an assistant message, as the page draws it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Block {
    Text(String),
    Thinking(String),
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
}

/// One thing in the conversation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Entry {
    /// The system prompt, summarised to one line.
    System(String),
    User(String),
    /// A complete assistant message, or the one being streamed.
    Assistant {
        blocks: Vec<Block>,
        streaming: bool,
    },
    ToolResult {
        name: String,
        lines: Vec<String>,
        refs: Vec<Ref>,
        is_error: bool,
    },
}

/// What a `[[render]]` hook was run for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum HookTarget {
    /// A tool call, by its call id.
    Tool { call_id: String },
    /// The `fence`-th fenced block of the `block`-th content block of the
    /// `entry`-th entry.
    Block {
        entry: usize,
        block: usize,
        fence: usize,
    },
}

/// A native picker: lines above, options below, the choice acted on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Picker {
    pub title: Vec<String>,
    pub options: Vec<String>,
    pub selected: usize,
    pub action: PickAction,
}

/// What choosing a picker option does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PickAction {
    /// Send the option's text as the next prompt (`when: now`).
    SendPrompt(LoopKey),
    /// Start an agent on the stored conversation at that index.
    OpenConversation,
    /// Open the file page for that entry of the agent's jump list.
    OpenFile(LoopKey),
    /// Start an agent in this directory on the server that was chosen.
    NewAgentIn { cwd: String },
}

/// One running agent, as the sidebar and its page see it.
#[derive(Debug)]
pub(crate) struct Agent {
    pub key: LoopKey,
    pub info: LoopInfo,
    /// The server said `idle` and the page has not been viewed since (S12).
    pub unviewed: bool,
    /// The `seq` of the last `loop.status` applied, for dedup between the
    /// `*` subscription and the loop's own replay.
    pub last_status_seq: u64,
    /// Dedup and resume point for the loop's own subscription.
    pub tracker: SeqTracker,
    pub attaching: bool,
    pub subscribed: bool,
    pub manifest: Option<Manifest>,
    pub entries: Vec<Entry>,
    pub status: BTreeMap<String, String>,
    pub widgets: BTreeMap<String, Vec<String>>,
    /// Files touched this run (`fs.changed`), in first-seen order.
    pub files: Vec<ServerPath>,
    /// Lines a `[[render]]` hook produced, in place of the default drawing.
    pub rendered: HashMap<HookTarget, Vec<String>>,
    /// Pickers that arrived while another page was showing.
    pub pending_pickers: VecDeque<Picker>,
    /// Tool results expanded (all of them) on this page.
    pub expanded: bool,
}

impl Agent {
    pub(crate) fn new(server: &str, info: LoopInfo) -> Agent {
        // A loop that is already stopped when we first hear of it has not
        // been looked at either, so it starts flagged (S12); the agent the
        // sidebar selects by itself is drawn at once and cleared.
        let unviewed = info.state == LoopState::Idle;
        Agent {
            key: LoopKey {
                server: server.to_owned(),
                loop_id: info.id.clone(),
            },
            info,
            unviewed,
            last_status_seq: 0,
            tracker: SeqTracker::new(),
            attaching: false,
            subscribed: false,
            manifest: None,
            entries: Vec::new(),
            status: BTreeMap::new(),
            widgets: BTreeMap::new(),
            files: Vec::new(),
            rendered: HashMap::new(),
            pending_pickers: VecDeque::new(),
            expanded: false,
        }
    }

    /// The name, or the id when there is none.
    pub(crate) fn label(&self) -> &str {
        self.info.name.as_deref().unwrap_or(&self.info.id)
    }

    pub(crate) fn state(&self) -> LoopState {
        self.info.state
    }

    /// The entry a `[[render]]` hook's target lives in: the assistant
    /// message the tool call is a block of, or the entry the fenced block
    /// was found in.
    fn target_entry(&self, target: &HookTarget) -> Option<usize> {
        match target {
            HookTarget::Block { entry, .. } => Some(*entry),
            HookTarget::Tool { call_id } => self.entries.iter().position(|e| {
                matches!(e, Entry::Assistant { blocks, .. }
                    if blocks.iter().any(|b| matches!(b, Block::ToolCall { id, .. } if id == call_id)))
            }),
        }
    }

    /// Whether a user message follows a hook's target. A hook runs again on
    /// every replay, so an old call whose answer is already in the
    /// conversation must not ask the question a second time (S6).
    pub(crate) fn answered(&self, target: &HookTarget) -> bool {
        let Some(index) = self.target_entry(target) else {
            return false;
        };
        self.entries
            .get(index + 1..)
            .into_iter()
            .flatten()
            .any(|e| matches!(e, Entry::User(_)))
    }

    pub(crate) fn attention(&self) -> bool {
        self.info.state == LoopState::Idle && self.unviewed
    }
}

/// A page: the main area shows one at a time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Page {
    Agent {
        key: LoopKey,
        /// `None` follows the tail; `Some(row)` is pinned.
        scroll: Option<usize>,
    },
    File {
        key: LoopKey,
        path: ServerPath,
        /// `None` until `fs.read` answers.
        content: Option<Result<String, String>>,
        scroll: usize,
    },
}

impl Page {
    pub(crate) fn key(&self) -> &LoopKey {
        match self {
            Page::Agent { key, .. } | Page::File { key, .. } => key,
        }
    }

    pub(crate) fn is_agent(&self, k: &LoopKey) -> bool {
        matches!(self, Page::Agent { key, .. } if key == k)
    }

    pub(crate) fn is_file(&self, k: &LoopKey, p: &ServerPath) -> bool {
        matches!(self, Page::File { key, path, .. } if key == k && path == p)
    }
}

/// What text entry does right now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Mode {
    Normal,
    /// The command list, filtered by what was typed after `/`.
    Command {
        filter: String,
        selected: usize,
    },
    Picker(Picker),
    /// A one-line question, such as `new`'s directory.
    Prompt {
        title: String,
        value: String,
        action: PromptAction,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PromptAction {
    NewAgent { server: String },
}

/// The UI's built-in commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Builtin {
    New,
    Close,
    Open,
    Edit,
    Quit,
    Reload,
    Theme,
}

impl Builtin {
    pub(crate) const ALL: [Builtin; 7] = [
        Builtin::New,
        Builtin::Close,
        Builtin::Open,
        Builtin::Edit,
        Builtin::Quit,
        Builtin::Reload,
        Builtin::Theme,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Builtin::New => "new",
            Builtin::Close => "close",
            Builtin::Open => "open",
            Builtin::Edit => "edit",
            Builtin::Quit => "quit",
            Builtin::Reload => "reload",
            Builtin::Theme => "theme",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Builtin::New => "start an agent in a directory",
            Builtin::Close => "close the selected agent",
            Builtin::Open => "start an agent on a stored conversation",
            Builtin::Edit => "open $EDITOR on this file in a tmux pane",
            Builtin::Quit => "leave the UI (agents keep running)",
            Builtin::Reload => "re-read tui.toml",
            Builtin::Theme => "switch between dark and light",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|b| b.name() == name)
    }
}

/// What a UI-side command does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UiAction {
    Builtin(Builtin),
    Shell(String),
}

/// Which side defines a command in the merged list (D-32).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CommandSide {
    Server,
    Ui(UiAction),
    /// Defined on both sides: shown, never run.
    Conflict,
}

/// One row of the merged command list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandEntry {
    pub name: String,
    pub description: String,
    pub side: CommandSide,
}

/// A transient message on the bottom line.
#[derive(Debug, Clone)]
pub(crate) struct Notice {
    pub at: std::time::Instant,
    pub level: NotifyLevel,
    pub text: String,
}

/// What a driver (terminal or script) tells the engine.
#[derive(Debug)]
pub(crate) enum Control {
    Key(Key),
    Text(String),
    /// The terminal changed size; the next draw picks it up.
    Resize,
    /// Draw now and send back the screen as text.
    Screen(oneshot::Sender<String>),
    Quit,
}

/// What the I/O tasks tell the engine.
#[derive(Debug)]
pub(crate) enum Msg {
    Event {
        server: String,
        event: Event,
    },
    /// A server's link dropped. The UI says so and retries with backoff;
    /// the subscriptions and the place in each loop's log are the
    /// wrapper's to keep (D-06).
    ServerGone {
        server: String,
    },
    /// A server's link is back, its subscriptions re-issued from the last
    /// `seq` seen. Whatever was missed follows.
    ServerBack {
        server: String,
    },
    /// Something that must stay on the screen: a server refused this
    /// client's protocol version, which no retry can fix.
    Persistent {
        text: String,
    },
    Listed {
        server: String,
        result: Result<LoopListResult, String>,
    },
    Attached {
        key: LoopKey,
        result: Result<LoopAttachResult, String>,
    },
    Created {
        server: String,
        result: Result<LoopInfo, String>,
    },
    FileRead {
        key: LoopKey,
        path: ServerPath,
        result: Result<String, String>,
    },
    Hook {
        key: LoopKey,
        target: HookTarget,
        result: Result<HookOutput, String>,
    },
    Shell {
        name: String,
        result: Result<String, String>,
    },
    /// A request failed; `what` names it.
    Failed {
        what: String,
        error: String,
    },
    /// Something the UI wants to say.
    Notice(String),
}

/// A stored conversation and the server holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredConversation {
    pub server: String,
    pub info: ConversationInfo,
}

impl StoredConversation {
    pub(crate) fn label(&self) -> &str {
        self.info.name.as_deref().unwrap_or(&self.info.id)
    }
}

/// One line of `args`, for a tool-call row.
pub(crate) fn args_summary(args: &Value, max: usize) -> String {
    let text = match args {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}={}", v.replace('\n', "⏎"))
            })
            .collect::<Vec<_>>()
            .join(" "),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    truncate(&text, max)
}

/// Cut `s` to at most `max` characters, marking the cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.pop();
        out.push('…');
    }
    out
}

/// A fenced code block inside a text block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fence {
    /// Line index of the opening fence.
    pub start: usize,
    /// Line index of the closing fence (or the last line when unclosed).
    pub end: usize,
    pub tag: String,
    pub body: String,
}

/// Find every ```` ```tag ```` block in `text`, by line.
pub(crate) fn fences(text: &str) -> Vec<Fence> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_start();
        if let Some(tag) = line.strip_prefix("```") {
            let tag = tag.trim().to_owned();
            let start = i;
            let mut end = lines.len().saturating_sub(1);
            let mut body = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                if lines[j].trim_start().starts_with("```") {
                    end = j;
                    break;
                }
                body.push(lines[j]);
                j += 1;
            }
            out.push(Fence {
                start,
                end,
                tag,
                body: body.join("\n"),
            });
            i = end + 1;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_blocks_are_found_by_tag() {
        let text = "intro\n```mermaid\ngraph TD\n```\nafter\n```\nplain\n```";
        let found = fences(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].tag, "mermaid");
        assert_eq!(found[0].body, "graph TD");
        assert_eq!((found[0].start, found[0].end), (1, 3));
        assert_eq!(found[1].tag, "");
        assert_eq!(found[1].body, "plain");
    }

    #[test]
    fn summaries_are_one_line_and_bounded() {
        let args = serde_json::json!({"command": "ls\n-la", "n": 3});
        assert_eq!(args_summary(&args, 80), "command=ls⏎-la n=3");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
