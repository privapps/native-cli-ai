//! Interaction seam for the searchable `/skills` picker.

use crate::tui::state::TuiSessionState;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub const SKILL_PICKER_MAX_ROWS: usize = 10;

/// Message shown in the picker when there is no row to render.
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
                let separator = if state.input_buffer.is_empty()
                    || state.input_buffer.ends_with(char::is_whitespace)
                {
                    ""
                } else {
                    " "
                };
                state.input_buffer.push_str(separator);
                state.input_buffer.push('/');
                state.input_buffer.push_str(&command);
                state.input_buffer.push(' ');
                state.cursor_char_idx = state.input_buffer.chars().count();
                state.slash_menu_index = 0;
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
    use std::path::PathBuf;

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
        assert_eq!(state.input_buffer, "/review ");
        assert_eq!(state.cursor_char_idx, 8);
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
