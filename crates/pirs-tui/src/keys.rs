//! Key names: parsing `tui.toml` bindings and headless-script keys, and
//! mapping crossterm key events onto the same type.
//!
//! A name is `[mod-]*key`, with `-` or `+` between parts. Modifiers are
//! `ctrl`, `alt` and `shift`; keys are a single character, `enter`, `esc`,
//! `tab`, `backtab`, `space`, `backspace`, `delete`, `insert`, `up`, `down`,
//! `left`, `right`, `home`, `end`, `pageup`, `pagedown` and `f1`…`f12`.
//! `shift-tab` is `backtab`; a shifted character is its uppercase form.

use std::fmt;

/// The key itself, without modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Code {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// A key press: a code and its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    pub(crate) fn plain(code: Code) -> Key {
        Key {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// The character this key types, if it is an unmodified character.
    pub(crate) fn typed_char(&self) -> Option<char> {
        match self.code {
            Code::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }

    /// Parse a key name. See the module documentation for the grammar.
    pub(crate) fn parse(name: &str) -> Result<Key, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("empty key name".to_owned());
        }
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Ok(Key::plain(Code::Char(c)));
        }
        let lower = name.to_lowercase();
        let mut tokens: Vec<&str> = lower.split(['-', '+']).collect();
        // `ctrl--` or `alt-+`: the key is the separator character itself.
        let mut trailing_separator = None;
        while tokens.last() == Some(&"") {
            tokens.pop();
            trailing_separator = lower.chars().last();
        }
        let key_token = match trailing_separator {
            Some(sep) => {
                tokens.pop();
                tokens.push(match sep {
                    '-' => "-",
                    _ => "+",
                });
                tokens.pop().unwrap_or("-")
            }
            None => match tokens.pop() {
                Some(token) => token,
                None => return Err(format!("`{name}` names no key")),
            },
        };
        let mut key = Key::plain(match key_token {
            "enter" | "return" | "cr" => Code::Enter,
            "esc" | "escape" => Code::Esc,
            "tab" => Code::Tab,
            "backtab" => Code::BackTab,
            "space" => Code::Char(' '),
            "backspace" | "bs" => Code::Backspace,
            "delete" | "del" => Code::Delete,
            "insert" | "ins" => Code::Insert,
            "up" => Code::Up,
            "down" => Code::Down,
            "left" => Code::Left,
            "right" => Code::Right,
            "home" => Code::Home,
            "end" => Code::End,
            "pageup" | "pgup" => Code::PageUp,
            "pagedown" | "pgdn" => Code::PageDown,
            other => {
                if let Some(n) = other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if (1..=12).contains(&n) {
                        Code::F(n)
                    } else {
                        return Err(format!("`{name}`: no function key f{n}"));
                    }
                } else {
                    let mut cs = other.chars();
                    match (cs.next(), cs.next()) {
                        (Some(c), None) => Code::Char(c),
                        _ => return Err(format!("`{name}`: unknown key `{other}`")),
                    }
                }
            }
        });
        for token in tokens {
            match token {
                "ctrl" | "control" | "c" => key.ctrl = true,
                "alt" | "meta" | "opt" | "m" | "a" => key.alt = true,
                "shift" | "s" => key.shift = true,
                other => return Err(format!("`{name}`: unknown modifier `{other}`")),
            }
        }
        Ok(key.normalised())
    }

    /// Fold `shift` into the code where the code carries it itself.
    fn normalised(mut self) -> Key {
        match self.code {
            Code::Tab if self.shift => {
                self.code = Code::BackTab;
                self.shift = false;
            }
            Code::BackTab => self.shift = false,
            Code::Char(c) if self.shift => {
                self.code = Code::Char(c.to_uppercase().next().unwrap_or(c));
                self.shift = false;
            }
            _ => {}
        }
        self
    }

    /// Map a crossterm key event; `None` for releases and keys this UI has
    /// no name for.
    pub(crate) fn from_crossterm(event: &crossterm::event::KeyEvent) -> Option<Key> {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let code = match event.code {
            KeyCode::Char(c) => Code::Char(c),
            KeyCode::Enter => Code::Enter,
            KeyCode::Esc => Code::Esc,
            KeyCode::Tab => Code::Tab,
            KeyCode::BackTab => Code::BackTab,
            KeyCode::Backspace => Code::Backspace,
            KeyCode::Delete => Code::Delete,
            KeyCode::Insert => Code::Insert,
            KeyCode::Up => Code::Up,
            KeyCode::Down => Code::Down,
            KeyCode::Left => Code::Left,
            KeyCode::Right => Code::Right,
            KeyCode::Home => Code::Home,
            KeyCode::End => Code::End,
            KeyCode::PageUp => Code::PageUp,
            KeyCode::PageDown => Code::PageDown,
            KeyCode::F(n) => Code::F(n),
            _ => return None,
        };
        let key = Key {
            code,
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
            shift: event.modifiers.contains(KeyModifiers::SHIFT),
        };
        Some(key.normalised())
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl-")?;
        }
        if self.alt {
            f.write_str("alt-")?;
        }
        if self.shift {
            f.write_str("shift-")?;
        }
        match self.code {
            Code::Char(' ') => f.write_str("space"),
            Code::Char(c) => write!(f, "{c}"),
            Code::Enter => f.write_str("enter"),
            Code::Esc => f.write_str("esc"),
            Code::Tab => f.write_str("tab"),
            Code::BackTab => f.write_str("shift-tab"),
            Code::Backspace => f.write_str("backspace"),
            Code::Delete => f.write_str("delete"),
            Code::Insert => f.write_str("insert"),
            Code::Up => f.write_str("up"),
            Code::Down => f.write_str("down"),
            Code::Left => f.write_str("left"),
            Code::Right => f.write_str("right"),
            Code::Home => f.write_str("home"),
            Code::End => f.write_str("end"),
            Code::PageUp => f.write_str("pageup"),
            Code::PageDown => f.write_str("pagedown"),
            Code::F(n) => write!(f, "f{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for name in [
            "j",
            "1",
            "/",
            "enter",
            "esc",
            "tab",
            "shift-tab",
            "ctrl-q",
            "alt-enter",
            "down",
            "pageup",
            "f5",
            "ctrl-alt-x",
            "space",
        ] {
            let key = Key::parse(name).unwrap();
            assert_eq!(Key::parse(&key.to_string()).unwrap(), key, "{name}");
        }
        assert_eq!(Key::parse("shift-tab").unwrap(), Key::plain(Code::BackTab));
        assert_eq!(Key::parse("backtab").unwrap(), Key::plain(Code::BackTab));
        assert_eq!(Key::parse("ctrl+q").unwrap(), Key::parse("ctrl-q").unwrap());
        assert_eq!(Key::parse("ctrl--").unwrap().code, Code::Char('-'));
        assert_eq!(Key::parse("shift-a").unwrap().code, Code::Char('A'));
        assert!(Key::parse("hyper-x").is_err());
        assert!(Key::parse("f99").is_err());
        assert!(Key::parse("").is_err());
    }

    #[test]
    fn crossterm_mapping() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let e = KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT);
        assert_eq!(Key::from_crossterm(&e), Some(Key::plain(Code::Char('Q'))));
        let e = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(Key::from_crossterm(&e), Some(Key::plain(Code::BackTab)));
        let e = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(Key::from_crossterm(&e), Key::parse("alt-enter").ok());
    }
}
