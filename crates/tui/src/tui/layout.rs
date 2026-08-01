//! Layout helpers and geometric constants for the TUI.
//!
//! Extracted from `tui/app.rs` in Phase 2.2.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub const SIDEBAR_WIDTH: u16 = 32;
pub const SIDEBAR_MIN_TOTAL_WIDTH: u16 = 110;
pub const COMMAND_PALETTE_WIDTH: u16 = 48;
pub const COMMAND_PALETTE_MAX_ROWS: usize = 10;

pub fn layout_chunks(
    area: Rect,
    slash_h: u16,
    requested_input_h: u16,
) -> (Rect, Rect, Option<Rect>, Rect) {
    // Keep at least four transcript rows available, while allowing very small
    // terminals to reduce the composer to its minimum bordered height.
    let input_h =
        requested_input_h.min(area.height.saturating_sub(slash_h.saturating_add(6)).max(3));
    if slash_h > 0 {
        let c = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(2),
                Constraint::Length(slash_h),
                Constraint::Length(input_h),
            ])
            .split(area);
        (c[0], c[1], Some(c[2]), c[3])
    } else {
        let c = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(2),
                Constraint::Length(input_h),
            ])
            .split(area);
        (c[0], c[1], None, c[2])
    }
}

pub fn sidebar_fit(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub fn layout_with_sidebar(area: Rect) -> (Rect, Option<Rect>) {
    if area.width < SIDEBAR_MIN_TOTAL_WIDTH {
        return (area, None);
    }
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(60), Constraint::Length(SIDEBAR_WIDTH)])
        .split(area);
    (chunks[0], Some(chunks[1]))
}

pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let popup_w = width
        .min(area.width.saturating_sub(2).max(20))
        .min(area.width);
    let popup_h = height
        .min(area.height.saturating_sub(2).max(6))
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(popup_w) / 2,
        area.y + area.height.saturating_sub(popup_h) / 2,
        popup_w,
        popup_h,
    )
}

pub fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}
