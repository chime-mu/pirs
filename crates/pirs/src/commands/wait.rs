//! `pirs wait <loop>`: block until an agent is idle (S17).
//!
//! What a shell script needs when it starts an agent and wants to know when
//! it has stopped: no polling, no events, one request. The argument is a
//! loop id or the name a loop was created with — running loops only, because
//! `loop.wait` is about a loop, not about a conversation on disk (D-28).
//!
//! Exit 0 once the loop is idle, 1 when there is no such loop. The state the
//! wait ended in — always `idle`, because that is when `loop.wait` answers —
//! is printed on stdout, so a script can read it too.

use std::path::Path;

use anyhow::Result;
use pirs_client::Client;
use pirs_protocol::{LoopInfo, LoopListParams, LoopState};

use crate::connect::{connect_options, resolve_socket};

/// Wait for the named loop and print the state it ended in.
pub(crate) async fn run(target: &str, socket: Option<&Path>, no_start: bool) -> Result<i32> {
    let socket = resolve_socket(socket);
    let client = Client::connect(connect_options(&socket, !no_start)).await?;
    let listed = client.loop_list(LoopListParams::default()).await?;
    let Some(info) = resolve(&listed.loops, target) else {
        eprintln!("pirs: no running agent {target:?}");
        return Ok(1);
    };
    let result = client.loop_wait(&info.id).await?;
    // The wait answers when the loop is idle and not before, so `idle` is
    // the only state it can end in (see `loop.wait` in docs/protocol.md).
    // It is printed all the same, so a script can read it.
    debug_assert_eq!(result.state, LoopState::Idle);
    println!("idle");
    Ok(0)
}

/// The loop with this id, or else the one with this name. An id is tried
/// first so a loop named after another loop's id cannot hide it.
fn resolve<'a>(loops: &'a [LoopInfo], target: &str) -> Option<&'a LoopInfo> {
    loops
        .iter()
        .find(|info| info.id == target)
        .or_else(|| loops.iter().find(|info| info.name.as_deref() == Some(target)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pirs_protocol::ModelSpec;

    fn info(id: &str, name: Option<&str>) -> LoopInfo {
        LoopInfo {
            id: id.to_owned(),
            name: name.map(str::to_owned),
            cwd: "/work".into(),
            model: ModelSpec {
                model: "faux/scripted".to_owned(),
                thinking: None,
            },
            state: LoopState::Idle,
            since: 1,
            conversation: format!("c-{id}"),
            parent: None,
        }
    }

    #[test]
    fn an_id_wins_over_a_name_and_a_name_is_found() {
        let loops = [info("a1", Some("review")), info("b2", Some("a1"))];
        assert_eq!(resolve(&loops, "a1").map(|i| i.id.as_str()), Some("a1"));
        assert_eq!(resolve(&loops, "review").map(|i| i.id.as_str()), Some("a1"));
        assert_eq!(resolve(&loops, "b2").map(|i| i.id.as_str()), Some("b2"));
        assert!(resolve(&loops, "nobody").is_none());
    }
}
