//! The page: a second face, in a browser on this machine. Read only,
//! unless the user has also asked that it may type to agents.
//!
//! It is handed the reading half of the engine's published copies
//! (`snapshot::Reader`), so by itself it has no way to ask the engine for
//! anything: "read only" is not a promise this file has to keep. A test
//! (`tests/boundaries.rs`) holds it to that.
//!
//! Typing is one more thing it may be handed, and only when the user has
//! switched it on: somewhere to pass on text for one agent (`Typing`).
//! What becomes of the text is decided where it arrives, by the same
//! command a paste in the terminal uses. Without it, no request can send
//! anything, whatever it says.
//!
//! Acting on a card of the Board is another, switched on apart from
//! typing: somewhere to pass on what the reader did to which card
//! (`Acting`). Whether it may be done, and the writing of it to the
//! Board's outbox, happen where it arrives. Without it no request can act
//! on a card.
//!
//! It listens on this machine only (127.0.0.1), and only when the user has
//! switched it on. It is the first place an agent's screen leaves the
//! terminal window, so every request is checked before it is answered:
//!
//! - only with the word in its address: a random word that only the person
//!   shown it knows. Without it a request gets nothing of the page and no
//!   data. While the page is read only the word is made once and kept in a
//!   file only this user can read, so the address can be bookmarked. While
//!   the page may type it is new each start and kept nowhere: a word that
//!   has leaked (to a program that took the port while Lillignode was
//!   closed and heard an open page ask for it, or to anything that reads
//!   this user's files) must not be enough to type to an agent later;
//! - only `GET`, and, where typing is on, the one `POST` the page's own
//!   Send button makes: from this page (`Origin` is required and must be
//!   this address), as JSON, with the token the page was given with its
//!   data. A link, a form or another site's script can make none of that;
//! - only when the browser says it is talking to this address (`Host`), so
//!   a web site that points a name of its own at this machine is refused;
//! - never for another site's page (`Origin`, `Sec-Fetch-Site`), and no
//!   answer says another site may read it.
//!
//! What the word cannot keep out: a program that can read the screen
//! Lillignode is drawn on, this user's files, or the browser's history.
//!
//! It tells the engine one thing: which agents' screens open pages are
//! showing (`Reader::show`), so that only those are copied.

use std::io::{BufRead, BufReader, Read as _, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::board;
use crate::private;
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

/// The most text one Send may carry, with what wraps it.
const MAX_BODY: usize = 64 * 1024;

/// How long a Send waits to be told what became of it. After that its
/// sender is told it is not known whether it was sent, and it is no longer
/// carried out.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(5);

/// Text typed on the page for one agent, passed on to be sent. The page
/// decides nothing about it.
pub struct Typed {
    /// The run of Lillignode the page believes it is talking to: agents'
    /// keys begin again with each run.
    pub run: String,
    /// The key number of the agent it is for.
    pub agent: u64,
    pub text: String,
    /// The reader was asked whether it may go line by line, and said yes.
    pub agreed: bool,
    /// Enter alone, from the page's own button for it. `text` is empty.
    pub enter: bool,
    /// When it arrived: one that has waited longer than its sender was
    /// told to wait is not carried out.
    pub asked: Instant,
    /// Where to say what became of it.
    pub answer: mpsc::Sender<TypedAnswer>,
}

/// What became of text passed on, in the words to show the reader.
pub struct TypedAnswer {
    pub outcome: TypedOutcome,
    pub words: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedOutcome {
    /// It was sent, and Enter after it.
    Sent,
    /// Nothing was sent: the reader is asked first, and may send it again
    /// having agreed.
    Ask,
    /// Nothing was sent.
    Refused,
}

/// Where the page passes on what was typed. False: there was nowhere left
/// to pass it to.
pub type Typing = Arc<dyn Fn(Typed) -> bool + Send + Sync>;

/// What the reader did to a card of the Board on the page, passed on. The
/// page decides nothing about it.
pub struct Acted {
    /// The card's id, as the Board's file has it.
    pub card: String,
    /// approve, revise, hold, done or got-it, as the page sent it.
    pub action: String,
    /// The note that goes with a Revise.
    pub note: String,
    /// The card's title and recommendation as the page showed them when
    /// the button was pressed.
    pub title: String,
    pub recommendation: Option<String>,
    /// When it arrived: one that has waited longer than its sender was
    /// told to wait is not carried out.
    pub asked: Instant,
    /// Take back what was done to this card, instead of doing something.
    pub undo: bool,
    /// Where to say what became of it.
    pub answer: mpsc::Sender<TypedAnswer>,
}

/// Where the page passes on what was done to a card. False: there was
/// nowhere left to pass it to.
pub type Acting = Arc<dyn Fn(Acted) -> bool + Send + Sync>;

/// The ways a page may send something, each only if the user switched it
/// on. With none, the page is read only.
#[derive(Default)]
pub struct Ways {
    pub typing: Option<Typing>,
    pub acting: Option<Acting>,
}

impl From<Option<Typing>> for Ways {
    fn from(typing: Option<Typing>) -> Self {
        Ways { typing, acting: None }
    }
}

/// The token as each way of sending is shown it with the page's data:
/// for typing, and for acting. None for a way that is off.
type Tokens<'a> = (Option<&'a str>, Option<&'a str>);

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

/// The file, in the folder Lillignode saves in, that keeps the word from
/// one start to the next.
pub const WORD_FILE: &str = "page-word";

fn is_a_word(text: &str) -> bool {
    text.len() == WORD_LETTERS && text.bytes().all(|b| LETTERS.contains(&b))
}

/// The word kept in `folder`: None if there is none, or what is there is
/// not a word, or others on this machine could read it. A word others
/// could read is no longer one only this user knows.
fn read_word(folder: &Path) -> Option<String> {
    let path = folder.join(WORD_FILE);
    if !private::is_private(&path) {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string()).filter(|word| is_a_word(word))
}

/// Keep `word` in `folder` for the next start, in a file only this user
/// can read. If it cannot be kept, the word kept before is taken away too,
/// so that an address meant to be gone does not come back with the next
/// start. Returns why it was not kept.
fn keep_word(folder: &Path, word: &str) -> Result<(), String> {
    let path = folder.join(WORD_FILE);
    let text = format!("{word}\n");
    let kept = std::fs::create_dir_all(folder).and_then(|()| private::write_whole(folder, WORD_FILE, &text));
    kept.map_err(|e| match std::fs::remove_file(&path) {
        Err(gone) if gone.kind() != std::io::ErrorKind::NotFound => {
            format!("{e}; and {} could not be removed, so delete it yourself", path.display())
        }
        _ => e.to_string(),
    })
}

/// The word to serve under: the one kept in `folder`, or a new one, kept
/// from now on, if none is. With it, why it is not kept, if it is not: it
/// then lasts until Lillignode closes.
fn kept_word(folder: Option<&Path>) -> (String, Option<String>) {
    let Some(folder) = folder else {
        return (new_word(), Some("Lillignode has nowhere to save".to_string()));
    };
    match read_word(folder) {
        Some(word) => (word, None),
        None => {
            let word = new_word();
            let not_kept = keep_word(folder, &word).err();
            (word, not_kept)
        }
    }
}

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

/// A page being served: where it listens, and the word in its address.
pub struct Served {
    pub address: SocketAddr,
    word: Arc<Mutex<String>>,
    /// Where the word is kept between starts.
    folder: Option<PathBuf>,
    /// Why the word is not kept for the next start, if it was meant to be
    /// and is not.
    pub word_not_kept: Option<String>,
    /// Whether the page was handed somewhere to pass typing on to. Then
    /// the word is new each start, on purpose, and kept nowhere.
    pub can_type: bool,
    /// Whether the page was handed somewhere to pass on what is done to a
    /// card. Then, too, the word is new each start and kept nowhere.
    pub can_act: bool,
    /// The token anything sent must carry, where the page may send.
    token: Option<Arc<Mutex<String>>>,
}

/// The word now. A thread that died holding it left it whole.
fn word_now(word: &Mutex<String>) -> String {
    word.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

impl Served {
    /// The address to give a person, word and all.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/", self.address.port(), word_now(&self.word))
    }

    /// Make a new word and answer only under it from now on: the address
    /// before stops working at once, for pages already open too. The new
    /// word is kept for the next start; returns why, if it could not be.
    ///
    /// Where the page may type the word is kept nowhere, and the token goes
    /// with it: whoever had the old address has nothing left that works.
    pub fn change_word(&mut self) -> Option<String> {
        let word = new_word();
        self.word_not_kept = match (&self.folder, self.can_type || self.can_act) {
            (_, true) => None,
            (Some(folder), false) => keep_word(folder, &word).err(),
            (None, false) => Some("Lillignode has nowhere to save".to_string()),
        };
        *self.word.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = word;
        if let Some(token) = &self.token {
            *token.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = new_token();
        }
        self.word_not_kept.clone()
    }
}

/// A new token for a Send to carry: 160 random bits.
fn new_token() -> String {
    format!("{}{}", new_word(), new_word())
}

/// What every request is answered with: the copies to read, the word, and,
/// where the page may send, where each kind of thing is passed on and the
/// token it must carry.
struct Serving {
    reader: Reader,
    port: u16,
    word: Arc<Mutex<String>>,
    typing: Option<Typing>,
    acting: Option<Acting>,
    /// There if and only if the page may send anything.
    token: Option<Arc<Mutex<String>>>,
    /// Where the Board's file is read: the folder Lillignode saves in.
    folder: Option<PathBuf>,
}

impl Serving {
    /// The token anything sent must carry now, where the page may send.
    fn token(&self) -> Option<String> {
        self.token.as_ref().map(|token| word_now(token))
    }

    /// The token as the page is shown it: under each way that is on.
    fn tokens<'a>(&self, token: Option<&'a str>) -> Tokens<'a> {
        (token.filter(|_| self.typing.is_some()), token.filter(|_| self.acting.is_some()))
    }
}

/// Start answering on this machine at `port` (0: any free port). With no
/// way to send (`ways`), the page is read only, under the word kept in
/// `folder`, or a new one kept there from now on. With any, the page may
/// pass on text for an agent or what was done to a card, and its word is
/// new each start and kept nowhere. Returns where, or why it could not be
/// started; then nothing is listening.
pub fn serve(
    reader: Reader,
    port: u16,
    folder: Option<&Path>,
    ways: impl Into<Ways>,
) -> Result<Served, String> {
    let Ways { typing, acting } = ways.into();
    let sends = typing.is_some() || acting.is_some();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let (word, word_not_kept) = if sends { (new_word(), None) } else { kept_word(folder) };
    let word = Arc::new(Mutex::new(word));
    // The token is new each start and is never kept: it reaches a page only
    // with the data, which only a page under the word is given.
    let token = sends.then(|| Arc::new(Mutex::new(new_token())));
    let served = Served {
        address,
        word: word.clone(),
        folder: folder.map(Path::to_path_buf),
        word_not_kept,
        can_type: typing.is_some(),
        can_act: acting.is_some(),
        token: token.clone(),
    };
    let serving = Arc::new(Serving {
        reader,
        port: address.port(),
        word,
        typing,
        acting,
        token,
        folder: folder.map(Path::to_path_buf),
    });
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
            let serving = serving.clone();
            let answer = move || {
                let _place = place;
                let _ = answer(&stream, &serving);
            };
            // If no thread can be started the place is given back with the
            // closure, and the connection closed.
            let _ = std::thread::Builder::new().name("page-request".into()).spawn(answer);
        }
    };
    std::thread::Builder::new().name("page".into()).spawn(accept).map_err(|e| e.to_string())?;
    Ok(served)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Get,
    /// Only where the page may type or act, and only from this page.
    Post,
}

struct Request {
    method: Method,
    path: String,
    query: String,
    /// What a `POST` carried. Empty for a `GET`.
    body: Vec<u8>,
}

/// Why a request is not answered, as a status line and a sentence.
type Refusal = (&'static str, String);

/// Reads from a connection until a time, and no later: each read may wait
/// only for what is left of it. So a request sent a byte at a time does
/// not hold a place for longer than `HEAD_WITHIN`, in all.
struct Until<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl std::io::Read for Until<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        (&mut &*self.stream).read(buf)
    }
}

/// Said to anything that is not a read where the page is read only.
const TAKES_NOTHING: &str = "This page is read only: it takes nothing.";

/// Read and check one request. Nothing is answered that fails a check.
/// `can_type`: the one request that is not a read is allowed, from this
/// page only.
///
/// A requester without `word` learns nothing from how it is refused about
/// whether the page may type: past the check of where it asks from, it is
/// told the same nothing as for any address without the word.
fn read_request(stream: &TcpStream, port: u16, can_type: bool, word: &str) -> Result<Request, Refusal> {
    let bad = |why: &str| ("400 Bad Request", why.to_string());
    let until = Until { stream, until: Instant::now() + HEAD_WITHIN };
    let mut all = BufReader::new(until);
    // No more than `MAX_HEAD` is read before the request's body, however
    // long a line claims to be.
    let mut head_left = MAX_HEAD as u64;
    let mut next_line = |line: &mut String| -> std::io::Result<usize> {
        let read = (&mut all).take(head_left).read_line(line)?;
        head_left -= read as u64;
        Ok(read)
    };
    let mut line = String::new();
    next_line(&mut line).map_err(|_| bad("The request could not be read."))?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(bad("The request could not be read."));
    };
    let (method, target) = (method.to_string(), target.to_string());
    let (mut host, mut origin, mut site, mut kind, mut length) = (None, None, None, None, None);
    loop {
        let mut line = String::new();
        match next_line(&mut line) {
            Ok(0) | Err(_) => return Err(bad("The request was cut short or too long.")),
            Ok(_) if !line.ends_with('\n') => return Err(bad("The request was cut short or too long.")),
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
        }
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim().to_ascii_lowercase();
        // A header given twice is not chosen between: one of the two would
        // be the one a browser did not write.
        let once = |held: &mut Option<String>| match held.replace(value.clone()) {
            None => Ok(()),
            Some(_) => Err(bad("The request says one thing twice.")),
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "host" => once(&mut host)?,
            "origin" => once(&mut origin)?,
            "sec-fetch-site" => once(&mut site)?,
            "content-type" => once(&mut kind)?,
            "content-length" => once(&mut length)?,
            // A body sent in pieces is not read: its length must be said.
            "transfer-encoding" => return Err(bad("The request could not be read.")),
            _ => {}
        }
    }
    // The address Lillignode shows, and for a page that only reads, this
    // machine's other name for itself too. A page that may type answers to
    // the one address only: `localhost` can lead a browser to another
    // program on this machine, and the word with it.
    let mut here = vec![format!("127.0.0.1:{port}")];
    if !can_type {
        here.push(format!("localhost:{port}"));
    }
    let elsewhere =
        || ("403 Forbidden", format!("This page answers only at http://127.0.0.1:{port}/… on this machine."));
    let Some(host) = host.filter(|host| here.contains(host)) else {
        return Err(elsewhere());
    };
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let given = path.strip_prefix('/').unwrap_or("").split('/').next().unwrap_or("");
    let has_word = same(given, word);
    let from_here = |origin: &String| *origin == format!("http://{host}");
    let mut body = Vec::new();
    let mut rest = || -> Result<Method, Refusal> {
        let method = match method.as_str() {
            "GET" => Method::Get,
            "POST" if can_type => Method::Post,
            _ if can_type => {
                return Err((
                    "405 Method Not Allowed",
                    "This page takes only what its own buttons send.".into(),
                ))
            }
            _ => return Err(("405 Method Not Allowed", TAKES_NOTHING.to_string())),
        };
        match method {
            Method::Get => {
                if origin
                    .as_ref()
                    .is_some_and(|origin| !here.iter().any(|h| *origin == format!("http://{h}")))
                {
                    return Err(elsewhere());
                }
                if site.as_ref().is_some_and(|site| site != "same-origin" && site != "none") {
                    return Err(elsewhere());
                }
            }
            // Something is being sent: the browser must say it comes from
            // this very page. A browser always says where a `POST` comes
            // from, so one that does not say is not this page's.
            Method::Post => {
                if !origin.as_ref().is_some_and(from_here) {
                    return Err(elsewhere());
                }
                if site.as_ref().is_some_and(|site| site != "same-origin") {
                    return Err(elsewhere());
                }
                // JSON, which a form or a link cannot send and another
                // site's script may not without asking first (and asking
                // is refused).
                let json = kind.as_deref().map(|kind| kind.split(';').next().unwrap_or("").trim());
                if json != Some("application/json") {
                    return Err((
                        "415 Unsupported Media Type",
                        "This page takes only what its own buttons send.".into(),
                    ));
                }
                let Some(length) = length.as_ref().and_then(|length| length.parse::<usize>().ok()) else {
                    return Err((
                        "411 Length Required",
                        "The request did not say how long it is.".to_string(),
                    ));
                };
                if length > MAX_BODY {
                    return Err((
                        "413 Content Too Large",
                        "That is too much text to send at once.".to_string(),
                    ));
                }
                if (&mut all).take(length as u64).read_to_end(&mut body).is_err() || body.len() != length {
                    return Err(bad("The request was cut short or too long."));
                }
            }
        }
        Ok(method)
    };
    let method = rest().map_err(|refusal| match refusal {
        // Where it asks from is judged the same for everyone.
        (status, _) if has_word || status.starts_with("403") => refusal,
        _ => ("404 Not Found", NOTHING.to_string()),
    })?;
    Ok(Request { method, path: path.to_string(), query: query.to_string(), body })
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

/// The part of `NOTHING` by which an open page knows that Lillignode is
/// running again under a new word (`page.js`, `lookAgain`).
#[cfg(test)]
const MOVED_ON: &str = "Lillignode shows its page's address";

/// Said to a request without the word. Nothing of the page, and no data.
const NOTHING: &str =
    "Nothing here. Lillignode shows its page's address, word and all, when it starts and in its \
                       status report.";

/// True when `given` is `wanted`, compared so that how long it takes says
/// nothing about how much of it was right.
fn same(given: &str, wanted: &str) -> bool {
    given.len() == wanted.len() && given.bytes().zip(wanted.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

fn answer(stream: &TcpStream, serving: &Serving) -> std::io::Result<()> {
    // A browser that stops reading or never finishes asking does not hold
    // a place for ever.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    // Everything is under the word: /<word>/ and what is beside it.
    let word = word_now(&serving.word);
    let request = match read_request(stream, serving.port, serving.token.is_some(), &word) {
        Ok(request) => request,
        Err((status, why)) => return refuse(stream, status, &why),
    };
    let under = request.path.strip_prefix('/').unwrap_or("");
    let (given, rest) = under.split_once('/').map_or((under, None), |(given, rest)| (given, Some(rest)));
    if !same(given, &word) {
        return refuse(stream, "404 Not Found", NOTHING);
    }
    let token = serving.token();
    let token = token.as_deref();
    if request.method == Method::Post {
        return match (token, rest, &serving.typing, &serving.acting) {
            (Some(token), Some("send"), Some(typing), _) => take_typed(stream, &request.body, typing, token),
            (Some(token), Some("act"), _, Some(acting)) => take_acted(stream, &request.body, acting, token),
            _ => refuse(stream, "404 Not Found", "There is nothing here."),
        };
    }
    let Some(rest) = rest else {
        // The word without the slash after it: the page's own files are
        // found beside it, so the address must end with one.
        let head = head("308 Permanent Redirect", "text/plain; charset=utf-8");
        return (&*stream)
            .write_all(format!("{head}Location: /{word}/\r\nContent-Length: 0\r\n\r\n").as_bytes());
    };
    let reader = &serving.reader;
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
            let tokens = serving.tokens(token);
            state_once(stream, latest.map(|(_, published)| published), agent, &request.query, tokens)
        }
        "stream" => stream_states(stream, serving, &word, &request.query),
        // Read again at each request: the file is the user's to change.
        "board" => {
            let folder = serving.folder.as_deref();
            // If which cards were acted on cannot be read, the page is told
            // why, and offers no button: a card must not be acted on twice.
            let (waiting, acting_off) = match board::sent(folder) {
                Ok(waiting) => (waiting, None),
                Err(why) => (Vec::new(), Some(why)),
            };
            let mut shown = board::read(folder).to_json(&waiting);
            shown["acting_off"] = json!(acting_off);
            let shown = json!({ "board": shown });
            send(stream, "200 OK", "application/json", &shown.to_string())
        }
        _ => refuse(stream, "404 Not Found", "There is nothing here."),
    }
}

/// What the page's Send button sends, and nothing else: a line this does
/// not know is a request this page did not write.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendBody {
    token: String,
    run: String,
    agent: u64,
    text: String,
    agreed: bool,
    /// The button that presses Enter alone. Left out by an older page: no.
    #[serde(default)]
    enter: bool,
}

/// Said to a Send whose token is not this start's. The page gets the token
/// with its data, so a page that is listening has the right one.
const STALE: &str = "This page is out of step with Lillignode, so nothing was sent. Reload the page and \
                     send it again.";

/// Said when a Send was passed on and no word came back in time.
const NO_ANSWER: &str = "Lillignode did not say what became of this, so it is not known whether it was \
                         sent. Look at the agent's terminal before sending it again.";

/// What the page's buttons on a card send, and nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActBody {
    token: String,
    card: String,
    action: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    undo: bool,
    /// What the page showed of the card: its title, and its
    /// recommendation if it had one.
    #[serde(default)]
    title: String,
    #[serde(default)]
    recommendation: Option<String>,
}

/// Something done to a card on this page: check its token, pass it on, and
/// say back what became of it. Nothing here decides whether it may be done.
fn take_acted(stream: &TcpStream, body: &[u8], acting: &Acting, token: &str) -> std::io::Result<()> {
    let Ok(sent) = serde_json::from_slice::<ActBody>(body) else {
        return refuse(stream, "400 Bad Request", "The request could not be read.");
    };
    if !same(&sent.token, token) {
        return refuse(stream, "403 Forbidden", STALE);
    }
    let (answer, heard) = mpsc::channel();
    let acted = Acted {
        card: sent.card,
        action: sent.action,
        note: sent.note,
        undo: sent.undo,
        title: sent.title,
        recommendation: sent.recommendation,
        asked: Instant::now(),
        answer,
    };
    if !acting(acted) {
        return refuse(stream, "503 Service Unavailable", "Lillignode is closing. Nothing was sent.");
    }
    said_back(stream, &heard)
}

/// Wait to be told what became of something passed on, and say it back.
fn said_back(stream: &TcpStream, heard: &mpsc::Receiver<TypedAnswer>) -> std::io::Result<()> {
    match heard.recv_timeout(ANSWER_WITHIN) {
        Ok(TypedAnswer { outcome, words }) => {
            let outcome = match outcome {
                TypedOutcome::Sent => "sent",
                TypedOutcome::Ask => "ask",
                TypedOutcome::Refused => "refused",
            };
            let body = json!({ "outcome": outcome, "words": words });
            send(stream, "200 OK", "application/json", &body.to_string())
        }
        Err(_) => refuse(stream, "503 Service Unavailable", NO_ANSWER),
    }
}

/// A Send from this page: check its token, pass it on, and say back what
/// became of it. Nothing here decides what is sent to whom.
fn take_typed(stream: &TcpStream, body: &[u8], typing: &Typing, token: &str) -> std::io::Result<()> {
    let Ok(sent) = serde_json::from_slice::<SendBody>(body) else {
        return refuse(stream, "400 Bad Request", "The request could not be read.");
    };
    if !same(&sent.token, token) {
        return refuse(stream, "403 Forbidden", STALE);
    }
    let (answer, heard) = mpsc::channel();
    // Enter alone carries no text: one that does is not the page's own.
    if sent.enter && !sent.text.is_empty() {
        return refuse(stream, "400 Bad Request", "The request could not be read.");
    }
    let typed = Typed {
        run: sent.run,
        agent: sent.agent,
        text: sent.text,
        agreed: sent.agreed,
        enter: sent.enter,
        asked: Instant::now(),
        answer,
    };
    if !typing(typed) {
        return refuse(stream, "503 Service Unavailable", "Lillignode is closing. Nothing was sent.");
    }
    said_back(stream, &heard)
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
    tokens: Tokens,
) -> std::io::Result<()> {
    {
        match published {
            Some(published) => {
                let after = after_in(query, &published);
                let body = json!({
                    "at": millis(published.state.taken_at),
                    "state": state_json(&published, agent, after, tokens),
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
///
/// It ends when the word in the page's address is changed: the page then
/// looks again, and finds that its address is gone.
fn stream_states(mut stream: &TcpStream, serving: &Serving, word: &str, query: &str) -> std::io::Result<()> {
    let reader = &serving.reader;
    // A stream ends when the word changes, and the token changes only with
    // the word: the one read here is right for as long as this runs.
    let token = serving.token();
    let tokens = serving.tokens(token.as_deref());
    let agent = number(query, "agent");
    // For as long as this page listens, its agent's screen is copied.
    let _showing = agent.map(|agent| reader.show(agent));
    let mut after = None;
    stream.write_all(format!("{}\r\n", head("200 OK", "text/event-stream")).as_bytes())?;
    let (mut count, mut sent) = (0, String::new());
    loop {
        let read = reader.newer_than(count, Duration::from_secs(1));
        if !same(word, &word_now(&serving.word)) {
            return stream.flush();
        }
        match read {
            Read::Copy(newer, published) => {
                count = newer;
                let at = millis(published.state.taken_at);
                let from = after.unwrap_or_else(|| after_in(query, &published));
                let mut state = state_json(&published, agent, from, tokens);
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
/// the events numbered after `after`. Where the page may type, or act on
/// a card, the token what it sends must carry.
fn state_json(published: &Published, agent: Option<u64>, after: u64, tokens: Tokens) -> Value {
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
        "typing": tokens.0,
        "acting": tokens.1,
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
        let served = serve(reader, 0, None, None).expect("a free port on this machine");
        let word: &'static str = Box::leak(word_now(&served.word).into_boxed_str());
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
        // The page tells a new start from a lost one by these words: the
        // two must not drift apart.
        assert!(NOTHING.contains(MOVED_ON) && JS.contains(MOVED_ON), "{MOVED_ON}");
        // With it, all of it.
        assert!(raw(&format!("/{word}/")).contains("<title>Lillignode</title>"));
        assert!(raw(&format!("/{word}/state")).contains("\"agents\""));
        // The word without the slash after it is sent on to it.
        let moved = raw(&format!("/{word}"));
        assert!(moved.starts_with("HTTP/1.1 308"), "{moved}");
        assert!(moved.contains(&format!("Location: /{word}/\r\n")), "{moved}");
    }

    #[test]
    fn a_request_sent_a_byte_at_a_time_is_dropped_when_its_time_is_up() {
        let (_publisher, at) = served();
        let began = std::time::Instant::now();
        let mut stream = TcpStream::connect(at.address).unwrap();
        // When the page lets go of the connection, by its 400 or by closing
        // it (on Windows, closing with bytes unread resets it, and the 400
        // may be lost): either way the reading here ends.
        let mut reading = stream.try_clone().unwrap();
        let ended = std::thread::spawn(move || {
            let mut answer = Vec::new();
            let _ = reading.read_to_end(&mut answer);
            began.elapsed()
        });
        // One byte every second, never ending its first line: each read
        // alone is well inside the five seconds a read may wait.
        for byte in b"GET /aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".iter() {
            if ended.is_finished() || stream.write_all(&[*byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        let held = ended.join().unwrap();
        assert!(held <= HEAD_WITHIN + Duration::from_secs(3), "held for {held:?}");
        assert!(held >= HEAD_WITHIN - Duration::from_secs(1), "let go too soon, at {held:?}");
    }

    /// A page that may type, passing on to `typing`: where it is, and the
    /// token that came with its data.
    fn served_passing_to(typing: Typing) -> (Publisher, At, String) {
        let (publisher, reader) = channel();
        let served = serve(reader, 0, None, Some(typing)).expect("a free port on this machine");
        assert!(served.can_type);
        let word: &'static str = Box::leak(word_now(&served.word).into_boxed_str());
        let at = At { address: served.address, word };
        // The token is in no page file: it comes only with the data.
        for path in ["/", "/page.js", "/page.css"] {
            assert!(!get(at, path).contains("\"typing\":"), "{path}");
        }
        publisher.publish(published(1));
        let state = get(at, "/state");
        let body: Value = serde_json::from_str(state.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let token = body["state"]["typing"].as_str().expect("the token comes with the data").to_string();
        (publisher, at, token)
    }

    /// A page that may type. What it passes on arrives at the returned
    /// receiver, already answered with `outcome` and these words.
    fn served_typing(outcome: TypedOutcome) -> (Publisher, At, String, mpsc::Receiver<Typed>) {
        let (passed, arrived) = mpsc::channel();
        let passed = Mutex::new(passed);
        let typing: Typing = Arc::new(move |typed: Typed| {
            let _ = typed.answer.send(TypedAnswer { outcome, words: "What Lillignode said.".into() });
            passed.lock().unwrap().send(typed).is_ok()
        });
        let (publisher, at, token) = served_passing_to(typing);
        (publisher, at, token, arrived)
    }

    /// The headers this page's own Send carries.
    fn own(at: At) -> String {
        format!(
            "Origin: http://127.0.0.1:{}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n",
            at.port()
        )
    }

    fn post_to(at: At, path: &str, headers: &str, body: &str) -> String {
        let (port, length) = (at.port(), body.len());
        ask_raw(
            at,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{headers}Content-Length: {length}\r\n\r\n{body}"
            ),
        )
    }

    fn post(at: At, headers: &str, body: &str) -> String {
        post_to(at, &format!("/{}/send", at.word), headers, body)
    }

    fn send_body(token: &str) -> String {
        json!({ "token": token, "run": "7", "agent": 2, "text": "do this\nthen that", "agreed": false })
            .to_string()
    }

    #[test]
    fn a_page_that_is_read_only_takes_nothing_and_is_given_no_token() {
        let (publisher, at) = served();
        publisher.publish(published(1));
        let state = get(at, "/state");
        let body: Value = serde_json::from_str(state.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["state"]["typing"], Value::Null);
        // Even a Send made exactly as the page would make it.
        let answer = post(at, &own(at), &send_body("anything"));
        assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
        assert!(answer.ends_with(TAKES_NOTHING), "{answer}");
    }

    #[test]
    fn a_send_from_this_page_with_its_token_is_passed_on_and_what_came_of_it_said_back() {
        for (outcome, word) in
            [(TypedOutcome::Sent, "sent"), (TypedOutcome::Ask, "ask"), (TypedOutcome::Refused, "refused")]
        {
            let (_publisher, at, token, arrived) = served_typing(outcome);
            assert_eq!(token.len(), 2 * WORD_LETTERS, "160 random bits");
            assert_ne!(&token[..WORD_LETTERS], at.word, "not the word, nor made from it");
            let answer = post(at, &own(at), &send_body(&token));
            assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
            let said: Value = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(said, json!({ "outcome": word, "words": "What Lillignode said." }));
            // Passed on as it came: the page chose nothing about it.
            let typed = arrived.try_recv().expect("passed on");
            assert_eq!(
                (typed.run.as_str(), typed.agent, typed.text.as_str(), typed.agreed, typed.enter),
                ("7", 2, "do this\nthen that", false, false)
            );
            assert!(arrived.try_recv().is_err(), "once");
        }
    }

    #[test]
    fn nothing_is_passed_on_for_a_send_that_is_not_this_pages_own() {
        let (_publisher, at, token, arrived) = served_typing(TypedOutcome::Sent);
        let port = at.port();
        let good = send_body(&token);
        let json = "Content-Type: application/json\r\n";
        let mine = format!("Origin: http://127.0.0.1:{port}\r\n");
        let cases: Vec<(&str, String, String, &str)> = vec![
            // Who is asking.
            ("no origin at all", json.to_string(), good.clone(), "403"),
            ("another site", format!("Origin: https://evil.example\r\n{json}"), good.clone(), "403"),
            (
                "another port on this machine",
                format!("Origin: http://127.0.0.1:1\r\n{json}"),
                good.clone(),
                "403",
            ),
            (
                "this machine by its other name",
                format!("Origin: http://localhost:{port}\r\n{json}"),
                good.clone(),
                "403",
            ),
            ("a page with no origin", format!("Origin: null\r\n{json}"), good.clone(), "403"),
            ("two origins", format!("{mine}Origin: https://evil.example\r\n{json}"), good.clone(), "400"),
            (
                "the browser says another site",
                format!("{mine}Sec-Fetch-Site: cross-site\r\n{json}"),
                good.clone(),
                "403",
            ),
            (
                "the browser says a sibling site",
                format!("{mine}Sec-Fetch-Site: same-site\r\n{json}"),
                good.clone(),
                "403",
            ),
            (
                "the browser says a typed address",
                format!("{mine}Sec-Fetch-Site: none\r\n{json}"),
                good.clone(),
                "403",
            ),
            // What a form or a link can send.
            (
                "a form",
                format!("{mine}Content-Type: application/x-www-form-urlencoded\r\n"),
                good.clone(),
                "415",
            ),
            ("plain text", format!("{mine}Content-Type: text/plain\r\n"), good.clone(), "415"),
            ("no kind said", mine.clone(), good.clone(), "415"),
            // The token.
            ("a wrong token", own(at), send_body(&token.to_uppercase()), "403"),
            ("the word for a token", own(at), send_body(at.word), "403"),
            ("an empty token", own(at), send_body(""), "403"),
            ("no token", own(at), r#"{"run":"7","agent":2,"text":"x","agreed":true}"#.to_string(), "400"),
            // A request this page did not write.
            ("a line the page never sends", own(at), good.replacen('{', r#"{"front":true,"#, 1), "400"),
            ("not json", own(at), "text=do+this".to_string(), "400"),
            ("too much", own(at), "x".repeat(MAX_BODY + 1), "413"),
        ];
        for (what, headers, body, status) in cases {
            let answer = post(at, &headers, &body);
            assert!(answer.starts_with(&format!("HTTP/1.1 {status}")), "{what}: {answer}");
            assert!(arrived.try_recv().is_err(), "{what}: passed on");
            assert!(!answer.contains(&token) && !answer.contains(at.word), "{what}: says a secret back");
        }
        // Without the word: the same nothing as any other request.
        for path in ["/send".to_string(), format!("/{}x/send", at.word), "/aaaaaaaaaaaaaaaa/send".to_string()]
        {
            let answer = post_to(at, &path, &own(at), &good);
            assert!(answer.starts_with("HTTP/1.1 404") && answer.ends_with(NOTHING), "{path}: {answer}");
        }
        // Under the word, only `send` takes anything.
        for path in ["state", "stream", "", "send/", "send/x"] {
            let answer = post_to(at, &format!("/{}/{path}", at.word), &own(at), &good);
            assert!(answer.starts_with("HTTP/1.1 404"), "{path}: {answer}");
        }
        // No link sends: a read of the same address passes nothing on,
        // whatever follows it.
        // Enter alone with text is not the page's own.
        let both =
            json!({ "token": token, "run": "7", "agent": 2, "text": "x", "agreed": false, "enter": true });
        assert!(post(at, &own(at), &both.to_string()).starts_with("HTTP/1.1 400"));
        assert!(arrived.try_recv().is_err(), "Enter with text: passed on");
        let link = format!("/send?token={token}&run=7&agent=2&text=x&agreed=true");
        assert!(get(at, &link).starts_with("HTTP/1.1 404"));
        // Nor any other way of asking.
        for method in ["PUT", "PATCH", "DELETE", "OPTIONS"] {
            let answer = ask_raw(
                at,
                &format!(
                    "{method} /{}/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Content-Length: {}\r\n\r\n{good}",
                    at.word,
                    own(at),
                    good.len()
                ),
            );
            assert!(answer.starts_with("HTTP/1.1 405"), "{method}: {answer}");
            assert!(!answer.to_ascii_lowercase().contains("access-control-allow"), "{method}: {answer}");
        }
        // A name some web site pointed at this machine.
        let rebound = format!(
            "POST /{}/send HTTP/1.1\r\nHost: evil.example:{port}\r\nOrigin: http://evil.example:{port}\r\n{json}\
             Content-Length: {}\r\n\r\n{good}",
            at.word,
            good.len()
        );
        assert!(ask_raw(at, &rebound).starts_with("HTTP/1.1 403"));
        // No length said, and a body sent in pieces.
        let unsized_ =
            format!("POST /{}/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}\r\n{good}", at.word, own(at));
        assert!(ask_raw(at, &unsized_).starts_with("HTTP/1.1 411"));
        let chunked = format!(
            "POST /{}/send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Transfer-Encoding: chunked\r\n\r\n",
            at.word,
            own(at)
        );
        assert!(ask_raw(at, &chunked).starts_with("HTTP/1.1 400"));
        assert!(arrived.try_recv().is_err(), "none of it was passed on");
        // And after all that, this page's own Send still is.
        assert!(post(at, &own(at), &good).starts_with("HTTP/1.1 200"));
        assert!(arrived.try_recv().is_ok());
    }

    #[test]
    fn the_board_is_read_from_its_file_under_the_word_and_nowhere_else() {
        let folder = scratch("board");
        let (_publisher, reader) = channel();
        let page = serve(reader, 0, Some(&folder), None).unwrap();
        let word: &'static str = Box::leak(word_now(&page.word).into_boxed_str());
        let at = At { address: page.address, word };
        let board = || {
            let answer = get(at, "/board");
            assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
            let body: Value = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            body["board"].clone()
        };
        // No file: empty, and nothing wrong.
        assert_eq!((board()["empty"].as_bool(), board()["problem"].is_null()), (Some(true), true));
        // The file is read again at each request.
        std::fs::write(folder.join(board::FILE), include_str!("../examples/board-standin.json")).unwrap();
        let shown = board();
        assert!(shown["decide"][0]["sent"].is_null(), "{shown}");
        assert_eq!(shown["decide"].as_array().unwrap().len(), 4, "{shown}");
        // A card acted on says so, with what and when.
        let c1 = board::read(Some(&folder)).decide.into_iter().find(|c| c.id == "c1").unwrap();
        let seen = board::Seen { title: c1.title, recommendation: c1.recommendation };
        board::act(Some(&folder), "c1", "approve", "", &seen, SystemTime::now()).unwrap();
        let acted = board()["decide"].as_array().unwrap().iter().find(|c| c["id"] == "c1").cloned().unwrap();
        assert_eq!(acted["sent"]["action"], "approve", "{acted}");
        assert!(acted["sent"]["at"].is_u64());
        // The fire is the Hub's: the page is told, and offered nothing.
        let fire = board()["decide"].as_array().unwrap().iter().find(|c| c["id"] == "c2").cloned().unwrap();
        assert_eq!((fire["hub_only"].clone(), fire["actions"].clone()), (json!(true), json!([])), "{fire}");
        assert_eq!(shown["yours"].as_array().unwrap().len(), 2, "{shown}");
        assert_eq!(shown["know"].as_array().unwrap().len(), 2, "{shown}");
        // Which cards were acted on cannot be read: the page is told why,
        // and that nothing can be acted on.
        assert!(board()["acting_off"].is_null());
        std::fs::write(folder.join(board::SENT), "not what Lillignode wrote").unwrap();
        let off = board()["acting_off"].as_str().unwrap_or_default().to_string();
        assert!(off.ends_with("no card can be acted on from the page."), "{off}");
        std::fs::write(folder.join(board::FILE), "{").unwrap();
        assert!(board()["problem"]
            .as_str()
            .unwrap()
            .starts_with("The Board's file, board.json, was not read"));
        // Without the word: nothing, as for anything else.
        let raw = ask_raw(at, &format!("GET /board HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", at.port()));
        assert!(raw.starts_with("HTTP/1.1 404") && raw.ends_with(NOTHING), "{raw}");
        drop(page);
        // With no folder there is no file to read.
        let (_publisher, at) = served();
        assert!(get(at, "/board").contains("\"empty\":true"));
    }

    #[test]
    fn a_page_that_may_type_has_a_word_that_is_new_each_start_and_kept_nowhere() {
        let folder = scratch("typing-word");
        let start = |typing: bool| {
            let (publisher, reader) = channel();
            let typing: Option<Typing> = typing.then(|| Arc::new(|_: Typed| false) as Typing);
            (publisher, serve(reader, 0, Some(&folder), typing).unwrap())
        };
        // Read only first: a word is kept.
        let (_p, reading) = start(false);
        let kept = word_now(&reading.word);
        assert_eq!(read_word(&folder).as_deref(), Some(kept.as_str()));
        // With typing: another word each start, and the kept one is neither
        // used nor changed.
        let (_p, one) = start(true);
        let (_p, two) = start(true);
        let (first, second) = (word_now(&one.word), word_now(&two.word));
        assert!(first != kept && second != kept && first != second, "{kept} {first} {second}");
        assert_eq!(one.word_not_kept, None, "not kept on purpose, so nothing to report");
        assert_eq!(read_word(&folder).as_deref(), Some(kept.as_str()));
        // A new word when asked: still nothing written, and a new token.
        let mut one = one;
        let before = word_now(one.token.as_ref().unwrap());
        assert_eq!(one.change_word(), None);
        assert_ne!(word_now(&one.word), first);
        assert_ne!(word_now(one.token.as_ref().unwrap()), before, "the token goes with the word");
        assert_eq!(read_word(&folder).as_deref(), Some(kept.as_str()));
        // Read only again: the bookmark still works.
        let (_p, reading) = start(false);
        assert_eq!(word_now(&reading.word), kept);
    }

    #[test]
    fn the_token_from_before_a_new_word_sends_nothing() {
        let (passed, arrived) = mpsc::channel();
        let passed = Mutex::new(passed);
        let typing: Typing = Arc::new(move |typed: Typed| {
            let _ = typed.answer.send(TypedAnswer { outcome: TypedOutcome::Sent, words: "Sent.".into() });
            passed.lock().unwrap().send(typed).is_ok()
        });
        let (publisher, reader) = channel();
        let mut served = serve(reader, 0, None, Some(typing)).unwrap();
        publisher.publish(published(1));
        let token_at = |served: &Served| {
            let word: &'static str = Box::leak(word_now(&served.word).into_boxed_str());
            let at = At { address: served.address, word };
            let state = get(at, "/state");
            let body: Value = serde_json::from_str(state.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            (at, body["state"]["typing"].as_str().unwrap().to_string())
        };
        let (old_at, old_token) = token_at(&served);
        served.change_word();
        let (new_at, new_token) = token_at(&served);
        assert_ne!(old_token, new_token);
        // Old word: nothing. New word with the old token: refused.
        assert!(post(old_at, &own(old_at), &send_body(&old_token)).starts_with("HTTP/1.1 404"));
        let stale = post(new_at, &own(new_at), &send_body(&old_token));
        assert!(stale.starts_with("HTTP/1.1 403") && stale.ends_with(STALE), "{stale}");
        assert!(arrived.try_recv().is_err());
        assert!(post(new_at, &own(new_at), &send_body(&new_token)).starts_with("HTTP/1.1 200"));
        assert!(arrived.try_recv().is_ok());
    }

    #[test]
    fn a_page_that_may_type_answers_to_one_address_and_tells_nobody_without_the_word_that_it_may() {
        let (_publisher, at, token, arrived) = served_typing(TypedOutcome::Sent);
        let port = at.port();
        // This machine's other name: refused for reading and for sending.
        let named = format!("GET /{}/state HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n", at.word);
        assert!(ask_raw(at, &named).starts_with("HTTP/1.1 403"));
        let good = send_body(&token);
        let named = format!(
            "POST /{}/send HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{good}",
            at.word,
            good.len()
        );
        assert!(ask_raw(at, &named).starts_with("HTTP/1.1 403"));
        assert!(arrived.try_recv().is_err());
        // Without the word, every way of asking that a read-only page and a
        // typing page would refuse differently gets the same nothing.
        let (_reading, read_only) = served();
        for page in [at, read_only] {
            let port = page.port();
            let host = format!("Host: 127.0.0.1:{port}\r\n");
            let mine = format!("{host}Origin: http://127.0.0.1:{port}\r\n");
            for (what, request) in [
                ("a send as the page makes it", format!("POST /send HTTP/1.1\r\n{mine}Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}")),
                ("a form", format!("POST /wrongwordwrongwor/send HTTP/1.1\r\n{mine}Content-Type: text/plain\r\nContent-Length: 1\r\n\r\nx")),
                ("no length", format!("POST /send HTTP/1.1\r\n{mine}Content-Type: application/json\r\n\r\n")),
                ("too long", format!("POST /send HTTP/1.1\r\n{mine}Content-Type: application/json\r\nContent-Length: 99999999\r\n\r\n")),
                ("another method", format!("PUT /send HTTP/1.1\r\n{host}\r\n")),
            ] {
                let answer = ask_raw(page, &request);
                assert!(answer.starts_with("HTTP/1.1 404") && answer.ends_with(NOTHING), "{what}: {answer}");
            }
        }
    }

    /// A page that may act on cards and cannot type. What it passes on
    /// arrives at the returned receiver, already answered.
    fn served_acting() -> (Publisher, At, String, mpsc::Receiver<Acted>) {
        let (passed, arrived) = mpsc::channel();
        let passed = Mutex::new(passed);
        let acting: Acting = Arc::new(move |acted: Acted| {
            let _ = acted
                .answer
                .send(TypedAnswer { outcome: TypedOutcome::Sent, words: "Sent: Approve.".into() });
            passed.lock().unwrap().send(acted).is_ok()
        });
        let (publisher, reader) = channel();
        let served = serve(reader, 0, None, Ways { typing: None, acting: Some(acting) }).unwrap();
        assert!(served.can_act && !served.can_type);
        let word: &'static str = Box::leak(word_now(&served.word).into_boxed_str());
        let at = At { address: served.address, word };
        publisher.publish(published(1));
        let state = get(at, "/state");
        let body: Value = serde_json::from_str(state.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["state"]["typing"], Value::Null, "it cannot type, and is not told it can");
        let token = body["state"]["acting"].as_str().expect("the token comes with the data").to_string();
        (publisher, at, token, arrived)
    }

    fn act_to(at: At, headers: &str, body: &str) -> String {
        post_to(at, &format!("/{}/act", at.word), headers, body)
    }

    fn act_body(token: &str) -> String {
        json!({
            "token": token, "card": "c1", "action": "revise", "note": "Say when.", "undo": false,
            "title": "Keep it?", "recommendation": "Keep it.",
        })
        .to_string()
    }

    #[test]
    fn acting_on_a_card_is_passed_on_only_from_this_page_with_its_token() {
        let (_publisher, at, token, arrived) = served_acting();
        let answer = act_to(at, &own(at), &act_body(&token));
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        let said: Value = serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(said, json!({ "outcome": "sent", "words": "Sent: Approve." }));
        // Passed on as it came: the page chose nothing about it.
        let acted = arrived.try_recv().expect("passed on");
        assert_eq!(
            (acted.card.as_str(), acted.action.as_str(), acted.note.as_str(), acted.undo),
            ("c1", "revise", "Say when.", false)
        );
        assert_eq!(
            (acted.title.as_str(), acted.recommendation.as_deref()),
            ("Keep it?", Some("Keep it.")),
            "with what the page showed of the card"
        );
        // An undo is passed on as one.
        let undo = json!({ "token": token, "card": "c1", "action": "revise", "undo": true }).to_string();
        assert!(act_to(at, &own(at), &undo).starts_with("HTTP/1.1 200"));
        assert!(arrived.try_recv().expect("passed on").undo);

        let port = at.port();
        let good = act_body(&token);
        let json = "Content-Type: application/json\r\n";
        let mine = format!("Origin: http://127.0.0.1:{port}\r\n");
        let cases: Vec<(&str, String, String, &str)> = vec![
            ("no origin at all", json.to_string(), good.clone(), "403"),
            ("another site", format!("Origin: https://evil.example\r\n{json}"), good.clone(), "403"),
            (
                "another port on this machine",
                format!("Origin: http://127.0.0.1:1\r\n{json}"),
                good.clone(),
                "403",
            ),
            ("a page with no origin", format!("Origin: null\r\n{json}"), good.clone(), "403"),
            (
                "the browser says another site",
                format!("{mine}Sec-Fetch-Site: cross-site\r\n{json}"),
                good.clone(),
                "403",
            ),
            (
                "a form",
                format!("{mine}Content-Type: application/x-www-form-urlencoded\r\n"),
                good.clone(),
                "415",
            ),
            ("plain text", format!("{mine}Content-Type: text/plain\r\n"), good.clone(), "415"),
            ("a wrong token", own(at), act_body(&token.to_uppercase()), "403"),
            ("the word for a token", own(at), act_body(at.word), "403"),
            ("no token", own(at), r#"{"card":"c1","action":"approve"}"#.to_string(), "400"),
            (
                "a line the page never sends",
                own(at),
                good.replacen('{', r#"{"status":"approved","#, 1),
                "400",
            ),
            ("no card named", own(at), json!({ "token": token, "action": "approve" }).to_string(), "400"),
            ("not json", own(at), "card=c1&action=approve".to_string(), "400"),
        ];
        for (what, headers, body, status) in cases {
            let answer = act_to(at, &headers, &body);
            assert!(answer.starts_with(&format!("HTTP/1.1 {status}")), "{what}: {answer}");
            assert!(arrived.try_recv().is_err(), "{what}: passed on");
            assert!(!answer.contains(&token) && !answer.contains(at.word), "{what}: says a secret back");
        }
        // Without the word, and by a plain link: nothing.
        let nowhere = post_to(at, "/act", &own(at), &good);
        assert!(nowhere.starts_with("HTTP/1.1 404") && nowhere.ends_with(NOTHING), "{nowhere}");
        assert!(get(at, &format!("/act?token={token}&card=c1&action=approve")).starts_with("HTTP/1.1 404"));
        // This machine's other name is refused, for reading and for acting.
        let named = format!("GET /{}/state HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n", at.word);
        assert!(ask_raw(at, &named).starts_with("HTTP/1.1 403"));
        let length = good.len();
        let named = format!(
            "POST /{}/act HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\n{json}\
             Content-Length: {length}\r\n\r\n{good}",
            at.word
        );
        assert!(ask_raw(at, &named).starts_with("HTTP/1.1 403"));
        // What the browser itself says of where the request comes from.
        for site in ["same-site", "none"] {
            let said = format!("{mine}Sec-Fetch-Site: {site}\r\n{json}");
            assert!(act_to(at, &said, &good).starts_with("HTTP/1.1 403"), "{site}");
        }
        assert!(act_to(at, &own(at), &"x".repeat(MAX_BODY + 1)).starts_with("HTTP/1.1 413"));
        // No other way of asking acts.
        for method in ["PUT", "PATCH", "DELETE", "OPTIONS"] {
            let asked = format!(
                "{method} /{}/act HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Content-Length: {length}\r\n\r\n{good}",
                at.word,
                own(at)
            );
            assert!(ask_raw(at, &asked).starts_with("HTTP/1.1 405"), "{method}");
        }
        assert!(arrived.try_recv().is_err(), "none of it was passed on");
    }

    #[test]
    fn each_way_of_sending_is_on_only_if_it_was_handed_over() {
        // Acting, not typing: a Send made as the page makes one is nothing.
        let (_publisher, at, token, arrived) = served_acting();
        let answer = post(at, &own(at), &send_body(&token));
        assert!(answer.starts_with("HTTP/1.1 404"), "{answer}");
        assert!(arrived.try_recv().is_err());
        // Typing, not acting: an action made as the page makes one is nothing.
        let (_publisher, at, token, typed) = served_typing(TypedOutcome::Sent);
        let state = get(at, "/state");
        let body: Value = serde_json::from_str(state.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["state"]["acting"], Value::Null, "it cannot act, and is not told it can");
        let answer = act_to(at, &own(at), &act_body(&token));
        assert!(answer.starts_with("HTTP/1.1 404"), "{answer}");
        assert!(typed.try_recv().is_err());
        // Neither: read only, and it takes nothing.
        let (publisher, at) = served();
        publisher.publish(published(1));
        let state = get(at, "/state");
        let body: Value = serde_json::from_str(state.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!((&body["state"]["typing"], &body["state"]["acting"]), (&Value::Null, &Value::Null));
        let answer = act_to(at, &own(at), &act_body("anything"));
        assert!(answer.starts_with("HTTP/1.1 405") && answer.ends_with(TAKES_NOTHING), "{answer}");
    }

    #[test]
    fn a_page_that_may_act_has_a_word_that_is_new_each_start_and_kept_nowhere() {
        let folder = scratch("acting-word");
        let start = |acting: bool| {
            let (publisher, reader) = channel();
            let acting: Option<Acting> = acting.then(|| Arc::new(|_: Acted| false) as Acting);
            (publisher, serve(reader, 0, Some(&folder), Ways { typing: None, acting }).unwrap())
        };
        let (_p, reading) = start(false);
        let kept = word_now(&reading.word);
        let (_p, one) = start(true);
        let (_p, mut two) = start(true);
        let (first, second) = (word_now(&one.word), word_now(&two.word));
        assert!(first != kept && second != kept && first != second, "{kept} {first} {second}");
        assert_eq!(read_word(&folder).as_deref(), Some(kept.as_str()), "the kept word is left alone");
        let before = word_now(two.token.as_ref().unwrap());
        assert_eq!(two.change_word(), None);
        assert_ne!(word_now(two.token.as_ref().unwrap()), before, "the token goes with the word");
        assert_eq!(read_word(&folder).as_deref(), Some(kept.as_str()));
    }

    #[test]
    fn a_send_that_is_never_answered_says_it_is_not_known_whether_it_was_sent() {
        let held = Arc::new(Mutex::new(Vec::new()));
        let keep = held.clone();
        // Taken, and never answered.
        let typing: Typing = Arc::new(move |typed: Typed| {
            keep.lock().unwrap().push(typed);
            true
        });
        let (_publisher, at, token) = served_passing_to(typing);
        let answer = post(at, &own(at), &send_body(&token));
        assert!(answer.starts_with("HTTP/1.1 503") && answer.ends_with(NO_ANSWER), "{answer}");
        assert_eq!(held.lock().unwrap().len(), 1);
        // Nowhere left to pass it: Lillignode is closing.
        let (_publisher, at, token) = served_passing_to(Arc::new(|_| false));
        let answer = post(at, &own(at), &send_body(&token));
        assert!(answer.starts_with("HTTP/1.1 503") && answer.ends_with("Nothing was sent."), "{answer}");
    }

    /// A folder of this test's own, empty.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-page-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn served_in(folder: &Path) -> (Publisher, Served) {
        let (publisher, reader) = channel();
        let served = serve(reader, 0, Some(folder), None).expect("a free port on this machine");
        publisher.publish(published(1));
        (publisher, served)
    }

    fn status_at(served: &Served, word: &str, path: &str) -> String {
        let port = served.address.port();
        let mut stream = TcpStream::connect(served.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream
            .write_all(format!("GET /{word}/{path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
            .unwrap();
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        answer
    }

    #[test]
    fn the_word_is_made_once_kept_where_only_this_user_can_read_it_and_used_at_every_start() {
        let folder = scratch("kept");
        let (first, served) = served_in(&folder);
        #[cfg(windows)]
        assert_eq!(served.word_not_kept, None, "the folder's own list: {:?}", private::who_may_use(&folder));
        assert_eq!(served.word_not_kept, None);
        let word = word_now(&served.word);
        assert_eq!(std::fs::read_to_string(folder.join(WORD_FILE)).unwrap().trim(), word);
        assert!(!folder.join(format!("{WORD_FILE}.new")).exists(), "nothing left beside it");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(folder.join(WORD_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "this user, and nobody else");
        }
        #[cfg(windows)]
        {
            // Nothing handed down from the folder, and nobody named but
            // this user (and, on some machines, the system and its
            // administrators, who can read any file).
            let list = private::who_may_use(&folder.join(WORD_FILE));
            assert!(private::only_this_users(&folder.join(WORD_FILE)), "{list:?}");
            assert!(list.as_deref().is_some_and(|list| !list.contains("ID;")), "{list:?}");
            // A file that everyone on the machine may read is not: the
            // check can say no.
            let open = folder.join("open-to-all");
            std::fs::write(&open, "x").unwrap();
            let everyone = std::ffi::OsString::from("*S-1-1-0:R");
            private::system_tool("icacls.exe", &[open.clone().into(), "/grant".into(), everyone]).unwrap();
            assert!(!private::only_this_users(&open), "{:?}", private::who_may_use(&open));
        }
        // Lillignode closes and is started again: the same address, so a
        // bookmark still works and an open page finds it again.
        let url = served.url();
        drop(first);
        let (_second, again) = served_in(&folder);
        assert_eq!(word_now(&again.word), word);
        assert_eq!(again.url().rsplit(':').next().unwrap().split_once('/').unwrap().1, format!("{word}/"));
        assert!(url.ends_with(&format!("/{word}/")));
        assert!(status_at(&again, &word, "state").starts_with("HTTP/1.1 200"));
    }

    #[test]
    fn a_kept_word_that_is_not_a_word_or_that_others_could_read_is_replaced() {
        let folder = scratch("replaced");
        for bad in
            ["", "short", "ABCDEFGHIJKMNPQR", "abcdefghijkmnpq0", "abcdefghijkmnpqrs", "../../etc/passwd"]
        {
            std::fs::write(folder.join(WORD_FILE), bad).unwrap();
            let (word, not_kept) = kept_word(Some(&folder));
            assert!(is_a_word(&word) && word != bad, "{bad:?} gave {word:?}");
            assert_eq!(not_kept, None);
            assert_eq!(read_word(&folder).as_deref(), Some(word.as_str()), "the new one is kept");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = folder.join(WORD_FILE);
            let open = "abcdefghijkmnpqr";
            std::fs::write(&path, open).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let (word, _) = kept_word(Some(&folder));
            assert_ne!(word, open, "a word others could read is no longer a secret");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        #[cfg(windows)]
        {
            // A file as open as its folder, as a copy or a restore makes.
            let path = folder.join(WORD_FILE);
            let _ = std::fs::remove_file(&path);
            let open = "abcdefghijkmnpqr";
            std::fs::write(&path, open).unwrap();
            let everyone = std::ffi::OsString::from("*S-1-1-0:R");
            private::system_tool("icacls.exe", &[path.clone().into(), "/grant".into(), everyone]).unwrap();
            assert!(!private::only_this_users(&path));
            let (word, not_kept) = kept_word(Some(&folder));
            assert_ne!(word, open, "a word others could read is no longer a secret");
            assert_eq!(not_kept, None);
            assert!(private::only_this_users(&path));
        }
        // Nowhere to keep it: a word all the same, and the reason said.
        let (word, not_kept) = kept_word(None);
        assert!(is_a_word(&word));
        assert!(not_kept.is_some());
        // A folder that cannot be written in: the same, and nothing of an
        // earlier word is left to come back.
        let file = folder.join("a-file");
        std::fs::write(&file, "x").unwrap();
        let (word, not_kept) = kept_word(Some(&file));
        assert!(is_a_word(&word));
        assert!(not_kept.is_some(), "said, not passed over");
    }

    #[test]
    fn a_new_word_when_asked_stops_the_old_address_at_once_and_is_the_one_kept() {
        let folder = scratch("changed");
        let (publisher, mut served) = served_in(&folder);
        let old = word_now(&served.word);
        // A page open on the old address, listening.
        let port = served.address.port();
        let mut open = TcpStream::connect(served.address).unwrap();
        open.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        open.write_all(format!("GET /{old}/stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
            .unwrap();
        let listening = std::thread::spawn(move || {
            let began = Instant::now();
            let mut heard = String::new();
            let _ = open.read_to_string(&mut heard);
            (began.elapsed(), heard)
        });
        std::thread::sleep(Duration::from_millis(300));

        assert_eq!(served.change_word(), None, "kept");
        let new = word_now(&served.word);
        assert!(is_a_word(&new) && new != old);
        assert!(served.url().ends_with(&format!("/{new}/")));
        // The old address: nothing, as for any wrong word. The new: all.
        let gone = status_at(&served, &old, "state");
        assert!(gone.starts_with("HTTP/1.1 404") && gone.ends_with(NOTHING), "{gone}");
        assert!(status_at(&served, &new, "state").starts_with("HTTP/1.1 200"));
        // The page that was open is let go within moments, and is told
        // nothing of the new word.
        publisher.publish(published(2));
        let (held, heard) = listening.join().unwrap();
        assert!(held < Duration::from_secs(4), "held for {held:?}");
        assert!(!heard.contains(&new), "{heard}");
        assert!(!heard.contains("\"as_of\":2"), "nothing more is sent to the old address: {heard}");
        // And it is the new one that the next start uses.
        assert_eq!(read_word(&folder).as_deref(), Some(new.as_str()));
        drop(publisher);
        let (_next, again) = served_in(&folder);
        assert_eq!(word_now(&again.word), new);
    }

    #[test]
    fn a_word_is_random_and_made_of_letters_that_are_not_mistaken() {
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
        // With nowhere to keep one, each start makes its own.
        let (_, one) = served();
        let (_, two) = served();
        assert_ne!(one.word, two.word);
        // Compared whole: a word that is only its start, or longer, is not it.
        assert!(same("abc", "abc"));
        assert!(!same("ab", "abc") && !same("abcd", "abc") && !same("abd", "abc"));
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
        // The Board's own headings.
        for heading in ["Decide", "Only you can do", "Good to know"] {
            assert!(HTML.contains(&format!(">{heading}</h3>")), "{heading}");
        }
        assert!(!HTML.contains("Not connected yet"), "the Board is connected to its file now");
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
