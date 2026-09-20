//! Settings: `~/.pi/agent/settings.json` merged with `.pi/settings.json`,
//! plus `models.json` provider definitions. Same files and keys as pi.

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) const CONFIG_DIR_NAME: &str = ".pi";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Settings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<String>,
    pub extensions: Vec<String>,
    pub packages: Vec<String>,
    pub quiet_startup: bool,
    pub hide_thinking_block: bool,
    pub theme: Option<String>,
    pub tool_execution: Option<String>,
    pub steering_mode: Option<String>,
    pub follow_up_mode: Option<String>,
    pub enabled_models: Vec<String>,
    #[serde(skip)]
    pub raw: Value,
}

pub(crate) fn agent_dir() -> PathBuf {
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

/// Load global settings, then overlay project settings from `cwd/.pi/settings.json`.
pub(crate) fn load_settings(cwd: &Path) -> Settings {
    let mut raw = read_json(&agent_dir().join("settings.json")).unwrap_or_else(|| Value::Object(Default::default()));
    if let Some(project) = read_json(&cwd.join(CONFIG_DIR_NAME).join("settings.json")) {
        merge(&mut raw, project);
    }
    let mut settings: Settings = serde_json::from_value(raw.clone()).unwrap_or_default();
    settings.raw = raw;
    settings
}

/// Apply `~/.pi/agent/models.json` and `.pi/models.json` to the registry.
pub(crate) fn load_models_json(cwd: &Path, registry: &pi_ai::ModelRegistry) {
    for path in [agent_dir().join("models.json"), cwd.join(CONFIG_DIR_NAME).join("models.json")] {
        if let Some(doc) = read_json(&path) {
            registry.apply_models_json(&doc);
        }
    }
}

/// Extension roots pi auto-discovers, in load order.
pub(crate) fn extension_roots(cwd: &Path) -> Vec<PathBuf> {
    vec![agent_dir().join("extensions"), cwd.join(CONFIG_DIR_NAME).join("extensions")]
}

/// Resolve `extensions` entries from settings (files or directories) to entry files.
pub(crate) fn settings_extension_paths(settings: &Settings, cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in &settings.extensions {
        let expanded = if let Some(rest) = e.strip_prefix("~/") { crate::session::get_agent_dir().parent().and_then(|p| p.parent()).map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(e)) } else { PathBuf::from(e) };
        let p = if expanded.is_absolute() { expanded } else { cwd.join(expanded) };
        if p.is_file() {
            out.push(p);
        } else if p.is_dir() {
            out.extend(pi_ext::discover_extensions(std::slice::from_ref(&p)));
            for idx in ["index.ts", "index.js"] {
                let f = p.join(idx);
                if f.is_file() && !out.contains(&f) {
                    out.push(f);
                }
            }
        }
    }
    out
}
