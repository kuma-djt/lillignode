//! Saved teams: named sets of agents, kept in `teams.json` beside the
//! workspace.
//!
//!   { "teams": [ { "name": "web", "agents": [
//!       { "name": "Claude Code", "command": "claude", "kind": "claude-code",
//!         "args": [], "folder": "C:/work/web" } ] } ] }
//!
//! A team holds each agent's program, kind, options and folder: no
//! conversation. Started again, a team's agents start new conversations.
//!
//! The file is written whole and kept private, as the other files beside
//! the workspace are. A file that is there and cannot be read is never
//! written over: saving a team must not lose the others.
//!
//! This file is a reader: it names nothing of the engine, and is given
//! only a folder to read and write in (`tests/boundaries.rs`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private;

/// The teams' file, in the folder Lillignode saves in.
pub const FILE: &str = "teams.json";

/// The longest name a team may have, in characters.
pub const NAME_MAX: usize = 40;

/// No more teams than this are kept.
pub const MOST: usize = 50;

/// No more agents than this in one team.
pub const MOST_AGENTS: usize = 20;

/// A teams' file larger than this is not read.
pub const MOST_BYTES: u64 = 256 * 1024;

/// One agent of a team, as it is started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAgent {
    pub name: String,
    pub command: String,
    /// The saved word for its kind ("claude-code", "codex", "program"), as
    /// the workspace keeps it.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub folder: PathBuf,
}

impl TeamAgent {
    /// What is run: the program and its options, as typed at a prompt. An
    /// option with a space, or nothing in it, is quoted.
    pub fn runs(&self) -> String {
        let mut words = vec![self.command.clone()];
        for arg in &self.args {
            if arg.is_empty() || arg.contains(char::is_whitespace) || arg.contains('"') {
                words.push(format!("\"{}\"", arg.replace('"', "\\\"")));
            } else {
                words.push(arg.clone());
            }
        }
        words.join(" ")
    }

    /// Why this agent cannot be started as it is written, if it cannot.
    fn problem(&self) -> Option<String> {
        if self.command.trim().is_empty() {
            return Some("an agent has no program".to_string());
        }
        let texts = [&self.name, &self.command].into_iter().chain(self.args.iter());
        if texts.clone().any(|t| t.chars().any(hidden)) {
            return Some(format!("\"{}\" holds a character that cannot be shown", self.name.escape_debug()));
        }
        if self.folder.to_string_lossy().chars().any(hidden) {
            return Some(format!("the folder of \"{}\" holds a character that cannot be shown", self.name));
        }
        if !self.folder.is_absolute() {
            return Some(format!("the folder of \"{}\" is not a full path", self.name));
        }
        None
    }

    /// How the agent is named to the reader: its name and its folder's last part.
    pub fn label(&self) -> String {
        let place = self.folder.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        if place.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({place})", self.name)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub agents: Vec<TeamAgent>,
}

#[derive(Default, Serialize, Deserialize)]
struct TeamsFile {
    teams: Vec<Team>,
}

/// A character that is not shown as itself: a control character, or one
/// that is invisible or turns the direction of text around it. A name or a
/// program holding one could read as something it is not.
pub fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
            | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}' | '\u{E0000}'..='\u{E0FFF}')
}

/// Two team names are the same name in any case.
pub fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// A short fingerprint of everything a team starts: its name, and each
/// agent's program, options, kind and folder. A start asked for after the
/// team was shown carries it, and is refused if the team has changed.
pub fn digest(team: &Team) -> String {
    let text = serde_json::to_string(team).unwrap_or_default();
    // FNV-1a, 64 bits: a fingerprint to notice a change, not a secret.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// A team's name as typed, made ready: its ends trimmed, refused with why
/// if it is empty, too long, or has a character that cannot be shown.
pub fn check_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err("A team needs a name.".to_string());
    }
    if name.chars().count() > NAME_MAX {
        return Err(format!("A team's name can have at most {NAME_MAX} characters."));
    }
    if name.chars().any(hidden) {
        return Err("A team's name cannot hold a character that cannot be shown.".to_string());
    }
    Ok(name.to_string())
}

/// The saved teams. None kept: none. A file that is there and cannot be
/// read is an error, with why.
pub fn read(folder: Option<&Path>) -> Result<Vec<Team>, String> {
    let Some(folder) = folder else { return Ok(Vec::new()) };
    let path = folder.join(FILE);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > MOST_BYTES => {
            return Err(format!("{FILE} is larger than Lillignode writes ({} KB or more)", MOST_BYTES / 1024))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => {}
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{FILE} could not be read ({e})")),
    };
    let teams = serde_json::from_str::<TeamsFile>(&text)
        .map(|file| file.teams)
        .map_err(|e| format!("{FILE} is not what Lillignode wrote ({e})"))?;
    check_all(&teams).map_err(|why| format!("{FILE} is not what Lillignode wrote ({why})"))?;
    Ok(teams)
}

/// What Lillignode writes, it can read: no more teams or agents than it
/// keeps, names that can be shown and are not given twice, and agents that
/// can be started as they are written. Anything else was written by hand,
/// or by another program, and is not started.
fn check_all(teams: &[Team]) -> Result<(), String> {
    if teams.len() > MOST {
        return Err(format!("more than {MOST} teams"));
    }
    for (at, team) in teams.iter().enumerate() {
        match check_name(&team.name) {
            Ok(name) if name == team.name => {}
            _ => return Err(format!("a team's name, \"{}\", cannot be used", team.name.escape_debug())),
        }
        if teams[..at].iter().any(|t| same_name(&t.name, &team.name)) {
            return Err(format!("the team \"{}\" is there twice", team.name));
        }
        if team.agents.is_empty() || team.agents.len() > MOST_AGENTS {
            return Err(format!("the team \"{}\" has {} agents", team.name, team.agents.len()));
        }
        if let Some(why) = team.agents.iter().find_map(TeamAgent::problem) {
            return Err(format!("in the team \"{}\", {why}", team.name));
        }
    }
    Ok(())
}

/// The team with this name, in any case.
pub fn find<'a>(teams: &'a [Team], name: &str) -> Option<&'a Team> {
    teams.iter().find(|t| same_name(&t.name, name))
}

/// Keeps `team`, in place of one of the same name (in any case). Refused,
/// with why, if the file cannot be read (it is not written over), if the
/// team has no agents or too many, or if there are already too many teams.
pub fn save(folder: Option<&Path>, team: Team) -> Result<(), String> {
    let Some(folder) = folder else {
        return Err("Lillignode has nowhere to save, so no team can be kept.".to_string());
    };
    if team.agents.is_empty() {
        return Err("A team needs at least one agent.".to_string());
    }
    if team.agents.len() > MOST_AGENTS {
        return Err(format!("A team can have at most {MOST_AGENTS} agents."));
    }
    if let Some(why) = team.agents.iter().find_map(TeamAgent::problem) {
        return Err(format!("It cannot be saved as it is: {why}."));
    }
    let mut teams = read(Some(folder)).map_err(|why| {
        format!("{why}. It is left as it is: put it right or delete it, then save the team again")
    })?;
    match teams.iter().position(|t| same_name(&t.name, &team.name)) {
        Some(at) => teams[at] = team,
        None if teams.len() >= MOST => return Err(format!("At most {MOST} teams can be kept.")),
        None => teams.push(team),
    }
    let text = serde_json::to_string_pretty(&TeamsFile { teams }).unwrap_or_default();
    private::write_whole(folder, FILE, &text).map_err(|e| format!("{FILE} could not be written ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-teams-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A full path on this system.
    fn full(folder: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", folder.replace('/', "\\")))
        } else {
            PathBuf::from(folder)
        }
    }

    fn agent(name: &str, folder: &str) -> TeamAgent {
        TeamAgent {
            name: name.into(),
            command: name.to_lowercase(),
            kind: None,
            args: vec![],
            folder: full(folder),
        }
    }

    fn write(dir: &Path, teams: &[Team]) {
        std::fs::write(dir.join(FILE), serde_json::to_string(&TeamsFile { teams: teams.to_vec() }).unwrap())
            .unwrap();
    }

    #[test]
    fn a_file_written_by_hand_is_not_started_if_lillignode_would_not_have_written_it() {
        let dir = folder("by-hand");
        let team = |name: &str, agents: Vec<TeamAgent>| Team { name: name.into(), agents };
        let cases: Vec<(Vec<Team>, &str)> = vec![
            (
                vec![team("web", vec![agent("A", "/a")]), team("WEB", vec![agent("B", "/b")])],
                "is there twice",
            ),
            (vec![team("web", vec![])], "has 0 agents"),
            (vec![team("we\u{202E}b", vec![agent("A", "/a")])], "cannot be used"),
            (vec![team(" web", vec![agent("A", "/a")])], "cannot be used"),
            (
                vec![team(
                    "web",
                    vec![TeamAgent { folder: PathBuf::from("relative/web"), ..agent("A", "/a") }],
                )],
                "is not a full path",
            ),
            (
                vec![team("web", vec![TeamAgent { command: " ".into(), ..agent("A", "/a") }])],
                "has no program",
            ),
            (
                vec![team("web", vec![TeamAgent { args: vec!["--x\u{200B}".into()], ..agent("A", "/a") }])],
                "cannot be shown",
            ),
            (vec![team("web", (0..=MOST_AGENTS).map(|i| agent(&format!("A{i}"), "/a")).collect())], "agents"),
            ((0..=MOST).map(|i| team(&format!("t{i}"), vec![agent("A", "/a")])).collect(), "more than"),
        ];
        for (teams, why) in cases {
            write(&dir, &teams);
            let said = read(Some(&dir)).unwrap_err();
            assert!(said.contains("is not what Lillignode wrote") && said.contains(why), "{why}: {said}");
        }
        std::fs::write(dir.join(FILE), " ".repeat(MOST_BYTES as usize + 1)).unwrap();
        assert!(read(Some(&dir)).unwrap_err().contains("larger than"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_runs_is_said_as_typed_and_any_change_changes_the_fingerprint() {
        let a =
            TeamAgent { args: vec!["--model".into(), "a b".into(), String::new()], ..agent("Claude", "/w") };
        assert_eq!(a.runs(), "claude --model \"a b\" \"\"");
        let team = Team { name: "web".into(), agents: vec![a.clone()] };
        let same = digest(&team);
        assert_eq!(same, digest(&team.clone()));
        assert_eq!(same.len(), 16);
        for changed in [
            Team { name: "Web".into(), ..team.clone() },
            Team { agents: vec![TeamAgent { command: "claudx".into(), ..a.clone() }], ..team.clone() },
            Team { agents: vec![TeamAgent { args: vec![], ..a.clone() }], ..team.clone() },
            Team { agents: vec![TeamAgent { folder: full("/v"), ..a.clone() }], ..team.clone() },
        ] {
            assert_ne!(digest(&changed), same, "{changed:?}");
        }
    }

    #[test]
    fn names_are_the_same_in_any_case_beyond_ascii() {
        assert!(same_name("Équipe", "équipe"));
        assert!(!same_name("web", "webs"));
        assert!(check_name("we\u{200F}b").is_err());
    }

    #[test]
    fn a_team_is_saved_read_back_and_saved_over_by_name_in_any_case() {
        let dir = folder("save");
        assert_eq!(read(Some(&dir)).unwrap(), vec![], "no file: no teams");
        save(Some(&dir), Team { name: "Web".into(), agents: vec![agent("Claude", "/w/web")] }).unwrap();
        save(Some(&dir), Team { name: "docs".into(), agents: vec![agent("Codex", "/w/docs")] }).unwrap();
        let teams = read(Some(&dir)).unwrap();
        assert_eq!(teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Web", "docs"]);
        save(Some(&dir), Team { name: "web".into(), agents: vec![agent("A", "/a"), agent("B", "/b")] })
            .unwrap();
        let teams = read(Some(&dir)).unwrap();
        assert_eq!(teams.len(), 2, "saved over, not added");
        assert_eq!(find(&teams, "WEB").unwrap().agents.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_written_over() {
        let dir = folder("broken");
        std::fs::write(dir.join(FILE), "{ not json").unwrap();
        let why = save(Some(&dir), Team { name: "web".into(), agents: vec![agent("A", "/a")] }).unwrap_err();
        assert!(why.contains("is not what Lillignode wrote") && why.contains("left as it is"), "{why}");
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_and_sizes_are_checked() {
        assert_eq!(check_name("  web  ").unwrap(), "web");
        assert!(check_name("   ").is_err());
        assert!(check_name(&"x".repeat(NAME_MAX + 1)).is_err());
        assert!(check_name("we\u{1b}b").is_err());
        let dir = folder("sizes");
        assert!(save(Some(&dir), Team { name: "none".into(), agents: vec![] }).is_err());
        let many = (0..=MOST_AGENTS).map(|i| agent(&format!("A{i}"), "/a")).collect();
        assert!(save(Some(&dir), Team { name: "many".into(), agents: many }).is_err());
        assert!(save(None, Team { name: "x".into(), agents: vec![agent("A", "/a")] }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_label_names_the_agent_and_its_folder() {
        assert_eq!(
            TeamAgent { name: "Claude Code".into(), ..agent("x", "/w/web") }.label(),
            "Claude Code (web)"
        );
    }
}
