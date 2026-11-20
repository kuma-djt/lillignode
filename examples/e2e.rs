//! Development tool, not part of the product.
//!
//! Drives the real `lillignode` program through a pseudo-terminal, the way a
//! person at a terminal window would, and checks what appears on screen.
//!
//!   cargo build --release --all-targets && cargo run --release --example e2e
//!
//! It never runs a real agent and never touches real settings:
//! - every agent it starts is the stand-in program (`examples/standin.rs`)
//!   or the system shell, reached through launchers in a throwaway folder
//!   that is put first on PATH (including one named `claude`);
//! - the saved workspace and settings live in the same throwaway folder;
//! - CLAUDE_CONFIG_DIR points there too, so even a mistake could not reach
//!   a real Claude Code configuration.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use lillignode::terminal::Terminal;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

const COLS: u16 = 150;
const ROWS: u16 = 40;
const CTRL_Q: &[u8] = &[0x11];
const F12: &[u8] = b"\x1b[24~";
const F9: &[u8] = b"\x1b[20~";
const PAGE_UP: &[u8] = b"\x1b[5~";
const PAGE_DOWN: &[u8] = b"\x1b[6~";
const HOME: &[u8] = b"\x1b[H";
const ESC: &[u8] = b"\x1b";

enum Ev {
    Out(Vec<u8>),
    Exit(u32),
}

/// Where everything this run creates lives.
struct Sandbox {
    exe: PathBuf,
    root: PathBuf,
    state: PathBuf,
    work: PathBuf,
    launchers: PathBuf,
}

struct Session {
    rx: Receiver<Ev>,
    term: Terminal,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    raw: Vec<u8>,
    exit: Option<u32>,
}

impl Session {
    fn start(sb: &Sandbox) -> Session {
        Session::start_in(sb, &sb.state)
    }

    fn start_in(sb: &Sandbox, state: &Path) -> Session {
        Session::start_with(sb, state, &[])
    }

    /// Starts Lillignode without Claude Code's status off switch or
    /// `TERM_PROGRAM`, whatever this run inherited, then with `env` on top.
    fn start_with(sb: &Sandbox, state: &Path, env: &[(&str, &str)]) -> Session {
        let pair = native_pty_system()
            .openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new(&sb.exe);
        cmd.cwd(&sb.work);
        cmd.env_remove(lillignode::status::OFF_SWITCH);
        cmd.env_remove("TERM_PROGRAM");
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.env("LILLIGNODE_STATE_DIR", state);
        cmd.env("STANDIN_HOME", sb.root.join("standin-conversations"));
        cmd.env("CLAUDE_CONFIG_DIR", sb.root.join("not-a-real-claude-config"));
        let mut paths = vec![sb.launchers.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        cmd.env("PATH", std::env::join_paths(paths).unwrap());
        let mut child = pair.slave.spawn_command(cmd).expect("start lillignode");
        drop(pair.slave);
        let (tx, rx) = mpsc::channel();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let out = tx.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || out.send(Ev::Out(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
        thread::spawn(move || {
            let code = child.wait().map(|s| s.exit_code()).unwrap_or(u32::MAX);
            let _ = tx.send(Ev::Exit(code));
        });
        let writer = pair.master.take_writer().unwrap();
        Session {
            rx,
            term: Terminal::new(ROWS, COLS),
            writer,
            master: pair.master,
            raw: Vec::new(),
            exit: None,
        }
    }

    fn pump(&mut self, wait: Duration) -> bool {
        match self.rx.recv_timeout(wait) {
            Ok(Ev::Out(bytes)) => {
                let reply = self.term.feed(&bytes);
                if !reply.is_empty() {
                    let _ = self.writer.write_all(&reply);
                    let _ = self.writer.flush();
                }
                self.raw.extend_from_slice(&bytes);
                true
            }
            Ok(Ev::Exit(code)) => {
                self.exit = Some(code);
                true
            }
            Err(_) => false,
        }
    }

    fn pump_for(&mut self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.pump(Duration::from_millis(20));
        }
    }

    /// Takes output until none has come for 100 ms (at most 10 s), so a
    /// screen is read whole and not halfway through being drawn.
    fn settle(&mut self) {
        let end = Instant::now() + Duration::from_secs(10);
        while self.pump(Duration::from_millis(100)) && Instant::now() < end {}
    }

    fn rows(&self) -> Vec<String> {
        let cols = self.term.screen().size().1.get();
        self.term.screen().rows(0, cols).collect()
    }

    fn screen(&self) -> String {
        self.rows().join("\n")
    }

    /// The screen with line breaks removed, so text that wraps across the two
    /// notice rows can still be matched.
    fn flat(&self) -> String {
        let cols = self.term.screen().size().1.get() as usize;
        self.rows().iter().map(|r| format!("{r:<cols$}")).collect()
    }

    /// True when the screen shows `needle`: across a line's end as the screen
    /// lays it out, or across a break Lillignode made at a space when it
    /// wrapped a notice.
    fn shows(&self, needle: &str) -> bool {
        let rows = self.rows();
        let joined = rows.iter().map(|r| r.trim_end().to_string()).collect::<Vec<_>>().join(" ");
        // The status report panel wraps its own lines, beside the sidebar.
        let right = rows
            .iter()
            .map(|r| r.split_once('│').map_or(r.as_str(), |(_, right)| right).trim().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        self.flat().contains(needle) || joined.contains(needle) || right.contains(needle)
    }

    fn fail(&self, step: &str, detail: &str) -> ! {
        println!("  FAIL  {step}\n        {detail}\n--- screen ---\n{}\n--------------", self.screen());
        std::process::exit(1);
    }

    fn expect(&mut self, step: &str, needle: &str) {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if self.shows(needle) {
                println!("  ok    {step}");
                return;
            }
            self.pump(Duration::from_millis(100));
        }
        self.fail(step, &format!("expected to see: {needle}"));
    }

    fn expect_gone(&mut self, step: &str, needle: &str) {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if !self.shows(needle) {
                println!("  ok    {step}");
                return;
            }
            self.pump(Duration::from_millis(100));
        }
        self.fail(step, &format!("still showing: {needle}"));
    }

    /// Wait, then check the text is absent: for things that must not happen.
    fn never(&mut self, step: &str, needle: &str) {
        self.pump_for(Duration::from_millis(700));
        if self.shows(needle) {
            self.fail(step, &format!("must not show: {needle}"));
        }
        println!("  ok    {step}");
    }

    /// Type like a person: one key at a time, with a gap between keys.
    fn keys(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.writer.write_all(&[*b]).unwrap();
            self.writer.flush().unwrap();
            self.pump_for(Duration::from_millis(40));
        }
        self.pump_for(Duration::from_millis(200));
    }

    /// Send bytes in one go: a function key, or what a terminal sends for a paste.
    fn raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
        self.pump_for(Duration::from_millis(250));
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        self.term.resize(rows, cols);
    }

    /// Start an agent through the menu in the default folder.
    fn new_agent(&mut self, command: &str) {
        self.raw(CTRL_Q);
        self.keys(b"n");
        self.keys(command.as_bytes());
        self.keys(b"\r");
        self.keys(b"\r");
        self.keys(b"\r");
    }

    /// Quit through the menu, answering the confirmation if one is asked.
    fn quit(mut self, step: &str, expect_confirm: bool) -> String {
        self.raw(CTRL_Q);
        self.keys(b"q");
        if expect_confirm {
            self.expect(&format!("{step}: asks before stopping agents"), "? y / n");
            self.keys(b"y");
        }
        self.wait_for_exit(step)
    }

    /// q has been pressed: Lillignode must close without asking anything.
    fn quit_already_asked(self, step: &str) -> String {
        if self.exit.is_none() && self.flat().contains("? y / n") {
            self.fail(step, "asked before quitting");
        }
        self.wait_for_exit(step)
    }

    fn wait_for_exit(mut self, step: &str) -> String {
        let end = Instant::now() + Duration::from_secs(15);
        while self.exit.is_none() && Instant::now() < end {
            self.pump(Duration::from_millis(100));
        }
        while self.pump(Duration::from_millis(300)) {}
        match self.exit {
            Some(0) => println!("  ok    {step}: exit code 0"),
            other => self.fail(step, &format!("exit was {other:?}")),
        }
        String::from_utf8_lossy(&self.raw).to_string()
    }
}

fn check(step: &str, ok: bool, detail: &str) {
    if ok {
        println!("  ok    {step}");
    } else {
        println!("  FAIL  {step}\n        {detail}");
        std::process::exit(1);
    }
}

fn json_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

/// A launcher called `name` that runs the stand-in with `args`.
fn launcher(dir: &Path, standin: &Path, name: &str, args: &str) {
    if cfg!(windows) {
        let text = format!("@\"{}\" {args} %*\r\n", standin.display());
        std::fs::write(dir.join(format!("{name}.cmd")), text).unwrap();
    } else {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexec \"{}\" {args} \"$@\"\n", standin.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// How many times Lillignode has rung the terminal bell. Lillignode draws
/// agents' screens itself and never passes their bytes on, so every BEL in
/// its output is its own.
fn bells(s: &Session) -> usize {
    s.raw.iter().filter(|&&b| b == 0x07).count()
}

/// How long to wait for the status deadline, with a little to spare.
fn status_deadline() -> Duration {
    lillignode::status::STARTING_DEADLINE + Duration::from_secs(1)
}

fn paste_after_status(sb: &Sandbox, standin: &Path) {
    println!("M. Paste into an agent that asked the status question");
    let dir = sb.root.join("status-echo");
    std::fs::create_dir_all(&dir).unwrap();
    launcher(&dir, standin, "claude", "status-echo");
    let mut s = Session::start_in(sb, &sb.root.join("state-paste-status"));
    s.new_agent(&dir.join("claude").display().to_string());
    s.expect("the agent asks the status question", "STATUS QUESTION ");
    s.expect("and takes pastes", "STANDIN TAKES PASTES");
    s.keys(b"ab");
    s.expect("typing reaches it", "typed=ab");
    s.raw(b"\x1b[200~one\rtwo\x1b[201~");
    s.expect("a marked paste reaches it as one paste", "PASTE 2 lines: one\\rtwo");
    s.raw(b"three\rfour\r");
    s.expect("an unmarked burst reaches it as one paste", "PASTE 3 lines: three\\rfour\\r");
    drop(s.quit("quit", true));
}

fn cost(sb: &Sandbox) {
    println!("N. Cost, read from the record the agent keeps (stand-in for Claude Code)");
    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let state = sb.root.join("state-cost");
    // Off unless asked for: nothing about cost is drawn, and the status
    // report says how to switch it on.
    let mut s = Session::start_in(sb, &state);
    s.keys(b"n");
    s.keys(b"\r");
    s.keys(b"\r");
    s.keys(b"\r");
    s.expect("a new Claude Code agent", "STANDIN CLAUDE new ");
    s.keys(b"early");
    s.keys(b"\r");
    s.expect("something is said before cost is switched on", "SAID early");
    s.never("with no setting, no cost is shown", "tokens");
    s.never("and no total", "ALL AGENTS");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report says it is off and how to switch it on", "Cost: off.");
    s.expect("naming the setting", "add \"cost\": true to the settings file");
    s.keys(b"z");
    s.raw(CTRL_Q);
    s.keys(b"x");
    s.keys(b"y");
    s.expect("the agent is closed", "Closed Claude Code (work)");
    drop(s.quit("quit", false));

    std::fs::write(state.join("config.json"), r#"{ "cost": true, "cost_warning_tokens": 30000 }"#).unwrap();
    let mut s = Session::start_in(sb, &state);
    s.keys(b"n");
    s.keys(b"\r");
    s.keys(b"\r");
    s.keys(b"\r");
    s.expect("a new Claude Code agent, with cost switched on", "STANDIN CLAUDE new ");
    s.expect("before anything is said its cost is pending, not zero", "cost pending");
    s.expect("and so is the total", " ALL AGENTS  cost pending");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report says why", "Cost (experimental): pending. Nothing to read yet");
    s.keys(b"z");
    s.keys(b"hello");
    s.keys(b"\r");
    s.expect("something is said", "SAID hello");
    s.expect("its tokens show beside its folder, without those read from cache", "11.5k tokens");
    s.expect("and in the total", " ALL AGENTS  11.5k tokens");
    s.never("under the warning, the row is not marked", "PAST WARNING");
    s.keys(b"again");
    s.keys(b"\r");
    s.expect("the number grows with the record", " ALL AGENTS  23.0k tokens");
    s.keys(b"more");
    s.keys(b"\r");
    s.expect("past the warning, the row says PAST WARNING", "PAST WARNING 34.5k tokens");
    s.expect(
        "one notice names the agent and both counts",
        "Claude Code (work) is past your cost warning (34.5k tokens; warning at 30.0k a conversation).",
    );
    s.expect("and says first that nothing was stopped", "Warning only, nothing stopped: Claude Code (work)");
    s.never("no question is asked", "? y / n");
    s.keys(b"still");
    s.keys(b"\r");
    s.expect("typing still reaches the agent: nothing was held", "SAID still");
    s.expect("and the count goes on", "PAST WARNING 46.0k tokens");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "the status report gives the exact count, and the cache reads apart",
        "Cost (experimental) of this conversation so far, including before today: 46,000 tokens, not counting 82,000 read from cache.",
    );
    s.expect("by kind", "input 4,000, output 2,000, written to cache 40,000, read from cache 82,000");
    s.expect(
        "what was used since Lillignode was started",
        "Since Lillignode was started: 46,000 tokens, not counting 82,000 read from cache.",
    );
    s.expect("for all agents too", "All agents open now, since Lillignode was started: 46,000 tokens");
    s.expect("and that it is past the warning", "Past your cost warning of 30,000 tokens");
    s.expect("an estimate in dollars, said to be one", "about $0.27 (est.: Lillignode's price list of");
    s.expect("and who it does not apply to", "you are not billed this.");
    s.expect("limits are Unknown, not left out", "Limits: Unknown.");
    s.keys(b"z");

    s.new_agent(shell);
    s.expect("an agent that is not Claude Code has no cost to read", "cost unknown");
    s.expect("the total says it leaves one out", "not counted: 1 agent");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "and the status report says why",
        "Cost (experimental): unknown. Lillignode reads cost only for Claude Code.",
    );
    s.keys(b"z");
    drop(s.quit("quit", true));

    // The user says they pay by key: dollars, as an estimate. The agent
    // continues its conversation, so the cost is the whole conversation's.
    std::fs::write(state.join("config.json"), r#"{ "cost": true, "billing": "key" }"#).unwrap();
    let mut s = Session::start_in(sb, &state);
    s.expect("restored, continuing its conversation", "Claude Code (work) (continuing its conversation)");
    s.expect("the conversation's cost is back, in dollars for a key user", "est. $0.27");
    s.expect("the total too", " ALL AGENTS  est. $0.27");
    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.keys(ESC);
    s.expect("Claude Code is in front", "Typing goes to Claude Code (work)");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "what was said before this start is not counted as since started",
        "Since Lillignode was started: 0 tokens, not counting 0 read from cache.",
    );
    s.keys(b"z");
    s.keys(b"newmodel");
    s.keys(b"\r");
    s.expect("a model with no price: tokens, never dollars that leave it out", "57.5k tokens");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "since started now counts the one new reply",
        "Since Lillignode was started: 11,500 tokens, not counting 20,500 read from cache.",
    );
    s.keys(b"z");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report names the model", "has no price for claude-new-9");
    s.keys(b"z");
    s.keys(b"garble");
    s.keys(b"\r");
    s.expect("a reply that cannot be read makes the total unknown", " ALL AGENTS  cost unknown");
    s.never("and no number is left standing", "57.5k tokens");
    s.expect("a notice says so once, and where the reason is", "Cost shows unknown for Claude Code (work)");
    s.expect("and that nothing was changed", "nothing was changed. Ctrl+Q then s says why.");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "the status report says which line and why",
        "was not understood: the reply has no token counts",
    );
    s.expect("and that nothing was changed there either", "Nothing was changed.");
    s.keys(b"z");
    drop(s.quit("quit", true));
}

fn side_by_side(sb: &Sandbox, standin: &Path) {
    println!("O. Two agents side by side (stand-ins that say what size they are)");
    launcher(&sb.launchers, standin, "sizer", "size");
    let dir = sb.root.join("status-pair");
    std::fs::create_dir_all(&dir).unwrap();
    launcher(&dir, standin, "claude", "status");
    let mut s = Session::start_in(sb, &sb.root.join("state-pair"));
    s.new_agent("sizer");
    s.expect("an agent that reports its size", "STANDIN READY size");
    s.keys(b"one\r");
    s.expect("one agent on screen has the whole agent area", "SIZE one is 122 by 37");
    s.raw(CTRL_Q);
    s.never("with one agent the menu does not offer two side by side", "v two side by side");
    s.keys(b"v");
    s.expect(
        "v with one agent is refused, with the reason",
        "Two agents side by side takes two agents, and one is open. Nothing was changed.",
    );
    s.new_agent("sizer");
    s.expect("a second agent", "Started sizer 2 in");
    s.keys(b"two\r");
    s.expect("it has the whole area too", "SIZE two is 122 by 37");
    s.raw(CTRL_Q);
    s.expect("now the menu offers it", "v two side by side");
    s.keys(b"v");
    s.expect("v says which two", "sizer 2 (work) and sizer (work) are side by side");
    s.never("the notice does not say where typing goes", "typing goes to the one");
    s.expect("and the way back", "If one draws badly, Ctrl+Q then v goes back to one.");
    s.expect("the agent in front has a heading that says so", " > TYPING GOES HERE  sizer 2 (work)");
    s.expect("the bar names the side", "Typing goes to sizer 2 (work), on the left.");
    s.keys(b"left\r");
    s.expect("the agent on the left is sized to its half", "SIZE left is 60 by 36");
    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.keys(ESC);
    s.expect("Tab moves the keyboard to the one beside it", "Typing goes to sizer (work), on the right.");
    s.keys(b"right\r");
    s.expect("the agent on the right is sized to its half too", "SIZE right is 60 by 36");
    s.expect("and both are on screen at once", "SIZE left is 60 by 36");

    s.resize(100, ROWS);
    s.expect(
        "a window made too narrow shows one agent and says why",
        "Too narrow for two agents now, so only sizer (work) is shown",
    );
    s.expect("and that the other was left alone", "sizer 2 (work) was not stopped or closed.");
    s.expect("and the way to stay with one", "or Ctrl+Q then v to stay with one.");
    s.keys(b"alone\r");
    s.expect("the one shown has the whole area again", "SIZE alone is 72 by 37");
    s.resize(COLS, ROWS);
    s.expect("widened again, both come back", "There is room again: two agents are side by side.");
    s.keys(b"again\r");
    s.expect("at half the width each", "SIZE again is 60 by 36");
    s.raw(CTRL_Q);
    s.expect("the menu offers the way back", "v one agent");
    s.keys(b"v");
    s.expect("v again goes back to one", "One agent on screen: sizer (work).");
    s.keys(b"whole\r");
    s.expect("with the whole area", "SIZE whole is 122 by 37");

    // An agent that is waiting for the user, and the view.
    s.new_agent(&dir.join("claude").display().to_string());
    s.expect("a stand-in for Claude Code that reports status", "STANDIN READY status");
    s.keys(b"p");
    s.expect("it needs you", " Lillignode  1 NEEDS YOU");
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.expect(
        "changing the view while an agent needs you asks first",
        "Resize every agent now? y / n   Waiting for you: Claude Code (work).",
    );
    s.keys(b"n");
    s.expect("n leaves everything as it was", "The view was left as it was. No agent was resized.");
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.keys(b"y");
    s.expect("y puts two side by side", "Claude Code (work) and sizer (work) are side by side");
    s.expect(
        "in front and needing you: its heading says typing goes here",
        " > NEEDS YOU - you are typing here  Claude Code (work)",
    );
    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.keys(ESC);
    s.expect(
        "beside the one in front and needing you: its heading says typing is not going there",
        " ! NEEDS YOU - you are NOT typing here  Claude Code (work)",
    );
    s.expect("and the bar says where it is going", "Typing goes to sizer (work), on the right.");
    s.keys(b"other\r");
    s.expect("typing goes to the agent in front, not the one waiting", "SIZE other is 60 by 36");
    s.expect(
        "the notice still names the one waiting and how to reach it",
        "Claude Code (work) needs you: permission. Ctrl+Q then Tab",
    );
    drop(s.quit("quit", true));
}

/// A second kind of agent. The stand-in is named codex, so it is taken for
/// Codex: its status is read from reports it sends without asking, which
/// need no answer, so this passes with either pseudo-terminal.
fn codex(sb: &Sandbox, standin: &Path) {
    println!("P. Status reported by a second kind of agent (stand-in for Codex)");
    let reports = sb.root.join("codex-reports");
    let silent = sb.root.join("codex-silent");
    for (dir, mode) in [(&reports, "codex"), (&silent, "status-silent")] {
        std::fs::create_dir_all(dir).unwrap();
        launcher(dir, standin, "codex", mode);
    }
    let state = sb.root.join("state-codex");
    let mut s = Session::start_in(sb, &state);
    s.new_agent(&reports.join("codex").display().to_string());
    s.expect("it starts", "STANDIN READY codex");
    s.expect("its first report shows, with nothing asked and nothing answered", "work  Idle ");
    s.expect("it is named Codex, with the sign for Idle", ">/\\z Codex");
    let bells_before = bells(&s);
    s.keys(b"w");
    s.expect("reported: working", "work  Working ");
    s.keys(b"p");
    s.expect("reported: needs permission, the yellow block", "work  NEEDS YOU   ");
    s.expect("the heading counts it", " Lillignode  1 NEEDS YOU");
    s.expect("and its mark carries the sign for Needs you", ">/\\! Codex");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report is about Codex", "Status report for Codex (work)");
    s.expect("what it needs is there", "Status: Needs you: permission.");
    s.expect("it says nothing was asked", "Status question: not asked. A program may report without asking.");
    s.expect("and who sent it", "Sent by: Codex.");
    s.expect("with the report as it arrived", "ESC ]7501;state=blocked:app=codex:kind=permission");
    s.expect("and which Codex it was checked with", "Checked with Codex 0.169.0.");
    s.never("and nothing about Claude Code's own switch", "CLAUDE_CODE_DISABLE_TERMINAL_TITLE");
    s.keys(b"z");
    s.keys(b"q");
    s.expect("reported: has a question", "SENT q");
    s.keys(b"a");
    s.expect("reported: needs signing in", "SENT a");
    s.expect("still needing you", "work  NEEDS YOU   ");
    s.keys(b"d");
    s.expect("reported: done", "work  Done ");
    s.expect_gone("the heading count goes", "NEEDS YOU");
    // The agent's own bell characters, one at the end of every title, are
    // not Lillignode's and are never passed on.
    check(
        "the bell rang once for one stretch of needing you",
        bells(&s) == bells_before + 1,
        &format!("{} bells", bells(&s) - bells_before),
    );
    s.keys(b"e");
    s.expect("reported: failed", "work  Error   ");
    s.keys(b"c");
    s.expect("a withdrawn status is Unknown", "work  Unknown   ");
    s.never("which is about this agent, not about Codex's reporting", "Status shows Unknown");
    s.keys(b"i");
    s.expect("the next report replaces it", "work  Idle ");
    s.keys(b"x");
    s.expect(
        "a report under another program's name is read: the report says who it is",
        "work  NEEDS YOU   ",
    );
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "and the status report says whose name it carried, and that it differs",
        "Sent by: Claude Code. This agent was started as Codex",
    );
    s.keys(b"z");
    s.keys(b"u");
    s.expect("a state Lillignode does not know", "SENT u");
    s.expect("is Unknown, never the nearest match", "work  Unknown   ");
    s.expect(
        "a notice says so, naming Codex",
        "Status shows Unknown: Codex sent a status Lillignode does not understand",
    );
    s.keys(b"d");
    s.expect("done again", "work  Done ");
    // Everything is finished, so quit does not ask.
    s.raw(CTRL_Q);
    s.keys(b"q");
    drop(s.quit_already_asked("quit with Codex Done does not ask"));
    let saved = std::fs::read_to_string(state.join("workspace.json")).unwrap();
    check("it is saved as Codex", saved.contains("\"kind\": \"codex\""), &saved);
    check("with no conversation to continue", !saved.contains("conversation"), &saved);

    let mut s = Session::start_in(sb, &state);
    s.expect(
        "it comes back, and is not said to continue a conversation",
        "Restored 1 agent: Codex (work) (new conversation).",
    );
    s.expect("and is read again from its first report", "work  Idle ");
    drop(s.quit("quit", false));

    println!("P2. A Codex that reports nothing, as when it is signed out");
    let mut s = Session::start_in(sb, &sb.root.join("state-codex-silent"));
    s.new_agent(&silent.join("codex").display().to_string());
    s.expect("it starts", "STANDIN READY status-silent");
    s.expect("shows Starting at first", "work  Starting ");
    s.pump_for(status_deadline());
    s.expect("and Unknown once the deadline passes", "work  Unknown   ");
    s.expect(
        "the notice is Codex's own, and leads with the likeliest cause",
        "Status shows Unknown: Codex is not reporting it. If it is signed out it reports nothing:",
    );
    s.expect(
        "with what to do and that nothing was changed",
        "sign in and it will. Agents work as before; nothing was changed.",
    );
    s.never("not the notice about the two files, which Codex does not need", "conpty.dll");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report says why", "Codex sent no status report within 15 seconds of starting.");
    s.keys(b"z");
    s.raw(CTRL_Q);
    s.keys(b"q");
    s.expect("quit asks, because its status is not known", "Quit and stop 1 running agent? y / n");
    s.expect("and names it", "Not finished or not known: Codex (work) (Unknown: no report).");
    s.keys(b"n");
    drop(s.quit("quit", true));
}

/// Any program that reports its status has it shown. The stand-in is
/// named deploy, which Lillignode knows nothing about, and reports without
/// asking, so this passes with either pseudo-terminal.
fn any_program(sb: &Sandbox, standin: &Path) {
    println!("Q. Status reported by a program Lillignode knows nothing about");
    let dir = sb.root.join("any-program");
    std::fs::create_dir_all(&dir).unwrap();
    launcher(&dir, standin, "deploy", "reports");
    let state = sb.root.join("state-any-program");
    let mut s = Session::start_in(sb, &state);
    s.new_agent(&dir.join("deploy").display().to_string());
    s.expect("it starts", "STANDIN READY reports");
    s.expect("with nothing said, it is Unknown from the start, not Starting", "work  Unknown   ");
    s.never("and that is not worth a notice: nothing was expected of it", "Status shows Unknown");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "the status report says why",
        "Status: Unknown: no report. This program has sent no status report.",
    );
    s.expect("and that none has come", "No report has been received.");
    s.keys(b"z");
    let bells_before = bells(&s);
    s.keys(b"w");
    s.expect("its first report shows", "work  Working ");
    s.keys(b"p");
    s.expect("needing you shows the yellow block", "work  NEEDS YOU   ");
    s.expect("the heading counts it", " Lillignode  1 NEEDS YOU");
    check("and the bell rings", bells(&s) == bells_before + 1, &format!("{}", bells(&s) - bells_before));
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("what it needs is there", "Status: Needs you: permission.");
    s.expect("the report names its sender, in its own word", "Sent by: \"deploy\".");
    s.expect("and Lillignode says it has not checked this program", "Not checked with this program.");
    s.keys(b"z");
    s.keys(b"d");
    s.expect("reported: done", "work  Done ");
    s.keys(b"i");
    s.expect("a report with no name is still the program's", "work  Idle ");
    s.keys(b"k");
    s.expect("a kind Lillignode does not know still needs you", "work  NEEDS YOU   ");
    s.keys(b"n");
    s.expect("unknown and repeated parts are passed over; the last state stands", "work  Working ");
    s.keys(b"u");
    s.expect("a state Lillignode does not know is Unknown, never the status before it", "work  Unknown   ");
    s.never("with no notice: this program was never known to work", "Status shows Unknown");
    s.keys(b"m");
    s.expect("a message that tries to draw is read for its status", "work  Error   ");
    s.expect("the sidebar is whole", " Lillignode ");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "its words are shown as words, with nothing that draws or reorders",
        "Status: Error: [2J [H ]0;owned first",
    );
    s.expect("and the report as it arrived is spelt out", "ESC ]7501;state=error:app=deploy:msg=");
    s.keys(b"z");
    check("the window title was not changed by it", !contains(&s.raw, b"]0;owned"), "owned");

    let bells_before = bells(&s);
    s.keys(b"f");
    s.expect("a flood of reports arrives", "SENT f");
    s.expect("the row shows the last of them", "work  NEEDS YOU   ");
    for _ in 0..6 {
        s.keys(b"b");
        s.pump_for(Duration::from_millis(300));
    }
    let rung = bells(&s) - bells_before;
    check("a flood rings a few bells, not thousands", (1..=3).contains(&rung), &format!("{rung} bells"));
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("the status report says bells were held back", "Bells not rung: ");
    s.keys(b"z");
    s.keys(b"$");
    s.expect("a shell prompt after needing you ends it", "work  Unknown   ");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("and the status report says why", "Status: Unknown: at a prompt.");
    s.keys(b"z");
    s.keys(b"d");
    s.expect("done", "work  Done ");
    s.keys(b"$");
    s.never("a shell prompt leaves Done alone", "work  Unknown   ");
    s.raw(CTRL_Q);
    s.keys(b"q");
    drop(s.quit_already_asked("quit with it Done does not ask"));
    let saved = std::fs::read_to_string(state.join("workspace.json")).unwrap();
    check("it is saved as a plain program", saved.contains("\"kind\": \"program\""), &saved);
}

/// Options typed when starting an agent, and a kind stated for it.
fn start_options(sb: &Sandbox, standin: &Path) {
    println!("R. Starting an agent with options and a stated kind");
    let dir = sb.root.join("start-options");
    std::fs::create_dir_all(&dir).unwrap();
    launcher(&dir, standin, "teller", "args");
    let teller = dir.join("teller").display().to_string();
    let state = sb.root.join("state-start-options");
    let mut s = Session::start_in(sb, &state);
    s.keys(b"n");
    s.keys(format!("{teller}\r").as_bytes());
    s.expect("options are asked for after the program", "as a plain program:");
    s.keys(b"one \"two three\"\r\r");
    s.expect("what will start is shown first", "Start it as a plain program? y / n / k");
    s.expect("with the options as they were split", "Options [one][two three]. n: change it");
    s.never("nothing has started yet", "STANDIN ARGS");
    s.keys(b"y");
    s.expect("the program gets them as split, a space kept inside one", "STANDIN ARGS [one][two three]");

    // A stated kind: a program not named claude, started as Claude Code,
    // is given a conversation.
    s.raw(CTRL_Q);
    s.keys(b"n");
    s.keys(format!("{teller}\r").as_bytes());
    s.keys(b"\t");
    s.expect("Tab changes the kind on the same line", "as Claude Code:");
    s.keys(b"\r\r");
    s.expect("a kind the name does not suggest is asked about", "Start it as Claude Code? y / n / k");
    s.expect("and what that means is said", "As Claude Code: it continues its conversation next time.");
    s.keys(b"y");
    s.expect("it is given a conversation, after nothing typed", "STANDIN ARGS [--session-id][");

    // Options Claude Code would take as a second conversation.
    s.raw(CTRL_Q);
    s.keys(b"n");
    s.keys(b"\r--resume x\r\r");
    s.expect("are refused before anything starts", "Cannot start it as typed. n: change it, k: another kind");
    s.expect("with what to do first", "Esc: cancel   Take out --resume: Lillignode gives Claude Code");
    s.keys(b"y");
    s.keys(ESC);
    s.expect("Esc leaves it", "Cancelled. Nothing was started.");

    if cfg!(windows) {
        // teller is a .cmd launcher here: the command interpreter would act
        // on an & in its options.
        s.raw(CTRL_Q);
        s.keys(b"n");
        s.keys(format!("{teller}\ra&b\r\r").as_bytes());
        s.expect(
            "options a launcher would act on are refused at the form",
            "Esc: cancel   Take out the \"&\": this program is a launcher",
        );
        s.keys(ESC);

        // A launcher in a folder with a space, given an option with a
        // space: its path reaches the interpreter in quotes, and both hold.
        let spaced = sb.root.join("two words");
        std::fs::create_dir_all(&spaced).unwrap();
        launcher(&spaced, standin, "teller", "args");
        s.raw(CTRL_Q);
        s.keys(b"n");
        s.keys(format!("{}\r\"a b\" c\r\ry", spaced.join("teller").display()).as_bytes());
        s.expect("a launcher in a folder with a space starts, its options whole", "STANDIN ARGS [a b][c]");

        // In a folder whose path holds an &, npm's launchers would cut
        // their own folder at it: refused before anything starts.
        let rd = sb.root.join("R&D");
        std::fs::create_dir_all(&rd).unwrap();
        launcher(&rd, standin, "teller", "args");
        s.raw(CTRL_Q);
        s.keys(b"n");
        s.keys(format!("{}\rone\r\r", rd.join("teller").display()).as_bytes());
        s.expect("a launcher in a folder named R&D is refused, and why", "the launcher's path holds \"&\"");
        s.keys(ESC);
    }
    drop(s.quit("quit", true));
    let saved = std::fs::read_to_string(state.join("workspace.json")).unwrap();
    check("the options are saved as split", saved.contains("\"two three\""), &saved);
    check("and the stated kind", saved.contains("\"kind\": \"claude-code\""), &saved);
    let started = if cfg!(windows) { 3 } else { 2 };
    check("only the agents asked for were started", saved.matches("\"command\"").count() == started, &saved);
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn conversation_id(file: &Path) -> String {
    let saved = std::fs::read_to_string(file).unwrap();
    let at = saved.find("\"conversation\"").expect("a conversation id is saved");
    saved[at..].split('"').nth(3).unwrap().to_string()
}

fn main() {
    let here = std::env::current_exe().unwrap();
    let examples = here.parent().unwrap();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let exe = examples.parent().unwrap().join(format!("lillignode{ext}"));
    let standin = examples.join(format!("standin{ext}"));
    assert!(exe.is_file(), "build lillignode first: {}", exe.display());
    assert!(standin.is_file(), "build the stand-in first: {}", standin.display());
    let shell = if cfg!(windows) { "cmd" } else { "sh" };

    // The system's own temporary folder, long as it is on macOS: a notice
    // naming a file in it must still keep its reason.
    let root: PathBuf = std::env::temp_dir().join(format!("lillignode-e2e-{}", std::process::id()));
    let sb = Sandbox {
        exe,
        state: root.join("state"),
        work: root.join("work"),
        launchers: root.join("launchers"),
        root,
    };
    std::fs::create_dir_all(&sb.work).unwrap();
    std::fs::create_dir_all(&sb.launchers).unwrap();
    launcher(&sb.launchers, &standin, "echoer", "echo");
    launcher(&sb.launchers, &standin, "plain", "echo-plain");
    launcher(&sb.launchers, &standin, "streamer", "stream 3000");
    launcher(&sb.launchers, &standin, "fullscreen", "fullscreen");
    launcher(&sb.launchers, &standin, "claude", "claude");
    let file = sb.state.join("workspace.json");
    println!("lillignode: {}\nthrowaway folder: {}\n", sb.exe.display(), sb.root.display());

    println!("A. First run");
    let mut s = Session::start(&sb);
    s.expect("empty workspace explains itself", "No agents yet.");
    s.expect("empty workspace says what to press", "Press n to start one");
    s.expect("and names the menu keys", "Ctrl+Q or F12 opens Lillignode's menu");
    s.keys(b"n");
    s.expect("n asks which agent", "Agent to start:");
    s.keys(b"nosuchagent\r");
    s.expect("then asks for its options", "Options for nosuchagent as a plain program:");
    s.keys(b"\r");
    s.expect("then asks which folder", "Folder to work in:");
    s.keys(b"\r");
    s.expect("unknown program: says so", "\"nosuchagent\" is not installed or not on PATH");
    s.expect("unknown program: says nothing was started", "Nothing was started");
    s.keys(ESC);
    s.expect("Esc cancels", "Cancelled. Nothing was started.");
    s.expect("failed start added nothing", "No agents yet. Press n.");

    s.keys(b"n");
    s.keys(format!("{shell}\r\r").as_bytes());
    s.keys(b"no-such-folder\r");
    s.expect("missing folder: says so", "does not exist");
    s.keys(&[0x7f; 14]);
    s.keys(b"\r");
    s.expect("corrected folder: agent starts", &format!("Started {shell} in"));
    s.expect("sidebar shows it running, status Unknown", "work  Unknown   ");
    s.expect("under a hat, with the sign for Unknown", ">/\\? ");
    s.expect("bar says where typing goes", &format!("Typing goes to {shell} (work)"));

    s.raw(CTRL_Q);
    s.expect("Ctrl+Q opens the menu", "MENU");
    s.keys(b"n");
    s.keys(format!("{shell}\r\r").as_bytes());
    s.keys(b"\r");
    s.expect("second agent in the same folder starts with a numbered name", &format!("Started {shell} 2 in"));

    println!("B. An agent that dies");
    s.keys(b"exit 3\r");
    s.expect("notice says it stopped and with what code", "stopped with an error (exit code 3)");
    s.expect("sidebar marks it", "Exited (error 3)");
    s.expect("bar offers what to do", "r start again");
    s.keys(b"r");
    s.expect("a bare key at a stopped agent is not a command", "has stopped, so what you typed was not sent");
    s.raw(CTRL_Q);
    s.keys(b"r");
    s.expect("r starts it again", &format!("Started {shell} 2 (work) again."));
    s.expect_gone("sidebar no longer shows the error", "Exited (error 3)");

    println!("C. Resize and quit");
    s.resize(100, 30);
    s.expect("still drawn after shrinking", " Lillignode ");
    s.resize(60, 20);
    s.expect("too small: says so", "This window is too small for Lillignode (60 by 20).");
    s.expect("too small: says what size is needed", "least 80 columns by 12 rows.");
    s.expect("too small: still names the menu keys", "Ctrl+Q or F12: menu, then q to quit");
    s.resize(COLS, ROWS);
    s.expect("drawn again after growing back", "Typing goes to");
    s.raw(CTRL_Q);
    s.keys(b"q");
    s.expect("quit names what will stop", "Quit and stop 2 running agents? y / n");
    s.keys(b"n");
    s.expect("n cancels the quit", "Typing goes to");
    let tail = s.quit("quit", true);
    check(
        "says the workspace was saved",
        tail.contains("Your workspace was saved"),
        &tail[tail.len().saturating_sub(300)..],
    );
    check("terminal handed back", tail.contains("\x1b[?1049l"), "did not leave the alternate screen");
    let saved = std::fs::read_to_string(&file).unwrap();
    check("saved file is plain JSON with both agents", saved.matches("\"command\"").count() == 2, &saved);

    println!("D. Restore");
    let mut s = Session::start(&sb);
    s.expect(
        "says what was restored, and does not claim a conversation continued",
        &format!(
            "Restored 2 agents: {shell} (work) (new conversation), {shell} 2 (work) (new conversation)."
        ),
    );
    s.expect("agents are running", "work  Unknown   ");
    drop(s.quit("quit", true));

    println!("E. Agents that cannot be restored");
    let gone = sb.root.join("gone");
    std::fs::write(
        &file,
        format!(
            r#"{{"version": 1, "focused": 0, "agents": [
  {{"name": "{shell}", "command": "{shell}", "args": [], "folder": "{w}"}},
  {{"name": "nosuchagent", "command": "nosuchagent", "args": [], "folder": "{w}"}},
  {{"name": "{shell}", "command": "{shell}", "args": [], "folder": "{g}"}}
]}}"#,
            w = json_path(&sb.work),
            g = json_path(&gone)
        ),
    )
    .unwrap();
    let mut s = Session::start(&sb);
    s.expect("counts what came back", "Restored 1 of 3 agents");
    s.expect(
        "names what did not and why",
        "Not restored: nosuchagent (work): \"nosuchagent\" is not installed",
    );
    s.expect("failed agents stay in the sidebar", "Not started");
    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.expect(
        "its own screen gives the reason",
        "nosuchagent (work) was not started: \"nosuchagent\" is not installed or not on PATH.",
    );
    s.expect("and what to do", "then r starts it again. Or the menu key, then x removes it.");
    s.keys(b"\t");
    s.expect("missing folder gets its own reason", "does not exist");
    std::fs::create_dir_all(&gone).unwrap();
    s.keys(b"r");
    s.expect("after fixing the cause, r starts it", &format!("Started {shell} (gone) again."));
    s.raw(CTRL_Q);
    s.keys(b"\t\t");
    s.expect("Tab keeps the menu open, so two Tabs move two agents", "nosuchagent (work) was not started");
    s.keys(b"x");
    s.expect("x asks first, even though nothing is running", "Remove nosuchagent (work)? y / n");
    s.keys(b"y");
    s.expect("y removes it", "Closed nosuchagent (work) and removed it");
    drop(s.quit("quit", true));
    let saved = std::fs::read_to_string(&file).unwrap();
    check("removed agent is gone from the saved file", !saved.contains("nosuchagent"), &saved);

    // What Lillignode would never have written is not started, and says why.
    // The launchers here are .cmd files on Windows only.
    let launcher_args = if cfg!(windows) { r#"["--x", "a&b"]"# } else { "[]" };
    let refused_state = sb.root.join("state-refused");
    std::fs::create_dir_all(&refused_state).unwrap();
    let refused_file = refused_state.join("workspace.json");
    std::fs::write(
        &refused_file,
        format!(
            r#"{{"version": 1, "focused": 0, "from_a_newer_one": true, "agents": [
  {{"name": "echoer", "command": "echoer", "args": {launcher_args}, "folder": "{w}"}},
  {{"name": "bell\u0007", "command": "echoer", "args": [], "folder": "{w}", "colour": "red"}}
]}}"#,
            w = json_path(&sb.work)
        ),
    )
    .unwrap();
    let mut s = Session::start_in(&sb, &refused_state);
    s.expect("Lillignode is up", "Ctrl+Q or F12:");
    if cfg!(windows) {
        s.expect(
            "an argument a .cmd launcher would act on: not started, and why",
            "echoer (work) was not started: its arguments hold \"&\", which Lillignode will not pass to a .cmd or .bat launcher.",
        );
    }
    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.expect(
        "a control character in a saved name: not started, and why",
        "bell  (work) was not started: its saved name holds a control character, which Lillignode never writes.",
    );
    s.never("it gives no second way out that r would not take", "then r starts it again");
    s.keys(ESC);
    drop(s.quit("quit", cfg!(not(windows))));
    let saved = std::fs::read_to_string(&refused_file).unwrap();
    check("the refused entries are saved back as they were", saved.contains("bell\\u0007"), &saved);
    check(
        "lines this Lillignode does not know are saved back",
        saved.contains("\"from_a_newer_one\": true") && saved.contains("\"colour\": \"red\""),
        &saved,
    );

    println!("F. Damaged saved workspace");
    std::fs::write(&file, "{ \"version\": 1, \"agents\": [ tru").unwrap();
    let mut s = Session::start(&sb);
    s.expect("says it could not be read", "Your saved workspace could not be read");
    s.expect("says it used the previous copy", "Loaded the previous copy, saved less than a minute ago");
    s.expect("previous copy's agents are back", "work  Unknown   ");
    let tail = s.quit("quit", true);
    check(
        "problem is repeated after closing",
        tail.contains("Earlier, on starting: Your saved workspace could not be read"),
        "not repeated",
    );
    let kept: Vec<_> = std::fs::read_dir(&sb.state)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("workspace.unreadable-"))
        .collect();
    check("damaged file was kept, not deleted", kept.len() == 1, "no workspace.unreadable-* file");
    check(
        "kept file still has the damaged contents",
        std::fs::read_to_string(kept[0].path()).unwrap().ends_with("tru"),
        "contents changed",
    );
    std::fs::write(&file, "").unwrap();
    std::fs::remove_file(sb.state.join("workspace.previous.json")).unwrap();
    let mut s = Session::start(&sb);
    s.expect("no earlier copy: says so", "There is no earlier copy, so the workspace starts empty");
    s.expect("and starts empty", "No agents yet.");
    drop(s.quit("quit", false));

    println!("G. A second window");
    let mut first = Session::start(&sb);
    first.expect("first window opens", "No agents yet.");
    let mut second = Session::start(&sb);
    second.expect("second window is refused and says why", "already open in another window");
    second.expect("and says nothing was started", "Nothing was started here.");
    let end = Instant::now() + Duration::from_secs(5);
    while second.exit.is_none() && Instant::now() < end {
        second.pump(Duration::from_millis(100));
    }
    check("second window exits with an error code", second.exit == Some(1), &format!("{:?}", second.exit));
    drop(first.quit("first window quits", false));

    println!("H. Getting to the menu");
    let mut s = Session::start(&sb);
    s.expect("Lillignode is up", "Ctrl+Q or F12:");
    s.new_agent("echoer");
    s.expect("stand-in agent is running", "STANDIN READY echo");
    s.expect("bar names both menu keys while an agent has the keyboard", "Ctrl+Q or F12: Lillignode menu");
    s.raw(F12);
    s.expect("F12 opens the menu without Ctrl+Q", "MENU");
    s.keys(ESC);
    s.never("neither the menu key nor Esc reached the agent", "STATUS");
    drop(s.quit("quit", true));

    std::fs::write(sb.state.join("config.json"), r#"{ "menu_keys": ["alt+m", "f9"] }"#).unwrap();
    let mut s = Session::start(&sb);
    s.expect(
        "settings file changes the keys, and the bar shows the new ones",
        "Alt+M or F9: Lillignode menu",
    );
    s.raw(CTRL_Q);
    s.never("Ctrl+Q is no longer a menu key", "MENU");
    s.raw(F9);
    s.expect("F9 opens the menu", "MENU");
    s.keys(b"q");
    s.keys(b"y");
    let end = Instant::now() + Duration::from_secs(10);
    while s.exit.is_none() && Instant::now() < end {
        s.pump(Duration::from_millis(100));
    }
    check("quit through the configured key", s.exit == Some(0), &format!("{:?}", s.exit));

    std::fs::write(sb.state.join("config.json"), r#"{ "menu_keys": ["ctrl+c"] }"#).unwrap();
    let mut s = Session::start(&sb);
    s.expect(
        "unusable settings: says so, leading with the fix",
        "To fix your settings file: pick another menu key than \"ctrl+c\"; agents need it.",
    );
    s.expect("unusable settings: falls back to keys that work", "Ctrl+Q or F12: Lillignode menu");
    let tail = s.quit("quit", true);
    check(
        "settings problem is repeated after closing",
        tail.contains("Earlier, on starting: To fix your settings file"),
        "not repeated",
    );
    std::fs::remove_file(sb.state.join("config.json")).unwrap();

    println!("H2. In VS Code's terminal, which keeps Ctrl+Q for itself");
    let mut s = Session::start_with(&sb, &sb.root.join("state-vscode"), &[("TERM_PROGRAM", "vscode")]);
    s.expect("F12 is named first", "F12 or Ctrl+Q opens Lillignode's menu");
    s.raw(F12);
    s.expect("and opens the menu", "MENU Esc back  q quit");
    s.keys(ESC);
    drop(s.quit("quit", false));

    println!("I. Paste");
    let mut s = Session::start(&sb);
    s.expect("stand-in agent is back", "STANDIN READY echo");
    check(
        "Lillignode asks the terminal to mark pastes",
        String::from_utf8_lossy(&s.raw).contains("\x1b[?2004h"),
        "no request seen",
    );
    s.raw(b"\x1b[200~alpha\rbeta\rgamma\x1b[201~");
    s.expect(
        "a marked three-line paste reaches the agent as one paste",
        "PASTE 3 lines: alpha\\rbeta\\rgamma",
    );
    s.expect("and not one Enter press", "STATUS enters=0 pastes=1");
    s.raw(b"delta\repsilon\r");
    s.expect("an unmarked burst with line breaks is also one paste", "PASTE 3 lines: delta\\repsilon\\r");
    s.expect("still no Enter presses", "STATUS enters=0 pastes=2");
    s.keys(b"\r");
    s.expect("a typed Enter is still an Enter", "STATUS enters=1 pastes=2");
    // Not on Windows: its pseudo-terminal splits what is written to it at a
    // control character, so the text before it arrives as typing and the
    // control key on its own. Lillignode cannot tell that from typing there.
    if !cfg!(windows) {
        s.raw(b"\x1b[200~ab\x04cd\rx\x1b[201~");
        s.expect("a Ctrl+D inside a paste does not reach the agent", "PASTE 2 lines: abcd\\rx");
        s.expect("and the user is told it was left out", "1 control character was left out (Ctrl+D).");
        s.raw(b"\x1b[200~ef\x04gh\x1b[201~");
        s.expect("on one line too", "PASTE 1 lines: efgh");
    }

    s.new_agent("plain");
    s.expect("an agent that does not take pastes whole", "STANDIN READY echo-plain");
    s.raw(b"\x1b[200~one\rtwo\x1b[201~");
    s.expect(
        "multi-line paste is held and the user is asked",
        "Paste 2 lines into plain (work) one line at a time? y / n",
    );
    s.never("nothing was sent while asking", "STATUS");
    s.keys(b"n");
    s.expect("n discards it", "Paste discarded. Nothing was sent.");
    s.never("and nothing reached the agent", "STATUS");
    s.raw(b"\x1b[200~one\rtwo\x1b[201~");
    s.keys(b"y");
    s.expect("y sends it line by line, as warned", "STATUS enters=1 pastes=0 typed=two");
    drop(s.quit("quit", true));

    println!("J. A long streamed output, and reading it back");
    let mut s = Session::start(&sb);
    // Until Lillignode has the terminal, Linux takes Ctrl+Q as flow control.
    s.expect("Lillignode is up", "Ctrl+Q or F12:");
    let started = Instant::now();
    s.new_agent("streamer");
    s.expect("3000 lines stream to the end", "STREAM DONE 3000");
    println!("        (start to finish: {} ms)", started.elapsed().as_millis());
    s.expect("the last line is on screen", "stream line 03000 of 03000");
    s.raw(CTRL_Q);
    s.raw(PAGE_UP);
    s.expect("PgUp in the menu shows history", "HISTORY of streamer (work)");
    s.raw(HOME);
    s.expect("Home reaches the first line", "stream line 00001 of 03000");
    // Each page is read only once it has been drawn in full: after PgDn,
    // wait for the first line shown to change and the output to go quiet.
    // A fixed pause read a page early on a busy machine and lost lines that
    // were never missing.
    let stream_lines = |s: &Session| -> Vec<u32> {
        s.rows()
            .iter()
            .filter_map(|row| {
                let at = row.find("stream line ")?;
                row[at + 12..].chars().take(5).collect::<String>().parse().ok()
            })
            .collect()
    };
    s.settle();
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for _ in 0..400 {
        let shown = stream_lines(&s);
        seen.extend(&shown);
        if !s.flat().contains("HISTORY of") {
            break;
        }
        s.writer.write_all(PAGE_DOWN).unwrap();
        s.writer.flush().unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while s.flat().contains("HISTORY of") && stream_lines(&s).first() == shown.first() {
            if Instant::now() > end {
                check(
                    "PgDn moves through history",
                    false,
                    &format!("no change in 10 s from {:?}", shown.first()),
                );
            }
            s.pump(Duration::from_millis(20));
        }
        s.settle();
    }
    let missing: Vec<u32> = (1..=3000).filter(|n| !seen.contains(n)).collect();
    check(
        "every one of the 3000 lines can be read back, none lost",
        missing.is_empty(),
        &format!("{} missing, first few: {:?}", missing.len(), &missing[..missing.len().min(10)]),
    );
    s.expect("paging down to the end returns to the present", "Typing goes to streamer (work)");
    s.raw(b"\x1b[5;2~");
    s.expect("Shift+PgUp scrolls without the menu", "HISTORY of streamer (work)");
    s.keys(b"z");
    s.expect("any other key returns to the present", "Typing goes to streamer (work)");
    s.never("and that key was not typed into the agent", "typed=z");

    s.new_agent("fullscreen");
    s.expect("an agent in a full-screen mode", "FULLSCREEN STANDIN");
    s.raw(CTRL_Q);
    s.raw(PAGE_UP);
    s.expect(
        "says plainly there is no history, and why",
        "is using a full-screen display, which redraws in place and leaves no history",
    );
    s.expect("and what to do instead", "Scroll with the agent's own keys instead.");
    drop(s.quit("quit", true));

    println!("K. Continuing a conversation (stand-in for Claude Code)");
    let state = sb.root.join("state-resume");
    let file = state.join("workspace.json");
    let mut s = Session::start_in(&sb, &state);
    s.keys(b"n");
    s.keys(b"\r");
    s.keys(b"\r");
    s.keys(b"\r");
    s.expect("a new Claude Code agent starts a new conversation", "STANDIN CLAUDE new ");
    s.expect("sidebar uses its name, under a hat, and marks it as in front", ">/\\. Claude Code");
    s.keys(b"remember-this");
    s.keys(b"\r");
    s.expect("something is said in it", "SAID remember-this");
    drop(s.quit("quit", true));
    let id = conversation_id(&file);
    let mut s = Session::start_in(&sb, &state);
    s.expect(
        "restore says it is continuing",
        "Restored 1 agent: Claude Code (work) (continuing its conversation).",
    );
    s.expect("the agent was asked for that same conversation", &format!("STANDIN CLAUDE resumed {id}"));
    s.expect("and the earlier conversation is there", "EARLIER remember-this");
    drop(s.quit("quit", true));

    let state = sb.root.join("state-empty-conversation");
    let file = state.join("workspace.json");
    let mut s = Session::start_in(&sb, &state);
    s.keys(b"n");
    s.keys(b"\r");
    s.keys(b"\r");
    s.keys(b"\r");
    s.expect("a second agent, in which nothing is said", "STANDIN CLAUDE new ");
    drop(s.quit("quit", true));
    let before = conversation_id(&file);
    let mut s = Session::start_in(&sb, &state);
    s.expect("when there is nothing to continue, it says so", "could not continue its earlier conversation");
    s.expect("and says it started a new one", "so it started a new conversation.");
    s.expect("the agent is running with a new conversation", "STANDIN CLAUDE new ");
    // This stand-in does not report status, so the sidebar shows Starting
    // or Unknown; what matters is that it is not shown as ended.
    s.never("not left dead in the sidebar", "Exited");
    s.never("nor as not started", "Not started");
    drop(s.quit("quit", true));
    check(
        "the id that could not be continued was replaced",
        conversation_id(&file) != before,
        "id unchanged",
    );

    println!("L. Status reported by the agent (stand-in for Claude Code)");
    // Each stand-in is named claude, in a folder of its own, and started by
    // its full path, so it is taken for Claude Code.
    let asks = sb.root.join("status-asks");
    let silent = sb.root.join("status-silent");
    for (dir, mode) in [(&asks, "status"), (&silent, "status-silent")] {
        std::fs::create_dir_all(dir).unwrap();
        launcher(dir, &standin, "claude", mode);
    }
    let state = sb.root.join("state-status");
    let mut s = Session::start_in(&sb, &state);
    s.new_agent(&asks.join("claude").display().to_string());
    s.expect("the agent asks the status question", "STATUS QUESTION ");
    // The pseudo-terminal built into Windows
    // answers "what kind of terminal are you?" itself, at once, and drops
    // every reply Lillignode sends into the agent, so Claude Code takes it as
    // "no status reports". Microsoft's newer one (conpty.dll and
    // OpenConsole.exe beside lillignode.exe) passes the answer through.
    let newer_pty = sb.exe.with_file_name("conpty.dll").is_file();
    if cfg!(windows) && !newer_pty {
        // A tripwire: it fails the day the built-in one lets the answer through.
        s.expect(
            "KNOWN FAILURE on Windows: Lillignode's answer does not reach the agent",
            "STATUS QUESTION NOT ANSWERED got=\\u{1b}[?61;",
        );
        s.expect("so the sidebar stays at Starting, not a guess", "work  Starting ");
        s.pump_for(status_deadline());
        s.expect("and is Unknown once the deadline passes, never Starting for ever", "work  Unknown   ");
        s.expect(
            "a notice says why, naming the missing files",
            "Status shows Unknown: conpty.dll and OpenConsole.exe are not beside lillignode.exe.",
        );
        s.expect(
            "and that nothing was changed, ending with what to do",
            r"Nothing was changed. Run scripts\package-windows.ps1 -BesideBuild.",
        );
        s.raw(CTRL_Q);
        s.keys(b"s");
        s.expect("the status report says the question was asked", "Status question: asked");
        s.expect("and that the answer may not have reached it", "The answer may not have reached it");
        s.keys(b"z");
        s.keys(b"i");
    } else {
        s.expect("Lillignode's answer reaches it, before any other answer", "STATUS QUESTION ANSWERED");
    }
    s.expect("a report shows in the sidebar", "work  Idle ");
    s.never("no heading count while nobody needs you", "NEEDS YOU");
    let bells_before = bells(&s);
    let steps: [(&[u8], &str, &str); 8] = [
        (b"w", "working", "work  Working "),
        (b"p", "needs permission: the yellow block", "work  NEEDS YOU   "),
        (b"q", "has a question", "work  NEEDS YOU   "),
        (b"a", "needs signing in", "work  NEEDS YOU   "),
        (b"k", "is waiting with no kind given", "work  NEEDS YOU   "),
        (b"d", "is done", "work  Done "),
        (b"e", "failed", "work  Error   "),
        (b"i", "is idle", "work  Idle "),
    ];
    for (key, what, shown) in steps {
        s.keys(key);
        s.expect(&format!("reported: {what}"), shown);
        // The sidebar says NEEDS YOU only; what it needs is in the status
        // report.
        let kind = match key {
            b"p" => Some("Status: Needs you: permission."),
            b"q" => Some("Status: Needs you: question."),
            b"a" => Some("Status: Needs you: sign in."),
            b"k" => Some("Status: Needs you. "),
            _ => None,
        };
        if let Some(kind) = kind {
            s.raw(CTRL_Q);
            s.keys(b"s");
            s.expect("what it needs is in the status report", kind);
            s.keys(b"z");
        }
        if key == b"p" {
            s.expect("the heading counts it", " Lillignode  1 NEEDS YOU");
            s.expect("and its mark carries the sign for Needs you", ">/\\! Claude Code");
        }
        if key == b"d" {
            s.expect("with the sign for Done", ">/\\= Claude Code");
        }
        if key == b"e" {
            s.expect("with the sign for Errored", ">/\\x Claude Code");
            // The sidebar says Error only; its own words are in the status
            // report, with control characters removed.
            s.raw(CTRL_Q);
            s.keys(b"s");
            s.expect("its own words in full, made safe to show", "Status: Error: rate limited, [2");
            s.keys(b"z");
        }
        if key == b"i" {
            s.expect("with the sign for Idle", ">/\\z Claude Code");
        }
    }
    s.expect_gone("the heading count goes when it stops needing you", "NEEDS YOU");
    // Permission, question, sign in and no kind were one stretch of needing
    // you: one bell, not four.
    check(
        "the bell rang once for one change into Needs you",
        bells(&s) == bells_before + 1,
        &format!("{} bells", bells(&s) - bells_before),
    );
    s.keys(b"t");
    s.expect("a background task's report arrives", "SENT t");
    s.never("and does not change the agent's status", "work  Working ");
    s.keys(b"u");
    s.expect("a state Lillignode does not know is Unknown, not a guess", "work  Unknown   ");
    if !cfg!(windows) || newer_pty {
        s.expect(
            "a notice says so, once",
            "Status shows Unknown: Claude Code sent a status Lillignode does not understand",
        );
    }
    s.keys(b"i");
    s.expect("the next good report replaces it", "work  Idle ");
    s.keys(b"n");
    s.expect("a part Lillignode does not know is passed over, and the rest read", "work  Working ");
    s.keys(b"u");
    s.expect("the unknown state again", "work  Unknown   ");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("menu then s shows the status report", "Status report for Claude Code (work)");
    s.expect("with the question and the answer", "Status question: asked");
    s.expect("with the raw report", "ESC ]7501;state=pondering:app=claude-code");
    s.expect("and why it was not understood", "state \"pondering\" is one Lillignode does not know");
    s.expect("who sent the last report it could read", "Sent by: Claude Code.");
    s.expect("and counts background-task reports", "(1 about its background tasks");
    s.expect(
        "the bar says how to leave",
        "STATUS REPORT  Any key: back to the agent.  Of Claude Code (work).",
    );
    s.keys(b"z");
    s.expect("any key goes back to the agent", "Typing goes to Claude Code (work)");
    s.keys(b"c");
    s.expect("a withdrawn status is Unknown", "work  Unknown   ");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("and the status report says why", "Status: Unknown: withdrawn.");
    s.keys(b"z");
    s.keys(b"l");
    s.expect("a long report", "SENT l");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("is shown cut, saying how much more there was", "more characters)");
    s.keys(b"z");
    // Leave the first one waiting for permission, then put another in front.
    s.keys(b"p");
    s.expect("the first agent needs you again", "work  NEEDS YOU   ");
    let bells_before = bells(&s);

    s.new_agent(&silent.join("claude").display().to_string());
    s.expect("an agent that never asks", "STANDIN READY status-silent");
    s.expect("shows Starting at first", "work  Starting ");
    s.expect(
        "the notice names the agent behind that needs you, and how to reach it",
        "Claude Code (work) needs you: permission. Ctrl+Q then Tab to go to it.",
    );
    s.expect("the heading still counts it", " Lillignode  1 NEEDS YOU");
    check("no new bell: it did not change", bells(&s) == bells_before, "a bell rang");
    s.pump_for(status_deadline());
    s.expect("and Unknown once the deadline passes", "work  Unknown   ");
    s.expect(
        "an ordinary notice cannot push the needs-you notice out",
        "Claude Code (work) needs you: permission. Ctrl+Q then Tab to go to it.",
    );
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect(
        "the status report says why",
        "Claude Code did not ask Lillignode's terminal for status reports",
    );
    s.expect("and that nothing was received", "No report has been received.");
    s.keys(b"z");

    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.keys(ESC);
    s.expect("Tab goes to it", "Typing goes to Claude Code (work)");
    s.never("in front, it is not named in the notice", "needs you: permission. Ctrl+Q");
    s.keys(b"i");
    s.expect_gone("it clears when the agent says so", "NEEDS YOU");

    // Quit asks only because of the one that may be mid-request, and names
    // only that one.
    s.raw(CTRL_Q);
    s.keys(b"q");
    s.expect("quit asks while an agent's status is not known", "Quit and stop 2 running agents? y / n");
    s.expect(
        "and names only that agent",
        "Not finished or not known: Claude Code 2 (work) (Unknown: no report).",
    );
    s.never("not the idle one", "Claude Code (work) (");
    s.keys(b"n");
    drop(s.quit("quit", true));

    println!("L2. Quit without asking when every agent is finished");
    let mut s = Session::start_in(&sb, &sb.root.join("state-status-quit"));
    s.new_agent(&asks.join("claude").display().to_string());
    s.expect("the agent asks the status question", "STATUS QUESTION ");
    s.keys(b"d");
    s.expect("it is done", "work  Done ");
    s.raw(CTRL_Q);
    s.keys(b"q");
    let tail = s.quit_already_asked("quit with every agent Done");
    check("and still stops it", tail.contains("Lillignode closed and stopped 1 agent"), "not stopped");

    println!("L3. Status switched off where Lillignode was started");
    let mut s =
        Session::start_with(&sb, &sb.root.join("state-status-off"), &[(lillignode::status::OFF_SWITCH, "1")]);
    s.new_agent(&silent.join("claude").display().to_string());
    s.pump_for(status_deadline());
    s.expect("the agent shows Unknown", "work  Unknown   ");
    s.expect(
        "the notice names the setting, not a fault",
        "Status shows Unknown: CLAUDE_CODE_DISABLE_TERMINAL_TITLE is set here.",
    );
    s.expect("and leaves it alone", "Lillignode leaves that setting alone.");
    drop(s.quit("quit", true));

    paste_after_status(&sb, &standin);
    cost(&sb);
    side_by_side(&sb, &standin);
    codex(&sb, &standin);
    any_program(&sb, &standin);
    start_options(&sb, &standin);

    let _ = std::fs::remove_dir_all(&sb.root);
    println!("\nAll checks passed.");
}
