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
//! - `{"took": "a1"}`, `{"refused": "a1", "why": "…"}`, or
//!   `{"unsure": "a1", "why": "…"}` (it may or may not have reached the
//!   service): what came of an action;
//! - `{"problem": "…"}`: something it cannot do, said on the page until
//!   the next Board arrives.
//!
//! To the adapter:
//! - `{"hello": 1}` when it starts;
//! - `{"id": "a1", "action": { …the action's line, whole… }}`: an action,
//!   sent only once the time to undo it has passed and it was not undone.
//!   Only `act_on`, on the reader's press, ever holds a line here.
//!
//! What waits: an action held for its ten seconds is kept only here, in
//! memory. Only once it is really sent is it written to the Board's record
//! of what waits (`board::SENT`), so that file never says "sent" of
//! something that was not. If Lillignode quits or the adapter stops while
//! an action is held, it is not sent, and nothing says it was.
//!
//! The adapter is not started again by itself if it stops: one that fails
//! at once would be started for ever.
//!
//! This file is a reader: it names nothing of the engine
//! (`tests/boundaries.rs`).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::board::{self, Board, Sent};

/// The pipe's version.
pub const VERSION: u64 = 1;

/// How long the adapter has to say hello.
pub const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// How long after an action was sent the reader is told there is no
/// answer yet.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// A Board not heard again for this long is said to be old, and nothing on
/// it is acted on: it may no longer be what the service has.
pub const BOARD_STALE: Duration = Duration::from_secs(60);

/// No line from the adapter may be longer than this; one that is ends it.
pub const LINE_MAX: usize = 1024 * 1024;

/// The adapter's last words (its error output) are kept to one line of
/// this many characters, and said only when it stops.
const LAST_WORDS_MAX: usize = 100;

/// No more than this many actions are kept to show.
const ACTIONS_KEPT: usize = 50;

/// What of this machine's environment an adapter is given, besides the
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

/// One agent that reports through the adapter, and its newest line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reporting {
    pub agent: String,
    pub line: String,
    /// Milliseconds since 1970, where the adapter said when.
    pub at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Took,
    Refused(String),
    Unsure(String),
}

/// An action held, sent, and what came of it.
#[derive(Clone, Debug)]
struct Held {
    id: String,
    title: String,
    entry: Sent,
    /// Not sent before this.
    until: Instant,
    sent_at: Option<Instant>,
    answer: Option<Answer>,
}

#[derive(Default)]
struct Inner {
    name: Option<String>,
    hello: bool,
    board: Option<Board>,
    board_at: Option<Instant>,
    reporting: Vec<Reporting>,
    held: Vec<Held>,
    problem: Option<String>,
    /// Why it is not running, once it is not.
    stopped: Option<String>,
    last_words: Option<String>,
    /// Lines it sent that are not one of the kinds above.
    unknown_lines: usize,
    next_id: u64,
}

enum Out {
    Line(String),
    Stop,
}

/// The parts every thread shares.
struct Shared {
    inner: Mutex<Inner>,
    child: Mutex<Option<Child>>,
    /// Where `board::SENT` is kept, and one lock for every change to it.
    folder: Option<PathBuf>,
    sent_lock: Mutex<()>,
}

/// A running adapter. Clones share it.
#[derive(Clone)]
pub struct Link {
    shared: Arc<Shared>,
    out: Sender<Out>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Seconds-and-text time ("2026-11-26T20:00:00Z") as milliseconds since 1970.
fn at_ms(value: &Value) -> Option<u64> {
    let text = value.as_str()?;
    board::parse_time(text).and_then(|secs| u64::try_from(secs).ok()).map(|secs| secs * 1000)
}

fn ms_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Shared {
    fn new(folder: Option<&Path>, inner: Inner) -> Shared {
        Shared {
            inner: Mutex::new(inner),
            child: Mutex::new(None),
            folder: folder.map(Path::to_path_buf),
            sent_lock: Mutex::new(()),
        }
    }

    /// It is not running from now on, for this reason: the program is
    /// ended, and nothing it says after is taken.
    fn stop(&self, why: String) {
        {
            let mut inner = lock(&self.inner);
            if inner.stopped.is_none() {
                inner.stopped = Some(why);
            }
        }
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn stopped(&self) -> bool {
        lock(&self.inner).stopped.is_some()
    }
}

impl Link {
    /// Starts `command` with `args` in `folder`, with only the environment
    /// a program needs plus the variables named in `env`, and says hello.
    /// `folder` is also where `board::SENT` is kept. Returns why if it
    /// could not be started; then nothing runs.
    pub fn start(command: &str, args: &[String], env: &[String], folder: &Path) -> Result<Link, String> {
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
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("The Board's adapter ({command}) could not be started: {e}."))?;
        let stdout = child.stdout.take().ok_or("no pipe from the adapter")?;
        let stderr = child.stderr.take().ok_or("no pipe from the adapter")?;
        let mut stdin = child.stdin.take().ok_or("no pipe to the adapter")?;
        let shared = Arc::new(Shared::new(Some(folder), Inner::default()));
        *lock(&shared.child) = Some(child);
        let (out, out_rx) = mpsc::channel::<Out>();
        let link = Link { shared: shared.clone(), out };

        // Its words: read line by line, at most LINE_MAX each.
        let from = shared.clone();
        let spawned = std::thread::Builder::new().name("board-adapter-in".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            let why = loop {
                match read_line(&mut reader) {
                    Ok(Some(line)) => heard(&from, &line),
                    Ok(None) => break "it ended".to_string(),
                    Err(why) => break why,
                }
                if from.stopped() {
                    return;
                }
            };
            from.stop(why);
        });
        // Its error output is drained, whatever it is, so the adapter never
        // waits on it; only its last line is kept, cut short.
        let words = shared.clone();
        let spawned = spawned.and_then(|_| {
            std::thread::Builder::new().name("board-adapter-err".into()).spawn(move || {
                let mut reader = BufReader::new(stderr);
                while let Ok(Some(line)) = read_line(&mut reader) {
                    let line = line.trim();
                    if !line.is_empty() {
                        lock(&words.inner).last_words = Some(line.chars().take(LAST_WORDS_MAX).collect());
                    }
                }
            })
        });
        // What goes to it: hello, then each held action once its time is up.
        let to = shared.clone();
        let started = Instant::now();
        let spawned = spawned.and_then(|_| {
            std::thread::Builder::new()
                .name("board-adapter-out".into())
                .spawn(move || speak(&to, &mut stdin, &out_rx, started))
        });
        if let Err(e) = spawned {
            shared.stop(format!("it could not be listened to ({e})"));
        }
        let _ = link.out.send(Out::Line(json!({ "hello": VERSION }).to_string()));
        Ok(link)
    }

    /// An adapter that could not be started: it says why wherever the
    /// Board would be, and refuses every action, so nothing goes to the
    /// Board's file instead.
    pub fn failed(why: &str) -> Link {
        let (out, _) = mpsc::channel::<Out>();
        let inner = Inner { stopped: Some(why.trim_end_matches('.').to_string()), ..Inner::default() };
        Link { shared: Arc::new(Shared::new(None, inner)), out }
    }

    /// The name the adapter gave itself, if it has said hello.
    pub fn name(&self) -> Option<String> {
        lock(&self.shared.inner).name.clone()
    }

    /// The Board as the adapter last sent it, or, before it has, a Board
    /// that says why there is none.
    pub fn board(&self) -> Board {
        let inner = lock(&self.shared.inner);
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

    /// The actions held and not yet sent, as waiting: a card is not acted
    /// on twice, and the page shows it with Undo.
    pub fn held(&self) -> Vec<Sent> {
        let inner = lock(&self.shared.inner);
        // Once it has stopped, nothing held will be sent: none of it waits.
        if inner.stopped.is_some() {
            return Vec::new();
        }
        inner.held.iter().filter(|h| h.sent_at.is_none()).map(|h| h.entry.clone()).collect()
    }

    /// Holds an action, to be sent once the time to undo it has passed.
    /// Refused, with why, if the adapter is not running, has not said
    /// hello, or its Board is old.
    pub fn hold(&self, title: &str, entry: &Sent) -> Result<(), String> {
        let mut inner = lock(&self.shared.inner);
        if let Some(why) = &inner.stopped {
            return Err(format!("{} So this cannot be sent.", stopped_words(why, &inner.last_words)));
        }
        let name = inner.name.clone().unwrap_or_else(|| "the adapter".to_string());
        match inner.board_at {
            None => return Err(format!("No Board has come from {name} yet, so nothing can be acted on.")),
            Some(at) if at.elapsed() >= BOARD_STALE => {
                return Err(format!(
                "The Board was last heard from {name} {} seconds ago, so it may no longer be what is there. \
                     Act on it once it is heard from again.",
                at.elapsed().as_secs()
            ))
            }
            Some(_) => {}
        }
        inner.next_id += 1;
        let id = format!("a{}", inner.next_id);
        inner.held.push(Held {
            id,
            title: title.to_string(),
            entry: entry.clone(),
            until: Instant::now() + board::UNDO_FOR,
            sent_at: None,
            answer: None,
        });
        Ok(())
    }

    /// Takes back the action held for `card`, if it has not been sent:
    /// what it was, or None if there is none still held.
    pub fn withdraw(&self, card: &str) -> Option<(String, Sent)> {
        let mut inner = lock(&self.shared.inner);
        let at = inner.held.iter().position(|h| h.entry.card == card && h.sent_at.is_none())?;
        let was = inner.held.remove(at);
        Some((was.title, was.entry))
    }

    /// What the page is told: who the adapter is, whether it runs, how
    /// long since its Board, what came of each action, and who reports
    /// through it.
    pub fn to_json(&self, now: SystemTime) -> Value {
        let inner = lock(&self.shared.inner);
        let now_ms = now.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let name = inner.name.clone().unwrap_or_else(|| "the adapter".to_string());
        let actions: Vec<Value> = inner
            .held
            .iter()
            .rev()
            .take(ACTIONS_KEPT)
            .rev()
            .map(|h| {
                let (state, words) = match (&h.sent_at, &h.answer) {
                    (None, _) if inner.stopped.is_some() => {
                        ("not-sent", format!("Not sent: {name} stopped before its ten seconds were up."))
                    }
                    (None, _) => ("held", "Sent when the ten seconds to undo it are up.".to_string()),
                    (Some(_), Some(Answer::Took)) => ("taken", format!("Taken by {name}.")),
                    (Some(_), Some(Answer::Refused(why))) => ("refused", format!("Refused by {name}: {why}")),
                    (Some(_), Some(Answer::Unsure(why))) => (
                        "unsure",
                        format!(
                            "Not known whether {name} has it ({why}). Look there before acting on it again."
                        ),
                    ),
                    (Some(at), None) if at.elapsed() >= ANSWER_WITHIN => {
                        ("late", format!("No answer from {name} yet."))
                    }
                    (Some(_), None) => ("sent", format!("Sent to {name}.")),
                };
                json!({ "card": h.entry.card, "title": h.title, "action": h.entry.action,
                        "state": state, "words": words })
            })
            .collect();
        let reporting: Vec<Value> = inner
            .reporting
            .iter()
            .map(|r| json!({ "agent": r.agent, "line": r.line, "at": r.at_ms }))
            .collect();
        json!({
            "name": name,
            "running": inner.stopped.is_none(),
            "stopped": inner.stopped.as_ref().map(|why| stopped_words(why, &inner.last_words)),
            "problem": inner.problem,
            "board_quiet_secs": inner.board_at.map(|at| at.elapsed().as_secs()),
            "actions": actions,
            "reporting": reporting,
            "unknown_lines": inner.unknown_lines,
            "now": now_ms,
        })
    }

    /// Ends the adapter. Actions still held are not sent, and nothing
    /// says they were.
    pub fn stop(&self) {
        let _ = self.out.send(Out::Stop);
        self.shared.stop("Lillignode closed".to_string());
    }
}

fn stopped_words(why: &str, last_words: &Option<String>) -> String {
    let last = last_words.as_ref().map(|w| format!(" Its last words: \"{w}\".")).unwrap_or_default();
    format!(
        "The Board's adapter is not running: {why}.{last} Nothing can be acted on until Lillignode is started again."
    )
}

/// One line, without its line break: None at the end, or why it could not
/// be read. A line longer than LINE_MAX is an error, whatever its bytes.
fn read_line(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    match reader.take(LINE_MAX as u64 + 2).read_until(b'\n', &mut line) {
        Ok(0) => Ok(None),
        Ok(_) => {
            while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                line.pop();
            }
            if line.len() > LINE_MAX {
                return Err(format!("it sent a line longer than {LINE_MAX} bytes"));
            }
            Ok(Some(String::from_utf8_lossy(&line).into_owned()))
        }
        Err(e) => Err(format!("its words could not be read ({e})")),
    }
}

/// One line from the adapter. Read before the lock is taken, so a large
/// Board does not hold up the page.
fn heard(shared: &Shared, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(Value::Object(said)) = serde_json::from_str::<Value>(line) else {
        lock(&shared.inner).unknown_lines += 1;
        return;
    };
    if said.get("hello").is_some() {
        if said["hello"] != VERSION {
            shared.stop(format!(
                "it speaks version {} of the pipe, and this Lillignode speaks {VERSION}",
                said["hello"]
            ));
            return;
        }
        let mut inner = lock(&shared.inner);
        inner.hello = true;
        inner.name = said.get("name").and_then(Value::as_str).map(|n| n.chars().take(60).collect());
    } else if let Some(cards) = said.get("cards") {
        let board = board::from_text(&json!({ "cards": cards }).to_string());
        let mut inner = lock(&shared.inner);
        inner.board = Some(board);
        inner.board_at = Some(Instant::now());
        // A Board that came through means what kept it from coming is over.
        inner.problem = None;
    } else if let Some(Value::Array(list)) = said.get("reporting") {
        let reporting = list
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
        lock(&shared.inner).reporting = reporting;
    } else if let Some((id, answer)) = answer_of(&said) {
        let found = {
            let inner = lock(&shared.inner);
            inner
                .held
                .iter()
                .find(|h| h.id == id && h.sent_at.is_some() && h.answer.is_none())
                .map(|h| h.entry.clone())
        };
        let Some(entry) = found else { return };
        // Refused: the card no longer waits, so it can be acted on again.
        // Took, or not known: it still waits. The record is put right
        // before the answer is shown, so the page never shows a refusal on
        // a card still marked as waiting.
        if matches!(answer, Answer::Refused(_)) {
            let _kept = lock(&shared.sent_lock);
            board::forget_sent(shared.folder.as_deref(), entry.line());
        }
        if let Some(h) = lock(&shared.inner).held.iter_mut().find(|h| h.id == id) {
            h.answer = Some(answer);
        }
    } else if let Some(problem) = said.get("problem") {
        lock(&shared.inner).problem = problem.as_str().map(|p| p.chars().take(300).collect());
    } else {
        lock(&shared.inner).unknown_lines += 1;
    }
}

/// What came of an action, if this line says.
fn answer_of(said: &serde_json::Map<String, Value>) -> Option<(String, Answer)> {
    let why =
        || said.get("why").and_then(Value::as_str).unwrap_or("no reason given").chars().take(300).collect();
    if let Some(id) = said.get("took").and_then(Value::as_str) {
        return Some((id.to_string(), Answer::Took));
    }
    if let Some(id) = said.get("refused").and_then(Value::as_str) {
        return Some((id.to_string(), Answer::Refused(why())));
    }
    if let Some(id) = said.get("unsure").and_then(Value::as_str) {
        return Some((id.to_string(), Answer::Unsure(why())));
    }
    None
}

/// Writes to the adapter: what is asked, and each held action once its
/// time is up. An action is kept in `board::SENT` as waiting before it is
/// written, so a card is never shown as free while its action is on its
/// way. Stops when told, or when the adapter can no longer be written to.
fn speak(shared: &Shared, stdin: &mut impl Write, asked: &Receiver<Out>, started: Instant) {
    let mut write = |line: &str| writeln!(stdin, "{line}").and_then(|()| stdin.flush());
    loop {
        match asked.recv_timeout(Duration::from_millis(200)) {
            Ok(Out::Line(line)) => {
                if let Err(e) = write(&line) {
                    shared.stop(format!("it could not be written to ({e})"));
                    return;
                }
            }
            Ok(Out::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let due: Vec<(String, Sent)> = {
            let mut inner = lock(&shared.inner);
            if inner.stopped.is_some() {
                return;
            }
            if !inner.hello {
                if started.elapsed() >= HELLO_WITHIN {
                    drop(inner);
                    shared.stop("it said nothing in ten seconds".to_string());
                    return;
                }
                continue;
            }
            let now = Instant::now();
            inner
                .held
                .iter_mut()
                .filter(|h| h.sent_at.is_none() && now >= h.until)
                .map(|h| {
                    h.sent_at = Some(now);
                    (h.id.clone(), h.entry.clone())
                })
                .collect()
        };
        for (id, entry) in due {
            {
                let _kept = lock(&shared.sent_lock);
                let sent = Sent::new(&entry.card, &entry.action, ms_now(), entry.line());
                let _ = board::keep_sent(shared.folder.as_deref(), &sent);
            }
            let action: Value = serde_json::from_str(entry.line()).unwrap_or(Value::Null);
            if let Err(e) = write(&json!({ "id": id, "action": action }).to_string()) {
                // Not delivered after all: not kept as sent.
                let _kept = lock(&shared.sent_lock);
                board::forget_sent(shared.folder.as_deref(), entry.line());
                shared.stop(format!("it could not be written to ({e})"));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_in(folder: Option<&Path>) -> Link {
        let (out, _) = mpsc::channel::<Out>();
        Link { shared: Arc::new(Shared::new(folder, Inner::default())), out }
    }

    fn link() -> Link {
        link_in(None)
    }

    fn card(id: &str) -> Value {
        json!({ "id": id, "owner": "x", "kind": "decide", "tier": "now", "title": format!("title {id}"), "status": "queued" })
    }

    fn entry(card: &str) -> Sent {
        Sent::new(card, "approve", 0, &format!(r#"{{"card":"{card}"}}"#))
    }

    /// A link that has said hello and sent a Board.
    fn ready(folder: Option<&Path>) -> Link {
        let l = link_in(folder);
        heard(&l.shared, r#"{"hello": 1, "name": "the Hub"}"#);
        heard(&l.shared, &json!({ "cards": [card("c1"), card("c2"), card("c5")] }).to_string());
        l
    }

    fn temp(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("lillignode-adapter-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// Runs the writing half until the held actions due are written.
    fn speak_once(l: &Link) -> String {
        let (tx, rx) = mpsc::channel::<Out>();
        let shared = l.shared.clone();
        let handle = std::thread::spawn(move || {
            let mut writer: Vec<u8> = Vec::new();
            speak(&shared, &mut writer, &rx, Instant::now());
            writer
        });
        std::thread::sleep(Duration::from_millis(600));
        drop(tx);
        String::from_utf8(handle.join().unwrap()).unwrap()
    }

    #[test]
    fn what_the_adapter_says_is_kept() {
        let l = link();
        heard(&l.shared, r#"{"hello": 1, "name": "the Hub"}"#);
        assert_eq!(l.name().as_deref(), Some("the Hub"));
        assert_eq!(l.board().problem.as_deref(), Some("Waiting for the Board from the Hub."));
        heard(&l.shared, &json!({ "cards": [card("c1"), card("c2")] }).to_string());
        assert_eq!(l.board().decide.len(), 2);
        heard(
            &l.shared,
            r#"{"reporting": [{"agent": "ops", "line": "deployed", "at": "2026-11-26T20:00:00Z"}, {"line": "no agent"}]}"#,
        );
        let shown = l.to_json(SystemTime::now());
        assert_eq!(shown["reporting"][0]["agent"], "ops");
        assert_eq!(shown["reporting"][0]["at"], 1_795_723_200_000u64);
        assert_eq!(shown["reporting"].as_array().unwrap().len(), 1, "a line with no agent is left out");
        heard(&l.shared, "not json");
        heard(&l.shared, r#"{"something": "else"}"#);
        assert_eq!(l.to_json(SystemTime::now())["unknown_lines"], 2);
    }

    #[test]
    fn a_problem_lasts_until_the_next_board() {
        let l = ready(None);
        heard(&l.shared, r#"{"problem": "the Hub could not be reached"}"#);
        assert_eq!(l.to_json(SystemTime::now())["problem"], "the Hub could not be reached");
        heard(&l.shared, &json!({ "cards": [card("c1")] }).to_string());
        assert!(l.to_json(SystemTime::now())["problem"].is_null());
    }

    #[test]
    fn another_version_of_the_pipe_stops_it_and_nothing_is_sent() {
        let l = link();
        heard(&l.shared, r#"{"hello": 2}"#);
        assert!(l.board().problem.unwrap().contains("version 2"));
        assert!(!lock(&l.shared.inner).hello);
        assert!(l.hold("t", &entry("c1")).is_err());
    }

    #[test]
    fn an_action_waits_its_ten_seconds_and_can_be_taken_back_until_then() {
        let folder = temp("ten");
        let l = ready(Some(&folder));
        l.hold("title c1", &entry("c1")).unwrap();
        assert_eq!(l.held().len(), 1, "held: shown as waiting");
        assert!(l.withdraw("c1").is_some(), "held: taken back");
        assert!(l.withdraw("c1").is_none(), "nothing left to take back");
        l.hold("title c2", &entry("c2")).unwrap();
        assert!(!speak_once(&l).contains("action"), "inside the ten seconds nothing is sent");
        assert!(board::sent(Some(&folder)).unwrap().is_empty(), "and nothing is kept as sent");
        lock(&l.shared.inner).held[0].until = Instant::now();
        let written = speak_once(&l);
        assert!(written.contains(r#"{"action":{"card":"c2"},"id":"a2"}"#), "{written}");
        assert!(l.withdraw("c2").is_none(), "sent: no longer taken back");
        let kept = board::sent(Some(&folder)).unwrap();
        assert_eq!(kept.iter().map(|s| s.card.as_str()).collect::<Vec<_>>(), ["c2"], "sent: kept as waiting");
        assert_eq!(l.to_json(SystemTime::now())["actions"][0]["state"], "sent");
        heard(&l.shared, r#"{"took": "a2"}"#);
        assert_eq!(l.to_json(SystemTime::now())["actions"][0]["words"], "Taken by the Hub.");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn an_action_held_when_it_stops_is_not_sent_and_not_kept_as_sent() {
        let folder = temp("stop");
        let l = ready(Some(&folder));
        l.hold("title c1", &entry("c1")).unwrap();
        l.stop();
        lock(&l.shared.inner).held[0].until = Instant::now();
        assert!(!speak_once(&l).contains("action"));
        assert!(board::sent(Some(&folder)).unwrap().is_empty(), "nothing says it was sent");
        assert_eq!(l.to_json(SystemTime::now())["actions"][0]["state"], "not-sent");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn nothing_is_sent_before_hello_and_silence_stops_it() {
        let l = link();
        heard(&l.shared, &json!({ "cards": [card("c1")] }).to_string());
        lock(&l.shared.inner).held.push(Held {
            id: "a1".into(),
            title: "t".into(),
            entry: entry("c1"),
            until: Instant::now(),
            sent_at: None,
            answer: None,
        });
        let (_tx, rx) = mpsc::channel::<Out>();
        let mut written: Vec<u8> = Vec::new();
        speak(&l.shared, &mut written, &rx, Instant::now() - HELLO_WITHIN);
        assert!(written.is_empty());
        assert!(l.board().problem.is_none(), "the last Board is still shown");
        assert!(l.to_json(SystemTime::now())["stopped"]
            .as_str()
            .unwrap()
            .contains("it said nothing in ten seconds"));
        assert!(l.hold("t", &entry("c2")).unwrap_err().contains("cannot be sent"));
    }

    #[test]
    fn nothing_is_acted_on_before_a_board_or_on_an_old_one() {
        let l = link();
        heard(&l.shared, r#"{"hello": 1, "name": "the Hub"}"#);
        assert!(l.hold("t", &entry("c1")).unwrap_err().contains("No Board has come"));
        heard(&l.shared, &json!({ "cards": [card("c1")] }).to_string());
        lock(&l.shared.inner).board_at = Some(Instant::now() - BOARD_STALE);
        assert!(l.hold("t", &entry("c1")).unwrap_err().contains("last heard from the Hub"));
    }

    #[test]
    fn a_refusal_lets_the_card_be_acted_on_again_and_unsure_does_not() {
        let folder = temp("refused");
        let l = ready(Some(&folder));
        let shown = l.board();
        let seen = board::Seen { title: "title c1".into(), recommendation: None };
        let hold1 = |e: &Sent| l.hold("title c1", e);
        let held = || board::Held { hold: &hold1, waiting: l.held() };
        board::act_on(Some(&folder), &shown, "c1", "approve", "", &seen, SystemTime::now(), Some(held()))
            .unwrap();
        assert!(!folder.join(board::OUTBOX).exists(), "nothing written to the outbox file");
        let again =
            board::act_on(Some(&folder), &shown, "c1", "approve", "", &seen, SystemTime::now(), Some(held()));
        assert!(again.unwrap_err().contains("already sent"), "held: not acted on twice");
        lock(&l.shared.inner).held[0].until = Instant::now();
        speak_once(&l);
        heard(&l.shared, r#"{"refused": "a1", "why": "the card moved on"}"#);
        assert_eq!(
            l.to_json(SystemTime::now())["actions"][0]["words"],
            "Refused by the Hub: the card moved on"
        );
        assert!(board::sent(Some(&folder)).unwrap().is_empty(), "refused: no longer waits");
        let seen5 = board::Seen { title: "title c5".into(), recommendation: None };
        let hold5 = |e: &Sent| l.hold("title c5", e);
        let held5 = board::Held { hold: &hold5, waiting: l.held() };
        board::act_on(Some(&folder), &shown, "c5", "approve", "", &seen5, SystemTime::now(), Some(held5))
            .unwrap();
        lock(&l.shared.inner).held[1].until = Instant::now();
        speak_once(&l);
        heard(&l.shared, r#"{"unsure": "a2", "why": "no answer in 15 seconds"}"#);
        assert!(l.to_json(SystemTime::now())["actions"][1]["words"].as_str().unwrap().contains("Look there"));
        assert_eq!(board::sent(Some(&folder)).unwrap().len(), 1, "not known: it still waits");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_line_is_read_whole_up_to_its_limit_and_no_further() {
        let mut ok = std::io::Cursor::new([vec![b'x'; LINE_MAX], b"\r\nnext\n".to_vec()].concat());
        assert_eq!(read_line(&mut ok).unwrap().unwrap().len(), LINE_MAX);
        assert_eq!(read_line(&mut ok).unwrap().as_deref(), Some("next"));
        let mut long = std::io::Cursor::new(vec![b'x'; LINE_MAX + 5]);
        assert!(read_line(&mut long).unwrap_err().contains("longer than"));
        let mut odd = std::io::Cursor::new(b"\xff\xfe broken\n".to_vec());
        assert!(
            read_line(&mut odd).unwrap().unwrap().ends_with("broken"),
            "bytes that are not text are still read"
        );
    }

    #[test]
    fn an_adapter_that_could_not_start_says_why_and_takes_nothing() {
        let l = Link::failed("The Board's adapter (hub) could not be started: not found.");
        assert!(l.board().problem.unwrap().contains("could not be started: not found"));
        assert!(l.hold("t", &entry("c1")).is_err());
        assert_eq!(l.to_json(SystemTime::now())["running"], false);
    }
}
