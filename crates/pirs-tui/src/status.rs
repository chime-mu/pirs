//! The status line: a `tui.toml` format string over `ui.status` keys and
//! `{loop.state}`. `{name}` is replaced by the key's text, or by nothing when
//! the key is unknown or cleared; everything else is copied through.

/// Expand `format` with `lookup`.
pub(crate) fn render(format: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(format.len());
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if !after[..close].contains('{') => {
                let key = after[..close].trim();
                if let Some(text) = lookup(key) {
                    out.push_str(&text);
                }
                rest = &after[close + 1..];
            }
            _ => {
                // An unclosed brace is text.
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_known_keys_and_blanks_unknown_ones() {
        let lookup = |key: &str| match key {
            "branch" => Some("main".to_owned()),
            "loop.state" => Some("idle".to_owned()),
            _ => None,
        };
        assert_eq!(render("{branch} · {loop.state}", lookup), "main · idle");
        assert_eq!(render("[{nope}] {branch}", lookup), "[] main");
        assert_eq!(render("plain", lookup), "plain");
        assert_eq!(render("open { brace", lookup), "open { brace");
    }
}
