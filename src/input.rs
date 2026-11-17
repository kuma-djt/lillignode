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
    Paste(Pasted),
    Resize { cols: u16, rows: u16 },
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

/// The control character a key held with Ctrl stands for: Ctrl+D is 0x04.
/// Inside pasted text a control character arrives as such a key.
fn control_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() => {
            Some(char::from(c.to_ascii_lowercase() as u8 - b'a' + 1))
        }
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

/// Text that was pasted, made safe to send. Line breaks are `\n`. It holds
/// no control character but the line break and the tab: the rest were taken
/// out, and are kept in `left_out`, in order, so the user can be told.
///
/// Taken out: everything below a space except line break and tab (Escape
/// among them, so a paste can never end itself early or speak to the
/// agent's terminal), Delete, and the one-character forms of the escape
/// sequences (U+0080 to U+009F). Characters that change the direction of
/// text are left in: they do nothing to the agent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pasted {
    pub text: String,
    pub left_out: Vec<char>,
}

impl From<&str> for Pasted {
    fn from(raw: &str) -> Self {
        let mut pasted = Pasted::default();
        for c in raw.replace("\r\n", "\n").replace('\r', "\n").chars() {
            if c.is_control() && c != '\n' && c != '\t' {
                pasted.left_out.push(c);
            } else {
                pasted.text.push(c);
            }
        }
        pasted
    }
}

impl From<String> for Pasted {
    fn from(raw: String) -> Self {
        Pasted::from(raw.as_str())
    }
}

/// How a control character is named to the user: by the key that types
/// it where there is one, otherwise by its number.
pub fn control_name(c: char) -> String {
    match c {
        '\x1b' => "Esc".to_string(),
        '\x7f' => "Backspace".to_string(),
        '\x00' => "Ctrl+@".to_string(),
        '\x01'..='\x1a' => format!("Ctrl+{}", char::from(b'A' + c as u8 - 1)),
        '\x1c'..='\x1f' => format!("Ctrl+{}", ['\\', ']', '^', '_'][c as usize - 0x1c]),
        c => format!("U+{:04X}", c as u32),
    }
}

fn clean(text: &str) -> Pasted {
    Pasted::from(text)
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
                        // A control character is kept as the character, so
                        // it is taken out and said with the rest.
                        if let Some(c) = control_char(&key).or_else(|| marker_char(&key)) {
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
///
/// Text with a line break cannot be typing. Nor can text with a control key
/// in it: a terminal that does not mark pastes sends the paste's control
/// characters as keys held with Ctrl, in the same burst. Control keys alone
/// (Ctrl+C held down, repeating) are still keys.
fn flush_run(run: &mut Vec<KeyEvent>, out: &mut Vec<Input>) {
    let text: Option<String> = run.iter().map(|k| control_char(k).or_else(|| as_text(k))).collect();
    let pasted = |text: &str| {
        let control = text.chars().any(|c| c.is_control() && c != '\n' && c != '\t');
        let words = text.chars().any(|c| !c.is_control());
        text.contains('\n') || (control && words)
    };
    match text {
        Some(text) if run.len() >= 2 && pasted(&text) => out.push(Input::Paste(clean(&text))),
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
        inputs
            .iter()
            .filter_map(|i| if let Input::Paste(t) = i { Some(t.text.as_str()) } else { None })
            .collect()
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
        assert_eq!(out, vec![Input::Paste(Pasted { text: "a[201~b\nc".into(), left_out: vec!['\x1b'] })]);
    }

    #[test]
    fn a_control_key_inside_a_marked_paste_is_counted_as_left_out() {
        let mut d = PasteDetector::default();
        let mut events = typed("\x1b[200~ab");
        events.push(Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        events.extend(typed("cd\x1b[201~"));
        let out = d.push_batch(events);
        assert_eq!(out, vec![Input::Paste(Pasted { text: "abcd".into(), left_out: vec!['\x03'] })]);
    }

    #[test]
    fn an_unmarked_burst_with_a_control_key_is_a_paste_and_the_key_is_left_out() {
        let ctrl = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        let mut d = PasteDetector::default();
        // One line, so no line break gives it away: the control key does.
        let mut events = typed("ab");
        events.push(ctrl('d'));
        events.extend(typed("cd"));
        assert_eq!(
            d.push_batch(events),
            vec![Input::Paste(Pasted { text: "abcd".into(), left_out: vec!['\x04'] })]
        );
        // Several lines with a control key: still one paste, no Enter keys.
        let mut events = typed("one");
        events.push(ctrl('C'));
        events.extend(typed("\ntwo"));
        let out = d.push_batch(events);
        assert_eq!(enter_keys(&out), 0);
        assert_eq!(pastes(&out), vec!["one\ntwo"]);
        // Ctrl+C held down is the key, every time, never a paste.
        let out = d.push_batch(vec![ctrl('c'), ctrl('c'), ctrl('c')]);
        assert_eq!(out.len(), 3);
        assert!(pastes(&out).is_empty());
    }

    #[test]
    fn control_characters_are_taken_out_and_kept_to_be_named() {
        let p = Pasted::from("a\x03b\tc\x04\x7f\u{9b}d\u{202e}e\n");
        assert_eq!(p.text, "ab\tcd\u{202e}e\n", "line break, tab and direction marks stay");
        assert_eq!(p.left_out, ['\x03', '\x04', '\x7f', '\u{9b}']);
        let names: Vec<String> = p.left_out.iter().copied().map(control_name).collect();
        assert_eq!(names, ["Ctrl+C", "Ctrl+D", "Backspace", "U+009B"]);
        assert_eq!(control_name('\x1b'), "Esc");
        assert_eq!(control_name('\x00'), "Ctrl+@");
        assert_eq!(control_name('\x1a'), "Ctrl+Z");
        assert_eq!(control_name('\x1f'), "Ctrl+_");
        // A carriage return is a line break, not a control left out.
        assert_eq!(Pasted::from("a\rb\r\nc"), Pasted { text: "a\nb\nc".into(), left_out: vec![] });
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
