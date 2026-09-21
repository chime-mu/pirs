//! The pirs server: everything that owns a loop and its state.
//!
//! This crate has no terminal. It never renders, so `print_stdout` and
//! `print_stderr` are denied crate-wide and diagnostics go to `tracing`.
//!
//! Server-side parts moved out of `pi-cli` (D-04):
//!
//! - [`session`] — the conversation log: append-only JSONL trees in
//!   `~/.pirs/sessions/<encoded cwd>/`, version 3 of pi's format plus a
//!   per-entry `seq` (D-06), with branching, forking and compaction.
//! - [`tools`] — the built-in tools (`read`, `bash`, `edit`, `write`, `grep`,
//!   `find`, `ls`) as [`pi_agent::AgentTool`] implementations.
//! - [`settings`] — what the loop reads before it has a policy: the DSL's
//!   `[settings]` table, and the `models.json` provider catalogue.
//! - [`dsl`] — policy files: locating, parsing, desugaring, composing and
//!   checking `*.pirs.toml` (D-17, D-18).
//! - [`system_prompt`] — the system prompt builder and `AGENTS.md` discovery.
//! - `process` (private) — the only module allowed to construct a child
//!   process (D-09).
//!
//! The loop server itself:
//!
//! - [`server`] — [`serve`]: the unix-socket listener speaking JSON lines,
//!   one task per connection, request dispatch, idle exit.
//! - `agent_loop` — a running loop: creation, prompting with `when`, abort,
//!   the agent hooks and the agent-event listener that turns the run into
//!   log entries and events.
//! - `dispatch` — handler registrations (D-23) and the slot request path with
//!   timeouts.
//! - `policy` — the DSL at runtime: what the loop does with the entries it
//!   loaded, in front of the registered handlers of the same slot.
//! - `log` — the session log as the sequenced event stream (D-06): every
//!   sequenced event is a log entry, replay walks the log, by-reference
//!   payloads (D-11, D-39).
//! - `convert` — `pi_agent` ↔ `pirs_protocol` message conversion (D-08).
//! - `fs` — `fs.list` / `fs.read` (D-07).

#![deny(unreachable_pub)]
#![deny(clippy::print_stdout, clippy::print_stderr)]
// D-09: `clippy.toml` names the two `Command::new` constructors; `process`
// carries the only allow.
#![deny(clippy::disallowed_methods)]

pub mod dsl;
pub mod server;
pub mod session;
pub mod settings;
pub mod system_prompt;
pub mod tools;

mod agent_loop;
mod convert;
mod dispatch;
mod fs;
mod log;
mod policy;
mod process;

pub use server::{serve, serve_until, ServeOptions};

/// Size in bytes above which a single text content block of a tool result
/// travels by reference in events (D-11, D-39). The session log always holds
/// the full content; `fs.read` serves the ref in full.
pub const REF_THRESHOLD: usize = 64 * 1024;

/// Largest file `fs.read` serves inline; above it the request fails with
/// `INVALID_PARAMS` (D-39).
pub const FS_READ_CAP: u64 = 16 * 1024 * 1024;
