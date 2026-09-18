//! Path resolution helpers (port of pi's `path-utils.ts` / `utils/paths.ts`).

use std::path::{Component, Path, PathBuf};

const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

fn is_unicode_space(c: char) -> bool {
    matches!(c, '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

/// Normalize user-supplied path text: collapse unicode spaces, strip a leading
/// `@`, expand `~`, and unwrap `file://` URLs.
pub fn expand_path(input: &str) -> PathBuf {
    let mut normalized: String = input.chars().map(|c| if is_unicode_space(c) { ' ' } else { c }).collect();
    if let Some(rest) = normalized.strip_prefix('@') {
        normalized = rest.to_string();
    }
    expand_tilde(&normalized)
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if input == "~" {
            return home;
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    if let Some(rest) = input.strip_prefix("file://") {
        return PathBuf::from(rest);
    }
    PathBuf::from(input)
}

/// Lexically normalize `.` and `..` components the way `path.resolve` does.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // Parent of root stays at root; parent of a relative start is dropped.
                    if out.as_os_str().is_empty() {
                        continue;
                    }
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(if path.is_absolute() { "/" } else { "." })
    } else {
        out
    }
}

/// Resolve a path relative to `cwd`, handling `~` and absolute paths.
pub fn resolve_to_cwd(path: &str, cwd: &Path) -> PathBuf {
    let expanded = expand_path(path);
    let joined = if expanded.is_absolute() { expanded } else { expand_tilde(&cwd.to_string_lossy()).join(expanded) };
    normalize_lexically(&joined)
}

pub fn path_exists(path: &Path) -> bool {
    path.exists()
}

fn try_macos_screenshot_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(idx) = rest.find(' ') {
        let after = &rest[idx + 1..];
        let matched = ["AM.", "PM.", "am.", "pm."].iter().find(|s| after.starts_with(*s));
        out.push_str(&rest[..idx]);
        match matched {
            Some(_) => out.push(NARROW_NO_BREAK_SPACE),
            None => out.push(' '),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn try_curly_quote_variant(path: &str) -> String {
    path.replace('\'', "\u{2019}")
}

/// Resolve a path for reading, trying macOS screenshot naming variants when
/// the literal path does not exist.
pub fn resolve_read_path(path: &str, cwd: &Path) -> PathBuf {
    let resolved = resolve_to_cwd(path, cwd);
    if resolved.exists() {
        return resolved;
    }
    let resolved_str = resolved.to_string_lossy().to_string();
    for candidate in [try_macos_screenshot_path(&resolved_str), try_curly_quote_variant(&resolved_str)] {
        if candidate != resolved_str {
            let candidate_path = PathBuf::from(&candidate);
            if candidate_path.exists() {
                return candidate_path;
            }
        }
    }
    resolved
}

/// Relative posix-style path of `path` under `root`, if it is inside it.
pub fn relative_posix(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute() {
        let cwd = Path::new("/work/dir");
        assert_eq!(resolve_to_cwd("a/b.txt", cwd), PathBuf::from("/work/dir/a/b.txt"));
        assert_eq!(resolve_to_cwd("/abs/x", cwd), PathBuf::from("/abs/x"));
        assert_eq!(resolve_to_cwd("../x", cwd), PathBuf::from("/work/x"));
        assert_eq!(resolve_to_cwd("./x/./y", cwd), PathBuf::from("/work/dir/x/y"));
        assert_eq!(resolve_to_cwd("@a.txt", cwd), PathBuf::from("/work/dir/a.txt"));
        assert_eq!(resolve_to_cwd(".", cwd), PathBuf::from("/work/dir"));
    }

    #[test]
    fn expands_tilde() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(resolve_to_cwd("~", Path::new("/x")), home);
        assert_eq!(resolve_to_cwd("~/foo", Path::new("/x")), home.join("foo"));
    }

    #[test]
    fn macos_screenshot_variant() {
        assert_eq!(
            try_macos_screenshot_path("/a/Screenshot 2024 at 1.00.00 PM.png"),
            "/a/Screenshot 2024 at 1.00.00\u{202F}PM.png"
        );
    }

    #[test]
    fn relative_posix_works() {
        assert_eq!(relative_posix(Path::new("/a/b/c"), Path::new("/a")), Some("b/c".to_string()));
        assert_eq!(relative_posix(Path::new("/x"), Path::new("/a")), None);
    }
}
