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
    /// The agent's shell marked the start of a new prompt (`OSC 133 ; A`).
    /// A program that was reporting inside that shell has gone.
    Prompt,
}

/// The message number shells use to mark their prompts, and the mark for
/// the start of one.
const SHELL_MARK: &[u8] = b"133";
const PROMPT_START: &[u8] = b"A";

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
        if code == SHELL_MARK && rest.first() == Some(&PROMPT_START) {
            self.status.push(StatusMessage::Prompt);
            return;
        }
        if code != status::CODE {
            return;
        }
        if rest == [b"?"] {
            // Answered for every program: any of them may send reports.
            // Answered here, in line with any other question, because
            // Claude Code takes "no" if a later question is answered first.
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
    /// When anything on the screen last changed, by what the agent sent:
    /// its text, a colour, or the cursor.
    changed_at: Option<std::time::SystemTime>,
    /// When its text or colours last changed. The cursor moving, or being
    /// shown and hidden as a blinking cursor is, does not count.
    text_changed_at: Option<std::time::SystemTime>,
}

/// A colour of text or of what is behind it, as the agent asked for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tint {
    /// The terminal's own.
    #[default]
    Default,
    /// One of the 256 numbered colours.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for Tint {
    fn from(color: vt100::Color) -> Self {
        match color {
            vt100::Color::Default => Tint::Default,
            vt100::Color::Idx(i) => Tint::Indexed(i),
            vt100::Color::Rgb(r, g, b) => Tint::Rgb(r, g, b),
        }
    }
}

/// How a stretch of text is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    pub fg: Tint,
    pub bg: Tint,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// Text on one row that is all drawn the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub drawn: Drawn,
}

/// What a terminal is showing, as plain text and how it is drawn: nothing
/// of the emulator, so it can be handed to another thread or written out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenCopy {
    pub rows: u16,
    pub cols: u16,
    /// One for each row. Plain blank space at the end of a row is left out.
    pub lines: Vec<Vec<Run>>,
    /// Row and column of the cursor, when it is shown.
    pub cursor: Option<(u16, u16)>,
    /// The view is scrolled back: this is history, not the live screen.
    pub scrolled_back: bool,
}

fn nz(n: u16) -> NonZeroU16 {
    // The emulator refuses a zero size, so a window squeezed to nothing is
    // treated as one cell.
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

impl Terminal {
    /// A terminal for any program. It answers the status question and
    /// hands over status reports, whoever is running in it.
    pub fn new(rows: u16, cols: u16) -> Self {
        let replies = Replies::default();
        Self {
            parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), HISTORY_ROWS, replies),
            changed_at: None,
            text_changed_at: None,
        }
    }

    /// What is on the screen now, as two numbers: its text and colours,
    /// and its cursor (where it is, and whether it shows). Two screens that
    /// differ in one give different numbers for it.
    fn drawn(&self) -> (u64, (u16, u16, bool)) {
        use std::hash::{Hash, Hasher};
        let screen = self.parser.screen();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Each row with its colours, and nothing of the cursor.
        for row in screen.rows_formatted(0, screen.size().1.get()) {
            row.hash(&mut hasher);
        }
        let (row, col) = screen.cursor_position();
        (hasher.finish(), (row, col, screen.hide_cursor()))
    }

    /// When the text or colours on the screen last changed because of
    /// something the agent sent. The cursor moving, or showing and hiding
    /// as it blinks, does not move it. None until the agent has drawn
    /// anything.
    pub fn text_changed_at(&self) -> Option<std::time::SystemTime> {
        self.text_changed_at
    }

    /// When anything on the screen last changed because of something the
    /// agent sent: text, colours or the cursor. Output that draws the same
    /// screen again, or draws nothing (a status report, an answer to a
    /// question), does not move it. None until the agent has changed the
    /// screen at all.
    pub fn screen_changed_at(&self) -> Option<std::time::SystemTime> {
        self.changed_at
    }

    /// Take in output from the agent. Returns anything that must be sent
    /// back to the agent in answer; empty most of the time.
    #[must_use = "replies must be sent to the agent or it may wait forever"]
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.feed_at(bytes, std::time::SystemTime::now())
    }

    /// As `feed`, at a time the caller gives.
    #[must_use = "replies must be sent to the agent or it may wait forever"]
    pub fn feed_at(&mut self, bytes: &[u8], now: std::time::SystemTime) -> Vec<u8> {
        let (text_before, cursor_before) = self.drawn();
        self.parser.process(bytes);
        let (text, cursor) = self.drawn();
        if text != text_before {
            self.text_changed_at = Some(now);
        }
        if text != text_before || cursor != cursor_before {
            self.changed_at = Some(now);
        }
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

    /// What the terminal is showing now, as a plain copy.
    pub fn copy_screen(&self) -> ScreenCopy {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows.get(), cols.get());
        let mut lines = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            let mut runs: Vec<Run> = Vec::new();
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else { continue };
                // The second half of a double-width letter: already drawn.
                if cell.is_wide_continuation() {
                    continue;
                }
                let drawn = Drawn {
                    fg: cell.fgcolor().into(),
                    bg: cell.bgcolor().into(),
                    bold: cell.bold(),
                    dim: cell.dim(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                };
                let text = if cell.has_contents() { cell.contents() } else { " " };
                match runs.last_mut() {
                    Some(run) if run.drawn == drawn => run.text.push_str(text),
                    _ => runs.push(Run { text: text.to_string(), drawn }),
                }
            }
            // Blank space at the end, drawn plainly, says nothing.
            if let Some(last) = runs.last_mut().filter(|run| run.drawn == Drawn::default()) {
                last.text.truncate(last.text.trim_end_matches(' ').len());
                if last.text.is_empty() {
                    runs.pop();
                }
            }
            lines.push(runs);
        }
        let scrolled_back = screen.scrollback() > 0;
        let cursor = (!screen.hide_cursor() && !scrolled_back).then(|| screen.cursor_position());
        ScreenCopy { rows, cols, lines, cursor, scrolled_back }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_time_the_screen_changed_moves_only_when_what_is_on_it_does() {
        use std::time::{Duration, SystemTime};
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let mut t = Terminal::new(5, 20);
        assert_eq!(t.screen_changed_at(), None);
        // A question to the terminal and a status report draw nothing.
        let _ = t.feed_at(b"\x1b[6n\x1b]7501;state=working\x07", at(1));
        assert_eq!(t.screen_changed_at(), None);
        let _ = t.feed_at(b"working", at(2));
        assert_eq!(t.screen_changed_at(), Some(at(2)));
        // The same line drawn over itself, as a program redrawing does.
        let _ = t.feed_at(b"\rworking", at(3));
        assert_eq!(t.screen_changed_at(), Some(at(2)));
        // A spinner's next frame is a change; so is the cursor moving, a
        // colour, and the cursor being hidden.
        for (n, bytes) in
            [&b"\r|orking"[..], b"\x1b[2;1H", b"\x1b[1;1H\x1b[31m|\x1b[0m\x1b[2;1H", b"\x1b[?25l"]
                .iter()
                .enumerate()
        {
            let _ = t.feed_at(bytes, at(10 + n as u64));
            assert_eq!(t.screen_changed_at(), Some(at(10 + n as u64)), "{n}");
        }
        // Reading history is not the agent's doing, and does not make the
        // next output that changes nothing look like a change.
        let _ = t.feed_at(b"\x1b[?25h", at(20));
        t.scroll(1);
        let _ = t.feed_at(b"\x1b]7501;state=idle\x07", at(21));
        assert_eq!(t.screen_changed_at(), Some(at(20)));
    }

    #[test]
    fn the_text_has_its_own_time_which_the_cursor_does_not_move() {
        use std::time::{Duration, SystemTime};
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let mut t = Terminal::new(5, 20);
        assert_eq!((t.text_changed_at(), t.screen_changed_at()), (None, None));
        let _ = t.feed_at(b"working", at(1));
        assert_eq!((t.text_changed_at(), t.screen_changed_at()), (Some(at(1)), Some(at(1))));
        // The cursor moving, and blinking (hidden, shown, hidden): only the
        // time anything changed moves.
        for (n, bytes) in [&b"\x1b[3;5H"[..], b"\x1b[?25l", b"\x1b[?25h", b"\x1b[?25l"].iter().enumerate() {
            let _ = t.feed_at(bytes, at(10 + n as u64));
            assert_eq!(t.text_changed_at(), Some(at(1)), "{n}: the text did not change");
            assert_eq!(t.screen_changed_at(), Some(at(10 + n as u64)), "{n}");
        }
        // A colour alone is a change of the text's time: the same letter,
        // now red.
        let _ = t.feed_at(b"\x1b[1;1H\x1b[31mw\x1b[0m", at(20));
        assert_eq!(t.text_changed_at(), Some(at(20)));
        // A letter.
        let _ = t.feed_at(b"\x1b[1;8H!", at(21));
        assert_eq!((t.text_changed_at(), t.screen_changed_at()), (Some(at(21)), Some(at(21))));
        // The same drawn again, cursor where it was: neither.
        let _ = t.feed_at(b"\x1b[1;8H!", at(22));
        assert_eq!((t.text_changed_at(), t.screen_changed_at()), (Some(at(21)), Some(at(21))));
    }

    fn line_text(copy: &ScreenCopy, row: usize) -> String {
        copy.lines[row].iter().map(|run| run.text.as_str()).collect()
    }

    #[test]
    fn the_copy_of_a_screen_is_its_text_in_runs_drawn_the_same_way() {
        let mut t = Terminal::new(4, 20);
        let _ = t.feed("plain \x1b[1;31mred bold\x1b[m after\r\n日本 ok\r\n\x1b[44m  \x1b[m".as_bytes());
        let copy = t.copy_screen();
        assert_eq!((copy.rows, copy.cols, copy.lines.len()), (4, 20, 4));
        assert_eq!(line_text(&copy, 0), "plain red bold after", "blank space at the end is left out");
        let runs = &copy.lines[0];
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert_eq!(runs[0].drawn, Drawn::default());
        assert_eq!(runs[1].text, "red bold");
        assert_eq!((runs[1].drawn.fg, runs[1].drawn.bold), (Tint::Indexed(1), true));
        // A double-width letter is there once, not once for each column.
        assert_eq!(line_text(&copy, 1), "日本 ok");
        // Space that is drawn (a colour behind it) is kept.
        assert_eq!(copy.lines[2].len(), 1);
        assert_eq!((copy.lines[2][0].text.as_str(), copy.lines[2][0].drawn.bg), ("  ", Tint::Indexed(4)));
        assert!(copy.lines[3].is_empty());
        assert_eq!(copy.cursor, Some((2, 2)));
        assert!(!copy.scrolled_back);
    }

    #[test]
    fn the_copy_says_when_it_is_history_and_shows_no_cursor_then() {
        let mut t = Terminal::new(2, 20);
        for i in 1..=6 {
            let _ = t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(!t.copy_screen().scrolled_back);
        t.scroll_to_oldest();
        let copy = t.copy_screen();
        assert!(copy.scrolled_back);
        assert_eq!(copy.cursor, None);
        assert_eq!(line_text(&copy, 0), "line 1");
        // A hidden cursor is not shown either.
        let mut t = Terminal::new(2, 20);
        let _ = t.feed(b"\x1b[?25l");
        assert_eq!(t.copy_screen().cursor, None);
    }

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
    fn a_program_that_reports_without_asking_has_its_reports_handed_over() {
        let mut t = Terminal::new(24, 80);
        // Either ending. Codex ends its reports the second way.
        assert!(t.feed(b"\x1b]7501;state=idle:app=codex\x1b\\").is_empty());
        assert!(t.feed(b"\x1b]7501;state=blocked:app=terraform:kind=permission\x07x").is_empty());
        assert_eq!(
            t.take_status(),
            vec![
                StatusMessage::Report(b"state=idle:app=codex".to_vec()),
                StatusMessage::Report(b"state=blocked:app=terraform:kind=permission".to_vec()),
            ]
        );
        assert_eq!(t.screen().rows(0, 80).next().unwrap(), "x");
    }

    #[test]
    fn the_answer_to_the_status_question_carries_nothing_the_program_sent() {
        let mut t = Terminal::new(24, 80);
        let _ = t.feed(b"\x1b]7501;state=working:app=evil:msg=QUJD\x07");
        // A question with more after it is a report, not the question.
        assert!(t.feed(b"\x1b]7501;?;x\x07").is_empty());
        assert_eq!(t.feed(b"\x1b]7501;?\x07"), b"\x1b]7501;?\x07");
    }

    #[test]
    fn a_shell_marking_a_new_prompt_is_handed_over_in_its_place() {
        let mut t = Terminal::new(24, 80);
        let _ = t.feed(b"\x1b]7501;state=working\x07\x1b]133;A\x07\x1b]7501;state=done\x1b\\");
        // Either ending, with or without more after the mark.
        let _ = t.feed(b"\x1b]133;A;cl=m\x1b\\");
        assert_eq!(
            t.take_status(),
            vec![
                StatusMessage::Report(b"state=working".to_vec()),
                StatusMessage::Prompt,
                StatusMessage::Report(b"state=done".to_vec()),
                StatusMessage::Prompt,
            ]
        );
        // The shell's other marks are not the start of a prompt.
        let _ = t.feed(b"\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133\x07\x1b]1337;A\x07");
        assert!(t.take_status().is_empty());
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
