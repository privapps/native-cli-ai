//! Layout helpers and geometric constants for the TUI.
//!
//! Extracted from `tui/app.rs` in Phase 2.2.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SIDEBAR_WIDTH: u16 = 32;
pub const SIDEBAR_MIN_TOTAL_WIDTH: u16 = 110;
pub const WORKSPACE_DISPLAY_WIDTH: usize = 26;
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

/// Fit a workspace path to a display-cell width while retaining both ends.
///
/// The ellipsis receives one cell. When the remaining width is odd, the prefix
/// receives the extra cell so the project root remains slightly more visible.
pub fn workspace_sidebar_fit(s: &str, max_width: usize) -> String {
    let text = s.trim();
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let content_width = max_width - 1;
    let prefix_width = content_width.div_ceil(2);
    let suffix_width = content_width - prefix_width;
    let prefix = take_display_prefix(text, prefix_width);
    let suffix = take_display_suffix(text, suffix_width);
    format!("{prefix}…{suffix}")
}

fn take_display_prefix(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result
}

fn take_display_suffix(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut width = 0;
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result.chars().rev().collect()
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

#[cfg(test)]
mod tests {
    use super::{sidebar_fit, workspace_sidebar_fit};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn workspace_paths_keep_short_and_exact_width_values() {
        assert_eq!(workspace_sidebar_fit("src/project", 20), "src/project");
        assert_eq!(workspace_sidebar_fit("src/project", 11), "src/project");
    }

    #[test]
    fn workspace_paths_middle_truncate_with_prefix_extra_on_odd_content() {
        let rendered = workspace_sidebar_fit("/workspace/project/leaf", 10);
        assert_eq!(rendered, "/work…leaf");
        assert_eq!(UnicodeWidthStr::width(rendered.as_str()), 10);
    }

    #[test]
    fn workspace_paths_bound_tiny_widths_and_unicode() {
        for width in 0..=3 {
            let rendered = workspace_sidebar_fit("/東京/leaf", width);
            assert!(UnicodeWidthStr::width(rendered.as_str()) <= width);
        }
        let rendered = workspace_sidebar_fit("/東京/プロジェクト/leaf", 12);
        assert!(rendered.contains('…'));
        assert!(rendered.starts_with("/東京"));
        assert!(rendered.ends_with("leaf"));
        assert!(UnicodeWidthStr::width(rendered.as_str()) <= 12);
    }

    #[test]
    fn ordinary_sidebar_fit_remains_right_truncation() {
        assert_eq!(sidebar_fit("ordinary-label", 8), "ordinar…");
    }
}
