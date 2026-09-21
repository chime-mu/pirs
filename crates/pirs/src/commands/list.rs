//! `pirs --list`: the running agents, then this directory's conversations.
//!
//! Two different things, printed in that order and never mixed (D-28): a
//! running agent is a loop on the server, a conversation is a file on disk
//! that `--continue` starts a fresh agent on. Both come from one
//! `loop.list { cwd }` (D-38); nothing here reads the server's filesystem.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use pirs_client::Client;
use pirs_protocol::{LoopListParams, LoopState, ServerPath};

use crate::connect::{connect_options, resolve_cwd, resolve_socket};

/// Print both lists, most recent first. Nothing else goes to stdout.
pub(crate) async fn run(cwd: Option<&Path>, socket: Option<&Path>, no_start: bool) -> Result<i32> {
    let cwd = resolve_cwd(cwd)?;
    let socket = resolve_socket(socket);
    let client = Client::connect(connect_options(&socket, !no_start)).await?;
    let listed = client
        .loop_list(LoopListParams {
            cwd: Some(ServerPath::from(cwd)),
        })
        .await?;

    let mut loops = listed.loops;
    loops.sort_by_key(|info| std::cmp::Reverse(info.since));
    for info in &loops {
        let state = match info.state {
            LoopState::Working => "working",
            LoopState::Idle => "idle",
        };
        println!(
            "{}",
            columns(&[
                &info.id,
                state,
                &info.model.model,
                info.cwd.as_str(),
                info.name.as_deref().unwrap_or_default(),
            ])
        );
    }
    for conversation in &listed.conversations {
        println!(
            "{}",
            columns(&[
                &conversation.id,
                &iso(conversation.updated),
                conversation.name.as_deref().unwrap_or_default(),
            ])
        );
    }
    Ok(0)
}

/// Two spaces between fields, and no trailing ones for an absent name.
fn columns(fields: &[&str]) -> String {
    let mut line = fields.join("  ");
    while line.ends_with(' ') {
        line.pop();
    }
    line
}

/// Unix milliseconds as an ISO 8601 instant in UTC.
fn iso(millis: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp_nanos(0))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_two_spaces_apart_and_an_absent_name_adds_nothing() {
        assert_eq!(columns(&["a", "idle", "faux/scripted"]), "a  idle  faux/scripted");
        assert_eq!(columns(&["a", "idle", ""]), "a  idle");
    }

    #[test]
    fn timestamps_are_iso_in_utc() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_700_000_000_000), "2023-11-14T22:13:20Z");
    }
}
