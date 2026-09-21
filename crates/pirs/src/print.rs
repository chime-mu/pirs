//! Print mode: one prompt, one answer on stdout, no agent left behind (D-28).
//!
//! The whole of S1 is here. Connect (starting a server if none is running),
//! create a loop in this directory — a fresh conversation, or a fresh agent on
//! a stored one (`--continue`, D-25) — subscribe, prompt, stream the
//! assistant's text to stdout as it arrives, and when the run ends wait for
//! idle and close the loop. The conversation stays on disk; nothing keeps
//! running but the server, which exits when it has been idle long enough.
//!
//! `ui.notify` goes to stderr as `[level] text` and widgets are ignored: print
//! mode has no screen to draw them on.
//!
//! Ctrl-C ends the answer: the run is aborted, the agent is closed (D-28) and
//! the process leaves with [`EXIT_INTERRUPTED`], printing nothing more on
//! stdout so a half-written answer stays exactly as far as it got. Only the
//! code mapping is unit-tested; the signal itself is a manual test —
//! `pirs "count slowly to fifty"`, Ctrl-C, then `pirs stop` must report no
//! running loop.

use std::io::Write as _;

use anyhow::{bail, Context as _, Result};
use futures::StreamExt as _;
use pirs_client::Client;
use pirs_protocol::{
    ConversationInfo, Delta, Event, LoopCreateParams, LoopListParams, LoopMessageBody, LoopSelector,
    Message, ModelSpec, NotifyLevel, PromptWhen, ServerPath, SubscribeParams,
};

use crate::cli::RunArgs;
use crate::connect::{connect_options, resolve_cwd, resolve_socket};

/// Run one prompt and return the process exit code.
pub(crate) async fn run(args: RunArgs) -> Result<i32> {
    let prompt = args.prompt.join(" ");
    if prompt.trim().is_empty() {
        bail!("no prompt given; try `pirs \"why does the build fail?\"` or `pirs --help`");
    }
    let cwd = resolve_cwd(args.cwd.as_deref())?;
    let socket = resolve_socket(args.socket.as_deref());
    let client = Client::connect(connect_options(&socket, !args.no_start)).await?;

    let session = match &args.continue_ {
        None => None,
        Some(wanted) => {
            let listed = client
                .loop_list(LoopListParams {
                    cwd: Some(ServerPath::from(cwd.clone())),
                })
                .await?;
            let chosen = select_conversation(&listed.conversations, wanted.as_deref())
                .map_err(anyhow::Error::msg)?;
            Some(chosen.id.clone())
        }
    };

    let info = client
        .loop_create(LoopCreateParams {
            cwd: ServerPath::from(cwd),
            model: args.model.clone().map(|model| ModelSpec {
                model,
                thinking: args.thinking,
            }),
            name: args.name.clone(),
            session,
        })
        .await?;

    // `--thinking` without `--model` has no `ModelSpec` to travel in at
    // create, so it is a `loop.model` on the model the loop already has.
    if let (Some(level), None) = (args.thinking, args.model.as_ref()) {
        client
            .loop_model(
                &info.id,
                ModelSpec {
                    model: info.model.model.clone(),
                    thinking: Some(level),
                },
            )
            .await?;
    }

    client
        .subscribe(SubscribeParams {
            loop_id: LoopSelector::Loop(info.id.clone()),
            events: None,
            since: None,
        })
        .await?;
    let mut events = client.events().context("the event stream was already taken")?;

    client.loop_prompt(&info.id, prompt, PromptWhen::Now).await?;

    // Ctrl-C at any point below leaves through the `None` arm. The streaming
    // and the wait are both interruptible; the abort and close that follow
    // are not, because they are what stops the agent.
    let finished = tokio::select! {
        outcome = stream_until_run_end(&mut events, &info.id) => Some(outcome),
        _ = tokio::signal::ctrl_c() => None,
    };
    let finished = match finished {
        None => None,
        Some(outcome) => tokio::select! {
            waited = client.loop_wait(&info.id) => {
                waited?;
                Some(outcome)
            }
            _ = tokio::signal::ctrl_c() => None,
        },
    };

    let Some(outcome) = finished else {
        let _ = client.loop_abort(&info.id).await;
        let _ = client.loop_close(&info.id).await;
        drop(events);
        return Ok(EXIT_INTERRUPTED);
    };

    client.loop_close(&info.id).await?;
    drop(events);

    if let Some(error) = &outcome.error {
        eprintln!("error: {error}");
    }
    Ok(exit_code(Some(&outcome)))
}

/// What a run the user interrupted with Ctrl-C leaves behind: 128 + SIGINT,
/// the shell's convention.
pub(crate) const EXIT_INTERRUPTED: i32 = 130;

/// The process's exit code: `None` is an interrupted run, `Some` a finished
/// one, which fails only when its last assistant message carried an error.
fn exit_code(outcome: Option<&Outcome>) -> i32 {
    match outcome {
        None => EXIT_INTERRUPTED,
        Some(outcome) if outcome.error.is_some() => 1,
        Some(_) => 0,
    }
}

/// What the run said, once it ended.
#[derive(Debug, Default)]
struct Outcome {
    /// The last assistant message's `errorMessage`, if it had one.
    error: Option<String>,
}

/// Print the loop's text as it streams and stop at its `loop.run_end`.
async fn stream_until_run_end(events: &mut pirs_client::EventStream, loop_id: &str) -> Outcome {
    let mut outcome = Outcome::default();
    let mut printed = false;
    let mut ends_with_newline = true;
    while let Some(event) = events.next().await {
        if event.loop_id() != loop_id {
            continue;
        }
        match event {
            Event::LoopMessage(message) => match message.body {
                LoopMessageBody::Delta {
                    delta: Delta::Text { text, .. },
                } => {
                    if !text.is_empty() {
                        print!("{text}");
                        let _ = std::io::stdout().flush();
                        printed = true;
                        ends_with_newline = text.ends_with('\n');
                    }
                }
                LoopMessageBody::Delta { .. } => {}
                LoopMessageBody::Message { message } => {
                    if let Message::Assistant(assistant) = *message {
                        outcome.error = assistant.error_message;
                    }
                }
            },
            Event::UiNotify(notify) => {
                eprintln!("[{}] {}", level_name(notify.level), notify.text);
            }
            Event::LoopRunEnd(_) => break,
            _ => {}
        }
    }
    if printed && !ends_with_newline {
        println!();
    }
    outcome
}

/// `ui.notify`'s level, as print mode spells it on stderr.
fn level_name(level: NotifyLevel) -> &'static str {
    match level {
        NotifyLevel::Info => "info",
        NotifyLevel::Warning => "warning",
        NotifyLevel::Error => "error",
    }
}

/// Which stored conversation `--continue` means (D-25, D-38).
///
/// With no value, the most recently updated conversation in the directory.
/// With one, the conversation whose id or name is exactly that — the most
/// recent, when a name was used twice. Neither is a fallback for the other: a
/// value that names nothing is an error, not "the most recent".
pub(crate) fn select_conversation<'a>(
    conversations: &'a [ConversationInfo],
    wanted: Option<&str>,
) -> Result<&'a ConversationInfo, String> {
    let most_recent = |mut candidates: Vec<&'a ConversationInfo>| -> Option<&'a ConversationInfo> {
        candidates.sort_by_key(|c| std::cmp::Reverse(c.updated));
        candidates.into_iter().next()
    };
    match wanted {
        None => most_recent(conversations.iter().collect()).ok_or_else(|| {
            "no conversation in this directory to continue; run `pirs \"prompt\"` first".to_owned()
        }),
        Some(wanted) => {
            let matching: Vec<&ConversationInfo> = conversations
                .iter()
                .filter(|c| c.id == wanted || c.name.as_deref() == Some(wanted))
                .collect();
            most_recent(matching)
                .ok_or_else(|| format!("no conversation named {wanted:?} in this directory"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(id: &str, name: Option<&str>, updated: u64) -> ConversationInfo {
        ConversationInfo {
            id: id.to_owned(),
            name: name.map(str::to_owned),
            cwd: ServerPath::from("/tmp/p"),
            path: ServerPath::from(format!("/tmp/sessions/{id}.jsonl")),
            updated,
        }
    }

    #[test]
    fn an_interrupted_run_exits_130_and_a_finished_one_by_its_error() {
        assert_eq!(exit_code(None), 130, "128 + SIGINT");
        assert_eq!(exit_code(Some(&Outcome::default())), 0);
        assert_eq!(exit_code(Some(&Outcome { error: Some("boom".into()) })), 1);
    }

    #[test]
    fn no_value_takes_the_most_recent() {
        let all = vec![
            conversation("old", None, 10),
            conversation("new", None, 30),
            conversation("middle", None, 20),
        ];
        assert_eq!(select_conversation(&all, None).unwrap().id, "new");
    }

    #[test]
    fn a_value_matches_an_id_or_a_name() {
        let all = vec![
            conversation("abc", Some("api work"), 10),
            conversation("def", None, 30),
        ];
        assert_eq!(select_conversation(&all, Some("abc")).unwrap().id, "abc");
        assert_eq!(
            select_conversation(&all, Some("api work")).unwrap().id,
            "abc"
        );
    }

    #[test]
    fn a_repeated_name_takes_the_most_recent_of_them() {
        let all = vec![
            conversation("one", Some("notes"), 10),
            conversation("two", Some("notes"), 40),
            conversation("three", None, 99),
        ];
        assert_eq!(select_conversation(&all, Some("notes")).unwrap().id, "two");
    }

    #[test]
    fn a_value_that_names_nothing_is_an_error() {
        let all = vec![conversation("abc", Some("api work"), 10)];
        let error = select_conversation(&all, Some("nope")).unwrap_err();
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn nothing_to_continue_is_an_error() {
        let error = select_conversation(&[], None).unwrap_err();
        assert!(error.contains("no conversation"), "{error}");
    }
}
