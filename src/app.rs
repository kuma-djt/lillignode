//! What the user can do, and every sentence Lillignode says to them.
//!
//! Keys go to the focused agent while it is running. A menu key (by default
//! Ctrl+Q or F12) opens Lillignode's own menu. When there is no running agent
//! in front, the menu keys work directly, since nothing else could be meant.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::action::Action;
use crate::agent::{AgentBackend, AgentEvent, Size, StartError};
use crate::board;
use crate::config::{Billing, Config};
use crate::cost::{self, Cost, Usage, Waiting, WhyNot};
use crate::engine::{
    About, Answered, Engine, Event, Outcome, PageTyped, PasteOutcome, QuestionId, Refused, Room, RoomChange,
    Sent, SentAs, Unfinished, ViewOutcome, Viewed,
};
use crate::fit;
use crate::input::Pasted;
use crate::keys;
use crate::kind::AgentKind;
use crate::look::Depth;
use crate::page::{self, TypedOutcome};
use crate::state::{self, LoadOutcome};
use crate::status::{self, AgentStatus, Reading};
use crate::terminal::HISTORY_ROWS;
use crate::ui;
use crate::workspace::{split_options, Entry, RestoreReport, Status, Workspace};

pub const DEFAULT_COMMAND: &str = "claude";
const AGENT_PROMPT: &str = "Agent to start: ";
const FOLDER_PROMPT: &str = "Folder to work in: ";
/// The last line of the cost total at the foot of the sidebar.
const EXPERIMENTAL: &str = "  cost is experimental";
/// Said in the status report whenever a cost is shown. The count is left
/// as it is: what Claude Code stores at the start of a conversation is
/// real use, so it is explained, not hidden.
const FIRST_REQUEST: &str = "Experimental: this readout has not been checked against /usage. A \
    conversation's first request counts tens of thousands of tokens (about 59,000 in one check), most of \
    them Claude Code writing its own instructions to its cache. That is real use, and it is counted. It \
    is also why a cost warning set below that is passed by every agent at once.\n";

/// Marks in the bar's text, taken out before it is shown: where a key's
/// name starts, where the name of the mode starts, and where either ends.
/// They let the bar be drawn with keys emphasised and labels dim while the
/// words are written once, in one place.
const KEY: char = '\u{1}';
const MODE: char = '\u{3}';
const END: char = '\u{2}';

/// What a piece of the bar is, which decides how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarPart {
    /// The mode the bar is in: MENU, HISTORY, STATUS REPORT.
    Mode,
    /// A key the user can press.
    Key,
    /// What a key does, and everything else.
    Label,
}

fn key(name: &str) -> String {
    format!("{KEY}{name}{END}")
}

fn mode(name: &str) -> String {
    format!("{MODE}{name}{END}")
}

/// The menu, in the narrowest window Lillignode draws in. The two ways out
/// come first, so no width can cut them off, and the whole line fits 80
/// columns. Shift+Tab (previous agent) works too and is in the README.
fn menu_line() -> String {
    let items = [
        ("Esc", "back"),
        ("q", "quit"),
        ("Tab", "next"),
        ("n", "new"),
        ("x", "close"),
        ("r", "start"),
        ("s", "status"),
        ("PgUp", "history"),
    ];
    let items: Vec<String> = items.iter().map(|(k, does)| format!("{} {does}", key(k))).collect();
    format!("{} {}", mode("MENU"), items.join("  "))
}

pub enum Mode {
    /// Keys go to the focused agent if it is running.
    Normal,
    /// A menu key was pressed; the next key is a Lillignode command.
    Menu,
    /// Reading back what scrolled off the focused agent's screen.
    History,
    /// Asking for the agent to start, then the folder.
    NewAgent(NewAgent),
    Confirm(Confirm),
    /// Showing the last status report received from the focused agent.
    StatusReport,
}

/// The new-agent form: the program, its options, the folder, and, when
/// options were typed or the kind is not the one the name suggests, a last
/// line that shows all of it before anything starts.
#[derive(Default)]
pub struct NewAgent {
    pub step: Step,
    /// What is being typed at this step.
    pub input: String,
    /// What was given at each step, kept so `n` at the last line goes back
    /// with nothing lost.
    command: String,
    options: String,
    folder: String,
    /// The kind chosen with Tab or `k`. None: the one the name suggests.
    kind: Option<AgentKind>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Step {
    #[default]
    Program,
    Options,
    Folder,
    /// Everything given, shown back before it starts.
    Check,
}

impl NewAgent {
    fn kind(&self) -> AgentKind {
        self.kind.unwrap_or_else(|| AgentKind::of_command(&self.command))
    }

    fn next_kind(&mut self) {
        self.kind = Some(self.kind().next());
    }

    /// Whether the last line is needed: options were typed, or the kind is
    /// not the one the name suggests.
    fn needs_check(&self) -> bool {
        !self.options.trim().is_empty() || self.kind() != AgentKind::of_command(&self.command)
    }

    fn options_prompt(&self) -> String {
        format!("Options for {} as {}: ", self.command, kind_words(self.kind()))
    }
}

/// A kind in the words the form uses.
fn kind_words(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "Claude Code",
        AgentKind::Codex => "Codex",
        AgentKind::Program => "a plain program",
    }
}

/// An unfinished agent as the quit question names it: with what it last
/// said, if anything.
fn named(u: &Unfinished) -> String {
    match &u.status {
        Some(s) => format!("{} ({})", u.label, s.text()),
        None => u.label.clone(),
    }
}

/// The line for options that cannot be started, longest first. What is said
/// after the reason's first sentence goes first (another way out, which
/// `k` offers too), then what the keys do is cut to one word each, then the
/// opening words go. The keys are left bare only when even one word each
/// does not fit. What to take out and why are kept to the last.
fn refusal_tries(why: &str) -> [String; 5] {
    let first = why.split_once(". ").map_or(why, |(first, _)| first);
    [
        format!("Cannot start it as typed. n: change it, k: another kind, Esc: cancel   {why}."),
        format!("Cannot start it as typed. n: change it, k: another kind, Esc: cancel   {first}."),
        format!("Cannot start it as typed. n change, k kind, Esc cancel   {first}."),
        format!("n change, k kind, Esc cancel   {first}."),
        format!("n / k / Esc   {first}."),
    ]
}

/// What starting an agent as this kind means, said before it starts.
fn kind_means(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "it continues its conversation next time",
        AgentKind::Codex => "a new conversation each time",
        AgentKind::Program => "status only if it reports one",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// The engine's question about closing an agent, by its id.
    Close(QuestionId),
    /// The engine's question about quitting, by its id.
    Quit(QuestionId),
    /// The engine's question about a paste of several lines into an agent
    /// that would take each line as a separate Enter, by its id. The
    /// engine holds the paste.
    Paste(QuestionId),
    /// The engine's question about changing between one agent on screen
    /// and two while an agent is waiting for the user, by its id: every
    /// agent is told a new size and redraws.
    View(QuestionId),
    /// Stop showing the waiting parts of the agent in front. Asked from
    /// its status report.
    Parts,
    /// Give the page a new address: the one it has stops working at once.
    /// The face's own question: the engine holds nothing of the page.
    NewAddress,
}

pub struct App<B: AgentBackend> {
    pub engine: Engine<B>,
    pub mode: Mode,
    pub config: Config,
    /// The latest thing the user should know. Stays until replaced, except
    /// a refusal (`refuse`), which goes at the next menu action.
    pub notice: Option<String>,
    /// The notice that said an action was refused, while it is still shown.
    /// It is about a key already pressed, so it does not stay to crowd what
    /// comes next, such as who needs you.
    refused: Option<String>,
    /// What is wrong with the settings file, led by how to fix it. Shown
    /// before any other notice and never cut before the fix; it goes at the
    /// next menu action, and is said again when Lillignode closes.
    settings_problem: Option<String>,
    /// The workspace could not be saved after a change. Shown before any
    /// other notice, and nothing that happens later replaces it: it goes at
    /// the next menu action, or when a save works again.
    save_problem: Option<String>,
    /// Problems found on starting, repeated on the way out so they remain in
    /// the terminal after Lillignode closes.
    pub start_problems: Vec<String>,
    /// Set by the caller each time it draws: the window cannot show agents.
    pub too_small: bool,
    /// How many colours to draw with. Full unless the caller says otherwise.
    pub colors: Depth,
    /// Whole seconds since Lillignode started, set by the caller before it
    /// draws. A Working agent's sign in the sidebar moves with it.
    pub clock: u64,
    /// The first agent the sidebar showed last time it was drawn, when not
    /// all of them fit. Kept so the list moves only as far as it must.
    pub sidebar_top: std::cell::Cell<usize>,
    /// The first line of the status report on screen, while it is open.
    pub report_top: usize,
    /// The window, as last told by `set_window`. Decides whether two
    /// agents fit side by side, and what size agents are given.
    window: Rect,
    /// The folder Lillignode was started in: the default for a new agent.
    launch_dir: PathBuf,
    /// The value of Claude Code's status off switch where Lillignode was
    /// started, if set. Every agent inherits it. Lillignode leaves it alone.
    status_off_switch: Option<String>,
    /// The Unknown notices now on screen, each with its words, so one that
    /// stops being true can be taken back. The engine says when each is
    /// given and when it is taken back.
    unknown_notice_up: Vec<(AgentKind, String)>,
    /// Windows only: Microsoft's newer pseudo-terminal is not beside
    /// `lillignode.exe`, so Claude Code's status cannot get through.
    pub newer_pty_missing: bool,
    /// The page's address, word and all, while it is being served.
    page_address: Option<String>,
    /// The page being served: held to give it a new address when asked.
    page: Option<page::Served>,
    /// The page was handed the way to type to agents.
    page_types: bool,
    /// The page was handed the way to act on the Board's cards.
    page_acts: bool,
    /// Where the Board's file and its outbox are: the folder Lillignode
    /// saves in, if it has one.
    board_folder: Option<PathBuf>,
    /// The Board's adapter, when the settings name one (running, or saying
    /// why not). With one, nothing goes to the outbox file.
    pub board_adapter: Option<crate::adapter::Link>,
    /// The program named, as said in the status report.
    board_adapter_named: Option<String>,
    /// Why the page's address will not be the same next start, if it will
    /// not.
    page_word_not_kept: Option<String>,
}

/// Said when the workspace could not be saved, with the reason.
fn save_failed(why: &str) -> String {
    format!(
        "Could not save your workspace ({why}). The last saved workspace is unchanged; changes since then \
         will not be restored."
    )
}

/// Said when the view cannot change because no agent can be shown at all.
const TOO_SMALL_FOR_VIEW: &str =
    "This window is too small to show agents at all, so the view was not changed. Make it bigger first.";

/// Said after a paste whose tabs became spaces.
const TABS_SAID: &str = " Its tabs were sent as spaces, so none was taken as a press of Tab.";

/// "Ctrl+C, Ctrl+D", up to three different ones, then how many more.
fn control_names(left_out: &[char]) -> String {
    let mut names: Vec<String> = Vec::new();
    for c in left_out {
        let name = crate::input::control_name(*c);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let more = names.len().saturating_sub(3);
    names.truncate(3);
    let names = names.join(", ");
    if more > 0 {
        format!("{names} and {more} more")
    } else {
        names
    }
}

/// A card's title as a notice may show it: nothing that draws no
/// character or turns the text after it around, and no more than a notice
/// would ever have room for.
fn shown_title(title: &str) -> String {
    const TURNS_TEXT: &str =
        "\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}\u{200e}\u{200f}";
    title
        .chars()
        .filter(|c| !c.is_control() && fit::char_width(*c) > 0 && !TURNS_TEXT.contains(*c))
        .take(60)
        .collect()
}

/// Said when a paste, or text from the page, was nothing but control
/// characters.
fn only_controls_sentence(left_out: &[char]) -> String {
    format!(
        "Nothing was pasted: the text was only control characters ({}). Nothing was sent.",
        control_names(left_out)
    )
}

/// The question about text that would go to an agent line by line, and
/// why it is asked. The terminal puts its keys between the two; the page
/// has buttons.
fn line_by_line_question(lines: usize, label: &str) -> (String, &'static str) {
    (
        format!("Paste {lines} lines into {label} one line at a time?"),
        "It does not take a paste as one block, so each line would be entered as if you pressed Enter \
         after it.",
    )
}

/// Said after a paste was sent, with what was taken out of it and whether
/// its tabs became spaces.
fn pasted_sentence(sent: &Sent) -> String {
    let how = if sent.how == SentAs::LineByLine { " line by line" } else { "" };
    let tabs = if sent.tabs { TABS_SAID } else { "" };
    format!("Pasted into {}{how}.{}{tabs}", sent.label, left_out_sentence(&sent.left_out))
}

/// " 2 control characters were left out (Ctrl+C, Ctrl+D).", or empty
/// when none were. Starts with a space, to follow a sentence.
fn left_out_sentence(left_out: &[char]) -> String {
    match left_out.len() {
        0 => String::new(),
        1 => format!(" 1 control character was left out ({}).", control_names(left_out)),
        n => format!(" {n} control characters were left out ({}).", control_names(left_out)),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// How long a part has said it is waiting: "waiting since 52 minutes ago".
/// A waiting part is not ended by a clock, so its age is always said.
fn waiting_since(at: SystemTime) -> String {
    format!("waiting since {}", ago(Some(at)))
}

/// One part of a program, for the status report: its name, its id, what
/// it last said and when.
fn part_line(part: &status::Part) -> String {
    let id = match part.id.as_str() {
        "" => "an id the format does not allow".to_string(),
        id => format!("id {id}"),
    };
    let said = match &part.state {
        status::PartState::Waiting(kind) => {
            let kind = kind.map(|k| format!(" ({})", k.word())).unwrap_or_default();
            let dismissed = if part.dismissed { ", no longer shown at your word" } else { "" };
            format!("waiting for you{kind}, {}{dismissed}", waiting_since(part.since))
        }
        status::PartState::Said(said) => format!("{}, {}", said.text(), ago(Some(part.at))),
        status::PartState::NotUnderstood => format!("not understood, {}", ago(Some(part.at))),
    };
    format!("  {} ({id}): {said}\n", part.name())
}

fn ago(when: Option<SystemTime>) -> String {
    let Some(secs) = when.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()) else {
        return "at an unknown time".to_string();
    };
    match secs {
        0..=59 => "less than a minute ago".to_string(),
        60..=3599 => format!("{} ago", plural((secs / 60) as usize, "minute", "minutes")),
        3600..=86_399 => format!("{} ago", plural((secs / 3600) as usize, "hour", "hours")),
        _ => format!("{} ago", plural((secs / 86_400) as usize, "day", "days")),
    }
}

/// What to say about a saved workspace that could not be read. None when it
/// was read normally or there was none.
pub fn load_problem_text(outcome: &LoadOutcome) -> Option<String> {
    // The kept file is named, not its whole path: a notice too long for its
    // rows loses its middle, and a long folder would push the reason out.
    // It sits beside the workspace file, which `lillignode --where` names.
    let kept = |path: &Path| path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
    match outcome {
        LoadOutcome::Nothing | LoadOutcome::Loaded => None,
        LoadOutcome::UsedPrevious { unreadable_kept_as, previous_saved, why } => Some(format!(
            "Your saved workspace could not be read: {why}. Loaded the previous copy, saved {}. \
             The unreadable file was kept beside it, as {}.",
            ago(*previous_saved),
            kept(unreadable_kept_as)
        )),
        LoadOutcome::Lost { unreadable_kept_as, why } => Some(format!(
            "Your saved workspace could not be read: {why}. There is no earlier copy, so the \
             workspace starts empty. The unreadable file was kept beside it, as {}.",
            kept(unreadable_kept_as)
        )),
    }
}

/// What to say about restoring. None when there was nothing to restore.
///
/// "continuing its conversation" is said only of agents that were asked to
/// continue. If one then cannot, a later message corrects it.
pub fn restore_text(report: &RestoreReport) -> Option<String> {
    let total = report.restored.len() + report.not_restored.len();
    if total == 0 {
        return None;
    }
    let came_back: Vec<String> = report
        .restored
        .iter()
        .map(|r| {
            let how = if r.continuing { "continuing its conversation" } else { "new conversation" };
            format!("{} ({how})", r.label)
        })
        .collect();
    if report.not_restored.is_empty() {
        return Some(format!("Restored {}: {}.", plural(total, "agent", "agents"), came_back.join(", ")));
    }
    let failures: Vec<String> =
        report.not_restored.iter().map(|(label, why)| format!("{label}: {why}")).collect();
    let restored = if came_back.is_empty() { String::new() } else { format!(" ({})", came_back.join(", ")) };
    // r is offered only where starting again can help.
    let cannot_help =
        report.not_restored.iter().filter(|(_, why)| crate::workspace::start_again_cannot_help(why)).count();
    let way_out = if cannot_help == 0 {
        "Switch to one, fix the cause, then r to try again or x to remove it."
    } else if cannot_help == report.not_restored.len() {
        "Switch to one, then x to remove it; r will not help."
    } else {
        "Switch to one, then x to remove it, or fix the cause and r to try again where r can help."
    };
    Some(format!(
        "Restored {} of {}{restored}. Not restored: {}. Each is still in your workspace. {way_out}",
        report.restored.len(),
        plural(total, "agent", "agents"),
        failures.join("; ")
    ))
}

impl<B: AgentBackend> App<B> {
    /// Load the saved workspace, reopen its agents, and say what happened.
    pub fn launch(
        backend: B,
        size: Size,
        state_dir: Result<PathBuf, String>,
        launch_dir: PathBuf,
        config: Config,
        config_problem: Option<String>,
    ) -> Self {
        let mut engine = Engine::new(Workspace::new(backend, size));
        let board_folder = state_dir.as_ref().ok().cloned();
        let mut start_problems: Vec<String> = config_problem.iter().cloned().collect();
        let mut said = Vec::new();
        match &state_dir {
            Ok(dir) => {
                let loaded = state::load(dir);
                if let Some(problem) = load_problem_text(&loaded.outcome) {
                    said.push(problem.clone());
                    start_problems.push(problem);
                }
                engine.save_in(dir.clone());
                // The engine saves at once: restoring may have given agents
                // new conversation ids.
                said.extend(restore_text(&engine.restore(loaded.workspace)));
            }
            Err(why) => said.push(format!(
                "Lillignode has nowhere to save your workspace ({why}). It works, but nothing will \
                 be restored next time."
            )),
        }
        let notice = (!said.is_empty()).then(|| said.join(" "));
        let mut app = Self {
            engine,
            mode: Mode::Normal,
            config,
            notice,
            refused: None,
            settings_problem: config_problem,
            save_problem: None,
            start_problems,
            too_small: false,
            colors: Depth::Full,
            clock: 0,
            report_top: 0,
            sidebar_top: std::cell::Cell::new(0),
            window: Rect::default(),
            launch_dir,
            status_off_switch: std::env::var(status::OFF_SWITCH).ok(),
            unknown_notice_up: Vec::new(),
            newer_pty_missing: newer_pty_missing(),
            page_address: None,
            page: None,
            page_types: false,
            page_acts: false,
            board_folder,
            board_adapter: None,
            board_adapter_named: None,
            page_word_not_kept: None,
        };
        app.say_events();
        app
    }

    /// Save the workspace. On failure the user is told, and what was saved
    /// before is left as it was.
    pub fn save(&mut self) -> Result<(), String> {
        self.engine.save().map_err(|why| save_failed(&why))
    }

    pub fn on_agent_event(&mut self, event: AgentEvent) {
        self.engine.handle(event);
        self.say_events();
    }

    /// Say what the engine reports has happened, oldest first. The engine
    /// saves the workspace after a change; a save that failed is one of the
    /// things said. Returns true if an agent began to need you.
    fn say_events(&mut self) -> bool {
        let still_here =
            "It is still in your workspace. Switch to it, then r to start it again or x to remove it.";
        let mut began = false;
        for event in self.engine.take_events() {
            // What happened to an agent or to the saved workspace replaces
            // the notice. What is said once in a run goes before it.
            let text = match event {
                Event::BeganNeedingYou { .. } => {
                    began = true;
                    continue;
                }
                Event::StatusBack { kind } => {
                    self.take_back_unknown_notice(kind);
                    continue;
                }
                // Never replaces a notice already up (it may be a failure
                // the user still has to act on): it goes first, the other
                // after it.
                Event::StatusUnavailable { kind, why } => {
                    if let Some(who) = kind.reporter() {
                        let text = self.unknown_notice(who, why);
                        self.unknown_notice_up.push((kind, text.clone()));
                        self.say_first(text);
                    }
                    continue;
                }
                Event::CostUnreadable { label, .. } => {
                    let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
                    self.say_first(format!(
                        "Cost shows unknown for {label}: Lillignode could not read Claude Code's record of \
                         it. Agents work as before; nothing was changed. {menu} then s says why."
                    ));
                    continue;
                }
                // Only told: no key is held, no agent stopped, no bell rung.
                Event::PassedCostWarning { label, tokens, .. } => {
                    let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
                    let limit = self.engine.ws.cost_warning().map(cost::short).unwrap_or_default();
                    self.say_first(format!(
                        "Warning only, nothing stopped: {label} is past your cost warning ({} tokens; \
                         warning at {limit} a conversation). {menu} then s shows the detail.",
                        cost::short(tokens)
                    ));
                    continue;
                }
                // Goes first and stays: what happens next (an agent that
                // could not continue, in the same instant) is said after it
                // and does not replace it.
                Event::SaveFailed { why } => {
                    self.save_problem = Some(save_failed(&why));
                    continue;
                }
                // No longer true, so no longer said.
                Event::SavedAgain => {
                    self.save_problem = None;
                    continue;
                }
                Event::Exited { label, exit, .. } if exit.clean() => format!("{label} exited. {still_here}"),
                Event::Exited { agent, label, exit } => {
                    // Started as a kind its name does not suggest, it may
                    // have been given options it does not take.
                    let chosen = self
                        .engine
                        .ws
                        .by_key(agent)
                        .filter(|e| e.kind() != AgentKind::Program && !e.kind_by_name())
                        .map(|e| {
                            format!(
                                " It was started as {0}: if it is not {0}, x removes it, then start it \
                                 again as a plain program.",
                                kind_words(e.kind())
                            )
                        })
                        .unwrap_or_default();
                    format!("{label} stopped with an error (exit code {}). {still_here}{chosen}", exit.code)
                }
                Event::StartedNewConversation { label, .. } => {
                    format!(
                        "{label} could not continue its earlier conversation (there may have been nothing \
                         in it yet), so it started a new conversation. If there was an earlier one, the \
                         agent's own resume command may still find it."
                    )
                }
                Event::CouldNotContinue { label, why, .. } => format!(
                    "{label} could not continue its earlier conversation, and starting a new one failed: \
                     {why}. {still_here}"
                ),
                // Said in the terminal each time, so nothing reaches an
                // agent from the page unseen here.
                Event::TypedFromPage { label, entered: true, .. } => {
                    format!("The page typed to {label}, then Enter.")
                }
                Event::EnterFromPage { label, .. } => format!("The page pressed Enter in {label}."),
                // Said in the terminal each time, as a send to an agent is.
                Event::CardActed { title, action, undone, .. } => {
                    let did = board::Action::parse(action).map_or(action, board::Action::name);
                    // A title is whatever the Board's file says: only what
                    // can be seen is shown, and no more than fits.
                    let title = fit::clip(&shown_title(&title), 40);
                    match (undone, self.board_adapter.is_some()) {
                        (true, false) => format!(
                            "From the page's Board: {did} was undone for \"{title}\". Its line was taken \
                             out of the outbox."
                        ),
                        (false, false) => format!(
                            "From the page's Board: {did}, for \"{title}\". It waits in the Board's outbox \
                             until it is picked up."
                        ),
                        (true, true) => format!(
                            "From the page's Board: {did} was undone for \"{title}\". Nothing went to the \
                             Board's adapter."
                        ),
                        (false, true) => format!(
                            "From the page's Board: {did}, for \"{title}\". It goes to the Board's adapter \
                             in ten seconds, unless it is undone on the page."
                        ),
                    }
                }
                Event::TypedFromPage { label, entered: false, .. } => {
                    format!("The page typed to {label}. Enter was not sent after it: the agent stopped just then.")
                }
            };
            self.notice = Some(text);
        }
        began
    }

    /// Say whether the page the user asked for was started: where it is
    /// and what it shows to whom, or why it is not there and what to do.
    /// Either way agents are not affected.
    pub fn page_started(&mut self, started: Result<String, String>) {
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        let text = match started {
            // Cut to fit, a notice keeps its last sentence: if the address
            // is lost from it, how to see it again is not. So this goes
            // after anything already said, not before it.
            Ok(address) => {
                self.page_address = Some(address.clone());
                // Each wording fits the two notice rows of the narrowest
                // window with the longest address there is.
                let who = match (self.page_types, self.page_acts) {
                    (false, false) => "Anyone with this address can see your agents.",
                    (true, false) => "Anyone with this address can see your agents and type to them.",
                    (false, true) => "Anyone with this address can see your agents and act on your Board.",
                    (true, true) => "Anyone with it can see your agents, type to them and act on your Board.",
                };
                format!("The page is on: {address}  {who} {menu} then s shows it again.")
            }
            Err(why) => format!(
                "The page was not started on port {} ({why}). Agents work as before. If another program \
                 has that port, set \"page_port\" in the settings file, or take \"page\" out.",
                self.config.page_port
            ),
        };
        self.notice = Some(match self.notice.take() {
            Some(earlier) => format!("{earlier}  {text}"),
            None => text,
        });
    }

    /// The page is being served. Says where, as `page_started` does, and
    /// keeps it, so its address can be changed when asked.
    pub fn page_serving(&mut self, served: page::Served) {
        self.page_types = served.can_type;
        self.page_acts = served.can_act;
        self.page_word_not_kept = served.word_not_kept.clone();
        let address = served.url();
        self.page = Some(served);
        self.page_started(Ok(address));
        if let Some(why) = &self.page_word_not_kept {
            self.say_first(format!(
                "The page's address could not be kept ({why}), so it lasts only until Lillignode closes."
            ));
        }
    }

    /// The settings name a program to keep the Board: said in the status
    /// report beside the page's address, so what runs is never a surprise.
    pub fn adapter_named(&mut self, command: &str, args: &[String]) {
        let shown: String = std::iter::once(command)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        self.board_adapter_named = Some(fit::clip(&shown, 80));
    }

    /// Give the page a new address. The one before stops working at once.
    fn new_page_address(&mut self) {
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        let Some(served) = self.page.as_mut() else {
            self.refuse(
                "The page is not on, so it has no address to change. Nothing was changed.".to_string(),
            );
            return;
        };
        self.page_word_not_kept = served.change_word();
        let address = served.url();
        self.page_address = Some(address.clone());
        // The address goes first and the way to see it again last: cut to
        // fit, a notice keeps its start and its last sentence.
        let not_kept = match &self.page_word_not_kept {
            Some(why) => format!(" It could not be kept ({why}), so the next start makes another."),
            None => String::new(),
        };
        self.notice = Some(format!(
            "New page address: {address}  The old one has stopped working, for open pages too.{not_kept} \
             {menu} then s shows it again."
        ));
    }

    /// Text typed on the page for the agent with this key number in this
    /// run. It goes to the engine by the command a paste uses, and what
    /// came of it is said in the terminal's own words, for the page to
    /// show. Nothing is sent to any other agent.
    pub fn typed_from_page(
        &mut self,
        run: &str,
        agent: u64,
        text: &str,
        agreed: bool,
    ) -> (TypedOutcome, String) {
        let refused = |words: String| (TypedOutcome::Refused, words);
        // Kept here as well as where the page is started: text reaches an
        // agent only if the user asked for that in the settings file.
        if !self.config.page_can_type {
            return refused("This page is read only. Nothing was sent.".to_string());
        }
        // Agents' keys begin again with each run: a key from another run
        // would name another agent.
        if run != self.engine.run().to_string() {
            return refused(
                "Lillignode was started again since this page last heard from it. Nothing was sent. \
                 Check which agent is selected, then send it again."
                    .to_string(),
            );
        }
        let Some(key) = self.engine.ws.entries().iter().map(Entry::key).find(|k| k.number() == agent) else {
            return refused(self.refusal(Refused::NoSuchAgent, ""));
        };
        let label = self.engine.ws.by_key(key).map(Entry::label).unwrap_or_default();
        // What a paste goes through: control characters out, line breaks
        // made one kind. The line breaks at its end are Send's own Enter.
        let Pasted { text, left_out } = Pasted::from(text);
        let text = text.trim_end_matches('\n').to_string();
        if text.is_empty() && left_out.is_empty() {
            return refused("There is nothing to send: the box is empty. Nothing was sent.".to_string());
        }
        let answer = match self.engine.type_from_page(key, text, left_out, agreed) {
            Ok(PageTyped::Sent { sent, entered }) => {
                let how = if sent.how == SentAs::LineByLine { " line by line" } else { "" };
                let enter = if entered {
                    ", then Enter."
                } else {
                    ". Enter was not sent after it: the agent stopped just then."
                };
                let tabs = if sent.tabs { TABS_SAID } else { "" };
                let left_out = left_out_sentence(&sent.left_out);
                (TypedOutcome::Sent, format!("Sent to {}{how}{enter}{left_out}{tabs}", sent.label))
            }
            Ok(PageTyped::OnlyControls(left_out)) => refused(only_controls_sentence(&left_out)),
            Ok(PageTyped::MustAgree { lines, label }) => {
                let (question, why) = line_by_line_question(lines, &label);
                (TypedOutcome::Ask, format!("{question} {why}"))
            }
            Err(why) => refused(self.refusal(why, &label)),
        };
        // The terminal says each send too.
        self.say_events();
        answer
    }

    /// Something done to a card of the Board on the page, or the undoing
    /// of it. Whether it may be done is decided here and in `board`; the
    /// page chose nothing. One line is added to the Board's outbox (or
    /// taken out of it), never to the Board's own file. Every one is an
    /// engine event and is said in the terminal.
    pub fn acted_from_page(
        &mut self,
        card: &str,
        action: &str,
        note: &str,
        seen: &board::Seen,
        undo: bool,
    ) -> (TypedOutcome, String) {
        // Kept here as well as where the page is started: a card is acted
        // on only if the user asked for that in the settings file.
        if !self.config.page_can_act {
            return (
                TypedOutcome::Refused,
                "This page cannot act on the Board. Nothing was sent.".to_string(),
            );
        }
        let folder = self.board_folder.as_deref();
        let now = SystemTime::now();
        let done = match &self.board_adapter {
            None if undo => board::undo(folder, card, now),
            None => board::act(folder, card, action, note, seen, now),
            // Only what is still held can be taken back: once sent on, the
            // adapter has it.
            Some(link) if undo => match link.withdraw(card) {
                Some((title, entry)) => Ok(board::Acted {
                    card: entry.card.clone(),
                    title,
                    action: board::Action::parse(&entry.action).unwrap_or(board::Action::Hold),
                    warning: None,
                    line: entry.line().to_string(),
                }),
                None => Err(
                    "There is nothing held for that card to undo: it has gone on to the Board's adapter, \
                     or was never sent. Nothing was changed."
                        .to_string(),
                ),
            },
            Some(link) => {
                let shown = link.board();
                let title = seen.title.clone();
                let hold = |entry: &board::Sent| link.hold(&title, entry);
                let held = board::Held { hold: &hold, waiting: link.held() };
                board::act_on(folder, &shown, card, action, note, seen, now, Some(held))
            }
        };
        match done {
            Ok(acted) => {
                // Undone even if keeping that it was undone failed: said
                // with what failed, and still an event and a notice.
                let words = match (undo, &acted.warning) {
                    (true, Some(but)) => format!("Undone: {}. {but}", acted.action.name()),
                    (true, None) => {
                        format!("Undone: {}. Nothing is waiting for that card.", acted.action.name())
                    }
                    (false, _) if self.board_adapter.is_some() => format!(
                        "{} goes to {} in ten seconds, unless you undo it on the card.",
                        acted.action.name(),
                        self.board_adapter
                            .as_ref()
                            .and_then(|l| l.name())
                            .unwrap_or_else(|| "the adapter".into())
                    ),
                    (false, _) => format!(
                        "Sent: {}. It waits until the Board no longer lists the card.",
                        acted.action.name()
                    ),
                };
                self.engine.card_acted(acted.card, acted.title, acted.action.word(), undo, acted.line);
                self.say_events();
                (TypedOutcome::Sent, words)
            }
            Err(why) => (TypedOutcome::Refused, why),
        }
    }

    /// Enter alone, pressed on the page in the agent with this key number
    /// in this run: to answer a prompt it is waiting at. Refused in the
    /// terminal's words, as text from the page is.
    pub fn enter_from_page(&mut self, run: &str, agent: u64) -> (TypedOutcome, String) {
        let refused = |words: String| (TypedOutcome::Refused, words);
        if !self.config.page_can_type {
            return refused("This page is read only. Nothing was sent.".to_string());
        }
        if run != self.engine.run().to_string() {
            return refused(
                "Lillignode was started again since this page last heard from it. Nothing was sent. \
                 Check which agent is selected, then press it again."
                    .to_string(),
            );
        }
        let Some(key) = self.engine.ws.entries().iter().map(Entry::key).find(|k| k.number() == agent) else {
            return refused(self.refusal(Refused::NoSuchAgent, ""));
        };
        let label = self.engine.ws.by_key(key).map(Entry::label).unwrap_or_default();
        let answer = match self.engine.enter_from_page(key) {
            Ok(label) => (TypedOutcome::Sent, format!("Pressed Enter in {label}.")),
            Err(why) => refused(self.refusal(why, &label)),
        };
        // The terminal says each press too.
        self.say_events();
        answer
    }

    /// Put before the notice already up, which stays after it.
    fn say_first(&mut self, text: String) {
        self.notice = Some(match self.notice.take() {
            Some(earlier) => format!("{text}  {earlier}"),
            None => text,
        });
    }

    fn focused_running(&self) -> bool {
        self.engine.ws.focused().is_some_and(|e| e.is_running())
    }

    fn focused_label(&self) -> String {
        self.engine.ws.focused().map(|e| e.label()).unwrap_or_default()
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let menu_key = keys::is_menu_key(&self.config, &key);
        let running = self.focused_running();
        match self.mode {
            Mode::Normal if menu_key => self.mode = Mode::Menu,
            // A window too small to show an agent takes no typing: it would
            // land in an agent the user cannot see. The menu still works, so
            // there is always a way to quit.
            Mode::Normal if self.too_small => {}
            Mode::Normal if running => {
                let shift = key.modifiers == KeyModifiers::SHIFT;
                if shift && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                    self.history_key(key);
                    return;
                }
                let app_cursor =
                    self.engine.ws.focused().is_some_and(|e| e.term.screen().application_cursor());
                if let Some(bytes) = keys::encode(key, app_cursor) {
                    self.engine.ws.send_to_focused(&bytes);
                }
            }
            // No agents yet: the welcome screen's keys work as they are.
            Mode::Normal if self.engine.ws.entries().is_empty() => self.command(key),
            // The agent in front has stopped. Typing meant for it must not
            // turn into commands (x would remove it, q would quit), so
            // commands need the menu key here too, and a typed key says so.
            Mode::Normal => {
                if matches!(key.code, KeyCode::Char(_) | KeyCode::Enter) {
                    let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
                    self.notice = Some(format!(
                        "{} has stopped, so what you typed was not sent. {menu} then r starts \
                         it again, or {menu} then x removes it.",
                        self.focused_label()
                    ));
                }
            }
            // The menu key again closes the menu, as Esc does.
            Mode::Menu if menu_key => self.mode = Mode::Normal,
            Mode::Menu => {
                // Switching keeps the menu open, so reaching the fourth agent
                // is Tab Tab Tab, not three trips through the menu.
                if !matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.mode = Mode::Normal;
                }
                self.command(key);
            }
            Mode::History => self.history_key(key),
            Mode::StatusReport => self.report_key(key),
            Mode::NewAgent(_) => self.new_agent_key(key),
            Mode::Confirm(what) => {
                self.mode = Mode::Normal;
                let yes = key.code == KeyCode::Char('y');
                match what {
                    Confirm::Close(id) => {
                        let two_before = self.engine.ws.pair().is_some();
                        match self.engine.answer(id, yes) {
                            Ok(Answered::Closed { label }) => self.closed(&label, two_before),
                            Ok(Answered::AlreadyGone) => {
                                self.notice = Some(
                                    "That agent was already gone, so nothing was closed. Nothing else was changed."
                                        .to_string(),
                                )
                            }
                            Ok(Answered::Nothing) => {}
                            // An answer about closing never quits or pastes.
                            Ok(
                                Answered::Closing
                                | Answered::Pasted(_)
                                | Answered::PasteDropped
                                | Answered::Viewed(_)
                                | Answered::ViewRefused(_),
                            ) => {
                                unreachable!("an answer about closing did something else")
                            }
                            Err(why) => self.refuse(self.refusal(why, "")),
                        }
                    }
                    Confirm::Quit(id) => {
                        if let Err(why) = self.engine.answer(id, yes) {
                            self.refuse(self.refusal(why, ""));
                        }
                    }
                    Confirm::Parts => {
                        let label = self.focused_label();
                        let dismissed = match (yes, self.engine.ws.focused_mut()) {
                            (true, Some(e)) => e.track.dismiss_waiting_parts(),
                            _ => 0,
                        };
                        self.notice = Some(match dismissed {
                            0 => format!("Left as it was: the waiting parts of {label} are still shown."),
                            n => format!(
                                "Stopped showing {} of {label}. Nothing was sent to it. A part that \
                                 repeats itself stays hidden; one that says something new and waits \
                                 is shown again.",
                                plural(n, "waiting part", "waiting parts")
                            ),
                        });
                    }
                    Confirm::NewAddress if yes => self.new_page_address(),
                    Confirm::NewAddress => {
                        self.notice = Some("The page's address was left as it was.".to_string());
                    }
                    Confirm::View(id) => match self.engine.answer(id, yes) {
                        Ok(Answered::Viewed(viewed)) => self.viewed(viewed),
                        Ok(Answered::ViewRefused(why)) => self.refuse(self.view_refusal(why)),
                        Ok(_) => {
                            self.notice =
                                Some("The view was left as it was. No agent was resized.".to_string())
                        }
                        Err(why) => self.refuse(self.refusal(why, "")),
                    },
                    Confirm::Paste(id) => match self.engine.answer(id, yes) {
                        Ok(Answered::Pasted(sent)) => self.notice = Some(pasted_sentence(&sent)),
                        // No, or its agent stopped while the question was up.
                        Ok(_) => self.notice = Some("Paste discarded. Nothing was sent.".to_string()),
                        Err(why) => self.refuse(self.refusal(why, "")),
                    },
                }
            }
        }
    }

    /// Text arrived as a paste. It goes to the agent as one paste or, where
    /// that is impossible, only after the user agrees. Its line breaks are
    /// never passed on as Enter presses unasked.
    ///
    /// Control characters were already taken out (`input::Pasted`); what
    /// was taken out is said. A paste sent as typed keys has its tabs
    /// turned into spaces, so none is taken as a press of Tab.
    pub fn on_paste(&mut self, pasted: Pasted) {
        let Pasted { text, left_out } = pasted;
        if let Mode::NewAgent(form) = &mut self.mode {
            // A pasted folder path: take the first line only, and nothing
            // that draws no character. Such a character would put the
            // typing cursor in the wrong place, and three of them are the
            // bar's own marks.
            // The last line takes keys, not text.
            if form.step != Step::Check {
                let line = text.lines().next().unwrap_or("").trim();
                form.input.extend(line.chars().filter(|c| !c.is_control()));
            }
            return;
        }
        // A window too small to show an agent takes no paste (the face
        // knows what is shown); every other rule is the engine's.
        let agent = self.engine.ws.focused().map(Entry::key);
        let outcome = match (&self.mode, agent) {
            (Mode::Normal, Some(agent)) if !self.too_small => self.engine.paste(agent, text, left_out).ok(),
            _ => None,
        };
        self.notice = Some(match outcome {
            Some(PasteOutcome::OnlyControls(left_out)) => only_controls_sentence(&left_out),
            // A clean paste sent whole or on one line says nothing.
            Some(PasteOutcome::Sent(sent)) if sent.left_out.is_empty() && !sent.tabs => return,
            Some(PasteOutcome::Sent(sent)) => pasted_sentence(&sent),
            Some(PasteOutcome::Asked(question)) => {
                self.mode = Mode::Confirm(Confirm::Paste(question.id));
                return;
            }
            None => {
                "Paste ignored: it can only go to a running agent in front. Nothing was sent.".to_string()
            }
        });
    }

    /// A Lillignode command key. Unknown keys do nothing.
    fn command(&mut self, key: KeyEvent) {
        // A key held with Ctrl or Alt is not a command: Ctrl+X is not x.
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return;
        }
        // The settings problem has been seen; it is said again on the way out.
        self.settings_problem = None;
        // So has a workspace that could not be saved. If it still cannot be
        // saved, that is said at the next change, and on the way out.
        self.save_problem = None;
        if let Some(refused) = self.refused.take() {
            if self.notice.as_ref() == Some(&refused) {
                self.notice = None;
            }
        }
        match key.code {
            KeyCode::Char('n') => {
                self.mode = Mode::NewAgent(NewAgent::default());
            }
            KeyCode::Tab => self.act(Action::Next),
            KeyCode::BackTab => self.act(Action::Previous),
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::Up | KeyCode::Down => {
                self.history_key(key)
            }
            KeyCode::Char('r') => self.act(Action::StartAgain),
            KeyCode::Char('x') if self.engine.ws.focused().is_some() => self.act(Action::Close),
            KeyCode::Char('s') => {
                if self.engine.ws.focused().is_some() {
                    self.report_top = 0;
                    self.mode = Mode::StatusReport;
                } else {
                    self.refuse("There is no agent to show a status report for.".to_string());
                }
            }
            // A bookmark and every open page stop working: asked first.
            KeyCode::Char('w') if self.page.is_some() => self.mode = Mode::Confirm(Confirm::NewAddress),
            KeyCode::Char('w') => self.new_page_address(),
            KeyCode::Char('v') if self.engine.ws.pair().is_some() => self.act(Action::ShowOne),
            KeyCode::Char('v') => self.act(Action::ShowTwo),
            KeyCode::Char('q') => self.act(Action::Quit),
            _ => {}
        }
    }

    /// Do what the user asked for. The rules about when to ask first and
    /// when to refuse are here; the keys that ask are in `command`.
    fn act(&mut self, action: Action) {
        match action {
            Action::Next => self.engine.ws.focus_next(),
            Action::Previous => self.engine.ws.focus_previous(),
            Action::StartAgain => {
                let Some((key, label)) = self.engine.ws.focused().map(|e| (e.key(), e.label())) else {
                    return;
                };
                match self.engine.start_again(key) {
                    Ok(continuing) => {
                        self.notice = self.engine.ws.by_key(key).map(|e| match &e.status {
                            Status::NotStarted(why) => {
                                format!(
                                    "{} still could not be started: {why}. Nothing was changed.",
                                    e.label()
                                )
                            }
                            _ if continuing => {
                                format!("Started {} again, continuing its conversation.", e.label())
                            }
                            _ => format!("Started {} again.", e.label()),
                        });
                        self.say_events();
                    }
                    // Said, so the key is never pressed into silence.
                    Err(why) => self.refuse(self.refusal(why, &label)),
                }
            }
            // The engine always asks first; the face shows its question.
            Action::Close => {
                let Some(key) = self.engine.ws.focused().map(Entry::key) else { return };
                match self.engine.close(key) {
                    Ok(Outcome::Asked(q)) => self.mode = Mode::Confirm(Confirm::Close(q.id)),
                    Ok(Outcome::Done) => {}
                    Err(why) => self.refuse(self.refusal(why, "")),
                }
            }
            // Changing the view tells every agent a new size, and each
            // redraws. While one is waiting for an answer, that is asked
            // about first. A change that would be refused anyway is refused
            // at once, without a question.
            Action::ShowOne => {
                let outcome = self.engine.show_one();
                self.view_outcome(outcome);
            }
            // A window too small to show agents at all is the face's to know
            // and to say; every other rule is the engine's.
            Action::ShowTwo if self.too_small || ui::areas(self.window).is_none() => {
                self.refuse(TOO_SMALL_FOR_VIEW.to_string());
            }
            Action::ShowTwo => {
                let outcome = self.engine.show_two();
                self.view_outcome(outcome);
            }
            // The engine decides whether to ask; the face shows its question.
            Action::Quit => match self.engine.quit() {
                Ok(Outcome::Asked(q)) => self.mode = Mode::Confirm(Confirm::Quit(q.id)),
                Ok(Outcome::Done) => {}
                Err(why) => self.refuse(self.refusal(why, "")),
            },
        }
    }

    /// The status report scrolls with the history keys. Any other key
    /// closes it and is not passed on. A report that fits does not move.
    fn report_key(&mut self, key: KeyEvent) {
        // Named in the bar while the agent has waiting parts, and only then.
        if key.code == KeyCode::Char('d') && key.modifiers.is_empty() && self.focused_waiting_parts() > 0 {
            self.mode = Mode::Confirm(Confirm::Parts);
            return;
        }
        // A report that fits closes on any key, as the bar says.
        let view = ui::report_view(&self.status_report_text(), self.window);
        let Some((lines, page)) =
            view.filter(|(lines, page)| lines.len() > *page && self.is_scroll_key(&key))
        else {
            self.mode = Mode::Normal;
            return;
        };
        let last = lines.len().saturating_sub(page);
        let top = self.report_top.min(last);
        self.report_top = match key.code {
            KeyCode::PageUp => top.saturating_sub(page),
            KeyCode::PageDown => (top + page).min(last),
            KeyCode::Up => top.saturating_sub(1),
            KeyCode::Down => (top + 1).min(last),
            _ => 0,
        };
    }

    /// How many parts of the agent in front are waiting for the user.
    fn focused_waiting_parts(&self) -> usize {
        self.engine.ws.focused().map_or(0, |e| e.track.waiting_parts().len())
    }

    /// The words in the status report's bar for the key that stops showing
    /// waiting parts. Empty when there are none.
    fn dismiss_hint(&self) -> String {
        match self.focused_waiting_parts() {
            0 => String::new(),
            _ => format!("   {}: stop showing its waiting parts", key("d")),
        }
    }

    /// The keys that move through history and the status report.
    fn is_scroll_key(&self, key: &KeyEvent) -> bool {
        key.modifiers.is_empty()
            && matches!(
                key.code,
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Up | KeyCode::Down | KeyCode::Home
            )
    }

    /// True while the status report is longer than its panel.
    fn report_scrolls(&self) -> bool {
        ui::report_view(&self.status_report_text(), self.window)
            .is_some_and(|(lines, page)| lines.len() > page)
    }

    /// Scroll the focused agent's history. Any key that is not a scroll key
    /// returns to the present and is not passed on.
    fn history_key(&mut self, key: KeyEvent) {
        let page = self.engine.ws.size().rows.saturating_sub(1).max(1) as isize;
        let label = self.focused_label();
        let Some(entry) = self.engine.ws.focused_mut() else {
            self.mode = Mode::Normal;
            return;
        };
        let back = match key.code {
            KeyCode::PageUp => Some(page),
            KeyCode::Up => Some(1),
            KeyCode::Home => Some(isize::MAX / 2),
            _ => None,
        };
        if let Some(rows) = back {
            let before = entry.term.scrolled_back();
            if rows == isize::MAX / 2 {
                entry.term.scroll_to_oldest();
            } else {
                entry.term.scroll(rows);
            }
            if entry.term.scrolled_back() == 0 {
                // There was nothing to scroll to. Say which of the two reasons.
                self.mode = Mode::Normal;
                self.notice = Some(if entry.term.full_screen() {
                    format!(
                        "{label} is using a full-screen display, which redraws in place and leaves no \
                         history for Lillignode to keep. Scroll with the agent's own keys instead. The \
                         mouse wheel is not passed to agents yet."
                    )
                } else {
                    format!("Nothing has scrolled off {label}'s screen yet.")
                });
            } else {
                self.mode = Mode::History;
                if entry.term.scrolled_back() == before {
                    self.notice = Some(format!(
                        "That is the oldest line kept for {label}. Lillignode keeps the last {HISTORY_ROWS} lines."
                    ));
                }
            }
            return;
        }
        match key.code {
            KeyCode::PageDown => entry.term.scroll(-page),
            KeyCode::Down => entry.term.scroll(-1),
            _ => entry.term.scroll_to_live(),
        }
        if entry.term.scrolled_back() == 0 {
            self.mode = Mode::Normal;
        }
    }

    /// Say that an agent was closed, and give the screen to the one left
    /// when two were side by side.
    fn closed(&mut self, label: &str, two_before: bool) {
        let mut said = format!("Closed {label} and removed it from your workspace.");
        if two_before && self.engine.ws.pair().is_none() {
            // Fewer than two agents are left to show side by side.
            self.engine.fit_agents();
            if let Some(left) = self.engine.ws.focused() {
                said += &format!(" {} has the whole screen again.", left.label());
            }
        }
        self.notice = Some(said);
        self.say_events();
    }

    /// The agent the engine's question about closing is about. While the
    /// question is open it is always there: nothing else can remove it.
    fn close_subject(&self, id: QuestionId) -> Option<&Entry> {
        let subject = self.engine.question().filter(|q| q.id == id).and_then(|q| match q.about {
            About::Close(agent) => self.engine.ws.by_key(agent),
            About::Quit | About::Paste(_) | About::View { .. } => None,
        });
        debug_assert!(subject.is_some(), "the face shows the engine's question, never one of its own");
        subject
    }

    fn close_subject_label(&self, id: QuestionId) -> String {
        self.close_subject(id).map(Entry::label).unwrap_or_else(|| self.focused_label())
    }

    /// Why the engine did not do what was asked. `label` names the agent
    /// it was asked about, where the sentence names one. Each says nothing
    /// was changed.
    fn refusal(&self, why: Refused, label: &str) -> String {
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        match why {
            Refused::NoSuchQuestion => {
                format!("That question was no longer open, so nothing was done. To ask again: {menu}, then the key.")
            }
            Refused::QuestionOpen => {
                "Answer the question on the bottom line first. Nothing was changed.".to_string()
            }
            Refused::NoSuchAgent => {
                "That agent is no longer in your workspace. Nothing was changed.".to_string()
            }
            Refused::AlreadyRunning => {
                format!("{label} is already running, so it was not started again. Nothing was changed.")
            }
            Refused::AgentStopped => format!("{label} is not running, so nothing was sent to it."),
            Refused::TooFewAgents(_) | Refused::TwoDoNotFit => self.view_refusal(why),
        }
    }

    /// The size of the window Lillignode last acted on.
    pub fn window(&self) -> Rect {
        self.window
    }

    /// Tell Lillignode the size of its window. The engine is told the room
    /// agents have: the whole agent area, or half of it each when two fit
    /// side by side. It decides what size agents are given.
    pub fn set_window(&mut self, window: Rect) {
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        self.window = window;
        let room = Room {
            one: ui::agent_size(window, false),
            two: ui::fits_two(window).then(|| ui::agent_size(window, true)),
        };
        let Some(pair) = self.engine.ws.pair() else {
            let _ = self.engine.set_room(room);
            return;
        };
        match self.engine.set_room(room) {
            // The user asked for two and still wants them: say why only
            // one is on screen, that the other was left alone, and the way
            // to stay with one.
            RoomChange::TwoGone => {
                let other =
                    if self.engine.ws.focused_index() == Some(pair.left) { pair.right } else { pair.left };
                let other = self.engine.ws.entries().get(other).map(Entry::label).unwrap_or_default();
                self.notice = Some(format!(
                    "Too narrow for two agents now, so only {} is shown. {other} was not stopped or closed. \
                     Widen to {} columns for both, or {menu} then v to stay with one.",
                    self.focused_label(),
                    ui::TWO_MIN_COLS
                ));
            }
            // Where typing goes is said by the headings and the bottom bar,
            // which follow every change. A notice stays up after it is true.
            RoomChange::TwoBack => {
                self.notice = Some("There is room again: two agents are side by side.".to_string());
            }
            RoomChange::Same => {}
        }
    }

    /// True while two agents are side by side on screen: the user asked
    /// for two and the window has room.
    pub fn two_shown(&self) -> bool {
        self.engine.two_shown()
    }

    /// Why two agents cannot be put side by side right now, said in full.
    fn can_show_two(&self) -> Result<(), String> {
        if self.too_small || ui::areas(self.window).is_none() {
            return Err(TOO_SMALL_FOR_VIEW.to_string());
        }
        self.engine.can_show_two().map_err(|why| self.view_refusal(why))
    }

    /// Why the view was not changed, in full. Each says nothing was changed.
    fn view_refusal(&self, why: Refused) -> String {
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        match why {
            // Not fitting because no agent fits at all is said as that,
            // wherever it is found: at the key or at the answer.
            Refused::TwoDoNotFit if self.too_small || ui::areas(self.window).is_none() => {
                TOO_SMALL_FOR_VIEW.to_string()
            }
            Refused::TwoDoNotFit => format!(
                "This window is too narrow for two agents side by side: it is {} columns wide and that \
                 takes {}. Nothing was changed. Widen the window, then {menu} then v.",
                self.window.width,
                ui::TWO_MIN_COLS
            ),
            Refused::TooFewAgents(0) => format!(
                "Two agents side by side takes two agents, and none is open. Nothing was changed. {menu} \
                 then n starts one."
            ),
            Refused::TooFewAgents(_) => format!(
                "Two agents side by side takes two agents, and one is open. Nothing was changed. {menu} \
                 then n starts another."
            ),
            other => self.refusal(other, ""),
        }
    }

    /// Say that an action was refused. The notice goes at the next menu
    /// action, unless something else has replaced it by then.
    fn refuse(&mut self, why: String) {
        self.refused = Some(why.clone());
        self.notice = Some(why);
    }

    /// Show what became of a request to change the view.
    fn view_outcome(&mut self, outcome: Result<ViewOutcome, Refused>) {
        match outcome {
            Ok(ViewOutcome::Done(viewed)) => self.viewed(viewed),
            Ok(ViewOutcome::Asked(q)) => self.mode = Mode::Confirm(Confirm::View(q.id)),
            Err(why) => self.refuse(self.view_refusal(why)),
        }
    }

    /// Say what a change of view did.
    fn viewed(&mut self, viewed: Viewed) {
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        self.notice = Some(match viewed {
            Viewed::One => {
                format!("One agent on screen: {}. Every agent was told its new size.", self.focused_label())
            }
            // Two were asked for and the window has no room, so one is on
            // screen already. Nothing is resized: this only stops two
            // coming back when the window is widened.
            Viewed::OneKept => "One agent on screen. Two side by side will not come back when the window is \
                                widened. No agent was resized."
                .to_string(),
            Viewed::Two(pair) => {
                let label = |i: usize| self.engine.ws.entries().get(i).map(Entry::label).unwrap_or_default();
                format!(
                    // Not where typing goes: that changes with Tab, and a
                    // notice stays up. The headings and the bar say it.
                    "{} and {} are side by side. Every agent was told its new size. If one draws badly, \
                     {menu} then v goes back to one.",
                    label(pair.left),
                    label(pair.right)
                )
            }
        });
    }

    /// With two agents side by side: which side typing goes to.
    fn side_in_front(&self) -> &'static str {
        match self.engine.ws.pair().filter(|_| self.two_shown()) {
            Some(pair) if self.engine.ws.focused_index() == Some(pair.right) => ", on the right",
            Some(_) => ", on the left",
            None => "",
        }
    }

    fn new_agent_key(&mut self, key: KeyEvent) {
        let Mode::NewAgent(form) = &mut self.mode else { return };
        let plain = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match (form.step, key.code) {
            (_, KeyCode::Esc) => {
                self.mode = Mode::Normal;
                self.notice = Some("Cancelled. Nothing was started.".to_string());
            }
            (Step::Check, KeyCode::Char('y')) if plain => {
                if self.check_problem().is_none() {
                    self.start_from_form();
                }
            }
            (Step::Check, KeyCode::Char('n')) if plain => {
                form.step = Step::Program;
                form.input = form.command.clone();
            }
            (Step::Check, KeyCode::Char('k')) if plain => form.next_kind(),
            (Step::Check, _) => {}
            (Step::Options, KeyCode::Tab) => form.next_kind(),
            (_, KeyCode::Backspace) => {
                form.input.pop();
            }
            (_, KeyCode::Char(c)) if plain => form.input.push(c),
            (Step::Program, KeyCode::Enter) => {
                let typed = form.input.trim().trim_matches('"');
                let command = if typed.is_empty() { DEFAULT_COMMAND } else { typed }.to_string();
                if command != form.command {
                    // A kind chosen for another program does not carry over.
                    form.kind = None;
                    form.command = command;
                }
                form.step = Step::Options;
                form.input = form.options.clone();
            }
            (Step::Options, KeyCode::Enter) => {
                form.options = std::mem::take(&mut form.input);
                form.step = Step::Folder;
                form.input = form.folder.clone();
            }
            (Step::Folder, KeyCode::Enter) => {
                form.folder = form.input.trim().trim_matches('"').to_string();
                if form.needs_check() {
                    form.step = Step::Check;
                    form.input.clear();
                } else {
                    self.start_from_form();
                }
            }
            _ => {}
        }
    }

    /// Why the agent on the form's last line cannot be started as it
    /// stands, or None.
    fn check_problem(&self) -> Option<String> {
        let Mode::NewAgent(form) = &self.mode else { return None };
        self.engine.ws.start_problem(&form.command, &split_options(&form.options).args, form.kind())
    }

    fn start_from_form(&mut self) {
        let Mode::NewAgent(form) = &mut self.mode else { return };
        let folder = resolve_folder(&self.launch_dir, &form.folder);
        let args = split_options(&form.options).args;
        match self.engine.start_new(&form.command, args, form.kind(), folder.clone()) {
            Ok(()) => {
                self.mode = Mode::Normal;
                self.notice = self
                    .engine
                    .ws
                    .focused()
                    .map(|e| format!("Started {} in {}.", e.saved.name, folder.display()));
                self.say_events();
            }
            // Refused for what was typed, not the folder: to the last line,
            // whose n goes back to change it.
            Err(why @ StartError::Refused(_)) => {
                self.notice = Some(format!("Could not start {}: {why}. Nothing was started.", form.command));
                form.step = Step::Check;
                form.input.clear();
            }
            Err(why) => {
                // Back to the folder, with what was typed, so it can be corrected.
                self.notice = Some(format!(
                    "Could not start {}: {why}. Nothing was started. Correct the folder, or Esc to cancel.",
                    form.command
                ));
                form.step = Step::Folder;
                form.input = form.folder.clone();
            }
        }
    }

    /// The one line at the bottom: what can be done right now. Outside a
    /// question it always names the menu keys, so the way out is never hidden.
    pub fn bar(&self) -> String {
        self.bar_marked().chars().filter(|c| ![KEY, MODE, END].contains(c)).collect()
    }

    /// The bar in pieces, for drawing: which words are keys, which name the
    /// mode, and which say what a key does. Joined, they are `bar()`.
    pub fn bar_parts(&self) -> Vec<(BarPart, String)> {
        let mut parts: Vec<(BarPart, String)> = Vec::new();
        let mut now = BarPart::Label;
        for c in self.bar_marked().chars() {
            let next = match c {
                KEY => BarPart::Key,
                MODE => BarPart::Mode,
                END => BarPart::Label,
                _ => {
                    match parts.last_mut() {
                        Some((part, text)) if *part == now => text.push(c),
                        _ => parts.push((now, c.to_string())),
                    }
                    continue;
                }
            };
            now = next;
        }
        parts
    }

    fn bar_marked(&self) -> String {
        let menu = self.config.menu_keys.iter().map(|k| key(&k.label())).collect::<Vec<_>>().join(" or ");
        match &self.mode {
            // Outside a question the menu keys come first, so nothing put
            // before them (a long agent name or folder) can push them off.
            // The sidebar is not drawn here, so who needs you is said too.
            Mode::Normal if self.too_small => match self.needs_you_count() {
                0 => format!("{menu}: menu, then {} to quit", key("q")),
                n => format!(
                    "{menu}: menu, then {} goes to who needs you, {} quits.  {} you.",
                    key("Tab"),
                    key("q"),
                    plural(n, "agent needs", "agents need")
                ),
            },
            Mode::Normal if self.focused_running() => format!(
                "{menu}: Lillignode menu   Typing goes to {}{}.",
                self.focused_label(),
                self.side_in_front()
            ),
            Mode::Normal if self.engine.ws.entries().is_empty() => {
                format!("{menu}: menu   {} new agent   {} quit", key("n"), key("q"))
            }
            // A key that cannot help is not offered.
            Mode::Normal if self.engine.ws.focused().is_some_and(Entry::start_again_cannot_help) => format!(
                "{menu}: menu, then {} remove   {} next agent   {} quit",
                key("x"),
                key("Tab"),
                key("q")
            ),
            Mode::Normal => format!(
                "{menu}: menu, then {} start again   {} remove   {} next agent   {} quit",
                key("r"),
                key("x"),
                key("Tab"),
                key("q")
            ),
            // The key for two agents side by side is named only where it
            // would do something: with two on screen, or room for two.
            Mode::Menu if self.engine.ws.pair().is_some() => {
                format!("{}  {} one agent", menu_line(), key("v"))
            }
            Mode::Menu if self.can_show_two().is_ok() => {
                format!("{}  {} two side by side", menu_line(), key("v"))
            }
            Mode::Menu => menu_line(),
            // The way back first, so a narrow window never cuts it off;
            // the agent's name, which may be long, last.
            Mode::StatusReport if self.report_scrolls() => format!(
                "{}  {}: back to the agent   {} scroll{}.  Of {}.",
                mode("STATUS REPORT"),
                key("Any other key"),
                ["PgUp", "PgDn", "Up", "Down", "Home"].map(key).join(" "),
                self.dismiss_hint(),
                self.focused_label(),
            ),
            // The key that stops showing waiting parts is named only while
            // there are some.
            Mode::StatusReport if self.focused_waiting_parts() > 0 => format!(
                "{}  {}: back to the agent{}.  Of {}.",
                mode("STATUS REPORT"),
                key("Any other key"),
                self.dismiss_hint(),
                self.focused_label(),
            ),
            Mode::StatusReport => format!(
                "{}  {}: back to the agent.  Of {}.",
                mode("STATUS REPORT"),
                key("Any key"),
                self.focused_label(),
            ),
            Mode::History => {
                let back = self.engine.ws.focused().map(|e| e.term.scrolled_back()).unwrap_or(0);
                let scroll = ["PgUp", "PgDn", "Up", "Down", "Home"].map(key).join(" ");
                format!(
                    "{} of {}: {} back.  {scroll} scroll   {}: back to now",
                    mode("HISTORY"),
                    self.focused_label(),
                    plural(back, "line", "lines"),
                    key("any other key")
                )
            }
            // What is being typed comes first and the hint after it, so a
            // long folder path cannot push the typing off the screen.
            Mode::NewAgent(form @ NewAgent { step: Step::Program, .. }) => {
                format!("{AGENT_PROMPT}{}   (Enter alone: {DEFAULT_COMMAND}   Esc: cancel)", form.input)
            }
            Mode::NewAgent(form @ NewAgent { step: Step::Options, .. }) => format!(
                "{}{}   (Enter alone: none   Tab: start it as {}   Esc: cancel)",
                form.options_prompt(),
                form.input,
                kind_words(form.kind().next())
            ),
            // The question and its keys first, then what the kind means and
            // the options as split; the program and folder, which may be
            // long paths, last.
            Mode::NewAgent(form @ NewAgent { step: Step::Check, .. }) => match self.check_problem() {
                Some(why) => self.fitted(&refusal_tries(&why)),
                None => {
                    let kind = kind_words(form.kind());
                    let options = split_options(&form.options);
                    let shown = match options.args.as_slice() {
                        [] => "No options.".to_string(),
                        args => {
                            format!("Options {}.", args.iter().map(|a| format!("[{a}]")).collect::<String>())
                        }
                    };
                    let unclosed = if options.unclosed_quote {
                        " There is a \" with no partner; it was kept as part of the last option."
                    } else {
                        ""
                    };
                    format!(
                        "Start it as {kind}? y / n / k   As {kind}: {}. {shown}{unclosed} n: change it, k: \
                         another kind. Program: {}. Folder: {}.",
                        kind_means(form.kind()),
                        form.command,
                        resolve_folder(&self.launch_dir, &form.folder).display(),
                    )
                }
            },
            Mode::NewAgent(form) => {
                format!(
                    "{FOLDER_PROMPT}{}   (Enter alone: {}   Esc: cancel)",
                    form.input,
                    self.launch_dir.display()
                )
            }
            // Every question puts the question and its keys first. The bar
            // is one line, and a question cut off before "y / n" cannot be
            // answered.
            Mode::Confirm(Confirm::Close(id)) if self.close_subject(*id).is_some_and(Entry::is_running) => {
                format!(
                    "Close {}? y / n   It is running: closing stops it and removes it from your workspace.",
                    self.close_subject_label(*id)
                )
            }
            Mode::Confirm(Confirm::Close(id)) => format!(
                "Remove {}? y / n   It has stopped: removing it takes it and its conversation out of your \
                 workspace.",
                self.close_subject_label(*id)
            ),
            Mode::Confirm(Confirm::Quit(id)) => {
                // The face shows the engine's question, never one of its own.
                debug_assert_eq!(self.engine.question().map(|q| q.id), Some(*id));
                self.quit_question()
            }
            Mode::Confirm(Confirm::NewAddress) => "Give the page a new address? y / n   The address it has \
                                                   now stops working at once, for bookmarks and open pages \
                                                   too."
                .to_string(),
            Mode::Confirm(Confirm::Parts) => {
                let waiting = self.engine.ws.focused().map(|e| e.track.waiting_parts()).unwrap_or_default();
                let which = match waiting.as_slice() {
                    [one] => format!("{}, {}", one.name(), waiting_since(one.since)),
                    many => format!(
                        "the longest {}",
                        many.iter().map(|p| p.since).min().map(waiting_since).unwrap_or_default()
                    ),
                };
                format!(
                    "Stop showing {} of {}? y / n   {which}. Nothing is sent to the agent. A part that \
                     repeats itself stays hidden; one that says something new and waits is shown again.",
                    plural(waiting.len(), "waiting part", "waiting parts"),
                    self.focused_label()
                )
            }
            Mode::Confirm(Confirm::View(id)) => {
                // The face shows the engine's question, never one of its own.
                debug_assert_eq!(self.engine.question().map(|q| q.id), Some(*id));
                let waiting: Vec<String> = self
                    .engine
                    .ws
                    .entries()
                    .iter()
                    .filter(|e| e.needs_you().is_some())
                    .map(Entry::label)
                    .collect();
                format!(
                    "Resize every agent now? y / n   Waiting for you: {}. Changing the view makes every \
                     agent redraw at a new size.",
                    waiting.join(", ")
                )
            }
            Mode::Confirm(Confirm::Paste(id)) => {
                // The face shows the engine's question, never one of its own.
                debug_assert_eq!(self.engine.question().map(|q| q.id), Some(*id));
                let lines = self.engine.held_paste_lines().unwrap_or(0);
                let (question, why) = line_by_line_question(lines, &self.focused_label());
                format!("{question} y / n   {why}")
            }
        }
    }

    /// The quit question, fitted to the window's one line. Its keys come
    /// first, then which agents may be in the middle of something, which is
    /// why it is asked, those that need you named first. When the whole
    /// does not fit, what is given up, in this order: the words saying the
    /// agents come back (the line printed on closing says so again), then
    /// the names of agents that do not need you, counted instead (the
    /// first is kept when none needs you), then that an agent starts a new
    /// conversation next time, then everything but who needs you, by name
    /// alone, and last that too, leaving the question and its keys. A
    /// sentence is given up whole: the question is never cut in the middle
    /// of one.
    fn quit_question(&self) -> String {
        let running = self.engine.ws.running_labels().len();
        // Every agent but Claude Code begins afresh, so what it was doing
        // is not picked up when it comes back.
        let fresh = match self.engine.fresh_next_time().as_slice() {
            [] => String::new(),
            [one] => format!(" {one} starts a new conversation next time."),
            many => format!(" {} start new conversations next time.", many.join(", ")),
        };
        // Shortened, an agent that needs you is still named: it is the one
        // quitting would interrupt in the middle of asking you something.
        // Only the others are counted instead. The engine puts those that
        // need you first.
        let listed = self.engine.unfinished();
        let need = listed.iter().filter(|u| u.needs_you).count();
        let unfinished: Vec<String> = listed.iter().map(named).collect();
        let kept = if need == 0 { unfinished.len().min(1) } else { need };
        let question = format!("Quit and stop {}? y / n", plural(running, "running agent", "running agents"));
        let head = format!("{question}   Not finished or not known: ");
        let all = unfinished.join(", ");
        let first = match unfinished.len() - kept {
            0 => all.clone(),
            more => format!("{} and {more} more", unfinished[..kept].join(", ")),
        };
        let back = if running == 1 { "It comes back" } else { "They come back" };
        let waiting: Vec<&str> = listed.iter().filter(|u| u.needs_you).map(|u| u.label.as_str()).collect();
        let mut tries = vec![
            format!("{head}{all}. {back} next time you start Lillignode.{fresh}"),
            format!("{head}{all}. {back} next time.{fresh}"),
            format!("{head}{all}.{fresh}"),
            format!("{head}{first}.{fresh}"),
            format!("{head}{first}."),
        ];
        if !waiting.is_empty() {
            tries.push(format!("{question}   Needs you: {}.", waiting.join(", ")));
        }
        tries.push(question);
        self.fitted(&tries)
    }

    /// The first of these, longest first, that fits the window's width, or
    /// the last if none does.
    fn fitted(&self, tries: &[String]) -> String {
        // No window yet: nothing to fit to.
        let room = match self.window.width {
            0 => usize::MAX,
            width => width as usize,
        };
        tries.iter().find(|t| fit::width(t) <= room).or(tries.last()).cloned().unwrap_or_default()
    }

    /// Call after each batch of events and ticks. The engine says what has
    /// become true and is said once: the Unknown notice, the first time the
    /// reporting of a program expected to report (Claude Code, Codex) is
    /// seen not to reach Lillignode; a cost that could not be read; a cost
    /// warning passed. Returns true if the bell should ring: an agent has
    /// just started needing you, and the bell is not switched off.
    pub fn after_events(&mut self) -> bool {
        self.engine.settle();
        let began = self.say_events();
        began && self.config.bell
    }

    /// A warning that has stopped being true does not stay on screen. Once
    /// no agent of a kind shows Unknown for want of reports (it reported
    /// after all, or was closed), the engine says so, the notice about that
    /// kind is taken out, and whatever else the notice says is left. Seen
    /// with Claude Code: it reports nothing while it asks whether to trust
    /// a folder, and reports as usual once that is answered. If that kind's
    /// reporting fails again later, the notice is given again.
    fn take_back_unknown_notice(&mut self, kind: AgentKind) {
        let (gone, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.unknown_notice_up).into_iter().partition(|(up, _)| *up == kind);
        self.unknown_notice_up = kept;
        for (_, text) in gone {
            if let Some(notice) = self.notice.take() {
                let rest = notice.replace(&text, "");
                self.notice = Some(rest.trim().to_string()).filter(|rest| !rest.is_empty());
            }
        }
    }
    /// Why every Claude Code agent shows Unknown, said once per run. The
    /// most specific cause Lillignode can see comes first. Each fits the two
    /// notice rows of the narrowest window Lillignode draws in
    /// (`NOTICE_LIMIT`), so what to do, which comes last, is never cut off.
    /// The longer explanation is in the status report panel.
    fn unknown_notice(&self, who: &status::Reporter, why: status::Why) -> String {
        let fine = "Agents work as before; nothing was changed.";
        let claude_code = *who == status::CLAUDE_CODE;
        if claude_code && self.status_off_switch.is_some() {
            return format!(
                "Status shows Unknown: {} is set here. {fine} Lillignode leaves that setting alone.",
                status::OFF_SWITCH
            );
        }
        if why == status::Why::NotUnderstood {
            return format!(
                "Status shows Unknown: {} sent a status Lillignode does not understand. {fine} Look for a \
                 newer Lillignode.",
                who.name
            );
        }
        // Only a program that waits for an answer needs the two files.
        if who.asks_first && self.newer_pty_missing {
            // Agents run as before; the fix is for people who built it
            // themselves, as the release zip has both files.
            return "Status shows Unknown: conpty.dll and OpenConsole.exe are not beside lillignode.exe. \
                    Nothing was changed. Run scripts\\package-windows.ps1 -BesideBuild."
                .to_string();
        }
        if !who.asks_first {
            // The likeliest cause comes first: seen with Codex signed out.
            return format!(
                "Status shows Unknown: {} is not reporting it. If it is signed out it reports nothing: \
                 sign in and it will. {fine}",
                who.name
            );
        }
        format!(
            "Status shows Unknown: {} is not reporting it. {fine} Checked with {} {}; get a newer \
             Lillignode.",
            who.name, who.name, who.checked_with
        )
    }

    /// How many agents are waiting for the user, the one in front included.
    pub fn needs_you_count(&self) -> usize {
        self.engine.ws.entries().iter().filter(|e| e.needs_you().is_some()).count()
    }

    /// The sentence naming agents that need the user and are not in front.
    /// It stays until they stop needing you, and an ordinary notice cannot
    /// replace it.
    pub fn needs_you_notice(&self) -> Option<String> {
        let focused = self.engine.ws.focused_index()?;
        let count = self.engine.ws.entries().len();
        let waiting: Vec<(usize, &Entry)> = self
            .engine
            .ws
            .entries()
            .iter()
            .enumerate()
            .filter(|(i, e)| *i != focused && e.needs_you().is_some())
            .collect();
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        match waiting.as_slice() {
            [] => None,
            [(i, e)] => {
                let kind = |kind: Option<status::Kind>| kind.map(|k| format!(" ({})", k.word()));
                // A part of the program, not the program itself: said, with
                // the part's own name and how long it has waited.
                let what = match (e.waiting_parts().as_slice(), e.needs_you().flatten()) {
                    ([one], _) => {
                        let why = match &one.state {
                            status::PartState::Waiting(k) => kind(*k).unwrap_or_default(),
                            _ => String::new(),
                        };
                        format!(
                            "needs you: one of its parts is waiting{why}, {}, {}",
                            one.name(),
                            waiting_since(one.since)
                        )
                    }
                    (many @ [_, _, ..], _) => format!(
                        "needs you: {} of its parts are waiting, the longest {}",
                        many.len(),
                        many.iter().map(|p| p.since).min().map(waiting_since).unwrap_or_default()
                    ),
                    ([], Some(kind)) => format!("needs you: {}", kind.word()),
                    ([], None) => "needs you".to_string(),
                };
                // Tab moves to the next agent in sidebar order and wraps
                // (`Workspace::focus_next`); the menu stays open between presses.
                let tabs = (i + count - focused) % count;
                let presses = if tabs == 1 { "Tab".to_string() } else { format!("Tab {tabs} times") };
                Some(format!("{} {what}. {menu} then {presses} to go to it.", e.label()))
            }
            many => {
                let names: Vec<String> = many.iter().map(|(_, e)| e.label()).collect();
                Some(format!(
                    "{} agents need you: {}. {menu} then Tab to go through them.",
                    many.len(),
                    names.join(", ")
                ))
            }
        }
    }

    /// What the notice rows show: who needs you first, then the latest notice.
    pub fn notice_text(&self) -> Option<String> {
        self.notice_fitted(usize::MAX)
    }

    /// The same, made to fit `room` characters. Each message ends with what
    /// to do, so when there is too much to show, words are taken out of the
    /// middle of a message and never off its end.
    pub fn notice_fitted(&self, room: usize) -> Option<String> {
        let (waiting, notice) = self.notice_parts(room)?;
        Some(match (waiting.is_empty(), notice.is_empty()) {
            (false, false) => format!("{waiting}  {notice}"),
            _ => format!("{waiting}{notice}"),
        })
    }

    /// The same in its two parts, so they can be drawn differently: the
    /// sentence about who needs you, then the latest notice. Either may be
    /// empty. None when there is nothing to say.
    pub fn notice_parts(&self, room: usize) -> Option<(String, String)> {
        let standing = self.standing();
        let notice = match (&standing, &self.notice) {
            (Some(settings), Some(notice)) => Some(format!("{settings}  {notice}")),
            (settings, notice) => settings.clone().or_else(|| notice.clone()),
        };
        let (waiting, notice) = match (self.needs_you_notice(), notice) {
            (Some(waiting), Some(notice)) => (waiting, notice),
            (Some(waiting), None) => return Some((keep_end(&waiting, room), String::new())),
            (None, notice) => return notice.map(|_| (String::new(), self.notice_fit(room))),
        };
        let count = fit::width;
        if count(&waiting) + 2 + count(&notice) <= room {
            return Some((waiting, notice));
        }
        // Who needs you comes first and is shortened last. The notice after
        // it is promised room for its last sentence, up to half the rows.
        let reserve = (count(last_sentence(&notice)) + 2).min(count(&notice)).min(room / 2);
        // A settings problem is never cut before its fix, its first sentence.
        let fix = standing.as_deref().map_or(0, |s| count(first_sentence(s)) + 2);
        let reserve = reserve.max(fix).min(room);
        let waiting = keep_end(&waiting, room.saturating_sub(reserve + 2));
        let notice = self.notice_fit(room.saturating_sub(count(&waiting) + 2));
        Some((waiting, notice))
    }

    /// What stays before any other notice until the next menu action: what
    /// is wrong with the settings file, then a workspace that could not be
    /// saved.
    fn standing(&self) -> Option<String> {
        match (&self.settings_problem, &self.save_problem) {
            (Some(settings), Some(save)) => Some(format!("{settings}  {save}")),
            (settings, save) => settings.clone().or_else(|| save.clone()),
        }
    }

    /// The notice, with any settings problem before it, in `room`
    /// characters. A notice keeps its last sentence, which says what to do.
    /// The settings problem leads with its fix, so it keeps its start, and
    /// it is shortened only when nothing else is left to shorten.
    fn notice_fit(&self, room: usize) -> String {
        let count = fit::width;
        let standing = self.standing();
        match (&standing, &self.notice) {
            (None, Some(notice)) => keep_end(notice, room),
            (None, None) => String::new(),
            (Some(settings), None) => cut_end(settings, room),
            (Some(settings), Some(_)) if count(settings) + 3 >= room => cut_end(settings, room),
            (Some(settings), Some(notice)) => {
                format!("{settings}  {}", keep_end(notice, room - count(settings) - 2))
            }
        }
    }

    /// A figure for the sidebar: tokens, or for a user who has said they
    /// pay by key, the estimate in dollars. Without a price for every model
    /// there is no dollar figure, so tokens are shown.
    fn figure(&self, usage: &Usage) -> String {
        match (self.config.billing, usage.dollars()) {
            (Billing::Key, Ok(amount)) => {
                format!("est. {}", cost::dollars_text(amount).replace("under ", "<"))
            }
            // Tokens read from cache are left out of the count shown here;
            // the status report panel has them (`Tokens::headline`).
            _ => format!("{} tokens", cost::short(usage.total().headline())),
        }
    }

    /// True when the user has asked for the cost readout. Off, nothing
    /// about cost is drawn and none of Claude Code's records are read.
    pub fn cost_shown(&self) -> bool {
        self.engine.ws.cost_on()
    }

    /// The few words about an agent's cost on its row of the sidebar.
    /// Pending and unknown are said as such, never shown as a number. A
    /// conversation past the user's warning says PAST WARNING before its figure.
    pub fn cost_short(&self, entry: &Entry) -> String {
        self.cost_words(&entry.cost(), self.engine.ws.over_cost_warning(entry).is_some())
    }

    /// The same, from a cost and whether it is past the user's warning.
    pub fn cost_words(&self, cost: &Cost, past_warning: bool) -> String {
        match cost {
            Cost::Known(usage) if past_warning => format!("PAST WARNING {}", self.figure(usage)),
            Cost::Known(usage) => self.figure(usage),
            Cost::Pending(_) => "cost pending".to_string(),
            Cost::Unknown(_) => "cost unknown".to_string(),
        }
    }

    /// The foot of the sidebar: the total for all agents whose cost is
    /// known, and on a second line how many agents are not in it. Last,
    /// always, that the readout is experimental. Empty when there are no
    /// agents, and when the readout is off.
    pub fn cost_total(&self) -> Vec<String> {
        if !self.cost_shown() {
            return Vec::new();
        }
        let all = self.engine.ws.cost_summary();
        let mut foot = match (all.known, all.pending, all.unknown) {
            (0, 0, 0) => return Vec::new(),
            (0, 0, _) => vec![" ALL AGENTS  cost unknown".to_string()],
            (0, _, _) => vec![" ALL AGENTS  cost pending".to_string()],
            (_, 0, 0) => vec![format!(" ALL AGENTS  {}", self.figure(&all.usage))],
            (_, pending, unknown) => vec![
                format!(" ALL AGENTS  {}", self.figure(&all.usage)),
                format!("  not counted: {}", plural(pending + unknown, "agent", "agents")),
            ],
        };
        foot.push(EXPERIMENTAL.to_string());
        foot
    }

    /// "11,500 tokens, not counting 20,500 read from cache"
    fn counted(tokens: &cost::Tokens) -> String {
        format!(
            "{} tokens, not counting {} read from cache",
            cost::exact(tokens.headline()),
            cost::exact(tokens.cache_read)
        )
    }

    /// The cost part of the status report panel: the breakdown, what the
    /// figure is and is not, and for pending or unknown, the reason.
    fn cost_report(&self, e: &Entry) -> String {
        if !self.cost_shown() {
            return "Cost: off. To see what each Claude Code conversation has used, add \"cost\": true to \
                    the settings file (lillignode --where says where it is) and start Lillignode again.\n"
                .to_string();
        }
        let record = "Claude Code's record of this conversation";
        // A warning that cannot be checked is said, not left silently off.
        let unchecked = match self.engine.ws.cost_warning() {
            Some(_) => " Your cost warning cannot be checked for this agent until its cost is known.",
            None => "",
        };
        let usage = match e.cost() {
            Cost::Known(usage) => usage,
            Cost::Pending(waiting) => {
                return format!(
                    "Cost (experimental): pending. {}{unchecked}\n",
                    match waiting {
                        Waiting::NotLooked => "Lillignode has not finished reading yet.".to_string(),
                        Waiting::NoRecord { looked_in } => format!(
                            "Nothing to read yet: Claude Code records a conversation once something has \
                             been said in it. Lillignode looked in {}.",
                            looked_in.display()
                        ),
                        Waiting::NoReplies => format!("{record} has no reply in it yet."),
                    }
                );
            }
            Cost::Unknown(why) => {
                return format!(
                    "Cost (experimental): unknown. {}{unchecked}\n",
                    match why {
                        WhyNot::NotClaudeCode => "Lillignode reads cost only for Claude Code.".to_string(),
                        WhyNot::NoConversation => {
                            "This agent has no conversation id for Lillignode to find its record by."
                                .to_string()
                        }
                        WhyNot::NoFolder =>
                            "Lillignode could not tell where Claude Code keeps its records: no \
                                             home folder is set."
                                .to_string(),
                        WhyNot::CannotRead(why) => {
                            format!("{record} could not be read ({why}). Nothing was changed.")
                        }
                        WhyNot::NotUnderstood { line, why } => format!(
                            "Line {line} of {record} was not understood: {why}. Lillignode shows no total \
                             rather than part of one. Nothing was changed. Type /usage in the agent to see \
                             its cost. This Lillignode reads the records of Claude Code {}; look for a newer \
                             Lillignode.",
                            cost::WRITTEN_FOR
                        ),
                    }
                );
            }
        };
        let all = usage.total();
        let mut out = format!(
            "Cost (experimental) of this conversation so far, including before today: {}.\n  input {}, output {}, \
             written to cache {}, read from cache {}\n",
            Self::counted(&all),
            cost::exact(all.input),
            cost::exact(all.output),
            cost::exact(all.cache_write),
            cost::exact(all.cache_read)
        );
        if usage.by_model.len() > 1 {
            for (model, tokens) in &usage.by_model {
                out += &format!("  {model}: {}\n", Self::counted(tokens));
            }
        }
        out += &match e.cost_since_started() {
            Some(since) => {
                format!("Since Lillignode was started: {}.\n", Self::counted(&since.total()))
            }
            None => format!(
                "Since Lillignode was started: not known. {record} was replaced while Lillignode was \
                 open, or could not be read when Lillignode first looked, so there is nothing to count \
                 from.\n"
            ),
        };
        let everyone = self.engine.ws.cost_summary();
        let left_out = everyone.pending + everyone.unknown;
        let left_out = match left_out {
            0 => String::new(),
            n => format!(" Not counted: {}.", plural(n, "agent", "agents")),
        };
        out += &match &everyone.since_started {
            Some(since) => format!(
                "All agents open now, since Lillignode was started: {}.{left_out}\n",
                Self::counted(&since.total())
            ),
            None => format!("All agents open now, since Lillignode was started: not known.{left_out}\n"),
        };
        if let Some(limit) = self.engine.ws.cost_warning() {
            out += &match self.engine.ws.over_cost_warning(e) {
                Some(_) => format!(
                    "Past your cost warning of {} tokens a conversation, counted the same way. It is only \
                     a warning: nothing was stopped or held.\n",
                    cost::exact(limit)
                ),
                None => format!(
                    "Your cost warning is at {} tokens a conversation, counted the same way. It is only a \
                     warning: nothing is stopped.\n",
                    cost::exact(limit)
                ),
            };
        }
        out += &match usage.dollars() {
            Ok(amount) => format!(
                "At list prices that is about {} (est.: Lillignode's price list of {}, cache writes at the \
                 5-minute rate, so it may be low).{}\n",
                cost::dollars_text(amount),
                cost::PRICES_DATED,
                match self.config.billing {
                    Billing::Key => "",
                    Billing::Subscription => " You are on a subscription, so you are not billed this.",
                    Billing::Unsaid => {
                        " On a subscription you are not billed this. If you pay by API key, put \
                         \"billing\": \"key\" in the settings file to see dollars in the sidebar."
                    }
                }
            ),
            Err(model) => format!(
                "No estimate in dollars: this Lillignode's price list of {} has no price for {model}.\n",
                cost::PRICES_DATED
            ),
        };
        out += "Limits: Unknown. Lillignode cannot read your plan's limits without changing Claude Code's \
                settings. Type /usage in the agent to see them.\n";
        out += &format!(
            "Read from {record}, which last changed {}. Work by its helper agents may not be included; \
             /usage in the agent is the figure to trust. Lillignode changes nothing and saves none of this.\n",
            ago(e.cost_last_grew())
        );
        out += FIRST_REQUEST;
        out
    }

    /// The engine's unfinished agents as the quit question names them,
    /// each with what it last said.
    #[cfg(test)]
    fn unfinished(&self) -> Vec<String> {
        self.engine.unfinished().iter().map(named).collect()
    }

    /// What the status report panel says about the focused agent: what the
    /// sidebar shows and why, and the last report exactly as it arrived, so
    /// an Unknown can be traced to its cause.
    /// Where the page is, for the foot of the status report: the one place
    /// the address can be read again once its notice is gone.
    fn page_lines(&self) -> String {
        let Some(address) = &self.page_address else { return String::new() };
        let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
        let types = if self.page_types {
            " They can also type to any agent from it: \"page_can_type\" is on in your settings file."
        } else {
            ""
        };
        let acts = if self.page_acts {
            " They can also act on the cards of your Board from it: \"page_can_act\" is on in your settings \
             file."
        } else {
            ""
        };
        let lasts = match &self.page_word_not_kept {
            // Kept nowhere on purpose: an address that has leaked must not
            // be enough to type to an agent later.
            None if self.page_types && self.page_acts => {
                "While it can type or act, the address is new each time Lillignode starts.".to_string()
            }
            None if self.page_acts => {
                "While it can act on the Board, the address is new each time Lillignode starts.".to_string()
            }
            None if self.page_types => {
                "While it can type, the address is new each time Lillignode starts.".to_string()
            }
            None => "The address stays the same from one start to the next.".to_string(),
            Some(why) => format!("The address could not be kept ({why}), so the next start makes another."),
        };
        let keeper = match &self.board_adapter_named {
            Some(program) => format!(
                "\nThe Board comes from a program your settings name (\"board_adapter\"): {program}. It runs as \
                 you, and what you press on a card goes to it.\n"
            ),
            None => String::new(),
        };
        format!(
            "\nThe page: {address}\nAnyone with this address can see every agent's name, status and screen \
             while Lillignode is open.{types}{acts} {lasts} For a new address now: {menu}, then w.\n{keeper}"
        )
    }

    pub fn status_report_text(&self) -> String {
        let mut out = self.status_report_body();
        out += &self.page_lines();
        out
    }

    fn status_report_body(&self) -> String {
        let Some(e) = self.engine.ws.focused() else { return "No agent is open.\n".to_string() };
        let t = &e.track;
        let heard = t.trace();
        // Who the agent was started as, if that is a program known to
        // report, and who its latest report says it is.
        let expected = e.kind().reporter();
        let sender = heard.app.as_deref();
        let mut out = format!("Status report for {}\n\n", e.label());
        // The status in full, with its reason: the sidebar shows it short.
        match e.reported() {
            Some(shown) => {
                out += &format!("Status: {}", shown.text());
                if let AgentStatus::Unknown(why) = shown {
                    out += &format!(". {}", status::why_long(why, expected));
                }
                out += &format!(". The sidebar shows it as {}.\n", shown.short());
                // Said when a part, not the program itself, is why.
                let (waiting, own) = (e.waiting_parts(), e.own_status());
                if let (false, Some(own)) = (waiting.is_empty(), own) {
                    out += &format!(
                        "Its own status: {}. {} waiting for you; they are listed below.\n",
                        own.text(),
                        match waiting.len() {
                            1 => "One of its parts is".to_string(),
                            n => format!("{n} of its parts are"),
                        }
                    );
                }
            }
            None => out += &format!("Status: {}. The program is not running.\n", e.status.text()),
        }
        // The status trace comes before cost, so the first page, in a short
        // window, still shows why a status is what it is.
        let claude_code = expected == Some(&status::CLAUDE_CODE);
        out += "\n";
        // Near the top, so a short window still shows it: when it is set,
        // nothing below can be other than empty.
        if let (true, Some(value)) = (claude_code, &self.status_off_switch) {
            out += &format!(
                "{} is set (to \"{}\") where Lillignode was started, and every agent inherits it. Claude \
                 Code does not report its status while it is switched on. Lillignode leaves it alone: it \
                 is your setting.\n",
                status::OFF_SWITCH,
                value.escape_debug()
            );
        }
        if e.is_running() {
            out += &match heard.asked_after {
                Some(after) => format!(
                    "Status question: asked {:.1} seconds after it started. Lillignode answered yes.\n",
                    after.as_secs_f32()
                ),
                None if expected.is_some_and(|who| who.asks_first) => format!(
                    "Status question: not asked in the {} since it started.\n",
                    plural(t.running_for(Instant::now()).as_secs() as usize, "second", "seconds")
                ),
                None => "Status question: not asked. A program may report without asking.\n".to_string(),
            };
        }
        out += &format!("Reports received since it started: {}", heard.reports);
        if heard.task_reports > 0 {
            out +=
                &format!(" ({} about parts of it, such as a session or a helper agent)", heard.task_reports);
        }
        out += ".\n";
        if !heard.parts.is_empty() || heard.parts_let_go > 0 {
            const LISTED: usize = 10;
            out += &format!("\nParts that have reported: {}.\n", heard.parts.len());
            // Newest first.
            for part in heard.parts.iter().rev().take(LISTED) {
                out += &part_line(part);
            }
            if let Some(more) = heard.parts.len().checked_sub(LISTED).filter(|more| *more > 0) {
                out += &format!("  and {more} more.\n");
            }
            if heard.parts_let_go > 0 {
                out += &format!(
                    "Let go, to keep no more than {}: {}. The one that reported longest ago and was \
                     not waiting goes first.\n",
                    status::PARTS_KEPT,
                    heard.parts_let_go
                );
            }
            if !t.waiting_parts().is_empty() {
                out += "A waiting part makes the agent Needs you until the part says otherwise, is \
                        withdrawn, or the program ends: it is not ended by a clock. d stops showing the \
                        parts that are waiting now, after asking. One that repeats itself stays hidden; \
                        one that says something new and waits is shown again.\n";
            }
        }
        if t.bells_held() > 0 {
            out += &format!(
                "Bells not rung: {}. It began to need you more often than Lillignode rings for one agent \
                 ({} in a row, then one every {} seconds). Its status was still shown each time.\n",
                t.bells_held(),
                status::BELLS,
                status::BELL_BACK.as_secs()
            );
        }
        out += "\n";
        match &heard.last {
            None => out += "No report has been received.\n",
            Some(last) => {
                let ago = last.at.elapsed().unwrap_or(Duration::ZERO).as_secs();
                let raw = status::visible(&last.raw);
                const SHOWN: usize = 1500;
                let raw = match raw.chars().count().checked_sub(SHOWN) {
                    Some(more) if more > 0 => {
                        format!("{} … ({more} more characters)", raw.chars().take(SHOWN).collect::<String>())
                    }
                    _ => raw,
                };
                out += &format!(
                    "Last report, received {} ago, as it arrived (its ending is not shown):\n{raw}\n",
                    plural(ago as usize, "second", "seconds")
                );
                out += &format!("Read as: {}.\n", reading_text(&last.reading));
            }
        }
        // Who the reports say they are from. The name is the program's own
        // word for itself, in the few characters the format allows.
        match (sender, expected) {
            (Some(app), Some(who)) if app != who.app => {
                out += &format!(
                    "Sent by: {}. This agent was started as {}; its reports name another program, and \
                     the status shown is that program's.\n",
                    sender_name(app),
                    who.name
                );
            }
            (Some(app), _) => out += &format!("Sent by: {}.\n", sender_name(app)),
            (None, _) if heard.reports > heard.task_reports => {
                out += "Its reports do not say which program sent them.\n";
            }
            (None, _) => {}
        }
        if claude_code && self.status_off_switch.is_none() {
            out += &format!("\n{} is not set where Lillignode was started.\n", status::OFF_SWITCH);
        }
        // What the reading was checked against: the program the reports
        // name, or else the one the agent was started as.
        let checked = match sender {
            Some(app) => status::known(app),
            None => expected,
        };
        out += "\nRead as the open Program Status format says (OSC 7501, revision 0.4). ";
        out += &match checked {
            Some(who) => format!("Checked with {} {}.", who.name, who.checked_with),
            None => "Not checked with this program.".to_string(),
        };
        out += " Kept in memory only, never on disk, and started afresh whenever the agent is started.";
        if checked == Some(&status::CODEX) {
            out += "\n\nTwo things to know about Codex's reports. On some of its own set-up screens that \
                    are waiting for an answer it reports Idle, so Lillignode shows Idle there and does \
                    not say it needs you. And Done means its turn ended, not that the work succeeded.";
        }
        // Cost is read for Claude Code only. While it is off, how to turn it
        // on is said on a Claude Code agent's report and nowhere else.
        if self.cost_shown() || e.is_claude_code() {
            out += "\n\n";
            out += &self.cost_report(e);
        } else {
            out += "\n";
        }
        out
    }

    /// Where the typing cursor sits in the bar, counted in columns, while
    /// the bar is asking for text.
    pub fn bar_cursor(&self) -> Option<usize> {
        match &self.mode {
            Mode::NewAgent(form) => {
                let prompt = match form.step {
                    Step::Program => AGENT_PROMPT.to_string(),
                    Step::Options => form.options_prompt(),
                    Step::Folder => FOLDER_PROMPT.to_string(),
                    Step::Check => return None,
                };
                Some(fit::width(&prompt) + fit::width(&form.input))
            }
            _ => None,
        }
    }
}

/// True on Windows when `conpty.dll` and `OpenConsole.exe` are not both
/// beside the running program. portable-pty then uses the pseudo-terminal
/// built into Windows, which drops Lillignode's answer to Claude Code.
fn newer_pty_missing() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    dir.is_none_or(|d| !(d.join("conpty.dll").is_file() && d.join("OpenConsole.exe").is_file()))
}

/// The first sentence of a message.
fn first_sentence(text: &str) -> &str {
    text.find(". ").map_or(text, |at| &text[..=at])
}

/// The last sentence of a message, which is where it says what to do.
fn last_sentence(text: &str) -> &str {
    let text = text.trim_end();
    text.rfind(". ").map_or(text, |at| text[at + 2..].trim_start())
}

/// `text` in at most `room` columns, cut at its end and the cut marked.
/// For a message that leads with what to do.
fn cut_end(text: &str, room: usize) -> String {
    fit::clip(text, room)
}

/// `text` in at most `room` columns. When it is too long its last
/// sentence is kept whole and the cut, marked with an ellipsis, is made
/// before it. Only a last sentence that is itself longer than the room loses
/// its end; then the start of the message is the most that can be shown.
pub(crate) fn keep_end(text: &str, room: usize) -> String {
    if fit::width(text) <= room {
        return text.to_string();
    }
    let tail = last_sentence(text);
    // Two more for the ellipsis and the space that join the two halves.
    let Some(head_cols) = room.checked_sub(fit::width(tail) + 2) else {
        return fit::clip(text, room);
    };
    let mut head = fit::take(text, head_cols).to_string();
    // The cut falls between words, not inside one.
    if text[head.len()..].chars().next().is_some_and(|c| c != ' ') {
        if let Some(space) = head.rfind(' ') {
            head.truncate(space);
        }
    }
    format!("{}… {tail}", head.trim_end())
}

fn reading_text(reading: &Reading) -> String {
    match reading {
        Reading::Status { status, message: Some(m), .. } => {
            format!("{}, with the message \"{m}\"", status.text())
        }
        Reading::Status { status, message: None, .. } => status.text(),
        Reading::Cleared => "the agent withdrew its status".to_string(),
        Reading::Task => "about one part of the program, not the program's own status".to_string(),
        Reading::NotUnderstood(why) => format!("not understood, because {why}"),
    }
}

/// How the sender of a report is named: by the name people use, for a
/// program Lillignode knows, and otherwise by its own word, in quotes.
fn sender_name(app: &str) -> String {
    match status::known(app) {
        Some(who) => who.name.to_string(),
        None => format!("\"{app}\""),
    }
}

/// Empty means the folder Lillignode was started in. A relative path is taken
/// from there too.
fn resolve_folder(launch_dir: &Path, typed: &str) -> PathBuf {
    if typed.is_empty() {
        launch_dir.to_path_buf()
    } else {
        launch_dir.join(typed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentId, AgentSpec, StartError};
    use crate::workspace::Restored;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Starts anything, and records what each agent was sent.
    #[derive(Default)]
    struct Fake {
        next: u32,
        sent: Rc<RefCell<Vec<String>>>,
    }

    impl AgentBackend for Fake {
        fn start(&mut self, _: &AgentSpec, _: Size) -> Result<AgentId, StartError> {
            self.next += 1;
            Ok(AgentId(self.next))
        }
        fn send(&mut self, _: AgentId, bytes: &[u8]) -> std::io::Result<()> {
            self.sent.borrow_mut().push(String::from_utf8_lossy(bytes).into_owned());
            Ok(())
        }
        fn resize(&mut self, _: AgentId, _: Size) {}
        fn close(&mut self, _: AgentId) {}
    }

    fn app_with_agent() -> (App<Fake>, Rc<RefCell<Vec<String>>>) {
        let backend = Fake::default();
        let sent = backend.sent.clone();
        let here = std::env::temp_dir();
        let mut app = App::launch(
            backend,
            Size { rows: 5, cols: 40 },
            Err("tests save nothing".into()),
            here.clone(),
            Config::default(),
            None,
        );
        app.engine.ws.start_as_named("cmd", here).unwrap();
        (app, sent)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn output(app: &mut App<Fake>, bytes: &[u8]) {
        app.on_agent_event(AgentEvent::Output(AgentId(1), bytes.to_vec()));
    }

    #[test]
    fn restore_text_says_which_conversations_continue() {
        assert_eq!(restore_text(&RestoreReport::default()), None);
        let all = RestoreReport {
            restored: vec![
                Restored { label: "Claude Code (api)".into(), continuing: true },
                Restored { label: "cmd (web)".into(), continuing: false },
            ],
            not_restored: vec![],
        };
        assert_eq!(
            restore_text(&all).unwrap(),
            "Restored 2 agents: Claude Code (api) (continuing its conversation), cmd (web) (new conversation)."
        );
        let some = RestoreReport {
            restored: vec![Restored { label: "Claude Code (api)".into(), continuing: true }],
            not_restored: vec![("codex (web)".into(), "\"codex\" is not installed or not on PATH".into())],
        };
        let text = restore_text(&some).unwrap();
        assert!(text.starts_with("Restored 1 of 2 agents (Claude Code (api) (continuing its conversation)). Not restored: codex (web): \"codex\" is not installed"), "{text}");
        assert!(
            text.ends_with("Switch to one, fix the cause, then r to try again or x to remove it."),
            "{text}"
        );
    }

    /// Found in a check by hand: with an agent refused for its saved entry,
    /// the screen and the bar no longer offered r, but the restore notice did.
    #[test]
    fn the_restore_notice_offers_r_only_where_starting_again_can_help() {
        let refused = format!("its name holds a control character. {}", crate::agent::FIX_IN_THE_FILE);
        let missing = "\"codex\" is not installed or not on PATH".to_string();
        let only_refused = RestoreReport {
            restored: vec![Restored { label: "Claude Code (api)".into(), continuing: true }],
            not_restored: vec![("bell (web)".into(), refused.clone())],
        };
        let text = restore_text(&only_refused).unwrap();
        assert!(
            text.ends_with(
                "Each is still in your workspace. Switch to one, then x to remove it; r will not help."
            ),
            "{text}"
        );
        assert!(!text.contains("r to try again"), "{text}");

        let both = RestoreReport {
            restored: vec![],
            not_restored: vec![("bell (web)".into(), refused), ("codex (web)".into(), missing)],
        };
        let text = restore_text(&both).unwrap();
        assert!(
            text.ends_with(
                "Switch to one, then x to remove it, or fix the cause and r to try again where r can help."
            ),
            "{text}"
        );

        // From a saved workspace, as at launch, the same.
        let mut app = app_with_claude_agents(0);
        let bell = crate::state::SavedAgent {
            name: "bell\u{7}".into(),
            command: "cmd".into(),
            kind: Some("program".into()),
            args: vec![],
            folder: std::env::temp_dir(),
            conversation: None,
            other: Default::default(),
        };
        let report = app.engine.ws.restore(crate::state::SavedWorkspace {
            version: crate::state::VERSION,
            agents: vec![bell],
            focused: Some(0),
            other: Default::default(),
        });
        let text = restore_text(&report).unwrap();
        assert!(!text.contains("r to try again"), "{text}");
        assert!(text.ends_with("r will not help."), "{text}");
    }

    #[test]
    fn folder_defaults_to_where_lillignode_was_started() {
        let base = Path::new(if cfg!(windows) { "C:\\work" } else { "/work" });
        assert_eq!(resolve_folder(base, ""), base);
        assert_eq!(resolve_folder(base, "api"), base.join("api"));
        let other = if cfg!(windows) { "D:\\else" } else { "/else" };
        assert_eq!(resolve_folder(base, other), Path::new(other));
    }

    #[test]
    fn either_menu_key_opens_the_menu_and_the_bar_always_names_them() {
        let (mut app, sent) = app_with_agent();
        assert!(app.bar().contains("Ctrl+Q or F12"), "{}", app.bar());
        app.on_key(press(KeyCode::F(12)));
        assert!(matches!(app.mode, Mode::Menu));
        app.on_key(press(KeyCode::Esc));
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Menu));
        // Neither menu key was passed to the agent.
        assert!(sent.borrow().is_empty());

        // With no agent, and in a window too small to draw, they are still named.
        app.on_key(press(KeyCode::Char('x')));
        app.on_key(press(KeyCode::Char('y')));
        assert!(app.bar().contains("Ctrl+Q or F12"), "{}", app.bar());
        app.too_small = true;
        assert!(app.bar().contains("Ctrl+Q or F12"), "{}", app.bar());
    }

    #[test]
    fn a_too_small_window_takes_no_typing_but_can_still_quit() {
        let (mut app, sent) = app_with_agent();
        app.too_small = true;
        app.on_key(press(KeyCode::Char('a')));
        app.on_paste("x\ny".into());
        assert!(sent.borrow().is_empty());
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        app.on_key(press(KeyCode::Char('y')));
        assert!(app.engine.closing());
    }

    #[test]
    fn a_paste_goes_as_one_marked_block_to_an_agent_that_takes_them() {
        let (mut app, sent) = app_with_agent();
        output(&mut app, b"\x1b[?2004h");
        app.on_paste("one\ntwo\nthree".into());
        assert_eq!(*sent.borrow(), vec!["\x1b[200~one\rtwo\rthree\x1b[201~".to_string()]);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn a_multi_line_paste_into_an_agent_that_does_not_is_held_until_the_user_agrees() {
        let (mut app, sent) = app_with_agent();
        app.on_paste("one\ntwo".into());
        assert!(sent.borrow().is_empty(), "nothing may be sent before the answer");
        assert!(app.bar().starts_with("Paste 2 lines into cmd"), "{}", app.bar());
        assert!(app.bar().contains("does not take a paste as one block"), "{}", app.bar());
        app.on_key(press(KeyCode::Char('n')));
        assert!(sent.borrow().is_empty());
        assert_eq!(app.notice.as_deref(), Some("Paste discarded. Nothing was sent."));

        app.on_paste("one\ntwo".into());
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(*sent.borrow(), vec!["one\rtwo".to_string()]);
    }

    #[test]
    fn a_yes_to_a_held_paste_whose_agent_stopped_says_nothing_was_sent() {
        let (mut app, sent) = app_with_agent();
        app.on_paste("one\ntwo".into());
        app.on_agent_event(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        app.on_key(press(KeyCode::Char('y')));
        assert!(sent.borrow().is_empty());
        // Not "Pasted into …": it was not.
        assert_eq!(app.notice.as_deref(), Some("Paste discarded. Nothing was sent."));
    }

    #[test]
    fn control_characters_never_reach_the_agent_and_the_user_is_told() {
        // As typed keys: Ctrl+C would interrupt the agent.
        let (mut app, sent) = app_with_agent();
        app.on_paste("make\x03 test\x04".into());
        assert_eq!(*sent.borrow(), vec!["make test".to_string()]);
        let label = app.engine.ws.focused().unwrap().label();
        assert_eq!(
            app.notice,
            Some(format!("Pasted into {label}. 2 control characters were left out (Ctrl+C, Ctrl+D)."))
        );

        // Inside paste markers too, and an escape cannot end the paste.
        let (mut app, sent) = app_with_agent();
        output(&mut app, b"\x1b[?2004h");
        app.on_paste("a\x1b[201~b\x1a".into());
        assert_eq!(*sent.borrow(), vec!["\x1b[200~a[201~b\x1b[201~".to_string()]);
        assert!(app
            .notice
            .as_deref()
            .unwrap()
            .ends_with("2 control characters were left out (Esc, Ctrl+Z)."));

        // Only control characters: nothing is sent, and that is said.
        let (mut app, sent) = app_with_agent();
        app.on_paste("\x03\x03".into());
        assert!(sent.borrow().is_empty());
        assert_eq!(
            app.notice.as_deref(),
            Some("Nothing was pasted: the text was only control characters (Ctrl+C). Nothing was sent.")
        );

        // More than three different ones: the first three are named.
        let (mut app, _) = app_with_agent();
        app.on_paste("x\x01\x02\x03\x04\x05".into());
        assert!(app.notice.as_deref().unwrap().ends_with("(Ctrl+A, Ctrl+B, Ctrl+C and 2 more)."));

        // A clean paste says nothing, as before.
        let (mut app, _) = app_with_agent();
        app.notice = None;
        app.on_paste("clean".into());
        assert_eq!(app.notice, None);
    }

    #[test]
    fn a_paste_sent_as_typed_keys_has_its_tabs_sent_as_spaces() {
        let (mut app, sent) = app_with_agent();
        app.on_paste("a\tb".into());
        assert_eq!(*sent.borrow(), vec!["a    b".to_string()]);
        assert!(app.notice.as_deref().unwrap().ends_with("so none was taken as a press of Tab."));

        // Line by line: the count is said after the answer, and the
        // question itself keeps its keys first.
        let (mut app, sent) = app_with_agent();
        app.on_paste("one\ttwo\x03\nthree".into());
        assert!(app.bar().starts_with("Paste 2 lines into cmd"), "{}", app.bar());
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(*sent.borrow(), vec!["one    two\rthree".to_string()]);
        let said = app.notice.clone().unwrap();
        assert!(said.contains("line by line. 1 control character was left out (Ctrl+C)."), "{said}");
        assert!(said.ends_with("press of Tab."), "{said}");

        // Inside paste markers a tab is text, and is kept.
        let (mut app, sent) = app_with_agent();
        output(&mut app, b"\x1b[?2004h");
        app.notice = None;
        app.on_paste("a\tb".into());
        assert_eq!(*sent.borrow(), vec!["\x1b[200~a\tb\x1b[201~".to_string()]);
        assert_eq!(app.notice, None);
    }

    #[test]
    fn every_question_fits_its_keys_into_a_narrow_bar() {
        // Found by the end-to-end run: with four agents the quit question
        // was cut off before "y / n".
        let (mut app, _) = app_with_agent();
        for i in 0..6 {
            app.engine
                .ws
                .start_as_named("cmd", std::env::temp_dir().join(format!("a-rather-long-folder-name-{i}")))
                .ok();
        }
        let narrow =
            |app: &App<Fake>| app.bar().chars().take(crate::ui::MIN_COLS as usize).collect::<String>();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(narrow(&app).contains("? y / n"), "{}", narrow(&app));
        app.on_key(press(KeyCode::Char('n')));
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('x')));
        assert!(narrow(&app).contains("? y / n"), "{}", narrow(&app));
        app.on_key(press(KeyCode::Char('n')));
        app.on_paste("a\nb".into());
        assert!(narrow(&app).contains("? y / n"), "{}", narrow(&app));
    }

    #[test]
    fn a_single_line_paste_needs_no_question_and_a_paste_cannot_answer_one() {
        let (mut app, sent) = app_with_agent();
        app.on_paste("just text".into());
        assert_eq!(*sent.borrow(), vec!["just text".to_string()]);

        // "y" arriving as a paste must not confirm a quit.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit(_))));
        app.on_paste("y".into());
        assert!(!app.engine.closing());
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit(_))));
    }

    #[test]
    fn history_scrolls_and_any_other_key_returns_without_reaching_the_agent() {
        let (mut app, sent) = app_with_agent();
        let lines: String = (1..=30).map(|i| format!("line {i}\r\n")).collect();
        output(&mut app, lines.as_bytes());

        app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT));
        assert!(matches!(app.mode, Mode::History));
        assert!(app.bar().starts_with("HISTORY of cmd"), "{}", app.bar());
        assert!(app.engine.ws.focused().unwrap().term.scrolled_back() > 0);

        app.on_key(press(KeyCode::Home));
        assert_eq!(app.engine.ws.focused().unwrap().term.screen().rows(0, 40).next().unwrap(), "line 1");

        app.on_key(press(KeyCode::Char('z')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.engine.ws.focused().unwrap().term.scrolled_back(), 0);
        assert!(sent.borrow().is_empty(), "the key that left history must not be typed into the agent");
    }

    #[test]
    fn scrolling_says_plainly_when_there_is_no_history_and_why() {
        let (mut app, _) = app_with_agent();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::PageUp));
        assert_eq!(
            app.notice.as_deref(),
            Some(&*format!("Nothing has scrolled off {}'s screen yet.", app.focused_label()))
        );
        assert!(matches!(app.mode, Mode::Normal));

        output(&mut app, b"\x1b[?1049h");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::PageUp));
        let notice = app.notice.clone().unwrap();
        assert!(notice.contains("full-screen display"), "{notice}");
        assert!(notice.contains("Scroll with the agent's own keys"), "{notice}");
        assert!(notice.contains("mouse wheel is not passed"), "{notice}");
    }
    #[test]
    fn s_shows_the_status_report_and_the_next_key_only_closes_it() {
        let (mut app, sent) = app_with_agent();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('s')));
        assert!(matches!(app.mode, Mode::StatusReport));
        assert!(app.bar().starts_with("STATUS REPORT  Any key: back to the agent.  Of cmd"), "{}", app.bar());
        let text = app.status_report_text();
        assert!(text.contains("Status: Unknown: no report."), "{text}");
        app.on_key(press(KeyCode::Char('z')));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(sent.borrow().is_empty(), "the closing key reached the agent: {:?}", sent.borrow());
    }

    #[test]
    fn a_status_report_longer_than_its_panel_scrolls_with_the_history_keys() {
        let (mut app, sent) = app_with_agent();
        let small = Rect::new(0, 0, crate::ui::MIN_COLS, crate::ui::MIN_ROWS);
        app.set_window(small);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('s')));
        let (lines, page) = crate::ui::report_view(&app.status_report_text(), small).unwrap();
        assert!(lines.len() > page, "the test needs a report that does not fit");
        assert!(
            app.bar().starts_with(
                "STATUS REPORT  Any other key: back to the agent   PgUp PgDn Up Down Home scroll.  Of cmd"
            ),
            "{}",
            app.bar()
        );
        let foot = |app: &App<Fake>| screen(app, small.width, small.height)[8].trim().to_string();
        assert_eq!(foot(&app), format!("│  lines 1 to {page} of {}", lines.len()));

        app.on_key(press(KeyCode::Down));
        assert_eq!(app.report_top, 1);
        app.on_key(press(KeyCode::PageDown));
        assert_eq!(app.report_top, (1 + page).min(lines.len() - page));
        for _ in 0..lines.len() {
            app.on_key(press(KeyCode::PageDown));
        }
        assert_eq!(app.report_top, lines.len() - page, "it stops at the last page");
        assert!(foot(&app).ends_with(&format!("to {n} of {n}", n = lines.len())), "{}", foot(&app));
        app.on_key(press(KeyCode::Up));
        assert_eq!(app.report_top, lines.len() - page - 1);
        app.on_key(press(KeyCode::Home));
        assert_eq!(app.report_top, 0);
        app.on_key(press(KeyCode::PageUp));
        assert_eq!(app.report_top, 0, "it stops at the top");
        assert!(matches!(app.mode, Mode::StatusReport), "scroll keys never close it");

        // Any other key closes it and goes nowhere; opened again, it starts at the top.
        app.on_key(press(KeyCode::PageDown));
        app.on_key(press(KeyCode::End));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(sent.borrow().is_empty(), "{:?}", sent.borrow());
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('s')));
        assert_eq!(app.report_top, 0);

        // A report that fits: any key closes it, the scroll keys too, as the
        // bar says.
        app.set_window(Rect::new(0, 0, 200, 80));
        assert!(app.bar().starts_with("STATUS REPORT  Any key: back to the agent.  Of cmd"), "{}", app.bar());
        app.on_key(press(KeyCode::PageDown));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(sent.borrow().is_empty(), "the closing key reached the agent");
    }

    #[test]
    fn s_with_no_agent_says_so() {
        let mut app = App::launch(
            Fake::default(),
            Size { rows: 5, cols: 40 },
            Err("tests save nothing".into()),
            std::env::temp_dir(),
            Config::default(),
            None,
        );
        app.on_key(press(KeyCode::Char('s')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.notice.as_deref(), Some("There is no agent to show a status report for."));
    }

    #[test]
    fn the_status_report_shows_a_long_report_cut_and_says_how_it_was_read() {
        let (mut app, _) = app_with_agent();
        app.engine.ws.start_as_named("claude", std::env::temp_dir()).unwrap();
        let report =
            format!("\x1b]7501;?\x07\x1b]7501;state=working:app=claude-code:msg={}\x07", "QUJD".repeat(600));
        app.on_agent_event(AgentEvent::Output(AgentId(2), report.into_bytes()));
        let text = app.status_report_text();
        assert!(text.starts_with("Status report for Claude Code"), "{text}");
        assert!(text.contains("Status: Working. The sidebar shows it as Working."), "{text}");
        // The trace comes before cost.
        let trace = text.find("Lillignode answered yes.").unwrap();
        let cost = text
            .find(
                "
Cost",
            )
            .unwrap_or(text.len());
        assert!(trace < cost, "{text}");
        assert!(text.contains("Lillignode answered yes."), "{text}");
        assert!(text.contains("more characters)"), "{text}");
        assert!(text.contains("Read as: Working, with the message \"ABCABC"), "{text}");
    }

    /// An app with `n` Claude Code agents, ids 1 to n, in folders a, b, c.
    /// Each has asked the status question, so its reports are used.
    fn app_with_claude_agents(n: u8) -> App<Fake> {
        let mut app = App::launch(
            Fake::default(),
            Size { rows: 5, cols: 40 },
            Err("tests save nothing".into()),
            std::env::temp_dir(),
            Config::default(),
            None,
        );
        app.status_off_switch = None;
        app.newer_pty_missing = false;
        for i in 0..n {
            let folder = std::env::temp_dir().join(char::from(b'a' + i).to_string());
            app.engine.ws.start_as_named("claude", folder).unwrap();
            report(&mut app, u32::from(i) + 1, "?");
        }
        app
    }

    fn report(app: &mut App<Fake>, id: u32, body: &str) {
        app.on_agent_event(AgentEvent::Output(AgentId(id), format!("\x1b]7501;{body}\x07").into_bytes()));
    }

    const PERMISSION: &str = "state=blocked:app=claude-code:kind=permission";

    #[test]
    fn a_sidebar_too_short_for_every_agent_says_how_many_are_where_and_who_needs_you() {
        let mut app = app_with_claude_agents(6);
        for id in 1..=6 {
            report(&mut app, id, "state=idle:app=claude-code");
        }
        report(&mut app, 1, PERMISSION);
        let side = |app: &App<Fake>| -> Vec<String> {
            screen(app, crate::ui::MIN_COLS, crate::ui::MIN_ROWS)
                .iter()
                .take(9)
                .map(|r| r.chars().take(27).collect::<String>().trim_end().to_string())
                .collect()
        };
        // The last one started, f, is in front, at the foot: a is hidden
        // above and needs you, so it is named.
        let rows = side(&app);
        assert_eq!(rows[0], " Lillignode  1 NEEDS YOU");
        assert_eq!(rows[1], " 3 more above");
        assert_eq!(rows[2], "  Claude Code (a) NEEDS YOU");
        assert!(rows[7].starts_with(">/\\z Claude Code"), "{rows:#?}");
        assert_eq!(rows[8], "  f  Idle");
        // Tab round to a: the list follows it, and says what is below.
        app.engine.ws.focus_next();
        let rows = side(&app);
        assert!(rows[1].starts_with(">/\\! Claude Code"), "{rows:#?}");
        assert_eq!(rows[7], " 3 more below");
        assert_eq!(rows[8], "", "nothing below the pointer: nobody hidden needs you");
        // Next, and next: the list does not move while the front is on screen.
        app.engine.ws.focus_next();
        app.engine.ws.focus_next();
        assert_eq!(side(&app)[1], " /\\! Claude Code", "a is still the first row shown");
        // A window with room for all: no pointer.
        let all = screen(&app, 100, 20);
        assert!(!all.iter().any(|r| r.contains("more above") || r.contains("more below")), "{all:#?}");
    }

    #[test]
    fn an_agent_behind_that_needs_you_is_named_with_the_keys_to_reach_it() {
        let mut app = app_with_claude_agents(3);
        // The last one started is in front: c. Go round to a.
        app.engine.ws.focus_next();
        assert_eq!(app.engine.ws.focused().unwrap().label(), "Claude Code (a)");
        assert_eq!(app.needs_you_notice(), None);
        report(&mut app, 3, PERMISSION);
        assert_eq!(
            app.needs_you_notice().as_deref(),
            Some("Claude Code (c) needs you: permission. Ctrl+Q then Tab 2 times to go to it.")
        );
        assert_eq!(app.needs_you_count(), 1);
        // An ordinary notice cannot replace it, and comes after it.
        app.notice = Some("Started something.".into());
        let shown = app.notice_text().unwrap();
        assert!(shown.starts_with("Claude Code (c) needs you"), "{shown}");
        assert!(shown.ends_with("Started something."), "{shown}");

        report(&mut app, 2, "state=blocked:app=claude-code");
        assert_eq!(
            app.needs_you_notice().as_deref(),
            Some("2 agents need you: Claude Code (b), Claude Code (c). Ctrl+Q then Tab to go through them.")
        );
        // In front, b has its block; the notice names only the others.
        app.engine.ws.focus_next();
        assert_eq!(
            app.needs_you_notice().as_deref(),
            Some("Claude Code (c) needs you: permission. Ctrl+Q then Tab to go to it.")
        );
        assert_eq!(app.needs_you_count(), 2, "the heading counts the one in front too");
        // It clears only when the agent says so, not on a key press.
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(app.needs_you_count(), 2);
        report(&mut app, 3, "state=working:app=claude-code");
        report(&mut app, 2, "state=working:app=claude-code");
        assert_eq!(app.needs_you_notice(), None);
        assert_eq!(app.notice_text().as_deref(), Some("Started something."));
    }

    #[test]
    fn the_bell_rings_once_per_change_into_needs_you_and_can_be_switched_off() {
        let mut app = app_with_claude_agents(2);
        assert!(!app.after_events());
        report(&mut app, 1, PERMISSION);
        assert!(app.after_events());
        assert!(!app.after_events(), "once, not on every redraw");
        report(&mut app, 1, "state=blocked:app=claude-code:kind=question");
        assert!(!app.after_events(), "still waiting: no new bell");
        report(&mut app, 2, PERMISSION);
        assert!(app.after_events(), "a second agent rings its own");

        app.config.bell = false;
        report(&mut app, 1, "state=working:app=claude-code");
        report(&mut app, 1, PERMISSION);
        assert!(!app.after_events());
    }

    #[test]
    fn quit_asks_only_while_an_agent_may_be_mid_request_and_names_only_those() {
        let mut app = app_with_claude_agents(2);
        report(&mut app, 1, "state=done:app=claude-code");
        report(&mut app, 2, "state=working:app=claude-code");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit(_))));
        let bar = app.bar();
        assert!(bar.starts_with("Quit and stop 2 running agents? y / n"), "{bar}");
        assert!(bar.contains("Not finished or not known: Claude Code (b) (Working)."), "{bar}");
        assert!(!bar.contains("Claude Code (a)"), "{bar}");
        app.on_key(press(KeyCode::Char('n')));
        assert!(!app.engine.closing());

        // Done, Idle and Error: nothing is in progress, so it quits at once.
        report(&mut app, 2, "state=error:app=claude-code");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.engine.closing());
    }

    #[test]
    fn quit_always_asks_for_starting_unknown_needs_you_and_other_agents() {
        for body in [None, Some("state=pondering:app=claude-code"), Some(PERMISSION)] {
            let mut app = app_with_claude_agents(1);
            if let Some(body) = body {
                report(&mut app, 1, body);
            }
            app.on_key(press(KeyCode::F(12)));
            app.on_key(press(KeyCode::Char('q')));
            assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit(_))), "{body:?}");
        }
        let (mut app, _) = app_with_agent();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.bar().contains("(Unknown: no report)"), "{}", app.bar());
    }

    #[test]
    fn the_unknown_notice_is_given_once_and_names_the_cause_it_can_see() {
        let mut app = app_with_claude_agents(2);
        app.notice = None;
        report(&mut app, 1, "state=pondering:app=claude-code");
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert!(
            notice.starts_with(
                "Status shows Unknown: Claude Code sent a status Lillignode does not understand"
            ),
            "{notice}"
        );
        assert!(notice.contains("Agents work as before; nothing was changed."), "{notice}");
        assert!(notice.ends_with("Look for a newer Lillignode."), "{notice}");
        app.notice = Some("later".into());
        report(&mut app, 2, "state=pondering:app=claude-code");
        app.after_events();
        assert_eq!(app.notice.as_deref(), Some("later"), "once per run");

        // It never replaces a message already up, which may be a failure.
        let mut app = app_with_claude_agents(1);
        app.notice = Some("Could not save your workspace (disk full).".into());
        report(&mut app, 1, "state=pondering:app=claude-code");
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert!(notice.starts_with("Status shows Unknown: Claude Code sent a status"), "{notice}");
        assert!(notice.ends_with("Could not save your workspace (disk full)."), "{notice}");

        // A withdrawal is about one agent, not about the reporting.
        let mut app = app_with_claude_agents(1);
        app.notice = None;
        report(&mut app, 1, "state=clear");
        app.after_events();
        assert_eq!(app.notice, None);

        // Causes Lillignode can see for itself come first.
        let mut app = app_with_claude_agents(0);
        app.newer_pty_missing = true;
        let text = app.unknown_notice(&status::CLAUDE_CODE, status::Why::NoReport);
        assert!(text.contains("conpty.dll and OpenConsole.exe are not beside lillignode.exe"), "{text}");
        assert!(
            text.ends_with(r"Nothing was changed. Run scripts\package-windows.ps1 -BesideBuild."),
            "{text}"
        );
        app.status_off_switch = Some("1".into());
        let text = app.unknown_notice(&status::CLAUDE_CODE, status::Why::NoReport);
        assert!(
            text.starts_with("Status shows Unknown: CLAUDE_CODE_DISABLE_TERMINAL_TITLE is set here."),
            "{text}"
        );
        app.status_off_switch = None;
        app.newer_pty_missing = false;
        let text = app.unknown_notice(&status::CLAUDE_CODE, status::Why::NotAsked);
        assert!(text.starts_with("Status shows Unknown: Claude Code is not reporting it."), "{text}");
    }

    /// Found in a check by hand: Claude Code sat at its folder trust
    /// question past the deadline, then reported Idle, and the notice
    /// saying it was not reporting stayed for the rest of the run.
    #[test]
    fn an_unknown_notice_that_stops_being_true_is_taken_back() {
        let mut app = app_with_claude_agents(1);
        app.notice = None;
        app.engine.ws.tick(Instant::now() + status::STARTING_DEADLINE);
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert!(notice.starts_with("Status shows Unknown: Claude Code is not reporting it."), "{notice}");
        // Still true: it stays.
        app.after_events();
        assert_eq!(app.notice.as_deref(), Some(notice.as_str()));
        // It reports after all: the warning goes.
        report(&mut app, 1, "state=idle:app=claude-code");
        app.after_events();
        assert_eq!(app.notice, None);

        // Only its own words go: a notice it was joined to is left.
        let mut app = app_with_claude_agents(1);
        app.notice = Some("Could not save your workspace (disk full).".into());
        app.engine.ws.tick(Instant::now() + status::STARTING_DEADLINE);
        app.after_events();
        report(&mut app, 1, "state=idle:app=claude-code");
        app.after_events();
        assert_eq!(app.notice.as_deref(), Some("Could not save your workspace (disk full)."));

        // A later notice that replaced it is not touched.
        let mut app = app_with_claude_agents(1);
        app.notice = None;
        app.engine.ws.tick(Instant::now() + status::STARTING_DEADLINE);
        app.after_events();
        app.notice = Some("later".into());
        report(&mut app, 1, "state=idle:app=claude-code");
        app.after_events();
        assert_eq!(app.notice.as_deref(), Some("later"));

        // If the reporting fails again, it is said again.
        report(&mut app, 1, "state=pondering:app=claude-code");
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert!(notice.starts_with("Status shows Unknown: Claude Code sent a status"), "{notice}");
    }

    /// Found in a check by hand, in a wide window: the question ran past
    /// the edge and its end was lost.
    #[test]
    fn the_quit_question_is_shortened_to_fit_and_keeps_its_keys_and_its_reason() {
        let mut app = app_with_claude_and_codex();
        report(&mut app, 1, PERMISSION);
        codex_reports(&mut app, 2, "state=working:app=codex");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        let reason =
            "Not finished or not known: Claude Code (a) (Needs you: permission), Codex (web) (Working).";
        let fresh = " Codex (web) starts a new conversation next time.";
        let count = |s: &str| s.chars().count();
        let whole = app.bar();
        assert!(
            whole.ends_with(&format!("They come back next time you start Lillignode.{fresh}")),
            "{whole}"
        );
        let mut seen = vec![whole.clone()];
        for width in (30..=count(&whole) as u16).rev() {
            app.set_window(Rect::new(0, 0, width, 30));
            let bar = app.bar();
            // The question and its keys come first at every width.
            assert!(bar.starts_with("Quit and stop 2 running agents? y / n"), "{width}: {bar}");
            // Never cut in the middle of a sentence: it ends with a whole
            // sentence, or with the keys. In any window Lillignode draws
            // in, it fits.
            assert!(bar.ends_with('.') || bar.ends_with("y / n"), "{width}: {bar}");
            assert!(!bar.contains('…'), "{width}: {bar}");
            if width >= crate::ui::MIN_COLS {
                assert!(count(&bar) <= width as usize, "{width}: {bar}");
            }
            if bar.ends_with(fresh) && count(&bar) <= width as usize {
                // Whenever every name fits, every name is there.
                let names = bar.contains(reason);
                assert!(
                    names
                        || count(&bar.replace("Claude Code (a) (Needs you: permission) and 1 more", ""))
                            < count(&bar)
                );
            }
            if seen.last() != Some(&bar) {
                seen.push(bar);
            }
        }
        // Each step gives up one thing, in this order.
        assert_eq!(seen.len(), 7, "{seen:#?}");
        // Then that an agent starts a new conversation, whole.
        assert!(
            seen[4].ends_with("or not known: Claude Code (a) (Needs you: permission) and 1 more."),
            "{}",
            seen[4]
        );
        // Then only who needs you, by name.
        assert_eq!(seen[5], "Quit and stop 2 running agents? y / n   Needs you: Claude Code (a).");
        assert!(count(&seen[5]) <= usize::from(crate::ui::MIN_COLS), "named in the narrowest window");
        // Last, the question and its keys alone.
        assert_eq!(seen[6], "Quit and stop 2 running agents? y / n");
        assert!(seen[1].contains(&format!("{reason} They come back next time.{fresh}")), "{}", seen[1]);
        assert!(seen[2].ends_with(&format!("{reason}{fresh}")), "{}", seen[2]);
        assert!(
            seen[3].ends_with(&format!("Claude Code (a) (Needs you: permission) and 1 more.{fresh}")),
            "{}",
            seen[3]
        );
    }

    fn b64(text: &str) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in text.as_bytes().chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
            for i in 0..=chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn a_waiting_part_is_named_in_the_notice_the_report_and_the_quit_question() {
        let mut app = app_with_claude_agents(2);
        app.notice = None;
        report(&mut app, 1, "state=done:app=claude-code");
        report(&mut app, 2, "state=idle:app=claude-code");
        // The title is the program's own text: what would draw or reorder
        // becomes spaces, and it is shown in quotes.
        let title = b64("subagent:\x1b[2J tests\u{202e}");
        report(&mut app, 1, &format!("state=blocked:kind=permission:id=sub/1:title={title}"));
        assert!(app.after_events(), "the bell rings for a part as for the program");
        assert_eq!(app.needs_you_count(), 1);
        // P5: its own record says Done, and the agent needs you.
        assert_eq!(app.engine.ws.entries()[0].own_status(), Some(AgentStatus::Done));
        assert_eq!(
            app.needs_you_notice().as_deref(),
            Some(
                "Claude Code (a) needs you: one of its parts is waiting (permission), \"subagent: [2J tests\", \
                 waiting since less than a minute ago. F12 then Tab to go to it."
            )
            .map(|s| s.replace("F12", &app.config.menu_keys[0].label()))
            .as_deref()
        );
        // A second, with no name: counted, and the oldest's age said.
        report(&mut app, 1, "state=blocked:id=sub/2");
        assert!(!app.after_events(), "already needing you: no second bell");
        let notice = app.needs_you_notice().unwrap();
        assert!(
            notice.starts_with(
                "Claude Code (a) needs you: 2 of its parts are waiting, the longest waiting since less than"
            ),
            "{notice}"
        );

        // Quit asks about it.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        let bar = app.bar();
        assert!(bar.contains("Not finished or not known: Claude Code (a) (Needs you)."), "{bar}");
        app.on_key(press(KeyCode::Char('n')));

        // The status report: why, the program's own status, and each part.
        app.engine.ws.focus_previous();
        let text = app.status_report_text();
        assert!(text.contains("Status: Needs you. The sidebar shows it as Needs you.\n"), "{text}");
        assert!(text.contains("Its own status: Done. 2 of its parts are waiting for you"), "{text}");
        assert!(text.contains("(2 about parts of it, such as a session or a helper agent)"), "{text}");
        let newest = text.find("  a part with no name (id sub/2): waiting for you, waiting since").unwrap();
        let older = text
            .find("  \"subagent: [2J tests\" (id sub/1): waiting for you (permission), waiting since")
            .unwrap();
        assert!(newest < older, "newest first: {text}");
        assert!(text.contains("it is not ended by a clock. d stops showing"), "{text}");
    }

    #[test]
    fn waiting_parts_are_dismissed_from_the_status_report_only_after_asking() {
        let (mut app, sent) = app_with_agent();
        report(&mut app, 1, "state=done:app=deploy");
        report(&mut app, 1, "state=blocked:id=a");
        report(&mut app, 1, "state=working:id=b");
        app.after_events();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('s')));
        let bar = app.bar();
        assert!(
            bar.starts_with("STATUS REPORT  Any other key: back to the agent   d: stop showing"),
            "{bar}"
        );
        app.on_key(press(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Parts)));
        let bar = app.bar();
        assert!(bar.starts_with("Stop showing 1 waiting part of cmd ("), "{bar}");
        assert!(
            bar.contains("? y / n   a part with no name, waiting since less than a minute ago."),
            "{bar}"
        );
        // No: nothing changes.
        app.on_key(press(KeyCode::Char('n')));
        assert_eq!(app.needs_you_count(), 1);
        assert!(app.notice.as_deref().unwrap().starts_with("Left as it was"), "{:?}", app.notice);
        // Yes: it is no longer shown, and is still listed.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('s')));
        app.on_key(press(KeyCode::Char('d')));
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(app.needs_you_count(), 0);
        let notice = app.notice.clone().unwrap();
        assert!(notice.starts_with("Stopped showing 1 waiting part of cmd ("), "{notice}");
        assert!(
            notice.ends_with("A part that repeats itself stays hidden; one that says something new and waits is shown again."),
            "{notice}"
        );
        let text = app.status_report_text();
        assert!(text.contains("Status: Done."), "{text}");
        assert!(text.contains("(id a): waiting for you, waiting since"), "{text}");
        assert!(text.contains("no longer shown at your word"), "{text}");
        // With none waiting, d is any key again: it closes the report.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('s')));
        assert!(app.bar().starts_with("STATUS REPORT  Any key: back to the agent."), "{}", app.bar());
        app.on_key(press(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::Normal));
        // It repeats itself: still hidden, and no bell.
        report(&mut app, 1, "state=blocked:id=a");
        assert!(!app.after_events());
        assert_eq!(app.needs_you_count(), 0);
        // It says something else, then waits again: shown again, with its
        // own bell.
        report(&mut app, 1, "state=working:id=a");
        report(&mut app, 1, "state=blocked:id=a");
        assert!(app.after_events());
        assert_eq!(app.needs_you_count(), 1);
        assert!(sent.borrow().is_empty(), "no key reached the agent: {:?}", sent.borrow());
    }

    #[test]
    fn the_status_report_lists_ten_parts_and_counts_the_rest_and_those_let_go() {
        let (mut app, _) = app_with_agent();
        for n in 0..70 {
            report(&mut app, 1, &format!("state=working:id=p{n}"));
        }
        // P3 and P4 as the report words them.
        report(&mut app, 1, "state=pondering:id=odd");
        report(&mut app, 1, "state=blocked:id=");
        let text = app.status_report_text();
        assert!(text.contains("Parts that have reported: 64.\n"), "{text}");
        assert!(
            text.contains(
                "  a part with no name (an id the format does not allow): waiting for you, waiting"
            ),
            "{text}"
        );
        assert!(
            text.contains("  a part with no name (id odd): not understood, less than a minute ago\n"),
            "{text}"
        );
        assert!(text.contains("  a part with no name (id p62): Working, less than a minute ago\n"), "{text}");
        assert!(!text.contains("(id p61)"), "{text}");
        assert!(text.contains("  and 54 more.\n"), "{text}");
        assert!(text.contains("Let go, to keep no more than 64: 8."), "{text}");
        // P4: a part not understood leaves the program's status alone.
        assert!(text.contains("Status: Needs you."), "{text}");
        assert_eq!(
            app.engine.ws.entries()[0].own_status(),
            Some(AgentStatus::Unknown(status::Why::NothingSent))
        );
    }

    /// An agent that needs you is named first, and is never one of the
    /// "and 1 more" when the question is shortened, even when it comes
    /// after another in the sidebar.
    #[test]
    fn the_quit_question_names_agents_that_need_you_first_and_never_folds_them_away() {
        let mut app = app_with_claude_and_codex();
        report(&mut app, 1, "state=working:app=claude-code");
        codex_reports(&mut app, 2, "state=blocked:app=codex:kind=permission");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        let needs = "Codex (web) (Needs you: permission)";
        let whole = app.bar();
        assert!(
            whole.contains(&format!("Not finished or not known: {needs}, Claude Code (a) (Working).")),
            "{whole}"
        );
        // Narrow enough that only the shortest form fits, which is where
        // names are given up.
        let mut shortened = false;
        for width in (crate::ui::MIN_COLS..=whole.chars().count() as u16).rev() {
            app.set_window(Rect::new(0, 0, width, 30));
            let bar = app.bar();
            assert!(bar.starts_with("Quit and stop 2 running agents? y / n"), "{width}: {bar}");
            // Named in every window Lillignode draws in: with what it said
            // while that fits, by name alone after.
            assert!(
                bar.contains(needs) || bar.ends_with("y / n   Needs you: Codex (web)."),
                "{width}: {bar}"
            );
            assert!(bar.chars().count() <= width as usize, "{width}: {bar}");
            if bar.contains("and 1 more") {
                shortened = true;
                assert!(bar.contains(&format!("{needs} and 1 more.")), "{width}: {bar}");
                assert!(!bar.contains("Claude Code (a) (Working)"), "{width}: {bar}");
            }
        }
        assert!(shortened, "the test never reached the shortened form");

        // Both need you: both are named, in sidebar order, and neither is
        // counted away, while a whole sentence naming them fits.
        report(&mut app, 1, PERMISSION);
        app.set_window(Rect::new(0, 0, 150, 30));
        let bar = app.bar();
        assert!(bar.ends_with(&format!("Claude Code (a) (Needs you: permission), {needs}.")), "{bar}");
        assert!(!bar.contains("more"), "{bar}");
        app.set_window(Rect::new(0, 0, crate::ui::MIN_COLS, 30));
        assert!(app.bar().ends_with("y / n   Needs you: Claude Code (a), Codex (web)."), "{}", app.bar());
        // Narrower than any sentence naming both: the question and its
        // keys, whole, and nothing cut.
        app.set_window(Rect::new(0, 0, 79, 30));
        assert_eq!(app.bar(), "Quit and stop 2 running agents? y / n");
    }

    /// Found in a check by hand: the screen said r would not help and the
    /// bar offered r.
    #[test]
    fn an_agent_refused_for_its_saved_entry_is_not_offered_start_again() {
        let mut app = app_with_claude_agents(0);
        let refused = state::SavedAgent {
            name: "bell\u{7}".into(),
            command: "claude".into(),
            kind: None,
            args: vec![],
            folder: std::env::temp_dir(),
            conversation: None,
            other: Default::default(),
        };
        let stops = state::SavedAgent { name: "Claude Code".into(), ..refused.clone() };
        app.engine.ws.restore(state::SavedWorkspace {
            version: state::VERSION,
            agents: vec![refused, stops],
            focused: Some(0),
            other: Default::default(),
        });
        app.on_agent_event(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert!(app.engine.ws.focused().unwrap().start_again_cannot_help());
        let bar = app.bar();
        assert!(bar.starts_with("Ctrl+Q or F12: menu, then x remove"), "{bar}");
        assert!(!bar.contains("start again"), "{bar}");
        // One that only needs its cause fixed still is.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Esc));
        assert!(!app.engine.ws.focused().unwrap().start_again_cannot_help());
        assert!(app.bar().starts_with("Ctrl+Q or F12: menu, then r start again"), "{}", app.bar());
    }

    /// Found in a check by hand: Codex, Grok Build and a plain program each
    /// showed the paragraph about turning on Claude Code's cost.
    #[test]
    fn how_to_turn_cost_on_is_said_only_on_a_claude_code_agent() {
        let mut app = app_with_claude_and_codex();
        let text = app.status_report_text();
        assert!(text.starts_with("Status report for Codex (web)"), "{text}");
        assert!(!text.contains("Cost"), "{text}");
        assert!(!text.contains("Claude Code"), "{text}");
        assert!(text.ends_with(".\n") && !text.ends_with("\n\n"), "{text:?}");
        app.engine.ws.focus_previous();
        let text = app.status_report_text();
        assert!(text.starts_with("Status report for Claude Code (a)"), "{text}");
        assert!(text.contains("Cost: off."), "{text}");
    }

    #[test]
    fn every_unknown_notice_fits_two_rows_of_the_narrowest_window() {
        // Found in a check by hand: the missing-files notice was cut
        // before its last instruction. Two notice rows at the narrowest width
        // Lillignode draws in hold this many characters.
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let mut app = app_with_claude_agents(0);
        for (off, missing) in [(true, false), (false, true), (false, false)] {
            app.status_off_switch = off.then(|| "1".to_string());
            app.newer_pty_missing = missing;
            for who in [&status::CLAUDE_CODE, &status::CODEX] {
                for why in [status::Why::NotAsked, status::Why::NoReport, status::Why::NotUnderstood] {
                    let text = app.unknown_notice(who, why);
                    assert!(text.chars().count() <= room, "{} characters: {text}", text.chars().count());
                }
            }
        }
    }

    #[test]
    fn a_notice_after_the_needs_you_sentence_keeps_its_last_instruction() {
        // Found before release: with the needs-you sentence first, a
        // long notice after it lost its end, which is where it says what to do.
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let mut app = app_with_claude_agents(3);
        app.engine.ws.focus_next();
        report(&mut app, 3, PERMISSION);
        let waiting = app.needs_you_notice().unwrap();
        let unknown = app.unknown_notice(&status::CLAUDE_CODE, status::Why::NoReport);
        let notices = [
            "Claude Code 2 (api) could not continue its earlier conversation (there may have been nothing \
             in it yet), so it started a new conversation. If there was an earlier one, the agent's own \
             resume command may still find it."
                .to_string(),
            "Restored 1 of 3 agents (Claude Code (api) (continuing its conversation)). Not restored: codex \
             (web): \"codex\" is not installed or not on PATH; Claude Code (gone): folder /work/gone does \
             not exist. Each is still in your workspace. Switch to one, fix the cause, then r to try again \
             or x to remove it."
                .to_string(),
            "Could not start claude: folder /a/long/path/that/goes/on/and/on/and/on/for/a/while/api does \
             not exist. Nothing was started. Correct the folder, or Esc to cancel."
                .to_string(),
            format!("{unknown}  Could not save your workspace (disk full). The last saved one is unchanged."),
        ];
        for notice in notices {
            let end = last_sentence(&notice).to_string();
            app.notice = Some(notice);
            assert!(app.notice_text().unwrap().chars().count() > room, "this test needs a notice too long");
            let shown = app.notice_fitted(room).unwrap();
            assert!(shown.chars().count() <= room, "{} characters: {shown}", shown.chars().count());
            assert!(shown.starts_with(&waiting), "who needs you is still whole and first: {shown}");
            assert!(shown.ends_with(&end), "the last instruction was lost: {shown}");
            assert!(shown.contains('…'), "the cut is marked: {shown}");
        }

        // Many agents waiting: that sentence is the one shortened, and it
        // keeps its own instruction too.
        let mut app = app_with_claude_agents(8);
        for id in 1..=7 {
            report(&mut app, id, PERMISSION);
        }
        app.notice = Some(
            "Claude Code (h) stopped with an error (exit code 3). It is still in your workspace. Switch to \
             it, then r to start it again or x to remove it."
                .into(),
        );
        let shown = app.notice_fitted(room).unwrap();
        assert!(shown.chars().count() <= room, "{shown}");
        assert!(shown.starts_with("7 agents need you: Claude Code (a)"), "{shown}");
        assert!(shown.contains("Ctrl+Q then Tab to go through them.  "), "{shown}");
        assert!(shown.ends_with("Switch to it, then r to start it again or x to remove it."), "{shown}");

        // One notice alone is cut the same way, and a short one is untouched.
        for id in 1..=7 {
            report(&mut app, id, "state=idle:app=claude-code");
        }
        app.notice = Some(format!("{}. Then press r.", "word ".repeat(60).trim_end()));
        let shown = app.notice_fitted(room).unwrap();
        assert!(shown.chars().count() <= room && shown.ends_with("… Then press r."), "{shown}");
        app.notice = Some("Short.".into());
        assert_eq!(app.notice_fitted(room).as_deref(), Some("Short."));
        // A last sentence longer than the rows: its start is all there is room for.
        assert_eq!(keep_end(&"x".repeat(50), 10), format!("{}…", "x".repeat(9)));
        assert_eq!(keep_end("anything", 0), "");
    }

    #[test]
    fn the_notice_rows_of_the_narrowest_window_end_with_what_to_do() {
        use ratatui::backend::TestBackend;
        let mut app = app_with_claude_agents(2);
        app.engine.ws.focus_next();
        report(&mut app, 2, PERMISSION);
        app.notice = Some(app.unknown_notice(&status::CLAUDE_CODE, status::Why::NoReport));
        let (cols, rows) = (crate::ui::MIN_COLS, crate::ui::MIN_ROWS);
        let mut term = ratatui::Terminal::new(TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let drawn: String = (rows - 3..rows - 1)
            .flat_map(|y| (0..cols).map(move |x| (x, y)))
            .map(|at| buf[at].symbol())
            .collect();
        assert!(
            drawn.starts_with("Claude Code (b) needs you: permission. Ctrl+Q then Tab to go to it."),
            "{drawn}"
        );
        assert!(drawn.trim_end().ends_with("get a newer Lillignode."), "{drawn}");
    }

    /// A throwaway stand-in for Claude Code's folder. Never the real one.
    fn claude_folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lillignode-test-app-cost-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("projects").join("p")).unwrap();
        dir
    }

    /// Record one reply in the conversation of the agent at `index`.
    fn record_reply(app: &App<Fake>, folder: &Path, index: usize, model: &str, line: &str) {
        use std::io::Write;
        let id = app.engine.ws.entries()[index].saved.conversation.clone().unwrap();
        let path = folder.join("projects").join("p").join(format!("{id}.jsonl"));
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        let line = line.replace("MODEL", model);
        writeln!(file, "{line}").unwrap();
    }

    const REPLY: &str =
        "{\"type\":\"assistant\",\"message\":{\"id\":\"msg_1\",\"model\":\"MODEL\",\"usage\":{\
        \"input_tokens\":1000,\"output_tokens\":500,\"cache_creation_input_tokens\":10000,\
        \"cache_read_input_tokens\":20500}}}";

    /// Take a fresh look at every agent's cost.
    fn look(app: &mut App<Fake>, nth: u32) {
        app.engine.ws.tick(Instant::now() + cost::LOOK_EVERY * nth);
    }

    #[test]
    fn cost_is_a_number_pending_or_unknown_and_the_total_says_what_it_leaves_out() {
        let folder = claude_folder("states");
        let mut app = app_with_claude_agents(2);
        app.engine.ws.start_as_named("cmd", std::env::temp_dir()).unwrap();
        // Off unless asked for: nothing is said about cost, and nothing read.
        assert!(!app.cost_shown());
        look(&mut app, 0);
        assert!(app.cost_total().is_empty());
        // Said on a Claude Code agent's report; the plain program is in front.
        app.engine.ws.focus_previous();
        assert!(app.status_report_text().contains("Cost: off. To see what each"));
        assert!(app.status_report_text().contains("add \"cost\": true to the settings file"));
        assert!(app.status_report_text().contains("and start Lillignode again."));
        app.engine.ws.focus_next();
        app.engine.ws.read_cost_from(Some(folder.clone()));
        assert_eq!(
            app.cost_total(),
            vec![" ALL AGENTS  cost pending", EXPERIMENTAL],
            "nothing has been looked at"
        );
        look(&mut app, 1);
        let shown =
            |app: &App<Fake>| app.engine.ws.entries().iter().map(|e| app.cost_short(e)).collect::<Vec<_>>();
        assert_eq!(shown(&app), ["cost pending", "cost pending", "cost unknown"]);
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  cost pending", EXPERIMENTAL]);

        // The count shown leaves out the 20,500 tokens read from cache.
        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 2);
        assert_eq!(shown(&app), ["11.5k tokens", "cost pending", "cost unknown"]);
        assert_eq!(
            app.cost_total(),
            vec![" ALL AGENTS  11.5k tokens", "  not counted: 2 agents", EXPERIMENTAL]
        );

        record_reply(&app, &folder, 1, "claude-opus-5-5", REPLY);
        look(&mut app, 3);
        assert_eq!(
            app.cost_total(),
            vec![" ALL AGENTS  23.0k tokens", "  not counted: 1 agent", EXPERIMENTAL]
        );

        // A reply that is not understood: unknown, and out of the total.
        record_reply(&app, &folder, 1, "claude-opus-5-5", "{\"type\":\"assistant\",\"message\":{}}");
        look(&mut app, 4);
        assert_eq!(shown(&app), ["11.5k tokens", "cost unknown", "cost unknown"]);
        assert_eq!(
            app.cost_total(),
            vec![" ALL AGENTS  11.5k tokens", "  not counted: 2 agents", EXPERIMENTAL]
        );
        for line in app.cost_total() {
            assert!(line.chars().count() <= 27, "does not fit the sidebar: {line}");
        }

        // No agents: nothing is said, rather than a total of zero.
        let empty = app_with_claude_agents(0);
        assert!(empty.cost_total().is_empty());
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn dollars_are_shown_only_to_a_user_who_pays_by_key_and_only_as_an_estimate() {
        let folder = claude_folder("dollars");
        let mut app = app_with_claude_agents(2);
        app.engine.ws.read_cost_from(Some(folder.clone()));
        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        record_reply(&app, &folder, 1, "claude-opus-5-5", REPLY);
        look(&mut app, 1);
        let first = |app: &App<Fake>| app.cost_short(&app.engine.ws.entries()[0]);
        assert_eq!(first(&app), "11.5k tokens");
        app.config.billing = Billing::Subscription;
        assert_eq!(first(&app), "11.5k tokens");
        app.config.billing = Billing::Key;
        assert_eq!(first(&app), "est. $0.07");
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  est. $0.14", EXPERIMENTAL]);

        // One agent on a model with no price: tokens for it, and tokens for
        // the total, never dollars that leave it out.
        record_reply(&app, &folder, 1, "claude-new-9", &REPLY.replace("msg_1", "msg_2"));
        look(&mut app, 2);
        assert_eq!(app.cost_short(&app.engine.ws.entries()[1]), "23.0k tokens");
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  34.5k tokens", EXPERIMENTAL]);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn the_status_report_explains_the_cost_in_every_state() {
        let folder = claude_folder("panel");
        let mut app = app_with_claude_agents(1);
        app.engine.ws.read_cost_from(Some(folder.clone()));
        look(&mut app, 1);
        let text = app.status_report_text();
        assert!(text.contains("Cost (experimental): pending. Nothing to read yet"), "{text}");
        assert!(text.contains("once something has been said in it"), "{text}");

        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 2);
        let text = app.status_report_text();
        assert!(
            text.contains(
                "Cost (experimental) of this conversation so far, including before today: 11,500 tokens, not counting \
                 20,500 read from cache."
            ),
            "{text}"
        );
        // The conversation began after Lillignode did, so all of it counts.
        assert!(
            text.contains(
                "Since Lillignode was started: 11,500 tokens, not counting 20,500 read from cache."
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "All agents open now, since Lillignode was started: 11,500 tokens, not counting 20,500 \
                 read from cache.\n"
            ),
            "{text}"
        );
        assert!(!text.contains("Your cost warning"), "no warning is set, so none is spoken of: {text}");
        assert!(text.ends_with(FIRST_REQUEST), "the first request is explained: {text}");
        assert!(
            text.contains("input 1,000, output 500, written to cache 10,000, read from cache 20,500"),
            "{text}"
        );
        assert!(text.contains("about $0.07 (est.: Lillignode's price list of 2026-11-15"), "{text}");
        assert!(text.contains("On a subscription you are not billed this."), "{text}");
        assert!(text.contains("Limits: Unknown."), "{text}");
        assert!(text.contains("which last changed less than a minute ago"), "{text}");
        assert!(text.contains("helper agents may not be included"), "{text}");
        app.config.billing = Billing::Subscription;
        assert!(app.status_report_text().contains("You are on a subscription, so you are not billed this."));

        record_reply(&app, &folder, 0, "claude-new-9", &REPLY.replace("msg_1", "msg_2"));
        look(&mut app, 3);
        let text = app.status_report_text();
        assert!(
            text.contains("  claude-new-9: 11,500 tokens, not counting 20,500 read from cache"),
            "by model once there is more than one: {text}"
        );
        assert!(text.contains("No estimate in dollars"), "{text}");
        assert!(text.contains("has no price for claude-new-9"), "{text}");
        assert!(!text.contains('$'), "no dollar figure without a price for every model: {text}");

        record_reply(&app, &folder, 0, "x", "{\"type\":\"assistant\",\"message\":{\"model\":\"m\"}}");
        look(&mut app, 4);
        let text = app.status_report_text();
        assert!(text.contains("Cost (experimental): unknown. Line 3 of Claude Code's record of this conversation was not understood: the reply has no id."), "{text}");
        assert!(
            text.contains("Lillignode shows no total rather than part of one. Nothing was changed."),
            "{text}"
        );
        assert!(text.contains("Type /usage in the agent"), "{text}");

        // Said once, in the notice rows, with where the reason is; and it
        // never replaces a notice already up.
        app.notice = Some("Earlier.".into());
        app.engine.status_already_said(&[AgentKind::ClaudeCode]);
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert!(
            notice.starts_with(
                "Cost shows unknown for Claude Code (a): Lillignode could not read Claude Code's record of it."
            ),
            "{notice}"
        );
        assert!(notice.contains("nothing was changed. Ctrl+Q then s says why.  Earlier."), "{notice}");
        app.notice = None;
        app.after_events();
        assert_eq!(app.notice, None, "once per run");

        // Any other agent: one plain line, and no notice. Nothing failed.
        let (mut app, _) = app_with_agent();
        app.engine.ws.read_cost_from(Some(folder.clone()));
        app.notice = None;
        look(&mut app, 1);
        app.after_events();
        assert_eq!(app.notice, None);
        assert!(app
            .status_report_text()
            .contains("Cost (experimental): unknown. Lillignode reads cost only for Claude Code."));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn the_sidebar_draws_each_agents_cost_beside_its_folder_and_the_total_at_the_foot() {
        use ratatui::backend::TestBackend;
        let folder = claude_folder("drawn");
        let mut app = app_with_claude_agents(2);
        let drawn = |app: &App<Fake>| {
            let mut term = ratatui::Terminal::new(TestBackend::new(100, 20)).unwrap();
            term.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..20u16)
                .map(|y| (0..27).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
                .collect::<Vec<_>>()
        };
        // Off unless asked for: two lines an agent, and no total.
        let rows = drawn(&app);
        assert_eq!(rows[1].trim_end(), " /\\. Claude Code");
        assert_eq!(rows[2].trim_end(), "  a  Starting");
        assert_eq!(rows[3].trim_end(), ">/\\. Claude Code", "the agent in front is marked");
        assert_eq!(rows[4].trim_end(), "  b  Starting");
        assert!(!rows.iter().any(|r| r.contains("cost") || r.contains("ALL AGENTS")), "{rows:?}");

        // On: a third line under each agent.
        app.engine.ws.read_cost_from(Some(folder.clone()));
        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 1);
        let rows = drawn(&app);
        assert_eq!(rows[2].trim_end(), "  a  Starting");
        assert_eq!(rows[3].trim_end(), "  11.5k tokens");
        assert_eq!(rows[5].trim_end(), "  b  Starting");
        assert_eq!(rows[6].trim_end(), "  cost pending");
        // The sidebar is 17 rows tall here: the total is on its last three.
        assert_eq!(rows[14].trim_end(), " ALL AGENTS  11.5k tokens");
        assert_eq!(rows[15].trim_end(), "  not counted: 1 agent");
        assert_eq!(rows[16].trim_end(), EXPERIMENTAL);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_notice_about_an_unreadable_workspace_keeps_its_reason_however_deep_the_folder() {
        let deep: PathBuf = std::iter::repeat_n("a-rather-long-folder-name", 12)
            .collect::<PathBuf>()
            .join("workspace.unreadable-1.json");
        let outcome = LoadOutcome::Lost { unreadable_kept_as: deep, why: "line 1: expected a value".into() };
        let text = load_problem_text(&outcome).unwrap();
        assert_eq!(
            text,
            "Your saved workspace could not be read: line 1: expected a value. There is no earlier copy, \
             so the workspace starts empty. The unreadable file was kept beside it, as \
             workspace.unreadable-1.json."
        );
        // Short enough that two rows of a 100-column notice hold it whole.
        assert!(text.chars().count() < 200, "{text}");
    }

    #[test]
    fn a_refused_agent_says_what_to_do_once_and_a_failed_one_gets_the_general_way_out() {
        let mut app = app_with_claude_agents(0);
        let agent = |name: &str, command: &str| crate::state::SavedAgent {
            name: name.into(),
            command: command.into(),
            kind: Some("program".into()),
            args: vec![],
            folder: std::env::temp_dir(),
            conversation: None,
            other: Default::default(),
        };
        app.engine.ws.restore(crate::state::SavedWorkspace {
            version: crate::state::VERSION,
            agents: vec![agent("bell\u{7}", "cmd")],
            focused: Some(0),
            other: Default::default(),
        });
        let shown = screen(&app, 140, 30).join("\n");
        assert!(shown.contains("r will not help"), "{shown}");
        assert!(!shown.contains("then r starts it again"), "no second way out that contradicts it: {shown}");
        assert!(shown.contains("It is still in your workspace."), "{shown}");
        assert!(crate::agent::FIX_IN_THE_FILE.starts_with(crate::agent::NOTHING_RUN));
    }

    #[test]
    fn with_no_room_for_the_cost_total_the_sidebar_says_where_it_is() {
        let folder = claude_folder("pointer");
        let mut app = app_with_claude_agents(4);
        app.engine.ws.read_cost_from(Some(folder.clone()));
        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 1);
        // 12 rows: 9 for the sidebar below its heading, too few for four
        // agents of three rows and the total.
        let rows = screen(&app, 100, 12);
        let side: Vec<String> = rows.iter().map(|r| r.chars().take(27).collect::<String>()).collect();
        assert_eq!(side[side.len() - 4].trim_end(), " Total: menu key, then s", "{side:#?}");
        assert!(!side.iter().any(|r| r.contains("ALL AGENTS")), "{side:#?}");
        // The agent in front is still shown.
        assert!(side.iter().any(|r| r.starts_with(">/\\")), "{side:#?}");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn passing_the_cost_warning_marks_the_row_and_says_so_once_and_nothing_else() {
        let folder = claude_folder("warning");
        let (mut app, sent) = app_with_agent();
        app.engine.ws.start_as_named("claude", std::env::temp_dir().join("a")).unwrap();
        app.engine.ws.read_cost_from(Some(folder.clone()));
        app.engine.ws.set_cost_warning(Some(20_000));
        record_reply(&app, &folder, 1, "claude-opus-5-5", REPLY);
        look(&mut app, 1);
        app.notice = None;
        assert!(!app.after_events(), "no bell");
        assert_eq!(app.notice, None, "11,500 is under 20,000");
        assert_eq!(app.cost_short(&app.engine.ws.entries()[1]), "11.5k tokens");
        assert!(app.status_report_text().contains("Your cost warning is at 20,000 tokens"));

        record_reply(&app, &folder, 1, "claude-opus-5-5", &REPLY.replace("msg_1", "msg_2"));
        look(&mut app, 2);
        app.notice = Some("Earlier.".into());
        assert!(!app.after_events(), "a warning rings no bell");
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "Warning only, nothing stopped: Claude Code (a) is past your cost warning (23.0k tokens; \
                 warning at 20.0k a conversation). Ctrl+Q then s shows the detail.  Earlier."
            )
        );
        assert_eq!(app.cost_short(&app.engine.ws.entries()[1]), "PAST WARNING 23.0k tokens");
        let text = app.status_report_text();
        assert!(text.contains("Past your cost warning of 20,000 tokens a conversation"), "{text}");
        assert!(text.contains("nothing was stopped or held"), "{text}");
        // Once: the row stays marked, the notice does not come back.
        app.notice = None;
        app.after_events();
        assert_eq!(app.notice, None);
        // Nothing is interrupted: no question is asked, the agent still
        // runs, and typing still reaches it.
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.engine.ws.entries()[1].is_running());
        app.on_key(press(KeyCode::Char('k')));
        assert_eq!(*sent.borrow(), vec!["k".to_string()]);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn the_menu_line_fits_the_narrowest_window_with_both_ways_out_first() {
        let (mut app, _) = app_with_agent();
        app.on_key(press(KeyCode::F(12)));
        let bar = app.bar();
        assert!(bar.chars().count() <= usize::from(crate::ui::MIN_COLS), "{} columns: {bar}", bar.len());
        assert!(bar.starts_with("MENU Esc back  q quit  "), "{bar}");
        for key in ["Tab next", "n new", "x close", "r start", "s status", "PgUp history"] {
            assert!(bar.contains(key), "{key} is not named: {bar}");
        }
    }

    #[test]
    fn needs_you_is_the_only_solid_block_in_the_sidebar_and_the_heading_counts_it() {
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        let mut app = app_with_claude_agents(2);
        report(&mut app, 1, PERMISSION);
        report(&mut app, 2, "state=working:app=claude-code");
        let mut term = ratatui::Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let row = |y: u16| (0..27).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        assert_eq!(row(0).trim_end(), " Lillignode  1 NEEDS YOU");
        // Its mark carries the sign, its second line is the block.
        assert_eq!(row(1).trim_end(), " /\\! Claude Code");
        let block = (0..20).find(|&y| row(y).trim_end() == "  a  NEEDS YOU").expect("block drawn");
        assert_eq!(block, 2);
        // Across the full sidebar width, black on yellow.
        for x in 0..27 {
            assert_eq!(buf[(x, block)].bg, Color::Yellow, "column {x}");
            assert_eq!(buf[(x, block)].fg, Color::Black, "column {x}");
        }
        let yellow_rows = (1..20).filter(|&y| buf[(0, y)].bg == Color::Yellow).count();
        assert_eq!(yellow_rows, 1, "no other sidebar row is a block");
        assert_eq!(row(3).trim_end(), ">/\\+ Claude Code");
        assert_eq!(row(4).trim_end(), "  b  Working");
        // Yellow is used for nothing else Lillignode draws: not the bar, not
        // an ordinary notice, not the menu.
        app.notice = Some("Started something.".into());
        app.on_key(press(KeyCode::F(12)));
        let mut term = ratatui::Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        for y in 0..20u16 {
            for x in 0..100u16 {
                let cell = &buf[(x, y)];
                let yellow = cell.bg == Color::Yellow || cell.fg == Color::Yellow;
                // The heading's count, the sign, the block, and the sentence
                // in the notice that names who needs you.
                let sentence = app.needs_you_notice().map_or(0, |s| s.chars().count());
                let in_sidebar = x < 28;
                let about_needs_you =
                    (in_sidebar && (y <= 1 || y == block)) || (y == 17 && usize::from(x) < sentence);
                assert!(!yellow || about_needs_you, "yellow at {x},{y}: {:?}", cell.symbol());
            }
        }
    }

    /// Columns `from` to `to` of a drawn row, without the spaces at its end.
    fn part(row: &str, from: usize, to: usize) -> String {
        row.chars().skip(from).take(to - from).collect::<String>().trim_end().to_string()
    }

    /// Everything on screen, row by row.
    fn screen(app: &App<Fake>, cols: u16, rows: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        let mut term = ratatui::Terminal::new(TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..rows).map(|y| (0..cols).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect()
    }

    #[test]
    fn each_status_draws_its_own_sign_and_word_under_a_hat() {
        let mut app = app_with_claude_agents(6);
        app.engine.ws.start_as_named("cmd", std::env::temp_dir().join("g")).unwrap();
        report(&mut app, 1, "state=working:app=claude-code");
        report(&mut app, 2, PERMISSION);
        report(&mut app, 3, "state=done:app=claude-code");
        report(&mut app, 4, "state=idle:app=claude-code");
        report(&mut app, 5, "state=error:app=claude-code");
        app.on_agent_event(AgentEvent::Exited(AgentId(6), crate::agent::Exit { code: 0 }));
        let rows = screen(&app, 100, 30);
        let sidebar: Vec<String> = rows.iter().map(|r| part(r, 0, 27)).collect();
        let expected = [
            (" /\\+ Claude Code", "  a  Working"),
            (" /\\! Claude Code", "  b  NEEDS YOU"),
            (" /\\= Claude Code", "  c  Done"),
            (" /\\z Claude Code", "  d  Idle"),
            (" /\\x Claude Code", "  e  Error"),
            (" /\\- Claude Code", "  f  Exited"),
            (">/\\? cmd", "  g  Unknown"),
        ];
        for (i, (first, second)) in expected.iter().enumerate() {
            assert_eq!(sidebar[1 + 2 * i], *first);
            assert_eq!(sidebar[2 + 2 * i], *second);
        }
        // Working moves, one frame a second; nothing else does.
        app.clock = 1;
        let later = screen(&app, 100, 30);
        assert_eq!(part(&later[1], 0, 27), " /\\* Claude Code");
        for y in 2..30 {
            assert_eq!(part(&later[y], 0, 27), part(&rows[y], 0, 27), "row {y} changed with the clock");
        }
    }

    #[test]
    fn a_long_folder_keeps_the_short_status_whole_and_marks_what_it_cut() {
        let mut app = app_with_claude_agents(0);
        app.engine.ws.start_as_named("cmd", std::env::temp_dir().join("a-long-folder-name")).unwrap();
        let rows = screen(&app, 100, 20);
        assert_eq!(part(&rows[2], 0, 27), "  a-long-folder-n…  Unknown");
    }

    #[test]
    fn the_agent_in_front_is_copper_and_the_sidebar_never_colours_the_agents_screen() {
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        let copper = Color::Rgb(0xC1, 0x7B, 0x3E);
        let mut app = app_with_claude_agents(2);
        output(&mut app, b"plain text from the agent");
        report(&mut app, 2, "state=working:app=claude-code");
        app.on_agent_event(AgentEvent::Output(AgentId(2), b"words the agent printed".to_vec()));
        let draw = |app: &App<Fake>| {
            let mut term = ratatui::Terminal::new(TestBackend::new(100, 20)).unwrap();
            term.draw(|f| crate::ui::draw(f, app)).unwrap();
            term.backend().buffer().clone()
        };
        let buf = draw(&app);
        // The wordmark, both hats, and the name of the agent in front.
        assert_eq!(buf[(1, 0)].fg, copper, "wordmark");
        assert_eq!((buf[(1, 1)].fg, buf[(2, 1)].fg), (copper, copper), "hat");
        assert_eq!((buf[(1, 3)].fg, buf[(2, 3)].fg), (copper, copper), "hat");
        assert_eq!(buf[(5, 3)].fg, copper, "the agent in front");
        assert_ne!(buf[(5, 1)].fg, copper, "an agent behind");
        // The sidebar has a background of its own; the agent's screen has
        // none of Lillignode's colours at all.
        assert_ne!(buf[(0, 5)].bg, Color::Reset);
        for y in 0..17u16 {
            for x in 28..100u16 {
                let cell = &buf[(x, y)];
                assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "at {x},{y}");
            }
        }
        // With fewer colours: no background, no copper, and the same words.
        let words = screen(&app, 100, 20);
        for depth in [Depth::Indexed, Depth::Basic, Depth::None] {
            app.colors = depth;
            assert_eq!(screen(&app, 100, 20), words, "{depth:?} changed what is written");
        }
        app.colors = Depth::None;
        let buf = draw(&app);
        for y in 0..20u16 {
            for x in 0..100u16 {
                let cell = &buf[(x, y)];
                assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "colour at {x},{y}");
            }
        }
    }

    #[test]
    fn the_bar_emphasises_keys_and_its_pieces_say_what_the_bar_says() {
        let (mut app, _) = app_with_agent();
        let joined = |app: &App<Fake>| app.bar_parts().into_iter().map(|(_, t)| t).collect::<String>();
        assert_eq!(joined(&app), app.bar());
        let keys = |app: &App<Fake>| {
            app.bar_parts()
                .into_iter()
                .filter(|(p, _)| *p == BarPart::Key)
                .map(|(_, t)| t)
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&app), ["Ctrl+Q", "F12"]);
        app.on_key(press(KeyCode::F(12)));
        assert_eq!(joined(&app), app.bar());
        assert_eq!(app.bar_parts()[0], (BarPart::Mode, "MENU".to_string()));
        assert_eq!(keys(&app), ["Esc", "q", "Tab", "n", "x", "r", "s", "PgUp"]);
        // No mark is ever shown.
        assert!(app.bar().chars().all(|c| !c.is_control()), "{:?}", app.bar());
    }

    #[test]
    fn a_pasted_folder_keeps_nothing_that_draws_no_character() {
        let (mut app, _) = app_with_agent();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('n')));
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        app.on_paste("C:\\work\u{1}\tapi\u{3}\nsecond line".into());
        assert!(app.bar().starts_with("Folder to work in: C:\\workapi   (Enter alone"), "{}", app.bar());
        // The cursor sits where the typing is, not past it.
        assert_eq!(app.bar_cursor(), Some("Folder to work in: C:\\workapi".chars().count()));
    }

    #[test]
    fn the_welcome_screen_shows_the_gnome_the_pitch_and_the_key_to_start() {
        let app = app_with_claude_agents(0);
        let rows = screen(&app, 100, 20);
        let agent_area: Vec<String> = rows.iter().map(|r| part(r, 28, 100)).collect();
        for (i, line) in crate::look::GNOME.iter().enumerate() {
            assert!(agent_area[1 + i].starts_with(&format!("  {line}")), "{:?}", agent_area[1 + i]);
        }
        let text = agent_area.join("\n");
        assert!(agent_area[1].contains("No agents yet. Press n to start one."), "{text}");
        assert!(text.contains("Lillignode runs several AI coding agents at once"), "{text}");
        assert!(text.contains("never asks for your keys."), "{text}");
        assert!(text.contains("Ctrl+Q or F12 opens Lillignode's menu."), "{text}");
        assert_eq!(part(&rows[1], 0, 27), " No agents yet. Press n.");

        // The narrowest window: the key to start is still whole, and first.
        let rows = screen(&app, crate::ui::MIN_COLS, crate::ui::MIN_ROWS);
        let text: String =
            rows[..9].iter().map(|r| part(r, 43, 80).trim().to_string()).collect::<Vec<_>>().join(" ");
        assert!(text.contains("No agents yet. Press n to start one."), "{text}");
        assert!(rows[11].starts_with("Ctrl+Q or F12: menu   n new agent   q quit"), "{}", rows[11]);
    }

    /// A window wide enough for two agents side by side.
    const WIDE: Rect = Rect { x: 0, y: 0, width: 130, height: 30 };

    #[test]
    fn a_settings_mistake_leads_with_its_fix_and_is_never_cut_before_it() {
        let dir = std::env::temp_dir().join(format!("lillignode-test-settings-fix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::config::FILE), r#"{"cost_warning_tokens": 500000}"#).unwrap();
        let (config, problem) = crate::config::load(&dir);
        std::fs::remove_dir_all(&dir).ok();
        let fix = "To fix your settings file: add \"cost\": true, or take out \"cost_warning_tokens\", which needs it.";
        assert!(problem.as_deref().unwrap().starts_with(fix), "{problem:?}");
        let mut app = App::launch(
            Fake::default(),
            Size { rows: 5, cols: 40 },
            Err("tests save nothing".into()),
            std::env::temp_dir(),
            config,
            problem,
        );
        // Another notice after it, and someone needing you before it, in the
        // narrowest window: the fix is still whole.
        app.engine.ws.start_as_named("claude", std::env::temp_dir()).unwrap();
        app.engine.ws.start_as_named("claude", std::env::temp_dir()).unwrap();
        report(&mut app, 1, "?");
        report(&mut app, 2, "?");
        let behind = 1 - app.engine.ws.focused_index().unwrap() as u32;
        report(&mut app, behind + 1, PERMISSION);
        app.notice = Some(
            "Claude Code (b) stopped with an error (exit code 3). It is still in your workspace. Switch to \
             it, then r to start it again or x to remove it."
                .into(),
        );
        let rows = screen(&app, crate::ui::MIN_COLS, crate::ui::MIN_ROWS);
        let text = rows.iter().map(|r| r.trim().to_string()).collect::<Vec<_>>().join(" ");
        assert!(text.contains(fix), "{text}");
        let waiting = app.engine.ws.entries()[behind as usize].label();
        let (who, at_fix) = (text.find(waiting.as_str()), text.find(fix));
        assert!(who.is_some() && who < at_fix, "who needs you is still first: {text}");
        assert!(text.contains("Ctrl+Q then Tab to go to it."), "{text}");

        // It has been seen once a menu key is used; it is still said on the way out.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Esc));
        assert!(!app.notice_text().unwrap_or_default().contains("settings file"));
        assert!(app.start_problems[0].starts_with(fix));
    }

    #[test]
    fn the_menu_key_pressed_twice_closes_the_menu_and_never_quits() {
        let mut app = app_with_claude_agents(2);
        for id in 1..=2 {
            report(&mut app, id, "state=idle:app=claude-code");
        }
        for menu_key in [KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL), press(KeyCode::F(12))] {
            app.on_key(menu_key);
            assert!(matches!(app.mode, Mode::Menu));
            app.on_key(menu_key);
            assert!(matches!(app.mode, Mode::Normal), "the menu key again closes the menu");
            assert!(!app.engine.closing(), "and does not quit");
        }
        // A key held with Ctrl or Alt is not a command.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.engine.ws.entries().len(), 2);
    }

    /// Menu, n, then each answer typed and Enter pressed after it.
    fn answer_form(app: &mut App<Fake>, answers: &[&str]) {
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('n')));
        for answer in answers {
            for c in answer.chars() {
                app.on_key(press(KeyCode::Char(c)));
            }
            app.on_key(press(KeyCode::Enter));
        }
    }

    #[test]
    fn with_no_options_and_the_kind_its_name_suggests_an_agent_starts_without_a_last_question() {
        let mut app = app_with_claude_agents(0);
        answer_form(&mut app, &["", "", ""]);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.engine.ws.entries().len(), 1);
        assert_eq!(app.engine.ws.entries()[0].kind(), AgentKind::ClaudeCode);
        assert!(app.notice.clone().unwrap().starts_with("Started Claude Code in "));
    }

    #[test]
    fn a_workspace_that_cannot_be_saved_is_said_after_each_change_and_the_change_stands() {
        // A file where the folder should be: nothing can be saved in it.
        let not_a_folder =
            std::env::temp_dir().join(format!("lillignode-app-no-save-{}", std::process::id()));
        std::fs::write(&not_a_folder, "x").unwrap();
        let mut app = App::launch(
            Fake::default(),
            Size { rows: 30, cols: 120 },
            Ok(not_a_folder.clone()),
            std::env::temp_dir(),
            Config::default(),
            None,
        );
        let failed = |app: &App<Fake>| {
            let said = app.notice_text().unwrap_or_default();
            assert!(said.starts_with("Could not save your workspace (could not create the folder"), "{said}");
            assert!(
                said.contains(
                    "The last saved workspace is unchanged; changes since then will not be restored."
                ),
                "{said}"
            );
        };
        failed(&app);
        app.notice = None;
        answer_form(&mut app, &["", "", ""]);
        assert_eq!(app.engine.ws.entries().len(), 1, "the agent started all the same");
        failed(&app);
        app.notice = None;
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('x')));
        app.on_key(press(KeyCode::Char('y')));
        assert!(app.engine.ws.entries().is_empty());
        failed(&app);
        // The same words on the way out, for the terminal.
        assert_eq!(app.save().err(), app.save_problem);
        let _ = std::fs::remove_file(&not_a_folder);
    }

    #[test]
    fn the_page_being_on_or_not_is_said_once_with_where_it_is_or_what_to_do() {
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let mut app = app_with_claude_agents(1);
        let word = "abcdefghijkmnpqr";
        app.notice = Some("Earlier notice.".into());
        assert!(!app.status_report_text().contains("The page"), "nothing said of a page not started");
        app.page_started(Ok(format!("http://127.0.0.1:7501/{word}/")));
        let said = app.notice.clone().unwrap();
        assert!(
            said.ends_with(&format!(
                "The page is on: http://127.0.0.1:7501/{word}/  Anyone with this address can see your agents. \
                 Ctrl+Q then s shows it again."
            )),
            "{said}"
        );
        assert!(said.starts_with("Earlier notice."), "what was up stays, before it");
        // Alone, it fits the two notice rows of the narrowest window whole,
        // with the longest address there is.
        app.notice = None;
        app.page_started(Ok(format!("http://127.0.0.1:65535/{word}/")));
        let said = app.notice.clone().unwrap();
        assert!(said.chars().count() <= room, "{} characters: {said}", said.chars().count());
        assert!(said.ends_with("Ctrl+Q then s shows it again."), "{said}");
        // Found by a review: started with a notice already up (an agent
        // that could not continue), the address and the way to see it
        // again were both cut from the two rows of a narrow window. Now
        // the way to see it again is kept, whatever was up before.
        let mut busy = app_with_claude_agents(1);
        busy.notice = Some(
            "Claude Code (api) could not continue its earlier conversation (there may have been nothing in \
             it yet), so it started a new conversation. If there was an earlier one, the agent's own resume \
             command may still find it."
                .into(),
        );
        busy.page_started(Ok(format!("http://127.0.0.1:65535/{word}/")));
        let fitted = busy.notice_fitted(room).unwrap();
        assert!(fitted.chars().count() <= room, "{fitted}");
        assert!(fitted.ends_with("Ctrl+Q then s shows it again."), "{fitted}");
        // And the address is never shown in part: whole, or not at all.
        assert!(!fitted.contains("127.0.0.1") || fitted.contains(&format!("/{word}/")), "{fitted}");
        // The status report has the address whole, at its foot, with or
        // without an agent open.
        let report = app.status_report_text();
        assert!(report.contains(&format!("\nThe page: http://127.0.0.1:65535/{word}/\n")), "{report}");
        assert!(
            report.trim_end().ends_with(
                "screen while Lillignode is open. The address stays the same from one start to the next. \
                 For a new address now: Ctrl+Q, then w."
            ),
            "{report}"
        );
        assert!(!report.contains("type"), "a page that is read only is not said to type: {report}");
        let mut empty = app_with_claude_agents(0);
        empty.page_started(Ok(format!("http://127.0.0.1:7501/{word}/")));
        let report = empty.status_report_text();
        assert!(report.starts_with("No agent is open.\n"), "{report}");
        assert!(report.contains(&format!("The page: http://127.0.0.1:7501/{word}/")), "{report}");

        // Not started: the port, the system's reason, that agents are not
        // affected, and what to do, which is kept however long the reason.
        app.notice = None;
        app.config.page_port = 65535;
        let reason = "Only one usage of each socket address (protocol/network address/port) is normally \
                      permitted. (os error 10048)";
        app.page_started(Err(reason.into()));
        let said = app.notice.clone().unwrap();
        assert!(said.starts_with("The page was not started on port 65535 (Only one usage"), "{said}");
        assert!(said.contains("Agents work as before."), "{said}");
        let fitted = app.notice_fitted(room).unwrap();
        assert!(fitted.chars().count() <= room, "{fitted}");
        assert!(fitted.ends_with("or take \"page\" out."), "{fitted}");
    }

    #[test]
    fn a_page_that_can_type_is_said_to_wherever_its_address_is_shown() {
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let mut app = app_with_claude_agents(1);
        let word = "abcdefghijkmnpqr";
        app.page_types = true;
        app.notice = None;
        app.page_started(Ok(format!("http://127.0.0.1:65535/{word}/")));
        let said = app.notice.clone().unwrap();
        assert_eq!(
            said,
            format!(
                "The page is on: http://127.0.0.1:65535/{word}/  Anyone with this address can see your \
                 agents and type to them. Ctrl+Q then s shows it again."
            )
        );
        // Whole, in the two notice rows of the narrowest window, with the
        // longest address there is.
        assert!(said.chars().count() <= room, "{} characters: {said}", said.chars().count());
        let report = app.status_report_text();
        assert!(
            report.contains(
                "They can also type to any agent from it: \"page_can_type\" is on in your settings file. \
                 While it can type, the address is new each time Lillignode starts. For a new address now: \
                 Ctrl+Q, then w."
            ),
            "{report}"
        );
    }

    /// A page being served for a test, on any free port, its word kept in
    /// a folder of the test's own.
    fn page_in(name: &str) -> (page::Served, std::path::PathBuf) {
        let folder = std::env::temp_dir().join(format!("lillignode-app-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let (_publisher, reader) = crate::snapshot::channel();
        (page::serve(reader, 0, Some(&folder), None).unwrap(), folder)
    }

    #[test]
    fn w_in_the_menu_gives_the_page_a_new_address_and_says_so() {
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let menu = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        // No page: said, and nothing changed.
        let mut app = app_with_claude_agents(1);
        app.on_key(menu);
        app.on_key(press(KeyCode::Char('w')));
        assert_eq!(
            app.notice.as_deref(),
            Some("The page is not on, so it has no address to change. Nothing was changed.")
        );

        let (served, folder) = page_in("w");
        let before = served.url();
        app.page_serving(served);
        assert!(app.notice.as_deref().unwrap().contains(&format!("The page is on: {before}")));
        // Asked first, keys first; anything but y leaves it as it was.
        for no in [press(KeyCode::Char('n')), press(KeyCode::Esc), press(KeyCode::Enter)] {
            app.on_key(menu);
            app.on_key(press(KeyCode::Char('w')));
            assert!(matches!(app.mode, Mode::Confirm(Confirm::NewAddress)));
            assert!(
                app.bar().starts_with(
                    "Give the page a new address? y / n   The address it has now stops working at once"
                ),
                "{}",
                app.bar()
            );
            app.on_key(no);
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(app.page_address.as_deref(), Some(before.as_str()), "left as it was");
            assert_eq!(app.notice.as_deref(), Some("The page's address was left as it was."));
        }
        let kept = std::fs::read_to_string(folder.join(page::WORD_FILE)).unwrap();
        assert!(before.ends_with(&format!("/{}/", kept.trim())), "the file too");
        let yes = |app: &mut App<Fake>| {
            app.on_key(menu);
            app.on_key(press(KeyCode::Char('w')));
            app.on_key(press(KeyCode::Char('y')));
        };
        yes(&mut app);
        let after = app.page_address.clone().unwrap();
        assert_ne!(after, before);
        assert_eq!(after.len(), before.len(), "the same address but for its word");
        let said = app.notice.clone().unwrap();
        assert_eq!(
            said,
            format!(
                "New page address: {after}  The old one has stopped working, for open pages too. Ctrl+Q then \
                 s shows it again."
            )
        );
        // It fits the two notice rows whole even at the longest address:
        // this one's port may be a digit or two shorter.
        assert!(said.chars().count() + 2 <= room, "{said}");
        assert!(matches!(app.mode, Mode::Normal), "back at the agent");
        // The status report has the new one, and the file keeps it.
        assert!(app.status_report_text().contains(&format!("The page: {after}\n")));
        let kept = std::fs::read_to_string(folder.join(page::WORD_FILE)).unwrap();
        assert!(after.ends_with(&format!("/{}/", kept.trim())), "{after} {kept}");

        // It cannot be kept: the new address is still the one in use, and
        // the notice says the next start will make another.
        std::fs::remove_dir_all(&folder).unwrap();
        std::fs::write(&folder, "a file where the folder was").unwrap();
        yes(&mut app);
        let third = app.page_address.clone().unwrap();
        assert_ne!(third, after);
        let said = app.notice.clone().unwrap();
        assert!(
            said.starts_with(&format!("New page address: {third}  The old one has stopped working")),
            "{said}"
        );
        assert!(said.contains("It could not be kept ("), "{said}");
        assert!(said.ends_with("so the next start makes another. Ctrl+Q then s shows it again."), "{said}");
        assert!(app.status_report_text().contains("The address could not be kept ("));
        let _ = std::fs::remove_file(&folder);
    }

    fn typing_app(agents: &[&str]) -> (App<Fake>, Rc<RefCell<Vec<String>>>, String) {
        let backend = Fake::default();
        let sent = backend.sent.clone();
        let config = Config { page: true, page_can_type: true, ..Config::default() };
        let mut app = App::launch(
            backend,
            Size { rows: 5, cols: 40 },
            Err("tests save nothing".into()),
            std::env::temp_dir(),
            config,
            None,
        );
        for name in agents {
            app.engine.ws.start_as_named(name, std::env::temp_dir().join("work")).unwrap();
        }
        app.notice = None;
        let run = app.engine.run().to_string();
        (app, sent, run)
    }

    #[test]
    fn text_from_the_page_goes_to_the_agent_named_then_enter_and_the_terminal_says_so() {
        let (mut app, sent, run) = typing_app(&["first", "second"]);
        let second = app.engine.ws.entries()[0].key().number();
        // The other agent is in front; the text is for this one.
        assert_eq!(app.engine.ws.focused().unwrap().label(), "second (work)");
        let (outcome, words) = app.typed_from_page(&run, second, "run the tests\r\n", false);
        assert_eq!((outcome, words.as_str()), (TypedOutcome::Sent, "Sent to first (work), then Enter."));
        assert_eq!(*sent.borrow(), ["run the tests", "\r"], "as typed, then Enter");
        assert_eq!(app.notice.as_deref(), Some("The page typed to first (work), then Enter."));
        // What a paste has taken out of it is taken out here, and said.
        sent.borrow_mut().clear();
        let (outcome, words) = app.typed_from_page(&run, second, "a\tb\x1b[31m\x03", false);
        assert_eq!(outcome, TypedOutcome::Sent);
        assert_eq!(
            words,
            "Sent to first (work), then Enter. 2 control characters were left out (Esc, Ctrl+C). Its tabs \
             were sent as spaces, so none was taken as a press of Tab."
        );
        assert_eq!(*sent.borrow(), ["a    b[31m", "\r"], "no escape reaches the agent");
        // An agent that takes a paste whole: text that holds the mark that
        // ends a paste cannot end it early, because its Escape is gone.
        app.on_agent_event(AgentEvent::Output(AgentId(1), b"\x1b[?2004h".to_vec()));
        sent.borrow_mut().clear();
        let (outcome, _) = app.typed_from_page(&run, second, "safe\x1b[201~rm -rf x\nmore", false);
        assert_eq!(outcome, TypedOutcome::Sent);
        assert_eq!(*sent.borrow(), ["\x1b[200~safe[201~rm -rf x\rmore\x1b[201~", "\r"], "one paste, whole");
    }

    /// An app whose settings let the page act, saving in a folder of the
    /// test's own that holds the stand-in Board.
    fn acting_app(name: &str) -> (App<Fake>, PathBuf) {
        let folder = std::env::temp_dir().join(format!("lillignode-app-act-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(board::FILE), include_str!("../examples/board-standin.json")).unwrap();
        let config = Config { page: true, page_can_act: true, ..Config::default() };
        let mut app = App::launch(
            Fake::default(),
            Size { rows: 5, cols: 40 },
            Ok(folder.clone()),
            std::env::temp_dir(),
            config,
            None,
        );
        app.notice = None;
        (app, folder)
    }

    /// A card as the page would have shown it: as the Board's file has it.
    fn seen_card(folder: &std::path::Path, id: &str) -> board::Seen {
        let shown = board::read(Some(folder));
        shown
            .decide
            .iter()
            .chain(&shown.yours)
            .chain(&shown.know)
            .find(|c| c.id == id)
            .map(|c| board::Seen { title: c.title.clone(), recommendation: c.recommendation.clone() })
            .unwrap_or_default()
    }

    fn outbox_lines(folder: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(folder.join(board::OUTBOX))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn acting_on_a_card_from_the_page_writes_the_outbox_and_is_said_in_the_terminal() {
        let (mut app, folder) = acting_app("act");
        let board_before = std::fs::read(folder.join(board::FILE)).unwrap();
        let (outcome, words) = app.acted_from_page("c1", "approve", "", &seen_card(&folder, "c1"), false);
        assert_eq!(
            (outcome, words.as_str()),
            (TypedOutcome::Sent, "Sent: Approve. It waits until the Board no longer lists the card.")
        );
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "From the page's Board: Approve, for \"Keep the old sign-in endpoint for one m…\". It waits in \
                 the Board's outbox until it is picked up."
            )
        );
        let lines = outbox_lines(&folder);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (&lines[0]["card"], &lines[0]["action"], &lines[0]["note"]),
            (&"c1".into(), &"approve".into(), &"".into())
        );
        assert!(board::parse_time(lines[0]["at"].as_str().unwrap()).is_some(), "with its time");
        assert_eq!(
            std::fs::read(folder.join(board::FILE)).unwrap(),
            board_before,
            "the Board's file is not written"
        );
        // An engine event each time, naming the card.
        let said = app.engine.events_after(0).events;
        let acted: Vec<_> =
            said.iter().map(|(n, e)| e.plain(*n)).filter(|p| p.what == "card-acted").collect();
        assert_eq!(acted.len(), 1);
        assert_eq!(acted[0].label.as_deref(), Some("c1"));
        // Undone within ten seconds: the line is out, and that is said too.
        let (outcome, words) = app.acted_from_page("c1", "approve", "", &board::Seen::default(), true);
        assert_eq!(
            (outcome, words.as_str()),
            (TypedOutcome::Sent, "Undone: Approve. Nothing is waiting for that card.")
        );
        assert!(outbox_lines(&folder).is_empty());
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "From the page's Board: Approve was undone for \"Keep the old sign-in endpoint for one m…\". \
                 Its line was taken out of the outbox."
            )
        );
        let undone = app
            .engine
            .events_after(0)
            .events
            .iter()
            .filter(|(n, e)| e.plain(*n).what == "card-action-undone")
            .count();
        assert_eq!(undone, 1);
        // A Revise carries its note.
        let (outcome, _) =
            app.acted_from_page("c1", "revise", "Say when it goes.", &seen_card(&folder, "c1"), false);
        assert_eq!(outcome, TypedOutcome::Sent);
        assert_eq!(outbox_lines(&folder)[0]["note"], "Say when it goes.");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn acting_on_a_card_is_refused_with_why_and_then_nothing_is_written_or_said() {
        let (mut app, folder) = acting_app("refused");
        let refused = |app: &mut App<Fake>, card: &str, action: &str, note: &str, undo: bool| {
            let (outcome, words) = app.acted_from_page(card, action, note, &seen_card(&folder, card), undo);
            assert_eq!(outcome, TypedOutcome::Refused, "{words}");
            words
        };
        assert_eq!(
            refused(&mut app, "nobody", "approve", "", false),
            "That card is no longer open on the Board. Nothing was sent."
        );
        assert_eq!(
            refused(&mut app, "c5", "approve", "", false),
            "That cannot be done to this card. Nothing was sent."
        );
        assert_eq!(
            refused(&mut app, "c1", "revise", "", false),
            "Revise needs a short note saying what to change. Nothing was sent."
        );
        assert_eq!(
            refused(&mut app, "c1", "approve", "", true),
            "There is nothing to undo for that card. Nothing was changed."
        );
        assert!(outbox_lines(&folder).is_empty());
        assert_eq!(app.notice, None, "nothing happened, so nothing is said");
        assert!(app
            .engine
            .events_after(0)
            .events
            .iter()
            .all(|(n, e)| !e.plain(*n).what.starts_with("card-")));
        // The card was rewritten under its id after the page showed it.
        let shown = seen_card(&folder, "c1");
        let rewritten = include_str!("../examples/board-standin.json").replace(
            "Keep the old sign-in endpoint for one more release?",
            "Remove the old sign-in endpoint today?",
        );
        std::fs::write(folder.join(board::FILE), rewritten).unwrap();
        let (outcome, words) = app.acted_from_page("c1", "approve", "", &shown, false);
        assert_eq!(
            (outcome, words.as_str()),
            (
                TypedOutcome::Refused,
                "That card has changed since the page showed it. Read it again, then decide. Nothing was sent."
            )
        );
        assert!(outbox_lines(&folder).is_empty());
        // Off in the settings: nothing, whatever is asked.
        app.config.page_can_act = false;
        assert_eq!(
            refused(&mut app, "c1", "approve", "", false),
            "This page cannot act on the Board. Nothing was sent."
        );
        assert!(outbox_lines(&folder).is_empty());
        // Nowhere to save: said.
        let (mut nowhere, _, _) = typing_app(&[]);
        nowhere.config.page_can_act = true;
        assert!(
            refused(&mut nowhere, "c1", "approve", "", false).starts_with("Lillignode has nowhere to save")
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_cards_title_in_a_notice_is_only_what_can_be_seen_and_never_long() {
        assert_eq!(shown_title("Keep it?"), "Keep it?");
        // Nothing that draws no character, clears the screen or turns the
        // text after it around.
        assert_eq!(shown_title("a\u{1b}[2J\u{200b}b\u{202e}c\u{7}\nd"), "a[2Jbcd");
        // A title of a great many characters that take no room is not
        // carried into a notice whole.
        assert_eq!(shown_title(&"\u{200b}".repeat(300_000)), "");
        assert_eq!(shown_title(&"x".repeat(5_000)).chars().count(), 60);
        // In the notice it is cut to forty columns, in quotes.
        let (mut app, folder) = acting_app("title");
        let long = include_str!("../examples/board-standin.json").replace(
            "Keep the old sign-in endpoint for one more release?",
            &format!("Fine\\u202e\\u200b. Its line was taken out{}", "!".repeat(400)),
        );
        std::fs::write(folder.join(board::FILE), long).unwrap();
        let (outcome, words) = app.acted_from_page("c1", "hold", "", &seen_card(&folder, "c1"), false);
        assert_eq!(outcome, TypedOutcome::Sent, "{words}");
        let said = app.notice.clone().unwrap();
        assert!(
            said.starts_with("From the page's Board: Hold, for \"Fine. Its line was taken out!!!"),
            "{said}"
        );
        assert!(said.ends_with("…\". It waits in the Board's outbox until it is picked up."), "{said}");
        assert!(said.chars().count() < 140, "{said}");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_page_that_can_act_is_said_to_wherever_its_address_is_shown() {
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let word = "abcdefghijkmnpqr";
        let address = format!("http://127.0.0.1:65535/{word}/");
        for (types, acts, who, lasts) in [
            (
                false,
                true,
                "Anyone with this address can see your agents and act on your Board.",
                "While it can act on the Board, the address is new each time Lillignode starts.",
            ),
            (
                true,
                true,
                "Anyone with it can see your agents, type to them and act on your Board.",
                "While it can type or act, the address is new each time Lillignode starts.",
            ),
        ] {
            let mut app = app_with_claude_agents(1);
            (app.page_types, app.page_acts) = (types, acts);
            app.notice = None;
            app.page_started(Ok(address.clone()));
            let said = app.notice.clone().unwrap();
            assert_eq!(said, format!("The page is on: {address}  {who} Ctrl+Q then s shows it again."));
            assert!(said.chars().count() <= room, "{} characters: {said}", said.chars().count());
            let report = app.status_report_text();
            assert!(
                report.contains(
                    "They can also act on the cards of your Board from it: \"page_can_act\" is on in your \
                     settings file."
                ),
                "{report}"
            );
            assert!(report.contains(lasts), "{report}");
        }
    }

    #[test]
    fn enter_from_the_page_goes_to_the_agent_named_alone_and_the_terminal_says_so() {
        let (mut app, sent, run) = typing_app(&["first", "second"]);
        let first = app.engine.ws.entries()[0].key().number();
        // The other agent is in front.
        assert_eq!(app.engine.ws.focused().unwrap().label(), "second (work)");
        let (outcome, words) = app.enter_from_page(&run, first);
        assert_eq!((outcome, words.as_str()), (TypedOutcome::Sent, "Pressed Enter in first (work)."));
        assert_eq!(*sent.borrow(), ["\r"], "Enter and nothing else");
        assert_eq!(app.notice.as_deref(), Some("The page pressed Enter in first (work)."));
        // Refused in the terminal's words.
        assert_eq!(
            app.enter_from_page(&run, 999).1,
            "That agent is no longer in your workspace. Nothing was changed."
        );
        assert!(app.enter_from_page("12345", first).1.starts_with("Lillignode was started again"));
        app.on_agent_event(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        assert_eq!(
            app.enter_from_page(&run, first),
            (TypedOutcome::Refused, "first (work) is not running, so nothing was sent to it.".to_string())
        );
        app.config.page_can_type = false;
        let second = app.engine.ws.entries()[1].key().number();
        assert_eq!(app.enter_from_page(&run, second).1, "This page is read only. Nothing was sent.");
        assert_eq!(sent.borrow().len(), 1, "nothing more was sent");
    }

    #[test]
    fn several_lines_from_the_page_ask_first_in_the_terminals_words() {
        let (mut app, sent, run) = typing_app(&["first"]);
        let first = app.engine.ws.entries()[0].key().number();
        let (outcome, words) = app.typed_from_page(&run, first, "one\ntwo\nthree\n\n", false);
        assert_eq!(outcome, TypedOutcome::Ask);
        assert!(sent.borrow().is_empty(), "nothing is sent before the reader agrees");
        assert!(matches!(app.mode, Mode::Normal), "the terminal is not asked: the page asks its reader");
        assert_eq!(app.notice, None);
        // The same words the terminal asks with, but for its keys.
        app.on_paste(Pasted::from("one\ntwo\nthree"));
        let asked = app.bar();
        app.on_key(press(KeyCode::Char('n')));
        assert_eq!(asked.replace(" y / n  ", ""), words, "the terminal's own question");
        assert!(words.starts_with("Paste 3 lines into first (work) one line at a time? It does not take"));
        // Agreed: line by line, then Enter.
        sent.borrow_mut().clear();
        let (outcome, words) = app.typed_from_page(&run, first, "one\ntwo\nthree", true);
        assert_eq!(
            (outcome, words.as_str()),
            (TypedOutcome::Sent, "Sent to first (work) line by line, then Enter.")
        );
        assert_eq!(*sent.borrow(), ["one\rtwo\rthree", "\r"]);
    }

    #[test]
    fn text_from_the_page_is_refused_in_the_terminals_words_and_nothing_is_sent() {
        let (mut app, sent, run) = typing_app(&["first", "second"]);
        let (first, second) =
            (app.engine.ws.entries()[0].key().number(), app.engine.ws.entries()[1].key().number());
        let refused = |app: &mut App<Fake>, run: &str, agent: u64, text: &str| {
            let (outcome, words) = app.typed_from_page(run, agent, text, true);
            assert_eq!(outcome, TypedOutcome::Refused, "{words}");
            words
        };
        // An agent that is not there.
        assert_eq!(
            refused(&mut app, &run, 999, "x"),
            "That agent is no longer in your workspace. Nothing was changed."
        );
        // A key from another run of Lillignode names another agent.
        let said = refused(&mut app, "12345", first, "x");
        assert!(
            said.starts_with("Lillignode was started again") && said.contains("Nothing was sent."),
            "{said}"
        );
        // Nothing to send, and only control characters.
        assert!(refused(&mut app, &run, first, "\n\n").starts_with("There is nothing to send"));
        assert_eq!(
            refused(&mut app, &run, first, "\x03"),
            "Nothing was pasted: the text was only control characters (Ctrl+C). Nothing was sent."
        );
        // A question open in the terminal is answered first.
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        app.on_key(press(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::Confirm(_)));
        assert_eq!(
            refused(&mut app, &run, second, "x"),
            "Answer the question on the bottom line first. Nothing was changed."
        );
        app.on_key(press(KeyCode::Char('n')));
        // A stopped agent: the words a paste into it gets.
        app.on_agent_event(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 0 }));
        assert_eq!(
            refused(&mut app, &run, second, "x"),
            "second (work) is not running, so nothing was sent to it."
        );
        assert!(sent.borrow().is_empty(), "to nobody: not to it, and not to the agent in front");
        // Off in the settings: nothing, whatever is asked, even if a page
        // were somehow handed the way in.
        app.config.page_can_type = false;
        assert_eq!(refused(&mut app, &run, first, "x"), "This page is read only. Nothing was sent.");
        assert!(sent.borrow().is_empty());
    }

    /// Found by a review: an agent that could not continue its conversation
    /// was given a new one, the workspace could not be saved with it, and
    /// the sentence about the conversation replaced the one about the save
    /// in the same instant. The save failure was never seen.
    #[test]
    fn a_save_failure_goes_first_and_stays_until_a_menu_key_or_a_save_that_works() {
        use crate::state::{SavedAgent, SavedWorkspace};
        let not_a_folder =
            std::env::temp_dir().join(format!("lillignode-app-save-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&not_a_folder);
        std::fs::write(&not_a_folder, "x").unwrap();
        let mut app = App::launch(
            Fake::default(),
            Size { rows: 30, cols: 120 },
            Ok(not_a_folder.clone()),
            std::env::temp_dir(),
            Config::default(),
            None,
        );
        let claude = SavedAgent {
            name: "Claude Code".into(),
            command: "claude".into(),
            kind: Some("claude-code".into()),
            args: vec![],
            folder: std::env::temp_dir(),
            conversation: Some("11111111-2222-4333-8444-555555555555".into()),
            other: Default::default(),
        };
        let _ = app.engine.restore(SavedWorkspace { agents: vec![claude], ..Default::default() });
        app.after_events();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.save_problem, None, "a menu key: it has been seen");

        // It cannot continue, starts a new conversation, and that cannot be
        // saved: both are said, the save first.
        let id = app.engine.ws.entries().len() as u32;
        app.on_agent_event(AgentEvent::Exited(AgentId(id), crate::agent::Exit { code: 1 }));
        let said = app.notice_text().unwrap();
        let save = "Could not save your workspace (";
        let conversation = "could not continue its earlier conversation";
        assert!(said.starts_with(save), "{said}");
        assert!(said.contains(conversation), "{said}");
        assert!(said.find(save) < said.find(conversation), "{said}");

        // Whatever is said next, it stays, and stays first.
        app.notice = Some("Something else happened.".into());
        let said = app.notice_text().unwrap();
        assert!(said.starts_with(save) && said.ends_with("Something else happened."), "{said}");
        // In two rows of the narrowest window its start is kept: what could
        // not be done.
        let narrow = app.notice_fitted(2 * usize::from(crate::ui::MIN_COLS)).unwrap();
        assert!(narrow.starts_with(save), "{narrow}");

        // A key that is not a menu action leaves it.
        app.on_key(press(KeyCode::Char('a')));
        assert!(app.notice_text().unwrap().starts_with(save));
        // The next menu action: seen, so it goes. The other notice stays.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Tab));
        assert!(!app.notice_text().unwrap_or_default().contains(save));

        // It fails again at the next change, and is said again.
        app.on_key(press(KeyCode::Esc));
        answer_form(&mut app, &["", "", ""]);
        assert!(app.notice_text().unwrap().starts_with(save));
        // Once a save works, it is no longer true and no longer said.
        std::fs::remove_file(&not_a_folder).unwrap();
        answer_form(&mut app, &["", "", ""]);
        assert_eq!(app.save_problem, None);
        assert!(!app.notice_text().unwrap_or_default().contains(save));
        let _ = std::fs::remove_dir_all(&not_a_folder);
    }

    #[test]
    fn options_are_asked_for_after_the_program_and_shown_back_before_anything_starts() {
        let mut app = app_with_claude_agents(0);
        answer_form(&mut app, &["codex"]);
        assert!(
            app.bar().starts_with(
                "Options for codex as Codex:    (Enter alone: none   Tab: start it as a plain program"
            ),
            "{}",
            app.bar()
        );
        assert_eq!(app.bar_cursor(), Some("Options for codex as Codex: ".chars().count()));
        for c in r#"-a on-request -c "x y""#.chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        let bar = app.bar();
        assert!(
            bar.starts_with(
                "Start it as Codex? y / n / k   As Codex: a new conversation each time. Options \
                 [-a][on-request][-c][x y]. n: change it, k: another kind. Program: codex. Folder: "
            ),
            "{bar}"
        );
        assert_eq!(app.bar_cursor(), None);
        assert!(app.engine.ws.entries().is_empty(), "nothing starts before y");
        app.on_key(press(KeyCode::Char('y')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.engine.ws.entries()[0].saved.args, ["-a", "on-request", "-c", "x y"]);
        assert_eq!(app.engine.ws.entries()[0].kind(), AgentKind::Codex);
    }

    #[test]
    fn the_kind_can_be_changed_before_it_starts_and_n_goes_back_with_nothing_lost() {
        let mut app = app_with_claude_agents(0);
        answer_form(&mut app, &["claude"]);
        // Tab at the options: another kind, said on the same line.
        app.on_key(press(KeyCode::Tab));
        assert!(app.bar().starts_with("Options for claude as Codex: "), "{}", app.bar());
        app.on_key(press(KeyCode::Tab));
        assert!(app.bar().starts_with("Options for claude as a plain program: "), "{}", app.bar());
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        // No options, but not the kind the name suggests: asked.
        let bar = app.bar();
        assert!(
            bar.starts_with(
                "Start it as a plain program? y / n / k   As a plain program: status only if it reports one. \
                 No options. n: change it, k: another kind. Program: claude. Folder: "
            ),
            "{bar}"
        );
        assert!(bar.contains("As a plain program: status only if it reports one."), "{bar}");
        app.on_key(press(KeyCode::Char('k')));
        assert!(app.bar().starts_with("Start it as Claude Code? y / n / k"), "{}", app.bar());
        assert!(app.bar().contains("As Claude Code: it continues its conversation next time."));
        app.on_key(press(KeyCode::Char('k')));
        app.on_key(press(KeyCode::Char('k')));
        // n: back to the start, with the program as typed.
        app.on_key(press(KeyCode::Char('n')));
        assert!(app.bar().starts_with("Agent to start: claude   (Enter alone"), "{}", app.bar());
        app.on_key(press(KeyCode::Enter));
        assert!(app.bar().starts_with("Options for claude as a plain program: "), "{}", app.bar());
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('y')));
        let e = &app.engine.ws.entries()[0];
        assert_eq!(e.kind(), AgentKind::Program);
        assert_eq!(e.saved.conversation, None, "not given a conversation");
    }

    #[test]
    fn a_kind_chosen_for_one_program_does_not_carry_over_to_another() {
        let mut app = app_with_claude_agents(0);
        answer_form(&mut app, &["claude"]);
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Char('n')));
        for _ in 0.."claude".len() {
            app.on_key(press(KeyCode::Backspace));
        }
        for c in "aider".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        app.on_key(press(KeyCode::Enter));
        assert!(app.bar().starts_with("Options for aider as a plain program: "), "{}", app.bar());
    }

    #[test]
    fn options_that_would_give_claude_code_a_second_conversation_cannot_be_started() {
        let mut app = app_with_claude_agents(0);
        answer_form(&mut app, &["claude", "--resume abc", ""]);
        let bar = app.bar();
        assert_eq!(
            bar,
            "Cannot start it as typed. n: change it, k: another kind, Esc: cancel   Take out --resume: \
             Lillignode sets the conversation itself. Or start it as a plain program."
        );
        app.on_key(press(KeyCode::Char('y')));
        assert!(app.engine.ws.entries().is_empty(), "y does nothing while it cannot start");
        // As a plain program it can.
        app.on_key(press(KeyCode::Char('k')));
        app.on_key(press(KeyCode::Char('k')));
        assert!(app.bar().starts_with("Start it as a plain program? y / n / k"), "{}", app.bar());
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.notice.as_deref(), Some("Cancelled. Nothing was started."));
        assert!(app.engine.ws.entries().is_empty());
    }

    #[test]
    fn a_refusal_keeps_what_to_take_out_and_why_in_a_narrow_window() {
        let reason = "Take out --resume: Lillignode sets the conversation itself";
        for width in [60, 95, 120, 150, 200] {
            let mut app = app_with_claude_agents(0);
            app.set_window(Rect::new(0, 0, width, 30));
            answer_form(&mut app, &["claude", "--resume abc", ""]);
            let bar = app.bar();
            assert!(bar.contains(reason), "{width}: {bar}");
            // The keys come first at every width.
            assert!(
                bar.starts_with("Cannot start it as typed. n")
                    || bar.starts_with("n change, k kind, Esc cancel   ")
                    || bar.starts_with("n / k / Esc   "),
                "{bar}"
            );
            if width >= 80 {
                assert!(bar.chars().count() <= width as usize, "{width}: {bar}");
            }
            // What the keys do goes only after the other way out has.
            if width >= 130 {
                assert!(bar.contains("n: change it, k: another kind, Esc: cancel"), "{bar}");
            }
            if width >= 200 {
                assert!(bar.ends_with("Or start it as a plain program."), "{bar}");
            }
        }
    }

    #[test]
    fn a_refusal_names_what_its_keys_do_in_a_word_each_wherever_that_fits() {
        let reason = "Take out --resume: Lillignode sets the conversation itself.";
        let at = |width, options: &str| {
            let mut app = app_with_claude_agents(0);
            app.set_window(Rect::new(0, 0, width, 30));
            answer_form(&mut app, &["claude", options, ""]);
            app.bar()
        };
        let long = "Cannot start it as typed. n: change it, k: another kind, Esc: cancel   ";
        assert_eq!(at(130, "--resume abc"), format!("{long}{reason}"));
        // One column less: a word for each key, and the opening words stay.
        let worded = "n change, k kind, Esc cancel   ";
        assert_eq!(at(129, "--resume abc"), format!("Cannot start it as typed. {worded}{reason}"));
        assert_eq!(at(116, "--resume abc"), format!("Cannot start it as typed. {worded}{reason}"));
        // Then the opening words go, and the keys keep their words.
        assert_eq!(at(115, "--resume abc"), format!("{worded}{reason}"));
        // At 95 columns, where a check by hand found bare keys, the keys
        // keep their words, for the longest option that is refused too, and
        // however long a value is joined to it.
        assert_eq!(at(95, "--resume abc"), format!("{worded}{reason}"));
        let longest = "Take out --session-id: Lillignode sets the conversation itself.";
        assert_eq!(at(95, "--session-id abc"), format!("{worded}{longest}"));
        assert_eq!(
            at(95, "--session-id=0123456789012345678901234567890123456789"),
            format!("{worded}{longest}")
        );
        assert_eq!(at(90, "--resume abc"), format!("{worded}{reason}"));
        // Bare keys only when even a word each does not fit.
        assert_eq!(at(89, "--resume abc"), format!("n / k / Esc   {reason}"));
        assert_eq!(at(80, "--resume abc"), format!("n / k / Esc   {reason}"));
    }

    /// Width is what a line takes on screen, not how many characters it
    /// has: a letter of Chinese, Japanese or Korean takes two columns. A
    /// form counted as fitting in characters ran past the edge and lost its
    /// end.
    #[test]
    fn a_line_with_double_width_letters_is_fitted_by_the_columns_it_takes() {
        // A launcher refuses an option that holds "&"; the reason names
        // the option as typed. Here the folder typed is in Japanese.
        let tries =
            refusal_tries("Take out 設定ファイル: Lillignode sets the conversation itself. Or start it");
        let mut app = app_with_claude_agents(0);
        for width in 60..=170u16 {
            app.set_window(Rect::new(0, 0, width, 30));
            let bar = app.fitted(&tries);
            let fits = fit::width(&bar) <= width as usize;
            // The last form is the shortest there is; every other form
            // shown fits on screen.
            assert!(fits || bar == tries[4], "{width}: {bar}");
            // And the longest that fits is the one shown.
            let longest = tries.iter().find(|t| fit::width(t) <= width as usize).unwrap_or(&tries[4]);
            assert_eq!(&bar, longest, "{width}");
        }
        // Counted in characters, the worded form would be taken to fit
        // here; it is six columns wider than that.
        let worded = &tries[3];
        assert_eq!(fit::width(worded), worded.chars().count() + 6);
        app.set_window(Rect::new(0, 0, worded.chars().count() as u16 + 2, 30));
        assert_eq!(app.fitted(&tries), tries[4], "the worded form does not fit: bare keys");

        // What is cut to fit is cut by columns too, and never through a letter.
        assert_eq!(cut_end("設定ファイルを直す", 9), "設定ファ…");
        let notice = "設定ファイルが壊れています. 壊れています. Then press r.";
        for room in 15..fit::width(notice) {
            let shown = keep_end(notice, room);
            assert!(fit::width(&shown) <= room, "{room}: {shown}");
            assert!(shown.ends_with("Then press r."), "{room}: {shown}");
        }
        // The typing cursor is where the text ends on screen.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('n')));
        for c in "設定".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        assert_eq!(app.bar_cursor(), Some(fit::width(AGENT_PROMPT) + 4));
    }

    #[test]
    fn an_open_quote_is_said_on_the_last_line() {
        let mut app = app_with_claude_agents(0);
        answer_form(&mut app, &["codex", r#"-m "two words"#, ""]);
        let bar = app.bar();
        assert!(
            bar.contains(r#"Options [-m]["two words]. There is a " with no partner; it was kept as part of the last option. n:"#),
            "{bar}"
        );
    }

    #[test]
    fn a_start_refused_for_another_reason_than_the_folder_does_not_send_you_to_the_folder() {
        let mut app = app_with_claude_agents(0);
        for i in 0..crate::state::MOST_AGENTS {
            app.engine.ws.start_as_named("cmd", std::env::temp_dir().join(format!("f{i}"))).unwrap();
        }
        answer_form(&mut app, &["cmd", "", ""]);
        let notice = app.notice.clone().unwrap();
        assert!(notice.contains("keeps at most 64 agents"), "{notice}");
        assert!(!notice.contains("Correct the folder"), "{notice}");
        assert!(app.bar().starts_with("Start it as a plain program? y / n / k"), "{}", app.bar());
    }

    #[test]
    fn an_agent_started_as_a_kind_its_name_does_not_suggest_says_so_when_it_fails() {
        let mut app = app_with_claude_agents(0);
        app.engine.ws.start_new("wrapper", vec![], AgentKind::ClaudeCode, std::env::temp_dir()).unwrap();
        app.on_agent_event(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 2 }));
        let notice = app.notice.clone().unwrap();
        assert!(
            notice.ends_with(
                "It was started as Claude Code: if it is not Claude Code, x removes it, then start it again as \
                 a plain program."
            ),
            "{notice}"
        );
        // Started as what its name says, nothing more is said.
        app.engine.ws.start_as_named("claude", std::env::temp_dir()).unwrap();
        app.on_agent_event(AgentEvent::Exited(AgentId(2), crate::agent::Exit { code: 2 }));
        assert!(app.notice.clone().unwrap().ends_with("x to remove it."), "{:?}", app.notice);
    }

    #[test]
    fn typing_at_a_stopped_agent_goes_nowhere_and_never_removes_or_quits() {
        let mut app = app_with_claude_agents(2);
        report(&mut app, 2, "state=idle:app=claude-code");
        let front = app.engine.ws.focused_index().unwrap() as u32 + 1;
        app.on_agent_event(AgentEvent::Exited(AgentId(front), crate::agent::Exit { code: 0 }));
        app.notice = None;
        // Shell habits: "exit", "quit", Enter.
        for c in "exit quit".chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.engine.ws.entries().len(), 2, "nothing was removed");
        assert!(!app.engine.closing(), "nothing quit");
        assert!(matches!(app.mode, Mode::Normal), "no question was opened");
        let notice = app.notice.clone().unwrap();
        // One line: no line break and no run of spaces inside it.
        assert_eq!(
            notice,
            "Claude Code (a) has stopped, so what you typed was not sent. Ctrl+Q then r starts it again, \
             or Ctrl+Q then x removes it."
                .replace("Claude Code (a)", &app.focused_label())
        );
        assert!(app.bar().starts_with("Ctrl+Q or F12: menu, then r start again"), "{}", app.bar());

        // Removing it is asked, even though it has stopped.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('x')));
        assert!(app.bar().starts_with("Remove "), "{}", app.bar());
        // The sentence is one line, with no run of spaces
        // inside it but the gap after the keys.
        let bar = app.bar();
        assert!(!bar.contains('\n'), "{bar:?}");
        let (keys, rest) = bar.split_once("? y / n   ").expect("keys first");
        assert!(!keys.contains("  ") && !rest.contains("  "), "{bar:?}");
        assert!(rest.ends_with("out of your workspace."), "{bar:?}");
        assert!(app.bar().contains("? y / n"), "{}", app.bar());
        app.on_key(press(KeyCode::Char('n')));
        assert_eq!(app.engine.ws.entries().len(), 2);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('x')));
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(app.engine.ws.entries().len(), 1);
    }

    #[test]
    fn no_notice_says_where_typing_goes_so_none_goes_stale_after_tab() {
        // Found in the hand check: after Tab to the right, the notice still
        // said typing went to the left. Where typing goes is for the
        // headings and the bar, which follow every change.
        let mut app = app_with_claude_agents(3);
        app.set_window(WIDE);
        let says_where = |app: &App<Fake>| {
            app.notice_text().unwrap_or_default().to_lowercase().contains("typing goes")
                || app.notice_text().unwrap_or_default().contains(", on the ")
        };
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(app.two_shown());
        assert!(!says_where(&app), "{:?}", app.notice_text());
        assert!(app.bar().ends_with(", on the left."), "{}", app.bar());

        // Tab to the right: the bar follows, and no notice is left saying left.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Esc));
        assert!(app.bar().ends_with(", on the right."), "{}", app.bar());
        assert!(!says_where(&app), "{:?}", app.notice_text());

        // Narrowed and widened again: the notice says two are back, not where typing goes.
        app.set_window(Rect::new(0, 0, 100, 30));
        app.set_window(WIDE);
        assert_eq!(app.notice.as_deref(), Some("There is room again: two agents are side by side."));
        assert!(app.bar().ends_with(", on the right."), "{}", app.bar());
    }

    #[test]
    fn a_refusal_goes_at_the_next_menu_action_and_stops_crowding_who_needs_you() {
        let mut app = app_with_claude_agents(2);
        app.set_window(Rect::new(0, 0, 108, 30));
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(app.notice.clone().unwrap().starts_with("This window is too narrow"));
        let behind = 1 - app.engine.ws.focused_index().unwrap() as u32;
        report(&mut app, behind + 1, PERMISSION);
        assert!(app.notice_text().unwrap().contains("This window is too narrow"), "still up until a key");

        // The next menu action takes the refusal away; who needs you stays.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.notice, None);
        let shown = app.notice_text().unwrap();
        assert!(shown.contains("needs you: permission"), "{shown}");
        assert!(!shown.contains("too narrow"), "{shown}");

        // A notice that replaced the refusal is not taken away with it.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        app.notice = Some("Could not save your workspace (disk full).".into());
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.notice.as_deref(), Some("Could not save your workspace (disk full)."));
    }

    #[test]
    fn two_side_by_side_is_refused_with_the_reason_when_it_cannot_be_shown() {
        // One agent.
        let (mut app, _) = app_with_agent();
        app.set_window(WIDE);
        let size = app.engine.ws.size();
        app.on_key(press(KeyCode::F(12)));
        assert!(!app.bar().contains("v "), "the key is not offered where it would be refused: {}", app.bar());
        app.on_key(press(KeyCode::Char('v')));
        let notice = app.notice.clone().unwrap();
        assert!(notice.starts_with("Two agents side by side takes two agents, and one is open."), "{notice}");
        assert!(notice.contains("Nothing was changed."), "{notice}");
        assert!(notice.ends_with("Ctrl+Q then n starts another."), "{notice}");
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, size));

        // Two agents, a window too narrow.
        let mut app = app_with_claude_agents(2);
        app.set_window(Rect::new(0, 0, 108, 30));
        let size = app.engine.ws.size();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        let notice = app.notice.clone().unwrap();
        assert!(
            notice.starts_with(
                "This window is too narrow for two agents side by side: it is 108 columns wide and that \
                 takes 109."
            ),
            "{notice}"
        );
        assert!(notice.ends_with("Widen the window, then Ctrl+Q then v."), "{notice}");
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, size));
        assert!(matches!(app.mode, Mode::Normal));

        // A window too small to show agents at all.
        app.set_window(Rect::new(0, 0, 60, 10));
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(app.notice.clone().unwrap().starts_with("This window is too small to show agents at all"));
        assert_eq!(app.engine.ws.pair(), None);
    }

    #[test]
    fn v_puts_two_side_by_side_each_sized_to_its_half_and_v_again_goes_back() {
        let mut app = app_with_claude_agents(3);
        app.set_window(WIDE);
        let whole = app.engine.ws.size();
        assert_eq!(whole, Size { rows: 27, cols: 102 });
        app.on_key(press(KeyCode::F(12)));
        assert!(app.bar().ends_with("v two side by side"), "{}", app.bar());
        app.on_key(press(KeyCode::Char('v')));
        // The agent in front (c) on the left, the next (a) beside it.
        assert_eq!(app.engine.ws.pair(), Some(crate::workspace::Pair { left: 2, right: 0 }));
        // Half the width less the rule between them, and a row for the heading.
        assert_eq!(app.engine.ws.size(), Size { rows: 26, cols: 50 });
        for entry in app.engine.ws.entries() {
            let (rows, cols) = entry.term.screen().size();
            assert_eq!((rows.get(), cols.get()), (26, 50), "every agent's terminal is the new size");
        }
        let notice = app.notice.clone().unwrap();
        assert!(notice.starts_with("Claude Code (c) and Claude Code (a) are side by side"), "{notice}");
        assert!(
            !notice.to_lowercase().contains("typing"),
            "where typing goes is not a notice's to say: {notice}"
        );
        assert!(notice.ends_with("If one draws badly, Ctrl+Q then v goes back to one."), "{notice}");
        assert!(app.bar().ends_with("Typing goes to Claude Code (c), on the left."), "{}", app.bar());

        // Tab to the one beside it: the keyboard moves, nothing is resized.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Esc));
        assert!(app.bar().ends_with("Typing goes to Claude Code (a), on the right."), "{}", app.bar());
        assert_eq!(app.engine.ws.size(), Size { rows: 26, cols: 50 });

        app.on_key(press(KeyCode::F(12)));
        assert!(app.bar().ends_with("v one agent"), "{}", app.bar());
        app.on_key(press(KeyCode::Char('v')));
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, whole));
        assert_eq!(
            app.notice.as_deref(),
            Some("One agent on screen: Claude Code (a). Every agent was told its new size.")
        );
        assert!(app.bar().ends_with("Typing goes to Claude Code (a)."), "{}", app.bar());
    }

    #[test]
    fn a_window_made_too_narrow_shows_one_agent_says_why_and_comes_back_when_widened() {
        let mut app = app_with_claude_agents(2);
        app.set_window(WIDE);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(app.two_shown());
        app.set_window(Rect::new(0, 0, 100, 30));
        assert!(!app.two_shown());
        // The user's choice is kept; the agents have the whole area for now.
        assert!(app.engine.ws.pair().is_some());
        assert_eq!(app.engine.ws.size(), Size { rows: 27, cols: 72 });
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "Too narrow for two agents now, so only Claude Code (b) is shown. Claude Code (a) was not \
                 stopped or closed. Widen to 109 columns for both, or Ctrl+Q then v to stay with one."
            )
        );
        assert!(app.bar().ends_with("Typing goes to Claude Code (b)."), "no side is named: {}", app.bar());
        // Narrower still says nothing new.
        app.notice = None;
        app.set_window(Rect::new(0, 0, 90, 30));
        assert_eq!(app.notice, None);
        app.set_window(WIDE);
        assert!(app.two_shown());
        assert_eq!(app.engine.ws.size(), Size { rows: 26, cols: 50 });
        assert_eq!(app.notice.as_deref(), Some("There is room again: two agents are side by side."));
        // Narrow again, and this time choose to stay with one: nothing is
        // resized, no question is asked, and widening brings nothing back.
        app.set_window(Rect::new(0, 0, 100, 30));
        let size = app.engine.ws.size();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "One agent on screen. Two side by side will not come back when the window is widened. \
                 No agent was resized."
            )
        );
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, size));
        app.set_window(WIDE);
        assert!(!app.two_shown());
    }

    #[test]
    fn a_yes_to_two_after_the_window_became_too_small_says_too_small() {
        let mut app = app_with_claude_agents(2);
        app.set_window(WIDE);
        report(&mut app, 1, PERMISSION);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::View(_))));
        app.set_window(Rect::new(0, 0, 200, 5));
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(app.notice.as_deref(), Some(TOO_SMALL_FOR_VIEW));
        assert_eq!(app.engine.ws.pair(), None);
    }

    #[test]
    fn changing_the_view_while_an_agent_needs_you_asks_first() {
        let mut app = app_with_claude_agents(2);
        app.set_window(WIDE);
        let whole = app.engine.ws.size();
        report(&mut app, 1, PERMISSION);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::View(_))));
        let narrow = app.bar().chars().take(usize::from(crate::ui::MIN_COLS)).collect::<String>();
        assert!(
            narrow.starts_with("Resize every agent now? y / n   Waiting for you: Claude Code (a)."),
            "{narrow}"
        );
        assert_eq!(
            (app.engine.ws.pair(), app.engine.ws.size()),
            (None, whole),
            "nothing happens before the answer"
        );
        app.on_key(press(KeyCode::Char('n')));
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, whole));
        assert_eq!(app.notice.as_deref(), Some("The view was left as it was. No agent was resized."));

        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        app.on_key(press(KeyCode::Char('y')));
        assert!(app.two_shown());
        // Going back is asked about too: it is the same resize.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(matches!(app.mode, Mode::Confirm(Confirm::View(_))));
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, whole));

        // A change that would be refused anyway is refused without a question.
        app.set_window(Rect::new(0, 0, 100, 30));
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.notice.clone().unwrap().starts_with("This window is too narrow"));
    }

    #[test]
    fn two_side_by_side_are_drawn_with_a_heading_saying_where_typing_goes() {
        use ratatui::style::Color;
        let mut app = app_with_claude_agents(2);
        let sent = app.engine.ws.entries().len();
        assert_eq!(sent, 2);
        app.set_window(WIDE);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        app.on_agent_event(AgentEvent::Output(AgentId(2), b"words from b".to_vec()));
        app.on_agent_event(AgentEvent::Output(AgentId(1), b"words from a".to_vec()));
        let rows = screen(&app, 130, 30);
        // b is in front, on the left; a is beside it. 28 columns of
        // sidebar, 50 for each agent, one for the rule between them.
        assert_eq!(part(&rows[0], 28, 78), " > TYPING GOES HERE  Claude Code (b)");
        assert_eq!(part(&rows[0], 79, 129), " - not typing here  Claude Code (a)");
        // It says so only while it is true. With the menu open, keys are
        // Lillignode's, and the heading stops claiming them for the agent.
        app.on_key(press(KeyCode::F(12)));
        let menu_open = screen(&app, 130, 30);
        assert_eq!(part(&menu_open[0], 28, 78), " > IN FRONT - keys are Lillignode's now  Claude C…");
        app.on_key(press(KeyCode::Esc));
        // The narrowest each side can be: the words about typing are whole,
        // and it is the name that gives way.
        let narrowest = screen(&app, 109, 30);
        assert_eq!(part(&narrowest[0], 28, 68), " > TYPING GOES HERE  Claude Code (b)");
        assert_eq!(part(&narrowest[0], 69, 109), " - not typing here  Claude Code (a)");
        assert_eq!(part(&rows[1], 28, 78), "words from b");
        assert_eq!(part(&rows[1], 79, 129), "words from a");
        assert!(rows.iter().take(27).all(|r| part(r, 78, 79) == "│"), "a rule between them");

        // The agent beside the one in front starts needing you. Its
        // heading becomes the block and says typing is not going there.
        report(&mut app, 1, PERMISSION);
        let rows = screen(&app, 130, 30);
        assert_eq!(part(&rows[0], 79, 129), " ! NEEDS YOU - you are NOT typing here  Claude Co…");
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 30)).unwrap();
        term.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf[(100, 0)].bg, Color::Yellow);
        assert_ne!(buf[(40, 0)].bg, Color::Yellow, "the heading of the agent in front is not the block");
        // The notice still names it and how to reach it, and typing still
        // goes where the bar says: nothing is held.
        assert!(app
            .notice_text()
            .unwrap()
            .starts_with("Claude Code (a) needs you: permission. Ctrl+Q then Tab"));
        assert!(app.bar().ends_with("Typing goes to Claude Code (b), on the left."), "{}", app.bar());
        // In front and needing you: the block says typing goes here.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Esc));
        let rows = screen(&app, 130, 30);
        assert_eq!(part(&rows[0], 79, 129), " > NEEDS YOU - you are typing here  Claude Code (…");
        assert_eq!(part(&rows[0], 28, 78), " - not typing here  Claude Code (b)");
        // The agent in front stops. Its heading no longer says typing goes
        // there: with nothing running in front, keys are Lillignode's, and
        // x would remove it.
        report(&mut app, 1, "state=idle:app=claude-code");
        app.on_agent_event(AgentEvent::Exited(AgentId(1), crate::agent::Exit { code: 0 }));
        let rows = screen(&app, 130, 30);
        assert_eq!(part(&rows[0], 79, 129), " > STOPPED - menu, then r start again, x remove  …");
        // And beside the one in front, a stopped agent's last screen is
        // not left looking live.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Tab));
        app.on_key(press(KeyCode::Esc));
        let rows = screen(&app, 130, 30);
        assert_eq!(part(&rows[0], 79, 129), " - stopped, its last screen  Claude Code (a)");
        assert_eq!(part(&rows[0], 28, 78), " > TYPING GOES HERE  Claude Code (b)");
        // r on an agent that is running says so, and does nothing.
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('r')));
        assert_eq!(
            app.notice.as_deref(),
            Some("Claude Code (b) is already running, so it was not started again. Nothing was changed.")
        );
    }

    #[test]
    fn closing_one_of_two_gives_the_other_the_whole_screen_and_says_so() {
        let mut app = app_with_claude_agents(2);
        app.set_window(WIDE);
        let whole = app.engine.ws.size();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        assert_ne!(app.engine.ws.size(), whole);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('x')));
        app.on_key(press(KeyCode::Char('y')));
        assert_eq!(
            app.notice.as_deref(),
            Some(
                "Closed Claude Code (b) and removed it from your workspace. Claude Code (a) has the whole \
                 screen again."
            )
        );
        assert_eq!((app.engine.ws.pair(), app.engine.ws.size()), (None, whole));
    }

    #[test]
    fn everything_lillignode_draws_is_ascii_or_one_of_two_listed_characters() {
        // What Lillignode draws must exist in the fonts Windows terminals
        // use. An agent's own screen is not Lillignode's and is left out.
        let check = |rows: Vec<String>, what: &str| {
            for (y, row) in rows.iter().enumerate() {
                for c in row.chars() {
                    assert!(
                        c.is_ascii() || crate::look::NOT_ASCII.contains(&c),
                        "{what}, row {y}: {c:?} in {row}"
                    );
                }
            }
        };
        let mut app = app_with_claude_agents(0);
        check(screen(&app, 100, 20), "welcome");
        check(screen(&app, 40, 8), "too small");
        app = app_with_claude_agents(6);
        app.engine.ws.start_as_named("cmd", std::env::temp_dir().join("a-folder-with-a-long-name")).unwrap();
        app.engine.ws.start_as_named("nosuch", std::env::temp_dir()).unwrap();
        report(&mut app, 1, "state=working:app=claude-code");
        report(&mut app, 2, PERMISSION);
        report(&mut app, 3, "state=done:app=claude-code");
        report(&mut app, 4, "state=idle:app=claude-code");
        report(&mut app, 5, "state=error:app=claude-code:msg=Ym9vbQ==");
        app.on_agent_event(AgentEvent::Exited(AgentId(6), crate::agent::Exit { code: 3 }));
        app.engine.ws.read_cost_from(None);
        app.notice = Some(format!("{}. Then press r.", "word ".repeat(80).trim_end()));
        for clock in 0..2 {
            app.clock = clock;
            check(screen(&app, 80, 30), "sidebar");
        }
        // Two side by side, one of them needing you.
        app.set_window(WIDE);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('v')));
        app.on_key(press(KeyCode::Char('y')));
        assert!(app.two_shown());
        check(screen(&app, 130, 30), "two side by side");
        app.set_window(Rect::new(0, 0, 80, 30));
        for key in ['s', 'q', 'x', 'n'] {
            app.on_key(press(KeyCode::F(12)));
            check(screen(&app, 80, 30), "menu");
            app.on_key(press(KeyCode::Char(key)));
            check(screen(&app, 80, 30), &format!("after {key}"));
            app.on_key(press(KeyCode::Esc));
        }
    }

    #[test]
    fn a_too_small_window_names_the_menu_keys_first_and_still_says_who_needs_you() {
        let mut app = app_with_claude_agents(2);
        app.too_small = true;
        assert_eq!(app.bar(), "Ctrl+Q or F12: menu, then q to quit");
        report(&mut app, 1, PERMISSION);
        assert!(
            app.bar().starts_with("Ctrl+Q or F12: menu, then Tab goes to who needs you"),
            "{}",
            app.bar()
        );
        assert!(app.bar().ends_with("1 agent needs you."), "{}", app.bar());
        report(&mut app, 2, PERMISSION);
        assert!(app.bar().ends_with("2 agents need you."), "{}", app.bar());
    }

    #[test]
    fn the_status_report_for_any_program_names_its_sender_and_says_what_was_not_checked() {
        let (mut app, _) = app_with_agent();
        app.notice = None;
        let text = app.status_report_text();
        assert!(
            text.contains("Status: Unknown: no report. This program has sent no status report."),
            "{text}"
        );
        assert!(text.contains("No report has been received."), "{text}");
        assert!(!app.after_events(), "nothing was expected of it, so no bell and no notice");
        assert_eq!(app.notice, None);

        report(&mut app, 1, "state=blocked:app=deploy:kind=coffee");
        assert!(app.after_events(), "any program that needs you rings");
        let text = app.status_report_text();
        assert!(text.contains("Status: Needs you. The sidebar shows it as Needs you."), "{text}");
        assert!(text.contains("Sent by: \"deploy\".\n"), "{text}");
        assert!(text.contains("(OSC 7501, revision 0.4). Not checked with this program."), "{text}");
        assert!(!text.contains("Codex"), "{text}");

        report(&mut app, 1, "state=idle");
        let text = app.status_report_text();
        assert!(text.contains("Its reports do not say which program sent them."), "{text}");
        // Not understood, from a program never known to work: no notice.
        report(&mut app, 1, "state=pondering");
        assert!(!app.after_events());
        assert_eq!(app.notice, None);
        assert!(app.status_report_text().contains("Status: Unknown: not understood."));
    }

    #[test]
    fn the_status_report_says_when_reports_name_another_program_than_the_agent_was_started_as() {
        let mut app = app_with_claude_and_codex();
        codex_reports(&mut app, 2, "state=working:app=claude-code");
        let text = app.status_report_text();
        assert!(text.contains("Status: Working."), "{text}");
        assert!(text.contains("Sent by: Claude Code. This agent was started as Codex;"), "{text}");
        assert!(text.contains("Checked with Claude Code 2.1.325."), "{text}");
        assert!(!text.contains("Two things to know about Codex's reports"), "{text}");
    }

    #[test]
    fn the_status_report_says_how_many_bells_were_not_rung() {
        let (mut app, _) = app_with_agent();
        let mut rung = 0;
        for _ in 0..10 {
            report(&mut app, 1, "state=working");
            report(&mut app, 1, "state=blocked");
            rung += u32::from(app.after_events());
        }
        assert_eq!(rung, u32::from(status::BELLS));
        let text = app.status_report_text();
        assert!(text.contains("Bells not rung: 7. It began to need you more often"), "{text}");
        assert!(text.contains("(3 in a row, then one every 20 seconds)"), "{text}");
    }

    /// One Claude Code agent (id 1, asked and answered) and one Codex agent
    /// (id 2), which is in front.
    fn app_with_claude_and_codex() -> App<Fake> {
        let mut app = app_with_claude_agents(1);
        app.engine.ws.start_as_named("codex", std::env::temp_dir().join("web")).unwrap();
        app.notice = None;
        app
    }

    fn codex_reports(app: &mut App<Fake>, id: u32, body: &str) {
        app.on_agent_event(AgentEvent::Output(AgentId(id), format!("\x1b]7501;{body}\x1b\\").into_bytes()));
    }

    #[test]
    fn the_quit_question_names_every_agent_that_does_not_keep_its_conversation() {
        let mut app = app_with_claude_and_codex();
        report(&mut app, 1, "state=working:app=claude-code");
        app.engine.ws.start_as_named("aider", std::env::temp_dir().join("tools")).unwrap();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        let bar = app.bar();
        assert!(bar.ends_with("Codex (web), aider (tools) start new conversations next time."), "{bar}");
        assert!(!bar.contains("Claude Code (a) start"), "{bar}");
    }

    #[test]
    fn the_quit_question_says_a_codex_agent_starts_a_new_conversation_next_time() {
        let mut app = app_with_claude_and_codex();
        report(&mut app, 1, "state=working:app=claude-code");
        codex_reports(&mut app, 2, "state=idle:app=codex");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        let bar = app.bar();
        assert!(bar.starts_with("Quit and stop 2 running agents? y / n"), "{bar}");
        assert!(bar.ends_with("They come back next time you start Lillignode. Codex (web) starts a new conversation next time."), "{bar}");
        // Said of Codex only: Claude Code continues its conversation.
        assert!(!bar.contains("Claude Code (a) starts"), "{bar}");

        // Two Codex agents: both named, in one sentence.
        app.on_key(press(KeyCode::Char('n')));
        app.engine.ws.start_as_named("codex", std::env::temp_dir().join("api")).unwrap();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(
            app.bar().ends_with("Codex (web), Codex (api) start new conversations next time."),
            "{}",
            app.bar()
        );

        // None running: nothing said about it.
        let mut app = app_with_claude_agents(1);
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.bar().ends_with("It comes back next time you start Lillignode."), "{}", app.bar());
    }

    #[test]
    fn a_codex_agent_that_needs_you_is_counted_named_and_rings_like_any_other() {
        let mut app = app_with_claude_and_codex();
        assert_eq!(app.engine.ws.focused().unwrap().label(), "Codex (web)");
        report(&mut app, 1, "state=idle:app=claude-code");
        codex_reports(&mut app, 2, "state=working:app=codex");
        assert!(!app.after_events());
        assert_eq!(app.unfinished(), ["Codex (web) (Working)"]);

        codex_reports(&mut app, 2, "state=blocked:app=codex:kind=permission");
        assert!(app.after_events(), "the bell rings for it");
        assert!(!app.after_events(), "once");
        assert_eq!(app.needs_you_count(), 1);
        // From behind another agent, the notice names it.
        app.engine.ws.focus_next();
        let notice = app.needs_you_notice().unwrap();
        assert!(notice.contains("Codex (web)"), "{notice}");

        // Done is settled: quitting would not ask about it.
        codex_reports(&mut app, 2, "state=done:app=codex");
        assert_eq!(app.needs_you_count(), 0);
        assert!(app.unfinished().is_empty(), "{:?}", app.unfinished());
    }

    #[test]
    fn the_status_report_for_codex_says_what_is_true_of_codex() {
        let mut app = app_with_claude_and_codex();
        // Set where Lillignode was started, but it is Claude Code's switch.
        app.status_off_switch = Some("1".into());
        codex_reports(&mut app, 2, "state=blocked:app=codex:kind=permission");
        let text = app.status_report_text();
        assert!(
            text.starts_with("Status report for Codex (web)\n\nStatus: Needs you: permission."),
            "{text}"
        );
        assert!(text.contains("Status question: not asked. A program may report without asking."), "{text}");
        assert!(text.contains("Reports received since it started: 1."), "{text}");
        assert!(text.contains("7501;state=blocked:app=codex:kind=permission"), "{text}");
        assert!(text.contains("Read as: "), "{text}");
        assert!(text.contains("Sent by: Codex.\n"), "{text}");
        assert!(text.contains("(OSC 7501, revision 0.4). Checked with Codex 0.169.0."), "{text}");
        // What Codex does not say, said plainly.
        assert!(text.contains("it reports Idle, so Lillignode shows Idle there"), "{text}");
        assert!(text.contains("Done means its turn ended, not that the work succeeded."), "{text}");
        // Nothing that is only true of Claude Code.
        for wrong in ["CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "Status question: asked", "2.1.325"] {
            assert!(!text.contains(wrong), "{wrong} in: {text}");
        }
        assert!(!text.contains("Checked with Claude Code"), "{text}");

        // The Claude Code agent's report is as it was.
        app.engine.ws.focus_next();
        let text = app.status_report_text();
        assert!(text.starts_with("Status report for Claude Code (a)"), "{text}");
        assert!(text.contains("CLAUDE_CODE_DISABLE_TERMINAL_TITLE is set"), "{text}");
        assert!(text.contains("Status question: asked"), "{text}");
        assert!(text.contains("Checked with Claude Code 2.1.325."), "{text}");
        assert!(!text.contains("Codex"), "{text}");
    }

    #[test]
    fn a_codex_agent_that_reports_nothing_gets_its_own_notice_once_and_its_own_reason() {
        let mut app = app_with_claude_and_codex();
        // Neither applies to Codex: it needs no answer, so no files, and the
        // switch is Claude Code's.
        app.newer_pty_missing = true;
        app.status_off_switch = Some("1".into());
        report(&mut app, 1, "state=idle:app=claude-code");
        app.engine.ws.tick(Instant::now() + status::STARTING_DEADLINE);
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert_eq!(
            notice,
            "Status shows Unknown: Codex is not reporting it. If it is signed out it reports nothing: \
             sign in and it will. Agents work as before; nothing was changed."
        );
        let text = app.status_report_text();
        assert!(
            text.contains("Status: Unknown: no report. Codex sent no status report within 15 seconds"),
            "{text}"
        );
        assert!(text.contains("If it is signed out, sign in and it reports."), "{text}");

        // Once for Codex: a second silent Codex agent says nothing more.
        app.notice = None;
        app.engine.ws.start_as_named("codex", std::env::temp_dir().join("docs")).unwrap();
        app.engine.ws.tick(Instant::now() + status::STARTING_DEADLINE + status::STARTING_DEADLINE);
        app.after_events();
        assert_eq!(app.notice, None);

        // Claude Code's reporting failing is a different matter, said once too.
        report(&mut app, 1, "state=pondering:app=claude-code");
        app.after_events();
        let notice = app.notice.clone().unwrap();
        assert!(
            notice.starts_with("Status shows Unknown: CLAUDE_CODE_DISABLE_TERMINAL_TITLE is set here."),
            "{notice}"
        );
    }

    #[test]
    fn a_plain_program_and_codex_never_have_a_cost_and_say_so() {
        let mut app = app_with_claude_and_codex();
        app.engine.ws.read_cost_from(None);
        app.config.cost = true;
        let text = app.status_report_text();
        assert!(text.contains("Lillignode reads cost only for Claude Code."), "{text}");
        assert_eq!(app.engine.ws.focused().unwrap().cost(), Cost::Unknown(WhyNot::NotClaudeCode));
    }
}
