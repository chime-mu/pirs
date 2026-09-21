//! Starting a server that is not running yet.
//!
//! The client never manages the server's life: it spawns it detached — its own
//! process group, no controlling terminal through this client, all three
//! standard streams on `/dev/null`, the environment inherited — and then waits
//! for the socket to appear. The server owns its own idle exit.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{ClientError, Result};

/// The environment variable that replaces the default server command.
pub const SERVER_COMMAND_ENV: &str = "PIRS_SERVER_COMMAND";

/// Split a command line into words the way a shell would, without a shell.
///
/// Whitespace separates words; single quotes protect everything up to the next
/// single quote; double quotes protect everything except `\"` and `\\`; a
/// backslash outside quotes protects the next character. Nothing is expanded:
/// no globs, no variables, no operators. An empty string yields no words.
///
/// ```
/// use pirs_client::split_command;
/// assert_eq!(split_command("pirs serve --idle 60"), ["pirs", "serve", "--idle", "60"]);
/// assert_eq!(split_command(r#"'/opt/my pirs' serve"#), ["/opt/my pirs", "serve"]);
/// assert!(split_command("   ").is_empty());
/// ```
pub fn split_command(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars();

    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    word.push(c);
                }
            }
            Some(_) => {
                if c == '"' {
                    quote = None;
                } else if c == '\\' {
                    match chars.next() {
                        Some(next @ ('"' | '\\')) => word.push(next),
                        Some(next) => {
                            word.push('\\');
                            word.push(next);
                        }
                        None => word.push('\\'),
                    }
                } else {
                    word.push(c);
                }
            }
            None => {
                if c.is_whitespace() {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                    started = true;
                } else if c == '\\' {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                    started = true;
                } else {
                    word.push(c);
                    started = true;
                }
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The command that starts a server: `explicit` if given, else
/// `PIRS_SERVER_COMMAND` split into words, else this executable with `serve`.
///
/// `socket` is the socket the caller named itself (not the default): the
/// started server must listen there, so `--socket <path>` is appended unless
/// the command already says which socket it wants.
pub(crate) fn server_command(
    explicit: Option<Vec<String>>,
    socket: Option<&Path>,
) -> Result<Vec<String>> {
    let mut command = command_words(explicit)?;
    if let Some(socket) = socket {
        if !command.iter().any(|w| w == "--socket") {
            command.push("--socket".to_owned());
            command.push(socket.to_string_lossy().into_owned());
        }
    }
    Ok(command)
}

fn command_words(explicit: Option<Vec<String>>) -> Result<Vec<String>> {
    if let Some(command) = explicit {
        if command.is_empty() {
            return Err(ClientError::EmptyServerCommand);
        }
        return Ok(command);
    }
    if let Some(line) = std::env::var_os(SERVER_COMMAND_ENV) {
        let line = line.to_string_lossy().into_owned();
        if !line.trim().is_empty() {
            let command = split_command(&line);
            if command.is_empty() {
                return Err(ClientError::EmptyServerCommand);
            }
            return Ok(command);
        }
    }
    let exe = std::env::current_exe().map_err(|source| ClientError::NoServerCommand { source })?;
    Ok(vec![exe.to_string_lossy().into_owned(), "serve".to_owned()])
}

/// Spawn `command` detached and forget it.
///
/// The child gets its own process group, so it outlives this client and is not
/// killed by a Ctrl-C meant for it. It is not waited on: a server that exits
/// while this client lives leaves one zombie until the client does, which is
/// the price of not owning a process we deliberately do not own.
pub(crate) fn spawn_detached(command: &[String]) -> Result<()> {
    let (program, args) = command.split_first().ok_or(ClientError::EmptyServerCommand)?;
    let mut spawned = Command::new(program);
    spawned
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    match spawned.spawn() {
        Ok(child) => {
            tracing::debug!(pid = child.id(), command = ?command, "started a pirs server");
            drop(child);
            Ok(())
        }
        Err(source) => Err(ClientError::StartFailed {
            command: command.to_vec(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_quotes_and_escapes() {
        assert_eq!(split_command("pirs serve"), ["pirs", "serve"]);
        assert_eq!(split_command("  a \t b\nc "), ["a", "b", "c"]);
        assert_eq!(split_command("'a b' c"), ["a b", "c"]);
        assert_eq!(split_command(r#""a b" c"#), ["a b", "c"]);
        assert_eq!(split_command(r#""a\"b""#), [r#"a"b"#]);
        assert_eq!(split_command(r"a\ b"), ["a b"]);
        assert_eq!(split_command("''"), [""]);
        assert!(split_command("").is_empty());
        assert!(split_command(" \t ").is_empty());
        // Unterminated quotes keep what they have rather than failing.
        assert_eq!(split_command("'a b"), ["a b"]);
    }

    #[test]
    fn explicit_command_wins_and_must_not_be_empty() {
        let explicit = vec!["/bin/echo".to_owned(), "hi".to_owned()];
        assert_eq!(
            server_command(Some(explicit.clone()), None).unwrap(),
            explicit
        );
        assert!(matches!(
            server_command(Some(Vec::new()), None),
            Err(ClientError::EmptyServerCommand)
        ));
    }

    #[test]
    fn an_explicit_socket_is_passed_to_the_started_server() {
        // A `--socket` only the client knew about would otherwise start a
        // server on the default socket, where this client never looks.
        let socket = Path::new("/tmp/other.sock");
        let started = server_command(
            Some(vec!["/opt/pirs".to_owned(), "serve".to_owned()]),
            Some(socket),
        )
        .unwrap();
        assert_eq!(
            started,
            ["/opt/pirs", "serve", "--socket", "/tmp/other.sock"]
        );
        // The default socket adds nothing, and a command that already names
        // a socket is left alone.
        assert_eq!(
            server_command(Some(vec!["/opt/pirs".to_owned(), "serve".to_owned()]), None).unwrap(),
            ["/opt/pirs", "serve"]
        );
        let named = vec![
            "/opt/pirs".to_owned(),
            "serve".to_owned(),
            "--socket".to_owned(),
            "/tmp/mine.sock".to_owned(),
        ];
        assert_eq!(
            server_command(Some(named.clone()), Some(socket)).unwrap(),
            named
        );
    }

    #[test]
    fn env_command_then_current_exe() {
        // This is the only test that touches `PIRS_SERVER_COMMAND`.
        std::env::set_var(SERVER_COMMAND_ENV, "'/opt/my pirs' serve --idle 5");
        assert_eq!(
            server_command(None, None).unwrap(),
            ["/opt/my pirs", "serve", "--idle", "5"]
        );
        std::env::set_var(SERVER_COMMAND_ENV, "   ");
        let default = server_command(None, None).unwrap();
        assert_eq!(default.len(), 2);
        assert_eq!(default[1], "serve");
        assert_eq!(
            default[0],
            std::env::current_exe().unwrap().to_string_lossy()
        );
        std::env::remove_var(SERVER_COMMAND_ENV);
        assert_eq!(server_command(None, None).unwrap(), default);
    }
}
