//! `~/.pirs/tui.toml`: key bindings, the status format, the theme, UI-side
//! commands, rendering hooks and per-server settings (D-30, D-32, D-37).
//!
//! ```toml
//! theme = "dark"                     # or "light"
//! status = "{branch} · {loop.state}" # over `ui.status` keys and `{loop.state}`
//!
//! [keys]                             # action = key name (see keys.rs); the defaults:
//! quit = "ctrl-q"
//! send = "enter"                     # loop.prompt { when: now }
//! send_after_turn = "alt-enter"      # loop.prompt { when: after_turn }
//! abort = "esc"                      # loop.abort while working; otherwise cancels
//! command = "/"                      # opens the command list (when the input is empty)
//! next_page = "tab"
//! prev_page = "shift-tab"
//! close_page = "ctrl-w"
//! sidebar_up = "up"
//! sidebar_down = "down"
//! scroll_up = "pageup"
//! scroll_down = "pagedown"
//! expand = "ctrl-t"                  # expand / collapse tool results on the page
//! edit = "ctrl-e"                    # $EDITOR in a tmux pane (file page)
//! open_file = "ctrl-o"               # pick a file touched this run
//! new_agent = "ctrl-n"
//! reload = "ctrl-r"                  # re-read this file now
//!
//! [[command]]                        # a UI-side slash command
//! name = "date"
//! description = "show the date"
//! action = "date"                    # a built-in (new, close, open, edit, quit, reload, theme) or a shell command
//!
//! [[render]]                         # draw a tool call or a fenced block with your own program
//! tool = "ask"                       # or: block = "mermaid"
//! run = "python3 ~/.pirs/render-ask.py"
//!
//! [[server]]
//! name = "local"
//! editor_prefix = ""                 # prepended to `$EDITOR <path>` in the editor pane (phase 6)
//! ```
//!
//! A missing file means defaults. A file that does not parse, or names a key
//! that does not exist, is a notice and the previous configuration stays.
//! The file is re-read when it changes (polled once a second) and on the
//! `reload` command (D-34).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::keys::Key;

/// The two palettes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Theme {
    #[default]
    Dark,
    Light,
}

/// A UI-side `[[command]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UiCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// A built-in name or a shell command run in the TUI's cwd.
    pub action: String,
}

/// A `[[render]]` hook: `tool` or `block`, and the command to run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenderHook {
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub block: Option<String>,
    pub run: String,
}

/// A `[[server]]` entry: per-server UI settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerConfig {
    pub name: String,
    /// Prepended to the editor command in the editor pane; empty locally.
    #[serde(default)]
    pub editor_prefix: String,
}

/// The `[keys]` table: every action and the key bound to it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Keys {
    pub quit: String,
    pub send: String,
    pub send_after_turn: String,
    pub abort: String,
    pub command: String,
    pub next_page: String,
    pub prev_page: String,
    pub close_page: String,
    pub sidebar_up: String,
    pub sidebar_down: String,
    pub scroll_up: String,
    pub scroll_down: String,
    pub expand: String,
    pub edit: String,
    pub open_file: String,
    pub new_agent: String,
    pub reload: String,
}

impl Default for Keys {
    fn default() -> Self {
        Keys {
            quit: "ctrl-q".into(),
            send: "enter".into(),
            send_after_turn: "alt-enter".into(),
            abort: "esc".into(),
            command: "/".into(),
            next_page: "tab".into(),
            prev_page: "shift-tab".into(),
            close_page: "ctrl-w".into(),
            sidebar_up: "up".into(),
            sidebar_down: "down".into(),
            scroll_up: "pageup".into(),
            scroll_down: "pagedown".into(),
            expand: "ctrl-t".into(),
            edit: "ctrl-e".into(),
            open_file: "ctrl-o".into(),
            new_agent: "ctrl-n".into(),
            reload: "ctrl-r".into(),
        }
    }
}

/// What a bound key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Action {
    Quit,
    Send,
    SendAfterTurn,
    Abort,
    Command,
    NextPage,
    PrevPage,
    ClosePage,
    SidebarUp,
    SidebarDown,
    ScrollUp,
    ScrollDown,
    Expand,
    Edit,
    OpenFile,
    NewAgent,
    Reload,
}

impl Action {
    /// The config name of the action, for messages and hints.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Send => "send",
            Action::SendAfterTurn => "send_after_turn",
            Action::Abort => "abort",
            Action::Command => "command",
            Action::NextPage => "next_page",
            Action::PrevPage => "prev_page",
            Action::ClosePage => "close_page",
            Action::SidebarUp => "sidebar_up",
            Action::SidebarDown => "sidebar_down",
            Action::ScrollUp => "scroll_up",
            Action::ScrollDown => "scroll_down",
            Action::Expand => "expand",
            Action::Edit => "edit",
            Action::OpenFile => "open_file",
            Action::NewAgent => "new_agent",
            Action::Reload => "reload",
        }
    }
}

impl Keys {
    /// Parse every binding into a key → action map.
    pub(crate) fn bindings(&self) -> Result<HashMap<Key, Action>, String> {
        let pairs = [
            (Action::Quit, &self.quit),
            (Action::Send, &self.send),
            (Action::SendAfterTurn, &self.send_after_turn),
            (Action::Abort, &self.abort),
            (Action::Command, &self.command),
            (Action::NextPage, &self.next_page),
            (Action::PrevPage, &self.prev_page),
            (Action::ClosePage, &self.close_page),
            (Action::SidebarUp, &self.sidebar_up),
            (Action::SidebarDown, &self.sidebar_down),
            (Action::ScrollUp, &self.scroll_up),
            (Action::ScrollDown, &self.scroll_down),
            (Action::Expand, &self.expand),
            (Action::Edit, &self.edit),
            (Action::OpenFile, &self.open_file),
            (Action::NewAgent, &self.new_agent),
            (Action::Reload, &self.reload),
        ];
        let mut map = HashMap::new();
        for (action, name) in pairs {
            let key = Key::parse(name).map_err(|e| format!("[keys] {}: {e}", action.name()))?;
            if let Some(other) = map.insert(key, action) {
                return Err(format!(
                    "[keys] {} and {} are both bound to `{key}`",
                    other.name(),
                    action.name()
                ));
            }
        }
        Ok(map)
    }
}

/// The whole file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub theme: Theme,
    pub status: String,
    pub keys: Keys,
    #[serde(rename = "command")]
    pub commands: Vec<UiCommand>,
    #[serde(rename = "render")]
    pub renders: Vec<RenderHook>,
    #[serde(rename = "server")]
    pub servers: Vec<ServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: Theme::Dark,
            status: "{loop.state}".into(),
            keys: Keys::default(),
            commands: Vec::new(),
            renders: Vec::new(),
            servers: Vec::new(),
        }
    }
}

impl Config {
    /// Parse the text of a `tui.toml` and check what parsing cannot.
    pub(crate) fn parse(text: &str) -> Result<Config, String> {
        let config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        for hook in &config.renders {
            match (&hook.tool, &hook.block) {
                (Some(_), None) | (None, Some(_)) => {}
                _ => return Err("[[render]]: exactly one of `tool` and `block` is required".into()),
            }
        }
        for command in &config.commands {
            if command.name.is_empty() || command.name.contains(char::is_whitespace) {
                return Err(format!("[[command]]: invalid name {:?}", command.name));
            }
        }
        Ok(config)
    }

    /// The `[[render]]` hook for a tool call.
    pub(crate) fn render_for_tool(&self, name: &str) -> Option<&RenderHook> {
        self.renders
            .iter()
            .find(|h| h.tool.as_deref() == Some(name))
    }

    /// The `[[render]]` hook for a fenced block tag.
    pub(crate) fn render_for_block(&self, tag: &str) -> Option<&RenderHook> {
        self.renders
            .iter()
            .find(|h| h.block.as_deref() == Some(tag))
    }

    /// The editor prefix configured for a server; empty when none.
    pub(crate) fn editor_prefix(&self, server: &str) -> &str {
        self.servers
            .iter()
            .find(|s| s.name == server)
            .map(|s| s.editor_prefix.as_str())
            .unwrap_or("")
    }
}

/// The file on disk, re-read when its text changes.
#[derive(Debug)]
pub(crate) struct ConfigFile {
    path: PathBuf,
    /// The text last seen; `None` when the file was absent.
    last: Option<String>,
}

/// What a read produced.
#[derive(Debug)]
pub(crate) enum Loaded {
    /// The file parsed (or is absent, giving defaults).
    Config(Box<Config>),
    /// The file is there but wrong; the message says how.
    Error(String),
}

impl ConfigFile {
    pub(crate) fn new(path: PathBuf) -> Self {
        ConfigFile { path, last: None }
    }

    fn read_text(&self) -> Result<Option<String>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    /// Read the file now, whatever it looked like last time.
    pub(crate) fn load(&mut self) -> Loaded {
        match self.read_text() {
            Ok(text) => {
                self.last = text.clone();
                match text {
                    None => Loaded::Config(Box::default()),
                    Some(text) => match Config::parse(&text) {
                        Ok(config) => Loaded::Config(Box::new(config)),
                        Err(e) => Loaded::Error(format!("{}: {e}", self.path.display())),
                    },
                }
            }
            Err(e) => Loaded::Error(e),
        }
    }

    /// Re-read the file only if its text changed since the last read.
    pub(crate) fn poll(&mut self) -> Option<Loaded> {
        match self.read_text() {
            Ok(text) if text == self.last => None,
            Ok(_) => Some(self.load()),
            Err(e) => Some(Loaded::Error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_bind_without_clashes() {
        let config = Config::parse("").unwrap();
        assert_eq!(config, Config::default());
        let bindings = config.keys.bindings().unwrap();
        assert_eq!(bindings.len(), 17);
    }

    #[test]
    fn a_full_file_parses() {
        let text = r#"
            theme = "light"
            status = "{branch} · {loop.state}"
            [keys]
            quit = "ctrl-c"
            [[command]]
            name = "date"
            description = "the date"
            action = "date"
            [[render]]
            tool = "ask"
            run = "./ask.py"
            [[render]]
            block = "mermaid"
            run = "./mermaid.py"
            [[server]]
            name = "build"
            editor_prefix = "ssh build"
        "#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config.theme, Theme::Light);
        assert_eq!(config.render_for_tool("ask").unwrap().run, "./ask.py");
        assert_eq!(
            config.render_for_block("mermaid").unwrap().run,
            "./mermaid.py"
        );
        assert_eq!(config.editor_prefix("build"), "ssh build");
        assert_eq!(config.editor_prefix("local"), "");
        assert_eq!(config.keys.quit, "ctrl-c");
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        assert!(Config::parse("theme = \"sepia\"").is_err());
        assert!(Config::parse("[keys]\nfly = \"f\"").is_err());
        assert!(Config::parse("[[render]]\nrun = \"x\"").is_err());
        let config = Config::parse("[keys]\nquit = \"hyper-q\"").unwrap();
        assert!(config.keys.bindings().is_err());
        let config = Config::parse("[keys]\nquit = \"enter\"").unwrap();
        assert!(config.keys.bindings().unwrap_err().contains("both bound"));
    }
}
