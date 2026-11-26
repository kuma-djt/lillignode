//! The Board, read from a file the user keeps beside the workspace, and
//! the outbox that what the user does to a card is written to.
//!
//! The page's third column shows cards: things to decide, things only the
//! reader can do, and things worth knowing. They come from one local file,
//! `board.json`, in the folder Lillignode saves in. Nothing is fetched from
//! anywhere else, and nothing here ever writes to that file.
//!
//!   { "cards": [ { "id": "c1", "owner": "Codex (web)", "kind": "decide",
//!                  "tier": "now", "title": "…", "recommendation": "…",
//!                  "status": "queued", "created_at": "2026-11-20T09:30:00Z" } ] }
//!
//! A card has an `id`, an `owner` (who raised it), a `kind` (decide, fire,
//! do, question or know), a `tier` (now or next), a `title` and a `status`.
//! It may have a `recommendation`, a `created_at` and a `due_at`, and
//! `risk`, `cost`, `blocks`, `why`, `options` and `silo`, which are not
//! shown. A card is open while its status is queued, revise, hold or more;
//! with any other status, draft among them, it is not shown.
//!
//! What the reader does to a card (approve, revise, hold, done, got it) is
//! added to `board-outbox.jsonl`, one line each, for whatever keeps the
//! Board to pick up. Which cards have been acted on is kept in
//! `board-sent.json`, so a card shows as sent and waiting until the
//! Board's file no longer lists it as open.
//!
//! Picking up: whatever takes lines from the outbox takes the whole file,
//! by renaming it to a name of its own, and reads that. It never edits the
//! outbox in place. Undo here does the same: it renames the outbox to a
//! name of its own, takes its one line out, and puts the file back. So a
//! line is in exactly one place at any moment, and an undo either takes
//! its line out or finds the outbox already taken.
//!
//! A line says which card by id and by the title the reader was shown, so
//! whatever picks it up can refuse a card that has since become another.
//! What the outbox is not: proof. Any program running as this user can
//! write a line to it. Lillignode keeps its own record of every line it
//! wrote; whatever picks lines up asks Lillignode
//! and never trusts the file alone.
//!
//! Some cards cannot be acted on from the page at all: a fire, a card with
//! an amount (`amount_cents` above 0), and a card whose risk is marked high. They say "Approve this
//! one on the Hub" where the buttons would be, and an action on one is
//! refused here.
//!
//! This file is a reader: it names nothing of the engine, and is given
//! only a folder to read and write in (`tests/boundaries.rs`).

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::private;

/// The Board's file, in the folder Lillignode saves in.
pub const FILE: &str = "board.json";

/// Where what the reader does to a card is added, one line each.
pub const OUTBOX: &str = "board-outbox.jsonl";

/// Which cards have been acted on and are waiting.
pub const SENT: &str = "board-sent.json";

/// How long after an action it can be undone.
pub const UNDO_FOR: Duration = Duration::from_secs(10);

/// A little more than `UNDO_FOR`, so an Undo pressed in its last moment is
/// not refused for the time it took to arrive.
const UNDO_GRACE: Duration = Duration::from_secs(2);

/// The most characters a note may have.
pub const NOTE_MAX: usize = 280;

/// A file larger than this is not read.
const MAX_BYTES: u64 = 1024 * 1024;

/// No more cards than this are read; the rest are counted.
const MAX_CARDS: usize = 500;

/// How many cards that could not be read are named; the rest are counted.
const NAMED: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Decide,
    Fire,
    Do,
    Question,
    Know,
}

impl Kind {
    fn parse(word: &str) -> Option<Kind> {
        Some(match word {
            "decide" => Kind::Decide,
            "fire" => Kind::Fire,
            "do" => Kind::Do,
            "question" => Kind::Question,
            "know" => Kind::Know,
            _ => return None,
        })
    }

    fn word(self) -> &'static str {
        match self {
            Kind::Decide => "decide",
            Kind::Fire => "fire",
            Kind::Do => "do",
            Kind::Question => "question",
            Kind::Know => "know",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Now,
    Next,
}

/// What the reader can do to a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    Revise,
    Hold,
    Done,
    GotIt,
}

impl Action {
    pub fn parse(word: &str) -> Option<Action> {
        Some(match word {
            "approve" => Action::Approve,
            "revise" => Action::Revise,
            "hold" => Action::Hold,
            "done" => Action::Done,
            "got-it" => Action::GotIt,
            _ => return None,
        })
    }

    /// As the outbox and the page's requests write it.
    pub fn word(self) -> &'static str {
        match self {
            Action::Approve => "approve",
            Action::Revise => "revise",
            Action::Hold => "hold",
            Action::Done => "done",
            Action::GotIt => "got-it",
        }
    }

    /// As a sentence names it.
    pub fn name(self) -> &'static str {
        match self {
            Action::Approve => "Approve",
            Action::Revise => "Revise",
            Action::Hold => "Hold",
            Action::Done => "Done",
            Action::GotIt => "Got it",
        }
    }

    const ALL: [Action; 5] = [Action::Approve, Action::Revise, Action::Hold, Action::Done, Action::GotIt];

    /// Whether this can be done to this card as it stands: by its kind, and
    /// not Hold on a card already on hold.
    fn fits(self, card: &Card) -> bool {
        match card.kind {
            Kind::Decide | Kind::Fire | Kind::Question => match self {
                Action::Approve | Action::Revise => true,
                Action::Hold => card.status != "hold",
                Action::Done | Action::GotIt => false,
            },
            Kind::Do => self == Action::Done,
            Kind::Know => self == Action::GotIt,
        }
    }
}

/// One open card, as read. Only what the page shows is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    /// Who raised it: the card's owner.
    pub raised_by: String,
    pub kind: Kind,
    pub tier: Tier,
    /// queued, revise, hold or more.
    pub status: String,
    pub title: String,
    pub recommendation: Option<String>,
    /// Seconds since 1970, where the card says when it was made.
    pub created_at: Option<i64>,
    /// Seconds since 1970, where the card has a time it is due.
    pub due_at: Option<i64>,
    /// It is due on a day, with no time of day: shown as that day
    /// wherever the reader is.
    pub due_whole_day: bool,
    /// It cannot be acted on from the page: a fire, a card with an amount,
    /// or one whose risk is marked high.
    pub hub_only: bool,
}

/// The Board as the page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Board {
    /// Decide, fire and question cards: now before next, then oldest first.
    pub decide: Vec<Card>,
    /// Do cards, each something only the reader can do: now before next,
    /// then oldest first.
    pub yours: Vec<Card>,
    /// Know cards, newest first.
    pub know: Vec<Card>,
    /// Cards that could not be read, each said with why, up to `NAMED`.
    pub not_read: Vec<String>,
    /// How many cards could not be read in all.
    pub not_read_count: usize,
    /// The whole file could not be read: why.
    pub problem: Option<String>,
}

#[derive(Deserialize)]
struct FileShape {
    cards: Vec<Value>,
}

/// The fields a card must have, and the ones it may have that the page
/// shows. Any other field is let be.
#[derive(Deserialize)]
struct CardShape {
    id: String,
    owner: String,
    kind: String,
    tier: String,
    title: String,
    status: String,
    #[serde(default)]
    recommendation: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    due_at: Option<String>,
    #[serde(default)]
    amount_cents: Option<Value>,
    /// The field's earlier name, still honoured: a card a reader already
    /// has with it must not become approvable from the page.
    #[serde(default)]
    amount: Option<Value>,
    #[serde(default)]
    risk: Option<Value>,
}

/// True for a card that has an amount: `amount_cents` above 0. A number
/// written as text counts as the number. Text that is not a number is
/// taken as an amount, so a card the reader cannot judge here goes to the
/// Hub; none, empty text, 0 and below are not.
fn has_amount(amount_cents: &Option<Value>) -> bool {
    match amount_cents {
        None | Some(Value::Null) => false,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n > 0.0),
        Some(Value::String(text)) if text.trim().is_empty() => false,
        Some(Value::String(text)) => text.trim().parse::<f64>().map_or(true, |n| n.is_nan() || n > 0.0),
        Some(_) => true,
    }
}

/// True for a risk marked high: the word "high" alone, or first and
/// followed by something that is not a letter ("high: it breaks two
/// callers"), in any case.
fn risk_is_high(risk: &Option<Value>) -> bool {
    let Some(Value::String(text)) = risk else { return false };
    let text = text.trim().to_lowercase();
    text.strip_prefix("high").is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
}

/// The most characters a card's id may have.
const ID_MAX: usize = 128;

/// True for an id that can name a card in the outbox: not too long, and
/// only letters, digits and plain marks, with no space at either end.
fn plain_id(id: &str) -> bool {
    id.chars().count() <= ID_MAX
        && id == id.trim()
        && id.chars().all(|c| c.is_alphanumeric() || " -_./:#@()+,".contains(c))
}

/// The statuses a card is open with.
const OPEN: [&str; 4] = ["queued", "revise", "hold", "more"];

/// Days from 1970-01-01 to this day of the calendar in use today.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The day of the calendar that many days from 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// A time as a card writes it, in seconds since 1970: a day
/// (`2026-12-03`, taken as its first moment in UTC) or a day and a time
/// with `Z` or an offset (`2026-11-20T09:30:00Z`, `…T09:30:00.5+01:00`).
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let number = |part: &str, len: usize| -> Option<i64> {
        (part.len() == len && part.bytes().all(|b| b.is_ascii_digit())).then(|| part.parse().ok())?
    };
    let (date, rest) = match text.find(['T', 't', ' ']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let (year, month, day) =
        (number(parts.next()?, 4)?, number(parts.next()?, 2)?, number(parts.next()?, 2)?);
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    // A day that does not exist (30 February) comes back as another day.
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    let Some(rest) = rest else { return Some(days * 86_400) };
    let (clock, offset) = if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (hours, minutes) = rest[at + 1..].split_once(':')?;
        let (hours, minutes) = (number(hours, 2)?, number(minutes, 2)?);
        if hours > 23 || minutes > 59 {
            return None;
        }
        let sign = if rest.as_bytes()[at] == b'-' { -1 } else { 1 };
        (&rest[..at], sign * (hours * 3600 + minutes * 60))
    };
    // Parts of a second are let go.
    let clock = clock.split('.').next()?;
    let mut parts = clock.split(':');
    let (hour, minute) = (number(parts.next()?, 2)?, number(parts.next()?, 2)?);
    let second = match parts.next() {
        Some(second) => number(second, 2)?,
        None => 0,
    };
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// A time as the outbox writes it: `2026-11-23T18:04:05Z`.
pub fn written_time(time: SystemTime) -> String {
    let secs = time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// Read a file of at most `MAX_BYTES`. None: there is no such file.
fn read_file(path: &Path) -> Result<Option<String>, String> {
    // Only a plain file is opened: anything else there (a pipe, a folder)
    // could hold the reader of it for ever.
    match std::fs::metadata(path) {
        Ok(found) if !found.is_file() => return Err("it is not a plain file".to_string()),
        _ => {}
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("it could not be opened ({e})")),
    };
    let mut text = String::new();
    match file.take(MAX_BYTES + 1).read_to_string(&mut text) {
        Ok(n) if n as u64 > MAX_BYTES => Err("it is over 1 MB".to_string()),
        Ok(_) => Ok(Some(text)),
        Err(e) => Err(format!("it could not be read ({e})")),
    }
}

/// Read the Board in `folder`. No folder or no file: an empty Board.
pub fn read(folder: Option<&Path>) -> Board {
    let Some(folder) = folder else { return Board::default() };
    match read_file(&folder.join(FILE)) {
        Ok(Some(text)) => from_text(&text),
        Ok(None) => Board::default(),
        Err(why) => unreadable(&why),
    }
}

fn unreadable(why: &str) -> Board {
    Board {
        problem: Some(format!("The Board's file, {FILE}, was not read: {why}. Nothing else is affected.")),
        ..Board::default()
    }
}

/// The Board from the file's text.
pub fn from_text(text: &str) -> Board {
    let file: FileShape = match serde_json::from_str(text) {
        Ok(file) => file,
        Err(e) => return unreadable(&format!("it is not a list of cards ({e})")),
    };
    let mut board = Board::default();
    let said_not_read = |board: &mut Board, why: String| {
        board.not_read_count += 1;
        if board.not_read.len() < NAMED {
            board.not_read.push(why);
        }
    };
    let mut cards: Vec<Card> = Vec::new();
    for (at, value) in file.cards.iter().enumerate() {
        if at >= MAX_CARDS {
            said_not_read(&mut board, format!("the cards after the {MAX_CARDS}th"));
            board.not_read_count += file.cards.len() - MAX_CARDS - 1;
            break;
        }
        // Named by its id where it has one, otherwise by where it is.
        let name = value
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("card {}", at + 1), |id| format!("card {id}"));
        let shape: CardShape = match serde_json::from_value(value.clone()) {
            Ok(shape) => shape,
            Err(e) => {
                said_not_read(&mut board, format!("{name}: {}", missing(&e.to_string())));
                continue;
            }
        };
        // Not open: not shown, and not checked either.
        if !OPEN.contains(&shape.status.as_str()) {
            continue;
        }
        let Some(kind) = Kind::parse(&shape.kind) else {
            said_not_read(&mut board, format!("{name}: no kind called \"{}\"", shape.kind));
            continue;
        };
        let tier = match shape.tier.as_str() {
            "now" => Tier::Now,
            "next" => Tier::Next,
            other => {
                said_not_read(&mut board, format!("{name}: no tier called \"{other}\""));
                continue;
            }
        };
        if shape.title.trim().is_empty() {
            said_not_read(&mut board, format!("{name}: its title is empty"));
            continue;
        }
        if shape.id.trim().is_empty() {
            said_not_read(&mut board, format!("card {}: its id is empty", at + 1));
            continue;
        }
        // An id is written to the outbox and into events, and names the
        // card to whatever picks a line up: short, and of plain characters
        // that cannot be taken for a line's end or go unseen.
        if !plain_id(&shape.id) {
            said_not_read(
                &mut board,
                format!("card {}: its id is over {ID_MAX} characters or has one that is not allowed", at + 1),
            );
            continue;
        }
        // An action names a card by its id: two cards cannot share one.
        if cards.iter().any(|card| card.id == shape.id) {
            said_not_read(&mut board, format!("{name}: another card has that id"));
            continue;
        }
        let time = |field: &str, text: &Option<String>| match text.as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(text) => parse_time(text).map(Some).ok_or(format!("{name}: its {field} is not a time")),
        };
        let (created_at, due_at) =
            match (time("created_at", &shape.created_at), time("due_at", &shape.due_at)) {
                (Ok(created), Ok(due)) => (created, due),
                (Err(why), _) | (_, Err(why)) => {
                    said_not_read(&mut board, why);
                    continue;
                }
            };
        cards.push(Card {
            id: shape.id,
            raised_by: shape.owner,
            kind,
            tier,
            status: shape.status,
            title: shape.title,
            recommendation: shape.recommendation.filter(|r| !r.trim().is_empty()),
            created_at,
            due_at,
            due_whole_day: shape.due_at.as_deref().is_some_and(|text| !text.trim().contains(['T', 't', ' '])),
            hub_only: kind == Kind::Fire
                || has_amount(&shape.amount_cents)
                || has_amount(&shape.amount)
                || risk_is_high(&shape.risk),
        });
    }
    for card in cards {
        match card.kind {
            Kind::Decide | Kind::Fire | Kind::Question => board.decide.push(card),
            Kind::Do => board.yours.push(card),
            Kind::Know => board.know.push(card),
        }
    }
    // What waits on the reader: now before next, then the longest waiting
    // first. What is only to know: the newest first. Either way a card
    // that does not say when it was made comes after those that do, in the
    // file's order: the sorts are stable.
    let undated_last = |card: &Card| card.created_at.is_none();
    board.decide.sort_by_key(|card| (card.tier, undated_last(card), card.created_at));
    board.yours.sort_by_key(|card| (card.tier, undated_last(card), card.created_at));
    board.know.sort_by_key(|card| (undated_last(card), std::cmp::Reverse(card.created_at)));
    board
}

/// What serde says is wrong with a card, in fewer words.
fn missing(said: &str) -> String {
    match said.strip_prefix("missing field `") {
        Some(rest) => format!("it has no {}", rest.split('`').next().unwrap_or("")),
        None => said.split(" at line ").next().unwrap_or(said).to_string(),
    }
}

impl Board {
    pub fn is_empty(&self) -> bool {
        self.decide.is_empty() && self.yours.is_empty() && self.know.is_empty()
    }

    /// Every open card.
    fn cards(&self) -> impl Iterator<Item = &Card> {
        self.decide.iter().chain(&self.yours).chain(&self.know)
    }

    /// The Board as the page reads it. Only what is shown leaves. A card
    /// in `sent` says what was done to it and when.
    pub fn to_json(&self, sent: &[Sent]) -> Value {
        let tier = |tier: Tier| if tier == Tier::Now { "now" } else { "next" };
        let card = |c: &Card| {
            let acted = sent.iter().find(|s| s.card == c.id);
            json!({
                "id": c.id,
                "kind": c.kind.word(),
                "tier": tier(c.tier),
                "status": c.status,
                "title": c.title,
                "recommendation": c.recommendation,
                "raised_by": c.raised_by,
                "created_at": c.created_at.map(|secs| secs * 1000),
                "due_at": c.due_at.map(|secs| secs * 1000),
                "due_whole_day": c.due_whole_day,
                "sent": acted.map(|s| json!({ "action": s.action, "at": s.at_ms })),
                "hub_only": c.hub_only,
                // What the page may offer: the actions that fit this card
                // as it stands, or none for a card that is the Hub's.
                "actions": if c.hub_only {
                    Vec::new()
                } else {
                    Action::ALL.iter().filter(|a| a.fits(c)).map(|a| a.word()).collect::<Vec<_>>()
                },
            })
        };
        let list = |cards: &[Card]| cards.iter().map(card).collect::<Vec<Value>>();
        let not_read = match self.not_read_count {
            0 => None,
            n => {
                let more = n.saturating_sub(self.not_read.len());
                let named = self.not_read.join("; ");
                let cards = if n == 1 { "1 card".to_string() } else { format!("{n} cards") };
                Some(if more > 0 {
                    format!(
                        "{cards} in {FILE} could not be read and are not shown: {named}; and {more} more."
                    )
                } else if n == 1 {
                    format!("{cards} in {FILE} could not be read and is not shown: {named}.")
                } else {
                    format!("{cards} in {FILE} could not be read and are not shown: {named}.")
                })
            }
        };
        json!({
            "decide": list(&self.decide),
            "yours": list(&self.yours),
            "know": list(&self.know),
            "empty": self.is_empty(),
            "not_read": not_read,
            "problem": self.problem,
        })
    }
}

/// One card that was acted on and is waiting: kept in `SENT`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub card: String,
    /// The action, as the outbox writes it.
    pub action: String,
    /// When, in milliseconds since 1970.
    pub at_ms: u64,
    /// The line added to the outbox, whole: undoing it takes out exactly
    /// this line.
    line: String,
}

impl Sent {
    pub fn new(card: &str, action: &str, at_ms: u64, line: &str) -> Sent {
        Sent { card: card.to_string(), action: action.to_string(), at_ms, line: line.to_string() }
    }

    /// The action's line, whole.
    pub fn line(&self) -> &str {
        &self.line
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SentFile {
    sent: Vec<Sent>,
}

/// The cards acted on and waiting. None kept: none. A file that is there
/// and cannot be read is an error, with why: then nothing may be acted on,
/// because which cards already were is not known.
pub fn sent(folder: Option<&Path>) -> Result<Vec<Sent>, String> {
    let Some(folder) = folder else { return Ok(Vec::new()) };
    let unreadable = |why: String| {
        format!(
            "{SENT}, which keeps which cards were acted on, was not read: {why}. Until it is put right or \
             deleted, no card can be acted on from the page."
        )
    };
    match read_file(&folder.join(SENT)) {
        Ok(None) => Ok(Vec::new()),
        Ok(Some(text)) => serde_json::from_str::<SentFile>(&text)
            .map(|file| file.sent)
            .map_err(|e| unreadable(format!("it is not what Lillignode wrote ({e})"))),
        Err(why) => Err(unreadable(why)),
    }
}

fn write_sent(folder: &Path, sent: Vec<Sent>) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&SentFile { sent }).unwrap_or_default();
    private::write_whole(folder, SENT, &text)
}

/// Said when a file of the Board's that should be this user's alone is
/// not: anyone else who can write it could add an action, or hide one.
fn not_private(name: &str) -> std::io::Error {
    std::io::Error::other(format!(
        "{name} can be read or written by others on this machine, so it is not trusted. Delete it, and \
         Lillignode makes it again as yours alone"
    ))
}

/// Add one line to the end of the outbox. The outbox must be a plain file
/// or not there yet: a link in its place is not written through. If what
/// is there does not end with a line's end (a write cut short), one is
/// put first, so this line is never joined onto another.
fn add_line(folder: &Path, line: &str) -> std::io::Result<()> {
    let path = folder.join(OUTBOX);
    match std::fs::symlink_metadata(&path) {
        Ok(found) if !found.is_file() => {
            return Err(std::io::Error::other("the outbox is not a plain file"));
        }
        // Made new, empty, as this user's alone, before a line is in it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => private::write_whole(folder, OUTBOX, "")?,
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    // Whoever else could write it could add a line of their own.
    if !private::is_private(&path) {
        return Err(not_private(OUTBOX));
    }
    let mut file = std::fs::OpenOptions::new().read(true).append(true).open(&path)?;
    let mut whole = Vec::new();
    file.read_to_end(&mut whole)?;
    let start = if whole.is_empty() || whole.ends_with(b"\n") { "" } else { "\n" };
    file.write_all(format!("{start}{line}\n").as_bytes())?;
    file.sync_all()
}

/// Take exactly `line` out of the outbox, once. False: it is not there,
/// or the outbox has been taken. The outbox is taken whole first, by
/// renaming it, as whatever picks lines up takes it; so the two never
/// change the same file.
fn take_line(folder: &Path, line: &str) -> std::io::Result<bool> {
    let (path, mine) = (folder.join(OUTBOX), folder.join(format!("{OUTBOX}.undo")));
    let _ = std::fs::remove_file(&mine);
    match std::fs::rename(&path, &mine) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }
    let taken = (|| {
        let text = std::fs::read_to_string(&mine)?;
        let mut lines: Vec<&str> = text.lines().collect();
        let found = lines.iter().rposition(|l| *l == line);
        if let Some(at) = found {
            lines.remove(at);
            let mut left = lines.join("\n");
            if !left.is_empty() {
                left.push('\n');
            }
            let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(&mine)?;
            file.write_all(left.as_bytes())?;
            file.sync_all()?;
        }
        Ok(found.is_some())
    })();
    // Put back whatever happened: the other lines are still waiting.
    let back = std::fs::rename(&mine, &path);
    match (taken, back) {
        (Ok(found), Ok(())) => Ok(found),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

/// What the reader was shown of a card when they acted on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub title: String,
    pub recommendation: Option<String>,
}

/// What was done, for the sentence that says so.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acted {
    pub card: String,
    pub title: String,
    pub action: Action,
    /// It was done, but something about it is not as it should be: said
    /// with the rest.
    pub warning: Option<String>,
    /// The outbox line it added, or took out.
    pub line: String,
}

const NOWHERE: &str =
    "Lillignode has nowhere to save, so the Board's outbox cannot be written. Nothing was sent.";

/// A note as it is kept: one line, no control characters, its ends
/// trimmed.
fn clean_note(note: &str) -> String {
    let one_line: String = note.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one_line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Do `action` to the open card `card`: one line is added to the outbox,
/// and the card is kept as sent and waiting. Nothing is written to the
/// Board's own file. `seen` is what the reader was shown of the card: if
/// the card with that id now says something else, it is not the card they
/// acted on, and it is refused. Refused too, with the words to say, if the
/// card is not open, the action does not fit it, it was already acted on,
/// a Revise has no note, or the outbox cannot be written. Then nothing
/// was sent.
pub fn act(
    folder: Option<&Path>,
    card: &str,
    action: &str,
    note: &str,
    seen: &Seen,
    now: SystemTime,
) -> Result<Acted, String> {
    let board = read(folder);
    act_on(folder, &board, card, action, note, seen, now, None)
}

/// Where an action goes when it is not added to the outbox file: held by
/// whoever keeps the Board for it, to be sent on later. `hold` says why if
/// it cannot be. `waiting` is what that keeper already holds or has sent,
/// so a card is not acted on twice. Nothing is written to `SENT` here:
/// the keeper writes there only what it has really sent on.
pub struct Held<'a> {
    pub hold: &'a dyn Fn(&Sent) -> Result<(), String>,
    pub waiting: Vec<Sent>,
}

/// As `act`, on `board` as given (from the Board's file or from an
/// adapter), and with the line given to `hold` in place of the outbox
/// file, if there is one. Which cards wait is kept in `SENT` either way.
#[allow(clippy::too_many_arguments)]
pub fn act_on(
    folder: Option<&Path>,
    board: &Board,
    card: &str,
    action: &str,
    note: &str,
    seen: &Seen,
    now: SystemTime,
    held: Option<Held>,
) -> Result<Acted, String> {
    let Some(folder) = folder else { return Err(NOWHERE.to_string()) };
    if let Some(problem) = &board.problem {
        return Err(format!("{problem} Nothing was sent."));
    }
    let Some(found) = board.cards().find(|c| c.id == card) else {
        return Err("That card is no longer open on the Board. Nothing was sent.".to_string());
    };
    if found.title != seen.title || found.recommendation != seen.recommendation {
        return Err(
            "That card has changed since the page showed it. Read it again, then decide. Nothing was sent."
                .to_string(),
        );
    }
    if found.hub_only {
        return Err("This card is approved on the Hub, not from this page. Nothing was sent.".to_string());
    }
    let Some(action) = Action::parse(action).filter(|action| action.fits(found)) else {
        if action == "hold" && found.status == "hold" {
            return Err("That card is already on hold. Nothing was sent.".to_string());
        }
        return Err("That cannot be done to this card. Nothing was sent.".to_string());
    };
    let note = if action == Action::Revise { clean_note(note) } else { String::new() };
    if action == Action::Revise && note.is_empty() {
        return Err("Revise needs a short note saying what to change. Nothing was sent.".to_string());
    }
    if note.chars().count() > NOTE_MAX {
        return Err(format!("The note is over {NOTE_MAX} characters. Nothing was sent."));
    }
    // A record of what waits that others could have written is not
    // trusted, nor is acting through it.
    if folder.join(SENT).exists() && !private::is_private(&folder.join(SENT)) {
        return Err(format!("{}. Nothing was sent.", not_private(SENT)));
    }
    // Which cards were already acted on must be known. Only cards still
    // open are kept as waiting.
    let mut waiting: Vec<Sent> = sent(Some(folder))
        .map_err(|why| format!("{why} Nothing was sent."))?
        .into_iter()
        .filter(|s| board.cards().any(|c| c.id == s.card))
        .collect();
    let also = held.as_ref().map(|h| h.waiting.clone()).unwrap_or_default();
    if let Some(earlier) = waiting.iter().chain(also.iter()).find(|s| s.card == card) {
        let did = Action::parse(&earlier.action).map_or("an action", Action::name);
        return Err(format!(
            "{did} was already sent for that card, and it is waiting. Nothing more was sent."
        ));
    }
    let at_ms = now.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let line = json!({
        "card": card,
        "action": action.word(),
        "note": note,
        "at": written_time(now),
        "title": found.title,
    })
    .to_string();
    if let Some(held) = held {
        let entry =
            Sent { card: card.to_string(), action: action.word().to_string(), at_ms, line: line.clone() };
        (held.hold)(&entry).map_err(|why| format!("{why} Nothing was sent."))?;
        return Ok(Acted { card: card.to_string(), title: found.title.clone(), action, warning: None, line });
    }
    let could_not =
        |e: std::io::Error| format!("The Board's outbox could not be written ({e}). Nothing was sent.");
    // Kept as waiting first: if the line then cannot be added, the keeping
    // is taken back, and nothing was sent.
    let before = waiting.clone();
    waiting.push(Sent {
        card: card.to_string(),
        action: action.word().to_string(),
        at_ms,
        line: line.clone(),
    });
    write_sent(folder, waiting).map_err(could_not)?;
    if let Err(e) = add_line(folder, &line) {
        let _ = write_sent(folder, before);
        return Err(could_not(e));
    }
    Ok(Acted { card: card.to_string(), title: found.title.clone(), action, warning: None, line })
}

/// Keeps that this action was really sent on, so the card shows as waiting
/// and is not acted on twice, also after Lillignode is started again. The
/// caller keeps this and `forget_sent` from running at the same time.
pub fn keep_sent(folder: Option<&Path>, entry: &Sent) -> Result<(), String> {
    let Some(folder) = folder else { return Ok(()) };
    let mut waiting = sent(Some(folder))?;
    waiting.retain(|s| s.card != entry.card);
    waiting.push(entry.clone());
    write_sent(folder, waiting).map_err(|e| format!("{SENT} could not be written ({e})"))
}

/// The card whose action's line this is waits no longer: what it was
/// sent to refused it, so it can be acted on again. Nothing is said if
/// that cannot be kept; the card then still shows as waiting.
pub fn forget_sent(folder: Option<&Path>, line: &str) {
    let Some(folder) = folder else { return };
    let Ok(waiting) = sent(Some(folder)) else { return };
    let left: Vec<Sent> = waiting.into_iter().filter(|s| s.line != line).collect();
    let _ = write_sent(folder, left);
}

/// Undo what was done to `card`, within `UNDO_FOR` of doing it and only
/// while its line is still in the outbox: the line is taken out, and the
/// card is no longer waiting. Refused, with the words to say, otherwise;
/// then nothing was changed.
pub fn undo(folder: Option<&Path>, card: &str, now: SystemTime) -> Result<Acted, String> {
    let board = read(folder);
    undo_on(folder, &board, card, now)
}

/// As `undo`, on `board` as given.
pub fn undo_on(folder: Option<&Path>, board: &Board, card: &str, now: SystemTime) -> Result<Acted, String> {
    let Some(folder) = folder else { return Err(NOWHERE.to_string()) };
    let mut waiting = sent(Some(folder)).map_err(|why| format!("{why} Nothing was changed."))?;
    let Some(at) = waiting.iter().position(|s| s.card == card) else {
        return Err("There is nothing to undo for that card. Nothing was changed.".to_string());
    };
    let was = waiting[at].clone();
    let done_at = SystemTime::UNIX_EPOCH + Duration::from_millis(was.at_ms);
    // A time that is still to come is not within ten seconds of now: the
    // clock went back, or the time kept was changed.
    let in_time = now.duration_since(done_at).is_ok_and(|since| since <= UNDO_FOR + UNDO_GRACE);
    if !in_time {
        return Err(format!(
            "That was sent more than {} seconds ago, so it cannot be undone from here. Nothing was changed.",
            UNDO_FOR.as_secs()
        ));
    }
    match take_line(folder, &was.line) {
        Ok(true) => {}
        Ok(false) => {
            return Err(
                "It has already been picked up from the outbox, so it cannot be undone. Nothing was changed."
                    .to_string(),
            )
        }
        Err(e) => return Err(format!("The Board's outbox could not be changed ({e}). Nothing was changed.")),
    }
    waiting.remove(at);
    // The line is out: it was undone, whatever happens next. If this
    // cannot be written the card still shows as waiting, though nothing
    // waits: said with the rest, so it is not taken for sent.
    let warning = write_sent(folder, waiting).err().map(|e| {
        format!("But {SENT} could not be written ({e}), so the card still shows as waiting, though nothing waits.")
    });
    let title = board.cards().find(|c| c.id == card).map(|c| c.title.clone()).unwrap_or_default();
    let action = Action::parse(&was.action).unwrap_or(Action::Hold);
    Ok(Acted { card: card.to_string(), title, action, warning, line: was.line })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, owner: &str, kind: &str, tier: &str, status: &str) -> Value {
        json!({ "id": id, "owner": owner, "kind": kind, "tier": tier, "title": format!("title {id}"),
                "recommendation": format!("do {id}"), "status": status })
    }

    fn made(mut card: Value, created_at: &str) -> Value {
        card["created_at"] = json!(created_at);
        card
    }

    fn board(cards: Vec<Value>) -> Board {
        from_text(&json!({ "cards": cards }).to_string())
    }

    fn ids(cards: &[Card]) -> Vec<&str> {
        cards.iter().map(|c| c.id.as_str()).collect()
    }

    /// A folder of this test's own, with these cards as its Board.
    fn folder(name: &str, cards: Vec<Value>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lillignode-board-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), json!({ "cards": cards }).to_string()).unwrap();
        dir
    }

    fn outbox(dir: &Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join(OUTBOX))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Act on a card having seen it as the Board's file has it now.
    fn act_seen(dir: &Path, card: &str, action: &str, note: &str, now: SystemTime) -> Result<Acted, String> {
        let board = read(Some(dir));
        let seen = board
            .cards()
            .find(|c| c.id == card)
            .map(|c| Seen { title: c.title.clone(), recommendation: c.recommendation.clone() })
            .unwrap_or_default();
        act(Some(dir), card, action, note, &seen, now)
    }

    fn waiting(dir: &Path) -> Vec<Sent> {
        sent(Some(dir)).unwrap()
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_795_000_000 + secs)
    }

    #[test]
    fn open_means_queued_revise_hold_or_more_and_a_draft_is_not_shown() {
        let b = board(vec![
            card("q", "x", "decide", "now", "queued"),
            card("r", "x", "decide", "now", "revise"),
            card("h", "x", "decide", "now", "hold"),
            card("m", "x", "decide", "now", "more"),
            card("draft", "x", "decide", "now", "draft"),
            card("done", "x", "decide", "now", "done"),
            card("open", "x", "decide", "now", "open"),
            card("approved", "x", "know", "now", "approved"),
            card("empty", "x", "do", "now", ""),
        ]);
        assert_eq!(ids(&b.decide), ["q", "r", "h", "m"]);
        assert!(b.yours.is_empty() && b.know.is_empty());
        assert_eq!(b.not_read_count, 0, "a card that is not open is not a mistake");
        assert_eq!(b.to_json(&[])["decide"][1]["status"], "revise", "the page is told which");
    }

    #[test]
    fn every_open_do_card_is_the_readers_and_says_who_raised_it() {
        let b = board(vec![
            card("a", "Codex (web)", "do", "now", "queued"),
            card("b", "Claude Code (api)", "do", "next", "queued"),
            card("c", "me", "do", "now", "queued"),
            card("d", "Codex (web)", "do", "now", "draft"),
        ]);
        assert_eq!(ids(&b.yours), ["a", "c", "b"], "whoever raised it; now before next");
        let shown = b.to_json(&[]);
        assert_eq!(shown["yours"][0]["raised_by"], "Codex (web)");
        assert_eq!(shown["yours"][1]["raised_by"], "me", "\"me\" is a name like any other");
        assert_eq!(shown["yours"][2]["raised_by"], "Claude Code (api)");
    }

    #[test]
    fn what_waits_on_the_reader_is_oldest_first_and_what_is_to_know_newest_first() {
        let b = board(vec![
            made(card("d-old", "x", "decide", "now", "queued"), "2026-11-01T08:00:00Z"),
            made(card("d-next", "x", "decide", "next", "queued"), "2026-11-22T08:00:00Z"),
            made(card("f-new", "x", "fire", "now", "queued"), "2026-11-20T08:00:00Z"),
            made(card("q-mid", "x", "question", "now", "hold"), "2026-11-10T08:00:00+02:00"),
            card("d-undated", "x", "decide", "now", "queued"),
            made(card("k-old", "x", "know", "now", "queued"), "2026-11-02"),
            made(card("k-new", "x", "know", "next", "queued"), "2026-11-21T23:59:59Z"),
            card("k-undated", "x", "know", "now", "queued"),
            made(card("y-old", "x", "do", "now", "queued"), "2026-11-03T00:00:00Z"),
            made(card("y-new", "x", "do", "now", "queued"), "2026-11-04T00:00:00Z"),
        ]);
        // Decide and only-you-can-do: now before next, then the longest
        // waiting first; a card that does not say when, after those that do.
        assert_eq!(ids(&b.decide), ["d-old", "q-mid", "f-new", "d-undated", "d-next"]);
        assert_eq!(ids(&b.yours), ["y-old", "y-new"]);
        // Good to know: newest first, whatever its tier.
        assert_eq!(ids(&b.know), ["k-new", "k-old", "k-undated"]);
        assert_eq!(b.to_json(&[])["know"][1]["created_at"], 1_793_577_600_000_i64, "2 November 2026, in ms");
    }

    #[test]
    fn times_are_read_as_written_and_a_time_that_is_not_one_is_said() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-23T18:04:05Z"), Some(1_795_457_045));
        assert_eq!(parse_time("2026-11-23T19:04:05+01:00"), Some(1_795_457_045));
        assert_eq!(parse_time("2026-11-23T12:34:05.250-05:30"), Some(1_795_457_045));
        assert_eq!(parse_time("2026-11-23 18:04Z"), Some(1_795_457_040));
        assert_eq!(parse_time("2026-11-23"), Some(1_795_392_000));
        assert_eq!(parse_time("2024-02-29"), Some(1_709_164_800), "a leap day");
        for bad in [
            "",
            "tomorrow",
            "2026-02-30",
            "2026-13-01",
            "2026-11-23T25:00:00Z",
            "2026-11-23T10:00:00",
            "23/11/2026",
            "2026-11-23T10:00:00+0100",
            "20261123",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert_eq!(
            written_time(SystemTime::UNIX_EPOCH + Duration::from_secs(1_795_457_045)),
            "2026-11-23T18:04:05Z"
        );
        assert_eq!(parse_time(&written_time(at(77))), Some(1_795_000_077), "what is written reads back");
        // On a card: a bad time leaves the card out and says which field.
        let mut due = card("due", "x", "do", "now", "queued");
        due["due_at"] = json!("2026-12-03");
        let mut bad = card("bad", "x", "do", "now", "queued");
        bad["due_at"] = json!("soon");
        let b = board(vec![due, bad, made(card("worse", "x", "do", "now", "queued"), "last week")]);
        assert_eq!(ids(&b.yours), ["due"]);
        assert_eq!(b.to_json(&[])["yours"][0]["due_at"], 1_796_256_000_000_i64, "3 December 2026, in ms");
        assert_eq!(b.to_json(&[])["yours"][0]["due_whole_day"], true, "a day, not a moment");
        let mut timed = card("timed", "x", "do", "now", "queued");
        timed["due_at"] = json!("2026-12-03T17:00:00-06:00");
        assert_eq!(board(vec![timed]).to_json(&[])["yours"][0]["due_whole_day"], false);
        assert_eq!(
            b.not_read,
            ["card bad: its due_at is not a time", "card worse: its created_at is not a time"]
        );
    }

    #[test]
    fn only_what_is_shown_leaves() {
        let mut full = card("c", "Codex (web)", "decide", "now", "queued");
        for (field, value) in [
            ("risk", json!("a private risk")),
            ("cost", json!("a private cost")),
            ("blocks", json!(["x"])),
            ("why", json!("a private why")),
            ("options", json!(["one", "two"])),
            ("silo", json!("a private silo")),
            ("a later field", json!(true)),
        ] {
            full[field] = value;
        }
        let shown = board(vec![full]).to_json(&[]).to_string();
        assert!(shown.contains("title c"), "{shown}");
        for private in ["private", "\"x\"", "\"two\"", "later"] {
            assert!(!shown.contains(private), "{private}: {shown}");
        }
    }

    #[test]
    fn an_empty_board_and_a_missing_file_are_both_just_empty() {
        assert!(board(vec![]).is_empty());
        assert!(board(vec![card("c", "x", "know", "now", "done")]).is_empty());
        let nowhere = std::env::temp_dir().join(format!("lillignode-board-none-{}", std::process::id()));
        let b = read(Some(&nowhere));
        assert!(b.is_empty() && b.problem.is_none());
        assert!(read(None).is_empty());
        let shown = board(vec![]).to_json(&[]);
        assert_eq!((shown["empty"].as_bool(), shown["problem"].is_null()), (Some(true), true));
    }

    #[test]
    fn a_file_that_cannot_be_read_says_so_and_why() {
        for text in ["{", "[]", r#"{"cards": {}}"#] {
            let b = from_text(text);
            let said = b.problem.clone().unwrap_or_default();
            assert!(
                said.starts_with("The Board's file, board.json, was not read: it is not a list of cards"),
                "{said}"
            );
            assert!(said.ends_with("Nothing else is affected."), "{said}");
            assert!(b.is_empty());
        }
        let dir = folder("big", vec![]);
        std::fs::write(dir.join(FILE), " ".repeat(MAX_BYTES as usize + 10)).unwrap();
        let said = read(Some(&dir)).problem.unwrap();
        assert!(said.contains("it is over 1 MB"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_card_that_cannot_be_read_is_left_out_and_named_and_the_rest_are_shown() {
        let mut no_title = card("t", "x", "decide", "now", "queued");
        no_title.as_object_mut().unwrap().remove("title");
        let mut blank = card("b", "x", "decide", "now", "queued");
        blank["title"] = json!("   ");
        let b = board(vec![
            card("good", "x", "decide", "now", "queued"),
            no_title,
            card("k", "x", "later", "now", "queued"),
            card("n", "x", "decide", "someday", "queued"),
            blank,
            json!("not a card"),
            card("good", "y", "know", "now", "queued"),
        ]);
        assert_eq!(ids(&b.decide), ["good"]);
        assert!(b.know.is_empty(), "a second card with the same id is not shown");
        assert_eq!(b.not_read_count, 6);
        let said = b.to_json(&[])["not_read"].as_str().unwrap().to_string();
        assert_eq!(
            said,
            "6 cards in board.json could not be read and are not shown: card t: it has no title; card k: no \
             kind called \"later\"; card n: no tier called \"someday\"; and 3 more."
        );
        let one = board(vec![card("k", "x", "later", "now", "queued")]).to_json(&[]);
        assert_eq!(
            one["not_read"],
            "1 card in board.json could not be read and is not shown: card k: no kind called \"later\"."
        );
        // A card that is not open is not checked: it is not shown either way.
        assert_eq!(board(vec![card("k", "x", "later", "now", "done")]).not_read_count, 0);
    }

    #[test]
    fn a_very_long_file_is_read_only_so_far() {
        let cards: Vec<Value> =
            (0..MAX_CARDS + 7).map(|i| card(&i.to_string(), "x", "know", "now", "queued")).collect();
        let b = board(cards);
        assert_eq!(b.know.len(), MAX_CARDS);
        assert_eq!(b.not_read_count, 7);
    }

    #[test]
    fn the_stand_in_file_is_eight_open_cards_that_all_read() {
        let text = include_str!("../examples/board-standin.json");
        let b = from_text(text);
        assert_eq!((b.problem.as_deref(), b.not_read_count), (None, 0));
        let file: Value = serde_json::from_str(text).unwrap();
        assert_eq!(file["cards"].as_array().unwrap().len(), 8);
        assert_eq!((b.decide.len(), b.yours.len(), b.know.len()), (4, 2, 2), "every card is shown somewhere");
        for kind in ["decide", "fire", "question", "do", "know"] {
            assert!(text.contains(&format!("\"kind\": \"{kind}\"")), "{kind}");
        }
        assert!(b.cards().all(|c| c.created_at.is_some()), "each says when it was made");
        assert!(b.cards().any(|c| c.due_at.is_some()), "one is due");
    }

    #[test]
    fn an_action_adds_one_line_to_the_outbox_and_the_card_waits_and_the_board_file_is_untouched() {
        let dir = folder(
            "act",
            vec![
                card("d", "Codex (web)", "decide", "now", "queued"),
                card("y", "Codex (web)", "do", "now", "queued"),
                card("k", "Codex (web)", "know", "now", "queued"),
            ],
        );
        let before = std::fs::read(dir.join(FILE)).unwrap();
        let acted = act_seen(&dir, "d", "revise", "  Keep it,\tbut\nsay when it goes.  ", at(5)).unwrap();
        assert_eq!(
            (acted.card.as_str(), acted.title.as_str(), acted.action),
            ("d", "title d", Action::Revise)
        );
        act_seen(&dir, "y", "done", "a note that is not wanted", at(6)).unwrap();
        act_seen(&dir, "k", "got-it", "", at(7)).unwrap();
        assert_eq!(
            outbox(&dir),
            [
                json!({ "card": "d", "action": "revise", "note": "Keep it, but say when it goes.", "at": written_time(at(5)), "title": "title d" }),
                json!({ "card": "y", "action": "done", "note": "", "at": written_time(at(6)), "title": "title y" }),
                json!({ "card": "k", "action": "got-it", "note": "", "at": written_time(at(7)), "title": "title k" }),
            ],
            "card id, action, note, time, and the title that was shown: one line each, in order"
        );
        assert_eq!(
            std::fs::read(dir.join(FILE)).unwrap(),
            before,
            "nothing is written to the Board's own file"
        );
        // Each card now shows as sent, with what and when.
        let shown = read(Some(&dir)).to_json(&waiting(&dir));
        assert_eq!(shown["decide"][0]["sent"]["action"], "revise");
        assert_eq!(shown["decide"][0]["sent"]["at"], 1_795_000_005_000_u64);
        assert_eq!(shown["yours"][0]["sent"]["action"], "done");
        assert_eq!(shown["know"][0]["sent"]["action"], "got-it");
        // And is not acted on twice.
        let again = act_seen(&dir, "d", "approve", "", at(30)).unwrap_err();
        assert_eq!(again, "Revise was already sent for that card, and it is waiting. Nothing more was sent.");
        assert_eq!(outbox(&dir).len(), 3);
        // Picked up, and the Board's keeper takes the card off: it waits no
        // longer, and the next action forgets it.
        std::fs::write(dir.join(OUTBOX), "").unwrap();
        std::fs::write(
            dir.join(FILE),
            json!({ "cards": [card("d", "x", "decide", "now", "approved"), card("y", "x", "do", "now", "queued"),
                              card("n", "x", "know", "now", "queued")] })
            .to_string(),
        )
        .unwrap();
        let shown = read(Some(&dir)).to_json(&waiting(&dir));
        assert!(shown["decide"].as_array().unwrap().is_empty(), "no longer open: no longer shown");
        assert_eq!(shown["yours"][0]["sent"]["action"], "done", "picked up but still open: still waiting");
        act_seen(&dir, "n", "got-it", "", at(60)).unwrap();
        let kept: Vec<String> = waiting(&dir).into_iter().map(|s| s.card).collect();
        assert_eq!(kept, ["y", "n"], "cards no longer open are let go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_action_that_does_not_fit_is_refused_and_then_nothing_is_written() {
        let dir = folder(
            "refused",
            vec![
                card("d", "x", "decide", "now", "queued"),
                card("f", "x", "fire", "now", "queued"),
                card("y", "x", "do", "now", "queued"),
                card("k", "x", "know", "now", "queued"),
                card("draft", "x", "decide", "now", "draft"),
            ],
        );
        let no =
            |card: &str, action: &str, note: &str| act_seen(&dir, card, action, note, at(1)).unwrap_err();
        let not_this = "That cannot be done to this card. Nothing was sent.";
        for (card, action) in [
            ("d", "done"),
            ("d", "got-it"),
            ("y", "approve"),
            ("y", "got-it"),
            ("k", "done"),
            ("k", "hold"),
            ("d", "delete"),
            ("d", ""),
            ("d", "Approve"),
        ] {
            assert_eq!(no(card, action, "x"), not_this, "{card} {action}");
        }
        let gone = "That card is no longer open on the Board. Nothing was sent.";
        for card in ["nobody", "draft", "", "../board"] {
            assert_eq!(no(card, "approve", ""), gone, "{card}");
        }
        assert_eq!(
            no("d", "revise", " \n\t "),
            "Revise needs a short note saying what to change. Nothing was sent."
        );
        assert_eq!(
            no("d", "revise", &"x".repeat(NOTE_MAX + 1)),
            "The note is over 280 characters. Nothing was sent."
        );
        assert_eq!(act(None, "d", "approve", "", &Seen::default(), at(1)).unwrap_err(), NOWHERE);
        assert!(!dir.join(OUTBOX).exists() && !dir.join(SENT).exists(), "nothing was written at all");
        // A note of exactly the most allowed is taken.
        act_seen(&dir, "d", "revise", &"é".repeat(NOTE_MAX), at(2)).unwrap();
        // A Board that cannot be read: nothing can be acted on.
        std::fs::write(dir.join(FILE), "{").unwrap();
        let said = no("f", "approve", "");
        assert!(
            said.starts_with("The Board's file, board.json, was not read")
                && said.ends_with("Nothing was sent.")
        );
        // The outbox cannot be written: nothing is sent, and the card does
        // not show as waiting.
        let dir2 = folder("no-outbox", vec![card("d", "x", "decide", "now", "queued")]);
        std::fs::create_dir_all(dir2.join(OUTBOX)).unwrap();
        let said = act_seen(&dir2, "d", "approve", "", at(1)).unwrap_err();
        assert!(
            said.starts_with("The Board's outbox could not be written (")
                && said.ends_with("Nothing was sent.")
        );
        assert!(waiting(&dir2).is_empty(), "not kept as waiting");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn undo_takes_the_line_out_within_ten_seconds_and_only_if_it_is_still_there() {
        let cards = vec![
            card("a", "x", "decide", "now", "queued"),
            card("b", "x", "decide", "now", "queued"),
            card("c", "x", "know", "now", "queued"),
        ];
        let dir = folder("undo", cards);
        act_seen(&dir, "a", "approve", "", at(0)).unwrap();
        act_seen(&dir, "b", "hold", "", at(1)).unwrap();
        act_seen(&dir, "c", "got-it", "", at(2)).unwrap();
        // Within ten seconds: its line, and only its line, is taken out.
        let undone = undo(Some(&dir), "b", at(9)).unwrap();
        assert_eq!(
            (undone.card.as_str(), undone.action, undone.title.as_str()),
            ("b", Action::Hold, "title b")
        );
        let left: Vec<String> =
            outbox(&dir).iter().map(|l| l["card"].as_str().unwrap().to_string()).collect();
        assert_eq!(left, ["a", "c"]);
        assert!(read(Some(&dir)).to_json(&waiting(&dir))["decide"][1]["sent"].is_null(), "b waits no longer");
        // And it can be acted on again.
        act_seen(&dir, "b", "approve", "", at(10)).unwrap();
        // Nothing to undo; too late; already picked up.
        assert_eq!(
            undo(Some(&dir), "nobody", at(10)).unwrap_err(),
            "There is nothing to undo for that card. Nothing was changed."
        );
        let too_late =
            "That was sent more than 10 seconds ago, so it cannot be undone from here. Nothing was changed.";
        assert_eq!(undo(Some(&dir), "a", at(13)).unwrap_err(), too_late, "a was sent at 0: 13 s is too late");
        // A clock that has gone back is not "within ten seconds".
        assert_eq!(
            undo(Some(&dir), "c", at(1)).unwrap_err(),
            too_late,
            "c was sent at 2: before then is not in time"
        );
        assert_eq!(outbox(&dir).len(), 3, "nothing was taken out");
        let text = std::fs::read_to_string(dir.join(OUTBOX)).unwrap();
        let without_c: String =
            text.lines().filter(|l| !l.contains("\"c\"")).map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join(OUTBOX), without_c).unwrap();
        assert_eq!(
            undo(Some(&dir), "c", at(4)).unwrap_err(),
            "It has already been picked up from the outbox, so it cannot be undone. Nothing was changed."
        );
        assert_eq!(
            read(Some(&dir)).to_json(&waiting(&dir))["know"][0]["sent"]["action"],
            "got-it",
            "still waiting"
        );
        // The limit is ten seconds and a little for the request to arrive:
        // twelve is in time, and the moment after is not.
        let edge = folder(
            "undo-edge",
            vec![card("a", "x", "decide", "now", "queued"), card("b", "x", "decide", "now", "queued")],
        );
        act_seen(&edge, "a", "approve", "", at(0)).unwrap();
        act_seen(&edge, "b", "approve", "", at(0)).unwrap();
        assert!(undo(Some(&edge), "a", at(12)).is_ok());
        assert_eq!(undo(Some(&edge), "b", at(0) + Duration::from_millis(12_001)).unwrap_err(), too_late);
        let _ = std::fs::remove_dir_all(&edge);
        // Undoing the last line leaves an empty outbox, not a broken one.
        let one = folder("undo-one", vec![card("a", "x", "decide", "now", "queued")]);
        act_seen(&one, "a", "approve", "", at(0)).unwrap();
        undo(Some(&one), "a", at(1)).unwrap();
        assert_eq!(std::fs::read_to_string(one.join(OUTBOX)).unwrap(), "");
        assert!(waiting(&one).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&one);
    }

    #[test]
    fn while_which_cards_were_acted_on_cannot_be_read_nothing_can_be_acted_on() {
        let dir = folder("sent-broken", vec![card("a", "x", "decide", "now", "queued")]);
        act_seen(&dir, "a", "approve", "", at(0)).unwrap();
        for broken in ["{ not json".to_string(), " ".repeat(MAX_BYTES as usize + 10)] {
            std::fs::write(dir.join(SENT), broken).unwrap();
            let why = sent(Some(&dir)).unwrap_err();
            assert!(
                why.starts_with("board-sent.json, which keeps which cards were acted on, was not read"),
                "{why}"
            );
            assert!(why.ends_with("no card can be acted on from the page."), "{why}");
            // Not acted on again, and not undone: which were is not known.
            let said = act_seen(&dir, "a", "approve", "", at(1)).unwrap_err();
            assert!(said.ends_with("Nothing was sent."), "{said}");
            assert!(undo(Some(&dir), "a", at(1)).unwrap_err().ends_with("Nothing was changed."));
            assert_eq!(outbox(&dir).len(), 1, "no second line");
        }
        // Deleted, as the sentence says: it works again.
        std::fs::remove_file(dir.join(SENT)).unwrap();
        assert!(waiting(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fire_a_card_with_an_amount_and_one_whose_risk_is_high_are_the_hubs() {
        let with = |id: &str, kind: &str, field: &str, value: Value| {
            let mut c = card(id, "x", kind, "now", "queued");
            if !field.is_empty() {
                c[field] = value;
            }
            c
        };
        let b = board(vec![
            with("fire", "fire", "", Value::Null),
            with("money", "decide", "amount_cents", json!(1200)),
            with("money-text", "do", "amount_cents", json!("4000")),
            with("money-words", "do", "amount_cents", json!("forty dollars")),
            with("zero", "decide", "amount_cents", json!(0)),
            with("zero-text", "decide", "amount_cents", json!("0")),
            with("below", "decide", "amount_cents", json!(-5)),
            with("old-field", "decide", "amount", json!(1200)),
            with("not-a-number", "decide", "amount_cents", json!("NaN")),
            with("high", "decide", "risk", json!("High")),
            with("high-said", "question", "risk", json!("high: two callers break")),
            with("high-know", "know", "risk", json!("HIGH")),
            with("no-amount", "decide", "amount_cents", Value::Null),
            with("empty-amount", "decide", "amount_cents", json!("  ")),
            with("low", "decide", "risk", json!("low")),
            with("higher", "decide", "risk", json!("higher than last week")),
            with("plain", "decide", "", Value::Null),
        ]);
        let hub: Vec<&str> = b.cards().filter(|c| c.hub_only).map(|c| c.id.as_str()).collect();
        let mut hub = hub;
        hub.sort_unstable();
        assert_eq!(
            hub,
            [
                "fire",
                "high",
                "high-know",
                "high-said",
                "money",
                "money-text",
                "money-words",
                "not-a-number",
                "old-field"
            ]
        );
        // The page is told, and offered no action for them.
        let shown = b.to_json(&[]);
        let of = |id: &str| -> Value {
            ["decide", "yours", "know"]
                .iter()
                .flat_map(|p| shown[*p].as_array().unwrap().clone())
                .find(|c| c["id"] == id)
                .unwrap()
        };
        assert_eq!((of("fire")["hub_only"].clone(), of("fire")["actions"].clone()), (json!(true), json!([])));
        assert_eq!(of("plain")["actions"], json!(["approve", "revise", "hold"]));
        assert_eq!(of("plain")["hub_only"], false);
        // And an action on one is refused here too, whatever the page sent.
        let dir = folder(
            "hub",
            vec![with("fire", "fire", "", Value::Null), with("money", "do", "amount_cents", json!(5))],
        );
        let hub_words = "This card is approved on the Hub, not from this page. Nothing was sent.";
        for (id, action) in [("fire", "approve"), ("fire", "hold"), ("money", "done")] {
            assert_eq!(act_seen(&dir, id, action, "", at(0)).unwrap_err(), hub_words, "{id} {action}");
        }
        assert!(!dir.join(OUTBOX).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_card_on_hold_offers_approve_and_revise_and_not_hold() {
        let b = board(vec![
            card("held", "x", "decide", "now", "hold"),
            card("q", "x", "question", "now", "queued"),
        ]);
        let shown = b.to_json(&[]);
        assert_eq!(shown["decide"][0]["actions"], json!(["approve", "revise"]));
        assert_eq!(shown["decide"][1]["actions"], json!(["approve", "revise", "hold"]));
        let dir = folder("held", vec![card("held", "x", "decide", "now", "hold")]);
        assert_eq!(
            act_seen(&dir, "held", "hold", "", at(0)).unwrap_err(),
            "That card is already on hold. Nothing was sent."
        );
        assert!(!dir.join(OUTBOX).exists());
        act_seen(&dir, "held", "approve", "", at(1)).unwrap();
        assert_eq!(outbox(&dir)[0]["action"], "approve");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_outbox_and_the_sent_file_are_made_this_users_alone_and_not_trusted_otherwise() {
        let dir = folder(
            "private",
            vec![card("a", "x", "decide", "now", "queued"), card("b", "x", "decide", "now", "queued")],
        );
        act_seen(&dir, "a", "approve", "", at(0)).unwrap();
        assert!(private::is_private(&dir.join(OUTBOX)), "the outbox is made this user's alone");
        assert!(private::is_private(&dir.join(SENT)), "so is the record of what waits");
        // Made by someone else, as open as the folder: refused, and why.
        let open = |name: &str, text: &str| {
            let _ = std::fs::remove_file(dir.join(name));
            std::fs::write(dir.join(name), text).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o666)).unwrap();
            }
        };
        let lines = std::fs::read_to_string(dir.join(OUTBOX)).unwrap();
        open(OUTBOX, &lines);
        let said = act_seen(&dir, "b", "approve", "", at(1)).unwrap_err();
        assert!(
            said.contains("board-outbox.jsonl can be read or written by others on this machine"),
            "{said}"
        );
        assert!(said.ends_with("Nothing was sent."), "{said}");
        assert_eq!(std::fs::read_to_string(dir.join(OUTBOX)).unwrap(), lines, "no line was added");
        std::fs::remove_file(dir.join(OUTBOX)).unwrap();
        let sent_text = std::fs::read_to_string(dir.join(SENT)).unwrap();
        open(SENT, &sent_text);
        let said = act_seen(&dir, "b", "approve", "", at(2)).unwrap_err();
        assert!(said.starts_with("board-sent.json can be read or written by others"), "{said}");
        assert!(!dir.join(OUTBOX).exists(), "nothing was written");
        // Deleted, as the sentence says: made again as this user's alone.
        std::fs::remove_file(dir.join(SENT)).unwrap();
        act_seen(&dir, "b", "approve", "", at(3)).unwrap();
        assert!(private::is_private(&dir.join(OUTBOX)) && private::is_private(&dir.join(SENT)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_card_that_changed_since_it_was_shown_is_not_acted_on() {
        let dir = folder("seen", vec![card("x", "Codex (web)", "decide", "now", "queued")]);
        let seen = Seen { title: "title x".into(), recommendation: Some("do x".into()) };
        // The Board's file is rewritten under the same id before the click arrives.
        let mut other = card("x", "Codex (web)", "decide", "now", "queued");
        other["title"] = json!("Drop the production table");
        std::fs::write(dir.join(FILE), json!({ "cards": [other.clone()] }).to_string()).unwrap();
        let changed =
            "That card has changed since the page showed it. Read it again, then decide. Nothing was sent.";
        assert_eq!(act(Some(&dir), "x", "approve", "", &seen, at(0)).unwrap_err(), changed);
        // Only what it recommends changed: still not the card that was shown.
        other["title"] = json!("title x");
        other["recommendation"] = json!("do the opposite");
        std::fs::write(dir.join(FILE), json!({ "cards": [other] }).to_string()).unwrap();
        assert_eq!(act(Some(&dir), "x", "approve", "", &seen, at(0)).unwrap_err(), changed);
        assert!(!dir.join(OUTBOX).exists() && !dir.join(SENT).exists(), "nothing was written");
        // Shown as it now is, it can be acted on, and the line says which title was approved.
        let now = Seen { title: "title x".into(), recommendation: Some("do the opposite".into()) };
        act(Some(&dir), "x", "approve", "", &now, at(1)).unwrap();
        assert_eq!(outbox(&dir)[0]["title"], "title x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_id_that_could_not_name_a_card_safely_leaves_the_card_out() {
        let long = "x".repeat(ID_MAX + 1);
        for bad in [
            "a\nb",
            "a\u{2028}b",
            "a\u{85}b",
            " padded",
            "padded ",
            "a\u{200b}b",
            "a\u{202e}b",
            "a\"b",
            "a\\b",
            long.as_str(),
        ] {
            let b = board(vec![
                card(bad, "x", "decide", "now", "queued"),
                card("good", "x", "decide", "now", "queued"),
            ]);
            assert_eq!(ids(&b.decide), ["good"], "{bad:?}");
            assert_eq!(b.not_read.len(), 1, "{bad:?}");
            assert!(
                b.not_read[0].starts_with("card 1: its id is over 128 characters or has one"),
                "{:?}",
                b.not_read
            );
        }
        for fine in ["c1", "PROJ-142", "api/keys#3", "docs: install (v2)", "é7", &"x".repeat(ID_MAX)] {
            assert_eq!(board(vec![card(fine, "x", "decide", "now", "queued")]).decide.len(), 1, "{fine:?}");
        }
    }

    #[test]
    fn the_outbox_is_never_joined_onto_never_written_through_a_link_and_taken_whole_to_undo() {
        let cards =
            vec![card("a", "x", "decide", "now", "queued"), card("b", "x", "decide", "now", "queued")];
        // A write cut short left no line's end: the next line starts its own.
        let dir = folder("outbox-cut", cards.clone());
        private::write_whole(&dir, OUTBOX, "{\"card\":\"old\"}").unwrap();
        act_seen(&dir, "a", "approve", "", at(0)).unwrap();
        let text = std::fs::read_to_string(dir.join(OUTBOX)).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(
            text.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()),
            "each line reads alone: {text}"
        );
        // Two lines that are the same to the letter: undo takes one.
        let line = text.lines().last().unwrap().to_string();
        private::write_whole(&dir, OUTBOX, &format!("{line}\n{line}\n")).unwrap();
        undo(Some(&dir), "a", at(1)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(OUTBOX)).unwrap(), format!("{line}\n"));
        assert!(!dir.join(format!("{OUTBOX}.undo")).exists(), "the outbox was put back under its own name");
        // The outbox taken away by whatever picks it up, as it is meant to
        // be: undo finds it gone, and does not bring it back.
        act_seen(&dir, "b", "hold", "", at(2)).unwrap();
        std::fs::rename(dir.join(OUTBOX), dir.join("taken-by-the-keeper")).unwrap();
        assert!(undo(Some(&dir), "b", at(3)).unwrap_err().starts_with("It has already been picked up"));
        assert!(!dir.join(OUTBOX).exists(), "what was picked up is not written back");
        // Something that is not a plain file where the outbox goes: refused.
        let dir2 = folder("outbox-folder", cards);
        std::fs::create_dir_all(dir2.join(OUTBOX)).unwrap();
        assert!(act_seen(&dir2, "a", "approve", "", at(0)).unwrap_err().ends_with("Nothing was sent."));
        // A file already where the new one is written beside: not written
        // through, but put aside.
        let dir3 = folder("beside", vec![card("a", "x", "decide", "now", "queued")]);
        let board_before = std::fs::read(dir3.join(FILE)).unwrap();
        std::fs::hard_link(dir3.join(FILE), dir3.join("board-sent.new")).unwrap();
        act_seen(&dir3, "a", "approve", "", at(0)).unwrap();
        assert_eq!(
            std::fs::read(dir3.join(FILE)).unwrap(),
            board_before,
            "the Board's file was not written through it"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for file in [OUTBOX, SENT] {
                let mode = std::fs::metadata(dir3.join(file)).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{file}: this user's alone");
            }
        }
        for d in [dir, dir2, dir3] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}
