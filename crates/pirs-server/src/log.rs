//! The session log as the sequenced event stream (D-06).
//!
//! Every sequenced event a loop emits is one entry of its session log, and the
//! event's `seq` is that entry's `seq`. Live emission and `subscribe { since }`
//! replay go through the same conversion ([`LoopLog::entry_to_event`]), so a
//! replay is exactly the live sequence minus streaming deltas, which carry no
//! `seq`, are not logged and are only sent live.
//!
//! How each event is encoded in the log:
//!
//! | event            | entry `type` | `customType`               | `data` / payload                                 |
//! |------------------|--------------|----------------------------|--------------------------------------------------|
//! | `loop.message`   | `message`    | —                          | the message itself (`message`)                   |
//! | `loop.status`    | `custom`     | `pirs.status`              | `{ state, since, detail? }`                      |
//! | `loop.turn_end`  | `custom`     | `pirs.turn_end`            | `{ messages: [seq, …] }` of the turn's messages  |
//! | `loop.run_end`   | `custom`     | `pirs.run_end`             | `{ messages: [seq, …] }` of the run's messages   |
//! | `ui.status`      | `custom`     | `pirs.ui.status`           | `{ key, text }`                                  |
//! | `ui.widget`      | `custom`     | `pirs.ui.widget`           | `{ key, lines }`                                 |
//! | `ui.notify`      | `custom`     | `pirs.ui.notify`           | `{ level, text }`                                |
//! | `fs.changed`     | `custom`     | `pirs.fs.changed`          | `{ path, by }`                                   |
//!
//! `turn_end` and `run_end` hold the seqs of the messages they cover, not
//! copies; the wire event carries the messages looked up by those seqs. Entries
//! with no event (`model_change`, `thinking_level_change`, `session_info`,
//! `pirs.tool_result_rewrite`, …) still consume a `seq`, so a replay may skip
//! numbers; `seq` is an index, not a count. Message entries whose role has no
//! wire form (see `convert`) are skipped the same way.
//!
//! **By reference (D-11, D-39).** A tool result whose text block, or whose
//! base64 image data, exceeds [`REF_THRESHOLD`] bytes is stored in full in
//! the log (the model needs it), but the *event* replaces that block's `text`
//! or `data` with `"[by reference]"` and adds to the message's `details`:
//!
//! ```json
//! { "ref": { "ref": "<session dir>/refs/<conversation>-<seq>-<block>", "bytes": 70000 },
//!   "refs": [ { "index": 0, "ref": "…", "bytes": 70000 } ] }
//! ```
//!
//! `details.ref` is the [`Ref`] of the first oversized block (the common
//! one-block case); `details.refs` lists every oversized block with its
//! content index. An image is written *decoded*, so the ref file is the image
//! itself and `bytes` counts the image's bytes rather than its base64 length;
//! its `mimeType` rides along in its `details.refs` entry (`details` is free
//! form, so nothing on the wire has to grow a field for it). The file is
//! written when the event is first produced and reused afterwards;
//! `fs.read { path: ref }` serves it in full. Nothing else travels by
//! reference this phase.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use base64::Engine as _;
use pi_agent::AgentMessage;
use pirs_protocol::{
    ChangedBy, Content, Delta, Envelope, Event, Frame, FsChangedEvent, LoopMessageBody, LoopMessageEvent,
    LoopRunEndEvent, LoopState, LoopStatusEvent, LoopTurnEndEvent, Message, NotifyLevel, Ref, Role, ServerPath,
    UiNotifyEvent, UiStatusEvent, UiWidgetEvent,
};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::session::{EntryKind, SessionEntry, SessionManager};
use crate::REF_THRESHOLD;

pub(crate) const CT_STATUS: &str = "pirs.status";
pub(crate) const CT_TURN_END: &str = "pirs.turn_end";
pub(crate) const CT_RUN_END: &str = "pirs.run_end";
pub(crate) const CT_UI_STATUS: &str = "pirs.ui.status";
pub(crate) const CT_UI_WIDGET: &str = "pirs.ui.widget";
pub(crate) const CT_UI_NOTIFY: &str = "pirs.ui.notify";
pub(crate) const CT_FS_CHANGED: &str = "pirs.fs.changed";
/// The original of a rewritten tool result, next to the message entry (D-21).
pub(crate) const CT_TOOL_RESULT_REWRITE: &str = "pirs.tool_result_rewrite";

/// The text that replaces an oversized block in an event.
pub(crate) const BY_REFERENCE: &str = "[by reference]";

/// An observer: a connection's writer channel plus its event filter.
pub(crate) struct Subscriber {
    /// The connection that subscribed.
    pub(crate) conn: u64,
    /// Encoded lines go here; the connection's writer task drains it.
    pub(crate) tx: UnboundedSender<String>,
    /// Event names to receive; `None` means all.
    pub(crate) events: Option<HashSet<String>>,
}

impl Subscriber {
    fn wants(&self, method: &str) -> bool {
        self.events.as_ref().is_none_or(|set| set.contains(method))
    }
}

/// The server-wide list of `subscribe { loop: "*" }` observers, shared by
/// every loop. They receive every loop's events, including loops created
/// after they subscribed; `since` is not allowed for them.
pub(crate) type StarSubscribers = Arc<Mutex<Vec<Subscriber>>>;

/// A loop's session log plus its observers, under one lock so that
/// "replay, then register" and "append, then fan out" never interleave.
pub(crate) struct LoopLog {
    loop_id: String,
    session: SessionManager,
    subscribers: Vec<Subscriber>,
    star: StarSubscribers,
}

impl LoopLog {
    pub(crate) fn new(loop_id: String, session: SessionManager, star: StarSubscribers) -> Self {
        LoopLog { loop_id, session, subscribers: Vec::new(), star }
    }

    #[cfg(test)]
    pub(crate) fn session(&self) -> &SessionManager {
        &self.session
    }

    pub(crate) fn session_mut(&mut self) -> &mut SessionManager {
        &mut self.session
    }

    /// The `seq` of the newest entry (0 for an empty log).
    pub(crate) fn latest_seq(&self) -> u64 {
        self.session.next_seq().saturating_sub(1)
    }

    /// Append a message entry and emit its `loop.message` event.
    pub(crate) fn append_message(&mut self, message: AgentMessage) -> Result<u64> {
        self.session.append_message(message)?;
        let seq = self.latest_seq();
        self.emit_seq(seq);
        Ok(seq)
    }

    /// Append a `custom` entry and emit the event it encodes, if any.
    pub(crate) fn append_custom(&mut self, custom_type: &str, data: Value) -> Result<u64> {
        self.session.append_custom_entry(custom_type, Some(data))?;
        let seq = self.latest_seq();
        self.emit_seq(seq);
        Ok(seq)
    }

    /// Send a streaming delta to the observers. Not logged, no `seq`.
    pub(crate) fn emit_delta(&mut self, role: Role, delta: Delta) {
        let event = Event::LoopMessage(LoopMessageEvent {
            loop_id: self.loop_id.clone(),
            seq: None,
            role,
            body: LoopMessageBody::Delta { delta },
        });
        self.fan_out(&event);
    }

    /// Register an observer, replaying entries with `seq > since` first.
    pub(crate) fn subscribe(&mut self, subscriber: Subscriber, since: Option<u64>) {
        if let Some(since) = since {
            let events: Vec<Event> = self.session.entries_since(since).into_iter().filter_map(|e| self.entry_to_event(e)).collect();
            for event in events {
                if subscriber.wants(event.method()) {
                    let _ = subscriber.tx.send(encode(&event));
                }
            }
        }
        self.subscribers.retain(|s| s.conn != subscriber.conn);
        self.subscribers.push(subscriber);
    }

    /// Remove a connection's subscription. `true` when there was one.
    pub(crate) fn unsubscribe(&mut self, conn: u64) -> bool {
        let before = self.subscribers.len();
        self.subscribers.retain(|s| s.conn != conn);
        self.subscribers.len() != before
    }

    /// The event the entry at `seq` encodes, if any.
    pub(crate) fn event_at(&self, seq: u64) -> Option<Event> {
        self.entry_at(seq).and_then(|e| self.entry_to_event(e))
    }

    fn emit_seq(&mut self, seq: u64) {
        let event = self.entry_at(seq).and_then(|e| self.entry_to_event(e));
        if let Some(event) = event {
            self.fan_out(&event);
        }
    }

    fn fan_out(&mut self, event: &Event) {
        let method = event.method();
        let line = encode(event);
        self.subscribers.retain(|s| !s.wants(method) || s.tx.send(line.clone()).is_ok());
        let mut star = self.star.lock().unwrap_or_else(|e| e.into_inner());
        star.retain(|s| !s.wants(method) || s.tx.send(line.clone()).is_ok());
    }

    fn entry_at(&self, seq: u64) -> Option<&SessionEntry> {
        let entries = self.session.get_entries();
        entries.binary_search_by_key(&seq, |e| e.seq).ok().map(|i| &entries[i])
    }

    /// The wire message at `seq`, with oversized tool-result blocks replaced
    /// by references.
    fn wire_message_at(&self, seq: u64) -> Option<Message> {
        let entry = self.entry_at(seq)?;
        let message = crate::convert::to_wire(entry.message()?)?;
        Some(self.by_reference(seq, message))
    }

    /// The event an entry encodes, or `None` for entries that are not events.
    pub(crate) fn entry_to_event(&self, entry: &SessionEntry) -> Option<Event> {
        let loop_id = self.loop_id.clone();
        let seq = entry.seq;
        match &entry.kind {
            EntryKind::Message { message } => {
                let message = self.by_reference(seq, crate::convert::to_wire(message)?);
                Some(Event::LoopMessage(LoopMessageEvent {
                    loop_id,
                    seq: Some(seq),
                    role: message.role(),
                    body: LoopMessageBody::Message { message: Box::new(message) },
                }))
            }
            EntryKind::Custom { custom_type, data } => {
                let data = data.clone().unwrap_or(Value::Null);
                let messages = |d: &Value| -> Vec<Message> {
                    d["messages"]
                        .as_array()
                        .map(|seqs| seqs.iter().filter_map(Value::as_u64).filter_map(|s| self.wire_message_at(s)).collect())
                        .unwrap_or_default()
                };
                match custom_type.as_str() {
                    CT_STATUS => Some(Event::LoopStatus(LoopStatusEvent {
                        loop_id,
                        seq,
                        state: serde_json::from_value(data["state"].clone()).unwrap_or(LoopState::Idle),
                        since: data["since"].as_u64().unwrap_or(0),
                        detail: data["detail"].as_str().map(String::from),
                    })),
                    CT_TURN_END => Some(Event::LoopTurnEnd(LoopTurnEndEvent { loop_id, seq, messages: messages(&data) })),
                    CT_RUN_END => Some(Event::LoopRunEnd(LoopRunEndEvent { loop_id, seq, messages: messages(&data) })),
                    CT_UI_STATUS => Some(Event::UiStatus(UiStatusEvent {
                        loop_id,
                        seq,
                        key: data["key"].as_str().unwrap_or("").to_owned(),
                        text: data["text"].as_str().unwrap_or("").to_owned(),
                    })),
                    CT_UI_WIDGET => Some(Event::UiWidget(UiWidgetEvent {
                        loop_id,
                        seq,
                        key: data["key"].as_str().unwrap_or("").to_owned(),
                        lines: serde_json::from_value(data["lines"].clone()).unwrap_or_default(),
                    })),
                    CT_UI_NOTIFY => Some(Event::UiNotify(UiNotifyEvent {
                        loop_id,
                        seq,
                        level: serde_json::from_value(data["level"].clone()).unwrap_or(NotifyLevel::Info),
                        text: data["text"].as_str().unwrap_or("").to_owned(),
                    })),
                    CT_FS_CHANGED => Some(Event::FsChanged(FsChangedEvent {
                        loop_id,
                        seq,
                        path: ServerPath::from(data["path"].as_str().unwrap_or("")),
                        by: serde_json::from_value(data["by"].clone()).unwrap_or(ChangedBy::Tool),
                    })),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Replace oversized text and image blocks of a tool result with
    /// references. Text travels as it is written; an image is decoded first,
    /// so the ref file is the image and its `mimeType` goes in `details.refs`.
    fn by_reference(&self, seq: u64, message: Message) -> Message {
        let Message::ToolResult(mut result) = message else { return message };
        if !result.content.iter().any(oversized) {
            return Message::ToolResult(result);
        }
        let dir = self.refs_dir();
        let mut refs: Vec<Value> = Vec::new();
        for (index, block) in result.content.iter_mut().enumerate() {
            if !oversized(block) {
                continue;
            }
            let (payload, mime_type) = match block {
                Content::Text { text, .. } => (text.as_bytes().to_vec(), None),
                Content::Image { data, mime_type } => {
                    match base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) {
                        Ok(bytes) => (bytes, Some(mime_type.clone())),
                        Err(e) => {
                            tracing::warn!(loop_id = %self.loop_id, seq, index, "undecodable image data, left in place: {e}");
                            continue;
                        }
                    }
                }
                _ => continue,
            };
            let path = dir.join(format!("{}-{seq}-{index}", self.session.get_session_id()));
            if !path.is_file() {
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &payload)) {
                    tracing::warn!(loop_id = %self.loop_id, seq, ?path, "could not write by-reference payload: {e}");
                    continue;
                }
            }
            let bytes = payload.len() as u64;
            let reference = Ref { path: ServerPath::from(path.to_string_lossy().into_owned()), bytes };
            match block {
                Content::Text { text, .. } => *text = BY_REFERENCE.to_owned(),
                Content::Image { data, .. } => *data = BY_REFERENCE.to_owned(),
                _ => {}
            }
            let mut entry = json!({ "index": index, "ref": reference.path, "bytes": bytes });
            if let Some(mime_type) = mime_type {
                entry["mimeType"] = Value::String(mime_type);
            }
            refs.push(entry);
        }
        if let Some(first) = refs.first().cloned() {
            let mut details = result.details.take().and_then(|d| d.as_object().cloned()).unwrap_or_default();
            details.insert("ref".into(), json!({ "ref": first["ref"], "bytes": first["bytes"] }));
            details.insert("refs".into(), Value::Array(refs));
            result.details = Some(Value::Object(details));
        }
        Message::ToolResult(result)
    }

    fn refs_dir(&self) -> PathBuf {
        self.session.get_session_dir().join("refs")
    }
}

/// Whether this block is too big to travel in the event.
fn oversized(block: &Content) -> bool {
    match block {
        Content::Text { text, .. } => text.len() > REF_THRESHOLD,
        Content::Image { data, .. } => data.len() > REF_THRESHOLD,
        _ => false,
    }
}

/// One event as one JSON line.
pub(crate) fn encode(event: &Event) -> String {
    Frame::encode(&Envelope::Notification(event.clone().into_rpc())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::ToolResultMessage;

    fn log(dir: &std::path::Path) -> LoopLog {
        let session = SessionManager::create("/tmp/project", Some(dir)).expect("session");
        LoopLog::new("abc123".into(), session, Default::default())
    }

    fn subscriber(conn: u64, events: Option<&[&str]>) -> (Subscriber, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Subscriber { conn, tx, events: events.map(|e| e.iter().map(|s| s.to_string()).collect()) }, rx)
    }

    fn method(line: &str) -> String {
        let v: Value = serde_json::from_str(line).unwrap();
        v["method"].as_str().unwrap().to_owned()
    }

    #[test]
    fn replay_matches_live_and_skips_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(dir.path());
        let (live, mut live_rx) = subscriber(1, None);
        log.subscribe(live, None);

        log.append_custom(CT_STATUS, json!({"state": "working", "since": 5})).unwrap();
        log.emit_delta(Role::Assistant, Delta::Text { index: 0, text: "hi".into() });
        let seq_user = log.append_message(AgentMessage::user("hello")).unwrap();
        log.append_custom(CT_TURN_END, json!({"messages": [seq_user]})).unwrap();
        log.session_mut().append_model_change("faux", "scripted").unwrap();
        log.append_custom(CT_STATUS, json!({"state": "idle", "since": 9, "detail": "done"})).unwrap();

        let mut live_lines = Vec::new();
        while let Ok(l) = live_rx.try_recv() {
            live_lines.push(l);
        }
        assert_eq!(live_lines.iter().map(|l| method(l)).collect::<Vec<_>>(), ["loop.status", "loop.message", "loop.message", "loop.turn_end", "loop.status"]);

        let (late, mut late_rx) = subscriber(2, None);
        log.subscribe(late, Some(0));
        let mut replayed = Vec::new();
        while let Ok(l) = late_rx.try_recv() {
            replayed.push(l);
        }
        let without_delta: Vec<&String> = live_lines.iter().filter(|l| !l.contains("\"delta\"")).collect();
        assert_eq!(replayed.iter().collect::<Vec<_>>(), without_delta);
        let turn_end: Value = serde_json::from_str(&replayed[2]).unwrap();
        assert_eq!(turn_end["params"]["messages"][0]["content"], "hello");
        assert_eq!(turn_end["params"]["seq"], 3);

        // `since` is exclusive and a filter narrows the replay.
        let (filtered, mut filtered_rx) = subscriber(3, Some(&["loop.status"]));
        log.subscribe(filtered, Some(1));
        let mut got = Vec::new();
        while let Ok(l) = filtered_rx.try_recv() {
            got.push(method(&l));
        }
        assert_eq!(got, ["loop.status"]);
    }

    #[test]
    fn oversized_tool_results_travel_by_reference_but_stay_whole_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(dir.path());
        let (sub, mut rx) = subscriber(1, None);
        log.subscribe(sub, None);
        let big = "x".repeat(REF_THRESHOLD + 1);
        let seq = log
            .append_message(AgentMessage::ToolResult(ToolResultMessage {
                tool_call_id: "c1".into(),
                tool_name: "big".into(),
                content: vec![pi_ai::Content::text(big.clone())],
                details: Some(json!({"kept": true})),
                usage: None,
                is_error: false,
                timestamp: 0,
            }))
            .unwrap();
        let line = rx.try_recv().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        let message = &v["params"]["message"];
        assert_eq!(message["content"][0]["text"], BY_REFERENCE);
        assert_eq!(message["details"]["kept"], true);
        assert_eq!(message["details"]["ref"]["bytes"], big.len());
        let path = message["details"]["ref"]["ref"].as_str().unwrap();
        assert!(path.ends_with(&format!("-{seq}-0")), "{path}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), big);
        // The log keeps the full text.
        let logged = log.session().get_entries().last().unwrap().message().unwrap();
        assert!(matches!(logged, AgentMessage::ToolResult(t) if t.content[0].as_text() == Some(big.as_str())));
    }

    #[test]
    fn an_oversized_image_travels_by_reference_as_decoded_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = log(dir.path());
        let (sub, mut rx) = subscriber(1, None);
        log.subscribe(sub, None);
        let image: Vec<u8> = (0..REF_THRESHOLD).map(|i| (i % 251) as u8).collect();
        let data = base64::engine::general_purpose::STANDARD.encode(&image);
        assert!(data.len() > REF_THRESHOLD, "the base64 is what the threshold is read against");
        let seq = log
            .append_message(AgentMessage::ToolResult(ToolResultMessage {
                tool_call_id: "c1".into(),
                tool_name: "screenshot".into(),
                content: vec![pi_ai::Content::Image { data: data.clone(), mime_type: "image/png".into() }],
                details: None,
                usage: None,
                is_error: false,
                timestamp: 0,
            }))
            .unwrap();
        let v: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let message = &v["params"]["message"];
        assert_eq!(message["content"][0]["data"], BY_REFERENCE);
        assert_eq!(message["content"][0]["mimeType"], "image/png");
        assert_eq!(message["details"]["refs"][0]["mimeType"], "image/png");
        assert_eq!(message["details"]["ref"]["bytes"], image.len(), "the image's bytes, not the base64's");
        let path = message["details"]["ref"]["ref"].as_str().unwrap();
        assert!(path.ends_with(&format!("-{seq}-0")), "{path}");
        assert_eq!(std::fs::read(path).unwrap(), image, "the ref file is the image itself");
        // The log keeps the base64 whole.
        let logged = log.session().get_entries().last().unwrap().message().unwrap();
        assert!(
            matches!(logged, AgentMessage::ToolResult(t) if matches!(&t.content[0], pi_ai::Content::Image { data: d, .. } if *d == data))
        );
    }
}
