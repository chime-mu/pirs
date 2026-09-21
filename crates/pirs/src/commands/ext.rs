//! `pirs ext new` and `pirs ext regen`: the policy file written from a
//! sentence (S10).
//!
//! A convenience for when no agent is running (D-33). It is the ordinary
//! one-shot in disguise: connect, create a loop, prompt it with the DSL
//! instruction set and the intent, take the file out of the answer, close the
//! loop (D-28), write the file and run `dsl.check` on the directory — the same
//! report `pirs check` prints, because it is the same formatter.
//!
//! Two things make it a tool rather than a conversation. Nothing the model
//! streams reaches stdout: the answer is a file, and stdout carries only the
//! check and the `wrote <path>` line, so a script can read it. And the
//! `intent` of the written file is *normalised* to the argument — the model is
//! asked to copy it verbatim and often paraphrases it, and the intent is the
//! shareable unit that `pirs ext regen` rebuilds the file from, so it must be
//! the sentence the person actually said.
//!
//! `docs/dsl.md` is embedded at compile time rather than read from disk: the
//! command has to work from an installed binary, and the loop it prompts may
//! be on a server that has never seen this repository.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use futures::StreamExt as _;
use pirs_client::ServerConfig;
use pirs_protocol::{
    Delta, DslCheckResult, Event, LoopCreateParams, LoopMessageBody, LoopSelector, Message,
    ModelSpec, NotifyLevel, PromptWhen, ServerPath, SubscribeParams,
};

use crate::cli::{ExtCommand, GlobalArgs};
use crate::commands::check::format_check;
use crate::connect::{connect_to, resolve_cwd, resolve_server};
use crate::print::EXIT_INTERRUPTED;

/// The instruction set the model writes the file from, as shipped.
const DSL_DOC: &str = include_str!("../../../../docs/dsl.md");

/// What is asked of the model, after the instruction set.
const INSTRUCTIONS: &str = "Write one complete `*.pirs.toml` policy file for this intent. \
Reply with the file only, in one fenced ```toml block, nothing else. The file must start \
with `intent = \"\"\"…\"\"\"` holding the intent verbatim.";

/// The longest a slug made from an intent may be.
const SLUG_MAX: usize = 40;

/// Run `pirs ext new` or `pirs ext regen`.
pub(crate) async fn run(command: ExtCommand, global: &GlobalArgs) -> Result<i32> {
    let config = resolve_server(global.server.as_deref(), global.socket.as_deref())?;
    // Before anything is asked of a model or written: a file here is not that
    // server's policy (D-26).
    refuse_remote(&config).map_err(anyhow::Error::msg)?;
    match command {
        ExtCommand::New {
            intent,
            name,
            model,
            force,
        } => {
            let intent = intent.join(" ").trim().to_owned();
            if intent.is_empty() {
                bail!("no intent given; try `pirs ext new \"show the git branch in the status line\"`");
            }
            let cwd = resolve_cwd(global.cwd.as_deref())?;
            let name = match name.as_deref() {
                Some(name) => file_name(name).map_err(anyhow::Error::msg)?,
                None => slug(&intent),
            };
            let path = free_path(&ext_dir(&cwd), &name, force)?;
            generate(&intent, &path, false, model.as_deref(), &config, cwd, global).await
        }
        ExtCommand::Regen { file, model } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            let cwd = check_dir_for(
                &file,
                &resolve_cwd(global.cwd.as_deref())?,
                &pirs_client::pirs_home().join("ext"),
            )
            .map_err(anyhow::Error::msg)?;
            let intent = intent_of(&text)
                .map_err(|message| anyhow::anyhow!("{}: {message}", file.display()))?;
            generate(&intent, &file, true, model.as_deref(), &config, cwd, global).await
        }
    }
}

/// `pirs ext` is refused for a server reached through a bridge command.
///
/// The file would be written here and the policy it is meant to be lives over
/// there, beside the server that reads it (D-26); there is no request that
/// writes a file, and the client must not construct a path on the server
/// (D-31).
fn refuse_remote(config: &ServerConfig) -> Result<(), String> {
    if config.is_remote() {
        return Err(format!(
            "pirs ext writes a policy file, and policy lives with the server (D-26): run \
             `pirs ext` on `{}` itself",
            config.name
        ));
    }
    Ok(())
}

/// Ask one loop for the file, write it, check it.
///
/// `backup` copies the file that is there to `<file>.bak` before it is
/// overwritten, which is what `regen` wants and `new` never does. `cwd` is the
/// directory the loop runs in and the one the check reports on.
async fn generate(
    intent: &str,
    path: &Path,
    backup: bool,
    model: Option<&str>,
    config: &ServerConfig,
    cwd: String,
    global: &GlobalArgs,
) -> Result<i32> {
    let client = connect_to(config, !global.no_start).await?;

    let info = client
        .loop_create(LoopCreateParams {
            cwd: ServerPath::from(cwd.clone()),
            // No `--model` means the loop's default model, like any other
            // one-shot.
            model: model.map(|model| ModelSpec {
                model: model.to_owned(),
                thinking: None,
            }),
            name: None,
            session: None,
        })
        .await?;
    eprintln!(
        "pirs ext: asking {} for a policy file for {intent:?}",
        info.model.model
    );

    client
        .subscribe(SubscribeParams {
            loop_id: LoopSelector::Loop(info.id.clone()),
            events: None,
            since: None,
        })
        .await?;
    let mut events = client.events().context("the event stream was already taken")?;
    client
        .loop_prompt(&info.id, prompt_for(intent), PromptWhen::Now)
        .await?;

    // Ctrl-C leaves through the `None` arm, as in print mode: the run is
    // aborted, the loop closed, and nothing is written.
    let reply = tokio::select! {
        answer = collect_until_run_end(&mut events, &info.id) => Some(answer),
        _ = tokio::signal::ctrl_c() => None,
    };
    let reply = match reply {
        None => None,
        Some(answer) => tokio::select! {
            waited = client.loop_wait(&info.id) => { waited?; Some(answer) }
            _ = tokio::signal::ctrl_c() => None,
        },
    };
    let Some(reply) = reply else {
        let _ = client.loop_abort(&info.id).await;
        let _ = client.loop_close(&info.id).await;
        drop(events);
        return Ok(EXIT_INTERRUPTED);
    };
    client.loop_close(&info.id).await?;
    drop(events);

    // No file yet: a reply with no policy in it is an error and leaves the
    // directory as it was.
    let body = extract_policy(&reply).map_err(anyhow::Error::msg)?;
    let (contents, broken) = match normalise_intent(&body, intent) {
        Ok(contents) => (contents, None),
        // The file is written anyway, unnormalised: seeing what the model
        // wrote is how the person fixes it.
        Err(message) => (ensure_newline(body), Some(message)),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    if backup && path.exists() {
        let backup = backup_path(path);
        std::fs::copy(path, &backup)
            .with_context(|| format!("cannot back up {} to {}", path.display(), backup.display()))?;
    }
    std::fs::write(path, &contents).with_context(|| format!("cannot write {}", path.display()))?;

    let result = client.dsl_check(ServerPath::from(cwd)).await?;
    print!("{}", format_check(&result));
    println!("wrote {}", path.display());

    let mut failed = false;
    if let Some(message) = broken {
        eprintln!(
            "pirs ext: {message}; the file is left at {} so you can fix it",
            path.display()
        );
        failed = true;
    }
    if conflicts_naming(&result, path) {
        eprintln!(
            "pirs ext: the conflicts above name {}; the file is left there so you can fix it",
            path.display()
        );
        failed = true;
    }
    Ok(i32::from(failed))
}

/// The one prompt: the instruction set, what to do with it, the intent.
fn prompt_for(intent: &str) -> String {
    format!("{DSL_DOC}\n\n---\n\n{INSTRUCTIONS}\n\nIntent: {intent}\n")
}

/// The model's last assistant text, with nothing printed on the way.
///
/// `ui.notify` still reaches stderr — a policy file in the directory may warn
/// about itself while this runs, and swallowing that would hide it.
async fn collect_until_run_end(events: &mut pirs_client::EventStream, loop_id: &str) -> String {
    let mut streamed = String::new();
    let mut complete = String::new();
    while let Some(event) = events.next().await {
        if event.loop_id() != loop_id {
            continue;
        }
        match event {
            Event::LoopMessage(message) => match message.body {
                LoopMessageBody::Delta {
                    delta: Delta::Text { text, .. },
                } => streamed.push_str(&text),
                LoopMessageBody::Delta { .. } => {}
                LoopMessageBody::Message { message } => {
                    if let Message::Assistant(assistant) = *message {
                        let text = assistant.text();
                        if !text.trim().is_empty() {
                            complete = text;
                        }
                    }
                }
            },
            Event::UiNotify(notify) => {
                let level = match notify.level {
                    NotifyLevel::Info => "info",
                    NotifyLevel::Warning => "warning",
                    NotifyLevel::Error => "error",
                };
                eprintln!("[{level}] {}", notify.text);
            }
            Event::LoopRunEnd(_) => break,
            _ => {}
        }
    }
    if complete.trim().is_empty() {
        streamed
    } else {
        complete
    }
}

// ---------------------------------------------------------------------------
// Where the file goes
// ---------------------------------------------------------------------------

/// `<cwd>/.pirs/ext`, where a project's policy files live.
fn ext_dir(cwd: &str) -> PathBuf {
    PathBuf::from(cwd).join(".pirs").join("ext")
}

/// The directory `regen` checks — and runs its loop in — for a file.
///
/// `<x>/.pirs/ext/<f>` is `<x>`'s policy and is checked there, wherever the
/// command was run from. `~/.pirs/ext/<f>` is global policy, which applies to
/// every directory, so the check is the one for here. A file anywhere else is
/// not policy at all and `regen` refuses it: rewriting it would say nothing
/// about what any server would then read.
///
/// `home_ext` is `~/.pirs/ext`, as [`pirs_client::pirs_home`] finds it.
fn check_dir_for(file: &Path, cwd: &str, home_ext: &Path) -> Result<String, String> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let dir = match dir.as_os_str().is_empty() {
        true => Path::new("."),
        false => dir,
    };
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let home_ext = std::fs::canonicalize(home_ext).unwrap_or_else(|_| home_ext.to_path_buf());
    if dir == home_ext {
        return Ok(cwd.to_owned());
    }
    if dir.file_name() == Some(OsStr::new("ext")) {
        if let Some(project) = dir
            .parent()
            .filter(|dot| dot.file_name() == Some(OsStr::new(".pirs")))
            .and_then(Path::parent)
        {
            return Ok(project.to_string_lossy().into_owned());
        }
    }
    Err(format!(
        "not a policy location: expected {cwd}/.pirs/ext/*.pirs.toml or ~/.pirs/ext/*.pirs.toml"
    ))
}

/// `<dir>/<slug>.pirs.toml`, or `<slug>-2`, `-3`, … when that is taken and
/// `--force` was not given.
fn free_path(dir: &Path, slug: &str, force: bool) -> Result<PathBuf> {
    let at = |name: &str| dir.join(format!("{name}.pirs.toml"));
    if force || !at(slug).exists() {
        return Ok(at(slug));
    }
    for n in 2..1000 {
        let candidate = at(&format!("{slug}-{n}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("{} already holds a thousand files named {slug}-N", dir.display())
}

/// `<file>.bak`, beside the file it backs up.
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// The name `--name` gives the file, checked rather than slugged.
///
/// A name the person typed is used as typed or refused: silently turning it
/// into something else would leave them looking for a file that is not there.
/// A trailing `.pirs.toml` is dropped, because the argument is the file name
/// and that suffix is the command's.
fn file_name(name: &str) -> Result<String, String> {
    let refuse = || {
        Err(format!(
            "--name {name:?} is not a file name: use letters, digits, `.`, `_` and `-`, \
             without `.pirs.toml`"
        ))
    };
    let stem = name.strip_suffix(".pirs.toml").unwrap_or(name);
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if stem.is_empty() || stem == "." || stem == ".." || !stem.chars().all(plain) {
        return refuse();
    }
    Ok(stem.to_owned())
}

/// The file name a sentence gets: lowercase, `[a-z0-9-]`, at most 40
/// characters, never empty and never starting or ending with a dash.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if c.is_alphanumeric() {
            // A letter this machine has no ASCII for is a separator, not a
            // file name: `ø` in a path is fine, in a slug it is a surprise.
            if !slug.ends_with('-') && !slug.is_empty() {
                slug.push('-');
            }
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= SLUG_MAX {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "intent".to_owned()
    } else {
        slug
    }
}

/// Does any conflict name the file just written?
fn conflicts_naming(result: &DslCheckResult, path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    result.conflicts.iter().any(|conflict| {
        conflict.files.iter().any(|file| {
            let file = file.as_str();
            Path::new(file) == path
                || name
                    .as_deref()
                    .is_some_and(|name| Path::new(file).file_name().is_some_and(|f| f == name))
        })
    })
}

// ---------------------------------------------------------------------------
// The file inside the answer
// ---------------------------------------------------------------------------

/// The policy file in the model's reply.
///
/// The first ```` ```toml ```` block, else a fenced block with no language
/// whose body is TOML, else the whole reply when that is TOML. Anything else
/// is an error: a reply that explains instead of answering must not become a
/// file.
fn extract_policy(reply: &str) -> Result<String, String> {
    let blocks = fenced_blocks(reply);
    if let Some(block) = blocks
        .iter()
        .find(|(info, _)| info.eq_ignore_ascii_case("toml"))
    {
        return Ok(block.1.clone());
    }
    if let Some(block) = blocks
        .iter()
        .find(|(info, body)| info.is_empty() && is_policy_toml(body))
    {
        return Ok(block.1.clone());
    }
    if is_policy_toml(reply) {
        return Ok(reply.trim_start_matches('\n').to_owned());
    }
    Err(no_policy_message(reply))
}

/// Why a reply is not a policy file.
///
/// An empty reply is its own case: the run produced no assistant text at all,
/// and the likeliest reason is a policy `[[input]]` entry that handled the
/// prompt before the model saw it — counting the characters of nothing says
/// none of that.
fn no_policy_message(reply: &str) -> String {
    if reply.trim().is_empty() {
        return "the model produced no answer (a policy `[[input]]` entry may have consumed \
                the prompt; see `pirs check`)"
            .to_owned();
    }
    format!(
        "the model did not answer with a policy file: no ```toml block in its {} character \
         reply, and the reply is not TOML either",
        reply.trim().chars().count()
    )
}

/// Every fenced block of a Markdown reply, as `(info string, body)`.
fn fenced_blocks(text: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, String, Vec<&str>)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let ticks = trimmed.chars().take_while(|c| *c == '`').count();
        match &mut open {
            None => {
                if ticks >= 3 {
                    let info = trimmed[ticks..].trim().to_owned();
                    open = Some((ticks, info, Vec::new()));
                }
            }
            Some((fence, _, lines)) => {
                if ticks >= *fence && trimmed[ticks..].trim().is_empty() {
                    let (_, info, lines) = open.take().expect("the block is open");
                    blocks.push((info, join_lines(&lines)));
                } else {
                    lines.push(line);
                }
            }
        }
    }
    // A block the model forgot to close is still the file it wrote.
    if let Some((_, info, lines)) = open {
        blocks.push((info, join_lines(&lines)));
    }
    blocks
}

/// The lines of a block, with the trailing newline a file has.
fn join_lines(lines: &[&str]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// Is this text a policy file rather than prose? Valid TOML, and not empty:
/// an empty document parses, and a reply of "Sorry" does not.
fn is_policy_toml(text: &str) -> bool {
    text.parse::<toml::Table>()
        .is_ok_and(|table| !table.is_empty())
}

// ---------------------------------------------------------------------------
// The intent the file carries
// ---------------------------------------------------------------------------

/// A policy file's own `intent`, for `regen`.
fn intent_of(text: &str) -> Result<String, String> {
    let table: toml::Table = text
        .parse()
        .map_err(|error| format!("not a policy file: {error}"))?;
    match table.get("intent") {
        Some(toml::Value::String(intent)) => Ok(intent.trim().to_owned()),
        Some(_) => Err("`intent` is not a string".to_owned()),
        None => Err(
            "no `intent` to rebuild it from; add one, or write the file with `pirs ext new`"
                .to_owned(),
        ),
    }
}

/// The model's file with its top-level `intent` replaced by this one.
///
/// Everything else — order, comments, spacing — is left exactly as the model
/// wrote it: only the one key is rewritten, and it is moved to the top, where
/// `40-dsl.md` says it belongs.
fn normalise_intent(body: &str, intent: &str) -> Result<String, String> {
    if let Err(error) = body.parse::<toml::Table>() {
        return Err(format!("the file the model wrote is not valid TOML: {error}"));
    }
    let rest = strip_top_level_intent(body);
    let rest = rest.trim_start_matches('\n');
    let mut out = format!("intent = {}\n", toml_string(intent));
    if !rest.is_empty() {
        out.push('\n');
        out.push_str(rest);
    }
    let out = ensure_newline(out);
    // The rewrite is mechanical, so this cannot normally fail; it is here
    // because writing a file whose intent is not the argument would quietly
    // break `regen`, which reads it back.
    match out.parse::<toml::Table>() {
        Ok(table) if table.get("intent") == Some(&toml::Value::String(intent.to_owned())) => {
            Ok(out)
        }
        Ok(_) => Err("the intent could not be written into the file the model wrote".to_owned()),
        Err(error) => Err(format!(
            "the intent could not be written into the file the model wrote: {error}"
        )),
    }
}

/// The document without its top-level `intent = …` assignment.
///
/// Textual, because the rest of the file must survive unchanged. The scan
/// knows enough TOML to tell a `[table]` header and a `#` comment from the
/// same characters inside a string, and to let a multi-line string hold
/// newlines.
fn strip_top_level_intent(text: &str) -> String {
    for (start, end) in logical_lines(text) {
        let line = &text[start..end];
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            // A table header: every key after it belongs to that table.
            break;
        }
        if is_intent_assignment(trimmed) {
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..start]);
            out.push_str(&text[end..]);
            return out;
        }
    }
    text.to_owned()
}

/// Does this logical line assign the top-level key `intent`?
fn is_intent_assignment(line: &str) -> bool {
    for key in ["intent", "\"intent\"", "'intent'"] {
        if let Some(rest) = line.strip_prefix(key) {
            if rest.trim_start().starts_with('=') {
                return true;
            }
        }
    }
    false
}

/// The byte ranges of the document's logical lines: a newline inside a string
/// does not end one, and neither does anything inside a comment.
fn logical_lines(text: &str) -> Vec<(usize, usize)> {
    #[derive(PartialEq)]
    enum State {
        Normal,
        Comment,
        Basic,
        Literal,
        MlBasic,
        MlLiteral,
    }
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut state = State::Normal;
    let (mut start, mut i) = (0usize, 0usize);
    // Past the closing `"""` or `'''`, up to two more of the same quote still
    // belong to the string's value.
    let trailing = |i: &mut usize, quote: u8| {
        for _ in 0..2 {
            if bytes.get(*i) == Some(&quote) {
                *i += 1;
            }
        }
    };
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            State::Normal => match b {
                b'#' => {
                    state = State::Comment;
                    i += 1;
                }
                b'"' if bytes[i..].starts_with(b"\"\"\"") => {
                    state = State::MlBasic;
                    i += 3;
                }
                b'"' => {
                    state = State::Basic;
                    i += 1;
                }
                b'\'' if bytes[i..].starts_with(b"'''") => {
                    state = State::MlLiteral;
                    i += 3;
                }
                b'\'' => {
                    state = State::Literal;
                    i += 1;
                }
                b'\n' => {
                    i += 1;
                    spans.push((start, i));
                    start = i;
                }
                _ => i += 1,
            },
            State::Comment => {
                if b == b'\n' {
                    state = State::Normal;
                } else {
                    i += 1;
                }
            }
            State::Basic => match b {
                b'\\' => i = (i + 2).min(bytes.len()),
                b'"' => {
                    state = State::Normal;
                    i += 1;
                }
                // Unterminated: the document is not TOML, but the scan must
                // still end.
                b'\n' => state = State::Normal,
                _ => i += 1,
            },
            State::Literal => match b {
                b'\'' => {
                    state = State::Normal;
                    i += 1;
                }
                b'\n' => state = State::Normal,
                _ => i += 1,
            },
            State::MlBasic => {
                if b == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i..].starts_with(b"\"\"\"") {
                    i += 3;
                    trailing(&mut i, b'"');
                    state = State::Normal;
                } else {
                    i += 1;
                }
            }
            State::MlLiteral => {
                if bytes[i..].starts_with(b"'''") {
                    i += 3;
                    trailing(&mut i, b'\'');
                    state = State::Normal;
                } else {
                    i += 1;
                }
            }
        }
    }
    if start < bytes.len() {
        spans.push((start, bytes.len()));
    }
    spans
}

/// One TOML string holding exactly this text.
///
/// A literal string when the text has no `'''` in it and no control character
/// a literal cannot hold, because a literal shows the intent as the person
/// wrote it; a basic string with escapes otherwise. Multi-line forms open with
/// a newline, which TOML trims, so what comes back out is the text itself.
fn toml_string(value: &str) -> String {
    let printable = |c: char| c == '\n' || c == '\t' || !c.is_control();
    let literal_ok = !value.contains("'''") && !value.ends_with('\'') && value.chars().all(printable);
    if literal_ok {
        if !value.contains(['\n', '\'']) {
            return format!("'{value}'");
        }
        return format!("'''\n{value}'''");
    }
    let mut escaped = String::with_capacity(value.len() + 8);
    let multi = value.contains('\n');
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' if multi => escaped.push('\n'),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04X}", c as u32)),
            c => escaped.push(c),
        }
    }
    if multi {
        format!("\"\"\"\n{escaped}\"\"\"")
    } else {
        format!("\"{escaped}\"")
    }
}

/// A file ends with a newline.
fn ensure_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent_in(file: &str) -> String {
        file.parse::<toml::Table>()
            .expect("the file parses")
            .get("intent")
            .and_then(toml::Value::as_str)
            .expect("it has a string intent")
            .to_owned()
    }

    // ------------------------------------------------------ the block

    #[test]
    fn a_fenced_toml_block_is_the_file() {
        let reply = "Here you go:\n\n```toml\nintent = \"a\"\n[[status]]\nkey = \"b\"\n```\n\nEnjoy.";
        assert_eq!(
            extract_policy(reply).unwrap(),
            "intent = \"a\"\n[[status]]\nkey = \"b\"\n"
        );
    }

    #[test]
    fn the_first_toml_block_wins_and_prose_around_it_is_dropped() {
        let reply = "```bash\nnot this\n```\n```toml\nintent = \"first\"\n```\n```toml\nintent = \"second\"\n```";
        assert_eq!(extract_policy(reply).unwrap(), "intent = \"first\"\n");
    }

    #[test]
    fn an_unfenced_reply_that_is_toml_is_accepted() {
        let reply = "intent = \"a\"\n\n[[status]]\nkey = \"branch\"\nrun = \"git\"\non = [\"start\"]\n";
        assert_eq!(extract_policy(reply).unwrap(), reply);
        // So is a fence with no language, as long as it holds TOML.
        let fenced = "```\nintent = \"a\"\n```";
        assert_eq!(extract_policy(fenced).unwrap(), "intent = \"a\"\n");
    }

    #[test]
    fn a_reply_with_no_file_in_it_is_an_error() {
        for reply in [
            "I cannot write that file without knowing more about your project.",
            "```\nnot: toml at all\n```",
        ] {
            let error = extract_policy(reply).unwrap_err();
            assert!(error.contains("did not answer with a policy file"), "{error}");
        }
    }

    #[test]
    fn an_empty_reply_is_reported_as_no_answer_at_all() {
        for reply in ["", "  \n\n"] {
            let error = extract_policy(reply).unwrap_err();
            assert_eq!(error, no_policy_message(reply));
            assert!(error.contains("the model produced no answer"), "{error}");
            assert!(error.contains("[[input]]"), "it names the likely cause: {error}");
            assert!(!error.contains("0 character"), "{error}");
        }
        // A reply that is prose still counts its characters.
        let message = no_policy_message("Sorry, no.");
        assert!(message.contains("did not answer with a policy file"), "{message}");
        assert!(message.contains("10 character reply"), "{message}");
    }

    #[test]
    fn an_unclosed_block_is_still_the_file() {
        let reply = "```toml\nintent = \"a\"\n";
        assert_eq!(extract_policy(reply).unwrap(), "intent = \"a\"\n");
    }

    // ------------------------------------------------- the normalisation

    #[test]
    fn a_file_without_an_intent_gets_one() {
        let body = "[[status]]\nkey = \"branch\"\n";
        let out = normalise_intent(body, "show the branch").unwrap();
        assert_eq!(out, "intent = 'show the branch'\n\n[[status]]\nkey = \"branch\"\n");
        assert_eq!(intent_in(&out), "show the branch");
    }

    #[test]
    fn a_paraphrased_intent_is_replaced_by_the_argument() {
        let body = "# a comment\nintent = \"Display the current Git branch.\"\npriority = 3\n\n[[status]]\nkey = \"branch\"\n";
        let out = normalise_intent(body, "show the git branch in the status line").unwrap();
        assert_eq!(intent_in(&out), "show the git branch in the status line");
        assert!(out.starts_with("intent = 'show the git branch in the status line'\n"), "{out}");
        assert!(out.contains("# a comment\n"), "the comment survives: {out}");
        assert!(out.contains("priority = 3\n"), "{out}");
        assert!(!out.contains("Display the current Git branch"), "{out}");
    }

    #[test]
    fn a_multi_line_intent_the_model_wrote_is_replaced_whole() {
        let body = "intent = \"\"\"\nOne line.\nAnd another, with a [bracket] and a # hash.\n\"\"\"\n\n[[on]]\nevent = \"start\"\n";
        let out = normalise_intent(body, "one sentence").unwrap();
        assert_eq!(out, "intent = 'one sentence'\n\n[[on]]\nevent = \"start\"\n");
        let literal = "intent = '''\nkeep [this] # out of the way\n'''\n[[on]]\nevent = \"start\"\n";
        let out = normalise_intent(literal, "one sentence").unwrap();
        assert_eq!(out, "intent = 'one sentence'\n\n[[on]]\nevent = \"start\"\n");
    }

    #[test]
    fn an_intent_inside_a_table_is_not_the_top_level_one() {
        let body = "[settings]\nintent = \"not the file's\"\n";
        let out = normalise_intent(body, "mine").unwrap();
        assert_eq!(out, "intent = 'mine'\n\n[settings]\nintent = \"not the file's\"\n");
        assert_eq!(intent_in(&out), "mine");
    }

    #[test]
    fn a_multi_line_intent_becomes_a_literal_string_and_survives_the_round_trip() {
        let intent = "Show the branch.\nLet me type ?question for a brief answer.";
        let out = normalise_intent("[[status]]\nkey = \"b\"\n", intent).unwrap();
        assert!(out.starts_with("intent = '''\n"), "{out}");
        assert_eq!(intent_in(&out), intent);
        assert_eq!(intent_of(&out).unwrap(), intent);
    }

    #[test]
    fn quotes_and_backslashes_survive_the_round_trip() {
        for intent in [
            "match \"^\\?(.*)$\" and replace it",
            "a 'single' quote",
            "three ''' in a row",
            "a backslash \\ and a \"quote\"",
            "several\nlines with \"quotes\"\nand a trailing backslash \\",
            "three ''' in a row\nover two lines",
            "ends with a quote '",
            "a tab\tand a carriage\rreturn",
        ] {
            let out = normalise_intent("[[status]]\nkey = \"b\"\n", intent)
                .unwrap_or_else(|e| panic!("{intent:?}: {e}"));
            assert_eq!(intent_in(&out), intent, "in {out}");
        }
    }

    #[test]
    fn a_literal_string_is_preferred_and_a_basic_one_is_the_fallback() {
        assert_eq!(toml_string("plain"), "'plain'");
        assert_eq!(toml_string("two\nlines"), "'''\ntwo\nlines'''");
        assert_eq!(toml_string("three ''' quotes"), "\"three ''' quotes\"");
        assert_eq!(toml_string("a \"quote\""), "'a \"quote\"'");
        assert_eq!(toml_string("ends with '"), "\"ends with '\"");
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_rather_than_normalised() {
        let error = normalise_intent("this is not = = toml\n", "x").unwrap_err();
        assert!(error.contains("not valid TOML"), "{error}");
    }

    #[test]
    fn regen_reads_the_intent_back_and_says_when_there_is_none() {
        assert_eq!(intent_of("intent = 'a sentence'\n").unwrap(), "a sentence");
        let error = intent_of("[[status]]\nkey = 'b'\n").unwrap_err();
        assert!(error.contains("no `intent`"), "{error}");
        let error = intent_of("intent = 3\n").unwrap_err();
        assert!(error.contains("not a string"), "{error}");
        let error = intent_of("nonsense ==\n").unwrap_err();
        assert!(error.contains("not a policy file"), "{error}");
    }

    #[test]
    fn normalising_twice_changes_nothing() {
        let once = normalise_intent("intent = \"paraphrase\"\n[[status]]\nkey = \"b\"\n", "mine").unwrap();
        let twice = normalise_intent(&once, "mine").unwrap();
        assert_eq!(once, twice, "regen must reproduce the file byte for byte");
    }

    // --------------------------------------------------------- the name

    #[test]
    fn a_slug_is_lowercase_dashes_and_at_most_forty_characters() {
        assert_eq!(slug("show the git branch in the status line"), "show-the-git-branch-in-the-status-line");
        assert_eq!(slug("Show the Git branch!"), "show-the-git-branch");
        assert_eq!(slug("  a  b  "), "a-b");
        assert_eq!(slug("a/b\\c:d"), "a-b-c-d");
        assert_eq!(slug("..."), "intent");
        assert_eq!(slug(""), "intent");
        assert_eq!(slug("søg efter grene"), "s-g-efter-grene");
        let long = slug("this intent is far too long to be a file name and keeps going");
        assert!(long.len() <= SLUG_MAX, "{long:?} is {} characters", long.len());
        assert!(!long.ends_with('-') && !long.starts_with('-'), "{long:?}");
        assert!(long.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{long:?}");
    }

    #[test]
    fn a_taken_name_gets_a_suffix_unless_force_is_given() {
        let dir = std::env::temp_dir().join(format!("pirs-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the directory is made");
        let first = free_path(&dir, "branch", false).unwrap();
        assert_eq!(first.file_name().unwrap(), "branch.pirs.toml");
        std::fs::write(&first, "intent = 'x'\n").expect("written");
        let second = free_path(&dir, "branch", false).unwrap();
        assert_eq!(second.file_name().unwrap(), "branch-2.pirs.toml");
        let forced = free_path(&dir, "branch", true).unwrap();
        assert_eq!(forced, first);
        std::fs::remove_dir_all(&dir).expect("cleaned up");
    }

    #[test]
    fn a_given_name_is_checked_rather_than_slugged() {
        assert_eq!(file_name("custom").unwrap(), "custom");
        assert_eq!(file_name("Git_branch-2.v1").unwrap(), "Git_branch-2.v1");
        // The suffix is the command's, so a name that carries it is accepted
        // without it.
        assert_eq!(file_name("custom.pirs.toml").unwrap(), "custom");
        for bad in ["", ".", "..", "a b", "a/b", "../escape", "søg", "a\tb", ".pirs.toml"] {
            let error = file_name(bad).unwrap_err();
            assert!(error.contains("is not a file name"), "{bad:?}: {error}");
            assert!(error.contains(".pirs.toml"), "{bad:?}: {error}");
        }
    }

    // ------------------------------------------------------- the server

    #[test]
    fn a_server_reached_through_a_bridge_is_refused() {
        let remote = ServerConfig {
            name: "build".to_owned(),
            command: Some(vec!["ssh".to_owned(), "build".to_owned(), "pirs".to_owned()]),
            socket: None,
            editor_prefix: None,
        };
        let error = refuse_remote(&remote).unwrap_err();
        assert_eq!(
            error,
            "pirs ext writes a policy file, and policy lives with the server (D-26): run \
             `pirs ext` on `build` itself"
        );
        // The local server, named or not, is the one this command is for.
        refuse_remote(&ServerConfig::local()).expect("the local server is fine");
        refuse_remote(&ServerConfig {
            name: "other".to_owned(),
            command: None,
            socket: Some(PathBuf::from("/run/other.sock")),
            editor_prefix: None,
        })
        .expect("another local socket is still local");
    }

    // ---------------------------------------------------- where regen runs

    #[test]
    fn regen_checks_the_directory_the_file_belongs_to() {
        let root = std::env::temp_dir().join(format!("pirs-ext-where-{}", std::process::id()));
        let project = root.join("proj");
        let home_ext = root.join("home/.pirs/ext");
        std::fs::create_dir_all(project.join(".pirs/ext")).expect("the project is made");
        std::fs::create_dir_all(&home_ext).expect("the home is made");
        let here = std::fs::canonicalize(&root).expect("it exists");
        let here = here.to_string_lossy().into_owned();

        // A project file is checked in its own project, wherever we are.
        let in_project = project.join(".pirs/ext/a.pirs.toml");
        assert_eq!(
            check_dir_for(&in_project, &here, &home_ext).unwrap(),
            std::fs::canonicalize(&project).unwrap().to_string_lossy()
        );
        // A global file applies everywhere, so the check is the one for here.
        assert_eq!(
            check_dir_for(&home_ext.join("a.pirs.toml"), &here, &home_ext).unwrap(),
            here
        );
        // Anywhere else is not policy.
        for outside in [root.join("a.pirs.toml"), project.join(".pirs/a.pirs.toml")] {
            let error = check_dir_for(&outside, &here, &home_ext).unwrap_err();
            assert_eq!(
                error,
                format!(
                    "not a policy location: expected {here}/.pirs/ext/*.pirs.toml or \
                     ~/.pirs/ext/*.pirs.toml"
                )
            );
        }
        std::fs::remove_dir_all(&root).expect("cleaned up");
    }

    #[test]
    fn the_backup_sits_beside_the_file() {
        assert_eq!(
            backup_path(Path::new("/p/.pirs/ext/a.pirs.toml")),
            PathBuf::from("/p/.pirs/ext/a.pirs.toml.bak")
        );
    }

    // ------------------------------------------------------- the prompt

    #[test]
    fn the_prompt_is_the_instruction_set_then_the_instruction_then_the_intent() {
        let prompt = prompt_for("show the git branch");
        assert!(prompt.starts_with("# The pirs DSL"), "the embedded docs/dsl.md comes first");
        assert!(prompt.contains("### `[[status]]`"), "the slots are in it");
        assert!(prompt.contains("Reply with the file only, in one fenced ```toml block"), "{prompt}");
        assert!(prompt.trim_end().ends_with("Intent: show the git branch"), "the intent comes last");
    }

    #[test]
    fn a_conflict_naming_the_new_file_is_the_one_that_counts() {
        use pirs_protocol::{DslConflict, Manifest};
        let result = |files: Vec<&str>| DslCheckResult {
            files: Vec::new(),
            manifest: Manifest::default(),
            conflicts: vec![DslConflict {
                message: "duplicate status key `branch`".to_owned(),
                files: files.into_iter().map(ServerPath::from).collect(),
            }],
            rendered: String::new(),
            system_prompt: String::new(),
        };
        let path = Path::new("/p/.pirs/ext/new.pirs.toml");
        assert!(conflicts_naming(&result(vec!["/p/.pirs/ext/new.pirs.toml", "/p/.pirs/ext/old.pirs.toml"]), path));
        assert!(!conflicts_naming(&result(vec!["/p/.pirs/ext/old.pirs.toml"]), path));
        assert!(!conflicts_naming(
            &DslCheckResult {
                files: Vec::new(),
                manifest: Manifest::default(),
                conflicts: Vec::new(),
                rendered: String::new(),
                system_prompt: String::new(),
            },
            path
        ));
    }
}
