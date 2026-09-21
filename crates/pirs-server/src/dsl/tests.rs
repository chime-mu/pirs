//! Tests for the DSL loader: the design file's own examples, every
//! composition rule, and the errors a person reads when a file is wrong.

use std::path::{Path, PathBuf};

use pirs_protocol::{OnEvent, ToolInfo};
use serde_json::json;
use tempfile::TempDir;

use super::*;

/// Every slot, copied from the examples in `docs/design/40-dsl.md`.
const DESIGN_EXAMPLES: &str = r#"
intent = """
Show the current git branch in the status line and refresh it after every turn.
Let me type ?question for a brief answer and !cmd to run a shell command.
"""

[[input]]                              # transform or consume user input
match = '^\?(.*)'
replace = "Explain briefly: $1"

[[input]]
match = '^!(.*)'
handled = true
run = "$1"

[[prompt]]                             # add to the system prompt
text = "Prefer small commits. Never rewrite history."

[[prompt]]
files = ".claude/rules/*.md"

[[prompt]]
run = "git log --oneline -5"
header = "Recent commits:"

[[status]]                             # emit ui.status
key = "branch"
run = "git branch --show-current"
on = ["start", "turn_end"]

[[widget]]                             # emit ui.widget
key = "todo"
file = ".pirs/todo.md"
on = ["start", "tool_result"]

[[tool_result]]                        # rewrite what the model reads back from a tool
tool = "bash"
run = "./tools/trim-test-output.sh"    # payload on stdin, rewritten result on stdout

[[on]]                                 # run something at an event
event = "turn_end"
run = "git add -A && git commit -qm 'pirs checkpoint'"
quiet = true

[[command]]                            # /name
name = "handoff"
description = "Start a fresh session with a summary"
run = "./scripts/handoff.sh $args"

[[tool]]                               # give the model an executable
name = "fetch"
description = "Fetch a URL and return its text"
params.url = { type = "string", description = "Absolute http(s) URL" }
params.max_length = { type = "integer", default = 50000 }
run = "./tools/fetch.py"
timeout = 30

[[tool]]
name = "review"
description = "Ask a second loop to review the diff"
loop = { model = "claude-opus", prompt = "Review this diff critically:\n$diff", wait = "idle" }

[[tool]]
name = "bash"
disabled = true                        # or wrap = "./tools/sandboxed-bash.sh"
"#;

fn parse(text: &str) -> PolicyFile {
    parse_file(Path::new("/p/a.pirs.toml"), text).expect("parses")
}

fn errors(text: &str) -> Vec<String> {
    parse_file(Path::new("/p/a.pirs.toml"), text).expect_err("rejected")
}

/// A file with `intent` and whatever else the test needs.
fn file(body: &str) -> String {
    format!("intent = \"a test\"\n{body}")
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn every_slot_of_the_design_file_parses() {
    let file = parse(DESIGN_EXAMPLES);
    assert!(file.intent.starts_with("Show the current git branch"));
    assert_eq!(file.priority, 0);

    assert_eq!(file.input.len(), 2);
    assert_eq!(file.input[0].pattern, r"^\?(.*)");
    assert_eq!(file.input[0].replace.as_deref(), Some("Explain briefly: $1"));
    assert!(!file.input[0].handled);
    assert!(file.input[1].handled);
    assert_eq!(file.input[1].run.as_deref(), Some("$1"));
    assert!(file.input[0].regex.is_match("?why"));

    assert_eq!(
        file.prompt.iter().map(|entry| entry.source.clone()).collect::<Vec<_>>(),
        [
            PromptSource::Text("Prefer small commits. Never rewrite history.".to_owned()),
            PromptSource::Files(".claude/rules/*.md".to_owned()),
            PromptSource::Run("git log --oneline -5".to_owned()),
        ]
    );
    assert_eq!(file.prompt[2].header.as_deref(), Some("Recent commits:"));

    assert_eq!(file.status[0].key, "branch");
    assert_eq!(file.status[0].on, [OnEvent::Start, OnEvent::TurnEnd]);
    assert_eq!(file.widget[0].source, WidgetSource::File(PathBuf::from(".pirs/todo.md")));
    assert_eq!(file.widget[0].on, [OnEvent::Start, OnEvent::ToolResult]);

    assert_eq!(file.tool_result[0].tool, "bash");
    assert_eq!(file.on[0].event, OnEvent::TurnEnd);
    assert!(file.on[0].quiet);

    assert_eq!(file.command[0].name, "handoff");
    assert_eq!(file.command[0].description, "Start a fresh session with a summary");

    assert_eq!(file.tool.len(), 3);
    assert_eq!(file.tool[0].name, "fetch");
    assert_eq!(file.tool[0].timeout, Duration::from_secs(30));
    assert_eq!(file.tool[0].params["url"], json!({"type": "string", "description": "Absolute http(s) URL"}));
    assert_eq!(
        file.tool[1].loop_spec,
        Some(LoopSpec {
            model: "claude-opus".to_owned(),
            prompt: "Review this diff critically:\n$diff".to_owned(),
            wait: "idle".to_owned(),
        })
    );
    assert_eq!(file.tool[2].timeout, DEFAULT_TOOL_TIMEOUT, "a tool without `timeout` gets the default");
    assert!(file.tool[2].disabled);
}

#[test]
fn an_origin_names_the_file_the_slot_and_the_entry() {
    let file = parse(DESIGN_EXAMPLES);
    assert_eq!(file.input[1].origin.to_string(), "/p/a.pirs.toml: [[input]] #2");
    assert_eq!(file.tool[0].origin, Origin { file: PathBuf::from("/p/a.pirs.toml"), slot: "tool", index: 0 });
}

#[test]
fn an_unknown_field_names_the_table_and_the_field() {
    let messages = errors(&file("[[input]]\nmatch = 'x'\nrepalce = 'y'\n"));
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("[[input]] #1:"), "{}", messages[0]);
    assert!(messages[0].contains("repalce"), "{}", messages[0]);
}

#[test]
fn an_unknown_field_is_rejected_in_every_table() {
    for (slot, body) in [
        ("prompt", "[[prompt]]\ntext = 'x'\nnope = 1\n"),
        ("tool_result", "[[tool_result]]\ntool = 'bash'\nrun = 'x'\nnope = 1\n"),
        ("status", "[[status]]\nkey = 'k'\nrun = 'x'\non = []\nnope = 1\n"),
        ("widget", "[[widget]]\nkey = 'k'\nrun = 'x'\non = []\nnope = 1\n"),
        ("on", "[[on]]\nevent = 'start'\nrun = 'x'\nnope = 1\n"),
        ("command", "[[command]]\nname = 'c'\ndescription = 'd'\nrun = 'x'\nnope = 1\n"),
        ("tool", "[[tool]]\nname = 't'\nrun = 'x'\nnope = 1\n"),
    ] {
        let messages = errors(&file(body));
        assert!(
            messages.iter().any(|m| m.starts_with(&format!("[[{slot}]] #1:")) && m.contains("nope")),
            "{slot}: {messages:?}"
        );
    }
    let messages = errors(&file("[settings]\nnope = 1\n"));
    assert!(messages[0].starts_with("[settings]:") && messages[0].contains("nope"), "{messages:?}");
    let messages = errors(&file("nope = 1\n"));
    assert!(messages[0].contains("unknown key `nope`"), "{messages:?}");
}

#[test]
fn a_file_without_an_intent_is_rejected() {
    let messages = errors("[[prompt]]\ntext = 'x'\n");
    assert_eq!(messages, ["missing `intent`: every policy file says what it is for"]);
}

#[test]
fn a_match_that_is_not_a_regex_is_rejected() {
    let messages = errors(&file("[[input]]\nmatch = '^('\n"));
    assert!(messages[0].contains("[[input]] #1") && messages[0].contains("not a regex"), "{messages:?}");
}

#[test]
fn an_unknown_event_name_is_rejected() {
    let messages = errors(&file("[[on]]\nevent = 'sunrise'\nrun = 'x'\n"));
    assert!(messages[0].contains("sunrise"), "{messages:?}");
    let messages = errors(&file("[[status]]\nkey = 'k'\nrun = 'x'\non = ['midnight']\n"));
    assert!(messages[0].contains("midnight"), "{messages:?}");
}

#[test]
fn a_prompt_entry_has_exactly_one_source() {
    assert!(errors(&file("[[prompt]]\nheader = 'h'\n"))[0].contains("one of `text`, `files` or `run`"));
    assert!(errors(&file("[[prompt]]\ntext = 'a'\nrun = 'b'\n"))[0].contains("alternatives"));
    assert!(errors(&file("[[widget]]\nkey = 'k'\non = []\n"))[0].contains("one of `file` or `run`"));
    assert!(errors(&file("[[widget]]\nkey = 'k'\nfile = 'f'\nrun = 'r'\non = []\n"))[0].contains("alternatives"));
}

#[test]
fn a_tool_must_define_or_change_something() {
    assert!(errors(&file("[[tool]]\nname = 't'\n"))[0].contains("needs `run`, `loop`, `disabled` or `wrap`"));
    assert!(errors(&file("[[tool]]\nname = 't'\nparams.a = 1\nrun = 'x'\n"))[0].contains("`params.a` must be a table"));
}

#[test]
fn every_problem_in_a_file_is_reported_at_once() {
    let messages = errors("[[input]]\nmatch = '^('\n\n[[on]]\nevent = 'sunrise'\nrun = 'x'\n");
    assert_eq!(messages.len(), 3, "{messages:?}");
}

#[test]
fn a_slot_written_as_a_table_says_so() {
    let messages = errors(&file("[input]\nmatch = 'x'\n"));
    assert!(messages[0].contains("must be an array of tables"), "{messages:?}");
}

// ---------------------------------------------------------------------------
// Desugaring
// ---------------------------------------------------------------------------

#[test]
fn status_and_widget_become_on_entries_that_emit() {
    let policy = compose(vec![parse(DESIGN_EXAMPLES)]);
    assert_eq!(policy.status_keys, ["branch"]);
    assert_eq!(policy.widget_keys, ["todo"]);

    let status: Vec<&OnEntry> = policy
        .on
        .iter()
        .filter(|entry| entry.emit == Some(Emit::Status { key: "branch".to_owned() }))
        .collect();
    assert_eq!(status.len(), 2, "one per event in `on`");
    assert_eq!(status[0].event, OnEvent::Start);
    assert_eq!(status[1].event, OnEvent::TurnEnd);
    assert_eq!(status[0].source, OnSource::Run("git branch --show-current".to_owned()));
    assert_eq!(status[0].origin.slot, "status", "the origin stays the slot the author wrote");

    let widget: Vec<&OnEntry> = policy
        .on
        .iter()
        .filter(|entry| entry.emit == Some(Emit::Widget { key: "todo".to_owned() }))
        .collect();
    assert_eq!(widget.len(), 2);
    assert_eq!(widget[0].source, OnSource::File(PathBuf::from(".pirs/todo.md")));
    assert_eq!(widget[1].event, OnEvent::ToolResult);

    // The file's own `[[on]]` is still there, and still first.
    assert_eq!(policy.on[0].origin.slot, "on");
    assert!(policy.on[0].emit.is_none());
}

#[test]
fn a_command_becomes_a_handled_input_entry() {
    let policy = compose(vec![parse(DESIGN_EXAMPLES)]);
    let command = policy.input.last().expect("the desugared command");
    assert_eq!(command.pattern, r"^/handoff\b(.*)");
    assert!(command.handled);
    assert_eq!(command.run.as_deref(), Some("./scripts/handoff.sh $args"));
    assert_eq!(command.origin.slot, "command");
    assert_eq!(command.replace, None);
    assert!(command.regex.is_match("/handoff now"));
    assert!(!command.regex.is_match("/handoffnow"));
    assert_eq!(
        command.command,
        Some(CommandInfo { name: "handoff".to_owned(), description: "Start a fresh session with a summary".to_owned() })
    );
    assert_eq!(policy.commands, [CommandInfo { name: "handoff".to_owned(), description: "Start a fresh session with a summary".to_owned() }]);
}

#[test]
fn a_command_name_with_regex_characters_is_still_one_command() {
    let policy = compose(vec![parse(&file("[[command]]\nname = 'a.b'\ndescription = 'd'\nrun = 'x'\n"))]);
    assert!(policy.input[0].regex.is_match("/a.b"));
    assert!(!policy.input[0].regex.is_match("/axb"));
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// A home and a project directory, with `PIRS_HOME` pointing at the home.
struct Dirs {
    home: TempDir,
    project: TempDir,
}

impl Dirs {
    fn new() -> Self {
        Dirs { home: TempDir::new().expect("tempdir"), project: TempDir::new().expect("tempdir") }
    }

    fn global(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.home.path().join(".pirs").join("ext");
        std::fs::create_dir_all(&dir).expect("global ext");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        path
    }

    fn project(&self, name: &str, body: &str) -> PathBuf {
        let dir = self.project.path().join(".pirs").join("ext");
        std::fs::create_dir_all(&dir).expect("project ext");
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        path
    }

    fn load(&self) -> Policy {
        let project = self.project.path().to_path_buf();
        crate::session::tests_support::with_pirs_home(self.home.path(), || load(&project))
    }

    fn paths(&self) -> Vec<PathBuf> {
        let project = self.project.path().to_path_buf();
        crate::session::tests_support::with_pirs_home(self.home.path(), || policy_paths(&project))
    }
}

#[test]
fn files_are_global_first_then_project_each_sorted_by_path() {
    let dirs = Dirs::new();
    let b = dirs.global("b.pirs.toml", &file(""));
    let a = dirs.global("a.pirs.toml", &file(""));
    let z = dirs.project("z.pirs.toml", &file(""));
    let m = dirs.project("m.pirs.toml", &file(""));
    dirs.project("notes.md", "ignored");
    dirs.project("pirs.toml", "ignored: the suffix needs a name in front of it");
    assert_eq!(dirs.paths(), [a, b, m, z]);
}

#[test]
fn a_higher_priority_moves_a_file_earlier() {
    let dirs = Dirs::new();
    dirs.global("a.pirs.toml", "intent = 'global'\n[[prompt]]\ntext = 'global'\n");
    dirs.project("m.pirs.toml", "intent = 'project'\n[[prompt]]\ntext = 'project'\n");
    dirs.project("z.pirs.toml", "intent = 'loud'\npriority = 10\n[[prompt]]\ntext = 'loud'\n");
    let policy = dirs.load();
    let texts: Vec<String> = policy
        .prompt
        .iter()
        .map(|entry| match &entry.source {
            PromptSource::Text(text) => text.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(texts, ["loud", "global", "project"]);
    assert_eq!(policy.intents.iter().map(|(_, intent)| intent.as_str()).collect::<Vec<_>>(), ["loud", "global", "project"]);
}

#[test]
fn equal_priorities_keep_the_path_order() {
    let dirs = Dirs::new();
    dirs.global("a.pirs.toml", "intent = 'a'\npriority = 5\n");
    dirs.global("b.pirs.toml", "intent = 'b'\npriority = 5\n");
    dirs.project("m.pirs.toml", "intent = 'm'\npriority = 5\n");
    let policy = dirs.load();
    assert_eq!(policy.intents.iter().map(|(_, intent)| intent.as_str()).collect::<Vec<_>>(), ["a", "b", "m"]);
}

#[test]
fn a_loops_own_write_to_a_policy_file_is_recognised() {
    let dirs = Dirs::new();
    let global = dirs.global("a.pirs.toml", &file(""));
    let project = dirs.project("m.pirs.toml", &file(""));
    let cwd = dirs.project.path().to_path_buf();
    crate::session::tests_support::with_pirs_home(dirs.home.path(), || {
        assert!(is_policy_path(&cwd, &global));
        assert!(is_policy_path(&cwd, &project));
        // A file that does not exist yet still answers: the write is what
        // triggers the question (D-33).
        assert!(is_policy_path(&cwd, &cwd.join(".pirs").join("ext").join("new.pirs.toml")));
        assert!(!is_policy_path(&cwd, &cwd.join(".pirs").join("ext").join("new.toml")));
        assert!(!is_policy_path(&cwd, &cwd.join("src").join("new.pirs.toml")));
        assert!(!is_policy_path(&cwd, &cwd.join(".pirs").join("new.pirs.toml")));
    });
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

#[test]
fn a_parse_error_in_one_file_does_not_hide_the_others() {
    let dirs = Dirs::new();
    let bad = dirs.global("a.pirs.toml", "[[prompt]]\ntext = 'no intent here'\n");
    dirs.project("m.pirs.toml", "intent = 'fine'\n[[prompt]]\ntext = 'still loaded'\n");
    let policy = dirs.load();
    assert_eq!(policy.errors.len(), 1, "{:?}", policy.errors);
    assert_eq!(policy.errors[0].0, bad);
    assert!(policy.errors[0].1.contains("missing `intent`"));
    assert_eq!(policy.prompt.len(), 1, "the good file still loads");
    assert_eq!(policy.files.len(), 1);
}

#[test]
fn a_file_that_cannot_be_read_is_an_error_not_a_panic() {
    let dirs = Dirs::new();
    let path = dirs.project("m.pirs.toml", &file(""));
    std::fs::remove_file(&path).expect("remove");
    std::fs::create_dir(&path).expect("a directory where the file was");
    let policy = dirs.load();
    assert_eq!(policy.files.len(), 0, "a directory is not a policy file");
}

#[test]
fn duplicate_status_widget_and_command_names_conflict_and_name_both_files() {
    let dirs = Dirs::new();
    let a = dirs.global(
        "a.pirs.toml",
        "intent = 'a'\n[[status]]\nkey = 'branch'\nrun = 'x'\non = ['start']\n\
         [[widget]]\nkey = 'todo'\nrun = 'x'\non = ['start']\n\
         [[command]]\nname = 'handoff'\ndescription = 'd'\nrun = 'x'\n",
    );
    let m = dirs.project(
        "m.pirs.toml",
        "intent = 'm'\n[[status]]\nkey = 'branch'\nrun = 'y'\non = ['start']\n\
         [[widget]]\nkey = 'todo'\nrun = 'y'\non = ['start']\n\
         [[command]]\nname = 'handoff'\ndescription = 'd'\nrun = 'y'\n",
    );
    let policy = dirs.load();
    assert_eq!(policy.conflicts.len(), 3, "{:?}", policy.conflicts);
    for conflict in &policy.conflicts {
        assert_eq!(conflict.files, [server(&a), server(&m)], "both files: {conflict:?}");
    }
    let messages: Vec<&str> = policy.conflicts.iter().map(|c| c.message.as_str()).collect();
    assert!(messages[0].starts_with("duplicate command `/handoff`"), "{messages:?}");
    assert!(messages[1].starts_with("duplicate status key `branch`"), "{messages:?}");
    assert!(messages[2].starts_with("duplicate widget key `todo`"), "{messages:?}");
    // The first definition survives; the key is not listed twice.
    assert_eq!(policy.status_keys, ["branch"]);
    assert_eq!(policy.widget_keys, ["todo"]);
    assert_eq!(policy.commands.len(), 1);
}

#[test]
fn a_status_and_a_widget_may_share_a_key() {
    let dirs = Dirs::new();
    dirs.project(
        "m.pirs.toml",
        "intent = 'm'\n[[status]]\nkey = 'todo'\nrun = 'x'\non = ['start']\n\
         [[widget]]\nkey = 'todo'\nrun = 'y'\non = ['start']\n",
    );
    let policy = dirs.load();
    assert!(policy.conflicts.is_empty(), "{:?}", policy.conflicts);
}

#[test]
fn two_tools_with_one_name_conflict() {
    let dirs = Dirs::new();
    let a = dirs.global("a.pirs.toml", "intent = 'a'\n[[tool]]\nname = 'fetch'\nrun = './a.py'\n");
    let m = dirs.project("m.pirs.toml", "intent = 'm'\n[[tool]]\nname = 'fetch'\nrun = './m.py'\n");
    let policy = dirs.load();
    assert_eq!(policy.conflicts.len(), 1, "{:?}", policy.conflicts);
    assert!(policy.conflicts[0].message.starts_with("tool `fetch` is defined twice"));
    assert_eq!(policy.conflicts[0].files, [server(&a), server(&m)]);
    // A `loop` tool is a definition too.
    let dirs = Dirs::new();
    dirs.global("a.pirs.toml", "intent = 'a'\n[[tool]]\nname = 'fetch'\nrun = './a.py'\n");
    dirs.project(
        "m.pirs.toml",
        "intent = 'm'\n[[tool]]\nname = 'fetch'\nloop = { model = 'x', prompt = 'p', wait = 'idle' }\n",
    );
    assert_eq!(dirs.load().conflicts.len(), 1);
}

#[test]
fn disabling_or_wrapping_a_tool_another_file_defines_is_not_a_conflict() {
    for modifier in ["disabled = true", "wrap = './sandbox.sh'"] {
        let dirs = Dirs::new();
        dirs.global("a.pirs.toml", "intent = 'a'\n[[tool]]\nname = 'fetch'\ndescription = 'd'\nrun = './a.py'\n");
        dirs.project("m.pirs.toml", &format!("intent = 'm'\n[[tool]]\nname = 'fetch'\n{modifier}\n"));
        let policy = dirs.load();
        assert!(policy.conflicts.is_empty(), "{modifier}: {:?}", policy.conflicts);
        assert_eq!(policy.tools.len(), 1, "one resolved tool");
        assert_eq!(policy.tools[0].source, ToolSource::Run("./a.py".to_owned()));
        assert_eq!(policy.tools[0].description, "d");
        assert_eq!(policy.tools[0].disabled, modifier.starts_with("disabled"));
        assert_eq!(policy.tools[0].wrap.is_some(), modifier.starts_with("wrap"));
    }
}

#[test]
fn two_files_may_disable_the_same_tool() {
    let dirs = Dirs::new();
    dirs.global("a.pirs.toml", "intent = 'a'\n[[tool]]\nname = 'bash'\ndisabled = true\n");
    dirs.project("m.pirs.toml", "intent = 'm'\n[[tool]]\nname = 'bash'\ndisabled = true\n");
    let policy = dirs.load();
    assert!(policy.conflicts.is_empty(), "{:?}", policy.conflicts);
    assert_eq!(policy.tools.len(), 1);
    assert!(policy.tools[0].disabled);
}

#[test]
fn two_files_may_not_wrap_the_same_tool() {
    let dirs = Dirs::new();
    let a = dirs.global("a.pirs.toml", "intent = 'a'\n[[tool]]\nname = 'bash'\nwrap = './a.sh'\n");
    let m = dirs.project("m.pirs.toml", "intent = 'm'\n[[tool]]\nname = 'bash'\nwrap = './m.sh'\n");
    let policy = dirs.load();
    assert_eq!(policy.conflicts.len(), 1, "{:?}", policy.conflicts);
    assert!(policy.conflicts[0].message.starts_with("tool `bash` is wrapped twice"));
    assert_eq!(policy.conflicts[0].files, [server(&a), server(&m)]);
}

#[test]
fn a_built_in_is_an_implicit_definition_so_disabling_one_stands_alone() {
    let dirs = Dirs::new();
    dirs.project("m.pirs.toml", "intent = 'm'\n[[tool]]\nname = 'bash'\ndisabled = true\n");
    let policy = dirs.load();
    assert!(policy.conflicts.is_empty(), "{:?}", policy.conflicts);
    assert!(policy.errors.is_empty(), "{:?}", policy.errors);
    assert_eq!(policy.tools[0].source, ToolSource::Builtin);
    assert!(policy.tools[0].disabled);
}

#[test]
fn a_tool_nobody_defines_is_an_error_against_the_file_that_names_it() {
    let dirs = Dirs::new();
    let m = dirs.project("m.pirs.toml", "intent = 'm'\n[[tool]]\nname = 'fetch'\ndisabled = true\n");
    let policy = dirs.load();
    assert_eq!(policy.errors.len(), 1, "{:?}", policy.errors);
    assert_eq!(policy.errors[0].0, m);
    assert!(policy.errors[0].1.contains("no `run` or `loop` and is not a built-in"));
    assert!(policy.tools.is_empty());
}

#[test]
fn every_other_slot_simply_unions_in_file_order() {
    let dirs = Dirs::new();
    dirs.global(
        "a.pirs.toml",
        "intent = 'a'\n[[input]]\nmatch = 'a'\n[[prompt]]\ntext = 'a'\n\
         [[tool_result]]\ntool = 'bash'\nrun = 'a'\n[[on]]\nevent = 'start'\nrun = 'a'\n",
    );
    dirs.project(
        "m.pirs.toml",
        "intent = 'm'\n[[input]]\nmatch = 'm'\n[[prompt]]\ntext = 'm'\n\
         [[tool_result]]\ntool = 'bash'\nrun = 'm'\n[[on]]\nevent = 'start'\nrun = 'm'\n",
    );
    let policy = dirs.load();
    assert_eq!(policy.input.iter().map(|e| e.pattern.as_str()).collect::<Vec<_>>(), ["a", "m"]);
    assert_eq!(policy.tool_result.iter().map(|e| e.run.as_str()).collect::<Vec<_>>(), ["a", "m"]);
    assert_eq!(policy.on.len(), 2);
    assert!(policy.conflicts.is_empty(), "several tool_results for one tool are not a conflict");
}

// ---------------------------------------------------------------------------
// Tools and the manifest
// ---------------------------------------------------------------------------

#[test]
fn params_become_a_json_schema_where_a_default_means_optional() {
    let policy = compose(vec![parse(DESIGN_EXAMPLES)]);
    let fetch = policy.tools.iter().find(|tool| tool.name == "fetch").expect("fetch");
    assert_eq!(
        fetch.parameters,
        json!({
            "type": "object",
            "properties": {
                "max_length": {"type": "integer", "default": 50000},
                "url": {"type": "string", "description": "Absolute http(s) URL"},
            },
            "required": ["url"],
        })
    );
    assert_eq!(fetch.timeout, Duration::from_secs(30));

    let review = policy.tools.iter().find(|tool| tool.name == "review").expect("review");
    assert_eq!(review.parameters, json!({"type": "object", "properties": {}, "required": []}));
    assert!(!review.declares_params());
    assert!(matches!(review.source, ToolSource::Loop(_)));
}

#[test]
fn the_manifest_is_the_built_ins_after_the_policy_has_had_its_say() {
    let policy = compose(vec![parse(DESIGN_EXAMPLES)]);
    let builtins = [
        ToolInfo { name: "read".to_owned(), description: "read a file".to_owned(), parameters: json!({"type": "object"}) },
        ToolInfo { name: "bash".to_owned(), description: "run a command".to_owned(), parameters: json!({"type": "object"}) },
    ];
    let manifest = manifest(&policy, &builtins);
    let names: Vec<&str> = manifest.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["read", "fetch", "review"], "bash is disabled, the policy tools follow the built-ins");
    assert_eq!(manifest.tools[1].description, "Fetch a URL and return its text");
    assert_eq!(manifest.tools[1].parameters["required"], json!(["url"]));
    assert_eq!(manifest.commands.len(), 1);
    assert_eq!(manifest.status_keys, ["branch"]);
    assert_eq!(manifest.widget_keys, ["todo"]);
}

#[test]
fn a_wrapped_built_in_keeps_its_declaration() {
    let policy = compose(vec![parse(&file("[[tool]]\nname = 'bash'\nwrap = './sandboxed-bash.sh'\n"))]);
    let builtins = [ToolInfo {
        name: "bash".to_owned(),
        description: "run a command".to_owned(),
        parameters: json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
    }];
    let manifest = manifest(&policy, &builtins);
    assert_eq!(manifest.tools, builtins, "the model sees the same tool; only the server routes it differently");
    assert_eq!(policy.tools[0].wrap.as_deref(), Some("./sandboxed-bash.sh"));
}

// ---------------------------------------------------------------------------
// `[settings]`
// ---------------------------------------------------------------------------

#[test]
fn the_last_file_to_set_a_settings_key_wins_and_is_remembered() {
    let dirs = Dirs::new();
    let a = dirs.global("a.pirs.toml", "intent = 'a'\n[settings]\nmodel = 'anthropic/one'\nthinking = 'low'\n");
    let m = dirs.project("m.pirs.toml", "intent = 'm'\n[settings]\nmodel = 'faux/scripted'\ntools = ['read', 'bash']\n");
    let policy = dirs.load();
    assert_eq!(policy.settings.model.as_deref(), Some("faux/scripted"));
    assert_eq!(policy.settings.thinking.as_deref(), Some("low"), "a key the project file leaves alone");
    assert_eq!(policy.settings.tools.as_deref(), Some(["read".to_owned(), "bash".to_owned()].as_slice()));
    assert_eq!(policy.settings_sources, [("thinking", a), ("model", m.clone()), ("tools", m)]);
}

#[test]
fn tool_execution_is_a_settings_key_and_must_name_one_of_the_two_modes() {
    let dirs = Dirs::new();
    dirs.project("a.pirs.toml", "intent = 'a'\n[settings]\ntool_execution = 'sequential'\n");
    assert_eq!(dirs.load().settings.tool_execution, Some(crate::dsl::ToolExecution::Sequential));

    let dirs = Dirs::new();
    dirs.project("b.pirs.toml", "intent = 'b'\n[settings]\ntool_execution = 'whenever'\n");
    let policy = dirs.load();
    assert!(policy.errors.iter().any(|(_, m)| m.contains("tool_execution")), "{:?}", policy.errors);
}

#[test]
fn the_settings_keys_are_model_thinking_tools_and_tool_execution() {
    assert_eq!(SettingsTable::KEYS, ["model", "thinking", "tools", "tool_execution"]);
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[test]
fn render_shows_every_slot_with_the_file_it_came_from() {
    let policy = compose(vec![parse(DESIGN_EXAMPLES)]);
    let text = render(&policy);
    for expected in [
        "/p/a.pirs.toml",
        "Show the current git branch in the status line",
        r"^\?(.*) -> replace",
        "text \"Prefer small commits. Never rewrite history.\"",
        "header \"Recent commits:\"",
        "bash -> run \"./tools/trim-test-output.sh\"",
        "turn_end run \"git add -A",
        "status \"branch\"",
        "widget \"todo\"",
        "fetch  run \"./tools/fetch.py\", params max_length, url, timeout 30s  (/p/a.pirs.toml: [[tool]] #1)",
        "bash  built-in, disabled, timeout 60s  (/p/a.pirs.toml: [[tool]] #3)",
        "/handoff  Start a fresh session with a summary",
        "status keys: branch",
        "widget keys: todo",
        "(/p/a.pirs.toml: [[command]] #1)",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}

#[test]
fn render_says_so_when_there_is_nothing() {
    assert_eq!(render(&Policy::default()), "files: none\n");
}

#[test]
fn render_prints_errors_and_conflicts_with_their_files() {
    let dirs = Dirs::new();
    let a = dirs.global("a.pirs.toml", "intent = 'a'\n[[status]]\nkey = 'k'\nrun = 'x'\non = ['start']\n");
    let m = dirs.project("m.pirs.toml", "intent = 'm'\n[[status]]\nkey = 'k'\nrun = 'y'\non = ['start']\n");
    dirs.project("z.pirs.toml", "[[prompt]]\ntext = 'no intent'\n");
    let text = render(&dirs.load());
    assert!(text.contains("error: ") && text.contains("missing `intent`"), "{text}");
    assert!(
        text.contains("conflict: duplicate status key `k`") && text.contains(&format!("({}, {})", a.display(), m.display())),
        "{text}"
    );
}

#[test]
fn errors_travel_as_conflicts_because_the_protocol_has_one_list() {
    let dirs = Dirs::new();
    dirs.project("z.pirs.toml", "[[prompt]]\ntext = 'no intent'\n");
    let policy = dirs.load();
    let conflicts = check_conflicts(&policy);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].files.len(), 1, "an error names one file");
    assert!(check_files(&policy).is_empty());
}

fn server(path: &Path) -> pirs_protocol::ServerPath {
    pirs_protocol::ServerPath::from(path.to_string_lossy().into_owned())
}
