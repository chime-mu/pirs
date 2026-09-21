//! Reaching the server: which socket, which working directory.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use pirs_client::{socket_path, Client, ConnectOptions, ServerConfig, LOCAL};

/// This client's name and version, as the server logs it.
pub(crate) const CLIENT_NAME: &str = concat!("pirs ", env!("CARGO_PKG_VERSION"));

/// `--socket` if given, else the default (`PIRS_SOCKET`, `$XDG_RUNTIME_DIR`,
/// `~/.pirs`).
pub(crate) fn resolve_socket(explicit: Option<&Path>) -> PathBuf {
    explicit.map(Path::to_path_buf).unwrap_or_else(socket_path)
}

/// Every configured server, or the one `--server` names (D-05).
///
/// `--server` without a `servers.toml` entry of that name is an error naming
/// the ones there are, except for `local`, which every client has whether or
/// not the file mentions it. `--socket` overrides the local server's socket
/// and is refused for a server reached through a bridge command, where it
/// would silently do nothing.
pub(crate) fn resolve_server(name: Option<&str>, socket: Option<&Path>) -> Result<ServerConfig> {
    let configured = pirs_client::servers()?;
    let wanted = name.unwrap_or(LOCAL);
    let mut config = match configured.iter().find(|s| s.name == wanted) {
        Some(config) => config.clone(),
        None if wanted == LOCAL => ServerConfig::local(),
        None => {
            let known: Vec<&str> = configured.iter().map(|s| s.name.as_str()).collect();
            bail!(
                "no server {wanted:?} in {}; there is {}",
                pirs_client::servers_path().display(),
                known.join(", ")
            );
        }
    };
    if let Some(socket) = socket {
        if config.is_remote() {
            bail!(
                "--socket names a local socket, and {:?} is reached with `command = {}`",
                config.name,
                config
                    .command
                    .as_deref()
                    .unwrap_or_default()
                    .join(" ")
            );
        }
        config.socket = Some(socket.to_path_buf());
    }
    Ok(config)
}

/// The list `--list` walks when no `--server` names one: every configured
/// server, which is just `local` when there is no `servers.toml`.
pub(crate) fn all_servers() -> Result<Vec<ServerConfig>> {
    Ok(pirs_client::servers()?)
}

/// How to reach one server: its bridge command, or its socket with the
/// auto-start command pointing at that same socket. Auto-start is a local
/// thing; a bridge is the whole transport and starts nothing here.
pub(crate) fn server_options(config: &ServerConfig, auto_start: bool) -> ConnectOptions {
    let mut options = ConnectOptions::for_server(config, CLIENT_NAME);
    if config.is_remote() {
        return options;
    }
    let socket = resolve_socket(config.socket.as_deref());
    options.auto_start = auto_start;
    options.server_command = server_command(&socket);
    options.socket = Some(socket);
    options
}

/// Connect to one configured server.
pub(crate) async fn connect_to(config: &ServerConfig, auto_start: bool) -> Result<Client> {
    Ok(Client::connect(server_options(config, auto_start)).await?)
}

/// How to reach the server, with the auto-start command pointing at the same
/// socket this client uses: a `--socket` that only the client knew about
/// would otherwise start a server somewhere else.
pub(crate) fn connect_options(socket: &Path, auto_start: bool) -> ConnectOptions {
    let mut options = ConnectOptions::new(CLIENT_NAME);
    options.socket = Some(socket.to_path_buf());
    options.auto_start = auto_start;
    options.server_command = server_command(socket);
    options
}

/// `<this executable> serve --socket <socket>`, the server a client starts
/// for itself.
fn server_command(socket: &Path) -> Option<Vec<String>> {
    std::env::current_exe().ok().map(|exe| {
        vec![
            exe.to_string_lossy().into_owned(),
            "serve".to_owned(),
            "--socket".to_owned(),
            socket.to_string_lossy().into_owned(),
        ]
    })
}

/// The working directory for a loop on `config`.
///
/// On the local server this is [`resolve_cwd`]: a real directory, checked
/// and canonicalised, so the server and the client name the same
/// conversations for it. On a server reached through a bridge it is the
/// text of `--cwd` and nothing else — the directory is over there, this
/// machine cannot check it and must not rewrite it (D-31). Without `--cwd`
/// the process's own directory is sent as written, which is what a bridge
/// to another server on this same machine wants.
pub(crate) fn resolve_cwd_on(config: &ServerConfig, explicit: Option<&Path>) -> Result<String> {
    if !config.is_remote() {
        return resolve_cwd(explicit);
    }
    let cwd = match explicit {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().context("no working directory")?,
    };
    Ok(cwd.to_string_lossy().into_owned())
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
