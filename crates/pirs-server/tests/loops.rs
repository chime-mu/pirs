//! `[[tool]] loop = { model, prompt, wait = "idle" }` against the real
//! server: one loop asks a second one and reads its answer (S16, S17).
//!
//! The faux provider's script cursor is process-wide, so the parent and its
//! child take turns from one ordered script: step 1 is the parent's tool
//! call, step 2 the child's answer, step 3 the parent's final message.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{text_of, Client, Harness, WAIT};
use serde_json::{json, Value};

/// A policy with a `review` tool that asks a second loop.
const REVIEW: &str = r#"intent = 'a second opinion'
[[tool]]
name = "review"
description = "Ask a second loop to review the text"
params.text = { type = "string" }
loop = { model = "faux/scripted", prompt = "Review this: $text", wait = "idle" }
"#;

fn project_policy(h: &Harness, name: &str, body: &str) {
    let dir = h.project.path().join(".pirs").join("ext");
    std::fs::create_dir_all(&dir).expect("ext dir");
    std::fs::write(dir.join(name), body).expect("write policy");
}

/// Every session log under the harness's home; the file name ends in
/// `_<conversation id>.jsonl`.
fn logs(h: &Harness) -> Vec<(String, Vec<Value>)> {
    let mut files = Vec::new();
    collect_jsonl(&h.home.path().join(".pirs").join("sessions"), &mut files);
    files
        .iter()
        .map(|path| {
            let entries = std::fs::read_to_string(path)
                .expect("read session")
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("a JSON line"))
                .collect();
            (path.file_stem().unwrap_or_default().to_string_lossy().into_owned(), entries)
        })
        .collect()
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// The loops `loop.list` reports, by id.
async fn list(c: &mut Client) -> Vec<Value> {
    c.call("loop.list", json!({})).await["loops"].as_array().cloned().unwrap_or_default()
}

/// Poll `loop.list` until it holds `n` loops, or give up.
async fn wait_for_loops(c: &mut Client, n: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let loops = list(c).await;
        if loops.len() >= n || tokio::time::Instant::now() >= deadline {
            return loops;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_loop_tool_asks_a_second_loop_and_returns_its_final_message() {
    let script = vec![
        json!({"toolCalls": [{"name": "review", "arguments": {"text": "the diff"}}]}),
        json!("CHILD-VERDICT: fine"),
        json!("parent says: done"),
    ];
    let h = Harness::start(WAIT, script).await;
    project_policy(&h, "review.pirs.toml", REVIEW);
    let mut c = h.client().await;

    let info = c.create(&h.cwd(), json!({"name": "parent"})).await;
    let parent = info["id"].as_str().expect("loop id").to_owned();
    c.subscribe(&parent, None).await;
    c.call("loop.prompt", json!({"loop": parent, "text": "go"})).await;
    let events = c.events_until("loop.run_end").await;

    // The parent's own last message, after the tool result came back.
    let assistant: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "assistant" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert!(
        assistant.last().is_some_and(|text| text.contains("parent says: done")),
        "the parent answered after the review: {assistant:?}"
    );

    // The child is a loop of its own, listed with its parent, still alive.
    let loops = list(&mut c).await;
    assert_eq!(loops.len(), 2, "the parent and its child: {loops:#?}");
    let child = loops.iter().find(|l| l["id"] != json!(parent)).expect("a child loop");
    assert_eq!(child["parent"], json!(parent), "the child names its parent");
    assert_eq!(child["name"], json!("parent/review"), "named after the parent and the tool");
    assert_eq!(child["cwd"], loops[0]["cwd"], "the same directory");
    let child_id = child["id"].as_str().expect("child id").to_owned();
    let child_conversation = child["conversation"].as_str().expect("conversation").to_owned();

    // The parent's log holds the child's answer as the tool result, with
    // the child's id in `details` so a reader can open it.
    let logs = logs(&h);
    assert_eq!(logs.len(), 2, "one conversation each");
    let parent_log = logs
        .iter()
        .find(|(name, _)| !name.ends_with(&child_conversation))
        .map(|(_, entries)| entries.clone())
        .expect("the parent's log");
    let results: Vec<&Value> = parent_log
        .iter()
        .filter(|e| e["type"] == "message" && e["message"]["role"] == "toolResult")
        .map(|e| &e["message"])
        .collect();
    assert_eq!(results.len(), 1, "one tool result: {parent_log:#?}");
    assert_eq!(results[0]["toolName"], "review");
    assert!(
        results[0]["content"].to_string().contains("CHILD-VERDICT: fine"),
        "the child's answer is the result: {}",
        results[0]
    );
    assert_eq!(results[0]["details"]["loop"], json!(child_id), "the child's id is in the log");
    assert_eq!(results[0]["details"]["conversation"], json!(child_conversation));

    // Closing the parent closes the child with it (D-28).
    c.call("loop.close", json!({"loop": parent})).await;
    assert!(list(&mut c).await.is_empty(), "the child went with its parent");
    h.stop().await;
}

#[tokio::test]
async fn aborting_the_parent_ends_the_call_and_the_child_without_hanging() {
    // The child's first move is a tool call that sleeps, so it is still
    // working when the parent is aborted.
    let script = vec![
        json!({"toolCalls": [{"name": "review", "arguments": {"text": "the diff"}}]}),
        json!({"toolCalls": [{"name": "slow", "arguments": {}}]}),
    ];
    let h = Harness::start(WAIT, script).await;
    project_policy(
        &h,
        "review.pirs.toml",
        &format!("{REVIEW}\n[[tool]]\nname = \"slow\"\nparams = {{}}\nrun = \"sleep 30\"\ntimeout = 60\n"),
    );
    let mut c = h.client().await;

    let parent = c.create(&h.cwd(), json!({})).await["id"].as_str().expect("loop id").to_owned();
    c.call("loop.prompt", json!({"loop": parent, "text": "go"})).await;
    let loops = wait_for_loops(&mut c, 2).await;
    assert_eq!(loops.len(), 2, "the child started: {loops:#?}");
    let child_id = loops
        .iter()
        .find(|l| l["id"] != json!(parent))
        .and_then(|l| l["id"].as_str())
        .expect("child id")
        .to_owned();

    c.call("loop.abort", json!({"loop": parent})).await;
    let started = tokio::time::Instant::now();
    assert_eq!(c.wait(&parent).await["state"], "idle", "the parent stopped waiting");
    assert_eq!(c.wait(&child_id).await["state"], "idle", "the child stopped too");
    assert!(started.elapsed() < WAIT, "neither wait hung");

    // The child outlives the call it served, and goes when the parent does.
    assert_eq!(list(&mut c).await.len(), 2);
    c.call("loop.close", json!({"loop": parent})).await;
    assert!(list(&mut c).await.is_empty());
    h.stop().await;
}

#[tokio::test]
async fn an_unresolvable_model_is_an_error_result_and_a_load_warning() {
    // Step 2 is the parent's own next turn: no second loop is started, so
    // nobody else takes a step from the script.
    let script = vec![
        json!({"toolCalls": [{"name": "review", "arguments": {"text": "the diff"}}]}),
        json!("the parent carried on"),
    ];
    let h = Harness::start(WAIT, script).await;
    project_policy(
        &h,
        "review.pirs.toml",
        &REVIEW.replace("model = \"faux/scripted\"", "model = \"nowhere/nothing\""),
    );
    let mut c = h.client().await;

    // The load itself says so, which is what `pirs check` prints (D-41).
    let conflicts = c.call("dsl.check", json!({"cwd": h.cwd()})).await["conflicts"].as_array().cloned().unwrap_or_default();
    assert!(
        conflicts
            .iter()
            .any(|c| c["message"].as_str().is_some_and(|m| m.contains("unknown model `nowhere/nothing`"))),
        "dsl.check reports the model that cannot be resolved: {conflicts:#?}"
    );

    let parent = c.create(&h.cwd(), json!({})).await["id"].as_str().expect("loop id").to_owned();
    c.subscribe(&parent, None).await;
    c.call("loop.prompt", json!({"loop": parent, "text": "go"})).await;
    let events = c.events_until("loop.run_end").await;

    assert!(
        events.iter().any(|(m, p)| m == "ui.notify"
            && p["level"] == "warning"
            && p["text"].as_str().is_some_and(|t| t.contains("unknown model `nowhere/nothing`"))),
        "the load warning names the model that could not be resolved: {events:#?}"
    );
    // And the model itself reads why nothing was reviewed.
    let results: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "toolResult")
        .map(|(_, p)| p["message"].to_string())
        .collect();
    assert!(
        results.iter().any(|r| r.contains("tool `review`: unknown model `nowhere/nothing`")),
        "the call is an error result the model sees: {results:#?}"
    );
    assert_eq!(list(&mut c).await.len(), 1, "no second loop was started with the wrong model");
    h.stop().await;
}

#[tokio::test]
async fn nesting_stops_at_the_depth_limit_with_an_error_result() {
    // Every turn calls `review`, so each loop starts one more: the chain
    // only ends because the server refuses to go deeper than 8 (D-19 is
    // about judging the model, not about the machine).
    let call = json!({"toolCalls": [{"name": "review", "arguments": {"text": "again"}}]});
    let script = vec![call; 9];
    let h = Harness::start(WAIT, script).await;
    project_policy(&h, "review.pirs.toml", REVIEW);
    let mut c = h.client().await;

    let root = c.create(&h.cwd(), json!({"name": "root"})).await["id"].as_str().expect("loop id").to_owned();
    c.call("loop.prompt", json!({"loop": root, "text": "go"})).await;
    assert_eq!(c.wait(&root).await["state"], "idle", "the recursion ended on its own");

    // The root plus the eight loops below it, and no ninth.
    let loops = list(&mut c).await;
    assert_eq!(loops.len(), 9, "the chain stopped at the limit: {loops:#?}");
    let logs = logs(&h);
    let refusals: Vec<&Value> = logs
        .iter()
        .flat_map(|(_, entries)| entries.iter())
        .filter(|e| {
            e["type"] == "message"
                && e["message"]["role"] == "toolResult"
                && e["message"]["content"].to_string().contains("nesting deeper than 8")
        })
        .collect();
    assert!(!refusals.is_empty(), "the deepest loop read the refusal: {logs:#?}");
    c.call("loop.close", json!({"loop": root})).await;
    h.stop().await;
}

#[tokio::test]
async fn only_wait_idle_parses_and_dsl_check_says_so() {
    let h = Harness::start(WAIT, vec![]).await;
    project_policy(
        &h,
        "review.pirs.toml",
        &REVIEW.replace("wait = \"idle\"", "wait = \"now\""),
    );
    let mut c = h.client().await;
    let result = c.call("dsl.check", json!({"cwd": h.cwd()})).await;
    let conflicts = result["conflicts"].as_array().cloned().unwrap_or_default();
    assert!(
        conflicts.iter().any(|c| c["message"].as_str().is_some_and(|m| m.contains("loop.wait") && m.contains("idle"))),
        "the parse error is reported: {conflicts:#?}"
    );
    assert!(
        !result["manifest"]["tools"].as_array().into_iter().flatten().any(|t| t["name"] == "review"),
        "the entry did not load"
    );
    h.stop().await;
}
