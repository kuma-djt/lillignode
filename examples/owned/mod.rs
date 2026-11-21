//! Development tool, not part of the product: a terminal the checks own.
//!
//! Starts the real `lillignode` program in a pseudo-terminal, the way a
//! person at a terminal window would, presses keys, and reads the screen.
//! No window exists. Shared by the checks beside it (`e2e.rs`,
//! `page_check.rs`).
//!
//! It never runs a real agent and never touches real settings:
//! - every agent is the stand-in program (`standin.rs`) or the system
//!   shell, reached through launchers in a throwaway folder that is put
//!   first on PATH;
//! - the saved workspace and settings live in the same throwaway folder;
//! - CLAUDE_CONFIG_DIR points there too, so even a mistake could not reach
//!   a real Claude Code configuration.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use lillignode::terminal::Terminal;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

pub const COLS: u16 = 150;
pub const ROWS: u16 = 40;
pub const CTRL_Q: &[u8] = &[0x11];
pub const F12: &[u8] = b"\x1b[24~";
pub const F9: &[u8] = b"\x1b[20~";
pub const PAGE_UP: &[u8] = b"\x1b[5~";
pub const PAGE_DOWN: &[u8] = b"\x1b[6~";
pub const HOME: &[u8] = b"\x1b[H";
pub const ESC: &[u8] = b"\x1b";

pub enum Ev {
    Out(Vec<u8>),
    Exit(u32),
}

/// Where everything this run creates lives.
pub struct Sandbox {
    pub exe: PathBuf,
    pub root: PathBuf,
    pub state: PathBuf,
    pub work: PathBuf,
    pub launchers: PathBuf,
}

pub struct Session {
    pub rx: Receiver<Ev>,
    pub term: Terminal,
    pub writer: Box<dyn Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub raw: Vec<u8>,
    pub exit: Option<u32>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
}

impl Session {
    pub fn start(sb: &Sandbox) -> Session {
        Session::start_in(sb, &sb.state)
    }

    pub fn start_in(sb: &Sandbox, state: &Path) -> Session {
        Session::start_with(sb, state, &[])
    }

    /// Starts Lillignode without Claude Code's status off switch or
    /// `TERM_PROGRAM`, whatever this run inherited, then with `env` on top.
    pub fn start_with(sb: &Sandbox, state: &Path, env: &[(&str, &str)]) -> Session {
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
        let killer = child.clone_killer();
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
            killer,
        }
    }

    pub fn pump(&mut self, wait: Duration) -> bool {
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

    pub fn pump_for(&mut self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.pump(Duration::from_millis(20));
        }
    }

    /// Takes output until none has come for 100 ms (at most 10 s), so a
    /// screen is read whole and not halfway through being drawn.
    pub fn settle(&mut self) {
        let end = Instant::now() + Duration::from_secs(10);
        while self.pump(Duration::from_millis(100)) && Instant::now() < end {}
    }

    pub fn rows(&self) -> Vec<String> {
        let cols = self.term.screen().size().1.get();
        self.term.screen().rows(0, cols).collect()
    }

    pub fn screen(&self) -> String {
        self.rows().join("\n")
    }

    /// The screen with line breaks removed, so text that wraps across the two
    /// notice rows can still be matched.
    pub fn flat(&self) -> String {
        let cols = self.term.screen().size().1.get() as usize;
        self.rows().iter().map(|r| format!("{r:<cols$}")).collect()
    }

    /// True when the screen shows `needle`: across a line's end as the screen
    /// lays it out, or across a break Lillignode made at a space when it
    /// wrapped a notice.
    pub fn shows(&self, needle: &str) -> bool {
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

    pub fn fail(&self, step: &str, detail: &str) -> ! {
        println!("  FAIL  {step}\n        {detail}\n--- screen ---\n{}\n--------------", self.screen());
        std::process::exit(1);
    }

    pub fn expect(&mut self, step: &str, needle: &str) {
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

    pub fn expect_gone(&mut self, step: &str, needle: &str) {
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
    pub fn never(&mut self, step: &str, needle: &str) {
        self.pump_for(Duration::from_millis(700));
        if self.shows(needle) {
            self.fail(step, &format!("must not show: {needle}"));
        }
        println!("  ok    {step}");
    }

    /// Type like a person: one key at a time, with a gap between keys.
    pub fn keys(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.writer.write_all(&[*b]).unwrap();
            self.writer.flush().unwrap();
            self.pump_for(Duration::from_millis(40));
        }
        self.pump_for(Duration::from_millis(200));
    }

    /// Send bytes in one go: a function key, or what a terminal sends for a paste.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
        self.pump_for(Duration::from_millis(250));
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        self.term.resize(rows, cols);
    }

    /// Start an agent through the menu in the default folder.
    pub fn new_agent(&mut self, command: &str) {
        self.raw(CTRL_Q);
        self.keys(b"n");
        self.keys(command.as_bytes());
        self.keys(b"\r");
        self.keys(b"\r");
        self.keys(b"\r");
    }

    /// Quit through the menu, answering the confirmation if one is asked.
    pub fn quit(mut self, step: &str, expect_confirm: bool) -> String {
        self.raw(CTRL_Q);
        self.keys(b"q");
        if expect_confirm {
            self.expect(&format!("{step}: asks before stopping agents"), "? y / n");
            self.keys(b"y");
        }
        self.wait_for_exit(step)
    }

    /// q has been pressed: Lillignode must close without asking anything.
    pub fn quit_already_asked(self, step: &str) -> String {
        if self.exit.is_none() && self.flat().contains("? y / n") {
            self.fail(step, "asked before quitting");
        }
        self.wait_for_exit(step)
    }

    pub fn wait_for_exit(mut self, step: &str) -> String {
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

pub fn check(step: &str, ok: bool, detail: &str) {
    if ok {
        println!("  ok    {step}");
    } else {
        println!("  FAIL  {step}\n        {detail}");
        std::process::exit(1);
    }
}

pub fn json_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

/// A launcher called `name` that runs the stand-in with `args`.
pub fn launcher(dir: &Path, standin: &Path, name: &str, args: &str) {
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
pub fn bells(s: &Session) -> usize {
    s.raw.iter().filter(|&&b| b == 0x07).count()
}

/// Stop the program at once, without asking it: what a crash or the
/// task manager does. Nothing is saved and nothing is said.
pub fn kill(s: &mut Session) {
    let _ = s.killer.kill();
    let end = Instant::now() + Duration::from_secs(10);
    while s.exit.is_none() && Instant::now() < end {
        s.pump(Duration::from_millis(50));
    }
}

/// The program under test and the stand-in, found beside this check, and a
/// throwaway folder for everything a run creates.
pub fn sandbox(tag: &str) -> (Sandbox, PathBuf) {
    let here = std::env::current_exe().unwrap();
    let examples = here.parent().unwrap();
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let exe = examples.parent().unwrap().join(format!("lillignode{ext}"));
    let standin = examples.join(format!("standin{ext}"));
    assert!(exe.is_file(), "build lillignode first: {}", exe.display());
    assert!(standin.is_file(), "build the stand-in first: {}", standin.display());
    let root: PathBuf = std::env::temp_dir().join(format!("lillignode-{tag}-{}", std::process::id()));
    let sb = Sandbox {
        exe,
        state: root.join("state"),
        work: root.join("work"),
        launchers: root.join("launchers"),
        root,
    };
    std::fs::create_dir_all(&sb.work).unwrap();
    std::fs::create_dir_all(&sb.launchers).unwrap();
    std::fs::create_dir_all(&sb.state).unwrap();
    (sb, standin)
}
