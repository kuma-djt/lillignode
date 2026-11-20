//! The engine: the agents and the rules of the product, apart from any
//! screen or keyboard. A face reads what it holds and asks it, by name, to
//! do things.
//!
//! The rules are moving here from the terminal face (`app.rs`) one at a
//! time; until each has moved, the face still holds it. Quitting has moved.

use crate::agent::AgentBackend;
use crate::status::AgentStatus;
use crate::workspace::{Entry, Workspace};

/// Names one question the engine asked. An answer carries it, so a "yes"
/// is never applied to a question the asker did not see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuestionId(u64);

/// What a question is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum About {
    /// Quit, although an agent may be in the middle of something.
    Quit,
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
}

impl<B: AgentBackend> Engine<B> {
    pub fn new(ws: Workspace<B>) -> Self {
        Self { ws, question: None, asked: 0, closing: false }
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
    pub fn quit(&mut self) -> Outcome {
        if let Some(open) = self.question.filter(|q| q.about == About::Quit) {
            return Outcome::Asked(open);
        }
        if self.unfinished().is_empty() {
            self.closing = true;
            return Outcome::Done;
        }
        Outcome::Asked(self.ask(About::Quit))
    }

    /// The answer to the open question. Whatever the answer, the question
    /// is closed. A yes to quitting quits even if every agent has finished
    /// while it was open: the user saw the question and said yes to it.
    pub fn answer(&mut self, id: QuestionId, yes: bool) -> Result<(), Refused> {
        let question = self.question.filter(|q| q.id == id).ok_or(Refused::NoSuchQuestion)?;
        self.question = None;
        match question.about {
            About::Quit => self.closing = yes,
        }
        Ok(())
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
    use std::path::PathBuf;

    /// Starts anything.
    #[derive(Default)]
    struct Fake {
        next: u32,
    }

    impl AgentBackend for Fake {
        fn start(&mut self, _: &AgentSpec, _: Size) -> Result<AgentId, StartError> {
            self.next += 1;
            Ok(AgentId(self.next))
        }
        fn send(&mut self, _: AgentId, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn resize(&mut self, _: AgentId, _: Size) {}
        fn close(&mut self, _: AgentId) {}
    }

    /// Plain programs, `a` then `b` (ids 1 and 2), and how each reports.
    fn engine(reports: &[&str]) -> Engine<Fake> {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        for (n, body) in reports.iter().enumerate() {
            let name = ["a", "b"][n];
            ws.start_new(name, Vec::new(), AgentKind::Program, PathBuf::from("work")).unwrap();
            let report = format!("\x1b]7501;{body}\x1b\\");
            ws.handle(AgentEvent::Output(AgentId(n as u32 + 1), report.into_bytes()));
        }
        Engine::new(ws)
    }

    #[test]
    fn quit_closes_at_once_when_nothing_is_unfinished() {
        let mut e = engine(&["state=idle", "state=done"]);
        assert_eq!(e.quit(), Outcome::Done);
        assert!(e.closing());
        assert_eq!(e.question(), None);
    }

    #[test]
    fn quit_asks_while_an_agent_may_be_busy_and_the_answer_decides() {
        let mut e = engine(&["state=working", "state=idle"]);
        let Outcome::Asked(q) = e.quit() else { panic!("asked") };
        assert_eq!(q.about, About::Quit);
        assert_eq!(e.question(), Some(q));
        assert!(!e.closing());
        // Asked again while open: the same question.
        assert_eq!(e.quit(), Outcome::Asked(q));

        assert_eq!(e.answer(q.id, false), Ok(()));
        assert!(!e.closing());
        assert_eq!(e.question(), None);

        let Outcome::Asked(again) = e.quit() else { panic!("asked") };
        assert_ne!(again.id, q.id, "a new question has a new id");
        assert_eq!(e.answer(again.id, true), Ok(()));
        assert!(e.closing());
    }

    #[test]
    fn an_answer_to_a_question_that_is_not_open_changes_nothing() {
        let mut e = engine(&["state=working"]);
        let Outcome::Asked(old) = e.quit() else { panic!("asked") };
        e.answer(old.id, false).unwrap();
        // Answered already.
        assert_eq!(e.answer(old.id, true), Err(Refused::NoSuchQuestion));
        assert!(!e.closing());
        // An older question while a newer one is open.
        let Outcome::Asked(new) = e.quit() else { panic!("asked") };
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
        assert_eq!(e.quit(), Outcome::Done);
        assert!(e.closing());
    }

    #[test]
    fn an_agent_whose_status_is_not_known_counts_as_unfinished() {
        let mut ws = Workspace::new(Fake::default(), Size { rows: 24, cols: 80 });
        ws.start_new("a", Vec::new(), AgentKind::Program, PathBuf::from("work")).unwrap();
        let mut e = Engine::new(ws);
        assert_eq!(e.unfinished().len(), 1);
        // Not known is not finished.
        assert!(matches!(e.unfinished()[0].status, Some(AgentStatus::Unknown(_))));
        assert!(matches!(e.quit(), Outcome::Asked(_)));
    }

    #[test]
    fn settled_agents_and_stopped_agents_do_not_count() {
        let mut e = engine(&["state=error", "state=working"]);
        assert_eq!(e.unfinished().len(), 1, "an error is settled; working is not");
        // The second stops while it said Working.
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert!(e.unfinished().is_empty(), "{:?}", e.unfinished());
        assert_eq!(e.quit(), Outcome::Done);
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
        e.ws.start_new("claude", Vec::new(), AgentKind::ClaudeCode, PathBuf::from("work")).unwrap();
        // b stops.
        e.ws.handle(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        let fresh = e.fresh_next_time();
        assert_eq!(fresh.len(), 1, "{fresh:?}");
        assert!(fresh[0].starts_with("a "), "{fresh:?}");
    }
}
