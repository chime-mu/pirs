//! The bridge, end to end: the real `pirs proxy` between a client and a
//! socket, and the `--server` surface that names which server to use (S18,
//! S20, S21, D-05).
//!
//! The servers here are fakes that speak nothing but `pirs-protocol`, so
//! these tests need no model, no provider and no network. What they exercise
//! is the transport and the command line: that a client reaches a server
//! through a program's stdio, that `pirs proxy` is that program, and that
//! `--server` picks between them.

// Temporary directories of this test's own, never labels from a server, so
// D-31's ban on joining paths does not apply here.
#![allow(clippy::disallowed_methods)]

mod support;

use std::path::Path;
use std::time::Duration;

use pirs_client::{Client, ConnectOptions};
use pirs_protocol::{
    Envelope, HelloResult, LoopInfo, LoopListResult, LoopState, ModelSpec, Request, RpcResponse,
    PROTOCOL_VERSION,
};
use serde_json::json;
use support::{Peer, TempDir};
use tokio::net::UnixListener;

/// The binary under test, built by cargo for this test.
const PIRS: &str = env!("CARGO_BIN_EXE_pirs");

/// The server's side of `hello`.
async fn answer_hello(peer: &mut Peer) {
    let request = peer.recv_request().await;
    assert_eq!(request.method, "hello");
    let result = HelloResult {
        server: "fake-pirs 0.1.0".to_owned(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    peer.send(&Envelope::Response(RpcResponse::ok(
        request.id,
        serde_json::to_value(result).expect("a serialisable result"),
    )))
    .await;
}

fn a_loop(id: &str) -> LoopInfo {
    LoopInfo {
        id: id.to_owned(),
        name: None,
        cwd: "/srv/project".into(),
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

/// A fake server that says hello, answers every `loop.list` with one loop,
/// and ends when the client goes away.
async fn serve_one_loop(listener: UnixListener, loop_id: String) {
    serve_once(&listener, &loop_id).await;
}

/// The same, for as many clients as turn up, until the task is dropped.
async fn serve_every_client(listener: UnixListener, loop_id: String) {
    loop {
        serve_once(&listener, &loop_id).await;
    }
}

/// One client, from `hello` to the connection closing.
async fn serve_once(listener: &UnixListener, loop_id: &str) {
    let mut peer = Peer::accept(listener).await;
    answer_hello(&mut peer).await;
    while let Some(Envelope::Request(request)) = peer.recv().await {
        let result = match Request::from_rpc(&request).expect("a known method") {
            Request::LoopList(_) => serde_json::to_value(LoopListResult {
                loops: vec![a_loop(loop_id)],
                conversations: Vec::new(),
            })
            .expect("a serialisable result"),
            _ => json!({}),
        };
        peer.send(&Envelope::Response(RpcResponse::ok(request.id, result)))
            .await;
    }
}

/// Run the binary with `PIRS_HOME` pointing at a temporary directory, so no
/// test ever reads the user's own configuration.
async fn pirs(home: &Path, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(PIRS)
            .args(args)
            .env("PIRS_HOME", home)
            .env_remove("PIRS_SOCKET")
            .output(),
    )
    .await
    .expect("pirs answers within twenty seconds")
    .expect("pirs runs")
}

#[tokio::test]
async fn a_client_reaches_a_server_through_pirs_proxy() {
    let dir = TempDir::new("bridge");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");
    let server = tokio::spawn(serve_one_loop(listener, "l1".to_owned()));

    // Exactly what `command = "..."` in servers.toml spawns, only without
    // the `ssh` or `docker exec` in front of it.
    let client = Client::connect(ConnectOptions::bridge(
        "test-client 0.1.0",
        vec![
            PIRS.to_owned(),
            "proxy".to_owned(),
            "--socket".to_owned(),
            socket.to_string_lossy().into_owned(),
        ],
    ))
    .await
    .expect("the bridge connects");

    assert_eq!(client.hello().server, "fake-pirs 0.1.0");
    // There is no socket on this side: the transport is the command.
    assert!(client.socket().is_none());
    let listed = client
        .loop_list(Default::default())
        .await
        .expect("a listing over the bridge");
    assert_eq!(listed.loops.len(), 1);
    assert_eq!(listed.loops[0].id, "l1");

    // Dropping the client kills the bridge, which closes the connection the
    // fake server is reading.
    drop(client);
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the fake server sees the bridge go away")
        .expect("the fake server finishes");
}

#[tokio::test]
async fn pirs_proxy_with_no_start_exits_one_when_there_is_no_server() {
    let dir = TempDir::new("no-server");
    let missing = dir.path().join("nothing.sock");
    let output = pirs(
        dir.path(),
        &[
            "proxy",
            "--no-start",
            "--socket",
            &missing.to_string_lossy(),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("nothing.sock"), "{stderr}");
    assert!(output.stdout.is_empty(), "a bridge writes nothing of its own");
}

/// Without `--no-start` the bridge starts the server on its own side: every
/// client auto-starts a missing server, and over SSH the proxy is the
/// client's stand-in on that machine (D-05).
#[tokio::test]
async fn pirs_proxy_starts_a_server_that_is_not_running() {
    let dir = TempDir::new("proxy-start");
    let socket = dir.socket();
    assert!(!socket.exists(), "nothing is listening yet");

    let client = Client::connect(ConnectOptions::bridge(
        "test-client 0.1.0",
        vec![
            PIRS.to_owned(),
            "proxy".to_owned(),
            "--socket".to_owned(),
            socket.to_string_lossy().into_owned(),
        ],
    ))
    .await
    .expect("the bridge starts a server and connects to it");
    assert!(client.hello().server.starts_with("pirs"), "{:?}", client.hello());
    // A real server, on the socket the bridge was given.
    assert!(socket.exists(), "the started server created {}", socket.display());
    drop(client);

    // Leave nothing running: the server it started is a real one, detached.
    let stopped = pirs(dir.path(), &["stop", "--socket", &socket.to_string_lossy()]).await;
    assert!(stopped.status.success(), "{:?}", stopped);
}

#[tokio::test]
async fn the_list_spans_every_server_and_names_each_loop_after_its_own() {
    let home = TempDir::new("two-servers");
    let one = TempDir::new("server-one");
    let two = TempDir::new("server-two");
    let first = UnixListener::bind(one.socket()).expect("a bindable socket");
    let second = UnixListener::bind(two.socket()).expect("a bindable socket");
    let servers = [
        tokio::spawn(serve_every_client(first, "aaa".to_owned())),
        tokio::spawn(serve_every_client(second, "bbb".to_owned())),
    ];

    std::fs::write(
        home.path().join("servers.toml"),
        format!(
            "[[server]]\nname = \"local\"\nsocket = {:?}\n\n\
             [[server]]\nname = \"two\"\ncommand = \"{} proxy --socket {}\"\n",
            one.socket().to_string_lossy(),
            PIRS,
            two.socket().to_string_lossy(),
        ),
    )
    .expect("a servers file");

    let output = pirs(home.path(), &["--list", "--no-start"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("local:aaa"), "{stdout}");
    assert!(stdout.contains("two:bbb"), "{stdout}");

    // One server, named, is listed plainly: there is nothing to tell apart.
    let output = pirs(home.path(), &["--list", "--server", "two", "--no-start"]).await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bbb"), "{stdout}");
    assert!(!stdout.contains("two:bbb"), "{stdout}");

    for server in servers {
        server.abort();
    }
}

#[tokio::test]
async fn stop_refuses_a_server_it_cannot_reach_the_machine_of() {
    let home = TempDir::new("stop-remote");
    std::fs::write(
        home.path().join("servers.toml"),
        "[[server]]\nname = \"local\"\n\n[[server]]\nname = \"build\"\ncommand = \"ssh build pirs proxy\"\n",
    )
    .expect("a servers file");

    let output = pirs(home.path(), &["--server", "build", "stop"]).await;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ssh build pirs proxy"), "{stderr}");
    assert!(stderr.contains("stop it where it runs"), "{stderr}");
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
}

#[tokio::test]
async fn a_server_the_file_does_not_name_is_an_error_that_says_what_there_is() {
    let home = TempDir::new("unknown-server");
    std::fs::write(
        home.path().join("servers.toml"),
        "[[server]]\nname = \"local\"\n\n[[server]]\nname = \"build\"\ncommand = \"ssh build pirs proxy\"\n",
    )
    .expect("a servers file");

    let output = pirs(home.path(), &["--server", "office", "--list"]).await;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("office"), "{stderr}");
    assert!(stderr.contains("local, build"), "{stderr}");
    assert!(stderr.contains("servers.toml"), "{stderr}");
}

#[tokio::test]
async fn a_servers_file_that_does_not_parse_names_itself() {
    let home = TempDir::new("bad-servers");
    std::fs::write(
        home.path().join("servers.toml"),
        "[[server]]\nname = \"build\"\nhost = \"build.example\"\n",
    )
    .expect("a servers file");

    let output = pirs(home.path(), &["--list"]).await;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("servers.toml"), "{stderr}");
    assert!(stderr.contains("host"), "{stderr}");
}
