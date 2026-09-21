//! What a loop reads before it has a policy, and the `models.json` catalogue.
//!
//! `settings.json` is gone: the DSL's `[settings]` table replaces it, so
//! there is one loader and one checker (`40-dsl.md`, open question 6). This
//! file is what is left of it — [`Settings`] is [`dsl::SettingsTable`] in the
//! shape the loop uses, plus the two things that are not settings at all:
//! the agent directory and the provider catalogue.
//!
//! `models.json` stays JSON and stays here. It is a provider catalogue —
//! endpoints, ids, context windows — not a statement about how this loop
//! should behave, and nothing in it belongs in a `.pirs.toml`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::dsl::{SettingsTable, ToolExecution};

/// The per-project configuration directory name.
pub const CONFIG_DIR_NAME: &str = ".pirs";

/// The settings a loop starts with, from the merged `[settings]` tables of
/// its policy files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// `model`: what a loop runs when `--model` is absent. `provider/id` or
    /// an id the registry resolves.
    pub default_model: Option<String>,
    /// `thinking`: how hard it thinks when `--thinking` is absent.
    pub default_thinking_level: Option<String>,
    /// `tools`: the tools the model is offered; `None` is the server's
    /// default selection.
    pub tools: Option<Vec<String>>,
    /// `tool_execution`: whether a turn's tool calls run together or one at
    /// a time; `None` is parallel, as `settings.json` defaulted.
    pub tool_execution: Option<ToolExecution>,
}

impl Settings {
    /// The loop's view of a merged `[settings]` table.
    pub fn from_policy(table: &SettingsTable) -> Self {
        Settings {
            default_model: table.model.clone(),
            default_thinking_level: table.thinking.clone(),
            tools: table.tools.clone(),
            tool_execution: table.tool_execution,
        }
    }

}

/// `$PIRS_HOME` or `~/.pirs`.
pub fn agent_dir() -> PathBuf {
    crate::session::get_agent_dir()
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Apply `~/.pirs/models.json` and `<cwd>/.pirs/models.json` to the registry.
pub fn load_models_json(cwd: &Path, registry: &pi_ai::ModelRegistry) {
    for path in [agent_dir().join("models.json"), cwd.join(CONFIG_DIR_NAME).join("models.json")] {
        if let Some(doc) = read_json(&path) {
            registry.apply_models_json(&doc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_come_from_the_policy_table() {
        let table = SettingsTable {
            model: Some("anthropic/claude-sonnet-4-5".to_owned()),
            thinking: Some("high".to_owned()),
            tools: Some(vec!["read".to_owned(), "bash".to_owned()]),
            tool_execution: Some(ToolExecution::Sequential),
        };
        let settings = Settings::from_policy(&table);
        assert_eq!(settings.default_model.as_deref(), Some("anthropic/claude-sonnet-4-5"));
        assert_eq!(settings.default_thinking_level.as_deref(), Some("high"));
        assert_eq!(settings.tools.as_deref(), Some(["read".to_owned(), "bash".to_owned()].as_slice()));
        assert_eq!(settings.tool_execution, Some(ToolExecution::Sequential));
    }

    #[test]
    fn a_directory_with_no_policy_files_has_no_settings() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("tempdir");
        crate::session::tests_support::with_pirs_home(home.path(), || {
            assert_eq!(Settings::from_policy(&crate::dsl::load(project.path()).settings), Settings::default());
        });
    }

    #[test]
    fn a_projects_settings_table_reaches_the_loop() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("tempdir");
        crate::session::tests_support::with_pirs_home(home.path(), || {
            let ext = project.path().join(CONFIG_DIR_NAME).join("ext");
            std::fs::create_dir_all(&ext).expect("ext dir");
            std::fs::write(
                ext.join("a.pirs.toml"),
                "intent = \"pick a model\"\n[settings]\nmodel = \"faux/scripted\"\nthinking = \"low\"\ntool_execution = \"sequential\"\n",
            )
            .expect("write");
            let settings = Settings::from_policy(&crate::dsl::load(project.path()).settings);
            assert_eq!(settings.default_model.as_deref(), Some("faux/scripted"));
            assert_eq!(settings.default_thinking_level.as_deref(), Some("low"));
            assert!(settings.tools.is_none());
            assert_eq!(settings.tool_execution, Some(ToolExecution::Sequential));
        });
    }
}
