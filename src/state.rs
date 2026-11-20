//! The saved workspace: one plain JSON file on local disk.
//!
//! What is saved: which agents are open, in which folders, and which one was
//! in front. What is never saved: anything an agent printed, and any key or
//! token. Nothing here leaves the machine.
//!
//! Failure rules:
//! - Saving never leaves a half-written file. The new file is written beside
//!   the old one and swapped in; the old one is kept as the previous copy.
//! - A file that cannot be read is never deleted or overwritten. It is moved
//!   aside under a new name and the user is told where it is.
//! - Loading always produces a workspace (possibly empty) plus a notice saying
//!   exactly which of the cases below happened.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Bumped only when an older Lillignode could misread the file.
pub const VERSION: u32 = 1;

const FILE: &str = "workspace.json";
const PREVIOUS: &str = "workspace.previous.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAgent {
    /// What the user sees in the sidebar, for example "Claude Code".
    pub name: String,
    pub command: String,
    /// What kind of agent this is, for example "claude-code". Absent in a
    /// workspace saved before kinds were recorded: the kind is then worked
    /// out from the command and saved from then on. A word this Lillignode
    /// does not know is kept as it was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub folder: PathBuf,
    /// The id of the agent's conversation, for agents Lillignode knows how to
    /// resume. It identifies a conversation the agent itself stores; it is
    /// not a key and grants nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// Lines this Lillignode does not know, which a newer one may have
    /// written. Kept as they were and saved back, so they are not lost.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    pub version: u32,
    pub agents: Vec<SavedAgent>,
    /// Index into `agents` of the one in front. Absent when there are none.
    #[serde(default)]
    pub focused: Option<usize>,
    /// Lines this Lillignode does not know, kept and saved back.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Default for SavedWorkspace {
    fn default() -> Self {
        Self { version: VERSION, agents: Vec::new(), focused: None, other: Map::new() }
    }
}

/// What happened when loading. Each case reads differently to the user.
#[derive(Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    /// No saved workspace: first run, or the user closed everything.
    Nothing,
    /// The saved workspace was read.
    Loaded,
    /// The saved workspace was unreadable; the previous copy was used.
    UsedPrevious { unreadable_kept_as: PathBuf, previous_saved: Option<SystemTime>, why: String },
    /// The saved workspace was unreadable and there was no usable previous copy.
    Lost { unreadable_kept_as: PathBuf, why: String },
}

pub struct Loaded {
    pub workspace: SavedWorkspace,
    pub outcome: LoadOutcome,
}

/// Where the workspace is saved. `LILLIGNODE_STATE_DIR` overrides the default.
pub fn default_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("LILLIGNODE_STATE_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        var("APPDATA").map(|d| d.join("Lillignode")).ok_or_else(|| "APPDATA is not set".to_string())
    } else if let Some(dir) = var("XDG_STATE_HOME") {
        Ok(dir.join("lillignode"))
    } else {
        var("HOME")
            .map(|d| d.join(".local").join("state").join("lillignode"))
            .ok_or_else(|| "HOME is not set".to_string())
    }
}

/// Why the workspace could not be claimed for this window.
#[derive(Debug)]
pub enum ClaimError {
    /// Another Lillignode window already has it.
    InUse,
    Other(String),
}

/// Claim the saved workspace for this window, for as long as the returned
/// file is kept. Two windows sharing one workspace would each reopen the same
/// agents in the same folders and then overwrite each other's saves, so the
/// second one is refused. The operating system releases the claim when the
/// process ends, however it ends, so a crash cannot leave it stuck.
pub fn claim(dir: &Path) -> Result<fs::File, ClaimError> {
    fs::create_dir_all(dir)
        .map_err(|e| ClaimError::Other(format!("could not create {}: {e}", dir.display())))?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("in-use.lock"))
        .map_err(|e| ClaimError::Other(e.to_string()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(ClaimError::InUse),
        Err(fs::TryLockError::Error(e)) => Err(ClaimError::Other(e.to_string())),
    }
}

pub fn file_in(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The largest saved workspace Lillignode reads. One it wrote itself is a
/// few hundred bytes an agent.
pub const LARGEST_FILE: u64 = 1_000_000;
/// The most agents a saved workspace may list. Each would be started at once.
pub const MOST_AGENTS: usize = 64;

fn read(path: &Path) -> Result<SavedWorkspace, String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > LARGEST_FILE {
        return Err(format!(
            "it is {size} bytes, larger than the {LARGEST_FILE} Lillignode reads, so none of it was started"
        ));
    }
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let ws: SavedWorkspace = serde_json::from_str(&text).map_err(|e| format!("it is not valid ({e})"))?;
    if ws.agents.len() > MOST_AGENTS {
        return Err(format!(
            "it lists {} agents, more than the {MOST_AGENTS} Lillignode starts at once, so none of them was \
             started",
            ws.agents.len()
        ));
    }
    if ws.version > VERSION {
        return Err(format!(
            "it was saved by a newer Lillignode (format {}, this one reads up to {VERSION})",
            ws.version
        ));
    }
    Ok(ws)
}

/// Move an unreadable file out of the way without losing it.
fn set_aside(path: &Path) -> PathBuf {
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let kept = path.with_file_name(format!("workspace.unreadable-{stamp}.json"));
    // If the move fails the file stays where it is, which is also not lost.
    // The next save will then keep it as the previous copy.
    match fs::rename(path, &kept) {
        Ok(()) => kept,
        Err(_) => path.to_path_buf(),
    }
}

pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(FILE);
    let previous = dir.join(PREVIOUS);
    let current_exists = path.exists();

    if current_exists {
        match read(&path) {
            Ok(workspace) => return Loaded { workspace, outcome: LoadOutcome::Loaded },
            Err(why) => {
                let unreadable_kept_as = set_aside(&path);
                return match read(&previous) {
                    Ok(workspace) => {
                        let previous_saved = fs::metadata(&previous).and_then(|m| m.modified()).ok();
                        Loaded {
                            workspace,
                            outcome: LoadOutcome::UsedPrevious { unreadable_kept_as, previous_saved, why },
                        }
                    }
                    Err(_) => Loaded {
                        workspace: SavedWorkspace::default(),
                        outcome: LoadOutcome::Lost { unreadable_kept_as, why },
                    },
                };
            }
        }
    }
    // No current file. A previous copy on its own means a save was interrupted
    // between its two steps; the previous copy is the newest complete one.
    match read(&previous) {
        Ok(workspace) => Loaded { workspace, outcome: LoadOutcome::Loaded },
        Err(_) => Loaded { workspace: SavedWorkspace::default(), outcome: LoadOutcome::Nothing },
    }
}

pub fn save(dir: &Path, workspace: &SavedWorkspace) -> Result<(), String> {
    let fail = |what: &str, e: std::io::Error| format!("{what}: {e}");
    fs::create_dir_all(dir).map_err(|e| fail("could not create the folder", e))?;
    let path = dir.join(FILE);
    let previous = dir.join(PREVIOUS);
    let fresh = dir.join("workspace.writing.json");

    let json = serde_json::to_string_pretty(workspace).map_err(|e| e.to_string())?;
    {
        let mut f = fs::File::create(&fresh).map_err(|e| fail("could not write", e))?;
        f.write_all(json.as_bytes()).map_err(|e| fail("could not write", e))?;
        f.write_all(b"\n").map_err(|e| fail("could not write", e))?;
        f.sync_all().map_err(|e| fail("could not write", e))?;
    }
    // Keep the last good file as the previous copy, then swap the new one in.
    // If we stop between the two steps, `load` falls back to the previous copy.
    if path.exists() {
        fs::rename(&path, &previous).map_err(|e| fail("could not keep the previous copy", e))?;
    }
    fs::rename(&fresh, &path).map_err(|e| fail("could not replace the saved workspace", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn one(name: &str) -> SavedWorkspace {
        SavedWorkspace {
            version: VERSION,
            agents: vec![SavedAgent {
                name: name.into(),
                command: "claude".into(),
                kind: Some("claude-code".into()),
                args: vec![],
                folder: PathBuf::from("C:/work"),
                conversation: Some("0b0e7f0a-0000-4000-8000-000000000001".into()),
                other: Default::default(),
            }],
            focused: Some(0),
            other: Default::default(),
        }
    }

    #[test]
    fn a_workspace_saved_before_kinds_were_recorded_still_loads() {
        let dir = temp_dir("no-kind");
        let text = r#"{"version":1,"agents":[{"name":"Claude Code","command":"claude","args":[],
            "folder":"C:/work","conversation":"0b0e7f0a-0000-4000-8000-000000000001"}],"focused":0}"#;
        fs::write(file_in(&dir), text).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.outcome, LoadOutcome::Loaded);
        assert_eq!(loaded.workspace.agents[0].kind, None);
        // And an agent with no kind is written without one, as before.
        save(&dir, &loaded.workspace).unwrap();
        assert!(!fs::read_to_string(file_in(&dir)).unwrap().contains("kind"));
    }

    #[test]
    fn a_kind_is_saved_and_read_back_as_written_even_one_not_known_here() {
        let dir = temp_dir("kind-kept");
        let mut ws = one("Claude Code");
        ws.agents[0].kind = Some("from-a-newer-lillignode".into());
        save(&dir, &ws).unwrap();
        assert_eq!(load(&dir).workspace, ws);
    }

    #[test]
    fn lines_this_lillignode_does_not_know_are_kept_and_saved_back() {
        let dir = temp_dir("unknown-lines");
        let text = r#"{"version":1,"team":{"name":"web"},"agents":[{"name":"Claude Code","command":"claude",
            "kind":"claude-code","args":[],"folder":"C:/work","colour":"red","limits":[1,2]}],"focused":0}"#;
        fs::write(file_in(&dir), text).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.outcome, LoadOutcome::Loaded);
        assert_eq!(loaded.workspace.other["team"], serde_json::json!({"name": "web"}));
        assert_eq!(loaded.workspace.agents[0].other["colour"], "red");
        save(&dir, &loaded.workspace).unwrap();
        let again = load(&dir).workspace;
        assert_eq!(again, loaded.workspace, "nothing lost on the way round");
        let written = fs::read_to_string(file_in(&dir)).unwrap();
        assert!(written.contains("\"limits\"") && written.contains("\"team\""), "{written}");
    }

    #[test]
    fn first_run_is_nothing_not_an_error() {
        let dir = temp_dir("first");
        let l = load(&dir.join("does-not-exist"));
        assert_eq!(l.outcome, LoadOutcome::Nothing);
        assert!(l.workspace.agents.is_empty());
    }

    #[test]
    fn saves_and_loads_back_the_same() {
        let dir = temp_dir("roundtrip");
        save(&dir, &one("a")).unwrap();
        let l = load(&dir);
        assert_eq!(l.outcome, LoadOutcome::Loaded);
        assert_eq!(l.workspace, one("a"));
    }

    #[test]
    fn corrupt_file_falls_back_to_previous_and_is_kept() {
        let dir = temp_dir("corrupt");
        save(&dir, &one("older")).unwrap();
        save(&dir, &one("newer")).unwrap();
        fs::write(dir.join(FILE), "{ this is not json").unwrap();

        let l = load(&dir);
        assert_eq!(l.workspace, one("older"));
        let LoadOutcome::UsedPrevious { unreadable_kept_as, .. } = l.outcome else {
            panic!("expected UsedPrevious, got {:?}", l.outcome)
        };
        assert_eq!(fs::read_to_string(unreadable_kept_as).unwrap(), "{ this is not json");
    }

    #[test]
    fn corrupt_file_with_no_previous_starts_empty_and_is_kept() {
        let dir = temp_dir("lost");
        fs::write(dir.join(FILE), "").unwrap();
        let l = load(&dir);
        assert!(l.workspace.agents.is_empty());
        let LoadOutcome::Lost { unreadable_kept_as, .. } = l.outcome else {
            panic!("expected Lost, got {:?}", l.outcome)
        };
        assert!(unreadable_kept_as.exists());
        // Saving afterwards must not destroy the file that was set aside.
        save(&dir, &one("after")).unwrap();
        assert!(unreadable_kept_as.exists());
    }

    #[test]
    fn file_from_a_newer_version_is_not_loaded_or_overwritten() {
        let dir = temp_dir("newer");
        let text = r#"{"version": 999, "agents": [], "future_field": true}"#;
        fs::write(dir.join(FILE), text).unwrap();
        let l = load(&dir);
        let LoadOutcome::Lost { unreadable_kept_as, why } = l.outcome else {
            panic!("expected Lost, got {:?}", l.outcome)
        };
        assert!(why.contains("newer Lillignode"), "{why}");
        save(&dir, &one("mine")).unwrap();
        assert_eq!(fs::read_to_string(unreadable_kept_as).unwrap(), text);
    }

    #[test]
    fn interrupted_save_loads_the_previous_copy() {
        let dir = temp_dir("interrupted");
        save(&dir, &one("a")).unwrap();
        // Simulate stopping after the current file was renamed away.
        fs::rename(dir.join(FILE), dir.join(PREVIOUS)).unwrap();
        fs::write(dir.join("workspace.writing.json"), "{ half").unwrap();
        let l = load(&dir);
        assert_eq!(l.outcome, LoadOutcome::Loaded);
        assert_eq!(l.workspace, one("a"));
    }

    #[test]
    fn a_file_too_large_or_listing_too_many_agents_is_set_aside_and_nothing_is_started() {
        let dir = temp_dir("too-many");
        save(&dir, &one("good")).unwrap();
        save(&dir, &one("good")).unwrap();
        let mut many = one("a");
        many.agents = vec![one("a").agents[0].clone(); MOST_AGENTS + 1];
        fs::write(file_in(&dir), serde_json::to_string(&many).unwrap()).unwrap();
        let l = load(&dir);
        let LoadOutcome::UsedPrevious { why, .. } = l.outcome else { panic!("{:?}", l.outcome) };
        assert!(why.starts_with("it lists 65 agents, more than the 64"), "{why}");
        assert_eq!(l.workspace, one("good"), "the previous copy is used");
        // Exactly the most is read.
        many.agents.truncate(MOST_AGENTS);
        fs::write(file_in(&dir), serde_json::to_string(&many).unwrap()).unwrap();
        assert_eq!(load(&dir).outcome, LoadOutcome::Loaded);

        let dir = temp_dir("too-large");
        let mut text = serde_json::to_string(&one("a")).unwrap();
        text.push_str(&" ".repeat(LARGEST_FILE as usize));
        fs::write(file_in(&dir), &text).unwrap();
        let l = load(&dir);
        let LoadOutcome::Lost { why, unreadable_kept_as } = l.outcome else { panic!("{:?}", l.outcome) };
        assert!(why.contains("larger than the 1000000 Lillignode reads"), "{why}");
        assert!(l.workspace.agents.is_empty());
        assert_eq!(fs::metadata(unreadable_kept_as).unwrap().len(), text.len() as u64, "kept whole");
    }

    #[test]
    fn missing_fields_are_an_error_not_a_silent_default() {
        let dir = temp_dir("fields");
        fs::write(dir.join(FILE), r#"{"version": 1, "agents": [{"name": "x"}]}"#).unwrap();
        assert!(matches!(load(&dir).outcome, LoadOutcome::Lost { .. }));
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    #[test]
    fn a_second_claim_is_refused_until_the_first_is_released() {
        let dir = std::env::temp_dir().join(format!("lillignode-test-claim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let first = claim(&dir).unwrap();
        assert!(matches!(claim(&dir), Err(ClaimError::InUse)));
        drop(first);
        assert!(claim(&dir).is_ok());
    }
}
