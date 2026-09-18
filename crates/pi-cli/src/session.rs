//! Session manager: append-only JSONL conversation trees, on-disk compatible
//! with pi's session format (version 3). Port of
//! `packages/coding-agent/src/core/session-manager.ts`.
//!
//! Every non-header line is an entry with `id` / `parentId` / `timestamp`
//! forming a tree. The "leaf" is the current position; appending creates a
//! child of the leaf, branching moves the leaf to an earlier entry.
//! `build_session_context()` resolves the active path (honouring compaction)
//! into the message list handed to the LLM.
//!
//! The public surface is consumed by the CLI front-end, which is wired up in
//! other modules of this crate.
#![allow(dead_code)]

use anyhow::{anyhow, bail, Context as _, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use pi_agent::{AgentMessage, BranchSummaryMessage, CompactionSummaryMessage, CustomMessage};
use pi_ai::{SystemMessage, Tool, Usage, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

pub const CURRENT_SESSION_VERSION: u32 = 3;
const APP_NAME: &str = "pi";
const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
/// Bound header discovery so a corrupt file cannot make listing read gigabytes.
const MAX_SESSION_HEADER_SCAN_BYTES: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// Entry types
// ---------------------------------------------------------------------------

/// First line of a session file. Metadata only, not part of the tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    /// Always `"session"`.
    #[serde(rename = "type")]
    pub entry_type: String,
    /// Missing on v1 sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub id: String,
    #[serde(default)]
    pub timestamp: String,
    /// Empty string for very old sessions.
    #[serde(default)]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    /// Unknown header fields written by other tools, preserved verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Payload of a session entry, tagged by `type`. Unknown types are kept as raw
/// JSON so that entries written by newer pi versions or extensions survive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    Message {
        message: AgentMessage,
    },
    #[serde(rename_all = "camelCase")]
    ThinkingLevelChange {
        thinking_level: String,
    },
    #[serde(rename_all = "camelCase")]
    ModelChange {
        provider: String,
        model_id: String,
    },
    #[serde(rename_all = "camelCase")]
    Compaction {
        summary: String,
        first_kept_entry_id: String,
        tokens_before: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
        /// Complete prompt and tool state at the compaction boundary.
        #[serde(default, skip_serializing_if = "Option::is_none", with = "tagged_system_message")]
        system_message: Option<SystemMessage>,
    },
    #[serde(rename_all = "camelCase")]
    BranchSummary {
        /// Previous leaf whose abandoned path was summarized (`"root"` when
        /// there was none).
        #[serde(default)]
        from_id: Option<String>,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    /// Extension state. Does NOT participate in LLM context.
    #[serde(rename_all = "camelCase")]
    Custom {
        custom_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    /// Extension-injected message that DOES participate in LLM context.
    #[serde(rename_all = "camelCase")]
    CustomMessage {
        custom_type: String,
        content: UserContent,
        display: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    #[serde(rename_all = "camelCase")]
    Label {
        target_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    SessionInfo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Any entry whose `type` is not known (or whose payload we could not
    /// parse). Holds the full JSON object minus `id`/`parentId`/`timestamp`.
    #[serde(untagged)]
    Unknown(Value),
}

/// `SystemMessage` is a bare struct in pi-ai (the `role` tag lives on the
/// `Message` enum), but pi writes the compaction checkpoint as a full
/// `{"role":"system",...}` message. Round-trip through the tagged enum.
mod tagged_system_message {
    use pi_ai::{Message, SystemMessage};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<SystemMessage>, s: S) -> Result<S::Ok, S::Error> {
        value.clone().map(Message::System).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<SystemMessage>, D::Error> {
        match Option::<Message>::deserialize(d)? {
            None => Ok(None),
            Some(Message::System(sys)) => Ok(Some(sys)),
            Some(other) => Err(serde::de::Error::custom(format!("compaction systemMessage has role {}", other.role()))),
        }
    }
}

impl EntryKind {
    /// The `type` tag as written to the file.
    pub fn type_name(&self) -> &str {
        match self {
            Self::Message { .. } => "message",
            Self::ThinkingLevelChange { .. } => "thinking_level_change",
            Self::ModelChange { .. } => "model_change",
            Self::Compaction { .. } => "compaction",
            Self::BranchSummary { .. } => "branch_summary",
            Self::Custom { .. } => "custom",
            Self::CustomMessage { .. } => "custom_message",
            Self::Label { .. } => "label",
            Self::SessionInfo { .. } => "session_info",
            Self::Unknown(v) => v.get("type").and_then(Value::as_str).unwrap_or(""),
        }
    }

    fn from_object(obj: Map<String, Value>) -> Self {
        // Try the typed representation first; anything we cannot represent is
        // kept verbatim so it is never dropped on rewrite.
        match serde_json::from_value::<EntryKind>(Value::Object(obj.clone())) {
            Ok(EntryKind::Unknown(_)) | Err(_) => EntryKind::Unknown(Value::Object(obj)),
            Ok(kind) => kind,
        }
    }
}

/// A session entry: tree metadata plus the typed payload.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    /// Usually 8 hex chars; may be a full UUID on collision fallback.
    pub id: String,
    /// `None` for a root entry.
    pub parent_id: Option<String>,
    /// ISO-8601 timestamp.
    pub timestamp: String,
    pub kind: EntryKind,
}

impl SessionEntry {
    pub fn type_name(&self) -> &str {
        self.kind.type_name()
    }

    pub fn message(&self) -> Option<&AgentMessage> {
        match &self.kind {
            EntryKind::Message { message } => Some(message),
            _ => None,
        }
    }

    fn is_assistant_message(&self) -> bool {
        matches!(self.message(), Some(AgentMessage::Assistant(_)))
    }

    fn is_system_message(&self) -> bool {
        matches!(self.message(), Some(AgentMessage::System(_)))
    }

    /// Unix-ms version of `timestamp` (0 when unparseable).
    pub fn timestamp_ms(&self) -> u64 {
        iso_to_ms(&self.timestamp)
    }

    fn from_value(value: Value) -> Result<Self> {
        let Value::Object(mut obj) = value else {
            bail!("session entry must be a JSON object");
        };
        let id = match obj.remove("id") {
            Some(Value::String(s)) => s,
            _ => bail!("session entry missing id"),
        };
        let parent_id = match obj.remove("parentId") {
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
        let timestamp = match obj.remove("timestamp") {
            Some(Value::String(s)) => s,
            _ => String::new(),
        };
        Ok(SessionEntry { id, parent_id, timestamp, kind: EntryKind::from_object(obj) })
    }
}

impl Serialize for SessionEntry {
    // Manual so the line layout matches pi: type, id, parentId, timestamp, payload.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeMap};
        let payload = match &self.kind {
            EntryKind::Unknown(v) => v.clone(),
            kind => serde_json::to_value(kind).map_err(S::Error::custom)?,
        };
        let Value::Object(payload) = payload else {
            return Err(S::Error::custom("session entry payload must be a JSON object"));
        };
        let mut map = serializer.serialize_map(None)?;
        if let Some(t) = payload.get("type") {
            map.serialize_entry("type", t)?;
        }
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("parentId", &self.parent_id)?;
        map.serialize_entry("timestamp", &self.timestamp)?;
        for (k, v) in &payload {
            if !matches!(k.as_str(), "type" | "id" | "parentId" | "timestamp") {
                map.serialize_entry(k, v)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for SessionEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        SessionEntry::from_value(value).map_err(serde::de::Error::custom)
    }
}

/// What gets sent to the LLM for the current branch.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    /// Most recent `thinking_level_change` on the path, if any.
    pub thinking_level: Option<String>,
    /// `(provider, model_id)` from the latest model change or assistant message.
    pub model: Option<(String, String)>,
}

/// Metadata about a session file, as shown by `/resume`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub id: String,
    /// Empty string for old sessions without a cwd.
    pub cwd: String,
    /// User-defined display name from the latest `session_info` entry.
    pub name: Option<String>,
    pub parent_session_path: Option<String>,
    pub created: DateTime<Utc>,
    pub modified: DateTime<Utc>,
    pub message_count: usize,
    /// Text of the first user message, or `"(no messages)"`.
    pub first_message: String,
    pub all_messages_text: String,
}

#[derive(Debug, Clone, Default)]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn iso_to_ms(iso: &str) -> u64 {
    DateTime::parse_from_rfc3339(iso).map(|d| d.timestamp_millis().max(0) as u64).unwrap_or(0)
}

fn parse_iso(iso: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(iso).ok().map(|d| d.with_timezone(&Utc))
}

fn ms_to_datetime(ms: u64) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(ms as i64)
}

/// `2024-12-03T14-00-00-000Z` from an ISO timestamp (used in file names).
fn file_timestamp(iso: &str) -> String {
    iso.replace([':', '.'], "-")
}

/// UUIDv7 (time-ordered) like pi's `createSessionId`.
fn create_session_id() -> String {
    let ms = pi_ai::now_ms();
    let random = Uuid::new_v4().into_bytes();
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&ms.to_be_bytes()[2..8]);
    bytes[6..].copy_from_slice(&random[6..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).hyphenated().to_string()
}

pub fn assert_valid_session_id(id: &str) -> Result<()> {
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    let first = id.chars().next();
    let last = id.chars().last();
    let valid = matches!(first, Some(c) if c.is_ascii_alphanumeric())
        && matches!(last, Some(c) if c.is_ascii_alphanumeric())
        && id.chars().all(ok_char);
    if !valid {
        bail!(
            "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character"
        );
    }
    Ok(())
}

/// Unique short id (8 hex chars, collision-checked against `taken`).
fn generate_id(taken: impl Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let id = Uuid::new_v4().simple().to_string()[..8].to_string();
        if !taken(&id) {
            return id;
        }
    }
    Uuid::new_v4().hyphenated().to_string()
}

/// Lexically absolute + normalized path (like Node's `path.resolve`).
pub fn resolve_path(input: impl AsRef<Path>) -> PathBuf {
    let input = input.as_ref();
    let joined = if input.is_absolute() {
        input.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(input)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if p == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(p)
}

/// `$PI_CODING_AGENT_DIR` or `~/.pi/agent`.
pub fn get_agent_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(ENV_AGENT_DIR) {
        if !dir.is_empty() {
            return expand_tilde(&dir);
        }
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(format!(".{APP_NAME}")).join("agent")
}

/// Root directory holding one sub-directory per project cwd.
pub fn get_sessions_dir() -> PathBuf {
    get_agent_dir().join("sessions")
}

/// `--<cwd with leading separator removed and / \ : replaced by ->--`.
pub fn encode_cwd_dir_name(resolved_cwd: &str) -> String {
    let stripped = resolved_cwd
        .strip_prefix('/')
        .or_else(|| resolved_cwd.strip_prefix('\\'))
        .unwrap_or(resolved_cwd);
    format!("--{}--", stripped.replace(['/', '\\', ':'], "-"))
}

/// Default session directory for a cwd under `agent_dir` (not created).
pub fn get_default_session_dir_path_in(cwd: &str, agent_dir: &Path) -> PathBuf {
    resolve_path(agent_dir).join("sessions").join(encode_cwd_dir_name(&path_string(&resolve_path(cwd))))
}

/// Default session directory for a cwd (`~/.pi/agent/sessions/--<cwd>--`, not created).
pub fn get_default_session_dir_path(cwd: &str) -> PathBuf {
    get_default_session_dir_path_in(cwd, &get_agent_dir())
}

/// Default session directory for a cwd, created if missing.
pub fn get_default_session_dir(cwd: &str) -> Result<PathBuf> {
    let dir = get_default_session_dir_path(cwd);
    fs::create_dir_all(&dir).with_context(|| format!("creating session dir {}", dir.display()))?;
    Ok(dir)
}

fn session_cwd_matches(cwd: &str, resolved_cwd: &Path) -> bool {
    !cwd.is_empty() && resolve_path(cwd) == resolved_cwd
}

fn sanitize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_break = false;
    for c in name.chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                out.push(' ');
                in_break = true;
            }
        } else {
            out.push(c);
            in_break = false;
        }
    }
    out.trim().to_string()
}

fn write_line(w: &mut impl Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *w, value)?;
    w.write_all(b"\n")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Loading & migration
// ---------------------------------------------------------------------------

struct LoadedFile {
    header: SessionHeader,
    entries: Vec<SessionEntry>,
    migrated: bool,
}

fn parse_session_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(line).ok().filter(Value::is_object)
}

fn is_header_value(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("session") && v.get("id").is_some_and(Value::is_string)
}

/// Parse raw JSONL content into (header, entries) values. Malformed lines are skipped.
fn parse_session_values(content: &str) -> Vec<Value> {
    content.lines().filter_map(parse_session_line).collect()
}

/// Migrate v1 -> v2: assign ids / parentIds forming a linear chain and turn
/// `firstKeptEntryIndex` into `firstKeptEntryId`.
fn migrate_v1_to_v2(values: &mut [Value]) {
    let mut ids: HashSet<String> = HashSet::new();
    let mut prev: Option<String> = None;
    for v in values.iter_mut() {
        if is_header_value(v) {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("version".into(), Value::from(2));
            }
            continue;
        }
        let Some(obj) = v.as_object_mut() else { continue };
        let id = generate_id(|c| ids.contains(c));
        ids.insert(id.clone());
        obj.insert("id".into(), Value::String(id.clone()));
        obj.insert("parentId".into(), prev.clone().map(Value::String).unwrap_or(Value::Null));
        prev = Some(id);
    }
    // Second pass: resolve compaction indices (they index the full file list, header included).
    let snapshot: Vec<Option<String>> =
        values.iter().map(|v| v.get("id").and_then(Value::as_str).map(String::from)).collect();
    for v in values.iter_mut() {
        if v.get("type").and_then(Value::as_str) != Some("compaction") {
            continue;
        }
        let Some(obj) = v.as_object_mut() else { continue };
        let index = obj.remove("firstKeptEntryIndex").and_then(|i| i.as_u64());
        if let Some(index) = index {
            if let Some(Some(target)) = snapshot.get(index as usize) {
                obj.insert("firstKeptEntryId".into(), Value::String(target.clone()));
            }
        }
    }
}

/// Migrate v2 -> v3: rename the `hookMessage` role to `custom`.
fn migrate_v2_to_v3(values: &mut [Value]) {
    for v in values.iter_mut() {
        if is_header_value(v) {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("version".into(), Value::from(3));
            }
            continue;
        }
        if v.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        if let Some(role) = v.get_mut("message").and_then(|m| m.get_mut("role")) {
            if role.as_str() == Some("hookMessage") {
                *role = Value::String("custom".into());
            }
        }
    }
}

/// Bring raw values up to the current version. Returns true if anything changed.
fn migrate_to_current_version(values: &mut [Value]) -> bool {
    let version = values
        .iter()
        .find(|v| is_header_value(v))
        .and_then(|h| h.get("version"))
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    if version >= CURRENT_SESSION_VERSION {
        return false;
    }
    if version < 2 {
        migrate_v1_to_v2(values);
    }
    if version < 3 {
        migrate_v2_to_v3(values);
    }
    true
}

fn values_to_loaded(mut values: Vec<Value>) -> Option<LoadedFile> {
    if values.is_empty() || !is_header_value(&values[0]) {
        return None;
    }
    let migrated = migrate_to_current_version(&mut values);
    let mut iter = values.into_iter();
    let header: SessionHeader = serde_json::from_value(iter.next()?).ok()?;
    let mut entries = Vec::new();
    for v in iter {
        match SessionEntry::from_value(v) {
            Ok(e) => entries.push(e),
            Err(err) => tracing::warn!("skipping unreadable session entry: {err}"),
        }
    }
    Some(LoadedFile { header, entries, migrated })
}

/// Load a session file. `None` when the file is empty or is not a pi session
/// (no valid header on the first line).
fn load_entries_from_file(path: &Path) -> Result<Option<LoadedFile>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let content = String::from_utf8_lossy(&bytes);
    let values = parse_session_values(&content);
    let loaded = values_to_loaded(values);
    if loaded.is_some() && !content.is_empty() && !content.ends_with('\n') {
        // Repair a missing trailing newline so future appends stay line-aligned.
        let mut f = fs::OpenOptions::new().append(true).open(path)?;
        f.write_all(b"\n")?;
    }
    Ok(loaded)
}

/// Read only the header line of a session file (bounded scan).
fn read_session_header(path: &Path) -> Result<Option<SessionHeader>> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file.take(MAX_SESSION_HEADER_SCAN_BYTES + 1));
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if line.len() as u64 > MAX_SESSION_HEADER_SCAN_BYTES {
            bail!("Session header exceeds {MAX_SESSION_HEADER_SCAN_BYTES}-byte scan limit: {}", path.display());
        }
        let Some(value) = parse_session_line(&line) else { continue };
        if !is_header_value(&value) {
            return Ok(None);
        }
        return Ok(serde_json::from_value(value).ok());
    }
}

fn read_session_header_for_discovery(path: &Path) -> Option<SessionHeader> {
    read_session_header(path).ok().flatten()
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    files
}

/// Most recent (by mtime) session file in `dir`, optionally restricted to `cwd`.
pub fn find_most_recent_session(dir: &Path, cwd: Option<&str>) -> Option<PathBuf> {
    let resolved_cwd = cwd.map(resolve_path);
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = jsonl_files(dir)
        .into_iter()
        .filter(|p| match read_session_header_for_discovery(p) {
            Some(h) => resolved_cwd.as_ref().is_none_or(|c| session_cwd_matches(&h.cwd, c)),
            None => false,
        })
        .filter_map(|p| fs::metadata(&p).and_then(|m| m.modified()).ok().map(|t| (p, t)))
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.into_iter().next().map(|(p, _)| p)
}

fn extract_text_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Port of `buildSessionInfo`: scans a file line by line without building a tree.
fn build_session_info(path: &Path) -> Option<SessionInfo> {
    let meta = fs::metadata(path).ok()?;
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut header: Option<SessionHeader> = None;
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity: Option<u64> = None;

    for line in reader.lines() {
        let line = line.ok()?;
        let Some(entry) = parse_session_line(&line) else { continue };
        if header.is_none() {
            if !is_header_value(&entry) {
                return None;
            }
            header = serde_json::from_value(entry).ok();
            continue;
        }
        let ty = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "session_info" {
            name = entry.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        }
        if ty != "message" {
            continue;
        }
        message_count += 1;
        let Some(message) = entry.get("message") else { continue };
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let activity = message
            .get("timestamp")
            .and_then(Value::as_u64)
            .or_else(|| entry.get("timestamp").and_then(Value::as_str).map(iso_to_ms).filter(|t| *t > 0));
        if let Some(t) = activity {
            last_activity = Some(last_activity.unwrap_or(0).max(t));
        }
        let Some(content) = message.get("content") else { continue };
        let text = extract_text_content(content);
        if text.is_empty() {
            continue;
        }
        if first_message.is_empty() && role == "user" {
            first_message = text.clone();
        }
        all_messages.push(text);
    }

    let header = header?;
    let mtime: DateTime<Utc> = meta.modified().map(DateTime::<Utc>::from).unwrap_or_else(|_| Utc::now());
    let header_time = parse_iso(&header.timestamp);
    let modified = last_activity
        .filter(|t| *t > 0)
        .and_then(ms_to_datetime)
        .or(header_time)
        .unwrap_or(mtime);
    Some(SessionInfo {
        path: path.to_path_buf(),
        id: header.id,
        cwd: header.cwd,
        name,
        parent_session_path: header.parent_session,
        created: header_time.unwrap_or(mtime),
        modified,
        message_count,
        first_message: if first_message.is_empty() { "(no messages)".to_string() } else { first_message },
        all_messages_text: all_messages.join(" "),
    })
}

fn list_sessions_from_dir(dir: &Path) -> Vec<SessionInfo> {
    if !dir.exists() {
        return Vec::new();
    }
    jsonl_files(dir).iter().filter_map(|p| build_session_info(p)).collect()
}

// ---------------------------------------------------------------------------
// Context building (free functions, mirroring the TS exports)
// ---------------------------------------------------------------------------

fn build_index(entries: &[SessionEntry]) -> HashMap<&str, &SessionEntry> {
    entries.iter().map(|e| (e.id.as_str(), e)).collect()
}

fn build_session_path<'a>(index: &HashMap<&str, &'a SessionEntry>, leaf_id: Option<&str>) -> Vec<&'a SessionEntry> {
    let mut path = Vec::new();
    let mut current = leaf_id.and_then(|id| index.get(id).copied());
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(entry) = current {
        if !seen.insert(entry.id.as_str()) {
            break; // defensive: cycle in a hand-edited file
        }
        path.push(entry);
        current = entry.parent_id.as_deref().and_then(|p| index.get(p).copied());
    }
    path.reverse();
    path
}

fn session_context_settings(path: &[&SessionEntry]) -> (Option<String>, Option<(String, String)>) {
    let mut thinking_level = None;
    let mut model = None;
    for entry in path {
        match &entry.kind {
            EntryKind::ThinkingLevelChange { thinking_level: level } => thinking_level = Some(level.clone()),
            EntryKind::ModelChange { provider, model_id } => model = Some((provider.clone(), model_id.clone())),
            EntryKind::Message { message: AgentMessage::Assistant(a) } => {
                model = Some((a.provider.clone(), a.model.clone()))
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

/// Project one selected entry into LLM/runtime messages. Plain `custom`
/// entries are state/display only and produce nothing.
pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    match &entry.kind {
        EntryKind::Message { message } => vec![message.clone()],
        EntryKind::CustomMessage { custom_type, content, display, details } => vec![AgentMessage::Custom(CustomMessage {
            custom_type: custom_type.clone(),
            content: content.clone(),
            display: *display,
            details: details.clone(),
            timestamp: entry.timestamp_ms(),
        })],
        EntryKind::BranchSummary { summary, from_id, .. } if !summary.is_empty() => {
            vec![AgentMessage::BranchSummary(BranchSummaryMessage {
                summary: summary.clone(),
                from_id: from_id.clone(),
                timestamp: entry.timestamp_ms(),
            })]
        }
        EntryKind::Compaction { summary, tokens_before, system_message, .. } => {
            let summary = AgentMessage::CompactionSummary(CompactionSummaryMessage {
                summary: summary.clone(),
                tokens_before: *tokens_before,
                timestamp: entry.timestamp_ms(),
            });
            match system_message {
                Some(sys) => vec![AgentMessage::System(sys.clone()), summary],
                None => vec![summary],
            }
        }
        _ => Vec::new(),
    }
}

/// Active, compaction-aware entry list for the path ending at `leaf_id`.
///
/// If the path contains compaction entries, the latest one comes first,
/// followed by the kept entries starting at `firstKeptEntryId` (system
/// messages excluded, they are folded into the compaction's checkpoint) and
/// every entry after the compaction.
pub fn build_context_entries<'a>(entries: &'a [SessionEntry], leaf_id: Option<&str>) -> Vec<&'a SessionEntry> {
    let index = build_index(entries);
    let path = build_session_path(&index, leaf_id);
    let Some(compaction_idx) = path.iter().rposition(|e| matches!(e.kind, EntryKind::Compaction { .. })) else {
        return path;
    };
    let compaction = path[compaction_idx];
    let EntryKind::Compaction { first_kept_entry_id, .. } = &compaction.kind else { unreachable!() };

    let mut out = vec![compaction];
    let mut found_first_kept = false;
    for entry in &path[..compaction_idx] {
        if entry.id == *first_kept_entry_id {
            found_first_kept = true;
        }
        if found_first_kept && !entry.is_system_message() {
            out.push(entry);
        }
    }
    out.extend_from_slice(&path[compaction_idx + 1..]);
    out
}

/// Messages + settings for the LLM, following the path from root to `leaf_id`.
pub fn build_session_context(entries: &[SessionEntry], leaf_id: Option<&str>) -> SessionContext {
    let index = build_index(entries);
    let path = build_session_path(&index, leaf_id);
    let (thinking_level, model) = session_context_settings(&path);
    let messages = build_context_entries(entries, leaf_id).iter().flat_map(|e| session_entry_to_context_messages(e)).collect();
    SessionContext { messages, thinking_level, model }
}

/// Replay every system message into one message holding the current prompt
/// and tools (port of pi-ai's `getCurrentSystemMessage`). Later `content` is
/// appended to the base prompt, `sections` are patched by name.
pub fn get_current_system_message(messages: &[AgentMessage]) -> Option<SystemMessage> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut tools: Vec<Tool> = Vec::new();
    let mut timestamp: Option<u64> = None;
    for message in messages {
        let AgentMessage::System(sys) = message else { continue };
        timestamp.get_or_insert(sys.timestamp);
        let text = sys.content.plain_text();
        if !text.is_empty() {
            content.push(text);
        }
        if let Some(patch) = &sys.sections {
            for (name, value) in patch {
                match value {
                    None => sections.retain(|(n, _)| n != name),
                    Some(v) => match sections.iter_mut().find(|(n, _)| n == name) {
                        Some(slot) => slot.1 = v.clone(),
                        None => sections.push((name.clone(), v.clone())),
                    },
                }
            }
        }
        if let Some(added) = &sys.tools_added {
            for tool in added {
                match tools.iter_mut().find(|t| t.name == tool.name) {
                    Some(slot) => *slot = tool.clone(),
                    None => tools.push(tool.clone()),
                }
            }
        }
    }
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }
    Some(SystemMessage {
        content: UserContent::Text(content.join("\n\n")),
        sections: if sections.is_empty() {
            None
        } else {
            Some(sections.into_iter().map(|(k, v)| (k, Some(v))).collect())
        },
        tools_added: if tools.is_empty() { None } else { Some(tools) },
        timestamp: timestamp.unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// SessionManager
// ---------------------------------------------------------------------------

/// Manages one conversation session as an append-only tree stored in a JSONL
/// file. See the module docs for the tree semantics.
#[derive(Debug)]
pub struct SessionManager {
    session_id: String,
    session_file: Option<PathBuf>,
    session_dir: PathBuf,
    cwd: String,
    persist: bool,
    /// Whether the on-disk file currently mirrors `header` + `entries`.
    flushed: bool,
    header: SessionHeader,
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, usize>,
    /// target entry id -> (label, timestamp of the label change)
    labels: HashMap<String, (String, String)>,
    leaf_id: Option<String>,
}

impl SessionManager {
    // ----- construction -----------------------------------------------------

    fn blank(cwd: &str, session_dir: PathBuf, persist: bool, options: &NewSessionOptions) -> Result<Self> {
        let cwd = path_string(&resolve_path(cwd));
        if persist && !session_dir.as_os_str().is_empty() && !session_dir.exists() {
            fs::create_dir_all(&session_dir).with_context(|| format!("creating {}", session_dir.display()))?;
        }
        let mut mgr = SessionManager {
            session_id: String::new(),
            session_file: None,
            session_dir,
            cwd,
            persist,
            flushed: false,
            header: SessionHeader {
                entry_type: "session".into(),
                version: Some(CURRENT_SESSION_VERSION),
                id: String::new(),
                timestamp: String::new(),
                cwd: String::new(),
                parent_session: None,
                extra: Map::new(),
            },
            entries: Vec::new(),
            by_id: HashMap::new(),
            labels: HashMap::new(),
            leaf_id: None,
        };
        mgr.new_session(options)?;
        Ok(mgr)
    }

    /// New session in `session_dir` (default: `~/.pi/agent/sessions/--<cwd>--/`).
    pub fn create(cwd: &str, session_dir: Option<&Path>) -> Result<Self> {
        Self::create_with_options(cwd, session_dir, &NewSessionOptions::default())
    }

    pub fn create_with_options(cwd: &str, session_dir: Option<&Path>, options: &NewSessionOptions) -> Result<Self> {
        let dir = match session_dir {
            Some(d) => resolve_path(d),
            None => get_default_session_dir(cwd)?,
        };
        Self::blank(cwd, dir, true, options)
    }

    /// Open an existing session file (or start a new session at that path if
    /// it does not exist yet). cwd comes from the header; the session dir is
    /// the file's parent directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, None, None)
    }

    /// `open` with an explicit session dir (for `/new`) and/or cwd override.
    pub fn open_with(path: impl AsRef<Path>, session_dir: Option<&Path>, cwd_override: Option<&str>) -> Result<Self> {
        let resolved = resolve_path(path);
        let loaded = load_entries_from_file(&resolved)?;
        let cwd = match cwd_override {
            Some(c) => c.to_string(),
            None => loaded
                .as_ref()
                .map(|l| l.header.cwd.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| path_string(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))),
        };
        let dir = match session_dir {
            Some(d) => resolve_path(d),
            None => resolved.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")),
        };
        let mut mgr = Self::blank(&cwd, dir, true, &NewSessionOptions::default())?;
        mgr.set_session_file_preloaded(resolved, loaded)?;
        Ok(mgr)
    }

    /// Continue the most recent session for `cwd`, or create a new one.
    pub fn continue_recent(cwd: &str, session_dir: Option<&Path>) -> Result<Self> {
        let dir = match session_dir {
            Some(d) => resolve_path(d),
            None => get_default_session_dir(cwd)?,
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd);
        match find_most_recent_session(&dir, if filter_cwd { Some(cwd) } else { None }) {
            Some(path) => Self::open_with(path, Some(&dir), Some(cwd)),
            None => Self::blank(cwd, dir, true, &NewSessionOptions::default()),
        }
    }

    /// Session without file persistence.
    pub fn in_memory(cwd: &str) -> Result<Self> {
        Self::blank(cwd, PathBuf::new(), false, &NewSessionOptions::default())
    }

    /// Reset to a brand-new session (new id, header, empty tree). Returns the
    /// new file path for persisted sessions. The file is created lazily.
    pub fn new_session(&mut self, options: &NewSessionOptions) -> Result<Option<PathBuf>> {
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        self.session_id = options.id.clone().unwrap_or_else(create_session_id);
        let timestamp = now_iso();
        self.header = SessionHeader {
            entry_type: "session".into(),
            version: Some(CURRENT_SESSION_VERSION),
            id: self.session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            parent_session: options.parent_session.clone(),
            extra: Map::new(),
        };
        self.entries.clear();
        self.by_id.clear();
        self.labels.clear();
        self.leaf_id = None;
        self.flushed = false;
        self.session_file = if self.persist {
            Some(self.session_dir.join(format!("{}_{}.jsonl", file_timestamp(&timestamp), self.session_id)))
        } else {
            None
        };
        Ok(self.session_file.clone())
    }

    /// Switch to a different session file (used for resume and branching).
    pub fn set_session_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let resolved = resolve_path(path);
        let loaded = load_entries_from_file(&resolved)?;
        self.set_session_file_preloaded(resolved, loaded)
    }

    fn set_session_file_preloaded(&mut self, resolved: PathBuf, loaded: Option<LoadedFile>) -> Result<()> {
        if resolved.exists() {
            match loaded {
                Some(loaded) => {
                    self.session_file = Some(resolved);
                    self.load_entries(loaded)?;
                    self.flushed = true;
                }
                None => {
                    // Empty file: initialise it. Non-empty but not a session: refuse.
                    if fs::metadata(&resolved)?.len() > 0 {
                        bail!("Session file is not a valid {APP_NAME} session: {}", resolved.display());
                    }
                    self.new_session(&NewSessionOptions::default())?;
                    self.session_file = Some(resolved);
                    self.rewrite_file()?;
                    self.flushed = true;
                }
            }
        } else {
            self.new_session(&NewSessionOptions::default())?;
            self.session_file = Some(resolved); // keep the explicit path
        }
        Ok(())
    }

    fn load_entries(&mut self, loaded: LoadedFile) -> Result<()> {
        self.session_id = loaded.header.id.clone();
        self.header = loaded.header;
        self.entries = loaded.entries;
        if loaded.migrated {
            self.rewrite_file()?;
        }
        self.build_index();
        Ok(())
    }

    fn build_index(&mut self) {
        self.by_id.clear();
        self.labels.clear();
        self.leaf_id = None;
        for (i, entry) in self.entries.iter().enumerate() {
            self.by_id.insert(entry.id.clone(), i);
            self.leaf_id = Some(entry.id.clone());
            if let EntryKind::Label { target_id, label } = &entry.kind {
                match label.as_deref().filter(|l| !l.is_empty()) {
                    Some(l) => {
                        self.labels.insert(target_id.clone(), (l.to_string(), entry.timestamp.clone()));
                    }
                    None => {
                        self.labels.remove(target_id);
                    }
                }
            }
        }
    }

    fn write_all_entries(&self, file: &mut fs::File) -> Result<()> {
        let mut w = std::io::BufWriter::new(file);
        write_line(&mut w, &self.header)?;
        for entry in &self.entries {
            write_line(&mut w, entry)?;
        }
        w.flush()?;
        Ok(())
    }

    fn rewrite_file(&self) -> Result<()> {
        let (true, Some(path)) = (self.persist, &self.session_file) else { return Ok(()) };
        let mut file = fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
        self.write_all_entries(&mut file)
    }

    /// Persist a newly appended entry. The file is only created once the
    /// session holds an assistant message; until then entries stay in memory
    /// and are flushed together with the first assistant response.
    fn persist_entry(&mut self, idx: usize) -> Result<()> {
        if !self.persist {
            return Ok(());
        }
        let Some(path) = self.session_file.clone() else { return Ok(()) };
        let has_assistant = self.entries.iter().any(SessionEntry::is_assistant_message);
        if !has_assistant {
            if self.flushed {
                self.append_line(&path, idx)?;
            } else {
                self.flushed = false;
            }
            return Ok(());
        }
        if !self.flushed {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("creating {}", path.display()))?;
            self.write_all_entries(&mut file)?;
            self.flushed = true;
        } else {
            self.append_line(&path, idx)?;
        }
        Ok(())
    }

    fn append_line(&self, path: &Path, idx: usize) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .with_context(|| format!("appending to {}", path.display()))?;
        write_line(&mut file, &self.entries[idx])
    }

    fn next_id(&self) -> String {
        generate_id(|id| self.by_id.contains_key(id))
    }

    fn append_entry(&mut self, kind: EntryKind) -> Result<String> {
        let entry = SessionEntry { id: self.next_id(), parent_id: self.leaf_id.clone(), timestamp: now_iso(), kind };
        let id = entry.id.clone();
        self.entries.push(entry);
        let idx = self.entries.len() - 1;
        self.by_id.insert(id.clone(), idx);
        self.leaf_id = Some(id.clone());
        self.persist_entry(idx)?;
        Ok(id)
    }

    // ----- accessors --------------------------------------------------------

    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    pub fn get_session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    /// `None` for in-memory sessions.
    pub fn get_session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }

    pub fn get_header(&self) -> &SessionHeader {
        &self.header
    }

    /// All entries in file order (header excluded).
    pub fn get_entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        self.leaf_id.as_deref().and_then(|id| self.get_entry(id))
    }

    pub fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.by_id.get(id).map(|&i| &self.entries[i])
    }

    pub fn get_children(&self, parent_id: &str) -> Vec<&SessionEntry> {
        self.entries.iter().filter(|e| e.parent_id.as_deref() == Some(parent_id)).collect()
    }

    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.labels.get(id).map(|(l, _)| l.as_str())
    }

    /// Path from root to the current leaf (all entry types).
    pub fn get_branch(&self) -> Vec<&SessionEntry> {
        self.get_branch_from(self.leaf_id.as_deref())
    }

    /// Path from root to `from_id` (`None` -> empty).
    pub fn get_branch_from(&self, from_id: Option<&str>) -> Vec<&SessionEntry> {
        let mut path = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut current = from_id.and_then(|id| self.get_entry(id));
        while let Some(entry) = current {
            if !seen.insert(entry.id.as_str()) {
                break;
            }
            path.push(entry);
            current = entry.parent_id.as_deref().and_then(|p| self.get_entry(p));
        }
        path.reverse();
        path
    }

    /// Active, compaction-aware entries for rendering.
    pub fn build_context_entries(&self) -> Vec<&SessionEntry> {
        build_context_entries(&self.entries, self.leaf_id.as_deref())
    }

    /// Messages and settings for the LLM (current branch, compaction applied).
    pub fn build_session_context(&self) -> SessionContext {
        build_session_context(&self.entries, self.leaf_id.as_deref())
    }

    /// Display name from the latest `session_info` entry (empty names clear it).
    pub fn get_session_name(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|e| match &e.kind {
            EntryKind::SessionInfo { name } => {
                Some(name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(String::from))
            }
            _ => None,
        })?
    }

    // ----- appending --------------------------------------------------------

    /// Append a message as child of the leaf and advance the leaf. Returns the
    /// entry id. Compaction and branch summaries must go through
    /// `append_compaction` / `append_branch_summary` so they stay top-level
    /// entries.
    pub fn append_message(&mut self, message: AgentMessage) -> Result<String> {
        self.append_entry(EntryKind::Message { message })
    }

    pub fn append_thinking_level_change(&mut self, thinking_level: &str) -> Result<String> {
        self.append_entry(EntryKind::ThinkingLevelChange { thinking_level: thinking_level.to_string() })
    }

    pub fn append_model_change(&mut self, provider: &str, model_id: &str) -> Result<String> {
        self.append_entry(EntryKind::ModelChange { provider: provider.to_string(), model_id: model_id.to_string() })
    }

    /// Record a compaction. The current system prompt/tool state is captured
    /// as the entry's `systemMessage` checkpoint, like pi does.
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: u64,
        details: Option<Value>,
    ) -> Result<String> {
        self.append_compaction_full(summary, first_kept_entry_id, tokens_before, details, None, None)
    }

    pub fn append_compaction_full(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: u64,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String> {
        let timestamp = now_iso();
        let system_message = get_current_system_message(&self.build_session_context().messages).map(|mut sys| {
            sys.timestamp = iso_to_ms(&timestamp);
            sys
        });
        let kind = EntryKind::Compaction {
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id.to_string(),
            tokens_before,
            details,
            usage,
            from_hook,
            system_message,
        };
        let entry = SessionEntry { id: self.next_id(), parent_id: self.leaf_id.clone(), timestamp, kind };
        let id = entry.id.clone();
        self.entries.push(entry);
        let idx = self.entries.len() - 1;
        self.by_id.insert(id.clone(), idx);
        self.leaf_id = Some(id.clone());
        self.persist_entry(idx)?;
        Ok(id)
    }

    /// Extension state entry (not part of LLM context).
    pub fn append_custom_entry(&mut self, custom_type: &str, data: Option<Value>) -> Result<String> {
        self.append_entry(EntryKind::Custom { custom_type: custom_type.to_string(), data })
    }

    /// Extension message that participates in LLM context.
    pub fn append_custom_message_entry(
        &mut self,
        custom_type: &str,
        content: UserContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String> {
        self.append_entry(EntryKind::CustomMessage { custom_type: custom_type.to_string(), content, display, details })
    }

    /// Set the display name (`session_info` entry). Newlines are collapsed.
    pub fn append_session_info(&mut self, name: &str) -> Result<String> {
        self.append_entry(EntryKind::SessionInfo { name: Some(sanitize_name(name)) })
    }

    pub fn set_session_name(&mut self, name: &str) -> Result<String> {
        self.append_session_info(name)
    }

    /// Set (`Some(non-empty)`) or clear (`None` / empty) a label on an entry.
    pub fn set_label(&mut self, target_id: &str, label: Option<&str>) -> Result<String> {
        if !self.by_id.contains_key(target_id) {
            bail!("Entry {target_id} not found");
        }
        let id = self.append_entry(EntryKind::Label { target_id: target_id.to_string(), label: label.map(String::from) })?;
        let timestamp = self.entries.last().map(|e| e.timestamp.clone()).unwrap_or_default();
        match label.filter(|l| !l.is_empty()) {
            Some(l) => {
                self.labels.insert(target_id.to_string(), (l.to_string(), timestamp));
            }
            None => {
                self.labels.remove(target_id);
            }
        }
        Ok(id)
    }

    /// Alias of `set_label` (pi: `appendLabelChange`).
    pub fn append_label_change(&mut self, target_id: &str, label: Option<&str>) -> Result<String> {
        self.set_label(target_id, label)
    }

    // ----- branching --------------------------------------------------------

    /// Move the leaf to an earlier entry; the next append becomes a sibling
    /// branch. Nothing is modified or deleted.
    pub fn branch(&mut self, entry_id: &str) -> Result<()> {
        if !self.by_id.contains_key(entry_id) {
            bail!("Entry {entry_id} not found");
        }
        self.leaf_id = Some(entry_id.to_string());
        Ok(())
    }

    /// Reset the leaf so the next append starts a new root.
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    /// Branch to `branch_from_id` (`None` = new root) and append a
    /// `branch_summary` entry describing the abandoned path (pi:
    /// `branchWithSummary`). Returns the new entry id.
    pub fn append_branch_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
    ) -> Result<String> {
        self.append_branch_summary_full(branch_from_id, summary, details, None, None)
    }

    pub fn append_branch_summary_full(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String> {
        if let Some(id) = branch_from_id {
            if !self.by_id.contains_key(id) {
                bail!("Entry {id} not found");
            }
        }
        let from_id = self.leaf_id.clone().unwrap_or_else(|| "root".to_string());
        self.leaf_id = branch_from_id.map(String::from);
        self.append_entry(EntryKind::BranchSummary {
            from_id: Some(from_id),
            summary: summary.to_string(),
            details,
            usage,
            from_hook,
        })
    }

    /// Create a new session containing only the path from root to `leaf_id`
    /// (pi: `createBranchedSession`, used by `/fork`). Label entries are
    /// re-created for entries on the path. The returned manager shares this
    /// session's cwd/session dir; its header's `parentSession` points at this
    /// file. `self` is left untouched. The file is created lazily (or right
    /// away when the path already holds an assistant message).
    pub fn fork(&self, leaf_id: &str) -> Result<SessionManager> {
        let path = self.get_branch_from(Some(leaf_id));
        if path.is_empty() {
            bail!("Entry {leaf_id} not found");
        }

        // Labels are tree entries, so dropping them requires re-chaining and
        // repointing compaction ranges that referenced a label entry.
        let mut path_without_labels: Vec<SessionEntry> = Vec::new();
        let mut replacement_by_label_id: HashMap<String, String> = HashMap::new();
        let mut pending_label_ids: Vec<String> = Vec::new();
        let mut parent_id: Option<String> = None;
        for entry in path {
            if matches!(entry.kind, EntryKind::Label { .. }) {
                pending_label_ids.push(entry.id.clone());
                continue;
            }
            for label_id in pending_label_ids.drain(..) {
                replacement_by_label_id.insert(label_id, entry.id.clone());
            }
            let mut copy = entry.clone();
            copy.parent_id = parent_id.clone();
            if let EntryKind::Compaction { first_kept_entry_id, .. } = &mut copy.kind {
                if let Some(r) = replacement_by_label_id.get(first_kept_entry_id) {
                    *first_kept_entry_id = r.clone();
                }
            }
            parent_id = Some(entry.id.clone());
            path_without_labels.push(copy);
        }

        let new_session_id = create_session_id();
        let timestamp = now_iso();
        let header = SessionHeader {
            entry_type: "session".into(),
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.clone(),
            parent_session: if self.persist { self.session_file.as_ref().map(|p| path_string(p)) } else { None },
            extra: Map::new(),
        };

        let mut ids: HashSet<String> = path_without_labels.iter().map(|e| e.id.clone()).collect();
        let mut entries = path_without_labels;
        let mut label_parent = entries.last().map(|e| e.id.clone());
        let mut labels: Vec<(&String, &(String, String))> =
            self.labels.iter().filter(|(target, _)| ids.contains(*target)).collect();
        labels.sort_by(|a, b| a.1 .1.cmp(&b.1 .1));
        for (target_id, (label, label_ts)) in labels {
            let id = generate_id(|c| ids.contains(c));
            ids.insert(id.clone());
            entries.push(SessionEntry {
                id: id.clone(),
                parent_id: label_parent.clone(),
                timestamp: label_ts.clone(),
                kind: EntryKind::Label { target_id: target_id.clone(), label: Some(label.clone()) },
            });
            label_parent = Some(id);
        }

        let mut mgr = SessionManager {
            session_id: new_session_id.clone(),
            session_file: if self.persist {
                Some(self.session_dir.join(format!("{}_{}.jsonl", file_timestamp(&timestamp), new_session_id)))
            } else {
                None
            },
            session_dir: self.session_dir.clone(),
            cwd: self.cwd.clone(),
            persist: self.persist,
            flushed: false,
            header,
            entries,
            by_id: HashMap::new(),
            labels: HashMap::new(),
            leaf_id: None,
        };
        mgr.build_index();
        if mgr.persist && mgr.entries.iter().any(SessionEntry::is_assistant_message) {
            mgr.rewrite_file()?;
            mgr.flushed = true;
        }
        Ok(mgr)
    }

    /// Fork a session file from another project into `target_cwd`, copying the
    /// full history (pi: `forkFrom`).
    pub fn fork_from(source_path: impl AsRef<Path>, target_cwd: &str, session_dir: Option<&Path>) -> Result<Self> {
        let source = resolve_path(source_path);
        let target_cwd = path_string(&resolve_path(target_cwd));
        let loaded = load_entries_from_file(&source)?
            .ok_or_else(|| anyhow!("Cannot fork: source session file is empty or invalid: {}", source.display()))?;
        let dir = match session_dir {
            Some(d) => resolve_path(d),
            None => get_default_session_dir(&target_cwd)?,
        };
        fs::create_dir_all(&dir)?;
        let new_session_id = create_session_id();
        let timestamp = now_iso();
        let new_file = dir.join(format!("{}_{}.jsonl", file_timestamp(&timestamp), new_session_id));
        let header = SessionHeader {
            entry_type: "session".into(),
            version: Some(CURRENT_SESSION_VERSION),
            id: new_session_id,
            timestamp,
            cwd: target_cwd.clone(),
            parent_session: Some(path_string(&source)),
            extra: Map::new(),
        };
        {
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&new_file)?;
            let mut w = std::io::BufWriter::new(&mut file);
            write_line(&mut w, &header)?;
            for entry in &loaded.entries {
                write_line(&mut w, entry)?;
            }
            w.flush()?;
        }
        Self::open_with(new_file, Some(&dir), Some(&target_cwd))
    }

    // ----- listing ----------------------------------------------------------

    /// Path of the session with exactly this id, without loading transcripts.
    pub fn find_by_id(cwd: &str, id: &str, session_dir: Option<&Path>) -> Result<Option<PathBuf>> {
        let dir = match session_dir {
            Some(d) => resolve_path(d),
            None => get_default_session_dir(cwd)?,
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd);
        let resolved_cwd = resolve_path(cwd);
        for path in jsonl_files(&dir) {
            let Some(header) = read_session_header_for_discovery(&path) else { continue };
            if header.id != id {
                continue;
            }
            if filter_cwd && !session_cwd_matches(&header.cwd, &resolved_cwd) {
                continue;
            }
            return Ok(Some(path));
        }
        Ok(None)
    }

    /// Sessions for `cwd`, newest first. With a custom `session_dir` that is
    /// not the default dir for `cwd`, only sessions whose header cwd matches
    /// are returned.
    pub fn list(cwd: &str, session_dir: Option<&Path>) -> Result<Vec<SessionInfo>> {
        let dir = match session_dir {
            Some(d) => resolve_path(d),
            None => get_default_session_dir(cwd)?,
        };
        let filter_cwd = session_dir.is_some() && dir != get_default_session_dir_path(cwd);
        let resolved_cwd = resolve_path(cwd);
        let mut sessions: Vec<SessionInfo> = list_sessions_from_dir(&dir)
            .into_iter()
            .filter(|s| !filter_cwd || session_cwd_matches(&s.cwd, &resolved_cwd))
            .collect();
        sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
        Ok(sessions)
    }

    /// Sessions across all projects (every sub-directory of the sessions
    /// root), newest first. With `session_dir`, lists that directory only.
    pub fn list_all(session_dir: Option<&Path>) -> Vec<SessionInfo> {
        let mut sessions = match session_dir {
            Some(d) => list_sessions_from_dir(&resolve_path(d)),
            None => {
                let root = get_sessions_dir();
                let Ok(rd) = fs::read_dir(&root) else { return Vec::new() };
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .flat_map(|p| list_sessions_from_dir(&p))
                    .collect()
            }
        };
        sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
        sessions
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::{AssistantMessage, Content, StopReason};
    use serde_json::json;

    fn assistant(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![Content::text(text)],
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            response_id: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            raw_stop_reason: None,
            timestamp: pi_ai::now_ms(),
        })
    }

    fn user(text: &str) -> AgentMessage {
        AgentMessage::user(text)
    }

    fn texts(ctx: &SessionContext) -> Vec<String> {
        ctx.messages
            .iter()
            .map(|m| match m {
                AgentMessage::User(u) => format!("user:{}", u.content.plain_text()),
                AgentMessage::Assistant(a) => format!("assistant:{}", a.text()),
                AgentMessage::System(s) => format!("system:{}", s.content.plain_text()),
                AgentMessage::CompactionSummary(c) => format!("compaction:{}", c.summary),
                AgentMessage::BranchSummary(b) => format!("branch:{}", b.summary),
                AgentMessage::Custom(c) => format!("custom:{}", c.content.plain_text()),
                other => format!("{}:", other.role()),
            })
            .collect()
    }

    fn read_values(path: &Path) -> Vec<Value> {
        parse_session_values(&fs::read_to_string(path).unwrap())
    }

    #[test]
    fn session_dir_encoding_matches_pi() {
        assert_eq!(encode_cwd_dir_name("/Users/me/proj"), "--Users-me-proj--");
        assert_eq!(encode_cwd_dir_name("C:\\work\\x"), "--C--work-x--");
        let dir = get_default_session_dir_path_in("/tmp/x", Path::new("/agent"));
        assert_eq!(dir, PathBuf::from("/agent/sessions/--tmp-x--"));
        assert_eq!(file_timestamp("2024-12-03T14:00:00.000Z"), "2024-12-03T14-00-00-000Z");
        assert_eq!(resolve_path("/a/b/../c/./d"), PathBuf::from("/a/c/d"));
    }

    #[test]
    fn append_and_reopen_preserves_branch_and_lazy_file_creation() {
        let root = tempfile::tempdir().unwrap();
        let mut mgr = SessionManager::create("/tmp/project", Some(root.path())).unwrap();
        let file = mgr.get_session_file().unwrap().to_path_buf();
        assert!(file.starts_with(root.path()));
        assert!(file.file_name().unwrap().to_string_lossy().ends_with(&format!("_{}.jsonl", mgr.get_session_id())));
        assert_eq!(mgr.get_cwd(), "/tmp/project");
        assert!(Uuid::parse_str(mgr.get_session_id()).is_ok());

        let u1 = mgr.append_message(user("hello")).unwrap();
        assert_eq!(u1.len(), 8);
        assert!(!file.exists(), "file must not exist before the first assistant message");
        let a1 = mgr.append_message(assistant("hi there")).unwrap();
        assert!(file.exists());
        let u2 = mgr.append_message(user("second")).unwrap();
        mgr.append_thinking_level_change("high").unwrap();
        mgr.append_model_change("openai", "gpt-4o").unwrap();

        let lines = read_values(&file);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0]["type"], "session");
        assert_eq!(lines[0]["version"], 3);
        assert_eq!(lines[0]["cwd"], "/tmp/project");
        assert_eq!(lines[1]["parentId"], Value::Null);
        assert_eq!(lines[2]["parentId"], u1);
        assert_eq!(lines[1]["message"]["role"], "user");
        // key order matches pi: type, id, parentId, timestamp, payload
        let raw = fs::read_to_string(&file).unwrap();
        assert!(raw.lines().nth(1).unwrap().starts_with(&format!("{{\"type\":\"message\",\"id\":\"{u1}\",\"parentId\":null,\"timestamp\":\"")));

        let reopened = SessionManager::open(&file).unwrap();
        assert_eq!(reopened.get_session_id(), mgr.get_session_id());
        assert_eq!(reopened.get_cwd(), "/tmp/project");
        assert_eq!(reopened.get_session_file(), Some(file.as_path()));
        let branch: Vec<&str> = reopened.get_branch().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(branch[..3], [u1.as_str(), a1.as_str(), u2.as_str()]);
        assert_eq!(branch.len(), 5);
        assert_eq!(reopened.get_leaf_id(), mgr.get_leaf_id());
        let ctx = reopened.build_session_context();
        assert_eq!(texts(&ctx), ["user:hello", "assistant:hi there", "user:second"]);
        assert_eq!(ctx.thinking_level.as_deref(), Some("high"));
        assert_eq!(ctx.model, Some(("openai".into(), "gpt-4o".into())));
        assert_eq!(reopened.get_children(&a1).len(), 1);

        // Appending to a reopened session appends a single line.
        let mut reopened = reopened;
        reopened.append_message(user("third")).unwrap();
        assert_eq!(read_values(&file).len(), 7);
    }

    #[test]
    fn branching_creates_sibling_and_context_follows_new_branch() {
        let root = tempfile::tempdir().unwrap();
        let mut mgr = SessionManager::create("/tmp/project", Some(root.path())).unwrap();
        let u1 = mgr.append_message(user("q1")).unwrap();
        let a1 = mgr.append_message(assistant("a1")).unwrap();
        let _u2 = mgr.append_message(user("q2")).unwrap();
        let _a2 = mgr.append_message(assistant("a2")).unwrap();

        mgr.branch(&a1).unwrap();
        assert_eq!(mgr.get_leaf_id(), Some(a1.as_str()));
        let u3 = mgr.append_message(user("q3 alt")).unwrap();
        let children: Vec<&str> = mgr.get_children(&a1).iter().map(|e| e.id.as_str()).collect();
        assert_eq!(children.len(), 2);
        assert!(children.contains(&u3.as_str()));
        assert_eq!(mgr.get_entry(&u3).unwrap().parent_id.as_deref(), Some(a1.as_str()));

        let ctx = mgr.build_session_context();
        assert_eq!(texts(&ctx), ["user:q1", "assistant:a1", "user:q3 alt"]);
        assert!(mgr.branch("nope").is_err());

        // Branch with summary back to the first user message.
        let bs = mgr.append_branch_summary(Some(&u1), "explored alt", None).unwrap();
        let entry = mgr.get_entry(&bs).unwrap();
        assert_eq!(entry.parent_id.as_deref(), Some(u1.as_str()));
        assert!(matches!(&entry.kind, EntryKind::BranchSummary { from_id: Some(f), .. } if f == &u3));
        let ctx = mgr.build_session_context();
        assert_eq!(texts(&ctx), ["user:q1", "branch:explored alt"]);

        // reset_leaf makes the next entry a new root
        mgr.reset_leaf();
        let r = mgr.append_message(user("fresh")).unwrap();
        assert_eq!(mgr.get_entry(&r).unwrap().parent_id, None);
        assert_eq!(texts(&mgr.build_session_context()), ["user:fresh"]);

        // Everything survives a reopen.
        let file = mgr.get_session_file().unwrap().to_path_buf();
        let reopened = SessionManager::open(&file).unwrap();
        assert_eq!(reopened.get_entries().len(), mgr.get_entries().len());
        assert_eq!(reopened.get_children(&a1).len(), 2);
    }

    #[test]
    fn compaction_is_applied_in_context() {
        let mut mgr = SessionManager::in_memory("/tmp/project").unwrap();
        assert!(mgr.get_session_file().is_none());
        mgr.append_message(AgentMessage::System(SystemMessage {
            content: UserContent::Text("You are helpful".into()),
            sections: None,
            tools_added: None,
            timestamp: 1,
        }))
        .unwrap();
        mgr.append_message(user("q1")).unwrap();
        mgr.append_message(assistant("a1")).unwrap();
        let u2 = mgr.append_message(user("q2")).unwrap();
        mgr.append_message(assistant("a2")).unwrap();
        let c = mgr
            .append_compaction("summary of q1/a1", &u2, 1234, Some(json!({"readFiles": ["x.rs"]})))
            .unwrap();
        mgr.append_message(user("q3")).unwrap();

        let entries: Vec<&str> = mgr.build_context_entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(entries[0], c);
        assert_eq!(entries.len(), 4); // compaction, q2, a2, q3

        let ctx = mgr.build_session_context();
        assert_eq!(
            texts(&ctx),
            ["system:You are helpful", "compaction:summary of q1/a1", "user:q2", "assistant:a2", "user:q3"]
        );
        match &ctx.messages[1] {
            AgentMessage::CompactionSummary(cs) => assert_eq!(cs.tokens_before, 1234),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(ctx.model, Some(("anthropic".into(), "claude-sonnet-4-5".into())));
        assert_eq!(ctx.thinking_level, None);

        // The checkpoint was persisted on the entry and is written as a full
        // `{"role":"system",...}` message so pi can replay it.
        let line: Value = serde_json::from_str(&serde_json::to_string(mgr.get_entry(&c).unwrap()).unwrap()).unwrap();
        assert_eq!(line["systemMessage"]["role"], "system");
        assert_eq!(line["firstKeptEntryId"], u2);
        match &mgr.get_entry(&c).unwrap().kind {
            EntryKind::Compaction { system_message: Some(sys), details: Some(d), .. } => {
                assert_eq!(sys.content.plain_text(), "You are helpful");
                assert_eq!(d["readFiles"][0], "x.rs");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn labels_and_session_name_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let mut mgr = SessionManager::create("/tmp/project", Some(root.path())).unwrap();
        let u1 = mgr.append_message(user("q1")).unwrap();
        mgr.append_message(assistant("a1")).unwrap();
        assert_eq!(mgr.get_session_name(), None);
        mgr.set_session_name("Refactor\nauth  ").unwrap();
        assert_eq!(mgr.get_session_name().as_deref(), Some("Refactor auth"));
        mgr.set_label(&u1, Some("checkpoint-1")).unwrap();
        assert_eq!(mgr.get_label(&u1), Some("checkpoint-1"));
        assert!(mgr.set_label("missing", Some("x")).is_err());

        let file = mgr.get_session_file().unwrap().to_path_buf();
        let reopened = SessionManager::open(&file).unwrap();
        assert_eq!(reopened.get_session_name().as_deref(), Some("Refactor auth"));
        assert_eq!(reopened.get_label(&u1), Some("checkpoint-1"));

        let mut mgr = reopened;
        mgr.set_label(&u1, None).unwrap();
        mgr.set_session_name("").unwrap();
        assert_eq!(mgr.get_label(&u1), None);
        assert_eq!(mgr.get_session_name(), None);
        let reopened = SessionManager::open(&file).unwrap();
        assert_eq!(reopened.get_label(&u1), None);
        assert_eq!(reopened.get_session_name(), None);
        // Labels are not part of the LLM context.
        assert_eq!(texts(&reopened.build_session_context()), ["user:q1", "assistant:a1"]);
    }

    #[test]
    fn list_and_continue_recent_find_sessions() {
        let root = tempfile::tempdir().unwrap();
        assert!(SessionManager::list("/tmp/project", Some(root.path())).unwrap().is_empty());

        let mut first = SessionManager::create("/tmp/project", Some(root.path())).unwrap();
        first.append_message(user("first question about things")).unwrap();
        first.append_message(assistant("answer")).unwrap();
        first.set_session_name("named").unwrap();

        let mut second = SessionManager::create("/tmp/project", Some(root.path())).unwrap();
        second.append_message(user("newer")).unwrap();
        second.append_message(assistant("ok")).unwrap();

        // A session for another cwd in the same custom dir is filtered out.
        let mut other = SessionManager::create("/tmp/other", Some(root.path())).unwrap();
        other.append_message(user("other")).unwrap();
        other.append_message(assistant("ok")).unwrap();

        // Unflushed sessions (no assistant yet) have no file and are not listed.
        let mut pending = SessionManager::create("/tmp/project", Some(root.path())).unwrap();
        pending.append_message(user("pending")).unwrap();

        let sessions = SessionManager::list("/tmp/project", Some(root.path())).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, second.get_session_id());
        assert_eq!(sessions[0].first_message, "newer");
        assert_eq!(sessions[0].name, None);
        assert_eq!(sessions[1].id, first.get_session_id());
        assert_eq!(sessions[1].first_message, "first question about things");
        assert_eq!(sessions[1].name.as_deref(), Some("named"));
        assert_eq!(sessions[1].message_count, 2);
        assert_eq!(sessions[1].cwd, "/tmp/project");
        assert_eq!(sessions[1].all_messages_text, "first question about things answer");
        assert!(sessions[0].modified >= sessions[1].modified);

        assert_eq!(SessionManager::list_all(Some(root.path())).len(), 3);

        let resumed = SessionManager::continue_recent("/tmp/other", Some(root.path())).unwrap();
        assert_eq!(resumed.get_session_id(), other.get_session_id());
        assert_eq!(resumed.get_cwd(), "/tmp/other");
        let found = SessionManager::find_by_id("/tmp/project", first.get_session_id(), Some(root.path())).unwrap();
        assert_eq!(found.as_deref(), first.get_session_file());

        let empty = tempfile::tempdir().unwrap();
        let fresh = SessionManager::continue_recent("/tmp/project", Some(empty.path())).unwrap();
        assert!(fresh.get_entries().is_empty());
        assert!(fresh.get_session_file().unwrap().starts_with(empty.path()));
    }

    const PI_V3_SAMPLE: &str = concat!(
        r#"{"type":"session","version":3,"id":"0193abcd-0000-7000-8000-000000000000","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/path/to/project","parentSession":"/path/to/original/session.jsonl"}"#, "\n",
        r#"{"type":"message","id":"a0b1c2d3","parentId":null,"timestamp":"2024-12-03T14:00:00.000Z","message":{"role":"system","content":"","sections":{"preamble":"You are an expert coding assistant...","tools":"<tools>\n- read: ...\n</tools>","cwd":"/project"},"toolsAdded":[{"name":"read","description":"...","parameters":{}}],"timestamp":1733234400000}}"#, "\n",
        r#"{"type":"message","id":"a1b2c3d4","parentId":"a0b1c2d3","timestamp":"2024-12-03T14:00:01.000Z","message":{"role":"user","content":"Hello","timestamp":1733234401000}}"#, "\n",
        r#"{"type":"message","id":"b2c3d4e5","parentId":"a1b2c3d4","timestamp":"2024-12-03T14:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1733234402000}}"#, "\n",
        r#"{"type":"message","id":"c3d4e5f6","parentId":"b2c3d4e5","timestamp":"2024-12-03T14:00:03.000Z","message":{"role":"toolResult","toolCallId":"call_123","toolName":"bash","content":[{"type":"text","text":"output"}],"isError":false,"timestamp":1733234403000}}"#, "\n",
        r#"{"type":"model_change","id":"d4e5f6g7","parentId":"c3d4e5f6","timestamp":"2024-12-03T14:05:00.000Z","provider":"openai","modelId":"gpt-4o"}"#, "\n",
        r#"{"type":"thinking_level_change","id":"e5f6g7h8","parentId":"d4e5f6g7","timestamp":"2024-12-03T14:06:00.000Z","thinkingLevel":"high"}"#, "\n",
        r#"{"type":"compaction","id":"f6g7h8i9","parentId":"e5f6g7h8","timestamp":"2024-12-03T14:10:00.000Z","summary":"User discussed X, Y, Z...","firstKeptEntryId":"c3d4e5f6","tokensBefore":50000,"systemMessage":{"role":"system","content":"You are a coding assistant.","toolsAdded":[],"timestamp":1733235000000}}"#, "\n",
        r#"{"type":"custom","id":"h8i9j0k1","parentId":"f6g7h8i9","timestamp":"2024-12-03T14:20:00.000Z","customType":"my-extension","data":{"count":42}}"#, "\n",
        r#"{"type":"custom_message","id":"i9j0k1l2","parentId":"h8i9j0k1","timestamp":"2024-12-03T14:25:00.000Z","customType":"my-extension","content":"Injected context...","display":true}"#, "\n",
        r#"{"type":"label","id":"j0k1l2m3","parentId":"i9j0k1l2","timestamp":"2024-12-03T14:30:00.000Z","targetId":"a1b2c3d4","label":"checkpoint-1"}"#, "\n",
        r#"{"type":"session_info","id":"k1l2m3n4","parentId":"j0k1l2m3","timestamp":"2024-12-03T14:35:00.000Z","name":"Refactor auth module"}"#, "\n",
        r#"{"type":"future_entry","id":"l2m3n4o5","parentId":"k1l2m3n4","timestamp":"2024-12-03T14:40:00.000Z","payload":{"nested":[1,2,3]},"flag":true}"#, "\n",
        r#"{"type":"message","id":"m3n4o5p6","parentId":"l2m3n4o5","timestamp":"2024-12-03T14:41:00.000Z","message":{"role":"assistant","content":[],"api":"x","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"deferred","deferred":{"opaque":1},"timestamp":1733236860000}}"#, "\n",
        r#"{"type":"branch_summary","id":"g7h8i9j0","parentId":"a1b2c3d4","timestamp":"2024-12-03T14:15:00.000Z","fromId":"f6g7h8i9","summary":"Branch explored approach A..."}"#, "\n",
    );

    #[test]
    fn pi_v3_sample_loads_and_unknown_entries_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("2024-12-03T14-00-00-000Z_sample.jsonl");
        // No trailing newline on purpose: the loader must repair it.
        fs::write(&file, PI_V3_SAMPLE.trim_end()).unwrap();

        let mgr = SessionManager::open(&file).unwrap();
        assert_eq!(mgr.get_session_id(), "0193abcd-0000-7000-8000-000000000000");
        assert_eq!(mgr.get_cwd(), "/path/to/project");
        assert_eq!(mgr.get_header().parent_session.as_deref(), Some("/path/to/original/session.jsonl"));
        assert_eq!(mgr.get_entries().len(), 14);
        assert_eq!(mgr.get_session_name().as_deref(), Some("Refactor auth module"));
        assert_eq!(mgr.get_label("a1b2c3d4"), Some("checkpoint-1"));
        assert!(fs::read_to_string(&file).unwrap().ends_with('\n'));

        let unknown = mgr.get_entry("l2m3n4o5").unwrap();
        assert_eq!(unknown.type_name(), "future_entry");
        assert!(matches!(unknown.kind, EntryKind::Unknown(_)));
        // An entry of a known type whose payload we cannot represent is kept raw too.
        assert!(matches!(mgr.get_entry("m3n4o5p6").unwrap().kind, EntryKind::Unknown(_)));

        // Leaf is the last line: the branch_summary hanging off the first user message.
        let ctx = mgr.build_session_context();
        assert_eq!(texts(&ctx), ["system:", "user:Hello", "branch:Branch explored approach A..."]);
        assert_eq!(ctx.model, None);
        assert_eq!(ctx.thinking_level, None);

        // Follow the main branch to the unknown entry: compaction applies.
        let main_ctx = build_session_context(mgr.get_entries(), Some("l2m3n4o5"));
        assert_eq!(
            texts(&main_ctx),
            [
                "system:You are a coding assistant.",
                "compaction:User discussed X, Y, Z...",
                "toolResult:",
                "custom:Injected context...",
            ]
        );
        assert_eq!(main_ctx.thinking_level.as_deref(), Some("high"));
        assert_eq!(main_ctx.model, Some(("openai".into(), "gpt-4o".into())));
        let ctx_ids: Vec<&str> = build_context_entries(mgr.get_entries(), Some("l2m3n4o5")).iter().map(|e| e.id.as_str()).collect();
        // Compaction first, then everything from firstKeptEntryId (system messages excluded), then the rest.
        assert_eq!(
            ctx_ids,
            ["f6g7h8i9", "c3d4e5f6", "d4e5f6g7", "e5f6g7h8", "h8i9j0k1", "i9j0k1l2", "j0k1l2m3", "k1l2m3n4", "l2m3n4o5"]
        );

        // Every line round-trips to the same JSON value. (`Cost` is f64 in
        // pi-ai, so integral costs come back as `0.0`; JSON readers treat
        // that as `0`, and so does this comparison.)
        fn normalize(v: Value) -> Value {
            match v {
                Value::Number(n) => match n.as_f64() {
                    Some(f) if f.fract() == 0.0 && n.as_i64().is_none() => json!(f as i64),
                    _ => Value::Number(n),
                },
                Value::Array(a) => Value::Array(a.into_iter().map(normalize).collect()),
                Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, normalize(v))).collect()),
                other => other,
            }
        }
        let originals = parse_session_values(PI_V3_SAMPLE);
        let header_rt: Value = serde_json::from_str(&serde_json::to_string(mgr.get_header()).unwrap()).unwrap();
        assert_eq!(header_rt, originals[0]);
        for (entry, original) in mgr.get_entries().iter().zip(&originals[1..]) {
            let rt: Value = serde_json::from_str(&serde_json::to_string(entry).unwrap()).unwrap();
            assert_eq!(normalize(rt), normalize(original.clone()), "round trip of {}", entry.id);
        }

        // Fork keeps parentSession pointing at the source and drops label entries.
        let forked = mgr.fork("k1l2m3n4").unwrap();
        assert_eq!(forked.get_header().parent_session.as_deref(), Some(file.to_string_lossy().as_ref()));
        assert_ne!(forked.get_session_id(), mgr.get_session_id());
        assert!(forked.get_session_file().unwrap().exists());
        assert_eq!(forked.get_label("a1b2c3d4"), Some("checkpoint-1"));
        let forked_ids: Vec<&str> = forked.get_branch_from(Some("k1l2m3n4")).iter().map(|e| e.id.as_str()).collect();
        assert!(!forked_ids.contains(&"j0k1l2m3"));
        assert_eq!(forked_ids.last(), Some(&"k1l2m3n4"));
        let reopened = SessionManager::open(forked.get_session_file().unwrap()).unwrap();
        assert_eq!(reopened.get_entries().len(), forked.get_entries().len());
        assert_eq!(texts(&build_session_context(reopened.get_entries(), Some("k1l2m3n4"))), texts(&main_ctx)[..4]);
    }

    #[test]
    fn v1_and_v2_sessions_are_migrated_and_rewritten() {
        let root = tempfile::tempdir().unwrap();
        let v1 = root.path().join("v1.jsonl");
        fs::write(
            &v1,
            concat!(
                r#"{"type":"session","id":"old","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/p"}"#, "\n",
                r#"{"type":"message","timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"user","content":"a","timestamp":1}}"#, "\n",
                r#"{"type":"message","timestamp":"2024-01-01T00:00:02.000Z","message":{"role":"hookMessage","customType":"x","content":"c","display":false,"timestamp":2}}"#, "\n",
                r#"{"type":"compaction","timestamp":"2024-01-01T00:00:03.000Z","summary":"s","firstKeptEntryIndex":2,"tokensBefore":10}"#, "\n",
            ),
        )
        .unwrap();
        let mgr = SessionManager::open(&v1).unwrap();
        assert_eq!(mgr.get_header().version, Some(3));
        let entries = mgr.get_entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].parent_id, None);
        assert_eq!(entries[1].parent_id.as_deref(), Some(entries[0].id.as_str()));
        assert!(matches!(entries[1].kind, EntryKind::Message { message: AgentMessage::Custom(_) }));
        match &entries[2].kind {
            EntryKind::Compaction { first_kept_entry_id, .. } => assert_eq!(first_kept_entry_id, &entries[1].id),
            other => panic!("unexpected {other:?}"),
        }
        let rewritten = read_values(&v1);
        assert_eq!(rewritten[0]["version"], 3);
        assert_eq!(rewritten[2]["message"]["role"], "custom");
        assert!(rewritten[3].get("firstKeptEntryIndex").is_none());
        assert_eq!(texts(&mgr.build_session_context()), ["compaction:s", "custom:c"]);

        // Non-session content is refused; an empty file is initialised.
        let bogus = root.path().join("bogus.jsonl");
        fs::write(&bogus, "not json\n").unwrap();
        assert!(SessionManager::open(&bogus).is_err());
        let empty = root.path().join("empty.jsonl");
        fs::write(&empty, "").unwrap();
        let mgr = SessionManager::open(&empty).unwrap();
        assert_eq!(read_values(&empty).len(), 1);
        assert_eq!(mgr.get_session_file(), Some(empty.as_path()));
    }

    #[test]
    fn current_system_message_replays_patches() {
        let sys = |content: &str, sections: Option<Vec<(&str, Option<&str>)>>, tools: Option<Vec<&str>>| {
            AgentMessage::System(SystemMessage {
                content: UserContent::Text(content.into()),
                sections: sections.map(|s| s.into_iter().map(|(k, v)| (k.to_string(), v.map(String::from))).collect()),
                tools_added: tools.map(|t| {
                    t.into_iter().map(|n| Tool { name: n.into(), description: String::new(), parameters: json!({}) }).collect()
                }),
                timestamp: 5,
            })
        };
        assert_eq!(get_current_system_message(&[user("x")]), None);
        let merged = get_current_system_message(&[
            sys("base", Some(vec![("a", Some("1")), ("b", Some("2"))]), Some(vec!["read", "write"])),
            user("x"),
            sys("", Some(vec![("a", None), ("c", Some("3"))]), Some(vec!["read"])),
        ])
        .unwrap();
        assert_eq!(merged.content.plain_text(), "base");
        let sections = merged.sections.unwrap();
        assert_eq!(sections.get("a"), None);
        assert_eq!(sections.get("b").cloned().flatten().as_deref(), Some("2"));
        assert_eq!(sections.get("c").cloned().flatten().as_deref(), Some("3"));
        let tools: Vec<String> = merged.tools_added.unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(tools, ["read", "write"]);
        assert_eq!(merged.timestamp, 5);
    }
}
