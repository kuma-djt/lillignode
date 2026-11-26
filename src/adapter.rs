//! The Board's adapter: one outside program that keeps the Board, started
//! by Lillignode and spoken to over its own pipe.
//!
//! Off unless the settings file names it (`board_adapter`). With it, the
//! Board's cards come from the adapter instead of `board.json`, and what
//! the reader does to a card goes to the adapter instead of the outbox
//! file. Nothing here knows what service the adapter speaks to.
//!
//! The pipe carries one JSON object a line, version 1.
//!
//! From the adapter, at any time:
//! - `{"hello": 1, "name": "the Hub"}`: first; the name says where the
//!   reporting comes from;
//! - `{"cards": [ … ]}`: the whole Board, each card as in `board.json`;
//! - `{"reporting": [ {"agent": "…", "line": "…", "at": "…"} ]}`: the
//!   newest line of each agent that reports through the adapter;
//! - `{"took": "a1"}` or `{"refused": "a1", "why": "…"}`: what came of an
//!   action;
//! - `{"problem": "…"}`: something it cannot do, said on the page.
//!
//! To the adapter:
//! - `{"hello": 1}` when it starts;
//! - `{"id": "a1", "action": { …the action's line, whole… }}`: an action,
//!   sent only once the time to undo it has passed and it was not undone.
//!   Only lines Lillignode wrote on the reader's press are ever held, so
//!   only those are ever sent.
//!
//! The adapter is not started again by itself if it stops: one that fails
//! at once would be started for ever. Held actions are then kept, and said.
//!
//! This file is a reader: it names nothing of the engine
//! (`tests/boundaries.rs`).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::board::{self, Board};

/// The pipe's version.
pub const VERSION: u64 = 1;

/// How long the adapter has to say hello.
pub const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// How long after an action was sent the reader is told there is no
/// answer yet.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// No line from the adapter may be longer than this; one that is ends it.
pub const LINE_MAX: usize = 1024 * 1024;

/// How many of the adapter's own last words (its error output) are kept.
const LAST_WORDS: usize = 5;

/// One agent that reports through the adapter, and its newest line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reporting {
    pub agent: String,
    pub line: String,
    /// Milliseconds since 1970, where the adapter said when.
    pub at_ms: Option<u64>,
}

/// An action held, sent, and what came of it.
#[derive(Clone, Debug)]
struct Held {
    id: String,
    card: String,
    title: String,
    action: String,
    line: String,
    /// Not sent before this.
    until: Instant,
    sent_at: Option<Instant>,
    /// None: no answer yet. Some(None): taken. Some(Some(why)): refused.
    answer: Option<Option<String>>,
}

#[derive(Default)]
struct Inner {
    name: Option<String>,
    hello: bool,
    board: Option<Board>,
    reporting: Vec<Reporting>,
    held: Vec<Held>,
    problem: Option<String>,
    /// Why it is not running, once it is not.
    stopped: Option<String>,
    last_words: VecDeque<String>,
    /// Lines it sent that are not one of the kinds above.
    unknown_lines: usize,
    next_id: u64,
}

enum Out {
    Line(String),
    Stop,
}

/// A running adapter. Clones share it.
#[derive(Clone)]
pub struct Link {
    inner: Arc<Mutex<Inner>>,
    out: Sender<Out>,
    child: Arc<Mutex<Option<Child>>>,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Seconds-and-text time ("2026-11-26T20:00:00Z") as milliseconds since 1970.
fn at_ms(value: &Value) -> Option<u64> {
    let text = value.as_str()?;
    board::parse_time(text).and_then(|secs| u64::try_from(secs).ok()).map(|secs| secs * 1000)
}

impl Link {
    /// Starts `command` with `args` in `folder`, with this program's
    /// environment, and says hello. Returns why if it could not be
    /// started; then nothing runs.
    pub fn start(
        command: &str,
        args: &[String],
        folder: &Path,
        folder_for_sent: Option<&Path>,
    ) -> Result<Link, String> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("The Board's adapter ({command}) could not be started: {e}."))?;
        let stdout = child.stdout.take().ok_or("no pipe from the adapter")?;
        let stderr = child.stderr.take().ok_or("no pipe from the adapter")?;
        let mut stdin = child.stdin.take().ok_or("no pipe to the adapter")?;
        let inner = Arc::new(Mutex::new(Inner::default()));
        let (out, out_rx) = mpsc::channel::<Out>();
        let link = Link { inner: inner.clone(), out, child: Arc::new(Mutex::new(Some(child))) };

        // Its words: read line by line, at most LINE_MAX each.
        let from = inner.clone();
        let sent_folder = folder_for_sent.map(Path::to_path_buf);
        std::thread::Builder::new()
            .name("board-adapter-in".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let why = loop {
                    let mut line = Vec::new();
                    match (&mut reader).take(LINE_MAX as u64 + 1).read_until(b'\n', &mut line) {
                        Ok(0) => break "it ended".to_string(),
                        Ok(_) if line.len() > LINE_MAX => {
                            break format!("it sent a line longer than {} bytes", LINE_MAX);
                        }
                        Ok(_) => heard(&from, &String::from_utf8_lossy(&line), sent_folder.as_deref()),
                        Err(e) => break format!("its words could not be read ({e})"),
                    }
                };
                stop(&from, why);
            })
            .map_err(|e| e.to_string())?;

        // Its error output: only the last few lines are kept, as its last words.
        let words = inner.clone();
        std::thread::Builder::new()
            .name("board-adapter-err".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut inner = lock(&words);
                    inner.last_words.push_back(line.chars().take(300).collect());
                    while inner.last_words.len() > LAST_WORDS {
                        inner.last_words.pop_front();
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        // What goes to it: hello, then each held action once its time is up.
        let to = inner.clone();
        let started = Instant::now();
        std::thread::Builder::new()
            .name("board-adapter-out".into())
            .spawn(move || speak(&to, &mut stdin, &out_rx, started))
            .map_err(|e| e.to_string())?;
        let _ = link.out.send(Out::Line(json!({ "hello": VERSION }).to_string()));
        Ok(link)
    }

    /// An adapter that could not be started: it says why wherever the
    /// Board would be, and refuses every action, so nothing goes to the
    /// Board's file instead.
    pub fn failed(why: &str) -> Link {
        let (out, _) = mpsc::channel::<Out>();
        let inner = Inner { stopped: Some(why.trim_end_matches('.').to_string()), ..Inner::default() };
        Link { inner: Arc::new(Mutex::new(inner)), out, child: Arc::new(Mutex::new(None)) }
    }

    /// The name the adapter gave itself, if it has said hello.
    pub fn name(&self) -> Option<String> {
        lock(&self.inner).name.clone()
    }

    /// The Board as the adapter last sent it, or, before it has, a Board
    /// that says why there is none.
    pub fn board(&self) -> Board {
        let inner = lock(&self.inner);
        match (&inner.board, &inner.stopped) {
            (Some(board), _) => board.clone(),
            (None, Some(why)) => {
                Board { problem: Some(stopped_words(why, &inner.last_words)), ..Board::default() }
            }
            (None, None) => Board {
                problem: Some(format!(
                    "Waiting for the Board from {}.",
                    inner.name.as_deref().unwrap_or("the adapter")
                )),
                ..Board::default()
            },
        }
    }

    /// Holds an action's line, to be sent once the time to undo it has
    /// passed. Refused, with why, if the adapter is not running.
    pub fn hold(&self, card: &str, title: &str, action: &str, line: &str) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        if let Some(why) = &inner.stopped {
            return Err(format!("{} So this cannot be sent.", stopped_words(why, &inner.last_words)));
        }
        inner.next_id += 1;
        let id = format!("a{}", inner.next_id);
        inner.held.push(Held {
            id,
            card: card.to_string(),
            title: title.to_string(),
            action: action.to_string(),
            line: line.to_string(),
            until: Instant::now() + board::UNDO_FOR,
            sent_at: None,
            answer: None,
        });
        Ok(())
    }

    /// Takes back a held line: true if it had not been sent yet.
    pub fn withdraw(&self, line: &str) -> bool {
        let mut inner = lock(&self.inner);
        match inner.held.iter().position(|h| h.line == line && h.sent_at.is_none()) {
            Some(at) => {
                inner.held.remove(at);
                true
            }
            None => false,
        }
    }

    /// What the page is told: who the adapter is, whether it runs, what
    /// came of each action, and who reports through it.
    pub fn to_json(&self, now: SystemTime) -> Value {
        let inner = lock(&self.inner);
        let now_ms = now.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let actions: Vec<Value> = inner
            .held
            .iter()
            .map(|h| {
                let (state, words) = match (&h.sent_at, &h.answer) {
                    (None, _) if inner.stopped.is_some() => ("held", "Not sent: the adapter stopped.".to_string()),
                    (None, _) => ("held", "Sent when the ten seconds to undo it are up.".to_string()),
                    (Some(_), Some(None)) => ("taken", "Taken by the adapter.".to_string()),
                    (Some(_), Some(Some(why))) => ("refused", format!("Refused by the adapter: {why}")),
                    (Some(at), None) if at.elapsed() >= ANSWER_WITHIN => {
                        ("late", "No answer from the adapter yet.".to_string())
                    }
                    (Some(_), None) => ("sent", "Sent to the adapter.".to_string()),
                };
                json!({ "card": h.card, "title": h.title, "action": h.action, "state": state, "words": words })
            })
            .collect();
        let reporting: Vec<Value> = inner
            .reporting
            .iter()
            .map(|r| json!({ "agent": r.agent, "line": r.line, "at": r.at_ms }))
            .collect();
        json!({
            "name": inner.name.clone().unwrap_or_else(|| "the adapter".to_string()),
            "running": inner.stopped.is_none(),
            "stopped": inner.stopped.as_ref().map(|why| stopped_words(why, &inner.last_words)),
            "problem": inner.problem,
            "actions": actions,
            "reporting": reporting,
            "unknown_lines": inner.unknown_lines,
            "now": now_ms,
        })
    }

    /// Ends the adapter. Actions still held are not sent.
    pub fn stop(&self) {
        let _ = self.out.send(Out::Stop);
        if let Some(mut child) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn stopped_words(why: &str, last_words: &VecDeque<String>) -> String {
    let last = last_words.back().map(|w| format!(" Its last words: \"{w}\".")).unwrap_or_default();
    format!(
        "The Board's adapter is not running: {why}.{last} Nothing can be acted on until Lillignode is started again."
    )
}

fn stop(inner: &Mutex<Inner>, why: String) {
    let mut inner = lock(inner);
    if inner.stopped.is_none() {
        inner.stopped = Some(why);
    }
}

/// One line from the adapter.
fn heard(shared: &Mutex<Inner>, line: &str, sent_folder: Option<&Path>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(Value::Object(said)) = serde_json::from_str::<Value>(line) else {
        lock(shared).unknown_lines += 1;
        return;
    };
    let mut inner = lock(shared);
    if said.get("hello").is_some() {
        if said["hello"] != VERSION {
            inner.problem = Some(format!(
                "The Board's adapter speaks version {} of the pipe; this Lillignode speaks {VERSION}.",
                said["hello"]
            ));
        }
        inner.hello = true;
        inner.name = said.get("name").and_then(Value::as_str).map(|n| n.chars().take(60).collect());
    } else if let Some(cards) = said.get("cards") {
        inner.board = Some(board::from_text(&json!({ "cards": cards }).to_string()));
    } else if let Some(Value::Array(list)) = said.get("reporting") {
        inner.reporting = list
            .iter()
            .filter_map(|r| {
                Some(Reporting {
                    agent: r.get("agent")?.as_str()?.chars().take(80).collect(),
                    line: r.get("line").and_then(Value::as_str).unwrap_or("").chars().take(300).collect(),
                    at_ms: r.get("at").and_then(at_ms),
                })
            })
            .take(100)
            .collect();
    } else if let Some(id) = said.get("took").and_then(Value::as_str) {
        if let Some(h) = inner.held.iter_mut().find(|h| h.id == id && h.sent_at.is_some()) {
            h.answer = Some(None);
        }
    } else if let Some(id) = said.get("refused").and_then(Value::as_str) {
        let why: String =
            said.get("why").and_then(Value::as_str).unwrap_or("no reason given").chars().take(300).collect();
        if let Some(h) = inner.held.iter_mut().find(|h| h.id == id && h.sent_at.is_some()) {
            h.answer = Some(Some(why));
            // Refused: the card no longer waits, so it can be acted on again.
            let line = h.line.clone();
            drop(inner);
            board::forget_sent(sent_folder, &line);
        }
    } else if let Some(problem) = said.get("problem").and_then(Value::as_str) {
        inner.problem = Some(problem.chars().take(300).collect());
    } else {
        inner.unknown_lines += 1;
    }
}

/// Writes to the adapter: what is asked, and each held action once its
/// time is up. Stops when told, or when the adapter can no longer be
/// written to.
fn speak(shared: &Mutex<Inner>, stdin: &mut impl Write, asked: &Receiver<Out>, started: Instant) {
    let mut write = |line: &str| writeln!(stdin, "{line}").and_then(|()| stdin.flush());
    loop {
        match asked.recv_timeout(Duration::from_millis(200)) {
            Ok(Out::Line(line)) => {
                if let Err(e) = write(&line) {
                    stop(shared, format!("it could not be written to ({e})"));
                    return;
                }
            }
            Ok(Out::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let due: Vec<(String, String)> = {
            let mut inner = lock(shared);
            if inner.stopped.is_some() {
                return;
            }
            if !inner.hello && started.elapsed() >= HELLO_WITHIN {
                drop(inner);
                stop(shared, "it said nothing in ten seconds".to_string());
                return;
            }
            if !inner.hello {
                continue;
            }
            let now = Instant::now();
            inner
                .held
                .iter_mut()
                .filter(|h| h.sent_at.is_none() && now >= h.until)
                .map(|h| {
                    h.sent_at = Some(now);
                    (h.id.clone(), h.line.clone())
                })
                .collect()
        };
        for (id, line) in due {
            let action: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            if let Err(e) = write(&json!({ "id": id, "action": action }).to_string()) {
                stop(shared, format!("it could not be written to ({e})"));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> Link {
        let (out, _) = mpsc::channel::<Out>();
        Link { inner: Arc::new(Mutex::new(Inner::default())), out, child: Arc::new(Mutex::new(None)) }
    }

    fn card(id: &str) -> Value {
        json!({ "id": id, "owner": "x", "kind": "decide", "tier": "now", "title": format!("title {id}"), "status": "queued" })
    }

    #[test]
    fn what_the_adapter_says_is_kept() {
        let l = link();
        heard(&l.inner, r#"{"hello": 1, "name": "the Hub"}"#, None);
        assert_eq!(l.name().as_deref(), Some("the Hub"));
        assert_eq!(l.board().problem.as_deref(), Some("Waiting for the Board from the Hub."));
        heard(&l.inner, &json!({ "cards": [card("c1"), card("c2")] }).to_string(), None);
        assert_eq!(l.board().decide.len(), 2);
        heard(
            &l.inner,
            r#"{"reporting": [{"agent": "ops", "line": "deployed", "at": "2026-11-26T20:00:00Z"}, {"line": "no agent"}]}"#,
            None,
        );
        let shown = l.to_json(SystemTime::now());
        assert_eq!(shown["reporting"][0]["agent"], "ops");
        assert_eq!(shown["reporting"][0]["at"], 1_795_723_200_000u64);
        assert_eq!(shown["reporting"].as_array().unwrap().len(), 1, "a line with no agent is left out");
        heard(&l.inner, "not json", None);
        heard(&l.inner, r#"{"something": "else"}"#, None);
        assert_eq!(l.to_json(SystemTime::now())["unknown_lines"], 2);
        heard(&l.inner, r#"{"problem": "no key"}"#, None);
        assert_eq!(l.to_json(SystemTime::now())["problem"], "no key");
    }

    #[test]
    fn another_version_of_the_pipe_is_said() {
        let l = link();
        heard(&l.inner, r#"{"hello": 2}"#, None);
        assert!(l.to_json(SystemTime::now())["problem"].as_str().unwrap().contains("version 2"));
    }

    #[test]
    fn an_action_waits_its_ten_seconds_and_can_be_taken_back_until_then() {
        let l = link();
        l.hold("c1", "title c1", "approve", r#"{"card":"c1"}"#).unwrap();
        assert!(l.withdraw(r#"{"card":"c1"}"#), "held: taken back");
        assert!(!l.withdraw(r#"{"card":"c1"}"#), "nothing left to take back");
        l.hold("c2", "title c2", "approve", r#"{"card":"c2"}"#).unwrap();
        // Written to an adapter that said hello: not before its time.
        lock(&l.inner).hello = true;
        let (tx, rx) = mpsc::channel::<Out>();
        let mut written: Vec<u8> = Vec::new();
        drop(tx);
        speak(&l.inner, &mut written, &rx, Instant::now());
        assert!(written.is_empty(), "inside the ten seconds nothing is sent");
        lock(&l.inner).held[0].until = Instant::now();
        let (tx, rx) = mpsc::channel::<Out>();
        tx.send(Out::Line("x".into())).unwrap();
        let shared = l.inner.clone();
        let mut writer: Vec<u8> = Vec::new();
        let handle = std::thread::spawn(move || {
            speak(&shared, &mut writer, &rx, Instant::now());
            writer
        });
        std::thread::sleep(Duration::from_millis(600));
        drop(tx);
        let written = String::from_utf8(handle.join().unwrap()).unwrap();
        assert!(written.contains(r#"{"action":{"card":"c2"},"id":"a2"}"#), "{written}");
        assert!(!l.withdraw(r#"{"card":"c2"}"#), "sent: no longer taken back");
        assert_eq!(l.to_json(SystemTime::now())["actions"][0]["state"], "sent");
        heard(&l.inner, r#"{"took": "a2"}"#, None);
        assert_eq!(l.to_json(SystemTime::now())["actions"][0]["state"], "taken");
    }

    #[test]
    fn nothing_is_sent_before_hello_and_silence_stops_it() {
        let l = link();
        l.hold("c1", "t", "approve", r#"{"card":"c1"}"#).unwrap();
        lock(&l.inner).held[0].until = Instant::now();
        let (_tx, rx) = mpsc::channel::<Out>();
        let mut written: Vec<u8> = Vec::new();
        speak(&l.inner, &mut written, &rx, Instant::now() - HELLO_WITHIN);
        assert!(written.is_empty());
        assert!(l.board().problem.unwrap().contains("it said nothing in ten seconds"));
        assert!(l.hold("c2", "t", "approve", "{}").unwrap_err().contains("cannot be sent"));
    }

    #[test]
    fn a_refusal_lets_the_card_be_acted_on_again() {
        let folder = std::env::temp_dir().join(format!("lillignode-adapter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let shown = board::from_text(&json!({ "cards": [card("c1")] }).to_string());
        let seen = board::Seen { title: "title c1".into(), recommendation: None };
        let l = link();
        let hold = |line: &str| l.hold("c1", "title c1", "approve", line);
        let acted =
            board::act_on(Some(&folder), &shown, "c1", "approve", "", &seen, SystemTime::now(), Some(&hold))
                .unwrap();
        assert!(!folder.join(board::OUTBOX).exists(), "nothing written to the outbox file");
        let again =
            board::act_on(Some(&folder), &shown, "c1", "approve", "", &seen, SystemTime::now(), Some(&hold));
        assert!(again.unwrap_err().contains("already sent"));
        lock(&l.inner).held[0].sent_at = Some(Instant::now());
        heard(&l.inner, r#"{"refused": "a1", "why": "the card moved on"}"#, Some(&folder));
        let shown_json = l.to_json(SystemTime::now());
        assert_eq!(shown_json["actions"][0]["words"], "Refused by the adapter: the card moved on");
        assert!(board::sent(Some(&folder)).unwrap().iter().all(|s| s.card != acted.card));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn an_adapter_that_could_not_start_says_why_and_takes_nothing() {
        let l = Link::failed("The Board's adapter (hub) could not be started: not found.");
        assert!(l.board().problem.unwrap().contains("could not be started: not found"));
        assert!(l.hold("c1", "t", "approve", "{}").is_err());
        assert_eq!(l.to_json(SystemTime::now())["running"], false);
    }
}
