//! What kind of agent a program is.
//!
//! Lillignode does more for an agent it knows by name: it gives the agent a
//! conversation to continue, reads its status and reads its cost. Each of
//! those is a question, and each is answered here, once, by the agent's kind.
//!
//! An agent's kind is worked out from its command when it is first started
//! and is saved with it. From then on the saved word decides, not the name.

use std::path::Path;

use crate::status::{ClaudeReport, NoSource, StatusSource, Why};
use crate::terminal::StatusMessages;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code: the one agent Lillignode supports by name.
    ClaudeCode,
    /// Any other program. Lillignode can tell only whether it is running.
    Program,
}

impl AgentKind {
    /// The kind of a command nobody has said the kind of: Claude Code if the
    /// program's file is named `claude`, with any extension, in any case.
    pub fn of_command(command: &str) -> AgentKind {
        let named_claude = Path::new(command)
            .file_stem()
            .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("claude"));
        if named_claude {
            AgentKind::ClaudeCode
        } else {
            AgentKind::Program
        }
    }

    /// The word saved with an agent of this kind.
    pub fn saved_word(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Program => "program",
        }
    }

    /// The kind a saved word names. None for a word this Lillignode does
    /// not know, which a newer one may have written.
    pub fn from_saved(word: &str) -> Option<AgentKind> {
        [AgentKind::ClaudeCode, AgentKind::Program].into_iter().find(|kind| kind.saved_word() == word)
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
            AgentKind::Program => command.to_string(),
        }
    }

    /// Whether an agent of this kind is given a conversation id when it is
    /// started, to be continued by that id later.
    pub fn keeps_conversations(self) -> bool {
        match self {
            AgentKind::ClaudeCode => true,
            AgentKind::Program => false,
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
            AgentKind::Program => Vec::new(),
        }
    }

    /// Where the status of an agent of this kind comes from.
    pub fn status_source(self) -> Box<dyn StatusSource> {
        match self {
            AgentKind::ClaudeCode => Box::new(ClaudeReport::default()),
            AgentKind::Program => Box::new(NoSource::new(Why::NotClaudeCode)),
        }
    }

    /// What the terminal of an agent of this kind does with status messages.
    pub fn status_messages(self) -> StatusMessages {
        match self {
            AgentKind::ClaudeCode => StatusMessages::Answered,
            AgentKind::Program => StatusMessages::Ignored,
        }
    }

    /// Whether Lillignode can read what a conversation of this kind has used.
    pub fn has_cost_record(self) -> bool {
        match self {
            AgentKind::ClaudeCode => true,
            AgentKind::Program => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_named_claude_is_claude_code_and_nothing_else_is() {
        for command in
            ["claude", "claude.cmd", "CLAUDE.EXE", "C:\\tools\\claude.exe", "/usr/local/bin/claude"]
        {
            assert_eq!(AgentKind::of_command(command), AgentKind::ClaudeCode, "{command}");
        }
        for command in ["codex", "claude-wrapper", "cc.cmd", "myclaude", "", "claude code"] {
            assert_eq!(AgentKind::of_command(command), AgentKind::Program, "{command:?}");
        }
    }

    #[test]
    fn every_kind_is_read_back_from_the_word_it_is_saved_as() {
        for kind in [AgentKind::ClaudeCode, AgentKind::Program] {
            assert_eq!(AgentKind::from_saved(kind.saved_word()), Some(kind));
        }
        assert_eq!(AgentKind::from_saved("Claude-Code"), None);
        assert_eq!(AgentKind::from_saved("codex"), None);
        assert_eq!(AgentKind::from_saved(""), None);
    }

    #[test]
    fn the_saved_word_decides_and_the_command_only_when_there_is_none() {
        assert_eq!(AgentKind::of_saved(None, "claude"), AgentKind::ClaudeCode);
        assert_eq!(AgentKind::of_saved(None, "codex"), AgentKind::Program);
        assert_eq!(AgentKind::of_saved(Some("program"), "claude"), AgentKind::Program);
        assert_eq!(AgentKind::of_saved(Some("claude-code"), "cc.cmd"), AgentKind::ClaudeCode);
        // A word from a newer Lillignode is not guessed at.
        assert_eq!(AgentKind::of_saved(Some("codex"), "claude"), AgentKind::Program);
    }

    #[test]
    fn only_claude_code_is_told_which_conversation_to_use() {
        assert_eq!(AgentKind::ClaudeCode.conversation_args("abc", false), ["--session-id", "abc"]);
        assert_eq!(AgentKind::ClaudeCode.conversation_args("abc", true), ["--resume", "abc"]);
        assert!(AgentKind::Program.conversation_args("abc", true).is_empty());
    }
}
