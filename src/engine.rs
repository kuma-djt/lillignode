//! The engine: the agents and the rules of the product, apart from any
//! screen or keyboard. A face reads what it holds and asks it, by name, to
//! do things.
//!
//! The rules are moving here from the terminal face (`app.rs`) one at a
//! time; until each has moved, the face still holds it. Quitting, closing
//! an agent, starting one again, the paste rule, the rules of the view
//! (one agent on screen or two, and what size each is given), saving the
//! workspace after a change, and what is said once in a run (an agent began
//! to need you, status or cost cannot be read, a cost warning was passed)
//! have moved.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::agent::{AgentBackend, AgentEvent, Exit, Size, StartError};
use crate::cost::{Cost, WhyNot};
use crate::kind::AgentKind;
use crate::snapshot::{AgentCopy, EventCopy, Published, StateCopy};
use crate::state::{self, SavedWorkspace};
use crate::status::{AgentStatus, Why};
use crate::workspace::{AgentKey, Entry, Happened, NoPair, Pair, RestoreReport, Workspace};

/// Something that happened, for a face to say in its own words. Events are
/// numbered from 1 in the order they happened, and the last few hundred are
/// kept (`EVENTS_KEPT`). The terminal face takes each once
/// (`Engine::take_events`); any other reader asks for everything after the
/// number it last saw (`Engine::events_after`) and is told how many it
/// missed. They are hints: whatever can be read from the engine at any time
/// (a status, who needs you, a cost) is not an event, and no rule depends
/// on a reader having seen one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An agent's program ended.
    Exited { agent: AgentKey, label: String, exit: Exit },
    /// An agent could not continue its earlier conversation and was started
    /// again with a new one.
    StartedNewConversation { agent: AgentKey, label: String },
    /// As above, but the fresh start failed too.
    CouldNotContinue { agent: AgentKey, label: String, why: String },
    /// The workspace could not be saved after a change. What was saved
    /// before is left as it was; nothing in memory is lost.
    SaveFailed { why: String },
    /// The workspace was saved after a change, and the save before it had
    /// failed: what was said then is no longer true.
    SavedAgain,
    /// This agent has just begun to need the user: the edge a bell rings
    /// on. Whether to ring is the face's setting. Who needs you now is read
    /// from the agents, not from this.
    BeganNeedingYou { agent: AgentKey, label: String },
    /// The status of an agent of this kind, which is expected to report it,
    /// is not reaching Lillignode. Once for each kind, until it is back.
    StatusUnavailable { kind: AgentKind, why: Why },
    /// No agent of this kind shows that any more: what was said about it
    /// has stopped being true. If it fails again it is said again.
    StatusBack { kind: AgentKind },
    /// The cost of this agent was looked for and could not be read. Once in
    /// a run.
    CostUnreadable { agent: AgentKey, label: String },
    /// This agent's conversation has passed the user's cost warning, at
    /// this many tokens. Once for a conversation. A warning only: nothing
    /// was stopped.
    PassedCostWarning { agent: AgentKey, label: String, tokens: u64 },
    /// Text typed on the page was sent to this agent, and Enter after it
    /// (`entered`; false only if the agent stopped between the two). Every
    /// such send, each time.
    TypedFromPage { agent: AgentKey, label: String, entered: bool },
    /// The page pressed Enter in this agent, and nothing else. Every such
    /// press, each time.
    EnterFromPage { agent: AgentKey, label: String },
    /// Something was done to a card of the Board on the page, or what was
    /// done was undone. `action` is the word the outbox has for it. Every
    /// one, each time.
    CardActed { card: String, title: String, action: &'static str, undone: bool },
}

/// How many of the latest events are kept for a reader that asks late.
pub const EVENTS_KEPT: usize = 512;

/// What a reader that asked for everything after a number is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventsAfter {
    /// How many events after that number are no longer kept. More than
    /// none: the reader has missed them for good, and reads the state again.
    pub missed: u64,
    /// Those still kept, oldest first, each with its number.
    pub events: Vec<(u64, Event)>,
    /// The number of the last event that has happened. Less than the number
    /// asked after: that number is not from this run.
    pub last: u64,
}

impl Event {
    /// The event said plainly, with its number: a fixed word for what
    /// happened, and the key, name and folder of the agent it happened to,
    /// if to one.
    pub fn plain(&self, number: u64) -> EventCopy {
        let (what, agent, label) = match self {
            Event::Exited { agent, label, .. } => ("exited", Some(*agent), Some(label)),
            Event::StartedNewConversation { agent, label } => {
                ("started-new-conversation", Some(*agent), Some(label))
            }
            Event::CouldNotContinue { agent, label, .. } => ("could-not-continue", Some(*agent), Some(label)),
            Event::SaveFailed { .. } => ("save-failed", None, None),
            Event::SavedAgain => ("saved-again", None, None),
            Event::BeganNeedingYou { agent, label } => ("began-needing-you", Some(*agent), Some(label)),
            Event::StatusUnavailable { .. } => ("status-unavailable", None, None),
            Event::StatusBack { .. } => ("status-back", None, None),
            Event::CostUnreadable { agent, label } => ("cost-unreadable", Some(*agent), Some(label)),
            Event::PassedCostWarning { agent, label, .. } => {
                ("passed-cost-warning", Some(*agent), Some(label))
            }
            Event::TypedFromPage { agent, label, .. } => ("typed-from-page", Some(*agent), Some(label)),
            Event::EnterFromPage { agent, label } => ("enter-from-page", Some(*agent), Some(label)),
            // Not about an agent: the card's id stands where a label would.
            Event::CardActed { card, undone: false, .. } => ("card-acted", None, Some(card)),
            Event::CardActed { card, undone: true, .. } => ("card-action-undone", None, Some(card)),
        };
        EventCopy { number, what, agent, label: label.cloned() }
    }
}

/// Names one question the engine asked. An answer carries it, so a "yes"
/// is never applied to a question the asker did not see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuestionId(u64);

/// What a question is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum About {
    /// Quit, although an agent may be in the middle of something.
    Quit,
    /// Close this agent: stop it if it runs and take it out of the
    /// workspace, with its conversation.
    Close(AgentKey),
    /// Send the held paste to this agent line by line: it does not take a
    /// paste as one block, so each line is entered as if Enter were
    /// pressed after it. How many lines it has is `Engine::held_paste_lines`.
    Paste(AgentKey),
    /// Change the view, to two side by side (`two`) or to one, although an
    /// agent is waiting for the user: every agent is told a new size and
    /// redraws.
    View { two: bool },
}

/// The room agents have, as the face measures it. The engine does not know
/// widths; it is told the size each agent gets with one on screen, and with
/// two side by side, or that two do not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    pub one: Size,
    /// None: two do not fit.
    pub two: Option<Size>,
}

/// What a change of view did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Viewed {
    /// One agent on screen. Every agent was told its new size.
    One,
    /// Two were asked for but did not fit, so one was on screen already.
    /// Now two will not come back when there is room. Nothing was resized.
    OneKept,
    /// These two are side by side. Every agent was told its new size.
    Two(Pair),
}

/// What a request to change the view came to.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewOutcome {
    Done(Viewed),
    /// An agent is waiting for the user: this question is open first.
    Asked(Question),
}

/// What new room did to the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomChange {
    Same,
    /// Two were side by side and no longer fit: only the one in front is
    /// shown. The other was not stopped or closed.
    TwoGone,
    /// Two were asked for and now fit again: they are side by side.
    TwoBack,
}

/// What a tab becomes in a paste sent as typed keys, so none is taken as a
/// press of Tab.
const TAB_AS_SPACES: &str = "    ";

/// A paste waiting for the answer to the question about it.
struct HeldPaste {
    /// Tabs already turned into spaces.
    text: String,
    left_out: Vec<char>,
    tabs: bool,
}

/// What became of a paste that was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The agent's name and folder.
    pub label: String,
    /// The control characters taken out of it before it came, said back.
    pub left_out: Vec<char>,
    /// Its tabs were sent as spaces.
    pub tabs: bool,
    /// How it went.
    pub how: SentAs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentAs {
    /// As one marked paste, which the agent takes whole.
    Whole,
    /// As typed keys, on one line.
    Typed,
    /// Line by line, after the user agreed.
    LineByLine,
}

/// What a paste came to.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasteOutcome {
    Sent(Sent),
    /// Nothing was left once control characters were taken out: these.
    /// Nothing was sent.
    OnlyControls(Vec<char>),
    /// It has more than one line and the agent does not take a paste
    /// whole: this question is open, and the paste is held until it is
    /// answered.
    Asked(Question),
}

/// What text typed on the page came to.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageTyped {
    /// It was sent as `sent` says. `entered`: Enter was sent after it;
    /// false only if the agent stopped between the two.
    Sent { sent: Sent, entered: bool },
    /// Nothing was left once control characters were taken out: these.
    /// Nothing was sent.
    OnlyControls(Vec<char>),
    /// It has this many lines and the agent does not take a paste whole,
    /// so it would go line by line. Nothing was sent and nothing is held:
    /// it is sent again once the page's reader has agreed.
    MustAgree { lines: usize, label: String },
}

/// A question the engine has asked and is waiting for an answer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: QuestionId,
    pub about: About,
}

/// What became of something asked of the engine. A question that is
/// asked must be shown: it stays open until it is answered.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It was done.
    Done,
    /// Nothing was done yet: this question is open, and its answer decides.
    Asked(Question),
}

/// Why the engine did not do what it was asked. Nothing was changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The answer is for a question that is not open, or not this one.
    NoSuchQuestion,
    /// Another question is open. One question at a time: it is answered
    /// first.
    QuestionOpen,
    /// The agent is gone: closed, or never there.
    NoSuchAgent,
    /// The agent is running, so it cannot be started again.
    AlreadyRunning,
    /// The agent is not running, so nothing can be sent to it.
    AgentStopped,
    /// Two side by side takes two agents; this many are open.
    TooFewAgents(usize),
    /// The face said two do not fit.
    TwoDoNotFit,
}

/// What an answer did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answered {
    /// No: nothing was done.
    Nothing,
    /// Yes to quitting: every agent is to be stopped.
    Closing,
    /// Yes to closing: the agent with this label was closed.
    Closed { label: String },
    /// Yes to closing, but the agent was already gone. Nothing was done.
    AlreadyGone,
    /// Yes to a held paste: it was sent line by line.
    Pasted(Sent),
    /// Yes to a held paste, but its agent had stopped or gone. The paste
    /// was dropped and nothing was sent.
    PasteDropped,
    /// Yes to changing the view: it changed.
    Viewed(Viewed),
    /// Yes to changing the view, but by then it could not be changed.
    /// Nothing was changed.
    ViewRefused(Refused),
}

/// A running agent that quitting would stop in the middle of something,
/// as far as Lillignode can know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unfinished {
    /// Its name and folder, as the sidebar shows them.
    pub label: String,
    /// What it last said, or None before it has said anything.
    pub status: Option<AgentStatus>,
    /// It is waiting for the user.
    pub needs_you: bool,
}

pub struct Engine<B: AgentBackend> {
    /// The open agents and what became of each.
    pub ws: Workspace<B>,
    /// At most one question is open.
    question: Option<Question>,
    /// Questions asked so far in this run; the next id.
    asked: u64,
    /// The user has decided to quit: every agent is to be stopped and the
    /// face closed.
    closing: bool,
    /// The paste the open question is about, if it is about one.
    held: Option<HeldPaste>,
    /// The room agents have, as last told by the face.
    room: Room,
    /// The folder the workspace is saved in. None: there is nowhere to
    /// save, and nothing is.
    save_in: Option<PathBuf>,
    /// The latest events, each with its number, oldest first. Never more
    /// than `EVENTS_KEPT`.
    events: VecDeque<(u64, Event)>,
    /// The number of the last event. Only ever counts up.
    happened: u64,
    /// The number of the last event the terminal face took.
    taken: u64,
    /// Tells this run from any other, for a reader that outlives one.
    run: u64,
    /// The kinds of agent whose status has been said not to reach
    /// Lillignode, and has not come back since.
    status_said: Vec<AgentKind>,
    /// A cost that could not be read has been said in this run.
    cost_said: bool,
    /// The last save after a change failed.
    save_failing: bool,
}

impl<B: AgentBackend> Engine<B> {
    /// The workspace's agents keep the size they have until the face tells
    /// the engine the room (`set_room`).
    pub fn new(ws: Workspace<B>) -> Self {
        let room = Room { one: ws.size(), two: None };
        Self {
            ws,
            question: None,
            asked: 0,
            closing: false,
            held: None,
            room,
            save_in: None,
            events: VecDeque::new(),
            happened: 0,
            taken: 0,
            run: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64),
            status_said: Vec::new(),
            cost_said: false,
            save_failing: false,
        }
    }

    /// Call after each batch of agent output and each tick. What has become
    /// true since the last call, and is said once, becomes an event, in
    /// this order: an agent began to need you; status that was said to be
    /// unavailable is back; status is unavailable, for one kind of agent at
    /// a time; a cost could not be read; a cost warning was passed.
    pub fn settle(&mut self) {
        let mut said = Vec::new();
        for (agent, label) in self.ws.take_began_needing_you_each() {
            said.push(Event::BeganNeedingYou { agent, label });
        }
        let broken = |e: &Entry| match e.reported() {
            Some(AgentStatus::Unknown(why)) if why.is_breakage() => Some(why),
            _ => None,
        };
        // Once no agent of a kind shows it (it reported after all, or was
        // closed), what was said about that kind is taken back.
        let entries = self.ws.entries();
        let (still, back): (Vec<_>, Vec<_>) = std::mem::take(&mut self.status_said)
            .into_iter()
            .partition(|kind| entries.iter().any(|e| e.kind() == *kind && broken(e).is_some()));
        self.status_said = still;
        said.extend(back.into_iter().map(|kind| Event::StatusBack { kind }));
        // Said once for each kind of agent: one kind's reporting failing
        // says nothing about another's. Only of a kind expected to report.
        let unavailable = entries.iter().find_map(|e| {
            let kind = e.kind();
            kind.reporter()?;
            let why = broken(e)?;
            (!self.status_said.contains(&kind)).then_some((kind, why))
        });
        if let Some((kind, why)) = unavailable {
            self.status_said.push(kind);
            said.push(Event::StatusUnavailable { kind, why });
        }
        if !self.cost_said {
            // A cost Lillignode tried to read and could not. Pending, and an
            // agent that is not Claude Code, are not failures.
            let failed = entries.iter().find(|e| {
                matches!(
                    e.cost(),
                    Cost::Unknown(WhyNot::NotUnderstood { .. } | WhyNot::CannotRead(_) | WhyNot::NoFolder)
                )
            });
            if let Some(entry) = failed {
                self.cost_said = true;
                said.push(Event::CostUnreadable { agent: entry.key(), label: entry.label() });
            }
        }
        for (agent, label, tokens) in self.ws.take_passed_cost_warning() {
            said.push(Event::PassedCostWarning { agent, label, tokens });
        }
        for event in said {
            self.say(event);
        }
    }

    /// Something happened: it gets the next number and is kept, and the
    /// oldest kept is let go when there are too many.
    fn say(&mut self, event: Event) {
        self.happened += 1;
        self.events.push_back((self.happened, event));
        if self.events.len() > EVENTS_KEPT {
            self.events.pop_front();
        }
    }

    /// The number of the last event that has happened. None yet: 0.
    pub fn last_event(&self) -> u64 {
        self.happened
    }

    /// Every event after number `after` that is still kept, oldest first,
    /// and how many after it are not. Takes nothing: asked again with the
    /// same number, the answer is the same or longer.
    pub fn events_after(&self, after: u64) -> EventsAfter {
        let oldest_kept = self.events.front().map_or(self.happened + 1, |(number, _)| *number);
        EventsAfter {
            missed: oldest_kept.saturating_sub(after + 1),
            events: self.events.iter().filter(|(number, _)| *number > after).cloned().collect(),
            last: self.happened,
        }
    }

    /// A plain copy of what the engine holds, as of `now`: every agent,
    /// which is in front, which two are on screen, whether a question is
    /// open, and cost. It carries the number of the last event, so a
    /// reader can tell how far it has read.
    pub fn copy(&self, now: SystemTime) -> StateCopy {
        let entries = self.ws.entries();
        let agents = entries
            .iter()
            .map(|e| AgentCopy {
                key: e.key(),
                name: e.name(),
                folder: e.saved.folder.clone(),
                folder_name: e.folder_name(),
                label: e.label(),
                kind: e.kind(),
                program: e.status.clone(),
                status: e.reported(),
                needs_you: e.needs_you(),
                needs_you_since: e.track.needs_you_since().filter(|_| e.is_running()),
                reported_at: e.track.reported_at().filter(|_| e.is_running()),
                screen_changed_at: e.term.screen_changed_at(),
                cost: e.cost(),
                over_cost_warning: self.ws.over_cost_warning(e),
                start_again_cannot_help: e.start_again_cannot_help(),
            })
            .collect();
        let key_at = |index: usize| entries.get(index).map(Entry::key);
        let shown_two = self
            .ws
            .pair()
            .filter(|_| self.two_shown())
            .and_then(|pair| Some((key_at(pair.left)?, key_at(pair.right)?)));
        StateCopy {
            run: self.run,
            as_of: self.happened,
            taken_at: now,
            agents,
            front: self.ws.focused().map(Entry::key),
            shown_two,
            question: self.question.map(|q| match q.about {
                About::Quit => "quit",
                About::Close(_) => "close",
                About::Paste(_) => "paste",
                About::View { .. } => "view",
            }),
            cost_on: self.ws.cost_on(),
            cost_summary: self.ws.cost_summary(),
        }
    }

    /// Everything a reader on another thread is handed: the copy of state,
    /// the events still kept, and the screens of the agents with these key
    /// numbers, as text. A screen is the costly part to copy, so only
    /// those a reader is showing are.
    pub fn published(&self, now: SystemTime, screens: &[u64]) -> Published {
        let shown = |e: &&Entry| screens.contains(&e.key().number());
        Published {
            state: self.copy(now),
            screens: self
                .ws
                .entries()
                .iter()
                .filter(shown)
                .map(|e| (e.key(), e.term.copy_screen()))
                .collect(),
            events: self.events.iter().map(|(number, event)| event.plain(*number)).collect(),
            events_let_go: self.happened - self.events.len() as u64,
        }
    }

    /// For a test of something else: as if status had already been said to
    /// be unavailable for these kinds. It lasts only for a kind that has an
    /// agent showing the failure: the next settling takes any other back.
    #[cfg(test)]
    pub(crate) fn status_already_said(&mut self, kinds: &[AgentKind]) {
        self.status_said = kinds.to_vec();
    }

    /// Where the workspace is saved from now on. Until this is called
    /// nothing is saved, so nothing built for a test writes anywhere.
    pub fn save_in(&mut self, folder: PathBuf) {
        self.save_in = Some(folder);
    }

    /// Save the workspace now. With nowhere to save, nothing is done. On
    /// failure what was saved before is left as it was, and the reason is
    /// returned.
    pub fn save(&self) -> Result<(), String> {
        match &self.save_in {
            Some(folder) => state::save(folder, &self.ws.to_saved()),
            None => Ok(()),
        }
    }

    /// Save after a change to what is saved. A failure is an event, each
    /// time; the engine carries on.
    fn changed(&mut self) {
        match self.save() {
            Err(why) => {
                self.save_failing = true;
                self.say(Event::SaveFailed { why });
            }
            Ok(()) if std::mem::take(&mut self.save_failing) => self.say(Event::SavedAgain),
            Ok(()) => {}
        }
    }

    /// What has happened since the last call, oldest first. For the one
    /// face that words every event, once: the terminal. It takes them after
    /// every batch of output and every tick, so it never falls behind what
    /// is kept.
    pub fn take_events(&mut self) -> Vec<Event> {
        let after = std::mem::replace(&mut self.taken, self.happened);
        debug_assert!(
            self.happened - after <= EVENTS_KEPT as u64,
            "the terminal face fell behind what is kept"
        );
        self.events.iter().filter(|(number, _)| *number > after).map(|(_, event)| event.clone()).collect()
    }

    /// Reopen the agents of a saved workspace. Saved at once: restoring may
    /// have given agents new conversation ids, and those are kept.
    pub fn restore(&mut self, saved: SavedWorkspace) -> RestoreReport {
        let report = self.ws.restore(saved);
        self.changed();
        report
    }

    /// Start a new agent and put it in front. On failure nothing is added.
    pub fn start_new(
        &mut self,
        command: &str,
        args: Vec<String>,
        kind: AgentKind,
        folder: PathBuf,
    ) -> Result<(), StartError> {
        self.ws.start_new(command, args, kind, folder)?;
        self.changed();
        Ok(())
    }

    /// Something came from a running agent: output, or its end. What the
    /// user should be told of it becomes an event.
    pub fn handle(&mut self, event: AgentEvent) {
        let Some(happened) = self.ws.handle(event) else { return };
        let event = match happened {
            Happened::Exited { agent, label, exit } => Event::Exited { agent, label, exit },
            Happened::StartedNewConversation { agent, label } => {
                // The new conversation's id is kept.
                self.changed();
                Event::StartedNewConversation { agent, label }
            }
            Happened::CouldNotContinue { agent, label, why } => Event::CouldNotContinue { agent, label, why },
        };
        self.say(event);
    }

    /// The face's measure of the room agents have. Agents are given the
    /// size that fits: one agent's, or two's when two are asked for and fit.
    /// Says whether that took two off the screen or put them back.
    pub fn set_room(&mut self, room: Room) -> RoomChange {
        let before = self.two_shown();
        self.room = room;
        self.fit_agents();
        match (before, self.two_shown()) {
            (true, false) => RoomChange::TwoGone,
            (false, true) => RoomChange::TwoBack,
            _ => RoomChange::Same,
        }
    }

    /// True while two agents are side by side: two were asked for and fit.
    pub fn two_shown(&self) -> bool {
        self.ws.pair().is_some() && self.room.two.is_some()
    }

    /// Two agents side by side: the one in front and the next. Refused
    /// when two do not fit, then when fewer than two are open; asked first
    /// while an agent is waiting for the user.
    pub fn show_two(&mut self) -> Result<ViewOutcome, Refused> {
        if let Some(open) = self.open_question(About::View { two: true })? {
            return Ok(ViewOutcome::Asked(open));
        }
        self.can_show_two()?;
        if self.anyone_needs_you() {
            return Ok(ViewOutcome::Asked(self.ask(About::View { two: true })));
        }
        self.do_show_two().map(ViewOutcome::Done)
    }

    /// One agent on screen. When two were asked for but did not fit, one
    /// is on screen already: nothing is resized and nothing is asked.
    /// Otherwise asked first while an agent is waiting for the user.
    pub fn show_one(&mut self) -> Result<ViewOutcome, Refused> {
        if let Some(open) = self.open_question(About::View { two: false })? {
            return Ok(ViewOutcome::Asked(open));
        }
        if !self.two_shown() {
            self.ws.show_one();
            return Ok(ViewOutcome::Done(Viewed::OneKept));
        }
        if self.anyone_needs_you() {
            return Ok(ViewOutcome::Asked(self.ask(About::View { two: false })));
        }
        Ok(ViewOutcome::Done(self.do_show_one()))
    }

    /// Tell every agent the size for the view as it is now. Called after
    /// anything that may have changed the view, such as closing one of two.
    pub fn fit_agents(&mut self) {
        let size = match self.room.two {
            Some(two) if self.ws.pair().is_some() => two,
            _ => self.room.one,
        };
        self.ws.resize(size);
    }

    /// Whether two could be put side by side now: refused when two do not
    /// fit, then when fewer than two are open. Asks nothing.
    pub fn can_show_two(&self) -> Result<(), Refused> {
        if self.room.two.is_none() {
            return Err(Refused::TwoDoNotFit);
        }
        match self.ws.entries().len() {
            n if n < 2 => Err(Refused::TooFewAgents(n)),
            _ => Ok(()),
        }
    }

    fn do_show_two(&mut self) -> Result<Viewed, Refused> {
        self.can_show_two()?;
        let pair = self.ws.show_two().map_err(|NoPair::TooFewAgents(n)| Refused::TooFewAgents(n))?;
        self.fit_agents();
        Ok(Viewed::Two(pair))
    }

    fn do_show_one(&mut self) -> Viewed {
        self.ws.show_one();
        self.fit_agents();
        Viewed::One
    }

    fn anyone_needs_you(&self) -> bool {
        self.ws.entries().iter().any(|e| e.needs_you().is_some())
    }

    /// Text pasted into an agent, with the control characters already taken
    /// out of it (`left_out`). Its line breaks never reach the agent as
    /// Enter presses unless the user agrees:
    ///
    /// - an agent that takes a paste whole gets it as one marked paste;
    /// - otherwise it goes as typed keys, its tabs as spaces, if it is one
    ///   line;
    /// - and if it is more than one, it is held and a question asked.
    pub fn paste(
        &mut self,
        agent: AgentKey,
        text: String,
        left_out: Vec<char>,
    ) -> Result<PasteOutcome, Refused> {
        let entry = self.ws.by_key(agent).ok_or(Refused::NoSuchAgent)?;
        if !entry.is_running() {
            return Err(Refused::AgentStopped);
        }
        if self.question.is_some() {
            return Err(Refused::QuestionOpen);
        }
        if text.is_empty() && !left_out.is_empty() {
            return Ok(PasteOutcome::OnlyControls(left_out));
        }
        let label = entry.label();
        if entry.term.takes_pastes_whole() {
            let body = text.replace('\n', "\r");
            self.ws.send_to(agent, format!("\x1b[200~{body}\x1b[201~").as_bytes());
            return Ok(PasteOutcome::Sent(Sent { label, left_out, tabs: false, how: SentAs::Whole }));
        }
        let tabs = text.contains('\t');
        let text = text.replace('\t', TAB_AS_SPACES);
        if text.contains('\n') {
            self.held = Some(HeldPaste { text, left_out, tabs });
            return Ok(PasteOutcome::Asked(self.ask(About::Paste(agent))));
        }
        self.ws.send_to(agent, text.as_bytes());
        Ok(PasteOutcome::Sent(Sent { label, left_out, tabs, how: SentAs::Typed }))
    }

    /// Text typed on the page for an agent: sent by `paste` itself, under
    /// its rules and with its refusals, then Enter. The page has no other
    /// way in. Two things differ from a paste in the terminal:
    ///
    /// - the page asks its own reader before text goes line by line, so no
    ///   question is left open here. Without `line_by_line` such text is not
    ///   sent, and `MustAgree` says so; with it the reader has agreed;
    /// - every send is an event that names the agent.
    ///
    /// `line_by_line` is the caller's word that its reader agreed: whoever
    /// can call this can say so. The question protects the page's reader
    /// from a surprise, not the agent from the page.
    pub fn type_from_page(
        &mut self,
        agent: AgentKey,
        text: String,
        left_out: Vec<char>,
        line_by_line: bool,
    ) -> Result<PageTyped, Refused> {
        let sent = match self.paste(agent, text, left_out)? {
            PasteOutcome::OnlyControls(left_out) => return Ok(PageTyped::OnlyControls(left_out)),
            PasteOutcome::Sent(sent) => sent,
            // The question is answered before this returns: a yes only
            // with the reader's agreement, and otherwise no.
            PasteOutcome::Asked(question) => {
                let lines = self.held_paste_lines().unwrap_or(0);
                let label = self.ws.by_key(agent).map(Entry::label).unwrap_or_default();
                match self.answer(question.id, line_by_line)? {
                    Answered::Pasted(sent) => sent,
                    Answered::Nothing => return Ok(PageTyped::MustAgree { lines, label }),
                    _ => return Err(Refused::AgentStopped),
                }
            }
        };
        let entered = self.ws.send_to(agent, b"\r");
        self.say(Event::TypedFromPage { agent, label: sent.label.clone(), entered });
        Ok(PageTyped::Sent { sent, entered })
    }

    /// Enter, and nothing else, pressed in an agent from the page: to
    /// answer a prompt the agent is waiting at. Refused as text from the
    /// page is: a gone or stopped agent, or a question open. Every press is
    /// an event that names the agent. Returns the agent's label.
    pub fn enter_from_page(&mut self, agent: AgentKey) -> Result<String, Refused> {
        let entry = self.ws.by_key(agent).ok_or(Refused::NoSuchAgent)?;
        if !entry.is_running() {
            return Err(Refused::AgentStopped);
        }
        if self.question.is_some() {
            return Err(Refused::QuestionOpen);
        }
        let label = entry.label();
        if !self.ws.send_to(agent, b"\r") {
            return Err(Refused::AgentStopped);
        }
        self.say(Event::EnterFromPage { agent, label: label.clone() });
        Ok(label)
    }

    /// Something was done to a card of the Board from the page, or undone:
    /// it is said, as an event. The engine keeps nothing of the Board; the
    /// outbox is written by whoever tells it this.
    pub fn card_acted(&mut self, card: String, title: String, action: &'static str, undone: bool) {
        self.say(Event::CardActed { card, title, action, undone });
    }

    /// Tells this run of Lillignode from any other: agents' keys and event
    /// numbers begin again with each run.
    pub fn run(&self) -> u64 {
        self.run
    }

    /// How many lines the held paste has, while the question about it is
    /// open.
    pub fn held_paste_lines(&self) -> Option<usize> {
        self.held.as_ref().map(|p| p.text.split('\n').count())
    }

    /// The question waiting for an answer, if any.
    pub fn question(&self) -> Option<Question> {
        self.question
    }

    /// True once quitting has been decided.
    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Quit. Every running agent is stopped either way; the user is asked
    /// first only when one may be stopped in the middle of something. Asked
    /// again while the question is open, it is the same question.
    pub fn quit(&mut self) -> Result<Outcome, Refused> {
        if let Some(open) = self.open_question(About::Quit)? {
            return Ok(Outcome::Asked(open));
        }
        if self.unfinished().is_empty() {
            self.closing = true;
            return Ok(Outcome::Done);
        }
        Ok(Outcome::Asked(self.ask(About::Quit)))
    }

    /// Close an agent. Always asked first: closing takes the agent and its
    /// conversation out of the workspace, and it does not come back.
    pub fn close(&mut self, agent: AgentKey) -> Result<Outcome, Refused> {
        self.ws.by_key(agent).ok_or(Refused::NoSuchAgent)?;
        if let Some(open) = self.open_question(About::Close(agent))? {
            return Ok(Outcome::Asked(open));
        }
        Ok(Outcome::Asked(self.ask(About::Close(agent))))
    }

    /// Start an agent again after it stopped or could not be started.
    /// Returns whether it was asked to continue its conversation. Whether
    /// it started this time is on the agent. Refusals are checked in this
    /// order: no such agent, already running, a question open.
    pub fn start_again(&mut self, agent: AgentKey) -> Result<bool, Refused> {
        let entry = self.ws.by_key(agent).ok_or(Refused::NoSuchAgent)?;
        if entry.is_running() {
            return Err(Refused::AlreadyRunning);
        }
        if self.question.is_some() {
            return Err(Refused::QuestionOpen);
        }
        let continuing = self.ws.restart(agent).ok_or(Refused::NoSuchAgent)?;
        self.changed();
        Ok(continuing)
    }

    /// The answer to the open question. Whatever the answer, the question
    /// is closed. A yes to quitting quits even if every agent has finished
    /// while it was open: the user saw the question and said yes to it.
    pub fn answer(&mut self, id: QuestionId, yes: bool) -> Result<Answered, Refused> {
        let question = self.question.filter(|q| q.id == id).ok_or(Refused::NoSuchQuestion)?;
        self.question = None;
        // A held paste goes with its question, whatever the answer.
        let held = self.held.take();
        if !yes {
            return Ok(Answered::Nothing);
        }
        Ok(match question.about {
            About::Quit => {
                self.closing = true;
                Answered::Closing
            }
            About::Close(agent) => match self.ws.close(agent) {
                Some(label) => {
                    self.changed();
                    Answered::Closed { label }
                }
                None => Answered::AlreadyGone,
            },
            About::Paste(agent) => {
                let Some(HeldPaste { text, left_out, tabs }) = held else {
                    return Ok(Answered::PasteDropped);
                };
                let label = self.ws.by_key(agent).map(Entry::label);
                match label {
                    Some(label) if self.ws.send_to(agent, text.replace('\n', "\r").as_bytes()) => {
                        Answered::Pasted(Sent { label, left_out, tabs, how: SentAs::LineByLine })
                    }
                    _ => Answered::PasteDropped,
                }
            }
            // Checked again: the room may have changed while it was asked.
            About::View { two: true } => match self.do_show_two() {
                Ok(viewed) => Answered::Viewed(viewed),
                Err(why) => Answered::ViewRefused(why),
            },
            About::View { two: false } => Answered::Viewed(self.do_show_one()),
        })
    }

    /// Running agents that may be in the middle of something: not Done,
    /// Idle or Error. Those that need you come first, then the rest, each
    /// in the order of the list.
    pub fn unfinished(&self) -> Vec<Unfinished> {
        let unfinished = |e: &&Entry| e.is_running() && !e.is_settled();
        let of = |e: &Entry| Unfinished {
            label: e.label(),
            status: e.reported(),
            needs_you: e.needs_you().is_some(),
        };
        let (need, rest): (Vec<_>, Vec<_>) =
            self.ws.entries().iter().filter(unfinished).map(of).partition(|u| u.needs_you);
        need.into_iter().chain(rest).collect()
    }

    /// Running agents that will not be asked to continue their
    /// conversation when they come back, so what they were doing is not
    /// picked up again.
    pub fn fresh_next_time(&self) -> Vec<String> {
        self.ws
            .entries()
            .iter()
            .filter(|e| e.is_running() && !e.kind().keeps_conversations())
            .map(Entry::label)
            .collect()
    }

    /// The open question if it is about this, None if none is open, and
    /// refused if a question about something else is open.
    fn open_question(&self, about: About) -> Result<Option<Question>, Refused> {
        match self.question {
            Some(open) if open.about == about => Ok(Some(open)),
            Some(_) => Err(Refused::QuestionOpen),
            None => Ok(None),
        }
    }

    fn ask(&mut self, about: About) -> Question {
        self.asked += 1;
        let question = Question { id: QuestionId(self.asked), about };
        self.question = Some(question);
        question
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentEvent, AgentId, AgentSpec, Size, StartError};
    use crate::kind::AgentKind;
    use crate::status::Kind;
    use std::time::SystemTime;

    /// Starts anything, and keeps the ids it was asked to stop and what was
    /// sent to each.
    #[derive(Default)]
    struct Fake {
        next: u32,
        stopped: std::rc::Rc<std::cell::RefCell<Vec<u32>>>,
        sent: Sent,
    }

    type Sent = std::rc::Rc<std::cell::RefCell<Vec<(u32, String)>>>;

    impl AgentBackend for Fake {
        fn start(&mut self, _: &AgentSpec, _: Size) -> Result<AgentId, StartError> {
            self.next += 1;
            Ok(AgentId(self.next))
        }
        fn send(&mut self, id: AgentId, bytes: &[u8]) -> std::io::Result<()> {
            self.sent.borrow_mut().push((id.0, String::from_utf8_lossy(bytes).into_owned()));
            Ok(())
        }
        fn resize(&mut self, _: AgentId, _: Size) {}
        fn close(&mut self, id: AgentId) {
            self.stopped.borrow_mut().push(id.0);
        }
    }

    /// Plain programs, `a` then `b` (ids 1 and 2), and how each reports.
    fn engine(reports: &[&str]) -> Engine<Fake> {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        for (n, body) in reports.iter().enumerate() {
            let name = ["a", "b"][n];
            ws.start_new(name, Vec::new(), AgentKind::Program, std::env::temp_dir().join("work")).unwrap();
            let report = format!("\x1b]7501;{body}\x1b\\");
            ws.handle(AgentEvent::Output(AgentId(n as u32 + 1), report.into_bytes()));
        }
        Engine::new(ws)
    }

    #[test]
    fn quit_closes_at_once_when_nothing_is_unfinished() {
        let mut e = engine(&["state=idle", "state=done"]);
        assert_eq!(e.quit().unwrap(), Outcome::Done);
        assert!(e.closing());
        assert_eq!(e.question(), None);
    }

    #[test]
    fn quit_asks_while_an_agent_may_be_busy_and_the_answer_decides() {
        let mut e = engine(&["state=working", "state=idle"]);
        let Outcome::Asked(q) = e.quit().unwrap() else { panic!("asked") };
        assert_eq!(q.about, About::Quit);
        assert_eq!(e.question(), Some(q));
        assert!(!e.closing());
        // Asked again while open: the same question.
        assert_eq!(e.quit().unwrap(), Outcome::Asked(q));

        assert_eq!(e.answer(q.id, false), Ok(Answered::Nothing));
        assert!(!e.closing());
        assert_eq!(e.question(), None);

        let Outcome::Asked(again) = e.quit().unwrap() else { panic!("asked") };
        assert_ne!(again.id, q.id, "a new question has a new id");
        assert_eq!(e.answer(again.id, true), Ok(Answered::Closing));
        assert!(e.closing());
    }

    #[test]
    fn an_answer_to_a_question_that_is_not_open_changes_nothing() {
        let mut e = engine(&["state=working"]);
        let Outcome::Asked(old) = e.quit().unwrap() else { panic!("asked") };
        e.answer(old.id, false).unwrap();
        // Answered already.
        assert_eq!(e.answer(old.id, true), Err(Refused::NoSuchQuestion));
        assert!(!e.closing());
        // An older question while a newer one is open.
        let Outcome::Asked(new) = e.quit().unwrap() else { panic!("asked") };
        assert_eq!(e.answer(old.id, true), Err(Refused::NoSuchQuestion));
        assert!(!e.closing());
        assert_eq!(e.question(), Some(new), "the open question stays");
    }

    #[test]
    fn unfinished_names_those_that_need_you_first() {
        let e = engine(&["state=working", "state=blocked:kind=permission"]);
        let unfinished = e.unfinished();
        let labels: Vec<&str> = unfinished.iter().map(|u| u.label.as_str()).collect();
        assert_eq!(labels.len(), 2);
        assert!(labels[0].starts_with("b "), "{labels:?}");
        assert!(unfinished[0].needs_you);
        assert_eq!(unfinished[0].status, Some(AgentStatus::NeedsYou(Some(Kind::Permission))));
        assert!(!unfinished[1].needs_you);
        assert_eq!(unfinished[1].status, Some(AgentStatus::Working));
    }

    #[test]
    fn with_no_agents_quit_closes_at_once() {
        let mut e = engine(&[]);
        assert_eq!(e.quit().unwrap(), Outcome::Done);
        assert!(e.closing());
    }

    #[test]
    fn an_agent_whose_status_is_not_known_counts_as_unfinished() {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        ws.start_new("a", Vec::new(), AgentKind::Program, std::env::temp_dir().join("work")).unwrap();
        let mut e = Engine::new(ws);
        assert_eq!(e.unfinished().len(), 1);
        // Not known is not finished.
        assert!(matches!(e.unfinished()[0].status, Some(AgentStatus::Unknown(_))));
        assert!(matches!(e.quit().unwrap(), Outcome::Asked(_)));
    }

    #[test]
    fn settled_agents_and_stopped_agents_do_not_count() {
        let mut e = engine(&["state=error", "state=working"]);
        assert_eq!(e.unfinished().len(), 1, "an error is settled; working is not");
        // The second stops while it said Working.
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert!(e.unfinished().is_empty(), "{:?}", e.unfinished());
        assert_eq!(e.quit().unwrap(), Outcome::Done);
    }

    #[test]
    fn needs_you_without_a_reason_still_needs_you() {
        let e = engine(&["state=blocked"]);
        assert!(e.unfinished()[0].needs_you);
        assert_eq!(e.unfinished()[0].status, Some(AgentStatus::NeedsYou(None)));
    }

    #[test]
    fn only_running_agents_that_do_not_keep_conversations_start_afresh() {
        let mut e = engine(&["state=idle", "state=idle"]);
        e.ws.start_new("claude", Vec::new(), AgentKind::ClaudeCode, std::env::temp_dir().join("work"))
            .unwrap();
        // b stops.
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        let fresh = e.fresh_next_time();
        assert_eq!(fresh.len(), 1, "{fresh:?}");
        assert!(fresh[0].starts_with("a "), "{fresh:?}");
    }

    fn key(e: &Engine<Fake>, n: usize) -> AgentKey {
        e.ws.entries()[n].key()
    }

    #[test]
    fn close_always_asks_and_the_answer_decides() {
        let mut e = engine(&["state=idle", "state=idle"]);
        let a = key(&e, 0);
        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        assert_eq!(q.about, About::Close(a));
        assert_eq!(e.close(a), Ok(Outcome::Asked(q)), "asked again: the same question");
        assert_eq!(e.answer(q.id, false), Ok(Answered::Nothing));
        assert_eq!(e.ws.entries().len(), 2);

        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        let Ok(Answered::Closed { label }) = e.answer(q.id, true) else { panic!("closed") };
        assert!(label.starts_with("a "), "{label}");
        assert_eq!(e.ws.entries().len(), 1);
        // Gone now: refused, and nothing else is touched.
        assert_eq!(e.close(a), Err(Refused::NoSuchAgent));
        assert_eq!(e.ws.entries().len(), 1);
    }

    #[test]
    fn a_yes_to_closing_an_agent_already_gone_closes_nothing() {
        let mut e = engine(&["state=idle", "state=idle"]);
        let a = key(&e, 0);
        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        e.ws.close(a);
        assert_eq!(e.answer(q.id, true), Ok(Answered::AlreadyGone));
        assert_eq!(e.ws.entries().len(), 1, "the other agent is left alone");
    }

    #[test]
    fn one_question_at_a_time() {
        let mut e = engine(&["state=working", "state=idle"]);
        let b = key(&e, 1);
        let Ok(Outcome::Asked(quit)) = e.quit() else { panic!("asked") };
        assert_eq!(e.close(b), Err(Refused::QuestionOpen));
        assert_eq!(e.question(), Some(quit), "the open question stays");
        e.answer(quit.id, false).unwrap();

        let Ok(Outcome::Asked(close)) = e.close(b) else { panic!("asked") };
        assert_eq!(e.quit(), Err(Refused::QuestionOpen));
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert_eq!(e.start_again(b), Err(Refused::QuestionOpen));
        assert_eq!(e.question(), Some(close));
        assert!(!e.closing());
    }

    #[test]
    fn start_again_only_what_has_stopped() {
        let mut e = engine(&["state=idle"]);
        let a = key(&e, 0);
        assert_eq!(e.start_again(a), Err(Refused::AlreadyRunning));
        e.ws.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert!(!e.ws.entries()[0].is_running());
        // A plain program has no conversation to continue.
        assert_eq!(e.start_again(a), Ok(false));
        assert!(e.ws.entries()[0].is_running());
        assert_eq!(e.ws.entries()[0].key(), a, "still the same agent");

        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(e.start_again(a), Err(Refused::NoSuchAgent));
    }

    #[test]
    fn closing_stops_a_running_agent_and_removes_a_stopped_one() {
        let fake = Fake::default();
        let stopped = fake.stopped.clone();
        let mut ws = Workspace::new(fake, Size { rows: 24, cols: 80 });
        for name in ["a", "b"] {
            ws.start_new(name, Vec::new(), AgentKind::Program, std::env::temp_dir().join("work")).unwrap();
        }
        let mut e = Engine::new(ws);
        let (a, b) = (key(&e, 0), key(&e, 1));
        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(*stopped.borrow(), [1], "the running agent was stopped");
        // b stops by itself, then is closed: removed. (The backend is told
        // to close its id as well, as it always was; for a program that has
        // ended that does nothing.)
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        let Ok(Outcome::Asked(q)) = e.close(b) else { panic!("asked") };
        assert!(matches!(e.answer(q.id, true), Ok(Answered::Closed { .. })));
        assert!(e.ws.entries().is_empty());
    }

    #[test]
    fn claude_code_started_again_continues_its_conversation() {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        ws.start_new("claude", Vec::new(), AgentKind::ClaudeCode, std::env::temp_dir().join("work")).unwrap();
        let mut e = Engine::new(ws);
        let a = key(&e, 0);
        e.ws.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert_eq!(e.start_again(a), Ok(true));
        assert!(e.ws.entries()[0].is_running());
    }

    #[test]
    fn an_agent_that_could_not_be_started_can_be_started_again() {
        use crate::state::{SavedAgent, SavedWorkspace};
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        // Saved with a folder that is not a full path: not started.
        let saved = SavedAgent {
            name: "a".into(),
            command: "a".into(),
            kind: Some("program".into()),
            args: vec![],
            folder: "work".into(),
            conversation: None,
            other: Default::default(),
        };
        ws.restore(SavedWorkspace { agents: vec![saved], ..Default::default() });
        let mut e = Engine::new(ws);
        let a = key(&e, 0);
        assert!(!e.ws.entries()[0].is_running());
        // Not refused: it is tried again, and is still not started, which
        // the agent says.
        assert_eq!(e.start_again(a), Ok(false));
        assert!(!e.ws.entries()[0].is_running());
        assert_eq!(e.ws.entries()[0].key(), a);
    }

    /// Plain programs as `engine` makes them, and what is sent to them.
    fn engine_and_sent(reports: &[&str]) -> (Engine<Fake>, Sent) {
        let fake = Fake::default();
        let sent = fake.sent.clone();
        let mut ws = Workspace::new(fake, Size { rows: 24, cols: 80 });
        for (n, body) in reports.iter().enumerate() {
            let name = ["a", "b"][n];
            ws.start_new(name, Vec::new(), AgentKind::Program, std::env::temp_dir().join("work")).unwrap();
            let report = format!("\x1b]7501;{body}\x1b\\");
            ws.handle(AgentEvent::Output(AgentId(n as u32 + 1), report.into_bytes()));
        }
        (Engine::new(ws), sent)
    }

    fn got(sent: &Sent) -> Vec<(u32, String)> {
        sent.borrow().clone()
    }

    #[test]
    fn a_paste_into_an_agent_that_takes_pastes_goes_as_one_marked_block() {
        let (mut e, sent) = engine_and_sent(&["state=idle"]);
        // The program asks to have pastes marked.
        e.ws.handle(AgentEvent::Output(AgentId(1), b"\x1b[?2004h".to_vec()));
        let a = key(&e, 0);
        let Ok(PasteOutcome::Sent(s)) = e.paste(a, "one\ttwo\nthree".into(), vec!['\x03']) else {
            panic!("sent")
        };
        assert_eq!(s.how, SentAs::Whole);
        assert!(!s.tabs, "a tab inside the markers is text");
        assert_eq!(s.left_out, ['\x03']);
        assert_eq!(got(&sent), [(1, "\x1b[200~one\ttwo\rthree\x1b[201~".to_string())]);
        assert_eq!(e.question(), None);
    }

    #[test]
    fn one_line_goes_as_typed_keys_with_tabs_as_spaces() {
        let (mut e, sent) = engine_and_sent(&["state=idle"]);
        let a = key(&e, 0);
        let Ok(PasteOutcome::Sent(s)) = e.paste(a, "a\tb".into(), vec![]) else { panic!("sent") };
        assert_eq!((s.how, s.tabs), (SentAs::Typed, true));
        assert_eq!(got(&sent), [(1, "a    b".to_string())], "no tab reaches the agent");
        let Ok(PasteOutcome::Sent(s)) = e.paste(a, "plain".into(), vec![]) else { panic!("sent") };
        assert!(!s.tabs && s.left_out.is_empty());
    }

    #[test]
    fn several_lines_are_held_until_the_user_agrees() {
        let (mut e, sent) = engine_and_sent(&["state=idle", "state=idle"]);
        let b = key(&e, 1);
        let Ok(PasteOutcome::Asked(q)) = e.paste(b, "one\ttwo\nthree".into(), vec![]) else {
            panic!("asked")
        };
        assert_eq!(q.about, About::Paste(b));
        assert_eq!(e.held_paste_lines(), Some(2));
        assert!(got(&sent).is_empty(), "nothing is sent before the answer");
        // One question at a time.
        assert_eq!(e.paste(b, "x".into(), vec![]), Err(Refused::QuestionOpen));
        assert_eq!(e.quit(), Err(Refused::QuestionOpen));

        let Ok(Answered::Pasted(s)) = e.answer(q.id, true) else { panic!("pasted") };
        assert_eq!((s.how, s.tabs), (SentAs::LineByLine, true));
        assert_eq!(got(&sent), [(2, "one    two\rthree".to_string())], "to the agent asked about");
        assert_eq!(e.held_paste_lines(), None);

        // No: nothing is sent, and the paste is gone.
        let Ok(PasteOutcome::Asked(q)) = e.paste(b, "x\ny".into(), vec![]) else { panic!("asked") };
        assert_eq!(e.answer(q.id, false), Ok(Answered::Nothing));
        assert_eq!(e.held_paste_lines(), None);
        assert_eq!(got(&sent).len(), 1);
    }

    #[test]
    fn a_held_paste_is_dropped_if_its_agent_stops_before_the_answer() {
        let (mut e, sent) = engine_and_sent(&["state=idle"]);
        let a = key(&e, 0);
        let Ok(PasteOutcome::Asked(q)) = e.paste(a, "x\ny".into(), vec![]) else { panic!("asked") };
        e.ws.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert_eq!(e.answer(q.id, true), Ok(Answered::PasteDropped));
        assert!(got(&sent).is_empty());
    }

    #[test]
    fn a_paste_is_refused_for_an_agent_that_is_stopped_or_gone() {
        let (mut e, sent) = engine_and_sent(&["state=idle"]);
        let a = key(&e, 0);
        assert_eq!(
            e.paste(a, String::new(), vec!['\x03', '\x04']),
            Ok(PasteOutcome::OnlyControls(vec!['\x03', '\x04']))
        );
        assert!(got(&sent).is_empty(), "only control characters: nothing sent");
        e.ws.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert_eq!(e.paste(a, "x".into(), vec![]), Err(Refused::AgentStopped));
        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(e.paste(a, "x".into(), vec![]), Err(Refused::NoSuchAgent));
        assert!(got(&sent).is_empty());
    }

    fn typed_events(e: &Engine<Fake>) -> Vec<(u64, EventCopy)> {
        let all = e.events_after(0).events;
        all.iter()
            .map(|(number, event)| (*number, event.plain(*number)))
            .filter(|(_, plain)| plain.what == "typed-from-page")
            .collect()
    }

    #[test]
    fn text_from_the_page_goes_as_a_paste_does_then_enter_and_is_an_event_naming_the_agent() {
        let (mut e, sent) = engine_and_sent(&["state=idle", "state=idle"]);
        let (a, b) = (key(&e, 0), key(&e, 1));
        // One line, to the second agent while the first is in front: typed,
        // tabs as spaces, then Enter. Nothing goes to any other agent.
        let Ok(PageTyped::Sent { sent: s, entered }) = e.type_from_page(b, "do\tthis".into(), vec![], false)
        else {
            panic!("sent")
        };
        assert_eq!((s.how, s.tabs, entered), (SentAs::Typed, true, true));
        assert_eq!(got(&sent), [(2, "do    this".to_string()), (2, "\r".to_string())]);
        let events = typed_events(&e);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.agent, Some(b), "the event names the agent it went to");
        assert_eq!(events[0].1.label.as_deref(), Some(s.label.as_str()));
        // An agent that takes pastes whole gets one marked paste, then Enter.
        e.ws.handle(AgentEvent::Output(AgentId(1), b"\x1b[?2004h".to_vec()));
        sent.borrow_mut().clear();
        let Ok(PageTyped::Sent { sent: s, .. }) = e.type_from_page(a, "one\ntwo".into(), vec!['\x03'], false)
        else {
            panic!("sent")
        };
        assert_eq!((s.how, s.left_out.as_slice()), (SentAs::Whole, &['\x03'][..]));
        assert_eq!(got(&sent), [(1, "\x1b[200~one\rtwo\x1b[201~".to_string()), (1, "\r".to_string())]);
        assert_eq!(typed_events(&e).len(), 2, "every send is an event");
        assert_eq!(e.question(), None);
    }

    #[test]
    fn what_is_done_to_a_card_is_an_event_naming_the_card_each_time() {
        let mut e = engine(&[]);
        e.card_acted("c1".into(), "Keep it?".into(), "approve", false);
        e.card_acted("c1".into(), "Keep it?".into(), "approve", true);
        e.card_acted("c2".into(), "Renew it".into(), "done", false);
        let said: Vec<EventCopy> = e.events_after(0).events.iter().map(|(n, ev)| ev.plain(*n)).collect();
        let plain: Vec<(&str, Option<&str>)> = said.iter().map(|p| (p.what, p.label.as_deref())).collect();
        assert_eq!(
            plain,
            [("card-acted", Some("c1")), ("card-action-undone", Some("c1")), ("card-acted", Some("c2"))]
        );
        assert!(said.iter().all(|p| p.agent.is_none()), "a card is not an agent");
        assert_eq!(e.take_events().len(), 3, "the terminal is told each");
    }

    #[test]
    fn the_copy_says_when_each_agents_screen_last_changed() {
        let mut e = engine(&["state=idle", "state=idle"]);
        let (a, b) = (key(&e, 0), key(&e, 1));
        let changed = |e: &Engine<Fake>, key| e.copy(SystemTime::now()).agent(key).unwrap().screen_changed_at;
        // A status report draws nothing: neither screen has changed yet.
        assert_eq!((changed(&e, a), changed(&e, b)), (None, None));
        let before = SystemTime::now();
        e.ws.handle(AgentEvent::Output(AgentId(1), b"hello".to_vec()));
        let first = changed(&e, a).expect("it drew something");
        assert!(first >= before && first <= SystemTime::now());
        assert_eq!(changed(&e, b), None, "the other agent's screen did not change");
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Another report, and the same text drawn again in the same place:
        // output, but the screen is as it was.
        e.ws.handle(AgentEvent::Output(AgentId(1), b"\x1b]7501;state=working\x1b\\".to_vec()));
        e.ws.handle(AgentEvent::Output(AgentId(1), b"\rhello".to_vec()));
        assert_eq!(changed(&e, a), Some(first), "nothing on screen changed");
        // One more character: it has.
        e.ws.handle(AgentEvent::Output(AgentId(1), b"!".to_vec()));
        assert!(changed(&e, a).unwrap() > first);
        // Kept after its program ends: it is when its last screen was drawn.
        let last = changed(&e, a);
        e.ws.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert_eq!(changed(&e, a), last);
    }

    #[test]
    fn enter_from_the_page_is_enter_alone_to_the_agent_named_and_an_event() {
        let (mut e, sent) = engine_and_sent(&["state=idle", "state=idle"]);
        let (a, b) = (key(&e, 0), key(&e, 1));
        assert_eq!(e.enter_from_page(b), Ok("b (work)".to_string()));
        assert_eq!(got(&sent), [(2, "\r".to_string())], "Enter, to it alone");
        let events = e.events_after(0).events;
        let said: Vec<EventCopy> =
            events.iter().map(|(n, ev)| ev.plain(*n)).filter(|p| p.what == "enter-from-page").collect();
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].agent, Some(b));
        // Refused as text is, and then nothing is sent or said.
        let Ok(Outcome::Asked(q)) = e.close(a) else { panic!("asked") };
        assert_eq!(e.enter_from_page(b), Err(Refused::QuestionOpen));
        e.answer(q.id, false).unwrap();
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert_eq!(e.enter_from_page(b), Err(Refused::AgentStopped));
        let Ok(Outcome::Asked(q)) = e.close(b) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(e.enter_from_page(b), Err(Refused::NoSuchAgent));
        assert_eq!(got(&sent).len(), 1, "nothing more was sent");
        let presses =
            e.events_after(0).events.iter().filter(|(n, ev)| ev.plain(*n).what == "enter-from-page").count();
        assert_eq!(presses, 1);
    }

    #[test]
    fn several_lines_from_the_page_are_not_sent_until_its_reader_has_agreed() {
        let (mut e, sent) = engine_and_sent(&["state=idle"]);
        let a = key(&e, 0);
        let asked = e.type_from_page(a, "one\ntwo\nthree".into(), vec![], false);
        let Ok(PageTyped::MustAgree { lines, label }) = asked else { panic!("must agree: {asked:?}") };
        assert_eq!((lines, label.as_str()), (3, "a (work)"));
        assert!(got(&sent).is_empty(), "nothing is sent before the reader agrees");
        // Nothing is held and no question is left for the terminal to find.
        assert_eq!((e.question(), e.held_paste_lines()), (None, None));
        assert!(typed_events(&e).is_empty(), "nothing sent, nothing said");
        assert_eq!(e.quit(), Ok(Outcome::Done), "nothing stands in the terminal's way");

        let (mut e, sent) = engine_and_sent(&["state=idle"]);
        let a = key(&e, 0);
        let Ok(PageTyped::Sent { sent: s, entered: true }) =
            e.type_from_page(a, "one\ntwo".into(), vec![], true)
        else {
            panic!("sent")
        };
        assert_eq!(s.how, SentAs::LineByLine);
        assert_eq!(got(&sent), [(1, "one\rtwo".to_string()), (1, "\r".to_string())]);
        assert_eq!((e.question(), typed_events(&e).len()), (None, 1));
    }

    #[test]
    fn text_from_the_page_is_refused_as_a_paste_is_and_then_nothing_is_sent_or_said() {
        let (mut e, sent) = engine_and_sent(&["state=working", "state=idle"]);
        let (a, b) = (key(&e, 0), key(&e, 1));
        // A question is open in the terminal: it is answered first.
        let Ok(Outcome::Asked(q)) = e.quit() else { panic!("asked") };
        assert_eq!(e.type_from_page(b, "x".into(), vec![], true), Err(Refused::QuestionOpen));
        assert_eq!(e.question(), Some(q), "the terminal's question is untouched");
        e.answer(q.id, false).unwrap();
        // Only control characters.
        assert_eq!(
            e.type_from_page(b, String::new(), vec!['\x1b'], true),
            Ok(PageTyped::OnlyControls(vec!['\x1b']))
        );
        // Stopped, then gone.
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert_eq!(e.type_from_page(b, "x".into(), vec![], true), Err(Refused::AgentStopped));
        let Ok(Outcome::Asked(q)) = e.close(b) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(e.type_from_page(b, "x".into(), vec![], true), Err(Refused::NoSuchAgent));
        assert!(got(&sent).is_empty(), "not to it, and not to the agent that is still running");
        assert!(typed_events(&e).is_empty());
        let _ = a;
    }

    /// An empty folder of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-engine-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn saved_names(dir: &std::path::Path) -> Vec<String> {
        state::load(dir).workspace.agents.into_iter().map(|a| a.name).collect()
    }

    #[test]
    fn the_workspace_is_saved_after_each_change_to_what_is_saved() {
        let dir = scratch("saves");
        let mut e = engine(&[]);
        e.save_in(dir.clone());
        let work = std::env::temp_dir();
        e.start_new("a", Vec::new(), AgentKind::Program, work.clone()).unwrap();
        assert_eq!(saved_names(&dir), ["a"], "after a start");
        e.start_new("b", Vec::new(), AgentKind::Program, work).unwrap();
        assert_eq!(saved_names(&dir), ["a", "b"]);

        let Ok(Outcome::Asked(q)) = e.close(key(&e, 0)) else { panic!("asked") };
        assert_eq!(saved_names(&dir), ["a", "b"], "a question changes nothing");
        e.answer(q.id, false).unwrap();
        assert_eq!(saved_names(&dir), ["a", "b"], "nor does a no");
        let Ok(Outcome::Asked(q)) = e.close(key(&e, 0)) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(saved_names(&dir), ["b"], "after a close");
        assert!(e.take_events().is_empty(), "every save worked: nothing to say");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restored_workspace_is_saved_at_once_with_its_new_conversation_ids() {
        use crate::state::SavedAgent;
        let dir = scratch("restore");
        let mut e = engine(&[]);
        e.save_in(dir.clone());
        let claude = SavedAgent {
            name: "Claude Code".into(),
            command: "claude".into(),
            kind: Some("claude-code".into()),
            args: vec![],
            folder: std::env::temp_dir(),
            conversation: None,
            other: Default::default(),
        };
        e.restore(SavedWorkspace { agents: vec![claude], ..Default::default() });
        let saved = state::load(&dir).workspace.agents;
        assert_eq!(saved.len(), 1);
        assert!(saved[0].conversation.is_some(), "the id it was given is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_that_fails_is_said_each_time_and_nothing_else_changes() {
        // A file where the folder should be: nothing can be saved in it.
        let dir = scratch("fails");
        let not_a_folder = dir.join("file");
        std::fs::write(&not_a_folder, "x").unwrap();
        let mut e = engine(&["state=idle"]);
        e.save_in(not_a_folder);
        assert!(e.save().is_err());
        assert!(e.take_events().is_empty(), "asking for a save returns the reason; it is no event");

        e.start_new("b", Vec::new(), AgentKind::Program, std::env::temp_dir()).unwrap();
        assert_eq!(e.ws.entries().len(), 2, "the agent started all the same");
        assert!(matches!(e.take_events().as_slice(), [Event::SaveFailed { .. }]));
        assert!(e.take_events().is_empty(), "taken once");

        // Its program ends: said, and not a change to what is saved.
        e.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert!(matches!(e.take_events().as_slice(), [Event::Exited { exit, .. }] if exit.clean()));
        // Started again: saved again, and it fails again.
        assert_eq!(e.start_again(key(&e, 1)), Ok(false));
        assert!(matches!(e.take_events().as_slice(), [Event::SaveFailed { .. }]));

        // Once a save works, that is said, once: what was said is no longer true.
        e.save_in(dir.join("folder"));
        e.start_new("c", Vec::new(), AgentKind::Program, std::env::temp_dir()).unwrap();
        assert_eq!(e.take_events(), [Event::SavedAgain]);
        e.start_new("d", Vec::new(), AgentKind::Program, std::env::temp_dir()).unwrap();
        assert!(e.take_events().is_empty(), "only after a failure");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_nowhere_to_save_nothing_is_saved_and_nothing_is_said() {
        let mut e = engine(&[]);
        e.start_new("a", Vec::new(), AgentKind::Program, std::env::temp_dir()).unwrap();
        assert_eq!(e.save(), Ok(()));
        assert!(e.take_events().is_empty());
    }

    #[test]
    fn output_is_no_event_and_an_agent_already_closed_says_nothing_when_it_ends() {
        let mut e = engine(&["state=idle"]);
        e.handle(AgentEvent::Output(AgentId(1), b"words".to_vec()));
        assert!(e.take_events().is_empty());
        let Ok(Outcome::Asked(q)) = e.close(key(&e, 0)) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        e.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 1 }));
        assert!(e.take_events().is_empty());
    }

    fn report(e: &mut Engine<Fake>, id: u32, body: &str) {
        e.handle(AgentEvent::Output(AgentId(id), format!("\x1b]7501;{body}\x1b\\").into_bytes()));
    }

    #[test]
    fn beginning_to_need_you_is_said_once_for_each_time_it_begins() {
        let mut e = engine(&["state=idle"]);
        e.settle();
        assert!(e.take_events().is_empty());
        report(&mut e, 1, "state=blocked:kind=permission");
        e.settle();
        let began = Event::BeganNeedingYou { agent: key(&e, 0), label: e.ws.entries()[0].label() };
        assert_eq!(e.take_events(), std::slice::from_ref(&began), "it names the agent");
        e.settle();
        assert!(e.take_events().is_empty(), "still waiting: not said again");
        report(&mut e, 1, "state=working");
        report(&mut e, 1, "state=blocked");
        e.settle();
        assert_eq!(e.take_events(), [began], "it began again");
    }

    #[test]
    fn status_that_does_not_arrive_is_said_once_for_a_kind_and_taken_back_when_it_does() {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        for kind in [AgentKind::ClaudeCode, AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::Program] {
            ws.start_new("x", Vec::new(), kind, std::env::temp_dir()).unwrap();
        }
        let mut e = Engine::new(ws);
        let unavailable = |kind| Event::StatusUnavailable { kind, why: crate::status::Why::NotUnderstood };
        // A plain program is expected to report nothing: nothing is said.
        report(&mut e, 4, "state=pondering");
        e.settle();
        assert!(e.take_events().is_empty());

        report(&mut e, 1, "state=pondering:app=claude-code");
        e.settle();
        assert_eq!(e.take_events(), [unavailable(AgentKind::ClaudeCode)]);
        // A second agent of the same kind: said already.
        report(&mut e, 2, "state=pondering:app=claude-code");
        e.settle();
        assert!(e.take_events().is_empty(), "once for a kind");
        // Another kind is its own matter.
        report(&mut e, 3, "state=pondering:app=codex");
        e.settle();
        assert_eq!(e.take_events(), [unavailable(AgentKind::Codex)]);

        // One Claude Code agent reports after all; the other still does not.
        report(&mut e, 1, "state=idle:app=claude-code");
        e.settle();
        assert!(e.take_events().is_empty(), "still true of the kind");
        report(&mut e, 2, "state=idle:app=claude-code");
        e.settle();
        assert_eq!(e.take_events(), [Event::StatusBack { kind: AgentKind::ClaudeCode }]);
        // It fails again later: said again.
        report(&mut e, 2, "state=pondering:app=claude-code");
        e.settle();
        assert_eq!(e.take_events(), [unavailable(AgentKind::ClaudeCode)]);
        // Closing the last agent that shows it takes it back too.
        let codex = key(&e, 2);
        let Ok(Outcome::Asked(q)) = e.close(codex) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        e.settle();
        assert_eq!(e.take_events(), [Event::StatusBack { kind: AgentKind::Codex }]);
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
    }

    /// A report that arrives at a time the test names.
    fn report_at(e: &mut Engine<Fake>, n: usize, body: &str, when: SystemTime) {
        let key = key(e, n);
        let index = e.ws.index_of(key).unwrap();
        e.ws.entry_mut_for_test(index).track.received(body.as_bytes(), when);
    }

    #[test]
    fn the_copy_says_what_each_agent_is_doing_and_when_it_last_said_so() {
        let mut e = engine(&[]);
        for name in ["a", "b", "c"] {
            e.start_new(name, Vec::new(), AgentKind::Program, std::env::temp_dir().join("work")).unwrap();
        }
        report_at(&mut e, 0, "state=working", at(100));
        report_at(&mut e, 1, "state=blocked:kind=permission", at(110));
        let copy = e.copy(at(130));
        assert_eq!(copy.taken_at, at(130));
        assert_eq!(copy.agents.len(), 3);
        let [a, b, c] = [&copy.agents[0], &copy.agents[1], &copy.agents[2]];
        assert_eq!((a.key, a.name.as_str(), a.folder_name.as_str()), (key(&e, 0), "a", "work"));
        assert_eq!(a.label, e.ws.entries()[0].label());
        assert_eq!(a.kind, AgentKind::Program);
        assert!(a.is_running());
        // Working, and the report that said so arrived at 100: a reader
        // can say how long ago.
        assert_eq!(a.status, Some(AgentStatus::Working));
        assert_eq!((a.reported_at, a.needs_you, a.needs_you_since), (Some(at(100)), None, None));
        // Needs you, since 110.
        assert_eq!(b.needs_you, Some(Some(Kind::Permission)));
        assert_eq!((b.reported_at, b.needs_you_since), (Some(at(110)), Some(at(110))));
        // Not known carries its reason, and no time: nothing was reported.
        assert_eq!(c.status, Some(AgentStatus::Unknown(Why::NothingSent)));
        assert_eq!((c.reported_at, c.needs_you_since), (None, None));
        assert_eq!(copy.front, Some(key(&e, 2)), "the last started is in front");
        assert_eq!(copy.front_index(), Some(2));

        // It says it is still waiting: when it last reported moves; when it
        // began to need you does not.
        report_at(&mut e, 1, "state=blocked:kind=permission", at(200));
        let b = e.copy(at(201)).agents[1].clone();
        assert_eq!((b.reported_at, b.needs_you_since), (Some(at(200)), Some(at(110))));
        // It goes back to work and waits again: a new wait, timed anew.
        report_at(&mut e, 1, "state=working", at(300));
        assert_eq!(e.copy(at(301)).agents[1].needs_you_since, None);
        report_at(&mut e, 1, "state=blocked", at(400));
        let b = e.copy(at(401)).agents[1].clone();
        assert_eq!((b.needs_you, b.needs_you_since), (Some(None), Some(at(400))));

        // A report about one of its parts, and one that is not about its
        // status at all, do not make what it said look newer than it is.
        report_at(&mut e, 0, "state=done:id=helper", at(450));
        let a = e.copy(at(460)).agents[0].clone();
        assert_eq!((a.status, a.reported_at), (Some(AgentStatus::Working), Some(at(100))));
        // One that is not understood is what is shown, from when it came.
        report_at(&mut e, 0, "state=pondering", at(470));
        let a = e.copy(at(480)).agents[0].clone();
        assert_eq!(
            (a.status, a.reported_at),
            (Some(AgentStatus::Unknown(Why::NotUnderstood)), Some(at(470)))
        );

        // Its program ends: how it ended, and nothing reported.
        e.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 3 }));
        let b = e.copy(at(500)).agents[1].clone();
        assert_eq!(b.program, crate::workspace::Status::Exited(crate::agent::Exit { code: 3 }));
        assert!(!b.is_running());
        assert_eq!((b.status, b.needs_you, b.needs_you_since, b.reported_at), (None, None, None, None));
    }

    #[test]
    fn a_waiting_part_times_the_wait_from_when_the_agent_began_to_need_you() {
        let mut e = engine(&["state=working"]);
        report_at(&mut e, 0, "state=blocked:id=helper", at(50));
        report_at(&mut e, 0, "state=working", at(60));
        let a = e.copy(at(70)).agents[0].clone();
        assert_eq!(a.needs_you, Some(None), "its part is waiting");
        assert_eq!(a.needs_you_since, Some(at(50)));
        assert_eq!(a.reported_at, Some(at(60)));
    }

    #[test]
    fn what_a_shell_prompt_ended_and_a_new_start_carry_no_time_from_before() {
        let mut e = engine(&["state=idle"]);
        report_at(&mut e, 0, "state=blocked", at(10));
        // The shell's prompt came back: whatever was waiting has gone.
        e.handle(AgentEvent::Output(AgentId(1), b"\x1b]133;A\x1b\\".to_vec()));
        let a = e.copy(at(20)).agents[0].clone();
        assert_eq!(a.status, Some(AgentStatus::Unknown(Why::AtPrompt)));
        assert_eq!((a.needs_you_since, a.reported_at), (None, None), "nothing reported what is shown");
        // It needs you again, stops, and is started again: a clean slate.
        report_at(&mut e, 0, "state=blocked", at(30));
        assert_eq!(e.copy(at(31)).agents[0].needs_you_since, Some(at(30)));
        e.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert_eq!(e.start_again(key(&e, 0)), Ok(false));
        let a = e.copy(at(40)).agents[0].clone();
        assert_eq!((a.needs_you, a.needs_you_since, a.reported_at), (None, None, None));
    }

    #[test]
    fn the_copy_says_which_two_are_on_screen_only_while_they_are() {
        let mut e = engine(&["state=idle", "state=idle"]);
        assert_eq!(e.copy(at(1)).shown_two, None);
        e.set_room(WIDE);
        let _ = e.show_two();
        let (a, b) = (key(&e, 0), key(&e, 1));
        assert_eq!(e.copy(at(1)).shown_two, Some((b, a)), "left, then right");
        // Asked for and no longer fitting: not on screen, so not said.
        e.set_room(NARROW);
        assert_eq!(e.copy(at(1)).shown_two, None);
        e.set_room(WIDE);
        assert_eq!(e.copy(at(1)).shown_two, Some((b, a)));
        // An open question is said, in a word.
        assert_eq!(e.copy(at(1)).question, None);
        let _ = e.close(a);
        assert_eq!(e.copy(at(1)).question, Some("close"));
    }

    #[test]
    fn the_copy_is_the_same_until_something_changes_and_names_its_run() {
        let mut e = engine(&["state=idle"]);
        let first = e.copy(at(9));
        assert_eq!(first, e.copy(at(9)));
        assert_eq!(first.as_of, 0);
        // A second engine is another run.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_ne!(engine(&[]).copy(at(9)).run, first.run);
        report(&mut e, 1, "state=blocked");
        e.settle();
        let then = e.copy(at(9));
        assert_ne!(first, then);
        assert_eq!(then.as_of, 1, "it carries the number of the last event");
    }

    #[test]
    fn events_are_numbered_and_a_reader_gets_everything_after_the_number_it_names() {
        let mut e = engine(&["state=idle", "state=idle"]);
        assert_eq!(e.last_event(), 0);
        assert_eq!(e.events_after(0), EventsAfter { missed: 0, events: vec![], last: 0 });
        e.handle(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        e.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 1 }));
        assert_eq!(e.last_event(), 2);
        let all = e.events_after(0);
        assert_eq!((all.missed, all.last), (0, 2));
        let numbers: Vec<u64> = all.events.iter().map(|(number, _)| *number).collect();
        assert_eq!(numbers, [1, 2], "numbered from 1, oldest first");
        assert!(matches!(&all.events[1].1, Event::Exited { exit, .. } if exit.code == 1));
        // Asking takes nothing: asked again, the same answer.
        assert_eq!(e.events_after(0), all);
        assert_eq!(e.events_after(1).events.len(), 1);
        assert!(e.events_after(2).events.is_empty());
        // A number from some other run: nothing, and `last` shows it.
        let other = e.events_after(40);
        assert_eq!((other.missed, other.events.len(), other.last), (0, 0, 2));
        // The terminal face takes each once, and that takes nothing from
        // any other reader.
        assert_eq!(e.take_events().len(), 2);
        assert!(e.take_events().is_empty());
        assert_eq!(e.events_after(0).events.len(), 2);
        // Said plainly for a reader on another thread.
        let published = e.published(at(5), &[key(&e, 0).number(), key(&e, 1).number()]);
        assert_eq!(published.state.as_of, 2);
        assert_eq!(published.events_let_go, 0);
        let plain: Vec<(u64, &str)> = published.events.iter().map(|ev| (ev.number, ev.what)).collect();
        assert_eq!(plain, [(1, "exited"), (2, "exited")]);
        // Each names the agent it happened to by its key, not only in words.
        assert_eq!(published.events[0].agent, Some(key(&e, 0)));
        assert_eq!(published.events[1].agent, Some(key(&e, 1)));
        assert_eq!(published.events[0].label.as_deref(), Some(e.ws.entries()[0].label().as_str()));
        assert_eq!(published.screens.len(), 2, "a screen for each agent asked for");
        assert_eq!(published.screens[0].0, key(&e, 0));
        // Only those asked for: a screen is the costly part to copy.
        assert!(e.published(at(5), &[]).screens.is_empty());
        let one = e.published(at(5), &[key(&e, 1).number(), 999]);
        assert_eq!(one.screens.len(), 1);
        assert_eq!(one.screens[0].0, key(&e, 1));
    }

    #[test]
    fn only_the_latest_events_are_kept_and_a_late_reader_is_told_how_many_it_missed() {
        let mut e = engine(&["state=idle"]);
        let a = key(&e, 0);
        // Each round: it stops, and is started again.
        let rounds = EVENTS_KEPT as u64 + 10;
        for round in 0..rounds {
            e.handle(AgentEvent::Exited(AgentId(round as u32 + 1), crate::agent::Exit { code: 0 }));
            assert_eq!(e.start_again(a), Ok(false));
        }
        assert_eq!(e.last_event(), rounds);
        let all = e.events_after(0);
        assert_eq!(all.events.len(), EVENTS_KEPT);
        assert_eq!(all.missed, 10, "ten are gone for good");
        assert_eq!(all.events[0].0, 11, "the oldest kept");
        assert_eq!(all.events.last().unwrap().0, rounds);
        // A reader that was only a little behind missed nothing.
        let near = e.events_after(rounds - 3);
        assert_eq!((near.missed, near.events.len()), (0, 3));
        // One that had read up to 4 missed 5 to 10.
        assert_eq!(e.events_after(4).missed, 6);
        assert_eq!(e.published(at(1), &[]).events_let_go, 10);
        // (The terminal face takes after every batch and never falls this
        // far behind; that is asserted where it takes.)
    }

    #[test]
    fn beginning_to_need_you_is_an_event_that_names_the_agent_for_any_reader() {
        let mut e = engine(&["state=idle", "state=idle"]);
        report(&mut e, 2, "state=blocked:kind=question");
        e.settle();
        let plain = e.published(at(1), &[]).events;
        assert_eq!(plain.len(), 1);
        assert_eq!((plain[0].what, plain[0].agent), ("began-needing-you", Some(key(&e, 1))));
    }

    const ONE: Size = Size { rows: 30, cols: 120 };
    const TWO: Size = Size { rows: 29, cols: 59 };
    const WIDE: Room = Room { one: ONE, two: Some(TWO) };
    const NARROW: Room = Room { one: Size { rows: 30, cols: 80 }, two: None };

    #[test]
    fn the_room_decides_the_size_and_two_only_where_they_fit() {
        let mut e = engine(&["state=idle", "state=idle"]);
        assert_eq!(e.set_room(WIDE), RoomChange::Same);
        assert_eq!(e.ws.size(), ONE);
        assert_eq!(e.show_two(), Ok(ViewOutcome::Done(Viewed::Two(Pair { left: 1, right: 0 }))));
        assert!(e.two_shown());
        assert_eq!(e.ws.size(), TWO, "every agent is told the size for two");

        // Narrowed: one shown, at one's size; the pair is kept.
        assert_eq!(e.set_room(NARROW), RoomChange::TwoGone);
        assert!(!e.two_shown() && e.ws.pair().is_some());
        assert_eq!(e.ws.size(), NARROW.one);
        // Widened again: two are back.
        assert_eq!(e.set_room(WIDE), RoomChange::TwoBack);
        assert_eq!(e.ws.size(), TWO);

        assert_eq!(e.show_one(), Ok(ViewOutcome::Done(Viewed::One)));
        assert_eq!(e.ws.size(), ONE);
        assert_eq!(e.set_room(NARROW), RoomChange::Same);
        assert_eq!(e.set_room(WIDE), RoomChange::Same, "one asked for: two do not come back");
    }

    #[test]
    fn two_are_refused_where_they_do_not_fit_then_where_there_are_too_few() {
        let mut e = engine(&["state=idle"]);
        e.set_room(NARROW);
        assert_eq!(e.show_two(), Err(Refused::TwoDoNotFit), "not fitting is said first");
        e.set_room(WIDE);
        assert_eq!(e.show_two(), Err(Refused::TooFewAgents(1)));
        assert_eq!(engine(&[]).show_two(), Err(Refused::TwoDoNotFit), "no room told yet: two do not fit");
        let mut none = engine(&[]);
        none.set_room(WIDE);
        assert_eq!(none.show_two(), Err(Refused::TooFewAgents(0)));
        assert_eq!(e.ws.size(), ONE, "nothing was resized");
        assert_eq!(e.question(), None);
    }

    #[test]
    fn one_when_two_did_not_fit_is_kept_without_a_resize_or_a_question() {
        let mut e = engine(&["state=blocked", "state=idle"]);
        e.set_room(WIDE);
        let Ok(ViewOutcome::Asked(q)) = e.show_two() else { panic!("asked: an agent needs you") };
        assert_eq!(e.answer(q.id, true), Ok(Answered::Viewed(Viewed::Two(Pair { left: 1, right: 0 }))));
        e.set_room(NARROW);
        // Someone still needs you, but nothing would be resized: no question.
        assert_eq!(e.show_one(), Ok(ViewOutcome::Done(Viewed::OneKept)));
        assert_eq!(e.ws.pair(), None);
        assert_eq!(e.ws.size(), NARROW.one);
    }

    #[test]
    fn a_change_of_view_is_asked_about_while_an_agent_needs_you() {
        let mut e = engine(&["state=idle", "state=blocked:kind=permission"]);
        e.set_room(WIDE);
        let Ok(ViewOutcome::Asked(q)) = e.show_two() else { panic!("asked") };
        assert_eq!(q.about, About::View { two: true });
        assert_eq!(e.ws.pair(), None, "nothing changes before the answer");
        assert_eq!(e.ws.size(), ONE);
        assert_eq!(e.show_two(), Ok(ViewOutcome::Asked(q)), "asked again: the same question");
        assert_eq!(e.quit(), Err(Refused::QuestionOpen));
        assert_eq!(e.answer(q.id, false), Ok(Answered::Nothing));
        assert_eq!(e.ws.pair(), None);

        // Yes, but the window narrowed while it was asked: refused then.
        let Ok(ViewOutcome::Asked(q)) = e.show_two() else { panic!("asked") };
        e.set_room(NARROW);
        assert_eq!(e.answer(q.id, true), Ok(Answered::ViewRefused(Refused::TwoDoNotFit)));
        assert_eq!(e.ws.pair(), None);

        // Back to one is asked about too.
        e.set_room(WIDE);
        let Ok(ViewOutcome::Asked(q)) = e.show_two() else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        let Ok(ViewOutcome::Asked(q)) = e.show_one() else { panic!("asked") };
        assert_eq!(q.about, About::View { two: false });
        assert_eq!(e.answer(q.id, true), Ok(Answered::Viewed(Viewed::One)));
        assert_eq!(e.ws.size(), ONE);
    }

    #[test]
    fn closing_one_of_two_gives_the_other_the_size_for_one() {
        let mut e = engine(&["state=idle", "state=idle"]);
        e.set_room(WIDE);
        let _ = e.show_two();
        let Ok(Outcome::Asked(q)) = e.close(key(&e, 1)) else { panic!("asked") };
        e.answer(q.id, true).unwrap();
        assert_eq!(e.ws.pair(), None);
        e.fit_agents();
        assert_eq!(e.ws.size(), ONE);
    }
}
