//! Best-effort repair of truncated JSON tool arguments.

use serde_json::Value;

/// Try to close unterminated strings/objects/arrays so that a truncated JSON
/// document parses. Returns an empty object if nothing helps.
pub fn salvage(raw: &str) -> Value {
    let mut s = raw.trim().to_string();
    if s.is_empty() {
        return Value::Object(Default::default());
    }
    // Determine open structures.
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for ch in s.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_string {
        s.push('"');
    }
    // Drop a trailing comma or colon.
    while s.ends_with(',') || s.ends_with(':') {
        s.pop();
    }
    while let Some(c) = stack.pop() {
        s.push(c);
    }
    serde_json::from_str(&s).unwrap_or_else(|_| Value::Object(Default::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closes_truncated() {
        let v = salvage(r#"{"path": "a.txt", "content": "hello"#);
        assert_eq!(v["path"], "a.txt");
        assert_eq!(v["content"], "hello");
    }
}
