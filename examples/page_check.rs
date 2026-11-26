//! Development tool, not part of the product.
//!
//! Checks the page against the real `lillignode` program, in a
//! terminal the check owns, with stand-in agents. No window, no browser,
//! no real agent, no real settings (see `owned/mod.rs`).
//!
//!   cargo build --release --all-targets && cargo run --release --example page_check
//!
//! It asks the page what a browser would ask (`/`, `/state`, `/stream`) and
//! checks the answers: a status line a stand-in forges reaches the page
//! within a second; an agent that needs you is said to, with since when;
//! a request that is not a plain read from this machine's own address is
//! refused; when Lillignode is stopped outright the stream ends; started
//! again, the page is at the same address, until the menu's `w` gives it a
//! new one. Then, with typing switched on in the settings: what the page's
//! Send would send reaches the agent it names and no other, asks first
//! where a paste would, and is refused for anyone but the page itself.
//!
//! What the page then draws is a browser's business. For a check that
//! drives a browser, run this with `--serve`: it starts Lillignode with two
//! agents, prints `READY <address>`, and takes commands on its input:
//!
//!   key <letters>   press these keys in the agent in front (the stand-in
//!                   reports a status for each: w working, p needs you,
//!                   d done, i idle, u not understood)
//!   kill            stop Lillignode outright, as a crash would
//!   start           start Lillignode again, after kill; prints its
//!                   address
//!   word            the menu's `w`, then y: a new address; prints it
//!   front           bring the next agent to the front
//!   end             end the program of the agent in front (Ctrl+D)
//!   board <which>   put a Board file in the state folder: `standin` (the
//!                   eight cards in board-standin.json), `broken`, or
//!                   `none` to take it away; `board close <id>` marks one
//!                   card of the file there as no longer open
//!   outbox          prints `DONE outbox` and the outbox's lines, joined
//!                   by ` | `
//!   pickup          empties the outbox, as whatever keeps the Board would
//!   shows <text>    whether Lillignode's screen shows this text within
//!                   three seconds: prints `DONE shows yes` or `no`
//!   quit            quit Lillignode through its menu, and end
//!
//! With `--serve --type` the settings also let the page type, and a third
//! agent, `plain`, is started first: a stand-in that says what it was sent
//! and does not take a paste as one block. With `--serve --act` the
//! settings let the page act on the Board's cards; with both, both.
//!
//! The page's address holds a word. The check reads it from Lillignode's
//! screen, as a person would.

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

/// The headers the page's own Send carries.
fn own(port: u16) -> String {
    format!("Origin: http://127.0.0.1:{port}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n")
}

/// A `POST` to the page's `send`, with these headers and this body: the
/// whole answer.
fn post(page: &Page, headers: &str, body: &str) -> String {
    let (port, length) = (page.port, body.len());
    ask(
        page,
        &format!("POST /send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{headers}Content-Length: {length}\r\n\r\n{body}"),
    )
    .unwrap_or_default()
}

/// What the page needs to send: its token and the run, from its data.
struct Typing {
    token: String,
    run: String,
}

/// Send as the page's own button would. Returns what came of it and the
/// words said back.
fn send(page: &Page, typing: &Typing, agent: u64, text: &str, agreed: bool) -> (String, String) {
    let body = serde_json::json!({
        "token": typing.token, "run": typing.run, "agent": agent, "text": text, "agreed": agreed,
    });
    let answer = post(page, &own(page.port), &body.to_string());
    let said: serde_json::Value =
        serde_json::from_str(answer.split_once("\r\n\r\n").map_or("", |(_, body)| body)).unwrap_or_default();
    match (said["outcome"].as_str(), said["words"].as_str()) {
        (Some(outcome), Some(words)) => (outcome.to_string(), words.to_string()),
        _ => {
            println!("  FAIL  the page says what became of a send\n        {answer}");
            std::process::exit(1);
        }
    }
}

/// An agent's screen as the page has it, as text.
fn screen_of(page: &Page, key: u64) -> String {
    state(page, &format!("?agent={key}"))["state"]["screen"]["lines"].to_string()
}

/// Wait until the page's copy of an agent's screen holds `text`.
fn screen_shows(page: &Page, key: u64, text: &str) -> bool {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(3) {
        if screen_of(page, key).contains(text) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
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

fn agent<'a>(state: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    state["agents"].as_array().and_then(|agents| agents.iter().find(|s| s["name"] == name)).unwrap_or_else(
        || {
            println!("  FAIL  the page lists an agent named {name}\n        {state}");
            std::process::exit(1);
        },
    )
}

fn serve(sb: &Sandbox, port: u16, can_type: bool, can_act: bool) {
    // With nothing at all: the page on, no agent, no Board.
    let empty = std::env::args().any(|a| a == "--empty");
    let (mut s, page) = if empty {
        settings(sb, &format!("{{\"page\": true, \"page_port\": {port}}}"));
        let mut s = Session::start(sb);
        let page = page_on_screen(&mut s, port);
        s.expect("no agents yet", "No agents yet.");
        (s, page)
    } else if can_act {
        let and_type = if can_type { ", \"page_can_type\": true" } else { "" };
        // A Board adapter, for a check that brings its own: the settings'
        // `board_adapter` as JSON, from the environment.
        let adapter = std::env::var("PAGE_CHECK_BOARD_ADAPTER")
            .map(|named| format!(", \"board_adapter\": {named}"))
            .unwrap_or_default();
        settings(
            sb,
            &format!("{{\"page\": true, \"page_port\": {port}, \"page_can_act\": true{and_type}{adapter}}}"),
        );
        let mut s = Session::start(sb);
        let page = page_on_screen(&mut s, port);
        s.new_agent("reporter");
        s.expect("a reporting stand-in starts", "STANDIN READY status");
        (s, page)
    } else if can_type {
        settings(sb, &format!("{{\"page\": true, \"page_port\": {port}, \"page_can_type\": true}}"));
        let mut s = Session::start(sb);
        let page = page_on_screen(&mut s, port);
        s.new_agent("plain");
        s.expect("a stand-in that says what it is sent starts", "STANDIN READY echo-plain");
        s.new_agent("reporter");
        s.expect("a reporting stand-in starts", "STANDIN READY status");
        (s, page)
    } else {
        started(sb, port)
    };
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
            ("word", _) => {
                s.raw(CTRL_Q);
                s.keys(b"w");
                s.expect("w asks first", "Give the page a new address? y / n");
                s.keys(b"y");
                s.expect("w gives the page a new address", "New page address:");
                let page = page_on_screen(&mut s, port);
                println!("DONE word {}", page.url());
            }
            ("front", _) => {
                s.raw(CTRL_Q);
                s.keys(b"\t");
                s.keys(ESC);
                println!("DONE front");
            }
            ("end", _) => {
                s.raw(&[4]);
                println!("DONE end");
            }
            ("outbox", _) => {
                let text =
                    std::fs::read_to_string(sb.state.join(lillignode::board::OUTBOX)).unwrap_or_default();
                println!("DONE outbox {}", text.lines().collect::<Vec<_>>().join(" | "));
            }
            ("pickup", _) => {
                std::fs::write(sb.state.join(lillignode::board::OUTBOX), "").unwrap();
                println!("DONE pickup");
            }
            ("board", which) if which.trim().starts_with("close ") => {
                close_card(sb, which.trim().trim_start_matches("close ").trim());
                println!("DONE board");
            }
            ("board", which) => {
                let path = sb.state.join(lillignode::board::FILE);
                match which.trim() {
                    "standin" => std::fs::write(&path, include_str!("board-standin.json")).unwrap(),
                    "broken" => std::fs::write(&path, "{ \"cards\": [ {").unwrap(),
                    _ => {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                println!("DONE board");
            }
            ("shows", text) => {
                let began = Instant::now();
                while !s.flat().contains(text) && began.elapsed() < Duration::from_secs(3) {
                    s.pump(Duration::from_millis(50));
                }
                println!("DONE shows {}", if s.flat().contains(text) { "yes" } else { "no" });
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

/// Mark one card of the Board's file as no longer open, as whatever keeps
/// the Board would once it has acted on what was sent.
fn close_card(sb: &Sandbox, id: &str) {
    let path = sb.state.join(lillignode::board::FILE);
    let mut file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for card in file["cards"].as_array_mut().unwrap() {
        if card["id"] == id {
            card["status"] = "approved".into();
        }
    }
    std::fs::write(&path, file.to_string()).unwrap();
}

/// The Board as the page is given it.
fn board_shown(page: &Page) -> serde_json::Value {
    let answer = get(page, "/board").unwrap_or_default();
    let body = answer.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    serde_json::from_str::<serde_json::Value>(body).unwrap_or_default()["board"].clone()
}

/// One card of the Board as the page is given it, wherever it is.
fn card_on(board: &serde_json::Value, id: &str) -> serde_json::Value {
    ["decide", "yours", "know"]
        .iter()
        .filter_map(|part| board[part].as_array())
        .flatten()
        .find(|card| card["id"] == id)
        .cloned()
        .unwrap_or_default()
}

/// Act on a card as the page's own buttons would. Returns what came of it
/// and the words said back.
fn act(page: &Page, token: &str, card: &str, action: &str, note: &str, undo: bool) -> (String, String) {
    // With what the page shows of the card now, as its buttons send.
    let shown = card_on(&board_shown(page), card);
    act_having_seen(page, token, card, action, note, undo, &shown)
}

/// Act on a card having been shown `shown` of it.
fn act_having_seen(
    page: &Page,
    token: &str,
    card: &str,
    action: &str,
    note: &str,
    undo: bool,
    shown: &serde_json::Value,
) -> (String, String) {
    let body = serde_json::json!({
        "token": token, "card": card, "action": action, "note": note, "undo": undo,
        "title": shown["title"].as_str().unwrap_or_default(), "recommendation": shown["recommendation"],
    });
    let port = page.port;
    let answer = ask(
        page,
        &format!(
            "POST /act HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Content-Length: {}\r\n\r\n{body}",
            own(port),
            body.to_string().len()
        ),
    )
    .unwrap_or_default();
    let said: serde_json::Value =
        serde_json::from_str(answer.split_once("\r\n\r\n").map_or("", |(_, body)| body)).unwrap_or_default();
    match (said["outcome"].as_str(), said["words"].as_str()) {
        (Some(outcome), Some(words)) => (outcome.to_string(), words.to_string()),
        _ => (answer.lines().next().unwrap_or("").to_string(), answer),
    }
}

fn main() {
    let (sb, standin) = sandbox("page-check");
    launcher(&sb.launchers, &standin, "reporter", "status");
    launcher(&sb.launchers, &standin, "plain", "echo-plain");
    launcher(&sb.launchers, &standin, "whole", "echo");
    let port = free_port();
    if std::env::args().any(|a| a == "--serve") {
        let has = |flag: &str| std::env::args().any(|a| a == flag);
        serve(&sb, port, has("--type"), has("--act"));
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
    check("with a column for the Board", html.contains("<h2 id=\"board-title\">Board</h2>"), &html);
    let board = get(&page, "/board").unwrap_or_default();
    check(
        "with no Board file, the Board is empty, and nothing is wrong",
        board.contains("\"empty\":true") && board.contains("\"problem\":null"),
        &board,
    );
    std::fs::write(sb.state.join("board.json"), include_str!("board-standin.json")).unwrap();
    let board = get(&page, "/board").unwrap_or_default();
    let body: serde_json::Value =
        serde_json::from_str(board.split_once("\r\n\r\n").map_or("", |(_, b)| b)).unwrap_or_default();
    let count = |part: &str| body["board"][part].as_array().map_or(0, Vec::len);
    check(
        "with the stand-in file: four to decide, two only you can do, two good to know",
        (count("decide"), count("yours"), count("know")) == (4, 2, 2),
        &board,
    );
    check(
        "and nothing the page does not show leaves",
        !board.contains("\"risk\"") && !board.contains("\"silo\""),
        &board,
    );
    let _ = std::fs::remove_file(sb.state.join("board.json"));
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report says the address again, whole", &format!("The page: {}", page.url()));
    s.keys(ESC);
    let first = state(&page, "");
    let reporter = agent(&first["state"], "reporter");
    let shell = agent(&first["state"], if cfg!(windows) { "cmd" } else { "sh" });
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
    let stream = listen(&page, &format!("?after=0&agent={key}"));
    let is = |name: &'static str, slug: &'static str| {
        move |state: &serde_json::Value| agent(state, name)["state"] == slug
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
        let r = agent(state, "reporter");
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
    let waiting = state(&page, &format!("?agent={key}"));
    let since = agent(&waiting["state"], "reporter")["needs_you_since"].as_u64().unwrap();
    check("the wait is counted from then", waiting["at"].as_u64().unwrap() >= since, &waiting.to_string());
    let screen = waiting["state"]["screen"]["lines"].to_string();
    check("the agent's screen is there as text", screen.contains("SENT p"), &screen);

    s.writer.write_all(b"d").unwrap();
    s.writer.flush().unwrap();
    let took = heard(&stream, second, |state| {
        let r = agent(state, "reporter");
        r["state"] == "done" && r["needs_you"] == false && r["needs_you_since"].is_null()
    });
    check("Done, and it no longer needs you, within a second", took.is_some(), "");
    s.pump_for(Duration::from_millis(300));
    s.writer.write_all(b"u").unwrap();
    s.writer.flush().unwrap();
    let took = heard(&stream, second, |state| agent(state, "reporter")["words"] == "Unknown: not understood");
    check("a status that is not understood is Unknown, with that reason", took.is_some(), "");
    s.pump_for(Duration::from_millis(300));

    println!("D. Read only, for this machine's own address only, and only with the word");
    let without = |path: &str| get(&nowhere, path).unwrap_or_default();
    for path in ["/", "/state", "/stream", &format!("/{}/state", &page.word[..15])] {
        let answer = without(path);
        check(
            &format!("without the word, {path} gets nothing"),
            answer.starts_with("HTTP/1.1 404") && !answer.contains("agents") && !answer.contains("<html"),
            &answer,
        );
    }
    let refused = |request: String, status: &str| ask(&page, &request).is_some_and(|a| a.starts_with(status));
    let host = format!("Host: 127.0.0.1:{port}\r\n");
    check("it takes nothing", refused(format!("POST /state HTTP/1.1\r\n{host}\r\n"), "HTTP/1.1 405"), "");
    check(
        "it is given no token to send with",
        first["state"]["typing"].is_null(),
        &first["state"].to_string(),
    );
    let as_the_page_would = serde_json::json!({
        "token": "", "run": first["state"]["run"], "agent": key, "text": "w", "agreed": true,
    });
    let answer = post(&page, &own(port), &as_the_page_would.to_string());
    check(
        "a send made just as the page would make one is refused",
        answer.starts_with("HTTP/1.1 405"),
        &answer,
    );
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

    println!("F. Started again: the same address, until a new one is asked for");
    // The workspace left by the run that was stopped comes back.
    let mut s = Session::start(&sb);
    let again = page_on_screen(&mut s, port);
    check("the page is at the address it had", again.word == page.word, &again.word);
    let back = state(&page, "");
    check(
        "the address answers again, and says it is another run",
        back["state"]["run"].is_string() && back["state"]["run"] != first["state"]["run"],
        &back["state"]["run"].to_string(),
    );
    let word_file = sb.state.join("page-word");
    let kept = std::fs::read_to_string(&word_file).unwrap_or_default();
    check(
        "the word is kept beside the workspace",
        kept.trim() == page.word,
        &word_file.display().to_string(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&word_file).unwrap().permissions().mode() & 0o777;
        check("in a file only this user can read", mode == 0o600, &format!("{mode:o}"));
    }
    let stream = listen(&again, "?after=0");
    check("the stream begins", heard(&stream, Duration::from_secs(3), |_| true).is_some(), "");
    s.raw(CTRL_Q);
    s.keys(b"w");
    s.expect("w in the menu asks first", "Give the page a new address? y / n");
    s.keys(b"n");
    s.expect("no leaves it as it was", "The page's address was left as it was.");
    check("and the address still answers", state(&page, "")["state"]["agents"].is_array(), "");
    s.raw(CTRL_Q);
    s.keys(b"w");
    s.expect("asked again", "Give the page a new address? y / n");
    s.keys(b"y");
    s.expect("yes makes a new address and says so", "New page address:");
    s.expect("and what became of the old one", "The old one has stopped working, for open pages too.");
    let new = page_on_screen(&mut s, port);
    check("the word is another", new.word != page.word, &new.word);
    let old = get(&page, "/state").unwrap_or_default();
    check("the old address gets nothing", old.starts_with("HTTP/1.1 404") && !old.contains("agents"), &old);
    check("a page open on the old address is let go", ended(&stream, Duration::from_secs(3)), "it went on");
    check("the new address has it all", state(&new, "")["state"]["agents"].is_array(), "");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report has the new address", &format!("The page: {}", new.url()));
    s.keys(ESC);
    drop(s.quit("quit", true));
    let mut s = Session::start(&sb);
    let third = page_on_screen(&mut s, port);
    check("the new address is the one the next start uses", third.word == new.word, &third.word);
    drop(s.quit("quit", true));

    println!("G. Typing from the page, when the settings allow it");
    settings(&sb, &format!("{{\"page\": true, \"page_port\": {port}, \"page_can_type\": true}}"));
    let mut s = Session::start(&sb);
    let page = page_on_screen(&mut s, port);
    s.expect("the notice says the page can type", "can see your agents and type to them.");
    check("while it can type, its word is not the kept one", page.word != third.word, &page.word);
    let kept = std::fs::read_to_string(sb.state.join("page-word")).unwrap_or_default();
    check("and is kept nowhere: the file still holds the other", kept.trim() == third.word, kept.trim());
    let named = format!(
        "GET /state HTTP/1.1
Host: localhost:{port}

"
    );
    check(
        "and it answers only at the address Lillignode shows",
        ask(&page, &named).is_some_and(|a| a.starts_with("HTTP/1.1 403")),
        "",
    );
    s.new_agent("whole");
    s.expect("a stand-in that takes a paste whole starts", "STANDIN READY echo");
    s.new_agent("plain");
    s.expect("and one that does not, in front", "STANDIN READY echo-plain");
    let now = state(&page, "");
    let typing = Typing {
        token: now["state"]["typing"].as_str().unwrap_or_default().to_string(),
        run: now["state"]["run"].as_str().unwrap_or_default().to_string(),
    };
    check(
        "the page is given a token with its data",
        typing.token.len() >= 32,
        &now["state"]["typing"].to_string(),
    );
    let (whole, plain) = (agent(&now["state"], "whole"), agent(&now["state"], "plain"));
    let (whole_key, plain_key) = (whole["key"].as_u64().unwrap(), plain["key"].as_u64().unwrap());
    let (whole_label, plain_label) = (whole["label"].as_str().unwrap(), plain["label"].as_str().unwrap());
    check("the agent in front is the other one", plain["front"] == true && whole["front"] == false, "");

    let (outcome, words) = send(&page, &typing, whole_key, "for the one behind", false);
    check(
        "text for an agent that is not in front is sent to it, and said so",
        outcome == "sent" && words == format!("Sent to {whole_label}, then Enter."),
        &words,
    );
    check(
        "it arrives as one paste, then Enter",
        screen_shows(&page, whole_key, "PASTE 1 lines: for the one behind")
            && screen_shows(&page, whole_key, "ENTER"),
        &screen_of(&page, whole_key),
    );
    check(
        "nothing of it went to the agent in front",
        !screen_of(&page, plain_key).contains("behind") && !screen_of(&page, plain_key).contains("ENTER"),
        &screen_of(&page, plain_key),
    );
    s.expect(
        "the terminal says the page typed, and to whom",
        &format!("The page typed to {whole_label}, then Enter."),
    );
    let after = state(&page, "");
    let said = after["state"]["events"].as_array().is_some_and(|events| {
        events.iter().any(|e| e["what"] == "typed-from-page" && e["agent"] == whole_key)
    });
    check("the send is an event that names the agent", said, &after["state"]["events"].to_string());

    let (outcome, words) = send(&page, &typing, plain_key, "hello plain", false);
    check(
        "one line goes as typed",
        outcome == "sent" && words == format!("Sent to {plain_label}, then Enter."),
        &words,
    );
    s.expect("with Enter after it", "STATUS enters=1 pastes=0 typed=");

    let (outcome, words) = send(&page, &typing, plain_key, "one\ntwo", false);
    check(
        "more than one line asks first, in the terminal's words",
        outcome == "ask"
            && words
                == format!(
                    "Paste 2 lines into {plain_label} one line at a time? It does not take a paste as one block, \
                     so each line would be entered as if you pressed Enter after it."
                ),
        &words,
    );
    s.pump_for(Duration::from_millis(500));
    check(
        "and nothing was sent",
        !s.flat().contains("enters=2") && !s.flat().contains("typed=one"),
        &s.flat(),
    );
    check("nor is the terminal asked anything", !s.flat().contains("? y / n"), &s.flat());
    let (outcome, words) = send(&page, &typing, plain_key, "one\ntwo", true);
    check(
        "agreed to, it goes line by line",
        outcome == "sent" && words == format!("Sent to {plain_label} line by line, then Enter."),
        &words,
    );
    s.expect("each line entered", "STATUS enters=3 pastes=0 typed=");
    let (outcome, _) = send(&page, &typing, whole_key, "three\nlines\nwhole", false);
    check("an agent that takes a paste whole is not asked about", outcome == "sent", "");
    check(
        "and gets it as one",
        screen_shows(&page, whole_key, "PASTE 3 lines:"),
        &screen_of(&page, whole_key),
    );

    let body = |token: &str, agent: u64| {
        serde_json::json!({ "token": token, "run": typing.run, "agent": agent, "text": "intruder", "agreed": true })
            .to_string()
    };
    let good = body(&typing.token, plain_key);
    let json = "Content-Type: application/json\r\n";
    let refusals = [
        (
            "without the page's token",
            own(port),
            body("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", plain_key),
            "HTTP/1.1 403",
        ),
        ("that does not say where it comes from", json.to_string(), good.clone(), "HTTP/1.1 403"),
        (
            "from another web site",
            format!("Origin: https://elsewhere.example\r\nSec-Fetch-Site: cross-site\r\n{json}"),
            good.clone(),
            "HTTP/1.1 403",
        ),
        (
            "from a form",
            format!("Origin: http://127.0.0.1:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\n"),
            good.clone(),
            "HTTP/1.1 415",
        ),
    ];
    for (what, headers, body, status) in refusals {
        let answer = post(&page, &headers, &body);
        check(&format!("a send {what} is refused"), answer.starts_with(status), &answer);
    }
    let link = get(
        &page,
        &format!(
            "/send?token={}&run={}&agent={plain_key}&text=intruder&agreed=true",
            typing.token, typing.run
        ),
    );
    check("a plain link sends nothing", link.unwrap_or_default().starts_with("HTTP/1.1 404"), "");
    let nowhere = Page { port, word: String::new() };
    let answer = post(&nowhere, &own(port), &good);
    check("without the word, a send gets nothing", answer.starts_with("HTTP/1.1 404"), &answer);
    let (outcome, words) = send(&page, &typing, 999_999, "intruder", true);
    check(
        "an agent that is not there refuses, in the terminal's words",
        outcome == "refused" && words == "That agent is no longer in your workspace. Nothing was changed.",
        &words,
    );
    let other_run = Typing { token: typing.token.clone(), run: "1".into() };
    let (outcome, words) = send(&page, &other_run, plain_key, "intruder", true);
    check(
        "a key from another run of Lillignode refuses",
        outcome == "refused" && words.contains("Nothing was sent."),
        &words,
    );
    s.pump_for(Duration::from_millis(500));
    check(
        "none of that reached an agent",
        !s.flat().contains("intruder") && !s.flat().contains("enters=4"),
        &s.flat(),
    );
    check("either of them", !screen_of(&page, whole_key).contains("intruder"), &screen_of(&page, whole_key));

    // Enter alone, from its own button, to the agent behind.
    let body = serde_json::json!({
        "token": typing.token, "run": typing.run, "agent": whole_key, "text": "", "agreed": false, "enter": true,
    });
    let answer = post(&page, &own(port), &body.to_string());
    check(
        "Enter alone reaches the agent named, and is said so",
        answer.contains(&format!("\"words\":\"Pressed Enter in {whole_label}.\"")),
        &answer,
    );
    check(
        "as Enter and nothing else",
        screen_shows(&page, whole_key, "enters=3 pastes=2 typed="),
        &screen_of(&page, whole_key),
    );
    s.expect(
        "the terminal says the page pressed Enter, and where",
        &format!("The page pressed Enter in {whole_label}."),
    );
    let after = state(&page, "");
    let said = after["state"]["events"].as_array().is_some_and(|events| {
        events.iter().any(|e| e["what"] == "enter-from-page" && e["agent"] == whole_key)
    });
    check("the press is an event that names the agent", said, &after["state"]["events"].to_string());
    check("nothing of it reached the agent in front", !s.flat().contains("enters=4"), &s.flat());

    // The agent in front ends: Ctrl+D ends the stand-in.
    s.raw(&[4]);
    s.expect("the agent in front has ended", "exited.");
    let (outcome, words) = send(&page, &typing, plain_key, "anyone there", false);
    check(
        "a stopped agent refuses, in the terminal's words",
        outcome == "refused" && words == format!("{plain_label} is not running, so nothing was sent to it."),
        &words,
    );
    drop(s.quit("quit", true));

    println!("H. Acting on a card of the Board, when the settings allow it");
    let board_file = sb.state.join(lillignode::board::FILE);
    let outbox_file = sb.state.join(lillignode::board::OUTBOX);
    let outbox = || -> Vec<serde_json::Value> {
        std::fs::read_to_string(&outbox_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    std::fs::write(&board_file, include_str!("board-standin.json")).unwrap();
    let as_written = std::fs::read(&board_file).unwrap();
    // Typing is on from the part before, acting is not: the page is told
    // so, and acting as the page would is nothing.
    settings(&sb, &format!("{{\"page\": true, \"page_port\": {port}, \"page_can_type\": true}}"));
    let mut s = Session::start(&sb);
    let page = page_on_screen(&mut s, port);
    let now = state(&page, "");
    check("a page that can only type is given nothing to act with", now["state"]["acting"].is_null(), "");
    let token = now["state"]["typing"].as_str().unwrap_or_default().to_string();
    let (status, _) = act(&page, &token, "c1", "approve", "", false);
    check(
        "and an action made just as the page would make one gets nothing",
        status.starts_with("HTTP/1.1 404"),
        &status,
    );
    check("nothing was written", !outbox_file.exists(), "");
    drop(s.quit("quit", true));

    settings(&sb, &format!("{{\"page\": true, \"page_port\": {port}, \"page_can_act\": true}}"));
    let mut s = Session::start(&sb);
    let page = page_on_screen(&mut s, port);
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("Lillignode says the page can act", "They can also act on the cards of your Board from it");
    s.keys(ESC);
    let now = state(&page, "");
    let token = now["state"]["acting"].as_str().unwrap_or_default().to_string();
    check("the page is given a token to act with", token.len() >= 32, &now["state"]["acting"].to_string());
    check("and cannot type", now["state"]["typing"].is_null(), "");
    let kept = std::fs::read_to_string(sb.state.join("page-word")).unwrap_or_default();
    check("while it can act, its word is new and kept nowhere", kept.trim() != page.word, kept.trim());

    let (outcome, words) = act(&page, &token, "c1", "approve", "", false);
    check(
        "Approve on a card to decide is sent, and said so",
        outcome == "sent" && words == "Sent: Approve. It waits until the Board no longer lists the card.",
        &words,
    );
    let lines = outbox();
    check(
        "one line in the outbox: card id, action, note, time, and the title that was shown",
        lines.len() == 1
            && lines[0]["card"] == "c1"
            && lines[0]["title"] == "Keep the old sign-in endpoint for one more release?"
            && lines[0]["action"] == "approve"
            && lines[0]["note"] == ""
            && lines[0]["at"].as_str().is_some_and(|at| lillignode::board::parse_time(at).is_some()),
        &format!("{lines:?}"),
    );
    check("the Board's own file is not written", std::fs::read(&board_file).unwrap() == as_written, "");
    check(
        "the card shows as sent and waiting",
        card_on(&board_shown(&page), "c1")["sent"]["action"] == "approve",
        "",
    );
    s.expect(
        "Lillignode's window says what was done from the page",
        "From the page's Board: Approve, for \"Keep the old sign-in endpoint",
    );
    let said = state(&page, "")["state"]["events"]
        .as_array()
        .is_some_and(|events| events.iter().any(|e| e["what"] == "card-acted" && e["label"] == "c1"));
    check("the action is an event that names the card", said, "");
    let (outcome, words) = act(&page, &token, "c1", "hold", "", false);
    check(
        "a card that is waiting is not acted on again",
        outcome == "refused"
            && words == "Approve was already sent for that card, and it is waiting. Nothing more was sent.",
        &words,
    );

    let (outcome, words) = act(&page, &token, "c1", "approve", "", true);
    check(
        "Undo within ten seconds takes the line out",
        outcome == "sent" && words.starts_with("Undone: Approve."),
        &words,
    );
    check("the outbox is empty again", outbox().is_empty(), &format!("{:?}", outbox()));
    check("and the card no longer waits", card_on(&board_shown(&page), "c1")["sent"].is_null(), "");
    s.expect("Lillignode's window says it was undone", "Approve was undone for");

    // The fire is the Hub's to approve: the page is offered nothing for it,
    // and an action on it is refused.
    let fire = card_on(&board_shown(&page), "c2");
    check(
        "a fire is offered no action from the page",
        fire["hub_only"] == true && fire["actions"] == serde_json::json!([]),
        &fire.to_string(),
    );
    let (outcome, words) = act(&page, &token, "c2", "approve", "", false);
    check(
        "and an action on it is refused, and says where to go",
        outcome == "refused"
            && words == "This card is approved on the Hub, not from this page. Nothing was sent.",
        &words,
    );
    // A card on hold is offered Approve and Revise, and Hold is refused.
    let held = card_on(&board_shown(&page), "c4");
    check(
        "a card on hold is offered Approve and Revise, not Hold",
        held["actions"] == serde_json::json!(["approve", "revise"]),
        &held.to_string(),
    );
    let (outcome, words) = act(&page, &token, "c4", "hold", "", false);
    check(
        "and Hold on it is refused",
        outcome == "refused" && words == "That card is already on hold. Nothing was sent.",
        &words,
    );
    let (outcome, words) = act(&page, &token, "c4", "revise", "Find out why first.", false);
    check(
        "Revise carries its note",
        outcome == "sent" && outbox().last().is_some_and(|l| l["note"] == "Find out why first."),
        &words,
    );
    let (outcome, words) = act(&page, &token, "c3", "revise", "  ", false);
    check(
        "Revise without a note is refused, and says why",
        outcome == "refused" && words == "Revise needs a short note saying what to change. Nothing was sent.",
        &words,
    );
    let (outcome, _) = act(&page, &token, "c5", "done", "", false);
    check(
        "Done on something only you can do",
        outcome == "sent" && outbox().last().is_some_and(|l| l["action"] == "done"),
        "",
    );
    let (outcome, _) = act(&page, &token, "c7", "got-it", "", false);
    check(
        "Got it on something good to know",
        outcome == "sent" && outbox().last().is_some_and(|l| l["action"] == "got-it"),
        "",
    );
    check(
        "three lines wait in the outbox, in order",
        outbox().iter().map(|l| l["card"].as_str().unwrap_or("")).collect::<Vec<_>>() == ["c4", "c5", "c7"],
        &format!("{:?}", outbox()),
    );

    let before = outbox().len();
    for (what, card, action, expect) in [
        (
            "an action that does not fit the card",
            "c5",
            "approve",
            "That cannot be done to this card. Nothing was sent.",
        ),
        (
            "a card that is not on the Board",
            "c99",
            "approve",
            "That card is no longer open on the Board. Nothing was sent.",
        ),
        (
            "an action the Board does not have",
            "c3",
            "delete",
            "That cannot be done to this card. Nothing was sent.",
        ),
    ] {
        let (outcome, words) = act(&page, &token, card, action, "", false);
        check(&format!("{what} is refused, and says why"), outcome == "refused" && words == expect, &words);
    }
    let good =
        serde_json::json!({ "token": token, "card": "c3", "action": "approve", "note": "", "undo": false })
            .to_string();
    let json = "Content-Type: application/json\r\n";
    let post_act = |headers: &str, body: &str| {
        ask(
            &page,
            &format!(
                "POST /act HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .unwrap_or_default()
    };
    let wrong = good.replace(&token, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    for (what, headers, body, status) in [
        ("without the page's token", own(port), wrong, "HTTP/1.1 403"),
        ("that does not say where it comes from", json.to_string(), good.clone(), "HTTP/1.1 403"),
        (
            "from another web site",
            format!("Origin: https://elsewhere.example\r\nSec-Fetch-Site: cross-site\r\n{json}"),
            good.clone(),
            "HTTP/1.1 403",
        ),
        (
            "from a form",
            format!("Origin: http://127.0.0.1:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\n"),
            good.clone(),
            "HTTP/1.1 415",
        ),
    ] {
        let answer = post_act(&headers, &body);
        check(&format!("an action {what} is refused"), answer.starts_with(status), &answer);
    }
    let link = get(&page, &format!("/act?token={token}&card=c3&action=approve"));
    check("a plain link acts on nothing", link.unwrap_or_default().starts_with("HTTP/1.1 404"), "");
    let nowhere = Page { port, word: String::new() };
    let answer = ask(
        &nowhere,
        &format!(
            "POST /act HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Content-Length: {}\r\n\r\n{good}",
            own(port),
            good.len()
        ),
    )
    .unwrap_or_default();
    check("without the word, an action gets nothing", answer.starts_with("HTTP/1.1 404"), &answer);
    let typed = serde_json::json!({ "token": token, "run": now["state"]["run"], "agent": 1, "text": "x", "agreed": true }).to_string();
    let answer = ask(
        &page,
        &format!(
            "POST /send HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{}Content-Length: {}\r\n\r\n{typed}",
            own(port),
            typed.len()
        ),
    )
    .unwrap_or_default();
    check("a page that can only act cannot type", answer.starts_with("HTTP/1.1 404"), &answer);
    check("none of that added a line", outbox().len() == before, &format!("{:?}", outbox()));
    check("nor touched the Board's own file", std::fs::read(&board_file).unwrap() == as_written, "");

    // The card is rewritten under its id between being shown and the press.
    let shown = card_on(&board_shown(&page), "c3");
    let rewritten = include_str!("board-standin.json").replace(
        "Should the install guide name the download by its version?",
        "Publish the signing key in the install guide?",
    );
    std::fs::write(&board_file, &rewritten).unwrap();
    let (outcome, words) = act_having_seen(&page, &token, "c3", "approve", "", false, &shown);
    check(
        "a card that changed since the page showed it is not acted on, and says why",
        outcome == "refused"
            && words == "That card has changed since the page showed it. Read it again, then decide. Nothing was sent.",
        &words,
    );
    check("and nothing was added", outbox().len() == before, &format!("{:?}", outbox()));
    std::fs::write(&board_file, &as_written).unwrap();

    // Whatever keeps the Board picks the lines up, and then closes a card.
    std::fs::write(&outbox_file, "").unwrap();
    check(
        "picked up, a card still open still shows as waiting",
        card_on(&board_shown(&page), "c5")["sent"]["action"] == "done",
        "",
    );
    close_card(&sb, "c5");
    check(
        "no longer open in the Board's file, it is no longer shown",
        card_on(&board_shown(&page), "c5").is_null(),
        "",
    );
    let (outcome, words) = act(&page, &token, "c4", "revise", "", true);
    check(
        "what was picked up cannot be undone, and says why",
        outcome == "refused" && words == "It has already been picked up from the outbox, so it cannot be undone. Nothing was changed.",
        &words,
    );
    drop(s.quit("quit", true));

    println!("I. A Board adapter, when the settings name one");
    adapter_section(&sb, port);

    let _ = std::fs::remove_dir_all(&sb.root);
    println!("\nAll checks passed.");
}

/// The adapter's page, read until `until` holds of it or `secs` pass.
fn board_until(page: &Page, secs: u64, until: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let shown = board_shown(page);
        if until(&shown) || std::time::Instant::now() >= end {
            return shown;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn adapter_section(sb: &Sandbox, port: u16) {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let here = std::env::current_exe().unwrap();
    let adapter = here.parent().unwrap().join(format!("adapter_standin{ext}"));
    check("the stand-in adapter is built", adapter.is_file(), &adapter.display().to_string());
    let (cards, log) = (sb.root.join("adapter-board.json"), sb.root.join("adapter-log.jsonl"));
    std::fs::write(&cards, include_str!("board-standin.json")).unwrap();
    let outbox_file = sb.state.join(lillignode::board::OUTBOX);
    let _ = std::fs::remove_file(&outbox_file);
    let _ = std::fs::remove_file(sb.state.join(lillignode::board::SENT));
    let logged = || std::fs::read_to_string(&log).unwrap_or_default();
    let named = serde_json::json!({ "page": true, "page_port": port, "page_can_act": true,
        "board_adapter": { "command": adapter, "args": [cards, log] } });
    settings(sb, &named.to_string());
    // A variable Lillignode has and the settings do not name: it must not
    // reach the adapter.
    let mut s = Session::start_with(sb, &sb.state, &[("ADAPTER_STANDIN_PROBE", "1")]);
    let page = page_on_screen(&mut s, port);
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "the status report names the program that keeps the Board",
        "The Board comes from a program your",
    );
    s.keys(ESC);
    let shown = board_until(&page, 10, |b| !card_on(b, "c1").is_null());
    check("the cards come from the adapter", !card_on(&shown, "c1").is_null(), &shown.to_string());
    check(
        "named as it named itself, and a variable the settings do not name did not reach it",
        shown["adapter"]["name"] == "the stand-in Hub",
        &shown["adapter"].to_string(),
    );
    check("it said hello, which it was sent", logged().lines().next() == Some(r#"{"hello":1}"#), &logged());
    let reporting = shown["adapter"]["reporting"].as_array().cloned().unwrap_or_default();
    check(
        "the agents reporting through it are given to the page, each with its newest line and when",
        reporting.len() == 2
            && reporting[0]["agent"] == "ops-bot"
            && reporting[0]["line"] == "Deployed the docs site."
            && reporting[0]["at"].is_u64(),
        &format!("{reporting:?}"),
    );
    let token = state(&page, "")["state"]["acting"].as_str().unwrap_or_default().to_string();

    let (outcome, words) = act_having_seen(&page, &token, "c1", "approve", "", false, &card_on(&shown, "c1"));
    check(
        "an action is held for its ten seconds, and the reader is told so",
        outcome == "sent"
            && words == "Approve goes to the stand-in Hub in ten seconds, unless you undo it on the card.",
        &words,
    );
    std::thread::sleep(Duration::from_secs(2));
    check("inside the ten seconds nothing reaches the adapter", !logged().contains("\"action\""), &logged());
    let (outcome, words) = act(&page, &token, "c1", "approve", "", true);
    check("undone inside them", outcome == "sent" && words.starts_with("Undone: Approve."), &words);

    let (outcome, _) = act_having_seen(&page, &token, "c3", "approve", "", false, &card_on(&shown, "c3"));
    check("another action is held", outcome == "sent", &outcome);
    let (outcome, _) = act_having_seen(&page, &token, "c5", "done", "", false, &card_on(&shown, "c5"));
    check("and one the adapter will refuse", outcome == "sent", &outcome);
    let answered = board_until(&page, 16, |b| {
        let actions = b["adapter"]["actions"].as_array().cloned().unwrap_or_default();
        actions.iter().filter(|a| a["state"] == "taken" || a["state"] == "refused").count() == 2
    });
    let log_now = logged();
    check(
        "after its ten seconds an action reaches the adapter, whole, with an id",
        log_now.lines().any(|l| {
            l.contains(r#""card":"c3""#) && l.contains(r#""action":"approve""#) && l.contains(r#""id":"a"#)
        }),
        &log_now,
    );
    check("the one undone never does", !log_now.contains(r#""card":"c1""#), &log_now);
    let actions = answered["adapter"]["actions"].as_array().cloned().unwrap_or_default();
    let said = |card: &str| {
        actions.iter().find(|a| a["card"] == card).map(|a| a["words"].clone()).unwrap_or_default()
    };
    check(
        "what the adapter took is said",
        said("c3") == "Taken by the stand-in Hub.",
        &said("c3").to_string(),
    );
    check(
        "what it refused is said, with its reason",
        said("c5") == "Refused by the stand-in Hub: that card was closed at the service",
        &said("c5").to_string(),
    );
    check(
        "and the refused card can be acted on again",
        card_on(&answered, "c5")["sent"].is_null() && !card_on(&answered, "c3")["sent"].is_null(),
        &answered.to_string(),
    );
    check("nothing was written to the outbox file", !outbox_file.exists(), "");
    let (outcome, words) = act(&page, &token, "c3", "approve", "", true);
    check(
        "what went on to the adapter cannot be undone, and says why",
        outcome == "refused"
            && words.starts_with("There is nothing held for that card to undo: it has gone on"),
        &words,
    );
    drop(s.quit("quit", true));

    // An adapter that cannot be started: said where the Board would be;
    // nothing is acted on, and nothing goes to the outbox file instead.
    let missing = serde_json::json!({ "page": true, "page_port": port, "page_can_act": true,
        "board_adapter": { "command": "no-such-adapter-program", "args": [] } });
    settings(sb, &missing.to_string());
    let mut s = Session::start(sb);
    let page = page_on_screen(&mut s, port);
    let shown = board_shown(&page);
    let problem = shown["problem"].as_str().unwrap_or_default().to_string();
    check(
        "an adapter that cannot be started says why where the Board would be",
        problem.contains("could not be started") && problem.contains("Nothing can be acted on"),
        &problem,
    );
    let token = state(&page, "")["state"]["acting"].as_str().unwrap_or_default().to_string();
    let (outcome, _) = act(&page, &token, "c1", "approve", "", false);
    check("and an action is refused", outcome == "refused", &outcome);
    check("with nothing written to the outbox file", !outbox_file.exists(), "");
    drop(s.quit("quit", true));

    // One that says nothing: stopped after its ten seconds, and said.
    // Only what the settings name reaches it: the variable is given by name.
    let mut quiet = named.clone();
    quiet["board_adapter"]["env"] = serde_json::json!(["ADAPTER_STANDIN_SILENT"]);
    settings(sb, &quiet.to_string());
    let mut s = Session::start_with(sb, &sb.state, &[("ADAPTER_STANDIN_SILENT", "1")]);
    let page = page_on_screen(&mut s, port);
    let shown = board_until(&page, 14, |b| b["problem"].as_str().is_some_and(|p| p.contains("said nothing")));
    check(
        "an adapter that never says hello is taken as stopped after ten seconds, and said",
        shown["problem"].as_str().is_some_and(|p| p.contains("it said nothing in ten seconds")),
        &shown.to_string(),
    );
    drop(s.quit("quit", true));
    let _ = std::fs::remove_file(sb.state.join(lillignode::board::SENT));
    settings(sb, &format!("{{\"page\": true, \"page_port\": {port}}}"));
}
