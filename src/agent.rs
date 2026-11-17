//! The one interface between Lillignode and running agents.
//!
//! Everything outside this module and `local` talks to agents only through
//! `AgentBackend`. Today the only backend starts agents as child processes of
//! this window (`local::LocalAgents`). Background running
//! is added later as a second backend that forwards the same calls to a
//! process that outlives the window; nothing else should need to change.

use std::fmt;
use std::path::PathBuf;

/// Identifies one running agent for the lifetime of this window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AgentId(pub u32);

/// What to run and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSpec {
    /// Program name or path, for example `claude`.
    pub command: String,
    pub args: Vec<String>,
    /// The folder the agent works in.
    pub folder: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

/// Why an agent could not be started. Each variant is a different thing for
/// the user to fix, so they are never merged into one message.
#[derive(Debug)]
pub enum StartError {
    /// The folder is gone, or is not a folder.
    FolderMissing(PathBuf),
    /// The program is not installed or not on PATH.
    CommandNotFound(String),
    /// Found, but the operating system refused to start it.
    Failed(String),
    /// Lillignode would not start it as given, and why.
    Refused(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::FolderMissing(p) => {
                write!(f, "folder {} does not exist", p.display())
            }
            StartError::CommandNotFound(c) => {
                write!(f, "\"{c}\" is not installed or not on PATH")
            }
            StartError::Failed(why) => write!(f, "it would not start ({why})"),
            StartError::Refused(why) => write!(f, "{why}"),
        }
    }
}

/// What to do about a saved entry Lillignode refuses to start. Starting it
/// again cannot help: the entry is the cause, and a running Lillignode would
/// save its own copy over a fix made to the file meanwhile.
pub const FIX_IN_THE_FILE: &str = "Nothing was run. r will not help: x removes it, or quit Lillignode, \
    correct workspace.json (lillignode --where says where it is) and start Lillignode again";

/// How an agent process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: u32,
}

impl Exit {
    pub fn clean(self) -> bool {
        self.code == 0
    }
}

/// Something an agent did. Delivered through the sender given to the backend.
#[derive(Debug)]
pub enum AgentEvent {
    /// Bytes the agent wrote to its terminal.
    Output(AgentId, Vec<u8>),
    /// The agent process ended. Sent exactly once per started agent.
    Exited(AgentId, Exit),
}

pub trait AgentBackend {
    /// Start an agent. On success its events begin to arrive.
    fn start(&mut self, spec: &AgentSpec, size: Size) -> Result<AgentId, StartError>;

    /// Send keystrokes to an agent. Errors mean the agent is gone.
    fn send(&mut self, id: AgentId, bytes: &[u8]) -> std::io::Result<()>;

    /// Tell an agent its drawing area changed size.
    fn resize(&mut self, id: AgentId, size: Size);

    /// Stop an agent and release everything it holds. Safe to call twice.
    fn close(&mut self, id: AgentId);
}
