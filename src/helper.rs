//! An advisory helper: one outside program, named in the settings, that
//! Lillignode may ask what an agent seems to be doing when the agent says
//! nothing itself. Off unless named.
//!
//! The helper only guesses. Its answer is shown as a guess, marked as one,
//! and never as the agent's own report: it never makes an agent need you,
//! rings no bell, and ends the moment the agent's screen changes or the
//! agent reports. A late answer, an unsure one, or one that cannot be read
//! is no guess.
//!
//! It is asked only about an agent whose own status is Unknown and whose
//! text has been still for `STILL_FOR`, once for each still screen. It is
//! given the agent's name and the text on its screen, and nothing else of
//! Lillignode: no keys, no files, no other agent.
//!
//! One line of JSON each way, over the helper's own pipes:
//!
//!   to it:    {"hello":1}
//!             {"ask":7,"agent":"Claude Code (web)","screen":["line", ...]}
//!   from it:  {"answer":7,"guess":"waiting"}      waiting, working, done or unsure
//!
//! Anything else it says is passed over. If it ends, or cannot be read,
//! nothing more is asked and the status report says why.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The version of the lines above.
pub const VERSION: u64 = 1;

/// How long an agent's text must be still before the helper is asked.
pub const STILL_FOR: Duration = Duration::from_secs(2);

/// An answer that comes later than this after the question is no guess.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(3);

/// No more lines of an agent's screen than this are sent.
const MOST_LINES: usize = 200;

/// A line from the helper longer than this stops it.
const LINE_MAX: usize = 16 * 1024;

/// No more questions than this wait to be written to it. A helper that
/// reads none of them is stopped, not queued for.
const QUEUE: usize = 8;

/// No more answers than this are kept waiting to be taken.
const MOST_ANSWERS: usize = 64;

/// What of this machine's environment the helper is given, besides the
/// names its settings list: what a program needs to run at all.
const BASE_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

/// What a helper may guess an agent is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guess {
    /// Waiting for the user.
    Waiting,
    Working,
    Done,
}

impl Guess {
    pub fn word(self) -> &'static str {
        match self {
            Guess::Waiting => "waiting",
            Guess::Working => "working",
            Guess::Done => "done",
        }
    }
}

/// What came of one question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Guess(Guess),
    /// It said it is unsure, or its answer could not be read: no guess.
    Unsure,
}

#[derive(Default)]
struct Inner {
    answers: HashMap<u64, (Answer, Instant)>,
    /// The questions asked and not yet answered: an answer to any other
    /// is passed over, so none can come before its question.
    open: Vec<u64>,
    /// Why it stopped, once it has.
    stopped: Option<String>,
}

/// A running helper.
pub struct Helper {
    inner: Arc<Mutex<Inner>>,
    out: SyncSender<String>,
    child: Arc<Mutex<Option<Child>>>,
    command: String,
}

/// Marks it stopped, with why (the first why stands), and ends its
/// program, so nothing of it is left running once it is not listened to.
fn give_up(inner: &Mutex<Inner>, child: &Mutex<Option<Child>>, why: String) {
    lock(inner).stopped.get_or_insert(why);
    if let Some(mut child) = lock(child).take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Helper {
    /// Starts `command` with `args` in `folder`, with only the environment
    /// a program needs plus the variables named in `env`, and says hello.
    /// Returns why if it could not be started; then nothing runs.
    pub fn start(command: &str, args: &[String], env: &[String], folder: &Path) -> Result<Helper, String> {
        let mut run = Command::new(command);
        run.args(args).current_dir(folder).env_clear();
        for name in BASE_ENV.iter().copied().chain(env.iter().map(String::as_str)) {
            if let Some(value) = std::env::var_os(name) {
                run.env(name, value);
            }
        }
        let mut child = run
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("The helper ({command}) could not be started: {e}."))?;
        let stdout = child.stdout.take().ok_or("no pipe from the helper")?;
        let mut stdin = child.stdin.take().ok_or("no pipe to the helper")?;
        let inner = Arc::new(Mutex::new(Inner::default()));
        let child = Arc::new(Mutex::new(Some(child)));
        let (out, out_rx): (SyncSender<String>, Receiver<String>) = mpsc::sync_channel(QUEUE);

        let (heard, its) = (inner.clone(), child.clone());
        std::thread::Builder::new()
            .name("helper-in".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let why = loop {
                    let mut line = Vec::new();
                    match (&mut reader).take(LINE_MAX as u64 + 2).read_until(b'\n', &mut line) {
                        Ok(0) => break "it ended".to_string(),
                        Ok(_) if line.len() > LINE_MAX && !line.ends_with(b"\n") => {
                            break format!("it sent a line longer than {LINE_MAX} bytes")
                        }
                        Ok(_) => {
                            if let Some((ask, answer)) = read_answer(&String::from_utf8_lossy(&line)) {
                                let mut inner = lock(&heard);
                                // Only to a question asked and still open,
                                // and never more than can be taken.
                                if inner.open.contains(&ask) && inner.answers.len() < MOST_ANSWERS {
                                    inner.open.retain(|open| *open != ask);
                                    inner.answers.insert(ask, (answer, Instant::now()));
                                }
                            }
                        }
                        Err(e) => break format!("its words could not be read ({e})"),
                    }
                };
                give_up(&heard, &its, why);
            })
            .map_err(|e| format!("The helper ({command}) could not be listened to: {e}."))?;
        let (said, its) = (inner.clone(), child.clone());
        std::thread::Builder::new()
            .name("helper-out".into())
            .spawn(move || {
                for line in out_rx {
                    if stdin
                        .write_all(line.as_bytes())
                        .and_then(|_| stdin.write_all(b"\n"))
                        .and_then(|_| stdin.flush())
                        .is_err()
                    {
                        give_up(&said, &its, "it stopped taking questions".to_string());
                        return;
                    }
                }
            })
            .map_err(|e| format!("The helper ({command}) could not be spoken to: {e}."))?;
        let helper = Helper { inner, out, child, command: command.to_string() };
        let _ = helper.out.try_send(json!({ "hello": VERSION }).to_string());
        Ok(helper)
    }

    /// The program the settings name.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Asks question `ask` about the agent named `agent`, with the text on
    /// its screen. Answered later, if at all (`answer`).
    pub fn ask(&self, ask: u64, agent: &str, screen: &str) {
        if self.stopped().is_some() {
            return;
        }
        let mut lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let from = lines.len().saturating_sub(MOST_LINES);
        let line = json!({ "ask": ask, "agent": agent, "screen": &lines[from..] }).to_string();
        lock(&self.inner).open.push(ask);
        // A helper that has not read the questions before it is not
        // reading: stopped, so nothing piles up for it.
        match self.out.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                give_up(&self.inner, &self.child, "it stopped reading questions".to_string())
            }
            Err(TrySendError::Disconnected(_)) => {
                give_up(&self.inner, &self.child, "it stopped taking questions".to_string())
            }
        }
    }

    /// The answer to question `ask`, once it has come, with when it came.
    /// Taken: asked again, it is not there.
    pub fn answer(&self, ask: u64) -> Option<(Answer, Instant)> {
        lock(&self.inner).answers.remove(&ask)
    }

    /// Questions no longer waited for, and their answers, are let go.
    pub fn forget_all_but(&self, keep: &[u64]) {
        let mut inner = lock(&self.inner);
        inner.answers.retain(|ask, _| keep.contains(ask));
        inner.open.retain(|ask| keep.contains(ask));
    }

    /// Why it stopped, once it has. Nothing more is asked.
    pub fn stopped(&self) -> Option<String> {
        lock(&self.inner).stopped.clone()
    }

    /// Stops it. Its answers are no longer taken.
    pub fn stop(&self) {
        give_up(&self.inner, &self.child, "Lillignode stopped it".to_string());
    }
}

#[cfg(test)]
impl Helper {
    /// A helper with no program, for tests: what it is asked comes out of
    /// the receiver, and `answered` puts in what it says.
    pub fn detached() -> (Helper, Receiver<String>) {
        let (out, asked) = mpsc::sync_channel(QUEUE * 4);
        let helper = Helper {
            inner: Arc::new(Mutex::new(Inner::default())),
            out,
            child: Arc::new(Mutex::new(None)),
            command: "test-helper".to_string(),
        };
        (helper, asked)
    }

    /// As if the helper had answered `ask` at `at`, by the same rule as a
    /// real answer: only to a question asked and still open.
    pub fn answered(&self, ask: u64, answer: Answer, at: Instant) {
        let mut inner = lock(&self.inner);
        if inner.open.contains(&ask) {
            inner.open.retain(|open| *open != ask);
            inner.answers.insert(ask, (answer, at));
        }
    }

    /// As if it had ended.
    pub fn ended(&self, why: &str) {
        lock(&self.inner).stopped.get_or_insert(why.to_string());
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One line from the helper: the question it answers, and its answer. A
/// line that answers nothing is passed over; an answer that cannot be read
/// is unsure.
fn read_answer(line: &str) -> Option<(u64, Answer)> {
    let said: Value = serde_json::from_str(line.trim()).ok()?;
    let ask = said.get("answer")?.as_u64()?;
    let answer = match said.get("guess").and_then(Value::as_str) {
        Some("waiting") => Answer::Guess(Guess::Waiting),
        Some("working") => Answer::Guess(Guess::Working),
        Some("done") => Answer::Guess(Guess::Done),
        _ => Answer::Unsure,
    };
    Some((ask, answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_and_anything_else_is_unsure_or_passed_over() {
        assert_eq!(
            read_answer(r#"{"answer":7,"guess":"waiting"}"#),
            Some((7, Answer::Guess(Guess::Waiting)))
        );
        assert_eq!(
            read_answer(r#"{"answer":7,"guess":"working"}"#),
            Some((7, Answer::Guess(Guess::Working)))
        );
        assert_eq!(read_answer(r#"{"answer":7,"guess":"done"}"#), Some((7, Answer::Guess(Guess::Done))));
        for unsure in
            [r#"{"answer":7,"guess":"unsure"}"#, r#"{"answer":7,"guess":"Waiting"}"#, r#"{"answer":7}"#]
        {
            assert_eq!(read_answer(unsure), Some((7, Answer::Unsure)), "{unsure}");
        }
        for nothing in ["", "hello", r#"{"guess":"done"}"#, r#"{"answer":-1,"guess":"done"}"#, "[1]"] {
            assert_eq!(read_answer(nothing), None, "{nothing}");
        }
    }

    #[test]
    fn a_program_that_is_not_there_is_said_and_nothing_runs() {
        let why = Helper::start("no-such-helper-program", &[], &[], &std::env::temp_dir()).err().unwrap();
        assert!(why.starts_with("The helper (no-such-helper-program) could not be started"), "{why}");
    }
}
