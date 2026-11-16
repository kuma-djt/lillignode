//! What Claude Code says it is doing, read from its own status report.
//!
//! At start-up Claude Code asks the terminal it runs in whether it takes
//! status reports (`ESC ] 7501 ; ? BEL`). Lillignode's terminal says yes, and
//! from then on Claude Code sends one report each time its status changes:
//!
//!   ESC ] 7501 ; state=working:app=claude-code:msg=<base64> BEL
//!
//! The format is not published. It was read from Claude Code 2.1.325 on
//! 2026-11-14. Lillignode repeats what the report says and
//! adds nothing: a report it does not understand in full is Unknown, never
//! the nearest match.

use std::time::{Duration, Instant, SystemTime};

/// The terminal message number Claude Code uses for status.
pub const CODE: &[u8] = b"7501";

/// What Lillignode sends back when asked. Any reply beginning with `?` is a yes.
pub const ANSWER: &[u8] = b"\x1b]7501;?\x07";

/// An agent that has not reported its status this long after it was started
/// shows Unknown.
pub const STARTING_DEADLINE: Duration = Duration::from_secs(15);

/// A Working agent draws a moving indicator. One that has drawn nothing at
/// all for this long still shows Working, with how long it has been quiet.
pub const SILENCE: Duration = Duration::from_secs(60);

/// The Claude Code version this reading was checked against.
pub const CHECKED_WITH: &str = "2.1.325";

/// The switch that stops Claude Code reporting. If Lillignode itself was
/// started with it set, every agent inherits it.
pub const OFF_SWITCH: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";

/// The longest message shown from a report, in characters.
const MESSAGE_CHARS: usize = 200;

/// The longest report read. Claude Code cuts its message to 2,048 and its
/// title to 192 characters before encoding them, so a real report is well
/// under this. A longer one is refused unread, and only its start is kept.
pub const LONGEST_REPORT: usize = 8 * 1024;

/// What the agent is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Question,
    SignIn,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::SignIn => "sign in",
        }
    }
}

/// Why Lillignode cannot say what the agent is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// It never asked the status question.
    NotAsked,
    /// It asked and was answered, but sent no report.
    NoReport,
    /// The latest report was not understood in full.
    NotUnderstood,
    /// The agent withdrew its status.
    Withdrawn,
    /// Not Claude Code, so there is no report to read.
    NotClaudeCode,
    /// Saved as a kind of agent this Lillignode does not know.
    KindNotKnown,
}

impl Why {
    /// Reasons that mean Claude Code's reporting itself is not reaching
    /// Lillignode, as opposed to something this one agent did.
    pub fn is_breakage(self) -> bool {
        match self {
            Why::NotAsked | Why::NoReport | Why::NotUnderstood => true,
            Why::Withdrawn | Why::NotClaudeCode | Why::KindNotKnown => false,
        }
    }
}

/// The status shown for a Claude Code agent that is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Starting,
    Working,
    /// Claude Code's last report was Working and the agent has drawn
    /// nothing at all since, for this many whole minutes (at least one). It
    /// is still what Claude Code said; only the quiet is Lillignode's own.
    WorkingQuiet(u64),
    NeedsYou(Option<Kind>),
    Done,
    Idle,
    /// The last request failed; the agent is still open. Its own words, if any.
    Error(Option<String>),
    Unknown(Why),
}

impl AgentStatus {
    /// The words shown in the sidebar: the status alone, short enough not
    /// to be cut. The reason (what an agent needs, why status is unknown,
    /// an error's own words) is in the notice and the status report, where
    /// it is written in full (`text`, `why_long`).
    pub fn short(&self) -> String {
        match self {
            AgentStatus::NeedsYou(_) => "Needs you".into(),
            AgentStatus::Error(_) => "Error".into(),
            AgentStatus::Unknown(_) => "Unknown".into(),
            other => other.text(),
        }
    }

    /// The status in full, with its reason: the notice and the status
    /// report say this.
    pub fn text(&self) -> String {
        match self {
            AgentStatus::Starting => "Starting".into(),
            AgentStatus::Working => "Working".into(),
            AgentStatus::WorkingQuiet(minutes) => format!("Working, quiet {minutes} min"),
            AgentStatus::NeedsYou(Some(kind)) => format!("Needs you: {}", kind.word()),
            AgentStatus::NeedsYou(None) => "Needs you".into(),
            AgentStatus::Done => "Done".into(),
            AgentStatus::Idle => "Idle".into(),
            AgentStatus::Error(Some(msg)) => format!("Error: {msg}"),
            AgentStatus::Error(None) => "Error".into(),
            AgentStatus::Unknown(why) => format!("Unknown: {}", why_short(*why)),
        }
    }
}

fn why_short(why: Why) -> &'static str {
    match why {
        Why::NotAsked | Why::NoReport => "no report",
        Why::NotUnderstood => "not understood",
        Why::Withdrawn => "withdrawn",
        Why::NotClaudeCode => "not Claude Code",
        Why::KindNotKnown => "kind not known",
    }
}

/// The full sentence for an Unknown, for the status report panel.
pub fn why_long(why: Why) -> &'static str {
    match why {
        Why::NotAsked => "Claude Code did not ask Lillignode's terminal for status reports within 15 seconds of starting",
        Why::NoReport => "Claude Code asked for status reports and Lillignode answered, but no report arrived within 15 seconds of starting. The answer may not have reached it",
        Why::NotUnderstood => "Claude Code sent a status Lillignode does not understand",
        Why::Withdrawn => "Claude Code withdrew its status and has not reported a new one",
        Why::NotClaudeCode => "Lillignode shows what an agent is doing only for Claude Code. For any other agent it can tell only whether its program is running",
        Why::KindNotKnown => "This agent was saved as a kind of agent this Lillignode does not know, most likely by a newer Lillignode. It runs as a plain program, and Lillignode can tell only whether it is running. What was saved about it is kept",
    }
}

/// How one report was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The agent's own status, and the message that came with it.
    Status { status: AgentStatus, message: Option<String> },
    /// The agent withdrew its status.
    Cleared,
    /// About one of the agent's background tasks, not the agent itself.
    Task,
    /// Not understood, and why.
    NotUnderstood(String),
}

impl Reading {
    /// What the report means for the sidebar. None leaves it as it is.
    fn shown(&self) -> Option<AgentStatus> {
        match self {
            Reading::Status { status, .. } => Some(status.clone()),
            Reading::Cleared => Some(AgentStatus::Unknown(Why::Withdrawn)),
            Reading::Task => None,
            Reading::NotUnderstood(_) => Some(AgentStatus::Unknown(Why::NotUnderstood)),
        }
    }
}

/// Read the part of a report after `7501;`. Exact match or not understood:
/// every part must be one Lillignode knows, given once, with a value it can read.
pub fn read(body: &[u8]) -> Reading {
    match read_parts(body) {
        Ok(reading) => reading,
        Err(why) => Reading::NotUnderstood(why),
    }
}

#[derive(Default)]
struct Parts<'a> {
    state: Option<&'a str>,
    app: Option<&'a str>,
    id: Option<&'a str>,
    kind: Option<&'a str>,
    progress: Option<&'a str>,
    title: Option<&'a str>,
    msg: Option<&'a str>,
}

fn read_parts(body: &[u8]) -> Result<Reading, String> {
    let text = std::str::from_utf8(body).map_err(|_| "it is not text".to_string())?;
    let mut parts = Parts::default();
    for part in text.split(':') {
        let (key, value) =
            part.split_once('=').ok_or_else(|| format!("part \"{}\" has no value", shown(part)))?;
        let slot = match key {
            "state" => &mut parts.state,
            "app" => &mut parts.app,
            "id" => &mut parts.id,
            "kind" => &mut parts.kind,
            "progress" => &mut parts.progress,
            "title" => &mut parts.title,
            "msg" => &mut parts.msg,
            other => return Err(format!("it has a part Lillignode does not know: \"{}\"", shown(other))),
        };
        if slot.replace(value).is_some() {
            return Err(format!("part \"{key}\" is given twice"));
        }
    }
    let state = parts.state.ok_or("it has no state")?;

    if state == "clear" {
        // Withdrawing a status names no program, only, for a task, its id.
        if let Some(extra) = [("app", parts.app), ("kind", parts.kind), ("progress", parts.progress)]
            .iter()
            .chain(&[("title", parts.title), ("msg", parts.msg)])
            .find_map(|(k, v)| v.map(|_| *k))
        {
            return Err(format!("a withdrawal with a \"{extra}\" part"));
        }
        return Ok(if parts.id.is_some() { Reading::Task } else { Reading::Cleared });
    }

    match parts.app {
        Some("claude-code") => {}
        Some(other) => return Err(format!("it is from \"{}\", not Claude Code", shown(other))),
        None => return Err("it does not say which program sent it".into()),
    }
    if parts.kind.is_some() && state != "blocked" {
        return Err(format!("state \"{}\" with a kind", shown(state)));
    }
    if let Some(p) = parts.progress {
        let digits = !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit());
        if !digits || !matches!(p.parse::<u8>(), Ok(0..=100)) {
            return Err(format!("progress \"{}\" is not 0 to 100", shown(p)));
        }
    }
    if let Some(t) = parts.title {
        decode_text(t).ok_or("its title cannot be read")?;
    }
    let mut message = match parts.msg {
        Some(m) => Some(decode_text(m).ok_or("its message cannot be read")?),
        None => None,
    };
    let status = match state {
        "working" => AgentStatus::Working,
        "blocked" => AgentStatus::NeedsYou(match parts.kind {
            None => None,
            Some("permission") => Some(Kind::Permission),
            Some("question") => Some(Kind::Question),
            Some("auth") => Some(Kind::SignIn),
            Some(other) => {
                return Err(format!(
                    "it is waiting for \"{}\", a kind Lillignode does not know",
                    shown(other)
                ))
            }
        }),
        "done" => AgentStatus::Done,
        "idle" => AgentStatus::Idle,
        "error" => AgentStatus::Error(message.take()),
        other => return Err(format!("state \"{}\" is one Lillignode does not know", shown(other))),
    };
    if parts.id.is_some() {
        return Ok(Reading::Task);
    }
    Ok(Reading::Status { status, message })
}

/// Characters that draw nothing, or change the direction or joining of the
/// text around them, so the words seen are not the words sent. They become
/// spaces, like control characters.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// Outside text made safe to show inside a sentence: nothing hidden, and short.
fn shown(text: &str) -> String {
    let clean: String = text.chars().map(|c| if hidden(c) { ' ' } else { c }).collect();
    cut(clean.trim(), 40)
}

fn cut(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut s: String = text.chars().take(chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// A message from a report: base64 of UTF-8 text. Control characters become
/// spaces, runs of space become one, and it is cut to fit, so the agent's
/// words can never draw over the sidebar.
fn decode_text(b64: &str) -> Option<String> {
    let bytes = base64_decode(b64)?;
    let text = String::from_utf8(bytes).ok()?;
    let spaced: String = text.chars().map(|c| if hidden(c) { ' ' } else { c }).collect();
    let words: Vec<&str> = spaced.split_whitespace().collect();
    Some(cut(&words.join(" "), MESSAGE_CHARS))
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    // Exactly the standard form: padding only where it belongs, and no
    // leftover bits in the last character.
    let body = text.trim_end_matches('=');
    let padding = text.len() - body.len();
    let spare = body.len() % 4;
    let padding_fits = match spare {
        0 => padding == 0,
        2 => padding == 0 || padding == 2,
        3 => padding == 0 || padding == 1,
        _ => false,
    };
    if !padding_fits {
        return None;
    }
    if let Some(&last) = body.as_bytes().last() {
        let unused_bits = match spare {
            2 => 0b1111,
            3 => 0b11,
            _ => 0,
        };
        if value(last)? & unused_bits != 0 {
            return None;
        }
    }
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    for chunk in body.as_bytes().chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    Some(out)
}

/// The last report received, kept for the status report panel. In memory
/// only, never written to disk.
#[derive(Clone, Debug)]
pub struct LastReport {
    /// Everything after `ESC ]`, as received.
    pub raw: Vec<u8>,
    pub at: SystemTime,
    pub reading: Reading,
    /// False if it arrived before the agent asked the status question, in
    /// which case it was not used.
    pub after_question: bool,
}

/// Everything Lillignode knows about one agent's status since it was started.
/// Begins again whenever the agent is started; nothing is carried over.
#[derive(Clone, Debug)]
pub struct Track {
    started: Instant,
    /// How long after starting the agent asked the status question.
    pub asked_after: Option<Duration>,
    pub shown: AgentStatus,
    pub reports: u32,
    pub task_reports: u32,
    pub last: Option<LastReport>,
    /// When the agent last drew anything.
    last_output: Instant,
    /// Set when the status changes into Needs you, until taken.
    began_needing_you: bool,
}

impl Track {
    pub fn new(started: Instant) -> Self {
        Self {
            started,
            asked_after: None,
            shown: AgentStatus::Starting,
            reports: 0,
            task_reports: 0,
            last: None,
            last_output: started,
            began_needing_you: false,
        }
    }

    fn show(&mut self, status: AgentStatus) {
        let was = matches!(self.shown, AgentStatus::NeedsYou(_));
        if !was && matches!(status, AgentStatus::NeedsYou(_)) {
            self.began_needing_you = true;
        }
        self.shown = status;
    }

    /// True once for each change into Needs you, so the bell rings once.
    pub fn take_began_needing_you(&mut self) -> bool {
        std::mem::take(&mut self.began_needing_you)
    }

    /// The agent drew something. Never grants a status: it only ends the
    /// quiet of an agent whose last report was Working.
    pub fn output(&mut self, now: Instant) {
        self.last_output = now;
        if matches!(self.shown, AgentStatus::WorkingQuiet(_)) {
            self.shown = AgentStatus::Working;
        }
    }

    /// How long ago the agent was started.
    pub fn running_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }

    pub fn asked(&mut self, now: Instant) {
        if self.asked_after.is_none() {
            self.asked_after = Some(now.saturating_duration_since(self.started));
        }
    }

    /// A report arrived. `body` is everything after `7501;`.
    pub fn received(&mut self, body: &[u8], now: SystemTime) {
        let reading = if body.len() > LONGEST_REPORT {
            Reading::NotUnderstood(format!(
                "it is {} characters long, far longer than Claude Code sends",
                body.len()
            ))
        } else {
            read(body)
        };
        let after_question = self.asked_after.is_some();
        self.reports = self.reports.saturating_add(1);
        if reading == Reading::Task {
            self.task_reports = self.task_reports.saturating_add(1);
        }
        // No report, no status: one that arrives unasked is kept for the
        // panel but not shown.
        if after_question {
            if let Some(status) = reading.shown() {
                self.show(status);
            }
        }
        let kept = &body[..body.len().min(LONGEST_REPORT)];
        let mut raw = Vec::with_capacity(5 + kept.len());
        raw.extend_from_slice(b"7501;");
        raw.extend_from_slice(kept);
        self.last = Some(LastReport { raw, at: now, reading, after_question });
    }

    /// Call regularly. True if the status changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        match self.shown {
            AgentStatus::Starting if now.saturating_duration_since(self.started) >= STARTING_DEADLINE => {
                let why = if self.asked_after.is_some() { Why::NoReport } else { Why::NotAsked };
                self.show(AgentStatus::Unknown(why));
                true
            }
            // Still Working: that is what Claude Code last said, and only it
            // can say otherwise. The quiet is shown beside it, in whole
            // minutes, so a long quiet tool reads as one and a stuck agent
            // shows a growing number.
            AgentStatus::Working | AgentStatus::WorkingQuiet(_) => {
                let quiet = now.saturating_duration_since(self.last_output);
                let shown = if quiet >= SILENCE {
                    AgentStatus::WorkingQuiet(quiet.as_secs() / 60)
                } else {
                    AgentStatus::Working
                };
                let changed = shown != self.shown;
                self.shown = shown;
                changed
            }
            _ => false,
        }
    }
}

/// A report or question as received, shown with its control characters
/// named, so it can be read and copied.
pub fn visible(raw: &[u8]) -> String {
    let mut out = String::from("ESC ]");
    for &b in raw {
        match b {
            0x07 => out.push_str("BEL"),
            0x1b => out.push_str("ESC"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        // Test-only encoder, so the decoder is checked against something else.
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in text.as_bytes().chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
            for i in 0..=chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        while !out.len().is_multiple_of(4) {
            out.push('=');
        }
        out
    }

    fn status(body: &str) -> Reading {
        read(body.as_bytes())
    }

    fn is(body: &str, expected: AgentStatus) {
        match status(body) {
            Reading::Status { status, .. } => assert_eq!(status, expected, "{body}"),
            other => panic!("{body}: {other:?}"),
        }
    }

    #[test]
    fn reads_each_state_claude_code_sends() {
        is("state=working:app=claude-code", AgentStatus::Working);
        is("state=working:app=claude-code:progress=40", AgentStatus::Working);
        is("state=blocked:app=claude-code:kind=permission", AgentStatus::NeedsYou(Some(Kind::Permission)));
        is("state=blocked:app=claude-code:kind=question", AgentStatus::NeedsYou(Some(Kind::Question)));
        is("state=blocked:app=claude-code:kind=auth", AgentStatus::NeedsYou(Some(Kind::SignIn)));
        // Sent while one of Claude Code's own dialogs is open.
        is("state=blocked:app=claude-code", AgentStatus::NeedsYou(None));
        is("state=done:app=claude-code", AgentStatus::Done);
        is("state=idle:app=claude-code", AgentStatus::Idle);
        is("state=error:app=claude-code", AgentStatus::Error(None));
    }

    #[test]
    fn decodes_the_message_and_makes_it_safe_to_draw() {
        let body = format!("state=error:app=claude-code:msg={}", b64("rate limited,\r\n\x1b[2Jwait"));
        is(&body, AgentStatus::Error(Some("rate limited, [2Jwait".into())));
        let long = format!("state=working:app=claude-code:msg={}", b64(&"é".repeat(2000)));
        let Reading::Status { message: Some(m), .. } = status(&long) else { panic!() };
        assert_eq!(m.chars().count(), MESSAGE_CHARS);
        assert!(m.ends_with('…'));
    }

    #[test]
    fn anything_not_understood_in_full_is_not_understood() {
        for body in [
            "",
            "app=claude-code",
            "state=thinking:app=claude-code",
            "state=working",
            "state=working:app=other-agent",
            "state=working:app=claude-code:colour=red",
            "state=working:app=claude-code:state=done",
            "state=working:app=claude-code:kind=permission",
            "state=blocked:app=claude-code:kind=coffee",
            "state=working:app=claude-code:progress=101",
            "state=working:app=claude-code:progress=-1",
            "state=working:app=claude-code:msg=@@@",
            "state=working:app=claude-code:title=A",
            "state=working:app=claude-code:noequals",
            "state=clear:app=claude-code",
        ] {
            assert!(matches!(status(body), Reading::NotUnderstood(_)), "{body:?} was understood");
        }
        assert!(matches!(read(b"state=working:app=claude-code:msg=\xff"), Reading::NotUnderstood(_)));
    }

    #[test]
    fn hidden_and_direction_changing_characters_do_not_reach_the_screen() {
        let body = format!("state=error:app=claude-code:msg={}", b64("ok\u{202E}tpircs\u{200B}x\u{FEFF}y"));
        is(&body, AgentStatus::Error(Some("ok tpircs x y".into())));
    }

    #[test]
    fn numbers_and_encodings_are_read_only_in_their_plain_form() {
        for body in [
            "state=working:app=claude-code:progress=+5",
            "state=working:app=claude-code:progress=0007",
            "state=working:app=claude-code:progress=",
        ] {
            assert!(matches!(status(body), Reading::NotUnderstood(_)), "{body:?} was understood");
        }
        is("state=working:app=claude-code:progress=0", AgentStatus::Working);
        is("state=working:app=claude-code:progress=100", AgentStatus::Working);
        // Leftover bits, and padding where none belongs.
        for bad in ["QR", "QQQ==", "QQ=", "QUJD="] {
            assert!(base64_decode(bad).is_none(), "{bad} was decoded");
        }
    }

    #[test]
    fn a_huge_report_is_refused_unread_and_only_its_start_kept() {
        let mut t = Track::new(Instant::now());
        t.asked(Instant::now());
        let body = format!("state=working:app=claude-code:msg={}", "A".repeat(LONGEST_REPORT * 4));
        t.received(body.as_bytes(), SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NotUnderstood));
        assert_eq!(t.last.as_ref().unwrap().raw.len(), 5 + LONGEST_REPORT);
    }

    #[test]
    fn a_report_before_the_question_is_not_used_and_one_after_it_is() {
        let t0 = Instant::now();
        let mut t = Track::new(t0);
        t.received(b"state=working:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Starting);
        t.asked(t0);
        t.received(b"state=done:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Done);
        assert!(t.last.as_ref().unwrap().after_question);
    }

    #[test]
    fn a_task_report_that_is_not_understood_still_makes_the_agent_unknown() {
        // Strict on purpose: a changed format shows, even if it first appears
        // in a background task's report.
        assert!(matches!(status("state=pondering:app=claude-code:id=t1"), Reading::NotUnderstood(_)));
        assert!(matches!(status("state=clear:id=a:id=b"), Reading::NotUnderstood(_)));
    }

    #[test]
    fn the_reason_never_carries_control_characters() {
        let Reading::NotUnderstood(why) = status("state=\x1b[2Jboom:app=claude-code") else { panic!() };
        assert!(!why.chars().any(char::is_control), "{why:?}");
    }

    #[test]
    fn background_task_reports_and_withdrawals_are_told_apart() {
        assert_eq!(status("state=working:app=claude-code:id=task-1"), Reading::Task);
        assert_eq!(status("state=clear:id=task-1"), Reading::Task);
        assert_eq!(status("state=clear"), Reading::Cleared);
    }

    #[test]
    fn decodes_base64_of_every_length() {
        for text in ["", "a", "ab", "abc", "abcd", "Working on it"] {
            assert_eq!(base64_decode(&b64(text)).unwrap(), text.as_bytes(), "{text}");
        }
        assert_eq!(base64_decode("QQ").unwrap(), b"A", "padding is optional");
        assert!(base64_decode("A").is_none());
        assert!(base64_decode("QQ===").is_none());
    }

    #[test]
    fn a_track_shows_starting_then_what_was_reported() {
        let t0 = Instant::now();
        let mut t = Track::new(t0);
        assert_eq!(t.shown, AgentStatus::Starting);
        t.asked(t0 + Duration::from_millis(300));
        t.received(b"state=idle:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle);
        t.received(b"state=working:app=claude-code:id=t1", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle, "a background task does not change the agent's status");
        assert_eq!((t.reports, t.task_reports), (2, 1));
        t.received(b"state=pondering:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NotUnderstood));
        t.received(b"state=clear", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Unknown(Why::Withdrawn));
        assert_eq!(t.last.as_ref().unwrap().raw, b"7501;state=clear");
    }

    #[test]
    fn a_report_that_comes_unasked_is_kept_but_not_shown() {
        let mut t = Track::new(Instant::now());
        t.received(b"state=working:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Starting);
        assert!(!t.last.as_ref().unwrap().after_question);
    }

    #[test]
    fn no_question_by_the_deadline_is_unknown() {
        let t0 = Instant::now();
        let mut t = Track::new(t0);
        assert!(!t.tick(t0 + Duration::from_secs(14)));
        assert_eq!(t.shown, AgentStatus::Starting);
        assert!(t.tick(t0 + STARTING_DEADLINE));
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NotAsked));
        assert!(!t.tick(t0 + STARTING_DEADLINE * 2), "says so once");

        // Asked and answered, but the answer never reached it: Unknown too,
        // with its own reason, never Starting for ever.
        let mut t = Track::new(t0);
        t.asked(t0 + Duration::from_secs(1));
        assert!(!t.tick(t0 + Duration::from_secs(2)));
        assert!(t.tick(t0 + STARTING_DEADLINE));
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NoReport));

        // A report after the deadline still counts.
        t.received(b"state=idle:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle);
    }

    #[test]
    fn a_quiet_working_agent_stays_working_and_shows_how_long_it_has_been_quiet() {
        let t0 = Instant::now();
        let mut t = Track::new(t0);
        t.asked(t0);
        t.output(t0);
        t.received(b"state=working:app=claude-code", SystemTime::now());
        t.output(t0 + Duration::from_secs(50));
        assert!(!t.tick(t0 + Duration::from_secs(100)), "drew 50 s in, so not yet a minute");
        assert_eq!(t.shown, AgentStatus::Working);
        assert!(t.tick(t0 + Duration::from_secs(110)));
        assert_eq!(t.shown, AgentStatus::WorkingQuiet(1));
        assert_eq!(t.shown.text(), "Working, quiet 1 min");
        assert!(!t.tick(t0 + Duration::from_secs(120)), "same minute: nothing to redraw");
        // Seen with real Claude Code: a tool quiet for 130 seconds.
        assert!(t.tick(t0 + Duration::from_secs(50 + 130)));
        assert_eq!(t.shown.text(), "Working, quiet 2 min");
        // Drawing again ends the quiet; the status is still Claude Code's.
        t.output(t0 + Duration::from_secs(181));
        assert_eq!(t.shown, AgentStatus::Working);
        assert!(!t.tick(t0 + Duration::from_secs(182)));
        // Its next report replaces it as usual.
        assert!(t.tick(t0 + Duration::from_secs(300)));
        t.received(b"state=done:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Done);
    }

    #[test]
    fn quiet_is_shown_only_for_working() {
        let t0 = Instant::now();
        let mut t = Track::new(t0);
        t.asked(t0);
        for body in [&b"state=done:app=claude-code"[..], b"state=blocked:app=claude-code:kind=permission"] {
            t.received(body, SystemTime::now());
            let before = t.shown.clone();
            assert!(!t.tick(t0 + SILENCE * 10));
            assert_eq!(t.shown, before);
        }
    }

    #[test]
    fn needing_you_is_announced_once_per_change_into_it() {
        let t0 = Instant::now();
        let mut t = Track::new(t0);
        t.asked(t0);
        assert!(!t.take_began_needing_you());
        t.received(b"state=blocked:app=claude-code:kind=permission", SystemTime::now());
        assert!(t.take_began_needing_you());
        assert!(!t.take_began_needing_you(), "taken once");
        // Still needing you, of another kind: not a new change into it.
        t.received(b"state=blocked:app=claude-code:kind=question", SystemTime::now());
        assert!(!t.take_began_needing_you());
        t.received(b"state=working:app=claude-code", SystemTime::now());
        t.received(b"state=blocked:app=claude-code", SystemTime::now());
        assert!(t.take_began_needing_you(), "a second time after it stopped");
    }

    #[test]
    fn only_reasons_about_the_reporting_itself_are_breakage() {
        assert!(
            Why::NotAsked.is_breakage() && Why::NoReport.is_breakage() && Why::NotUnderstood.is_breakage()
        );
        assert!(!Why::Withdrawn.is_breakage() && !Why::NotClaudeCode.is_breakage());
        assert!(!Why::KindNotKnown.is_breakage());
        // Every short reason fits the sidebar's 24 columns after "Unknown: ",
        // and so does a quiet of up to 99 minutes.
        for why in [
            Why::NotAsked,
            Why::NoReport,
            Why::NotUnderstood,
            Why::Withdrawn,
            Why::NotClaudeCode,
            Why::KindNotKnown,
        ] {
            assert!(AgentStatus::Unknown(why).text().chars().count() <= 24, "{why:?}");
        }
        assert!(AgentStatus::WorkingQuiet(99).text().chars().count() <= 24);
    }

    #[test]
    fn shows_control_characters_by_name() {
        assert_eq!(visible(b"7501;?\x07"), "ESC ]7501;?BEL");
        assert_eq!(visible(b"a\x01"), "ESC ]a\\x01");
    }
}
