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

use std::path::PathBuf;

use crate::agent::{AgentBackend, AgentEvent, Exit, Size, StartError};
use crate::cost::{Cost, WhyNot};
use crate::kind::AgentKind;
use crate::state::{self, SavedWorkspace};
use crate::status::{AgentStatus, Why};
use crate::workspace::{AgentKey, Entry, Happened, NoPair, Pair, RestoreReport, Workspace};

/// Something that happened, for a face to say in its own words. A face
/// takes them in the order they happened (`Engine::take_events`). They are
/// hints: whatever can be read from the engine at any time (a status, who
/// needs you, a cost) is not an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An agent's program ended.
    Exited { label: String, exit: Exit },
    /// An agent could not continue its earlier conversation and was started
    /// again with a new one.
    StartedNewConversation { label: String },
    /// As above, but the fresh start failed too.
    CouldNotContinue { label: String, why: String },
    /// The workspace could not be saved after a change. What was saved
    /// before is left as it was; nothing in memory is lost.
    SaveFailed { why: String },
    /// An agent has just begun to need the user: the edge a bell rings on.
    /// Whether to ring is the face's setting. Who needs you now is read
    /// from the agents, not from this.
    BeganNeedingYou,
    /// The status of an agent of this kind, which is expected to report it,
    /// is not reaching Lillignode. Once for each kind, until it is back.
    StatusUnavailable { kind: AgentKind, why: Why },
    /// No agent of this kind shows that any more: what was said about it
    /// has stopped being true. If it fails again it is said again.
    StatusBack { kind: AgentKind },
    /// The cost of this agent was looked for and could not be read. Once in
    /// a run.
    CostUnreadable { label: String },
    /// This agent's conversation has passed the user's cost warning, at
    /// this many tokens. Once for a conversation. A warning only: nothing
    /// was stopped.
    PassedCostWarning { label: String, tokens: u64 },
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
    /// What has happened since a face last took them.
    events: Vec<Event>,
    /// The kinds of agent whose status has been said not to reach
    /// Lillignode, and has not come back since.
    status_said: Vec<AgentKind>,
    /// A cost that could not be read has been said in this run.
    cost_said: bool,
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
            events: Vec::new(),
            status_said: Vec::new(),
            cost_said: false,
        }
    }

    /// Call after each batch of agent output and each tick. What has become
    /// true since the last call, and is said once, becomes an event, in
    /// this order: an agent began to need you; status that was said to be
    /// unavailable is back; status is unavailable, for one kind of agent at
    /// a time; a cost could not be read; a cost warning was passed.
    pub fn settle(&mut self) {
        if self.ws.take_began_needing_you() {
            self.events.push(Event::BeganNeedingYou);
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
        self.events.extend(back.into_iter().map(|kind| Event::StatusBack { kind }));
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
            self.events.push(Event::StatusUnavailable { kind, why });
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
                self.events.push(Event::CostUnreadable { label: entry.label() });
            }
        }
        for (label, tokens) in self.ws.take_passed_cost_warning() {
            self.events.push(Event::PassedCostWarning { label, tokens });
        }
    }

    /// For a test of something else: as if status had already been said to
    /// be unavailable for these kinds.
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
        if let Err(why) = self.save() {
            self.events.push(Event::SaveFailed { why });
        }
    }

    /// What has happened since the last call, oldest first.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
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
            Happened::Exited { label, exit } => Event::Exited { label, exit },
            Happened::StartedNewConversation { label } => {
                // The new conversation's id is kept.
                self.changed();
                Event::StartedNewConversation { label }
            }
            Happened::CouldNotContinue { label, why } => Event::CouldNotContinue { label, why },
        };
        self.events.push(event);
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
        assert_eq!(e.take_events(), [Event::BeganNeedingYou]);
        e.settle();
        assert!(e.take_events().is_empty(), "still waiting: not said again");
        report(&mut e, 1, "state=working");
        report(&mut e, 1, "state=blocked");
        e.settle();
        assert_eq!(e.take_events(), [Event::BeganNeedingYou], "it began again");
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
