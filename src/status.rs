//! What a program says it is doing, read from the status reports it sends
//! to its terminal.
//!
//! The format is an open proposal, the Program Status Protocol (OSC 7501)
//! by Mitchell Hashimoto, read here as its revision 0.4 of 2026-10-10:
//!
//!   https://www.superlogical.com/rex/docs/build/program-status
//!
//! A program may first ask its terminal whether it takes status reports
//! (`ESC ] 7501 ; ? BEL`). Lillignode's terminal says yes. Asked or not, the
//! program sends one report each time its status changes:
//!
//!   ESC ] 7501 ; state=working:app=claude-code:msg=<base64> BEL
//!
//! Any program may send them, and the report's own `app` says who it is.
//! Lillignode repeats what the report says and adds nothing. Where the
//! proposal says to pass over something (a part it does not know, a kind it
//! does not know) so does Lillignode. Where the proposal says to keep the
//! earlier status (a state it does not know, text it cannot read) Lillignode
//! shows Unknown instead: the earlier status is no longer known to be true.

use std::time::{Duration, Instant, SystemTime};

/// The terminal message number of a status report.
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

/// A program whose status reports Lillignode reads.
#[derive(Debug, PartialEq, Eq)]
pub struct Reporter {
    /// What it calls itself in a report.
    pub app: &'static str,
    /// What the user calls it.
    pub name: &'static str,
    /// The version of it this reading was checked against.
    pub checked_with: &'static str,
    /// Whether it reports only after asking the status question and being
    /// answered. Used to say why such a program is silent; a report counts
    /// whether or not anything was asked.
    pub asks_first: bool,
}

pub const CLAUDE_CODE: Reporter =
    Reporter { app: "claude-code", name: "Claude Code", checked_with: CHECKED_WITH, asks_first: true };

/// Codex sends the same report, under its own name, without asking first.
/// On its sign-in screen and some of its set-up screens it sends none.
pub const CODEX: Reporter =
    Reporter { app: "codex", name: "Codex", checked_with: "0.169.0", asks_first: false };

/// The switch that stops Claude Code reporting. If Lillignode itself was
/// started with it set, every agent inherits it.
pub const OFF_SWITCH: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";

/// The longest message shown from a report, in characters.
const MESSAGE_CHARS: usize = 200;

/// The longest report read: the proposal's 4,096 bytes for the whole
/// message, less its opening and ending. A longer one is refused unread,
/// and only its start is kept.
pub const LONGEST_REPORT: usize = 4096 - 9;

/// The proposal's sizes for a message and a title, as sent (base64).
const LONGEST_MSG: usize = 2732;
const LONGEST_TITLE: usize = 256;

/// A bell is rung when an agent begins to need the user. A program can
/// change status as fast as it can write, so each agent may ring this many
/// times in a row, and gets one ring back after each `BELL_BACK`.
pub const BELLS: u8 = 3;
pub const BELL_BACK: Duration = Duration::from_secs(20);

/// The reporter a report names, if it is one Lillignode knows by name.
pub fn known(app: &str) -> Option<&'static Reporter> {
    [&CLAUDE_CODE, &CODEX].into_iter().find(|who| who.app == app)
}

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
    /// A program expected to ask the status question never did.
    NotAsked,
    /// A program expected to report sent no report.
    NoReport,
    /// The latest report was not understood in full.
    NotUnderstood,
    /// The agent withdrew its status.
    Withdrawn,
    /// A program nothing is known about has sent no report.
    NothingSent,
    /// Saved as a kind of agent this Lillignode does not know, and it has
    /// sent no report.
    KindNotKnown,
    /// It last said Working or Needs you, and then a shell prompt appeared:
    /// whatever was reporting has gone.
    AtPrompt,
}

impl Why {
    /// Reasons that mean the reporting of a program expected to report is
    /// not reaching Lillignode, as opposed to something this one agent did.
    pub fn is_breakage(self) -> bool {
        match self {
            Why::NotAsked | Why::NoReport | Why::NotUnderstood => true,
            Why::Withdrawn | Why::NothingSent | Why::KindNotKnown | Why::AtPrompt => false,
        }
    }
}

/// The status shown for an agent that is running.
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
        Why::NotAsked | Why::NoReport | Why::NothingSent => "no report",
        Why::NotUnderstood => "not understood",
        Why::Withdrawn => "withdrawn",
        Why::KindNotKnown => "kind not known",
        Why::AtPrompt => "at a prompt",
    }
}

/// The full sentence for an Unknown, for the status report panel. `who` is
/// the program this agent is expected to be, if it is one known by name.
pub fn why_long(why: Why, who: Option<&Reporter>) -> String {
    let name = who.map_or("This program", |who| who.name);
    let asks_first = match who {
        Some(who) => who.asks_first,
        None => true,
    };
    match why {
        Why::NotAsked => {
            format!(
                "{name} did not ask Lillignode's terminal for status reports within 15 seconds of starting"
            )
        }
        Why::NoReport if asks_first => format!(
            "{name} asked for status reports and Lillignode answered, but no report arrived within 15 \
             seconds of starting. The answer may not have reached it"
        ),
        Why::NoReport => format!(
            "{name} sent no status report within 15 seconds of starting. The likeliest cause is that it \
             is signed out, when it sends none. If it is signed out, sign in and it reports. Otherwise this \
             {name} may report differently from the one Lillignode was checked with"
        ),
        Why::NotUnderstood => format!("{name} sent a status Lillignode does not understand"),
        Why::Withdrawn => format!("{name} withdrew its status and has not reported a new one"),
        Why::NothingSent => "This program has sent no status report. Lillignode shows what a program is \
                             doing when the program says so itself, in the open Program Status format (OSC \
                             7501). Until it does, Lillignode can tell only whether it is running"
            .to_string(),
        Why::KindNotKnown => "This agent was saved as a kind of agent this Lillignode does not know, most \
                              likely by a newer Lillignode. It runs as a plain program and has sent no \
                              status report, so Lillignode can tell only whether it is running. What was \
                              saved about it is kept"
            .to_string(),
        Why::AtPrompt => format!(
            "{name} last said it was working or waiting for you, and then a shell prompt appeared. \
             Whatever was reporting has stopped, and nothing has reported since"
        ),
    }
}

/// How one report was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The program's own status, the message that came with it, and the
    /// name the program gave itself, if it gave one the format allows.
    Status { status: AgentStatus, message: Option<String>, app: Option<String> },
    /// The program withdrew its status.
    Cleared,
    /// About one part of the program (it has an id), not the program itself.
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

/// Read the part of a report after `7501;`, whoever sent it.
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
    title: Option<&'a str>,
    msg: Option<&'a str>,
}

/// The characters the format allows in a value.
fn value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b)
}

/// A program's name for itself, as the format allows it: short, and of
/// characters that are safe to show.
fn app_name(app: &str) -> bool {
    (1..=32).contains(&app.len()) && app.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn read_parts(body: &[u8]) -> Result<Reading, String> {
    let text = std::str::from_utf8(body).map_err(|_| "it is not text".to_string())?;
    let mut parts = Parts::default();
    for part in text.split(':') {
        // As the format says: a part that is not `key=value` in the allowed
        // characters is passed over, a part with a key not known here is
        // passed over, and the last of a repeated part stands.
        let Some((key, value)) = part.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        let key_fits = (1..=16).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_lowercase());
        if !key_fits || !value.bytes().all(value_byte) {
            continue;
        }
        match key {
            "state" => parts.state = Some(value),
            "app" => parts.app = Some(value),
            "id" => parts.id = Some(value),
            "kind" => parts.kind = Some(value),
            "title" => parts.title = Some(value),
            "msg" => parts.msg = Some(value),
            _ => {}
        }
    }
    // A report with an id is about one part of the program. Whatever it
    // says, the program's own status stands.
    if parts.id.is_some() {
        return Ok(Reading::Task);
    }
    let state = parts.state.ok_or("it has no state")?;
    if state == "clear" {
        return Ok(Reading::Cleared);
    }
    if let Some(t) = parts.title {
        if t.len() > LONGEST_TITLE {
            return Err("its title is longer than the format allows".into());
        }
        decode_text(t).ok_or("its title cannot be read")?;
    }
    let mut message = match parts.msg {
        Some(m) if m.len() > LONGEST_MSG => return Err("its message is longer than the format allows".into()),
        Some(m) => Some(decode_text(m).ok_or("its message cannot be read")?),
        None => None,
    };
    let status = match state {
        "working" => AgentStatus::Working,
        // A kind not known here is passed over, as the format says: the
        // program is still waiting for the user.
        "blocked" => AgentStatus::NeedsYou(match parts.kind {
            Some("permission") => Some(Kind::Permission),
            Some("question") => Some(Kind::Question),
            Some("auth") => Some(Kind::SignIn),
            _ => None,
        }),
        "done" => AgentStatus::Done,
        "idle" => AgentStatus::Idle,
        "error" => AgentStatus::Error(message.take()),
        other => return Err(format!("state \"{}\" is one Lillignode does not know", shown(other))),
    };
    let app = parts.app.filter(|app| app_name(app)).map(str::to_string);
    Ok(Reading::Status { status, message, app })
}

/// Characters that draw nothing, or change the direction or joining of the
/// text around them, so the words seen are not the words sent. They become
/// spaces, like control characters.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E0FFF}')
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

/// A message from a report: base64 of UTF-8 text. Control characters and
/// characters that hide or reorder text become spaces, runs of space become
/// one, and it is cut to fit, so a program's words can never draw over
/// Lillignode's screen. (The proposal refuses such a report whole. Here the
/// status is kept and only the message is made harmless, so that a stray
/// tab cannot hide that a program needs the user.)
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
}

/// What a source has heard from its agent, kept for the status report
/// panel. In memory only.
#[derive(Clone, Debug, Default)]
pub struct Trace {
    /// How long after starting the agent asked the status question.
    pub asked_after: Option<Duration>,
    pub reports: u32,
    pub task_reports: u32,
    pub last: Option<LastReport>,
    /// The name the program gave itself in its latest report about itself.
    pub app: Option<String>,
}

/// Where an agent's status comes from. A source repeats what the agent said,
/// exactly or not at all; the timing around it (`Track`) is the same for
/// every source.
pub trait StatusSource {
    /// What is shown from the moment the agent starts, before it has said
    /// anything.
    fn at_start(&self) -> AgentStatus;
    /// The agent asked whether its terminal takes status reports, this
    /// long after it started.
    fn asked(&mut self, after: Duration);
    /// A report arrived: everything after `7501;`. Returns the status to
    /// show, if this report changes it.
    fn received(&mut self, body: &[u8], now: SystemTime) -> Option<AgentStatus>;
    /// Why there is no status to show for an agent that is still Starting
    /// when the deadline passes.
    fn why_silent(&self) -> Why;
    /// Whether a shell's prompt mark ends what this agent last said. True
    /// only for a program nothing is known about, which may be a shell
    /// with reporting programs coming and going inside it. An agent known
    /// by name is not a shell, and nothing but its own word changes its
    /// status.
    fn may_be_a_shell(&self) -> bool;
    fn trace(&self) -> &Trace;
}

/// The status reports a program sends, read the same way whoever sends
/// them.
pub struct Report {
    /// The program this agent is expected to be, if it is one known to
    /// report. Only what is said about its silence depends on it.
    expected: Option<&'static Reporter>,
    /// Why nothing can be said about a program not expected to report,
    /// until it reports.
    unsaid: Why,
    trace: Trace,
}

impl Report {
    /// For a program known to report: it shows Starting until it does, and
    /// its silence past the deadline is worth a notice.
    pub fn expected(who: &'static Reporter) -> Self {
        Self { expected: Some(who), unsaid: Why::NoReport, trace: Trace::default() }
    }

    /// For any other program: Unknown, for this reason, until it reports.
    pub fn unexpected(unsaid: Why) -> Self {
        Self { expected: None, unsaid, trace: Trace::default() }
    }
}

impl StatusSource for Report {
    fn at_start(&self) -> AgentStatus {
        match self.expected {
            Some(_) => AgentStatus::Starting,
            None => AgentStatus::Unknown(self.unsaid),
        }
    }

    fn asked(&mut self, after: Duration) {
        if self.trace.asked_after.is_none() {
            self.trace.asked_after = Some(after);
        }
    }

    fn received(&mut self, body: &[u8], now: SystemTime) -> Option<AgentStatus> {
        let reading = if body.len() > LONGEST_REPORT {
            Reading::NotUnderstood(format!("it is {} bytes long, longer than the format allows", body.len()))
        } else {
            read(body)
        };
        self.trace.reports = self.trace.reports.saturating_add(1);
        match &reading {
            Reading::Task => self.trace.task_reports = self.trace.task_reports.saturating_add(1),
            Reading::Status { app, .. } => self.trace.app = app.clone(),
            Reading::Cleared | Reading::NotUnderstood(_) => {}
        }
        let shown = reading.shown();
        let kept = &body[..body.len().min(LONGEST_REPORT)];
        let mut raw = Vec::with_capacity(5 + kept.len());
        raw.extend_from_slice(b"7501;");
        raw.extend_from_slice(kept);
        self.trace.last = Some(LastReport { raw, at: now, reading });
        shown
    }

    fn why_silent(&self) -> Why {
        match self.expected {
            Some(who) if who.asks_first && self.trace.asked_after.is_none() => Why::NotAsked,
            Some(_) => Why::NoReport,
            None => self.unsaid,
        }
    }

    fn may_be_a_shell(&self) -> bool {
        self.expected.is_none()
    }

    fn trace(&self) -> &Trace {
        &self.trace
    }
}

/// Everything Lillignode knows about one agent's status since it was started.
/// Begins again whenever the agent is started; nothing is carried over.
pub struct Track {
    started: Instant,
    pub shown: AgentStatus,
    /// When the agent last drew anything.
    last_output: Instant,
    /// Set when the status changes into Needs you, until taken.
    began_needing_you: bool,
    /// How many bells this agent may still ring in a row, and when one was
    /// last given back.
    bells: u8,
    bell_back_since: Instant,
    /// Bells not rung because the agent had rung too many.
    bells_held: u32,
    source: Box<dyn StatusSource>,
}

impl Track {
    pub fn new(started: Instant, source: Box<dyn StatusSource>) -> Self {
        Self {
            started,
            shown: source.at_start(),
            last_output: started,
            began_needing_you: false,
            bells: BELLS,
            bell_back_since: started,
            bells_held: 0,
            source,
        }
    }

    /// What the agent's source has heard, for the status report panel.
    pub fn trace(&self) -> &Trace {
        self.source.trace()
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

    /// True if a bell should ring now: the agent began to need the user
    /// since the last call, and has not rung too often. An agent whose
    /// status flips as fast as it can write rings `BELLS` times and then
    /// once each `BELL_BACK`; the rest are counted and not rung.
    pub fn take_bell(&mut self, now: Instant) -> bool {
        if !self.take_began_needing_you() {
            return false;
        }
        let back = now.saturating_duration_since(self.bell_back_since).as_secs() / BELL_BACK.as_secs();
        if back >= u64::from(BELLS - self.bells) {
            self.bells = BELLS;
            self.bell_back_since = now;
        } else {
            self.bells += back as u8;
            self.bell_back_since += BELL_BACK * back as u32;
        }
        if self.bells == 0 {
            self.bells_held = self.bells_held.saturating_add(1);
            return false;
        }
        self.bells -= 1;
        true
    }

    /// How many bells were not rung because this agent had rung too many.
    pub fn bells_held(&self) -> u32 {
        self.bells_held
    }

    /// The agent's shell marked a new prompt. A program that was working or
    /// waiting when its shell came back has gone, so what it last said no
    /// longer holds. Done, Idle and Error stay, as the format says.
    pub fn prompt(&mut self) {
        if !self.source.may_be_a_shell() {
            return;
        }
        if matches!(
            self.shown,
            AgentStatus::Working | AgentStatus::WorkingQuiet(_) | AgentStatus::NeedsYou(_)
        ) {
            self.shown = AgentStatus::Unknown(Why::AtPrompt);
            // Whatever needed the user has gone: no bell for it now.
            self.began_needing_you = false;
        }
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
        self.source.asked(now.saturating_duration_since(self.started));
    }

    /// A report arrived. `body` is everything after `7501;`.
    pub fn received(&mut self, body: &[u8], now: SystemTime) {
        if let Some(status) = self.source.received(body, now) {
            self.show(status);
        }
    }

    /// Call regularly. True if the status changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        match self.shown {
            AgentStatus::Starting if now.saturating_duration_since(self.started) >= STARTING_DEADLINE => {
                self.show(AgentStatus::Unknown(self.source.why_silent()));
                true
            }
            // Still Working: that is what the agent last said, and only it
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

    /// The tracker of a Claude Code agent.
    fn claude(started: Instant) -> Track {
        Track::new(started, Box::new(Report::expected(&CLAUDE_CODE)))
    }

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
        let long = format!("state=working:app=claude-code:msg={}", b64(&"é".repeat(1000)));
        let Reading::Status { message: Some(m), .. } = status(&long) else { panic!() };
        assert_eq!(m.chars().count(), MESSAGE_CHARS);
        assert!(m.ends_with('…'));
    }

    #[test]
    fn a_report_whose_state_or_text_cannot_be_read_is_not_understood() {
        for body in [
            "",
            "app=claude-code",
            "state=thinking:app=claude-code",
            "state=:app=claude-code",
            // A state in characters the format does not allow is no state.
            "state=w\u{f6}rking:app=claude-code",
            "state=working:app=claude-code:msg=A",
            "state=working:app=claude-code:title=A",
        ] {
            assert!(matches!(status(body), Reading::NotUnderstood(_)), "{body:?} was understood");
        }
        let not_text = read(b"state=working:app=claude-code:msg=\xff");
        assert!(matches!(not_text, Reading::NotUnderstood(_)));
        // Longer than the format allows, though each would decode.
        let long_msg = format!("state=working:msg={}", "QUJD".repeat(LONGEST_MSG / 4 + 1));
        assert!(matches!(status(&long_msg), Reading::NotUnderstood(_)));
        let long_title = format!("state=working:title={}", "QUJD".repeat(LONGEST_TITLE / 4 + 1));
        assert!(matches!(status(&long_title), Reading::NotUnderstood(_)));
        is(&format!("state=working:msg={}", "QUJD".repeat(LONGEST_MSG / 4)), AgentStatus::Working);
    }

    #[test]
    fn what_the_format_says_to_pass_over_is_passed_over() {
        // A part not known here: this is how the format grows.
        is("state=working:app=claude-code:colour=red", AgentStatus::Working);
        // A part with no value, an empty key, a key or value in characters
        // the format does not allow.
        is("state=working:app=claude-code:noequals", AgentStatus::Working);
        is("state=working:=x", AgentStatus::Working);
        is("state=working:Mood=fine:mood=fi ne:msg=@@@", AgentStatus::Working);
        // Spaces around a key or a value are not part of it.
        is(" state = done : app = brew ", AgentStatus::Done);
        // The last of a repeated part stands, even when it is the one
        // that cannot be read: nothing is picked from among them.
        is("state=working:app=claude-code:state=done", AgentStatus::Done);
        assert!(matches!(status("state=working:state=pondering"), Reading::NotUnderstood(_)));
        // A kind means something only when blocked; one not known is no kind.
        // Either way the program is still waiting for the user.
        is("state=working:app=claude-code:kind=permission", AgentStatus::Working);
        is("state=blocked:app=claude-code:kind=coffee", AgentStatus::NeedsYou(None));
        // Progress is not shown, so nothing about it can spoil a report.
        for progress in ["101", "-1", "+5", "0007", "", "40"] {
            is(&format!("state=working:app=claude-code:progress={progress}"), AgentStatus::Working);
        }
        // A withdrawal is one whatever else it carries.
        assert_eq!(status("state=clear:app=claude-code"), Reading::Cleared);
    }

    #[test]
    fn any_program_is_read_and_its_own_name_says_who_it_is() {
        let named = |body: &str| match status(body) {
            Reading::Status { app, .. } => app,
            other => panic!("{body}: {other:?}"),
        };
        assert_eq!(named("state=working:app=terraform").as_deref(), Some("terraform"));
        assert_eq!(named("state=idle:app=codex").as_deref(), Some("codex"));
        // No name is allowed; the status is still the program's.
        assert_eq!(named("state=blocked:kind=auth"), None);
        // A name the format does not allow is no name: too long, or empty.
        assert_eq!(named(&format!("state=idle:app={}", "a".repeat(33))), None);
        assert_eq!(named("state=idle:app="), None);
        assert_eq!(named("state=idle:app=a,b"), None);
        assert_eq!(known("codex"), Some(&CODEX));
        assert_eq!(known("claude-code"), Some(&CLAUDE_CODE));
        assert_eq!(known("terraform"), None);
    }

    #[test]
    fn hidden_and_direction_changing_characters_do_not_reach_the_screen() {
        let body = format!("state=error:app=claude-code:msg={}", b64("ok\u{202E}tpircs\u{200B}x\u{FEFF}y"));
        is(&body, AgentStatus::Error(Some("ok tpircs x y".into())));
        // Fillers that look like nothing, marks that change the letter
        // before them, and the hidden "tag" letters.
        let body = format!("state=error:msg={}", b64("a\u{034F}b\u{3164}c\u{FE0F}d\u{E0041}e\u{115F}f"));
        is(&body, AgentStatus::Error(Some("a b c d e f".into())));
    }

    #[test]
    fn base64_is_read_only_in_its_plain_form() {
        // Leftover bits, and padding where none belongs.
        for bad in ["QR", "QQQ==", "QQ=", "QUJD="] {
            assert!(base64_decode(bad).is_none(), "{bad} was decoded");
        }
    }

    #[test]
    fn a_huge_report_is_refused_unread_and_only_its_start_kept() {
        let mut t = claude(Instant::now());
        t.asked(Instant::now());
        let body = format!("state=working:app=claude-code:msg={}", "A".repeat(LONGEST_REPORT * 4));
        t.received(body.as_bytes(), SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NotUnderstood));
        assert_eq!(t.trace().last.as_ref().unwrap().raw.len(), 5 + LONGEST_REPORT);
    }

    #[test]
    fn a_report_counts_whether_or_not_the_question_was_asked() {
        let t0 = Instant::now();
        let mut t = claude(t0);
        t.received(b"state=working:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Working);
        assert!(t.trace().asked_after.is_none(), "a report is not a question");
        t.asked(t0);
        t.received(b"state=done:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Done);
        assert_eq!(t.trace().app.as_deref(), Some("claude-code"));
        // Each report stands alone, as the format says: one with no name
        // leaves no name.
        t.received(b"state=idle", SystemTime::now());
        assert_eq!(t.trace().app, None);
    }

    #[test]
    fn a_report_about_a_part_of_the_program_never_changes_its_status() {
        let mut t = claude(Instant::now());
        t.received(b"state=idle:app=claude-code", SystemTime::now());
        // Whatever it says, understood or not.
        // An id the format does not allow (an empty one) is still not the
        // program itself: the format says such a report must not fall back
        // to the program's own record.
        for body in [
            "state=pondering:app=claude-code:id=t1",
            "state=blocked:id=t1",
            "id=t1",
            "state=clear:id=a",
            "state=blocked:id=",
            "state=blocked:id=a:id=b",
        ] {
            assert_eq!(status(body), Reading::Task, "{body}");
            t.received(body.as_bytes(), SystemTime::now());
            assert_eq!(t.shown, AgentStatus::Idle, "{body}");
        }
        assert_eq!((t.trace().reports, t.trace().task_reports), (7, 6));
        assert!(!t.take_began_needing_you());
    }

    #[test]
    fn the_reason_never_carries_control_characters() {
        // Characters the format does not allow never get as far as a reason.
        let Reading::NotUnderstood(why) = status("state=\x1b[2Jboom:app=claude-code") else { panic!() };
        assert_eq!(why, "it has no state");
        let Reading::NotUnderstood(why) = status(&format!("state={}", "x".repeat(300))) else { panic!() };
        assert!(why.chars().count() < 100, "{why:?}");
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
        let mut t = claude(t0);
        assert_eq!(t.shown, AgentStatus::Starting);
        t.asked(t0 + Duration::from_millis(300));
        t.received(b"state=idle:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle);
        t.received(b"state=working:app=claude-code:id=t1", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle, "a background task does not change the agent's status");
        assert_eq!((t.trace().reports, t.trace().task_reports), (2, 1));
        t.received(b"state=pondering:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NotUnderstood));
        t.received(b"state=clear", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Unknown(Why::Withdrawn));
        assert_eq!(t.trace().last.as_ref().unwrap().raw, b"7501;state=clear");
    }

    #[test]
    fn no_question_by_the_deadline_is_unknown() {
        let t0 = Instant::now();
        let mut t = claude(t0);
        assert!(!t.tick(t0 + Duration::from_secs(14)));
        assert_eq!(t.shown, AgentStatus::Starting);
        assert!(t.tick(t0 + STARTING_DEADLINE));
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NotAsked));
        assert!(!t.tick(t0 + STARTING_DEADLINE * 2), "says so once");

        // Asked and answered, but the answer never reached it: Unknown too,
        // with its own reason, never Starting for ever.
        let mut t = claude(t0);
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
        let mut t = claude(t0);
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
        let mut t = claude(t0);
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
        let mut t = claude(t0);
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
        assert!(!Why::Withdrawn.is_breakage() && !Why::NothingSent.is_breakage());
        assert!(!Why::KindNotKnown.is_breakage() && !Why::AtPrompt.is_breakage());
        // Every short reason fits the sidebar's 24 columns after "Unknown: ",
        // and so does a quiet of up to 99 minutes.
        for why in [
            Why::NotAsked,
            Why::NoReport,
            Why::NotUnderstood,
            Why::Withdrawn,
            Why::NothingSent,
            Why::KindNotKnown,
            Why::AtPrompt,
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

    /// The tracker of a program nothing is known about.
    fn program(started: Instant) -> Track {
        Track::new(started, Box::new(Report::unexpected(Why::NothingSent)))
    }

    #[test]
    fn a_program_that_sends_nothing_is_unknown_from_the_start_and_says_why() {
        let t0 = Instant::now();
        let mut t = program(t0);
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NothingSent));
        assert_eq!(t.shown.text(), "Unknown: no report");
        // Asking the question is not a report, and drawing is not one.
        t.asked(t0);
        t.output(t0);
        assert!(!t.tick(t0 + STARTING_DEADLINE + SILENCE), "nothing was expected, so no deadline passes");
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NothingSent));
        assert!(!t.take_began_needing_you());
        assert!(why_long(Why::NothingSent, None).starts_with("This program has sent no status report."));
    }

    #[test]
    fn any_program_that_reports_gets_its_status_and_its_bell() {
        let t0 = Instant::now();
        let mut t = program(t0);
        for (body, shown) in [
            ("state=working:app=terraform", AgentStatus::Working),
            ("state=blocked:kind=permission:app=terraform", AgentStatus::NeedsYou(Some(Kind::Permission))),
            ("state=done:app=terraform", AgentStatus::Done),
            ("state=error", AgentStatus::Error(None)),
            ("state=clear", AgentStatus::Unknown(Why::Withdrawn)),
            ("state=idle:app=terraform", AgentStatus::Idle),
        ] {
            t.received(body.as_bytes(), SystemTime::now());
            assert_eq!(t.shown, shown, "{body}");
        }
        assert_eq!(t.trace().app.as_deref(), Some("terraform"));
        assert!(t.take_bell(t0));
        // And the same for a program saved as a kind not known here.
        let mut t = Track::new(t0, Box::new(Report::unexpected(Why::KindNotKnown)));
        assert_eq!(t.shown, AgentStatus::Unknown(Why::KindNotKnown));
        t.received(b"state=idle", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle);
    }

    #[test]
    fn an_agent_that_flips_into_needing_you_over_and_over_rings_only_so_often() {
        let t0 = Instant::now();
        let mut t = program(t0);
        let flip = |t: &mut Track| {
            t.received(b"state=working", SystemTime::now());
            t.received(b"state=blocked", SystemTime::now());
        };
        // Many flips between two looks are one change to tell of.
        for _ in 0..1000 {
            flip(&mut t);
        }
        assert!(t.take_bell(t0));
        assert!(!t.take_bell(t0), "taken once");
        // Three in a row ring; after that they are counted, not rung.
        let mut rung = 1;
        for i in 0..50 {
            flip(&mut t);
            rung += u32::from(t.take_bell(t0 + Duration::from_millis(i)));
        }
        assert_eq!(rung, u32::from(BELLS));
        assert_eq!(t.bells_held(), 51 - u32::from(BELLS));
        // One comes back after each stretch, and no more than one.
        flip(&mut t);
        assert!(t.take_bell(t0 + BELL_BACK + Duration::from_secs(1)));
        flip(&mut t);
        assert!(!t.take_bell(t0 + BELL_BACK + Duration::from_secs(2)));
        // After a long calm it has all of them again, and no more.
        let later = t0 + BELL_BACK * 100;
        let mut rung = 0;
        for _ in 0..10 {
            flip(&mut t);
            rung += u32::from(t.take_bell(later));
        }
        assert_eq!(rung, u32::from(BELLS));
        // The status shown is still the latest said.
        assert_eq!(t.shown, AgentStatus::NeedsYou(None));
    }

    #[test]
    fn an_agent_that_needs_you_now_and_then_always_rings() {
        let t0 = Instant::now();
        let mut t = program(t0);
        for minute in 0..20 {
            t.received(b"state=working", SystemTime::now());
            t.received(b"state=blocked", SystemTime::now());
            assert!(t.take_bell(t0 + Duration::from_secs(60 * minute)), "minute {minute}");
        }
        assert_eq!(t.bells_held(), 0);
    }

    #[test]
    fn a_shell_prompt_ends_working_and_needing_you_and_nothing_else() {
        let t0 = Instant::now();
        for (body, after) in [
            ("state=working", AgentStatus::Unknown(Why::AtPrompt)),
            ("state=blocked:kind=question", AgentStatus::Unknown(Why::AtPrompt)),
            ("state=done", AgentStatus::Done),
            ("state=error", AgentStatus::Error(None)),
            ("state=idle", AgentStatus::Idle),
            ("state=clear", AgentStatus::Unknown(Why::Withdrawn)),
        ] {
            let mut t = program(t0);
            t.received(body.as_bytes(), SystemTime::now());
            t.prompt();
            assert_eq!(t.shown, after, "{body}");
        }
        // A quiet Working agent too, and a program that never reported is
        // left saying so.
        let mut t = program(t0);
        t.received(b"state=working", SystemTime::now());
        assert!(t.tick(t0 + SILENCE * 2));
        t.prompt();
        assert_eq!(t.shown.text(), "Unknown: at a prompt");
        let mut t = program(t0);
        t.prompt();
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NothingSent));
        // The next report is shown as usual.
        t.received(b"state=working", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Working);
        // Needing you and then a prompt, before anyone looked: no bell
        // for something that has gone.
        t.received(b"state=blocked", SystemTime::now());
        t.prompt();
        assert!(!t.take_bell(t0));
    }

    #[test]
    fn a_prompt_mark_never_changes_an_agent_known_by_name() {
        // Claude Code and Codex are not shells. If one ever draws a prompt
        // mark, or something inside it does, its own word still stands.
        for mut t in [claude(Instant::now()), codex(Instant::now())] {
            t.prompt();
            assert_eq!(t.shown, AgentStatus::Starting);
            for body in ["state=working", "state=blocked:kind=permission"] {
                t.received(body.as_bytes(), SystemTime::now());
                let before = t.shown.clone();
                t.prompt();
                assert_eq!(t.shown, before, "{body}");
            }
            assert!(t.take_bell(Instant::now()));
        }
    }

    #[test]
    fn a_clock_that_seems_to_run_backwards_does_not_upset_the_bell() {
        let t0 = Instant::now() + BELL_BACK * 10;
        let mut t = program(t0);
        for i in 0..5u32 {
            t.received(b"state=working", SystemTime::now());
            t.received(b"state=blocked", SystemTime::now());
            // Each look is earlier than the one before.
            let rang = t.take_bell(t0 - Duration::from_secs(u64::from(i) * 30));
            assert_eq!(rang, i < u32::from(BELLS), "look {i}");
        }
    }

    /// The tracker of a Codex agent.
    fn codex(started: Instant) -> Track {
        Track::new(started, Box::new(Report::expected(&CODEX)))
    }

    #[test]
    fn codex_reports_are_used_without_a_question_and_read_the_same_way() {
        let t0 = Instant::now();
        let mut t = codex(t0);
        assert_eq!(t.shown, AgentStatus::Starting);
        // As seen from Codex 0.169.0: unasked, three parts at most.
        for (body, shown) in [
            ("state=idle:app=codex", AgentStatus::Idle),
            ("state=working:app=codex", AgentStatus::Working),
            ("state=blocked:app=codex:kind=permission", AgentStatus::NeedsYou(Some(Kind::Permission))),
            ("state=blocked:app=codex:kind=question", AgentStatus::NeedsYou(Some(Kind::Question))),
            ("state=blocked:app=codex:kind=auth", AgentStatus::NeedsYou(Some(Kind::SignIn))),
            ("state=done:app=codex", AgentStatus::Done),
            ("state=error:app=codex", AgentStatus::Error(None)),
            ("state=clear", AgentStatus::Unknown(Why::Withdrawn)),
        ] {
            t.received(body.as_bytes(), SystemTime::now());
            assert_eq!(t.shown, shown, "{body}");
        }
        assert!(t.trace().asked_after.is_none());
        // One stretch of needing you, one bell.
        assert!(t.take_began_needing_you());
        assert!(!t.take_began_needing_you());
    }

    #[test]
    fn a_report_under_another_name_is_read_and_the_name_is_kept() {
        // The report says who it is, whatever the agent was started as.
        let mut t = codex(Instant::now());
        t.received(b"state=blocked:app=claude-code", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::NeedsYou(None));
        assert_eq!(t.trace().app.as_deref(), Some("claude-code"));
        assert!(t.take_began_needing_you());
        // The same care about states as for any program.
        assert!(matches!(read(b"state=pondering:app=codex"), Reading::NotUnderstood(_)));
        assert_eq!(
            read(b"state=idle:app=codex:mood=fine"),
            Reading::Status { status: AgentStatus::Idle, message: None, app: Some("codex".into()) }
        );
    }

    #[test]
    fn a_codex_agent_that_sends_nothing_is_unknown_for_no_report_never_for_not_asking() {
        let t0 = Instant::now();
        let mut t = codex(t0);
        assert!(!t.tick(t0 + STARTING_DEADLINE - Duration::from_millis(1)));
        assert_eq!(t.shown, AgentStatus::Starting);
        assert!(t.tick(t0 + STARTING_DEADLINE));
        assert_eq!(t.shown, AgentStatus::Unknown(Why::NoReport));
        // It is not stuck there: the first report is shown.
        t.received(b"state=idle:app=codex", SystemTime::now());
        assert_eq!(t.shown, AgentStatus::Idle);
        // The sentence for it is Codex's own, and says what to do.
        let said = why_long(Why::NoReport, Some(&CODEX));
        assert!(said.starts_with("Codex sent no status report"), "{said}");
        assert!(said.contains("If it is signed out, sign in and it reports."), "{said}");
        assert!(
            why_long(Why::NoReport, Some(&CLAUDE_CODE)).starts_with("Claude Code asked for status reports")
        );
    }
}
