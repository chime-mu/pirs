//! Module resolution and loading for extensions: relative imports, TypeScript
//! files, virtual pi/node modules, and simple `node_modules` ESM packages.

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Module};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const VIRTUAL_PREFIX: &str = "pirs-virtual:";

/// Bare specifiers we serve from embedded sources.
pub fn virtual_module_source(name: &str) -> Option<&'static str> {
    Some(match name {
        "typebox" | "@sinclair/typebox" | "typebox/compile" | "typebox/value" | "@sinclair/typebox/value" | "@sinclair/typebox/compile" => {
            include_str!("js/typebox.js")
        }
        "@earendil-works/pi-ai" | "@mariozechner/pi-ai" | "@earendil-works/pi-ai/compat" | "@mariozechner/pi-ai/compat" => {
            include_str!("js/pi-ai.js")
        }
        "@earendil-works/pi-coding-agent" | "@mariozechner/pi-coding-agent" => include_str!("js/pi-coding-agent.js"),
        "@earendil-works/pi-agent-core" | "@mariozechner/pi-agent-core" => include_str!("js/pi-agent-core.js"),
        "@earendil-works/pi-tui" | "@mariozechner/pi-tui" => include_str!("js/pi-tui.js"),
        "node:fs" | "fs" => include_str!("js/node-fs.js"),
        "node:fs/promises" | "fs/promises" => include_str!("js/node-fs-promises.js"),
        "node:path" | "path" | "node:path/posix" | "path/posix" => include_str!("js/node-path.js"),
        "node:os" | "os" => include_str!("js/node-os.js"),
        "node:child_process" | "child_process" => include_str!("js/node-child-process.js"),
        "node:util" | "util" => include_str!("js/node-util.js"),
        "node:url" | "url" => include_str!("js/node-url.js"),
        "node:crypto" | "crypto" => include_str!("js/node-crypto.js"),
        "node:process" | "process" => include_str!("js/node-process.js"),
        "node:events" | "events" => include_str!("js/node-events.js"),
        "node:readline" | "readline" | "node:readline/promises" | "readline/promises" => include_str!("js/node-readline.js"),
        "node:module" | "module" => include_str!("js/node-module.js"),
        _ => return None,
    })
}

fn probe_file(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    let s = base.to_string_lossy().to_string();
    for ext in ["ts", "js", "mjs", "mts", "json"] {
        let p = PathBuf::from(format!("{s}.{ext}"));
        if p.is_file() {
            return Some(p);
        }
    }
    // TS convention: `./foo.js` may refer to `./foo.ts`.
    if let Some(stem) = s.strip_suffix(".js") {
        let p = PathBuf::from(format!("{stem}.ts"));
        if p.is_file() {
            return Some(p);
        }
    }
    if base.is_dir() {
        for idx in ["index.ts", "index.js", "index.mjs"] {
            let p = base.join(idx);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn resolve_package_entry(pkg_dir: &Path) -> Option<PathBuf> {
    let pkg_json = pkg_dir.join("package.json");
    if let Ok(text) = std::fs::read_to_string(&pkg_json) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let mut candidates: Vec<String> = Vec::new();
            match v.get("exports") {
                Some(Value::String(s)) => candidates.push(s.clone()),
                Some(Value::Object(o)) => {
                    let dot = o.get(".").cloned().unwrap_or(Value::Object(o.clone()));
                    collect_export_targets(&dot, &mut candidates);
                }
                _ => {}
            }
            for key in ["module", "main"] {
                if let Some(s) = v.get(key).and_then(|m| m.as_str()) {
                    candidates.push(s.to_string());
                }
            }
            for c in candidates {
                if let Some(p) = probe_file(&pkg_dir.join(c)) {
                    return Some(p);
                }
            }
        }
    }
    probe_file(&pkg_dir.join("index"))
}

fn collect_export_targets(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Object(o) => {
            for key in ["import", "module", "default", "require", "node"] {
                if let Some(t) = o.get(key) {
                    collect_export_targets(t, out);
                }
            }
        }
        _ => {}
    }
}

fn resolve_node_module(from_dir: &Path, spec: &str) -> Option<PathBuf> {
    let (pkg, sub) = if spec.starts_with('@') {
        let mut parts = spec.splitn(3, '/');
        let scope = parts.next()?;
        let name = parts.next()?;
        (format!("{scope}/{name}"), parts.next().map(|s| s.to_string()))
    } else {
        let mut parts = spec.splitn(2, '/');
        (parts.next()?.to_string(), parts.next().map(|s| s.to_string()))
    };
    let mut dir = Some(from_dir.to_path_buf());
    while let Some(d) = dir {
        let pkg_dir = d.join("node_modules").join(&pkg);
        if pkg_dir.is_dir() {
            return match &sub {
                Some(sub) => probe_file(&pkg_dir.join(sub)),
                None => resolve_package_entry(&pkg_dir),
            };
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

pub struct PiResolver;

impl Resolver for PiResolver {
    fn resolve<'js>(&mut self, _ctx: &Ctx<'js>, base: &str, name: &str, _attrs: Option<ImportAttributes<'js>>) -> rquickjs::Result<String> {
        if virtual_module_source(name).is_some() {
            return Ok(format!("{VIRTUAL_PREFIX}{name}"));
        }
        let base_path = Path::new(base.strip_prefix(VIRTUAL_PREFIX).unwrap_or(base));
        let base_dir = if base_path.is_dir() { base_path.to_path_buf() } else { base_path.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
        if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') || name.starts_with("file://") {
            let raw = name.strip_prefix("file://").unwrap_or(name);
            let candidate = if Path::new(raw).is_absolute() { PathBuf::from(raw) } else { base_dir.join(raw) };
            let normalized = normalize(&candidate);
            return probe_file(&normalized)
                .map(|p| p.to_string_lossy().to_string())
                .ok_or_else(|| rquickjs::Error::new_resolving_message(base, name, format!("file not found: {}", normalized.display())));
        }
        if let Some(p) = resolve_node_module(&base_dir, name) {
            return Ok(p.to_string_lossy().to_string());
        }
        Err(rquickjs::Error::new_resolving_message(
            base,
            name,
            "pirs cannot resolve this module. Built-in virtual modules: typebox, @earendil-works/pi-*, node:fs/path/os/child_process/util/url/crypto/process/events. npm packages must be ESM and installed in a node_modules folder next to the extension.",
        ))
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub struct PiLoader;

impl Loader for PiLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str, _attrs: Option<ImportAttributes<'js>>) -> rquickjs::Result<Module<'js, Declared>> {
        if let Some(virt) = name.strip_prefix(VIRTUAL_PREFIX) {
            let src = virtual_module_source(virt).ok_or_else(|| rquickjs::Error::new_loading(name))?;
            return Module::declare(ctx.clone(), name, src);
        }
        let path = Path::new(name);
        let raw = std::fs::read_to_string(path).map_err(|e| rquickjs::Error::new_loading_message(name, e.to_string()))?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let source = match ext {
            "ts" | "mts" | "cts" | "tsx" => crate::strip::strip_types(&raw, name).map_err(|e| rquickjs::Error::new_loading_message(name, format!("TypeScript error: {e}")))?,
            "json" => format!("export default {};", raw.trim()),
            _ => raw,
        };
        Module::declare(ctx.clone(), name, source)
    }
}
