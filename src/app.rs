//! What the user can do, and every sentence Lillignode says to them.
//!
//! Keys go to the focused agent while it is running. A menu key (by default
//! Ctrl+Q or F12) opens Lillignode's own menu. When there is no running agent
//! in front, the menu keys work directly, since nothing else could be meant.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agent::{AgentBackend, AgentEvent, Size};
use crate::config::{Billing, Config};
use crate::cost::{self, Cost, Usage, Waiting, WhyNot};
use crate::keys;
use crate::state::{self, LoadOutcome};
use crate::status::{self, AgentStatus, Reading};
use crate::terminal::HISTORY_ROWS;
use crate::workspace::{Entry, Happened, RestoreReport, Status, Workspace};

pub const DEFAULT_COMMAND: &str = "claude";
const AGENT_PROMPT: &str = "Agent to start: ";
const FOLDER_PROMPT: &str = "Folder to work in: ";

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

pub struct NewAgent {
    /// None while asking for the command; Some(command) while asking for the folder.
    pub command: Option<String>,
    pub input: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Close,
    Quit,
    /// A paste of several lines into an agent that would take each line as
    /// a separate Enter. The text waits in `App::held_paste`.
    Paste,
}

pub struct App<B: AgentBackend> {
    pub ws: Workspace<B>,
    pub mode: Mode,
    pub config: Config,
    /// The latest thing the user should know. Stays until replaced.
    pub notice: Option<String>,
    /// Problems found on starting, repeated on the way out so they remain in
    /// the terminal after Lillignode closes.
    pub start_problems: Vec<String>,
    pub quit: bool,
    /// Set by the caller each time it draws: the window cannot show agents.
    pub too_small: bool,
    held_paste: Option<String>,
    /// Where the workspace is saved, or why it cannot be.
    state_dir: Result<PathBuf, String>,
    /// The folder Lillignode was started in: the default for a new agent.
    launch_dir: PathBuf,
    /// The value of Claude Code's status off switch where Lillignode was
    /// started, if set. Every agent inherits it. Lillignode leaves it alone.
    status_off_switch: Option<String>,
    /// The notice that Claude Code's status is not reaching Lillignode is
    /// given once per run.
    unknown_notice_given: bool,
    /// The notice that a cost could not be read is given once per run too.
    cost_notice_given: bool,
    /// Windows only: Microsoft's newer pseudo-terminal is not beside
    /// `lillignode.exe`, so Claude Code's status cannot get through.
    pub newer_pty_missing: bool,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
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
    match outcome {
        LoadOutcome::Nothing | LoadOutcome::Loaded => None,
        LoadOutcome::UsedPrevious { unreadable_kept_as, previous_saved, why } => Some(format!(
            "Your saved workspace could not be read: {why}. Loaded the previous copy, saved {}. \
             The unreadable file was kept as {}.",
            ago(*previous_saved),
            unreadable_kept_as.display()
        )),
        LoadOutcome::Lost { unreadable_kept_as, why } => Some(format!(
            "Your saved workspace could not be read: {why}. There is no earlier copy, so the \
             workspace starts empty. The unreadable file was kept as {}.",
            unreadable_kept_as.display()
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
    Some(format!(
        "Restored {} of {}{restored}. Not restored: {}. Each is still in your workspace. Switch to \
         one, fix the cause, then r to try again or x to remove it.",
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
        let mut ws = Workspace::new(backend, size);
        let mut start_problems: Vec<String> = config_problem.into_iter().collect();
        let mut said = start_problems.clone();
        match &state_dir {
            Ok(dir) => {
                let loaded = state::load(dir);
                if let Some(problem) = load_problem_text(&loaded.outcome) {
                    said.push(problem.clone());
                    start_problems.push(problem);
                }
                said.extend(restore_text(&ws.restore(loaded.workspace)));
            }
            Err(why) => said.push(format!(
                "Lillignode has nowhere to save your workspace ({why}). It works, but nothing will \
                 be restored next time."
            )),
        }
        let notice = (!said.is_empty()).then(|| said.join(" "));
        let mut app = Self {
            ws,
            mode: Mode::Normal,
            config,
            notice,
            start_problems,
            quit: false,
            too_small: false,
            held_paste: None,
            state_dir,
            launch_dir,
            status_off_switch: std::env::var(status::OFF_SWITCH).ok(),
            unknown_notice_given: false,
            cost_notice_given: false,
            newer_pty_missing: newer_pty_missing(),
        };
        // Restoring may have given agents new conversation ids; keep them.
        app.save_or_say();
        app
    }

    /// Save the workspace. On failure the user is told, and what was saved
    /// before is left as it was.
    pub fn save(&mut self) -> Result<(), String> {
        let Ok(dir) = &self.state_dir else { return Ok(()) };
        state::save(dir, &self.ws.to_saved()).map_err(|why| {
            format!(
                "Could not save your workspace ({why}). The last saved workspace is unchanged; \
                 changes since then will not be restored."
            )
        })
    }

    fn save_or_say(&mut self) {
        if let Err(problem) = self.save() {
            self.notice = Some(problem);
        }
    }

    pub fn on_agent_event(&mut self, event: AgentEvent) {
        let Some(happened) = self.ws.handle(event) else { return };
        let still_here =
            "It is still in your workspace. Switch to it, then r to start it again or x to remove it.";
        self.notice = Some(match happened {
            Happened::Exited { label, exit } if exit.clean() => format!("{label} exited. {still_here}"),
            Happened::Exited { label, exit } => {
                format!("{label} stopped with an error (exit code {}). {still_here}", exit.code)
            }
            Happened::StartedNewConversation { label } => {
                self.save_or_say();
                format!(
                    "{label} could not continue its earlier conversation (there may have been nothing \
                     in it yet), so it started a new conversation. If there was an earlier one, the \
                     agent's own resume command may still find it."
                )
            }
            Happened::CouldNotContinue { label, why } => format!(
                "{label} could not continue its earlier conversation, and starting a new one failed: {why}. {still_here}"
            ),
        });
    }

    fn focused_running(&self) -> bool {
        self.ws.focused().is_some_and(|e| e.is_running())
    }

    fn focused_label(&self) -> String {
        self.ws.focused().map(|e| e.label()).unwrap_or_default()
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let menu_key = self.config.is_menu_key(&key);
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
                let app_cursor = self.ws.focused().is_some_and(|e| e.term.screen().application_cursor());
                if let Some(bytes) = keys::encode(key, app_cursor) {
                    self.ws.send_to_focused(&bytes);
                }
            }
            Mode::Normal => self.command(key),
            Mode::Menu => {
                // Switching keeps the menu open, so reaching the fourth agent
                // is Tab Tab Tab, not three trips through the menu.
                if !matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.mode = Mode::Normal;
                }
                self.command(key);
            }
            Mode::History => self.history_key(key),
            // Any key closes it and is not passed on.
            Mode::StatusReport => self.mode = Mode::Normal,
            Mode::NewAgent(_) => self.new_agent_key(key),
            Mode::Confirm(what) => {
                self.mode = Mode::Normal;
                let yes = key.code == KeyCode::Char('y');
                match what {
                    Confirm::Close if yes => self.close_focused(),
                    Confirm::Quit if yes => self.quit = true,
                    Confirm::Paste => {
                        let text = self.held_paste.take().unwrap_or_default();
                        if yes {
                            self.ws.send_to_focused(text.replace('\n', "\r").as_bytes());
                            self.notice = Some(format!("Pasted into {} line by line.", self.focused_label()));
                        } else {
                            self.notice = Some("Paste discarded. Nothing was sent.".to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Text arrived as a paste. It goes to the agent as one paste or, where
    /// that is impossible, only after the user agrees. Its line breaks are
    /// never passed on as Enter presses unasked.
    pub fn on_paste(&mut self, text: String) {
        if let Mode::NewAgent(form) = &mut self.mode {
            // A pasted folder path: take the first line only.
            form.input.push_str(text.lines().next().unwrap_or("").trim());
            return;
        }
        let can_take_it = self.focused_running() && !self.too_small;
        match self.mode {
            Mode::Normal if can_take_it => {
                let whole = self.ws.focused().is_some_and(|e| e.term.takes_pastes_whole());
                if whole {
                    let body = text.replace('\n', "\r");
                    self.ws.send_to_focused(format!("\x1b[200~{body}\x1b[201~").as_bytes());
                } else if !text.contains('\n') {
                    self.ws.send_to_focused(text.as_bytes());
                } else {
                    self.held_paste = Some(text);
                    self.mode = Mode::Confirm(Confirm::Paste);
                }
            }
            _ => {
                self.notice = Some(
                    "Paste ignored: it can only go to a running agent in front. Nothing was sent."
                        .to_string(),
                );
            }
        }
    }

    /// A Lillignode command key. Unknown keys do nothing.
    fn command(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('n') => {
                self.mode = Mode::NewAgent(NewAgent { command: None, input: String::new() });
            }
            KeyCode::Tab => self.ws.focus_next(),
            KeyCode::BackTab => self.ws.focus_previous(),
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::Up | KeyCode::Down => {
                self.history_key(key)
            }
            KeyCode::Char('r') => {
                if let Some(continuing) = self.ws.restart_focused() {
                    self.notice = self.ws.focused().map(|e| match &e.status {
                        Status::NotStarted(why) => {
                            format!("{} still could not be started: {why}. Nothing was changed.", e.label())
                        }
                        _ if continuing => {
                            format!("Started {} again, continuing its conversation.", e.label())
                        }
                        _ => format!("Started {} again.", e.label()),
                    });
                    self.save_or_say();
                }
            }
            KeyCode::Char('x') => {
                if self.focused_running() {
                    self.mode = Mode::Confirm(Confirm::Close);
                } else {
                    self.close_focused();
                }
            }
            KeyCode::Char('s') => {
                if self.ws.focused().is_some() {
                    self.mode = Mode::StatusReport;
                } else {
                    self.notice = Some("There is no agent to show a status report for.".to_string());
                }
            }
            KeyCode::Char('q') => {
                // Every running agent is stopped either way; the question is
                // asked only when one may be stopped mid-request.
                if self.unfinished().is_empty() {
                    self.quit = true;
                } else {
                    self.mode = Mode::Confirm(Confirm::Quit);
                }
            }
            _ => {}
        }
    }

    /// Scroll the focused agent's history. Any key that is not a scroll key
    /// returns to the present and is not passed on.
    fn history_key(&mut self, key: KeyEvent) {
        let page = self.ws.size().rows.saturating_sub(1).max(1) as isize;
        let label = self.focused_label();
        let Some(entry) = self.ws.focused_mut() else {
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

    fn close_focused(&mut self) {
        if let Some(label) = self.ws.close_focused() {
            self.notice = Some(format!("Closed {label} and removed it from your workspace."));
            self.save_or_say();
        }
    }

    fn new_agent_key(&mut self, key: KeyEvent) {
        let Mode::NewAgent(form) = &mut self.mode else { return };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.notice = Some("Cancelled. Nothing was started.".to_string());
            }
            KeyCode::Backspace => {
                form.input.pop();
            }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                form.input.push(c);
            }
            KeyCode::Enter => {
                let typed = form.input.trim().trim_matches('"').to_string();
                match form.command.take() {
                    None => {
                        form.command =
                            Some(if typed.is_empty() { DEFAULT_COMMAND.to_string() } else { typed });
                        form.input.clear();
                    }
                    Some(command) => {
                        let folder = resolve_folder(&self.launch_dir, &typed);
                        match self.ws.start_new(&command, folder.clone()) {
                            Ok(()) => {
                                self.mode = Mode::Normal;
                                self.notice = self
                                    .ws
                                    .focused()
                                    .map(|e| format!("Started {} in {}.", e.saved.name, folder.display()));
                                self.save_or_say();
                            }
                            Err(why) => {
                                // Stay on the question so it can be corrected.
                                self.notice = Some(format!(
                                    "Could not start {command}: {why}. Nothing was started. Correct the folder, or Esc to cancel."
                                ));
                                form.command = Some(command);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// The one line at the bottom: what can be done right now. Outside a
    /// question it always names the menu keys, so the way out is never hidden.
    pub fn bar(&self) -> String {
        let menu = self.config.menu_keys_label();
        match &self.mode {
            // The sidebar is not drawn here, so who needs you is said first.
            Mode::Normal if self.too_small => match self.needs_you_count() {
                0 => format!("{menu}: menu, then q to quit"),
                n => format!(
                    "{} you.  {menu}: menu, then Tab to go to it, q to quit",
                    plural(n, "agent needs", "agents need")
                ),
            },
            Mode::Normal if self.focused_running() => {
                format!("Typing goes to {}.  {menu}: Lillignode menu", self.focused_label())
            }
            Mode::Normal if self.ws.entries().is_empty() => format!("n new agent   q quit   ({menu}: menu)"),
            Mode::Normal => {
                format!("r start again   x remove   Tab next agent   n new agent   q quit   ({menu}: menu)")
            }
            Mode::Menu => "MENU  Tab next   Shift+Tab previous   PgUp history   n new agent   x close   \
                           r start again   s status, cost   q quit   Esc back to agent"
                .to_string(),
            Mode::StatusReport => {
                format!("STATUS REPORT of {}.  Any key: back to the agent", self.focused_label())
            }
            Mode::History => {
                let back = self.ws.focused().map(|e| e.term.scrolled_back()).unwrap_or(0);
                format!(
                    "HISTORY of {}: {} back.  PgUp PgDn Up Down Home scroll   any other key: back to now",
                    self.focused_label(),
                    plural(back, "line", "lines")
                )
            }
            // What is being typed comes first and the hint after it, so a
            // long folder path cannot push the typing off the screen.
            Mode::NewAgent(NewAgent { command: None, input }) => {
                format!("{AGENT_PROMPT}{input}   (Enter alone: {DEFAULT_COMMAND}   Esc: cancel)")
            }
            Mode::NewAgent(NewAgent { command: Some(_), input }) => {
                format!("{FOLDER_PROMPT}{input}   (Enter alone: {}   Esc: cancel)", self.launch_dir.display())
            }
            // Every question puts the question and its keys first. The bar
            // is one line, and a question cut off before "y / n" cannot be
            // answered.
            Mode::Confirm(Confirm::Close) => format!(
                "Close {}? y / n   It is running: closing stops it and removes it from your workspace.",
                self.focused_label()
            ),
            Mode::Confirm(Confirm::Quit) => {
                let running = self.ws.running_labels().len();
                format!(
                    "Quit and stop {}? y / n   Not finished or not known: {}. {} back next time you \
                     start Lillignode.",
                    plural(running, "running agent", "running agents"),
                    self.unfinished().join(", "),
                    if running == 1 { "It comes" } else { "They come" },
                )
            }
            Mode::Confirm(Confirm::Paste) => {
                let lines = self.held_paste.as_deref().map(|t| t.split('\n').count()).unwrap_or(0);
                format!(
                    "Paste {lines} lines into {} one line at a time? y / n   It does not take a paste as \
                     one block, so each line would be entered as if you pressed Enter after it.",
                    self.focused_label()
                )
            }
        }
    }

    /// Call after each batch of events and ticks. Gives the Unknown notice
    /// the first time Claude Code's reporting is seen not to reach Lillignode,
    /// and returns true if the bell should ring: an agent has just started
    /// needing you, and the bell is not switched off.
    pub fn after_events(&mut self) -> bool {
        let began = self.ws.take_began_needing_you();
        if !self.unknown_notice_given {
            let broken =
                self.ws.entries().iter().filter(|e| e.is_claude_code()).find_map(|e| match e.reported() {
                    Some(AgentStatus::Unknown(why)) if why.is_breakage() => Some(why),
                    _ => None,
                });
            if let Some(why) = broken {
                self.unknown_notice_given = true;
                // Never replaces a notice already up (it may be a failure the
                // user still has to act on): it goes first, the other after it.
                let text = self.unknown_notice(why);
                self.notice = Some(match self.notice.take() {
                    Some(earlier) => format!("{text}  {earlier}"),
                    None => text,
                });
            }
        }
        if !self.cost_notice_given {
            // A cost Lillignode tried to read and could not. Pending, and an
            // agent that is not Claude Code, are not failures.
            let failed = self.ws.entries().iter().find(|e| {
                matches!(
                    e.cost(),
                    Cost::Unknown(WhyNot::NotUnderstood { .. } | WhyNot::CannotRead(_) | WhyNot::NoFolder)
                )
            });
            if let Some(entry) = failed {
                self.cost_notice_given = true;
                let menu = self.config.menu_keys.first().map(|k| k.label()).unwrap_or_default();
                let text = format!(
                    "Cost shows unknown for {}: Lillignode could not read Claude Code's record of it. Agents \
                     work as before; nothing was changed. {menu} then s says why.",
                    entry.label()
                );
                self.notice = Some(match self.notice.take() {
                    Some(earlier) => format!("{text}  {earlier}"),
                    None => text,
                });
            }
        }
        began && self.config.bell
    }

    /// Why every Claude Code agent shows Unknown, said once per run. The
    /// most specific cause Lillignode can see comes first. Each fits the two
    /// notice rows of the narrowest window Lillignode draws in
    /// (`NOTICE_LIMIT`), so what to do, which comes last, is never cut off.
    /// The longer explanation is in the status report panel.
    fn unknown_notice(&self, why: status::Why) -> String {
        let fine = "Agents work as before; nothing was changed.";
        if self.status_off_switch.is_some() {
            return format!(
                "Status shows Unknown: {} is set here. {fine} Lillignode leaves that setting alone.",
                status::OFF_SWITCH
            );
        }
        if why == status::Why::NotUnderstood {
            return format!(
                "Status shows Unknown: Claude Code sent a status Lillignode does not understand. {fine} \
                 Look for a newer Lillignode."
            );
        }
        if self.newer_pty_missing {
            return format!(
                "Status shows Unknown: conpty.dll and OpenConsole.exe are not beside lillignode.exe. \
                 {fine} Reinstall Lillignode to fix it."
            );
        }
        format!(
            "Status shows Unknown: Claude Code is not reporting it. {fine} Checked with Claude Code {}; \
             get a newer Lillignode.",
            status::CHECKED_WITH
        )
    }

    /// How many agents are waiting for the user, the one in front included.
    pub fn needs_you_count(&self) -> usize {
        self.ws.entries().iter().filter(|e| e.needs_you().is_some()).count()
    }

    /// The sentence naming agents that need the user and are not in front.
    /// It stays until they stop needing you, and an ordinary notice cannot
    /// replace it.
    pub fn needs_you_notice(&self) -> Option<String> {
        let focused = self.ws.focused_index()?;
        let count = self.ws.entries().len();
        let waiting: Vec<(usize, &Entry)> = self
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
                let what = match e.needs_you().flatten() {
                    Some(kind) => format!("needs you: {}", kind.word()),
                    None => "needs you".to_string(),
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
        let (waiting, notice) = match (self.needs_you_notice(), &self.notice) {
            (Some(waiting), Some(notice)) => (waiting, notice),
            (Some(waiting), None) => return Some(keep_end(&waiting, room)),
            (None, notice) => return notice.as_deref().map(|n| keep_end(n, room)),
        };
        let count = |s: &str| s.chars().count();
        if count(&waiting) + 2 + count(notice) <= room {
            return Some(format!("{waiting}  {notice}"));
        }
        // Who needs you comes first and is shortened last. The notice after
        // it is promised room for its last sentence, up to half the rows.
        let reserve = (count(last_sentence(notice)) + 2).min(count(notice)).min(room / 2);
        let waiting = keep_end(&waiting, room.saturating_sub(reserve + 2));
        let notice = keep_end(notice, room.saturating_sub(count(&waiting) + 2));
        Some(format!("{waiting}  {notice}"))
    }

    /// A figure for the sidebar: tokens, or for a user who has said they
    /// pay by key, the estimate in dollars. Without a price for every model
    /// there is no dollar figure, so tokens are shown.
    fn figure(&self, usage: &Usage) -> String {
        match (self.config.billing, usage.dollars()) {
            (Billing::Key, Ok(amount)) => {
                format!("est. {}", cost::dollars_text(amount).replace("under ", "<"))
            }
            _ => format!("{} tokens", cost::short(usage.total().total())),
        }
    }

    /// The few words about an agent's cost on its row of the sidebar.
    /// Pending and unknown are said as such, never shown as a number.
    pub fn cost_short(&self, entry: &Entry) -> String {
        match entry.cost() {
            Cost::Known(usage) => self.figure(&usage),
            Cost::Pending(_) => "cost pending".to_string(),
            Cost::Unknown(_) => "cost unknown".to_string(),
        }
    }

    /// The foot of the sidebar: the total for all agents whose cost is
    /// known, and on a second line how many agents are not in it. Empty
    /// when there are no agents.
    pub fn cost_total(&self) -> Vec<String> {
        let mut all = Usage::default();
        let (mut known, mut pending, mut unknown) = (0, 0, 0);
        for entry in self.ws.entries() {
            match entry.cost() {
                Cost::Known(usage) => {
                    known += 1;
                    for (model, tokens) in usage.by_model {
                        let sum = all.by_model.entry(model).or_default();
                        sum.input = sum.input.saturating_add(tokens.input);
                        sum.output = sum.output.saturating_add(tokens.output);
                        sum.cache_write = sum.cache_write.saturating_add(tokens.cache_write);
                        sum.cache_read = sum.cache_read.saturating_add(tokens.cache_read);
                    }
                }
                Cost::Pending(_) => pending += 1,
                Cost::Unknown(_) => unknown += 1,
            }
        }
        match (known, pending, unknown) {
            (0, 0, 0) => Vec::new(),
            (0, 0, _) => vec![" ALL AGENTS  cost unknown".to_string()],
            (0, _, _) => vec![" ALL AGENTS  cost pending".to_string()],
            (_, 0, 0) => vec![format!(" ALL AGENTS  {}", self.figure(&all))],
            _ => vec![
                format!(" ALL AGENTS  {}", self.figure(&all)),
                format!("  not counted: {}", plural(pending + unknown, "agent", "agents")),
            ],
        }
    }

    /// The cost part of the status report panel: the breakdown, what the
    /// figure is and is not, and for pending or unknown, the reason.
    fn cost_report(&self, e: &Entry) -> String {
        let record = "Claude Code's record of this conversation";
        let usage = match e.cost() {
            Cost::Known(usage) => usage,
            Cost::Pending(waiting) => {
                return format!(
                    "Cost: pending. {}\n",
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
                    "Cost: unknown. {}\n",
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
            "Cost of this conversation so far, including before today: {} tokens.\n  input {}, output {}, \
             written to cache {}, read from cache {}\n",
            cost::exact(all.total()),
            cost::exact(all.input),
            cost::exact(all.output),
            cost::exact(all.cache_write),
            cost::exact(all.cache_read)
        );
        if usage.by_model.len() > 1 {
            for (model, tokens) in &usage.by_model {
                out += &format!("  {model}: {}\n", cost::exact(tokens.total()));
            }
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
        out
    }

    /// Running agents that may be in the middle of something: not Done,
    /// Idle, or Error. Quitting asks first only if there are any.
    fn unfinished(&self) -> Vec<String> {
        self.ws
            .entries()
            .iter()
            .filter(|e| e.is_running() && !e.is_settled())
            .map(|e| match e.reported() {
                Some(s) => format!("{} ({})", e.label(), s.text()),
                None => e.label(),
            })
            .collect()
    }

    /// What the status report panel says about the focused agent: what the
    /// sidebar shows and why, and the last report exactly as it arrived, so
    /// an Unknown can be traced to its cause.
    pub fn status_report_text(&self) -> String {
        let Some(e) = self.ws.focused() else { return "No agent is open.".to_string() };
        let t = &e.track;
        let mut out = format!("Status report for {}\n\n", e.label());
        match e.reported() {
            Some(shown) => {
                out += &format!("The sidebar shows: {}", shown.text());
                if let AgentStatus::Unknown(why) = shown {
                    out += &format!(". {}", status::why_long(why));
                }
                out += ".\n";
            }
            None => out += &format!("The sidebar shows: {}. The program is not running.\n", e.status.text()),
        }
        out += &format!("\n{}", self.cost_report(e));
        if !e.is_claude_code() {
            return out;
        }
        out += "\n";
        // Near the top, so a short window still shows it: when it is set,
        // nothing below can be other than empty.
        if let Some(value) = &self.status_off_switch {
            out += &format!(
                "{} is set (to \"{}\") where Lillignode was started, and every agent inherits it. Claude \
                 Code does not report its status while it is switched on. Lillignode leaves it alone: it \
                 is your setting.\n",
                status::OFF_SWITCH,
                value.escape_debug()
            );
        }
        if e.is_running() {
            out += &match t.asked_after {
                Some(after) => format!(
                    "Status question: asked {:.1} seconds after it started. Lillignode answered yes.\n",
                    after.as_secs_f32()
                ),
                None => format!(
                    "Status question: not asked in the {} since it started.\n",
                    plural(t.running_for(Instant::now()).as_secs() as usize, "second", "seconds")
                ),
            };
        }
        out += &format!("Reports received since it started: {}", t.reports);
        if t.task_reports > 0 {
            out +=
                &format!(" ({} about its background tasks, which do not change its status)", t.task_reports);
        }
        out += ".\n\n";
        match &t.last {
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
                out += &format!("Read as: {}", reading_text(&last.reading));
                if !last.after_question {
                    out += ". Not used: it came before the agent asked the status question";
                }
                out += ".\n";
            }
        }
        if self.status_off_switch.is_none() {
            out += &format!("\n{} is not set where Lillignode was started.\n", status::OFF_SWITCH);
        }
        out += &format!(
            "\nRead the way Claude Code {} reports. Kept in memory only, never on disk, and started \
             afresh whenever the agent is started.",
            status::CHECKED_WITH
        );
        out
    }

    /// Where the typing cursor sits in the bar, counted in characters, while
    /// the bar is asking for text.
    pub fn bar_cursor(&self) -> Option<usize> {
        match &self.mode {
            Mode::NewAgent(form) => {
                let prompt = if form.command.is_none() { AGENT_PROMPT } else { FOLDER_PROMPT };
                Some(prompt.chars().count() + form.input.chars().count())
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

/// The last sentence of a message, which is where it says what to do.
fn last_sentence(text: &str) -> &str {
    let text = text.trim_end();
    text.rfind(". ").map_or(text, |at| text[at + 2..].trim_start())
}

/// `text` in at most `room` characters. When it is too long its last
/// sentence is kept whole and the cut, marked with an ellipsis, is made
/// before it. Only a last sentence that is itself longer than the room loses
/// its end; then the start of the message is the most that can be shown.
fn keep_end(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    let tail = last_sentence(text);
    let tail_chars = tail.chars().count();
    // Two more for the ellipsis and the space that join the two halves.
    let Some(head_chars) = room.checked_sub(tail_chars + 2) else {
        let mut cut: String = text.chars().take(room.saturating_sub(1)).collect();
        if room > 0 {
            cut.push('…');
        }
        return cut;
    };
    let head: String = text.chars().take(head_chars).collect();
    format!("{}… {tail}", head.trim_end())
}

fn reading_text(reading: &Reading) -> String {
    match reading {
        Reading::Status { status, message: Some(m) } => {
            format!("{}, with the message \"{m}\"", status.text())
        }
        Reading::Status { status, message: None } => status.text(),
        Reading::Cleared => "the agent withdrew its status".to_string(),
        Reading::Task => "about one of its background tasks, so not used for its status".to_string(),
        Reading::NotUnderstood(why) => format!("not understood, because {why}"),
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
        app.ws.start_new("cmd", here).unwrap();
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
        assert!(app.quit);
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
    fn every_question_fits_its_keys_into_a_narrow_bar() {
        // Found by the end-to-end run: with four agents the quit question
        // was cut off before "y / n".
        let (mut app, _) = app_with_agent();
        for i in 0..6 {
            app.ws.start_new("cmd", std::env::temp_dir().join(format!("a-rather-long-folder-name-{i}"))).ok();
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
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)));
        app.on_paste("y".into());
        assert!(!app.quit);
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)));
    }

    #[test]
    fn history_scrolls_and_any_other_key_returns_without_reaching_the_agent() {
        let (mut app, sent) = app_with_agent();
        let lines: String = (1..=30).map(|i| format!("line {i}\r\n")).collect();
        output(&mut app, lines.as_bytes());

        app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::SHIFT));
        assert!(matches!(app.mode, Mode::History));
        assert!(app.bar().starts_with("HISTORY of cmd"), "{}", app.bar());
        assert!(app.ws.focused().unwrap().term.scrolled_back() > 0);

        app.on_key(press(KeyCode::Home));
        assert_eq!(app.ws.focused().unwrap().term.screen().rows(0, 40).next().unwrap(), "line 1");

        app.on_key(press(KeyCode::Char('z')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.ws.focused().unwrap().term.scrolled_back(), 0);
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
        assert!(app.bar().starts_with("STATUS REPORT of cmd"), "{}", app.bar());
        let text = app.status_report_text();
        assert!(text.contains("only for Claude Code"), "{text}");
        app.on_key(press(KeyCode::Char('z')));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(sent.borrow().is_empty(), "the closing key reached the agent: {:?}", sent.borrow());
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
        app.ws.start_new("claude", std::env::temp_dir()).unwrap();
        let report =
            format!("\x1b]7501;?\x07\x1b]7501;state=working:app=claude-code:msg={}\x07", "QUJD".repeat(600));
        app.on_agent_event(AgentEvent::Output(AgentId(2), report.into_bytes()));
        let text = app.status_report_text();
        assert!(text.contains("The sidebar shows: Working."), "{text}");
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
            app.ws.start_new("claude", folder).unwrap();
            report(&mut app, u32::from(i) + 1, "?");
        }
        app
    }

    fn report(app: &mut App<Fake>, id: u32, body: &str) {
        app.on_agent_event(AgentEvent::Output(AgentId(id), format!("\x1b]7501;{body}\x07").into_bytes()));
    }

    const PERMISSION: &str = "state=blocked:app=claude-code:kind=permission";

    #[test]
    fn an_agent_behind_that_needs_you_is_named_with_the_keys_to_reach_it() {
        let mut app = app_with_claude_agents(3);
        // The last one started is in front: c. Go round to a.
        app.ws.focus_next();
        assert_eq!(app.ws.focused().unwrap().label(), "Claude Code (a)");
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
        app.ws.focus_next();
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
        assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)));
        let bar = app.bar();
        assert!(bar.starts_with("Quit and stop 2 running agents? y / n"), "{bar}");
        assert!(bar.contains("Not finished or not known: Claude Code (b) (Working)."), "{bar}");
        assert!(!bar.contains("Claude Code (a)"), "{bar}");
        app.on_key(press(KeyCode::Char('n')));
        assert!(!app.quit);

        // Done, Idle and Error: nothing is in progress, so it quits at once.
        report(&mut app, 2, "state=error:app=claude-code");
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.quit);
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
            assert!(matches!(app.mode, Mode::Confirm(Confirm::Quit)), "{body:?}");
        }
        let (mut app, _) = app_with_agent();
        app.on_key(press(KeyCode::F(12)));
        app.on_key(press(KeyCode::Char('q')));
        assert!(app.bar().contains("(Unknown: not Claude Code)"), "{}", app.bar());
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
        let text = app.unknown_notice(status::Why::NoReport);
        assert!(text.contains("conpty.dll and OpenConsole.exe are not beside lillignode.exe"), "{text}");
        assert!(text.ends_with("Reinstall Lillignode to fix it."), "{text}");
        app.status_off_switch = Some("1".into());
        let text = app.unknown_notice(status::Why::NoReport);
        assert!(
            text.starts_with("Status shows Unknown: CLAUDE_CODE_DISABLE_TERMINAL_TITLE is set here."),
            "{text}"
        );
        app.status_off_switch = None;
        app.newer_pty_missing = false;
        let text = app.unknown_notice(status::Why::NotAsked);
        assert!(text.starts_with("Status shows Unknown: Claude Code is not reporting it."), "{text}");
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
            for why in [status::Why::NotAsked, status::Why::NoReport, status::Why::NotUnderstood] {
                let text = app.unknown_notice(why);
                assert!(text.chars().count() <= room, "{} characters: {text}", text.chars().count());
            }
        }
    }

    #[test]
    fn a_notice_after_the_needs_you_sentence_keeps_its_last_instruction() {
        // Found before release: with the needs-you sentence first, a
        // long notice after it lost its end, which is where it says what to do.
        let room = 2 * usize::from(crate::ui::MIN_COLS);
        let mut app = app_with_claude_agents(3);
        app.ws.focus_next();
        report(&mut app, 3, PERMISSION);
        let waiting = app.needs_you_notice().unwrap();
        let unknown = app.unknown_notice(status::Why::NoReport);
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
        app.ws.focus_next();
        report(&mut app, 2, PERMISSION);
        app.notice = Some(app.unknown_notice(status::Why::NoReport));
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
        let id = app.ws.entries()[index].saved.conversation.clone().unwrap();
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
        app.ws.tick(Instant::now() + cost::LOOK_EVERY * nth);
    }

    #[test]
    fn cost_is_a_number_pending_or_unknown_and_the_total_says_what_it_leaves_out() {
        let folder = claude_folder("states");
        let mut app = app_with_claude_agents(2);
        app.ws.start_new("cmd", std::env::temp_dir()).unwrap();
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  cost pending"], "nothing has been looked at");
        app.ws.set_claude_folder(Some(folder.clone()));
        look(&mut app, 1);
        let shown = |app: &App<Fake>| app.ws.entries().iter().map(|e| app.cost_short(e)).collect::<Vec<_>>();
        assert_eq!(shown(&app), ["cost pending", "cost pending", "cost unknown"]);
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  cost pending"]);

        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 2);
        assert_eq!(shown(&app), ["32.0k tokens", "cost pending", "cost unknown"]);
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  32.0k tokens", "  not counted: 2 agents"]);

        record_reply(&app, &folder, 1, "claude-opus-5-5", REPLY);
        look(&mut app, 3);
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  64.0k tokens", "  not counted: 1 agent"]);

        // A reply that is not understood: unknown, and out of the total.
        record_reply(&app, &folder, 1, "claude-opus-5-5", "{\"type\":\"assistant\",\"message\":{}}");
        look(&mut app, 4);
        assert_eq!(shown(&app), ["32.0k tokens", "cost unknown", "cost unknown"]);
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  32.0k tokens", "  not counted: 2 agents"]);
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
        app.ws.set_claude_folder(Some(folder.clone()));
        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        record_reply(&app, &folder, 1, "claude-opus-5-5", REPLY);
        look(&mut app, 1);
        let first = |app: &App<Fake>| app.cost_short(&app.ws.entries()[0]);
        assert_eq!(first(&app), "32.0k tokens");
        app.config.billing = Billing::Subscription;
        assert_eq!(first(&app), "32.0k tokens");
        app.config.billing = Billing::Key;
        assert_eq!(first(&app), "est. $0.07");
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  est. $0.14"]);

        // One agent on a model with no price: tokens for it, and tokens for
        // the total, never dollars that leave it out.
        record_reply(&app, &folder, 1, "claude-new-9", &REPLY.replace("msg_1", "msg_2"));
        look(&mut app, 2);
        assert_eq!(app.cost_short(&app.ws.entries()[1]), "64.0k tokens");
        assert_eq!(app.cost_total(), vec![" ALL AGENTS  96.0k tokens"]);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn the_status_report_explains_the_cost_in_every_state() {
        let folder = claude_folder("panel");
        let mut app = app_with_claude_agents(1);
        app.ws.set_claude_folder(Some(folder.clone()));
        look(&mut app, 1);
        let text = app.status_report_text();
        assert!(text.contains("Cost: pending. Nothing to read yet"), "{text}");
        assert!(text.contains("once something has been said in it"), "{text}");

        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 2);
        let text = app.status_report_text();
        assert!(
            text.contains("Cost of this conversation so far, including before today: 32,000 tokens."),
            "{text}"
        );
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
        assert!(text.contains("  claude-new-9: 32,000"), "by model once there is more than one: {text}");
        assert!(text.contains("No estimate in dollars"), "{text}");
        assert!(text.contains("has no price for claude-new-9"), "{text}");
        assert!(!text.contains('$'), "no dollar figure without a price for every model: {text}");

        record_reply(&app, &folder, 0, "x", "{\"type\":\"assistant\",\"message\":{\"model\":\"m\"}}");
        look(&mut app, 4);
        let text = app.status_report_text();
        assert!(text.contains("Cost: unknown. Line 3 of Claude Code's record of this conversation was not understood: the reply has no id."), "{text}");
        assert!(
            text.contains("Lillignode shows no total rather than part of one. Nothing was changed."),
            "{text}"
        );
        assert!(text.contains("Type /usage in the agent"), "{text}");

        // Said once, in the notice rows, with where the reason is; and it
        // never replaces a notice already up.
        app.notice = Some("Earlier.".into());
        app.unknown_notice_given = true;
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
        app.notice = None;
        look(&mut app, 1);
        app.after_events();
        assert_eq!(app.notice, None);
        assert!(app
            .status_report_text()
            .contains("Cost: unknown. Lillignode reads cost only for Claude Code."));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn the_sidebar_draws_each_agents_cost_beside_its_folder_and_the_total_at_the_foot() {
        use ratatui::backend::TestBackend;
        let folder = claude_folder("drawn");
        let mut app = app_with_claude_agents(2);
        app.ws.set_claude_folder(Some(folder.clone()));
        record_reply(&app, &folder, 0, "claude-opus-5-5", REPLY);
        look(&mut app, 1);
        let mut term = ratatui::Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let row = |y: u16| (0..27).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        assert_eq!(row(2), "  a           32.0k tokens ");
        assert_eq!(row(5), "  b           cost pending ");
        // The sidebar is 17 rows tall here: the total is on its last two.
        assert_eq!(row(15).trim_end(), " ALL AGENTS  32.0k tokens");
        assert_eq!(row(16).trim_end(), "  not counted: 1 agent");
        let _ = std::fs::remove_dir_all(folder);
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
        assert_eq!(row(0).trim_end(), " AGENTS  1 NEEDS YOU");
        let block = (0..20).find(|&y| row(y).starts_with(" ! NEEDS YOU: permission")).expect("block drawn");
        // Across the full sidebar width, black on yellow.
        for x in 0..27 {
            assert_eq!(buf[(x, block)].bg, Color::Yellow, "column {x}");
            assert_eq!(buf[(x, block)].fg, Color::Black, "column {x}");
        }
        let yellow_rows = (1..20).filter(|&y| buf[(0, y)].bg == Color::Yellow).count();
        assert_eq!(yellow_rows, 1, "no other sidebar row is a block");
        assert!((0..20).any(|y| row(y).starts_with("  Working")));
    }

    #[test]
    fn a_too_small_window_still_says_who_needs_you_before_the_menu_keys() {
        let mut app = app_with_claude_agents(2);
        app.too_small = true;
        assert_eq!(app.bar(), "Ctrl+Q or F12: menu, then q to quit");
        report(&mut app, 1, PERMISSION);
        assert!(app.bar().starts_with("1 agent needs you.  Ctrl+Q or F12: menu"), "{}", app.bar());
        report(&mut app, 2, PERMISSION);
        assert!(app.bar().starts_with("2 agents need you."), "{}", app.bar());
    }
}
