//! The page: a second face, in a browser on this machine. Read only.
//!
//! It is handed the reading half of the engine's published copies
//! (`snapshot::Reader`) and nothing else, so it has no way to ask the
//! engine for anything: "read only" is not a promise this file has to
//! keep. A test (`tests/boundaries.rs`) holds it to that.
//!
//! It listens on this machine only (127.0.0.1), and only when the user has
//! switched it on. It is the first place an agent's screen leaves the
//! terminal window, so every request is checked before it is answered:
//!
//! - only with the word in its address: a random word, new each time
//!   Lillignode starts, that only the person shown it knows. Without it a
//!   request gets nothing of the page and no data;
//! - only `GET`;
//! - only when the browser says it is talking to this address (`Host`), so
//!   a web site that points a name of its own at this machine is refused;
//! - never for another site's page (`Origin`, `Sec-Fetch-Site`), and no
//!   answer says another site may read it.
//!
//! What the word cannot keep out: a program that can read the screen
//! Lillignode is drawn on, or the browser's history.
//!
//! It tells the engine one thing: which agents' screens open pages are
//! showing (`Reader::show`), so that only those are copied.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::snapshot::{AgentCopy, Published, Read, Reader};
use crate::status::AgentStatus;
use crate::terminal::{Run, ScreenCopy, Tint};
use crate::workspace::Status;

/// No more than this many browsers' requests are answered at once. One
/// open page holds one.
const MAX_OPEN: usize = 32;

/// The most of a request that is read before it is refused, and the
/// longest it may take to arrive.
const MAX_HEAD: usize = 16 * 1024;
const HEAD_WITHIN: Duration = Duration::from_secs(10);

/// One place among the requests being answered, given back when dropped:
/// however the answering ends, and if it never starts.
struct Place(Arc<AtomicUsize>);

impl Drop for Place {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

const HTML: &str = include_str!("page.html");
const CSS: &str = include_str!("page.css");
const JS: &str = include_str!("page.js");

/// What the browser may do with the page: load its own style and script,
/// listen to its own address, and nothing else.
const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
                      base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The letters a word is made of: no 0, 1, l or o, which are easily
/// taken for each other.
const LETTERS: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";

/// How many letters a word has. Five random bits each: 80 in all.
const WORD_LETTERS: usize = 16;

/// A new random word for the page's address.
fn new_word() -> String {
    // A version 4 id is random but for its version (byte 6) and variant
    // (byte 8): these ten bytes are random throughout.
    let id = uuid::Uuid::new_v4();
    let b = id.as_bytes();
    let random: Vec<u8> = b[..6].iter().chain(&b[9..13]).copied().collect();
    let bits = random.iter().fold(0u128, |all, byte| all << 8 | u128::from(*byte));
    (0..WORD_LETTERS).map(|i| LETTERS[(bits >> (5 * i) & 31) as usize] as char).collect()
}

/// True when `given` is the word, compared so that how long it takes says
/// nothing about how much of it was right.
fn is_word(given: &str, word: &str) -> bool {
    given.len() == word.len() && given.bytes().zip(word.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// A page being served: where it listens, and the word in its address.
pub struct Served {
    pub address: SocketAddr,
    word: String,
}

impl Served {
    /// The address to give a person, word and all.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/", self.address.port(), self.word)
    }
}

/// Start answering on this machine at `port` (0: any free port), under a
/// new word. Returns where, or why it could not be started; then nothing
/// is listening.
pub fn serve(reader: Reader, port: u16) -> Result<Served, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let word = new_word();
    let served = Served { address, word: word.clone() };
    let open = Arc::new(AtomicUsize::new(0));
    let accept = move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                // Whatever stops connections being taken, do not spin on it.
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
            let place = Place(open.clone());
            if open.fetch_add(1, Ordering::SeqCst) >= MAX_OPEN {
                let _ = refuse(&stream, "503 Service Unavailable", "Too many pages are open. Close one.");
                continue;
            }
            let (reader, word) = (reader.clone(), word.clone());
            let answer = move || {
                let _place = place;
                let _ = answer(&stream, &reader, address.port(), &word);
            };
            // If no thread can be started the place is given back with the
            // closure, and the connection closed.
            let _ = std::thread::Builder::new().name("page-request".into()).spawn(answer);
        }
    };
    std::thread::Builder::new().name("page".into()).spawn(accept).map_err(|e| e.to_string())?;
    Ok(served)
}

struct Request {
    path: String,
    query: String,
}

/// Why a request is not answered, as a status line and a sentence.
type Refusal = (&'static str, String);

/// Read and check one request. Nothing is answered that fails a check.
fn read_request(stream: &TcpStream, port: u16) -> Result<Request, Refusal> {
    let bad = |why: &str| ("400 Bad Request", why.to_string());
    let mut head = BufReader::new(std::io::Read::take(stream, MAX_HEAD as u64));
    let mut line = String::new();
    head.read_line(&mut line).map_err(|_| bad("The request could not be read."))?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(bad("The request could not be read."));
    };
    let (method, target) = (method.to_string(), target.to_string());
    let (mut host, mut origin, mut site) = (None, None, None);
    let began = Instant::now();
    loop {
        // A request fed a byte at a time does not hold a place for hours.
        if began.elapsed() > HEAD_WITHIN {
            return Err(bad("The request took too long to arrive."));
        }
        let mut line = String::new();
        match head.read_line(&mut line) {
            Ok(0) | Err(_) => return Err(bad("The request was cut short or too long.")),
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim().to_ascii_lowercase();
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => host = Some(value),
            "origin" => origin = Some(value),
            "sec-fetch-site" => site = Some(value),
            _ => {}
        }
    }
    if method != "GET" {
        return Err(("405 Method Not Allowed", "This page is read only: it takes nothing.".to_string()));
    }
    let here = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let elsewhere =
        || ("403 Forbidden", format!("This page answers only at http://127.0.0.1:{port}/… on this machine."));
    if !host.is_some_and(|host| here.contains(&host)) {
        return Err(elsewhere());
    }
    if origin.is_some_and(|origin| !here.iter().any(|h| origin == format!("http://{h}"))) {
        return Err(elsewhere());
    }
    if site.is_some_and(|site| site != "same-origin" && site != "none") {
        return Err(elsewhere());
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Ok(Request { path: path.to_string(), query: query.to_string() })
}

/// The value of `name` in a query such as `after=3&agent=2`, as a number.
fn number(query: &str, name: &str) -> Option<u64> {
    query.split('&').filter_map(|pair| pair.split_once('=')).find(|(n, _)| *n == name)?.1.parse().ok()
}

fn head(status: &str, kind: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\nCross-Origin-Opener-Policy: same-origin\r\n\
         Content-Security-Policy: {POLICY}\r\n\
         Connection: close\r\n"
    )
}

fn send(mut stream: &TcpStream, status: &str, kind: &str, body: &str) -> std::io::Result<()> {
    let head = head(status, kind);
    stream.write_all(format!("{head}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes())?;
    stream.flush()
}

fn refuse(stream: &TcpStream, status: &str, why: &str) -> std::io::Result<()> {
    send(stream, status, "text/plain; charset=utf-8", why)
}

/// Said to a request without the word. Nothing of the page, and no data.
const NOTHING: &str =
    "Nothing here. Lillignode shows its page's address, word and all, when it starts and in its \
                       status report.";

fn answer(stream: &TcpStream, reader: &Reader, port: u16, word: &str) -> std::io::Result<()> {
    // A browser that stops reading or never finishes asking does not hold
    // a place for ever.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let request = match read_request(stream, port) {
        Ok(request) => request,
        Err((status, why)) => return refuse(stream, status, &why),
    };
    // Everything is under the word: /<word>/ and what is beside it.
    let under = request.path.strip_prefix('/').unwrap_or("");
    let (given, rest) = under.split_once('/').map_or((under, None), |(given, rest)| (given, Some(rest)));
    if !is_word(given, word) {
        return refuse(stream, "404 Not Found", NOTHING);
    }
    let Some(rest) = rest else {
        // The word without the slash after it: the page's own files are
        // found beside it, so the address must end with one.
        let head = head("308 Permanent Redirect", "text/plain; charset=utf-8");
        return (&*stream)
            .write_all(format!("{head}Location: /{word}/\r\nContent-Length: 0\r\n\r\n").as_bytes());
    };
    match rest {
        "" => send(stream, "200 OK", "text/html; charset=utf-8", HTML),
        "page.css" => send(stream, "200 OK", "text/css; charset=utf-8", CSS),
        "page.js" => send(stream, "200 OK", "text/javascript; charset=utf-8", JS),
        "state" => {
            // A screen is copied only while a reader shows it: say so, and
            // give the engine a moment to publish a copy that has it.
            let agent = number(&request.query, "agent");
            let _showing = agent.map(|agent| reader.show(agent));
            let has_screen = |p: &Published| p.screens.iter().any(|(key, _)| Some(key.number()) == agent);
            let mut latest = reader.latest();
            let limit = Instant::now() + Duration::from_millis(500);
            while let (Some((count, published)), true) = (&latest, agent.is_some()) {
                if has_screen(published) || Instant::now() >= limit {
                    break;
                }
                match reader.newer_than(*count, Duration::from_millis(100)) {
                    Read::Copy(count, published) => latest = Some((count, published)),
                    Read::NothingNewer => {}
                    Read::Ended => break,
                }
            }
            state_once(stream, latest.map(|(_, published)| published), agent, &request.query)
        }
        "stream" => stream_states(stream, reader, &request.query),
        _ => refuse(stream, "404 Not Found", "There is nothing here."),
    }
}

/// The events to send to a reader that has read up to `after` in the run
/// it names: everything, if that was another run.
fn after_in(query: &str, published: &Published) -> u64 {
    match query.split('&').find_map(|pair| pair.strip_prefix("run=")) {
        Some(run) if run != published.state.run.to_string() => 0,
        _ => number(query, "after").unwrap_or(0),
    }
}

fn state_once(
    stream: &TcpStream,
    published: Option<Arc<Published>>,
    agent: Option<u64>,
    query: &str,
) -> std::io::Result<()> {
    {
        match published {
            Some(published) => {
                let after = after_in(query, &published);
                let body = json!({
                    "at": millis(published.state.taken_at),
                    "state": state_json(&published, agent, after),
                });
                send(stream, "200 OK", "application/json", &body.to_string())
            }
            None => refuse(stream, "503 Service Unavailable", "Lillignode has not said anything yet."),
        }
    }
}

/// Send each new state as it is published, for as long as the browser
/// listens. At least once a second something is sent, so a page can tell
/// silence from a quiet moment. One agent's screen is sent: `agent`.
fn stream_states(mut stream: &TcpStream, reader: &Reader, query: &str) -> std::io::Result<()> {
    let agent = number(query, "agent");
    // For as long as this page listens, its agent's screen is copied.
    let _showing = agent.map(|agent| reader.show(agent));
    let mut after = None;
    stream.write_all(format!("{}\r\n", head("200 OK", "text/event-stream")).as_bytes())?;
    let (mut count, mut sent) = (0, String::new());
    loop {
        match reader.newer_than(count, Duration::from_secs(1)) {
            Read::Copy(newer, published) => {
                count = newer;
                let at = millis(published.state.taken_at);
                let from = after.unwrap_or_else(|| after_in(query, &published));
                let mut state = state_json(&published, agent, from);
                after = Some(from.max(published.state.as_of));
                // Events are sent once. Apart from them, has anything
                // changed since the last state sent?
                let events = std::mem::replace(&mut state["events"], json!([]));
                let quiet = events.as_array().is_none_or(Vec::is_empty);
                let without_events = state.to_string();
                let same = without_events == sent;
                sent = without_events;
                state["events"] = events;
                // Nothing but the time has changed: say only the time.
                if same && quiet {
                    stream.write_all(format!("event: state\ndata: {{\"at\":{at}}}\n\n").as_bytes())?;
                } else {
                    let message = format!("event: state\ndata: {{\"at\":{at},\"state\":{state}}}\n\n");
                    stream.write_all(message.as_bytes())?;
                }
            }
            // Lillignode is there and has said nothing new: not a word from
            // it, so the page's count of silence runs on.
            Read::NothingNewer => stream.write_all(b": quiet\n\n")?,
            Read::Ended => {
                stream.write_all(b"event: ended\ndata: {}\n\n")?;
                return stream.flush();
            }
        }
        stream.flush()?;
    }
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// The state as the page reads it: every agent, the screen of `agent`, and
/// the events numbered after `after`.
fn state_json(published: &Published, agent: Option<u64>, after: u64) -> Value {
    let state = &published.state;
    let agents: Vec<Value> = state.agents.iter().map(|a| agent_json(a, state.front == Some(a.key))).collect();
    let screen = published
        .screens
        .iter()
        .find(|(key, _)| Some(key.number()) == agent)
        .map(|(key, screen)| screen_json(key.number(), screen));
    let events: Vec<Value> = published
        .events
        .iter()
        .filter(|e| e.number > after)
        .map(|e| json!({ "n": e.number, "what": e.what, "agent": e.agent.map(|a| a.number()), "label": e.label }))
        .collect();
    json!({
        "run": state.run.to_string(),
        "as_of": state.as_of,
        "agents": agents,
        "screen": screen,
        "events": events,
        // Events after `after` that are no longer kept.
        "missed": published.events_let_go.saturating_sub(after),
    })
}

/// One agent as the page reads it. The words for its status are the
/// engine's, the same as the terminal shows; ages are worked out by the
/// page from the two times.
fn agent_json(agent: &AgentCopy, front: bool) -> Value {
    let slug = match &agent.status {
        Some(AgentStatus::Starting) => "starting",
        Some(AgentStatus::Working | AgentStatus::WorkingQuiet(_)) => "working",
        Some(AgentStatus::NeedsYou(_)) => "needs-you",
        Some(AgentStatus::Done) => "done",
        Some(AgentStatus::Idle) => "idle",
        Some(AgentStatus::Error(_)) => "error",
        Some(AgentStatus::Unknown(_)) => "unknown",
        None => match &agent.program {
            Status::Running => "unknown",
            Status::Exited(_) => "exited",
            Status::NotStarted(_) => "not-started",
        },
    };
    let words = agent.status.as_ref().map_or_else(|| agent.program.text(), AgentStatus::text);
    let program_note = match &agent.program {
        Status::Running => None,
        Status::Exited(_) => Some("Its program has ended. This is its last screen.".to_string()),
        Status::NotStarted(why) => Some(format!("It was not started: {why}.")),
    };
    json!({
        "key": agent.key.number(),
        "name": agent.name,
        "folder": agent.folder_name,
        "label": agent.label,
        "kind": agent.kind.saved_word(),
        "running": agent.is_running(),
        "front": front,
        "state": slug,
        "words": words,
        "needs_you": agent.needs_you.is_some(),
        "needs": agent.needs_you.flatten().map(|kind| kind.word()),
        "needs_you_since": agent.needs_you_since.map(millis),
        "reported_at": agent.reported_at.map(millis),
        "program_note": program_note,
    })
}

fn tint_json(tint: Tint) -> Value {
    match tint {
        Tint::Default => Value::Null,
        Tint::Indexed(i) => json!(i),
        Tint::Rgb(r, g, b) => json!([r, g, b]),
    }
}

fn run_json(run: &Run) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("t".into(), json!(run.text));
    let drawn = run.drawn;
    if drawn.fg != Tint::Default {
        out.insert("fg".into(), tint_json(drawn.fg));
    }
    if drawn.bg != Tint::Default {
        out.insert("bg".into(), tint_json(drawn.bg));
    }
    let marks = [("b", drawn.bold), ("d", drawn.dim), ("i", drawn.italic), ("u", drawn.underline)];
    for (name, on) in marks.into_iter().chain([("v", drawn.inverse)]) {
        if on {
            out.insert(name.into(), json!(1));
        }
    }
    Value::Object(out)
}

fn screen_json(key: u64, screen: &ScreenCopy) -> Value {
    let lines: Vec<Vec<Value>> =
        screen.lines.iter().map(|runs| runs.iter().map(run_json).collect()).collect();
    json!({
        "key": key,
        "rows": screen.rows,
        "cols": screen.cols,
        "lines": lines,
        "cursor": screen.cursor.map(|(row, col)| [row, col]),
        "scrolled_back": screen.scrolled_back,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{channel, EventCopy, Publisher, StateCopy};
    use std::io::Read as _;

    fn published(as_of: u64) -> Published {
        Published {
            state: StateCopy {
                run: 7,
                as_of,
                taken_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1000),
                agents: Vec::new(),
                front: None,
                shown_two: None,
                question: None,
                cost_on: false,
                cost_summary: crate::cost::Summary::new(),
            },
            screens: Vec::new(),
            events: (1..=as_of)
                .map(|number| EventCopy { number, what: "exited", agent: None, label: Some("a (x)".into()) })
                .collect(),
            events_let_go: 0,
        }
    }

    /// A page being served, as a test reaches it: its address and word.
    #[derive(Clone, Copy)]
    struct At {
        address: SocketAddr,
        word: &'static str,
    }

    impl At {
        fn port(self) -> u16 {
            self.address.port()
        }
    }

    fn served() -> (Publisher, At) {
        let (publisher, reader) = channel();
        let served = serve(reader, 0).expect("a free port on this machine");
        let word: &'static str = Box::leak(served.word.clone().into_boxed_str());
        assert_eq!(served.url(), format!("http://127.0.0.1:{}/{word}/", served.address.port()));
        (publisher, At { address: served.address, word })
    }

    /// Send `request` as it is, and read everything that comes back until
    /// the page closes the connection.
    fn ask_raw(at: At, request: &str) -> String {
        let mut stream = TcpStream::connect(at.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    }

    /// The same, with the word put before the path the request names, as
    /// the page's own requests have it.
    fn ask(at: At, request: &str) -> String {
        let (first, rest) = request.split_once("\r\n").unwrap_or((request, ""));
        let first = match first.split_once(" /") {
            Some((method, target)) => format!("{method} /{}/{target}", at.word),
            None => first.to_string(),
        };
        ask_raw(at, &format!("{first}\r\n{rest}"))
    }

    fn get(at: At, path: &str) -> String {
        ask(at, &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", at.port()))
    }

    #[test]
    fn without_the_word_a_request_gets_nothing_of_the_page_and_no_data() {
        let (publisher, at) = served();
        publisher.publish(published(1));
        let raw = |path: &str| {
            ask_raw(at, &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", at.port()))
        };
        let word = at.word;
        let mut wrong: Vec<u8> = word.bytes().collect();
        wrong[15] = if wrong[15] == b'a' { b'b' } else { b'a' };
        let wrong = String::from_utf8(wrong).unwrap();
        for path in [
            "/".to_string(),
            "/state".into(),
            "/stream".into(),
            "/page.js".into(),
            format!("/{wrong}/"),
            format!("/{wrong}/state"),
            format!("/{}/state", word.to_uppercase()),
            format!("/{word}x/state"),
            format!("/{}/state", &word[..15]),
            format!("/x/{word}/state"),
            "//state".into(),
        ] {
            let answer = raw(&path);
            assert!(answer.starts_with("HTTP/1.1 404"), "{path}: {answer}");
            let body = answer.split_once("\r\n\r\n").unwrap().1;
            assert_eq!(body, NOTHING, "{path}: the same few words, whatever was asked");
            assert!(!answer.contains(word), "{path}: the word is never said back");
        }
        // With it, all of it.
        assert!(raw(&format!("/{word}/")).contains("<title>Lillignode</title>"));
        assert!(raw(&format!("/{word}/state")).contains("\"agents\""));
        // The word without the slash after it is sent on to it.
        let moved = raw(&format!("/{word}"));
        assert!(moved.starts_with("HTTP/1.1 308"), "{moved}");
        assert!(moved.contains(&format!("Location: /{word}/\r\n")), "{moved}");
    }

    #[test]
    fn the_word_is_new_each_start_and_made_of_letters_that_are_not_mistaken() {
        let words: Vec<String> = (0..50).map(|_| new_word()).collect();
        for word in &words {
            assert_eq!(word.len(), WORD_LETTERS);
            assert!(word.bytes().all(|b| LETTERS.contains(&b)), "{word}");
            assert!(!word.contains(['0', '1', 'l', 'o']), "{word}");
        }
        let mut distinct = words.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), words.len(), "no two alike");
        let (_, one) = served();
        let (_, two) = served();
        assert_ne!(one.word, two.word, "each start its own");
        // Compared whole: a word that is only its start, or longer, is not it.
        assert!(is_word("abc", "abc"));
        assert!(!is_word("ab", "abc") && !is_word("abcd", "abc") && !is_word("abd", "abc"));
    }

    #[test]
    fn it_listens_on_this_machine_only_and_serves_the_page_with_its_limits() {
        let (_publisher, address) = served();
        assert!(address.address.ip().is_loopback(), "{}", address.address);
        let page = get(address, "/");
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        assert!(page.contains("<title>Lillignode</title>"));
        assert!(page.contains("Content-Security-Policy: default-src 'none'"));
        assert!(page.contains("Cache-Control: no-store"));
        assert!(!page.to_ascii_lowercase().contains("access-control-allow"), "no other site may read it");
        assert!(get(address, "/page.js").contains("EventSource"));
        assert!(get(address, "/page.css").contains("#14202B"));
        // Orange means an agent needs you, and nothing else: every rule
        // that uses it is the strip's or the row's of an agent that needs
        // you. The old copper is gone.
        assert!(CSS.contains("--orange: #E39A55;"));
        for gone in ["copper", "C17B3E", "c17b3e"] {
            assert!(!CSS.contains(gone) && !JS.contains(gone), "{gone}");
        }
        for rule in CSS.split('}') {
            let Some((selector, body)) = rule.split_once('{') else { continue };
            if body.contains("var(--orange)") {
                let selector = selector.rsplit("*/").next().unwrap().trim();
                assert!(
                    selector == "#blocking" || selector.starts_with("#agents li.needs-you "),
                    "orange used for {selector:?}"
                );
            }
        }
        assert!(CSS.contains("h1 { font-size: 18px; color: var(--cream);"), "the header's word is cream");
        assert!(get(address, "/elsewhere").starts_with("HTTP/1.1 404"));
        // The page's own script is the only script, and it writes what an
        // agent wrote as text, never as markup.
        assert!(!JS.contains("innerHTML") && !JS.contains("insertAdjacentHTML") && !JS.contains("eval("));
        assert!(!HTML.contains("<script>"), "no script in the page itself: the policy forbids it");
        // The user's words: the rows are agents, as everywhere else in
        // Lillignode; the one other heading is the Board.
        assert!(HTML.contains(r#"<h2 id="agents-title">Agents</h2>"#), "{HTML}");
        assert!(HTML.contains(r#"<h2 id="board-title">Board</h2>"#), "{HTML}");
        for (name, text) in [("page.html", HTML), ("page.js", JS), ("page.css", CSS)] {
            let lower = text.to_lowercase();
            for word in ["thread", "seat"] {
                assert!(!lower.contains(word), "{name} says {word}");
            }
        }
    }

    #[test]
    fn a_request_that_is_not_a_plain_read_from_this_address_is_refused() {
        let (publisher, address) = served();
        publisher.publish(published(1));
        let port = address.port();
        let with = |first: &str, headers: &str| ask(address, &format!("{first}\r\n{headers}\r\n"));
        let host = format!("Host: 127.0.0.1:{port}\r\n");
        // It takes nothing.
        for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
            let answer = with(&format!("{method} /state HTTP/1.1"), &host);
            assert!(answer.starts_with("HTTP/1.1 405"), "{method}: {answer}");
        }
        // A name some web site pointed at this machine.
        let answer = with("GET /state HTTP/1.1", &format!("Host: evil.example:{port}\r\n"));
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        assert!(!answer.contains("agents"), "{answer}");
        assert!(with("GET /state HTTP/1.1", "").starts_with("HTTP/1.1 403"), "no Host at all");
        // Another site's page asking from the browser.
        let cross = format!("{host}Origin: https://evil.example\r\n");
        assert!(with("GET /state HTTP/1.1", &cross).starts_with("HTTP/1.1 403"));
        let cross = format!("{host}Sec-Fetch-Site: cross-site\r\n");
        assert!(with("GET /stream HTTP/1.1", &cross).starts_with("HTTP/1.1 403"));
        // The page's own requests, and a person typing the address.
        let own = format!("{host}Origin: http://127.0.0.1:{port}\r\nSec-Fetch-Site: same-origin\r\n");
        assert!(with("GET /state HTTP/1.1", &own).starts_with("HTTP/1.1 200"));
        let typed = format!("Host: localhost:{port}\r\nSec-Fetch-Site: none\r\n");
        assert!(with("GET /state HTTP/1.1", &typed).starts_with("HTTP/1.1 200"));
        // Not a request at all.
        assert!(ask(address, "\r\n\r\n").starts_with("HTTP/1.1 400"));
    }

    #[test]
    fn the_state_carries_its_time_its_event_number_and_only_the_events_asked_after() {
        let (publisher, address) = served();
        assert!(get(address, "/state").starts_with("HTTP/1.1 503"), "nothing published yet");
        publisher.publish(published(3));
        let answer = get(address, "/state?after=2");
        let body: Value = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["at"], 1_000_000);
        assert_eq!(body["state"]["as_of"], 3);
        assert_eq!(body["state"]["events"].as_array().unwrap().len(), 1);
        assert_eq!(body["state"]["events"][0]["n"], 3);
        assert_eq!(body["state"]["missed"], 0);
        // A reader that asks after a number already let go is told how many
        // it missed.
        let mut late = published(3);
        late.events.remove(0);
        late.events_let_go = 1;
        publisher.publish(late);
        let answer = get(address, "/state");
        let body: Value = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["state"]["missed"], 1);
        assert_eq!(body["state"]["events"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_stream_sends_each_new_state_then_only_the_time_and_says_when_lillignode_has_gone() {
        let (publisher, address) = served();
        publisher.publish(published(1));
        let reading = std::thread::spawn(move || get(address, "/stream?after=0"));
        std::thread::sleep(Duration::from_millis(300));
        // The same state again: only its time is sent.
        publisher.publish(published(1));
        std::thread::sleep(Duration::from_millis(300));
        publisher.publish(published(2));
        std::thread::sleep(Duration::from_millis(300));
        drop(publisher);
        let heard = reading.join().unwrap();
        assert!(heard.contains("Content-Type: text/event-stream"), "{heard}");
        let body = heard.split_once("\r\n\r\n").unwrap().1;
        let messages: Vec<&str> = body.split("\n\n").filter(|m| m.starts_with("event: ")).collect();
        assert_eq!(messages.len(), 4, "{heard}");
        assert!(messages[0].contains("\"state\":") && messages[0].contains("\"as_of\":1"), "{}", messages[0]);
        assert_eq!(messages[1], "event: state\ndata: {\"at\":1000000}", "nothing changed but the time");
        // The second state brings only the event the reader has not had.
        assert!(messages[2].contains("\"as_of\":2") && messages[2].contains("\"n\":2"), "{}", messages[2]);
        assert!(!messages[2].contains("\"n\":1"), "{}", messages[2]);
        assert_eq!(messages[3], "event: ended\ndata: {}");
    }
}
