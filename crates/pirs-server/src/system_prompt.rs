//! System prompt construction and project context loading.
//!
//! The `<docs>` section points the model at the files it needs to change how
//! pirs behaves: `docs/dsl.md` is the policy vocabulary and `docs/protocol.md`
//! the wire protocol. Both are compiled into the binary, and written to
//! `$PIRS_HOME/docs` at server start when there is no docs tree on disk, so
//! an installed or jailed pirs has them too. Phase 2 assembles the rest of
//! the prompt from the DSL's `[[prompt]]` entries on top of what this builds
//! (D-21).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub force_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub tool_snippets: HashMap<String, String>,
    pub tool_guidelines: HashMap<String, Vec<String>>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: String,
    pub sections: BTreeMap<String, String>,
    pub cwd: String,
    pub context_files: Vec<ContextFile>,
}

fn render_project_context(files: &[ContextFile]) -> String {
    let mut parts = vec!["Project-specific instructions and guidelines:".to_string()];
    for f in files {
        parts.push(format!("<project_instructions path=\"{}\">\n{}\n</project_instructions>", f.path, f.content));
    }
    parts.join("\n\n")
}

fn build_rules(selected: &[String], tool_guidelines: &HashMap<String, Vec<String>>, prompt_guidelines: &[String]) -> String {
    let mut rules: Vec<String> = Vec::new();
    let mut add = |rule: &str| {
        let r = rule.trim();
        if !r.is_empty() && !rules.iter().any(|x| x == r) {
            rules.push(r.to_string());
        }
    };
    let has = |n: &str| selected.iter().any(|s| s == n);
    if has("bash") && !has("grep") && !has("find") && !has("ls") {
        add("Use bash for file operations like ls, rg, find");
    }
    for name in selected {
        if let Some(g) = tool_guidelines.get(name) {
            for r in g {
                add(r);
            }
        }
    }
    for r in prompt_guidelines {
        add(r);
    }
    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    rules.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")
}

/// Ordered prompt sections; `preamble` is untagged, everything else is
/// wrapped in a tag of the same name.
pub fn build_system_prompt_sections(o: &BuildSystemPromptOptions) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    if let Some(custom) = &o.custom_prompt {
        sections.push(("preamble".into(), custom.clone()));
    } else {
        sections.push(("preamble".into(), "You are an expert coding assistant operating inside pirs, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.".into()));
        let visible: Vec<&String> = o.selected_tools.iter().filter(|n| o.tool_snippets.contains_key(*n)).collect();
        let tools = if visible.is_empty() { "(none)".to_string() } else { visible.iter().map(|n| format!("- {n}: {}", o.tool_snippets[*n])).collect::<Vec<_>>().join("\n") };
        sections.push(("tools".into(), format!("{tools}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.")));
        sections.push(("rules".into(), build_rules(&o.selected_tools, &o.tool_guidelines, &o.prompt_guidelines)));
        if let Some(docs) = docs_section() {
            sections.push(("docs".into(), docs));
        }
    }
    if !o.append_system_prompt.is_empty() {
        sections.push(("addendum".into(), o.append_system_prompt.clone()));
    }
    if !o.context_files.is_empty() {
        sections.push(("project_context".into(), render_project_context(&o.context_files)));
    }
    sections.push(("cwd".into(), o.cwd.replace('\\', "/")));
    for (name, content) in &o.sections {
        if !content.is_empty() {
            sections.push((name.clone(), content.clone()));
        }
    }
    sections
        .into_iter()
        .map(|(name, content)| if name == "preamble" { (name, content) } else { (name.clone(), format!("<{name}>\n{content}\n</{name}>")) })
        .collect()
}

pub fn build_system_prompt(o: &BuildSystemPromptOptions) -> String {
    if let Some(forced) = &o.force_system_prompt {
        return forced.clone();
    }
    build_system_prompt_sections(o).into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n\n")
}

/// The two documents the model needs, compiled into the binary: an installed
/// or jailed pirs carries them, with no build tree to find and nothing extra
/// to copy into an image.
const EMBEDDED_DOCS: &[(&str, &str)] = &[
    ("dsl.md", include_str!("../../../docs/dsl.md")),
    ("protocol.md", include_str!("../../../docs/protocol.md")),
];

/// Set by the test that exercises the installed-binary path, where the source
/// tree this test binary was built from must not count.
#[cfg(test)]
static SKIP_BUILD_TREE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The source tree this binary was built from (the workspace root above
/// `crates/pirs-server`). It is a build-machine path, so it is only ever a
/// candidate, never an assumption.
fn build_tree_root() -> Option<PathBuf> {
    #[cfg(test)]
    if SKIP_BUILD_TREE.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// A directory counts as the docs root when it holds `docs/protocol.md`.
fn as_docs_root(dir: PathBuf) -> Option<PathBuf> {
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    dir.join("docs").join("protocol.md").is_file().then_some(dir)
}

/// A docs tree that exists independently of the server: `PIRS_DOCS_DIR`, then
/// the build tree if it is still there.
fn external_docs_root() -> Option<PathBuf> {
    std::env::var_os("PIRS_DOCS_DIR")
        .and_then(|d| as_docs_root(PathBuf::from(d)))
        .or_else(|| build_tree_root().and_then(as_docs_root))
}

/// Where the pirs docs live: [`external_docs_root`], else `$PIRS_HOME`, where
/// [`install_embedded_docs`] writes the compiled-in copies.
pub fn docs_root() -> Option<PathBuf> {
    external_docs_root().or_else(|| as_docs_root(crate::session::get_agent_dir()))
}

/// Write the compiled-in docs to `$PIRS_HOME/docs`, unless a docs tree is
/// already on disk. Called once at server start so the `<docs>` section names
/// files that exist even for an installed or jailed binary; a copy left by an
/// older binary is overwritten when its content differs.
pub fn install_embedded_docs() {
    if external_docs_root().is_some() {
        return;
    }
    let dir = crate::session::get_agent_dir().join("docs");
    if let Err(e) = write_embedded_docs(&dir) {
        tracing::warn!(dir = %dir.display(), "could not write the built-in docs: {e}");
    }
}

fn write_embedded_docs(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, content) in EMBEDDED_DOCS {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == *content) {
            continue;
        }
        std::fs::write(&path, content)?;
    }
    Ok(())
}

/// Paths, never content: the model reads the file when it needs it. A root
/// that is only the written-out docs has no `README.md`, `examples/` or
/// `STATUS.md`, so those lines appear only when the files do.
fn docs_section() -> Option<String> {
    let root = docs_root()?;
    let docs = root.join("docs");
    let mut lines = vec!["pirs documentation (read only when the user asks about pirs itself, its policy files, tools, sessions, or how to change how it behaves):".to_string()];
    let readme = root.join("README.md");
    if readme.is_file() {
        lines.push(format!("- Main documentation: {}", readme.display()));
    }
    lines.push(format!("- Additional docs: {}", docs.display()));
    let examples = root.join("examples");
    if examples.is_dir() {
        lines.push(format!("- Examples: {}", examples.display()));
    }
    lines.push(format!(
        "- When asked to change how pirs behaves, read {} completely first and write a `*.pirs.toml` under `.pirs/ext/`. Only use slots and fields that file lists.",
        docs.join("dsl.md").display()
    ));
    lines.push(format!("- The wire protocol between the pirs server and its clients: {}", docs.join("protocol.md").display()));
    let status = root.join("STATUS.md");
    if status.is_file() {
        lines.push(format!("- Implementation status: {}", status.display()));
    }
    Some(lines.join("\n"))
}

const CONTEXT_FILE_CANDIDATES: &[&str] = &["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

fn find_context_file(dir: &Path) -> Option<PathBuf> {
    for c in CONTEXT_FILE_CANDIDATES {
        let p = dir.join(c);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Global `~/.pirs/AGENTS.md`, then every AGENTS.md / CLAUDE.md from the
/// filesystem root down to `cwd` (parents first).
pub fn load_context_files(cwd: &Path) -> Vec<ContextFile> {
    let mut out = Vec::new();
    if let Some(global) = find_context_file(&crate::session::get_agent_dir()) {
        if let Ok(content) = std::fs::read_to_string(&global) {
            out.push(ContextFile { path: global.to_string_lossy().to_string(), content: content.trim().to_string() });
        }
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut cur = Some(cwd.to_path_buf());
    while let Some(d) = cur {
        dirs.push(d.clone());
        cur = d.parent().map(|p| p.to_path_buf());
    }
    dirs.reverse();
    for d in dirs {
        if let Some(p) = find_context_file(&d) {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let content = content.trim().to_string();
                if !content.is_empty() {
                    out.push(ContextFile { path: p.to_string_lossy().to_string(), content });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_sections_in_pi_order() {
        let mut o = BuildSystemPromptOptions { cwd: "/p".into(), selected_tools: vec!["read".into(), "bash".into()], ..Default::default() };
        o.tool_snippets.insert("read".into(), "Read file contents".into());
        o.tool_guidelines.insert("read".into(), vec!["Use read to examine files instead of cat or sed.".into()]);
        o.context_files.push(ContextFile { path: "/p/AGENTS.md".into(), content: "rules".into() });
        let s = build_system_prompt(&o);
        assert!(s.starts_with("You are an expert coding assistant"));
        assert!(s.contains("<tools>\n- read: Read file contents"));
        assert!(s.contains("- Use bash for file operations like ls, rg, find"));
        assert!(s.contains("<project_instructions path=\"/p/AGENTS.md\">"));
        assert!(s.trim_end().ends_with("<cwd>\n/p\n</cwd>"));
        o.force_system_prompt = Some("forced".into());
        assert_eq!(build_system_prompt(&o), "forced");
    }

    /// Run `body` with `PIRS_DOCS_DIR` set (or cleared), serialised with the
    /// other tests that move the environment.
    fn with_docs_dir<T>(dir: Option<&Path>, body: impl FnOnce() -> T) -> T {
        let home = tempfile::tempdir().expect("tempdir");
        crate::session::tests_support::with_pirs_home(home.path(), || {
            let previous = std::env::var_os("PIRS_DOCS_DIR");
            match dir {
                Some(d) => std::env::set_var("PIRS_DOCS_DIR", d),
                None => std::env::remove_var("PIRS_DOCS_DIR"),
            }
            let out = body();
            match previous {
                Some(v) => std::env::set_var("PIRS_DOCS_DIR", v),
                None => std::env::remove_var("PIRS_DOCS_DIR"),
            }
            out
        })
    }

    #[test]
    fn the_docs_section_keys_on_protocol_md_and_points_at_the_dsl() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("docs")).expect("docs dir");
        std::fs::write(root.path().join("STATUS.md"), "phases").expect("write");
        with_docs_dir(Some(root.path()), || {
            // No `docs/protocol.md` yet: this is not a docs root.
            std::fs::write(root.path().join("docs").join("dsl.md"), "slots").expect("write");
            assert_ne!(docs_root().as_deref(), Some(root.path()));

            std::fs::write(root.path().join("docs").join("protocol.md"), "wire").expect("write");
            let section = docs_section().expect("docs section");
            assert!(section.contains("docs/dsl.md"), "{section}");
            assert!(section.contains("`*.pirs.toml` under `.pirs/ext/`"), "{section}");
            assert!(section.contains("docs/protocol.md"), "{section}");
            assert!(section.contains("STATUS.md"), "{section}");
            assert!(!section.contains("extensions.md"), "{section}");
            assert!(!section.contains("--list-extensions"), "{section}");
        });
    }

    /// An installed binary: no `PIRS_DOCS_DIR`, no build tree. The docs come
    /// out of the binary into `$PIRS_HOME/docs` and the section names them.
    #[test]
    fn the_docs_are_written_out_when_there_is_no_docs_tree_on_disk() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::session::tests_support::with_pirs_home(home.path(), || {
            let previous = std::env::var_os("PIRS_DOCS_DIR");
            std::env::remove_var("PIRS_DOCS_DIR");
            SKIP_BUILD_TREE.store(true, std::sync::atomic::Ordering::SeqCst);

            assert_eq!(docs_root(), None, "nothing on disk yet");
            install_embedded_docs();

            let docs = home.path().join(".pirs").join("docs");
            for (name, content) in EMBEDDED_DOCS {
                assert_eq!(std::fs::read_to_string(docs.join(name)).expect(name), *content);
            }
            // A stale copy from an older binary is replaced.
            std::fs::write(docs.join("dsl.md"), "old").expect("write");
            install_embedded_docs();
            assert_eq!(std::fs::read_to_string(docs.join("dsl.md")).expect("dsl.md"), EMBEDDED_DOCS[0].1);

            let o = BuildSystemPromptOptions { cwd: "/p".into(), ..Default::default() };
            let prompt = build_system_prompt(&o);
            assert!(prompt.contains(&docs.join("dsl.md").display().to_string()), "{prompt}");
            assert!(prompt.contains(&docs.join("protocol.md").display().to_string()), "{prompt}");
            // No README, examples or STATUS.md next to the written-out docs.
            assert!(!prompt.contains("README.md"), "{prompt}");
            assert!(!prompt.contains("STATUS.md"), "{prompt}");

            SKIP_BUILD_TREE.store(false, std::sync::atomic::Ordering::SeqCst);
            if let Some(v) = previous {
                std::env::set_var("PIRS_DOCS_DIR", v);
            }
        });
    }
}
