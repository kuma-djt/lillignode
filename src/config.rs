//! The user's settings: one small JSON file beside the saved workspace.
//!
//!   { "menu_keys": ["ctrl+q", "f12"], "bell": true, "billing": "key" }
//!
//! `bell`: ring the terminal bell once when an agent starts needing you.
//! On unless set to false.
//!
//! `billing`: "key" if Claude Code is paid for by API key, "subscription" if
//! by a subscription. With "key" the sidebar shows an estimate in dollars;
//! otherwise it shows tokens. Left out, Lillignode does not assume either.
//!
//! A missing file means the defaults. A file that cannot be used also means
//! the defaults, with a message saying what was wrong: a mistake in this
//! file must never leave the user without a way into the menu.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

pub const FILE: &str = "config.json";
pub const DEFAULT_MENU_KEYS: [&str; 2] = ["ctrl+q", "f12"];

/// A key that opens Lillignode's menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuKey {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl MenuKey {
    pub fn parse(spec: &str) -> Result<MenuKey, String> {
        let lower = spec.trim().to_ascii_lowercase();
        let bad = || {
            format!("\"{spec}\" is not a key Lillignode can use (use ctrl+letter, alt+letter, or f1 to f12)")
        };
        if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            return if (1..=12).contains(&n) {
                Ok(MenuKey { code: KeyCode::F(n), modifiers: KeyModifiers::NONE })
            } else {
                Err(bad())
            };
        }
        let (modifiers, letter) = match lower.split_once('+') {
            Some(("ctrl", l)) => (KeyModifiers::CONTROL, l),
            Some(("alt", l)) => (KeyModifiers::ALT, l),
            _ => return Err(bad()),
        };
        let mut chars = letter.chars();
        let (Some(c @ 'a'..='z'), None) = (chars.next(), chars.next()) else { return Err(bad()) };
        // These reach a terminal program as Enter, Tab, Backspace, or are the
        // keys every agent relies on to interrupt and to end input.
        if modifiers == KeyModifiers::CONTROL && "cdhijm".contains(c) {
            return Err(format!("\"{spec}\" cannot be a menu key because agents need it"));
        }
        Ok(MenuKey { code: KeyCode::Char(c), modifiers })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        // Shift is ignored: some terminals report it with function keys.
        let modifiers = key.modifiers - KeyModifiers::SHIFT;
        match (self.code, key.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                a == b.to_ascii_lowercase() && modifiers == self.modifiers
            }
            (a, b) => a == b && modifiers == self.modifiers,
        }
    }

    /// How the key is written on screen: "Ctrl+Q", "F12".
    pub fn label(&self) -> String {
        match (self.code, self.modifiers) {
            (KeyCode::F(n), _) => format!("F{n}"),
            (KeyCode::Char(c), m) if m == KeyModifiers::ALT => format!("Alt+{}", c.to_ascii_uppercase()),
            (KeyCode::Char(c), _) => format!("Ctrl+{}", c.to_ascii_uppercase()),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Never empty.
    pub menu_keys: Vec<MenuKey>,
    /// Ring the terminal bell once each time an agent starts needing you.
    pub bell: bool,
    /// How the user pays for Claude Code, if they have said.
    pub billing: Billing,
}

/// How the user pays, which decides whether a dollar figure means anything
/// to them. Lillignode cannot tell by itself, so it is Unsaid until the user
/// says: `"billing": "key"` or `"billing": "subscription"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Billing {
    #[default]
    Unsaid,
    /// By API key: billed per token, so an estimate in dollars is shown.
    Key,
    /// A subscription: not billed per token. Tokens only.
    Subscription,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            menu_keys: DEFAULT_MENU_KEYS
                .iter()
                .map(|k| MenuKey::parse(k).expect("defaults are valid"))
                .collect(),
            bell: true,
            billing: Billing::Unsaid,
        }
    }
}

impl Config {
    /// "Ctrl+Q or F12"
    pub fn menu_keys_label(&self) -> String {
        self.menu_keys.iter().map(MenuKey::label).collect::<Vec<_>>().join(" or ")
    }

    pub fn is_menu_key(&self, key: &KeyEvent) -> bool {
        self.menu_keys.iter().any(|k| k.matches(key))
    }

    /// Name function keys first, keeping the user's order otherwise. For
    /// terminals that take Ctrl and Alt keys for themselves (VS Code takes
    /// Ctrl+Q), so the first key named on screen is one that works there.
    /// Every key still opens the menu.
    pub fn function_keys_first(&mut self) {
        self.menu_keys.sort_by_key(|k| !matches!(k.code, KeyCode::F(_)));
    }
}

/// The terminal Lillignode is running in, from `TERM_PROGRAM`, when it is one
/// that keeps Ctrl+Q for itself.
pub fn terminal_takes_ctrl_keys(term_program: Option<&str>) -> bool {
    term_program == Some("vscode")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    menu_keys: Option<Vec<String>>,
    bell: Option<bool>,
    billing: Option<String>,
}

fn parse(text: &str) -> Result<Config, String> {
    let file: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut config = Config::default();
    if let Some(bell) = file.bell {
        config.bell = bell;
    }
    config.billing = match file.billing.as_deref() {
        None => Billing::Unsaid,
        Some("key") => Billing::Key,
        Some("subscription") => Billing::Subscription,
        Some(other) => {
            return Err(format!("\"billing\" is \"{other}\"; it can be \"key\" or \"subscription\""));
        }
    };
    let Some(specs) = file.menu_keys else { return Ok(config) };
    if specs.is_empty() {
        return Err("\"menu_keys\" is empty, which would leave no way into the menu".to_string());
    }
    config.menu_keys = specs.iter().map(|s| MenuKey::parse(s)).collect::<Result<Vec<_>, _>>()?;
    Ok(config)
}

/// Returns the settings to use and, if the file was there but unusable, what
/// to tell the user.
pub fn load(dir: &Path) -> (Config, Option<String>) {
    let path = dir.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => {
            let config = Config::default();
            let problem = format!(
                "Your settings file {} could not be read ({e}). Using the defaults: {} opens the menu.",
                path.display(),
                config.menu_keys_label()
            );
            return (config, Some(problem));
        }
    };
    match parse(&text) {
        Ok(config) => (config, None),
        Err(why) => {
            let config = Config::default();
            let problem = format!(
                "Your settings file {} was not used: {why}. Using the defaults: {} opens the menu. The file was not changed.",
                path.display(),
                config.menu_keys_label()
            );
            (config, Some(problem))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn defaults_are_two_different_kinds_of_key() {
        let c = Config::default();
        assert_eq!(c.menu_keys_label(), "Ctrl+Q or F12");
        assert!(c.is_menu_key(&press(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        assert!(c.is_menu_key(&press(KeyCode::F(12), KeyModifiers::NONE)));
        assert!(!c.is_menu_key(&press(KeyCode::Char('q'), KeyModifiers::NONE)));
    }

    #[test]
    fn reads_the_users_keys() {
        let c = parse(r#"{"menu_keys": ["Alt+M", "f9"]}"#).unwrap();
        assert_eq!(c.menu_keys_label(), "Alt+M or F9");
        assert!(c.is_menu_key(&press(KeyCode::Char('m'), KeyModifiers::ALT)));
        assert!(!c.is_menu_key(&press(KeyCode::Char('q'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn in_vs_code_the_function_key_is_named_first_and_both_still_work() {
        assert!(terminal_takes_ctrl_keys(Some("vscode")));
        assert!(!terminal_takes_ctrl_keys(Some("WezTerm")));
        assert!(!terminal_takes_ctrl_keys(None));
        let mut c = Config::default();
        c.function_keys_first();
        assert_eq!(c.menu_keys_label(), "F12 or Ctrl+Q");
        assert!(c.is_menu_key(&press(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        // The user's own order is kept among keys of one kind.
        let mut c = parse(r#"{"menu_keys": ["alt+m", "f9", "ctrl+t", "f2"]}"#).unwrap();
        c.function_keys_first();
        assert_eq!(c.menu_keys_label(), "F9 or F2 or Alt+M or Ctrl+T");
    }

    #[test]
    fn the_bell_is_on_unless_switched_off() {
        assert!(Config::default().bell);
        assert!(!parse(r#"{"bell": false}"#).unwrap().bell);
        let both = parse(r#"{"bell": false, "menu_keys": ["f9"]}"#).unwrap();
        assert!(!both.bell);
        assert_eq!(both.menu_keys_label(), "F9");
        assert!(parse(r#"{"bell": "off"}"#).is_err(), "a value that is not true or false is reported");
    }

    #[test]
    fn billing_is_unsaid_unless_the_user_says_and_a_misspelling_is_reported() {
        assert_eq!(Config::default().billing, Billing::Unsaid);
        assert_eq!(parse(r#"{"billing": "key"}"#).unwrap().billing, Billing::Key);
        assert_eq!(parse(r#"{"billing": "subscription"}"#).unwrap().billing, Billing::Subscription);
        let problem = parse(r#"{"billing": "api"}"#).unwrap_err();
        assert!(problem.contains("it can be \"key\" or \"subscription\""), "{problem}");
        assert!(parse(r#"{"billing": true}"#).is_err());
    }

    #[test]
    fn refuses_settings_that_would_lock_the_user_out_or_break_agents() {
        assert!(parse(r#"{"menu_keys": []}"#).unwrap_err().contains("no way into the menu"));
        assert!(parse(r#"{"menu_keys": ["ctrl+c"]}"#).unwrap_err().contains("agents need it"));
        assert!(parse(r#"{"menu_keys": ["q"]}"#).is_err());
        assert!(parse(r#"{"menu_keys": ["f13"]}"#).is_err());
        // A misspelt setting is reported, not ignored.
        assert!(parse(r#"{"menu_key": ["f9"]}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn an_unusable_file_falls_back_to_defaults_and_says_so() {
        let dir = std::env::temp_dir().join(format!("lillignode-test-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir), (Config::default(), None));
        std::fs::write(dir.join(FILE), r#"{"menu_keys": ["ctrl+c"]}"#).unwrap();
        let (config, problem) = load(&dir);
        assert_eq!(config, Config::default());
        let problem = problem.unwrap();
        assert!(problem.contains("Ctrl+Q or F12 opens the menu"), "{problem}");
        assert!(problem.contains("was not changed"), "{problem}");
    }
}
