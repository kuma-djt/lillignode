//! Where things go on screen.
//!
//!   +-------------+-------------------------+
//!   | Lillignode  |                         |
//!   |>/\+ name    |   the agent in front    |
//!   |  folder  status                       |
//!   | /\= name    |                         |
//!   |  folder  status                       |
//!   | ALL AGENTS  |                         |
//!   +-------------+-------------------------+
//!   | notice: the latest thing to know (2)  |
//!   | bar: what can be done right now  (1)  |
//!   +---------------------------------------+
//!
//! The notice and bar rows are always reserved, so an agent's area does not
//! change size (and the agent does not redraw) when a message appears.
//!
//! Lillignode colours its own sidebar, notice and bar. It never colours an
//! agent's screen: that is drawn exactly as the agent sent it (`view`).

use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use crate::agent::{AgentBackend, Size};
use crate::app::{App, BarPart, Mode};
use crate::cost::Cost;
use crate::look::{self, Palette, Tone};
use crate::status::AgentStatus;
use crate::view;
use crate::workspace::{printable, Entry, Status};

pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 12;
const SIDEBAR: u16 = 28;
const NOTICE_ROWS: u16 = 2;

/// Where an agent's name starts on its row: the marker for the agent in
/// front, the hat, the sign, and a space.
const NAME_AT: usize = 5;

/// Where the lines under the name start. Less than the name's indent: the
/// folder and the status share one narrow line and need the room.
const DETAIL_AT: usize = 2;

/// The least of a folder's name kept beside a long status.
const FOLDER_KEPT: usize = 6;

pub struct Areas {
    pub sidebar: Rect,
    pub agent: Rect,
    pub notice: Rect,
    pub bar: Rect,
}

/// None when the window is too small to show an agent and the sidebar together.
pub fn areas(window: Rect) -> Option<Areas> {
    if window.width < MIN_COLS || window.height < MIN_ROWS {
        return None;
    }
    let body = window.height - NOTICE_ROWS - 1;
    Some(Areas {
        sidebar: Rect::new(window.x, window.y, SIDEBAR, body),
        agent: Rect::new(window.x + SIDEBAR, window.y, window.width - SIDEBAR, body),
        notice: Rect::new(window.x, window.y + body, window.width, NOTICE_ROWS),
        bar: Rect::new(window.x, window.y + body + NOTICE_ROWS, window.width, 1),
    })
}

/// The least width each agent gets when two are side by side.
pub const PAIR_MIN_COLS: u16 = 40;

/// The narrowest window that shows two agents side by side: the sidebar,
/// two agents, and the rule between them.
pub const TWO_MIN_COLS: u16 = SIDEBAR + 2 * PAIR_MIN_COLS + 1;

/// Where two agents side by side go. Each has a heading row saying which
/// agent it is and whether typing goes to it.
pub struct Panes {
    pub left_head: Rect,
    pub left: Rect,
    pub rule: Rect,
    pub right_head: Rect,
    pub right: Rect,
}

/// True when the window has room for two agents side by side.
pub fn fits_two(window: Rect) -> bool {
    window.width >= TWO_MIN_COLS && areas(window).is_some()
}

/// Both agents get the same width, so one size does for every agent. A
/// column left over goes unused on the right.
pub fn panes(agent: Rect) -> Panes {
    let half = agent.width.saturating_sub(1) / 2;
    let below = agent.height.saturating_sub(1);
    let right_x = agent.x + half + 1;
    Panes {
        left_head: Rect::new(agent.x, agent.y, half, 1),
        left: Rect::new(agent.x, agent.y + 1, half, below),
        rule: Rect::new(agent.x + half, agent.y, 1, agent.height),
        right_head: Rect::new(right_x, agent.y, half, 1),
        right: Rect::new(right_x, agent.y + 1, half, below),
    }
}

/// The size agents are given: the whole agent area, or with `two`, one of
/// the two side by side. When the window is too small to draw, agents keep
/// a minimal size rather than being told to squeeze into nothing. When two
/// are asked for and do not fit, agents get the size for one.
pub fn agent_size(window: Rect, two: bool) -> Size {
    match areas(window) {
        Some(a) if two && fits_two(window) => {
            let panes = panes(a.agent);
            Size { rows: panes.left.height, cols: panes.left.width }
        }
        Some(a) => Size { rows: a.agent.height, cols: a.agent.width },
        None => Size { rows: MIN_ROWS - NOTICE_ROWS - 1, cols: MIN_COLS - SIDEBAR },
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(width.saturating_sub(1)).collect();
        if width > 0 {
            s.push('…');
        }
        s
    }
}

/// The words for an agent's status on the second line of its row: the
/// short status only. Its reason is in the notice and the status report.
fn status_words(entry: &Entry) -> String {
    match entry.reported() {
        // In capitals: with its block, the loudest thing on screen.
        Some(AgentStatus::NeedsYou(_)) => "NEEDS YOU".to_string(),
        Some(reported) => reported.short(),
        None => entry.status.text(),
    }
}

/// An agent's two lines: its gnome mark and name, then its folder and
/// status. `width` is the whole sidebar row.
fn agent_rows(entry: &Entry, focused: bool, width: usize, clock: u64, p: Palette) -> [Line<'static>; 2] {
    let reported = entry.reported();
    let (sign, tone) = look::sign(reported.as_ref(), &entry.status, clock);
    let room = width.saturating_sub(NAME_AT);
    // The agent in front: a marker, and its name in copper.
    let name_style = if focused { p.copper() } else { Style::default() };
    let first = Line::from(vec![
        Span::styled(if focused { ">" } else { " " }, p.copper()),
        Span::styled(look::HAT, p.copper()),
        Span::styled(sign.to_string(), p.tone(tone)),
        Span::raw(" "),
        Span::styled(clip(&entry.name(), room), name_style),
    ]);

    // The folder is how agents with the same name are told apart, so some
    // of it is always kept; the status has the rest of the line.
    let room = width.saturating_sub(DETAIL_AT);
    let folder = entry.folder_name();
    let kept = folder.chars().count().min(FOLDER_KEPT);
    let status = clip(&status_words(entry), room.saturating_sub(kept + 2));
    let folder = clip(&folder, room.saturating_sub(status.chars().count() + 2));
    let indent = " ".repeat(DETAIL_AT);
    let second = if tone == Tone::NeedsYou {
        // A solid block across the sidebar, the only row ever drawn so.
        let text = format!("{indent}{folder}  {status}");
        Line::from(Span::styled(format!("{:<width$}", clip(&text, width)), p.needs_you_block()))
    } else {
        // Colour is never the only signal: the sign and the word say it too.
        Line::from(vec![
            Span::raw(indent),
            Span::styled(folder, p.dim()),
            Span::raw("  "),
            Span::styled(status, p.tone(tone)),
        ])
    };
    [first, second]
}

/// The rows that stand for agents the sidebar has no room for: how many,
/// and where, in words; and when any of them needs you, who, on a yellow
/// row, so an agent that needs you is never out of sight without a pointer.
fn more_rows(hidden: &[Entry], where_: &str, width: usize, p: Palette) -> Vec<Line<'static>> {
    let mut rows =
        vec![Line::from(Span::styled(clip(&format!(" {} more {where_}", hidden.len()), width), p.dim()))];
    let waiting: Vec<&Entry> = hidden.iter().filter(|e| e.needs_you().is_some()).collect();
    let who = match waiting.as_slice() {
        [] => return rows,
        [one] => format!("{}{} NEEDS YOU", " ".repeat(DETAIL_AT), one.label()),
        many => format!("{}{} of them NEED YOU", " ".repeat(DETAIL_AT), many.len()),
    };
    rows.push(Line::from(Span::styled(format!("{:<width$}", clip(&who, width)), p.needs_you_block())));
    rows
}

/// The rows a "more" pointer takes for these hidden agents: none when
/// there are none, two when one of them needs you, otherwise one.
fn more_height(needs: &[bool]) -> usize {
    match (needs.is_empty(), needs.contains(&true)) {
        (true, _) => 0,
        (false, true) => 2,
        (false, false) => 1,
    }
}

/// Which agents the sidebar shows, first to last, in `room` rows, `per`
/// rows an agent. All of them when they fit. Otherwise the list scrolls only
/// as far as it must to keep the agent in front on screen (and, with two
/// side by side, the one `beside` it when there is room), starting as near
/// as it can to where it started last time (`top`). Hidden agents above and
/// below are pointed to by "more" rows, which take room too.
pub(crate) fn shown_agents(
    needs: &[bool],
    per: usize,
    room: usize,
    front: usize,
    beside: Option<usize>,
    top: usize,
) -> (usize, usize) {
    let count = needs.len();
    if count == 0 || count * per <= room {
        return (0, count.saturating_sub(1));
    }
    let fits = |a: usize, b: usize| {
        more_height(&needs[..a]) + (b - a + 1) * per + more_height(&needs[b + 1..]) <= room
    };
    // For each first row that keeps the front on screen, the most it can show.
    let windows: Vec<(usize, usize)> = (0..=front.min(count - 1))
        .filter_map(|a| (front..count).rev().find(|&b| fits(a, b)).map(|b| (a, b)))
        .collect();
    let with_beside: Vec<(usize, usize)> =
        windows.iter().copied().filter(|&(a, b)| beside.is_none_or(|i| a <= i && i <= b)).collect();
    let choices = if with_beside.is_empty() { windows } else { with_beside };
    choices.into_iter().min_by_key(|&(a, _)| (a.abs_diff(top), a)).unwrap_or((front, front))
}

fn draw_sidebar<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>, p: Palette) {
    let panel = p.panel();
    let row = (area.width - 1) as usize;
    frame.render_widget(Block::default().style(panel), area);

    let mut heading = vec![Span::styled(" Lillignode", p.copper())];
    // Shown even when the row of the agent that needs you is off screen.
    let waiting = app.needs_you_count();
    if waiting > 0 {
        heading.push(Span::raw("  "));
        heading.push(Span::styled(format!("{waiting} NEEDS YOU"), p.needs_you_text()));
    }
    let mut lines = vec![Line::from(heading)];
    if app.ws.entries().is_empty() {
        lines.push(Line::from(Span::styled(" No agents yet. Press n.", p.dim())));
    }
    // Which agents fit. The total of cost at the foot gives way first.
    let entries = app.ws.entries();
    let per = if app.cost_shown() { 3 } else { 2 };
    let total = app.cost_total();
    let room = (area.height as usize).saturating_sub(1);
    let total_fits = entries.len() * per + total.len() <= room;
    let needs: Vec<bool> = entries.iter().map(|e| e.needs_you().is_some()).collect();
    let front = app.ws.focused_index().unwrap_or(0);
    let beside = app.ws.pair().map(|pair| if pair.left == front { pair.right } else { pair.left });
    let (first, last) = shown_agents(&needs, per, room, front, beside, app.sidebar_top.get());
    app.sidebar_top.set(first);
    if first > 0 {
        lines.extend(more_rows(&entries[..first], "above", row, p));
    }
    for (i, entry) in entries.iter().enumerate().take(last + 1).skip(first) {
        let focused = app.ws.focused_index() == Some(i);
        lines.extend(agent_rows(entry, focused, row, app.clock, p));
        if app.cost_shown() {
            // A third line, only when the user has switched cost on.
            let room = row.saturating_sub(DETAIL_AT);
            let known = matches!(entry.cost(), Cost::Known(_));
            // Past the user's warning: PAST WARNING leads the figure, and
            // it is bold. Not yellow, which means only Needs you.
            let style = match (known, app.ws.over_cost_warning(entry)) {
                (_, Some(_)) => Style::default().add_modifier(Modifier::BOLD),
                (true, None) => Style::default(),
                (false, None) => p.dim(),
            };
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(DETAIL_AT)),
                Span::styled(clip(&app.cost_short(entry), room), style),
            ]));
        }
    }
    if last + 1 < entries.len() {
        lines.extend(more_rows(&entries[last + 1..], "below", row, p));
    }
    frame.render_widget(Paragraph::new(lines).style(panel), Rect { width: area.width - 1, ..area });
    // The total stays at the foot of the sidebar, below every agent row.
    // With no room for it there it is not drawn; the status report has it.
    let total = if total_fits { total } else { Vec::new() };
    let total: Vec<Line> =
        total.iter().map(|text| Line::from(format!("{:<row$}", clip(text, row)))).collect();
    let rows = (total.len() as u16).min(area.height);
    frame.render_widget(
        Paragraph::new(total).style(panel),
        Rect { y: area.y + area.height - rows, height: rows, width: area.width - 1, ..area },
    );
    // A plain rule between the sidebar and the agent.
    let rule: Vec<Line> = (0..area.height).map(|_| Line::from(Span::styled("│", p.dim()))).collect();
    frame.render_widget(
        Paragraph::new(rule).style(panel),
        Rect::new(area.x + area.width - 1, area.y, 1, area.height),
    );
}

/// Where a message goes inside the agent's area: two columns in from each
/// side, one row down.
fn padded(area: Rect) -> Rect {
    Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), area.height.saturating_sub(1))
}

fn message(frame: &mut Frame, area: Rect, text: String) {
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), padded(area));
}

/// The status report as it is drawn in this window: its lines, wrapped at
/// spaces, and how many of them are shown at once. When they do not all
/// fit, one row fewer is shown, for the line that says where the reader is.
/// None when the window is too small to draw it.
pub fn report_view(text: &str, window: Rect) -> Option<(Vec<String>, usize)> {
    let area = padded(areas(window)?.agent);
    let width = area.width as usize;
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let chars: Vec<char> = paragraph.chars().collect();
        let wrapped = wrap_at_spaces(&chars, width);
        if wrapped.is_empty() {
            lines.push(String::new());
        }
        lines.extend(wrapped.iter().map(|&(s, e)| chars[s..e].iter().collect::<String>()));
    }
    // The text ends with a line break, which leaves nothing to show.
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let rows = area.height as usize;
    let shown = if lines.len() > rows { rows.saturating_sub(1).max(1) } else { rows };
    Some((lines, shown))
}

/// The status report, from line `top`, with "line 21 of 46" at its foot
/// when it does not all fit.
fn draw_report<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>, p: Palette) {
    let Some((lines, shown)) = report_view(&app.status_report_text(), frame.area()) else { return };
    let top = app.report_top.min(lines.len().saturating_sub(shown));
    let mut rows: Vec<Line> = lines.iter().skip(top).take(shown).map(|l| Line::from(l.clone())).collect();
    if lines.len() > shown {
        let last = (top + shown).min(lines.len());
        rows.push(Line::from(Span::styled(
            format!("lines {} to {last} of {}", top + 1, lines.len()),
            p.dim(),
        )));
    }
    frame.render_widget(Paragraph::new(rows), padded(area));
}

/// No agents are open: the gnome, what Lillignode is in two sentences, and
/// the key that starts an agent. The key comes first, so a short window
/// cuts the pitch and never the way forward.
fn draw_welcome<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>, p: Palette) {
    let gnome_width = look::GNOME.iter().map(|row| row.len()).max().unwrap_or(0) as u16;
    let gnome_fits = area.width >= gnome_width + 30 && area.height > look::GNOME.len() as u16;
    let text_x = if gnome_fits { area.x + 2 + gnome_width + 3 } else { area.x + 2 };
    if gnome_fits {
        let rows: Vec<Line> = look::GNOME
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let style = if i < look::GNOME_HAT_ROWS { p.copper() } else { Style::default() };
                Line::from(Span::styled(*row, style))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(rows),
            Rect::new(area.x + 2, area.y + 1, gnome_width, look::GNOME.len() as u16),
        );
    }
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let text = vec![
        Line::from(vec![
            Span::styled("No agents yet. ", bold),
            Span::raw("Press "),
            Span::styled("n", p.key()),
            Span::raw(" to start one."),
        ]),
        Line::default(),
        Line::from(vec![
            Span::styled("Lillignode", p.copper()),
            Span::raw(
                " runs several AI coding agents at once in one terminal window and shows what each \
                 one is doing. Each agent works in its own folder with the account it already has; \
                 Lillignode never asks for your keys.",
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            format!("{} opens Lillignode's menu.", app.config.menu_keys_label()),
            p.dim(),
        )),
    ];
    let width = (area.x + area.width).saturating_sub(text_x + 2);
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }),
        Rect::new(text_x, area.y + 1, width, area.height.saturating_sub(1)),
    );
}

/// What an agent that was not started shows in place of its screen. A
/// reason that already says what to do is not followed by a second, generic
/// way out that could contradict it.
fn not_started_text(entry: &Entry, why: &str) -> String {
    let next = if why.contains(crate::agent::NOTHING_RUN) {
        "It is still in your workspace."
    } else {
        "Nothing was changed, and it is still in your workspace.\n\n\
         Once the cause is fixed: the menu key, then r starts it again. Or the menu key, then x removes it."
    };
    format!(
        "{} was not started: {why}.\n\n{next}\n\nCommand: {}\nFolder:  {}",
        entry.label(),
        printable(&entry.saved.command),
        printable(&entry.saved.folder.display().to_string())
    )
}

/// One agent's own screen, or why it has none.
fn draw_entry(frame: &mut Frame, area: Rect, entry: &Entry) {
    match &entry.status {
        Status::NotStarted(why) => message(frame, area, not_started_text(entry, why)),
        // An exited agent keeps its last screen, so what it said before
        // stopping can still be read.
        _ => view::draw_screen(entry.term.screen(), area, frame.buffer_mut()),
    }
}

/// The row above one of two agents side by side. It says, before
/// anything else, whether typing goes to this agent: with two on screen the
/// slip to guard against is answering the one that is not listening. The
/// agent's name comes after, where a narrow heading may cut it.
///
/// "Typing goes here" is said only when it is true: the agent is in front,
/// it is running, and Lillignode is not showing its menu, a question or
/// history, when keys are Lillignode's.
fn pane_head(entry: &Entry, front: bool, keys_go_to_agents: bool, width: usize, p: Palette) -> Line<'static> {
    let label = entry.label();
    let typing = front && keys_go_to_agents && entry.is_running();
    let (text, style) = match (entry.needs_you().is_some(), front) {
        (true, false) => (" ! NEEDS YOU - you are NOT typing here", p.needs_you_block()),
        (true, true) if typing => (" > NEEDS YOU - you are typing here", p.needs_you_block()),
        (true, true) => (" > NEEDS YOU - in front, keys are Lillignode's now", p.needs_you_block()),
        (false, true) if typing => (" > TYPING GOES HERE", p.mode()),
        (false, true) if entry.start_again_cannot_help() => {
            (" > NOT STARTED - menu, then x remove", p.mode())
        }
        (false, true) if !entry.is_running() => (" > STOPPED - menu, then r start again, x remove", p.mode()),
        (false, true) => (" > IN FRONT - keys are Lillignode's now", p.mode()),
        (false, false) if !entry.is_running() => (" - stopped, its last screen", p.dim()),
        (false, false) => (" - not typing here", p.dim()),
    };
    let text = format!("{text}  {label}");
    Line::from(Span::styled(format!("{:<width$}", clip(&text, width)), style))
}

/// The two agents side by side, if the user asked for two and the window
/// has room for them.
fn pair_shown<B: AgentBackend>(app: &App<B>, window: Rect) -> Option<crate::workspace::Pair> {
    app.ws.pair().filter(|_| fits_two(window))
}

fn draw_agent<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>, p: Palette) {
    if matches!(app.mode, Mode::StatusReport) {
        draw_report(frame, area, app, p);
        return;
    }
    let Some(front) = app.ws.focused() else {
        draw_welcome(frame, area, app, p);
        return;
    };
    let Some(pair) = pair_shown(app, frame.area()) else {
        draw_entry(frame, area, front);
        return;
    };
    let panes = panes(area);
    let sides = [(pair.left, panes.left_head, panes.left), (pair.right, panes.right_head, panes.right)];
    for (index, head, body) in sides {
        let Some(entry) = app.ws.entries().get(index) else { continue };
        let in_front = app.ws.focused_index() == Some(index);
        let keys_go_to_agents = matches!(app.mode, Mode::Normal);
        let line = pane_head(entry, in_front, keys_go_to_agents, head.width as usize, p);
        frame.render_widget(Paragraph::new(line), head);
        draw_entry(frame, body, entry);
    }
    let rule: Vec<Line> = (0..panes.rule.height).map(|_| Line::from(Span::styled("│", p.dim()))).collect();
    frame.render_widget(Paragraph::new(rule), panes.rule);
}

/// Two rows. A notice too long for them has already had words taken out of
/// its middle (`App::notice_parts`), so what to do, which comes last, is
/// still there. The full reason for any agent that failed is also on that
/// agent's own screen.
///
/// The sentence about who needs you is yellow, the one meaning of yellow.
/// Everything else is plain text.
fn draw_notice<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>, p: Palette) {
    let width = (area.width as usize).max(1);
    let rows = area.height as usize;
    let Some(Wrapped { chars, yellow, lines: ranges }) =
        notice_lines(width, rows, |room| app.notice_parts(room))
    else {
        return;
    };
    let lines: Vec<Line> = ranges
        .iter()
        .map(|&(start, end)| {
            let split = yellow.clamp(start, end);
            Line::from(vec![
                Span::styled(chars[start..split].iter().collect::<String>(), p.needs_you_text()),
                Span::raw(chars[split..end].iter().collect::<String>()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// A notice as drawn: its characters, how many of them (from the start)
/// are yellow, and its lines as ranges of the characters.
struct Wrapped {
    chars: Vec<char>,
    yellow: usize,
    lines: Vec<(usize, usize)>,
}

/// The notice as it is drawn: its characters, how many of them are the
/// yellow sentence, and the lines, each a range of those characters,
/// broken at spaces. Breaking at spaces costs some room, so when the
/// words do not fit in `rows` lines the notice is asked for again, shorter,
/// until they do; it is shortened in its middle, as always, so what to do
/// is still at the end.
fn notice_lines(
    width: usize,
    rows: usize,
    parts: impl Fn(usize) -> Option<(String, String)>,
) -> Option<Wrapped> {
    let mut room = width * rows;
    loop {
        let (waiting, rest) = parts(room)?;
        let joined = match (waiting.is_empty(), rest.is_empty()) {
            (false, false) => format!("{waiting}  {rest}"),
            _ => format!("{waiting}{rest}"),
        };
        let chars: Vec<char> = clip(&joined, room).chars().collect();
        let mut lines = wrap_at_spaces(&chars, width);
        if lines.len() <= rows || room == 0 {
            lines.truncate(rows);
            return Some(Wrapped { yellow: waiting.chars().count(), chars, lines });
        }
        let over: usize = lines[rows..].iter().map(|(s, e)| e - s + 1).sum();
        room = room.saturating_sub(over.max(1));
    }
}

/// Lines of at most `width` characters, broken at spaces, as ranges of
/// `chars`. The spaces at a break are dropped. Only a word longer than a
/// whole line is broken inside itself.
pub(crate) fn wrap_at_spaces(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if !lines.is_empty() {
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
            if start == chars.len() {
                break;
            }
        }
        if chars.len() - start <= width {
            lines.push((start, chars.len()));
            break;
        }
        // `limit` is the first character that does not fit on this line.
        let limit = start + width;
        match (start + 1..=limit).rev().find(|&i| chars[i] == ' ') {
            Some(space) => {
                let mut end = space;
                while end > start && chars[end - 1] == ' ' {
                    end -= 1;
                }
                lines.push((start, end));
                start = space;
            }
            None => {
                lines.push((start, limit));
                start = limit;
            }
        }
    }
    lines
}

/// The bar outside a question: the mode it is in, keys emphasised, what
/// each key does dim.
fn bar_line(parts: &[(BarPart, String)], width: usize, p: Palette) -> Line<'static> {
    let mut spans = Vec::new();
    let mut left = width;
    for (part, text) in parts {
        if left == 0 {
            break;
        }
        let shown = clip(text, left);
        left -= shown.chars().count();
        let style = match part {
            BarPart::Mode => p.mode(),
            BarPart::Key => p.key(),
            BarPart::Label => p.dim(),
        };
        spans.push(Span::styled(shown, style));
    }
    spans.push(Span::raw(" ".repeat(left)));
    Line::from(spans)
}

pub fn draw<B: AgentBackend>(frame: &mut Frame, app: &App<B>) {
    let window = frame.area();
    let p = Palette(app.colors);
    let Some(a) = areas(window) else {
        let running = app.ws.running_labels().len();
        // The bar text is included so the way to the menu, and any question
        // Lillignode is asking, stay visible even here.
        let text = format!(
            "This window is too small for Lillignode ({} by {}). Make it at least {MIN_COLS} columns \
             by {MIN_ROWS} rows.{}\n\n{}",
            window.width,
            window.height,
            if running > 0 {
                " Your agents are still running. Typing is not passed to them until then."
            } else {
                ""
            },
            app.bar()
        );
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }), window);
        return;
    };

    draw_sidebar(frame, a.sidebar, app, p);
    draw_agent(frame, a.agent, app, p);
    draw_notice(frame, a.notice, app, p);

    let width = a.bar.width as usize;
    // While text is being typed, keep the typing cursor in view even if what
    // was typed is wider than the window.
    let cursor = app.bar_cursor();
    let skip = cursor.map_or(0, |c| (c + 1).saturating_sub(width));
    if matches!(app.mode, Mode::NewAgent(_) | Mode::Confirm(_)) {
        // Lillignode is waiting for the user: make that hard to miss.
        let bar = clip(&app.bar().chars().skip(skip).collect::<String>(), width);
        frame.render_widget(Paragraph::new(format!("{bar:<width$}")).style(p.question()), a.bar);
    } else {
        frame.render_widget(Paragraph::new(bar_line(&app.bar_parts(), width, p)).style(p.panel()), a.bar);
    }

    if let Some(cursor) = cursor {
        frame.set_cursor_position(Position::new(a.bar.x + (cursor - skip) as u16, a.bar.y));
    } else if matches!(app.mode, Mode::Normal) {
        if let Some(entry) = app.ws.focused().filter(|e| e.is_running()) {
            let screen = entry.term.screen();
            // While reading history the cursor's row is not on screen.
            if !screen.hide_cursor() && entry.term.scrolled_back() == 0 {
                // With two agents side by side, the cursor is in the one
                // that typing goes to.
                let at = match pair_shown(app, window) {
                    Some(pair) if app.ws.focused_index() == Some(pair.right) => panes(a.agent).right,
                    Some(_) => panes(a.agent).left,
                    None => a.agent,
                };
                let (row, col) = screen.cursor_position();
                if row < at.height && col < at.width {
                    frame.set_cursor_position(Position::new(at.x + col, at.y + row));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(text: &str, width: usize) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        wrap_at_spaces(&chars, width).iter().map(|&(s, e)| chars[s..e].iter().collect()).collect()
    }

    #[test]
    fn the_sidebar_shows_every_agent_that_fits_and_otherwise_follows_the_one_in_front() {
        let calm = [false; 10];
        // All fit: all shown, from the first.
        assert_eq!(shown_agents(&calm[..4], 2, 8, 3, None, 0), (0, 3));
        // Ten agents in 8 rows, two rows each, the first in front: three,
        // then one row saying more are below.
        assert_eq!(shown_agents(&calm, 2, 8, 0, None, 0), (0, 2));
        // The front moves down: the list moves only as far as it must.
        assert_eq!(shown_agents(&calm, 2, 8, 3, None, 0), (1, 3));
        assert_eq!(shown_agents(&calm, 2, 8, 9, None, 1), (7, 9));
        // And back up: it keeps where it was while the front is in view.
        assert_eq!(shown_agents(&calm, 2, 8, 8, None, 7), (7, 9));
        assert_eq!(shown_agents(&calm, 2, 8, 5, None, 7), (5, 7));
        // An agent that needs you, hidden, takes a second row to be named.
        let mut needs = calm;
        needs[9] = true;
        let (first, last) = shown_agents(&needs, 2, 8, 0, None, 0);
        assert_eq!((first, last), (0, 2), "one more row below: 3 agents and 2 rows still fit");
        assert_eq!(2 * 3 + more_height(&needs[last + 1..]), 8);
        // With cost on, three rows an agent: the front is always shown.
        assert_eq!(shown_agents(&needs, 3, 8, 5, None, 0), (5, 5));
        // Two side by side: the one beside the front is kept too, when it fits.
        assert_eq!(shown_agents(&calm, 2, 8, 4, Some(5), 0), (3, 5));
        let (first, last) = shown_agents(&calm, 3, 8, 4, Some(9), 0);
        assert!(first <= 4 && (4..9).contains(&last), "no room for both: the front is kept");
        assert_eq!(shown_agents(&[], 2, 8, 0, None, 0), (0, 0));
    }

    #[test]
    fn notices_wrap_at_spaces_and_never_inside_a_word() {
        assert_eq!(
            wrapped("Nothing was changed. The window is too narrow.", 22),
            vec!["Nothing was changed.", "The window is too", "narrow."]
        );
        // A break that falls exactly on a space, and runs of spaces, drop
        // the spaces rather than starting a line with them.
        assert_eq!(wrapped("abc def", 3), vec!["abc", "def"]);
        assert_eq!(wrapped("needs you.  Nothing", 10), vec!["needs you.", "Nothing"]);
        // Only a word longer than a whole line is broken.
        assert_eq!(wrapped("abcdefgh ij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrapped("short", 40), vec!["short"]);
        assert!(wrapped("", 10).is_empty());
        // No line is ever wider than asked.
        let text = "Claude Code (b) needs you: permission. Ctrl+Q then Tab to go to it.  This window is \
                    too narrow for two agents side by side. Nothing was changed.";
        for width in 1..60 {
            for line in wrapped(text, width) {
                assert!(line.chars().count() <= width, "{width}: {line:?}");
            }
        }
    }

    #[test]
    fn a_notice_that_no_longer_fits_once_wrapped_is_shortened_in_its_middle() {
        let long = "Claude Code (a) stopped with an error (exit code 3). Nothing was changed. It is still \
                    in your workspace. Switch to it, then r to start it again or x to remove it.";
        let parts = |room: usize| Some((String::new(), crate::app::keep_end(long, room)));
        let Wrapped { chars, lines, .. } = notice_lines(30, 3, parts).unwrap();
        assert!(lines.len() <= 3, "{lines:?}");
        let shown: Vec<String> = lines.iter().map(|&(s, e)| chars[s..e].iter().collect()).collect();
        assert!(shown.last().unwrap().ends_with("x to remove it."), "{shown:?}");
        for line in &shown {
            assert!(line.chars().count() <= 30, "{line:?}");
            assert!(!line.starts_with(' ') && !line.ends_with(' '), "{line:?}");
        }
    }
}
