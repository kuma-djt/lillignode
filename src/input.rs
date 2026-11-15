//! Reading the keyboard, and telling typing from pasting.
//!
//! A paste must reach an agent as one paste. If its line breaks arrived as
//! Enter presses, an agent would take the first line as a finished request
//! and act on it. Two independent checks keep that from happening:
//!
//! 1. Marked pastes. Lillignode asks the terminal to wrap each paste in start
//!    and end markers. Where the terminal does, the paste is exact. On
//!    Windows the markers arrive as individual key presses, so they are
//!    recognised here rather than by the input library.
//! 2. Bursts. Where a terminal does not mark pastes, its keys still arrive
//!    all at once. Several text keys read in one go that include an Enter
//!    cannot be typing, so they are treated as a paste too.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, PartialEq)]
pub enum Input {
    Key(KeyEvent),
    /// Text that was pasted. Line breaks are `\n`. Never contains Escape.
    Paste(String),
    Resize {
        cols: u16,
        rows: u16,
    },
}

const START: &str = "\x1b[200~";
const END: &str = "\x1b[201~";

/// How long to wait for the rest of a marker before treating a lone Escape
/// as the Escape key.
pub const MARKER_WAIT: Duration = Duration::from_millis(25);
/// How long a marked paste may stall before what has arrived is delivered.
pub const PASTE_WAIT: Duration = Duration::from_secs(2);

/// The character a key stands for inside pasted text, if any.
fn as_text(key: &KeyEvent) -> Option<char> {
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Char(c) => Some(c),
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        _ => None,
    }
}

/// The character a key contributes when matching a marker.
fn marker_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Esc => Some('\x1b'),
        _ => as_text(key),
    }
}

fn clean(text: &str) -> String {
    // Escape is removed so pasted text can never contain an end marker or
    // any other instruction to the agent's terminal.
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\x1b', "")
}

#[derive(Default)]
pub struct PasteDetector {
    /// Keys at the end of the last batch that may be the start of a marker.
    held: Vec<KeyEvent>,
    /// Text of a marked paste whose end has not arrived yet.
    paste: Option<String>,
}

impl PasteDetector {
    /// How long the reader should wait for more before calling `flush`.
    /// None means nothing is pending.
    pub fn waiting(&self) -> Option<Duration> {
        if self.paste.is_some() {
            Some(PASTE_WAIT)
        } else if !self.held.is_empty() {
            Some(MARKER_WAIT)
        } else {
            None
        }
    }

    /// Give up waiting: deliver whatever is pending as it stands.
    pub fn flush(&mut self) -> Vec<Input> {
        let mut out = Vec::new();
        if let Some(text) = self.paste.take() {
            out.push(Input::Paste(clean(&text)));
        }
        out.extend(std::mem::take(&mut self.held).into_iter().map(Input::Key));
        out
    }

    /// Take everything that was read in one go.
    pub fn push_batch(&mut self, events: Vec<Event>) -> Vec<Input> {
        let mut out = Vec::new();
        let mut run: Vec<KeyEvent> = std::mem::take(&mut self.held);
        for event in events {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Release => {}
                Event::Key(key) => {
                    if let Some(text) = &mut self.paste {
                        if let Some(c) = marker_char(&key) {
                            text.push(c);
                            if text.ends_with(END) {
                                text.truncate(text.len() - END.len());
                                out.push(Input::Paste(clean(text)));
                                self.paste = None;
                            }
                        }
                        continue;
                    }
                    run.push(key);
                    if ends_with_marker(&run, START.len()) {
                        run.truncate(run.len() - START.len());
                        flush_run(&mut run, &mut out);
                        self.paste = Some(String::new());
                    }
                }
                // Where the input library understands marked pastes itself.
                Event::Paste(text) => {
                    flush_run(&mut run, &mut out);
                    out.push(Input::Paste(clean(&text)));
                }
                Event::Resize(cols, rows) => {
                    flush_run(&mut run, &mut out);
                    out.push(Input::Resize { cols, rows });
                }
                _ => {}
            }
        }
        // Keep back a tail that could be the beginning of a start marker.
        let tail = (1..START.len()).rev().find(|&n| ends_with_marker(&run, n)).unwrap_or(0);
        self.held = run.split_off(run.len() - tail);
        flush_run(&mut run, &mut out);
        out
    }
}

/// True if the last `n` keys spell the first `n` characters of the start marker.
fn ends_with_marker(run: &[KeyEvent], n: usize) -> bool {
    run.len() >= n && run[run.len() - n..].iter().map(marker_char).eq(START.chars().take(n).map(Some))
}

/// Deliver a run of keys read together: as a paste if it can only have been
/// one, otherwise key by key.
fn flush_run(run: &mut Vec<KeyEvent>, out: &mut Vec<Input>) {
    let text: Option<String> = run.iter().map(as_text).collect();
    match text {
        Some(text) if run.len() >= 2 && text.contains('\n') => out.push(Input::Paste(clean(&text))),
        _ => out.extend(run.iter().copied().map(Input::Key)),
    }
    run.clear();
}

/// Read the keyboard on its own thread and send what arrives, with the time
/// it was read.
pub fn spawn_reader<M: Send + 'static>(tx: Sender<M>, wrap: fn(Input, Instant) -> M) {
    thread::spawn(move || {
        let mut detector = PasteDetector::default();
        loop {
            let ready = match detector.waiting() {
                None => true,
                Some(wait) => event::poll(wait).unwrap_or(false),
            };
            let inputs = if ready {
                let Ok(first) = event::read() else { return };
                let mut batch = vec![first];
                // Everything already waiting belongs to the same moment.
                while batch.len() < 65_536 && event::poll(Duration::ZERO).unwrap_or(false) {
                    match event::read() {
                        Ok(ev) => batch.push(ev),
                        Err(_) => return,
                    }
                }
                detector.push_batch(batch)
            } else {
                detector.flush()
            };
            let now = Instant::now();
            for input in inputs {
                if tx.send(wrap(input, now)).is_err() {
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(text: &str) -> Vec<Event> {
        text.chars()
            .map(|c| match c {
                '\x1b' => key(KeyCode::Esc),
                '\n' => key(KeyCode::Enter),
                c => key(KeyCode::Char(c)),
            })
            .collect()
    }

    fn pastes(inputs: &[Input]) -> Vec<&str> {
        inputs.iter().filter_map(|i| if let Input::Paste(t) = i { Some(t.as_str()) } else { None }).collect()
    }

    fn enter_keys(inputs: &[Input]) -> usize {
        inputs.iter().filter(|i| matches!(i, Input::Key(k) if k.code == KeyCode::Enter)).count()
    }

    #[test]
    fn a_marked_paste_arrives_as_one_paste_with_no_enter_keys() {
        let mut d = PasteDetector::default();
        let out = d.push_batch(typed("\x1b[200~first\nsecond\nthird\x1b[201~"));
        assert_eq!(out, vec![Input::Paste("first\nsecond\nthird".into())]);
        assert_eq!(d.waiting(), None);
    }

    #[test]
    fn a_marked_paste_split_across_reads_is_still_one_paste() {
        let mut d = PasteDetector::default();
        let all = typed("ab\x1b[200~one\ntwo\x1b[201~c");
        let mut out = Vec::new();
        // Worst case: every key arrives in its own read.
        for ev in all {
            out.extend(d.push_batch(vec![ev]));
        }
        assert_eq!(pastes(&out), vec!["one\ntwo"]);
        assert_eq!(enter_keys(&out), 0);
        let keys: String = out
            .iter()
            .filter_map(|i| {
                if let Input::Key(KeyEvent { code: KeyCode::Char(c), .. }) = i {
                    Some(*c)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(keys, "abc");
    }

    #[test]
    fn an_unmarked_burst_with_a_line_break_is_a_paste() {
        let mut d = PasteDetector::default();
        let out = d.push_batch(typed("rm -rf build\nmake\n"));
        assert_eq!(out, vec![Input::Paste("rm -rf build\nmake\n".into())]);
    }

    #[test]
    fn ordinary_typing_is_keys() {
        let mut d = PasteDetector::default();
        assert_eq!(d.push_batch(typed("a")).len(), 1);
        // Enter on its own is the Enter key.
        let out = d.push_batch(typed("\n"));
        assert_eq!(enter_keys(&out), 1);
        // Several letters read together with no line break are still typing.
        assert_eq!(d.push_batch(typed("abc")).len(), 3);
        assert!(pastes(&d.push_batch(typed("abc"))).is_empty());
    }

    #[test]
    fn a_lone_escape_is_held_briefly_then_delivered_as_the_key() {
        let mut d = PasteDetector::default();
        assert!(d.push_batch(typed("\x1b")).is_empty());
        assert_eq!(d.waiting(), Some(MARKER_WAIT));
        let out = d.flush();
        assert!(matches!(out[..], [Input::Key(KeyEvent { code: KeyCode::Esc, .. })]));
        assert_eq!(d.waiting(), None);
    }

    #[test]
    fn a_paste_that_never_ends_is_delivered_as_a_paste_not_as_keys() {
        let mut d = PasteDetector::default();
        assert!(d.push_batch(typed("\x1b[200~half\nway")).is_empty());
        assert_eq!(d.waiting(), Some(PASTE_WAIT));
        assert_eq!(d.flush(), vec![Input::Paste("half\nway".into())]);
    }

    #[test]
    fn pasted_text_cannot_carry_an_escape() {
        let mut d = PasteDetector::default();
        let out = d.push_batch(vec![Event::Paste("a\x1b[201~b\r\nc".into())]);
        assert_eq!(out, vec![Input::Paste("a[201~b\nc".into())]);
    }

    #[test]
    fn key_releases_are_ignored_and_resizes_pass_through() {
        let mut d = PasteDetector::default();
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        let out = d.push_batch(vec![Event::Key(release), Event::Resize(100, 30)]);
        assert_eq!(out, vec![Input::Resize { cols: 100, rows: 30 }]);
    }
}
