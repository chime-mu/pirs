//! Conversion between the agent's message types and the protocol's (D-08).
//!
//! `pirs_protocol::Message` serialises to exactly the session-log JSON that
//! `pi_agent::AgentMessage` does for the four LLM roles, so those convert by
//! a JSON round trip. The four app-level roles have no wire counterpart and
//! are mapped to the user message the model reads for them (the same text
//! `pi_agent::default_convert_to_llm` produces), or skipped:
//!
//! | agent role          | wire role                                            |
//! |---------------------|------------------------------------------------------|
//! | `custom`            | `user`, same content                                 |
//! | `bashExecution`     | `user` with `$ cmd\noutput`; skipped when excluded   |
//! | `branchSummary`     | `user` wrapped in `<branch_summary>`                 |
//! | `compactionSummary` | `user` wrapped in `<compaction_summary>`             |

use pi_agent::{AgentMessage, ToolResult};
use pirs_protocol::{Content, Message, ToolContent, ToolReply, UserContent, UserMessage};
use serde_json::Value;

fn round_trip<T: serde::Serialize, U: serde::de::DeserializeOwned>(value: &T) -> Option<U> {
    serde_json::from_value(serde_json::to_value(value).ok()?).ok()
}

/// The wire form of an agent message, or `None` when it has none.
pub(crate) fn to_wire(message: &AgentMessage) -> Option<Message> {
    match message {
        AgentMessage::System(_) | AgentMessage::User(_) | AgentMessage::Assistant(_) | AgentMessage::ToolResult(_) => {
            round_trip(message)
        }
        AgentMessage::Custom(c) => Some(Message::User(UserMessage {
            content: round_trip(&c.content)?,
            timestamp: c.timestamp,
        })),
        AgentMessage::BashExecution(b) if b.exclude_from_context => None,
        AgentMessage::BashExecution(b) => {
            let mut text = format!("$ {}\n{}", b.command, b.output);
            if let Some(code) = b.exit_code.filter(|c| *c != 0) {
                text.push_str(&format!("\n(exit code {code})"));
            }
            Some(Message::User(UserMessage { content: UserContent::Text(text), timestamp: b.timestamp }))
        }
        AgentMessage::BranchSummary(b) => Some(Message::User(UserMessage {
            content: UserContent::Text(format!("<branch_summary>\n{}\n</branch_summary>", b.summary)),
            timestamp: b.timestamp,
        })),
        AgentMessage::CompactionSummary(c) => Some(Message::User(UserMessage {
            content: UserContent::Text(format!("<compaction_summary>\n{}\n</compaction_summary>", c.summary)),
            timestamp: c.timestamp,
        })),
    }
}

/// Wire content blocks from agent content blocks (same JSON).
pub(crate) fn content_to_wire(content: &[pi_ai::Content]) -> Vec<Content> {
    content.iter().filter_map(round_trip).collect()
}

/// Agent content blocks from wire content blocks (same JSON).
pub(crate) fn content_from_wire(content: &[Content]) -> Vec<pi_ai::Content> {
    content.iter().filter_map(round_trip).collect()
}

/// What a `tool_result` handler sees: a tool's result as a [`ToolReply`].
pub(crate) fn reply_from_result(result: &ToolResult, is_error: bool) -> ToolReply {
    if is_error {
        ToolReply::Error { error: result.content.iter().filter_map(pi_ai::Content::as_text).collect::<Vec<_>>().join("") }
    } else {
        ToolReply::Ok { content: ToolContent::Blocks(content_to_wire(&result.content)), details: result.details.clone() }
    }
}

/// A handler's [`ToolReply`] as the tool result the loop records, with its
/// error flag.
pub(crate) fn result_from_reply(reply: &ToolReply) -> (ToolResult, bool) {
    match reply {
        ToolReply::Ok { content, details } => {
            let content = match content {
                ToolContent::Text(text) => vec![pi_ai::Content::text(text.clone())],
                ToolContent::Blocks(blocks) => content_from_wire(blocks),
            };
            (ToolResult { content, details: details.clone(), ..Default::default() }, false)
        }
        ToolReply::Error { error } => (ToolResult::error(error.clone()), true),
    }
}

/// The protocol's thinking level from the agent's (same names).
pub(crate) fn thinking_to_wire(level: pi_ai::ThinkingLevel) -> pirs_protocol::ThinkingLevel {
    round_trip(&level).unwrap_or_default()
}

/// The agent's thinking level from the protocol's (same names).
pub(crate) fn thinking_from_wire(level: pirs_protocol::ThinkingLevel) -> pi_ai::ThinkingLevel {
    round_trip(&level).unwrap_or_default()
}

/// A tool declaration as the manifest lists it.
pub(crate) fn tool_info(tool: &pi_agent::ToolRef) -> pirs_protocol::ToolInfo {
    pirs_protocol::ToolInfo { name: tool.name(), description: tool.description(), parameters: tool.parameters() }
}

/// The text of a `Value` that is a string, for `details` lookups.
pub(crate) fn value_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent::{BashExecutionMessage, CustomMessage};

    #[test]
    fn llm_roles_round_trip_and_app_roles_become_user_messages() {
        let user = AgentMessage::user("hi");
        assert!(matches!(to_wire(&user), Some(Message::User(u)) if u.content.plain_text() == "hi"));

        let custom = AgentMessage::Custom(CustomMessage {
            custom_type: "x".into(),
            content: "note".into(),
            display: true,
            details: None,
            timestamp: 3,
        });
        assert!(matches!(to_wire(&custom), Some(Message::User(u)) if u.content.plain_text() == "note" && u.timestamp == 3));

        let bash = |exclude: bool| {
            AgentMessage::BashExecution(BashExecutionMessage {
                command: "ls".into(),
                output: "a".into(),
                exit_code: Some(2),
                cancelled: false,
                truncated: false,
                full_output_path: None,
                exclude_from_context: exclude,
                timestamp: 0,
            })
        };
        assert!(to_wire(&bash(true)).is_none());
        assert!(matches!(to_wire(&bash(false)), Some(Message::User(u)) if u.content.plain_text() == "$ ls\na\n(exit code 2)"));
    }

    #[test]
    fn tool_replies_map_both_ways() {
        let ok = ToolResult::text("out").with_details(serde_json::json!({"k": 1}));
        let reply = reply_from_result(&ok, false);
        assert!(matches!(&reply, ToolReply::Ok { details: Some(d), .. } if d["k"] == 1));
        let (back, err) = result_from_reply(&reply);
        assert!(!err);
        assert_eq!(back.content[0].as_text(), Some("out"));

        let failed = reply_from_result(&ToolResult::error("boom"), true);
        assert!(matches!(&failed, ToolReply::Error { error } if error == "boom"));
        let (back, err) = result_from_reply(&failed);
        assert!(err);
        assert_eq!(back.content[0].as_text(), Some("boom"));

        let (text, _) = result_from_reply(&ToolReply::Ok { content: "plain".into(), details: None });
        assert_eq!(text.content[0].as_text(), Some("plain"));
    }

    #[test]
    fn thinking_levels_share_names() {
        assert_eq!(thinking_to_wire(pi_ai::ThinkingLevel::High), pirs_protocol::ThinkingLevel::High);
        assert_eq!(thinking_from_wire(pirs_protocol::ThinkingLevel::Max), pi_ai::ThinkingLevel::Max);
    }
}
