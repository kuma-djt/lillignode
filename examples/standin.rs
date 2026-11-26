//! Development tool, not part of the product.
//!
//! A stand-in for an agent, so tests never run against a real one. It reads
//! its input the way Claude Code does (raw, with pastes arriving as one
//! marked block) and reports exactly what it received.
//!
//!   standin echo            accept pastes as one block; report input
//!   standin echo-plain      do not ask for pastes as one block
//!   standin stream <n>      print n numbered lines quickly, then echo
//!   standin fullscreen      switch to a full-screen mode with no history
//!   standin args ...        say the arguments it was given, each in [ ],
//!                           as `STANDIN ARGS [a][b c]`, then echo
//!   standin size            on each Enter, say what was typed and the size
//!                           the terminal says it is: `SIZE <typed> is <c> by <r>`
//!   standin claude ...      behave like Claude Code for conversation ids:
//!                             --session-id <id>   new conversation
//!                             --resume <id>       continue one, or fail
//!                           Conversations are files in $STANDIN_HOME.
//!                           Each thing said is also recorded, with token
//!                           counts, the way Claude Code is described as
//!                           recording a conversation, under
//!                           $CLAUDE_CONFIG_DIR (see `record_reply`). Saying
//!                           "garble" records a reply with no counts, and
//!                           "newmodel" one from a model with no price.
//!   standin status          report status the way Claude Code 2.1.325 does:
//!                           ask the status question, then report on keys
//!                           (see `status_mode`)
//!   standin status-silent   never ask, never report
//!   standin codex           report status the way Codex 0.169.0 does:
//!                           without asking, under its own name, on keys
//!                           (see `codex_mode`)
//!   standin reports         report status the way any program may: without
//!                           asking, under a name of its own or none, on keys
//!                           (see `reports_mode`)
//!   standin status-echo     ask the status question the way Claude Code
//!                           does, then take pastes and report input as
//!                           `echo` does
//!   standin play <tape>     replay a tape: what a real agent wrote, at the
//!                           pace it wrote it, waiting at each key a person
//!                           pressed until that key arrives (see `tape`)
//!
//! Ctrl+D ends it. What it prints:
//!   ENTER                    an Enter press that was not inside a paste
//!   PASTE <n> lines: <text>  a paste that arrived as one block
//!   STATUS enters=<a> pastes=<b> typed=<text>

use std::io::{Read, Write};
use std::path::PathBuf;

mod tape;

#[cfg(windows)]
mod console {
    use std::ffi::c_void;
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }
    const STDIN: u32 = -10i32 as u32;
    const STDOUT: u32 = -11i32 as u32;

    /// Raw input with terminal sequences passed through untouched, which is
    /// what a Node program such as Claude Code asks Windows for.
    pub fn raw() {
        // SAFETY: plain calls into the Windows console API with handles the
        // system gave us and a pointer to a local.
        unsafe {
            let input = GetStdHandle(STDIN);
            let mut mode = 0u32;
            if GetConsoleMode(input, &mut mode) != 0 {
                // Clear line input, echo, and Ctrl+C handling; ask for raw sequences.
                SetConsoleMode(input, (mode & !0x0007) | 0x0200);
            }
            let output = GetStdHandle(STDOUT);
            if GetConsoleMode(output, &mut mode) != 0 {
                SetConsoleMode(output, mode | 0x0004);
            }
        }
    }
}

#[cfg(windows)]
mod console_size {
    use std::ffi::c_void;
    extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleScreenBufferInfo(handle: *mut c_void, info: *mut [i16; 11]) -> i32;
    }

    /// The size the terminal says it is, as (columns, rows).
    pub fn now() -> Option<(u16, u16)> {
        // The console's description of its screen: eleven 16-bit numbers,
        // of which the sixth to ninth are the window's left, top, right
        // and bottom.
        let mut info = [0i16; 11];
        // SAFETY: a plain call into the Windows console API with the handle
        // the system gave us and a pointer to a local of the right size.
        let ok = unsafe { GetConsoleScreenBufferInfo(GetStdHandle(-11i32 as u32), &mut info) };
        (ok != 0).then(|| ((info[7] - info[5] + 1) as u16, (info[8] - info[6] + 1) as u16))
    }
}

#[cfg(not(windows))]
mod console_size {
    /// The size the terminal says it is, as (columns, rows).
    pub fn now() -> Option<(u16, u16)> {
        let out = std::process::Command::new("stty")
            .arg("size")
            .stdin(std::process::Stdio::inherit())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.split_whitespace().map(|n| n.parse::<u16>().ok());
        let (rows, cols) = (parts.next()??, parts.next()??);
        Some((cols, rows))
    }
}

#[cfg(not(windows))]
mod console {
    pub fn raw() {
        let _ = std::process::Command::new("stty").args(["raw", "-echo"]).status();
    }
}

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

fn say(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "{text}\r\n");
    let _ = out.flush();
    // For checks in a real terminal window, where the screen cannot be read.
    if let Some(log) = std::env::var_os("STANDIN_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{text}");
        }
    }
}

fn shown(text: &str) -> String {
    text.escape_debug().to_string()
}

/// Read input until Ctrl+D, reporting what arrives. `on_message` is called
/// with the typed text each time Enter is pressed outside a paste.
fn echo(mut on_message: impl FnMut(&str)) {
    let (mut enters, mut pastes) = (0u32, 0u32);
    let mut typed = String::new();
    let mut pending = String::new();
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 4096];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        pending.push_str(&String::from_utf8_lossy(&buf[..n]));
        loop {
            if let Some(rest) = pending.strip_prefix(PASTE_START) {
                let Some(end) = rest.find(PASTE_END) else { break };
                let body = rest[..end].to_string();
                pending = rest[end + PASTE_END.len()..].to_string();
                pastes += 1;
                let lines = body.split(['\r', '\n']).count();
                say(&format!("PASTE {lines} lines: {}", shown(&body)));
                typed.push_str(&body);
                continue;
            }
            // Part of a paste marker may have arrived; wait for the rest.
            if !pending.is_empty() && PASTE_START.starts_with(pending.as_str()) && pending.len() > 1 {
                break;
            }
            let Some(c) = pending.chars().next() else { break };
            pending.drain(..c.len_utf8());
            match c {
                '\u{4}' => return,
                '\r' => {
                    enters += 1;
                    say("ENTER");
                    on_message(&typed);
                    typed.clear();
                }
                '\u{7f}' | '\u{8}' => {
                    typed.pop();
                }
                c => typed.push(c),
            }
        }
        say(&format!("STATUS enters={enters} pastes={pastes} typed={}", shown(&typed)));
    }
}

fn base64(text: &str) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

/// Record one request and its reply for conversation `id`, the way Claude
/// Code is described as doing: a file of one JSON object per line, named for
/// the conversation, in a folder per project. Each reply counts 32,000
/// tokens. Nothing is written unless CLAUDE_CONFIG_DIR names a folder, so
/// this can never land in a real Claude Code folder by default.
fn record_reply(id: &str, nth: usize, said: &str) {
    let Some(config) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) else { return };
    let dir = PathBuf::from(config).join("projects").join("standin-project");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut file) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{id}.jsonl")))
    else {
        return;
    };
    let model = if said == "newmodel" { "claude-new-9" } else { "claude-opus-5-5" };
    let reply = |output: u32| {
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"{id}\",\"message\":{{\"id\":\"msg_{nth}\",\"model\":\"{model}\",\
             \"role\":\"assistant\",\"content\":[],\"usage\":{{\"input_tokens\":1000,\"output_tokens\":{output},\
             \"cache_creation_input_tokens\":10000,\"cache_read_input_tokens\":20500}}}}}}\n"
        )
    };
    let mut lines = format!(
        "{{\"type\":\"user\",\"sessionId\":\"{id}\",\"message\":{{\"role\":\"user\",\"content\":\"...\"}}}}\n"
    );
    if said == "garble" {
        lines += &format!(
            "{{\"type\":\"assistant\",\"message\":{{\"id\":\"msg_{nth}\",\"model\":\"{model}\",\"content\":[]}}}}\n"
        );
    } else {
        // A reply is written more than once as it arrives; the last stands.
        lines += &reply(1);
        lines += &reply(500);
    }
    let _ = file.write_all(lines.as_bytes());
}

fn report(body: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]7501;{body}\x07");
    let _ = out.flush();
}

/// Ask the status question the way Claude Code does: the question, then
/// "what kind of terminal are you?". Claude Code takes the question as
/// answered only if its answer comes before the second one. Says which;
/// None if input ended first.
fn ask_status() -> Option<bool> {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]7501;?\x07\x1b[c");
    let _ = out.flush();
    let mut stdin = std::io::stdin();
    let mut got: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256];
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    // Wait for the reply to the second question, which every terminal gives.
    let kind_reply = loop {
        if let Some(at) = find(&got, b"\x1b[?") {
            if got[at..].contains(&b'c') {
                break at;
            }
        }
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    };
    let answered = find(&got, b"\x1b]7501;?").is_some_and(|at| at < kind_reply);
    if answered {
        say("STATUS QUESTION ANSWERED");
    } else {
        say(&format!("STATUS QUESTION NOT ANSWERED got={}", shown(&String::from_utf8_lossy(&got))));
    }
    Some(answered)
}

fn status_mode() {
    let Some(answered) = ask_status() else { return };
    // Claude Code would stop here and never report if it was not answered.
    // The stand-in goes on reporting when asked to, so Lillignode's half can
    // still be tested.
    if answered {
        report("state=idle:app=claude-code");
    }
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 256];
    // One key, one report, the same parts Claude Code sends.
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &key in &buf[..n] {
            let body = match key {
                b'w' => format!("state=working:app=claude-code:progress=30:msg={}", base64("Reading files")),
                b'p' => format!("state=blocked:app=claude-code:kind=permission:msg={}", base64("Bash: ls")),
                b'q' => "state=blocked:app=claude-code:kind=question".to_string(),
                b'a' => "state=blocked:app=claude-code:kind=auth".to_string(),
                b'k' => "state=blocked:app=claude-code".to_string(),
                b'd' => "state=done:app=claude-code".to_string(),
                b'i' => "state=idle:app=claude-code".to_string(),
                b'e' => {
                    format!("state=error:app=claude-code:msg={}", base64("rate limited,\r\n\x1b[2J wait"))
                }
                b'u' => "state=pondering:app=claude-code".to_string(),
                b'n' => "state=working:app=claude-code:mood=fine".to_string(),
                b't' => "state=working:app=claude-code:id=task-1".to_string(),
                b'c' => "state=clear".to_string(),
                b'l' => format!("state=working:app=claude-code:msg={}", base64(&"long ".repeat(600))),
                0x04 => return,
                _ => continue,
            };
            report(&body);
            say(&format!("SENT {}", key as char));
        }
    }
}

/// Report the way Codex was seen to: nothing is asked; each report names
/// `codex` and ends with ESC \ where Claude Code's ends with a bell; and the
/// window title is set before it, ending in a bell that is not meant for
/// anybody.
fn codex_report(body: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]0;codex\x07\x1b]7501;{body}\x1b\\");
    let _ = out.flush();
}

fn codex_mode() {
    codex_report("state=idle:app=codex");
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 256];
    // One key, one report.
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &key in &buf[..n] {
            let body = match key {
                b'w' => "state=working:app=codex",
                b'p' => "state=blocked:app=codex:kind=permission",
                b'q' => "state=blocked:app=codex:kind=question",
                b'a' => "state=blocked:app=codex:kind=auth",
                b'd' => "state=done:app=codex",
                b'i' => "state=idle:app=codex",
                b'e' => "state=error:app=codex",
                b'c' => "state=clear",
                // A state nobody has seen, and a report under another name.
                b'u' => "state=pondering:app=codex",
                b'x' => "state=blocked:app=claude-code",
                0x04 => return,
                _ => continue,
            };
            codex_report(body);
            say(&format!("SENT {}", key as char));
        }
    }
}

/// Report the way any program may, as the open format describes: nothing
/// is asked, the program names itself `deploy` or gives no name, and some
/// keys send what a careless or hostile program might.
/// A program with parts (helper agents), each reporting for itself with
/// an id, as the format allows. One key sends one report.
fn parts_mode() {
    let send = |body: &str| {
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]7501;{body}\x1b\\");
        let _ = out.flush();
    };
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 256];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &key in &buf[..n] {
            match key {
                // The program's own record.
                b'o' => send("state=done:app=helper"),
                b'w' => send("state=working:app=helper"),
                // A helper with a name asks permission; one with no name waits.
                b'1' => send(&format!(
                    "state=blocked:kind=permission:app=helper:id=sub/1:title={}",
                    base64("subagent: tests")
                )),
                b'2' => send("state=blocked:app=helper:id=sub/2"),
                // The first helper goes on.
                b'a' => send("state=working:app=helper:id=sub/1"),
                // The second helper goes on.
                b'b' => send("state=working:app=helper:id=sub/2"),
                // Everything beneath "sub" is withdrawn; then everything.
                b'c' => send("state=clear:id=sub"),
                b'x' => send("state=clear"),
                // Two hundred parts, each working.
                b'f' => {
                    let mut all = String::new();
                    for n in 0..200 {
                        all += &format!("\x1b]7501;state=working:app=helper:id=flood/{n}\x1b\\");
                    }
                    let mut out = std::io::stdout();
                    let _ = out.write_all(all.as_bytes());
                    let _ = out.flush();
                }
                3 => return,
                _ => continue,
            }
            say(&format!("SENT {}", key as char));
        }
    }
}

fn reports_mode() {
    let send = |body: &str| {
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]7501;{body}\x1b\\");
        let _ = out.flush();
    };
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 256];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &key in &buf[..n] {
            match key {
                b'w' => send("state=working:app=deploy"),
                b'p' => send("state=blocked:kind=permission:app=deploy"),
                b'd' => send("state=done:app=deploy"),
                // No name at all: the format allows it.
                b'i' => send("state=idle"),
                b'c' => send("state=clear"),
                // A kind nobody knows, a part nobody knows, a part given twice.
                b'k' => send("state=blocked:kind=coffee:app=deploy"),
                b'n' => send("state=idle:app=deploy:mood=fine:state=working"),
                // A state nobody knows.
                b'u' => send("state=pondering:app=deploy"),
                // A message that tries to clear the screen, move the cursor,
                // set the window title and reverse the text after it.
                b'm' => send(&format!(
                    "state=error:app=deploy:msg={}",
                    base64("\x1b[2J\x1b[H\x1b]0;owned\x07first\r\nsecond\u{202e}third")
                )),
                // As fast as it can write: two thousand changes into and
                // out of needing the user, ending in needing the user.
                b'f' => {
                    let mut all = String::new();
                    for _ in 0..2000 {
                        all += "\x1b]7501;state=working:app=deploy\x1b\\";
                        all += "\x1b]7501;state=blocked:app=deploy\x1b\\";
                    }
                    let mut out = std::io::stdout();
                    let _ = out.write_all(all.as_bytes());
                    let _ = out.flush();
                }
                // One more change into needing the user.
                b'b' => {
                    send("state=working:app=deploy");
                    send("state=blocked:app=deploy");
                }
                // A shell marking its next prompt.
                b'$' => {
                    let mut out = std::io::stdout();
                    let _ = write!(out, "\x1b]133;A\x07");
                    let _ = out.flush();
                }
                0x04 => return,
                _ => continue,
            }
            say(&format!("SENT {}", key as char));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("echo");
    console::raw();
    match mode {
        "echo" => {
            print!("\x1b[?2004h");
            say("STANDIN READY echo");
            echo(|_| {});
        }
        "echo-plain" => {
            say("STANDIN READY echo-plain");
            echo(|_| {});
        }
        "args" => {
            say(&format!("STANDIN ARGS [{}]", args[1..].join("][")));
            echo(|_| {});
        }
        "stream" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
            let mut out = std::io::stdout().lock();
            for i in 1..=n {
                let colour = if i % 10 == 0 { "\x1b[32m" } else { "" };
                let filler = if i % 25 == 0 { "wrap ".repeat(50) } else { String::new() };
                let _ = write!(out, "{colour}stream line {i:05} of {n:05} {filler}\x1b[m\r\n");
                if i % 100 == 0 {
                    let _ = out.flush();
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
            let _ = out.flush();
            drop(out);
            print!("\x1b[?2004h");
            say(&format!("STREAM DONE {n}"));
            echo(|_| {});
        }
        "size" => {
            print!("\x1b[?2004h");
            say("STANDIN READY size");
            // Each Enter reports the size the terminal says it is now, with
            // what was typed, so two of these on screen can be told apart.
            echo(|typed| match console_size::now() {
                Some((cols, rows)) => say(&format!("SIZE {} is {cols} by {rows}", typed.trim())),
                None => say(&format!("SIZE {} is not known", typed.trim())),
            });
        }
        "fullscreen" => {
            print!("\x1b[?1049h\x1b[2J\x1b[H\x1b[?2004h");
            say("FULLSCREEN STANDIN");
            echo(|_| {});
            print!("\x1b[?1049l");
        }
        "claude" => {
            let home = PathBuf::from(std::env::var("STANDIN_HOME").expect("STANDIN_HOME must be set"));
            let _ = std::fs::create_dir_all(&home);
            let value =
                |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
            let (id, resumed) = match (value("--resume"), value("--session-id")) {
                (Some(id), _) => (id, true),
                (None, Some(id)) => (id, false),
                (None, None) => ("no-id".to_string(), false),
            };
            let file = home.join(format!("{id}.txt"));
            if resumed {
                let Ok(earlier) = std::fs::read_to_string(&file) else {
                    println!("No conversation found with session ID: {id}");
                    std::process::exit(1);
                };
                print!("\x1b[?2004h");
                say(&format!("STANDIN CLAUDE resumed {id}"));
                for line in earlier.lines() {
                    say(&format!("EARLIER {line}"));
                }
            } else {
                if file.exists() {
                    println!("Error: Session ID {id} is already in use.");
                    std::process::exit(1);
                }
                print!("\x1b[?2004h");
                say(&format!("STANDIN CLAUDE new {id}"));
            }
            // Like the real thing, a conversation exists only once something was said.
            echo(|message| {
                if !message.trim().is_empty() {
                    let mut all = std::fs::read_to_string(&file).unwrap_or_default();
                    record_reply(&id, all.lines().count() + 1, message.trim());
                    all.push_str(message.trim());
                    all.push('\n');
                    let _ = std::fs::write(&file, all);
                    say(&format!("SAID {}", message.trim()));
                }
            });
        }
        "status" => {
            say("STANDIN READY status");
            status_mode();
        }
        "status-echo" => {
            say("STANDIN READY status-echo");
            if ask_status().is_none() {
                return;
            }
            print!("\x1b[?2004h");
            say("STANDIN TAKES PASTES");
            echo(|_| {});
        }
        "codex" => {
            say("STANDIN READY codex");
            codex_mode();
        }
        "reports" => {
            say("STANDIN READY reports");
            reports_mode();
        }
        "parts" => {
            say("STANDIN READY parts");
            parts_mode();
        }
        "play" => {
            let Some(path) = args.get(1) else {
                println!("standin: play needs a tape");
                std::process::exit(2);
            };
            tape::play(path);
        }
        "status-silent" => {
            say("STANDIN READY status-silent");
            echo(|_| {});
        }
        other => {
            println!("standin: unknown mode {other}");
            std::process::exit(2);
        }
    }
    print!("\x1b[?2004l");
    say("STANDIN BYE");
}
