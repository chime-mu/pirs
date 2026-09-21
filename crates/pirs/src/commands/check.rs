//! `pirs check`: what a loop in this directory would start with.
//!
//! The merged policy, every conflict naming its files, and the fully
//! assembled system prompt — printed without starting a loop, so a person can
//! see exactly what a `[[prompt]]` injects before the model does (D-21).
//!
//! The checking happens on the server (`dsl.check`), where the policy files
//! are: this stays a thin client and works against a server in a container
//! whose files the client cannot see.

use anyhow::Result;
use pirs_protocol::{DslCheckResult, ServerPath};

use crate::cli::GlobalArgs;
use crate::connect::{connect_to, resolve_cwd_on, resolve_server};

/// Print the merged policy and exit 1 if anything conflicts.
pub(crate) async fn run(global: &GlobalArgs) -> Result<i32> {
    let config = resolve_server(global.server.as_deref(), global.socket.as_deref())?;
    let cwd = resolve_cwd_on(&config, global.cwd.as_deref())?;
    let client = connect_to(&config, !global.no_start).await?;
    let result = client.dsl_check(ServerPath::from(cwd)).await?;
    print!("{}", format_check(&result));
    Ok(if result.conflicts.is_empty() { 0 } else { 1 })
}

/// The whole report: files, manifest, the server's rendering of the merged
/// policy (D-40), conflicts, then the system prompt verbatim under a line
/// that separates it from everything above.
fn format_check(result: &DslCheckResult) -> String {
    let mut out = String::new();

    if result.files.is_empty() {
        out.push_str("files: none\n");
    } else {
        out.push_str("files:\n");
        for file in &result.files {
            out.push_str(&format!("  {file}\n"));
        }
    }

    let manifest = &result.manifest;
    out.push_str(&format!("tools: {}\n", list(manifest.tools.iter().map(|tool| tool.name.clone()))));
    out.push_str(&format!(
        "commands: {}\n",
        list(manifest.commands.iter().map(|command| format!("/{}", command.name)))
    ));
    out.push_str(&format!("status keys: {}\n", list(manifest.status_keys.iter().cloned())));
    out.push_str(&format!("widget keys: {}\n", list(manifest.widget_keys.iter().cloned())));

    out.push_str("--- policy ---\n");
    out.push_str(&result.rendered);
    if !result.rendered.is_empty() && !result.rendered.ends_with('\n') {
        out.push('\n');
    }

    for conflict in &result.conflicts {
        let files: Vec<&str> = conflict.files.iter().map(ServerPath::as_str).collect();
        out.push_str(&format!("conflict: {}  ({})\n", conflict.message, files.join(", ")));
    }

    out.push_str("--- system prompt ---\n");
    out.push_str(&result.system_prompt);
    if !result.system_prompt.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Comma-separated, or `none` for an empty list, so every line has a value.
fn list(items: impl Iterator<Item = String>) -> String {
    let items: Vec<String> = items.collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pirs_protocol::{CommandInfo, DslConflict, Manifest, ToolInfo};
    use serde_json::json;

    fn result() -> DslCheckResult {
        DslCheckResult {
            files: vec![ServerPath::from("/home/u/.pirs/ext/git.pirs.toml"), ServerPath::from("/p/.pirs/ext/a.pirs.toml")],
            manifest: Manifest {
                tools: vec![ToolInfo {
                    name: "fetch".to_owned(),
                    description: "Fetch a URL".to_owned(),
                    parameters: json!({"type": "object"}),
                }],
                commands: vec![CommandInfo { name: "handoff".to_owned(), description: "Start fresh".to_owned() }],
                status_keys: vec!["branch".to_owned()],
                widget_keys: Vec::new(),
            },
            conflicts: vec![DslConflict {
                message: "duplicate status key `branch`".to_owned(),
                files: vec![ServerPath::from("/a.pirs.toml"), ServerPath::from("/b.pirs.toml")],
            }],
            rendered: "files:\n  /p/.pirs/ext/a.pirs.toml\ninput:\n  ^\\?(.*) -> replace \"Explain\"  (a.pirs.toml [[input]] #1)\n"
                .to_owned(),
            system_prompt: "You are pirs.".to_owned(),
        }
    }

    #[test]
    fn the_report_is_files_then_manifest_then_conflicts_then_the_prompt() {
        assert_eq!(
            format_check(&result()),
            "files:\n  \
             /home/u/.pirs/ext/git.pirs.toml\n  \
             /p/.pirs/ext/a.pirs.toml\n\
             tools: fetch\n\
             commands: /handoff\n\
             status keys: branch\n\
             widget keys: none\n\
             --- policy ---\n\
             files:\n  /p/.pirs/ext/a.pirs.toml\n\
             input:\n  ^\\?(.*) -> replace \"Explain\"  (a.pirs.toml [[input]] #1)\n\
             conflict: duplicate status key `branch`  (/a.pirs.toml, /b.pirs.toml)\n\
             --- system prompt ---\n\
             You are pirs.\n"
        );
    }

    #[test]
    fn a_directory_with_no_policy_says_so_and_still_prints_the_prompt() {
        let empty = DslCheckResult {
            files: Vec::new(),
            manifest: Manifest::default(),
            conflicts: Vec::new(),
            rendered: "files: none\n".to_owned(),
            system_prompt: "You are pirs.\n".to_owned(),
        };
        assert_eq!(
            format_check(&empty),
            "files: none\n\
             tools: none\n\
             commands: none\n\
             status keys: none\n\
             widget keys: none\n\
             --- policy ---\n\
             files: none\n\
             --- system prompt ---\n\
             You are pirs.\n"
        );
    }

    #[test]
    fn an_error_naming_one_file_prints_that_one_file() {
        let mut result = result();
        result.conflicts = vec![DslConflict {
            message: "missing `intent`".to_owned(),
            files: vec![ServerPath::from("/a.pirs.toml")],
        }];
        assert!(format_check(&result).contains("conflict: missing `intent`  (/a.pirs.toml)\n"));
    }
}
