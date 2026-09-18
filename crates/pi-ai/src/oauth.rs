//! Anthropic OAuth credentials: pi's `~/.pi/agent/auth.json` and Claude Code's
//! stored login (`~/.claude/.credentials.json`, or the macOS keychain item
//! "Claude Code-credentials"). OAuth access tokens start with `sk-ant-oat`
//! and are sent as Bearer tokens with Claude Code identity headers.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const EXPIRY_SKEW_MS: u64 = 5 * 60 * 1000;

pub fn is_oauth_token(key: &str) -> bool {
    key.contains("sk-ant-oat")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    /// Unix milliseconds.
    pub expires: u64,
}

impl OAuthCredential {
    pub fn is_expired(&self) -> bool {
        crate::types::now_ms() >= self.expires
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CredentialSource {
    /// `~/.pi/agent/auth.json`, provider `anthropic`, `type: "oauth"`.
    PiAuthJson(PathBuf),
    /// `~/.claude/.credentials.json` (Claude Code).
    ClaudeCodeFile(PathBuf),
    /// macOS keychain item "Claude Code-credentials".
    ClaudeCodeKeychain,
}

impl CredentialSource {
    pub fn label(&self) -> String {
        match self {
            Self::PiAuthJson(p) => format!("oauth ({})", p.display()),
            Self::ClaudeCodeFile(p) => format!("Claude Code login ({})", p.display()),
            Self::ClaudeCodeKeychain => "Claude Code login (keychain)".into(),
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Parse pi's auth.json `anthropic` entry.
pub fn parse_pi_auth(doc: &Value) -> Option<OAuthCredential> {
    let a = doc.get("anthropic")?;
    if a.get("type")?.as_str()? != "oauth" {
        return None;
    }
    Some(OAuthCredential { access: a["access"].as_str()?.to_string(), refresh: a["refresh"].as_str()?.to_string(), expires: a["expires"].as_u64()? })
}

/// Parse Claude Code's credentials document (`claudeAiOauth`).
pub fn parse_claude_code(doc: &Value) -> Option<OAuthCredential> {
    let o = doc.get("claudeAiOauth")?;
    Some(OAuthCredential {
        access: o["accessToken"].as_str()?.to_string(),
        refresh: o["refreshToken"].as_str().unwrap_or("").to_string(),
        expires: o["expiresAt"].as_u64().unwrap_or(u64::MAX),
    })
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn keychain_credential() -> Option<OAuthCredential> {
    if std::env::consts::OS != "macos" {
        return None;
    }
    let out = std::process::Command::new("security").args(["find-generic-password", "-s", "Claude Code-credentials", "-w"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let doc: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
    parse_claude_code(&doc)
}

/// All stored Anthropic OAuth credentials, in priority order.
pub fn find_credentials(agent_dir: &Path) -> Vec<(OAuthCredential, CredentialSource)> {
    let mut out = Vec::new();
    let pi_auth = agent_dir.join("auth.json");
    if let Some(c) = read_json(&pi_auth).and_then(|d| parse_pi_auth(&d)) {
        out.push((c, CredentialSource::PiAuthJson(pi_auth)));
    }
    let cc_file = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".claude")).join(".credentials.json");
    if let Some(c) = read_json(&cc_file).and_then(|d| parse_claude_code(&d)) {
        out.push((c, CredentialSource::ClaudeCodeFile(cc_file)));
    }
    if let Some(c) = keychain_credential() {
        out.push((c, CredentialSource::ClaudeCodeKeychain));
    }
    out
}

/// The best stored credential without refreshing: the first non-expired one,
/// else the first one found.
pub fn find_credential(agent_dir: &Path) -> Option<(OAuthCredential, CredentialSource)> {
    let all = find_credentials(agent_dir);
    all.iter().find(|(c, _)| !c.is_expired()).cloned().or_else(|| all.into_iter().next())
}

/// Refresh an access token. Returns the new credential (refresh tokens rotate).
pub async fn refresh(refresh_token: &str) -> Result<OAuthCredential, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .json(&json!({"grant_type": "refresh_token", "client_id": CLIENT_ID, "refresh_token": refresh_token}))
        .send()
        .await
        .map_err(|e| format!("Anthropic token refresh request failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Anthropic token refresh failed: HTTP {status}: {body}"));
    }
    let data: Value = serde_json::from_str(&body).map_err(|e| format!("Anthropic token refresh returned invalid JSON: {e}"))?;
    let access = data["access_token"].as_str().ok_or("token refresh response has no access_token")?.to_string();
    let refresh = data["refresh_token"].as_str().unwrap_or(refresh_token).to_string();
    let expires_in = data["expires_in"].as_u64().unwrap_or(3600);
    Ok(OAuthCredential { access, refresh, expires: crate::types::now_ms() + expires_in * 1000 - EXPIRY_SKEW_MS })
}

fn store(source: &CredentialSource, cred: &OAuthCredential) -> Result<(), String> {
    match source {
        CredentialSource::PiAuthJson(path) => {
            let mut doc = read_json(path).unwrap_or_else(|| json!({}));
            doc["anthropic"] = json!({"type": "oauth", "access": cred.access, "refresh": cred.refresh, "expires": cred.expires});
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap_or_default()).map_err(|e| e.to_string())
        }
        CredentialSource::ClaudeCodeFile(path) => {
            let mut doc = read_json(path).unwrap_or_else(|| json!({}));
            doc["claudeAiOauth"]["accessToken"] = json!(cred.access);
            doc["claudeAiOauth"]["refreshToken"] = json!(cred.refresh);
            doc["claudeAiOauth"]["expiresAt"] = json!(cred.expires);
            std::fs::write(path, serde_json::to_string(&doc).unwrap_or_default()).map_err(|e| e.to_string())
        }
        // Never rewrite the user's keychain from a side tool.
        CredentialSource::ClaudeCodeKeychain => Ok(()),
    }
}

/// Resolve a usable OAuth access token. Sources are tried in order; a valid
/// token wins immediately, an expired file-based token is refreshed and
/// persisted, and a source whose refresh fails is skipped. Keychain
/// credentials are used only while valid so that Claude Code's own login is
/// never rotated behind its back.
pub async fn resolve_access_token(agent_dir: &Path) -> Result<Option<(String, CredentialSource)>, String> {
    let all = find_credentials(agent_dir);
    if all.is_empty() {
        return Ok(None);
    }
    if let Some((c, s)) = all.iter().find(|(c, _)| !c.is_expired()) {
        return Ok(Some((c.access.clone(), s.clone())));
    }
    let mut errors = Vec::new();
    for (cred, source) in all {
        if source == CredentialSource::ClaudeCodeKeychain || cred.refresh.is_empty() {
            errors.push(format!("{}: expired; run `claude` once to refresh it", source.label()));
            continue;
        }
        match refresh(&cred.refresh).await {
            Ok(fresh) => {
                store(&source, &fresh)?;
                return Ok(Some((fresh.access, source)));
            }
            Err(e) => errors.push(format!("{}: {e}", source.label())),
        }
    }
    Err(format!("No usable Anthropic login. {}", errors.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_credential_shapes() {
        let pi = json!({"anthropic": {"type": "oauth", "access": "sk-ant-oat01-x", "refresh": "r", "expires": 1u64}});
        let c = parse_pi_auth(&pi).unwrap();
        assert_eq!(c.access, "sk-ant-oat01-x");
        assert!(c.is_expired());
        let cc = json!({"claudeAiOauth": {"accessToken": "sk-ant-oat01-y", "refreshToken": "rr", "expiresAt": u64::MAX - 1, "scopes": ["user:inference"]}});
        let c = parse_claude_code(&cc).unwrap();
        assert_eq!(c.refresh, "rr");
        assert!(!c.is_expired());
        assert!(parse_pi_auth(&json!({"anthropic": {"type": "api_key", "key": "k"}})).is_none());
        assert!(is_oauth_token("sk-ant-oat01-abc"));
        assert!(!is_oauth_token("sk-ant-api03-abc"));
    }

    #[test]
    fn finds_file_credentials_in_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        // Point Claude Code's config dir at the temp dir so the test never touches the real login.
        std::env::set_var("CLAUDE_CONFIG_DIR", dir.path());
        std::fs::write(dir.path().join(".credentials.json"), json!({"claudeAiOauth": {"accessToken": "sk-ant-oat01-cc", "refreshToken": "r", "expiresAt": u64::MAX - 1}}).to_string()).unwrap();
        let (c, s) = find_credential(&agent).unwrap();
        assert_eq!(c.access, "sk-ant-oat01-cc");
        assert!(matches!(s, CredentialSource::ClaudeCodeFile(_)));
        std::fs::write(agent.join("auth.json"), json!({"anthropic": {"type": "oauth", "access": "sk-ant-oat01-pi", "refresh": "r", "expires": u64::MAX - 1}}).to_string()).unwrap();
        let (c, s) = find_credential(&agent).unwrap();
        assert_eq!(c.access, "sk-ant-oat01-pi");
        assert!(matches!(s, CredentialSource::PiAuthJson(_)));
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }
}
