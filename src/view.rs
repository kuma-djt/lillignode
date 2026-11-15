//! Draw an agent's terminal screen into a rectangle of the window.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Draws whatever the terminal is showing: the live screen, or history if
/// the view has been scrolled back.
pub fn draw_screen(screen: &vt100::Screen, area: Rect, buf: &mut Buffer) {
    let (rows, cols) = screen.size();
    let (rows, cols) = (rows.get(), cols.get());
    for row in 0..rows.min(area.height) {
        for col in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::default().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
            let mut m = Modifier::empty();
            m.set(Modifier::BOLD, cell.bold());
            m.set(Modifier::DIM, cell.dim());
            m.set(Modifier::ITALIC, cell.italic());
            m.set(Modifier::UNDERLINED, cell.underline());
            m.set(Modifier::REVERSED, cell.inverse());
            style = style.add_modifier(m);

            let target = &mut buf[(area.x + col, area.y + row)];
            target.set_style(style);
            if cell.has_contents() {
                target.set_symbol(cell.contents());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn draws_text_with_symbols_and_colour_changes() {
        let mut t = Terminal::new(4, 60);
        let _ = t.feed(
            "\r\n▎ [✻] [✻] \x1b[38;2;153;153;153m· Share Claude Code\x1b[K\x1b[m\r\n日本 ok".as_bytes(),
        );
        let area = Rect::new(0, 0, 60, 4);
        let mut buf = Buffer::empty(area);
        draw_screen(t.screen(), area, &mut buf);
        assert!(row_text(&buf, 1).starts_with("▎ [✻] [✻] · Share Claude Code"), "{:?}", row_text(&buf, 1));
        assert!(row_text(&buf, 2).contains("ok"), "{:?}", row_text(&buf, 2));
    }

    /// Claude Code's logo and its mode icon. If these show as boxed question
    /// marks in a window, the window's font lacks them: Lillignode passes on
    /// the same characters it was given.
    #[test]
    fn passes_on_claude_codes_logo_and_mode_icon_unchanged() {
        let logo = " ▐▛███▜▌\r\n▝▜█████▛▘\r\n  ▘▘ ▝▝\r\n⏵⏵ auto mode on";
        let mut t = Terminal::new(4, 30);
        let _ = t.feed(logo.as_bytes());
        let area = Rect::new(0, 0, 30, 4);
        let mut buf = Buffer::empty(area);
        draw_screen(t.screen(), area, &mut buf);
        for (y, line) in logo.split("\r\n").enumerate() {
            assert_eq!(row_text(&buf, y as u16).trim_end(), line);
        }
    }

    #[test]
    fn draws_history_when_the_view_is_scrolled_back() {
        let mut t = Terminal::new(2, 20);
        for i in 1..=6 {
            let _ = t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll_to_oldest();
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        draw_screen(t.screen(), area, &mut buf);
        assert!(row_text(&buf, 0).starts_with("line 1"), "{:?}", row_text(&buf, 0));
    }
}
