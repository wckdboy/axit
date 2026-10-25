use super::theme::*;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

pub(super) const MASK: &str = include_str!("../../assets/axe.txt");

fn color(pixel: u8) -> Color {
    match pixel {
        b'A' => ACCENT,
        _ => BG,
    }
}

/// Half blocks keep the pixels square in a terminal cell with a 1:2 aspect ratio.
pub(super) fn draw(frame: &mut Frame, area: Rect) {
    let rows: Vec<_> = MASK.lines().map(str::as_bytes).collect();
    for y in 0..4 {
        let line: Vec<_> = (0..8)
            .map(|x| {
                Span::styled(
                    "▀",
                    Style::default()
                        .fg(color(rows[y * 2][x]))
                        .bg(color(rows[y * 2 + 1][x])),
                )
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Line::from(line)),
            Rect::new(area.x, area.y + y as u16, 8.min(area.width), 1),
        );
    }
}
