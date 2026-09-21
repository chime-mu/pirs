//! Settings: `~/.pirs/settings.json` overlaid by `<cwd>/.pirs/settings.json`,
//! plus `models.json` provider definitions.
//!
//! Phase 2 replaces this file with the DSL's `[settings]` table, so the struct
//! is deliberately small: only the keys the loop reads before any policy file
//! is loaded.

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The per-project configuration directory name.
pub const CONFIG_DIR_NAME: &str = ".pirs";

/// The settings the server reads before any policy file applies.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Provider to use when `--model` names no provider.
    pub default_provider: Option<String>,
    /// Model to use when `--model` is absent.
    pub default_model: Option<String>,
    /// Thinking level to use when `--thinking` is absent.
    pub default_thinking_level: Option<String>,
    /// `"sequential"` runs tool calls one at a time; anything else is parallel.
    pub tool_execution: Option<String>,
    /// Model keys a UI offers; empty means "all of them".
    pub enabled_models: Vec<String>,
    /// The merged JSON, for keys this struct does not name.
    #[serde(skip)]
    pub raw: Value,
}

/// `$PIRS_HOME` or `~/.pirs`.
pub fn agent_dir() -> PathBuf {
    crate::session::get_agent_dir()
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge(existing, v),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Load `~/.pirs/settings.json`, then overlay `<cwd>/.pirs/settings.json`.
pub fn load_settings(cwd: &Path) -> Settings {
    let mut raw = read_json(&agent_dir().join("settings.json")).unwrap_or_else(|| Value::Object(Default::default()));
    if let Some(project) = read_json(&cwd.join(CONFIG_DIR_NAME).join("settings.json")) {
        merge(&mut raw, project);
    }
    let mut settings: Settings = serde_json::from_value(raw.clone()).unwrap_or_default();
    settings.raw = raw;
    settings
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
    fn project_settings_overlay_the_home_ones() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("tempdir");
        crate::session::tests_support::with_pirs_home(home.path(), || {
            std::fs::create_dir_all(agent_dir()).expect("agent dir");
            std::fs::write(
                agent_dir().join("settings.json"),
                r#"{"defaultModel":"a","defaultProvider":"anthropic","enabledModels":["a"],"extra":{"x":1}}"#,
            )
            .expect("write");
            std::fs::create_dir_all(project.path().join(CONFIG_DIR_NAME)).expect("project dir");
            std::fs::write(
                project.path().join(CONFIG_DIR_NAME).join("settings.json"),
                r#"{"defaultModel":"b","toolExecution":"sequential","extra":{"y":2}}"#,
            )
            .expect("write");

            let s = load_settings(project.path());
            assert_eq!(s.default_model.as_deref(), Some("b"));
            assert_eq!(s.default_provider.as_deref(), Some("anthropic"));
            assert_eq!(s.tool_execution.as_deref(), Some("sequential"));
            assert_eq!(s.enabled_models, ["a"]);
            // Objects merge rather than replace, and unnamed keys survive in `raw`.
            assert_eq!(s.raw["extra"]["x"], 1);
            assert_eq!(s.raw["extra"]["y"], 2);
        });
    }

    #[test]
    fn missing_files_give_defaults() {
        let home = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("tempdir");
        crate::session::tests_support::with_pirs_home(home.path(), || {
            let s = load_settings(project.path());
            assert!(s.default_model.is_none());
            assert!(s.enabled_models.is_empty());
        });
    }
}
