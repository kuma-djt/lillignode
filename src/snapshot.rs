//! A plain copy of what the engine holds, for a face to read.
//!
//! The copy is finished data: it holds no reference into the engine and
//! nothing of the terminal emulator, so it can be handed to another thread
//! or written out. It says how old it is (`taken_at`) and which event was
//! the last to happen before it was taken (`as_of`), so a reader can say
//! "as of" and can tell that it has fallen behind.
//!
//! **The copy is the truth; events are hints that something changed.** No
//! rule may depend on a reader having seen an event.
//!
//! A second face on another thread is given a `Reader`: the reading half of
//! `channel()`. It can read the latest copy and wait for a newer one. It
//! has no way to ask the engine to do anything. The one thing it can say is
//! which agents' screens it is showing, so that only those are copied.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use crate::cost::{self, Cost};
use crate::kind::AgentKind;
use crate::status::{AgentStatus, Kind};
use crate::terminal::ScreenCopy;
use crate::workspace::{AgentKey, Status};

/// One agent, as it was when the copy was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCopy {
    /// Names the agent for as long as this Lillignode runs.
    pub key: AgentKey,
    /// The name the sidebar shows.
    pub name: String,
    /// The folder it works in, and that folder's own name.
    pub folder: PathBuf,
    pub folder_name: String,
    /// Its name and folder, as sentences name it.
    pub label: String,
    pub kind: AgentKind,
    /// Whether its program is running, or how it ended.
    pub program: Status,
    /// What it last said it is doing. None once its program is not
    /// running. Unknown carries its reason.
    pub status: Option<AgentStatus>,
    /// Some while it needs the user, with what for when it said.
    pub needs_you: Option<Option<Kind>>,
    /// When it began to need the user, while it does.
    pub needs_you_since: Option<SystemTime>,
    /// When the program last reported its own status: the report `status`
    /// rests on. A report about one of its parts does not move it. None if
    /// it has reported none since it was started, if a shell's prompt has
    /// ended what it said, or if it is not running.
    pub reported_at: Option<SystemTime>,
    /// When what is on its screen last changed, by something it sent.
    /// Output that draws the same screen again, or nothing, does not move
    /// it. None if it has changed nothing since it was started. Kept after
    /// its program ends: it is when its last screen was drawn.
    pub screen_changed_at: Option<SystemTime>,
    /// What its conversation has used, or why that is not known.
    pub cost: Cost,
    /// Its count of tokens, if that is past the user's warning.
    pub over_cost_warning: Option<u64>,
    /// It was refused, and starting it again as it is would be refused too.
    pub start_again_cannot_help: bool,
}

impl AgentCopy {
    pub fn is_running(&self) -> bool {
        self.program == Status::Running
    }
}

/// Everything a face reads, as it was at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateCopy {
    /// Tells one run of Lillignode from the next: agent keys and event
    /// numbers begin again with each run.
    pub run: u64,
    /// The number of the last event that happened before this was taken.
    pub as_of: u64,
    pub taken_at: SystemTime,
    /// In the order of the list.
    pub agents: Vec<AgentCopy>,
    /// The agent typing goes to.
    pub front: Option<AgentKey>,
    /// The two side by side, left then right, while two are on screen.
    pub shown_two: Option<(AgentKey, AgentKey)>,
    /// What the open question is about, in a word, if one is open.
    pub question: Option<&'static str>,
    /// Whether cost is read at all.
    pub cost_on: bool,
    pub cost_summary: cost::Summary,
}

impl StateCopy {
    pub fn agent(&self, key: AgentKey) -> Option<&AgentCopy> {
        self.agents.iter().find(|a| a.key == key)
    }

    /// Where the agent in front is in the list.
    pub fn front_index(&self) -> Option<usize> {
        let front = self.front?;
        self.agents.iter().position(|a| a.key == front)
    }
}

/// One numbered event, said plainly: which kind of thing happened and to
/// which agent, if to one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventCopy {
    pub number: u64,
    /// A fixed word for what happened, such as `began-needing-you`.
    pub what: &'static str,
    pub agent: Option<AgentKey>,
    pub label: Option<String>,
}

/// What is handed to a reader on another thread: the state, each agent's
/// screen as text, and the latest events.
#[derive(Clone, Debug)]
pub struct Published {
    pub state: StateCopy,
    /// The screens a reader said it is showing, each by its agent's key.
    pub screens: Vec<(AgentKey, ScreenCopy)>,
    /// The events still kept, oldest first.
    pub events: Vec<EventCopy>,
    /// How many events have happened that are no longer kept.
    pub events_let_go: u64,
}

struct Shared {
    latest: Mutex<Latest>,
    newer: Condvar,
    /// The agents whose screens readers are showing now, by key number,
    /// once for each reader showing one.
    shown: Mutex<Vec<u64>>,
}

#[derive(Default)]
struct Latest {
    /// Counts what has been published. Zero: nothing yet.
    count: u64,
    copy: Option<Arc<Published>>,
    /// The publishing half is gone: nothing newer will come.
    ended: bool,
}

/// The writing half: the engine's loop puts each finished copy here.
pub struct Publisher(Arc<Shared>);

/// The reading half. It can read and wait, and nothing else.
#[derive(Clone)]
pub struct Reader(Arc<Shared>);

/// A place to publish copies, and the half that reads them.
pub fn channel() -> (Publisher, Reader) {
    let shared = Arc::new(Shared {
        latest: Mutex::new(Latest::default()),
        newer: Condvar::new(),
        shown: Mutex::new(Vec::new()),
    });
    (Publisher(shared.clone()), Reader(shared))
}

/// A reader that died while reading leaves nothing half written: the lock
/// is held only to swap one pointer. So a poisoned lock is still good.
fn locked(shared: &Shared) -> std::sync::MutexGuard<'_, Latest> {
    shared.latest.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Publisher {
    /// Put a finished copy where readers find it. Never waits on a reader:
    /// the lock is held for as long as it takes to swap a pointer.
    pub fn publish(&self, copy: Published) {
        let copy = Arc::new(copy);
        let mut latest = locked(&self.0);
        latest.count += 1;
        let earlier = latest.copy.replace(copy);
        drop(latest);
        // The copy before, if no reader still holds it, is freed here:
        // outside the lock, which is held only for the swap.
        drop(earlier);
        self.0.newer.notify_all();
    }
}

impl Publisher {
    /// The agents whose screens some reader is showing now, by key number.
    /// Only these need copying; with no reader, none.
    pub fn screens_shown(&self) -> Vec<u64> {
        let mut shown = self.0.shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        shown.sort_unstable();
        shown.dedup();
        shown
    }
}

/// Held by a reader for as long as it shows one agent's screen.
pub struct Showing {
    shared: Arc<Shared>,
    agent: u64,
}

impl Drop for Showing {
    fn drop(&mut self) {
        let mut shown = self.shared.shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(at) = shown.iter().position(|agent| *agent == self.agent) {
            shown.swap_remove(at);
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        locked(&self.0).ended = true;
        self.0.newer.notify_all();
    }
}

/// What a reader got when it asked.
#[derive(Clone, Debug)]
pub enum Read {
    /// A copy, and its count: later copies have higher counts.
    Copy(u64, Arc<Published>),
    /// Nothing newer came in the time given.
    NothingNewer,
    /// The engine has gone. Nothing newer will ever come.
    Ended,
}

impl Reader {
    /// Say that this reader is showing the screen of the agent with this
    /// key number, until what is returned is dropped. Copies published
    /// from then on carry that screen.
    pub fn show(&self, agent: u64) -> Showing {
        self.0.shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(agent);
        Showing { shared: self.0.clone(), agent }
    }

    /// The latest copy, if any has been published.
    pub fn latest(&self) -> Option<(u64, Arc<Published>)> {
        let latest = locked(&self.0);
        latest.copy.clone().map(|copy| (latest.count, copy))
    }

    /// Wait up to `limit` for a copy newer than the one counted `after`.
    pub fn newer_than(&self, after: u64, limit: Duration) -> Read {
        let latest = locked(&self.0);
        let (latest, _) = self
            .0
            .newer
            .wait_timeout_while(latest, limit, |l| l.count <= after && !l.ended)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &latest.copy {
            Some(copy) if latest.count > after => Read::Copy(latest.count, copy.clone()),
            _ if latest.ended => Read::Ended,
            _ => Read::NothingNewer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(as_of: u64) -> Published {
        Published {
            state: StateCopy {
                run: 1,
                as_of,
                taken_at: SystemTime::UNIX_EPOCH,
                agents: Vec::new(),
                front: None,
                shown_two: None,
                question: None,
                cost_on: false,
                cost_summary: cost::Summary::new(),
            },
            screens: Vec::new(),
            events: Vec::new(),
            events_let_go: 0,
        }
    }

    #[test]
    fn a_reader_gets_the_latest_copy_and_waits_only_for_a_newer_one() {
        let (publisher, reader) = channel();
        assert!(reader.latest().is_none());
        assert!(matches!(reader.newer_than(0, Duration::from_millis(1)), Read::NothingNewer));
        publisher.publish(published(3));
        publisher.publish(published(4));
        let (count, copy) = reader.latest().unwrap();
        assert_eq!((count, copy.state.as_of), (2, 4), "the latest, and nothing queues up");
        // It has that one: nothing newer.
        assert!(matches!(reader.newer_than(2, Duration::from_millis(1)), Read::NothingNewer));
        // Behind by any amount, it is given the latest at once.
        assert!(matches!(reader.newer_than(0, Duration::from_secs(5)), Read::Copy(2, _)));
    }

    #[test]
    fn a_waiting_reader_is_woken_by_a_new_copy_and_told_when_the_engine_has_gone() {
        let (publisher, reader) = channel();
        let waiting = std::thread::spawn(move || {
            let first = reader.newer_than(0, Duration::from_secs(10));
            let then = reader.newer_than(1, Duration::from_secs(10));
            (first, then)
        });
        publisher.publish(published(1));
        std::thread::sleep(Duration::from_millis(50));
        drop(publisher);
        let (first, then) = waiting.join().unwrap();
        assert!(matches!(first, Read::Copy(1, _)), "{first:?}");
        assert!(matches!(then, Read::Ended), "{then:?}");
    }

    #[test]
    fn a_copy_published_before_the_engine_went_is_still_read_and_then_it_is_said_to_be_gone() {
        let (publisher, reader) = channel();
        publisher.publish(published(1));
        drop(publisher);
        assert!(matches!(reader.newer_than(0, Duration::from_secs(30)), Read::Copy(1, _)));
        let began = std::time::Instant::now();
        assert!(matches!(reader.newer_than(1, Duration::from_secs(30)), Read::Ended));
        assert!(began.elapsed() < Duration::from_secs(5), "told at once, not after the wait");
    }

    #[test]
    fn only_the_screens_readers_are_showing_are_asked_for_and_only_while_they_show_them() {
        let (publisher, reader) = channel();
        assert!(publisher.screens_shown().is_empty(), "no reader: no screen is copied");
        let first = reader.show(4);
        let second = reader.clone().show(4);
        let other = reader.show(2);
        assert_eq!(publisher.screens_shown(), [2, 4]);
        drop(first);
        assert_eq!(publisher.screens_shown(), [2, 4], "another reader still shows it");
        drop(second);
        assert_eq!(publisher.screens_shown(), [2]);
        drop(other);
        assert!(publisher.screens_shown().is_empty());
    }

    #[test]
    fn publishing_does_not_wait_for_a_reader_that_never_reads_or_has_died() {
        let (publisher, reader) = channel();
        let dead = reader.clone();
        // A reader that panics while it holds nothing leaves the lock good;
        // one that panicked inside would poison it. Either way publishing
        // goes on.
        let _ = std::thread::spawn(move || {
            let _held = dead.0.latest.lock().unwrap();
            panic!("a reader died");
        })
        .join();
        let started = std::time::Instant::now();
        for n in 0..1000 {
            publisher.publish(published(n));
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(reader.latest().unwrap().0, 1000);
    }
}
