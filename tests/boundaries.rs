//! Keeps the part of Lillignode that runs agents apart from the part that
//! draws the screen and reads the keyboard.
//!
//! Every file in `src/` is on one of two sides. An engine file may not name
//! the drawing library or the keyboard library, and may not use a file from
//! the other side. A new file must be put on a side here before this passes.
//!
//! A third kind of file is a reader: a face on another thread that is
//! handed the engine's published copies and nothing else. It may not name
//! the engine, the agents or the half that publishes, so it has no way to
//! ask for anything.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Runs agents, tracks them, saves them. No screen, no keyboard.
const ENGINE: &[&str] = &[
    "action",
    "agent",
    "config",
    "cost",
    "engine",
    "kind",
    "local",
    "snapshot",
    "state",
    "status",
    "terminal",
    "workspace",
];

/// Draws, reads keys, and words what the engine decided.
const FACE: &[&str] = &["app", "fit", "input", "keys", "look", "main", "ui", "view"];

/// Reads the engine's published copies and nothing else: the page, and
/// the Board it reads from a file.
const READER: &[&str] = &["adapter", "board", "page", "private"];

/// What a reader may not name outside its tests: the engine and what it
/// holds, the running agents, the saved workspace, the terminal face, and
/// the half that publishes.
const NOT_FOR_A_READER: &[&str] = &[
    "crate::agent",
    "crate::config",
    "engine::",
    "local::",
    "state::",
    "Engine",
    "Workspace",
    "AgentBackend",
    "Publisher",
];

/// Declares the modules and nothing else.
const ROOT: &str = "lib";

/// The libraries an engine file may not name, even in a comment.
const SCREEN_AND_KEYBOARD: &[&str] = &["ratatui", "crossterm"];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The first name of each path that follows `crate::` in `text`:
/// `crate::ui::draw` gives `ui`; `crate::{agent, ui::draw}` gives both.
fn used_from_crate(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (at, _) in text.match_indices("crate::") {
        let rest = &text[at + "crate::".len()..];
        let Some(inside) = rest.strip_prefix('{') else {
            found.insert(rest.chars().take_while(|c| is_ident(*c)).collect());
            continue;
        };
        // A group. Only names at its top level start a path from the crate.
        let (mut depth, mut name, mut at_start) = (0usize, String::new(), true);
        for c in inside.chars() {
            match c {
                '}' if depth == 0 => break,
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    found.insert(std::mem::take(&mut name));
                    at_start = true;
                    continue;
                }
                c if depth == 0 && at_start && is_ident(c) => name.push(c),
                c if c.is_whitespace() && name.is_empty() => continue,
                _ => {}
            }
            if !name.is_empty() && !is_ident(c) {
                at_start = false;
            }
        }
        found.insert(name);
    }
    found.remove("");
    found
}

/// What an engine file's text does that it may not. Empty when it is clean.
fn trespasses(text: &str) -> Vec<String> {
    let mut found: Vec<String> = SCREEN_AND_KEYBOARD
        .iter()
        .filter(|library| text.contains(**library))
        .map(|library| format!("names {library}"))
        .collect();
    let used = used_from_crate(text);
    found.extend(FACE.iter().filter(|face| used.contains(**face)).map(|face| format!("uses crate::{face}")));
    found
}

fn source_files() -> BTreeSet<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&src).expect("src can be listed") {
        let path = entry.expect("an entry in src can be read").path();
        assert!(path.is_file(), "{} is a folder: this check reads only files in src", path.display());
        if path.extension().is_some_and(|e| e == "rs") {
            names.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    names
}

#[test]
fn every_source_file_is_on_one_side() {
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for name in ENGINE.iter().chain(FACE).chain(READER).chain([&ROOT]) {
        assert!(listed.insert(name.to_string()), "{name} is listed twice in tests/boundaries.rs");
    }
    let present = source_files();
    let unlisted: Vec<_> = present.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "src has files that tests/boundaries.rs does not place: {unlisted:?}. Add each to ENGINE (no \
         screen or keyboard code) or FACE."
    );
    let missing: Vec<_> = listed.difference(&present).collect();
    assert!(missing.is_empty(), "tests/boundaries.rs lists files that are not in src: {missing:?}");
}

#[test]
fn engine_files_hold_no_screen_or_keyboard_code() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut problems = Vec::new();
    for name in ENGINE {
        let text = fs::read_to_string(src.join(format!("{name}.rs"))).expect("an engine file can be read");
        problems.extend(trespasses(&text).into_iter().map(|what| format!("src/{name}.rs {what}")));
    }
    assert!(
        problems.is_empty(),
        "engine files must not draw or read keys, or use a file that does:\n  {}",
        problems.join("\n  ")
    );
}

/// What a reader's text does that it may not. Its tests may publish, to
/// have something to read; they are not looked at.
fn reader_trespasses(text: &str) -> Vec<String> {
    // A checkout on Windows may end its lines with a carriage return too.
    let text = text.replace("\r\n", "\n");
    let own = text.split("#[cfg(test)]\nmod tests").next().unwrap_or(&text);
    let mut found = trespasses(own);
    found.extend(
        NOT_FOR_A_READER.iter().filter(|name| own.contains(**name)).map(|name| format!("names {name}")),
    );
    found
}

#[test]
fn a_reader_is_handed_copies_and_can_reach_nothing_else() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut problems = Vec::new();
    for name in READER {
        let text = fs::read_to_string(src.join(format!("{name}.rs"))).expect("a reader file can be read");
        problems.extend(reader_trespasses(&text).into_iter().map(|what| format!("src/{name}.rs {what}")));
    }
    assert!(
        problems.is_empty(),
        "a reader must not draw, read keys, or reach the engine or its agents:\n  {}",
        problems.join("\n  ")
    );
    assert_eq!(
        reader_trespasses("use crate::snapshot::Reader;\nuse crate::status::AgentStatus;"),
        Vec::<String>::new()
    );
    assert_eq!(reader_trespasses("use crate::engine::Event;"), ["names engine::"]);
    assert_eq!(reader_trespasses("fn f(p: &snapshot::Publisher) {}"), ["names Publisher"]);
    assert_eq!(reader_trespasses("fn f<B>(w: &Workspace<B>) {}"), ["names Workspace"]);
    assert_eq!(reader_trespasses("use crate::ui;"), ["uses crate::ui"]);
    assert_eq!(
        reader_trespasses("fn f() {}\n#[cfg(test)]\nmod tests { use super::Publisher; }"),
        Vec::<String>::new()
    );
    assert_eq!(reader_trespasses("#[cfg(test)]\nfn f() {}\nfn g(p: Publisher) {}"), ["names Publisher"]);
    assert_eq!(reader_trespasses("let e = super::engine::quit();"), ["names engine::"]);
}

#[test]
fn the_check_catches_what_it_is_for() {
    assert_eq!(trespasses("use std::path::Path;\nuse crate::agent::Size;\n"), Vec::<String>::new());
    assert_eq!(trespasses("use ratatui::layout::Rect;"), ["names ratatui"]);
    assert_eq!(trespasses("fn f(k: crossterm::event::KeyEvent) {}"), ["names crossterm"]);
    assert_eq!(trespasses("use crate::ui;"), ["uses crate::ui"]);
    assert_eq!(trespasses("let a = crate::ui::areas(w);"), ["uses crate::ui"]);
    assert_eq!(trespasses("use crate::{agent::Size, look::{Depth, Tone}, status};"), ["uses crate::look"]);
    assert_eq!(trespasses("use crate::{\n    agent,\n    app::App,\n};"), ["uses crate::app"]);
    // A name that only starts like a face file, or sits deeper in a path, is not one.
    assert_eq!(trespasses("use crate::viewer::X;"), Vec::<String>::new());
    assert_eq!(trespasses("use crate::{status::{ui, view}, agent};"), Vec::<String>::new());
}
