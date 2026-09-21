//! Where the socket is.

use std::ffi::OsString;
use std::path::PathBuf;

/// The name of the socket inside its directory.
const SOCKET_NAME: &str = "pirs.sock";

/// Read an environment variable, treating an empty value as unset.
fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// pirs's own directory: `PIRS_HOME` if set, else `~/.pirs`.
///
/// Falls back to `.pirs` in the current directory when there is no home
/// directory at all, so the function is total.
pub fn pirs_home() -> PathBuf {
    if let Some(home) = env_path("PIRS_HOME") {
        return home;
    }
    let home: Option<OsString> = dirs::home_dir().map(PathBuf::into_os_string);
    match home {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".pirs"),
        _ => PathBuf::from(".pirs"),
    }
}

/// The unix socket a local pirs server listens on.
///
/// `PIRS_SOCKET` overrides everything; otherwise it is
/// `$XDG_RUNTIME_DIR/pirs.sock`, and `~/.pirs/pirs.sock` when there is no
/// `XDG_RUNTIME_DIR` (`PIRS_HOME` overrides `~/.pirs`; see [`pirs_home`]).
/// This is the default only — [`ConnectOptions::socket`] names a socket
/// explicitly.
///
/// [`ConnectOptions::socket`]: crate::ConnectOptions::socket
pub fn socket_path() -> PathBuf {
    if let Some(socket) = env_path("PIRS_SOCKET") {
        return socket;
    }
    if let Some(runtime) = env_path("XDG_RUNTIME_DIR") {
        return runtime.join(SOCKET_NAME);
    }
    pirs_home().join(SOCKET_NAME)
}
