//! What an agent's conversation has used, read from the record Claude Code
//! keeps of it.
//!
//! Claude Code writes each conversation to a file of its own, one JSON
//! object per line:
//!
//!   <its folder>/projects/<folder name>/<conversation id>.jsonl
//!
//! Each reply from the model is a line with `"type": "assistant"` and, under
//! `message`, the model's name and a `usage` object of token counts.
//! Lillignode reads those counts and nothing else, and changes nothing.
//!
//! Claude Code's documentation says this format is internal and changes
//! between versions. The field names here come from public descriptions of
//! it and were NOT checked against a real record. So the
//! reading is exact or not at all: a reply Lillignode does not understand in
//! full makes the cost Unknown, never a partial sum. Missing is never zero.
//!
//! Nothing read here is written to disk, and nothing but the counts and the
//! model name is kept.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// The Claude Code version whose records this reading was written for.
pub const WRITTEN_FOR: &str = "2.1.325";

/// The date of the price list below.
pub const PRICES_DATED: &str = "2026-11-15";

/// How often a record is looked at.
pub const LOOK_EVERY: Duration = Duration::from_secs(5);

/// The most read from a record in one look, so a long conversation cannot
/// hold up typing. The rest is read a little later, and so on until it is
/// all in. Reading happens between key presses, on the same thread.
const MOST_PER_LOOK: u64 = 1024 * 1024;

/// The pause between looks while a long record is being caught up with.
const CATCH_UP_EVERY: Duration = Duration::from_millis(50);

/// An agent whose conversation has had no record for a minute is looked for
/// less often: each search lists every one of Claude Code's project folders.
const SEARCH_EVERY_WHEN_QUIET: Duration = Duration::from_secs(15);
const QUICK_SEARCHES: u32 = 12;

/// How much of a record's beginning is remembered, to tell when it has been
/// replaced by another file of the same name.
const START_KEPT: usize = 1024;

/// A line longer than this is not one of Claude Code's.
const LONGEST_LINE: usize = 64 * 1024 * 1024;

/// Token counts, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    /// The count shown at a glance: everything but tokens read from cache.
    /// Those are most of a long conversation's count and the cheapest part
    /// of it, so a figure that includes them says little about the work
    /// done. They are kept, and shown in the breakdown.
    pub fn headline(&self) -> u64 {
        self.input.saturating_add(self.output).saturating_add(self.cache_write)
    }

    /// What was used after `earlier`, kind by kind. Never below zero.
    fn less(&self, earlier: &Tokens) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(earlier.input),
            output: self.output.saturating_sub(earlier.output),
            cache_write: self.cache_write.saturating_sub(earlier.cache_write),
            cache_read: self.cache_read.saturating_sub(earlier.cache_read),
        }
    }

    fn add(&mut self, other: &Tokens) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
    }
}

/// What a conversation has used, by model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub by_model: BTreeMap<String, Tokens>,
}

impl Usage {
    pub fn total(&self) -> Tokens {
        let mut all = Tokens::default();
        for tokens in self.by_model.values() {
            all.add(tokens);
        }
        all
    }

    /// Add another conversation's use to this one, model by model.
    pub fn add(&mut self, other: &Usage) {
        for (model, tokens) in &other.by_model {
            self.by_model.entry(model.clone()).or_default().add(tokens);
        }
    }

    /// What was used after `earlier`: this, less what was already there.
    /// Models nothing was used of since are left out.
    pub fn since(&self, earlier: &Usage) -> Usage {
        let mut used = Usage::default();
        for (model, tokens) in &self.by_model {
            let more = match earlier.by_model.get(model) {
                Some(before) => tokens.less(before),
                None => *tokens,
            };
            if more != Tokens::default() {
                used.by_model.insert(model.clone(), more);
            }
        }
        used
    }

    /// An estimate in dollars at the list prices built into this Lillignode.
    /// Err names a model there is no price for: then there is no figure at
    /// all, never a figure that leaves that model out.
    pub fn dollars(&self) -> Result<f64, String> {
        let mut sum = 0.0;
        for (model, t) in &self.by_model {
            let p = price(model).ok_or_else(|| model.clone())?;
            sum += (t.input as f64 * p.input
                + t.output as f64 * p.output
                + t.cache_write as f64 * p.cache_write
                + t.cache_read as f64 * p.cache_read)
                / 1_000_000.0;
        }
        Ok(sum)
    }
}

/// Dollars per million tokens.
struct Price {
    input: f64,
    cache_write: f64,
    cache_read: f64,
    output: f64,
}

/// List prices as published on 2026-11-15, cache writes at the 5-minute
/// rate. Models whose price depends on the size of the request, or carries
/// a condition, are left out: no price is better than a wrong one.
const PRICES: &[(&str, Price)] = &[
    ("claude-opus-5-5", Price { input: 4.0, cache_write: 5.0, cache_read: 0.20, output: 20.0 }),
    ("claude-opus-5", Price { input: 5.0, cache_write: 6.25, cache_read: 0.50, output: 25.0 }),
    ("claude-opus-4-8", Price { input: 5.0, cache_write: 6.25, cache_read: 0.50, output: 25.0 }),
    ("claude-opus-4-7", Price { input: 5.0, cache_write: 6.25, cache_read: 0.50, output: 25.0 }),
    ("claude-opus-4-6", Price { input: 5.0, cache_write: 6.25, cache_read: 0.50, output: 25.0 }),
    ("claude-opus-4-5", Price { input: 5.0, cache_write: 6.25, cache_read: 0.50, output: 25.0 }),
    ("claude-sonnet-5-5", Price { input: 2.0, cache_write: 2.5, cache_read: 0.10, output: 10.0 }),
    ("claude-sonnet-4-6", Price { input: 3.0, cache_write: 3.75, cache_read: 0.30, output: 15.0 }),
    ("claude-sonnet-4-5", Price { input: 3.0, cache_write: 3.75, cache_read: 0.30, output: 15.0 }),
    ("claude-haiku-4-5", Price { input: 1.0, cache_write: 1.25, cache_read: 0.10, output: 5.0 }),
    ("claude-fable-5-2", Price { input: 8.0, cache_write: 10.0, cache_read: 0.20, output: 40.0 }),
    ("claude-mythos-5-2", Price { input: 8.0, cache_write: 10.0, cache_read: 0.20, output: 40.0 }),
    ("claude-fable-5-1", Price { input: 10.0, cache_write: 12.5, cache_read: 0.25, output: 50.0 }),
    ("claude-mythos-5-1", Price { input: 10.0, cache_write: 12.5, cache_read: 0.25, output: 50.0 }),
    ("claude-fable-5", Price { input: 10.0, cache_write: 12.5, cache_read: 1.0, output: 50.0 }),
    ("claude-mythos-5", Price { input: 10.0, cache_write: 12.5, cache_read: 1.0, output: 50.0 }),
];

/// The price of exactly this model: its name as listed, or that name with a
/// date after it. A name that merely begins the same way is another model.
fn price(model: &str) -> Option<&'static Price> {
    PRICES.iter().find_map(|(name, price)| {
        let rest = model.strip_prefix(name)?;
        let dated =
            rest.strip_prefix('-').is_some_and(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()));
        (rest.is_empty() || dated).then_some(price)
    })
}

/// Why there is nothing to show yet. Not a fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// Lillignode has not looked yet.
    NotLooked,
    /// Claude Code has made no record of this conversation. It makes one
    /// once something has been said.
    NoRecord { looked_in: PathBuf },
    /// There is a record, with no reply from the model in it yet.
    NoReplies,
}

/// Why the cost cannot be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WhyNot {
    /// Cost is read only for Claude Code.
    NotClaudeCode,
    /// The agent has no conversation id to find a record by.
    NoConversation,
    /// Lillignode does not know where Claude Code keeps its records.
    NoFolder,
    /// The record is there and could not be read. The system's reason.
    CannotRead(String),
    /// A reply in the record was not understood in full.
    NotUnderstood { line: u64, why: String },
}

/// What Lillignode knows about a conversation's cost. The three are never
/// merged, and none of them is ever shown as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cost {
    Pending(Waiting),
    Known(Usage),
    Unknown(WhyNot),
}

/// The count that is compared with the user's warning threshold, when a
/// cost is known and has reached it. The threshold is on the same count the
/// sidebar shows, so the mark on a row can be checked against its number.
/// Pending and unknown are never over: nothing is known to compare.
pub fn over(cost: &Cost, warn_at: Option<u64>) -> Option<u64> {
    let (Cost::Known(usage), Some(limit)) = (cost, warn_at) else { return None };
    let count = usage.total().headline();
    (count >= limit).then_some(count)
}

/// Every open agent's cost, added up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// The use of every agent whose cost is known, whole conversations.
    pub usage: Usage,
    /// The part of `usage` that came after Lillignode was started. None
    /// when that cannot be told for one of the agents counted.
    pub since_started: Option<Usage>,
    pub known: usize,
    pub pending: usize,
    pub unknown: usize,
}

impl Summary {
    pub fn new() -> Self {
        Summary { since_started: Some(Usage::default()), ..Summary::default() }
    }

    /// Count one agent. `since` is what it used after Lillignode started.
    pub fn count(&mut self, cost: &Cost, since: Option<Usage>) {
        match cost {
            Cost::Known(usage) => {
                self.known += 1;
                self.usage.add(usage);
                self.since_started = match (self.since_started.take(), since) {
                    (Some(mut sum), Some(more)) => {
                        sum.add(&more);
                        Some(sum)
                    }
                    _ => None,
                };
            }
            Cost::Pending(_) => self.pending += 1,
            Cost::Unknown(_) => self.unknown += 1,
        }
    }
}

/// One reply's counts, read exactly. Ok(None) for a line that is not a
/// reply from a model. Err says what was wrong, and never quotes the line.
fn read_line(line: &[u8]) -> Result<Option<(String, String, Tokens)>, String> {
    let value: Value = serde_json::from_slice(line).map_err(|_| "it is not JSON".to_string())?;
    let entry = value.as_object().ok_or("it is not a JSON object")?;
    if entry.get("type").and_then(Value::as_str) != Some("assistant") {
        return Ok(None);
    }
    let message = entry.get("message").and_then(Value::as_object).ok_or("the reply has no message")?;
    let model = message.get("model").and_then(Value::as_str).ok_or("the reply does not name its model")?;
    // Messages Claude Code writes itself, not replies from a model.
    if model == "<synthetic>" {
        return Ok(None);
    }
    if model.is_empty() || model.len() > 100 || !model.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("the reply's model name is not one Lillignode can show".into());
    }
    let id = message.get("id").and_then(Value::as_str).ok_or("the reply has no id")?;
    let usage = message.get("usage").and_then(Value::as_object).ok_or("the reply has no token counts")?;
    let count = |name: &str| {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("the reply's \"{name}\" is not a count"))
    };
    let tokens = Tokens {
        input: count("input_tokens")?,
        output: count("output_tokens")?,
        cache_write: count("cache_creation_input_tokens")?,
        cache_read: count("cache_read_input_tokens")?,
    };
    Ok(Some((id.to_string(), model.to_string(), tokens)))
}

/// Reads one conversation's record, a little at a time, as it grows.
#[derive(Debug)]
pub struct Reader {
    conversation: String,
    file: Option<PathBuf>,
    /// How much of the file has been taken in.
    offset: u64,
    /// The first bytes of the file as first read. A record only grows; one
    /// that begins differently is another file.
    start: Vec<u8>,
    /// How many searches in a row found no record.
    searches: u32,
    /// More of the record is waiting to be read than one look takes.
    catching_up: bool,
    /// The start of a line whose end has not been written yet.
    partial: Vec<u8>,
    lines: u64,
    /// One entry per reply. Claude Code writes a reply on several lines as
    /// it arrives, each with the counts so far; the last one read stands.
    replies: HashMap<String, (String, Tokens)>,
    state: Cost,
    /// What the conversation had already used when Lillignode first read
    /// its record in full: the starting point for "since started". Empty
    /// for a conversation with nothing in it then.
    baseline: Option<Usage>,
    /// There is no honest starting point: the record was taken away or
    /// swapped for another while Lillignode was open, or could not be read
    /// when Lillignode first looked.
    replaced: bool,
    /// A line of this record that was not understood. It stays not
    /// understood for as long as the record is the same one, whatever
    /// happens on later looks: no partial sum is ever shown.
    bad: Option<WhyNot>,
    /// When the record last changed, by the file's own date.
    pub last_grew: Option<SystemTime>,
    next_look: Option<Instant>,
}

impl Reader {
    pub fn new(conversation: &str) -> Self {
        Self {
            conversation: conversation.to_string(),
            file: None,
            offset: 0,
            start: Vec::new(),
            searches: 0,
            catching_up: false,
            partial: Vec::new(),
            lines: 0,
            replies: HashMap::new(),
            state: Cost::Pending(Waiting::NotLooked),
            baseline: None,
            replaced: false,
            bad: None,
            last_grew: None,
            next_look: None,
        }
    }

    /// What the conversation has used since Lillignode was started. None
    /// while the cost is not known, and when the record was replaced while
    /// Lillignode was open: then there is no honest starting point.
    pub fn since_started(&self) -> Option<Usage> {
        match (&self.state, &self.baseline, self.replaced) {
            (Cost::Known(usage), Some(baseline), false) => Some(usage.since(baseline)),
            _ => None,
        }
    }

    pub fn cost(&self) -> &Cost {
        &self.state
    }

    /// Look at the record if it is time to. `claude_folder` is where Claude
    /// Code keeps its files. True if what is known changed.
    pub fn look(&mut self, now: Instant, claude_folder: Option<&Path>) -> bool {
        if self.next_look.is_some_and(|at| now < at) {
            return false;
        }
        let before = self.state.clone();
        self.catching_up = false;
        self.state = match claude_folder {
            None => Cost::Unknown(WhyNot::NoFolder),
            Some(folder) => self.read(folder),
        };
        let wait = if self.catching_up {
            CATCH_UP_EVERY
        } else if self.file.is_none() && self.searches > QUICK_SEARCHES {
            SEARCH_EVERY_WHEN_QUIET
        } else {
            LOOK_EVERY
        };
        self.next_look = Some(now + wait);
        if self.baseline.is_none() {
            // The first full reading is where "since started" counts from.
            // A record still being caught up with is not a full reading.
            self.baseline = match &self.state {
                Cost::Known(usage) => Some(usage.clone()),
                Cost::Pending(Waiting::NoRecord { .. } | Waiting::NoReplies) => Some(Usage::default()),
                Cost::Pending(Waiting::NotLooked) => None,
                Cost::Unknown(why) => {
                    // A record that could not be read: what is used before
                    // it can be is not counted, so a later reading is not a
                    // starting point.
                    self.replaced |= matches!(why, WhyNot::CannotRead(_));
                    None
                }
            };
        }
        self.state != before
    }

    fn read(&mut self, claude_folder: &Path) -> Cost {
        // A conversation id is made by Lillignode (a UUID). One from a saved
        // workspace is outside data: it must not be able to name another file.
        if self.conversation.is_empty()
            || !self.conversation.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Cost::Unknown(WhyNot::NoConversation);
        }
        let projects = claude_folder.join("projects");
        if self.file.as_ref().is_none_or(|f| !f.is_file()) {
            // A record that was there and is gone is not one that grew.
            self.replaced |= self.file.is_some();
            self.start_over();
            self.file = match find_record(&projects, &self.conversation) {
                Ok(found) => found,
                // Not being able to look is not the same as nothing there.
                Err(why) => return Cost::Unknown(WhyNot::CannotRead(why)),
            };
            self.searches = if self.file.is_some() { 0 } else { self.searches.saturating_add(1) };
        }
        let Some(path) = self.file.clone() else {
            return Cost::Pending(Waiting::NoRecord { looked_in: projects });
        };
        match self.take_in(&path) {
            Err(why) => return Cost::Unknown(WhyNot::CannotRead(why)),
            Ok(Some(bad)) => return Cost::Unknown(bad),
            Ok(None) => {}
        }
        if self.file_len(&path).is_some_and(|len| len > self.offset) {
            // More to read than one look takes: go on shortly, and show
            // nothing new until all of it is in. A total already shown for
            // this same record stays; it is replaced when the rest is read.
            self.catching_up = true;
            return match &self.state {
                Cost::Known(_) | Cost::Pending(_) => self.state.clone(),
                Cost::Unknown(_) => Cost::Pending(Waiting::NotLooked),
            };
        }
        if self.replies.is_empty() {
            return Cost::Pending(Waiting::NoReplies);
        }
        let mut usage = Usage::default();
        for (model, tokens) in self.replies.values() {
            usage.by_model.entry(model.clone()).or_default().add(tokens);
        }
        Cost::Known(usage)
    }

    fn start_over(&mut self) {
        self.file = None;
        self.offset = 0;
        self.start.clear();
        // Whatever was shown was about the record that is gone.
        self.state = Cost::Pending(Waiting::NotLooked);
        self.partial.clear();
        self.lines = 0;
        self.replies.clear();
        self.bad = None;
        self.last_grew = None;
    }

    fn file_len(&self, path: &Path) -> Option<u64> {
        std::fs::metadata(path).ok().map(|m| m.len())
    }

    /// Read what was added since the last look. Ok(Some) is a line that was
    /// not understood; once found, nothing after it is read.
    fn take_in(&mut self, path: &Path) -> Result<Option<WhyNot>, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if meta.len() < self.offset || !self.begins_as_before(&mut file) {
            // Shorter than what was read, or not beginning as it did: it is
            // not the record it was. Everything read from it is dropped.
            let path = self.file.take();
            self.start_over();
            self.file = path;
            self.replaced = true;
        } else if let Some(bad) = &self.bad {
            // Still the same record, with the same line in it.
            return Ok(Some(bad.clone()));
        }
        self.last_grew = meta.modified().ok();
        if meta.len() == self.offset {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(self.offset)).map_err(|e| e.to_string())?;
        let mut added = Vec::new();
        file.take(MOST_PER_LOOK).read_to_end(&mut added).map_err(|e| e.to_string())?;
        if self.offset == 0 {
            self.start = added[..added.len().min(START_KEPT)].to_vec();
        }
        self.offset += added.len() as u64;
        self.partial.extend_from_slice(&added);
        let mut rest = std::mem::take(&mut self.partial);
        let mut start = 0;
        while let Some(end) = rest[start..].iter().position(|&b| b == b'\n') {
            let line = &rest[start..start + end];
            start += end + 1;
            self.lines += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match read_line(line) {
                Ok(None) => {}
                Ok(Some((id, model, tokens))) => {
                    self.replies.insert(id, (model, tokens));
                }
                // Nothing more is read from this record after this, so what
                // is left of `rest` is not kept: the cost stays unknown until
                // the record is replaced, and then reading starts over.
                Err(why) => {
                    let bad = WhyNot::NotUnderstood { line: self.lines, why };
                    self.bad = Some(bad.clone());
                    return Ok(Some(bad));
                }
            }
        }
        rest.drain(..start);
        if rest.len() > LONGEST_LINE {
            let bad = WhyNot::NotUnderstood {
                line: self.lines + 1,
                why: "it is far longer than Claude Code writes".into(),
            };
            self.bad = Some(bad.clone());
            return Ok(Some(bad));
        }
        self.partial = rest;
        Ok(None)
    }

    /// True if the file still begins with the bytes first read from it.
    fn begins_as_before(&self, file: &mut File) -> bool {
        if self.start.is_empty() {
            return true;
        }
        let mut now = vec![0u8; self.start.len()];
        file.seek(SeekFrom::Start(0)).is_ok() && file.read_exact(&mut now).is_ok() && now == self.start
    }
}

/// The record of `conversation`, in whichever of Claude Code's project
/// folders holds it. Found by its name, so Lillignode does not depend on how
/// Claude Code names those folders.
///
/// Ok(None) when it is not there, which includes Claude Code having no
/// projects folder yet. Err when the folder is there and cannot be listed.
fn find_record(projects: &Path, conversation: &str) -> Result<Option<PathBuf>, String> {
    let name = format!("{conversation}.jsonl");
    let folders = match std::fs::read_dir(projects) {
        Ok(folders) => folders,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    Ok(folders.flatten().map(|dir| dir.path().join(&name)).find(|file| file.is_file()))
}

/// Where Claude Code keeps its files, by its own rule: `CLAUDE_CONFIG_DIR`
/// if set, otherwise `.claude` in the user's home folder. None if neither
/// can be told. Only the real program calls this; tests name a throwaway
/// folder instead.
pub fn claude_folder_from_env() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(home).filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(".claude"))
}

/// A count short enough for the sidebar: 950, 18.2k, 182k, 1.4M.
pub fn short(n: u64) -> String {
    // Cut, not rounded up: a count is never shown as more than it is.
    let tenths = |unit: u64| format!("{}.{}", n / unit, n % unit / (unit / 10));
    match n {
        0..=999 => n.to_string(),
        1_000..=99_999 => format!("{}k", tenths(1_000)),
        100_000..=999_999 => format!("{}k", n / 1_000),
        1_000_000..=99_999_999 => format!("{}M", tenths(1_000_000)),
        100_000_000..=999_999_999 => format!("{}M", n / 1_000_000),
        _ => format!("{}B", tenths(1_000_000_000)),
    }
}

/// A count in full, with commas: 1,234,567.
pub fn exact(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// An estimate in dollars: "$1.24", or "under $0.01" for less than a cent
/// that is not nothing.
pub fn dollars_text(amount: f64) -> String {
    if amount > 0.0 && amount < 0.01 {
        "under $0.01".to_string()
    } else {
        format!("${amount:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway stand-in for Claude Code's folder. Never the real one.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lillignode-test-cost-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("projects").join("C--work-api")).unwrap();
            std::fs::create_dir_all(dir.join("projects").join("C--work-web")).unwrap();
            Folder(dir)
        }
        fn record(&self) -> PathBuf {
            self.0.join("projects").join("C--work-api").join(format!("{ID}.jsonl"))
        }
        fn append(&self, text: &str) {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.record()).unwrap();
            f.write_all(text.as_bytes()).unwrap();
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const ID: &str = "11111111-2222-4333-8444-555555555555";

    fn reply(id: &str, model: &str, counts: [u64; 4]) -> String {
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"{ID}\",\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"secret words\"}}],\"usage\":{{\
             \"input_tokens\":{},\"output_tokens\":{},\"cache_creation_input_tokens\":{},\
             \"cache_read_input_tokens\":{},\"service_tier\":\"standard\"}}}}}}\n",
            counts[0], counts[1], counts[2], counts[3]
        )
    }

    /// Looks now, whatever the time of the last look.
    fn look(reader: &mut Reader, folder: &Folder) -> Cost {
        reader.next_look = None;
        reader.look(Instant::now(), Some(&folder.0));
        reader.cost().clone()
    }

    fn known(cost: &Cost) -> Tokens {
        match cost {
            Cost::Known(usage) => usage.total(),
            other => panic!("not known: {other:?}"),
        }
    }

    #[test]
    fn no_record_is_pending_never_zero() {
        let folder = Folder::new("pending");
        let mut reader = Reader::new(ID);
        assert_eq!(reader.cost(), &Cost::Pending(Waiting::NotLooked));
        assert!(matches!(look(&mut reader, &folder), Cost::Pending(Waiting::NoRecord { .. })));
        // A record with only the user's words in it: still nothing to count.
        folder.append("{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n");
        assert_eq!(look(&mut reader, &folder), Cost::Pending(Waiting::NoReplies));
    }

    #[test]
    fn adds_up_replies_by_kind_and_by_model_as_the_record_grows() {
        let folder = Folder::new("adds");
        let mut reader = Reader::new(ID);
        folder.append(&reply("msg_1", "claude-opus-5-5", [100, 50, 1000, 2000]));
        assert_eq!(
            known(&look(&mut reader, &folder)),
            Tokens { input: 100, output: 50, cache_write: 1000, cache_read: 2000 }
        );
        folder.append("{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"more\"}}\n\n");
        folder.append(&reply("msg_2", "claude-haiku-4-5", [10, 5, 0, 300]));
        let cost = look(&mut reader, &folder);
        assert_eq!(known(&cost).total(), 3150 + 315);
        let Cost::Known(usage) = cost else { panic!() };
        assert_eq!(usage.by_model.len(), 2);
        assert_eq!(usage.by_model["claude-haiku-4-5"].cache_read, 300);
        assert!(reader.last_grew.is_some());
    }

    #[test]
    fn a_reply_written_on_several_lines_is_counted_once_with_its_last_counts() {
        let folder = Folder::new("once");
        let mut reader = Reader::new(ID);
        folder.append(&reply("msg_1", "claude-opus-5-5", [100, 1, 0, 0]));
        folder.append(&reply("msg_1", "claude-opus-5-5", [100, 40, 0, 0]));
        folder.append(&reply("msg_1", "claude-opus-5-5", [100, 75, 0, 0]));
        assert_eq!(known(&look(&mut reader, &folder)).total(), 175);
    }

    #[test]
    fn a_line_still_being_written_waits_for_its_end() {
        let folder = Folder::new("partial");
        let mut reader = Reader::new(ID);
        let line = reply("msg_1", "claude-opus-5-5", [7, 3, 0, 0]);
        let (first, second) = line.split_at(60);
        folder.append(first);
        assert_eq!(look(&mut reader, &folder), Cost::Pending(Waiting::NoReplies));
        folder.append(second);
        assert_eq!(known(&look(&mut reader, &folder)).total(), 10);
    }

    #[test]
    fn a_reply_not_understood_in_full_makes_the_cost_unknown_and_says_where() {
        let good = reply("msg_1", "claude-opus-5-5", [100, 50, 0, 0]);
        let cases = [
            ("not json at all\n".to_string(), "not JSON"),
            ("[1,2]\n".to_string(), "not a JSON object"),
            ("{\"type\":\"assistant\"}\n".to_string(), "no message"),
            (good.replace("\"model\":\"claude-opus-5-5\",", ""), "does not name its model"),
            (good.replace("\"id\":\"msg_1\",", ""), "no id"),
            (good.replace("\"output_tokens\":50,", ""), "\"output_tokens\" is not a count"),
            (
                good.replace("\"output_tokens\":50", "\"output_tokens\":-5"),
                "\"output_tokens\" is not a count",
            ),
            (
                good.replace("\"output_tokens\":50", "\"output_tokens\":\"50\""),
                "\"output_tokens\" is not a count",
            ),
            (
                good.replace("\"cache_read_input_tokens\":0", "\"cache_read_input_tokens\":null"),
                "is not a count",
            ),
            (good.replace("claude-opus-5-5", "bad\\u001b[2Jname"), "model name"),
        ];
        for (i, (bad, expected)) in cases.iter().enumerate() {
            let folder = Folder::new(&format!("bad{i}"));
            let mut reader = Reader::new(ID);
            folder.append(&good);
            folder.append(bad);
            folder.append(&reply("msg_3", "claude-opus-5-5", [1, 1, 0, 0]));
            let Cost::Unknown(WhyNot::NotUnderstood { line, why }) = look(&mut reader, &folder) else {
                panic!("case {i} was understood: {:?}", reader.cost());
            };
            assert_eq!(line, 2, "case {i}");
            assert!(why.contains(expected), "case {i}: {why}");
            assert!(!why.contains("secret"), "the reason must never quote the conversation");
            // It stays unknown: a later good line does not make a partial sum.
            folder.append(&reply("msg_4", "claude-opus-5-5", [1, 1, 0, 0]));
            assert!(matches!(
                look(&mut reader, &folder),
                Cost::Unknown(WhyNot::NotUnderstood { line: 2, .. })
            ));
        }
    }

    #[test]
    fn lines_that_are_not_replies_from_a_model_are_passed_over() {
        let folder = Folder::new("others");
        let mut reader = Reader::new(ID);
        folder.append("{\"type\":\"summary\",\"summary\":\"x\"}\n");
        folder.append("{\"type\":\"something-new\",\"usage\":\"not ours to read\"}\n");
        folder.append("{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[]}}\n");
        folder.append(&reply("msg_1", "claude-opus-5-5", [1, 2, 3, 4]));
        assert_eq!(known(&look(&mut reader, &folder)).total(), 10);
    }

    #[test]
    fn a_record_that_is_replaced_or_removed_is_read_afresh() {
        let folder = Folder::new("replaced");
        let mut reader = Reader::new(ID);
        folder.append(&reply("msg_1", "claude-opus-5-5", [100, 50, 0, 0]));
        folder.append(&reply("msg_2", "claude-opus-5-5", [100, 50, 0, 0]));
        assert_eq!(known(&look(&mut reader, &folder)).total(), 300);
        std::fs::write(folder.record(), reply("msg_9", "claude-opus-5-5", [1, 1, 0, 0])).unwrap();
        assert_eq!(known(&look(&mut reader, &folder)).total(), 2);
        // Replaced by a longer one: told by its beginning, not its length.
        let longer: String =
            (0..5).map(|i| reply(&format!("new_{i}"), "claude-opus-5-5", [10, 0, 0, 0])).collect();
        std::fs::write(folder.record(), longer).unwrap();
        assert_eq!(known(&look(&mut reader, &folder)).total(), 50);
        // A line not understood stays so while it is the same record, and
        // not once the record is another one.
        folder.append("nonsense\n");
        assert!(matches!(look(&mut reader, &folder), Cost::Unknown(WhyNot::NotUnderstood { .. })));
        folder.append(&reply("new_9", "claude-opus-5-5", [10, 0, 0, 0]));
        assert!(matches!(look(&mut reader, &folder), Cost::Unknown(WhyNot::NotUnderstood { .. })));
        let other: String =
            (0..9).map(|i| reply(&format!("other_{i}"), "claude-opus-5-5", [1, 0, 0, 0])).collect();
        std::fs::write(folder.record(), other).unwrap();
        assert_eq!(known(&look(&mut reader, &folder)).total(), 9);
        std::fs::remove_file(folder.record()).unwrap();
        assert!(matches!(look(&mut reader, &folder), Cost::Pending(Waiting::NoRecord { .. })));
    }

    #[test]
    fn a_long_record_is_read_over_several_looks_and_shows_no_part_total() {
        let folder = Folder::new("long");
        let mut reader = Reader::new(ID);
        let replies = 9_000u64;
        let all: String =
            (0..replies).map(|i| reply(&format!("msg_{i}"), "claude-opus-5-5", [1, 1, 0, 0])).collect();
        assert!(all.len() as u64 > 2 * MOST_PER_LOOK, "the test needs more than two looks' worth");
        folder.append(&all);
        let t0 = Instant::now();
        assert!(!reader.look(t0, Some(&folder.0)), "still pending after one look: no part total");
        assert_eq!(reader.cost(), &Cost::Pending(Waiting::NotLooked));
        assert!(!reader.look(t0 + Duration::from_millis(1), Some(&folder.0)), "not on every key press");
        let mut looks = 1;
        while !matches!(reader.cost(), Cost::Known(_)) {
            looks += 1;
            assert!(looks < 50, "never finished");
            reader.look(t0 + CATCH_UP_EVERY * looks, Some(&folder.0));
        }
        assert!(looks >= 3, "{looks}");
        assert_eq!(known(reader.cost()).total(), replies * 2);
    }

    #[test]
    fn an_agent_with_no_record_for_a_minute_is_looked_for_less_often() {
        let folder = Folder::new("quiet");
        let mut reader = Reader::new(ID);
        let mut now = Instant::now();
        for _ in 0..=QUICK_SEARCHES {
            assert!(reader.next_look.is_none_or(|at| at <= now));
            reader.look(now, Some(&folder.0));
            now += LOOK_EVERY;
        }
        folder.append(&reply("msg_1", "claude-opus-5-5", [1, 1, 0, 0]));
        assert!(!reader.look(now, Some(&folder.0)), "not yet");
        assert!(reader.look(now + SEARCH_EVERY_WHEN_QUIET, Some(&folder.0)));
        assert!(matches!(reader.cost(), Cost::Known(_)));
    }

    #[test]
    fn looks_only_every_few_seconds_and_says_when_something_changed() {
        let folder = Folder::new("pace");
        let mut reader = Reader::new(ID);
        let t0 = Instant::now();
        assert!(reader.look(t0, Some(&folder.0)), "NotLooked became NoRecord");
        folder.append(&reply("msg_1", "claude-opus-5-5", [1, 1, 0, 0]));
        assert!(!reader.look(t0 + Duration::from_secs(1), Some(&folder.0)), "too soon to look again");
        assert!(reader.look(t0 + LOOK_EVERY, Some(&folder.0)));
        assert!(!reader.look(t0 + LOOK_EVERY * 2, Some(&folder.0)), "nothing new");
    }

    #[test]
    fn without_a_folder_or_a_usable_id_the_cost_is_unknown() {
        let folder = Folder::new("ids");
        let mut reader = Reader::new(ID);
        reader.look(Instant::now(), None);
        assert_eq!(reader.cost(), &Cost::Unknown(WhyNot::NoFolder));
        // An id from a saved workspace is outside data: it cannot be a path.
        for id in ["", "..\\..\\secrets", "../x", "a/b", "C:evil"] {
            let mut reader = Reader::new(id);
            assert_eq!(look(&mut reader, &folder), Cost::Unknown(WhyNot::NoConversation), "{id:?}");
        }
    }

    #[test]
    fn dollars_are_an_estimate_only_when_every_model_has_a_price() {
        let mut usage = Usage::default();
        usage.by_model.insert(
            "claude-opus-5-5".into(),
            Tokens { input: 1_000_000, output: 1_000_000, cache_write: 1_000_000, cache_read: 1_000_000 },
        );
        assert!((usage.dollars().unwrap() - 29.20).abs() < 1e-9);
        usage
            .by_model
            .insert("claude-haiku-4-5-20251001".into(), Tokens { input: 1_000_000, ..Tokens::default() });
        assert!((usage.dollars().unwrap() - 30.20).abs() < 1e-9, "a dated name is the same model");
        // A model with no price: no figure at all, not a figure without it.
        usage.by_model.insert("claude-opus-5-7".into(), Tokens { input: 5, ..Tokens::default() });
        assert_eq!(usage.dollars(), Err("claude-opus-5-7".to_string()));
        for unpriced in
            ["claude-opus-5-5-fast", "claude-sonnet-5", "claude-haiku-5-5", "gpt-9", "claude-opus-5-1234567"]
        {
            assert!(price(unpriced).is_none(), "{unpriced} was given a price");
        }
        assert!(price("claude-opus-5").is_some() && price("claude-fable-5-2").is_some());
    }

    #[test]
    fn counts_are_cut_short_for_the_sidebar_and_exact_in_full() {
        let cases = [
            (0, "0"),
            (950, "950"),
            (1_000, "1.0k"),
            (18_260, "18.2k"),
            (99_999, "99.9k"),
            (182_900, "182k"),
            (1_450_000, "1.4M"),
            (182_000_000, "182M"),
            (2_500_000_000, "2.5B"),
        ];
        for (n, text) in cases {
            assert_eq!(short(n), text);
        }
        assert_eq!(exact(0), "0");
        assert_eq!(exact(999), "999");
        assert_eq!(exact(1_000), "1,000");
        assert_eq!(exact(1_234_567), "1,234,567");
        assert_eq!(dollars_text(1.236), "$1.24");
        assert_eq!(dollars_text(0.004), "under $0.01");
        assert_eq!(dollars_text(0.0), "$0.00");
    }
}
