//! Development tool, not part of the product.
//!
//! Checks the read-only page against the real `lillignode` program, in a
//! terminal the check owns, with stand-in agents. No window, no browser,
//! no real agent, no real settings (see `owned/mod.rs`).
//!
//!   cargo build --release --all-targets && cargo run --release --example page_check
//!
//! It asks the page what a browser would ask (`/`, `/state`, `/stream`) and
//! checks the answers: a status line a stand-in forges reaches the page
//! within a second; an agent that needs you is said to, with since when;
//! a request that is not a plain read from this machine's own address is
//! refused; and when Lillignode is stopped outright the stream ends.
//!
//! What the page then draws is a browser's business. For a check that
//! drives a browser, run this with `--serve`: it starts Lillignode with two
//! agents, prints `READY <address>`, and takes commands on its input:
//!
//!   key <letters>   press these keys in the agent in front (the stand-in
//!                   reports a status for each: w working, p needs you,
//!                   d done, i idle, u not understood)
//!   kill            stop Lillignode outright, as a crash would
//!   start           start Lillignode again, after kill; prints its new
//!                   address
//!   quit            quit Lillignode through its menu, and end
//!
//! The page's address holds a word that is new each start. The check reads
//! it from Lillignode's screen, as a person would.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[path = "owned/mod.rs"]
mod owned;
use owned::*;

/// A port nothing is listening on, on this machine.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// Where the page is: its port, and the word in its address. An empty word
/// asks without one.
struct Page {
    port: u16,
    word: String,
}

impl Page {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/", self.port, self.word)
    }
}

/// The page's address as Lillignode's screen shows it.
fn page_on_screen(s: &mut Session, port: u16) -> Page {
    let start = format!("http://127.0.0.1:{port}/");
    s.expect("the page being on is said, with its address", &start);
    let flat = s.flat();
    let after = &flat[flat.find(&start).unwrap() + start.len()..];
    let word: String = after.chars().take_while(char::is_ascii_alphanumeric).collect();
    check("and a word in it", word.len() == 16 && after[word.len()..].starts_with('/'), &flat);
    Page { port, word }
}

/// Send one request and read the whole answer. The word is put before the
/// path the request names, as the page's own requests have it.
fn ask(page: &Page, request: &str) -> Option<String> {
    let port = page.port;
    let request = match (page.word.as_str(), request.split_once(" /")) {
        ("", _) | (_, None) => request.to_string(),
        (word, Some((method, rest))) => format!("{method} /{word}/{rest}"),
    };
    let mut stream = TcpStream::connect_timeout(&address(port), Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream.write_all(request.as_bytes()).ok()?;
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    Some(answer)
}

fn get(page: &Page, path: &str) -> Option<String> {
    ask(page, &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", page.port))
}

fn state(page: &Page, query: &str) -> serde_json::Value {
    let answer = get(page, &format!("/state{query}")).unwrap_or_default();
    let body = answer.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    serde_json::from_str(body).unwrap_or_else(|_| {
        println!("  FAIL  the page's state is not readable\n        {answer}");
        std::process::exit(1);
    })
}

/// What the stream sent, one message at a time: its name and its data.
/// `None` when the stream ended.
fn listen(page: &Page, query: &str) -> Receiver<Option<(String, String)>> {
    let (tx, rx) = mpsc::channel();
    let (port, word) = (page.port, &page.word);
    let mut stream = TcpStream::connect_timeout(&address(port), Duration::from_secs(2)).expect("the page");
    stream
        .write_all(format!("GET /{word}/stream{query} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .unwrap();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stream).lines();
        let mut name = String::new();
        while let Some(Ok(line)) = lines.next() {
            if let Some(event) = line.strip_prefix("event: ") {
                name = event.to_string();
            } else if let Some(data) = line.strip_prefix("data: ") {
                if tx.send(Some((std::mem::take(&mut name), data.to_string()))).is_err() {
                    return;
                }
            }
        }
        let _ = tx.send(None);
    });
    rx
}

/// Wait for a state on the stream that `wanted` accepts. Returns how long
/// it took, or None if none came in `limit`.
fn heard(
    stream: &Receiver<Option<(String, String)>>,
    limit: Duration,
    wanted: impl Fn(&serde_json::Value) -> bool,
) -> Option<Duration> {
    let began = Instant::now();
    while began.elapsed() < limit {
        let Ok(Some((name, data))) = stream.recv_timeout(limit.saturating_sub(began.elapsed())) else {
            return None;
        };
        if name != "state" {
            continue;
        }
        let message: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
        if message.get("state").is_some_and(&wanted) {
            return Some(began.elapsed());
        }
    }
    None
}

/// True once the stream has ended, by Lillignode's word or by the
/// connection closing, within `limit`.
fn ended(stream: &Receiver<Option<(String, String)>>, limit: Duration) -> bool {
    let began = Instant::now();
    while began.elapsed() < limit {
        match stream.recv_timeout(limit.saturating_sub(began.elapsed())) {
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return true,
            Ok(Some((name, _))) if name == "ended" => return true,
            Ok(Some(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return false,
        }
    }
    false
}

fn settings(sb: &Sandbox, text: &str) {
    std::fs::write(sb.state.join("config.json"), text).unwrap();
}

/// Lillignode with the page on, a reporting stand-in in front and a shell
/// behind it.
fn started(sb: &Sandbox, port: u16) -> (Session, Page) {
    settings(sb, &format!("{{\"page\": true, \"page_port\": {port}}}"));
    let mut s = Session::start(sb);
    let page = page_on_screen(&mut s, port);
    s.expect("and who can read it", "Anyone with this address can see your agents.");
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    s.new_agent(shell);
    s.expect("a shell starts", &format!("Started {shell} in"));
    s.new_agent("reporter");
    s.expect("a reporting stand-in starts", "STANDIN READY status");
    (s, page)
}

fn seat<'a>(state: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    state["seats"].as_array().and_then(|seats| seats.iter().find(|s| s["name"] == name)).unwrap_or_else(
        || {
            println!("  FAIL  the page lists an agent named {name}\n        {state}");
            std::process::exit(1);
        },
    )
}

fn serve(sb: &Sandbox, port: u16) {
    let (mut s, page) = started(sb, port);
    println!("READY {}", page.url());
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        s.pump(Duration::from_millis(50));
        let Ok(line) = rx.try_recv() else { continue };
        match line.trim().split_once(' ').unwrap_or((line.trim(), "")) {
            ("key", letters) => {
                s.keys(letters.as_bytes());
                println!("DONE key {letters}");
            }
            ("kill", _) => {
                kill(&mut s);
                println!("DONE kill");
            }
            ("start", _) => {
                s = Session::start(sb);
                let page = page_on_screen(&mut s, port);
                println!("DONE start {}", page.url());
            }
            ("quit", _) => {
                if s.exit.is_none() {
                    drop(s.quit("quit", true));
                }
                println!("DONE quit");
                return;
            }
            _ => println!("UNKNOWN {line}"),
        }
    }
}

fn main() {
    let (sb, standin) = sandbox("page-check");
    launcher(&sb.launchers, &standin, "reporter", "status");
    let port = free_port();
    if std::env::args().any(|a| a == "--serve") {
        serve(&sb, port);
        let _ = std::fs::remove_dir_all(&sb.root);
        return;
    }
    println!("lillignode: {}\nthrowaway folder: {}\n", sb.exe.display(), sb.root.display());

    println!("A. Off unless asked for");
    let mut s = Session::start(&sb);
    s.expect("Lillignode starts", "No agents yet.");
    let nowhere = Page { port, word: String::new() };
    check(
        "nothing listens when the page was not asked for",
        get(&nowhere, "/").is_none(),
        "something answered",
    );
    drop(s.quit("quit", false));

    println!("B. On: what the page is told");
    let (mut s, page) = started(&sb, port);
    let html = get(&page, "/").unwrap_or_default();
    check("the page is served", html.starts_with("HTTP/1.1 200") && html.contains("Blocking now"), &html);
    check("with a column that says the board is not connected", html.contains("Not connected yet."), &html);
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report says the address again, whole", &format!("The page: {}", page.url()));
    s.keys(ESC);
    let first = state(&page, "");
    let reporter = seat(&first["state"], "reporter");
    let shell = seat(&first["state"], if cfg!(windows) { "cmd" } else { "sh" });
    check("an agent that reported is shown as it said", reporter["words"] == "Idle", &reporter.to_string());
    check("with when it last reported", reporter["reported_at"].is_u64(), &reporter.to_string());
    check("and which agent is in front", reporter["front"] == true, &reporter.to_string());
    check(
        "an agent that reports nothing is Unknown, with why",
        shell["words"] == "Unknown: no report",
        &shell.to_string(),
    );
    check("and no time, because nothing was reported", shell["reported_at"].is_null(), &shell.to_string());
    let key = reporter["key"].as_u64().unwrap();

    println!("C. A forged status reaches the page within a second");
    let stream = listen(&page, &format!("?after=0&seat={key}"));
    let is = |name: &'static str, slug: &'static str| {
        move |state: &serde_json::Value| seat(state, name)["state"] == slug
    };
    check(
        "the stream begins with the state",
        heard(&stream, Duration::from_secs(3), is("reporter", "idle")).is_some(),
        "",
    );
    let second = Duration::from_secs(1);
    // The key is the last thing sent before the clock starts.
    s.writer.write_all(b"w").unwrap();
    s.writer.flush().unwrap();
    let took = heard(&stream, second, is("reporter", "working"));
    check("Working, within a second", took.is_some(), "not within a second");
    println!("        ({} ms)", took.unwrap().as_millis());
    s.pump_for(Duration::from_millis(300));

    s.writer.write_all(b"p").unwrap();
    s.writer.flush().unwrap();
    let took = heard(&stream, second, |state| {
        let r = seat(state, "reporter");
        let said = state["events"].as_array().is_some_and(|events| {
            events.iter().any(|e| e["what"] == "began-needing-you" && e["agent"] == r["key"])
        });
        r["needs_you"] == true && r["needs"] == "permission" && r["needs_you_since"].is_u64() && said
    });
    check(
        "Needs you, what for, since when, and the event that names it, within a second",
        took.is_some(),
        "",
    );
    println!("        ({} ms)", took.unwrap().as_millis());
    s.expect("the terminal says the same", "NEEDS YOU");
    let waiting = state(&page, &format!("?seat={key}"));
    let since = seat(&waiting["state"], "reporter")["needs_you_since"].as_u64().unwrap();
    check("the wait is counted from then", waiting["at"].as_u64().unwrap() >= since, &waiting.to_string());
    let screen = waiting["state"]["screen"]["lines"].to_string();
    check("the agent's screen is there as text", screen.contains("SENT p"), &screen);

    s.writer.write_all(b"d").unwrap();
    s.writer.flush().unwrap();
    let took = heard(&stream, second, |state| {
        let r = seat(state, "reporter");
        r["state"] == "done" && r["needs_you"] == false && r["needs_you_since"].is_null()
    });
    check("Done, and it no longer needs you, within a second", took.is_some(), "");
    s.pump_for(Duration::from_millis(300));
    s.writer.write_all(b"u").unwrap();
    s.writer.flush().unwrap();
    let took = heard(&stream, second, |state| seat(state, "reporter")["words"] == "Unknown: not understood");
    check("a status that is not understood is Unknown, with that reason", took.is_some(), "");
    s.pump_for(Duration::from_millis(300));

    println!("D. Read only, for this machine's own address only, and only with the word");
    let without = |path: &str| get(&nowhere, path).unwrap_or_default();
    for path in ["/", "/state", "/stream", &format!("/{}/state", &page.word[..15])] {
        let answer = without(path);
        check(
            &format!("without the word, {path} gets nothing"),
            answer.starts_with("HTTP/1.1 404") && !answer.contains("seats") && !answer.contains("<html"),
            &answer,
        );
    }
    let refused = |request: String, status: &str| ask(&page, &request).is_some_and(|a| a.starts_with(status));
    let host = format!("Host: 127.0.0.1:{port}\r\n");
    check("it takes nothing", refused(format!("POST /state HTTP/1.1\r\n{host}\r\n"), "HTTP/1.1 405"), "");
    check(
        "a name pointed at this machine from elsewhere is refused",
        refused(format!("GET /state HTTP/1.1\r\nHost: elsewhere.example:{port}\r\n\r\n"), "HTTP/1.1 403"),
        "",
    );
    check(
        "another site's page is refused",
        refused(
            format!("GET /state HTTP/1.1\r\n{host}Origin: https://elsewhere.example\r\n\r\n"),
            "HTTP/1.1 403",
        ),
        "",
    );
    s.expect("Lillignode is as it was", "reporter");

    println!("E. Lillignode stopped outright");
    kill(&mut s);
    check("it has stopped", s.exit.is_some(), "still running");
    check("the stream ends within a second, so the page knows", ended(&stream, second), "the stream went on");
    check("and nothing answers at the address", get(&page, "/state").is_none(), "something answered");

    println!("F. Started again, then quit in the ordinary way");
    // The workspace left by the run that was stopped comes back.
    let mut s = Session::start(&sb);
    let again = page_on_screen(&mut s, port);
    check("the page has a new word", again.word != page.word, &again.word);
    let old = get(&page, "/state").unwrap_or_default();
    check("the old address gets nothing", old.starts_with("HTTP/1.1 404") && !old.contains("seats"), &old);
    let stream = listen(&again, "?after=0");
    check("the stream begins", heard(&stream, Duration::from_secs(3), |_| true).is_some(), "");
    drop(s.quit("quit", true));
    check("the stream ends", ended(&stream, Duration::from_secs(2)), "the stream went on");

    let _ = std::fs::remove_dir_all(&sb.root);
    println!("\nAll checks passed.");
}
