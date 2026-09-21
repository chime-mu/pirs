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
    // Not `args_conflicts_with_subcommands`: clap stops looking for a
    // subcommand once that is set and any argument has been seen, so
    // `pirs --cwd DIR check` would be the *prompt* "check". A subcommand name
    // is still only recognised before the prompt begins, so
    // `pirs "check the build"` stays a prompt.
    subcommand_negates_reqs = true
)]
pub(crate) struct Cli {
    /// `serve`, `proxy`, `stop`, `check`, `wait`, `ext` or `tui`; absent
    /// means print mode.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    /// Print mode and `--list`.
    #[command(flatten)]
    pub(crate) run: RunArgs,

    /// The options every mode takes.
    #[command(flatten)]
    pub(crate) global: GlobalArgs,
}

/// The options that mean the same thing in every mode, so they are accepted
/// on either side of a subcommand: `pirs --cwd DIR check` and
/// `pirs check --cwd DIR` are the same line.
#[derive(Debug, clap::Args)]
pub(crate) struct GlobalArgs {
    /// Working directory for the agent (default: the current one).
    #[arg(long, global = true, value_name = "DIR")]
    pub(crate) cwd: Option<PathBuf>,

    /// The server to work on, by the name `~/.pirs/servers.toml` gives it
    /// (default: `local`). `--list` without it lists every server.
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) server: Option<String>,

    /// The local server's unix socket; overrides `PIRS_SOCKET`.
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) socket: Option<PathBuf>,

    /// Fail instead of starting a server when none is listening.
    #[arg(long, global = true)]
    pub(crate) no_start: bool,
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
    },

    /// Forward protocol lines between stdin/stdout and the server's socket.
    ///
    /// The bridge a remote or contained server is reached through (D-05,
    /// D-36): `ssh build pirs proxy` on the other machine,
    /// `docker exec -i jail pirs proxy` into a container. It parses nothing
    /// but the line endings, opens no port, and knows nothing about loops.
    /// A server that is not running is started, because that is what every
    /// client does with a missing server and the bridge is the client's
    /// stand-in on that machine; `--no-start` fails instead.
    ///
    /// Exits 0 when either side closes, 1 when the socket cannot be reached.
    Proxy,

    /// Close the running agents and stop the server.
    ///
    /// Only a local server: a server reached through a bridge command is
    /// stopped where it runs.
    Stop,

    /// Print the policy a loop in this directory would start with.
    ///
    /// The merged files, the manifest, every conflict, and the fully
    /// assembled system prompt. Exits 1 when anything conflicts, so a
    /// script can gate on it.
    Check,

    /// Wait until an agent is idle, then exit (for scripts).
    ///
    /// The argument is a running agent's id or its name, as `pirs --list`
    /// shows them. Prints the state the wait ended in; exits 0 when the
    /// agent is idle and 1 when there is no such agent.
    Wait {
        /// The agent to wait for: its id or its name.
        #[arg(value_name = "LOOP")]
        target: String,
    },

    /// Write a policy file from a sentence, or rebuild one from its own
    /// intent.
    ///
    /// The convenience for when no agent is running (D-33): `ext new` asks
    /// the model for one `*.pirs.toml` from `docs/dsl.md` and the sentence,
    /// writes it under `<cwd>/.pirs/ext/`, and prints what `pirs check` would
    /// print for the directory. `ext regen <file>` does the same from the
    /// file's own `intent`, which is the shareable unit: the file is a cache
    /// of it.
    Ext {
        /// `new` or `regen`.
        #[command(subcommand)]
        command: ExtCommand,
    },

    /// The terminal UI: a sidebar of agents, one page each, files and
    /// widgets.
    ///
    /// A client like any other: it talks to the loop server over the socket
    /// and starts one if none is listening. `--cwd` is the directory whose
    /// conversations the sidebar lists. The sidebar spans every server
    /// `~/.pirs/servers.toml` names (D-05), each agent shown `server:id`;
    /// `--server <name>` narrows it to one and `--socket` overrides the
    /// local one.
    Tui {
        /// Drive the UI from a script instead of a terminal, on a screen
        /// this many columns by rows (`100x30`).
        ///
        /// The scriptable mode the acceptance tests use: stdin is a script
        /// of JSON lines, one command per line, and stdout carries an echo
        /// per command and the screens `{"dump":true}` asks for. The
        /// commands are listed in `crates/pirs-tui/README.md`.
        #[arg(long, value_name = "WxH", value_parser = parse_size)]
        headless: Option<(u16, u16)>,

        /// The UI's configuration file (default: `~/.pirs/tui.toml`).
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
}

/// What `pirs ext` does: write a file from a sentence, or write it again.
#[derive(Debug, Subcommand)]
pub(crate) enum ExtCommand {
    /// Write `<cwd>/.pirs/ext/<name>.pirs.toml` from a sentence.
    ///
    /// Several words are joined with spaces, so the quotes are optional.
    /// The file's `intent` is the sentence verbatim, whatever the model
    /// wrote, because `ext regen` rebuilds the file from it. Exits 1 when
    /// the file does not parse or the check conflicts on it; the file is
    /// left in place either way.
    New {
        /// The sentence: what the file is for, in your own words.
        #[arg(value_name = "INTENT", required = true)]
        intent: Vec<String>,

        /// The file name, without `.pirs.toml` (default: a slug of the
        /// intent).
        #[arg(long, value_name = "NAME")]
        name: Option<String>,

        /// Model as `provider/id` (default: the loop's own).
        #[arg(short = 'm', long, value_name = "SPEC")]
        model: Option<String>,

        /// Overwrite a file of that name instead of picking a free one.
        #[arg(long)]
        force: bool,
    },

    /// Write a policy file again from its own `intent`.
    ///
    /// The file that is there is copied to `<file>.bak` first.
    Regen {
        /// The policy file to rebuild.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Model as `provider/id` (default: the loop's own).
        #[arg(short = 'm', long, value_name = "SPEC")]
        model: Option<String>,
    },
}

/// `--headless <W>x<H>`, the size of the screen the script draws on.
fn parse_size(value: &str) -> Result<(u16, u16), String> {
    let (w, h) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{value:?} is not a size: write it as WxH, e.g. 100x30"))?;
    let parse = |part: &str, what: &str| -> Result<u16, String> {
        part.trim()
            .parse::<u16>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("{value:?}: {what} {part:?} is not a positive number"))
    };
    Ok((parse(w, "the width")?, parse(h, "the height")?))
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
        let (run, global) = (cli.run, cli.global);
        assert_eq!(run.model.as_deref(), Some("faux/scripted"));
        assert_eq!(run.thinking, Some(ThinkingLevel::High));
        assert_eq!(global.cwd, Some(PathBuf::from("/tmp/p")));
        assert_eq!(run.name.as_deref(), Some("api work"));
        assert_eq!(global.socket, Some(PathBuf::from("/tmp/s.sock")));
        assert!(global.no_start);
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
        assert_eq!(cli.global.cwd, Some(PathBuf::from("/tmp/p")));
    }

    #[test]
    fn serve_defaults_to_ten_minutes_idle() {
        let cli = parse(&["pirs", "serve"]);
        match cli.command {
            Some(Command::Serve { idle }) => assert_eq!(idle, 600),
            other => panic!("expected serve, got {other:?}"),
        }
        assert!(cli.global.socket.is_none());
        let cli = parse(&["pirs", "serve", "--idle", "1", "--socket", "/tmp/s"]);
        match cli.command {
            Some(Command::Serve { idle }) => assert_eq!(idle, 1),
            other => panic!("expected serve, got {other:?}"),
        }
        assert_eq!(cli.global.socket, Some(PathBuf::from("/tmp/s")));
    }

    #[test]
    fn stop_takes_a_socket() {
        let cli = parse(&["pirs", "stop", "--socket", "/tmp/s"]);
        assert!(matches!(cli.command, Some(Command::Stop)), "{:?}", cli.command);
        assert_eq!(cli.global.socket, Some(PathBuf::from("/tmp/s")));
    }

    #[test]
    fn proxy_takes_a_socket_and_nothing_else() {
        let cli = parse(&["pirs", "proxy"]);
        assert!(matches!(cli.command, Some(Command::Proxy)), "{:?}", cli.command);
        assert!(cli.global.socket.is_none());
        let cli = parse(&["pirs", "proxy", "--socket", "/run/pirs.sock"]);
        assert!(matches!(cli.command, Some(Command::Proxy)), "{:?}", cli.command);
        assert_eq!(cli.global.socket, Some(PathBuf::from("/run/pirs.sock")));
        // A bridge has no prompt and no options of its own.
        let no_start = parse(&["pirs", "proxy", "--no-start"]);
        assert!(matches!(no_start.command, Some(Command::Proxy)), "{:?}", no_start.command);
        assert!(no_start.global.no_start);
        assert!(Cli::try_parse_from(["pirs", "proxy", "hi"]).is_err());
        assert!(Cli::try_parse_from(["pirs", "proxy", "--listen", "8080"]).is_err());
        // And a prompt that starts with the word is still a prompt.
        let prompt = parse(&["pirs", "proxy the request"]);
        assert!(prompt.command.is_none(), "{:?}", prompt.command);
    }

    #[test]
    fn server_names_which_server_and_defaults_to_none() {
        assert_eq!(parse(&["pirs", "hi"]).global.server, None);
        let cli = parse(&["pirs", "--server", "two", "hi"]);
        assert_eq!(cli.global.server.as_deref(), Some("two"));
        assert_eq!(cli.run.prompt, ["hi"]);
        // It is global: it comes on either side of a subcommand.
        for args in [
            ["pirs", "--server", "build", "stop"],
            ["pirs", "stop", "--server", "build"],
        ] {
            let cli = parse(&args);
            assert!(matches!(cli.command, Some(Command::Stop)), "{:?}", cli.command);
            assert_eq!(cli.global.server.as_deref(), Some("build"));
        }
        let cli = parse(&["pirs", "--list", "--server", "build"]);
        assert!(cli.run.list);
        assert_eq!(cli.global.server.as_deref(), Some("build"));
        let cli = parse(&["pirs", "wait", "a7f3", "--server", "build"]);
        assert_eq!(cli.global.server.as_deref(), Some("build"));
        let cli = parse(&["pirs", "check", "--server", "build"]);
        assert!(matches!(cli.command, Some(Command::Check)), "{:?}", cli.command);
        assert_eq!(cli.global.server.as_deref(), Some("build"));
        // It takes a name.
        assert!(Cli::try_parse_from(["pirs", "--server"]).is_err());
    }

    #[test]
    fn check_takes_a_cwd_and_defaults_to_this_one() {
        let cli = parse(&["pirs", "check"]);
        assert!(matches!(cli.command, Some(Command::Check)), "{:?}", cli.command);
        assert!(cli.global.cwd.is_none() && cli.global.socket.is_none() && !cli.global.no_start);
        let cli = parse(&["pirs", "check", "--cwd", "/tmp/p", "--no-start"]);
        assert!(matches!(cli.command, Some(Command::Check)), "{:?}", cli.command);
        assert_eq!(cli.global.cwd, Some(PathBuf::from("/tmp/p")));
        assert!(cli.global.no_start);
    }

    #[test]
    fn a_global_option_may_come_before_the_subcommand() {
        let before = parse(&["pirs", "--cwd", "/tmp/p", "check"]);
        assert!(matches!(before.command, Some(Command::Check)), "{:?}", before.command);
        assert_eq!(before.global.cwd, Some(PathBuf::from("/tmp/p")));
        assert!(before.run.prompt.is_empty(), "`check` is the subcommand, not a prompt");
        let after = parse(&["pirs", "check", "--cwd", "/tmp/p"]);
        assert!(matches!(after.command, Some(Command::Check)), "{:?}", after.command);
        assert_eq!(after.global.cwd, Some(PathBuf::from("/tmp/p")));
    }

    #[test]
    fn a_prompt_that_starts_with_a_command_name_is_still_a_prompt() {
        let cli = parse(&["pirs", "check the build"]);
        assert!(cli.command.is_none(), "{:?}", cli.command);
        assert_eq!(cli.run.prompt, ["check the build"]);
        let with_cwd = parse(&["pirs", "--cwd", "/tmp/p", "check the build"]);
        assert!(with_cwd.command.is_none(), "{:?}", with_cwd.command);
        assert_eq!(with_cwd.run.prompt, ["check the build"]);
        assert_eq!(with_cwd.global.cwd, Some(PathBuf::from("/tmp/p")));
    }

    #[test]
    fn wait_takes_one_agent_and_the_global_options() {
        match parse(&["pirs", "wait", "a7f3"]).command {
            Some(Command::Wait { target }) => assert_eq!(target, "a7f3"),
            other => panic!("expected wait, got {other:?}"),
        }
        let cli = parse(&["pirs", "wait", "review", "--socket", "/tmp/s.sock", "--no-start"]);
        match cli.command {
            Some(Command::Wait { target }) => assert_eq!(target, "review"),
            other => panic!("expected wait, got {other:?}"),
        }
        assert_eq!(cli.global.socket, Some(PathBuf::from("/tmp/s.sock")));
        assert!(cli.global.no_start);
        // The agent is required: `pirs wait` alone is an error, not a
        // prompt.
        assert!(Cli::try_parse_from(["pirs", "wait"]).is_err());
        // A prompt that starts with the word is still a prompt.
        let prompt = parse(&["pirs", "wait for the build"]);
        assert!(prompt.command.is_none(), "{:?}", prompt.command);
    }

    #[test]
    fn ext_new_takes_an_intent_a_name_a_model_and_the_global_options() {
        match parse(&["pirs", "ext", "new", "show the git branch"]).command {
            Some(Command::Ext {
                command: ExtCommand::New { intent, name, model, force },
            }) => {
                assert_eq!(intent, ["show the git branch"]);
                assert_eq!(name, None);
                assert_eq!(model, None);
                assert!(!force);
            }
            other => panic!("expected ext new, got {other:?}"),
        }
        // Unquoted words are joined by the command, as in print mode.
        let cli = parse(&[
            "pirs", "ext", "new", "--name", "custom", "-m", "faux/scripted", "--force", "--cwd",
            "/tmp/p", "--no-start", "show", "the", "branch",
        ]);
        match cli.command {
            Some(Command::Ext {
                command: ExtCommand::New { intent, name, model, force },
            }) => {
                assert_eq!(intent, ["show", "the", "branch"]);
                assert_eq!(name.as_deref(), Some("custom"));
                assert_eq!(model.as_deref(), Some("faux/scripted"));
                assert!(force);
            }
            other => panic!("expected ext new, got {other:?}"),
        }
        assert_eq!(cli.global.cwd, Some(PathBuf::from("/tmp/p")));
        assert!(cli.global.no_start);
        // An intent is required, and `ext` alone is not a command.
        assert!(Cli::try_parse_from(["pirs", "ext", "new"]).is_err());
        assert!(Cli::try_parse_from(["pirs", "ext"]).is_err());
        // A prompt that starts with the word is still a prompt.
        let prompt = parse(&["pirs", "ext new files"]);
        assert!(prompt.command.is_none(), "{:?}", prompt.command);
    }

    #[test]
    fn ext_regen_takes_one_file() {
        match parse(&["pirs", "ext", "regen", ".pirs/ext/a.pirs.toml"]).command {
            Some(Command::Ext {
                command: ExtCommand::Regen { file, model },
            }) => {
                assert_eq!(file, PathBuf::from(".pirs/ext/a.pirs.toml"));
                assert_eq!(model, None);
            }
            other => panic!("expected ext regen, got {other:?}"),
        }
        let cli = parse(&["pirs", "ext", "regen", "a.pirs.toml", "-m", "faux/scripted", "--server", "build"]);
        match cli.command {
            Some(Command::Ext {
                command: ExtCommand::Regen { file, model },
            }) => {
                assert_eq!(file, PathBuf::from("a.pirs.toml"));
                assert_eq!(model.as_deref(), Some("faux/scripted"));
            }
            other => panic!("expected ext regen, got {other:?}"),
        }
        assert_eq!(cli.global.server.as_deref(), Some("build"));
        // One file, and it is required.
        assert!(Cli::try_parse_from(["pirs", "ext", "regen"]).is_err());
        assert!(Cli::try_parse_from(["pirs", "ext", "regen", "a", "b"]).is_err());
    }

    #[test]
    fn tui_takes_a_headless_size_a_config_and_the_global_options() {
        match parse(&["pirs", "tui"]).command {
            Some(Command::Tui { headless, config }) => {
                assert_eq!(headless, None);
                assert_eq!(config, None);
            }
            other => panic!("expected tui, got {other:?}"),
        }
        let cli = parse(&[
            "pirs",
            "tui",
            "--headless",
            "100x30",
            "--config",
            "/tmp/tui.toml",
            "--cwd",
            "/tmp/p",
            "--socket",
            "/tmp/s.sock",
            "--no-start",
        ]);
        match cli.command {
            Some(Command::Tui { headless, config }) => {
                assert_eq!(headless, Some((100, 30)));
                assert_eq!(config, Some(PathBuf::from("/tmp/tui.toml")));
            }
            other => panic!("expected tui, got {other:?}"),
        }
        assert_eq!(cli.global.cwd, Some(PathBuf::from("/tmp/p")));
        assert_eq!(cli.global.socket, Some(PathBuf::from("/tmp/s.sock")));
        // `--no-start` reaches the UI as `TuiOptions::auto_start`.
        assert!(cli.global.no_start);
        assert!(!parse(&["pirs", "tui"]).global.no_start);
    }

    #[test]
    fn the_old_interactive_modes_options_are_gone() {
        // `pirs tui` is a client now: `pi-cli`'s flags are not accepted and
        // its `--extension` is nowhere in the help (D-04).
        assert!(Cli::try_parse_from(["pirs", "tui", "--extension", "x"]).is_err());
        assert!(Cli::try_parse_from(["pirs", "tui", "-p", "hi"]).is_err());
        let help = Cli::command()
            .find_subcommand_mut("tui")
            .expect("tui is a subcommand")
            .render_long_help()
            .to_string();
        assert!(!help.contains("--extension"), "{help}");
        assert!(help.contains("--headless"), "{help}");
    }

    #[test]
    fn a_headless_size_is_two_positive_numbers() {
        assert_eq!(parse_size("100x30").unwrap(), (100, 30));
        assert_eq!(parse_size("80X24").unwrap(), (80, 24));
        assert!(parse_size("100").unwrap_err().contains("WxH"));
        assert!(parse_size("0x30").unwrap_err().contains("width"));
        assert!(parse_size("100xtall").unwrap_err().contains("height"));
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
