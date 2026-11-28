//! Pictures for agents' round slots on the page, from a folder of the
//! user's own: `faces`, beside the workspace. A picture named after an
//! agent's name or its kind (`claude-code.png`, `bartlebey.png`) is shown
//! for that agent; any other agent keeps its initials.
//!
//! The folder is the switch: until you make it, nothing is read. Only PNG,
//! JPEG and WebP files, at most 512 KB each, whose first bytes say they are
//! what their name says; nothing is ever fetched from the web. The folder
//! is read once, and again only when what is in it changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// The folder's name, beside the workspace.
pub const FOLDER: &str = "faces";
/// The largest picture taken.
pub const MOST_BYTES: u64 = 512 * 1024;
/// At most this many pictures are kept.
const MOST_FILES: usize = 64;
/// The folder is looked at no more often than this.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// One picture, and what kind of picture it is.
pub struct Face {
    pub bytes: Vec<u8>,
    pub kind: &'static str,
}

/// What the folder held when it was last read.
pub struct Faces {
    folder: Option<PathBuf>,
    seen: Vec<(String, u64, Option<SystemTime>)>,
    faces: HashMap<String, Arc<Face>>,
    looked: Option<Instant>,
}

/// The name a picture is found by: lower case, spaces as hyphens.
pub fn stem(name: &str) -> String {
    name.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-")
}

/// A stem may hold letters, digits, hyphens, dots and underscores only.
fn plain(stem: &str) -> bool {
    !stem.is_empty() && stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What the first bytes say the picture is, if it is one of the three.
fn kind_of(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The kind a file's name promises.
fn kind_named(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

impl Faces {
    /// Pictures from `folder`, read when first asked for.
    pub fn new(folder: Option<PathBuf>) -> Faces {
        Faces { folder, seen: Vec::new(), faces: HashMap::new(), looked: None }
    }

    /// The pictures from beside the workspace in `dir`.
    pub fn beside(dir: Option<&Path>) -> Faces {
        Faces::new(dir.map(|d| d.join(FOLDER)))
    }

    /// Read the folder again if what is in it has changed: names, sizes or
    /// times. At most once every two seconds.
    pub fn refresh(&mut self) {
        if self.looked.is_some_and(|t| t.elapsed() < LOOK_EVERY) {
            return;
        }
        self.looked = Some(Instant::now());
        let Some(folder) = &self.folder else { return };
        let mut now: Vec<(String, u64, Option<SystemTime>)> = match std::fs::read_dir(folder) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    // A link is not followed: only a file that is itself here.
                    let meta = std::fs::symlink_metadata(e.path()).ok()?;
                    meta.is_file().then(|| {
                        (e.file_name().to_string_lossy().to_string(), meta.len(), meta.modified().ok())
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        now.sort();
        if now == self.seen {
            return;
        }
        self.faces.clear();
        for (file, len, _) in &now {
            if self.faces.len() >= MOST_FILES {
                break;
            }
            let path = Path::new(file);
            let (Some(name), Some(extension)) = (path.file_stem(), path.extension()) else { continue };
            let (name, Some(named)) =
                (stem(&name.to_string_lossy()), kind_named(&extension.to_string_lossy()))
            else {
                continue;
            };
            if !plain(&name) || *len > MOST_BYTES || self.faces.contains_key(&name) {
                continue;
            }
            let Ok(bytes) = std::fs::read(folder.join(file)) else { continue };
            if bytes.len() as u64 > MOST_BYTES || kind_of(&bytes) != Some(named) {
                continue;
            }
            self.faces.insert(name, Arc::new(Face { bytes, kind: named }));
        }
        self.seen = now;
    }

    /// The picture for an agent: by its name first, then by its kind.
    pub fn for_agent(&self, name: &str, kind: &str) -> Option<String> {
        [stem(name), stem(kind)].into_iter().find(|s| self.faces.contains_key(s))
    }

    /// A picture by the name the page was given.
    pub fn get(&self, stem: &str) -> Option<Arc<Face>> {
        self.faces.get(stem).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10];
    const WEBP: &[u8] = b"RIFF\0\0\0\0WEBPVP8 ";

    fn folder(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-faces-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(FOLDER)).unwrap();
        dir
    }

    #[test]
    fn a_picture_is_found_by_the_agents_name_then_its_kind_and_otherwise_none() {
        let dir = folder("names");
        let faces_dir = dir.join(FOLDER);
        std::fs::write(faces_dir.join("bartlebey.png"), PNG).unwrap();
        std::fs::write(faces_dir.join("claude-code.JPG"), JPEG).unwrap();
        std::fs::write(faces_dir.join("codex.webp"), WEBP).unwrap();
        let mut faces = Faces::beside(Some(&dir));
        faces.refresh();
        assert_eq!(faces.for_agent("Bartlebey", "claude-code").as_deref(), Some("bartlebey"));
        assert_eq!(faces.for_agent("Claude Code", "program").as_deref(), Some("claude-code"));
        assert_eq!(faces.for_agent("helper", "claude-code").as_deref(), Some("claude-code"));
        assert_eq!(faces.for_agent("shell", "program"), None);
        assert_eq!(faces.get("codex").unwrap().kind, "image/webp");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_small_pictures_that_are_what_their_name_says_are_taken() {
        let dir = folder("limits");
        let faces_dir = dir.join(FOLDER);
        // A page, not a picture, named as one; a picture too large; a
        // picture of another kind than its name; a name with a space in it
        // is found as its hyphened stem; a text file.
        std::fs::write(faces_dir.join("page.png"), b"<html>").unwrap();
        let mut big = PNG.to_vec();
        big.resize(MOST_BYTES as usize + 1, 0);
        std::fs::write(faces_dir.join("big.png"), &big).unwrap();
        std::fs::write(faces_dir.join("liar.png"), JPEG).unwrap();
        std::fs::write(faces_dir.join("night runner.png"), PNG).unwrap();
        std::fs::write(faces_dir.join("notes.txt"), PNG).unwrap();
        let mut faces = Faces::beside(Some(&dir));
        faces.refresh();
        for refused in ["page", "big", "liar", "notes"] {
            assert!(faces.get(refused).is_none(), "{refused}");
        }
        assert!(faces.get("night-runner").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_folder_is_no_pictures_and_a_change_is_read_again() {
        let mut none = Faces::beside(None);
        none.refresh();
        assert_eq!(none.for_agent("a", "b"), None);
        let dir = folder("change");
        let mut faces = Faces::beside(Some(&dir));
        faces.refresh();
        assert!(faces.get("a").is_none());
        std::fs::write(dir.join(FOLDER).join("a.png"), PNG).unwrap();
        faces.looked = None;
        faces.refresh();
        assert!(faces.get("a").is_some());
        std::fs::remove_file(dir.join(FOLDER).join("a.png")).unwrap();
        faces.looked = None;
        faces.refresh();
        assert!(faces.get("a").is_none(), "a picture taken away is gone");
        std::fs::remove_dir_all(&dir).ok();
    }
}
