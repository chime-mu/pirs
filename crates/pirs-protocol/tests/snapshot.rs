//! The JSON schema of the whole protocol, compared to `docs/protocol.schema.json`.
//!
//! Run `UPDATE_SNAPSHOT=1 cargo test -p pirs-protocol` to rewrite the file.

use pirs_protocol::ProtocolSchema;

const SNAPSHOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/protocol.schema.json"
);

fn generate() -> String {
    let schema = schemars::schema_for!(ProtocolSchema);
    let mut text = serde_json::to_string_pretty(&schema).expect("schema serialises");
    text.push('\n');
    text
}

#[test]
fn schema_matches_snapshot() {
    let generated = generate();
    if std::env::var_os("UPDATE_SNAPSHOT").is_some() {
        std::fs::write(SNAPSHOT, &generated).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(SNAPSHOT).unwrap_or_else(|e| {
        panic!("cannot read {SNAPSHOT}: {e}\nrun UPDATE_SNAPSHOT=1 cargo test -p pirs-protocol to create it")
    });
    if expected != generated {
        let diff = similar::TextDiff::from_lines(&expected, &generated);
        let mut out = String::new();
        for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
            out.push_str(&hunk.to_string());
        }
        panic!(
            "docs/protocol.schema.json is out of date (- snapshot, + generated):\n{out}\n\
             The wire types changed. If that is intended, update 30-protocol.md and run\n\
             UPDATE_SNAPSHOT=1 cargo test -p pirs-protocol"
        );
    }
}

#[test]
fn schema_is_deterministic() {
    assert_eq!(generate(), generate());
}

#[test]
fn schema_names_every_method_event_and_slot() {
    let text = generate();
    for m in pirs_protocol::Request::METHODS {
        assert!(
            text.contains(&format!("\"{m}\"")),
            "request {m} missing from schema"
        );
    }
    for e in pirs_protocol::Event::METHODS {
        assert!(
            text.contains(&format!("\"{e}\"")),
            "event {e} missing from schema"
        );
    }
    for slot in ["input", "prompt", "tool_result", "tool\\\\.", "on\\\\."] {
        assert!(text.contains(slot), "slot {slot} missing from schema");
    }
}
