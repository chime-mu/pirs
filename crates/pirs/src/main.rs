//! `pirs`: the command, the server, and the UI.
//!
//! One binary with four jobs, because a user should install one thing:
//!
//! - print mode (`pirs "prompt"`) — [`print`], the whole of S1 and S2;
//! - `pirs serve` — the loop server in this process, and what a client
//!   auto-starts;
//! - `pirs proxy` — the bridge a remote or contained server is reached
//!   through;
//! - `pirs stop`, `pirs --list`, `pirs check` and `pirs wait` — the small
//!   administrative commands;
//! - `pirs tui` — the reference UI, a client of the server like any other.
//!
//! This file is argument parsing and dispatch; everything else is in
//! [`print`] and [`commands`]. Diagnostics go to stderr; `PIRS_LOG` turns on
//! `tracing` output, also on stderr, so it never mixes with an answer.

#![deny(unreachable_pub)]

mod cli;
mod commands;
mod connect;
mod print;

use clap::Parser;

use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    init_tracing();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("pirs: cannot start the async runtime: {error}");
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(dispatch(cli));
    std::process::exit(code);
}

/// Run the chosen command and turn its error into an exit code.
async fn dispatch(cli: Cli) -> i32 {
    let global = cli.global;
    let result = match cli.command {
        Some(Command::Serve { idle }) => commands::serve::run(idle, global.socket.as_deref()).await,
        Some(Command::Proxy) => {
            commands::proxy::run(global.socket.as_deref(), global.no_start).await
        }
        Some(Command::Stop) => {
            commands::stop::run(global.server.as_deref(), global.socket.as_deref()).await
        }
        Some(Command::Check) => commands::check::run(&global).await,
        Some(Command::Wait { target }) => commands::wait::run(&target, &global).await,
        Some(Command::Tui { headless, config }) => {
            commands::tui::run(headless, config, &global).await
        }
        None if cli.run.list => commands::list::run(&global).await,
        None => print::run(cli.run, global).await,
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("pirs: {}", report(&error));
            1
        }
    }
}

/// One plain line for an error and its causes.
///
/// `{:#}` would do, except that the error types here already name their
/// source in their own message (`#[error("... {source}")]`), so it prints
/// half of them twice. A cause the message already contains is dropped.
fn report(error: &anyhow::Error) -> String {
    let mut message = error.to_string();
    for cause in error.chain().skip(1) {
        let text = cause.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
    }
    message
}

/// `PIRS_LOG` turns on tracing to stderr; without it the binary is silent.
fn init_tracing() {
    let Ok(filter) = std::env::var("PIRS_LOG") else {
        return;
    };
    if filter.trim().is_empty() {
        return;
    }
    let env_filter = match tracing_subscriber::EnvFilter::try_new(&filter) {
        Ok(env_filter) => env_filter,
        Err(error) => {
            eprintln!("pirs: PIRS_LOG={filter:?} is not a filter: {error}");
            return;
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::report;

    #[test]
    fn a_cause_the_message_already_names_is_not_repeated() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let inner = anyhow::Error::new(io);
        let wrapped = inner.context("cannot read the pid file: no such file");
        assert_eq!(report(&wrapped), "cannot read the pid file: no such file");
    }

    #[test]
    fn a_cause_the_message_does_not_name_is_appended() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let wrapped = anyhow::Error::new(io).context("binding the socket");
        assert_eq!(report(&wrapped), "binding the socket: no such file");
    }
}
