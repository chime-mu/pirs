//! `pirs --list`: the running agents, then this directory's conversations.
//!
//! Two different things, printed in that order and never mixed (D-28): a
//! running agent is a loop on a server, a conversation is a file on disk
//! that `--continue` starts a fresh agent on. Both come from one
//! `loop.list { cwd }` (D-38); nothing here reads a server's filesystem.
//!
//! With more than one server configured (D-05) the list spans them all and
//! every id is written `server:id`, because two servers can hand out the
//! same id and a loop is `(server, loop)` from here on. `--server <name>`
//! lists that one server, unprefixed. Conversations are asked for only where
//! this directory means something — the local server — since a path is owned
//! by the server that produced it (D-31); `--server <name> --list` passes it
//! on as written, since the user named the server they mean.

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use pirs_protocol::{ConversationInfo, LoopInfo, LoopListParams, LoopState, ServerPath};

use crate::cli::GlobalArgs;
use crate::connect::{all_servers, connect_to, resolve_cwd_on, resolve_server};

/// What one server answered.
struct Listing {
    server: String,
    loops: Vec<LoopInfo>,
    conversations: Vec<ConversationInfo>,
}

/// Print both lists, most recent first. Nothing else goes to stdout.
pub(crate) async fn run(global: &GlobalArgs) -> Result<i32> {
    let (targets, one) = match &global.server {
        Some(name) => (
            vec![resolve_server(Some(name), global.socket.as_deref())?],
            true,
        ),
        None => {
            let mut all = all_servers()?;
            let single = all.len() == 1;
            if let Some(socket) = global.socket.as_deref() {
                for config in all.iter_mut() {
                    if !config.is_remote() && (single || config.name == pirs_client::LOCAL) {
                        config.socket = Some(socket.to_path_buf());
                    }
                }
            }
            (all, single)
        }
    };

    let mut listings = Vec::new();
    let mut failed = false;
    for config in &targets {
        // A directory is the server's, not the client's (D-31). Asked about
        // one server — `--server build --list`, the same line print mode
        // takes — the directory is passed on as written, because the user
        // named the server they mean. Sweeping every server, a local path
        // means nothing on a remote one, so only the local servers are
        // asked about this directory's conversations.
        let cwd = match !one && config.is_remote() {
            true => None,
            false => Some(ServerPath::from(resolve_cwd_on(config, global.cwd.as_deref())?)),
        };
        let listed = async {
            let client = connect_to(config, !global.no_start).await?;
            let listed = client.loop_list(LoopListParams { cwd }).await?;
            anyhow::Ok(listed)
        }
        .await;
        match listed {
            Ok(listed) => listings.push(Listing {
                server: config.name.clone(),
                loops: listed.loops,
                conversations: listed.conversations,
            }),
            // One server being unreachable is not the end of the list: the
            // others are still there, and the exit code says something was
            // missed. With only one server there is nothing to carry on
            // with, and the error is the whole answer.
            Err(error) if one => return Err(error),
            Err(error) => {
                eprintln!("pirs: {}: {error}", config.name);
                failed = true;
            }
        }
    }

    let prefix = |server: &str, id: &str| match one {
        true => id.to_owned(),
        false => format!("{server}:{id}"),
    };

    for listing in &mut listings {
        listing.loops.sort_by_key(|info| std::cmp::Reverse(info.since));
        for info in &listing.loops {
            let state = match info.state {
                LoopState::Working => "working",
                LoopState::Idle => "idle",
            };
            println!(
                "{}",
                columns(&[
                    &prefix(&listing.server, &info.id),
                    state,
                    &info.model.model,
                    info.cwd.as_str(),
                    info.name.as_deref().unwrap_or_default(),
                ])
            );
        }
    }
    for listing in &listings {
        for conversation in &listing.conversations {
            println!(
                "{}",
                columns(&[
                    &prefix(&listing.server, &conversation.id),
                    &iso(conversation.updated),
                    conversation.name.as_deref().unwrap_or_default(),
                ])
            );
        }
    }
    Ok(if failed { 1 } else { 0 })
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
