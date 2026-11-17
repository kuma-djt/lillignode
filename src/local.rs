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

/// What a launcher's own path may not hold. The path reaches the command
/// interpreter in quotes (`command_for`), which keep `& | < > ^` as text;
/// quotes do not keep `%` or `"`.
const NOT_IN_A_LAUNCHER_PATH: &[char] = &['%', '"'];

/// The variable the launcher's quoted path is handed over in.
const LAUNCHER_VARIABLE: &str = "LILLIGNODE_LAUNCHER";

/// What in a batch launcher's path or arguments the command interpreter
/// would act on: the first such character, and where it is.
enum LauncherProblem {
    Path(char),
    Arg(char),
}

fn launcher_problem(program: &Path, args: &[String]) -> Option<LauncherProblem> {
    if let Some(c) = program.to_string_lossy().chars().find(|c| NOT_IN_A_LAUNCHER_PATH.contains(c)) {
        return Some(LauncherProblem::Path(c));
    }
    args.iter().flat_map(|a| a.chars()).find(|c| NOT_FOR_A_LAUNCHER.contains(c)).map(LauncherProblem::Arg)
}

fn char_name(c: char) -> String {
    match c {
        '\n' | '\r' => "a line break".to_string(),
        c => format!("\"{c}\""),
    }
}

/// Why a batch launcher cannot be started with these arguments, or None.
fn launcher_refusal(program: &Path, args: &[String]) -> Option<String> {
    Some(match launcher_problem(program, args)? {
        LauncherProblem::Path(c) => format!(
            "the path of its launcher holds {}, which Lillignode will not pass to the command \
             interpreter. Nothing was run. Start the program itself rather than its launcher, or move the \
             launcher to a folder whose path has no such character",
            char_name(c)
        ),
        LauncherProblem::Arg(c) => format!(
            "its arguments hold {}, which Lillignode will not pass to a .cmd or .bat launcher. {}",
            char_name(c),
            crate::agent::FIX_IN_THE_FILE
        ),
    })
}

/// As `launcher_refusal`, worded for options just typed at the new-agent
/// form rather than saved in the workspace file. What to do comes first, so
/// a narrow bar keeps it.
fn launcher_refusal_typed(program: &Path, args: &[String]) -> Option<String> {
    Some(match launcher_problem(program, args)? {
        LauncherProblem::Path(c) => format!(
            "Start the program itself, not its launcher: the launcher's path holds {}, which the \
             command interpreter would act on",
            char_name(c)
        ),
        LauncherProblem::Arg(c) => format!(
            "Take out the {}: this program is a launcher, and the command interpreter would act on it. \
             Or start the program itself",
            char_name(c)
        ),
    })
}

/// Build the process to launch. Windows batch launchers (`.cmd`, `.bat`)
/// cannot be started directly and go through the command interpreter.
///
/// The launcher's path reaches the interpreter in quotes, so a folder such
/// as `C:\Users\R&D` works. The pseudo-terminal library cannot put quotes
/// on the command line as the interpreter needs them, so the quoted path is
/// handed over in a variable the interpreter expands before it reads the
/// line: `cmd /d /c %LILLIGNODE_LAUNCHER% args`.
fn command_for(program: &Path, args: &[String]) -> CommandBuilder {
    let mut cmd = if cfg!(windows) && is_batch(program) {
        let mut c = CommandBuilder::new("cmd.exe");
        c.args(["/d", "/c", &format!("%{LAUNCHER_VARIABLE}%")]);
        c.env(LAUNCHER_VARIABLE, format!("\"{}\"", program.display()));
        c
    } else {
        CommandBuilder::new(program)
    };
    cmd.args(args);
    cmd
}

impl<E: From<AgentEvent> + Send + 'static> AgentBackend for LocalAgents<E> {
    fn would_refuse(&self, command: &str, args: &[String]) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        // A program that cannot be found is said when it fails to start.
        let program = resolve_command(command)?;
        if !is_batch(&program) {
            return None;
        }
        launcher_refusal_typed(&program, args)
    }

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
            assert!(why.contains(".cmd or .bat launcher. Nothing was run."), "{bad:?}: {why}");
        }
        assert_eq!(
            launcher_refusal(launcher, &args(&["x&y"])).unwrap(),
            format!(
                "its arguments hold \"&\", which Lillignode will not pass to a .cmd or .bat launcher. {}",
                crate::agent::FIX_IN_THE_FILE
            )
        );
        assert!(launcher_refusal(launcher, &args(&["a\nb"])).unwrap().contains("a line break"));
        // The launcher's own path goes through the interpreter in quotes,
        // which keep & | < > ^ as text but not % or ".
        for fine in
            ["C:/Users/R&D/npm/claude.cmd", "C:/a|b<c>d^e/claude.cmd", "C:/Program Files/x/claude.cmd"]
        {
            assert_eq!(launcher_refusal(Path::new(fine), &[]), None, "{fine}");
        }
        let why = launcher_refusal(Path::new("C:/100%/claude.cmd"), &[]).unwrap();
        assert!(why.starts_with("the path of its launcher holds \"%\""), "{why}");
        assert!(is_batch(Path::new("x.CMD")) && is_batch(Path::new("x.bat")));
        assert!(!is_batch(Path::new("x.exe")) && !is_batch(Path::new("cmd")));
    }

    #[cfg(windows)]
    #[test]
    fn a_launcher_path_reaches_the_interpreter_in_quotes_through_a_variable() {
        let cmd = command_for(Path::new(r"C:\Users\R&D\claude.cmd"), &["--x".to_string()]);
        let argv: Vec<String> = cmd.get_argv().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(argv, ["cmd.exe", "/d", "/c", "%LILLIGNODE_LAUNCHER%", "--x"]);
        assert_eq!(cmd.get_env(LAUNCHER_VARIABLE).unwrap(), r#""C:\Users\R&D\claude.cmd""#);
        // A program that is not a launcher is started as itself.
        let cmd = command_for(Path::new(r"C:\tools\codex.exe"), &[]);
        assert_eq!(cmd.get_argv().len(), 1);
        assert!(cmd.get_env(LAUNCHER_VARIABLE).is_none());
    }

    #[test]
    fn options_typed_for_a_launcher_are_refused_in_words_for_the_form() {
        let launcher = Path::new("C:/npm/codex.cmd");
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(launcher_refusal_typed(launcher, &args(&["-a", "on-request"])), None);
        assert_eq!(
            launcher_refusal_typed(launcher, &args(&["a&b"])).unwrap(),
            "Take out the \"&\": this program is a launcher, and the command interpreter would act on it. \
             Or start the program itself"
        );
        assert_eq!(launcher_refusal_typed(Path::new("C:/R&D/codex.cmd"), &[]), None);
        let why = launcher_refusal_typed(Path::new("C:/100%/codex.cmd"), &[]).unwrap();
        assert!(
            why.starts_with("Start the program itself, not its launcher: the launcher's path holds \"%\"")
        );
        // Nothing about the workspace file: the options were just typed.
        let why = launcher_refusal_typed(launcher, &args(&["%x%"])).unwrap();
        assert!(!why.contains("workspace.json"), "{why}");
    }
}
