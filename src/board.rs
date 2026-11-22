//! The Board, read from a file the user keeps beside the workspace.
//!
//! The page's third column shows cards: things to decide, things only the
//! user can do, and things worth knowing. For now they come from one local
//! file, `board.json`, in the folder Lillignode saves in. Nothing is
//! fetched from anywhere else.
//!
//!   { "cards": [ { "id": "c1", "owner": "Codex (web)", "kind": "decide",
//!                  "tier": "now", "title": "…", "recommendation": "…",
//!                  "status": "open" } ] }
//!
//! A card has an `id`, an `owner`, a `kind` (decide, fire, do, question or
//! know), a `tier` (now or next), a `title` and a `status`. It may also have
//! a `recommendation`, `risk`, `cost`, `blocks`, `why`, `options` and
//! `silo`. Only open cards are shown. The owner `"me"` is the person reading
//! the page.
//!
//! This file is a reader: it names nothing of the engine, and is given
//! only a folder to read in (`tests/boundaries.rs`).

use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

/// The file's name, in the folder Lillignode saves in.
pub const FILE: &str = "board.json";

/// The owner that is the person reading the page.
pub const YOU: &str = "me";

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

/// One card, as read. Only what the page shows is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub owner: String,
    pub kind: Kind,
    pub tier: Tier,
    pub title: String,
    pub recommendation: Option<String>,
}

/// The Board as the page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Board {
    /// Decide, fire and question cards: now before next, then in the
    /// file's order.
    pub decide: Vec<Card>,
    /// Do cards owned by the person reading: now before next.
    pub yours: Vec<Card>,
    /// Know cards, newest first: the last in the file is the newest.
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
}

/// Read the Board in `folder`. No folder or no file: an empty Board.
pub fn read(folder: Option<&Path>) -> Board {
    let Some(folder) = folder else { return Board::default() };
    let path = folder.join(FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Board::default(),
        Err(e) => return unreadable(&format!("it could not be opened ({e})")),
    };
    let mut text = String::new();
    match file.take(MAX_BYTES + 1).read_to_string(&mut text) {
        Ok(n) if n as u64 > MAX_BYTES => unreadable("it is over 1 MB"),
        Ok(_) => from_text(&text),
        Err(e) => unreadable(&format!("it could not be read ({e})")),
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
    let mut cards = Vec::new();
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
        if shape.status != "open" {
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
        cards.push(Card {
            id: shape.id,
            owner: shape.owner,
            kind,
            tier,
            title: shape.title,
            recommendation: shape.recommendation.filter(|r| !r.trim().is_empty()),
        });
    }
    for card in cards {
        match card.kind {
            Kind::Decide | Kind::Fire | Kind::Question => board.decide.push(card),
            Kind::Do if card.owner == YOU => board.yours.push(card),
            // Someone else's to do: not on this person's Board.
            Kind::Do => {}
            Kind::Know => board.know.push(card),
        }
    }
    // Stable: within a tier, the file's order.
    board.decide.sort_by_key(|card| card.tier);
    board.yours.sort_by_key(|card| card.tier);
    board.know.reverse();
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

    /// The Board as the page reads it. Only what is shown leaves.
    pub fn to_json(&self) -> Value {
        let raised_by = |owner: &str| if owner == YOU { "you".to_string() } else { owner.to_string() };
        let tier = |tier: Tier| if tier == Tier::Now { "now" } else { "next" };
        let decide: Vec<Value> = self
            .decide
            .iter()
            .map(|c| {
                json!({
                    "id": c.id, "kind": c.kind.word(), "tier": tier(c.tier), "title": c.title,
                    "recommendation": c.recommendation, "raised_by": raised_by(&c.owner),
                })
            })
            .collect();
        let yours: Vec<Value> = self
            .yours
            .iter()
            .map(|c| json!({ "id": c.id, "tier": tier(c.tier), "title": c.title }))
            .collect();
        let know: Vec<Value> = self
            .know
            .iter()
            .map(|c| json!({ "id": c.id, "title": c.title, "raised_by": raised_by(&c.owner) }))
            .collect();
        let not_read = match self.not_read_count {
            0 => None,
            n => {
                let more = n.saturating_sub(self.not_read.len());
                let named = self.not_read.join("; ");
                let cards = if n == 1 { "1 card" } else { &format!("{n} cards") };
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
            "decide": decide,
            "yours": yours,
            "know": know,
            "empty": self.is_empty(),
            "not_read": not_read,
            "problem": self.problem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, owner: &str, kind: &str, tier: &str, status: &str) -> Value {
        json!({ "id": id, "owner": owner, "kind": kind, "tier": tier, "title": format!("title {id}"),
                "recommendation": format!("do {id}"), "status": status })
    }

    fn board(cards: Vec<Value>) -> Board {
        from_text(&json!({ "cards": cards }).to_string())
    }

    fn ids(cards: &[Card]) -> Vec<&str> {
        cards.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn each_kind_goes_to_its_place_and_only_open_cards_are_shown() {
        let b = board(vec![
            card("d-next", "Codex (web)", "decide", "next", "open"),
            card("d-now", "Codex (web)", "decide", "now", "open"),
            card("f", "Claude Code (api)", "fire", "now", "open"),
            card("q", YOU, "question", "next", "open"),
            card("closed", "Codex (web)", "decide", "now", "done"),
            card("mine", YOU, "do", "next", "open"),
            card("mine-now", YOU, "do", "now", "open"),
            card("theirs", "Codex (web)", "do", "now", "open"),
            card("old", "Codex (web)", "know", "now", "open"),
            card("new", "Codex (web)", "know", "next", "open"),
            card("gone", "Codex (web)", "know", "now", "dismissed"),
        ]);
        assert_eq!(ids(&b.decide), ["d-now", "f", "d-next", "q"], "now first, then the file's order");
        assert_eq!(ids(&b.yours), ["mine-now", "mine"], "only the reader's, now first");
        assert_eq!(ids(&b.know), ["new", "old"], "newest first: the last in the file");
        assert_eq!((b.not_read_count, b.problem.as_deref()), (0, None));
        assert!(!b.is_empty());
        let shown = b.to_json();
        assert_eq!(shown["decide"][3]["raised_by"], "you", "the reader is \"you\"");
        assert_eq!(shown["decide"][0]["raised_by"], "Codex (web)");
        assert_eq!(shown["decide"][0]["recommendation"], "do d-now");
        assert!(!shown.to_string().contains("closed") && !shown.to_string().contains("theirs"));
    }

    #[test]
    fn only_what_is_shown_leaves() {
        let mut full = card("c", "Codex (web)", "decide", "now", "open");
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
        let shown = board(vec![full]).to_json().to_string();
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
        let shown = board(vec![]).to_json();
        assert_eq!((shown["empty"].as_bool(), shown["problem"].is_null()), (Some(true), true));
    }

    #[test]
    fn a_file_that_cannot_be_read_says_so_and_why() {
        for (text, why) in [
            ("{", "it is not a list of cards"),
            ("[]", "it is not a list of cards"),
            (r#"{"cards": {}}"#, "it is not a list of cards"),
        ] {
            let b = from_text(text);
            let said = b.problem.clone().unwrap_or_default();
            assert!(
                said.starts_with(&format!("The Board's file, board.json, was not read: {why}")),
                "{said}"
            );
            assert!(said.ends_with("Nothing else is affected."), "{said}");
            assert!(b.is_empty());
        }
        let folder = std::env::temp_dir().join(format!("lillignode-board-big-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(FILE), " ".repeat(MAX_BYTES as usize + 10)).unwrap();
        let said = read(Some(&folder)).problem.unwrap();
        assert!(said.contains("it is over 1 MB"), "{said}");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_card_that_cannot_be_read_is_left_out_and_named_and_the_rest_are_shown() {
        let mut no_title = card("t", "x", "decide", "now", "open");
        no_title.as_object_mut().unwrap().remove("title");
        let mut blank = card("b", "x", "decide", "now", "open");
        blank["title"] = json!("   ");
        let b = board(vec![
            card("good", "x", "decide", "now", "open"),
            no_title,
            card("k", "x", "later", "now", "open"),
            card("n", "x", "decide", "someday", "open"),
            blank,
            json!("not a card"),
        ]);
        assert_eq!(ids(&b.decide), ["good"]);
        assert_eq!(b.not_read_count, 5);
        let said = b.to_json()["not_read"].as_str().unwrap().to_string();
        assert_eq!(
            said,
            "5 cards in board.json could not be read and are not shown: card t: it has no title; card k: no \
             kind called \"later\"; card n: no tier called \"someday\"; and 2 more."
        );
        let one = board(vec![card("k", "x", "later", "now", "open")]).to_json();
        assert_eq!(
            one["not_read"],
            "1 card in board.json could not be read and is not shown: card k: no kind called \"later\"."
        );
        // A closed card is not checked: it is not shown either way.
        assert_eq!(board(vec![card("k", "x", "later", "now", "done")]).not_read_count, 0);
    }

    #[test]
    fn a_very_long_file_is_read_only_so_far() {
        let cards: Vec<Value> =
            (0..MAX_CARDS + 7).map(|i| card(&i.to_string(), "x", "know", "now", "open")).collect();
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
        assert_eq!(b.decide.len() + b.yours.len() + b.know.len(), 8, "every card is shown somewhere");
        assert!(!b.decide.is_empty() && !b.yours.is_empty() && !b.know.is_empty());
        for kind in ["decide", "fire", "question", "do", "know"] {
            assert!(text.contains(&format!("\"kind\": \"{kind}\"")), "{kind}");
        }
    }
}
