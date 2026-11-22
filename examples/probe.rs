//! Development tool, not part of the product.
//!
//! Runs a terminal program inside a pseudo-terminal the way a terminal window
//! would, and measures it: what the screen shows after each resize, how long a
//! typed character takes to appear, and whether it exits cleanly.
//!
//! Run it twice with the same script, once on the stand-in agent directly
//! and once on `lillignode` running that stand-in. The difference between the
//! two runs is what Lillignode adds.
//!
//!   probe <label> <exit-mode> <command> [args...]
//!   probe direct ctrld target/release/examples/standin echo
//!   probe hosted menu  target/release/lillignode
//!
//! exit-mode: `ctrld` sends Ctrl+D (the stand-in on its own); `menu` sends
//! Ctrl+Q, q, y (Lillignode).
//! Writes screen dumps and a report to the folder in PROBE_OUT (default: .).
//!
//! To measure Lillignode, point LILLIGNODE_STATE_DIR at a throwaway folder whose
//! workspace.json holds exactly one stand-in agent, so Lillignode opens
//! straight into it and never touches a real workspace. Set
//! LILLIGNODE_TIMING=<file> as well to get Lillignode's own per-key timings.
//!
//! PROBE_IDLE_SECS=<n>: before typing, leave the program alone for n seconds
//! and say how many bytes it wrote to its terminal meanwhile. The line
//! `idle: watching pid` comes first, so its processor time over those
//! seconds can be read from outside.
//!
//! PROBE_MAX_MEDIAN_MS=<n>: end with a failure if the median keystroke time
//! is above n milliseconds. On Windows, Lillignode at 0.2.0 took about 19 ms
//! (it wrote the cursor's place again after every key, and the terminal made
//! the echo wait a frame for that); about 2 ms since.
//!
//! Rules this probe keeps (CONTRIBUTING.md):
//! - It is for the stand-in, not for a real agent. It sets CLAUDE_CONFIG_DIR
//!   to a throwaway folder for whatever it starts, so nothing it runs can
//!   read or change a real Claude Code configuration.
//! - It types nothing until it sees the text it expects (PROBE_READY_TEXT,
//!   by default the stand-in's greeting). Any other screen stops it.
//! - It never presses Enter.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

enum Ev {
    Out(Instant, Vec<u8>),
    Exit(Instant, u32),
}

struct Probe {
    rx: Receiver<Ev>,
    term: lillignode::terminal::Terminal,
    writer: Box<dyn Write + Send>,
    raw: Vec<u8>,
    exit: Option<(Instant, u32)>,
}

impl Probe {
    fn take(&mut self, ev: Ev) -> Option<Instant> {
        match ev {
            Ev::Out(at, bytes) => {
                // Answer the questions a terminal window would answer.
                let reply = self.term.feed(&bytes);
                if !reply.is_empty() {
                    self.writer.write_all(&reply).unwrap();
                    self.writer.flush().unwrap();
                }
                self.raw.extend_from_slice(&bytes);
                Some(at)
            }
            Ev::Exit(at, code) => {
                self.exit = Some((at, code));
                None
            }
        }
    }

    /// Read until nothing has arrived for `quiet`, or `max` has passed.
    fn settle(&mut self, quiet: Duration, max: Duration) {
        let end = Instant::now() + max;
        while Instant::now() < end {
            match self.rx.recv_timeout(quiet) {
                Ok(ev) => {
                    self.take(ev);
                }
                Err(_) => return,
            }
        }
    }

    /// Read until the screen contains `needle`. Returns when the chunk that
    /// completed it arrived.
    fn until_shows(&mut self, needle: &str, max: Duration) -> Option<Instant> {
        let end = Instant::now() + max;
        loop {
            let left = end.checked_duration_since(Instant::now())?;
            match self.rx.recv_timeout(left) {
                Ok(ev) => {
                    if let Some(at) = self.take(ev) {
                        if self.term.screen().contents().contains(needle) {
                            return Some(at);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let label = args.next().expect("label");
    let exit_mode = args.next().expect("exit mode");
    let command = args.next().expect("command");
    let out_dir = PathBuf::from(std::env::var("PROBE_OUT").unwrap_or_else(|_| ".".into()));
    let mut report = String::new();
    let mut say = |line: String| {
        println!("{line}");
        report.push_str(&line);
        report.push('\n');
    };

    let (cols, rows) = (120u16, 36u16);
    let pair = native_pty_system()
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .expect("open pty");
    // Same lookup the product uses, so both runs start the same file.
    let program = lillignode::local::resolve_command(&command).expect("command not found");
    let is_batch = program.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd"));
    let mut cmd = if is_batch {
        let mut c = CommandBuilder::new("cmd.exe");
        c.args(["/d", "/c"]);
        c.arg(&program);
        c
    } else {
        CommandBuilder::new(&program)
    };
    cmd.args(args);
    cmd.cwd(std::env::current_dir().unwrap());
    // Whatever is started must not find a real Claude Code configuration.
    cmd.env("CLAUDE_CONFIG_DIR", std::env::temp_dir().join("lillignode-probe-not-a-real-claude-config"));
    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let pid = child.process_id();

    let (tx, rx) = mpsc::channel();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let out_tx = tx.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || out_tx.send(Ev::Out(Instant::now(), buf[..n].to_vec())).is_err() {
                break;
            }
        }
    });
    thread::spawn(move || {
        let code = child.wait().map(|s| s.exit_code()).unwrap_or(u32::MAX);
        let _ = tx.send(Ev::Exit(Instant::now(), code));
    });
    let writer = pair.master.take_writer().unwrap();

    let mut p = Probe {
        rx,
        // Answers the status question too, as Lillignode does.
        term: lillignode::terminal::Terminal::new(rows, cols),
        writer,
        raw: Vec::new(),
        exit: None,
    };
    let dump = |p: &Probe, name: &str| {
        let (_, cols) = p.term.screen().size();
        let grid: Vec<String> = p.term.screen().rows(0, cols.get()).collect();
        std::fs::write(out_dir.join(format!("{label}-{name}.txt")), grid.join("\n")).unwrap();
    };

    say(format!("label={label} command={command} pid={pid:?}"));

    // 1. Start.
    let t = Instant::now();
    // Wait for the first output however long it takes (up to 45 s), then for
    // the screen to stop changing.
    if let Ok(ev) = p.rx.recv_timeout(Duration::from_secs(45)) {
        p.take(ev);
    }
    say(format!("start: first output after {} ms", t.elapsed().as_millis()));
    p.settle(Duration::from_millis(2500), Duration::from_secs(45));
    say(format!("start: settled after {} ms, {} bytes", t.elapsed().as_millis(), p.raw.len()));
    dump(&p, "0-start-120x36");
    std::fs::write(
        out_dir.join(format!("{label}-start-raw.txt")),
        String::from_utf8_lossy(&p.raw).escape_debug().to_string(),
    )
    .unwrap();
    // PROBE_LOOK_ONLY: read the first screen and leave. No key is sent at all.
    if std::env::var("PROBE_LOOK_ONLY").is_ok() {
        dump(&p, "look");
        say(format!("look only: no keys sent. exited={:?}", p.exit.map(|(_, code)| code)));
        return;
    }
    // Type only into a screen this probe recognises. An earlier version
    // pressed keys at whatever was there and accepted an offer on the user's
    // behalf.
    let ready = std::env::var("PROBE_READY_TEXT").unwrap_or_else(|_| "STANDIN READY".into());
    dump(&p, "0c-ready-120x36");
    if !p.term.screen().contents().contains(&ready) {
        say(format!("start: STOPPED. Expected to see \"{ready}\" and did not; nothing was typed."));
        return;
    }
    if std::env::var("PROBE_ONLY_START").is_ok() {
        return;
    }

    if let Some(secs) = std::env::var("PROBE_IDLE_SECS").ok().and_then(|s| s.parse::<u64>().ok()) {
        say(format!("idle: watching pid {} for {secs} s", pid.unwrap_or(0)));
        let before = p.raw.len();
        let end = Instant::now() + Duration::from_secs(secs);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(ev) = p.rx.recv_timeout(left) {
                p.take(ev);
            }
        }
        say(format!("idle: {} bytes written to the terminal in {secs} s", p.raw.len() - before));
    }

    // 2. Keystroke echo. One character at a time, timed from the write to the
    // arrival of the output that makes it visible on screen.
    let text = "zqjxkvwzqjxkvwzqjxkvwzqjxkvwzqjxkvwzqjxk";
    let mut times: Vec<f64> = Vec::new();
    let mut lost = 0;
    for i in 1..=text.len() {
        let t0 = Instant::now();
        p.writer.write_all(&text.as_bytes()[i - 1..i]).unwrap();
        p.writer.flush().unwrap();
        match p.until_shows(&text[..i], Duration::from_secs(3)) {
            Some(at) => times.push(at.duration_since(t0).as_secs_f64() * 1000.0),
            None => lost += 1,
        }
        if i == 1 && lost == 1 {
            say("echo: STOPPED. The first character never appeared, so nothing more was typed.".into());
            return;
        }
        thread::sleep(Duration::from_millis(60));
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut too_slow = false;
    if times.is_empty() {
        say("echo: no typed character ever appeared".into());
    } else {
        let pct = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
        say(format!(
            "echo: n={} lost={} min={:.1} median={:.1} p95={:.1} max={:.1} ms",
            times.len(),
            lost,
            times[0],
            pct(0.5),
            pct(0.95),
            times[times.len() - 1]
        ));
        if let Some(most) = std::env::var("PROBE_MAX_MEDIAN_MS").ok().and_then(|s| s.parse::<f64>().ok()) {
            too_slow = pct(0.5) > most;
            if too_slow {
                say(format!("echo: FAILED. The median is above the {most} ms allowed."));
            }
        }
    }
    dump(&p, "1-typed-120x36");
    p.writer.write_all(&vec![0x7f; text.len()]).unwrap();
    p.writer.flush().unwrap();
    p.settle(Duration::from_millis(800), Duration::from_secs(5));

    // 3. Resize: narrower, wider, small, back.
    for (i, (c, r)) in [(100u16, 30u16), (160, 45), (80, 24), (120, 36)].into_iter().enumerate() {
        pair.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 }).unwrap();
        p.term.resize(r, c);
        p.settle(Duration::from_millis(1500), Duration::from_secs(10));
        dump(&p, &format!("{}-resize-{c}x{r}", i + 2));
        say(format!("resize {c}x{r}: alive={}", p.exit.is_none()));
    }

    // 4. Exit.
    let mark = p.raw.len();
    let t0 = Instant::now();
    match exit_mode.as_str() {
        // The stand-in ends on Ctrl+D.
        "ctrld" => p.writer.write_all(&[0x04]).unwrap(),
        // Lillignode's own way out: menu, quit, yes.
        "menu" => {
            for key in [&[0x11u8][..], b"q", b"y"] {
                p.writer.write_all(key).unwrap();
                p.writer.flush().unwrap();
                p.settle(Duration::from_millis(300), Duration::from_secs(2));
            }
        }
        other => panic!("unknown exit mode {other}"),
    }
    p.writer.flush().unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while p.exit.is_none() && Instant::now() < end {
        if let Ok(ev) = p.rx.recv_timeout(Duration::from_millis(200)) {
            p.take(ev);
        }
    }
    p.settle(Duration::from_millis(500), Duration::from_secs(2));
    match p.exit {
        Some((at, code)) => say(format!("exit: code={code} after {} ms", at.duration_since(t0).as_millis())),
        None => say("exit: DID NOT EXIT within 15 s".into()),
    }
    let tail = String::from_utf8_lossy(&p.raw[mark..]).to_string();
    say(format!(
        "exit: left alternate screen={} showed cursor={}",
        tail.contains("\x1b[?1049l"),
        tail.contains("\x1b[?25h")
    ));
    std::fs::write(out_dir.join(format!("{label}-exit-tail.txt")), tail.escape_debug().to_string()).unwrap();
    drop(say);
    std::fs::write(out_dir.join(format!("{label}-report.txt")), report).unwrap();
    std::process::exit(i32::from(too_slow));
}
