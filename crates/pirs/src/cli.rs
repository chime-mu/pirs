//! The command line: what `pirs --help` prints, and nothing else.
//!
//! Parsing lives here so `main.rs` is dispatch only and so the surface can be
//! unit-tested with [`clap::Parser::try_parse_from`] rather than by running
//! the binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use pirs_protocol::ThinkingLevel;

/// `pirs`: one agent per command, a server that starts itself.
#[derive(Debug, Parser)]
#[command(
    name = "pirs",
    version,
    about = "Run a coding agent from the command line",
    long_about = "Run a coding agent from the command line.\n\n\
                  `pirs \"prompt\"` starts an agent in this directory, streams the answer, \
                  and closes the agent when it exits; the conversation stays on disk and \
                  `--continue` starts a fresh agent on it. A loop server is started for \
                  you if none is running, and exits by itself when it has been idle.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub(crate) struct Cli {
    /// `serve`, `stop` or `tui`; absent means print mode.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    /// Print mode and `--list`.
    #[command(flatten)]
    pub(crate) run: RunArgs,
}

/// Everything `pirs [OPTIONS] [PROMPT]...` takes.
#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    /// The prompt. Several words are joined with spaces.
    #[arg(value_name = "PROMPT")]
    pub(crate) prompt: Vec<String>,

    /// Model as `provider/id`, or an id the server's registry resolves.
    #[arg(short = 'm', long, value_name = "SPEC")]
    pub(crate) model: Option<String>,

    /// How hard the model thinks: off, minimal, low, medium, high, xhigh, max.
    #[arg(long, value_name = "LEVEL", value_parser = parse_thinking)]
    pub(crate) thinking: Option<ThinkingLevel>,

    /// Working directory for the agent (default: the current one).
    #[arg(long, value_name = "DIR")]
    pub(crate) cwd: Option<PathBuf>,

    /// Name the conversation, so `--continue=<name>` can find it later.
    #[arg(long, value_name = "NAME")]
    pub(crate) name: Option<String>,

    /// Continue a conversation with a fresh agent: alone, the most recent one
    /// in this directory;
    /// with a value (`--continue=<name-or-id>`, `-c=<name>`), that one. The
    /// value needs the `=` so a bare `--continue "prompt"` is the prompt,
    /// not the conversation.
    #[arg(
        short = 'c',
        long = "continue",
        value_name = "NAME_OR_ID",
        num_args = 0..=1,
        require_equals = true
    )]
    pub(crate) continue_: Option<Option<String>>,

    /// List running agents, then this directory's conversations, and exit.
    #[arg(long)]
    pub(crate) list: bool,

    /// The server's unix socket; overrides `PIRS_SOCKET`.
    #[arg(long, value_name = "PATH")]
    pub(crate) socket: Option<PathBuf>,

    /// Fail instead of starting a server when none is listening.
    #[arg(long)]
    pub(crate) no_start: bool,
}

/// The subcommands. Print mode is the absent one.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run the loop server in this process until it is stopped or idle.
    ///
    /// This is what a client auto-starts, so its arguments stay compatible.
    Serve {
        /// Exit after this many seconds with no working agent and no client.
        #[arg(long, value_name = "SECS", default_value_t = 600)]
        idle: u64,
        /// The unix socket to listen on; overrides `PIRS_SOCKET`.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
    },

    /// Close the running agents and stop the server.
    Stop {
        /// The server's unix socket; overrides `PIRS_SOCKET`.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
    },

    /// The old in-process interactive mode: a phase 1-3 stopgap.
    ///
    /// Every argument is passed to it unchanged, `--help` included; it does
    /// not speak the protocol and does not use the loop server. Phase 3
    /// replaces it with a client of the server.
    #[command(disable_help_flag = true)]
    Tui {
        /// Arguments for the old interactive mode (`pirs tui --help`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "ARGS")]
        args: Vec<String>,
    },
}

/// `--thinking <level>`, spelled as the protocol spells it.
fn parse_thinking(value: &str) -> Result<ThinkingLevel, String> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::Xhigh),
        "max" => Ok(ThinkingLevel::Max),
        other => Err(format!(
            "unknown thinking level {other:?}: off, minimal, low, medium, high, xhigh or max"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("parses")
    }

    #[test]
    fn a_bare_prompt_is_print_mode() {
        let cli = parse(&["pirs", "why does the build fail?"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.run.prompt, ["why does the build fail?"]);
        assert!(!cli.run.list);
        assert!(cli.run.continue_.is_none());
    }

    #[test]
    fn print_mode_options() {
        let cli = parse(&[
            "pirs",
            "-m",
            "faux/scripted",
            "--thinking",
            "high",
            "--cwd",
            "/tmp/p",
            "--name",
            "api work",
            "--socket",
            "/tmp/s.sock",
            "--no-start",
            "hi",
            "there",
        ]);
        let run = cli.run;
        assert_eq!(run.model.as_deref(), Some("faux/scripted"));
        assert_eq!(run.thinking, Some(ThinkingLevel::High));
        assert_eq!(run.cwd, Some(PathBuf::from("/tmp/p")));
        assert_eq!(run.name.as_deref(), Some("api work"));
        assert_eq!(run.socket, Some(PathBuf::from("/tmp/s.sock")));
        assert!(run.no_start);
        assert_eq!(run.prompt, ["hi", "there"]);
    }

    #[test]
    fn continue_alone_takes_no_value_and_leaves_the_prompt_alone() {
        let cli = parse(&["pirs", "--continue", "keep going"]);
        assert_eq!(cli.run.continue_, Some(None));
        assert_eq!(cli.run.prompt, ["keep going"]);
    }

    #[test]
    fn continue_with_a_value_needs_an_equals_sign() {
        let cli = parse(&["pirs", "--continue=api work", "more"]);
        assert_eq!(cli.run.continue_, Some(Some("api work".to_owned())));
        assert_eq!(cli.run.prompt, ["more"]);
        let short = parse(&["pirs", "-c=api", "more"]);
        assert_eq!(short.run.continue_, Some(Some("api".to_owned())));
    }

    #[test]
    fn list_takes_a_cwd() {
        let cli = parse(&["pirs", "--list", "--cwd", "/tmp/p"]);
        assert!(cli.run.list);
        assert_eq!(cli.run.cwd, Some(PathBuf::from("/tmp/p")));
    }

    #[test]
    fn serve_defaults_to_ten_minutes_idle() {
        match parse(&["pirs", "serve"]).command {
            Some(Command::Serve { idle, socket }) => {
                assert_eq!(idle, 600);
                assert!(socket.is_none());
            }
            other => panic!("expected serve, got {other:?}"),
        }
        match parse(&["pirs", "serve", "--idle", "1", "--socket", "/tmp/s"]).command {
            Some(Command::Serve { idle, socket }) => {
                assert_eq!(idle, 1);
                assert_eq!(socket, Some(PathBuf::from("/tmp/s")));
            }
            other => panic!("expected serve, got {other:?}"),
        }
    }

    #[test]
    fn stop_takes_a_socket() {
        match parse(&["pirs", "stop", "--socket", "/tmp/s"]).command {
            Some(Command::Stop { socket }) => assert_eq!(socket, Some(PathBuf::from("/tmp/s"))),
            other => panic!("expected stop, got {other:?}"),
        }
    }

    #[test]
    fn tui_passes_everything_through_including_help() {
        match parse(&["pirs", "tui", "--help"]).command {
            Some(Command::Tui { args }) => assert_eq!(args, ["--help"]),
            other => panic!("expected tui, got {other:?}"),
        }
        match parse(&["pirs", "tui", "-p", "hi", "--model", "faux/scripted"]).command {
            Some(Command::Tui { args }) => assert_eq!(args, ["-p", "hi", "--model", "faux/scripted"]),
            other => panic!("expected tui, got {other:?}"),
        }
    }

    #[test]
    fn unknown_thinking_levels_are_refused() {
        let error = Cli::try_parse_from(["pirs", "--thinking", "sideways", "hi"]).unwrap_err();
        assert!(error.to_string().contains("sideways"), "{error}");
    }

    #[test]
    fn the_command_line_is_internally_consistent() {
        Cli::command().debug_assert();
    }
}
