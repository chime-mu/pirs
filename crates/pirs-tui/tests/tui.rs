//! The TUI against a fake server, driven through `Harness` and asserted on
//! `screen()`. Every test is one of the phase 3 acceptance points.

// The paths this file builds are its own temporary directories on this
// machine, never labels from a server, so D-31's ban on joining does not
// apply (see `clippy.toml`).
#![allow(clippy::disallowed_methods)]

mod support;

use std::time::Duration;

use pirs_protocol::{
    AssistantMessage, ChangedBy, Content, Delta, Event, FsChangedEvent, LoopMessageBody,
    LoopMessageEvent, LoopState, LoopStatusEvent, Manifest, Message, Role, StopReason,
    ToolResultMessage, UserMessage,
};
use pirs_client::{split_command, ServerConfig};
use pirs_tui::{run_headless, Harness, TuiOptions};
use serde_json::{json, Value};
use support::{loop_info, FakeServer, State};

const WAIT: Duration = Duration::from_secs(10);
const SIZE: (u16, u16) = (100, 30);

fn options(server: &FakeServer, config: Option<&str>) -> TuiOptions {
    // One server called `local`, named here rather than read from a
    // `servers.toml` this machine may or may not have.
    servers(
        server.dir(),
        config,
        vec![ServerConfig {
            socket: Some(server.socket()),
            ..ServerConfig::local()
        }],
    )
}

/// The same for any set of servers: the config file lives in `dir`.
fn servers(dir: &std::path::Path, config: Option<&str>, servers: Vec<ServerConfig>) -> TuiOptions {
    let config_path = dir.join("tui.toml");
    if let Some(text) = config {
        std::fs::write(&config_path, text).expect("a writable config");
    }
    let mut opts = TuiOptions::new("/srv/project");
    opts.servers = servers;
    opts.config_path = Some(config_path);
    opts.client_name = "pirs-tui test".to_owned();
    opts
}

/// A server of the pool: a fake on its own socket, under the name the
/// sidebar writes in front of every one of its loops.
fn named(name: &str, server: &FakeServer) -> ServerConfig {
    ServerConfig {
        name: name.to_owned(),
        command: None,
        socket: Some(server.socket()),
        editor_prefix: None,
    }
}

async fn start(server: &FakeServer, config: Option<&str>) -> Harness {
    Harness::start(options(server, config), SIZE)
        .await
        .expect("the harness starts")
}

fn two_loops() -> State {
    State {
        loops: vec![
            loop_info("l1", "alpha", LoopState::Working),
            loop_info("l2", "beta", LoopState::Idle),
        ],
        ..State::default()
    }
}

fn status(loop_id: &str, seq: u64, state: LoopState) -> Event {
    Event::LoopStatus(LoopStatusEvent {
        loop_id: loop_id.to_owned(),
        seq,
        state,
        since: seq * 10,
        detail: None,
    })
}

fn user(loop_id: &str, seq: u64, text: &str) -> Event {
    Event::LoopMessage(LoopMessageEvent {
        loop_id: loop_id.to_owned(),
        seq: Some(seq),
        role: Role::User,
        body: LoopMessageBody::Message {
            message: Box::new(Message::User(UserMessage {
                content: text.into(),
                timestamp: 0,
            })),
        },
    })
}

fn assistant(loop_id: &str, seq: u64, content: Vec<Content>) -> Event {
    Event::LoopMessage(LoopMessageEvent {
        loop_id: loop_id.to_owned(),
        seq: Some(seq),
        role: Role::Assistant,
        body: LoopMessageBody::Message {
            message: Box::new(Message::Assistant(AssistantMessage {
                content,
                api: "faux".to_owned(),
                provider: "faux".to_owned(),
                model: "scripted".to_owned(),
                response_id: None,
                usage: Default::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                raw_stop_reason: None,
                timestamp: 0,
            })),
        },
    })
}

fn tool_result(loop_id: &str, seq: u64, call_id: &str, name: &str, text: &str) -> Event {
    Event::LoopMessage(LoopMessageEvent {
        loop_id: loop_id.to_owned(),
        seq: Some(seq),
        role: Role::ToolResult,
        body: LoopMessageBody::Message {
            message: Box::new(Message::ToolResult(ToolResultMessage {
                tool_call_id: call_id.to_owned(),
                tool_name: name.to_owned(),
                content: vec![Content::text(text)],
                details: None,
                usage: None,
                is_error: false,
                timestamp: 0,
            })),
        },
    })
}

/// An `ask` tool call, the kind the `[[render]]` hook below answers.
fn ask_call(loop_id: &str, seq: u64, call_id: &str) -> Event {
    assistant(
        loop_id,
        seq,
        vec![Content::ToolCall {
            id: call_id.to_owned(),
            name: "ask".to_owned(),
            arguments: json!({ "question": "Which one?", "options": ["alpha option", "beta option"] }),
            thought_signature: None,
        }],
    )
}

/// A `[[render]]` hook for `ask` that prints lines and options.
const ASK_HOOK: &str = r#"
[[render]]
tool = "ask"
run = "cat > /dev/null; printf '%s\n' '{\"lines\":[\"Which one?\"],\"options\":[\"alpha option\",\"beta option\"]}'"
"#;

/// The end of a turn: sequenced, so it is replayed after a reconnect.
fn turn_end(loop_id: &str, seq: u64) -> Event {
    Event::LoopTurnEnd(pirs_protocol::LoopTurnEndEvent {
        loop_id: loop_id.to_owned(),
        seq,
        messages: Vec::new(),
    })
}

fn delta(loop_id: &str, text: &str) -> Event {
    Event::LoopMessage(LoopMessageEvent {
        loop_id: loop_id.to_owned(),
        seq: None,
        role: Role::Assistant,
        body: LoopMessageBody::Delta {
            delta: Delta::Text {
                index: 0,
                text: text.to_owned(),
            },
        },
    })
}

fn changed(loop_id: &str, seq: u64, path: &str) -> Event {
    Event::FsChanged(FsChangedEvent {
        loop_id: loop_id.to_owned(),
        seq,
        path: path.into(),
        by: ChangedBy::Tool,
    })
}

/// The sidebar row naming `label`.
fn sidebar_row<'a>(screen: &'a str, label: &str) -> Option<&'a str> {
    screen
        .lines()
        .find(|l| l.split('│').next().is_some_and(|s| s.contains(label)))
}

/// Whether the sidebar row for `label` carries the attention flag.
fn flagged(screen: &str, label: &str) -> bool {
    sidebar_row(screen, label).is_some_and(|row| row.contains(&format!("! {label}")))
}

async fn wait(harness: &Harness, what: &str, predicate: impl Fn(&str) -> bool) -> String {
    match harness.wait_until(predicate, WAIT).await {
        Ok(screen) => screen,
        Err(screen) => panic!("timed out waiting for {what}; last screen:\n{screen}"),
    }
}

// (1) Two loops from loop.list appear in the sidebar with their states.
#[tokio::test]
async fn sidebar_lists_loops_with_states() {
    let server = FakeServer::start(two_loops()).await;
    let harness = start(&server, None).await;
    let screen = wait(&harness, "both agents", |s| {
        sidebar_row(s, "alpha").is_some_and(|r| r.contains("working"))
            && sidebar_row(s, "beta").is_some_and(|r| r.contains("idle"))
    })
    .await;
    assert!(
        sidebar_row(&screen, "alpha").unwrap().starts_with('>'),
        "the first agent is selected:\n{screen}"
    );
    // beta was already stopped when the UI first heard of it and nobody has
    // looked at it since, so it wants attention (S12); alpha is working, and
    // the UI selected and drew it anyway.
    assert!(
        flagged(&screen, "beta"),
        "an agent idle at start-up is unviewed:\n{screen}"
    );
    assert!(!flagged(&screen, "alpha"), "{screen}");
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (1b) A loop another loop started is drawn under it, indented (S16).
#[tokio::test]
async fn a_child_loop_is_indented_under_its_parent() {
    let mut child = loop_info("l2", "review", LoopState::Working);
    child.parent = Some("l1".to_owned());
    let mut orphan = loop_info("l3", "stray", LoopState::Idle);
    orphan.parent = Some("gone".to_owned());
    let state = State {
        loops: vec![
            loop_info("l1", "alpha", LoopState::Working),
            child,
            orphan,
        ],
        ..State::default()
    };
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;
    let screen = wait(&harness, "all three agents", |s| {
        sidebar_row(s, "review").is_some() && sidebar_row(s, "stray").is_some()
    })
    .await;

    let parent = sidebar_row(&screen, "alpha ").expect("the parent row");
    let child = sidebar_row(&screen, "review").expect("the child row");
    let stray = sidebar_row(&screen, "stray").expect("the orphan row");
    let indent = |row: &str| row.len() - row.trim_start_matches([' ', '>', '!']).len();
    assert!(
        indent(child) > indent(parent),
        "the child is indented under its parent:\n{screen}"
    );
    assert_eq!(
        indent(stray),
        indent(parent),
        "a child whose parent is not listed stays at the top level:\n{screen}"
    );
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (1c) `loop.list` is ordered by id, so a child can arrive before its
// parent; the sidebar still draws it under the parent it is indented under.
#[tokio::test]
async fn a_child_whose_id_sorts_first_is_still_drawn_after_its_parent() {
    let mut child = loop_info("a2", "review", LoopState::Working);
    child.parent = Some("b1".to_owned());
    let state = State {
        // As `loop.list` sorts them: the child's id comes first.
        loops: vec![child, loop_info("b1", "alpha", LoopState::Working)],
        ..State::default()
    };
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;
    let screen = wait(&harness, "both agents", |s| {
        sidebar_row(s, "review").is_some() && sidebar_row(s, "alpha ").is_some()
    })
    .await;

    let row_of = |label: &str| {
        screen
            .lines()
            .position(|l| l.split('│').next().is_some_and(|s| s.contains(label)))
            .expect("a sidebar row")
    };
    assert!(
        row_of("alpha ") < row_of("review"),
        "the parent is drawn above the child it owns:\n{screen}"
    );
    let indent = |row: &str| row.len() - row.trim_start_matches([' ', '>', '!']).len();
    assert!(
        indent(sidebar_row(&screen, "review").unwrap())
            > indent(sidebar_row(&screen, "alpha ").unwrap()),
        "and the child is still indented:\n{screen}"
    );
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (2) The attention flag: idle while unviewed sets it, viewing clears it,
// working→idle sets it again.
#[tokio::test]
async fn attention_flag_follows_idle_and_viewing() {
    let mut state = two_loops();
    state.loops[1].state = LoopState::Working;
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;
    wait(&harness, "beta working", |s| {
        sidebar_row(s, "beta").is_some_and(|r| r.contains("working")) && !flagged(s, "beta")
    })
    .await;

    server.send(status("l2", 5, LoopState::Idle));
    wait(&harness, "beta flagged", |s| flagged(s, "beta")).await;

    // Viewing beta's page clears the flag.
    harness.key("down");
    wait(&harness, "beta viewed", |s| {
        !flagged(s, "beta") && sidebar_row(s, "beta").is_some_and(|r| r.starts_with('>'))
    })
    .await;

    // Back on alpha; beta works and stops again.
    harness.key("up");
    wait(&harness, "alpha selected", |s| {
        sidebar_row(s, "alpha").is_some_and(|r| r.starts_with('>'))
    })
    .await;
    server.send(status("l2", 6, LoopState::Working));
    server.send(status("l2", 7, LoopState::Idle));
    wait(&harness, "beta flagged again", |s| flagged(s, "beta")).await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (3) Selecting a loop attaches and subscribes since 0; the replay renders;
// a delta streams and the complete message replaces it.
#[tokio::test]
async fn selecting_replays_then_streams() {
    let mut state = two_loops();
    state.replay.insert(
        "l2".to_owned(),
        vec![
            user("l2", 1, "hello there"),
            assistant("l2", 2, vec![Content::text("hi back")]),
        ],
    );
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;

    // Start-up selects alpha: attach + subscribe since 0 for l1.
    server
        .next_request_where("attach l1", |r| {
            r.method == "loop.attach" && r.params["loop"] == "l1"
        })
        .await;
    let sub = server
        .next_request_where("subscribe l1", |r| {
            r.method == "subscribe" && r.params["loop"] == "l1"
        })
        .await;
    assert_eq!(sub.params["since"], json!(0));

    harness.key("down");
    server
        .next_request_where("attach l2", |r| {
            r.method == "loop.attach" && r.params["loop"] == "l2"
        })
        .await;
    let sub = server
        .next_request_where("subscribe l2", |r| {
            r.method == "subscribe" && r.params["loop"] == "l2"
        })
        .await;
    assert_eq!(sub.params["since"], json!(0));
    assert!(sub.params.get("events").is_none());

    wait(&harness, "the replay", |s| {
        s.contains("> hello there") && s.contains("hi back")
    })
    .await;

    server.send(delta("l2", "streaming partial"));
    wait(&harness, "the delta", |s| s.contains("streaming partial")).await;
    server.send(assistant("l2", 3, vec![Content::text("final answer")]));
    wait(&harness, "the complete message", |s| {
        s.contains("final answer") && !s.contains("streaming partial")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (4) fs.changed → jump list → file page from fs.read; a second fs.changed
// refreshes it.
#[tokio::test]
async fn file_pages_open_from_the_jump_list_and_refresh() {
    let mut state = two_loops();
    state.files.insert(
        "/srv/project/src/lib.rs".to_owned(),
        "fn one() {}\n".to_owned(),
    );
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;
    server
        .next_request_where("subscribe l1", |r| {
            r.method == "subscribe" && r.params["loop"] == "l1"
        })
        .await;

    server.send(changed("l1", 1, "/srv/project/src/lib.rs"));
    wait(&harness, "the jump list", |s| {
        s.contains("files: 1 /srv/project/src/lib.rs")
    })
    .await;

    harness.key("1");
    let read = server.next_request("fs.read").await;
    assert_eq!(read.params["path"], "/srv/project/src/lib.rs");
    assert_eq!(read.params["loop"], "l1");
    wait(&harness, "the file page", |s| s.contains("fn one() {}")).await;

    server.state().files.insert(
        "/srv/project/src/lib.rs".to_owned(),
        "fn two() {}\n".to_owned(),
    );
    server.send(changed("l1", 2, "/srv/project/src/lib.rs"));
    wait(&harness, "the refreshed page", |s| {
        s.contains("fn two() {}") && !s.contains("fn one() {}")
    })
    .await;

    // Tab returns to the agent page; the file tab stays open.
    harness.key("tab");
    wait(&harness, "the agent page again", |s| {
        s.contains("files: 1 /srv/project/src/lib.rs")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (5) A [[render]] hook for `ask` draws a picker; down+enter sends the
// second option as a prompt.
#[tokio::test]
async fn render_hook_picker_sends_the_choice() {
    let server = FakeServer::start(two_loops()).await;
    let harness = start(&server, Some(ASK_HOOK)).await;
    server
        .next_request_where("subscribe l1", |r| {
            r.method == "subscribe" && r.params["loop"] == "l1"
        })
        .await;

    server.send(ask_call("l1", 1, "call-1"));
    wait(&harness, "the picker", |s| {
        s.contains("> alpha option") && s.contains("  beta option") && s.contains("Which one?")
    })
    .await;

    harness.key("down").key("enter");
    let prompt = server.next_request("loop.prompt").await;
    assert_eq!(prompt.params["loop"], "l1");
    assert_eq!(prompt.params["text"], "beta option");
    assert_eq!(prompt.params["when"], "now");

    // The hook's lines replaced the default tool-call rendering.
    wait(&harness, "the rendered lines", |s| {
        s.contains("[ask] Which one?") && !s.contains("[call] ask")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (6) A command name defined by both sides shows as a conflict.
#[tokio::test]
async fn command_list_marks_conflicts() {
    let mut state = two_loops();
    let manifest: Manifest = serde_json::from_value(json!({
        "tools": [],
        "commands": [
            { "name": "theme", "description": "the server's theme" },
            { "name": "review", "description": "review the tree" }
        ],
        "status_keys": [],
        "widget_keys": []
    }))
    .unwrap();
    state.manifests.insert("l1".to_owned(), manifest);
    let server = FakeServer::start(state).await;
    let config = r#"
[[command]]
name = "review"
description = "the UI's review"
action = "echo ui"
"#;
    let harness = start(&server, Some(config)).await;
    server
        .next_request_where("subscribe l1", |r| {
            r.method == "subscribe" && r.params["loop"] == "l1"
        })
        .await;

    harness.key("/");
    let screen = wait(&harness, "the command list", |s| {
        s.contains("commands") && s.contains("quit")
    })
    .await;
    let theme = screen.lines().find(|l| l.contains("theme")).unwrap();
    assert!(theme.contains("conflict"), "{theme}");
    let review = screen.lines().find(|l| l.contains("review")).unwrap();
    assert!(review.contains("conflict"), "{review}");
    let new = screen.lines().find(|l| l.contains("new ")).unwrap();
    assert!(!new.contains("conflict"), "{new}");

    // Running a conflicting command does nothing but say so.
    harness.text("theme").key("enter");
    wait(&harness, "the conflict notice", |s| {
        s.contains("both the server and the UI")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (7) Enter → loop.prompt now; Alt+Enter → after_turn; Esc while working →
// loop.abort.
#[tokio::test]
async fn prompts_and_abort() {
    let server = FakeServer::start(two_loops()).await;
    let harness = start(&server, None).await;
    wait(&harness, "alpha selected", |s| {
        sidebar_row(s, "alpha").is_some_and(|r| r.starts_with('>'))
    })
    .await;

    harness.text("hello now").key("enter");
    let prompt = server.next_request("loop.prompt").await;
    assert_eq!(prompt.params["loop"], "l1");
    assert_eq!(prompt.params["text"], "hello now");
    assert_eq!(prompt.params["when"], "now");

    harness.text("hello later").key("alt-enter");
    let prompt = server.next_request("loop.prompt").await;
    assert_eq!(prompt.params["text"], "hello later");
    assert_eq!(prompt.params["when"], "after_turn");

    // alpha is working (from loop.list): Esc aborts.
    harness.key("esc");
    let abort = server.next_request("loop.abort").await;
    assert_eq!(abort.params["loop"], "l1");
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (8) Config reload picks up a changed status format.
#[tokio::test]
async fn config_reload_changes_the_status_line() {
    let server = FakeServer::start(two_loops()).await;
    let harness = start(&server, Some("status = \"S={loop.state}\"\n")).await;
    wait(&harness, "the first status", |s| {
        s.contains("alpha · S=working")
    })
    .await;

    std::fs::write(
        server.dir().join("tui.toml"),
        "status = \"state is {loop.state} on {branch}\"\n",
    )
    .unwrap();
    wait(&harness, "the reloaded status", |s| {
        s.contains("alpha · state is working on")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (9) The headless script protocol: dump after wait prints the screen, quit
// exits 0; an error line makes it 1.
#[tokio::test]
async fn headless_script_dumps_and_quits() {
    let server = FakeServer::start(two_loops()).await;
    let script = b"{\"wait\":\"alpha\",\"timeout\":10000}\n{\"settle\":50}\n{\"dump\":true}\n{\"quit\":true}\n";
    let mut out: Vec<u8> = Vec::new();
    let code = run_headless(options(&server, None), SIZE, &script[..], &mut out)
        .await
        .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(code, 0, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "{\"ok\":\"wait\"}");
    assert_eq!(lines[1], "{\"ok\":\"settle\"}");
    assert_eq!(lines[2], "{\"ok\":\"dump\"}");
    assert_eq!(lines[3], "=== screen ===");
    let end = lines.iter().position(|l| *l == "=== end ===").unwrap();
    assert_eq!(end - 4, SIZE.1 as usize, "one line per row");
    assert!(lines[4..end].iter().any(|l| l.contains("alpha")));
    assert_eq!(lines[end + 1], "{\"ok\":\"quit\"}");

    let script = b"{\"key\":\"hyper-x\"}\n{\"wait\":\"never on screen\",\"timeout\":100}\n";
    let mut out: Vec<u8> = Vec::new();
    let code = run_headless(options(&server, None), SIZE, &script[..], &mut out)
        .await
        .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(code, 1, "{out}");
    let errors: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(errors[0]["error"].as_str().unwrap().starts_with("key:"));
    assert!(errors[1]["error"].as_str().unwrap().contains("timed out"));
}

// (10) A replayed `ask` call does not ask again: its hook's lines are used,
// but a call a later user message already answered opens no picker. A live
// call with nothing after it still does (S6).
#[tokio::test]
async fn a_replayed_answered_call_opens_no_picker() {
    let mut state = two_loops();
    // The turn as the session log has it: the call, its result, the answer
    // the user gave then, and the assistant's reply to that answer.
    state.replay.insert(
        "l1".to_owned(),
        vec![
            ask_call("l1", 1, "call-1"),
            tool_result("l1", 2, "call-1", "ask", "no such tool"),
            user("l1", 3, "beta option"),
            assistant("l1", 4, vec![Content::text("you chose it")]),
        ],
    );
    let server = FakeServer::start(state).await;
    let harness = start(&server, Some(ASK_HOOK)).await;

    // The hook ran on the replayed call: its lines are on the page.
    wait(&harness, "the replayed turn", |s| {
        s.contains("[ask] Which one?") && s.contains("you chose it")
    })
    .await;
    harness.settle().await;
    let screen = harness.screen().await;
    assert!(
        !screen.contains("choose (enter picks"),
        "a replayed, answered call must not re-open its picker:\n{screen}"
    );

    // The same call live, with no user message after it, is a question.
    server.send(ask_call("l1", 5, "call-2"));
    wait(&harness, "the picker", |s| {
        s.contains("choose (enter picks") && s.contains("> alpha option")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (11) The same `loop.status` twice changes nothing the second time. The
// server fans an event out once per matching subscription
// (`crates/pirs-server/src/log.rs`, `fan_out`: the loop's own subscribers
// and the `*` list are two lists, and a UI is on both), so every status
// event reaches this client twice; `seq` is what makes the second one a
// no-op.
#[tokio::test]
async fn a_duplicate_status_event_changes_nothing() {
    let mut state = two_loops();
    state.loops[1].state = LoopState::Working;
    // The loop's own replay carries the event the `*` subscription also
    // delivers below, with the same seq: the same event twice.
    state
        .replay
        .insert("l2".to_owned(), vec![status("l2", 5, LoopState::Idle)]);
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;

    wait(&harness, "both agents working", |s| {
        sidebar_row(s, "beta").is_some_and(|r| r.contains("working"))
    })
    .await;

    // Selecting beta subscribes to it and replays the event; beta's page is
    // the one on screen, so drawing it clears the flag at once.
    harness.key("down");
    wait(&harness, "beta idle and viewed", |s| {
        sidebar_row(s, "beta").is_some_and(|r| r.starts_with('>') && r.contains("idle"))
            && !flagged(s, "beta")
    })
    .await;
    harness.key("up");
    wait(&harness, "alpha selected", |s| {
        sidebar_row(s, "alpha").is_some_and(|r| r.starts_with('>'))
    })
    .await;

    // The duplicate, as the `*` subscription delivers it.
    server.send(status("l2", 5, LoopState::Idle));
    harness.settle().await;
    let screen = harness.screen().await;
    assert!(
        !flagged(&screen, "beta"),
        "a status event already applied must not flag beta again:\n{screen}"
    );
    assert!(
        sidebar_row(&screen, "beta").is_some_and(|r| r.contains("idle")),
        "{screen}"
    );

    // A later event on the same loop still arrives: the flag is not stuck.
    server.send(status("l2", 6, LoopState::Idle));
    wait(&harness, "beta flagged by the new event", |s| {
        flagged(s, "beta")
    })
    .await;
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// ---------------------------------------------------------------- phase 6

// (6a) Two servers, one sidebar: every loop is `(server, loop)`, written
// `server:id`, and a request about one goes to its own server (S18, S21).
#[tokio::test]
async fn two_servers_share_one_sidebar_and_each_request_goes_to_its_own() {
    // The same loop id on both, so only the key can tell them apart.
    let one = FakeServer::start(State {
        loops: vec![loop_info("l1", "alpha", LoopState::Idle)],
        ..State::default()
    })
    .await;
    let two = FakeServer::start(State {
        loops: vec![loop_info("l1", "beta", LoopState::Idle)],
        ..State::default()
    })
    .await;
    let harness = Harness::start(
        servers(one.dir(), None, vec![named("one", &one), named("two", &two)]),
        SIZE,
    )
    .await
    .expect("the harness starts");

    let screen = wait(&harness, "both servers' loops", |s| {
        sidebar_row(s, "one:alpha").is_some() && sidebar_row(s, "two:beta").is_some()
    })
    .await;
    assert!(
        !screen.contains(" alpha ") || screen.contains("one:alpha"),
        "a loop is named after its server:\n{screen}"
    );

    // Select the loop on `two` and prompt it: the request must reach that
    // server and no other, although both have a loop called `l1`.
    harness.key("down");
    wait(&harness, "beta's page", |s| {
        sidebar_row(s, "two:beta").is_some_and(|r| r.starts_with('>'))
    })
    .await;
    two.next_request_where("attach l1 on two", |r| {
        r.method == "loop.attach" && r.params["loop"] == "l1"
    })
    .await;
    harness.text("only for two");
    harness.key("enter");
    let prompt = two
        .next_request_where("loop.prompt on two", |r| r.method == "loop.prompt")
        .await;
    assert_eq!(prompt.params["text"], json!("only for two"));
    assert_eq!(prompt.params["loop"], json!("l1"));

    let on_one = one.requests_so_far().await;
    assert!(
        on_one.iter().all(|r| r.method != "loop.prompt"),
        "server one saw a prompt meant for two: {:?}",
        on_one.iter().map(|r| r.method.clone()).collect::<Vec<_>>()
    );
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (6b) A dropped link: a notice, a reconnect with backoff, and the events
// missed while away replayed exactly once (D-06, S19).
#[tokio::test]
async fn a_dropped_link_reconnects_and_replays_what_was_missed() {
    let mut state = two_loops();
    state.replay.insert(
        "l1".to_owned(),
        vec![user("l1", 1, "hello there"), turn_end("l1", 2)],
    );
    let server = FakeServer::start(state).await;
    let harness = start(&server, None).await;
    wait(&harness, "the first replay", |s| s.contains("hello there")).await;

    // The turn that happens while the link is down: it is in the log, so a
    // subscription resuming from seq 2 replays it.
    server.state().replay.insert(
        "l1".to_owned(),
        vec![
            user("l1", 1, "hello there"),
            turn_end("l1", 2),
            assistant("l1", 3, vec![Content::text("MISSED ANSWER")]),
            turn_end("l1", 4),
        ],
    );
    server.drop_connections();

    wait(&harness, "the disconnected notice", |s| {
        s.contains("disconnected; reconnecting")
    })
    .await;
    wait(&harness, "the offline marker", |s| s.contains("! local offline")).await;

    let screen = wait(&harness, "the missed turn", |s| s.contains("MISSED ANSWER")).await;
    assert!(
        !screen.contains("! local offline"),
        "the marker goes when the link is back:\n{screen}"
    );
    assert_eq!(
        screen.matches("MISSED ANSWER").count(),
        1,
        "the replay arrives once:\n{screen}"
    );
    // And the resumed subscription asked for exactly what it was missing.
    let resumed = server
        .next_request_where("the resumed subscribe", |r| {
            r.method == "subscribe" && r.params["loop"] == "l1" && r.params["since"] == json!(2)
        })
        .await;
    assert_eq!(resumed.params["since"], json!(2));
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (6c) A server that refuses this client's protocol version on reconnect:
// a notice that stays, and no further attempt (S21).
#[tokio::test]
async fn a_refused_version_on_reconnect_is_a_notice_that_stays() {
    let server = FakeServer::start(two_loops()).await;
    // Wide enough for the whole message: it has to name both versions.
    let harness = Harness::start(options(&server, None), (140, 30))
        .await
        .expect("the harness starts");
    wait(&harness, "the sidebar", |s| sidebar_row(s, "alpha").is_some()).await;

    server.state().refuse_version = Some("1.0".to_owned());
    server.drop_connections();

    let refused = wait(&harness, "the refusal", |s| {
        s.contains("it speaks protocol 1.0")
    })
    .await;
    assert!(
        refused.contains("this client speaks 0.1"),
        "the notice names both versions:\n{refused}"
    );
    // Long enough for an ordinary notice to have expired, and for four
    // more attempts had any been made.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let screen = harness.screen().await;
    assert!(
        screen.contains("speaks protocol 1.0"),
        "a refused version does not fade away:\n{screen}"
    );
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (6d) The editor pane runs the server's own prefix in front of `$EDITOR`
// (D-29). `PIRS_TUI_TMUX` is the seam: it stands in for tmux and records
// nothing, so what is asserted is the command line the UI built.
#[tokio::test]
async fn the_editor_pane_uses_the_servers_prefix() {
    // The prefix the bridge command derives to; the derivation itself is
    // `pirs-client`'s own test.
    let bridge = ServerConfig {
        name: "build".to_owned(),
        command: Some(split_command("ssh build pirs proxy")),
        socket: None,
        editor_prefix: None,
    };
    assert_eq!(bridge.editor_prefix(), "ssh build");

    let mut state = two_loops();
    state
        .files
        .insert("/srv/project/src/lib.rs".to_owned(), "fn one() {}\n".to_owned());
    state.replay.insert(
        "l1".to_owned(),
        vec![changed("l1", 1, "/srv/project/src/lib.rs")],
    );
    let server = FakeServer::start(state).await;
    // A fake speaks over a socket, so the config carries the derived prefix
    // rather than the bridge command that produced it.
    let config = ServerConfig {
        name: "build".to_owned(),
        command: None,
        socket: Some(server.socket()),
        editor_prefix: Some(bridge.editor_prefix()),
    };
    // Only this test opens an editor pane, so the two variables it needs
    // are set here rather than around the whole binary.
    std::env::set_var("EDITOR", "vi");
    std::env::set_var("PIRS_TUI_TMUX", "true");
    let harness = Harness::start(servers(server.dir(), None, vec![config]), SIZE)
        .await
        .expect("the harness starts");

    wait(&harness, "the jump list", |s| s.contains("files: 1 ")).await;
    harness.key("1");
    wait(&harness, "the file page", |s| s.contains("fn one()")).await;
    harness.key("/");
    harness.text("edit");
    harness.key("enter");
    let screen = wait(&harness, "the editor pane", |s| s.contains("editor pane:")).await;
    assert!(
        screen.contains("ssh build vi /srv/project/src/lib.rs"),
        "the pane runs the server's prefix in front of the editor:\n{screen}"
    );
    std::env::remove_var("PIRS_TUI_TMUX");
    assert_eq!(harness.quit().await.unwrap(), 0);
}

// (6e) `/new` asks for a directory and then, because there is more than one
// server, which one runs the agent (S18).
#[tokio::test]
async fn new_asks_which_server_runs_the_agent() {
    let one = FakeServer::start(State {
        loops: vec![loop_info("l1", "alpha", LoopState::Idle)],
        ..State::default()
    })
    .await;
    let two = FakeServer::start(State::default()).await;
    let harness = Harness::start(
        servers(one.dir(), None, vec![named("one", &one), named("two", &two)]),
        SIZE,
    )
    .await
    .expect("the harness starts");
    wait(&harness, "the sidebar", |s| {
        sidebar_row(s, "one:alpha").is_some()
    })
    .await;

    harness.key("/");
    harness.text("new");
    harness.key("enter");
    wait(&harness, "the directory question", |s| s.contains("directory")).await;
    harness.key("ctrl-u");
    harness.text("/srv/other");
    harness.key("enter");
    let screen = wait(&harness, "the server question", |s| {
        s.contains("which server runs the agent in /srv/other?")
    })
    .await;
    assert!(
        screen.contains("> one") && screen.contains("two"),
        "both servers are offered, the selected agent's first:\n{screen}"
    );

    harness.key("down");
    harness.key("enter");
    let created = two
        .next_request_where("loop.create on two", |r| r.method == "loop.create")
        .await;
    assert_eq!(created.params["cwd"], json!("/srv/other"));
    let on_one = one.requests_so_far().await;
    assert!(
        on_one.iter().all(|r| r.method != "loop.create"),
        "the agent was started on the server that was chosen"
    );
    assert_eq!(harness.quit().await.unwrap(), 0);
}
