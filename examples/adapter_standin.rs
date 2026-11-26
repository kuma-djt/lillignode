//! Development tool, not part of the product: a stand-in for a Board
//! adapter, so the checks never speak to a real service.
//!
//!   adapter_standin <board.json> <log>
//!
//! Speaks version 1 of the pipe (`src/adapter.rs`): says hello as "the
//! stand-in Hub", sends the cards of `<board.json>` and two agents that
//! report through it, then writes each line it is sent to `<log>`, one a
//! line, and answers each action: taken, except an action on card `c5`,
//! which it refuses. It sends the cards again every second, so a change to
//! the file reaches the page as a change at the service would.
//!
//! `ADAPTER_STANDIN_SILENT=1` makes it say nothing at all.

use std::io::{BufRead, Write};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(board), Some(log)) = (args.first().cloned(), args.get(1).cloned()) else {
        eprintln!("adapter_standin <board.json> <log>");
        std::process::exit(2);
    };
    if std::env::var_os("ADAPTER_STANDIN_SILENT").is_some() {
        for _ in std::io::stdin().lock().lines() {}
        return;
    }
    let say = |line: String| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };
    say(serde_json::json!({ "hello": 1, "name": "the stand-in Hub" }).to_string());
    say(serde_json::json!({ "reporting": [
        { "agent": "ops-bot", "line": "Deployed the docs site.", "at": "2026-11-26T20:00:00Z" },
        { "agent": "night-runner", "line": "Waiting for the nightly build.", "at": "2026-11-26T19:30:00Z" },
    ] })
    .to_string());
    std::thread::spawn(move || loop {
        let cards = std::fs::read_to_string(&board)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .map(|b| b["cards"].clone())
            .unwrap_or_default();
        say(serde_json::json!({ "cards": cards }).to_string());
        std::thread::sleep(Duration::from_secs(1));
    });
    let say = |line: String| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            let _ = writeln!(f, "{line}");
        }
        let Ok(said) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(id) = said["id"].as_str() else { continue };
        if said["action"]["card"] == "c5" {
            say(serde_json::json!({ "refused": id, "why": "that card was closed at the service" })
                .to_string());
        } else {
            say(serde_json::json!({ "took": id }).to_string());
        }
    }
}
