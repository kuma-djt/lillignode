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
    if name.chars().any(char::is_control) {
        return Err("A team's name cannot hold a character that cannot be shown.".to_string());
    }
    Ok(name.to_string())
}

/// The saved teams. None kept: none. A file that is there and cannot be
/// read is an error, with why.
pub fn read(folder: Option<&Path>) -> Result<Vec<Team>, String> {
    let Some(folder) = folder else { return Ok(Vec::new()) };
    let path = folder.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{FILE} could not be read ({e})")),
    };
    serde_json::from_str::<TeamsFile>(&text)
        .map(|file| file.teams)
        .map_err(|e| format!("{FILE} is not what Lillignode wrote ({e})"))
}

/// The team with this name, in any case.
pub fn find<'a>(teams: &'a [Team], name: &str) -> Option<&'a Team> {
    teams.iter().find(|t| t.name.eq_ignore_ascii_case(name))
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
    let mut teams = read(Some(folder)).map_err(|why| {
        format!("{why}. It is left as it is: put it right or delete it, then save the team again")
    })?;
    match teams.iter().position(|t| t.name.eq_ignore_ascii_case(&team.name)) {
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

    fn agent(name: &str, folder: &str) -> TeamAgent {
        TeamAgent {
            name: name.into(),
            command: name.to_lowercase(),
            kind: None,
            args: vec![],
            folder: PathBuf::from(folder),
        }
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
        assert_eq!(agent("Claude Code", "/w/web").label(), "Claude Code (web)");
    }
}
