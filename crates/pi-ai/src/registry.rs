//! Model catalog, provider configuration, and API-key resolution.

use crate::types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

/// Provider configuration as found in `models.json` or passed to
/// `pi.registerProvider(name, config)` by extensions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Literal key, `$ENV_VAR`, or `${ENV_VAR}`.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub models: Option<Vec<ProviderModelConfig>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub input: Option<Vec<String>>,
    #[serde(default)]
    pub cost: Option<ModelCost>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
struct ProviderEntry {
    api_key: Option<String>,
    env_var: Option<String>,
    headers: HashMap<String, String>,
}

#[derive(Default)]
struct Inner {
    models: BTreeMap<String, Model>, // key: provider/id
    providers: HashMap<String, ProviderEntry>,
    /// pi agent dir used to look up OAuth credentials (`auth.json`, Claude Code login).
    agent_dir: Option<std::path::PathBuf>,
}

/// Thread-safe registry of models and provider credentials.
#[derive(Clone, Default)]
pub struct ModelRegistry {
    inner: Arc<RwLock<Inner>>,
}

pub fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("claude-sonnet-4-5"),
        "openai" => Some("gpt-5"),
        "openrouter" => Some("anthropic/claude-sonnet-4.5"),
        "faux" => Some("scripted"),
        _ => None,
    }
}

fn cost(input: f64, output: f64, cache_read: f64, cache_write: f64) -> ModelCost {
    ModelCost { input, output, cache_read, cache_write }
}

fn anthropic(id: &str, name: &str, c: ModelCost, ctx: u64, max: u64) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: crate::anthropic::API.into(),
        provider: "anthropic".into(),
        base_url: "https://api.anthropic.com".into(),
        reasoning: true,
        input: vec!["text".into(), "image".into()],
        cost: c,
        context_window: ctx,
        max_tokens: max,
        headers: None,
    }
}

fn openai(id: &str, name: &str, c: ModelCost, ctx: u64, max: u64, reasoning: bool) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: crate::openai::API.into(),
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning,
        input: vec!["text".into(), "image".into()],
        cost: c,
        context_window: ctx,
        max_tokens: max,
        headers: None,
    }
}

pub fn builtin_models() -> Vec<Model> {
    vec![
        // Anthropic. Costs are USD per million tokens (input, output, cache read, cache write).
        anthropic("claude-fable-5-1", "Claude Fable 5.1", cost(15.0, 75.0, 1.5, 18.75), 200_000, 64_000),
        anthropic("claude-opus-5", "Claude Opus 5", cost(5.0, 25.0, 0.5, 6.25), 200_000, 64_000),
        anthropic("claude-sonnet-5", "Claude Sonnet 5", cost(3.0, 15.0, 0.3, 3.75), 200_000, 64_000),
        anthropic("claude-opus-4-5", "Claude Opus 4.5", cost(5.0, 25.0, 0.5, 6.25), 200_000, 64_000),
        anthropic("claude-sonnet-4-5", "Claude Sonnet 4.5", cost(3.0, 15.0, 0.3, 3.75), 200_000, 64_000),
        anthropic("claude-haiku-4-5-20251001", "Claude Haiku 4.5", cost(1.0, 5.0, 0.1, 1.25), 200_000, 64_000),
        anthropic("claude-opus-4-1", "Claude Opus 4.1", cost(15.0, 75.0, 1.5, 18.75), 200_000, 32_000),
        // OpenAI
        openai("gpt-5", "GPT-5", cost(1.25, 10.0, 0.125, 0.0), 400_000, 128_000, true),
        openai("gpt-5-mini", "GPT-5 mini", cost(0.25, 2.0, 0.025, 0.0), 400_000, 128_000, true),
        openai("gpt-4.1", "GPT-4.1", cost(2.0, 8.0, 0.5, 0.0), 1_047_576, 32_768, false),
        openai("o3", "o3", cost(2.0, 8.0, 0.5, 0.0), 200_000, 100_000, true),
        // Faux (scripted) provider for tests and offline demos.
        Model {
            id: "scripted".into(),
            name: "Faux scripted model".into(),
            api: crate::faux::API.into(),
            provider: "faux".into(),
            base_url: String::new(),
            reasoning: false,
            input: vec!["text".into(), "image".into()],
            cost: ModelCost::default(),
            context_window: 1_000_000,
            max_tokens: 100_000,
            headers: None,
        },
    ]
}

impl ModelRegistry {
    pub fn with_builtins() -> Self {
        let reg = Self::default();
        {
            let mut inner = reg.inner.write().unwrap();
            for m in builtin_models() {
                inner.models.insert(m.key(), m);
            }
            inner.providers.insert("anthropic".into(), ProviderEntry { api_key: None, env_var: Some("ANTHROPIC_API_KEY".into()), headers: Default::default() });
            inner.providers.insert("openai".into(), ProviderEntry { api_key: None, env_var: Some("OPENAI_API_KEY".into()), headers: Default::default() });
            inner.providers.insert("openrouter".into(), ProviderEntry { api_key: None, env_var: Some("OPENROUTER_API_KEY".into()), headers: Default::default() });
            inner.providers.insert("faux".into(), ProviderEntry { api_key: Some("faux".into()), env_var: None, headers: Default::default() });
        }
        reg
    }

    /// Apply a `models.json`-style document: `{ "providers": { name: ProviderConfig } }`.
    pub fn apply_models_json(&self, doc: &Value) {
        let Some(providers) = doc.get("providers").and_then(|p| p.as_object()) else { return };
        for (name, cfg) in providers {
            if let Ok(cfg) = serde_json::from_value::<ProviderConfig>(cfg.clone()) {
                self.register_provider(name, &cfg);
            }
        }
    }

    pub fn register_provider(&self, name: &str, cfg: &ProviderConfig) {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.providers.entry(name.to_string()).or_insert(ProviderEntry { api_key: None, env_var: None, headers: Default::default() });
        if let Some(k) = &cfg.api_key {
            entry.api_key = Some(k.clone());
        }
        if let Some(h) = &cfg.headers {
            entry.headers = h.clone();
        }
        if let Some(models) = &cfg.models {
            inner.models.retain(|_, m| m.provider != name);
            for mc in models {
                let api = mc.api.clone().or_else(|| cfg.api.clone()).unwrap_or_else(|| crate::openai::API.to_string());
                let base_url = mc.base_url.clone().or_else(|| cfg.base_url.clone()).unwrap_or_default();
                let model = Model {
                    id: mc.id.clone(),
                    name: mc.name.clone().unwrap_or_else(|| mc.id.clone()),
                    api,
                    provider: name.to_string(),
                    base_url,
                    reasoning: mc.reasoning.unwrap_or(false),
                    input: mc.input.clone().unwrap_or_else(|| vec!["text".into()]),
                    cost: mc.cost.clone().unwrap_or_default(),
                    context_window: mc.context_window.unwrap_or(128_000),
                    max_tokens: mc.max_tokens.unwrap_or(8192),
                    headers: mc.headers.clone(),
                };
                inner.models.insert(model.key(), model);
            }
        }
    }

    pub fn unregister_provider(&self, name: &str) {
        let mut inner = self.inner.write().unwrap();
        inner.providers.remove(name);
        inner.models.retain(|_, m| m.provider != name);
    }

    pub fn models(&self) -> Vec<Model> {
        self.inner.read().unwrap().models.values().cloned().collect()
    }

    pub fn get(&self, provider: &str, id: &str) -> Option<Model> {
        self.inner.read().unwrap().models.get(&format!("{provider}/{id}")).cloned()
    }

    /// Resolve `provider/id`, `id`, or a unique substring of an id.
    pub fn find(&self, spec: &str) -> Option<Model> {
        let inner = self.inner.read().unwrap();
        if let Some(m) = inner.models.get(spec) {
            return Some(m.clone());
        }
        if let Some((provider, id)) = spec.split_once('/') {
            if let Some(m) = inner.models.get(&format!("{provider}/{id}")) {
                return Some(m.clone());
            }
        }
        let mut by_id: Vec<&Model> = inner.models.values().filter(|m| m.id == spec).collect();
        if by_id.len() == 1 {
            return Some(by_id.remove(0).clone());
        }
        // Prefer providers with credentials.
        if let Some(m) = by_id.iter().find(|m| self.resolve_api_key_locked(&inner, &m.provider).is_some()) {
            return Some((*m).clone());
        }
        by_id.first().map(|m| (*m).clone())
    }

    fn resolve_api_key_locked(&self, inner: &Inner, provider: &str) -> Option<String> {
        let entry = inner.providers.get(provider)?;
        if let Some(k) = &entry.api_key {
            let resolved = resolve_key_spec(k);
            if resolved.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
                return resolved;
            }
        }
        if let Some(var) = &entry.env_var {
            if let Ok(v) = std::env::var(var) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        None
    }

    pub fn resolve_api_key(&self, provider: &str) -> Option<String> {
        let inner = self.inner.read().unwrap();
        if let Some(k) = self.resolve_api_key_locked(&inner, provider) {
            return Some(k);
        }
        if provider == "anthropic" {
            if let Some(dir) = &inner.agent_dir {
                // Stored OAuth login (pi auth.json or Claude Code); refreshed lazily by resolve_api_key_async.
                return crate::oauth::find_credential(dir).map(|(c, _)| c.access);
            }
        }
        None
    }

    /// Enable OAuth credential lookup (pi's `auth.json` and the Claude Code login).
    pub fn set_agent_dir(&self, dir: std::path::PathBuf) {
        self.inner.write().unwrap().agent_dir = Some(dir);
    }

    /// Like `resolve_api_key`, but refreshes an expired OAuth token first.
    pub async fn resolve_api_key_async(&self, provider: &str) -> Result<Option<String>, String> {
        let (direct, agent_dir) = {
            let inner = self.inner.read().unwrap();
            (self.resolve_api_key_locked(&inner, provider), inner.agent_dir.clone())
        };
        if direct.is_some() {
            return Ok(direct);
        }
        if provider == "anthropic" {
            if let Some(dir) = agent_dir {
                return crate::oauth::resolve_access_token(&dir).await.map(|r| r.map(|(t, _)| t));
            }
        }
        Ok(None)
    }

    /// Human-readable description of where a provider's credentials come from.
    pub fn credential_source(&self, provider: &str) -> Option<String> {
        let inner = self.inner.read().unwrap();
        if let Some(entry) = inner.providers.get(provider) {
            if let Some(k) = &entry.api_key {
                if resolve_key_spec(k).map(|s| !s.is_empty()).unwrap_or(false) {
                    return Some(if k.starts_with('$') { format!("env {}", k.trim_start_matches('$')) } else { "models.json".into() });
                }
            }
            if let Some(var) = &entry.env_var {
                if std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false) {
                    return Some(format!("env {var}"));
                }
            }
        }
        if provider == "anthropic" {
            if let Some(dir) = &inner.agent_dir {
                return crate::oauth::find_credential(dir).map(|(c, s)| if c.is_expired() { format!("{} (expired)", s.label()) } else { s.label() });
            }
        }
        None
    }

    pub fn provider_headers(&self, provider: &str) -> HashMap<String, String> {
        self.inner.read().unwrap().providers.get(provider).map(|p| p.headers.clone()).unwrap_or_default()
    }

    pub fn has_credentials(&self, provider: &str) -> bool {
        self.resolve_api_key(provider).is_some()
    }

    /// Models whose provider has credentials configured.
    pub fn available(&self) -> Vec<Model> {
        self.models().into_iter().filter(|m| self.has_credentials(&m.provider)).collect()
    }
}

/// `$VAR`, `${VAR}`, or a literal.
pub fn resolve_key_spec(spec: &str) -> Option<String> {
    if let Some(rest) = spec.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(rest).ok();
    }
    if let Some(var) = spec.strip_prefix('$') {
        return std::env::var(var).ok();
    }
    if let Some(cmd) = spec.strip_prefix('!') {
        let out = std::process::Command::new("sh").arg("-c").arg(cmd).output().ok()?;
        return Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    Some(spec.to_string())
}

/// Dispatch a request to the provider implementation for `model.api`.
pub fn stream(model: &Model, context: Context, options: StreamOptions) -> EventStream {
    match model.api.as_str() {
        crate::anthropic::API => crate::anthropic::stream(model.clone(), context, options),
        crate::openai::API => crate::openai::stream(model.clone(), context, options),
        crate::faux::API => crate::faux::stream(model.clone(), context, options),
        other => {
            use futures::StreamExt;
            let msg = AssistantMessage::error(model, format!("Unsupported api '{other}'"), false);
            futures::stream::once(async move { AssistantMessageEvent::Error { reason: StopReason::Error, error: msg } }).boxed()
        }
    }
}

/// Collect a stream into its final message.
pub async fn complete(model: &Model, context: Context, options: StreamOptions) -> AssistantMessage {
    use futures::StreamExt;
    let mut s = stream(model, context, options);
    let mut last: Option<AssistantMessage> = None;
    while let Some(ev) = s.next().await {
        if let Some(m) = ev.final_message() {
            last = Some(m.clone());
        }
    }
    last.unwrap_or_else(|| AssistantMessage::error(model, "Stream ended without a final message", false))
}
