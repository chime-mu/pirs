//! Opaque server paths and by-reference payloads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A path on the server that runs the loop.
///
/// Every path in every message is one of these: a label the server produced
/// and that a client only ever hands back to the same server (`fs.list`,
/// `fs.read`, `loop.create { cwd }`). Clients do not parse, join, normalise or
/// open it — it may belong to a machine or container the client cannot see.
/// Accordingly this type has no filesystem operations and no conversion to
/// `std::path::Path`; a server converts at its own edge.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ServerPath(pub String);

impl ServerPath {
    /// The label as a string, for display and for handing back to the server.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ServerPath {
    fn from(s: String) -> Self {
        ServerPath(s)
    }
}

impl From<&str> for ServerPath {
    fn from(s: &str) -> Self {
        ServerPath(s.to_owned())
    }
}

impl std::fmt::Display for ServerPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A payload that travels by reference because it is above the size threshold
/// (64 KB by default).
///
/// The server writes the content to a file in the loop's session directory
/// and sends this instead. `ref` is itself a [`ServerPath`] that `fs.read`
/// serves, so one request fetches it; `bytes` is the content's size so a
/// client can decide whether to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ref {
    /// Where the content is; read it with `fs.read { loop, path: ref }`.
    #[serde(rename = "ref")]
    pub path: ServerPath,
    /// Size of the content in bytes.
    pub bytes: u64,
}
