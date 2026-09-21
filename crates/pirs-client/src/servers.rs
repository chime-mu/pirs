//! `~/.pirs/servers.toml`: which servers there are and how to reach them.
//!
//! One table per server, a name and a bridge command, and nothing else (D-05):
//!
//! ```toml
//! [[server]]
//! name = "local"                              # no command: the local socket
//!
//! [[server]]
//! name = "build"
//! command = "ssh build pirs proxy"            # a machine
//!
//! [[server]]
//! name = "jail"
//! command = "docker exec -i jail pirs proxy"  # a container
//! editor_prefix = "docker exec -it jail"      # optional; see `editor_prefix`
//! ```
//!
//! A `command` is a program that forwards protocol lines between its stdio and
//! the server's socket — `pirs proxy` on the other side of whatever gets you
//! there. pirs knows nothing about SSH or containers: the command is the whole
//! of the transport, and whoever runs it owns the login (D-36).
//!
//! A server without a `command` is the local socket: `socket` names one other
//! than the default, and auto-start applies to it and to nothing else.
//!
//! A missing file means one server, [`ServerConfig::local`].

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{ClientError, Result};
use crate::socket::pirs_home;
use crate::spawn::split_command;

/// The name of the server every client has: the local socket.
pub const LOCAL: &str = "local";

/// The file inside [`pirs_home`].
const SERVERS_FILE: &str = "servers.toml";

/// One server: its name, and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// What the user calls it: `pirs --server build`, `build:a7f3` in the UI.
    pub name: String,
    /// The bridge command, program first, as
    /// [`split_command`](crate::split_command) reads it. `None` is the local
    /// socket.
    pub command: Option<Vec<String>>,
    /// A local socket other than the default. Meaningless with a `command`,
    /// and ignored there.
    pub socket: Option<PathBuf>,
    /// What to put in front of `$EDITOR <path>` to edit a file on this
    /// server, when the derivation in [`editor_prefix`](Self::editor_prefix)
    /// is not what the user wants.
    pub editor_prefix: Option<String>,
}

impl ServerConfig {
    /// The local server on the default socket.
    pub fn local() -> ServerConfig {
        ServerConfig {
            name: LOCAL.to_owned(),
            command: None,
            socket: None,
            editor_prefix: None,
        }
    }

    /// Whether this server is reached through a bridge command rather than a
    /// local socket. A remote server starts no server of its own and cannot
    /// be stopped from here.
    pub fn is_remote(&self) -> bool {
        self.command.is_some()
    }

    /// What to put in front of `$EDITOR <path>` to edit a file on this
    /// server (D-27, D-29).
    ///
    /// The explicit `editor_prefix` when there is one. Otherwise the bridge
    /// command with the `pirs proxy` on its end removed, because what is left
    /// is exactly the way to run a command over there: `ssh build pirs proxy`
    /// leaves `ssh build`, `docker exec -i jail pirs proxy` leaves
    /// `docker exec -i jail`. The proxy's own flags — `--socket <path>` and
    /// `--no-start`, in whatever order they were written — belong to it and
    /// are removed with it. Anything else — including the local server — has
    /// no prefix, and the empty string is the answer.
    pub fn editor_prefix(&self) -> String {
        if let Some(explicit) = &self.editor_prefix {
            return explicit.clone();
        }
        let Some(command) = &self.command else {
            return String::new();
        };
        let mut words: &[String] = command;
        // The proxy's flags come off first, in any order: what is left has
        // `pirs proxy` on its end or this is not a bridge we can read.
        loop {
            if words.last().is_some_and(|word| word == "--no-start") {
                words = &words[..words.len() - 1];
            } else if words.len() >= 2 && words[words.len() - 2] == "--socket" {
                words = &words[..words.len() - 2];
            } else {
                break;
            }
        }
        let is_proxy = words.len() >= 2
            && words[words.len() - 1] == "proxy"
            && is_pirs(&words[words.len() - 2]);
        if !is_proxy {
            return String::new();
        }
        words[..words.len() - 2].join(" ")
    }
}

/// Whether a word names the pirs binary: `pirs`, or a path ending in it.
///
/// A plain string test, not a path one: this is a command line the user
/// wrote, not a path a server produced (D-31).
fn is_pirs(word: &str) -> bool {
    word == "pirs" || word.ends_with("/pirs")
}

/// Where the servers file is: `$PIRS_HOME/servers.toml`, else
/// `~/.pirs/servers.toml`.
pub fn servers_path() -> PathBuf {
    // A local file of ours, not a path from a server: joining is allowed here
    // and nowhere else in this crate (D-31).
    #[allow(clippy::disallowed_methods)]
    pirs_home().join(SERVERS_FILE)
}

/// Every configured server, in the order the file names them.
///
/// A missing file means one server, [`ServerConfig::local`]; so does a file
/// with no `[[server]]` table. A file that does not parse is an error, named
/// with its path, because a typo in it would otherwise silently lose a
/// machine.
pub fn servers() -> Result<Vec<ServerConfig>> {
    let path = servers_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(vec![ServerConfig::local()])
        }
        Err(error) => {
            return Err(ClientError::Servers {
                path,
                message: error.to_string(),
            })
        }
    };
    parse_servers(&text).map_err(|message| ClientError::Servers { path, message })
}

/// The file's text as a list of servers; the error is one line, without the
/// path, which [`servers`] adds.
pub fn parse_servers(text: &str) -> std::result::Result<Vec<ServerConfig>, String> {
    let file: ServersFile = toml::from_str(text).map_err(|error| one_line(&error.to_string()))?;
    let mut servers = Vec::with_capacity(file.servers.len());
    for raw in file.servers {
        if raw.name.trim().is_empty() {
            return Err("a [[server]] has an empty name".to_owned());
        }
        if servers
            .iter()
            .any(|other: &ServerConfig| other.name == raw.name)
        {
            return Err(format!("two servers are called {:?}", raw.name));
        }
        let command = match raw.command {
            None => None,
            Some(line) => {
                let words = split_command(&line);
                if words.is_empty() {
                    return Err(format!("server {:?} has an empty command", raw.name));
                }
                Some(words)
            }
        };
        if command.is_some() && raw.socket.is_some() {
            return Err(format!(
                "server {:?} has both a command and a socket; a bridge command is the whole transport",
                raw.name
            ));
        }
        servers.push(ServerConfig {
            name: raw.name,
            command,
            socket: raw.socket,
            editor_prefix: raw.editor_prefix,
        });
    }
    if servers.is_empty() {
        servers.push(ServerConfig::local());
    }
    Ok(servers)
}

/// Collapse a multi-line `toml` error into one line, so it reads in a
/// terminal next to the file name.
fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServersFile {
    #[serde(default, rename = "server")]
    servers: Vec<RawServer>,
}

/// One `[[server]]` table, as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    name: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    socket: Option<PathBuf>,
    #[serde(default)]
    editor_prefix: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<ServerConfig> {
        parse_servers(text).expect("a parsable file")
    }

    #[test]
    fn a_file_names_local_and_remote_servers() {
        let servers = parse(
            r#"
            [[server]]
            name = "local"

            [[server]]
            name = "build"
            command = "ssh build pirs proxy"

            [[server]]
            name = "jail"
            command = "docker exec -i jail pirs proxy"
            editor_prefix = "docker exec -it jail"

            [[server]]
            name = "other"
            socket = "/tmp/other.sock"
            "#,
        );
        assert_eq!(servers.len(), 4);
        assert_eq!(servers[0], ServerConfig::local());
        assert!(!servers[0].is_remote());
        assert_eq!(
            servers[1].command.as_deref(),
            Some(["ssh".to_owned(), "build".to_owned(), "pirs".to_owned(), "proxy".to_owned()].as_slice())
        );
        assert!(servers[1].is_remote());
        assert_eq!(servers[2].editor_prefix.as_deref(), Some("docker exec -it jail"));
        assert_eq!(servers[3].socket, Some(PathBuf::from("/tmp/other.sock")));
        assert!(!servers[3].is_remote());
    }

    #[test]
    fn a_quoted_word_survives_the_split() {
        let servers = parse(
            r#"
            [[server]]
            name = "build"
            command = "ssh build '/opt/pirs bin/pirs' proxy"
            "#,
        );
        assert_eq!(
            servers[0].command.as_deref().expect("a command"),
            ["ssh", "build", "/opt/pirs bin/pirs", "proxy"]
        );
        assert_eq!(servers[0].editor_prefix(), "ssh build");
    }

    #[test]
    fn an_empty_or_absent_file_is_the_local_server() {
        assert_eq!(parse(""), [ServerConfig::local()]);
        assert_eq!(parse("\n# nothing here\n"), [ServerConfig::local()]);
    }

    #[test]
    fn unknown_fields_missing_names_and_duplicates_are_errors() {
        let unknown = parse_servers(
            r#"
            [[server]]
            name = "build"
            host = "build.example"
            "#,
        )
        .unwrap_err();
        assert!(unknown.contains("host"), "{unknown}");

        let top_level = parse_servers("servers = 3").unwrap_err();
        assert!(top_level.contains("servers"), "{top_level}");

        let nameless = parse_servers("[[server]]\ncommand = \"ssh build pirs proxy\"\n").unwrap_err();
        assert!(nameless.contains("name"), "{nameless}");

        let empty_name = parse_servers("[[server]]\nname = \"\"\n").unwrap_err();
        assert!(empty_name.contains("empty name"), "{empty_name}");

        let twice = parse_servers("[[server]]\nname = \"a\"\n\n[[server]]\nname = \"a\"\n").unwrap_err();
        assert!(twice.contains("two servers"), "{twice}");

        let empty_command = parse_servers("[[server]]\nname = \"a\"\ncommand = \"   \"\n").unwrap_err();
        assert!(empty_command.contains("empty command"), "{empty_command}");

        let both = parse_servers(
            "[[server]]\nname = \"a\"\ncommand = \"ssh a pirs proxy\"\nsocket = \"/tmp/s\"\n",
        )
        .unwrap_err();
        assert!(both.contains("both a command and a socket"), "{both}");

        // The message is one line, so it reads next to the file name.
        assert!(!unknown.contains('\n'), "{unknown}");
    }

    #[test]
    fn the_editor_prefix_is_the_command_without_its_proxy() {
        let prefix = |command: &str| {
            ServerConfig {
                name: "s".to_owned(),
                command: Some(split_command(command)),
                socket: None,
                editor_prefix: None,
            }
            .editor_prefix()
        };
        assert_eq!(prefix("ssh build pirs proxy"), "ssh build");
        assert_eq!(prefix("docker exec -i jail pirs proxy"), "docker exec -i jail");
        assert_eq!(prefix("ssh build pirs proxy --socket /run/pirs.sock"), "ssh build");
        assert_eq!(prefix("ssh build /usr/local/bin/pirs proxy"), "ssh build");
        // The proxy's flags, each on its own and in either order.
        assert_eq!(prefix("ssh build pirs proxy --no-start"), "ssh build");
        assert_eq!(
            prefix("ssh build pirs proxy --no-start --socket /run/pirs.sock"),
            "ssh build"
        );
        assert_eq!(
            prefix("ssh build pirs proxy --socket /run/pirs.sock --no-start"),
            "ssh build"
        );
        // A bridge that is not `pirs proxy` says nothing about how to run a
        // command over there, so it gets no prefix rather than a wrong one.
        assert_eq!(prefix("socat - UNIX:/run/pirs.sock"), "");
        assert_eq!(prefix("pirs proxy"), "");
        // The local server has none either.
        assert_eq!(ServerConfig::local().editor_prefix(), "");
    }

    #[test]
    fn an_explicit_editor_prefix_wins() {
        let server = ServerConfig {
            name: "jail".to_owned(),
            command: Some(split_command("docker exec -i jail pirs proxy")),
            socket: None,
            editor_prefix: Some("docker exec -it jail".to_owned()),
        };
        assert_eq!(server.editor_prefix(), "docker exec -it jail");
        // Including an explicit empty one, which turns the derivation off.
        let none = ServerConfig {
            editor_prefix: Some(String::new()),
            ..server
        };
        assert_eq!(none.editor_prefix(), "");
    }
}
