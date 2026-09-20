//! System prompt construction and project context loading (port of pi's
//! `system-prompt.ts` and the AGENTS.md discovery in `resource-loader.ts`).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ContextFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BuildSystemPromptOptions {
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
pub(crate) fn build_system_prompt_sections(o: &BuildSystemPromptOptions) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    if let Some(custom) = &o.custom_prompt {
        sections.push(("preamble".into(), custom.clone()));
    } else {
        sections.push(("preamble".into(), "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.".into()));
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

pub(crate) fn build_system_prompt(o: &BuildSystemPromptOptions) -> String {
    if let Some(forced) = &o.force_system_prompt {
        return forced.clone();
    }
    build_system_prompt_sections(o).into_iter().map(|(_, c)| c).collect::<Vec<_>>().join("\n\n")
}

/// Where the pirs docs live: `PIRS_DOCS_DIR`, or the source tree this binary
/// was built from (workspace root of `crates/pi-cli`), or `~/.pi/agent/pirs`.
pub(crate) fn docs_root() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("PIRS_DOCS_DIR") {
        candidates.push(PathBuf::from(d));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    candidates.push(crate::session::get_agent_dir().join("pirs"));
    candidates.into_iter().map(|c| std::fs::canonicalize(&c).unwrap_or(c)).find(|c| c.join("docs").join("extensions.md").is_file())
}

fn docs_section() -> Option<String> {
    let root = docs_root()?;
    let docs = root.join("docs");
    let examples = root.join("examples");
    Some(format!(
        "pirs documentation (read only when the user asks about pirs itself, its extensions, tools, sessions, or how to extend it):
- Main documentation: {}
- Additional docs: {}
- Examples: {} (extensions)
- When asked to write or debug an extension, read docs/extensions.md completely first, then the relevant files under examples/extensions/. Consult docs/pi-extensions-reference.md for detailed event payload fields. Only use APIs listed as supported in docs/extensions.md.
- Test an extension with `pirs --list-extensions -e <file>` before telling the user it works.
- Implementation status: {}",
        root.join("README.md").display(),
        docs.display(),
        examples.display(),
        root.join("STATUS.md").display()
    ))
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

/// Global `~/.pi/agent/AGENTS.md`, then every AGENTS.md / CLAUDE.md from the
/// filesystem root down to `cwd` (parents first).
pub(crate) fn load_context_files(cwd: &Path) -> Vec<ContextFile> {
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
}
