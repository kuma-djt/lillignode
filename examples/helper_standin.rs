//! A stand-in advisory helper, for checks. It reads Lillignode's questions
//! (one line of JSON each) and answers by a rule anyone can predict, from
//! the last line of text on the agent's screen:
//!
//!   ends with "?" or "(y/n)"   waiting
//!   ends with "..."            working
//!   anything else              done
//!
//! Its first argument changes how it answers:
//!
//!   read     as above (the default)
//!   unsure   always "unsure"
//!   late     as above, but four seconds after the question
//!   quit     says nothing and ends after hello
//!   noise    a line that is not an answer before each answer
//!
//! It writes what it was asked to the file named by HELPER_STANDIN_LOG, if
//! set, so a check can see what left Lillignode.

use std::io::{BufRead, Write};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "read".to_string());
    let log = std::env::var_os("HELPER_STANDIN_LOG");
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if let Some(path) = &log {
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{line}");
            }
        }
        let Ok(said) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if said.get("hello").is_some() {
            if mode == "quit" {
                return;
            }
            continue;
        }
        let Some(ask) = said.get("ask").and_then(serde_json::Value::as_u64) else { continue };
        let last = said["screen"]
            .as_array()
            .and_then(|lines| lines.iter().rev().filter_map(|l| l.as_str()).find(|l| !l.trim().is_empty()))
            .unwrap_or("")
            .trim()
            .to_string();
        let guess = match mode.as_str() {
            "unsure" => "unsure",
            _ if last.ends_with('?') || last.ends_with("(y/n)") => "waiting",
            _ if last.ends_with("...") => "working",
            _ => "done",
        };
        if mode == "late" {
            std::thread::sleep(std::time::Duration::from_secs(4));
        }
        if mode == "noise" {
            let _ = writeln!(out, "thinking about it");
        }
        let _ = writeln!(out, "{}", serde_json::json!({ "answer": ask, "guess": guess }));
        let _ = out.flush();
    }
}
