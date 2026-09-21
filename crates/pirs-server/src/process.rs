//! The one place in the server that constructs a child process.
//!
//! `clippy.toml` disallows `std::process::Command::new` and
//! `tokio::process::Command::new` everywhere in this crate; this module carries
//! the module-level allow (D-09), so every process the server spawns — the
//! `bash` tool's shell, and from phase 2 on the called policy processes — is
//! created here and nowhere else.
#![allow(clippy::disallowed_methods)]

use std::ffi::OsStr;

/// An async command builder. The only way to build one inside this crate.
pub(crate) fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::new(program)
}

/// A blocking command builder, for the paths that cannot await (process-group
/// teardown from a `Drop` impl, for instance).
pub(crate) fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_carry_the_program_name() {
        assert_eq!(std_command("true").get_program(), OsStr::new("true"));
        assert_eq!(tokio_command("true").as_std().get_program(), OsStr::new("true"));
    }
}
