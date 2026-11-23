//! Development tool, not part of the product.
//!
//! The steps of two hand checks that a script can judge, done by a script:
//! the real `lillignode` in a terminal this check owns, with stand-in
//! agents. No window, no real agent, no real settings (see `owned/mod.rs`).
//!
//!   cargo build --release --all-targets && cargo run --release --example handcheck
//!
//! Each line it prints begins with the step of the hand check it stands
//! for, so the two can be read side by side:
//!
//! - "since" A3 to A7, B1 to B5 and C1 to C12: paste into an agent that
//!   does not take one whole, the refusal line at two widths, how a new
//!   agent is named, and two agents side by side;
//! - "parts" 1 to 18 and 10b: a waiting part of a program shows as Needs
//!   you, is dismissed, stays hidden while it repeats itself, and is
//!   withdrawn.
//!
//! Where a hand check has a person type a status report into PowerShell,
//! a stand-in sends the same report on a key. What is left for a person is
//! what a script cannot judge: a paste from the real clipboard, where the
//! closing line lands in a real window after a resize, and how any of it
//! looks.
//!
//! It takes about three minutes: one step waits a minute on purpose.

use std::time::{Duration, Instant};

#[path = "owned/mod.rs"]
mod owned;
use owned::*;

/// How many columns the sidebar takes: an agent alone has the rest.
const SIDEBAR: u16 = 28;

/// Wait for `needle` to be on screen one more time than `before`.
fn once_more(s: &mut Session, step: &str, needle: &str, before: usize) {
    let end = Instant::now() + Duration::from_secs(20);
    while s.screen().matches(needle).count() <= before && Instant::now() < end {
        s.pump(Duration::from_millis(50));
    }
    check(step, s.screen().matches(needle).count() > before, &format!("no new {needle}"));
}

/// The row at the foot of the screen: the bar.
fn bar(s: &Session) -> String {
    s.rows().last().cloned().unwrap_or_default().trim_end().to_string()
}

fn since_paste(sb: &Sandbox) {
    println!("since A. Paste: the rule, in an agent that does not take a paste whole");
    let state = sb.root.join("state-paste");
    let mut s = Session::start_in(sb, &state);
    s.new_agent("plain");
    s.expect("       an agent that says what reaches it", "STANDIN READY echo-plain");
    // A1 and A2 are a paste from the real clipboard, which no script here
    // has. What Windows Terminal hands over for one is text in one go with
    // no marks: the rule for that is checked, the clipboard is not.
    s.raw(b"a\tb");
    s.expect("A1/A2  (the rule only) text in one go with a tab: the tab goes as four spaces", "typed=a    b");
    s.expect(
        "A1/A2  (the rule only) and the notice says so",
        "Its tabs were sent as spaces, so none was taken as a press of Tab.",
    );
    s.keys(b"\r");
    s.raw(b"\t");
    s.expect("A3     a Tab pressed alone is still Tab", "typed=\\t");
    s.new_agent("lines");
    s.expect("A4     a second agent starts", "Started lines in");
    s.expect("A4     and is ready", "STANDIN READY echo-plain");
    s.raw(b"\x1b[200~echo one\recho two\x1b[201~");
    s.expect(
        "A5     a two-line paste is asked about",
        "Paste 2 lines into lines (work) one line at a time? y / n",
    );
    s.never("A5     and nothing reached the agent yet", "STATUS");
    s.keys(b"n");
    s.expect("A6     n discards it, and says so", "Paste discarded. Nothing was sent.");
    s.never("A6     nothing reached the agent", "STATUS");
    s.raw(b"\x1b[200~echo one\recho two\x1b[201~");
    s.expect("A7     pasted again, it is asked about again", "one line at a time? y / n");
    s.keys(b"y");
    s.expect(
        "A7     y sends it line by line: the first line entered",
        "STATUS enters=1 pastes=0 typed=echo two",
    );
    s.expect("A7     and the notice says how it went", "Pasted into lines (work) line by line.");
    drop(s.quit("       quit", true));
}

fn since_fixes(sb: &Sandbox, standin: &std::path::Path) {
    println!("since B. The refusal line at two widths, and how an agent is named");
    launcher(&sb.launchers, standin, "claude", "args");
    let tools = sb.root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    launcher(&tools, standin, "tell", "echo-plain");
    let tell = tools.join(if cfg!(windows) { "tell.cmd" } else { "tell" });

    // B5 first needs a workspace saved by an earlier Lillignode, with an
    // agent named by its whole path.
    let state = sb.root.join("state-fixes");
    std::fs::create_dir_all(&state).unwrap();
    let whole = tell.display().to_string();
    let saved = serde_json::json!({
        "version": 1,
        "agents": [{ "name": whole, "command": whole, "args": [], "folder": sb.work, "kind": "program" }],
        "focused": 0,
    });
    std::fs::write(state.join("workspace.json"), saved.to_string()).unwrap();

    let mut s = Session::start_in(sb, &state);
    s.expect("       the saved agent starts", "STANDIN READY echo-plain");
    s.resize(95, ROWS);
    s.raw(CTRL_Q);
    s.keys(b"n");
    s.keys(b"claude\r--resume abc\r\r");
    let reason = "Take out --resume: Lillignode sets the conversation itself.";
    s.expect(
        "B1     at 95 columns the keys keep a word each, and the reason is whole",
        &format!("n change, k kind, Esc cancel   {reason}"),
    );
    check("B1     with no part of the line cut away", !bar(&s).contains('…'), &bar(&s));
    s.never("B1     and nothing has started", "STANDIN ARGS");
    s.resize(150, ROWS);
    s.expect(
        "B2     at 150 columns the line says what each key does, and still ends with the reason",
        &format!("Cannot start it as typed. n: change it, k: another kind, Esc: cancel   {reason}"),
    );
    s.keys(ESC);
    s.expect("B3     Esc cancels, and says nothing was started", "Cancelled. Nothing was started.");
    s.new_agent(&whole);
    s.expect("B4     an agent started by its whole path is named by its file's name", "Started tell in");
    s.never("B4     not by the path", &format!("Started {whole}"));
    let saved_name = |state: &std::path::Path, at: usize| -> String {
        let text = std::fs::read_to_string(state.join("workspace.json")).unwrap_or_default();
        let file: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        file["agents"][at]["name"].as_str().unwrap_or_default().to_string()
    };
    drop(s.quit("       quit", true));
    check(
        "B5     the name saved before is left as it was",
        saved_name(&state, 0) == whole,
        &saved_name(&state, 0),
    );
    check(
        "B4     and the new one is saved under its short name",
        saved_name(&state, 1) == "tell",
        &saved_name(&state, 1),
    );
}

fn since_two(sb: &Sandbox, standin: &std::path::Path) {
    println!("since C. Two agents side by side");
    launcher(&sb.launchers, standin, "sizer", "size");
    launcher(&sb.launchers, standin, "waiter", "reports");
    let state = sb.root.join("state-two");
    let mut s = Session::start_in(sb, &state);
    s.resize(140, ROWS);
    s.new_agent("sizer");
    s.expect("       an agent that says what size it is", "STANDIN READY size");
    s.new_agent("sizer");
    s.expect("       and a second", "Started sizer 2 in");
    let whole = format!("is {} by 37", 140 - SIDEBAR);
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.expect(
        "C1     v puts two side by side, and says which, that each was resized, and the way back",
        "sizer 2 (work) and sizer (work) are side by side. Every agent was told its new size. If one draws badly, \
         Ctrl+Q then v goes back to one.",
    );
    s.resize(100, ROWS);
    s.expect(
        "C2     too narrow for two: one is shown, the other left alone, and both ways on are named",
        "Too narrow for two agents now, so only sizer 2 (work) is shown. sizer (work) was not stopped or closed. \
         Widen to 109 columns for both, or Ctrl+Q then v to stay with one.",
    );
    s.resize(140, ROWS);
    s.expect("C3     widened again, both come back", "There is room again: two agents are side by side.");
    s.resize(100, ROWS);
    s.expect("       narrowed again", "Too narrow for two agents now");
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.expect(
        "C4     v while too narrow stays with one, and says two will not come back",
        "One agent on screen. Two side by side will not come back when the window is widened. No agent was resized.",
    );
    s.resize(140, ROWS);
    s.never("C5     widened, it says nothing about room", "There is room again");
    s.keys(b"five\r");
    s.expect("C5     and the one agent has the whole area", &format!("SIZE five {whole}"));

    s.new_agent("waiter");
    s.expect("       an agent that reports its status on a key", "STANDIN READY reports");
    let bells_before = bells(&s);
    s.keys(b"p");
    s.expect("C6     a report that it is blocked: its row reads NEEDS YOU", " Lillignode  1 NEEDS YOU");
    check("C6     with one bell", bells(&s) == bells_before + 1, &format!("{}", bells(&s) - bells_before));
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.expect(
        "C7     changing the view while one waits asks first, and names it",
        "Resize every agent now? y / n   Waiting for you: waiter (work).",
    );
    s.keys(b"n");
    s.expect(
        "C8     n leaves the view as it was, and says so",
        "The view was left as it was. No agent was resized.",
    );
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.keys(b"y");
    s.expect(
        "C9     y puts two side by side, with the same sentence as before",
        "waiter (work) and sizer (work) are side by side. Every agent was told its new size. If one draws badly, \
         Ctrl+Q then v goes back to one.",
    );
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.keys(b"y");
    s.expect(
        "C10    and back to one, saying which and that each was resized",
        "One agent on screen: waiter (work). Every agent was told its new size.",
    );
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.expect("       asked again", "Resize every agent now? y / n");
    s.resize(100, ROWS);
    // Time for Lillignode to learn the window's new width, as it has when
    // a person does this by hand.
    s.pump_for(Duration::from_millis(1500));
    s.keys(b"y");
    s.expect(
        "C11    narrowed before the answer: yes is refused, with the widths, and nothing changed",
        "This window is too narrow for two agents side by side: it is 100 columns wide and that takes 109. Nothing \
         was changed.",
    );
    // The hand check has two agents open at this step. A third was started
    // here to be the one that waits: one of the other two is closed first.
    s.raw(CTRL_Q);
    s.keys(b"	");
    s.keys(ESC);
    s.raw(CTRL_Q);
    s.keys(b"x");
    s.expect("       closing one of the three asks", "? y / n");
    s.keys(b"y");
    s.expect("       and it is closed", "and removed it from your workspace.");
    s.resize(140, ROWS);
    s.raw(CTRL_Q);
    s.keys(b"v");
    s.keys(b"y");
    s.expect("       the two that are left, side by side", "are side by side");
    s.raw(CTRL_Q);
    s.keys(b"x");
    s.expect("       closing one of the two asks", "? y / n");
    s.keys(b"y");
    s.expect(
        "C12    closing one of two says it was closed and removed",
        "and removed it from your workspace.",
    );
    s.expect("C12    and that the one left has the whole screen", "has the whole screen again.");
    s.never("C12    with no heading for a second agent", "TYPING GOES HERE");
    drop(s.quit("       quit", true));
}

fn parts(sb: &Sandbox, standin: &std::path::Path) {
    println!("parts. A waiting part of a program shows as Needs you");
    launcher(&sb.launchers, standin, "helper", "parts");
    launcher(&sb.launchers, standin, "other", "parts");
    let state = sb.root.join("state-parts");
    let mut s = Session::start_in(sb, &state);
    s.new_agent("helper");
    s.expect("       an agent that reports for itself and for its parts, on keys", "STANDIN READY parts");
    let sent = |s: &Session, key: char| s.screen().matches(&format!("SENT {key}")).count();
    let rung = |s: &Session, from: usize| bells(s) - from;

    s.keys(b"o");
    s.expect("1      its own report, done: the row reads Done", "work  Done ");
    let start = bells(&s);
    s.keys(b"1");
    s.expect("2      a part that is blocked: the row reads NEEDS YOU", "work  NEEDS YOU   ");
    s.expect("2      and the heading counts it", " Lillignode  1 NEEDS YOU");
    check("2      with one bell", rung(&s, start) == 1, &format!("{}", rung(&s, start)));
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("3      the status report says what for", "Status: Needs you: permission.");
    s.expect(
        "3      and that the program itself is done and a part of it waits",
        "Its own status: Done. One of its parts is waiting for you",
    );
    s.expect(
        "4      and names the part, its id, what for and since when",
        "\"subagent: tests\" (id sub/1): waiting for you (permission), waiting since less than a minute ago",
    );
    s.expect(
        "5      the bottom line names the key that stops showing it",
        "d: stop showing its waiting parts",
    );
    s.keys(b"d");
    s.expect("6      d asks first, keys first", "Stop showing 1 waiting part of helper (work)? y / n");
    s.expect("6      and names the part", "subagent: tests");
    s.keys(b"n");
    s.expect("7      n leaves it, and says so", "Left as it was");
    s.expect("7      and the row still reads NEEDS YOU", "work  NEEDS YOU   ");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.keys(b"d");
    s.keys(b"y");
    s.expect("8      y stops showing it, and says so", "Stopped showing 1 waiting part");
    s.expect("8      and the row reads Done", "work  Done ");
    s.never("8      the keys were not typed into the agent", "SENT d");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("9      the part is still listed, as hidden at the user's word", "no longer shown at your word");
    s.keys(b"z");
    s.expect_gone("9      any key closes the report", "STATUS REPORT");
    let before = sent(&s, '1');
    s.keys(b"1");
    once_more(&mut s, "10     the same report again", "SENT 1", before);
    s.never("10     keeps it hidden: the row does not read NEEDS YOU", "NEEDS YOU");
    check("10     and no bell", rung(&s, start) == 1, &format!("{}", rung(&s, start)));
    s.keys(b"a");
    s.expect("10b    the part says something else", "SENT a");
    s.keys(b"1");
    s.expect("10b    then waits again: NEEDS YOU is back", "work  NEEDS YOU   ");
    check("10b    with one bell", rung(&s, start) == 2, &format!("{}", rung(&s, start)));
    s.keys(b"2");
    s.expect("11     a second part waits", "SENT 2");
    s.expect("11     the row still reads NEEDS YOU", "work  NEEDS YOU   ");
    check("11     and no second bell", rung(&s, start) == 2, &format!("{}", rung(&s, start)));
    s.new_agent("other");
    s.expect(
        "12     with another agent in front, the notice says two parts wait, and the longest wait",
        "helper (work) needs you: 2 of its parts are waiting, the longest waiting since",
    );
    s.raw(CTRL_Q);
    s.keys(b"\t");
    s.keys(ESC);
    s.expect("13     back at the first agent", "Typing goes to helper (work).");
    s.keys(b"c");
    s.expect("14     withdrawing what is above them", "SENT c");
    s.expect_gone("14     ends both parts: no NEEDS YOU", "NEEDS YOU");
    s.expect("14     and the row reads Done", "work  Done ");
    s.keys(b"1");
    s.expect("15     a part waits again", " Lillignode  1 NEEDS YOU");
    println!("        (waiting 65 seconds: a waiting part is not ended by time)");
    s.pump_for(Duration::from_secs(65));
    check(
        "15     after a minute and more the row still reads NEEDS YOU",
        s.shows("work  NEEDS YOU   "),
        &s.screen(),
    );
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("16     the status report says how long it has waited", "waiting since 1 minute ago");
    s.keys(b"z");
    s.raw(CTRL_Q);
    s.keys(b"q");
    s.expect("17     quit asks", "Quit and stop 2 running agents? y / n");
    s.expect("17     and names this agent as needing you", "helper (work) (Needs you: permission)");
    s.keys(b"n");
    s.keys(b"x");
    s.expect("18     a withdrawal with no id", "SENT x");
    s.expect_gone("18     ends the parts too: no NEEDS YOU", "NEEDS YOU");
    s.expect("18     and the row reads Unknown", "work  Unknown");
    s.raw(CTRL_Q);
    s.keys(b"s");
    s.expect("18     because it withdrew its status", "withdrew its status and has not reported a new one");
    s.keys(b"z");
    drop(s.quit("       quit", true));
}

fn main() {
    let (sb, standin) = sandbox("handcheck");
    println!("lillignode: {}\nthrowaway folder: {}\n", sb.exe.display(), sb.root.display());
    launcher(&sb.launchers, &standin, "plain", "echo-plain");
    launcher(&sb.launchers, &standin, "lines", "echo-plain");
    since_paste(&sb);
    since_fixes(&sb, &standin);
    since_two(&sb, &standin);
    parts(&sb, &standin);
    let _ = std::fs::remove_dir_all(&sb.root);
    println!("\nAll checks passed.");
}
