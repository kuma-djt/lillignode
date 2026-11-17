//! What kind of agent a program is.
//!
//! Lillignode does more for an agent it knows by name: it may give the agent
//! a conversation to continue, read its status and read its cost. Each of
//! those is a question, and each is answered here, once, by the agent's kind.
//!
//! An agent's kind is worked out from its command when it is first started
//! and is saved with it. From then on the saved word decides, not the name.

use std::path::Path;

use crate::status::{self, NoSource, Report, Reporter, StatusSource, Why};
use crate::terminal::StatusMessages;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code: a conversation to continue, its status and its cost.
    ClaudeCode,
    /// Codex: its status. Its conversation is not continued and its cost
    /// is not read.
    Codex,
    /// Any other program. Lillignode can tell only whether it is running.
    Program,
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

    /// The name shown for an agent of this kind. A plain program is shown
    /// as typed.
    pub fn name(self, command: &str) -> String {
        match self {
            AgentKind::ClaudeCode => "Claude Code".to_string(),
            AgentKind::Codex => "Codex".to_string(),
            AgentKind::Program => command.to_string(),
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

    /// The program whose status reports are read for an agent of this kind.
    /// None for a kind whose status is not read.
    pub fn reporter(self) -> Option<&'static Reporter> {
        match self {
            AgentKind::ClaudeCode => Some(&status::CLAUDE_CODE),
            AgentKind::Codex => Some(&status::CODEX),
            AgentKind::Program => None,
        }
    }

    /// Where the status of an agent of this kind comes from.
    pub fn status_source(self) -> Box<dyn StatusSource> {
        match self.reporter() {
            Some(who) => Box::new(Report::from(who)),
            None => Box::new(NoSource::new(Why::NotClaudeCode)),
        }
    }

    /// What the terminal of an agent of this kind does with status messages.
    pub fn status_messages(self) -> StatusMessages {
        match self {
            AgentKind::ClaudeCode => StatusMessages::Answered,
            AgentKind::Codex => StatusMessages::Kept,
            AgentKind::Program => StatusMessages::Ignored,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_known_by_the_name_of_its_file_and_by_nothing_looser() {
        for command in
            ["claude", "claude.cmd", "CLAUDE.EXE", "C:\\tools\\claude.exe", "/usr/local/bin/claude"]
        {
            assert_eq!(AgentKind::of_command(command), AgentKind::ClaudeCode, "{command}");
        }
        for command in ["codex", "codex.cmd", "Codex.EXE", "C:\\npm\\codex.cmd", "/usr/local/bin/codex"] {
            assert_eq!(AgentKind::of_command(command), AgentKind::Codex, "{command}");
        }
        for command in
            ["aider", "claude-wrapper", "cc.cmd", "myclaude", "", "claude code", "codex-cli", "xcodex"]
        {
            assert_eq!(AgentKind::of_command(command), AgentKind::Program, "{command:?}");
        }
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
    fn codex_has_its_status_read_and_nothing_else() {
        let codex = AgentKind::Codex;
        assert_eq!(codex.name("codex.cmd"), "Codex");
        assert_eq!(codex.reporter(), Some(&status::CODEX));
        assert_eq!(codex.status_messages(), StatusMessages::Kept);
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
