//! How Lillignode looks: its colours, the gnome mark beside each agent, and
//! the gnome on the welcome screen.
//!
//! Colour is never the only signal. Every status has its own sign and its
//! own word, so the sidebar reads the same with no colour at all.
//!
//! Every character drawn here is plain ASCII, so it exists in every
//! terminal font. The two characters Lillignode draws that are not ASCII are
//! listed in `NOT_ASCII`, and `scripts/check-fonts.ps1` checks them against
//! the fonts Windows terminals use.

use ratatui::style::{Color, Modifier, Style};

use crate::status::AgentStatus;
use crate::workspace::Status;

/// The only characters Lillignode itself draws that are not ASCII: the rule
/// beside the sidebar (a light box-drawing line) and the mark where text
/// was cut. A test holds every screen Lillignode draws to this list.
pub const NOT_ASCII: [char; 2] = ['│', '…'];

/// The hat every gnome mark wears.
pub const HAT: &str = "/\\";

/// The gnome on the welcome screen, after the icon: a tall hat whose tip
/// curls over to the right, a brim, a round nose, and a soft beard drawn
/// with rounded characters.
pub const GNOME: [&str; 7] = [
    r"     .--._",
    r"    /    `)",
    r"   /      \",
    r"  (________)",
    r"   (  ()  )",
    r"    (    )",
    r"     `--'",
];

/// How many of the gnome's rows are hat.
pub const GNOME_HAT_ROWS: usize = 4;

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

/// How many colours to use, from what the terminal says about itself.
/// `var` reads an environment variable. A terminal that says nothing gets
/// the 16 named colours, which every colour terminal has.
pub fn depth(asked: Colors, var: impl Fn(&str) -> Option<String>, windows: bool) -> Depth {
    if let Colors::Set(depth) = asked {
        return depth;
    }
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    // The convention at no-color.org: any value means no colour.
    if set("NO_COLOR").is_some() {
        return Depth::None;
    }
    let term = set("TERM").unwrap_or_default();
    if term == "dumb" {
        return Depth::None;
    }
    let says_full = set("COLORTERM").is_some_and(|v| v == "truecolor" || v == "24bit");
    // Windows Terminal and VS Code's terminal show any colour. So does the
    // console window of Windows 10 and later, which is all that is left
    // when neither is named.
    if says_full || set("WT_SESSION").is_some() || set("TERM_PROGRAM").as_deref() == Some("vscode") || windows
    {
        return Depth::Full;
    }
    if term.contains("256color") {
        return Depth::Indexed;
    }
    Depth::Basic
}

/// What a piece of the sidebar is saying, which decides its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Ordinary text.
    Plain,
    /// Working: the brightest ordinary text.
    Strong,
    /// Needs you. The one meaning of yellow.
    NeedsYou,
    /// Done.
    Good,
    /// Idle, and anything secondary.
    Dim,
    /// Errored.
    Bad,
}

/// The colours, for one depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette(pub Depth);

impl Palette {
    fn fg(self, full: (u8, u8, u8), indexed: u8, basic: Option<Color>) -> Style {
        let color = match self.0 {
            Depth::Full => Some(Color::Rgb(full.0, full.1, full.2)),
            Depth::Indexed => Some(Color::Indexed(indexed)),
            Depth::Basic => basic,
            Depth::None => None,
        };
        color.map_or_else(Style::default, |c| Style::default().fg(c))
    }

    /// Copper, #C17B3E: the wordmark, the hats and the agent in front.
    /// With 16 colours or none there is no copper, and nothing near it that
    /// is not yellow, so it is bold alone.
    pub fn copper(self) -> Style {
        self.fg((0xC1, 0x7B, 0x3E), 173, None).add_modifier(Modifier::BOLD)
    }

    /// The sidebar and the bar: a panel a little apart from the agent's
    /// screen. It sets its own text colour as well as its background, so
    /// it reads the same whatever the terminal's theme. With 16 colours or
    /// none it is left as the terminal draws it.
    pub fn panel(self) -> Style {
        match self.0 {
            Depth::Full => Style::default().bg(Color::Rgb(0x20, 0x24, 0x2B)).fg(Color::Rgb(0xD6, 0xDB, 0xE4)),
            Depth::Indexed => Style::default().bg(Color::Indexed(235)).fg(Color::Indexed(252)),
            Depth::Basic | Depth::None => Style::default(),
        }
    }

    pub fn tone(self, tone: Tone) -> Style {
        match tone {
            Tone::Plain => Style::default(),
            Tone::Strong => Style::default().add_modifier(Modifier::BOLD),
            Tone::NeedsYou => self.needs_you_text(),
            Tone::Good => self.fg((0x8F, 0xBF, 0x6A), 107, Some(Color::Green)),
            Tone::Dim => self.dim(),
            Tone::Bad => self.fg((0xE0, 0x56, 0x3B), 167, Some(Color::Red)),
        }
    }

    /// Dim grey-blue: Idle, folders, labels in the bar.
    pub fn dim(self) -> Style {
        self.fg((0x7F, 0x8F, 0xA6), 103, Some(Color::DarkGray))
    }

    /// A key named in the bar.
    pub fn key(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    /// The word that says which mode the bar is in: MENU, HISTORY.
    pub fn mode(self) -> Style {
        match self.0 {
            Depth::Full => Style::default().fg(Color::Black).bg(Color::Rgb(0xC1, 0x7B, 0x3E)),
            Depth::Indexed => Style::default().fg(Color::Black).bg(Color::Indexed(173)),
            Depth::Basic | Depth::None => Style::default().add_modifier(Modifier::REVERSED),
        }
        .add_modifier(Modifier::BOLD)
    }

    /// Lillignode is asking something and waiting for the answer. Reversed,
    /// so it stands out with any number of colours. Not yellow: that is
    /// kept for an agent that needs you.
    pub fn question(self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }

    /// The solid block on the row of an agent that needs you.
    pub fn needs_you_block(self) -> Style {
        match self.0 {
            Depth::None => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            _ => Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        }
    }

    /// Yellow words: the count in the heading, the sentence in the notice.
    pub fn needs_you_text(self) -> Style {
        match self.0 {
            Depth::None => Style::default().add_modifier(Modifier::BOLD),
            _ => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        }
    }
}

/// The sign under an agent's hat and what it is saying. `clock` is whole
/// seconds: a Working agent's sign alternates between two frames, one a
/// second. One that has gone quiet holds still.
///
/// Working is `+`, Needs you `!`, Done `=`, Idle `z`, Errored `x`, Unknown
/// `?`, Starting `.`, and an agent that has exited `-`.
pub fn sign(reported: Option<&AgentStatus>, program: &Status, clock: u64) -> (char, Tone) {
    match reported {
        Some(AgentStatus::Working) => (if clock.is_multiple_of(2) { '+' } else { '*' }, Tone::Strong),
        Some(AgentStatus::WorkingQuiet(_)) => ('+', Tone::Strong),
        Some(AgentStatus::NeedsYou(_)) => ('!', Tone::NeedsYou),
        Some(AgentStatus::Done) => ('=', Tone::Good),
        Some(AgentStatus::Idle) => ('z', Tone::Dim),
        Some(AgentStatus::Error(_)) => ('x', Tone::Bad),
        Some(AgentStatus::Unknown(_)) => ('?', Tone::Plain),
        Some(AgentStatus::Starting) => ('.', Tone::Dim),
        None => match program {
            // Running with nothing reported cannot happen; if it did, it
            // would be an Unknown, not a guess.
            Status::Running => ('?', Tone::Plain),
            Status::Exited(exit) if exit.clean() => ('-', Tone::Dim),
            Status::Exited(_) | Status::NotStarted(_) => ('x', Tone::Bad),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Exit;
    use crate::status::{Kind, Why};
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn colours_fall_back_to_what_the_terminal_says_it_can_show() {
        let auto = Colors::Auto;
        assert_eq!(depth(auto, env(&[("COLORTERM", "truecolor")]), false), Depth::Full);
        assert_eq!(depth(auto, env(&[("WT_SESSION", "abc")]), true), Depth::Full);
        assert_eq!(depth(auto, env(&[("TERM_PROGRAM", "vscode")]), false), Depth::Full);
        assert_eq!(depth(auto, env(&[("TERM", "xterm-256color")]), false), Depth::Indexed);
        assert_eq!(depth(auto, env(&[("TERM", "xterm")]), false), Depth::Basic);
        assert_eq!(depth(auto, env(&[]), false), Depth::Basic, "a terminal that says nothing");
        assert_eq!(depth(auto, env(&[("TERM", "dumb"), ("COLORTERM", "truecolor")]), false), Depth::None);
        // NO_COLOR wins over everything the terminal says, and an empty one is not set.
        assert_eq!(depth(auto, env(&[("NO_COLOR", "1"), ("WT_SESSION", "abc")]), true), Depth::None);
        assert_eq!(depth(auto, env(&[("NO_COLOR", ""), ("COLORTERM", "24bit")]), false), Depth::Full);
        // The user's own setting wins over all of it.
        assert_eq!(depth(Colors::Set(Depth::Basic), env(&[("NO_COLOR", "1")]), true), Depth::Basic);
    }

    #[test]
    fn the_colors_setting_is_read_exactly_or_reported() {
        assert_eq!(Colors::parse("auto"), Ok(Colors::Auto));
        assert_eq!(Colors::parse("256"), Ok(Colors::Set(Depth::Indexed)));
        assert_eq!(Colors::parse("none"), Ok(Colors::Set(Depth::None)));
        assert!(Colors::parse("lots")
            .unwrap_err()
            .contains("\"auto\", \"full\", \"256\", \"16\" or \"none\""));
    }

    #[test]
    fn copper_is_exact_with_full_colour_and_never_yellow_with_fewer() {
        assert_eq!(Palette(Depth::Full).copper().fg, Some(Color::Rgb(0xC1, 0x7B, 0x3E)));
        assert_eq!(Palette(Depth::Indexed).copper().fg, Some(Color::Indexed(173)));
        for depth in [Depth::Basic, Depth::None] {
            let copper = Palette(depth).copper();
            assert_eq!(copper.fg, None, "{depth:?}: no named colour stands in for copper");
            assert!(copper.add_modifier.contains(Modifier::BOLD));
        }
        // With no colour at all, nothing sets a colour.
        let plain = Palette(Depth::None);
        let every = [
            plain.copper(),
            plain.panel(),
            plain.dim(),
            plain.key(),
            plain.mode(),
            plain.question(),
            plain.needs_you_block(),
            plain.needs_you_text(),
            plain.tone(Tone::Good),
            plain.tone(Tone::Bad),
            plain.tone(Tone::Strong),
        ];
        for style in every {
            assert_eq!((style.fg, style.bg), (None, None), "{style:?}");
        }
    }

    #[test]
    fn yellow_means_needs_you_and_nothing_else() {
        for depth in [Depth::Full, Depth::Indexed, Depth::Basic] {
            let p = Palette(depth);
            assert_eq!(p.needs_you_block().bg, Some(Color::Yellow));
            assert_eq!(p.needs_you_text().fg, Some(Color::Yellow));
            let others = [
                p.copper(),
                p.panel(),
                p.dim(),
                p.key(),
                p.mode(),
                p.question(),
                p.tone(Tone::Plain),
                p.tone(Tone::Strong),
                p.tone(Tone::Good),
                p.tone(Tone::Dim),
                p.tone(Tone::Bad),
            ];
            for style in others {
                let yellow = [Some(Color::Yellow), Some(Color::LightYellow)];
                assert!(!yellow.contains(&style.fg) && !yellow.contains(&style.bg), "{depth:?} {style:?}");
            }
        }
    }

    #[test]
    fn every_status_has_its_own_sign_so_colour_is_never_the_only_difference() {
        let running = Status::Running;
        let of = |status: AgentStatus| sign(Some(&status), &running, 0).0;
        let signs = [
            of(AgentStatus::Working),
            of(AgentStatus::NeedsYou(Some(Kind::Permission))),
            of(AgentStatus::Done),
            of(AgentStatus::Idle),
            of(AgentStatus::Error(None)),
            of(AgentStatus::Unknown(Why::NoReport)),
            of(AgentStatus::Starting),
            sign(None, &Status::Exited(Exit { code: 0 }), 0).0,
        ];
        assert_eq!(signs, ['+', '!', '=', 'z', 'x', '?', '.', '-']);
        // An agent that stopped with an error, or never started, is Errored.
        assert_eq!(sign(None, &Status::Exited(Exit { code: 3 }), 0).0, 'x');
        assert_eq!(sign(None, &Status::NotStarted("why".into()), 0).0, 'x');
    }

    #[test]
    fn working_alternates_two_frames_and_holds_still_when_quiet() {
        let running = Status::Running;
        let frames: Vec<char> =
            (0..4).map(|clock| sign(Some(&AgentStatus::Working), &running, clock).0).collect();
        assert_eq!(frames, ['+', '*', '+', '*']);
        for clock in 0..4 {
            assert_eq!(sign(Some(&AgentStatus::WorkingQuiet(2)), &running, clock).0, '+');
            assert_eq!(sign(Some(&AgentStatus::Done), &running, clock).0, '=');
        }
        // Neither frame is another status's sign.
        assert!(!"!=zx?.-".contains('*'));
    }

    #[test]
    fn the_gnome_and_the_hat_are_plain_ascii() {
        assert_eq!(GNOME.len(), 7);
        for row in GNOME {
            assert!(row.is_ascii(), "{row}");
        }
        assert!(HAT.is_ascii());
        // The gnome is no wider than the room the welcome screen gives it.
        assert!(GNOME.iter().all(|row| row.len() <= 12));
    }
}
