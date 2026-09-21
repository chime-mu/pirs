//! The client against a fake server that speaks only `pirs-protocol`.

mod support;

use std::time::Duration;

use pirs_client::{Client, ClientError, ConnectOptions};
use pirs_protocol::{
    code, Empty, Envelope, Event, HelloParams, HelloResult, Id, LoopInfo, LoopListParams,
    LoopListResult, LoopSelector, LoopState, LoopStatusEvent, ModelSpec, PromptWhen, Request,
    RpcError, RpcResponse, SlotReply, SlotRequest, ToolCallPayload, ToolContent, ToolReply,
    PROTOCOL_VERSION,
};
use serde_json::json;
use support::{Peer, TempDir};
use tokio::net::UnixListener;

/// The server's side of `hello`.
async fn answer_hello(peer: &mut Peer) {
    let request = peer.recv_request().await;
    assert_eq!(request.method, "hello");
    let params: HelloParams =
        serde_json::from_value(request.params.clone()).expect("hello params");
    assert_eq!(params.protocol_version, PROTOCOL_VERSION);
    assert_eq!(params.client, "test-client 0.1.0");
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

fn options(socket: &std::path::Path) -> ConnectOptions {
    let mut options = ConnectOptions::new("test-client 0.1.0");
    options.socket = Some(socket.to_path_buf());
    options.auto_start = false;
    options
}

fn a_loop() -> LoopInfo {
    LoopInfo {
        id: "l1".to_owned(),
        name: Some("work".to_owned()),
        cwd: "/srv/project".into(),
        model: ModelSpec {
            model: "faux/scripted".to_owned(),
            thinking: None,
        },
        state: LoopState::Idle,
        since: 17,
        conversation: "c1".to_owned(),
        parent: None,
    }
}

#[tokio::test]
async fn hello_requests_events_and_slots() {
    let dir = TempDir::new("conversation");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;

        // A request with a real result.
        let request = peer.recv_request().await;
        assert!(matches!(
            Request::from_rpc(&request).expect("a known method"),
            Request::LoopList(_)
        ));
        let result = LoopListResult {
            loops: vec![a_loop()],
            conversations: Vec::new(),
        };
        peer.send(&Envelope::Response(RpcResponse::ok(
            request.id,
            serde_json::to_value(result).expect("a serialisable result"),
        )))
        .await;

        // A request with an empty result.
        let request = peer.recv_request().await;
        match Request::from_rpc(&request).expect("a known method") {
            Request::LoopPrompt(params) => {
                assert_eq!(params.loop_id, "l1");
                assert_eq!(params.text, "hi");
                assert_eq!(params.when, PromptWhen::AfterTurn);
            }
            other => panic!("expected loop.prompt, got {other:?}"),
        }
        peer.send(&Envelope::Response(RpcResponse::ok(
            request.id,
            json!({}),
        )))
        .await;

        // An event nobody asked a question about.
        let event = Event::LoopStatus(LoopStatusEvent {
            loop_id: "l1".to_owned(),
            seq: 4,
            state: LoopState::Working,
            since: 99,
            detail: None,
        });
        peer.send(&Envelope::Notification(event.into_rpc())).await;

        // A slot request that expects a reply.
        let slot = SlotRequest::Tool {
            name: "echo".to_owned(),
            payload: ToolCallPayload {
                args: json!({ "text": "ping" }),
                id: "call-1".to_owned(),
            },
        };
        peer.send(&slot.clone().into_rpc(7)).await;
        let reply = match peer.recv().await.expect("a reply to the slot request") {
            Envelope::Response(response) => {
                assert_eq!(response.id, Id::Number(7));
                slot.parse_reply(response.result.expect("a result"))
                    .expect("the slot's own reply type")
            }
            other => panic!("expected a response, got {other:?}"),
        };
        assert_eq!(
            reply,
            SlotReply::Tool(ToolReply::Ok {
                content: ToolContent::Text("pong".to_owned()),
                details: None,
            })
        );

        // A slot request that does not: `on.<event>` is fire and forget.
        let started = SlotRequest::On(pirs_protocol::OnPayload::Start(
            pirs_protocol::OnStartPayload {
                loop_id: "l1".to_owned(),
                cwd: "/srv/project".into(),
            },
        ));
        peer.send(&started.into_rpc(0)).await;

        // Something this client version does not know: logged and dropped.
        peer.send(&Envelope::notification("future.event", json!({ "x": 1 })))
            .await;

        while peer.recv().await.is_some() {}
    });

    let client = Client::connect(options(&socket)).await.expect("a client");
    assert_eq!(client.hello().server, "fake-pirs 0.1.0");
    assert!(client.is_connected());

    let list = client
        .loop_list(LoopListParams::default())
        .await
        .expect("loop.list");
    assert_eq!(list.loops, vec![a_loop()]);

    assert_eq!(
        client
            .loop_prompt("l1", "hi", PromptWhen::AfterTurn)
            .await
            .expect("loop.prompt"),
        Empty {}
    );

    let mut events = client.events().expect("the first caller takes the stream");
    assert!(client.events().is_none(), "the stream is taken only once");
    match events.recv().await.expect("an event") {
        Event::LoopStatus(status) => {
            assert_eq!(status.seq, 4);
            assert_eq!(status.state, LoopState::Working);
        }
        other => panic!("expected loop.status, got {other:?}"),
    }

    let mut slots = client.slot_requests().expect("the slot stream");
    let (id, request) = slots.recv().await.expect("a slot request");
    assert_eq!(id, Some(Id::Number(7)));
    match &request {
        SlotRequest::Tool { name, payload } => {
            assert_eq!(name, "echo");
            assert_eq!(payload.args, json!({ "text": "ping" }));
        }
        other => panic!("expected tool.echo, got {other:?}"),
    }
    client
        .reply_slot(
            id.expect("a slot that expects a reply"),
            SlotReply::Tool(ToolReply::Ok {
                content: ToolContent::Text("pong".to_owned()),
                details: None,
            }),
        )
        .await
        .expect("the reply reaches the server");

    let (id, request) = slots.recv().await.expect("the on.start notification");
    assert_eq!(id, None, "on.<event> carries no id and takes no reply");
    assert!(matches!(request, SlotRequest::On(_)));

    drop(client);
    server.await.expect("the fake server finishes cleanly");
}

#[tokio::test]
async fn subscribe_with_replay_passes_since() {
    let dir = TempDir::new("replay");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        let request = peer.recv_request().await;
        match Request::from_rpc(&request).expect("a known method") {
            Request::Subscribe(params) => {
                assert_eq!(params.loop_id, LoopSelector::Loop("l1".to_owned()));
                assert_eq!(params.since, Some(4));
                assert_eq!(params.events, Some(vec!["loop.message".to_owned()]));
            }
            other => panic!("expected subscribe, got {other:?}"),
        }
        peer.send(&Envelope::Response(RpcResponse::ok(request.id, json!({}))))
            .await;
        while peer.recv().await.is_some() {}
    });

    let client = Client::connect(options(&socket)).await.expect("a client");
    client
        .subscribe_with_replay(
            LoopSelector::Loop("l1".to_owned()),
            Some(vec!["loop.message".to_owned()]),
            Some(4),
        )
        .await
        .expect("subscribe");
    drop(client);
    server.await.expect("the fake server finishes cleanly");
}

#[tokio::test]
async fn an_rpc_error_keeps_its_code() {
    let dir = TempDir::new("rpcerror");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        let request = peer.recv_request().await;
        peer.send(&Envelope::Response(RpcResponse::err(
            request.id,
            RpcError::new(code::UNKNOWN_LOOP, "no such loop"),
        )))
        .await;
        while peer.recv().await.is_some() {}
    });

    let client = Client::connect(options(&socket)).await.expect("a client");
    match client.loop_attach("gone").await {
        Err(ClientError::Rpc { method, source }) => {
            assert_eq!(method, "loop.attach");
            assert_eq!(source.code, code::UNKNOWN_LOOP);
        }
        other => panic!("expected an rpc error, got {other:?}"),
    }
    assert!(client.is_connected(), "an error reply is not a disconnect");
    drop(client);
    server.await.expect("the fake server finishes cleanly");
}

#[tokio::test]
async fn a_version_refusal_is_typed() {
    let dir = TempDir::new("refused");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        let request = peer.recv_request().await;
        assert_eq!(request.method, "hello");
        peer.send(&Envelope::Response(RpcResponse::err(
            request.id,
            RpcError::new(code::VERSION_REFUSED, "protocol major 9 != 0")
                .with_data(json!({ "server": "9.2" })),
        )))
        .await;
        // The server closes the connection after a refusal.
    });

    match Client::connect(options(&socket)).await {
        Err(ClientError::VersionRefused {
            server,
            protocol_version,
        }) => {
            assert_eq!(server, "9.2");
            assert_eq!(protocol_version, PROTOCOL_VERSION);
        }
        other => panic!("expected a version refusal, got {other:?}"),
    }
    server.await.expect("the fake server finishes cleanly");
}

#[tokio::test]
async fn a_closed_connection_ends_both_streams() {
    let dir = TempDir::new("disconnect");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        // Drop the connection without answering anything else.
    });

    let client = Client::connect(options(&socket)).await.expect("a client");
    let mut events = client.events().expect("the event stream");
    let mut slots = client.slot_requests().expect("the slot stream");
    server.await.expect("the fake server finishes cleanly");

    assert!(events.recv().await.is_none(), "events end with the socket");
    assert!(slots.recv().await.is_none(), "slots end with the socket");
    assert!(!client.is_connected());
    match client.loop_list(LoopListParams::default()).await {
        Err(ClientError::Disconnected) => {}
        other => panic!("expected Disconnected, got {other:?}"),
    }
}

#[tokio::test]
async fn a_dropped_event_stream_does_not_close_the_connection() {
    let dir = TempDir::new("dropped");
    let socket = dir.socket();
    let listener = UnixListener::bind(&socket).expect("a bindable socket");

    let server = tokio::spawn(async move {
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        // An event with nobody to hand it to, then an ordinary request.
        let event = Event::LoopStatus(LoopStatusEvent {
            loop_id: "l1".to_owned(),
            seq: 4,
            state: LoopState::Working,
            since: 99,
            detail: None,
        });
        peer.send(&Envelope::Notification(event.into_rpc())).await;
        let request = peer.recv_request().await;
        assert_eq!(request.method, "loop.list");
        let result = LoopListResult {
            loops: vec![a_loop()],
            conversations: Vec::new(),
        };
        peer.send(&Envelope::Response(RpcResponse::ok(
            request.id,
            serde_json::to_value(result).expect("a serialisable result"),
        )))
        .await;
        while peer.recv().await.is_some() {}
    });

    let client = Client::connect(options(&socket)).await.expect("a client");
    drop(client.events().expect("the event stream"));
    let list = client
        .loop_list(LoopListParams::default())
        .await
        .expect("the connection survives the dropped stream");
    assert_eq!(list.loops, vec![a_loop()]);
    assert!(client.is_connected());
    drop(client);
    server.await.expect("the fake server finishes cleanly");
}

#[tokio::test]
async fn auto_start_waits_for_the_socket() {
    let dir = TempDir::new("autostart");
    let socket = dir.socket();
    let late = socket.clone();

    let server = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let listener = UnixListener::bind(&late).expect("a bindable socket");
        let mut peer = Peer::accept(&listener).await;
        answer_hello(&mut peer).await;
        while peer.recv().await.is_some() {}
    });

    let mut options = options(&socket);
    options.auto_start = true;
    // A "server" that does nothing: this test is about the wait, and the
    // socket is created by the task above.
    options.server_command = Some(vec!["true".to_owned()]);
    options.start_timeout = Duration::from_secs(10);

    let client = Client::connect(options).await.expect("a client");
    assert_eq!(client.hello().protocol_version, PROTOCOL_VERSION);
    drop(client);
    server.await.expect("the fake server finishes cleanly");
}

#[tokio::test]
async fn a_socket_that_never_appears_times_out() {
    let dir = TempDir::new("timeout");
    let mut options = options(&dir.socket());
    options.auto_start = true;
    options.server_command = Some(vec!["true".to_owned()]);
    options.start_timeout = Duration::from_millis(120);

    match Client::connect(options).await {
        Err(ClientError::StartTimeout { waited, .. }) => {
            assert_eq!(waited, Duration::from_millis(120));
        }
        other => panic!("expected a start timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn without_auto_start_a_missing_socket_fails_at_once() {
    let dir = TempDir::new("noserver");
    match Client::connect(options(&dir.socket())).await {
        Err(ClientError::NoServer { socket, .. }) => assert_eq!(socket, dir.socket()),
        other => panic!("expected NoServer, got {other:?}"),
    }
}
