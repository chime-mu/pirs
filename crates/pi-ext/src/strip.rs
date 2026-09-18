//! TypeScript type stripping using swc's fast stripper (the same engine Node
//! uses for `--experimental-strip-types`). Erasable syntax is stripped in
//! place; if that fails (enums, parameter properties, ...) we fall back to a
//! full transform.

use swc_common::errors::{ColorConfig, Handler};
use swc_common::sync::Lrc;
use swc_common::SourceMap;
use swc_ts_fast_strip::{operate, Mode, Options};

pub fn strip_types(source: &str, filename: &str) -> Result<String, String> {
    match run(source, filename, Mode::StripOnly) {
        Ok(code) => Ok(code),
        Err(strip_err) => run(source, filename, Mode::Transform).map_err(|e| format!("{strip_err}; transform also failed: {e}")),
    }
}

fn run(source: &str, filename: &str, mode: Mode) -> Result<String, String> {
    let cm: Lrc<SourceMap> = Default::default();
    // Collect diagnostics into a buffer instead of printing them.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer = SharedWriter(buf.clone());
    let handler = Handler::with_emitter_writer(Box::new(writer), Some(cm.clone()));
    let opts = Options { module: Some(true), filename: Some(filename.to_string()), mode, ..Default::default() };
    match operate(&cm, &handler, source.to_string(), opts) {
        Ok(out) => Ok(out.code),
        Err(e) => {
            let diag = String::from_utf8_lossy(&buf.lock().unwrap()).to_string();
            let diag = diag.trim();
            if diag.is_empty() {
                Err(e.to_string())
            } else {
                Err(format!("{e}: {diag}"))
            }
        }
    }
}

struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[allow(dead_code)]
fn _unused(_: ColorConfig) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_types() {
        let src = r#"import type { X } from "y";
interface Foo { a: number }
export default function (pi: X): void { const n: number = 1 as number; pi.on("x", async (e: Foo, c) => e); }"#;
        let out = strip_types(src, "a.ts").unwrap();
        assert!(!out.contains("interface"));
        assert!(!out.contains(": number"));
        assert!(out.contains("export default function"));
    }

    #[test]
    fn transform_fallback_for_enums() {
        let src = "enum E { A, B }\nexport default function () { return E.A; }";
        let out = strip_types(src, "a.ts").unwrap();
        assert!(out.contains("export default function"));
    }
}
