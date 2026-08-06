//! Interaction seam for the searchable `/skills` picker.

use crate::tui::state::{SkillPickerEntry, TuiSessionState};
use crate::tui::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear as ClearWidget, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const SKILL_PICKER_MAX_ROWS: usize = 10;
const SKILL_PICKER_FIXED_CONTENT_ROWS: usize = 4;

/// Truncate text to a display-cell width without splitting UTF-8 or leaving a
/// combining mark behind after its base character was removed. The ellipsis
/// occupies one display cell and is only added when truncation is necessary.
pub fn truncate_display_cells(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut result = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch_width == 0 {
            if !result.is_empty() {
                result.push(ch);
            }
            continue;
        }
        if width + ch_width > max_width - 1 {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result.push('…');
    result
}

/// Format one picker entry for the popup's content width.
///
/// The identity portion is kept ahead of descriptions and source metadata so
/// command and display name survive ordinary truncation. The caller supplies
/// the width inside the popup border; no terminal width is guessed here.
pub fn skill_picker_row(entry: &SkillPickerEntry, content_width: usize) -> String {
    if content_width == 0 {
        return String::new();
    }

    let identity = format!(" /{} — {}", entry.command, entry.display_name);
    let identity_width = UnicodeWidthStr::width(identity.as_str());
    if identity_width > content_width {
        return truncate_display_cells(&identity, content_width);
    }

    let manual = if entry.manual_only {
        " · manual-only"
    } else {
        ""
    };
    let metadata = format!(
        ": {} · {} [{}]{}",
        entry.description, entry.directory, entry.source, manual
    );
    let remaining = content_width - identity_width;
    if remaining == 0 {
        return identity;
    }
    format!("{identity}{}", truncate_display_cells(&metadata, remaining))
}

/// Number of entry rows that can actually be displayed in a popup rectangle.
pub fn skill_picker_visible_rows(popup_area: Rect, filtered_len: usize) -> usize {
    let content_height = usize::from(popup_area.height.saturating_sub(2));
    content_height
        .saturating_sub(SKILL_PICKER_FIXED_CONTENT_ROWS)
        .min(filtered_len)
        .min(SKILL_PICKER_MAX_ROWS)
}

fn pad_display_cells(text: String, width: usize) -> String {
    let used = UnicodeWidthStr::width(text.as_str());
    if used >= width {
        text
    } else {
        format!("{text}{}", " ".repeat(width - used))
    }
}

/// Render the picker using the supplied popup rectangle.
///
/// Keeping this renderer beside the picker interaction seam lets TestBackend
/// tests exercise the same row geometry and width calculation used by the
/// fullscreen application.
pub fn render_skill_picker(frame: &mut Frame, popup_area: Rect, state: &mut TuiSessionState) {
    let filtered = filtered_skill_indices(state);
    let viewport = skill_picker_visible_rows(popup_area, filtered.len());
    let pick = state
        .skill_picker_index()
        .min(filtered.len().saturating_sub(1));
    let max_scroll = filtered.len().saturating_sub(viewport);
    let scroll = state.skill_picker_scroll().min(max_scroll).min(pick);
    *state.skill_picker_scroll_mut().unwrap() = scroll;
    let start = scroll;
    let end = (start + viewport).min(filtered.len());
    let content_width = usize::from(popup_area.width.saturating_sub(2));

    let query = if state.skill_picker_query().is_empty() {
        "type to filter".to_string()
    } else {
        state.skill_picker_query().to_string()
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                " Search ",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(query, Style::default().fg(theme::TEXT)),
        ]),
        Line::default(),
    ];
    if filtered.is_empty() {
        let message = empty_skill_picker_message(state).unwrap();
        lines.push(Line::from(Span::styled(
            truncate_display_cells(message, content_width),
            Style::default().fg(theme::MUTED),
        )));
    } else {
        for (visible, entry_index) in filtered[start..end].iter().enumerate() {
            let entry = &state.skill_picker_entries()[*entry_index];
            let selected = start + visible == pick;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::USER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };
            let row = pad_display_cells(skill_picker_row(entry, content_width), content_width);
            lines.push(Line::from(Span::styled(row, style)));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " ↑↓/jk select · Enter insert · Esc/q close ",
        Style::default().fg(theme::MUTED),
    )));

    frame.render_widget(ClearWidget, popup_area);
    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Span::styled(" skills ", Style::default().fg(theme::MUTED))),
        )
        .style(Style::default().bg(theme::SURFACE));
    frame.render_widget(popup, popup_area);
}

pub fn empty_skill_picker_message(state: &TuiSessionState) -> Option<&'static str> {
    if !filtered_skill_indices(state).is_empty() {
        return None;
    }
    Some(if state.skill_picker_entries().is_empty() {
        " No skills discovered"
    } else {
        " No skills match this search"
    })
}

pub fn filtered_skill_indices(state: &TuiSessionState) -> Vec<usize> {
    let query = state.skill_picker_query().trim().to_ascii_lowercase();
    state
        .skill_picker_entries()
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            query.is_empty()
                || entry.command.to_ascii_lowercase().contains(&query)
                || entry.display_name.to_ascii_lowercase().contains(&query)
                || entry.description.to_ascii_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect()
}

fn reset_selection(state: &mut TuiSessionState) {
    *state
        .skill_picker_index_mut()
        .expect("skill picker is open") = 0;
    *state
        .skill_picker_scroll_mut()
        .expect("skill picker is open") = 0;
}

fn sync_scroll(state: &mut TuiSessionState, filtered_len: usize) {
    let index = state
        .skill_picker_index_mut()
        .expect("skill picker is open");
    *index = (*index).min(filtered_len.saturating_sub(1));
    let index = *index;
    let scroll = state
        .skill_picker_scroll_mut()
        .expect("skill picker is open");
    if index < *scroll {
        *scroll = index;
    } else if index >= scroll.saturating_add(SKILL_PICKER_MAX_ROWS) {
        *scroll = index.saturating_sub(SKILL_PICKER_MAX_ROWS - 1);
    }
    *scroll = (*scroll).min(filtered_len.saturating_sub(SKILL_PICKER_MAX_ROWS));
}

fn insert_selected_skill(state: &mut TuiSessionState, command: &str) {
    let separator =
        if state.input_buffer.is_empty() || state.input_buffer.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
    state.input_buffer.push_str(separator);
    state.input_buffer.push('$');
    state.input_buffer.push_str(command);
    state.input_buffer.push(' ');
    state.cursor_char_idx = state.input_buffer.chars().count();
    state.slash_menu_index = 0;
}

/// Return the command-picker popup height used by both drawing and mouse hit testing.
pub fn skill_picker_popup_height(filtered_len: usize) -> u16 {
    (filtered_len.min(SKILL_PICKER_MAX_ROWS) as u16)
        .saturating_add(7)
        .max(9)
}

/// Select and insert the skill row under a mouse click.
///
/// The popup's first two content rows are the search line and spacer, so skill
/// rows begin three rows below its outer top border. A click edits only the
/// existing draft; it never submits or executes the selected skill.
pub fn handle_skill_picker_mouse(
    state: &mut TuiSessionState,
    popup_area: ratatui::layout::Rect,
    row: u16,
) -> bool {
    if !state.skill_picker_open() {
        return false;
    }
    let filtered = filtered_skill_indices(state);
    let viewport = skill_picker_visible_rows(popup_area, filtered.len());
    let start = state
        .skill_picker_scroll()
        .min(filtered.len().saturating_sub(viewport));
    let row_offset = usize::from(row.saturating_sub(popup_area.y.saturating_add(3)));
    if row < popup_area.y.saturating_add(3) || row_offset >= viewport {
        return false;
    }
    let filtered_index = start + row_offset;
    let Some(entry_index) = filtered.get(filtered_index) else {
        return false;
    };
    let Some(command) = state
        .skill_picker_entries()
        .get(*entry_index)
        .map(|entry| entry.command.clone())
    else {
        return false;
    };
    insert_selected_skill(state, &command);
    state.close_skill_picker();
    true
}

/// Handle one key while the picker owns input. Enter inserts the selected
/// command into the draft and closes the picker; it never submits a turn.
pub fn handle_skill_picker_key(state: &mut TuiSessionState, key: KeyEvent) -> bool {
    if !state.skill_picker_open() {
        return false;
    }

    let filtered = filtered_skill_indices(state);
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            state.close_skill_picker();
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            if !filtered.is_empty() {
                *state.skill_picker_index_mut().unwrap() =
                    state.skill_picker_index().saturating_sub(1);
                sync_scroll(state, filtered.len());
            }
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            if !filtered.is_empty() {
                *state.skill_picker_index_mut().unwrap() =
                    (state.skill_picker_index() + 1).min(filtered.len() - 1);
                sync_scroll(state, filtered.len());
            }
        }
        (KeyCode::Enter, _) => {
            let selected = filtered
                .get(
                    state
                        .skill_picker_index()
                        .min(filtered.len().saturating_sub(1)),
                )
                .and_then(|index| state.skill_picker_entries().get(*index))
                .map(|entry| entry.command.clone());
            if let Some(command) = selected {
                insert_selected_skill(state, &command);
            }
            state.close_skill_picker();
        }
        (KeyCode::Backspace, _) => {
            state.skill_picker_query_mut().unwrap().pop();
            reset_selection(state);
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            state.skill_picker_query_mut().unwrap().push(c);
            reset_selection(state);
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::SkillPickerEntry;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    fn state() -> TuiSessionState {
        let mut state = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "a".into(),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        state.open_skill_picker(
            vec![
                SkillPickerEntry {
                    command: "review".into(),
                    display_name: "Review Changes".into(),
                    description: "Inspect a diff".into(),
                    source: "filesystem".into(),
                    directory: "/workspace/.agents/skills/review".into(),
                    manual_only: false,
                },
                SkillPickerEntry {
                    command: "deploy".into(),
                    display_name: "Release Helper".into(),
                    description: "Prepare a release".into(),
                    source: "filesystem".into(),
                    directory: "/workspace/.nca/skills/deploy".into(),
                    manual_only: true,
                },
            ],
            "",
        );
        state
    }

    fn row_entry() -> SkillPickerEntry {
        SkillPickerEntry {
            command: "review-wide".into(),
            display_name: "Review 東京́ Changes".into(),
            description:
                "Inspect a very long description with wide 東京 and combining e\u{301} marks".into(),
            source: "filesystem".into(),
            directory: "/workspace/.agents/skills/review-wide".into(),
            manual_only: true,
        }
    }

    #[test]
    fn picker_rows_bound_long_wide_and_combining_text_to_display_cells() {
        let entry = row_entry();
        let identity = format!(" /{} — {}", entry.command, entry.display_name);
        let identity_width = UnicodeWidthStr::width(identity.as_str());
        let row = skill_picker_row(&entry, identity_width + 8);
        assert!(row.starts_with(identity.as_str()));
        assert!(row.contains('…'));
        assert!(UnicodeWidthStr::width(row.as_str()) <= identity_width + 8);

        for width in 0..=32 {
            let row = skill_picker_row(&entry, width);
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= width,
                "width={width}: {row:?}"
            );
            assert!(std::str::from_utf8(row.as_bytes()).is_ok());
        }
        let combining = truncate_display_cells("Cafe\u{301}́ au lait", 5);
        assert!(combining.starts_with("Cafe"));
        assert!(UnicodeWidthStr::width(combining.as_str()) <= 5);
        let wide = truncate_display_cells("東京のレビュー", 5);
        assert!(wide.contains('…'));
        assert!(UnicodeWidthStr::width(wide.as_str()) <= 5);
    }

    #[test]
    fn picker_renderer_uses_popup_content_width_and_keeps_rows_single_line() {
        let mut state = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "a".into(),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        state.open_skill_picker(vec![row_entry()], "");
        let popup = Rect::new(2, 2, 24, skill_picker_popup_height(1));
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        let completed = terminal
            .draw(|frame| render_skill_picker(frame, popup, &mut state))
            .unwrap();
        let content_width = usize::from(popup.width - 2);
        let row_y = popup.y + 3;
        let rendered_row: String = (popup.x + 1..popup.x + popup.width - 1)
            .map(|x| {
                completed
                    .buffer
                    .cell((x, row_y))
                    .unwrap()
                    .symbol()
                    .to_string()
            })
            .collect();
        assert_eq!(UnicodeWidthStr::width(rendered_row.as_str()), content_width);
        assert!(rendered_row.contains('…'));
        assert!(!rendered_row.contains('\n'));
    }

    #[test]
    fn picker_renderer_keeps_empty_and_no_match_states_visible_at_narrow_widths() {
        for (entries, query, expected) in [
            (Vec::new(), "", "No skills discover"),
            (vec![row_entry()], "missing", "No skills match"),
        ] {
            let mut state = TuiSessionState::new(
                "s".into(),
                "m".into(),
                "a".into(),
                "default".into(),
                PathBuf::from("/tmp"),
            );
            state.open_skill_picker(entries, query);
            let popup = Rect::new(0, 0, 22, skill_picker_popup_height(0));
            let mut terminal = Terminal::new(TestBackend::new(22, 12)).unwrap();
            let completed = terminal
                .draw(|frame| render_skill_picker(frame, popup, &mut state))
                .unwrap();
            let rendered: String = (0..12)
                .flat_map(|y| {
                    (0..22)
                        .map(move |x| completed.buffer.cell((x, y)).unwrap().symbol().to_string())
                        .chain(std::iter::once("\n".to_string()))
                })
                .collect();
            assert!(rendered.contains(expected));
        }
    }

    #[test]
    fn mouse_geometry_uses_the_same_scrolled_rows_as_keyboard_selection() {
        let mut state = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "a".into(),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        let entries = (0..SKILL_PICKER_MAX_ROWS + 2)
            .map(|index| SkillPickerEntry {
                command: format!("skill-{index}"),
                display_name: format!("Skill {index}"),
                description: "long metadata".into(),
                source: "filesystem".into(),
                directory: "/tmp".into(),
                manual_only: false,
            })
            .collect();
        state.open_skill_picker(entries, "");
        for _ in 0..SKILL_PICKER_MAX_ROWS + 1 {
            handle_skill_picker_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let popup = Rect::new(0, 2, 40, skill_picker_popup_height(12));
        assert!(handle_skill_picker_mouse(&mut state, popup, popup.y + 3));
        assert_eq!(state.input_buffer, "$skill-2 ");
    }
    #[test]
    fn filters_command_display_name_and_description_case_insensitively() {
        let mut state = state();
        state.skill_picker_query_mut().unwrap().push_str("RELEASE");
        assert_eq!(filtered_skill_indices(&state), vec![1]);
        state.skill_picker_query_mut().unwrap().clear();
        state.skill_picker_query_mut().unwrap().push_str("inspect");
        assert_eq!(filtered_skill_indices(&state), vec![0]);
    }

    #[test]
    fn navigation_is_bounded_and_jk_are_supported() {
        let mut state = state();
        handle_skill_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        handle_skill_picker_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_skill_picker_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.skill_picker_index(), 1);
        handle_skill_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        assert_eq!(state.skill_picker_index(), 0);
    }

    #[test]
    fn navigation_scrolls_and_query_edit_resets_selection() {
        let mut state = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "a".into(),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        let entries = (0..SKILL_PICKER_MAX_ROWS + 2)
            .map(|index| SkillPickerEntry {
                command: format!("skill-{index}"),
                display_name: format!("Skill {index}"),
                description: "test".into(),
                source: "filesystem".into(),
                directory: "/tmp".into(),
                manual_only: false,
            })
            .collect();
        state.open_skill_picker(entries, "");
        for _ in 0..SKILL_PICKER_MAX_ROWS + 1 {
            handle_skill_picker_key(&mut state, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(state.skill_picker_index(), SKILL_PICKER_MAX_ROWS + 1);
        assert_eq!(state.skill_picker_scroll(), 2);
        handle_skill_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(state.skill_picker_index(), 0);
        assert_eq!(state.skill_picker_scroll(), 0);
    }

    #[test]
    fn escape_and_q_close_without_changing_draft() {
        let mut q_state = state();
        q_state.input_buffer = "draft".into();
        handle_skill_picker_key(
            &mut q_state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );
        assert!(!q_state.skill_picker_open());
        assert_eq!(q_state.input_buffer, "draft");

        let mut escape_state = state();
        escape_state.input_buffer = "draft".into();
        handle_skill_picker_key(
            &mut escape_state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(!escape_state.skill_picker_open());
        assert_eq!(escape_state.input_buffer, "draft");
    }

    #[test]
    fn enter_inserts_command_with_trailing_space_without_submitting() {
        let mut state = state();
        handle_skill_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(!state.skill_picker_open());
        assert_eq!(state.input_buffer, "$review ");
        assert_eq!(state.cursor_char_idx, 8);
    }

    #[test]
    fn mouse_row_selection_inserts_the_same_reference_as_keyboard_selection() {
        let mut state = state();
        let popup = ratatui::layout::Rect::new(0, 10, 80, skill_picker_popup_height(2));
        assert!(handle_skill_picker_mouse(&mut state, popup, 14));
        assert!(!state.skill_picker_open());
        assert_eq!(state.input_buffer, "$deploy ");
        assert!(state.blocks.is_empty());
    }

    #[test]
    fn enter_appends_a_reference_after_existing_draft_without_submitting() {
        let mut state = state();
        state.input_buffer = "draft".into();
        state.cursor_char_idx = state.input_buffer.chars().count();
        handle_skill_picker_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(state.input_buffer, "draft $review ");
        assert_eq!(state.cursor_char_idx, state.input_buffer.chars().count());
    }
    #[test]
    fn empty_picker_messages_distinguish_empty_catalog_and_no_match() {
        let mut state = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "a".into(),
            "default".into(),
            PathBuf::from("/tmp"),
        );
        state.open_skill_picker(Vec::new(), "");
        assert_eq!(
            empty_skill_picker_message(&state),
            Some(" No skills discovered")
        );

        state.open_skill_picker(
            vec![SkillPickerEntry {
                command: "review".into(),
                display_name: "Review".into(),
                description: "Inspect changes".into(),
                source: "filesystem".into(),
                directory: "/tmp".into(),
                manual_only: false,
            }],
            "missing",
        );
        assert_eq!(
            empty_skill_picker_message(&state),
            Some(" No skills match this search")
        );
    }
}
