//! Handler registrations and the slot request path (D-23).
//!
//! A connected client `register`s a slot on a loop with a timeout. When the
//! slot fires, the server sends that connection a JSON-RPC request whose
//! `method` is the slot and waits up to the timeout for the reply. Handlers
//! run in registration order, each seeing the previous one's outcome. A
//! handler that times out, replies with an error, replies with something that
//! is not the slot's reply type, or disconnects counts as "no opinion": the
//! call continues as if it were not registered, and a `ui.notify` warning is
//! emitted on the loop. `on.<event>` slots are notifications and are never
//! waited for.
//!
//! A `tool.<name>` registration becomes an [`AgentTool`] in the loop's tool
//! set ([`HandlerTool`]). `register` carries no schema this phase, so the
//! manifest entry is the name with `{ "type": "object" }` parameters and a
//! description naming the registrant; phase 4's `[[tool]]` with `params`
//! supplies the real one.
//!
//! The server initiates nothing else: observers are never waited on, and the
//! only other thing it calls is a process it spawned itself (phase 2+).

use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent::{AgentTool, ToolRef, ToolResult, UpdateFn};
use pirs_protocol::{
    code, InputPayload, InputReply, NotifyLevel, OnPayload, PromptPayload, PromptReply, Slot, SlotReply, SlotRequest,
    ToolCallPayload, ToolReply, ToolResultPayload, ToolResultReply,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::LoopHandle;
use crate::server::{Connection, HandlerFailure};

/// One `register` call: the connection, the slot and how long to wait.
#[derive(Clone)]
pub(crate) struct Registration {
    pub(crate) conn: Arc<Connection>,
    pub(crate) slot: Slot,
    pub(crate) timeout: Duration,
}

impl LoopHandle {
    /// Register `slot` for `conn`. Registering the same slot twice from one
    /// connection replaces the timeout and keeps the original position.
    pub(crate) fn register(self: &Arc<Self>, conn: Arc<Connection>, slot: Slot, timeout: Duration) {
        {
            let mut handlers = self.handlers.lock().unwrap_or_else(|e| e.into_inner());
            match handlers.iter_mut().find(|r| r.conn.id() == conn.id() && r.slot == slot) {
                Some(existing) => existing.timeout = timeout,
                None => handlers.push(Registration { conn, slot: slot.clone(), timeout }),
            }
        }
        if matches!(slot, Slot::Tool(_)) {
            self.refresh_tools();
        }
    }

    /// Remove `conn`'s registration of `slot`. `false` when there was none.
    pub(crate) fn unregister(self: &Arc<Self>, conn: u64, slot: &Slot) -> bool {
        let removed = {
            let mut handlers = self.handlers.lock().unwrap_or_else(|e| e.into_inner());
            let before = handlers.len();
            handlers.retain(|r| !(r.conn.id() == conn && r.slot == *slot));
            handlers.len() != before
        };
        if removed && matches!(slot, Slot::Tool(_)) {
            self.refresh_tools();
        }
        removed
    }

    /// Drop everything `conn` registered (it disconnected).
    pub(crate) fn unregister_connection(self: &Arc<Self>, conn: u64) {
        let had_tool = {
            let mut handlers = self.handlers.lock().unwrap_or_else(|e| e.into_inner());
            let had_tool = handlers.iter().any(|r| r.conn.id() == conn && matches!(r.slot, Slot::Tool(_)));
            handlers.retain(|r| r.conn.id() != conn);
            had_tool
        };
        if had_tool {
            self.refresh_tools();
        }
    }

    fn registrations_for(&self, slot: &Slot) -> Vec<Registration> {
        self.handlers.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|r| r.slot == *slot).cloned().collect()
    }

    /// The tools registered handlers provide, one per distinct name, in
    /// registration order.
    pub(crate) fn handler_tools(self: &Arc<Self>) -> Vec<ToolRef> {
        let mut names: Vec<(String, String)> = Vec::new();
        for r in self.handlers.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if let Slot::Tool(name) = &r.slot {
                if !names.iter().any(|(n, _)| n == name) {
                    names.push((name.clone(), r.conn.label()));
                }
            }
        }
        names
            .into_iter()
            .map(|(name, by)| Arc::new(HandlerTool { handle: Arc::downgrade(self), name, by }) as ToolRef)
            .collect()
    }

    /// Send one slot request and wait for its reply. `None` is "no opinion".
    async fn call_slot(&self, reg: &Registration, request: SlotRequest) -> Option<SlotReply> {
        let slot = reg.slot.to_string();
        let outcome = reg.conn.call(slot.clone(), request.params(), reg.timeout).await;
        let value = match outcome {
            Ok(v) => v,
            Err(HandlerFailure::Timeout) => {
                self.warn(
                    format!("handler {} for {slot} did not reply within {} ms; skipped", reg.conn.label(), reg.timeout.as_millis()),
                    code::HANDLER_TIMEOUT,
                );
                return None;
            }
            Err(HandlerFailure::Error(e)) => {
                self.warn(format!("handler {} for {slot} failed: {}; skipped", reg.conn.label(), e.message), code::HANDLER_ERROR);
                return None;
            }
            Err(HandlerFailure::Disconnected) => {
                tracing::debug!(slot, "handler disconnected before replying");
                return None;
            }
        };
        match request.parse_reply(value) {
            Ok(reply) => Some(reply),
            Err(e) => {
                self.warn(format!("handler {} for {slot} replied with the wrong shape: {e}; skipped", reg.conn.label()), code::HANDLER_ERROR);
                None
            }
        }
    }

    fn warn(&self, text: String, code: i64) {
        tracing::warn!(loop_id = %self.id, code, "{text}");
        self.ui_notify(NotifyLevel::Warning, text);
    }

    /// Run the `input` slot over a prompt: the policy's own `[[input]]`
    /// entries first, in file order, then the registered handlers in
    /// registration order. `None` means it was consumed.
    pub(crate) async fn dispatch_input(self: &Arc<Self>, text: String) -> Option<String> {
        let mut text = self.dsl_input(text).await?;
        for reg in self.registrations_for(&Slot::Input) {
            match self.call_slot(&reg, SlotRequest::Input(InputPayload { text: text.clone() })).await {
                Some(SlotReply::Input(InputReply::Text { text: t })) => text = t,
                Some(SlotReply::Input(InputReply::Handled { .. })) => return None,
                _ => {}
            }
        }
        Some(text)
    }

    /// Run the registered `prompt` handlers over the assembled system
    /// prompt. The policy's own `[[prompt]]` entries are already in it: they
    /// are the `<policy>` section of the base prompt (D-21).
    pub(crate) async fn dispatch_prompt(&self, mut prompt: String) -> String {
        for reg in self.registrations_for(&Slot::Prompt) {
            match self.call_slot(&reg, SlotRequest::Prompt(PromptPayload { system_prompt: prompt.clone() })).await {
                Some(SlotReply::Prompt(PromptReply::Append { append })) => {
                    if !append.is_empty() {
                        prompt.push_str("\n\n");
                        prompt.push_str(&append);
                    }
                }
                Some(SlotReply::Prompt(PromptReply::Replace { replace })) => prompt = replace,
                _ => {}
            }
        }
        prompt
    }

    /// Run the `tool_result` slot: the policy's `[[tool_result]]` entries
    /// for this tool in file order, then the registered handlers, each
    /// seeing the previous one's result. `Some((result, by))` when at least
    /// one rewrote it, `by` naming the last that did.
    pub(crate) async fn dispatch_tool_result(
        self: &Arc<Self>,
        tool: &str,
        args: &Value,
        result: ToolReply,
    ) -> Option<(ToolReply, String)> {
        let from_policy = self.dsl_tool_result(tool, args, result.clone()).await;
        let (mut result, mut by) = match from_policy {
            Some((result, by)) => (result, Some(by)),
            None => (result, None),
        };
        for reg in self.registrations_for(&Slot::ToolResult) {
            let payload = ToolResultPayload { tool: tool.to_owned(), args: args.clone(), result: result.clone() };
            if let Some(SlotReply::ToolResult(ToolResultReply { result: r })) =
                self.call_slot(&reg, SlotRequest::ToolResult(payload)).await
            {
                result = r;
                by = Some(reg.conn.label());
            }
        }
        by.map(|by| (result, by))
    }

    /// Call the handler that registered `tool.<name>`. A missing, timed-out
    /// or failing handler yields an error result.
    pub(crate) async fn call_tool(&self, name: &str, args: Value, id: &str) -> ToolReply {
        let Some(reg) = self.registrations_for(&Slot::Tool(name.to_owned())).into_iter().next() else {
            return ToolReply::Error { error: format!("no handler registered for tool {name}") };
        };
        let request = SlotRequest::Tool { name: name.to_owned(), payload: ToolCallPayload { args, id: id.to_owned() } };
        match self.call_slot(&reg, request).await {
            Some(SlotReply::Tool(reply)) => reply,
            _ => ToolReply::Error { error: format!("tool {name} gave no result (handler {} timed out or failed)", reg.conn.label()) },
        }
    }

    /// Fire an `on.<event>`: the policy's `[[on]]` entries (and the
    /// `[[status]]` and `[[widget]]` ones that desugared to them) first,
    /// then a notification to every registered handler. Never waited for.
    pub(crate) fn notify_on(self: &Arc<Self>, payload: OnPayload) {
        self.fire_dsl_on(&payload);
        let slot = Slot::On(payload.event());
        let regs = self.registrations_for(&slot);
        if regs.is_empty() {
            return;
        }
        let request = SlotRequest::On(payload);
        let params = request.params();
        for reg in regs {
            reg.conn.notify(slot.to_string(), params.clone());
        }
    }
}

/// A tool implemented by a connected handler that registered `tool.<name>`.
pub(crate) struct HandlerTool {
    handle: Weak<LoopHandle>,
    name: String,
    by: String,
}

#[async_trait]
impl AgentTool for HandlerTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        format!("Tool `{}` provided by connected handler {}.", self.name, self.by)
    }

    fn parameters(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    async fn execute(&self, tool_call_id: &str, args: Value, cancel: CancellationToken, _on_update: UpdateFn) -> anyhow::Result<ToolResult> {
        let Some(handle) = self.handle.upgrade() else {
            anyhow::bail!("loop closed");
        };
        // `loop.abort` and `loop.close` must not wait out the handler's
        // registered timeout: the call is given up on the moment the run is
        // cancelled, and the tool reports "aborted".
        let reply = tokio::select! {
            biased;
            () = cancel.cancelled() => anyhow::bail!("aborted"),
            reply = handle.call_tool(&self.name, args, tool_call_id) => reply,
        };
        let (result, is_error) = crate::convert::result_from_reply(&reply);
        if is_error {
            anyhow::bail!("{}", result.content.iter().filter_map(pi_ai::Content::as_text).collect::<Vec<_>>().join(""));
        }
        Ok(result)
    }
}
