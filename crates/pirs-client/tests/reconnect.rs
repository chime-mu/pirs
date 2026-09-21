//! A link that drops and comes back (S19, D-06).
//!
//! The fake server here does what an SSH link does when it dies: it closes
//! the connection in the middle of a run. The client is expected to come
//! back with the same subscriptions and registrations, ask for what happened
//! while it was away, and hand the caller each event exactly once.

mod support;

use std::time::Duration;

use pirs_client::{
    ConnectOptions, Pool, ReconnectingClient, ServerConfig, ServerItem,
};
use pirs_protocol::{
    code, Envelope, Event, HelloResult, LoopSelector, LoopState, LoopStatusEvent, Request,
    RpcError, RpcResponse, Slot, PROTOCOL_VERSION,
};
use serde_json::json;
use support::{Peer, TempDir};
use tokio::net::UnixListener;

/// The server's side of `hello`, at the version this client speaks.
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

/// Answer whatever request comes next with `{}` and return it.
async fn answer_empty(peer: &mut Peer) -> Request {
    let request = peer.recv_request().await;
    let parsed = Request::from_rpc(&request).expect("a known method");
    peer.send(&Envelope::Response(RpcResponse::ok(request.id, json!({}))))
        .await;
    parsed
}

fn status(loop_id: &str, seq: u64) -> Event {
    Event::LoopStatus(LoopStatusEvent {
        loop_id: loop_id.to_owned(),
        seq,
        state: LoopState::Idle,
        since: seq,
        detail: None,
    })
}

/// A local server on this socket, which never starts one of its own: a test
/// that spawned `pirs serve` would be testing the machine it runs on.
fn options(socket: &std::path::Path) -> ConnectOptions {
    let mut options = ConnectOptions::new("test-client 0.1.0");
    options.socket = Some(socket.to_path_buf());
    options.auto_start = false;
    options
}

fn config(name: &str) -> ServerConfig {
    ServerConfig {
        name: name.to_owned(),
        ..ServerConfig::local()
    }
}

/// The next item, or a failure rather than a hung test.
async fn next(stream: &mut pirs_client::ServerStream) -> ServerItem {
    tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("an item within five seconds")
        .expect("the stream is still open")
}

#[tokio::test]
async fn a_dropped_link_is_resumed_from_the_last_seq_and_replays_nothing_twice() {
    let dir = TempDir::new("reconnect");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        // First connection: hello, one subscription, one registration, one
        // event, then the link dies.
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        match answer_empty(&mut peer).await {
            Request::Subscribe(params) => {
                assert_eq!(params.loop_id, LoopSelector::Loop("l1".to_owned()));
                assert_eq!(params.since, None, "the first subscription has nothing to resume from");
            }
            other => panic!("expected subscribe, got {other:?}"),
        }
        match answer_empty(&mut peer).await {
            Request::Register(params) => assert_eq!(params.slot, Slot::Input),
            other => panic!("expected register, got {other:?}"),
        }
        peer.send(&Envelope::Notification(status("l1", 1).into_rpc()))
            .await;
        // The link dies here.
        drop(peer);

        // Second connection: the same subscription, resumed, and the same
        // registration.
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        match answer_empty(&mut peer).await {
            Request::Subscribe(params) => {
                assert_eq!(params.loop_id, LoopSelector::Loop("l1".to_owned()));
                assert_eq!(params.since, Some(1), "resumes from the last seq it saw");
            }
            other => panic!("expected subscribe, got {other:?}"),
        }
        match answer_empty(&mut peer).await {
            Request::Register(params) => {
                assert_eq!(params.slot, Slot::Input);
                assert_eq!(params.timeout, 5_000);
            }
            other => panic!("expected register, got {other:?}"),
        }
        // The replay: what was missed, and one event the client already has,
        // because a replay may overlap.
        peer.send(&Envelope::Notification(status("l1", 1).into_rpc()))
            .await;
        peer.send(&Envelope::Notification(status("l1", 2).into_rpc()))
            .await;
        peer
    });

    let client = ReconnectingClient::with_options(config("local"), options(&socket));
    client.reconnect().await.expect("a first connection");
    let mut items = client.items().expect("nobody has taken the stream");
    client
        .subscribe(LoopSelector::Loop("l1".to_owned()), None, None)
        .await
        .expect("subscribed");
    client
        .register("l1", Slot::Input, 5_000)
        .await
        .expect("registered");

    match next(&mut items).await {
        ServerItem::Event(event) => assert_eq!(event.seq(), Some(1)),
        other => panic!("expected the first event, got {other:?}"),
    }
    match next(&mut items).await {
        ServerItem::Disconnected { server } => assert_eq!(server, "local"),
        other => panic!("expected the link to drop, got {other:?}"),
    }
    assert!(!client.is_connected());
    assert_eq!(client.last_seq("l1"), Some(1));

    client.reconnect().await.expect("a second connection");
    assert!(client.is_connected());
    match next(&mut items).await {
        ServerItem::Reconnected { server } => assert_eq!(server, "local"),
        other => panic!("expected the reconnect marker, got {other:?}"),
    }
    // Only the missed event: the replayed `seq` 1 is dropped as already
    // seen, so the caller's stream has each event once.
    match next(&mut items).await {
        ServerItem::Event(event) => assert_eq!(event.seq(), Some(2)),
        other => panic!("expected the missed event, got {other:?}"),
    }
    assert_eq!(client.last_seq("l1"), Some(2));

    let _peer = server.await.expect("the fake server finishes");
}

#[tokio::test]
async fn a_server_on_another_protocol_major_is_refused_in_words() {
    let dir = TempDir::new("refused");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        drop(peer);

        // The server was upgraded while we were away.
        let mut peer = Peer::accept(&listener).await;
        let request = peer.recv_request().await;
        assert_eq!(request.method, "hello");
        peer.send(&Envelope::Response(RpcResponse::err(
            request.id,
            RpcError::new(code::VERSION_REFUSED, "this server speaks protocol 9.0")
                .with_data(json!({ "server": "9.0" })),
        )))
        .await;
        peer
    });

    let client = ReconnectingClient::with_options(config("build"), options(&socket));
    client.reconnect().await.expect("a first connection");
    let mut items = client.items().expect("nobody has taken the stream");
    match next(&mut items).await {
        ServerItem::Disconnected { .. } => {}
        other => panic!("expected the link to drop, got {other:?}"),
    }

    let error = client.reconnect().await.expect_err("the version is refused");
    let message = error.to_string();
    assert!(message.contains("9.0"), "{message}");
    assert!(message.contains(PROTOCOL_VERSION), "{message}");
    assert!(
        matches!(error, pirs_client::ClientError::VersionRefused { .. }),
        "{error:?}"
    );
    // And the wrapper says why it is down, rather than looking merely idle.
    assert!(!client.is_connected());
    assert!(client.last_error().expect("an error").contains("9.0"));

    let _peer = server.await.expect("the fake server finishes");
}

#[tokio::test]
async fn a_pool_tags_every_item_with_the_server_it_came_from() {
    let one = TempDir::new("pool-one");
    let two = TempDir::new("pool-two");

    let mut listeners = Vec::new();
    for dir in [&one, &two] {
        listeners.push(UnixListener::bind(dir.socket()).expect("a bindable socket"));
    }
    let mut servers = Vec::new();
    for (index, listener) in listeners.into_iter().enumerate() {
        servers.push(tokio::spawn(async move {
            let mut peer = Peer::accept(&listener).await;
            answer_hello(&mut peer).await;
            let request = answer_empty(&mut peer).await;
            assert!(matches!(request, Request::Subscribe(_)));
            peer.send(&Envelope::Notification(
                status(&format!("l{index}"), 1).into_rpc(),
            ))
            .await;
            peer
        }));
    }

    let clients = vec![
        ReconnectingClient::with_options(config("one"), options(&one.socket())),
        ReconnectingClient::with_options(config("two"), options(&two.socket())),
    ];
    for client in &clients {
        client.reconnect().await.expect("a connection");
    }
    let pool = Pool::from_clients(clients);
    assert_eq!(pool.servers(), ["one", "two"]);
    assert!(pool.client("three").is_none());

    for name in ["one", "two"] {
        pool.client(name)
            .expect("a client")
            .subscribe(LoopSelector::All, None, None)
            .await
            .expect("subscribed");
    }

    let mut events = pool.events().expect("nobody has taken the stream");
    let mut seen = Vec::new();
    for _ in 0..2 {
        let (server, item) = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("an item within five seconds")
            .expect("the stream is open");
        match item {
            ServerItem::Event(event) => seen.push((server, event.loop_id().to_owned())),
            other => panic!("expected an event, got {other:?}"),
        }
    }
    seen.sort();
    assert_eq!(
        seen,
        [
            ("one".to_owned(), "l0".to_owned()),
            ("two".to_owned(), "l1".to_owned())
        ]
    );

    for server in servers {
        let _peer = server.await.expect("the fake server finishes");
    }
}

#[tokio::test]
async fn reconnecting_a_live_wrapper_is_not_a_second_connection() {
    let dir = TempDir::new("idempotent");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    // One connection, answered and then held open. The listener accepts
    // nothing else, so a second connection would be a hello nobody answers.
    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        peer
    });

    let client = ReconnectingClient::with_options(config("local"), options(&socket));
    client.reconnect().await.expect("a first connection");
    let mut items = client.items().expect("nobody has taken the stream");
    for _ in 0..2 {
        client
            .reconnect()
            .await
            .expect("the connection is already there");
    }
    assert!(client.is_connected());
    // Nothing on the stream: no second `Reconnected`, and above all no
    // `Disconnected` from a connection replaced behind the caller's back.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), items.recv())
            .await
            .is_err(),
        "reconnecting a live wrapper put something on the stream"
    );

    let _peer = server.await.expect("the fake server finishes");
}

#[tokio::test]
async fn a_loops_replay_outlives_a_wildcard_that_is_already_delivering() {
    let dir = TempDir::new("wildcard");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        // First connection: `*` for statuses and the loop's own
        // subscription, one event, then the link dies.
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        match answer_empty(&mut peer).await {
            Request::Subscribe(params) => assert_eq!(params.loop_id, LoopSelector::All),
            other => panic!("expected the `*` subscribe, got {other:?}"),
        }
        match answer_empty(&mut peer).await {
            Request::Subscribe(params) => {
                assert_eq!(params.loop_id, LoopSelector::Loop("l1".to_owned()));
            }
            other => panic!("expected the loop's subscribe, got {other:?}"),
        }
        peer.send(&Envelope::Notification(status("l1", 1).into_rpc()))
            .await;
        drop(peer);

        // Second connection. Each re-subscribe starts delivering the moment
        // it is answered, as a real server's does: the loop's own replays
        // the missed seq 2, and `*` carries a live status at seq 3 in the
        // same loop's `seq` space. Answered in whichever order they arrive,
        // so the test is about the client's order and not the server's.
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        for _ in 0..2 {
            match answer_empty(&mut peer).await {
                Request::Subscribe(params) => match params.loop_id {
                    LoopSelector::Loop(_) => {
                        assert_eq!(params.since, Some(1), "resumes from the last seq it saw");
                        peer.send(&Envelope::Notification(status("l1", 2).into_rpc()))
                            .await;
                    }
                    LoopSelector::All => {
                        peer.send(&Envelope::Notification(status("l1", 3).into_rpc()))
                            .await;
                    }
                },
                other => panic!("expected a subscribe, got {other:?}"),
            }
        }
        peer
    });

    let client = ReconnectingClient::with_options(config("local"), options(&socket));
    client.reconnect().await.expect("a first connection");
    let mut items = client.items().expect("nobody has taken the stream");
    client
        .subscribe(LoopSelector::All, Some(vec!["loop.status".to_owned()]), None)
        .await
        .expect("subscribed to `*`");
    client
        .subscribe(LoopSelector::Loop("l1".to_owned()), None, None)
        .await
        .expect("subscribed to the loop");

    match next(&mut items).await {
        ServerItem::Event(event) => assert_eq!(event.seq(), Some(1)),
        other => panic!("expected the first event, got {other:?}"),
    }
    match next(&mut items).await {
        ServerItem::Disconnected { .. } => {}
        other => panic!("expected the link to drop, got {other:?}"),
    }

    client.reconnect().await.expect("a second connection");
    match next(&mut items).await {
        ServerItem::Reconnected { .. } => {}
        other => panic!("expected the reconnect marker, got {other:?}"),
    }
    // The replay first and the live status after it, each exactly once: the
    // loop is re-subscribed before `*`, so nothing has carried its tracker
    // past what the replay was for.
    match next(&mut items).await {
        ServerItem::Event(event) => assert_eq!(event.seq(), Some(2), "the missed event is dropped"),
        other => panic!("expected the missed event, got {other:?}"),
    }
    match next(&mut items).await {
        ServerItem::Event(event) => assert_eq!(event.seq(), Some(3)),
        other => panic!("expected the live event, got {other:?}"),
    }
    assert_eq!(client.last_seq("l1"), Some(3));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), items.recv())
            .await
            .is_err(),
        "an event arrived twice"
    );

    let _peer = server.await.expect("the fake server finishes");
}
