use super::theme::*;
use super::*;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::{cmp::Reverse, collections::BinaryHeap};

#[derive(Clone, Debug)]
pub(super) struct Part {
    pub id: Option<u32>,
    pub bytes: u64,
}

pub(super) fn parts(index: &Index, ids: &[u32], total: u64) -> Vec<Part> {
    let mut largest = BinaryHeap::new();
    for id in ids {
        let Some(entry) = index.entry(*id) else {
            continue;
        };
        if entry.allocated == 0 {
            continue;
        }
        let value = Reverse((entry.allocated, entry.id));
        if largest.len() < 8 {
            largest.push(value);
        } else if largest.peek().is_some_and(|old| value.0 > old.0) {
            largest.pop();
            largest.push(value);
        }
    }
    let mut result: Vec<_> = largest
        .into_iter()
        .map(|Reverse((bytes, id))| Part {
            id: Some(id),
            bytes,
        })
        .collect();
    let represented = result
        .iter()
        .fold(0u64, |sum, p| sum.saturating_add(p.bytes));
    if total > represented {
        result.push(Part {
            id: None,
            bytes: total - represented,
        });
    }
    result.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.id.cmp(&b.id)));
    result
}

/// Split a weighted set into disjoint character rectangles without floating-point rounding.
pub(super) fn tiles(parts: &[Part], area: Rect, out: &mut Vec<(usize, Rect)>, start: usize) {
    if parts.is_empty() || area.is_empty() {
        return;
    }
    if parts.len() == 1 || (area.width == 1 && area.height == 1) {
        out.push((start, area));
        return;
    }
    let total: u128 = parts.iter().map(|p| u128::from(p.bytes)).sum();
    if total == 0 {
        return;
    }
    let mut running = 0u128;
    let mut split = 1;
    let mut best = u128::MAX;
    for (i, part) in parts.iter().enumerate().take(parts.len() - 1) {
        running += u128::from(part.bytes);
        let distance = running.abs_diff(total - running);
        if distance < best {
            best = distance;
            split = i + 1;
        }
    }
    let weight: u128 = parts[..split].iter().map(|p| u128::from(p.bytes)).sum();
    let horizontal = area.width >= area.height.saturating_mul(2) || area.height == 1;
    let length = if horizontal { area.width } else { area.height };
    let first = (u128::from(length) * weight / total).clamp(1, u128::from(length - 1)) as u16;
    let (a, b) = if horizontal {
        (
            Rect::new(area.x, area.y, first, area.height),
            Rect::new(area.x + first, area.y, area.width - first, area.height),
        )
    } else {
        (
            Rect::new(area.x, area.y, area.width, first),
            Rect::new(area.x, area.y + first, area.width, area.height - first),
        )
    };
    tiles(&parts[..split], a, out, start);
    tiles(&parts[split..], b, out, start + split);
}

const COLORS: [Color; 6] = [
    Color::Rgb(42, 64, 50),
    Color::Rgb(32, 52, 62),
    Color::Rgb(60, 47, 53),
    Color::Rgb(59, 54, 34),
    Color::Rgb(36, 61, 52),
    Color::Rgb(44, 48, 71),
];

pub(super) fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" Space map · g hide · click / double-click ")
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL).fg(FG));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 || inner.width == 0 {
        return;
    }
    let mut line = vec![Span::styled(
        if app.listing {
            "Loading indexed shares…".into()
        } else {
            format!(
                "{} indexed · {} items",
                bytes(app.folder_bytes),
                app.folder_count
            )
        },
        Style::default().fg(DIM),
    )];
    if app.needs_rescan {
        line.push(Span::styled(" · r refresh", Style::default().fg(AMBER)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(line)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let map = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    let Some(index) = app.index.clone() else {
        return;
    };
    let parts = parts(&index, &app.ids, app.folder_bytes);
    if parts.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.listing {
                "Reading metadata · navigation stays available"
            } else {
                "No allocated data in these indexed children."
            })
            .style(Style::default().fg(DIM)),
            map,
        );
        return;
    }
    let mut layout = Vec::with_capacity(parts.len());
    tiles(&parts, map, &mut layout, 0);
    for (i, rect) in layout {
        let part = &parts[i];
        let selected = part
            .id
            .is_some_and(|id| app.focused() == Some(id) || app.selected.contains(&id));
        let color = part
            .id
            .and_then(|id| index.entry(id))
            .map(|e| COLORS[e.inode as usize % COLORS.len()])
            .unwrap_or(PANEL);
        let background = if selected { ACCENT } else { color };
        let foreground = if selected { BG } else { FG };
        let mut tile = Block::default().style(Style::default().bg(background).fg(foreground));
        if rect.width >= 6 && rect.height >= 3 {
            tile = tile
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(BG));
        }
        let inside = tile.inner(rect);
        frame.render_widget(tile, rect);
        let name = part
            .id
            .and_then(|id| index.entry(id))
            .map(|e| index.display_name(e))
            .unwrap_or_else(|| "Other items".into());
        let mut text = vec![Line::styled(
            name,
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if inside.height >= 2 && inside.width >= 13 {
            let percent = u128::from(part.bytes) * 100 / u128::from(app.folder_bytes.max(1));
            text.push(Line::raw(format!("{} · {percent}%", bytes(part.bytes))));
        }
        if inside.width >= 10 {
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(foreground).bg(background)),
                inside,
            );
        }
        if let Some(id) = part.id {
            app.ui.hits.push((rect, Hit::Tile(id)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn treemap_rectangles_are_disjoint_bounded_and_cover_the_available_area() {
        let values = [u64::MAX, u64::MAX / 2, 100, 5, 1];
        let parts: Vec<_> = values
            .iter()
            .enumerate()
            .map(|(i, bytes)| Part {
                id: Some(i as u32),
                bytes: *bytes,
            })
            .collect();
        for (width, height) in [(1, 1), (1, 40), (80, 1), (40, 5), (120, 8)] {
            let area = Rect::new(2, 3, width, height);
            let mut layout = vec![];
            tiles(&parts, area, &mut layout, 0);
            let mut cells = HashSet::new();
            for (_, tile) in layout {
                assert!(
                    tile.x >= area.x
                        && tile.y >= area.y
                        && tile.right() <= area.right()
                        && tile.bottom() <= area.bottom()
                );
                for y in tile.y..tile.bottom() {
                    for x in tile.x..tile.right() {
                        assert!(cells.insert((x, y)), "Overlapping tile");
                    }
                }
            }
            assert_eq!(cells.len(), width as usize * height as usize);
        }
    }
}
