//! What kind of agent a program is.
//!
//! Lillignode does more for an agent it knows by name: it may give the agent
//! a conversation to continue and read its cost, and it knows whether to
//! expect status reports from it. Each of those is a question, and each is
//! answered here, once, by the agent's kind.
//!
//! Status itself does not depend on the kind: any program that reports its
//! status has it shown.
//!
//! An agent's kind is worked out from its command when it is first started
//! and is saved with it. From then on the saved word decides, not the name.

use std::path::Path;

use crate::status::{self, Report, Reporter, StatusSource, Why};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code: a conversation to continue and its cost. It is expected
    /// to report its status.
    ClaudeCode,
    /// Codex: expected to report its status. Its conversation is not
    /// continued and its cost is not read.
    Codex,
    /// Any other program. It has a status if it reports one.
    Program,
}

/// The agent programs Lillignode knows by name, in the order they are
/// offered: each command and what it is called.
pub const KNOWN: &[(&str, &str)] = &[("claude", "Claude Code"), ("codex", "Codex")];

/// Which of the agents Lillignode knows by name are on this machine:
/// those `resolve` finds a program for. Given the same lookup Lillignode
/// starts programs with, "found" means "would start". Nothing is run.
pub fn found(resolve: impl Fn(&str) -> Option<std::path::PathBuf>) -> Vec<(&'static str, &'static str)> {
    KNOWN.iter().copied().filter(|(command, _)| resolve(command).is_some()).collect()
}

impl AgentKind {
    /// The kind of a command nobody has said the kind of, by the name of the
    /// program's file, with any extension, in any case: `claude` is Claude
    /// Code and `codex` is Codex.
    pub fn of_command(command: &str) -> AgentKind {
        let named = |name: &str| {
            Path::new(command).file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name))
        };
        if named("claude") {
            AgentKind::ClaudeCode
        } else if named("codex") {
            AgentKind::Codex
        } else {
            AgentKind::Program
        }
    }

    /// The kind after this one, in the order the new-agent form offers
    /// them, coming round again after the last.
    pub fn next(self) -> AgentKind {
        match self {
            AgentKind::ClaudeCode => AgentKind::Codex,
            AgentKind::Codex => AgentKind::Program,
            AgentKind::Program => AgentKind::ClaudeCode,
        }
    }

    /// The word saved with an agent of this kind.
    pub fn saved_word(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
            AgentKind::Program => "program",
        }
    }

    /// The kind a saved word names. None for a word this Lillignode does
    /// not know, which a newer one may have written.
    pub fn from_saved(word: &str) -> Option<AgentKind> {
        [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Program]
            .into_iter()
            .find(|kind| kind.saved_word() == word)
    }

    /// The kind of a saved agent. Its saved word decides; a word not known
    /// here is treated as a plain program, never as the nearest match.
    /// Without a word, the command decides.
    pub fn of_saved(word: Option<&str>, command: &str) -> AgentKind {
        match word {
            Some(word) => AgentKind::from_saved(word).unwrap_or(AgentKind::Program),
            None => AgentKind::of_command(command),
        }
    }

    /// The name a new agent of this kind is given. A plain program is named
    /// by its file's short name.
    pub fn name(self, command: &str) -> String {
        match self {
            AgentKind::ClaudeCode => "Claude Code".to_string(),
            AgentKind::Codex => "Codex".to_string(),
            AgentKind::Program => short_name(command),
        }
    }

    /// Whether an agent of this kind is given a conversation id when it is
    /// started, to be continued by that id later.
    ///
    /// Codex is not: it cannot be given an id when it starts, and its "most
    /// recent conversation" need not be this agent's.
    pub fn keeps_conversations(self) -> bool {
        match self {
            AgentKind::ClaudeCode => true,
            AgentKind::Codex | AgentKind::Program => false,
        }
    }

    /// What to add to the agent's arguments so it begins the conversation
    /// with this id, or continues it.
    pub fn conversation_args(self, id: &str, continuing: bool) -> Vec<String> {
        match self {
            AgentKind::ClaudeCode => {
                let flag = if continuing { "--resume" } else { "--session-id" };
                vec![flag.to_string(), id.to_string()]
            }
            AgentKind::Codex | AgentKind::Program => Vec::new(),
        }
    }

    /// The program an agent of this kind is expected to be, for a kind
    /// known to report its status. None for a kind nothing is expected of.
    pub fn reporter(self) -> Option<&'static Reporter> {
        match self {
            AgentKind::ClaudeCode => Some(&status::CLAUDE_CODE),
            AgentKind::Codex => Some(&status::CODEX),
            AgentKind::Program => None,
        }
    }

    /// Where the status of an agent of this kind comes from: its own
    /// reports, for every kind. The kind decides only what is shown and
    /// said while it has sent none.
    pub fn status_source(self) -> Box<dyn StatusSource> {
        match self.reporter() {
            Some(who) => Box::new(Report::expected(who)),
            None => Box::new(Report::unexpected(Why::NothingSent)),
        }
    }

    /// Whether Lillignode can read what a conversation of this kind has used.
    pub fn has_cost_record(self) -> bool {
        match self {
            AgentKind::ClaudeCode => true,
            AgentKind::Codex | AgentKind::Program => false,
        }
    }
}

/// A program's short name: its file's name without the folder or the
/// extension, split at either kind of slash on every system, so a Windows
/// path saved on Windows is named the same everywhere. A command with
/// nothing left after that is shown as typed.
fn short_name(command: &str) -> String {
    let file = command.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(command);
    let stem = match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file,
    };
    if stem.trim().is_empty() || stem.chars().all(|c| c == '.') {
        command.to_string()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_agents_found_are_those_the_lookup_finds() {
        let only = |names: &'static [&'static str]| {
            move |c: &str| names.contains(&c).then(|| std::path::PathBuf::from(format!("/bin/{c}")))
        };
        assert_eq!(found(only(&["claude"])), [("claude", "Claude Code")]);
        assert_eq!(found(only(&["codex", "claude"])), [("claude", "Claude Code"), ("codex", "Codex")]);
        assert!(found(only(&["claudette"])).is_empty());
    }

    #[test]
    fn a_plain_program_is_named_by_its_file_without_folder_or_extension() {
        let name = |c| AgentKind::Program.name(c);
        assert_eq!(name(r"C:\temp\lg-opts\two words\tell"), "tell");
        assert_eq!(name(r"C:\tools\tell.cmd"), "tell");
        assert_eq!(name("/usr/local/bin/aider"), "aider");
        assert_eq!(name("powershell"), "powershell");
        assert_eq!(name("powershell.exe"), "powershell");
        assert_eq!(name(".hidden"), ".hidden");
        assert_eq!(name("C:/mixed\\slashes/run.ps1"), "run");
        // Nothing left: shown as typed rather than as nothing.
        assert_eq!(name("/"), "/");
        assert_eq!(name(".."), "..");
        assert_eq!(name("tools/.."), "tools/..");
        assert_eq!(name("tools/run/"), "run");
        assert_eq!(name("run."), "run");
        assert_eq!(name(""), "");
        // Claude Code and Codex keep their own names, from any path.
        assert_eq!(AgentKind::ClaudeCode.name(r"C:\npm\claude.cmd"), "Claude Code");
        assert_eq!(AgentKind::Codex.name("/bin/codex"), "Codex");
    }

    #[test]
    fn a_program_is_known_by_the_name_of_its_file_and_by_nothing_looser() {
        for command in ["claude", "claude.cmd", "CLAUDE.EXE", "/usr/local/bin/claude"] {
            assert_eq!(AgentKind::of_command(command), AgentKind::ClaudeCode, "{command}");
        }
        for command in ["codex", "codex.cmd", "Codex.EXE", "/usr/local/bin/codex"] {
            assert_eq!(AgentKind::of_command(command), AgentKind::Codex, "{command}");
        }
        // A path is split by the rules of the system Lillignode runs on, so
        // a Windows path is a path only there.
        if cfg!(windows) {
            assert_eq!(AgentKind::of_command("C:\\tools\\claude.exe"), AgentKind::ClaudeCode);
            assert_eq!(AgentKind::of_command("C:\\npm\\codex.cmd"), AgentKind::Codex);
        }
        for command in
            ["aider", "claude-wrapper", "cc.cmd", "myclaude", "", "claude code", "codex-cli", "xcodex"]
        {
            assert_eq!(AgentKind::of_command(command), AgentKind::Program, "{command:?}");
        }
    }

    #[test]
    fn the_form_offers_every_kind_in_turn_and_comes_round_again() {
        let mut seen = vec![AgentKind::ClaudeCode];
        while seen.len() < 4 {
            seen.push(seen.last().unwrap().next());
        }
        assert_eq!(
            seen,
            [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Program, AgentKind::ClaudeCode]
        );
    }

    #[test]
    fn every_kind_is_read_back_from_the_word_it_is_saved_as() {
        for kind in [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Program] {
            assert_eq!(AgentKind::from_saved(kind.saved_word()), Some(kind));
        }
        assert_eq!(AgentKind::from_saved("Claude-Code"), None);
        assert_eq!(AgentKind::from_saved("something-newer"), None);
        assert_eq!(AgentKind::from_saved(""), None);
    }

    #[test]
    fn the_saved_word_decides_and_the_command_only_when_there_is_none() {
        assert_eq!(AgentKind::of_saved(None, "claude"), AgentKind::ClaudeCode);
        assert_eq!(AgentKind::of_saved(None, "codex"), AgentKind::Codex);
        assert_eq!(AgentKind::of_saved(None, "aider"), AgentKind::Program);
        assert_eq!(AgentKind::of_saved(Some("program"), "claude"), AgentKind::Program);
        // Saved as a plain program, a program named codex stays one.
        assert_eq!(AgentKind::of_saved(Some("program"), "codex"), AgentKind::Program);
        assert_eq!(AgentKind::of_saved(Some("claude-code"), "cc.cmd"), AgentKind::ClaudeCode);
        // A word from a newer Lillignode is not guessed at.
        assert_eq!(AgentKind::of_saved(Some("something-newer"), "claude"), AgentKind::Program);
    }

    #[test]
    fn only_claude_code_is_told_which_conversation_to_use() {
        assert_eq!(AgentKind::ClaudeCode.conversation_args("abc", false), ["--session-id", "abc"]);
        assert_eq!(AgentKind::ClaudeCode.conversation_args("abc", true), ["--resume", "abc"]);
        assert!(AgentKind::Program.conversation_args("abc", true).is_empty());
    }

    #[test]
    fn codex_is_expected_to_report_and_gets_no_extras() {
        let codex = AgentKind::Codex;
        assert_eq!(codex.name("codex.cmd"), "Codex");
        assert_eq!(codex.reporter(), Some(&status::CODEX));
        // It is never told which conversation to use, and never asked to
        // continue one: its idea of the last one may not be this agent's.
        assert!(!codex.keeps_conversations());
        assert!(codex.conversation_args("abc", true).is_empty());
        assert!(codex.conversation_args("abc", false).is_empty());
        assert!(!codex.has_cost_record());
        assert_eq!(AgentKind::ClaudeCode.reporter(), Some(&status::CLAUDE_CODE));
        assert_eq!(AgentKind::Program.reporter(), None);
    }
}
