//! Reaching the server: which socket, which working directory.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use pirs_client::{socket_path, ConnectOptions};

/// This client's name and version, as the server logs it.
pub(crate) const CLIENT_NAME: &str = concat!("pirs ", env!("CARGO_PKG_VERSION"));

/// `--socket` if given, else the default (`PIRS_SOCKET`, `$XDG_RUNTIME_DIR`,
/// `~/.pirs`).
pub(crate) fn resolve_socket(explicit: Option<&Path>) -> PathBuf {
    explicit.map(Path::to_path_buf).unwrap_or_else(socket_path)
}

/// How to reach the server, with the auto-start command pointing at the same
/// socket this client uses: a `--socket` that only the client knew about
/// would otherwise start a server somewhere else.
pub(crate) fn connect_options(socket: &Path, auto_start: bool) -> ConnectOptions {
    let mut options = ConnectOptions::new(CLIENT_NAME);
    options.socket = Some(socket.to_path_buf());
    options.auto_start = auto_start;
    options.server_command = std::env::current_exe().ok().map(|exe| {
        vec![
            exe.to_string_lossy().into_owned(),
            "serve".to_owned(),
            "--socket".to_owned(),
            socket.to_string_lossy().into_owned(),
        ]
    });
    options
}

/// `--cwd` if given, else this process's working directory; canonicalised so
/// the server and the client name the same conversations for it.
pub(crate) fn resolve_cwd(explicit: Option<&Path>) -> Result<String> {
    let cwd = match explicit {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().context("no working directory")?,
    };
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("no such directory: {}", cwd.display()))?;
    Ok(cwd.to_string_lossy().into_owned())
}
