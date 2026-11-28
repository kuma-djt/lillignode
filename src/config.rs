//! The user's settings: one small JSON file beside the saved workspace.
//!
//!   { "menu_keys": ["ctrl+q", "f12"], "bell": true, "cost": true,
//!     "cost_warning_tokens": 500000, "billing": "key" }
//!
//! `bell`: ring the terminal bell once when an agent starts needing you.
//! On unless set to false.
//!
//! `cost`: show what each Claude Code conversation has used. Off unless set
//! to true. While it is off Lillignode reads none of Claude Code's records.
//!
//! `cost_warning_tokens`: a count of tokens. A conversation that reaches it
//! is marked in the sidebar and named once in a notice. It is a warning
//! only: nothing is stopped. Needs `cost` to be true.
//!
//! `billing`: "key" if Claude Code is paid for by API key, "subscription" if
//! by a subscription. With "key" the sidebar shows an estimate in dollars;
//! otherwise it shows tokens. Left out, Lillignode does not assume either.
//!
//! `colors`: "auto" (the default: Lillignode works it out from the terminal),
//! "full", "256", "16" or "none". For a terminal whose colours come out
//! wrong.
//!
//! `page`: true to serve a read-only page on this machine, for a browser:
//! each agent's status and screen. Off unless set to true. It listens only
//! on this machine (127.0.0.1), at `page_port` (7501 unless said), under a
//! random word in its address. Anyone with the whole address can read it.
//!
//! `page_can_type`: true to let that page type to agents as well: a text
//! box under the agent's terminal, sent as if typed, then Enter. Off unless
//! set to true, whatever `page` is. Needs `page` to be true.
//!
//! `page_can_act`: true to let that page act on the cards of its Board:
//! approve, revise, hold, done, got it. What is done is added to a file
//! beside the workspace, `board-outbox.jsonl`; the Board's own file is
//! never written. Off unless set to true, whatever `page` and
//! `page_can_type` are. Needs `page` to be true.
//!
//! A missing file means the defaults. A file that cannot be used also means
//! the defaults, with a message saying what was wrong: a mistake in this
//! file must never leave the user without a way into the menu.

use std::path::Path;

use serde::Deserialize;

pub const FILE: &str = "config.json";
pub const DEFAULT_MENU_KEYS: [&str; 2] = ["ctrl+q", "f12"];
/// Where the page is served unless the settings file says otherwise.
pub const DEFAULT_PAGE_PORT: u16 = 7501;

/// A key that opens Lillignode's menu, as the settings file names it. The
/// terminal face decides whether a key pressed is this one (`keys`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    /// Ctrl and a lower-case letter.
    Ctrl(char),
    /// Alt and a lower-case letter.
    Alt(char),
    /// F1 to F12.
    F(u8),
}

impl MenuKey {
    pub fn parse(spec: &str) -> Result<MenuKey, String> {
        let lower = spec.trim().to_ascii_lowercase();
        let bad = || {
            format!("change \"{spec}\" to ctrl+letter, alt+letter or f1 to f12; Lillignode cannot use it as a key")
        };
        if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            return if (1..=12).contains(&n) { Ok(MenuKey::F(n)) } else { Err(bad()) };
        }
        let (ctrl, letter) = match lower.split_once('+') {
            Some(("ctrl", l)) => (true, l),
            Some(("alt", l)) => (false, l),
            _ => return Err(bad()),
        };
        let mut chars = letter.chars();
        let (Some(c @ 'a'..='z'), None) = (chars.next(), chars.next()) else { return Err(bad()) };
        // These reach a terminal program as Enter, Tab, Backspace, or are the
        // keys every agent relies on to interrupt and to end input.
        if ctrl && "cdhijm".contains(c) {
            return Err(format!("pick another menu key than \"{spec}\"; agents need it"));
        }
        Ok(if ctrl { MenuKey::Ctrl(c) } else { MenuKey::Alt(c) })
    }

    /// How the key is written on screen: "Ctrl+Q", "F12".
    pub fn label(&self) -> String {
        match self {
            MenuKey::F(n) => format!("F{n}"),
            MenuKey::Alt(c) => format!("Alt+{}", c.to_ascii_uppercase()),
            MenuKey::Ctrl(c) => format!("Ctrl+{}", c.to_ascii_uppercase()),
        }
    }
}

/// How many colours the terminal can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// Any colour.
    Full,
    /// The 256 numbered colours.
    Indexed,
    /// The 16 named colours, which the terminal's own theme decides.
    Basic,
    /// No colour: bold and reversed only.
    None,
}

/// What the user asked for in the settings file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colors {
    /// Work it out from the terminal.
    #[default]
    Auto,
    Set(Depth),
}

impl Colors {
    pub fn parse(text: &str) -> Result<Colors, String> {
        match text {
            "auto" => Ok(Colors::Auto),
            "full" => Ok(Colors::Set(Depth::Full)),
            "256" => Ok(Colors::Set(Depth::Indexed)),
            "16" => Ok(Colors::Set(Depth::Basic)),
            "none" => Ok(Colors::Set(Depth::None)),
            other => Err(format!(
                "make \"colors\" \"auto\", \"full\", \"256\", \"16\" or \"none\"; it is \"{other}\""
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Never empty.
    pub menu_keys: Vec<MenuKey>,
    /// Ring the terminal bell once each time an agent starts needing you.
    pub bell: bool,
    /// Show what each conversation has used. Off unless the user asks.
    pub cost: bool,
    /// The count of tokens at which a conversation is marked. A warning only.
    pub cost_warning_tokens: Option<u64>,
    /// How the user pays for Claude Code, if they have said.
    pub billing: Billing,
    /// How many colours to draw with, if the user has said.
    pub colors: Colors,
    /// Serve the read-only page on this machine. Off unless the user asks.
    pub page: bool,
    /// The port the page is served at.
    pub page_port: u16,
    /// Let the page type to agents. Off unless the user asks, apart from
    /// `page`: a page that is on is read only until this is too.
    pub page_can_type: bool,
    /// Let the page act on the Board's cards. Off unless the user asks,
    /// apart from `page` and from `page_can_type`.
    pub page_can_act: bool,
    /// Let the page start a saved team. Off unless the user asks, apart
    /// from `page`: starting agents is more than typing to them.
    pub page_can_start: bool,
    /// The page may offer a notification, with a sound, when an agent comes
    /// to need you. Off unless `"page_notify": true`.
    pub page_notify: bool,
    /// What a settings file with a mistake in it names for another
    /// program: withheld from agents all the same.
    pub withheld_anyway: Vec<String>,
    /// One outside program to start and talk to over its own pipe for the
    /// Board, in place of the Board's files. Off unless named.
    pub board_adapter: Option<BoardAdapter>,
    /// One outside program Lillignode may ask what an agent that says
    /// nothing seems to be doing. Its answers are shown as guesses. Off
    /// unless named.
    pub helper: Option<BoardAdapter>,
    /// How the first screen is laid out: "wide" (down the left) unless the
    /// settings name "beside" or "stacked".
    pub welcome_look: WelcomeLook,
    /// How the page looks: "cards" unless the settings name "soft", "bold"
    /// or "plain" (the page as it was in 0.2).
    pub page_look: PageLook,
}

/// The page's looks: the same page, styled four ways. "Cards" unless the
/// settings name another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageLook {
    Plain,
    Soft,
    #[default]
    Cards,
    Bold,
}

/// The three ways the first screen can be laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WelcomeLook {
    /// The gnome on the left, the large name beside it, the key under the name.
    Beside,
    /// The gnome, the large name and the key, one under another, in the middle.
    Stacked,
    /// Down the left: the gnome, the large name twice as wide where there is
    /// room, then the key. The first screen unless the settings name another.
    #[default]
    Wide,
}

impl Config {
    /// The environment variables named for the Board's program and the
    /// helper: given to those programs only, and taken out of every agent's
    /// environment.
    pub fn withheld_from_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let named = [&self.board_adapter, &self.helper].into_iter().flatten().flat_map(|p| p.env.iter());
        for name in named.chain(self.withheld_anyway.iter()) {
            let same = |n: &String| if cfg!(windows) { n.eq_ignore_ascii_case(name) } else { n == name };
            if !names.iter().any(same) {
                names.push(name.clone());
            }
        }
        names
    }
}

/// The program that keeps the Board: what to run, and its arguments.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardAdapter {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The names of the environment variables it is given, besides what
    /// any program needs to run (its path, its temporary folder, the
    /// user's folders). Nothing else of Lillignode's environment reaches it.
    #[serde(default)]
    pub env: Vec<String>,
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
            cost: false,
            cost_warning_tokens: None,
            billing: Billing::Unsaid,
            colors: Colors::Auto,
            page: false,
            page_port: DEFAULT_PAGE_PORT,
            page_can_type: false,
            page_can_act: false,
            page_can_start: false,
            page_notify: false,
            withheld_anyway: Vec::new(),
            board_adapter: None,
            helper: None,
            welcome_look: WelcomeLook::Wide,
            page_look: PageLook::Cards,
        }
    }
}

impl Config {
    /// "Ctrl+Q or F12"
    pub fn menu_keys_label(&self) -> String {
        self.menu_keys.iter().map(MenuKey::label).collect::<Vec<_>>().join(" or ")
    }

    /// Name function keys first, keeping the user's order otherwise. For
    /// terminals that take Ctrl and Alt keys for themselves (VS Code takes
    /// Ctrl+Q), so the first key named on screen is one that works there.
    /// Every key still opens the menu.
    pub fn function_keys_first(&mut self) {
        self.menu_keys.sort_by_key(|k| !matches!(k, MenuKey::F(_)));
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
    cost: Option<bool>,
    cost_warning_tokens: Option<u64>,
    billing: Option<String>,
    colors: Option<String>,
    page: Option<bool>,
    page_port: Option<u32>,
    page_can_type: Option<bool>,
    page_can_act: Option<bool>,
    page_can_start: Option<bool>,
    page_notify: Option<bool>,
    board_adapter: Option<BoardAdapter>,
    helper: Option<BoardAdapter>,
    welcome_look: Option<String>,
    page_look: Option<String>,
}

fn parse(text: &str) -> Result<Config, String> {
    let file: File = serde_json::from_str(text).map_err(|e| json_fix(&e))?;
    let mut config = Config::default();
    if let Some(bell) = file.bell {
        config.bell = bell;
    }
    config.cost = file.cost.unwrap_or(false);
    config.cost_warning_tokens = match file.cost_warning_tokens {
        None => None,
        Some(0) => {
            return Err(
                "give \"cost_warning_tokens\" a count above 0, or leave it out for no warning".to_string()
            );
        }
        // Said, so the user does not wait for a warning that cannot come.
        Some(_) if !config.cost => {
            return Err("add \"cost\": true, or take out \"cost_warning_tokens\", which needs it".to_string());
        }
        Some(count) => Some(count),
    };
    config.billing = match file.billing.as_deref() {
        None => Billing::Unsaid,
        Some("key") => Billing::Key,
        Some("subscription") => Billing::Subscription,
        Some(other) => {
            return Err(format!("make \"billing\" \"key\" or \"subscription\"; it is \"{other}\""));
        }
    };
    if let Some(colors) = &file.colors {
        config.colors = Colors::parse(colors)?;
    }
    config.page = file.page.unwrap_or(false);
    config.page_port = match file.page_port {
        None => DEFAULT_PAGE_PORT,
        // Said, so the user does not look for a page that is not there.
        Some(_) if !config.page => {
            return Err("add \"page\": true, or take out \"page_port\", which needs it".to_string());
        }
        Some(port @ 1024..=65535) => port as u16,
        Some(port) => {
            return Err(format!("make \"page_port\" a number from 1024 to 65535; it is {port}"));
        }
    };
    config.page_can_type = match file.page_can_type {
        // Said, so the user does not look for a box that is not there.
        Some(true) if !config.page => {
            return Err("add \"page\": true, or take out \"page_can_type\", which needs it".to_string());
        }
        can => can.unwrap_or(false),
    };
    config.page_can_act = match file.page_can_act {
        // Said, so the user does not look for buttons that are not there.
        Some(true) if !config.page => {
            return Err("add \"page\": true, or take out \"page_can_act\", which needs it".to_string());
        }
        can => can.unwrap_or(false),
    };
    config.page_notify = match file.page_notify {
        Some(true) if !config.page => {
            return Err("add \"page\": true, or take out \"page_notify\", which needs it".to_string());
        }
        on => on.unwrap_or(false),
    };
    config.page_can_start = match file.page_can_start {
        // Said, so the user does not look for buttons that are not there.
        Some(true) if !config.page => {
            return Err("add \"page\": true, or take out \"page_can_start\", which needs it".to_string());
        }
        can => can.unwrap_or(false),
    };
    config.board_adapter = match file.board_adapter {
        // The Board is only on the page.
        Some(_) if !config.page => {
            return Err("add \"page\": true, or take out \"board_adapter\", which needs it".to_string());
        }
        Some(adapter) if adapter.command.trim().is_empty() => {
            return Err("give \"board_adapter\" a \"command\" to run, or take it out".to_string());
        }
        adapter => adapter,
    };
    config.helper = match file.helper {
        Some(helper) if helper.command.trim().is_empty() => {
            return Err("give \"helper\" a \"command\" to run, or take it out".to_string());
        }
        helper => helper,
    };
    // In any case: "Stacked" is meant as "stacked".
    config.welcome_look = match file.welcome_look.as_deref().map(str::to_lowercase).as_deref() {
        Some("beside") => WelcomeLook::Beside,
        Some("stacked") => WelcomeLook::Stacked,
        None | Some("wide") => WelcomeLook::Wide,
        Some(_) => {
            return Err("give \"welcome_look\" one of \"beside\", \"stacked\" or \"wide\", or take it out"
                .to_string())
        }
    };
    config.page_look = match file.page_look.as_deref() {
        Some("plain") => PageLook::Plain,
        Some("soft") => PageLook::Soft,
        None | Some("cards") => PageLook::Cards,
        Some("bold") => PageLook::Bold,
        Some(_) => {
            return Err(
                "give \"page_look\" one of \"plain\", \"soft\", \"cards\" or \"bold\", or take it out"
                    .to_string(),
            )
        }
    };
    let Some(specs) = file.menu_keys else { return Ok(config) };
    if specs.is_empty() {
        return Err(
            "name at least one key in \"menu_keys\"; with none there is no way into the menu".to_string()
        );
    }
    config.menu_keys = specs.iter().map(|s| MenuKey::parse(s)).collect::<Result<Vec<_>, _>>()?;
    Ok(config)
}

/// What to do about a settings file that is not valid JSON or does not
/// match the settings, said first, then what was found and where.
fn json_fix(e: &serde_json::Error) -> String {
    let full = e.to_string();
    // serde_json ends its message with " at line L column C".
    let what = full.rsplit_once(" at line ").map_or(full.as_str(), |(what, _)| what);
    let at = format!("line {}, column {}", e.line(), e.column());
    if let Some(rest) = what.strip_prefix("unknown field `") {
        let name = rest.split('`').next().unwrap_or(rest);
        return format!("take out or correct \"{name}\" at {at}; Lillignode has no such setting");
    }
    format!("correct {at}: {what}")
}

/// Returns the settings to use and, if the file was there but unusable, what
/// to tell the user. The file stays strict: one mistake and none of it is
/// used. The message leads with how to fix it, so a message cut short for
/// lack of room keeps the fix (`App::notice_parts`).
pub fn load(dir: &Path) -> (Config, Option<String>) {
    let path = dir.join(FILE);
    let mut config = Config::default();
    let fix = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (config, None),
        Err(e) => format!(
            "make it readable ({e}). Until then no environment variable is kept from agents, as none can be \
             read from it"
        ),
        Ok(text) => match parse(&text) {
            Ok(config) => return (config, None),
            Err(fix) => {
                // Even a file with a mistake in it keeps what it names for
                // another program from the agents: withholding fails closed.
                config.withheld_anyway = named_env(&text).unwrap_or_default();
                fix
            }
        },
    };
    let problem = format!(
        "To fix your settings file: {fix}. Until then none of it is used: every setting is at its default \
         ({} opens the menu), and the file was not changed. It is {}.",
        config.menu_keys_label(),
        path.display()
    );
    (config, Some(problem))
}

/// The variables a settings file names for the Board's program or the
/// helper, read from the file as it is, whether or not the rest of it is
/// right. None if it is not JSON at all.
fn named_env(text: &str) -> Option<Vec<String>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Some(env_lists_in(text));
    };
    let mut names = Vec::new();
    for program in ["board_adapter", "helper"] {
        for name in value[program]["env"].as_array().into_iter().flatten().filter_map(|n| n.as_str()) {
            names.push(name.to_string());
        }
    }
    Some(names)
}

/// In a file that is not JSON at all: every quoted name in a list that
/// follows `"env"`, so a stray comma does not hand agents a key.
fn env_lists_in(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (at, _) in text.match_indices("\"env\"") {
        let rest = &text[at + 5..];
        let Some(open) = rest.find('[') else { continue };
        let Some(close) = rest[open..].find(']') else { continue };
        for piece in rest[open + 1..open + close].split(',') {
            let name = piece.trim().trim_matches('"').trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                names.push(name.to_string());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_two_different_kinds_of_key() {
        let c = Config::default();
        assert_eq!(c.menu_keys_label(), "Ctrl+Q or F12");
        assert_eq!(c.menu_keys, [MenuKey::Ctrl('q'), MenuKey::F(12)]);
    }

    #[test]
    fn reads_the_users_keys() {
        let c = parse(r#"{"menu_keys": ["Alt+M", "f9"]}"#).unwrap();
        assert_eq!(c.menu_keys_label(), "Alt+M or F9");
        assert_eq!(c.menu_keys, [MenuKey::Alt('m'), MenuKey::F(9)]);
    }

    #[test]
    fn in_vs_code_the_function_key_is_named_first_and_both_still_work() {
        assert!(terminal_takes_ctrl_keys(Some("vscode")));
        assert!(!terminal_takes_ctrl_keys(Some("WezTerm")));
        assert!(!terminal_takes_ctrl_keys(None));
        let mut c = Config::default();
        c.function_keys_first();
        assert_eq!(c.menu_keys_label(), "F12 or Ctrl+Q");
        assert!(c.menu_keys.contains(&MenuKey::Ctrl('q')));
        // The user's own order is kept among keys of one kind.
        let mut c = parse(r#"{"menu_keys": ["alt+m", "f9", "ctrl+t", "f2"]}"#).unwrap();
        c.function_keys_first();
        assert_eq!(c.menu_keys_label(), "F9 or F2 or Alt+M or Ctrl+T");
    }

    #[test]
    fn the_page_look_is_cards_unless_said() {
        assert_eq!(Config::default().page_look, PageLook::Cards);
        assert_eq!(parse("{}").unwrap().page_look, PageLook::Cards);
        assert_eq!(parse(r#"{"page_look": "plain"}"#).unwrap().page_look, PageLook::Plain);
        assert_eq!(parse(r#"{"page_look": "soft"}"#).unwrap().page_look, PageLook::Soft);
        assert!(parse(r#"{"page_look": "neon"}"#).unwrap_err().contains("\"page_look\""));
    }

    #[test]
    fn the_welcome_look_is_one_of_three_and_wide_unless_said() {
        assert_eq!(Config::default().welcome_look, WelcomeLook::Wide);
        assert_eq!(parse("{}").unwrap().welcome_look, WelcomeLook::Wide);
        assert_eq!(parse(r#"{"welcome_look": "stacked"}"#).unwrap().welcome_look, WelcomeLook::Stacked);
        assert_eq!(parse(r#"{"welcome_look": "wide"}"#).unwrap().welcome_look, WelcomeLook::Wide);
        assert_eq!(parse(r#"{"welcome_look": "beside"}"#).unwrap().welcome_look, WelcomeLook::Beside);
        assert!(parse(r#"{"welcome_look": "big"}"#).unwrap_err().contains("\"welcome_look\""));
        assert_eq!(parse(r#"{"welcome_look": "Stacked"}"#).unwrap().welcome_look, WelcomeLook::Stacked);
    }

    #[test]
    fn the_page_notifies_only_when_asked_and_only_with_the_page() {
        assert!(!Config::default().page_notify);
        assert!(parse(r#"{"page": true, "page_notify": true}"#).unwrap().page_notify);
        assert!(parse(r#"{"page_notify": true}"#).unwrap_err().contains("\"page_notify\""));
    }

    #[test]
    fn a_settings_file_that_is_not_json_still_withholds_what_it_lists() {
        let text = r#"{ "board_adapter": { "command": "a", "env": ["SERVICE_URL", "SERVICE_KEY"], }, }"#;
        assert!(serde_json::from_str::<serde_json::Value>(text).is_err());
        assert_eq!(named_env(text).unwrap(), ["SERVICE_URL", "SERVICE_KEY"]);
    }

    #[test]
    fn a_settings_file_with_a_mistake_still_withholds_what_it_names() {
        let dir = std::env::temp_dir().join(format!("lillignode-withheld-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // page_can_act without page: a mistake, so nothing else is used.
        std::fs::write(
            dir.join(FILE),
            r#"{"page_can_act": true, "board_adapter": {"command": "a", "env": ["SERVICE_URL", "SERVICE_KEY"]}}"#,
        )
        .unwrap();
        let (config, problem) = load(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert!(problem.is_some() && config.board_adapter.is_none());
        assert_eq!(config.withheld_from_agents(), ["SERVICE_URL", "SERVICE_KEY"]);
    }

    #[test]
    fn what_the_boards_program_and_the_helper_are_given_is_withheld_from_agents() {
        assert!(Config::default().withheld_from_agents().is_empty());
        let c = parse(
            r#"{"page": true, "page_can_act": true,
                "board_adapter": {"command": "a", "env": ["SERVICE_URL", "SERVICE_KEY"]},
                "helper": {"command": "h", "env": ["service_key", "GUESS_KEY"]}}"#,
        )
        .unwrap();
        assert_eq!(c.withheld_from_agents(), ["SERVICE_URL", "SERVICE_KEY", "GUESS_KEY"]);
    }

    #[test]
    fn the_helper_is_off_unless_named_and_needs_a_program() {
        assert_eq!(Config::default().helper, None);
        assert_eq!(parse(r#"{"page": true}"#).unwrap().helper, None);
        let named = parse(r#"{"helper": {"command": "guess", "args": ["--fast"], "env": ["KEY"]}}"#).unwrap();
        assert_eq!(
            named.helper,
            Some(BoardAdapter {
                command: "guess".into(),
                args: vec!["--fast".into()],
                env: vec!["KEY".into()]
            })
        );
        assert!(!named.page, "it needs no page");
        assert!(parse(r#"{"helper": {"command": " "}}"#).unwrap_err().contains("\"helper\""));
        assert!(parse(r#"{"helper": {"command": "x", "screen": true}}"#).is_err());
    }

    #[test]
    fn the_page_is_off_unless_asked_for_and_its_port_must_be_usable() {
        assert!(!Config::default().page);
        assert!(!parse(r#"{"bell": false}"#).unwrap().page);
        let adapter =
            parse(r#"{"page": true, "board_adapter": {"command": "python", "args": ["hub.py"]}}"#).unwrap();
        assert_eq!(
            adapter.board_adapter,
            Some(BoardAdapter { command: "python".into(), args: vec!["hub.py".into()], env: vec![] })
        );
        assert_eq!(parse(r#"{"page": true}"#).unwrap().board_adapter, None);
        assert!(parse(r#"{"board_adapter": {"command": "python"}}"#).unwrap_err().contains("\"page\": true"));
        assert!(parse(r#"{"page": true, "board_adapter": {"command": " "}}"#)
            .unwrap_err()
            .contains("command"));
        assert!(parse(r#"{"page": true, "board_adapter": {"command": "x", "key": "y"}}"#).is_err());
        let on = parse(r#"{"page": true}"#).unwrap();
        assert_eq!((on.page, on.page_port), (true, DEFAULT_PAGE_PORT));
        assert_eq!(parse(r#"{"page": true, "page_port": 8123}"#).unwrap().page_port, 8123);
        assert!(!parse(r#"{"page": false}"#).unwrap().page);
        // Each mistake is said, with what to do.
        let needs = parse(r#"{"page_port": 8123}"#).unwrap_err();
        assert!(needs.starts_with("add \"page\": true"), "{needs}");
        for port in ["0", "80", "70000"] {
            let bad = parse(&format!(r#"{{"page": true, "page_port": {port}}}"#)).unwrap_err();
            assert!(bad.starts_with("make \"page_port\" a number from 1024 to 65535"), "{bad}");
        }
        assert!(parse(r#"{"page": "yes"}"#).is_err());
    }

    #[test]
    fn the_page_cannot_act_on_the_board_unless_asked_apart_from_the_other_two() {
        assert!(!Config::default().page_can_act);
        // On, and able to type, it still cannot act.
        assert!(!parse(r#"{"page": true}"#).unwrap().page_can_act);
        let typing = parse(r#"{"page": true, "page_can_type": true}"#).unwrap();
        assert!(typing.page_can_type && !typing.page_can_act);
        // Able to act, it still cannot type.
        let acting = parse(r#"{"page": true, "page_can_act": true}"#).unwrap();
        assert!(acting.page_can_act && !acting.page_can_type);
        for text in [r#"{"page_can_act": true}"#, r#"{"page": false, "page_can_act": true}"#] {
            let needs = parse(text).unwrap_err();
            assert!(needs.starts_with("add \"page\": true, or take out \"page_can_act\""), "{needs}");
        }
        assert!(!parse(r#"{"page_can_act": false}"#).unwrap().page_can_act, "off needs nothing");
        assert!(parse(r#"{"page": true, "page_can_act": 1}"#).is_err());
    }

    #[test]
    fn the_page_cannot_type_unless_asked_apart_from_being_on() {
        assert!(!Config::default().page_can_type);
        // On, it is still read only.
        assert!(!parse(r#"{"page": true}"#).unwrap().page_can_type);
        assert!(!parse(r#"{"page": true, "page_can_type": false}"#).unwrap().page_can_type);
        assert!(parse(r#"{"page": true, "page_can_type": true}"#).unwrap().page_can_type);
        // Asked for with no page: said, not passed over.
        for text in [r#"{"page_can_type": true}"#, r#"{"page": false, "page_can_type": true}"#] {
            let needs = parse(text).unwrap_err();
            assert!(needs.starts_with("add \"page\": true, or take out \"page_can_type\""), "{needs}");
        }
        assert!(!parse(r#"{"page_can_type": false}"#).unwrap().page_can_type, "off needs nothing");
        assert!(parse(r#"{"page": true, "page_can_type": "yes"}"#).is_err());
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
    fn cost_is_off_unless_asked_for_and_a_warning_needs_it_on() {
        let c = Config::default();
        assert!(!c.cost);
        assert_eq!(c.cost_warning_tokens, None);
        assert!(!parse(r#"{"bell": false}"#).unwrap().cost);
        let c = parse(r#"{"cost": true}"#).unwrap();
        assert!(c.cost);
        assert_eq!(c.cost_warning_tokens, None);
        let c = parse(r#"{"cost": true, "cost_warning_tokens": 500000}"#).unwrap();
        assert_eq!(c.cost_warning_tokens, Some(500_000));

        let problem = parse(r#"{"cost_warning_tokens": 500000}"#).unwrap_err();
        assert!(problem.starts_with("add \"cost\": true"), "{problem}");
        assert!(parse(r#"{"cost": false, "cost_warning_tokens": 5}"#).is_err());
        let problem = parse(r#"{"cost": true, "cost_warning_tokens": 0}"#).unwrap_err();
        assert!(problem.contains("leave it out for no warning"), "{problem}");
        assert!(parse(r#"{"cost": true, "cost_warning_tokens": -5}"#).is_err());
        assert!(parse(r#"{"cost": true, "cost_warning_tokens": "500k"}"#).is_err());
        assert!(parse(r#"{"cost": "yes"}"#).is_err());
    }

    #[test]
    fn billing_is_unsaid_unless_the_user_says_and_a_misspelling_is_reported() {
        assert_eq!(Config::default().billing, Billing::Unsaid);
        assert_eq!(parse(r#"{"billing": "key"}"#).unwrap().billing, Billing::Key);
        assert_eq!(parse(r#"{"billing": "subscription"}"#).unwrap().billing, Billing::Subscription);
        let problem = parse(r#"{"billing": "api"}"#).unwrap_err();
        assert!(problem.starts_with("make \"billing\" \"key\" or \"subscription\""), "{problem}");
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

    /// An empty folder of this test's own. A process id is reused by the
    /// operating system, so a folder left by an earlier run that stopped
    /// part way can carry the same name: it is emptied first, never trusted.
    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-test-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_left_by_an_earlier_run_is_emptied_before_use() {
        let stale = fresh_dir("stale");
        std::fs::write(stale.join(FILE), r#"{"menu_keys": ["ctrl+c"]}"#).unwrap();
        let dir = fresh_dir("stale");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(load(&dir), (Config::default(), None));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unusable_file_falls_back_to_defaults_and_says_so() {
        let dir = fresh_dir("unusable");
        assert_eq!(load(&dir), (Config::default(), None));
        std::fs::write(dir.join(FILE), r#"{"menu_keys": ["ctrl+c"]}"#).unwrap();
        let (config, problem) = load(&dir);
        assert_eq!(config, Config::default());
        let problem = problem.unwrap();
        // How to fix it comes first.
        assert!(
            problem.starts_with(
                "To fix your settings file: pick another menu key than \"ctrl+c\"; agents need it."
            ),
            "{problem}"
        );
        assert!(problem.contains("Ctrl+Q or F12 opens the menu"), "{problem}");
        assert!(problem.contains("was not changed"), "{problem}");

        // Not JSON, or a setting that does not exist: the place is named.
        std::fs::write(dir.join(FILE), "{\n  \"colours\": \"none\"\n}").unwrap();
        let problem = load(&dir).1.unwrap();
        assert!(
            problem.starts_with(
                "To fix your settings file: take out or correct \"colours\" at line 2, column 11; Lillignode \
                 has no such setting."
            ),
            "{problem}"
        );
        std::fs::write(dir.join(FILE), "{\"bell\": off}").unwrap();
        let problem = load(&dir).1.unwrap();
        assert!(problem.starts_with("To fix your settings file: correct line 1, column 10: "), "{problem}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
