//! The workspace: the agents the user has open, and what became of each.
//!
//! Every saved agent always has an entry here, whether or not it could be
//! started. An agent that could not be restored is shown with the reason and
//! stays in the workspace until the user removes it; nothing is dropped
//! silently.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::agent::{AgentBackend, AgentEvent, AgentId, AgentSpec, Exit, Size, StartError};
use crate::cost::{self, Cost};
use crate::state::{SavedAgent, SavedWorkspace, VERSION};
use crate::status::{AgentStatus, Kind, Track, Why};
use crate::terminal::{StatusMessage, Terminal};

/// A resumed agent that stops with an error this soon, before the user has
/// typed anything to it, is taken to have failed to resume.
const RESUME_WINDOW: Duration = Duration::from_secs(15);

/// What Lillignode knows for certain about an agent's program: running, ended,
/// or never started. What a running Claude Code agent is doing is in
/// `Entry::track`, and comes only from the agent's own report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    /// The process ended. It was running, and now it is not.
    Exited(Exit),
    /// It was never started, and why.
    NotStarted(String),
}

impl Status {
    /// The words shown for the program itself.
    pub fn text(&self) -> String {
        match self {
            Status::Running => "Running".into(),
            Status::Exited(e) if e.clean() => "Exited".into(),
            Status::Exited(e) => format!("Exited (error {})", e.code),
            Status::NotStarted(_) => "Not started".into(),
        }
    }
}

pub struct Entry {
    pub saved: SavedAgent,
    pub status: Status,
    pub term: Terminal,
    /// What the agent has said about its status since it was last started.
    pub track: Track,
    /// Reads what the agent's conversation has used. None for an agent
    /// whose cost Lillignode cannot read at all.
    usage: Option<cost::Reader>,
    id: Option<AgentId>,
    /// Set while a resume is unproven: when it was started.
    resuming_since: Option<Instant>,
}

impl Entry {
    /// "Claude Code (api-refactor)": the name plus the folder's last part.
    pub fn label(&self) -> String {
        format!("{} ({})", self.saved.name, self.folder_name())
    }

    pub fn folder_name(&self) -> String {
        folder_name(&self.saved.folder)
    }

    pub fn is_running(&self) -> bool {
        self.status == Status::Running
    }

    pub fn is_claude_code(&self) -> bool {
        is_claude_code(&self.saved.command)
    }

    /// What the sidebar shows a running agent doing: what Claude Code
    /// reported, or Unknown for any other agent. None once its program
    /// is not running: then the program state is all there is to show.
    pub fn reported(&self) -> Option<AgentStatus> {
        if !self.is_running() {
            None
        } else if self.is_claude_code() {
            Some(self.track.shown.clone())
        } else {
            Some(AgentStatus::Unknown(Why::NotClaudeCode))
        }
    }

    /// What the agent's conversation has used so far, or why that is not
    /// known. Read only for Claude Code, running or not: the cost
    /// belongs to the conversation, which outlives the program.
    pub fn cost(&self) -> Cost {
        if !self.is_claude_code() {
            return Cost::Unknown(cost::WhyNot::NotClaudeCode);
        }
        match &self.usage {
            Some(reader) => reader.cost().clone(),
            None => Cost::Unknown(cost::WhyNot::NoConversation),
        }
    }

    /// When Claude Code's record of the conversation last changed.
    pub fn cost_last_grew(&self) -> Option<SystemTime> {
        self.usage.as_ref().and_then(|reader| reader.last_grew)
    }

    /// A running agent that is finished, so stopping it loses nothing in
    /// progress: Done, Idle, or Error. Anything else, Unknown included, may be
    /// in the middle of something.
    pub fn is_settled(&self) -> bool {
        matches!(self.reported(), Some(AgentStatus::Done | AgentStatus::Idle | AgentStatus::Error(_)))
    }

    /// Some(kind) while the agent needs you; the kind is None when Claude
    /// Code did not say what for.
    pub fn needs_you(&self) -> Option<Option<Kind>> {
        match self.reported() {
            Some(AgentStatus::NeedsYou(kind)) => Some(kind),
            _ => None,
        }
    }
}

/// The last part of a folder's path, which is what the user is shown.
fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string())
}

/// True for the one agent Lillignode supports by name.
pub fn is_claude_code(command: &str) -> bool {
    std::path::Path::new(command)
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("claude"))
}

/// The name shown for a command. Only agents Lillignode knows by name get one;
/// anything else is shown as typed.
pub fn display_name(command: &str) -> String {
    if is_claude_code(command) {
        "Claude Code".to_string()
    } else {
        command.to_string()
    }
}

/// Whether to begin a conversation or continue the saved one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conversation {
    New,
    Continue,
}

/// The arguments to start an agent with. Claude Code is told which
/// conversation to use; anything else gets only what was saved.
fn launch_args(saved: &SavedAgent, conversation: Conversation) -> Vec<String> {
    let mut args = saved.args.clone();
    if let (true, Some(id)) = (is_claude_code(&saved.command), &saved.conversation) {
        let flag = if conversation == Conversation::Continue { "--resume" } else { "--session-id" };
        args.extend([flag.to_string(), id.clone()]);
    }
    args
}

/// The reader of a saved agent's cost, for an agent that has one to read.
fn usage_reader(saved: &SavedAgent) -> Option<cost::Reader> {
    let id = saved.conversation.as_deref().filter(|_| is_claude_code(&saved.command))?;
    Some(cost::Reader::new(id))
}

fn new_conversation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// How one agent came back when the workspace was restored.
#[derive(Debug, PartialEq, Eq)]
pub struct Restored {
    pub label: String,
    /// True if it was asked to continue its earlier conversation.
    pub continuing: bool,
}

/// What restoring did, for the message shown at launch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored: Vec<Restored>,
    /// (label, reason)
    pub not_restored: Vec<(String, String)>,
}

/// Something that happened to an agent that the user should be told.
#[derive(Debug, PartialEq, Eq)]
pub enum Happened {
    Exited {
        label: String,
        exit: Exit,
    },
    /// It could not continue its earlier conversation and was started again
    /// with a new one.
    StartedNewConversation {
        label: String,
    },
    /// As above, but the fresh start failed too.
    CouldNotContinue {
        label: String,
        why: String,
    },
}

pub struct Workspace<B: AgentBackend> {
    backend: B,
    entries: Vec<Entry>,
    focused: usize,
    size: Size,
    /// Where Claude Code keeps its records, for reading cost. None until
    /// the program says, so nothing built for a test ever looks at a real one.
    claude_folder: Option<PathBuf>,
}

impl<B: AgentBackend> Workspace<B> {
    pub fn new(backend: B, size: Size) -> Self {
        Self { backend, entries: Vec::new(), focused: 0, size, claude_folder: None }
    }

    /// Say where Claude Code keeps its records. Cost is Unknown until then.
    pub fn set_claude_folder(&mut self, folder: Option<PathBuf>) {
        self.claude_folder = folder;
    }

    pub fn claude_folder(&self) -> Option<&Path> {
        self.claude_folder.as_deref()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn focused_index(&self) -> Option<usize> {
        (!self.entries.is_empty()).then_some(self.focused)
    }

    pub fn focused(&self) -> Option<&Entry> {
        self.entries.get(self.focused)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Entry> {
        self.entries.get_mut(self.focused)
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn running_labels(&self) -> Vec<String> {
        self.entries.iter().filter(|e| e.is_running()).map(Entry::label).collect()
    }

    /// A name that no agent already open shares for a folder shown the same
    /// way: "Claude Code", then "Claude Code 2", "Claude Code 3". The number
    /// is part of the saved name, so it does not change when another agent
    /// is closed.
    fn distinct_name(&self, name: &str, folder: &Path) -> String {
        let shown = folder_name(folder);
        let taken = |candidate: &str| {
            self.entries.iter().any(|e| e.saved.name == candidate && e.folder_name() == shown)
        };
        if !taken(name) {
            return name.to_string();
        }
        (2..).map(|n| format!("{name} {n}")).find(|c| !taken(c)).expect("some number is free")
    }

    fn try_start(&mut self, saved: &SavedAgent, conversation: Conversation) -> (Status, Option<AgentId>) {
        // Checked before the backend sees it: a saved entry is outside data.
        if saved.command.trim().is_empty() {
            return (Status::NotStarted("its saved entry has no command".into()), None);
        }
        if !saved.folder.is_absolute() {
            return (
                Status::NotStarted(format!(
                    "its saved folder \"{}\" is not a full path",
                    saved.folder.display()
                )),
                None,
            );
        }
        let spec = AgentSpec {
            command: saved.command.clone(),
            args: launch_args(saved, conversation),
            folder: saved.folder.clone(),
        };
        match self.backend.start(&spec, self.size) {
            Ok(id) => (Status::Running, Some(id)),
            Err(e) => (Status::NotStarted(e.to_string()), None),
        }
    }

    /// Start a saved agent, continuing its conversation if it has one.
    /// Returns the entry and whether it was asked to continue.
    fn open(&mut self, mut saved: SavedAgent) -> (Entry, bool) {
        let continuing = is_claude_code(&saved.command) && saved.conversation.is_some();
        if is_claude_code(&saved.command) && saved.conversation.is_none() {
            // Saved before conversations were tracked: begin tracking now.
            saved.conversation = Some(new_conversation_id());
        }
        let how = if continuing { Conversation::Continue } else { Conversation::New };
        let (status, id) = self.try_start(&saved, how);
        let resuming_since = (continuing && id.is_some()).then(Instant::now);
        let entry = Entry {
            usage: usage_reader(&saved),
            saved,
            status,
            term: Terminal::new(self.size.rows, self.size.cols),
            track: Track::new(Instant::now()),
            id,
            resuming_since,
        };
        (entry, continuing)
    }

    /// Reopen every saved agent. Each one ends up Running or NotStarted with
    /// its reason; none is skipped.
    pub fn restore(&mut self, saved: SavedWorkspace) -> RestoreReport {
        let mut report = RestoreReport::default();
        for mut agent in saved.agents {
            // A workspace saved before names were numbered may hold twins.
            agent.name = self.distinct_name(&agent.name, &agent.folder);
            let (entry, continuing) = self.open(agent);
            match &entry.status {
                Status::NotStarted(why) => report.not_restored.push((entry.label(), why.clone())),
                _ => report.restored.push(Restored { label: entry.label(), continuing }),
            }
            self.entries.push(entry);
        }
        // A focus index pointing past the list is stale, not fatal.
        self.focused = saved.focused.filter(|&i| i < self.entries.len()).unwrap_or(0);
        report
    }

    /// Start a new agent and put it in front. On failure nothing is added.
    pub fn start_new(&mut self, command: &str, folder: PathBuf) -> Result<(), StartError> {
        let saved = SavedAgent {
            name: self.distinct_name(&display_name(command), &folder),
            command: command.to_string(),
            args: Vec::new(),
            folder: folder.clone(),
            conversation: is_claude_code(command).then(new_conversation_id),
        };
        let spec =
            AgentSpec { command: command.to_string(), args: launch_args(&saved, Conversation::New), folder };
        let id = self.backend.start(&spec, self.size)?;
        self.entries.push(Entry {
            usage: usage_reader(&saved),
            saved,
            status: Status::Running,
            term: Terminal::new(self.size.rows, self.size.cols),
            track: Track::new(Instant::now()),
            id: Some(id),
            resuming_since: None,
        });
        self.focused = self.entries.len() - 1;
        Ok(())
    }

    /// Start the focused agent again after it exited or failed to start.
    /// Returns whether it was asked to continue its conversation, or None if
    /// there was nothing to restart.
    pub fn restart_focused(&mut self) -> Option<bool> {
        let entry = self.entries.get(self.focused)?;
        if entry.is_running() {
            return None;
        }
        let (entry, continuing) = self.open(entry.saved.clone());
        self.entries[self.focused] = entry;
        Some(continuing)
    }

    /// Stop the focused agent if it is running and remove it from the workspace.
    pub fn close_focused(&mut self) -> Option<String> {
        if self.focused >= self.entries.len() {
            return None;
        }
        let entry = self.entries.remove(self.focused);
        if let Some(id) = entry.id {
            self.backend.close(id);
        }
        self.focused = self.focused.min(self.entries.len().saturating_sub(1));
        Some(entry.label())
    }

    pub fn focus_next(&mut self) {
        if !self.entries.is_empty() {
            self.focused = (self.focused + 1) % self.entries.len();
        }
    }

    pub fn focus_previous(&mut self) {
        if !self.entries.is_empty() {
            self.focused = (self.focused + self.entries.len() - 1) % self.entries.len();
        }
    }

    /// Send what the user typed or pasted to the focused agent. Dropped if it
    /// is not running. The view returns to the present, so the user sees
    /// where their typing lands.
    pub fn send_to_focused(&mut self, bytes: &[u8]) {
        if let Some(entry) = self.entries.get_mut(self.focused) {
            if let (Some(id), Status::Running) = (entry.id, &entry.status) {
                entry.term.scroll_to_live();
                // Once the user is using it, the resume worked.
                entry.resuming_since = None;
                let _ = self.backend.send(id, bytes);
            }
        }
    }

    pub fn resize(&mut self, size: Size) {
        if size == self.size {
            return;
        }
        self.size = size;
        for entry in &mut self.entries {
            entry.term.resize(size.rows, size.cols);
            if let (Some(id), true) = (entry.id, entry.status == Status::Running) {
                self.backend.resize(id, size);
            }
        }
    }

    pub fn handle(&mut self, event: AgentEvent) -> Option<Happened> {
        match event {
            AgentEvent::Output(id, bytes) => {
                let entry = self.entries.iter_mut().find(|e| e.id == Some(id))?;
                entry.track.output(Instant::now());
                let reply = entry.term.feed(&bytes);
                if !reply.is_empty() {
                    let _ = self.backend.send(id, &reply);
                }
                for message in entry.term.take_status() {
                    match message {
                        StatusMessage::Asked => entry.track.asked(Instant::now()),
                        StatusMessage::Report(body) => entry.track.received(&body, SystemTime::now()),
                    }
                }
                None
            }
            AgentEvent::Exited(id, exit) => {
                // An agent the user already closed also reports its exit; by
                // then it has no entry and there is nothing to say.
                let index = self.entries.iter().position(|e| e.id == Some(id))?;
                self.backend.close(id);
                let entry = &mut self.entries[index];
                entry.id = None;
                entry.status = Status::Exited(exit);
                let label = entry.label();
                let resume_failed =
                    !exit.clean() && entry.resuming_since.take().is_some_and(|t| t.elapsed() < RESUME_WINDOW);
                if !resume_failed {
                    return Some(Happened::Exited { label, exit });
                }
                // The earlier conversation is not there to continue (for
                // example, nothing was ever said in it). Start a new one
                // rather than leave a dead agent.
                let mut saved = entry.saved.clone();
                saved.conversation = Some(new_conversation_id());
                let (status, id) = self.try_start(&saved, Conversation::New);
                let entry = &mut self.entries[index];
                match status {
                    Status::Running => {
                        // A new conversation has a new record, and its own cost.
                        entry.usage = usage_reader(&saved);
                        entry.saved = saved;
                        entry.status = Status::Running;
                        entry.id = id;
                        entry.term = Terminal::new(self.size.rows, self.size.cols);
                        entry.track = Track::new(Instant::now());
                        Some(Happened::StartedNewConversation { label })
                    }
                    Status::NotStarted(why) => Some(Happened::CouldNotContinue { label, why }),
                    Status::Exited(_) => unreachable!("try_start never reports an exit"),
                }
            }
        }
    }

    /// True if any Claude Code agent changed into Needs you since the last
    /// call: the bell rings once for each such change.
    pub fn take_began_needing_you(&mut self) -> bool {
        let mut any = false;
        for entry in self.entries.iter_mut().filter(|e| e.is_running() && e.is_claude_code()) {
            any |= entry.track.take_began_needing_you();
        }
        any
    }

    /// Call regularly. True if any status or cost changed and should be
    /// redrawn.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            if entry.is_running() {
                changed |= entry.track.tick(now);
            }
            // Whether or not the program is running: the record is still there.
            if let Some(reader) = &mut entry.usage {
                changed |= reader.look(now, self.claude_folder.as_deref());
            }
        }
        changed
    }

    pub fn to_saved(&self) -> SavedWorkspace {
        SavedWorkspace {
            version: VERSION,
            agents: self.entries.iter().map(|e| e.saved.clone()).collect(),
            focused: self.focused_index(),
        }
    }

    /// Stop every agent. Used at quit.
    pub fn stop_all(&mut self) {
        for entry in &mut self.entries {
            if let Some(id) = entry.id.take() {
                self.backend.close(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A backend that starts nothing. `claude` in an existing folder works;
    /// everything else fails the way the real one would.
    #[derive(Default)]
    struct Fake {
        next: u32,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl AgentBackend for Fake {
        fn start(&mut self, spec: &AgentSpec, _: Size) -> Result<AgentId, StartError> {
            if spec.folder.to_string_lossy().contains("gone") {
                return Err(StartError::FolderMissing(spec.folder.clone()));
            }
            if spec.command != "claude" {
                return Err(StartError::CommandNotFound(spec.command.clone()));
            }
            self.next += 1;
            self.log.borrow_mut().push(format!("start {} {}", self.next, spec.args.join(" ")));
            Ok(AgentId(self.next))
        }
        fn send(&mut self, id: AgentId, bytes: &[u8]) -> std::io::Result<()> {
            self.log.borrow_mut().push(format!("send {} {:?}", id.0, String::from_utf8_lossy(bytes)));
            Ok(())
        }
        fn resize(&mut self, _: AgentId, _: Size) {}
        fn close(&mut self, id: AgentId) {
            self.log.borrow_mut().push(format!("close {}", id.0));
        }
    }

    const SIZE: Size = Size { rows: 24, cols: 80 };
    const ID: &str = "11111111-2222-4333-8444-555555555555";

    fn root(name: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\work\\{name}"))
        } else {
            PathBuf::from(format!("/work/{name}"))
        }
    }

    fn agent(command: &str, folder: &str) -> SavedAgent {
        SavedAgent {
            name: display_name(command),
            command: command.into(),
            args: vec![],
            folder: root(folder),
            conversation: is_claude_code(command).then(|| ID.to_string()),
        }
    }

    fn saved(agents: Vec<SavedAgent>, focused: Option<usize>) -> SavedWorkspace {
        SavedWorkspace { version: VERSION, agents, focused }
    }

    fn log(ws: &Workspace<Fake>) -> Vec<String> {
        ws.backend.log.borrow().clone()
    }

    #[test]
    fn restore_reports_what_came_back_and_what_did_not_and_keeps_every_entry() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let report = ws.restore(saved(
            vec![agent("claude", "api"), agent("codex", "web"), agent("claude", "gone")],
            Some(1),
        ));

        assert_eq!(report.restored, vec![Restored { label: "Claude Code (api)".into(), continuing: true }]);
        assert_eq!(report.not_restored.len(), 2);
        assert_eq!(report.not_restored[0].0, "codex (web)");
        assert!(report.not_restored[0].1.contains("not installed"), "{:?}", report.not_restored[0]);
        assert!(report.not_restored[1].1.contains("does not exist"), "{:?}", report.not_restored[1]);

        // Nothing was dropped: a failed agent is still in the workspace and
        // still saved, so fixing the cause and restarting works.
        assert_eq!(ws.entries().len(), 3);
        assert_eq!(ws.to_saved().agents.len(), 3);
        assert_eq!(ws.focused_index(), Some(1));
    }

    #[test]
    fn restore_rejects_incomplete_saved_entries_with_a_reason() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let mut no_command = agent("claude", "api");
        no_command.command = "  ".into();
        let mut relative = agent("claude", "api");
        relative.folder = PathBuf::from("api");
        let report = ws.restore(saved(vec![no_command, relative], Some(7)));
        assert!(report.restored.is_empty());
        assert!(report.not_restored[0].1.contains("no command"));
        assert!(report.not_restored[1].1.contains("not a full path"));
        // A stale focus index falls back to the first agent.
        assert_eq!(ws.focused_index(), Some(0));
    }

    #[test]
    fn a_new_claude_code_agent_gets_a_conversation_id_and_restore_continues_it() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("api")).unwrap();
        let id = ws.to_saved().agents[0].conversation.clone().expect("a conversation id is saved");
        assert_eq!(log(&ws), vec![format!("start 1 --session-id {id}")]);

        let mut again = Workspace::new(Fake::default(), SIZE);
        let report = again.restore(ws.to_saved());
        assert!(report.restored[0].continuing);
        assert_eq!(log(&again), vec![format!("start 1 --resume {id}")]);
    }

    #[test]
    fn a_resume_that_fails_at_once_becomes_a_new_conversation_and_says_so() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "api")], Some(0)));
        // Claude Code prints "No conversation found" and exits with code 1.
        let happened = ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 1 }));
        assert_eq!(happened, Some(Happened::StartedNewConversation { label: "Claude Code (api)".into() }));
        assert_eq!(ws.focused().unwrap().status, Status::Running);

        let new_id = ws.to_saved().agents[0].conversation.clone().unwrap();
        assert_ne!(new_id, ID, "the id that failed must not be kept");
        assert_eq!(log(&ws).last().unwrap(), &format!("start 2 --session-id {new_id}"));

        // If the new one dies too, that is an ordinary exit: no second retry.
        let happened = ws.handle(AgentEvent::Exited(AgentId(2), Exit { code: 1 }));
        assert!(matches!(happened, Some(Happened::Exited { .. })));
    }

    #[test]
    fn an_exit_after_the_user_typed_is_an_exit_not_a_failed_resume() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "api")], Some(0)));
        ws.send_to_focused(b"hello");
        let happened = ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 1 }));
        assert!(matches!(happened, Some(Happened::Exited { .. })));
        assert_eq!(ws.to_saved().agents[0].conversation.as_deref(), Some(ID));
    }

    #[test]
    fn an_older_saved_workspace_without_ids_starts_fresh_and_begins_tracking() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let mut old = agent("claude", "api");
        old.conversation = None;
        let report = ws.restore(saved(vec![old], Some(0)));
        assert!(!report.restored[0].continuing);
        let id = ws.to_saved().agents[0].conversation.clone().unwrap();
        assert_eq!(log(&ws), vec![format!("start 1 --session-id {id}")]);
    }

    #[test]
    fn an_agent_that_dies_is_marked_exited_and_can_be_started_again() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("api")).unwrap();
        assert_eq!(ws.focused().unwrap().status, Status::Running);

        let died = ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 3 }));
        assert_eq!(
            died,
            Some(Happened::Exited { label: "Claude Code (api)".into(), exit: Exit { code: 3 } })
        );
        assert_eq!(ws.focused().unwrap().status, Status::Exited(Exit { code: 3 }));
        // Still in the workspace, so it comes back on the next launch.
        assert_eq!(ws.to_saved().agents.len(), 1);

        // Keys typed at a dead agent go nowhere.
        ws.send_to_focused(b"x");
        assert!(!log(&ws).iter().any(|l| l.starts_with("send")));

        assert_eq!(ws.restart_focused(), Some(true));
        assert_eq!(ws.focused().unwrap().status, Status::Running);
        assert!(log(&ws).last().unwrap().contains("--resume"));
    }

    #[test]
    fn a_failed_start_adds_nothing() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        assert!(ws.start_new("codex", root("api")).is_err());
        assert!(ws.start_new("claude", root("gone")).is_err());
        assert!(ws.entries().is_empty());
        assert_eq!(ws.focused_index(), None);
    }

    #[test]
    fn closing_stops_the_agent_removes_it_and_ignores_its_late_exit() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("a")).unwrap();
        ws.start_new("claude", root("b")).unwrap();
        assert_eq!(ws.close_focused(), Some("Claude Code (b)".to_string()));
        assert!(log(&ws).contains(&"close 2".to_string()));
        assert_eq!(ws.focused().unwrap().label(), "Claude Code (a)");
        assert_eq!(ws.handle(AgentEvent::Exited(AgentId(2), Exit { code: 1 })), None);
        assert_eq!(ws.entries().len(), 1);
    }

    #[test]
    fn agents_shown_with_the_same_folder_get_numbered_names_that_stay_put() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        for _ in 0..3 {
            ws.start_new("claude", root("api")).unwrap();
        }
        ws.start_new("claude", root("web")).unwrap();
        let labels = |ws: &Workspace<Fake>| ws.entries().iter().map(Entry::label).collect::<Vec<_>>();
        assert_eq!(
            labels(&ws),
            ["Claude Code (api)", "Claude Code 2 (api)", "Claude Code 3 (api)", "Claude Code (web)"]
        );

        // Closing one does not renumber the others, and its number is reused
        // only by a new agent.
        ws.focus_next();
        ws.focus_next();
        assert_eq!(ws.close_focused(), Some("Claude Code 2 (api)".to_string()));
        assert_eq!(labels(&ws), ["Claude Code (api)", "Claude Code 3 (api)", "Claude Code (web)"]);
        ws.start_new("claude", root("api")).unwrap();
        assert_eq!(ws.entries().last().unwrap().label(), "Claude Code 2 (api)");

        // Restoring keeps the numbers as saved.
        let mut again = Workspace::new(Fake::default(), SIZE);
        again.restore(ws.to_saved());
        assert_eq!(labels(&again), labels(&ws));
    }

    #[test]
    fn a_workspace_saved_with_identical_agents_is_numbered_on_restore() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let report = ws.restore(saved(vec![agent("claude", "api"), agent("claude", "api")], Some(0)));
        assert_eq!(report.restored[1].label, "Claude Code 2 (api)");
        assert_eq!(ws.to_saved().agents[1].name, "Claude Code 2");
    }

    #[test]
    fn terminal_questions_are_answered_to_the_agent_that_asked() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("a")).unwrap();
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b[6n".to_vec()));
        assert!(log(&ws).contains(&"send 1 \"\\u{1b}[1;1R\"".to_string()));
    }

    #[test]
    fn a_claude_code_agent_shows_what_it_reports_and_starts_over_when_restarted() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("a")).unwrap();
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Starting));
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;?\x07".to_vec()));
        assert!(log(&ws).contains(&"send 1 \"\\u{1b}]7501;?\\u{7}\"".to_string()), "{:?}", log(&ws));
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;state=working:app=claude-code\x07".to_vec()));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Working));

        // Once it exits there is no reported status, and starting it again
        // begins at Starting: nothing is carried over.
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 0 }));
        assert_eq!(ws.focused().unwrap().reported(), None);
        ws.restart_focused();
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Starting));
        assert!(ws.focused().unwrap().track.last.is_none());
    }

    #[test]
    fn only_claude_code_gets_a_reported_status() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "a")], Some(0)));
        // Same backend, but saved under another command name.
        ws.entries[0].saved.command = "codex".into();
        // Even if it sends something that looks like a report.
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;?\x07".to_vec()));
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;state=blocked:app=claude-code\x07".to_vec()));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::NotClaudeCode)));
        assert!(!ws.take_began_needing_you(), "no bell for an agent whose status is not read");
        assert!(!ws.focused().unwrap().is_settled());
    }

    #[test]
    fn needs_you_rings_once_and_settled_means_done_idle_or_error() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("a")).unwrap();
        let report = |ws: &mut Workspace<Fake>, body: &str| {
            ws.handle(AgentEvent::Output(AgentId(1), format!("\x1b]7501;{body}\x07").into_bytes()));
        };
        report(&mut ws, "?");
        assert!(!ws.focused().unwrap().is_settled(), "Starting is not settled");
        report(&mut ws, "state=blocked:app=claude-code:kind=permission");
        assert_eq!(ws.focused().unwrap().needs_you(), Some(Some(Kind::Permission)));
        assert!(ws.take_began_needing_you());
        assert!(!ws.take_began_needing_you());
        for (body, settled) in [
            ("state=working:app=claude-code", false),
            ("state=done:app=claude-code", true),
            ("state=idle:app=claude-code", true),
            ("state=error:app=claude-code", true),
            ("state=pondering:app=claude-code", false),
        ] {
            report(&mut ws, body);
            assert_eq!(ws.focused().unwrap().is_settled(), settled, "{body}");
        }
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 0 }));
        assert!(!ws.focused().unwrap().is_settled(), "an exited agent is not running at all");
    }

    #[test]
    fn the_deadline_turns_a_silent_agent_unknown() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("a")).unwrap();
        // The first tick also takes a first look at the cost.
        ws.tick(Instant::now());
        assert!(!ws.tick(Instant::now()));
        assert!(ws.tick(Instant::now() + crate::status::STARTING_DEADLINE));
        assert!(matches!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(_))));
    }

    #[test]
    fn a_working_agent_that_draws_nothing_for_a_minute_is_working_quiet_and_not_settled() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("a")).unwrap();
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;?\x07".to_vec()));
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;state=working:app=claude-code\x07".to_vec()));
        let start = Instant::now();
        ws.tick(start);
        assert!(!ws.tick(start + crate::status::SILENCE / 2));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Working));
        assert!(ws.tick(start + crate::status::SILENCE + Duration::from_secs(1)));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::WorkingQuiet(1)));
        assert!(!ws.focused().unwrap().is_settled(), "quit still asks");
    }

    /// A throwaway stand-in for Claude Code's folder, with one reply
    /// recorded for conversation `id`. Never the real one.
    fn claude_folder_with_record(name: &str, id: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-test-ws-cost-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("projects").join("C--work-a");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            "{\"type\":\"assistant\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-5-5\",\"usage\":{\
             \"input_tokens\":100,\"output_tokens\":50,\"cache_creation_input_tokens\":1000,\
             \"cache_read_input_tokens\":2000}}}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn cost_is_read_for_claude_code_only_and_is_unknown_until_lillignode_is_told_where_to_look() {
        use crate::cost::{Cost, Waiting, WhyNot};
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "a"), agent("claude", "b")], Some(0)));
        ws.entries[1].saved.command = "codex".into();
        assert_eq!(ws.entries[0].cost(), Cost::Pending(Waiting::NotLooked));
        assert_eq!(ws.entries[1].cost(), Cost::Unknown(WhyNot::NotClaudeCode));
        // No folder named: nothing is looked at, and it says so.
        assert!(ws.tick(Instant::now()));
        assert_eq!(ws.entries[0].cost(), Cost::Unknown(WhyNot::NoFolder));

        let folder = claude_folder_with_record("read", ID);
        ws.set_claude_folder(Some(folder.clone()));
        assert!(ws.tick(Instant::now() + crate::cost::LOOK_EVERY));
        let Cost::Known(usage) = ws.entries[0].cost() else { panic!("{:?}", ws.entries[0].cost()) };
        assert_eq!(usage.total().total(), 3150);
        assert!(ws.entries[0].cost_last_grew().is_some());

        // The cost belongs to the conversation: it is still there once the
        // program has ended.
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 0 }));
        ws.tick(Instant::now() + crate::cost::LOOK_EVERY * 2);
        assert!(matches!(ws.entries[0].cost(), Cost::Known(_)));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_new_conversation_starts_its_cost_afresh() {
        use crate::cost::Cost;
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let folder = claude_folder_with_record("fresh", ID);
        ws.set_claude_folder(Some(folder.clone()));
        ws.restore(saved(vec![agent("claude", "a")], Some(0)));
        ws.tick(Instant::now());
        assert!(matches!(ws.entries[0].cost(), Cost::Known(_)));
        // The resume fails, so a new conversation is started: the earlier
        // conversation's cost must not be shown as this one's.
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 1 }));
        ws.tick(Instant::now() + crate::cost::LOOK_EVERY);
        assert!(matches!(ws.entries[0].cost(), Cost::Pending(_)), "{:?}", ws.entries[0].cost());
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn typing_returns_the_view_to_the_present() {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 3, cols: 20 });
        ws.start_new("claude", root("a")).unwrap();
        let lines: String = (1..=10).map(|i| format!("line {i}\r\n")).collect();
        ws.handle(AgentEvent::Output(AgentId(1), lines.into_bytes()));
        ws.focused_mut().unwrap().term.scroll(5);
        assert_eq!(ws.focused().unwrap().term.scrolled_back(), 5);
        ws.send_to_focused(b"x");
        assert_eq!(ws.focused().unwrap().term.scrolled_back(), 0);
    }
}
