//! Where things go on screen.
//!
//!   +-----------+---------------------------+
//!   | AGENTS    |                           |
//!   | > name    |   the focused agent       |
//!   |   folder  cost                        |
//!   |   status  |                           |
//!   | ALL AGENTS|                           |
//!   +-----------+---------------------------+
//!   | notice: the latest thing to know (2)  |
//!   | bar: what can be done right now  (1)  |
//!   +---------------------------------------+
//!
//! The notice and bar rows are always reserved, so an agent's area does not
//! change size (and the agent does not redraw) when a message appears.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::agent::{AgentBackend, Size};
use crate::app::{App, Mode};
use crate::cost::Cost;
use crate::status::AgentStatus;
use crate::view;
use crate::workspace::{Entry, Status};

pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 12;
const SIDEBAR: u16 = 28;
const NOTICE_ROWS: u16 = 2;

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

/// The size agents are given. When the window is too small to draw, agents
/// keep a minimal size rather than being told to squeeze into nothing.
pub fn agent_size(window: Rect) -> Size {
    match areas(window) {
        Some(a) => Size { rows: a.agent.height, cols: a.agent.width },
        None => Size { rows: MIN_ROWS - NOTICE_ROWS - 1, cols: MIN_COLS - SIDEBAR },
    }
}

fn status_style(status: &AgentStatus) -> Style {
    match status {
        AgentStatus::Working | AgentStatus::WorkingQuiet(_) => Style::default().fg(Color::Green),
        AgentStatus::NeedsYou(_) => needs_you_style(),
        AgentStatus::Error(_) => Style::default().fg(Color::Red),
        AgentStatus::Starting => Style::default().fg(Color::DarkGray),
        AgentStatus::Done | AgentStatus::Idle | AgentStatus::Unknown(_) => Style::default(),
    }
}

/// The one meaning of yellow: something is waiting for you.
fn needs_you_style() -> Style {
    Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
}

/// The status row under an agent's name. `width` is the whole sidebar row.
fn status_line(entry: &Entry, width: usize) -> Line<'static> {
    // Colour is never the only signal: the word says the same thing.
    if let Some(reported) = entry.reported() {
        if let AgentStatus::NeedsYou(kind) = &reported {
            // The loudest thing on screen: a solid block across the sidebar,
            // the only row ever drawn this way, in capitals.
            let text = match kind {
                Some(kind) => format!(" ! NEEDS YOU: {}", kind.word()),
                None => " ! NEEDS YOU".to_string(),
            };
            return Line::from(Span::styled(format!("{:<width$}", clip(&text, width)), needs_you_style()));
        }
        let style = status_style(&reported);
        return Line::from(vec![Span::raw("  "), Span::styled(clip(&reported.text(), width - 2), style)]);
    }
    let span = match &entry.status {
        Status::Running => Span::styled("Running", Style::default().fg(Color::Green)),
        Status::Exited(e) if e.clean() => Span::styled("Exited", Style::default().fg(Color::DarkGray)),
        Status::Exited(e) => {
            Span::styled(format!("Exited (error {})", e.code), Style::default().fg(Color::Red))
        }
        Status::NotStarted(_) => Span::styled("Not started", Style::default().fg(Color::Red)),
    };
    Line::from(vec![Span::raw("  "), span])
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(width.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

fn draw_sidebar<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>) {
    let inner = (area.width - 2) as usize;
    let dim = Style::default().fg(Color::DarkGray);
    let row = (area.width - 1) as usize;
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let mut heading = vec![Span::styled(" AGENTS", bold)];
    // Shown even when the row of the agent that needs you is off screen.
    let waiting = app.needs_you_count();
    if waiting > 0 {
        heading.push(Span::raw("  "));
        heading.push(Span::styled(
            format!("{waiting} NEEDS YOU"),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    let mut lines = vec![Line::from(heading)];
    if app.ws.entries().is_empty() {
        lines.push(Line::from(Span::styled(" None yet. Press n.", dim)));
    }
    for (i, entry) in app.ws.entries().iter().enumerate() {
        let focused = app.ws.focused_index() == Some(i);
        let name_style =
            if focused { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
        let marker = if focused { ">" } else { " " };
        lines.push(Line::from(Span::styled(
            format!("{marker} {}", clip(&entry.saved.name, inner - 2)),
            name_style,
        )));
        // The folder on the left, what its conversation has cost on the right.
        let cost = clip(&app.cost_short(entry), inner.saturating_sub(3));
        let known = matches!(entry.cost(), Cost::Known(_));
        let folder_room = inner.saturating_sub(cost.chars().count() + 3);
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<folder_room$} ", clip(&entry.folder_name(), folder_room)), dim),
            Span::styled(cost, if known { Style::default() } else { dim }),
        ]));
        lines.push(status_line(entry, row));
    }
    frame.render_widget(Paragraph::new(lines), Rect { width: area.width - 1, ..area });
    // The total stays at the foot of the sidebar, whatever is above it.
    let total: Vec<Line> =
        app.cost_total().iter().map(|text| Line::from(format!("{:<row$}", clip(text, row)))).collect();
    let rows = (total.len() as u16).min(area.height);
    frame.render_widget(
        Paragraph::new(total),
        Rect { y: area.y + area.height - rows, height: rows, width: area.width - 1, ..area },
    );
    // A plain rule between the sidebar and the agent.
    let rule: Vec<Line> = (0..area.height).map(|_| Line::from(Span::styled("│", dim))).collect();
    frame.render_widget(Paragraph::new(rule), Rect::new(area.x + area.width - 1, area.y, 1, area.height));
}

fn message(frame: &mut Frame, area: Rect, text: String) {
    let padded =
        Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), area.height.saturating_sub(1));
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), padded);
}

fn draw_agent<B: AgentBackend>(frame: &mut Frame, area: Rect, app: &App<B>) {
    if matches!(app.mode, Mode::StatusReport) {
        message(frame, area, app.status_report_text());
        return;
    }
    match app.ws.focused() {
        None => message(
            frame,
            area,
            "No agents yet.\n\n\
             Press n to start one. Lillignode asks which agent (Enter for Claude Code) and which \
             folder it should work in.\n\n\
             Lillignode runs AI coding agents side by side. Each agent uses its own account. \
             Lillignode does not ask for your keys.\n\n\
             While an agent is running, your typing goes to it. {} opens Lillignode's menu: \
             switch agents, read back history, close, quit.\n\n\
             When you quit, your agents are remembered and reopened next time."
                .replace("{}", &app.config.menu_keys_label()),
        ),
        Some(entry) => match &entry.status {
            Status::NotStarted(why) => message(
                frame,
                area,
                format!(
                    "{} was not started: {why}.\n\n\
                     Nothing was changed, and it is still in your workspace.\n\n\
                     Fix the cause, then press r to try again. Or press x to remove it.\n\n\
                     Command: {}\nFolder:  {}",
                    entry.label(),
                    entry.saved.command,
                    entry.saved.folder.display()
                ),
            ),
            // An exited agent keeps its last screen, so what it said before
            // stopping can still be read.
            _ => view::draw_screen(entry.term.screen(), area, frame.buffer_mut()),
        },
    }
}

/// Two rows. A notice too long for them has already had words taken out of
/// its middle (`App::notice_fitted`), so what to do, which comes last, is
/// still there. The full reason for any agent that failed is also on that
/// agent's own screen.
fn draw_notice(frame: &mut Frame, area: Rect, notice: Option<&str>) {
    let Some(text) = notice else { return };
    let width = area.width as usize;
    let room = width * area.height as usize;
    let chars: Vec<char> = clip(text, room).chars().collect();
    let lines: Vec<Line> =
        chars.chunks(width.max(1)).map(|c| Line::from(c.iter().collect::<String>())).collect();
    frame.render_widget(Paragraph::new(lines).style(Style::default().fg(Color::Yellow)), area);
}

pub fn draw<B: AgentBackend>(frame: &mut Frame, app: &App<B>) {
    let window = frame.area();
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

    draw_sidebar(frame, a.sidebar, app);
    draw_agent(frame, a.agent, app);
    let notice_room = usize::from(a.notice.width) * usize::from(a.notice.height);
    draw_notice(frame, a.notice, app.notice_fitted(notice_room).as_deref());

    let width = a.bar.width as usize;
    let full = app.bar();
    // While text is being typed, keep the typing cursor in view even if what
    // was typed is wider than the window.
    let cursor = app.bar_cursor();
    let skip = cursor.map_or(0, |c| (c + 1).saturating_sub(width));
    let bar = clip(&full.chars().skip(skip).collect::<String>(), width);
    let bar_style = match app.mode {
        Mode::Normal => Style::default().add_modifier(Modifier::REVERSED),
        // Lillignode is waiting for the user: make that hard to miss.
        _ => Style::default().fg(Color::Black).bg(Color::Yellow),
    };
    frame.render_widget(Paragraph::new(format!("{bar:<width$}")).style(bar_style), a.bar);

    if let Some(cursor) = cursor {
        frame.set_cursor_position(Position::new(a.bar.x + (cursor - skip) as u16, a.bar.y));
    } else if matches!(app.mode, Mode::Normal) {
        if let Some(entry) = app.ws.focused().filter(|e| e.is_running()) {
            let screen = entry.term.screen();
            // While reading history the cursor's row is not on screen.
            if !screen.hide_cursor() && entry.term.scrolled_back() == 0 {
                let (row, col) = screen.cursor_position();
                if row < a.agent.height && col < a.agent.width {
                    frame.set_cursor_position(Position::new(a.agent.x + col, a.agent.y + row));
                }
            }
        }
    }
}
