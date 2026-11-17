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
use crate::kind::AgentKind;
use crate::state::{SavedAgent, SavedWorkspace, VERSION};
use crate::status::{AgentStatus, Kind, Report, Track, Why};
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

/// Names one agent for as long as Lillignode runs. Its place in the list
/// changes when another agent is closed, and its `AgentId` changes each time
/// it is started; its key does neither, and is never given to another agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AgentKey(u64);

pub struct Entry {
    key: AgentKey,
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
    /// The user has been told this conversation passed their cost warning.
    cost_warned: bool,
}

impl Entry {
    pub fn key(&self) -> AgentKey {
        self.key
    }

    /// "Claude Code (api-refactor)": the name plus the folder's last part.
    pub fn label(&self) -> String {
        format!("{} ({})", self.name(), self.folder_name())
    }

    /// The name as it is shown. A saved name is outside data: any control
    /// character in it is shown as a space, so it cannot act on the screen.
    pub fn name(&self) -> String {
        printable(&self.saved.name)
    }

    pub fn folder_name(&self) -> String {
        printable(&folder_name(&self.saved.folder))
    }

    pub fn is_running(&self) -> bool {
        self.status == Status::Running
    }

    pub fn kind(&self) -> AgentKind {
        kind_of(&self.saved)
    }

    pub fn is_claude_code(&self) -> bool {
        self.kind() == AgentKind::ClaudeCode
    }

    /// What the sidebar shows a running agent doing: what Claude Code
    /// reported, or Unknown for any other agent. None once its program
    /// is not running: then the program state is all there is to show.
    pub fn reported(&self) -> Option<AgentStatus> {
        self.is_running().then(|| self.track.shown.clone())
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

    /// What the conversation has used since Lillignode was started. None
    /// while its cost is not known, or when that cannot be told.
    pub fn cost_since_started(&self) -> Option<cost::Usage> {
        self.usage.as_ref().and_then(cost::Reader::since_started)
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

/// `text` with every control character shown as a space.
pub fn printable(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// What in a saved agent Lillignode would never have written itself, said
/// as the reason it is not started. None when the entry is as Lillignode
/// writes it. A saved entry is outside data: someone else may have written it.
fn not_as_written(saved: &SavedAgent) -> Option<String> {
    let control = |text: &str| text.chars().any(char::is_control);
    let what = if control(&saved.name) {
        "name"
    } else if control(&saved.command) {
        "command"
    } else if saved.args.iter().any(|a| control(a)) {
        "arguments"
    } else if control(&saved.folder.to_string_lossy()) {
        "folder"
    } else {
        return saved.conversation.as_deref().filter(|id| !is_conversation_id(id)).map(|_| {
            "its saved conversation id is not one Lillignode writes. It was left as written, and no new \
             conversation was begun in its place"
                .to_string()
        });
    };
    Some(format!(
        "its saved {what} holds a control character, which Lillignode never writes. Nothing was run"
    ))
}

/// A conversation id as Lillignode writes one: a UUID in its usual form.
fn is_conversation_id(id: &str) -> bool {
    uuid::Uuid::try_parse(id).is_ok_and(|u| u.hyphenated().to_string() == id)
}

/// The last part of a folder's path, which is what the user is shown.
fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string())
}

/// The kind of a saved agent: what was saved with it, or, for one saved
/// before kinds were recorded, what its command says.
fn kind_of(saved: &SavedAgent) -> AgentKind {
    AgentKind::of_saved(saved.kind.as_deref(), &saved.command)
}

/// False for an agent saved with a kind this Lillignode does not know. It
/// runs as a plain program, and says why.
fn kind_is_known(saved: &SavedAgent) -> bool {
    match saved.kind.as_deref() {
        Some(word) => AgentKind::from_saved(word).is_some(),
        None => true,
    }
}

/// Whether to begin a conversation or continue the saved one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conversation {
    New,
    Continue,
}

/// The arguments to start an agent with. An agent of a kind that keeps
/// conversations is told which one to use; anything else gets only what
/// was saved.
fn launch_args(saved: &SavedAgent, conversation: Conversation) -> Vec<String> {
    let mut args = saved.args.clone();
    if let Some(id) = &saved.conversation {
        args.extend(kind_of(saved).conversation_args(id, conversation == Conversation::Continue));
    }
    args
}

/// The terminal a saved agent runs in and the tracker of its status, both
/// new. Every agent's status is read from its own reports; its kind decides
/// what is shown while it has sent none. An agent saved with a kind this
/// Lillignode does not know says that is why, until it reports.
fn terminal_and_track(saved: &SavedAgent, size: Size) -> (Terminal, Track) {
    let source = if kind_is_known(saved) {
        kind_of(saved).status_source()
    } else {
        Box::new(Report::unexpected(Why::KindNotKnown))
    };
    (Terminal::new(size.rows, size.cols), Track::new(Instant::now(), source))
}

/// The reader of a saved agent's cost, for an agent that has one to read.
fn usage_reader(saved: &SavedAgent) -> Option<cost::Reader> {
    // The id names a file to read, so one Lillignode did not write is not used.
    let id = saved
        .conversation
        .as_deref()
        .filter(|id| kind_of(saved).has_cost_record() && is_conversation_id(id))?;
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

/// Two agents side by side. The agent in front is always one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub left: usize,
    pub right: usize,
}

/// Why two agents cannot be put side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoPair {
    /// How many agents are open: fewer than two.
    TooFewAgents(usize),
}

pub struct Workspace<B: AgentBackend> {
    backend: B,
    entries: Vec<Entry>,
    /// The key the next agent gets. Only ever counts up.
    next_key: u64,
    focused: usize,
    /// Set while the user has asked for two agents side by side. Whether
    /// the window has room for them is the caller's to decide.
    pair: Option<Pair>,
    size: Size,
    /// Cost is read only once the program says to. Until then no record is
    /// looked for or opened.
    cost_on: bool,
    /// Where Claude Code keeps its records, for reading cost. None until
    /// the program says, so nothing built for a test ever looks at a real one.
    claude_folder: Option<PathBuf>,
    /// The count at which a conversation's cost is marked and the user is
    /// told. A warning only: nothing is stopped or held.
    cost_warning: Option<u64>,
    /// Lines of the saved workspace this Lillignode does not know, saved
    /// back as they were read.
    saved_other: serde_json::Map<String, serde_json::Value>,
}

impl<B: AgentBackend> Workspace<B> {
    pub fn new(backend: B, size: Size) -> Self {
        Self {
            backend,
            entries: Vec::new(),
            next_key: 0,
            focused: 0,
            pair: None,
            size,
            cost_on: false,
            claude_folder: None,
            cost_warning: None,
            saved_other: serde_json::Map::new(),
        }
    }

    /// Switch the cost readout on and say where Claude Code keeps its
    /// records. With no folder, every cost is Unknown and says why. Until
    /// this is called nothing of Claude Code's is read.
    pub fn read_cost_from(&mut self, folder: Option<PathBuf>) {
        self.cost_on = true;
        self.claude_folder = folder;
    }

    pub fn cost_on(&self) -> bool {
        self.cost_on
    }

    /// The count at which a conversation is marked as over the user's
    /// warning. None for no warning.
    pub fn set_cost_warning(&mut self, at: Option<u64>) {
        self.cost_warning = at;
    }

    pub fn cost_warning(&self) -> Option<u64> {
        self.cost_warning
    }

    /// The count that put this agent over the warning, if it is over.
    pub fn over_cost_warning(&self, entry: &Entry) -> Option<u64> {
        cost::over(&entry.cost(), self.cost_warning)
    }

    /// Agents that have passed the warning since the last call, with the
    /// count each had reached. Each is named once for its conversation.
    pub fn take_passed_cost_warning(&mut self) -> Vec<(String, u64)> {
        let warn_at = self.cost_warning;
        let mut passed = Vec::new();
        for entry in self.entries.iter_mut().filter(|e| !e.cost_warned) {
            if let Some(count) = cost::over(&entry.cost(), warn_at) {
                entry.cost_warned = true;
                passed.push((entry.label(), count));
            }
        }
        passed
    }

    /// Every open agent's cost, added up, and how many are left out.
    pub fn cost_summary(&self) -> cost::Summary {
        let mut all = cost::Summary::new();
        for entry in &self.entries {
            all.count(&entry.cost(), entry.cost_since_started());
        }
        all
    }

    pub fn claude_folder(&self) -> Option<&Path> {
        self.claude_folder.as_deref()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Where the agent with this key is in the list now. None once it has
    /// been closed.
    pub fn index_of(&self, key: AgentKey) -> Option<usize> {
        self.entries.iter().position(|e| e.key == key)
    }

    pub fn by_key(&self, key: AgentKey) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    fn new_key(&mut self) -> AgentKey {
        let key = AgentKey(self.next_key);
        self.next_key += 1;
        key
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
        if let Some(why) = not_as_written(saved) {
            return (Status::NotStarted(why), None);
        }
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
    fn open(&mut self, key: AgentKey, mut saved: SavedAgent) -> (Entry, bool) {
        if saved.kind.is_none() {
            // Saved before kinds were recorded: work it out once. From now
            // on it is saved with the agent.
            let kind = AgentKind::of_command(&saved.command);
            saved.kind = Some(kind.saved_word().to_string());
            // Then, an agent that was not known by name was named as it
            // was typed ("codex", "codex 2"). It gets the name a new one of
            // its kind would, keeping its number.
            let number = saved
                .name
                .strip_prefix(&saved.command)
                .filter(|rest| rest.strip_prefix(' ').map_or(rest.is_empty(), |n| n.parse::<u32>().is_ok()));
            if let Some(number) = number {
                saved.name = format!("{}{number}", kind.name(&saved.command));
            }
        }
        let keeps_conversations = kind_of(&saved).keeps_conversations();
        let continuing = keeps_conversations && saved.conversation.is_some();
        if keeps_conversations && saved.conversation.is_none() {
            // Saved before conversations were tracked: begin tracking now.
            saved.conversation = Some(new_conversation_id());
        }
        let how = if continuing { Conversation::Continue } else { Conversation::New };
        let (status, id) = self.try_start(&saved, how);
        let resuming_since = (continuing && id.is_some()).then(Instant::now);
        let (term, track) = terminal_and_track(&saved, self.size);
        let entry = Entry {
            key,
            usage: usage_reader(&saved),
            saved,
            status,
            term,
            track,
            id,
            resuming_since,
            cost_warned: false,
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
            let key = self.new_key();
            let (entry, continuing) = self.open(key, agent);
            match &entry.status {
                Status::NotStarted(why) => report.not_restored.push((entry.label(), why.clone())),
                _ => report.restored.push(Restored { label: entry.label(), continuing }),
            }
            self.entries.push(entry);
        }
        // A focus index pointing past the list is stale, not fatal.
        self.focused = saved.focused.filter(|&i| i < self.entries.len()).unwrap_or(0);
        self.saved_other = saved.other;
        report
    }

    /// Start a new agent and put it in front. On failure nothing is added.
    pub fn start_new(&mut self, command: &str, folder: PathBuf) -> Result<(), StartError> {
        let kind = AgentKind::of_command(command);
        let saved = SavedAgent {
            name: self.distinct_name(&kind.name(command), &folder),
            command: command.to_string(),
            kind: Some(kind.saved_word().to_string()),
            args: Vec::new(),
            folder: folder.clone(),
            conversation: kind.keeps_conversations().then(new_conversation_id),
            other: Default::default(),
        };
        let spec =
            AgentSpec { command: command.to_string(), args: launch_args(&saved, Conversation::New), folder };
        let id = self.backend.start(&spec, self.size)?;
        let key = self.new_key();
        let (term, track) = terminal_and_track(&saved, self.size);
        self.entries.push(Entry {
            key,
            usage: usage_reader(&saved),
            saved,
            status: Status::Running,
            term,
            track,
            id: Some(id),
            resuming_since: None,
            cost_warned: false,
        });
        self.bring_to_front(self.entries.len() - 1);
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
        let conversation = entry.saved.conversation.clone();
        // Started again, it is still the same agent.
        let (mut entry, continuing) = self.open(entry.key, entry.saved.clone());
        if entry.saved.conversation == conversation {
            // The same conversation, so the same record: what it had used
            // when Lillignode started, and whether its warning was given,
            // are still true.
            let old = &mut self.entries[self.focused];
            entry.usage = old.usage.take();
            entry.cost_warned = old.cost_warned;
        }
        self.entries[self.focused] = entry;
        Some(continuing)
    }

    /// Stop the focused agent if it is running and remove it from the workspace.
    pub fn close_focused(&mut self) -> Option<String> {
        if self.focused >= self.entries.len() {
            return None;
        }
        let removed = self.focused;
        let entry = self.entries.remove(removed);
        if let Some(id) = entry.id {
            self.backend.close(id);
        }
        self.focused = self.focused.min(self.entries.len().saturating_sub(1));
        self.mend_pair(removed);
        Some(entry.label())
    }

    pub fn focus_next(&mut self) {
        if !self.entries.is_empty() {
            self.bring_to_front((self.focused + 1) % self.entries.len());
        }
    }

    pub fn focus_previous(&mut self) {
        if !self.entries.is_empty() {
            self.bring_to_front((self.focused + self.entries.len() - 1) % self.entries.len());
        }
    }

    /// Put agent `index` in front. With two agents side by side: one that
    /// is already shown is simply given the keyboard, where it is. Any
    /// other takes the place of the agent that was in front, and the agent
    /// beside it stays where it was.
    fn bring_to_front(&mut self, index: usize) {
        if let Some(pair) = &mut self.pair {
            if index != pair.left && index != pair.right {
                if pair.left == self.focused {
                    pair.left = index;
                } else {
                    pair.right = index;
                }
            }
        }
        self.focused = index;
    }

    /// The two agents shown side by side, if the user has asked for two.
    pub fn pair(&self) -> Option<Pair> {
        self.pair
    }

    /// Put the agent in front on the left and the next one beside it.
    /// Nothing is resized here: sizes are the caller's, with `resize`.
    pub fn show_two(&mut self) -> Result<Pair, NoPair> {
        let count = self.entries.len();
        if count < 2 {
            return Err(NoPair::TooFewAgents(count));
        }
        let pair = Pair { left: self.focused, right: (self.focused + 1) % count };
        self.pair = Some(pair);
        Ok(pair)
    }

    /// Back to one agent on screen: the one in front.
    pub fn show_one(&mut self) {
        self.pair = None;
    }

    /// After the agent at `removed` has been taken out of the list: keep
    /// the pair pointing at the same agents, give the empty side to an
    /// agent not already shown, and go back to one agent on screen when
    /// there are no longer two to show.
    fn mend_pair(&mut self, removed: usize) {
        let Some(mut pair) = self.pair else { return };
        let count = self.entries.len();
        if count < 2 {
            self.pair = None;
            return;
        }
        let kept = |side: usize| match side.cmp(&removed) {
            std::cmp::Ordering::Less => Some(side),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(side - 1),
        };
        // `focused` has already been moved to an agent that still exists.
        let spare = |taken: usize, focused: usize| {
            if focused != taken {
                focused
            } else {
                (taken + 1) % count
            }
        };
        match (kept(pair.left), kept(pair.right)) {
            (Some(left), Some(right)) => pair = Pair { left, right },
            (None, Some(right)) => pair = Pair { left: spare(right, self.focused), right },
            (Some(left), None) => pair = Pair { left, right: spare(left, self.focused) },
            // One agent was removed and the sides differ, so this cannot
            // happen. If it ever did, one agent on screen is the safe answer.
            (None, None) => {
                self.pair = None;
                return;
            }
        }
        // The agent in front is always one of the two.
        if self.focused != pair.left && self.focused != pair.right {
            self.focused = pair.left;
        }
        self.pair = Some(pair);
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
                        StatusMessage::Prompt => entry.track.prompt(),
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
                        entry.cost_warned = false;
                        (entry.term, entry.track) = terminal_and_track(&saved, self.size);
                        entry.saved = saved;
                        entry.status = Status::Running;
                        entry.id = id;
                        Some(Happened::StartedNewConversation { label })
                    }
                    Status::NotStarted(why) => Some(Happened::CouldNotContinue { label, why }),
                    Status::Exited(_) => unreachable!("try_start never reports an exit"),
                }
            }
        }
    }

    /// True if any agent changed into Needs you since the last call and
    /// may ring: the bell rings once for each such change, and an agent that
    /// changes over and over is held to a few (`Track::take_bell`).
    pub fn take_began_needing_you(&mut self) -> bool {
        let now = Instant::now();
        let mut any = false;
        for entry in self.entries.iter_mut().filter(|e| e.is_running()) {
            any |= entry.track.take_bell(now);
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
            if let (true, Some(reader)) = (self.cost_on, &mut entry.usage) {
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
            other: self.saved_other.clone(),
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
        let kind = AgentKind::of_command(command);
        SavedAgent {
            name: kind.name(command),
            command: command.into(),
            kind: None,
            args: vec![],
            folder: root(folder),
            conversation: kind.keeps_conversations().then(|| ID.to_string()),
            other: Default::default(),
        }
    }

    fn saved(agents: Vec<SavedAgent>, focused: Option<usize>) -> SavedWorkspace {
        SavedWorkspace { version: VERSION, agents, focused, other: Default::default() }
    }

    fn log(ws: &Workspace<Fake>) -> Vec<String> {
        ws.backend.log.borrow().clone()
    }

    #[test]
    fn restore_reports_what_came_back_and_what_did_not_and_keeps_every_entry() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let report = ws.restore(saved(
            vec![agent("claude", "api"), agent("aider", "web"), agent("claude", "gone")],
            Some(1),
        ));

        assert_eq!(report.restored, vec![Restored { label: "Claude Code (api)".into(), continuing: true }]);
        assert_eq!(report.not_restored.len(), 2);
        assert_eq!(report.not_restored[0].0, "aider (web)");
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
    fn unknown_lines_survive_restore_restart_and_save() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let mut a = agent("claude", "api");
        a.other.insert("colour".into(), "red".into());
        let mut saved_ws = saved(vec![a], Some(0));
        saved_ws.other.insert("team".into(), "web".into());
        ws.restore(saved_ws);
        // Started again after it stopped: still the same saved agent.
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 0 }));
        ws.restart_focused();
        let back = ws.to_saved();
        assert_eq!(back.other["team"], "web");
        assert_eq!(back.agents[0].other["colour"], "red");
        // A new agent has none.
        ws.start_new("claude", root("web")).unwrap();
        assert!(ws.to_saved().agents[1].other.is_empty());
    }

    #[test]
    fn restore_starts_nothing_lillignode_would_never_have_written_and_says_what() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let with = |change: fn(&mut SavedAgent)| {
            let mut a = agent("claude", "api");
            change(&mut a);
            a
        };
        let agents = vec![
            with(|a| a.name = "Claude\x1b[2J Code".into()),
            with(|a| a.command = "claude\r\nother".into()),
            with(|a| a.args = vec!["--x".into(), "a\x07b".into()]),
            with(|a| a.folder = root("a\u{9b}b")),
            with(|a| a.conversation = Some("not-an-id".into())),
            with(|a| a.conversation = Some("{11111111-2222-4333-8444-555555555555}".into())),
            agent("claude", "fine"),
        ];
        let report = ws.restore(saved(agents, Some(0)));
        let why: Vec<&str> = report.not_restored.iter().map(|(_, why)| why.as_str()).collect();
        assert_eq!(why.len(), 6, "{why:?}");
        for (i, what) in ["name", "command", "arguments", "folder"].iter().enumerate() {
            assert_eq!(
                why[i],
                format!(
                    "its saved {what} holds a control character, which Lillignode never writes. Nothing was run"
                )
            );
        }
        assert!(why[4].starts_with("its saved conversation id is not one Lillignode writes"), "{}", why[4]);
        assert!(why[5].starts_with("its saved conversation id"), "the braced form is not written either");
        assert_eq!(report.restored.len(), 1, "the good one still starts");
        assert_eq!(log(&ws).len(), 1, "nothing else was started");
        // Each stays in the workspace, as written: its id was not replaced.
        assert_eq!(ws.entries().len(), 7);
        assert_eq!(ws.entries()[4].saved.conversation.as_deref(), Some("not-an-id"));
        assert_eq!(ws.to_saved().agents[0].name, "Claude\x1b[2J Code");
        // What is shown of it cannot act on the screen.
        assert_eq!(ws.entries()[0].label(), "Claude [2J Code (api)");
        assert!(!ws.entries()[3].label().chars().any(char::is_control));
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

    /// A workspace with agents in folders a, b, c, d; `front` is in front.
    fn four(front: usize) -> Workspace<Fake> {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        for folder in ["a", "b", "c", "d"] {
            ws.start_new("claude", root(folder)).unwrap();
        }
        while ws.focused_index() != Some(front) {
            ws.focus_next();
        }
        ws
    }

    fn shown(ws: &Workspace<Fake>) -> (String, String, String) {
        let pair = ws.pair().expect("two are shown");
        let folder = |i: usize| ws.entries()[i].folder_name();
        (folder(pair.left), folder(pair.right), ws.focused().unwrap().folder_name())
    }

    #[test]
    fn two_side_by_side_takes_two_agents_and_starts_with_the_one_in_front_on_the_left() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        assert_eq!(ws.show_two(), Err(NoPair::TooFewAgents(0)));
        ws.start_new("claude", root("a")).unwrap();
        assert_eq!(ws.show_two(), Err(NoPair::TooFewAgents(1)));
        assert_eq!(ws.pair(), None, "a refusal changes nothing");

        let mut ws = four(1);
        assert_eq!(ws.show_two(), Ok(Pair { left: 1, right: 2 }));
        // The last agent pairs with the first: the list wraps.
        let mut last = four(3);
        assert_eq!(last.show_two(), Ok(Pair { left: 3, right: 0 }));
        ws.show_one();
        assert_eq!(ws.pair(), None);
        assert_eq!(ws.focused_index(), Some(1), "the agent in front stays in front");
    }

    #[test]
    fn with_two_shown_switching_moves_the_keyboard_or_replaces_only_the_side_in_front() {
        let mut ws = four(1);
        ws.show_two().unwrap();
        assert_eq!(shown(&ws), ("b".into(), "c".into(), "b".into()));
        // The next agent is already on screen: only the keyboard moves.
        ws.focus_next();
        assert_eq!(shown(&ws), ("b".into(), "c".into(), "c".into()));
        // The next is not on screen: it takes the side in front (the
        // right), and the agent on the left stays where it was.
        ws.focus_next();
        assert_eq!(shown(&ws), ("b".into(), "d".into(), "d".into()));
        ws.focus_next();
        assert_eq!(shown(&ws), ("b".into(), "a".into(), "a".into()));
        // Back to one already shown, then on to one that is not.
        ws.focus_next();
        assert_eq!(shown(&ws), ("b".into(), "a".into(), "b".into()));
        ws.focus_previous();
        assert_eq!(shown(&ws), ("b".into(), "a".into(), "a".into()));
        ws.focus_previous();
        assert_eq!(shown(&ws), ("b".into(), "d".into(), "d".into()));
        // A new agent takes the side in front too.
        ws.start_new("claude", root("e")).unwrap();
        assert_eq!(shown(&ws), ("b".into(), "e".into(), "e".into()));
    }

    #[test]
    fn closing_one_of_two_gives_its_side_to_another_agent_or_goes_back_to_one() {
        // Close the one in front, on the left. As with one agent on
        // screen, the next in the list comes to the front: here that is the
        // agent already on the right, which does not move. The empty side
        // goes to the next agent not already shown.
        let mut ws = four(1);
        ws.show_two().unwrap();
        ws.close_focused();
        assert_eq!(shown(&ws), ("d".into(), "c".into(), "c".into()));

        // Close the one in front, on the right, when it is last in the list.
        let mut ws = four(2);
        ws.show_two().unwrap();
        ws.focus_next();
        assert_eq!(shown(&ws), ("c".into(), "d".into(), "d".into()));
        ws.close_focused();
        assert_eq!(shown(&ws), ("c".into(), "a".into(), "c".into()));

        // Down to two agents, then one: back to one on screen, by itself.
        let mut ws = four(0);
        ws.show_two().unwrap();
        ws.close_focused();
        ws.close_focused();
        assert_eq!(ws.entries().len(), 2);
        let (left, right, front) = shown(&ws);
        assert_ne!(left, right, "never the same agent on both sides");
        assert!(front == left || front == right, "the agent in front is always one of the two");
        ws.close_focused();
        assert_eq!(ws.pair(), None);
        assert_eq!(ws.entries().len(), 1);
        ws.close_focused();
        assert_eq!(ws.pair(), None);
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
        assert!(ws.focused().unwrap().track.trace().last.is_none());
    }

    #[test]
    fn any_program_that_reports_gets_a_status_and_one_that_does_not_is_unknown() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        // Same backend, but an agent that is not Claude Code.
        let mut other = agent("claude", "a");
        other.kind = Some("program".into());
        ws.restore(saved(vec![other], Some(0)));
        // Nothing is expected of it: Unknown from the start, and no
        // deadline to pass.
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::NothingSent)));
        assert!(!ws.tick(Instant::now() + crate::status::STARTING_DEADLINE));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::NothingSent)));
        assert!(!ws.focused().unwrap().is_settled());
        // Its status question is answered, like any program's.
        assert_eq!(ask(&mut ws, 1), yes(1));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::NothingSent)));
        // What it reports is shown, and it rings.
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;state=blocked:app=terraform\x1b\\".to_vec()));
        assert_eq!(ws.focused().unwrap().needs_you(), Some(None));
        assert!(ws.take_began_needing_you());
        assert_eq!(ws.focused().unwrap().track.trace().app.as_deref(), Some("terraform"));
        // Its shell coming back to a prompt ends that.
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]133;A\x07".to_vec()));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::AtPrompt)));
        // It has no extras: no conversation and no cost.
        assert_eq!(ws.focused().unwrap().cost(), Cost::Unknown(cost::WhyNot::NotClaudeCode));
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
        let mut other = agent("claude", "b");
        other.kind = Some("program".into());
        ws.restore(saved(vec![agent("claude", "a"), other], Some(0)));
        assert_eq!(ws.entries[0].cost(), Cost::Pending(Waiting::NotLooked));
        assert_eq!(ws.entries[1].cost(), Cost::Unknown(WhyNot::NotClaudeCode));
        // The readout is off until the program switches it on: no look.
        assert!(!ws.cost_on());
        assert!(!ws.tick(Instant::now()));
        assert_eq!(ws.entries[0].cost(), Cost::Pending(Waiting::NotLooked));
        // On, with no folder named: nothing is looked at, and it says so.
        ws.read_cost_from(None);
        assert!(ws.tick(Instant::now()));
        assert_eq!(ws.entries[0].cost(), Cost::Unknown(WhyNot::NoFolder));

        let folder = claude_folder_with_record("read", ID);
        ws.read_cost_from(Some(folder.clone()));
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

    /// Add one more reply, of the same size, to the record made above.
    fn record_another(folder: &Path, id: &str, reply: &str) {
        use std::io::Write;
        let path = folder.join("projects").join("C--work-a").join(format!("{id}.jsonl"));
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(
            file,
            "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{reply}\",\"model\":\"claude-opus-5-5\",\
             \"usage\":{{\"input_tokens\":100,\"output_tokens\":50,\"cache_creation_input_tokens\":1000,\
             \"cache_read_input_tokens\":2000}}}}}}"
        )
        .unwrap();
    }

    #[test]
    fn since_started_counts_only_what_came_after_the_first_full_reading() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let folder = claude_folder_with_record("since", ID);
        ws.read_cost_from(Some(folder.clone()));
        ws.restore(saved(vec![agent("claude", "a")], Some(0)));
        assert_eq!(ws.entries[0].cost_since_started(), None, "not known before the first look");
        let now = Instant::now();
        ws.tick(now);
        // What was there before Lillignode started is not counted.
        let since = ws.entries[0].cost_since_started().unwrap();
        assert_eq!(since.total().total(), 0);
        record_another(&folder, ID, "msg_2");
        ws.tick(now + crate::cost::LOOK_EVERY);
        let since = ws.entries[0].cost_since_started().unwrap();
        assert_eq!((since.total().total(), since.total().headline()), (3150, 1150));
        let all = ws.cost_summary();
        assert_eq!((all.known, all.pending, all.unknown), (1, 0, 0));
        assert_eq!(all.usage.total().total(), 6300);
        assert_eq!(all.since_started.unwrap().total().total(), 3150);

        // A record swapped for another while Lillignode is open: there is
        // no honest starting point any more, so none is claimed.
        let path = folder.join("projects").join("C--work-a").join(format!("{ID}.jsonl"));
        std::fs::write(&path, "").unwrap();
        record_another(&folder, ID, "msg_9");
        ws.tick(now + crate::cost::LOOK_EVERY * 2);
        assert!(matches!(ws.entries[0].cost(), crate::cost::Cost::Known(_)));
        assert_eq!(ws.entries[0].cost_since_started(), None);
        assert_eq!(ws.cost_summary().since_started, None);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn passing_the_cost_warning_is_told_once_and_stops_nothing() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let folder = claude_folder_with_record("warn", ID);
        ws.read_cost_from(Some(folder.clone()));
        ws.restore(saved(vec![agent("claude", "a")], Some(0)));
        let now = Instant::now();
        ws.tick(now);
        // No threshold set: nothing is ever over.
        assert_eq!(ws.over_cost_warning(&ws.entries[0]), None);
        assert!(ws.take_passed_cost_warning().is_empty());
        // The count compared is the one shown: 1,150, without cache reads.
        ws.set_cost_warning(Some(1151));
        assert!(ws.take_passed_cost_warning().is_empty());
        record_another(&folder, ID, "msg_2");
        ws.tick(now + crate::cost::LOOK_EVERY);
        assert_eq!(ws.over_cost_warning(&ws.entries[0]), Some(2300));
        let passed = ws.take_passed_cost_warning();
        assert_eq!(passed, vec![(ws.entries[0].label(), 2300)]);
        assert!(ws.take_passed_cost_warning().is_empty(), "once");
        assert_eq!(ws.over_cost_warning(&ws.entries[0]), Some(2300), "the row stays marked");
        // A cost that is pending or unknown is never over: there is no
        // count to compare. And the agent still runs.
        use crate::cost::{Cost, Waiting, WhyNot};
        assert_eq!(cost::over(&Cost::Pending(Waiting::NoReplies), Some(1)), None);
        assert_eq!(cost::over(&Cost::Unknown(WhyNot::NoFolder), Some(1)), None);
        assert!(ws.entries[0].is_running());
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn starting_an_agent_again_keeps_what_its_conversation_had_used_and_its_warning() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let folder = claude_folder_with_record("again", ID);
        ws.read_cost_from(Some(folder.clone()));
        ws.set_cost_warning(Some(2000));
        ws.restore(saved(vec![agent("claude", "a")], Some(0)));
        let now = Instant::now();
        ws.tick(now);
        record_another(&folder, ID, "msg_2");
        ws.tick(now + crate::cost::LOOK_EVERY);
        assert_eq!(ws.take_passed_cost_warning().len(), 1);
        // It exits cleanly and is started again: the same conversation.
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 0 }));
        assert_eq!(ws.restart_focused(), Some(true));
        ws.tick(now + crate::cost::LOOK_EVERY * 2);
        let since = ws.entries[0].cost_since_started().unwrap();
        assert_eq!(since.total().total(), 3150, "what was used before the restart still counts");
        assert!(ws.take_passed_cost_warning().is_empty(), "and its warning is not given twice");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_new_conversation_starts_its_cost_afresh() {
        use crate::cost::Cost;
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let folder = claude_folder_with_record("fresh", ID);
        ws.read_cost_from(Some(folder.clone()));
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

    #[test]
    fn an_agents_key_outlives_closing_another_and_starting_it_again() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(
            vec![agent("claude", "api"), agent("claude", "web"), agent("claude", "docs")],
            Some(0),
        ));
        let keys: Vec<AgentKey> = ws.entries().iter().map(Entry::key).collect();
        assert!(keys[0] != keys[1] && keys[1] != keys[2] && keys[0] != keys[2]);

        // Closing the first moves the others up the list. Their keys stay.
        assert_eq!(ws.close_focused().as_deref(), Some("Claude Code (api)"));
        assert_eq!(ws.index_of(keys[0]), None);
        assert!(ws.by_key(keys[0]).is_none());
        assert_eq!(ws.index_of(keys[1]), Some(0));
        assert_eq!(ws.index_of(keys[2]), Some(1));
        assert_eq!(ws.by_key(keys[2]).unwrap().label(), "Claude Code (docs)");

        // The agent in front stops and is started again: the same agent.
        let id = ws.focused().unwrap().id.unwrap();
        ws.handle(AgentEvent::Exited(id, Exit { code: 0 }));
        assert_eq!(ws.restart_focused(), Some(true));
        assert_eq!(ws.focused().unwrap().key(), keys[1]);

        // A new agent never gets a key that was used before, the closed one's included.
        ws.start_new("claude", root("new")).unwrap();
        let new = ws.focused().unwrap().key();
        assert!(!keys.contains(&new));
    }

    #[test]
    fn an_agent_saved_without_a_kind_gets_one_from_its_command_and_it_is_saved() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "api"), agent("aider", "web")], Some(0)));
        assert_eq!(ws.entries()[0].kind(), AgentKind::ClaudeCode);
        assert_eq!(ws.entries()[1].kind(), AgentKind::Program);
        // Recorded for the one that could not be started too.
        let kinds =
            |ws: &Workspace<Fake>| ws.to_saved().agents.into_iter().map(|a| a.kind).collect::<Vec<_>>();
        let both = [Some("claude-code".to_string()), Some("program".to_string())];
        assert_eq!(kinds(&ws), both);
        // Nothing else about it changed: it was asked to continue, as before.
        assert_eq!(log(&ws), [format!("start 1 --resume {ID}")]);

        // It stops at once with an error, so it is given a new conversation:
        // still Claude Code, and told the new id the way Claude Code is.
        assert_eq!(
            ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 1 })),
            Some(Happened::StartedNewConversation { label: "Claude Code (api)".into() })
        );
        assert_eq!(kinds(&ws), both);
        let new_id = ws.focused().unwrap().saved.conversation.clone().unwrap();
        assert_eq!(log(&ws).last().unwrap(), &format!("start 2 --session-id {new_id}"));

        // Stopped and started again: the same kind, continuing by its id.
        ws.handle(AgentEvent::Exited(AgentId(2), Exit { code: 0 }));
        assert_eq!(ws.restart_focused(), Some(true));
        assert_eq!(kinds(&ws), both);
        assert_eq!(log(&ws).last().unwrap(), &format!("start 3 --resume {new_id}"));
    }

    #[test]
    fn a_new_agent_is_saved_with_its_kind() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.start_new("claude", root("api")).unwrap();
        assert_eq!(ws.to_saved().agents[0].kind.as_deref(), Some("claude-code"));
    }

    #[test]
    fn the_saved_kind_decides_not_the_name_of_the_program() {
        // A program named claude that was saved as a plain program is one:
        // no conversation is passed, and none is made up for it.
        let mut plain = agent("claude", "api");
        plain.kind = Some("program".into());
        plain.conversation = None;
        // The same, but with a conversation saved beside it: it is not
        // passed on, not read for cost, and not thrown away.
        let mut with_id = agent("claude", "web");
        with_id.kind = Some("program".into());
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let report = ws.restore(saved(vec![plain, with_id], Some(0)));
        assert_eq!(
            report.restored,
            vec![
                Restored { label: "Claude Code (api)".into(), continuing: false },
                Restored { label: "Claude Code (web)".into(), continuing: false },
            ]
        );
        assert_eq!(log(&ws), ["start 1 ", "start 2 "]);
        assert!(ws.entries()[1].usage.is_none());
        assert_eq!(ws.entries()[1].cost(), Cost::Unknown(cost::WhyNot::NotClaudeCode));
        assert_eq!(ws.to_saved().agents[1].conversation.as_deref(), Some(ID));
        let entry = ws.focused().unwrap();
        assert!(!entry.is_claude_code());
        assert_eq!(entry.saved.conversation, None);
        assert_eq!(entry.reported(), Some(AgentStatus::Unknown(Why::NothingSent)));
        assert_eq!(entry.cost(), Cost::Unknown(cost::WhyNot::NotClaudeCode));
    }

    #[test]
    fn a_kind_from_a_newer_lillignode_runs_as_a_plain_program_and_is_kept() {
        let mut newer = agent("claude", "api");
        newer.kind = Some("something-newer".into());
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let report = ws.restore(saved(vec![newer], Some(0)));

        // Started, with nothing added to its arguments and not asked to
        // continue: this Lillignode does not know how that kind does it.
        assert_eq!(report.restored, vec![Restored { label: "Claude Code (api)".into(), continuing: false }]);
        assert_eq!(log(&ws), ["start 1 "]);
        let entry = ws.focused().unwrap();
        assert_eq!(entry.kind(), AgentKind::Program);
        // It says why its status is not shown, and it is not the usual reason.
        assert_eq!(entry.reported(), Some(AgentStatus::Unknown(Why::KindNotKnown)));
        // Its cost is not read either.
        assert!(entry.usage.is_none());
        assert_eq!(entry.cost(), Cost::Unknown(cost::WhyNot::NotClaudeCode));
        // What the newer Lillignode saved is still there for it to read.
        let kept = &ws.to_saved().agents[0];
        assert_eq!(kept.kind.as_deref(), Some("something-newer"));
        assert_eq!(kept.conversation.as_deref(), Some(ID));
    }

    /// Have agent `id` ask the status question; what was sent back to it.
    fn ask(ws: &mut Workspace<Fake>, id: u32) -> Vec<String> {
        let before = log(ws).len();
        ws.handle(AgentEvent::Output(AgentId(id), b"\x1b]7501;?\x07".to_vec()));
        log(ws)[before..].to_vec()
    }

    fn yes(id: u32) -> Vec<String> {
        vec![format!("send {id} {:?}", "\x1b]7501;?\x07")]
    }

    #[test]
    fn claude_code_is_answered_and_read_however_it_was_started() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "a")], Some(0)));
        assert_eq!(ask(&mut ws, 1), yes(1));

        // It cannot continue, so it is started with a new conversation.
        ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 1 }));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Starting));
        assert_eq!(ask(&mut ws, 2), yes(2));
        ws.handle(AgentEvent::Output(AgentId(2), b"\x1b]7501;state=idle:app=claude-code\x07".to_vec()));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Idle));

        // It exits and the user starts it again.
        ws.handle(AgentEvent::Exited(AgentId(2), Exit { code: 0 }));
        ws.restart_focused();
        assert_eq!(ask(&mut ws, 3), yes(3));

        // A new size is the same terminal, still answering.
        ws.resize(Size { rows: 30, cols: 100 });
        assert_eq!(ask(&mut ws, 3), yes(3));

        // And a new one, started by the user.
        ws.start_new("claude", root("b")).unwrap();
        assert_eq!(ask(&mut ws, 4), yes(4));
    }

    #[test]
    fn a_silent_claude_code_agent_says_which_silence_it_was() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![agent("claude", "a"), agent("claude", "b")], Some(0)));
        // The second asks and is answered; neither reports.
        assert_eq!(ask(&mut ws, 2), yes(2));
        assert!(ws.tick(Instant::now() + crate::status::STARTING_DEADLINE));
        assert_eq!(ws.entries()[0].reported(), Some(AgentStatus::Unknown(Why::NotAsked)));
        assert_eq!(ws.entries()[1].reported(), Some(AgentStatus::Unknown(Why::NoReport)));
    }

    #[test]
    fn an_agent_of_a_kind_not_known_says_so_until_it_reports() {
        let mut newer = agent("claude", "api");
        newer.kind = Some("something-newer".into());
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![newer], Some(0)));
        assert_eq!(ask(&mut ws, 1), yes(1));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::KindNotKnown)));
        assert!(!ws.tick(Instant::now() + crate::status::STARTING_DEADLINE));
        ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;state=blocked:app=claude-code\x07".to_vec()));
        let entry = ws.focused().unwrap();
        assert_eq!(entry.needs_you(), Some(None));
        assert_eq!(entry.track.trace().reports, 1);
        assert!(ws.take_began_needing_you());
    }

    /// A Codex agent that the fake backend will start: it starts only
    /// programs named claude, so this one is Codex by its saved kind.
    fn codex_agent(folder: &str) -> SavedAgent {
        SavedAgent {
            name: "Codex".into(),
            command: "claude".into(),
            kind: Some("codex".into()),
            args: vec![],
            folder: root(folder),
            conversation: None,
            other: Default::default(),
        }
    }

    fn codex_says(ws: &mut Workspace<Fake>, id: u32, body: &str) {
        // Codex ends its reports this way.
        ws.handle(AgentEvent::Output(AgentId(id), format!("\x1b]7501;{body}\x1b\\").into_bytes()));
    }

    #[test]
    fn a_program_named_codex_is_codex_and_is_saved_as_it() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        // The fake backend does not start it, which is enough to see what it is.
        ws.restore(saved(vec![agent("codex", "web")], Some(0)));
        let entry = ws.focused().unwrap();
        assert_eq!(entry.kind(), AgentKind::Codex);
        assert_eq!(entry.label(), "Codex (web)");
        let kept = &ws.to_saved().agents[0];
        assert_eq!(kept.kind.as_deref(), Some("codex"));
        // No conversation id is made up for it.
        assert_eq!(kept.conversation, None);
    }

    #[test]
    fn codex_shows_what_it_reports_without_being_answered_and_rings_once() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        let report = ws.restore(saved(vec![codex_agent("web")], Some(0)));
        // Started as typed: nothing added, and not asked to continue.
        assert_eq!(report.restored, vec![Restored { label: "Codex (web)".into(), continuing: false }]);
        assert_eq!(log(&ws), ["start 1 "]);
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Starting));

        codex_says(&mut ws, 1, "state=idle:app=codex");
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Idle));
        assert!(ws.focused().unwrap().is_settled());
        codex_says(&mut ws, 1, "state=working:app=codex");
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Working));
        assert!(!ws.focused().unwrap().is_settled());
        assert!(!ws.take_began_needing_you());
        codex_says(&mut ws, 1, "state=blocked:app=codex:kind=permission");
        assert_eq!(ws.focused().unwrap().needs_you(), Some(Some(Kind::Permission)));
        assert!(ws.take_began_needing_you());
        assert!(!ws.take_began_needing_you());
        codex_says(&mut ws, 1, "state=done:app=codex");
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Done));

        // Nothing was sent back to it: it asked nothing. If it ever asks,
        // it is answered like any program.
        assert_eq!(log(&ws), ["start 1 "]);
        assert_eq!(ask(&mut ws, 1), yes(1));
        // Its cost is not read, and says so; it is never zero or pending.
        assert_eq!(ws.focused().unwrap().cost(), Cost::Unknown(cost::WhyNot::NotClaudeCode));
    }

    #[test]
    fn codex_started_again_begins_a_new_conversation_and_is_read_again() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![codex_agent("web")], Some(0)));
        codex_says(&mut ws, 1, "state=working:app=codex");
        // It stops with an error straight away. That is not a failed
        // continue, because it was never asked to continue.
        assert_eq!(
            ws.handle(AgentEvent::Exited(AgentId(1), Exit { code: 1 })),
            Some(Happened::Exited { label: "Codex (web)".into(), exit: Exit { code: 1 } })
        );
        assert_eq!(ws.focused().unwrap().reported(), None);
        assert_eq!(ws.restart_focused(), Some(false));
        assert_eq!(log(&ws).last().unwrap(), "start 2 ");
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Starting));
        codex_says(&mut ws, 2, "state=idle:app=codex");
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Idle));
        assert_eq!(ws.to_saved().agents[0].conversation, None);
    }

    #[test]
    fn a_report_under_another_programs_name_is_shown_and_the_extras_follow_the_kind() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![codex_agent("web"), agent("claude", "api")], Some(0)));
        codex_says(&mut ws, 1, "state=blocked:app=claude-code");
        assert_eq!(ws.entries()[0].needs_you(), Some(None));
        assert_eq!(ws.entries()[0].track.trace().app.as_deref(), Some("claude-code"));
        ws.handle(AgentEvent::Output(AgentId(2), b"\x1b]7501;state=done:app=codex\x07".to_vec()));
        assert_eq!(ws.entries()[1].reported(), Some(AgentStatus::Done));
        assert!(ws.take_began_needing_you());
        // Who reported changes nothing else: the agent started as Codex
        // still has no conversation or cost, and the other keeps both.
        assert_eq!(ws.entries()[0].kind(), AgentKind::Codex);
        assert_eq!(ws.entries()[0].cost(), Cost::Unknown(cost::WhyNot::NotClaudeCode));
        assert!(ws.entries()[1].is_claude_code());
        assert!(ws.to_saved().agents[1].conversation.is_some());
        assert_eq!(ws.to_saved().agents[0].conversation, None);
    }

    #[test]
    fn a_silent_codex_agent_is_unknown_for_no_report_until_it_reports() {
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![codex_agent("web")], Some(0)));
        assert!(ws.tick(Instant::now() + crate::status::STARTING_DEADLINE));
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Unknown(Why::NoReport)));
        assert!(!ws.focused().unwrap().is_settled(), "quit asks about it");
        codex_says(&mut ws, 1, "state=idle:app=codex");
        assert_eq!(ws.focused().unwrap().reported(), Some(AgentStatus::Idle));
    }

    #[test]
    fn a_codex_agent_saved_before_kinds_were_recorded_is_named_as_a_new_one_would_be() {
        // As the released program saved them: named as typed, numbered, no kind.
        let old = |name: &str| SavedAgent { name: name.into(), ..agent("codex", "web") };
        // Only a name that is the command, or the command and a number.
        let mut other = old("codexplorer");
        other.folder = root("docs");
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![old("codex"), old("codex 2"), other, agent("claude", "api")], Some(0)));
        let names: Vec<_> = ws.entries().iter().map(|e| e.label()).collect();
        assert_eq!(names, ["Codex (web)", "Codex 2 (web)", "codexplorer (docs)", "Claude Code (api)"]);
        // A new one beside them is numbered after them, not given a twin's name.
        assert!(ws.start_new("codex", root("web")).is_err(), "the fake backend starts only claude");
        // Once the kind is saved the name is left alone, whatever it is.
        let again = ws.to_saved();
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(again.clone());
        assert_eq!(ws.to_saved().agents, again.agents);
    }

    #[test]
    fn a_program_named_codex_that_was_saved_as_a_plain_program_stays_one() {
        let mut plain = agent("codex", "web");
        plain.name = "codex".into();
        plain.kind = Some("program".into());
        let mut ws = Workspace::new(Fake::default(), SIZE);
        ws.restore(saved(vec![plain], Some(0)));
        let entry = ws.focused().unwrap();
        assert_eq!(entry.kind(), AgentKind::Program);
        assert_eq!(entry.label(), "codex (web)");
        assert_eq!(ws.to_saved().agents[0].kind.as_deref(), Some("program"));
    }
}
