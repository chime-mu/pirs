//! Parsing one `*.pirs.toml` into a [`PolicyFile`].
//!
//! The file is walked table by table rather than deserialised in one go, so
//! every message can name the slot and the entry it came from: serde's
//! `unknown field` on its own would only give a line number. Every problem
//! found is collected, so one `pirs check` shows all of them.
//!
//! Messages here do not name the file: the caller pairs them with its path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use pirs_protocol::{CommandInfo, OnEvent};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use super::{
    CommandEntry, Emit, InputEntry, LoopSpec, OnEntry, OnSource, Origin, PolicyFile, PromptEntry,
    PromptSource, SettingsTable, StatusEntry, ToolEntry, ToolResultEntry, WidgetEntry,
    WidgetSource,
};

/// Every key a policy file may have at the top level.
const TOP_LEVEL: &[&str] = &[
    "intent",
    "priority",
    "settings",
    "input",
    "prompt",
    "tool_result",
    "status",
    "widget",
    "on",
    "command",
    "tool",
];

// ---------------------------------------------------------------------------
// The wire shapes: exactly the fields `40-dsl.md` lists, and no others
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    r#match: String,
    replace: Option<String>,
    #[serde(default)]
    handled: bool,
    run: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrompt {
    text: Option<String>,
    files: Option<String>,
    run: Option<String>,
    header: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolResult {
    tool: String,
    run: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStatus {
    key: String,
    run: String,
    on: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWidget {
    key: String,
    file: Option<String>,
    run: Option<String>,
    on: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOn {
    event: String,
    run: String,
    #[serde(default)]
    quiet: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    name: String,
    description: String,
    run: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTool {
    name: String,
    description: Option<String>,
    params: Option<BTreeMap<String, Value>>,
    run: Option<String>,
    timeout: Option<u64>,
    r#loop: Option<RawLoop>,
    #[serde(default)]
    disabled: bool,
    wrap: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLoop {
    model: Option<String>,
    prompt: String,
    wait: String,
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

/// Parse `text` as the policy file at `path`.
pub(super) fn parse_file(path: &Path, text: &str) -> Result<PolicyFile, Vec<String>> {
    let doc: toml::Table = match text.parse() {
        Ok(doc) => doc,
        Err(error) => return Err(vec![format!("not TOML: {error}")]),
    };
    let mut errors = Vec::new();

    for key in doc.keys() {
        if !TOP_LEVEL.contains(&key.as_str()) {
            errors.push(format!("unknown key `{key}`: expected one of {}", TOP_LEVEL.join(", ")));
        }
    }

    let intent = match doc.get("intent") {
        Some(toml::Value::String(intent)) => intent.trim().to_owned(),
        Some(_) => {
            errors.push("`intent` must be a string".to_owned());
            String::new()
        }
        None => {
            errors.push("missing `intent`: every policy file says what it is for".to_owned());
            String::new()
        }
    };

    let priority = match doc.get("priority") {
        Some(toml::Value::Integer(priority)) => *priority,
        Some(_) => {
            errors.push("`priority` must be an integer".to_owned());
            0
        }
        None => 0,
    };

    let settings = match doc.get("settings") {
        Some(value @ toml::Value::Table(_)) => match value.clone().try_into::<SettingsTable>() {
            Ok(settings) => Some(settings),
            Err(error) => {
                errors.push(format!("[settings]: {}", one_line(&error.to_string())));
                None
            }
        },
        Some(_) => {
            errors.push("`settings` must be a table".to_owned());
            None
        }
        None => None,
    };

    let mut file = PolicyFile {
        path: path.to_path_buf(),
        intent,
        priority,
        settings,
        input: Vec::new(),
        prompt: Vec::new(),
        tool_result: Vec::new(),
        status: Vec::new(),
        widget: Vec::new(),
        on: Vec::new(),
        command: Vec::new(),
        tool: Vec::new(),
    };

    for (index, raw) in entries::<RawInput>(&doc, "input", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "input", index };
        match Regex::new(&raw.r#match) {
            Ok(regex) => file.input.push(InputEntry {
                origin,
                pattern: raw.r#match,
                regex,
                replace: raw.replace,
                handled: raw.handled,
                run: raw.run,
                command: None,
            }),
            Err(error) => errors.push(format!("{origin}: `match` is not a regex: {}", one_line(&error.to_string()))),
        }
    }

    for (index, raw) in entries::<RawPrompt>(&doc, "prompt", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "prompt", index };
        let sources: Vec<PromptSource> = [
            raw.text.map(PromptSource::Text),
            raw.files.map(PromptSource::Files),
            raw.run.map(PromptSource::Run),
        ]
        .into_iter()
        .flatten()
        .collect();
        match <[PromptSource; 1]>::try_from(sources) {
            Ok([source]) => file.prompt.push(PromptEntry { origin, source, header: raw.header }),
            Err(sources) => errors.push(format!(
                "{origin}: {}",
                if sources.is_empty() {
                    "one of `text`, `files` or `run` is required".to_owned()
                } else {
                    format!("`text`, `files` and `run` are alternatives, not {} at once", sources.len())
                }
            )),
        }
    }

    for (index, raw) in entries::<RawToolResult>(&doc, "tool_result", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "tool_result", index };
        file.tool_result.push(ToolResultEntry { origin, tool: raw.tool, run: raw.run });
    }

    for (index, raw) in entries::<RawStatus>(&doc, "status", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "status", index };
        if let Some(on) = events(&raw.on, &origin, &mut errors) {
            file.status.push(StatusEntry { origin, key: raw.key, run: raw.run, on });
        }
    }

    for (index, raw) in entries::<RawWidget>(&doc, "widget", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "widget", index };
        let source = match (raw.file, raw.run) {
            (Some(file), None) => Some(WidgetSource::File(PathBuf::from(file))),
            (None, Some(run)) => Some(WidgetSource::Run(run)),
            (None, None) => {
                errors.push(format!("{origin}: one of `file` or `run` is required"));
                None
            }
            (Some(_), Some(_)) => {
                errors.push(format!("{origin}: `file` and `run` are alternatives, not both"));
                None
            }
        };
        let on = events(&raw.on, &origin, &mut errors);
        if let (Some(source), Some(on)) = (source, on) {
            file.widget.push(WidgetEntry { origin, key: raw.key, source, on });
        }
    }

    for (index, raw) in entries::<RawOn>(&doc, "on", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "on", index };
        match OnEvent::from_str(&raw.event) {
            Ok(event) => file.on.push(OnEntry {
                origin,
                event,
                source: OnSource::Run(raw.run),
                quiet: raw.quiet,
                emit: None,
            }),
            Err(error) => errors.push(format!("{origin}: {error}")),
        }
    }

    for (index, raw) in entries::<RawCommand>(&doc, "command", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "command", index };
        file.command.push(CommandEntry { origin, name: raw.name, description: raw.description, run: raw.run });
    }

    for (index, raw) in entries::<RawTool>(&doc, "tool", &mut errors) {
        let origin = Origin { file: path.to_path_buf(), slot: "tool", index };
        let mut bad = false;
        for (name, spec) in raw.params.iter().flatten() {
            if !spec.is_object() {
                errors.push(format!("{origin}: `params.{name}` must be a table of JSON Schema keys"));
                bad = true;
            }
        }
        if raw.run.is_some() && raw.r#loop.is_some() {
            errors.push(format!("{origin}: `run` and `loop` are alternatives, not both"));
            bad = true;
        }
        // `timeout` is how long the server waits for what it starts: a
        // `run`, the `wrap` a built-in's call is routed through, or the
        // second loop a `loop` asks. With none of them there is nothing to
        // bound, and saying `timeout` there reads as a promise the server
        // cannot keep.
        if raw.timeout.is_some() && raw.run.is_none() && raw.wrap.is_none() && raw.r#loop.is_none() {
            errors.push(format!("{origin}: `timeout` needs `run`; a declared tool uses its registrant's timeout"));
            bad = true;
        }
        // `wait = "idle"` is the whole vocabulary: the call returns when the
        // second loop goes idle. Nothing else is implemented and nothing
        // else is planned, so a typo is a parse error, not a surprise at
        // call time.
        if let Some(spec) = &raw.r#loop {
            if spec.wait != "idle" {
                errors.push(format!("{origin}: `loop.wait` is {:?}; the only value is \"idle\"", spec.wait));
                bad = true;
            }
            if spec.prompt.trim().is_empty() {
                errors.push(format!("{origin}: `loop.prompt` is empty; it is what the second loop is asked"));
                bad = true;
            }
        }
        // `params` with neither `run` nor `loop` declares a tool a connected
        // handler serves (D-23); `params = {}` declares one with no arguments.
        let declared = raw.params.is_some() && raw.run.is_none() && raw.r#loop.is_none();
        if !declared && raw.run.is_none() && raw.r#loop.is_none() && !raw.disabled && raw.wrap.is_none() {
            errors.push(format!("{origin}: a tool needs `run`, `loop`, `params`, `disabled` or `wrap`"));
            bad = true;
        }
        if bad {
            continue;
        }
        file.tool.push(ToolEntry {
            origin,
            name: raw.name,
            description: raw.description,
            params: raw.params.unwrap_or_default(),
            declared,
            run: raw.run,
            timeout: raw.timeout.map(Duration::from_secs),
            loop_spec: raw.r#loop.map(|l| LoopSpec {
                model: l.model.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()),
                prompt: l.prompt,
                wait: l.wait,
            }),
            disabled: raw.disabled,
            wrap: raw.wrap,
        });
    }

    if errors.is_empty() {
        Ok(file)
    } else {
        Err(errors)
    }
}

/// Every element of one array-of-tables slot, deserialised; an element that
/// does not fit becomes an error and is skipped.
fn entries<T: for<'de> Deserialize<'de>>(doc: &toml::Table, slot: &'static str, errors: &mut Vec<String>) -> Vec<(usize, T)> {
    let Some(value) = doc.get(slot) else {
        return Vec::new();
    };
    let Some(array) = value.as_array() else {
        errors.push(format!("`{slot}` must be an array of tables, written `[[{slot}]]`"));
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, element) in array.iter().enumerate() {
        match element.clone().try_into::<T>() {
            Ok(entry) => out.push((index, entry)),
            Err(error) => errors.push(format!("[[{slot}]] #{}: {}", index + 1, one_line(&error.to_string()))),
        }
    }
    out
}

/// The `on = [...]` list of a `[[status]]` or `[[widget]]`.
fn events(names: &[String], origin: &Origin, errors: &mut Vec<String>) -> Option<Vec<OnEvent>> {
    let mut events = Vec::new();
    for name in names {
        match OnEvent::from_str(name) {
            Ok(event) => events.push(event),
            Err(error) => {
                errors.push(format!("{origin}: {error}"));
                return None;
            }
        }
    }
    Some(events)
}

/// A serde or regex message as one line, so an error list stays a list.
fn one_line(message: &str) -> String {
    message.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The `[[command]]` desugaring (D-18), shared with the composer.
pub(super) fn command_as_input(entry: &CommandEntry) -> Result<InputEntry, String> {
    let pattern = format!(r"^/{}\b(.*)", regex::escape(&entry.name));
    let regex = Regex::new(&pattern).map_err(|error| format!("{}: /{}: {error}", entry.origin, entry.name))?;
    Ok(InputEntry {
        origin: entry.origin.clone(),
        pattern,
        regex,
        replace: None,
        handled: true,
        run: Some(entry.run.clone()),
        command: Some(CommandInfo { name: entry.name.clone(), description: entry.description.clone() }),
    })
}

/// The `[[status]]` desugaring (D-17): one `on` entry per event.
pub(super) fn status_as_on(entry: &StatusEntry) -> Vec<OnEntry> {
    entry
        .on
        .iter()
        .map(|event| OnEntry {
            origin: entry.origin.clone(),
            event: *event,
            source: OnSource::Run(entry.run.clone()),
            quiet: false,
            emit: Some(Emit::Status { key: entry.key.clone() }),
        })
        .collect()
}

/// The `[[widget]]` desugaring (D-17): one `on` entry per event.
pub(super) fn widget_as_on(entry: &WidgetEntry) -> Vec<OnEntry> {
    entry
        .on
        .iter()
        .map(|event| OnEntry {
            origin: entry.origin.clone(),
            event: *event,
            source: match &entry.source {
                WidgetSource::File(path) => OnSource::File(path.clone()),
                WidgetSource::Run(run) => OnSource::Run(run.clone()),
            },
            quiet: false,
            emit: Some(Emit::Widget { key: entry.key.clone() }),
        })
        .collect()
}
