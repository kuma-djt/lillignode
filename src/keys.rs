//! Turn a key press into the bytes a terminal program expects to receive,
//! and tell whether it is one of the menu keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Config, MenuKey};

/// True if `key` is one of the menu keys in the settings.
pub fn is_menu_key(config: &Config, key: &KeyEvent) -> bool {
    config.menu_keys.iter().any(|menu| matches(*menu, key))
}

fn matches(menu: MenuKey, key: &KeyEvent) -> bool {
    // Shift is ignored: some terminals report it with function keys.
    let modifiers = key.modifiers - KeyModifiers::SHIFT;
    match (menu, key.code) {
        (MenuKey::F(n), KeyCode::F(m)) => n == m && modifiers == KeyModifiers::NONE,
        (MenuKey::Ctrl(a), KeyCode::Char(b)) => {
            a == b.to_ascii_lowercase() && modifiers == KeyModifiers::CONTROL
        }
        (MenuKey::Alt(a), KeyCode::Char(b)) => a == b.to_ascii_lowercase() && modifiers == KeyModifiers::ALT,
        _ => false,
    }
}

/// `app_cursor` is true when the agent asked for application cursor keys.
/// Returns None for keys with no terminal encoding (for example a bare Shift).
pub fn encode(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let mut out: Vec<u8> = Vec::new();
    match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '/' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return None,
            };
            out.push(b);
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(if ctrl { 0x08 } else { 0x7f }),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up | KeyCode::Down | KeyCode::Right | KeyCode::Left | KeyCode::Home | KeyCode::End => {
            let letter = match key.code {
                KeyCode::Up => 'A',
                KeyCode::Down => 'B',
                KeyCode::Right => 'C',
                KeyCode::Left => 'D',
                KeyCode::Home => 'H',
                _ => 'F',
            };
            let m = modifier_param(shift, alt, ctrl);
            let s = match (m, app_cursor) {
                (Some(m), _) => format!("\x1b[1;{m}{letter}"),
                (None, true) => format!("\x1bO{letter}"),
                (None, false) => format!("\x1b[{letter}"),
            };
            return Some(s.into_bytes());
        }
        KeyCode::Insert | KeyCode::Delete | KeyCode::PageUp | KeyCode::PageDown => {
            let n = match key.code {
                KeyCode::Insert => 2,
                KeyCode::Delete => 3,
                KeyCode::PageUp => 5,
                _ => 6,
            };
            let s = match modifier_param(shift, alt, ctrl) {
                Some(m) => format!("\x1b[{n};{m}~"),
                None => format!("\x1b[{n}~"),
            };
            return Some(s.into_bytes());
        }
        KeyCode::F(n @ 1..=4) => {
            out.extend_from_slice(format!("\x1bO{}", (b'P' + n - 1) as char).as_bytes());
            return Some(out);
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            return Some(format!("\x1b[{code}~").into_bytes());
        }
        _ => return None,
    }
    // Alt sends the key prefixed with Escape.
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}

fn modifier_param(shift: bool, alt: bool, ctrl: bool) -> Option<u8> {
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    (m > 1).then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
        encode(KeyEvent::new(code, mods), false)
    }

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn the_menu_keys_are_the_keys_the_settings_name_and_no_others() {
        let c = Config::default();
        assert!(is_menu_key(&c, &press(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        assert!(is_menu_key(&c, &press(KeyCode::Char('Q'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)));
        assert!(is_menu_key(&c, &press(KeyCode::F(12), KeyModifiers::NONE)));
        assert!(is_menu_key(&c, &press(KeyCode::F(12), KeyModifiers::SHIFT)));
        assert!(!is_menu_key(&c, &press(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(!is_menu_key(&c, &press(KeyCode::Char('q'), KeyModifiers::ALT)));
        assert!(!is_menu_key(&c, &press(KeyCode::F(12), KeyModifiers::CONTROL)));
        let c = Config { menu_keys: vec![MenuKey::Alt('m'), MenuKey::F(9)], ..Config::default() };
        assert!(is_menu_key(&c, &press(KeyCode::Char('m'), KeyModifiers::ALT)));
        assert!(is_menu_key(&c, &press(KeyCode::F(9), KeyModifiers::NONE)));
        assert!(!is_menu_key(&c, &press(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        assert!(!is_menu_key(&c, &press(KeyCode::F(12), KeyModifiers::NONE)));
    }

    #[test]
    fn plain_and_control_characters() {
        assert_eq!(k(KeyCode::Char('a'), KeyModifiers::NONE), Some(b"a".to_vec()));
        assert_eq!(k(KeyCode::Char('é'), KeyModifiers::NONE), Some("é".as_bytes().to_vec()));
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), Some(vec![3]));
        assert_eq!(k(KeyCode::Char('x'), KeyModifiers::ALT), Some(b"\x1bx".to_vec()));
        assert_eq!(k(KeyCode::Enter, KeyModifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(k(KeyCode::Backspace, KeyModifiers::NONE), Some(vec![0x7f]));
    }

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE), Some(b"\x1b[A".to_vec()));
        let app = encode(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true);
        assert_eq!(app, Some(b"\x1bOA".to_vec()));
        assert_eq!(k(KeyCode::Left, KeyModifiers::CONTROL), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(k(KeyCode::Delete, KeyModifiers::NONE), Some(b"\x1b[3~".to_vec()));
    }
}
