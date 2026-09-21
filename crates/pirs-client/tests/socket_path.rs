//! Where `socket_path` looks, in order. One test, because environment
//! variables belong to the whole process.

use std::path::PathBuf;

use pirs_client::{pirs_home, socket_path};

/// Set or clear a variable.
fn set(key: &str, value: Option<&str>) {
    match value {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

#[test]
fn the_socket_is_found_in_order() {
    let saved: Vec<(&str, Option<std::ffi::OsString>)> =
        ["PIRS_SOCKET", "XDG_RUNTIME_DIR", "PIRS_HOME", "HOME"]
            .iter()
            .map(|key| (*key, std::env::var_os(key)))
            .collect();

    set("HOME", Some("/home/tester"));
    set("PIRS_HOME", None);
    set("XDG_RUNTIME_DIR", None);
    set("PIRS_SOCKET", None);
    assert_eq!(pirs_home(), PathBuf::from("/home/tester/.pirs"));
    assert_eq!(
        socket_path(),
        PathBuf::from("/home/tester/.pirs/pirs.sock"),
        "no XDG_RUNTIME_DIR falls back to the pirs home"
    );

    set("PIRS_HOME", Some("/var/lib/pirs"));
    assert_eq!(pirs_home(), PathBuf::from("/var/lib/pirs"));
    assert_eq!(socket_path(), PathBuf::from("/var/lib/pirs/pirs.sock"));

    set("XDG_RUNTIME_DIR", Some("/run/user/1000"));
    assert_eq!(
        socket_path(),
        PathBuf::from("/run/user/1000/pirs.sock"),
        "XDG_RUNTIME_DIR beats the pirs home"
    );

    set("PIRS_SOCKET", Some("/tmp/explicit.sock"));
    assert_eq!(
        socket_path(),
        PathBuf::from("/tmp/explicit.sock"),
        "PIRS_SOCKET beats everything"
    );

    set("PIRS_SOCKET", Some(""));
    assert_eq!(
        socket_path(),
        PathBuf::from("/run/user/1000/pirs.sock"),
        "an empty variable counts as unset"
    );

    for (key, value) in saved {
        set(key, value.as_deref().and_then(std::ffi::OsStr::to_str));
    }
}
