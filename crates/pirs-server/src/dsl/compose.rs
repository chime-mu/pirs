//! Composition: order the files, desugar, union the slots, check.
//!
//! The order is the one `40-dsl.md` fixes — global before project, path
//! sorted, `priority = N` moves a file earlier — and every slot is unioned in
//! it. The checks are that table's four rows; each conflict names both files
//! so the reader knows which two to look at.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use pirs_protocol::{DslConflict, ServerPath};
use serde_json::{Map, Value};

use super::parse::{command_as_input, status_as_on, widget_as_on};
use super::{
    Origin, Policy, PolicyFile, ResolvedTool, SettingsTable, ToolEntry, ToolSource,
    DEFAULT_TOOL_TIMEOUT,
};

/// Merge parsed files into one policy.
pub(super) fn compose(mut files: Vec<PolicyFile>) -> Policy {
    // The caller supplies global-before-project, path-sorted order; a higher
    // `priority` moves a file earlier and a stable sort keeps the rest.
    files.sort_by_key(|file| std::cmp::Reverse(file.priority));

    let mut policy = Policy::default();
    let mut status_seen: Vec<(String, PathBuf)> = Vec::new();
    let mut widget_seen: Vec<(String, PathBuf)> = Vec::new();
    let mut command_seen: Vec<(String, PathBuf)> = Vec::new();
    // Tool entries by name, first-seen order kept so the result is stable.
    let mut tool_order: Vec<String> = Vec::new();
    let mut tool_groups: BTreeMap<String, Vec<ToolEntry>> = BTreeMap::new();

    for file in &files {
        policy.files.push(file.path.clone());
        policy.intents.push((file.path.clone(), file.intent.clone()));

        policy.input.extend(file.input.iter().cloned());
        for entry in &file.command {
            match duplicate(&mut command_seen, &entry.name, &entry.origin) {
                Some(conflict) => policy.conflicts.push(conflict_of(format!("duplicate command `/{}`", entry.name), conflict, &entry.origin)),
                None => match command_as_input(entry) {
                    Ok(input) => {
                        policy.commands.push(input.command.clone().expect("a desugared command carries its info"));
                        policy.input.push(input);
                    }
                    Err(message) => policy.errors.push((file.path.clone(), message)),
                },
            }
        }

        policy.prompt.extend(file.prompt.iter().cloned());
        policy.tool_result.extend(file.tool_result.iter().cloned());
        policy.on.extend(file.on.iter().cloned());

        for entry in &file.status {
            match duplicate(&mut status_seen, &entry.key, &entry.origin) {
                Some(first) => policy.conflicts.push(conflict_of(format!("duplicate status key `{}`", entry.key), first, &entry.origin)),
                None => {
                    policy.status_keys.push(entry.key.clone());
                    policy.on.extend(status_as_on(entry));
                }
            }
        }
        for entry in &file.widget {
            match duplicate(&mut widget_seen, &entry.key, &entry.origin) {
                Some(first) => policy.conflicts.push(conflict_of(format!("duplicate widget key `{}`", entry.key), first, &entry.origin)),
                None => {
                    policy.widget_keys.push(entry.key.clone());
                    policy.on.extend(widget_as_on(entry));
                }
            }
        }

        for entry in &file.tool {
            if !tool_groups.contains_key(&entry.name) {
                tool_order.push(entry.name.clone());
            }
            tool_groups.entry(entry.name.clone()).or_default().push(entry.clone());
        }

        if let Some(settings) = &file.settings {
            merge_settings(&mut policy, settings, &file.path);
        }
    }

    for name in tool_order {
        let group = tool_groups.remove(&name).unwrap_or_default();
        resolve_tool(&mut policy, &name, &group);
    }

    policy
}

/// Remember `key`, or report the file that already had it.
fn duplicate(seen: &mut Vec<(String, PathBuf)>, key: &str, origin: &Origin) -> Option<PathBuf> {
    match seen.iter().find(|(had, _)| had == key) {
        Some((_, first)) => Some(first.clone()),
        None => {
            seen.push((key.to_owned(), origin.file.clone()));
            None
        }
    }
}

/// A conflict naming both files, the first one first.
fn conflict_of(message: String, first: PathBuf, second: &Origin) -> DslConflict {
    DslConflict {
        message: format!("{message} ({second})"),
        files: vec![server_path(&first), server_path(&second.file)],
    }
}

fn server_path(path: &std::path::Path) -> ServerPath {
    ServerPath::from(path.to_string_lossy().into_owned())
}

/// Later file wins, key by key, and we remember which one that was.
fn merge_settings(policy: &mut Policy, settings: &SettingsTable, path: &std::path::Path) {
    let mut set = |key: &'static str| {
        policy.settings_sources.retain(|(had, _)| *had != key);
        policy.settings_sources.push((key, path.to_path_buf()));
    };
    if let Some(model) = &settings.model {
        policy.settings.model = Some(model.clone());
        set("model");
    }
    if let Some(thinking) = &settings.thinking {
        policy.settings.thinking = Some(thinking.clone());
        set("thinking");
    }
    if let Some(tools) = &settings.tools {
        policy.settings.tools = Some(tools.clone());
        set("tools");
    }
    if let Some(mode) = settings.tool_execution {
        policy.settings.tool_execution = Some(mode);
        set("tool_execution");
    }
}

/// Apply every `[[tool]]` entry for one name.
///
/// A *base* entry (`run`, `loop`, or a `params` declaration served by a
/// connected handler) defines the tool; a modifier only `disabled`s or
/// `wrap`s one. Two bases are the conflict `40-dsl.md` names;
/// a base plus any number of modifiers is one resolved tool, and so is a
/// modifier alone when the name is a built-in.
fn resolve_tool(policy: &mut Policy, name: &str, group: &[ToolEntry]) {
    let bases: Vec<&ToolEntry> = group.iter().filter(|entry| entry.is_base()).collect();
    for extra in bases.iter().skip(1) {
        policy.conflicts.push(conflict_of(
            format!("tool `{name}` is defined twice"),
            bases[0].origin.file.clone(),
            &extra.origin,
        ));
    }
    let wraps: Vec<&ToolEntry> = group.iter().filter(|entry| entry.wrap.is_some()).collect();
    for extra in wraps.iter().skip(1) {
        policy.conflicts.push(conflict_of(
            format!("tool `{name}` is wrapped twice"),
            wraps[0].origin.file.clone(),
            &extra.origin,
        ));
    }

    let base = bases.first().copied();
    let builtin = crate::tools::ALL_TOOL_NAMES.contains(&name);
    let source = match base {
        Some(entry) => match (&entry.run, &entry.loop_spec) {
            (Some(run), _) => ToolSource::Run(run.clone()),
            (None, Some(spec)) => ToolSource::Loop(spec.clone()),
            // A declaration hands the name to a connected registrant, and a
            // built-in's name is not the file's to give away: say so and
            // leave the built-in as it was (D-22).
            (None, None) if builtin => {
                policy.conflicts.push(DslConflict {
                    message: format!(
                        "built-in `{name}` declared as a handler tool; use `wrap` or `disabled` ({})",
                        entry.origin
                    ),
                    files: vec![server_path(&entry.origin.file)],
                });
                ToolSource::Builtin
            }
            (None, None) => ToolSource::Handler,
        },
        None if builtin => ToolSource::Builtin,
        None => {
            let origin = &group[0].origin;
            policy.errors.push((
                origin.file.clone(),
                format!("{origin}: tool `{name}` has no `run` or `loop` and is not a built-in"),
            ));
            return;
        }
    };

    let described = base
        .filter(|entry| entry.description.is_some())
        .or_else(|| group.iter().find(|entry| entry.description.is_some()));
    let params = base.map(|entry| &entry.params).filter(|params| !params.is_empty()).or_else(|| {
        group.iter().map(|entry| &entry.params).find(|params| !params.is_empty())
    });

    policy.tools.push(ResolvedTool {
        name: name.to_owned(),
        description: described.and_then(|entry| entry.description.clone()).unwrap_or_default(),
        parameters: schema(params.cloned().unwrap_or_default()),
        source,
        disabled: group.iter().any(|entry| entry.disabled),
        wrap: wraps.first().and_then(|entry| entry.wrap.clone()),
        timeout: timeout(base, group),
        origins: group.iter().map(|entry| entry.origin.clone()).collect(),
    });
}

/// The first explicit `timeout` in the group, preferring the base entry.
fn timeout(base: Option<&ToolEntry>, group: &[ToolEntry]) -> Duration {
    if let Some(entry) = base {
        if entry.timeout != DEFAULT_TOOL_TIMEOUT {
            return entry.timeout;
        }
    }
    group
        .iter()
        .map(|entry| entry.timeout)
        .find(|timeout| *timeout != DEFAULT_TOOL_TIMEOUT)
        .unwrap_or(DEFAULT_TOOL_TIMEOUT)
}

/// The JSON Schema object a `params` table describes: a param without a
/// `default` is required.
fn schema(params: BTreeMap<String, Value>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (name, spec) in params {
        if !spec.as_object().is_some_and(|spec| spec.contains_key("default")) {
            required.push(Value::String(name.clone()));
        }
        properties.insert(name, spec);
    }
    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), Value::Array(required));
    Value::Object(schema)
}
