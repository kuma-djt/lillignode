//! The engine: the agents and the rules of the product, apart from any
//! screen or keyboard. A face reads what it holds and asks it, by name, to
//! do things.
//!
//! The rules are moving here from the terminal face (`app.rs`) one at a
//! time; until each has moved, the face still holds it. Quitting, closing
//! an agent and starting one again have moved.

use crate::agent::AgentBackend;
use crate::status::AgentStatus;
use crate::workspace::{AgentKey, Entry, Workspace};

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
}

impl<B: AgentBackend> Engine<B> {
    pub fn new(ws: Workspace<B>) -> Self {
        Self { ws, question: None, asked: 0, closing: false, held: None }
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
        self.ws.restart(agent).ok_or(Refused::NoSuchAgent)
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
                Some(label) => Answered::Closed { label },
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
}
