//! Development tool, not part of the product.
//!
//! Draws Lillignode's own screens with made-up agents and writes them as one
//! HTML page, so the look can be judged without starting a real agent:
//!
//!   cargo run --example look > look.html
//!   cargo run --example look -- "Lucida Console" Welcome 16 > one.html
//!
//! The arguments, all optional: the font to show it in, a word from the
//! title of the one screen wanted, and the one colour depth wanted.
//!
//! Each screen is drawn four times: with any colour, 256 colours, 16
//! colours, and none. Nothing is started and nothing outside this process
//! is read: the agents are fakes that exist only here.

use std::path::PathBuf;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use lillignode::agent::{AgentBackend, AgentEvent, AgentId, AgentSpec, Exit, Size, StartError};
use lillignode::app::App;
use lillignode::config::Config;
use lillignode::look::Depth;
use lillignode::ui;

/// Starts nothing. Every agent "runs" and says only what this file feeds it.
#[derive(Default)]
struct Fake(u32);

impl AgentBackend for Fake {
    fn start(&mut self, _: &AgentSpec, _: Size) -> Result<AgentId, StartError> {
        self.0 += 1;
        Ok(AgentId(self.0))
    }
    fn send(&mut self, _: AgentId, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn resize(&mut self, _: AgentId, _: Size) {}
    fn close(&mut self, _: AgentId) {}
}

fn app(cols: u16, rows: u16) -> App<Fake> {
    let window = ratatui::layout::Rect::new(0, 0, cols, rows);
    App::launch(
        Fake::default(),
        ui::agent_size(window, false),
        Err("the preview saves nothing".into()),
        PathBuf::from("."),
        Config::default(),
        None,
    )
}

fn says(app: &mut App<Fake>, id: u32, text: &str) {
    app.on_agent_event(AgentEvent::Output(AgentId(id), text.as_bytes().to_vec()));
}

fn reports(app: &mut App<Fake>, id: u32, body: &str) {
    says(app, id, &format!("\x1b]7501;{body}\x07"));
}

/// Seven agents, one in each status, the last one in front.
fn busy(cols: u16, rows: u16) -> App<Fake> {
    let mut app = app(cols, rows);
    let folders = ["api", "web", "docs", "billing", "search", "infra"];
    for (i, folder) in folders.iter().enumerate() {
        app.ws.start_as_named("claude", PathBuf::from("/work").join(folder)).unwrap();
        reports(&mut app, i as u32 + 1, "?");
    }
    app.ws.start_as_named("aider", PathBuf::from("/work/tools")).unwrap();
    reports(&mut app, 1, "state=working:app=claude-code");
    reports(&mut app, 2, "state=blocked:app=claude-code:kind=permission");
    reports(&mut app, 3, "state=done:app=claude-code");
    reports(&mut app, 4, "state=idle:app=claude-code");
    reports(&mut app, 5, "state=error:app=claude-code");
    app.on_agent_event(AgentEvent::Exited(AgentId(6), Exit { code: 0 }));
    // Round to the first agent, so a working one is in front.
    app.ws.focus_next();
    says(
        &mut app,
        1,
        "\x1b[1mAn agent's own screen\x1b[m\r\n\r\nLillignode draws this exactly as the agent sent it,\r\n\
         \x1b[32mwith the agent's own colours\x1b[m and none of Lillignode's.\r\n\r\n> ",
    );
    app.notice = Some("Started Claude Code in /work/api.".into());
    app.after_events();
    app
}

fn hex(color: Color, fallback: &str) -> String {
    const NAMED: [&str; 16] = [
        "#0c0c0c", "#c50f1f", "#13a10e", "#c19c00", "#0037da", "#881798", "#3a96dd", "#cccccc", "#767676",
        "#e74856", "#16c60c", "#f9f1a5", "#3b78ff", "#b4009e", "#61d6d6", "#f2f2f2",
    ];
    let indexed = |i: u8| match i {
        0..=15 => NAMED[i as usize].to_string(),
        16..=231 => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v as u32 };
            format!("#{:02x}{:02x}{:02x}", level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let v = 8 + 10 * (i as u32 - 232);
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    };
    match color {
        Color::Reset => fallback.to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => indexed(i),
        Color::Black => indexed(0),
        Color::Red => indexed(1),
        Color::Green => indexed(2),
        Color::Yellow => indexed(3),
        Color::Blue => indexed(4),
        Color::Magenta => indexed(5),
        Color::Cyan => indexed(6),
        Color::Gray => indexed(7),
        Color::DarkGray => indexed(8),
        Color::LightRed => indexed(9),
        Color::LightGreen => indexed(10),
        Color::LightYellow => indexed(11),
        Color::LightBlue => indexed(12),
        Color::LightMagenta => indexed(13),
        Color::LightCyan => indexed(14),
        Color::White => indexed(15),
    }
}

fn html(buf: &Buffer, ground: (&str, &str)) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let (mut fg, mut bg) = (hex(cell.fg, ground.0), hex(cell.bg, ground.1));
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let weight = if cell.modifier.contains(Modifier::BOLD) { ";font-weight:bold" } else { "" };
            let symbol = match cell.symbol() {
                "<" => "&lt;".to_string(),
                ">" => "&gt;".to_string(),
                "&" => "&amp;".to_string(),
                other => other.to_string(),
            };
            out += &format!("<span style=\"color:{fg};background:{bg}{weight}\">{symbol}</span>");
        }
        out.push('\n');
    }
    out
}

fn draw(app: &App<Fake>, cols: u16, rows: u16) -> Buffer {
    let mut term = ratatui::Terminal::new(TestBackend::new(cols, rows)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    term.backend().buffer().clone()
}

fn main() {
    let depths =
        [(Depth::Full, "any colour"), (Depth::Indexed, "256"), (Depth::Basic, "16"), (Depth::None, "none")];
    let font = std::env::args().nth(1).unwrap_or_else(|| "Consolas".to_string());
    println!(
        "<!doctype html><meta charset=\"utf-8\"><title>Lillignode look</title>\
         <style>body{{background:#444;color:#eee;font-family:sans-serif}}\
         pre{{font-family:'{font}',monospace;font-size:15px;line-height:1.2;display:inline-block;\
         margin:0 12px 12px 0;padding:6px}}</style>"
    );
    let dark = ("#cccccc", "#0c0c0c");
    let light = ("#1e1e1e", "#f7f7f7");
    type Scene = (&'static str, u16, u16, fn(u16, u16) -> App<Fake>, fn(&mut App<Fake>));
    let scenes: [Scene; 5] = [
        ("Welcome", 100, 16, app, |_| {}),
        ("Welcome, narrowest window", 80, 12, app, |_| {}),
        ("Seven agents, one in each status", 100, 22, busy, |_| {}),
        ("One second later: Working has moved", 100, 22, busy, |a| a.clock = 1),
        ("The menu", 80, 20, busy, |a| {
            a.on_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::F(12),
                crossterm::event::KeyModifiers::NONE,
            ))
        }),
    ];
    // Optional: only scenes whose title contains the second argument, and
    // only the depth named by the third.
    let only_scene = std::env::args().nth(2).unwrap_or_default();
    let only_depth = std::env::args().nth(3).unwrap_or_default();
    for (title, cols, rows, make, then) in scenes {
        if !title.contains(&only_scene) {
            continue;
        }
        for (depth, name) in depths {
            if !only_depth.is_empty() && name != only_depth {
                continue;
            }
            let mut app = make(cols, rows);
            app.colors = depth;
            then(&mut app);
            let buf = draw(&app, cols, rows);
            let grounds: &[(&str, &str)] = if depth == Depth::Full { &[dark] } else { &[dark, light] };
            for ground in grounds {
                let theme = if *ground == dark { "dark terminal" } else { "light terminal" };
                println!("<h3>{title}: {name} ({theme}), {font}</h3>");
                println!("<pre style=\"background:{}\">{}</pre>", ground.1, html(&buf, *ground));
            }
        }
    }
}
