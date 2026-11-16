//! The terminal an agent believes it is running in.
//!
//! Besides keeping the screen, a terminal has to answer questions programs
//! ask it. If it stays silent the program waits: on Windows the
//! pseudo-terminal asks for the cursor position before it lets the agent
//! start, so an unanswered question means a blank agent forever.

use std::num::NonZeroU16;

use crate::status;

/// How many rows that have scrolled off the top are kept per agent. Held in
/// memory only, never written to disk, and gone when the agent is closed.
pub const HISTORY_ROWS: usize = 5000;

/// A status message from the agent, in the order it arrived.
#[derive(Debug, PartialEq, Eq)]
pub enum StatusMessage {
    /// The agent asked whether this terminal takes status reports. It has
    /// already been answered yes.
    Asked,
    /// A report. Everything after `7501;`, as received.
    Report(Vec<u8>),
}

#[derive(Default)]
struct Replies {
    pending: Vec<u8>,
    status: Vec<StatusMessage>,
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            // "Where is the cursor?"
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.pending.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            // "Are you there?"
            (None, 'n', 5) => self.pending.extend_from_slice(b"\x1b[0n"),
            // "What kind of terminal are you?" A VT100 with advanced video.
            (None, 'c', 0) => self.pending.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        let Some((&code, rest)) = params.split_first() else { return };
        if code != status::CODE {
            return;
        }
        if rest == [b"?"] {
            // Answered here, in line with any other question, because Claude
            // Code takes "no" if a later question is answered first.
            self.pending.extend_from_slice(status::ANSWER);
            self.status.push(StatusMessage::Asked);
        } else {
            // The parser splits on ';'. A report has none, but if one ever
            // does it is put back as it was, to be read (and refused) whole.
            self.status.push(StatusMessage::Report(rest.join(&b';')));
        }
    }
}

pub struct Terminal {
    parser: vt100::Parser<Replies>,
}

fn nz(n: u16) -> NonZeroU16 {
    // The emulator refuses a zero size, so a window squeezed to nothing is
    // treated as one cell.
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

impl Terminal {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), HISTORY_ROWS, Replies::default()),
        }
    }

    /// Take in output from the agent. Returns anything that must be sent
    /// back to the agent in answer; empty most of the time.
    #[must_use = "replies must be sent to the agent or it may wait forever"]
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.process(bytes);
        std::mem::take(&mut self.parser.callbacks_mut().pending)
    }

    /// Status messages received since the last call, oldest first.
    pub fn take_status(&mut self) -> Vec<StatusMessage> {
        std::mem::take(&mut self.parser.callbacks_mut().status)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(nz(rows), nz(cols));
    }

    /// How many rows back from the present the view is. Zero means live.
    pub fn scrolled_back(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// Move the view `rows` further back (positive) or forward (negative).
    /// Stops at the oldest kept row and at the present.
    pub fn scroll(&mut self, rows: isize) {
        let now = self.scrolled_back() as isize;
        self.parser.screen_mut().set_scrollback((now + rows).max(0) as usize);
    }

    pub fn scroll_to_oldest(&mut self) {
        self.parser.screen_mut().set_scrollback(usize::MAX);
    }

    pub fn scroll_to_live(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    /// True while the agent has switched to a full-screen display. Such a
    /// display redraws in place, so nothing scrolls off and there is no
    /// history to keep.
    pub fn full_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// True if the agent asked for pastes to arrive as one marked block.
    pub fn takes_pastes_whole(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a narrower size costs, written down so nobody has to guess:
    /// what is on screen beyond the new width is gone until the agent draws
    /// it again, and making the terminal wide again does not bring it back.
    /// Lines that had already scrolled into history keep their full width.
    #[test]
    fn narrowing_cuts_what_is_on_screen_and_history_keeps_its_lines_whole() {
        let mut t = Terminal::new(3, 20);
        for i in 1..=6 {
            let _ = t.feed(format!("line {i} abcdefghijklm\r\n").as_bytes());
        }
        let _ = t.feed(b"on screen 0123456789");
        t.resize(3, 10);
        t.resize(3, 20);
        let screen: Vec<String> = t.screen().rows(0, 20).collect();
        assert_eq!(screen, ["line 5 abc", "line 6 abc", "on screen "]);
        t.scroll_to_oldest();
        let oldest: Vec<String> = t.screen().rows(0, 20).collect();
        assert_eq!(oldest[0], "line 1 abcdefghijklm");
    }

    #[test]
    fn answers_the_cursor_position_question() {
        let mut t = Terminal::new(24, 80);
        assert!(t.feed(b"hello").is_empty());
        assert_eq!(t.feed(b"\x1b[6n"), b"\x1b[1;6R");
        // A question split across two reads is still answered, once.
        assert!(t.feed(b"\r\n\x1b[6").is_empty());
        assert_eq!(t.feed(b"n"), b"\x1b[2;1R");
        assert!(t.feed(b"x").is_empty());
    }

    #[test]
    fn rows_that_scroll_off_can_be_read_back_and_the_view_holds_still() {
        let mut t = Terminal::new(3, 20);
        for i in 1..=10 {
            let _ = t.feed(format!("line {i}\r\n").as_bytes());
        }
        let top = |t: &Terminal| t.screen().rows(0, 20).next().unwrap();
        assert_eq!(top(&t), "line 9");
        t.scroll(2);
        assert_eq!(top(&t), "line 7");
        // New output must not move what the user is reading.
        let _ = t.feed(b"line 11\r\n");
        assert_eq!(top(&t), "line 7");
        t.scroll_to_oldest();
        assert_eq!(top(&t), "line 1");
        t.scroll(-1000);
        assert_eq!(t.scrolled_back(), 0);
        assert_eq!(top(&t), "line 10");
    }

    #[test]
    fn a_full_screen_display_keeps_no_history() {
        let mut t = Terminal::new(3, 20);
        let _ = t.feed(b"\x1b[?1049h");
        assert!(t.full_screen());
        for i in 1..=10 {
            let _ = t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll_to_oldest();
        assert_eq!(t.scrolled_back(), 0);
        let _ = t.feed(b"\x1b[?1049l");
        assert!(!t.full_screen());
    }

    #[test]
    fn remembers_whether_the_agent_takes_pastes_whole() {
        let mut t = Terminal::new(3, 20);
        assert!(!t.takes_pastes_whole());
        let _ = t.feed(b"\x1b[?2004h");
        assert!(t.takes_pastes_whole());
    }

    #[test]
    fn answers_the_status_question_in_order_and_hands_over_reports() {
        let mut t = Terminal::new(24, 80);
        // Claude Code asks, then asks what kind of terminal this is. The
        // status answer must come first.
        assert_eq!(t.feed(b"\x1b]7501;?\x07\x1b[c"), b"\x1b]7501;?\x07\x1b[?1;2c");
        assert_eq!(t.take_status(), vec![StatusMessage::Asked]);
        assert!(t.take_status().is_empty());

        // Either ending, and a report split across reads.
        let _ = t.feed(b"\x1b]7501;state=idle:app=claude-code\x1b\\x");
        let _ = t.feed(b"\x1b]7501;state=work");
        assert!(t.feed(b"ing:app=claude-code\x07").is_empty());
        assert_eq!(
            t.take_status(),
            vec![
                StatusMessage::Report(b"state=idle:app=claude-code".to_vec()),
                StatusMessage::Report(b"state=working:app=claude-code".to_vec()),
            ]
        );
        // Nothing of it is drawn.
        assert_eq!(t.screen().rows(0, 80).next().unwrap(), "x");
    }

    #[test]
    fn a_long_report_arrives_whole() {
        let mut t = Terminal::new(24, 80);
        let body = format!("state=working:app=claude-code:msg={}", "QUJD".repeat(700));
        let _ = t.feed(format!("\x1b]7501;{body}\x07").as_bytes());
        assert_eq!(t.take_status(), vec![StatusMessage::Report(body.into_bytes())]);
    }

    #[test]
    fn other_terminal_messages_are_not_status() {
        let mut t = Terminal::new(24, 80);
        let _ = t.feed(b"\x1b]0;title\x07\x1b]9;4;1;50\x07\x1b]75010;?\x07");
        assert!(t.take_status().is_empty());
    }

    #[test]
    fn zero_size_does_not_panic() {
        let mut t = Terminal::new(0, 0);
        t.resize(0, 0);
        let _ = t.feed(b"x");
    }
}
