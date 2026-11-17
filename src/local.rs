//! Agents as child processes of this window, each in its own pseudo-terminal.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::agent::{AgentBackend, AgentEvent, AgentId, AgentSpec, Exit, Size, StartError};

struct Running {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

pub struct LocalAgents<E> {
    next_id: u32,
    running: HashMap<AgentId, Running>,
    events: Sender<E>,
}

impl<E: From<AgentEvent> + Send + 'static> LocalAgents<E> {
    pub fn new(events: Sender<E>) -> Self {
        Self { next_id: 1, running: HashMap::new(), events }
    }
}

fn pty_size(size: Size) -> PtySize {
    PtySize { rows: size.rows.max(1), cols: size.cols.max(1), pixel_width: 0, pixel_height: 0 }
}

/// Find the file the operating system should run for `command`.
///
/// On Windows this must not be left to the pseudo-terminal library: tools
/// installed through npm (Claude Code among them) put an extensionless Unix
/// shell script next to the real `.cmd` launcher, and a lookup that tries the
/// bare name first picks the script and fails with "not a valid Win32
/// application". So extensions from PATHEXT are tried first, and a bare name
/// is never accepted on Windows.
pub fn resolve_command(command: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    let candidates = |base: &Path| -> Option<PathBuf> {
        if cfg!(windows) && base.extension().is_some() && base.is_file() {
            return Some(base.to_path_buf());
        }
        exts.iter()
            .map(|ext| {
                let mut name = base.as_os_str().to_owned();
                name.push(ext);
                PathBuf::from(name)
            })
            .find(|p| p.is_file())
    };

    let given = Path::new(command);
    if given.components().count() > 1 {
        return candidates(given);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| candidates(&dir.join(command)))
}

fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// What the command interpreter reads as an instruction of its own rather
/// than as text: with one of these in it, an argument could start a second
/// program nobody meant. Quoting cannot be made safe for every text, so an
/// argument that holds one is refused.
const NOT_FOR_A_LAUNCHER: &[char] = &['&', '|', '<', '>', '^', '%', '"', '\n', '\r'];

/// Why a batch launcher cannot be started with these arguments, or None.
fn launcher_refusal(program: &Path, args: &[String]) -> Option<String> {
    let name = |c: char| match c {
        '\n' | '\r' => "a line break".to_string(),
        c => format!("\"{c}\""),
    };
    if let Some(c) = program.to_string_lossy().chars().find(|c| NOT_FOR_A_LAUNCHER.contains(c)) {
        return Some(format!(
            "the path of its launcher holds {}, which Lillignode will not pass to the command \
             interpreter. Nothing was run",
            name(c)
        ));
    }
    let c = args.iter().flat_map(|a| a.chars()).find(|c| NOT_FOR_A_LAUNCHER.contains(c))?;
    Some(format!(
        "its arguments hold {}, which Lillignode will not pass to a .cmd or .bat launcher. Nothing was run",
        name(c)
    ))
}

/// Build the process to launch. Windows batch launchers (`.cmd`, `.bat`)
/// cannot be started directly and go through the command interpreter.
fn command_for(program: &Path, args: &[String]) -> CommandBuilder {
    let mut cmd = if cfg!(windows) && is_batch(program) {
        let mut c = CommandBuilder::new("cmd.exe");
        c.args(["/d", "/c"]);
        c.arg(program);
        c
    } else {
        CommandBuilder::new(program)
    };
    cmd.args(args);
    cmd
}

impl<E: From<AgentEvent> + Send + 'static> AgentBackend for LocalAgents<E> {
    fn start(&mut self, spec: &AgentSpec, size: Size) -> Result<AgentId, StartError> {
        if !spec.folder.is_dir() {
            return Err(StartError::FolderMissing(spec.folder.clone()));
        }
        let Some(program) = resolve_command(&spec.command) else {
            return Err(StartError::CommandNotFound(spec.command.clone()));
        };
        if cfg!(windows) && is_batch(&program) {
            if let Some(why) = launcher_refusal(&program, &spec.args) {
                return Err(StartError::Refused(why));
            }
        }

        let failed = |e: &dyn std::fmt::Display| StartError::Failed(e.to_string());
        let pair = native_pty_system().openpty(pty_size(size)).map_err(|e| failed(&e))?;

        let mut cmd = command_for(&program, &spec.args);
        cmd.cwd(&spec.folder);
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| failed(&e))?;
        // The agent holds its own end now; keeping ours open would stop the
        // reader below from ever seeing end-of-output.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| failed(&e))?;
        let writer = pair.master.take_writer().map_err(|e| failed(&e))?;
        let killer = child.clone_killer();

        let id = AgentId(self.next_id);
        self.next_id += 1;

        let out = self.events.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.send(AgentEvent::Output(id, buf[..n].to_vec()).into()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let done = self.events.clone();
        thread::spawn(move || {
            // If waiting itself fails we cannot know the exit code. Report a
            // non-zero one so it is never shown as a clean exit.
            let code = child.wait().map(|s| s.exit_code()).unwrap_or(u32::MAX);
            let _ = done.send(AgentEvent::Exited(id, Exit { code }).into());
        });

        self.running.insert(id, Running { master: pair.master, writer, killer });
        Ok(id)
    }

    fn send(&mut self, id: AgentId, bytes: &[u8]) -> std::io::Result<()> {
        match self.running.get_mut(&id) {
            Some(r) => {
                r.writer.write_all(bytes)?;
                r.writer.flush()
            }
            None => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "agent is not running")),
        }
    }

    fn resize(&mut self, id: AgentId, size: Size) {
        if let Some(r) = self.running.get(&id) {
            let _ = r.master.resize(pty_size(size));
        }
    }

    fn close(&mut self, id: AgentId) {
        if let Some(mut r) = self.running.remove(&id) {
            // Killing an agent that already exited fails, and that is fine.
            let _ = r.killer.kill();
        }
    }
}

impl<E> Drop for LocalAgents<E> {
    fn drop(&mut self) {
        for (_, mut r) in self.running.drain() {
            let _ = r.killer.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launcher_is_refused_an_argument_the_interpreter_would_act_on() {
        let launcher = Path::new("C:/tools/claude.cmd");
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            launcher_refusal(launcher, &args(&["--resume", "0b0e7f0a-0000-4000-8000-000000000001"])),
            None
        );
        assert_eq!(launcher_refusal(launcher, &args(&["a b", "c-d", "e.f", "g/h"])), None);
        for bad in ["a&b", "a|b", "<", ">", "^", "%PATH%", "\"", "a\nb", "a\rb"] {
            let why = launcher_refusal(launcher, &args(&["ok", bad])).unwrap_or_default();
            assert!(why.starts_with("its arguments hold "), "{bad:?}: {why}");
            assert!(why.ends_with(".cmd or .bat launcher. Nothing was run"), "{bad:?}: {why}");
        }
        assert_eq!(
            launcher_refusal(launcher, &args(&["x&y"])).unwrap(),
            "its arguments hold \"&\", which Lillignode will not pass to a .cmd or .bat launcher. Nothing was run"
        );
        assert!(launcher_refusal(launcher, &args(&["a\nb"])).unwrap().contains("a line break"));
        // The launcher's own path goes through the interpreter too.
        let why = launcher_refusal(Path::new("C:/a&b/claude.cmd"), &[]).unwrap();
        assert!(why.starts_with("the path of its launcher holds \"&\""), "{why}");
        assert!(is_batch(Path::new("x.CMD")) && is_batch(Path::new("x.bat")));
        assert!(!is_batch(Path::new("x.exe")) && !is_batch(Path::new("cmd")));
    }
}
