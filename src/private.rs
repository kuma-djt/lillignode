//! Files that only this user can read and write: the page's word, the
//! Board's outbox and the record of what waits.
//!
//! On macOS and Linux a file is made with room for its owner alone. On
//! Windows a new file is as open as its folder until it is told otherwise,
//! and a program that opened it in that moment keeps what it opened; so the
//! file is made inside a folder that is first made this user's alone, given
//! its own list of who may use it, then moved out. The system's own tools
//! (`whoami`, `icacls`) do that, named by their whole path.
//!
//! This is a reader's helper: it names nothing of the engine
//! (`tests/boundaries.rs`).

use std::io::Write;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// True if `path` is a file that only this user can read and write. On
/// Windows the system and the machine's administrators may be named too:
/// they can read any file whatever its list says. Not being able to tell
/// is false.
pub fn is_private(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|found| found.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        only_this_users(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

/// Put `text` in `folder`/`name`, whole or not at all, in a file only this
/// user can read and write: at no moment is there a file with a letter of
/// it that anyone else could open. Whatever was at that name is replaced.
#[cfg(unix)]
pub fn write_whole(folder: &Path, name: &str, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let new = folder.join(format!("{name}.new"));
    match std::fs::remove_file(&new) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&new)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&new, folder.join(name))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    written
}

#[cfg(windows)]
pub fn write_whole(folder: &Path, name: &str, text: &str) -> std::io::Result<()> {
    let staging = folder.join(format!("{name}.new"));
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_file(&staging);
    let written = (|| {
        std::fs::create_dir(&staging)?;
        only_this_user(&staging, true)?;
        if !only_this_users(&staging) {
            return Err(std::io::Error::other(format!(
                "its folder could not be made this user's alone: Windows lists {}",
                who_may_use(&staging).unwrap_or_else(|| "nothing readable".into())
            )));
        }
        let inside = staging.join(name);
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&inside)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        only_this_user(&inside, false)?;
        if !only_this_users(&inside) {
            return Err(std::io::Error::other(format!(
                "the file could not be made this user's alone: Windows lists {}",
                who_may_use(&inside).unwrap_or_else(|| "nothing readable".into())
            )));
        }
        std::fs::rename(&inside, folder.join(name))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    written
}

/// Windows: one of the system's own tools, named whole so that nothing
/// else of that name on PATH is run. What it printed, if it succeeded.
#[cfg(windows)]
pub fn system_tool(program: &str, args: &[std::ffi::OsString]) -> std::io::Result<String> {
    use std::process::{Command, Stdio};
    let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let out = Command::new(system.join("System32").join(program)).args(args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!("{program} could not do it")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Windows: the id the system knows this user by, asked once.
#[cfg(windows)]
fn this_user() -> std::io::Result<&'static str> {
    static ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let id = ID.get_or_init(|| {
        // "name","S-1-5-21-..."
        let who =
            system_tool("whoami.exe", &["/user".into(), "/fo".into(), "csv".into(), "/nh".into()]).ok()?;
        let id = who.trim().rsplit(',').next().unwrap_or("").trim_matches('"').to_string();
        (id.starts_with("S-1-") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).then_some(id)
    });
    id.as_deref().ok_or_else(|| std::io::Error::other("whoami did not say who this user is"))
}

/// Windows: take away what the folder above gave everyone else, and give
/// this to this user alone. `hands_down`: for a folder, what is made in it
/// afterwards is this user's alone too.
#[cfg(windows)]
fn only_this_user(path: &Path, hands_down: bool) -> std::io::Result<()> {
    let id = this_user()?;
    let grant = if hands_down { format!("*{id}:(OI)(CI)F") } else { format!("*{id}:F") };
    system_tool("icacls.exe", &[path.into(), "/inheritance:r".into(), "/grant:r".into(), grant.into()])?;
    Ok(())
}

/// Windows: the list of who may use `path`, as the system writes it out
/// in its own plain form, the same in every language: for a file that is
/// one user's alone, `P(A;;FA;;;S-1-5-21-…)`. None if it could not be read.
#[cfg(windows)]
pub fn who_may_use(path: &Path) -> Option<String> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let list = std::env::temp_dir().join(format!(
        "lillignode-who-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let saved = system_tool("icacls.exe", &[path.into(), "/save".into(), list.clone().into()]);
    let bytes = std::fs::read(&list).unwrap_or_default();
    let _ = std::fs::remove_file(&list);
    saved.ok()?;
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let text = String::from_utf16_lossy(&units);
    // The file's name, then on the next line "D:" and the list. A part
    // that begins "S:" may follow: it is about records kept, not about who
    // may use the file.
    let line = text.lines().find(|line| line.starts_with("D:"))?;
    let list = line["D:".len()..].split("S:").next().unwrap_or("");
    Some(list.trim_end_matches('\u{0}').trim().to_string())
}

/// Windows: true if `path` takes nothing from the folder above and the
/// list of who may use it names nobody but this user, the system itself
/// and the machine's administrators. Anything else, or not being able to
/// tell, is false.
#[cfg(windows)]
pub fn only_this_users(path: &Path) -> bool {
    let (Ok(id), Some(list)) = (this_user(), who_may_use(path)) else { return false };
    // "P…" takes nothing from above. Then one "(A;…;…;;;who)" for each
    // entry: "A" allows, and the last part is whom. The system writes
    // itself as "SY", the administrators as "BA", and the machine's first
    // account, whose id ends in 500, as "LA".
    let Some((flags, entries)) = list.split_once('(') else { return false };
    let mine = |who: &str| who == id || (who == "LA" && id.ends_with("-500"));
    let mut this_user_is_named = false;
    for entry in entries.trim_end_matches(')').split(")(") {
        let parts: Vec<&str> = entry.split(';').collect();
        let (Some(&"A"), Some(who)) = (parts.first(), parts.last()) else { return false };
        this_user_is_named |= mine(who);
        if !mine(who) && !["SY", "BA"].contains(who) {
            return false;
        }
    }
    flags.starts_with('P') && this_user_is_named
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-private-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_written_whole_is_this_users_alone_and_replaces_what_was_there() {
        let dir = scratch("whole");
        write_whole(&dir, "kept", "one").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("kept")).unwrap(), "one");
        assert!(is_private(&dir.join("kept")));
        write_whole(&dir, "kept", "two").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("kept")).unwrap(), "two");
        assert!(is_private(&dir.join("kept")));
        assert!(!dir.join("kept.new").exists(), "nothing left beside it");
        // A file like any other, as its folder made it, is not.
        std::fs::write(dir.join("plain"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join("plain"), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(!is_private(&dir.join("plain")));
        assert!(!is_private(&dir.join("not there")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
