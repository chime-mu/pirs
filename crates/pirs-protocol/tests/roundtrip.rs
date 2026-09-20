//! Every request, event and slot request through `Frame`, and the message
//! type against the session format's JSON.

use pirs_protocol::*;
use serde_json::{json, Value};

fn roundtrip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let line = Frame::encode(value).unwrap();
    assert!(
        line.ends_with('\n') && line.matches('\n').count() == 1,
        "one line: {line:?}"
    );
    let back: T = Frame::decode(&line).unwrap();
    assert_eq!(&back, value);
    back
}

fn model() -> ModelSpec {
    ModelSpec {
        model: "faux/scripted".into(),
        thinking: Some(ThinkingLevel::High),
    }
}

fn assistant() -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![
            Content::Thinking {
                thinking: "hmm".into(),
                thinking_signature: None,
                redacted: false,
            },
            Content::text("Hi!"),
            Content::ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: json!({"command": "ls"}),
                thought_signature: None,
            },
        ],
        api: "faux".into(),
        provider: "faux".into(),
        model: "scripted".into(),
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        raw_stop_reason: None,
        timestamp: 1,
    })
}

fn tool_result() -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: "call_1".into(),
        tool_name: "bash".into(),
        content: vec![Content::text("a\nb\n")],
        details: Some(json!({"exit": 0})),
        usage: None,
        is_error: false,
        timestamp: 2,
    })
}

fn all_requests() -> Vec<Request> {
    let id = || "L1".to_string();
    vec![
        Request::Hello(HelloParams {
            client: "test".into(),
            protocol_version: PROTOCOL_VERSION.into(),
        }),
        Request::LoopCreate(LoopCreateParams {
            cwd: "/work".into(),
            model: Some(model()),
            name: Some("review".into()),
            session: Some("conv-1".into()),
        }),
        Request::LoopCreate(LoopCreateParams {
            cwd: "/work".into(),
            model: None,
            name: None,
            session: None,
        }),
        Request::LoopList(LoopListParams {
            cwd: Some("/work".into()),
        }),
        Request::LoopList(LoopListParams::default()),
        Request::LoopAttach(LoopAttachParams { loop_id: id() }),
        Request::LoopClose(LoopCloseParams { loop_id: id() }),
        Request::LoopPrompt(LoopPromptParams {
            loop_id: id(),
            text: "line 1\nline 2".into(),
            when: PromptWhen::AfterTurn,
        }),
        Request::LoopAbort(LoopAbortParams { loop_id: id() }),
        Request::LoopWait(LoopWaitParams { loop_id: id() }),
        Request::Subscribe(SubscribeParams {
            loop_id: LoopSelector::All,
            events: Some(vec!["loop.turn_end".into()]),
            since: Some(41),
        }),
        Request::Subscribe(SubscribeParams {
            loop_id: LoopSelector::Loop(id()),
            events: None,
            since: None,
        }),
        Request::Unsubscribe(UnsubscribeParams {
            loop_id: LoopSelector::Loop(id()),
        }),
        Request::Register(RegisterParams {
            loop_id: id(),
            slot: Slot::Tool("fetch".into()),
            timeout: 60_000,
        }),
        Request::Register(RegisterParams {
            loop_id: id(),
            slot: Slot::On(OnEvent::TurnEnd),
            timeout: 5_000,
        }),
        Request::Unregister(UnregisterParams {
            loop_id: id(),
            slot: Slot::Input,
        }),
        Request::UiStatus(UiStatusParams {
            loop_id: id(),
            key: "git".into(),
            text: "main +2".into(),
        }),
        Request::UiWidget(UiWidgetParams {
            loop_id: id(),
            key: "todo".into(),
            lines: vec!["[ ] a".into()],
        }),
        Request::UiNotify(UiNotifyParams {
            loop_id: id(),
            level: NotifyLevel::Warning,
            text: "slow".into(),
        }),
        Request::FsList(FsListParams {
            loop_id: id(),
            path: "src".into(),
        }),
        Request::FsRead(FsReadParams {
            loop_id: id(),
            path: "/work/src/main.rs".into(),
        }),
        Request::LoopTools(LoopToolsParams {
            loop_id: id(),
            names: vec!["read".into(), "bash".into()],
        }),
        Request::LoopModel(LoopModelParams {
            loop_id: id(),
            spec: model(),
        }),
        Request::LoopReload(LoopReloadParams { loop_id: id() }),
        Request::DslCheck(DslCheckParams {
            cwd: "/work".into(),
        }),
    ]
}

fn all_responses() -> Vec<(Request, Response)> {
    let info = LoopInfo {
        id: "L1".into(),
        name: None,
        cwd: "/work".into(),
        model: model(),
        state: LoopState::Idle,
        since: 5,
        conversation: "conv-1".into(),
    };
    let manifest = Manifest {
        tools: vec![ToolInfo {
            name: "bash".into(),
            description: "run".into(),
            parameters: json!({"type": "object"}),
        }],
        commands: vec![CommandInfo {
            name: "todo".into(),
            description: "list".into(),
        }],
        status_keys: vec!["git".into()],
        widget_keys: vec!["todo".into()],
    };
    let r = |req: &Request| req.clone();
    let reqs = all_requests();
    vec![
        (
            r(&reqs[0]),
            Response::Hello(HelloResult {
                server: "pirs 0.1".into(),
                protocol_version: "0.1".into(),
            }),
        ),
        (r(&reqs[1]), Response::LoopCreate(info.clone())),
        (
            r(&reqs[3]),
            Response::LoopList(LoopListResult {
                loops: vec![info.clone()],
                conversations: vec![ConversationInfo {
                    id: "conv-1".into(),
                    name: Some("review".into()),
                    cwd: "/work".into(),
                    path: "/home/u/.pirs/sessions/x/conv-1.jsonl".into(),
                    updated: 9,
                }],
            }),
        ),
        (
            r(&reqs[5]),
            Response::LoopAttach(LoopAttachResult {
                info,
                manifest: manifest.clone(),
                seq: 42,
            }),
        ),
        (r(&reqs[6]), Response::LoopClose(Empty {})),
        (
            r(&reqs[9]),
            Response::LoopWait(LoopWaitResult {
                state: LoopState::Idle,
            }),
        ),
        (
            r(&reqs[19]),
            Response::FsList(FsListResult {
                entries: vec![
                    FsEntry {
                        path: ServerPath::from("/home/me/proj/src/main.rs"),
                        name: "main.rs".into(),
                        kind: FsEntryKind::File,
                        bytes: Some(10),
                    },
                    FsEntry {
                        path: ServerPath::from("/home/me/proj/src/sub"),
                        name: "sub".into(),
                        kind: FsEntryKind::Dir,
                        bytes: None,
                    },
                ],
            }),
        ),
        (
            r(&reqs[20]),
            Response::FsRead(FsReadResult::Content {
                content: "fn main() {}\n".into(),
            }),
        ),
        (
            r(&reqs[20]),
            Response::FsRead(FsReadResult::Ref(Ref {
                path: "/sess/blob-1".into(),
                bytes: 70_000,
            })),
        ),
        (
            r(&reqs[23]),
            Response::LoopReload(LoopReloadResult {
                files: vec!["/work/.pirs/ext/a.pirs.toml".into()],
            }),
        ),
        (
            r(&reqs[24]),
            Response::DslCheck(DslCheckResult {
                files: vec![
                    "/work/.pirs/ext/a.pirs.toml".into(),
                    "/work/.pirs/ext/b.pirs.toml".into(),
                ],
                manifest,
                conflicts: vec![DslConflict {
                    message: "status key `git` defined twice".into(),
                    files: vec![
                        "/work/.pirs/ext/a.pirs.toml".into(),
                        "/work/.pirs/ext/b.pirs.toml".into(),
                    ],
                }],
                system_prompt: "You are…".into(),
            }),
        ),
    ]
}

fn all_events() -> Vec<Event> {
    let id = || "L1".to_string();
    vec![
        Event::LoopStatus(LoopStatusEvent {
            loop_id: id(),
            seq: 1,
            state: LoopState::Working,
            since: 5,
            detail: None,
        }),
        Event::LoopStatus(LoopStatusEvent {
            loop_id: id(),
            seq: 2,
            state: LoopState::Idle,
            since: 6,
            detail: Some("aborted".into()),
        }),
        Event::LoopMessage(LoopMessageEvent {
            loop_id: id(),
            seq: None,
            role: Role::Assistant,
            body: LoopMessageBody::Delta {
                delta: Delta::Text {
                    index: 1,
                    text: "Hi".into(),
                },
            },
        }),
        Event::LoopMessage(LoopMessageEvent {
            loop_id: id(),
            seq: None,
            role: Role::Assistant,
            body: LoopMessageBody::Delta {
                delta: Delta::Thinking {
                    index: 0,
                    thinking: "hm".into(),
                },
            },
        }),
        Event::LoopMessage(LoopMessageEvent {
            loop_id: id(),
            seq: Some(3),
            role: Role::Assistant,
            body: LoopMessageBody::Message {
                message: Box::new(assistant()),
            },
        }),
        Event::LoopMessage(LoopMessageEvent {
            loop_id: id(),
            seq: Some(4),
            role: Role::ToolResult,
            body: LoopMessageBody::Message {
                message: Box::new(tool_result()),
            },
        }),
        Event::LoopTurnEnd(LoopTurnEndEvent {
            loop_id: id(),
            seq: 5,
            messages: vec![assistant(), tool_result()],
        }),
        Event::LoopRunEnd(LoopRunEndEvent {
            loop_id: id(),
            seq: 6,
            messages: vec![assistant()],
        }),
        Event::UiStatus(UiStatusEvent {
            loop_id: id(),
            seq: 7,
            key: "git".into(),
            text: "main".into(),
        }),
        Event::UiWidget(UiWidgetEvent {
            loop_id: id(),
            seq: 8,
            key: "todo".into(),
            lines: vec![],
        }),
        Event::UiNotify(UiNotifyEvent {
            loop_id: id(),
            seq: 9,
            level: NotifyLevel::Error,
            text: "boom".into(),
        }),
        Event::FsChanged(FsChangedEvent {
            loop_id: id(),
            seq: 10,
            path: "/work/a.rs".into(),
            by: ChangedBy::Tool,
        }),
    ]
}

fn all_slot_requests() -> Vec<(SlotRequest, SlotReply)> {
    let id = || "L1".to_string();
    let ok = ToolReply::Ok {
        content: "out".into(),
        details: Some(json!({"exit": 0})),
    };
    vec![
        (
            SlotRequest::Input(InputPayload {
                text: "/todo".into(),
            }),
            SlotReply::Input(InputReply::Handled { handled: True }),
        ),
        (
            SlotRequest::Input(InputPayload { text: "hi".into() }),
            SlotReply::Input(InputReply::Text { text: "hi!".into() }),
        ),
        (
            SlotRequest::Prompt(PromptPayload {
                system_prompt: "You are…".into(),
            }),
            SlotReply::Prompt(PromptReply::Append {
                append: "Be brief.".into(),
            }),
        ),
        (
            SlotRequest::Prompt(PromptPayload {
                system_prompt: "You are…".into(),
            }),
            SlotReply::Prompt(PromptReply::Replace {
                replace: "Nope.".into(),
            }),
        ),
        (
            SlotRequest::ToolResult(ToolResultPayload {
                tool: "bash".into(),
                args: json!({"command": "ls"}),
                result: ok.clone(),
            }),
            SlotReply::ToolResult(ToolResultReply {
                result: ToolReply::Ok {
                    content: ToolContent::Blocks(vec![Content::text("filtered")]),
                    details: None,
                },
            }),
        ),
        (
            SlotRequest::Tool {
                name: "fetch".into(),
                payload: ToolCallPayload {
                    args: json!({"url": "file:///x"}),
                    id: "call_1".into(),
                },
            },
            SlotReply::Tool(ok.clone()),
        ),
        (
            SlotRequest::Tool {
                name: "fetch".into(),
                payload: ToolCallPayload {
                    args: json!({}),
                    id: "call_2".into(),
                },
            },
            SlotReply::Tool(ToolReply::Error {
                error: "no url".into(),
            }),
        ),
        (
            SlotRequest::On(OnPayload::Start(OnStartPayload {
                loop_id: id(),
                cwd: "/work".into(),
            })),
            SlotReply::None,
        ),
        (
            SlotRequest::On(OnPayload::TurnEnd(LoopTurnEndEvent {
                loop_id: id(),
                seq: 5,
                messages: vec![assistant()],
            })),
            SlotReply::None,
        ),
        (
            SlotRequest::On(OnPayload::RunEnd(LoopRunEndEvent {
                loop_id: id(),
                seq: 6,
                messages: vec![],
            })),
            SlotReply::None,
        ),
        (
            SlotRequest::On(OnPayload::ToolResult(OnToolResultPayload {
                loop_id: id(),
                tool: "bash".into(),
                args: json!({}),
                result: ok,
            })),
            SlotReply::None,
        ),
        (
            SlotRequest::On(OnPayload::Reload(OnReloadPayload {
                loop_id: id(),
                files: vec!["/work/.pirs/ext/a.pirs.toml".into()],
            })),
            SlotReply::None,
        ),
    ]
}

#[test]
fn every_request_roundtrips_through_frame_and_envelope() {
    let mut seen = std::collections::BTreeSet::new();
    for (i, req) in all_requests().into_iter().enumerate() {
        roundtrip(&req);
        seen.insert(req.method());
        let rpc = req.clone().into_rpc(i as i64);
        assert_eq!(rpc.method, req.method());
        assert!(
            rpc.params.is_object(),
            "params of {} is an object",
            req.method()
        );
        let env = roundtrip(&Envelope::Request(rpc.clone()));
        assert_eq!(env.method(), Some(req.method()));
        assert_eq!(Request::from_rpc(&rpc).unwrap(), req);
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), {
        let mut m = Request::METHODS.to_vec();
        m.sort();
        m
    });
}

#[test]
fn every_response_roundtrips() {
    for (req, resp) in all_responses() {
        let value = resp.to_value();
        let env = roundtrip(&Envelope::Response(RpcResponse::ok(
            Id::from(1),
            value.clone(),
        )));
        let Envelope::Response(back) = env else {
            panic!("not a response")
        };
        let result = back.into_result().unwrap();
        assert_eq!(result, value);
        assert_eq!(req.parse_response(result).unwrap(), resp);
    }
    let err = RpcResponse::err(
        Id::from("x"),
        RpcError::new(code::VERSION_REFUSED, "major 1 != 0").with_data(json!({"server": "1.0"})),
    );
    let Envelope::Response(back) = roundtrip(&Envelope::Response(err)) else {
        panic!()
    };
    let e = back.into_result().unwrap_err();
    assert_eq!(e.code, code::VERSION_REFUSED);
    assert_eq!(e.data, Some(json!({"server": "1.0"})));
}

#[test]
fn every_event_roundtrips_through_frame_and_envelope() {
    let mut seen = std::collections::BTreeSet::new();
    for ev in all_events() {
        roundtrip(&ev);
        seen.insert(ev.method());
        assert_eq!(ev.loop_id(), "L1");
        let rpc = ev.clone().into_rpc();
        let env = roundtrip(&Envelope::Notification(rpc.clone()));
        assert!(env.id().is_none());
        assert_eq!(Event::from_rpc(&rpc).unwrap(), ev);
    }
    assert_eq!(seen.len(), Event::METHODS.len());
}

#[test]
fn deltas_carry_no_seq_and_messages_do() {
    let events = all_events();
    let delta = &events[2];
    let msg = &events[4];
    assert_eq!(delta.seq(), None);
    assert_eq!(msg.seq(), Some(3));
    let line = Frame::encode(delta).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert!(v["params"].get("seq").is_none());
    assert_eq!(v["params"]["delta"]["type"], "text");
    assert_eq!(v["params"]["delta"]["index"], 1);
    let line = Frame::encode(msg).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["params"]["seq"], 3);
    assert_eq!(v["params"]["role"], "assistant");
    assert_eq!(v["params"]["message"]["role"], "assistant");
}

#[test]
fn every_slot_request_roundtrips_and_matches_stdin_shape() {
    let mut seen = std::collections::BTreeSet::new();
    for (req, reply) in all_slot_requests() {
        roundtrip(&req);
        seen.insert(req.slot().to_string());
        let rpc = req.clone().into_rpc("h1");
        assert_eq!(
            rpc.method().map(str::to_owned),
            Some(req.slot().to_string())
        );
        // `on.<event>` is fire and forget, so it travels as a notification.
        assert_eq!(rpc.id().is_some(), req.expects_reply());
        // The socket binding and the stdin binding carry the same payload.
        let params = match &rpc {
            Envelope::Request(r) => r.params.clone(),
            Envelope::Notification(n) => n.params.clone(),
            Envelope::Response(_) => panic!("a slot request is never a response"),
        };
        assert_eq!(params, req.params());
        let env = roundtrip(&rpc);
        assert_eq!(
            env.method().map(str::to_owned),
            Some(req.slot().to_string())
        );
        assert_eq!(SlotRequest::from_rpc(&env).unwrap(), req);
        assert_eq!(req.expects_reply(), reply != SlotReply::None);
        match reply.to_value() {
            Some(v) => {
                let line = Frame::encode(&v).unwrap();
                let back: Value = Frame::decode(&line).unwrap();
                assert_eq!(req.parse_reply(back).unwrap(), reply);
            }
            None => assert_eq!(req.parse_reply(Value::Null).unwrap(), SlotReply::None),
        }
    }
    let expected: std::collections::BTreeSet<String> =
        ["input", "prompt", "tool_result", "tool.fetch"]
            .into_iter()
            .map(String::from)
            .chain(OnEvent::NAMES.iter().map(|e| format!("on.{e}")))
            .collect();
    assert_eq!(seen, expected);
}

#[test]
fn slot_requests_with_unknown_methods_are_rejected() {
    let rpc = RpcRequest {
        jsonrpc: JsonRpcVersion,
        id: 1.into(),
        method: "guard".into(),
        params: json!({}),
    };
    assert!(SlotRequest::from_rpc(&Envelope::Request(rpc.clone())).is_err());
    assert!(Request::from_rpc(&rpc).is_err());
}

#[test]
fn envelope_kinds_are_told_apart() {
    let req: Envelope =
        Frame::decode(r#"{"jsonrpc":"2.0","id":1,"method":"loop.wait","params":{"loop":"L1"}}"#)
            .unwrap();
    assert!(matches!(req, Envelope::Request(_)));
    let note: Envelope =
        Frame::decode(r#"{"jsonrpc":"2.0","method":"loop.status","params":{}}"#).unwrap();
    assert!(matches!(note, Envelope::Notification(_)));
    let resp: Envelope = Frame::decode(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#).unwrap();
    assert!(matches!(resp, Envelope::Response(_)));
    let err: Envelope =
        Frame::decode(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"no"}}"#)
            .unwrap();
    let Envelope::Response(r) = err else { panic!() };
    assert_eq!(r.into_result().unwrap_err().code, code::METHOD_NOT_FOUND);
    assert!(
        Frame::decode::<Envelope>(r#"{"jsonrpc":"1.0","id":1,"method":"hello","params":{}}"#)
            .is_err()
    );
    assert!(Frame::decode::<Envelope>(r#"{"id":1,"method":"hello","params":{}}"#).is_err());
}

#[test]
fn message_json_matches_the_session_format() {
    // Lines lifted from docs/session-format.md.
    let samples = [
        r#"{"role":"user","content":"Hello","timestamp":1733234401000}"#,
        r#"{"role":"assistant","content":[{"type":"text","text":"Hi!"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"stop","timestamp":1733234402000}"#,
        r#"{"role":"toolResult","toolCallId":"call_123","toolName":"bash","content":[{"type":"text","text":"output"}],"isError":false,"timestamp":1733234403000}"#,
        r#"{"role":"system","content":"","sections":{"cwd":"/project","preamble":"You are…","tools":null},"toolsAdded":[{"name":"read","description":"...","parameters":{}}],"timestamp":1733234400000}"#,
        r#"{"role":"assistant","content":[{"type":"thinking","thinking":"","thinkingSignature":"sig","redacted":true},{"type":"toolCall","id":"c","name":"edit","arguments":{"path":"a"},"thoughtSignature":"ts"}],"api":"faux","provider":"faux","model":"scripted","responseId":"r1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"reasoning":5,"totalTokens":0,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},"stopReason":"toolUse","errorMessage":"e","rawStopReason":"tool_use","timestamp":7}"#,
        r#"{"role":"user","content":[{"type":"text","text":"see"},{"type":"image","data":"AA==","mimeType":"image/png"}],"timestamp":3}"#,
    ];
    for s in samples {
        let m: Message = serde_json::from_str(s).unwrap();
        let back = serde_json::to_string(&m).unwrap();
        let (a, b): (Value, Value) = (
            serde_json::from_str(s).unwrap(),
            serde_json::from_str(&back).unwrap(),
        );
        assert_eq!(a, b, "message JSON drifted for {s}");
        roundtrip(&m);
    }
}

#[test]
fn server_path_is_a_plain_string_on_the_wire() {
    let p = ServerPath::from("/srv/a b");
    assert_eq!(serde_json::to_string(&p).unwrap(), "\"/srv/a b\"");
    assert_eq!(
        serde_json::to_string(&Ref {
            path: p.clone(),
            bytes: 3
        })
        .unwrap(),
        r#"{"ref":"/srv/a b","bytes":3}"#
    );
    assert_eq!(p.to_string(), "/srv/a b");
}

#[test]
fn loop_selector_star() {
    assert_eq!(serde_json::to_string(&LoopSelector::All).unwrap(), "\"*\"");
    assert_eq!(
        serde_json::from_str::<LoopSelector>("\"*\"").unwrap(),
        LoopSelector::All
    );
    assert_eq!(
        serde_json::from_str::<LoopSelector>("\"L9\"").unwrap(),
        LoopSelector::Loop("L9".into())
    );
}

#[test]
fn versions() {
    assert!(compatible("0.1", "0.9"));
    assert!(!compatible("0.1", "1.0"));
    assert_eq!(PROTOCOL_VERSION, "0.1");
}

#[test]
fn slot_forms() {
    assert_eq!(
        "tool.fetch".parse::<Slot>().unwrap().to_string(),
        "tool.fetch"
    );
    assert_eq!(
        "on.turn_end".parse::<Slot>().unwrap(),
        Slot::On(OnEvent::TurnEnd)
    );
    assert!("guard".parse::<Slot>().is_err());
}
