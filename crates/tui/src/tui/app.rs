//! Full-screen session TUI: transcript, streaming assistant, composer.

use crate::file_mentions;
use crate::skill_references::{
    apply_skill_reference_completion_with_space, matching_skill_reference_commands,
};
use crate::tui::composer::{
    PaletteRow, SLASH_PANEL_MAX_ROWS, apply_at_completion, apply_selected_at_completion,
    at_completion_active, at_completion_matches, at_panel_height, branch_picker_enter_command,
    composer_chrome_height, composer_input_height_with_width, composer_render_model_with_width,
    cursor_byte_index, delete_completed_at_mention, filter_palette_rows, filter_slash_entries,
    filtered_branch_indices, insert_text_at_cursor, load_slash_entries, move_cursor_end,
    move_cursor_home, move_cursor_vertical_with_width, normalize_paste, palette_command_for_label,
    palette_selectable_indices, sanitize_single_line_paste, slash_panel_visible,
};
use crate::tui::connect_modal::{
    ConnectRow, build_connect_rows, clamp_selection, row_index_for_selection,
};
use crate::tui::input::{
    ApprovalAnswer, ConnectModalKeyResult, CustomProviderSetupKeyResult, filtered_skill_indices,
    handle_approval_key, handle_connect_modal_key, handle_custom_provider_setup_key,
    handle_skill_picker_key, handle_skill_picker_mouse, parse_tui_question_answer,
    render_branch_picker, render_command_palette, render_skill_picker, skill_picker_popup_height,
};
use crate::tui::layout::{
    WORKSPACE_DISPLAY_WIDTH, centered_rect, layout_chunks, layout_with_sidebar, rect_contains,
    sidebar_fit, workspace_sidebar_fit,
};
use crate::tui::state::{
    CustomProviderSetupStep, DisplayBlock, ModelPickerAction, ModelPickerEntry, TuiSessionState,
};
use crate::tui::terminal::{TerminalRestoreGuard, restore_terminal, setup_terminal};
use crate::tui::theme;
use crate::tui::transcript::{
    ensure_transcript_cache, live_activity_lines, parse_approval_verdict, transcript_lines,
};
use crossterm::{
    cursor::MoveToColumn,
    event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind, poll, read},
    execute,
};
use nca_common::config::{ProviderCompatibility, ProviderKind};
use nca_common::event::{BusyState, QuestionSelection};
use nca_core::approval::suggest_allow_pattern;
use nca_core::skills::SkillCatalog;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear as ClearWidget, Paragraph, Wrap},
};
use std::io::stdout;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug)]
pub enum TuiCmd {
    Submit(String),
    /// Answer for the current `ask_question` (from question mode or `/auto-answer`).
    QuestionAnswer(nca_common::event::QuestionSelection),
    CycleAgent,
    CancelTurn,
    Exit,
    /// Open the branch picker popup.
    OpenBranchPicker,
    /// Switch to the given branch name.
    SwitchBranch(String),
    /// Create a new branch with the given name and switch to it.
    CreateBranch(String),
    /// Read and stage a clipboard image away from the UI thread.
    PasteClipboard,
    /// Copy the latest assistant response to the system clipboard.
    CopyLastAssistant,
    /// Apply workspace default provider (from TUI picker).
    ApplyDefaultProvider(ProviderKind),
    /// Open API key modal for provider; bool indicates whether to connect after save/confirm.
    PromptApiKey(ProviderKind, bool),
    /// Apply a model name (from the model picker).
    ApplyModel(String),
    /// Switch provider (from the model picker).
    ApplyModelProvider(ProviderKind),
    /// Apply permission mode (from the permission picker).
    ApplyPermission(usize),
    /// Switch agent profile (from the agent picker).
    SwitchAgent(usize),
    /// Open external editor via leader key.
    OpenEditor,
    /// Start a new session.
    NewSession,
    /// Run compact.
    RunCompact,
    /// Open model picker (triggered by leader key or command palette).
    OpenModelPicker,
    /// Open status info modal.
    OpenStatus,
    /// Open help info modal.
    OpenHelp,
    /// Open agent picker.
    OpenAgentPicker,
    /// Open permission picker (reserved for future shortcut).
    #[allow(dead_code)]
    OpenPermissionPicker,
    /// Open sessions picker/info.
    OpenSessions,
    /// Cycle to the next recent model (F2 forward, Shift+F2 backward).
    CycleModel(bool),
    /// Validate an API key for onboarding (provider, api_key).
    /// The repl handler looks up base_url from config.
    ValidateApiKey(ProviderKind, String),
    /// Apply custom provider settings from the TUI wizard.
    ApplyCustomProviderSetup {
        compatibility: ProviderCompatibility,
        base_url: String,
        api_key_env: String,
        api_key: Option<String>,
        model: String,
    },
    /// Resolve a failed custom-provider probe.
    CustomProviderProbeAction(crate::tui::input::CustomProviderProbeAction),
    /// Open the in-session custom-provider setup flow from `/connect`.
    ConfigureCustomProvider,
    /// Mark onboarding as complete and persist the flag.
    #[allow(dead_code)]
    CompleteOnboarding,
    /// Resume a different session by ID.
    ResumeSession(String),
}

const MOUSE_SCROLL_LINES: usize = 3;

/// Maximum width of the question overlay. Unlike the other picker overlays,
/// questions size themselves from their content up to this limit and then
/// clamp to the terminal.
pub(crate) const QUESTION_POPUP_MAX_WIDTH: u16 = 72;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QuestionPopupLayout {
    pub(crate) popup_area: Rect,
    pub(crate) inner_width: u16,
    pub(crate) prompt_lines: Vec<String>,
    pub(crate) option_start: usize,
    pub(crate) option_end: usize,
    pub(crate) option_line_counts: Vec<usize>,
    pub(crate) option_row_budget: usize,
    pub(crate) indicator_lines: Vec<String>,
    pub(crate) footer_lines: Vec<String>,
    pub(crate) spacer_rows: usize,
}

/// Wrap a question-owned string before giving it to `Paragraph`.
///
/// Paragraph wrapping is display-only, which made it impossible for the old
/// fixed-height formula to know how many rows a prompt or option consumed.
/// Keeping the wrapping here means sizing and rendering use the same logical
/// rows, including hard-wrapped words and wide unicode characters.
fn wrap_question_text(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut result = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            result.push(String::new());
            continue;
        }

        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let word_width = UnicodeWidthStr::width(word);
            if word_width > width {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
                for ch in word.chars() {
                    let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if !current.is_empty()
                        && UnicodeWidthStr::width(current.as_str()) + ch_width > width
                    {
                        result.push(std::mem::take(&mut current));
                    }
                    current.push(ch);
                }
            } else if current.is_empty() {
                current.push_str(word);
            } else if UnicodeWidthStr::width(current.as_str()) + 1 + word_width <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                result.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            result.push(current);
        }
    }

    if result.is_empty() {
        result.push(String::new());
    }
    result
}

fn question_popup_item_count(q: &nca_common::event::InteractiveQuestionPayload) -> usize {
    1 + q.options.len() + usize::from(q.allow_custom)
}

fn question_popup_item_text(
    q: &nca_common::event::InteractiveQuestionPayload,
    index: usize,
    selected: bool,
) -> String {
    let marker = if selected { " ► " } else { "   " };
    if index == 0 {
        format!("{marker}Suggested: {}", q.suggested_answer)
    } else if index <= q.options.len() {
        format!("{marker}{}", q.options[index - 1].label)
    } else {
        format!("{marker}Chat about this")
    }
}

fn question_popup_indicator_lines(above: bool, below: bool, width: u16) -> Vec<String> {
    if !above && !below {
        return Vec::new();
    }
    let text = match (above, below) {
        (true, true) => " ▲ More above · ▼ More below",
        (true, false) => " ▲ More above (Up)",
        (false, true) => " ▼ More below (Down)",
        (false, false) => unreachable!(),
    };
    wrap_question_text(text, width)
}

fn question_popup_width(area: Rect, q: &nca_common::event::InteractiveQuestionPayload) -> u16 {
    let mut widest = UnicodeWidthStr::width(q.prompt.as_str());
    widest = widest.max(UnicodeWidthStr::width(
        format!("Suggested: {}", q.suggested_answer).as_str(),
    ));
    for option in &q.options {
        widest = widest.max(UnicodeWidthStr::width(option.label.as_str()));
    }
    if q.allow_custom {
        widest = widest.max(UnicodeWidthStr::width("Chat about this"));
    }
    widest = widest.max(UnicodeWidthStr::width(
        " ↑↓ select · Enter confirm · Esc chat ",
    ));
    let desired = widest
        .saturating_add(6)
        .max(44)
        .min(usize::from(QUESTION_POPUP_MAX_WIDTH));
    let available = area.width.saturating_sub(2);
    if available == 0 {
        area.width.min(desired as u16)
    } else {
        desired.min(usize::from(available)) as u16
    }
}

fn question_popup_rect(area: Rect, width: u16, height: u16) -> Rect {
    let popup_w = width
        .min(area.width.saturating_sub(2).max(20))
        .min(area.width);
    let popup_h = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(popup_w) / 2,
        area.y + area.height.saturating_sub(popup_h) / 2,
        popup_w,
        popup_h,
    )
}

///
/// `requested_scroll` is the existing `QuestionModal` scroll offset. The
/// selected item is always brought into the returned logical range; the draw
/// path writes `option_start` back to that same overlay state. Option labels
/// are sized in wrapped display rows, while scrolling itself remains by
/// logical option, preserving the answer contract.
pub(crate) fn question_popup_layout(
    area: Rect,
    q: &nca_common::event::InteractiveQuestionPayload,
    selected: usize,
    requested_scroll: usize,
) -> QuestionPopupLayout {
    let popup_width = question_popup_width(area, q);
    let inner_width = popup_width.saturating_sub(2).max(1);
    let item_count = question_popup_item_count(q);
    let selected = selected.min(item_count.saturating_sub(1));

    let raw_prompt_lines = wrap_question_text(&format!(" {} ", q.prompt), inner_width);
    let footer_text = if q.allow_custom {
        " ↑↓ select · Enter confirm · Esc chat "
    } else {
        " ↑↓ select · Enter confirm "
    };
    let footer_lines = wrap_question_text(footer_text, inner_width);

    // Keep the footer and at least one logical option available in a short
    // terminal. Tall terminals retain the complete wrapped prompt.
    let prompt_cap = usize::from(area.height)
        .saturating_sub(4 + footer_lines.len())
        .max(1);
    let prompt_lines = raw_prompt_lines
        .into_iter()
        .take(prompt_cap)
        .collect::<Vec<_>>();

    let option_line_counts = (0..item_count)
        .map(|index| {
            wrap_question_text(
                &question_popup_item_text(q, index, index == selected),
                inner_width,
            )
            .len()
        })
        .collect::<Vec<_>>();
    let compact_chrome = 2 + prompt_lines.len() + footer_lines.len();
    let full_compact_height = compact_chrome + option_line_counts.iter().sum::<usize>();
    let border_rows = 2;
    let spacer_rows = 0;
    let chrome_rows = compact_chrome + spacer_rows;

    if full_compact_height
        .saturating_add(spacer_rows)
        .saturating_add(border_rows)
        <= usize::from(area.height)
    {
        let popup_height = border_rows + full_compact_height + spacer_rows;
        let option_rows = option_line_counts.iter().sum();
        return QuestionPopupLayout {
            popup_area: question_popup_rect(area, popup_width, popup_height as u16),
            inner_width,
            prompt_lines,
            option_start: 0,
            option_end: item_count,
            option_line_counts,
            option_row_budget: option_rows,
            indicator_lines: Vec::new(),
            footer_lines,
            spacer_rows,
        };
    }

    let worst_indicator_rows = question_popup_indicator_lines(true, true, inner_width).len();
    let option_row_budget = usize::from(area.height)
        .saturating_sub(border_rows)
        .saturating_sub(chrome_rows + worst_indicator_rows)
        .max(1);
    let max_start = item_count.saturating_sub(1);
    let mut option_start = requested_scroll.min(max_start);
    let fit_end = |start: usize| {
        let mut rows = 0;
        let mut end = start;
        while end < item_count {
            let next_rows = rows + option_line_counts[end];
            if end > start && next_rows > option_row_budget {
                break;
            }
            rows = next_rows;
            end += 1;
            if rows >= option_row_budget {
                break;
            }
        }
        end.max((start + 1).min(item_count))
    };
    let mut option_end = fit_end(option_start);
    if selected < option_start {
        option_start = selected;
        option_end = fit_end(option_start);
    } else if selected >= option_end {
        // When moving down, retain as much context as fits before the selected
        // item instead of showing the selected row alone at the top.
        option_start = selected;
        let mut rows = option_line_counts[selected];
        while option_start > 0 && rows + option_line_counts[option_start - 1] <= option_row_budget {
            option_start -= 1;
            rows += option_line_counts[option_start];
        }
        option_end = fit_end(option_start);
    }

    let above = option_start > 0;
    let below = option_end < item_count;
    let indicator_lines = question_popup_indicator_lines(above, below, inner_width);
    let visible_option_rows = option_line_counts[option_start..option_end]
        .iter()
        .sum::<usize>()
        .min(option_row_budget);
    let popup_height = border_rows + chrome_rows + visible_option_rows + indicator_lines.len();

    QuestionPopupLayout {
        popup_area: question_popup_rect(area, popup_width, popup_height as u16),
        inner_width,
        prompt_lines,
        option_start,
        option_end,
        option_line_counts,
        option_row_budget,
        indicator_lines,
        footer_lines,
        spacer_rows,
    }
}

/// Render the production question modal popup into a ratatui frame.
///
/// The helper owns both layout calculation and line emission so in-memory
/// render tests exercise the same path as the interactive TUI. It returns the
/// layout used for the draw, or `None` when the question modal is not active.
pub(crate) fn render_question_popup(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &mut TuiSessionState,
) -> Option<QuestionPopupLayout> {
    if !state.question_modal_open() {
        return None;
    }
    let q = state.active_question.clone()?;
    let layout = question_popup_layout(
        area,
        &q,
        state.question_modal_index(),
        state.question_modal_scroll(),
    );
    if layout.option_start != state.question_modal_scroll()
        && let Some(scroll) = state.question_modal_scroll_mut()
    {
        *scroll = layout.option_start;
    }

    let mut lines: Vec<Line> = layout
        .prompt_lines
        .iter()
        .map(|line| {
            Line::from(Span::styled(
                line.clone(),
                Style::default()
                    .fg(theme::ASSISTANT)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    lines.push(Line::default());
    for line in &layout.indicator_lines {
        lines.push(Line::from(Span::styled(
            line.clone(),
            Style::default().fg(theme::MUTED),
        )));
    }

    // A wrapped option can be taller than a very short terminal's remaining
    // option budget. Stop at the rows reserved by the layout so the footer is
    // never pushed outside the popup by rendering overflow.
    let selected = state.question_modal_index();
    let mut remaining_option_rows = layout.option_row_budget;
    for index in layout.option_start..layout.option_end {
        if remaining_option_rows == 0 {
            break;
        }
        let selected_item = index == selected;
        let style = if selected_item {
            Style::default()
                .fg(Color::Black)
                .bg(theme::USER)
                .add_modifier(Modifier::BOLD)
        } else if q.allow_custom && index == 1 + q.options.len() {
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(theme::TEXT)
        };
        let option_lines = wrap_question_text(
            &question_popup_item_text(&q, index, selected_item),
            layout.inner_width,
        );
        let rendered_rows = option_lines.len().min(remaining_option_rows);
        for option_line in option_lines.into_iter().take(rendered_rows) {
            lines.push(Line::from(Span::styled(option_line, style)));
        }
        remaining_option_rows -= rendered_rows;
    }
    lines.push(Line::default());
    for _ in 0..layout.spacer_rows {
        lines.push(Line::default());
    }
    for line in &layout.footer_lines {
        lines.push(Line::from(Span::styled(
            line.clone(),
            Style::default().fg(theme::MUTED),
        )));
    }

    frame.render_widget(ClearWidget, layout.popup_area);
    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Span::styled(" question ", Style::default().fg(theme::WARN))),
        )
        .style(Style::default().bg(theme::SURFACE));
    frame.render_widget(popup, layout.popup_area);
    Some(layout)
}

fn newline_key_inserts_newline(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => {
            key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT)
        }
        // crossterm decodes raw Ctrl+J as `Char('j')`; CSI-u capable
        // terminals may report the control character itself.
        KeyCode::Char('j') | KeyCode::Char('\n') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

fn handle_newline_key(state: &mut TuiSessionState, key: KeyEvent) -> bool {
    if !newline_key_inserts_newline(key) {
        return false;
    }

    state.detach_prompt_history();
    let (buffer, cursor) = insert_text_at_cursor(&state.input_buffer, state.cursor_char_idx, "\n");
    state.input_buffer = buffer;
    state.cursor_char_idx = cursor;
    state.slash_menu_index = 0;
    true
}

/// Matches `PermissionMode` as stored via `format!("{:?}", mode)` (e.g. `BypassPermissions`).
fn toolbar_permission_is_bypass(mode: &str) -> bool {
    mode.contains("BypassPermissions")
}

fn permission_status_span(mode: &str) -> Span<'static> {
    if toolbar_permission_is_bypass(mode) {
        Span::styled(
            " bypass ",
            Style::default()
                .fg(Color::Black)
                .bg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )
    } else if mode.contains("AcceptEdits") || mode == "Default" {
        Span::styled(
            " edits ok ",
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::DIM),
        )
    } else if mode.contains("Plan") {
        Span::styled(
            " plan ",
            Style::default()
                .fg(theme::ASSISTANT)
                .add_modifier(Modifier::DIM),
        )
    } else if mode.contains("DontAsk") {
        Span::styled(
            " readonly ",
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::DIM),
        )
    } else {
        Span::styled(format!(" {mode} "), Style::default().fg(theme::MUTED))
    }
}

fn escape_cancels_active_turn(state: &TuiSessionState) -> bool {
    matches!(
        state.current_busy_state,
        BusyState::Thinking
            | BusyState::Streaming
            | BusyState::ToolRunning
            | BusyState::ApprovalPending
    ) || state.active_question.is_some()
        || state.active_approval.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimaryInputMode {
    Approval,
    QuestionModal,
    Normal,
}

fn primary_input_mode(active_approval: bool, question_modal_open: bool) -> PrimaryInputMode {
    if active_approval {
        PrimaryInputMode::Approval
    } else if question_modal_open {
        PrimaryInputMode::QuestionModal
    } else {
        PrimaryInputMode::Normal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalShortcutAction {
    Approve,
    Deny,
    AllowPattern,
}

fn approval_shortcut_action(
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Option<ApprovalShortcutAction> {
    match (code, modifiers) {
        (KeyCode::Char('y'), KeyModifiers::CONTROL) => Some(ApprovalShortcutAction::Approve),
        (KeyCode::Char('n'), KeyModifiers::CONTROL) => Some(ApprovalShortcutAction::Deny),
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Some(ApprovalShortcutAction::AllowPattern),
        _ => None,
    }
}

/// Route the custom-provider overlays exactly as the production event loop does.
///
/// Keeping this seam in `app.rs` makes acceptance tests observe both overlay
/// ownership and the command emitted to the runtime bridge. The lower-level
/// input handlers remain responsible only for interpreting their key events.
pub(crate) fn dispatch_custom_provider_key(
    state: &mut TuiSessionState,
    key: KeyEvent,
    cmd_tx: &Sender<TuiCmd>,
) -> bool {
    if state.connect_modal_open() {
        match handle_connect_modal_key(state, key) {
            ConnectModalKeyResult::PromptApiKey(provider) => {
                let _ = cmd_tx.try_send(TuiCmd::PromptApiKey(provider, true));
            }
            ConnectModalKeyResult::ConfigureCustom => {
                let _ = cmd_tx.try_send(TuiCmd::ConfigureCustomProvider);
            }
            ConnectModalKeyResult::Handled => {}
        }
        return true;
    }

    if state.custom_provider_setup_open() {
        match handle_custom_provider_setup_key(state, key) {
            CustomProviderSetupKeyResult::InvalidInput(error) => {
                state.push_error(format!("[custom] {error}"));
            }
            CustomProviderSetupKeyResult::Submit(submission) => {
                state.start_custom_provider_probe();
                let _ = cmd_tx.try_send(TuiCmd::ApplyCustomProviderSetup {
                    compatibility: submission.compatibility,
                    base_url: submission.base_url,
                    api_key_env: submission.api_key_env,
                    api_key: submission.api_key,
                    model: submission.model,
                });
            }
            CustomProviderSetupKeyResult::ProbeAction(action) => {
                if matches!(action, crate::tui::input::CustomProviderProbeAction::Cancel) {
                    state.close_custom_provider_setup();
                }
                let _ = cmd_tx.try_send(TuiCmd::CustomProviderProbeAction(action));
            }
            CustomProviderSetupKeyResult::Handled
            | CustomProviderSetupKeyResult::ContinueWithPreservedCredential
            | CustomProviderSetupKeyResult::ContinueWithInlineCredential => {}
        }
        return true;
    }

    false
}

/// Route a crossterm atomic paste to the active input context. Full-screen
/// chat and inline custom answers preserve newlines; every single-line field
/// receives the same payload with line endings replaced by spaces.
fn dispatch_paste(state: &mut TuiSessionState, text: &str) -> bool {
    let multiline = normalize_paste(text);
    let single_line = sanitize_single_line_paste(text);

    if state.question_modal_open()
        || state.info_modal_open()
        || state.provider_picker_open()
        || state.permission_picker_open()
        || state.agent_picker_open()
    {
        return true;
    }
    if state.command_palette_open() {
        if let Some(query) = state.command_palette_query_mut() {
            query.push_str(&single_line);
            *state.palette_index_mut().unwrap() = 0;
        }
        return true;
    }
    if state.model_picker_open() {
        if let Some(query) = state.model_picker_search_mut() {
            query.push_str(&single_line);
            *state.model_picker_index_mut().unwrap() = 0;
            *state.model_picker_scroll_mut().unwrap() = 0;
        }
        return true;
    }
    if state.connect_modal_open() {
        if let Some(query) = state.connect_search_mut() {
            query.push_str(&single_line);
            *state.connect_menu_index_mut().unwrap() = 0;
            *state.connect_modal_scroll_mut().unwrap() = 0;
        }
        return true;
    }
    if state.branch_picker_open() {
        if let Some(query) = state.branch_picker_query_mut() {
            query.push_str(&single_line);
            *state.branch_picker_index_mut().unwrap() = 0;
        }
        return true;
    }
    if state.session_picker_open() {
        if let Some(query) = state.session_picker_search_mut() {
            query.push_str(&single_line);
            *state.session_picker_index_mut().unwrap() = 0;
            *state.session_picker_scroll_mut().unwrap() = 0;
        }
        return true;
    }
    if state.skill_picker_open() {
        state
            .skill_picker_query_mut()
            .unwrap()
            .push_str(&single_line);
        *state.skill_picker_index_mut().unwrap() = 0;
        *state.skill_picker_scroll_mut().unwrap() = 0;
        return true;
    }
    if state.api_key_modal_open() {
        state.api_key_input_mut().unwrap().push_str(&single_line);
        return true;
    }
    if state.custom_provider_setup_open() {
        state
            .custom_setup_input_mut()
            .unwrap()
            .push_str(&single_line);
        return true;
    }
    if state.active_approval.is_some() {
        state.input_buffer.push_str(&single_line);
        state.cursor_char_idx = state.input_buffer.chars().count();
        return true;
    }

    state.detach_prompt_history();
    let (buffer, cursor) =
        insert_text_at_cursor(&state.input_buffer, state.cursor_char_idx, &multiline);
    state.input_buffer = buffer;
    state.cursor_char_idx = cursor;
    state.slash_menu_index = 0;
    true
}

fn transcript_scroll_metrics(
    state: &mut TuiSessionState,
    area: Rect,
    slash_entries: &[crate::tui::composer::SlashEntry],
    workspace_files: &[String],
    dollar_skill_commands: &[String],
) -> (usize, usize) {
    let (main_area, _) = layout_with_sidebar(area);
    let history_navigating = state.prompt_history_is_navigating();
    let dollar_matches = if history_navigating {
        Vec::new()
    } else {
        matching_skill_reference_commands(
            &state.input_buffer,
            cursor_byte_index(&state.input_buffer, state.cursor_char_idx),
            dollar_skill_commands,
        )
    };
    let chrome_height = if history_navigating {
        0
    } else {
        composer_chrome_height(
            slash_entries,
            workspace_files,
            &state.input_buffer,
            state.cursor_char_idx,
        )
        .max(at_panel_height(dollar_matches.len()))
    };
    let input_height = composer_input_height_with_width(
        &state.input_buffer,
        usize::from(!state.staged_image_attachments.is_empty()) + 1,
        main_area.width as usize,
    );
    let (transcript_area, _, _, _) = layout_chunks(main_area, chrome_height, input_height);
    let inner_width = transcript_area.width.saturating_sub(2);
    let total = ensure_transcript_cache(state, inner_width).lines.len();
    let viewport = transcript_area.height.saturating_sub(2) as usize;
    (total.saturating_sub(viewport), viewport.max(1))
}

/// Application-level seam for the history-owned arrow-key cases.
///
/// Returning `true` means the caller must not offer the same key to completion,
/// multiline cursor movement, or transcript scrolling.
pub(crate) fn handle_prompt_history_key(state: &mut TuiSessionState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Up if state.prompt_history_is_navigating() || state.input_buffer.is_empty() => {
            if let Some((buffer, cursor)) = state.prompt_history_previous() {
                state.input_buffer = buffer;
                state.cursor_char_idx = cursor;
            }
            true
        }
        KeyCode::Down if state.prompt_history_is_navigating() || state.input_buffer.is_empty() => {
            if let Some((buffer, cursor)) = state.prompt_history_next() {
                state.input_buffer = buffer;
                state.cursor_char_idx = cursor;
            }
            true
        }
        _ => false,
    }
}

pub(crate) fn apply_transcript_page_scroll(
    state: &mut TuiSessionState,
    max_scroll: usize,
    page: usize,
    down: bool,
) {
    let page = page.max(1);
    if down {
        state.scroll_lines = (state.scroll_lines + page).min(max_scroll);
        if state.scroll_lines >= max_scroll {
            state.transcript_follow_tail = true;
        }
    } else {
        state.transcript_follow_tail = false;
        state.scroll_lines = state.scroll_lines.saturating_sub(page).min(max_scroll);
    }
}

fn model_identity_spans(
    model: &str,
    reasoning_effort: Option<&str>,
    agent_profile: &str,
) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::raw(" │ "),
        Span::styled(model.to_string(), Style::default().fg(theme::USER)),
    ];
    if let Some(effort) = reasoning_effort {
        spans.push(Span::raw(" │ "));
        spans.push(Span::styled(
            format!("reasoning: {effort}"),
            Style::default().fg(theme::MUTED),
        ));
    }
    spans.extend([
        Span::raw(" │ "),
        Span::styled(
            agent_profile.to_string(),
            Style::default().fg(theme::ASSISTANT),
        ),
    ]);
    spans
}

fn workspace_sidebar_line(workspace_display: &str) -> String {
    if workspace_display.is_empty() {
        "—".to_string()
    } else {
        workspace_sidebar_fit(workspace_display, WORKSPACE_DISPLAY_WIDTH)
    }
}

fn sidebar_footer_lines() -> Vec<Line<'static>> {
    vec![
        Line::default(),
        Line::from(Span::styled(
            "dev",
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Ctrl+P commands",
            Style::default().fg(theme::MUTED),
        )),
        Line::from(Span::styled(
            "/skills browse skills",
            Style::default().fg(theme::USER),
        )),
        Line::from(Span::styled(
            "$skill inline guidance",
            Style::default().fg(theme::MUTED),
        )),
    ]
}

fn dispatch_leader_key(state: &mut TuiSessionState, key: KeyEvent, cmd_tx: &Sender<TuiCmd>) {
    state.leader_pending = false;
    match key.code {
        KeyCode::Char('m') | KeyCode::Char('M') => {
            let _ = cmd_tx.try_send(TuiCmd::OpenModelPicker);
        }
        KeyCode::Char('e') | KeyCode::Char('E') => {
            let _ = cmd_tx.try_send(TuiCmd::OpenEditor);
        }
        KeyCode::Char('l') | KeyCode::Char('L') => {
            let _ = cmd_tx.try_send(TuiCmd::OpenSessions);
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            let _ = cmd_tx.try_send(TuiCmd::NewSession);
        }
        KeyCode::Char('c') | KeyCode::Char('C') => {
            let _ = cmd_tx.try_send(TuiCmd::RunCompact);
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            let _ = cmd_tx.try_send(TuiCmd::OpenStatus);
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            let _ = cmd_tx.try_send(TuiCmd::OpenAgentPicker);
        }
        KeyCode::Char('h') | KeyCode::Char('H') => {
            let _ = cmd_tx.try_send(TuiCmd::OpenHelp);
        }
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            state.should_exit = true;
            let _ = cmd_tx.try_send(TuiCmd::Exit);
        }
        _ => {}
    }
}

/// `question_answer_tx`: when `Some`, answers are sent there so they unblock `ask_question` while
/// the async loop is stuck in `run_turn` (that task does not poll `cmd_rx` until the turn ends).
#[allow(clippy::too_many_arguments)]
pub fn run_blocking(
    state: Arc<Mutex<TuiSessionState>>,
    mut version_rx: tokio::sync::watch::Receiver<u64>,
    version_tx: tokio::sync::watch::Sender<u64>,
    cmd_tx: Sender<TuiCmd>,
    question_answer_tx: Option<Sender<(String, QuestionSelection)>>,
    approval_answer_tx: Option<Sender<ApprovalAnswer>>,
    show_run_banner: bool,
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    skill_directories: Vec<PathBuf>,
) -> anyhow::Result<()> {
    let _ = version_rx.borrow_and_update();
    let mut terminal = setup_terminal()?;
    let mut terminal_restore_guard = TerminalRestoreGuard::new();

    // Load the built-in slash surface once: canonical commands + aliases.
    let workspace_root = {
        let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        g.workspace_root.clone()
    };
    let slash_entries = load_slash_entries(&workspace_root, &skill_directories);
    let mut dollar_skill_commands = SkillCatalog::discover(&workspace_root, &skill_directories)
        .unwrap_or_default()
        .into_iter()
        .map(|skill| skill.command)
        .collect::<Vec<_>>();
    dollar_skill_commands.sort();
    dollar_skill_commands.dedup();
    let (workspace_files_tx, workspace_files_rx) = std::sync::mpsc::channel();
    let discovery_root = workspace_root.clone();
    std::thread::spawn(move || {
        let files = file_mentions::discover_workspace_files(&discovery_root);
        let _ = workspace_files_tx.send(files);
    });
    let mut workspace_files = vec!["Indexing workspace…".to_string()];
    let mut workspace_files_indexing = true;

    if show_run_banner && let Ok(mut g) = state.lock() {
        g.blocks.push(DisplayBlock::System(
            "Interactive run — type a message, Tab cycles agent profile, Ctrl+P opens commands."
                .into(),
        ));
        g.mark_transcript_dirty();
    }

    // Dirty-flag rendering: only call `terminal.draw` when the state actually
    // changed (or on resize / busy animation ticks). This is the #1 win called
    // out in docs/research/rust-ratatui-optimization.md — static content drops
    // from ~7% CPU to near-zero.
    let mut last_rendered_version: u64 = 0;
    let mut last_rendered_size: (u16, u16) = (0, 0);
    let mut last_busy_tick: std::time::Instant = std::time::Instant::now();
    loop {
        if workspace_files_indexing && let Ok(files) = workspace_files_rx.try_recv() {
            workspace_files = files;
            workspace_files_indexing = false;
            if let Ok(mut g) = state.lock() {
                g.mark_dirty();
                let _ = version_tx.send(g.state_version);
            }
        }
        let version_changed = version_rx.has_changed().unwrap_or(false);
        {
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            if g.should_exit {
                break;
            }

            // Busy animation: advance the indicator at most every ~120ms while
            // the agent is Streaming/ToolRunning/etc; otherwise skip the redraw.
            let busy_animating = matches!(
                g.current_busy_state,
                BusyState::Thinking
                    | BusyState::Streaming
                    | BusyState::ToolRunning
                    | BusyState::ApprovalPending
            );
            let now = std::time::Instant::now();
            let busy_tick_due =
                busy_animating && now.duration_since(last_busy_tick).as_millis() >= 120;

            let cur_size = terminal
                .size()
                .map(|r| (r.width, r.height))
                .unwrap_or(last_rendered_size);
            let size_changed = cur_size != last_rendered_size;
            let state_changed = version_changed || g.state_version != last_rendered_version;

            if !state_changed && !size_changed && !busy_tick_due {
                // Nothing to redraw. Drop the lock and go straight to polling.
                drop(g);
            } else {
                let history_navigating = g.prompt_history_is_navigating();
                let slash_filtered = if history_navigating {
                    Vec::new()
                } else {
                    filter_slash_entries(&slash_entries, &g.input_buffer)
                };
                let at_matches = if history_navigating {
                    Vec::new()
                } else {
                    at_completion_matches(&workspace_files, &g.input_buffer, g.cursor_char_idx)
                };
                let dollar_matches = if history_navigating {
                    Vec::new()
                } else {
                    matching_skill_reference_commands(
                        &g.input_buffer,
                        cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                        &dollar_skill_commands,
                    )
                };
                let chrome_h = if history_navigating {
                    0
                } else {
                    composer_chrome_height(
                        &slash_entries,
                        &workspace_files,
                        &g.input_buffer,
                        g.cursor_char_idx,
                    )
                    .max(at_panel_height(dollar_matches.len()))
                };
                let auxiliary_rows = usize::from(!g.staged_image_attachments.is_empty()) + 1;
                let preview_area = Rect::new(0, 0, cur_size.0, cur_size.1);
                let (preview_main_area, _) = layout_with_sidebar(preview_area);
                let input_h = composer_input_height_with_width(
                    &g.input_buffer,
                    auxiliary_rows,
                    preview_main_area.width as usize,
                );

                terminal.draw(|frame| {
                let area = frame.area();
                let (main_area, sidebar_opt) = layout_with_sidebar(area);
                let (tr, st_r, slash_opt, inp_r) =
                    layout_chunks(main_area, chrome_h, input_h);

                let transcript_h = tr.height.saturating_sub(2) as usize;
                let inner_w = tr.width.saturating_sub(2);
                let transcript_lines = {
                    let cache = ensure_transcript_cache(&mut g, inner_w);
                    let mut lines = cache.lines.clone();
                    // Overlay keeps the "still working" footer ticking on busy
                    // animation frames without invalidating the full cache.
                    lines.extend(live_activity_lines(&g));
                    lines
                };
                let total = transcript_lines.len();
                let max_scroll = total.saturating_sub(transcript_h);
                if g.transcript_follow_tail {
                    g.scroll_lines = max_scroll;
                } else {
                    g.scroll_lines = g.scroll_lines.min(max_scroll);
                }
                let start = g.scroll_lines;
                let end = (start + transcript_h).min(total);
                let visible: Vec<Line> = if start < end {
                    transcript_lines[start..end].to_vec()
                } else {
                    vec![]
                };

                let title = format!(
                    " transcript — {} lines (↑↓ wheel · End bottom) ",
                    total
                );
                let main = Paragraph::new(Text::from(visible))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme::BORDER))
                            .title(Span::styled(title, Style::default().fg(theme::MUTED))),
                    )
                    .wrap(Wrap { trim: false })
                    .style(Style::default().bg(theme::BG));

                frame.render_widget(main, tr);

                if let Some(sidebar) = sidebar_opt {
                    let sections = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(12),
                            Constraint::Length(8),
                            Constraint::Length(10),
                            Constraint::Min(8),
                        ])
                        .split(sidebar);

                    let ws_line = workspace_sidebar_line(&g.workspace_display);
                    let session_lines = vec![
                        Line::from(Span::styled(
                            "workspace",
                            Style::default().fg(theme::MUTED),
                        )),
                        Line::from(ws_line),
                        Line::default(),
                        Line::from(format!("session {}", &g.session_id[..8.min(g.session_id.len())])),
                        Line::from(format!("model   {}", g.model)),
                        Line::from(format!("agent   {}", g.agent_profile)),
                        Line::from(format!("mode    {}", g.permission_mode)),
                        Line::from(format!("status  {}", g.current_busy_state.label())),
                        Line::from(format!("blocks  {}", g.blocks.len())),
                        Line::from(format!("lines   {total}")),
                    ];
                    let session_block = Paragraph::new(Text::from(session_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(
                                    " context ",
                                    Style::default().fg(theme::MUTED),
                                )),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(session_block, sections[0]);

                    let total = g.input_tokens + g.output_tokens;
                    let bar_width: usize = 16;
                    let bar = if total == 0 {
                        "·".repeat(bar_width)
                    } else {
                        let in_w = ((g.input_tokens as f64 / total as f64)
                            * bar_width as f64)
                            .round() as usize;
                        let in_w = in_w.min(bar_width);
                        let out_w = bar_width - in_w;
                        format!("{}{}", "▒".repeat(in_w), "█".repeat(out_w))
                    };
                    let usage_lines = vec![
                        Line::from(format!("input   {}", g.input_tokens)),
                        Line::from(format!("output  {}", g.output_tokens)),
                        Line::from(format!("total   {total}")),
                        Line::from(vec![
                            Span::styled("i/o     ", Style::default().fg(theme::MUTED)),
                            Span::styled(bar, Style::default().fg(theme::TOOL)),
                        ]),
                        Line::from(format!("cost    ${:.4}", g.cost_usd)),
                        Line::default(),
                        Line::from(if g.active_approval.is_some() {
                            "pending approval"
                        } else if g.active_question.is_some() {
                            "pending question"
                        } else {
                            "no pending prompt"
                        }),
                    ];
                    let usage_block = Paragraph::new(Text::from(usage_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(
                                    " usage ",
                                    Style::default().fg(theme::MUTED),
                                )),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(usage_block, sections[1]);

                    let mut task_lines: Vec<Line> = vec![Line::from(Span::styled(
                        "tasks",
                        Style::default()
                            .fg(theme::MUTED)
                            .add_modifier(Modifier::BOLD),
                    ))];
                    for line in g.todo_sidebar_lines(6) {
                        let style = if line.starts_with('+') || line == "none yet" {
                            Style::default().fg(theme::MUTED)
                        } else if line.contains('/')
                            && !line.starts_with(['○', '◉', '✓', '✗'])
                        {
                            Style::default().fg(theme::TOOL)
                        } else {
                            Style::default().fg(theme::TEXT)
                        };
                        task_lines.push(Line::from(Span::styled(line, style)));
                    }
                    let tasks_block = Paragraph::new(Text::from(task_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(
                                    " tasks ",
                                    Style::default().fg(theme::MUTED),
                                )),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(tasks_block, sections[2]);

                    let mut todo_lines: Vec<Line> = vec![Line::from(Span::styled(
                        "sub-agents",
                        Style::default()
                            .fg(theme::MUTED)
                            .add_modifier(Modifier::BOLD),
                    ))];
                    if g.subagents.is_empty() {
                        todo_lines.push(Line::from(Span::styled(
                            "none (spawn shows here)",
                            Style::default().fg(theme::MUTED),
                        )));
                    } else {
                        for row in g.subagents.iter().take(8) {
                            let dot = if row.running { "●" } else { "○" };
                            let id8 = sidebar_fit(&row.id, 8);
                            let ph = sidebar_fit(&row.phase, 11);
                            todo_lines.push(Line::from(vec![
                                Span::styled(
                                    format!("{dot} "),
                                    Style::default().fg(if row.running {
                                        theme::WARN
                                    } else {
                                        theme::MUTED
                                    }),
                                ),
                                Span::styled(format!("{id8} "), Style::default().fg(theme::TEXT)),
                                Span::styled(ph, Style::default().fg(theme::TOOL)),
                            ]));
                            if row.tokens_in > 0 || row.tokens_out > 0 {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  {}↑ {}↓", row.tokens_in, row.tokens_out),
                                    Style::default().fg(theme::MUTED),
                                )));
                            }
                            if !row.detail.is_empty() {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  {}", sidebar_fit(&row.detail, 26)),
                                    Style::default().fg(theme::MUTED),
                                )));
                            }
                            if let Some(ref skill_name) = row.skill {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  [{}]", sidebar_fit(skill_name, 24)),
                                    Style::default().fg(theme::WARN),
                                )));
                            }
                            if !row.task.is_empty() && row.task != "(sub-agent)" {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  {}", sidebar_fit(&row.task, 26)),
                                    Style::default().fg(theme::TEXT),
                                )));
                            }
                        }
                    }
                    todo_lines.extend(sidebar_footer_lines());
                    let todo_block = Paragraph::new(Text::from(todo_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(
                                    " sidebar ",
                                    Style::default().fg(theme::MUTED),
                                )),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(todo_block, sections[3]);
                }

                let elapsed = g.started.elapsed().as_secs();
                let indicator_text = crate::tui::busy_indicator::render_indicator(
                    g.current_busy_state,
                    g.busy_state_since,
                );
                let indicator_color =
                    crate::tui::busy_indicator::color_for_state(g.current_busy_state);
                let busy = Span::styled(indicator_text, Style::default().fg(indicator_color));
                let activity_detail = match g.current_busy_state {
                    BusyState::ToolRunning => g.blocks.iter().rev().find_map(|b| match b {
                        crate::tui::state::DisplayBlock::ToolRunning { name, input, .. } => {
                            Some(format!(
                                "{} ",
                                crate::tui::transcript::tool_running_summary(name, input)
                            ))
                        }
                        _ => None,
                    }),
                    BusyState::Thinking => Some("model… ".into()),
                    BusyState::Streaming => Some("out… ".into()),
                    _ => None,
                };
                let activity_span = activity_detail
                    .map(|d| Span::styled(d, Style::default().fg(theme::MUTED)))
                    .unwrap_or_else(|| Span::raw(""));
                let approval_hint = if g.active_approval.is_some() {
                    Span::styled(
                        " approve ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::WARN)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("")
                };
                let q_hint = if g.active_question.is_some() {
                    Span::styled(
                        " answer ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::ASSISTANT)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw("")
                };
                // Session / tokens / cost live in the sidebar; keep the bar short.
                let perm_span = permission_status_span(&g.permission_mode);
                let time_span = Span::styled(
                    format!("{:02}:{:02}", elapsed / 60, elapsed % 60),
                    Style::default().fg(theme::MUTED),
                );

                let cancel_hint_text = " Esc cancel ";
                let cancel_hint = escape_cancels_active_turn(&g).then(|| {
                    Span::styled(
                        cancel_hint_text,
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::WARN)
                            .add_modifier(Modifier::BOLD),
                    )
                });
                let status_rect = if cancel_hint.is_some() && st_r.width > cancel_hint_text.len() as u16 {
                    Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Min(0),
                            Constraint::Length(cancel_hint_text.len() as u16),
                        ])
                        .split(st_r)[0]
                } else {
                    st_r
                };

                // Compute the character-cell x-offset before any borrow of `g` escapes into `status_spans`.
                let reasoning_text = g
                    .reasoning_effort
                    .as_ref()
                    .map(|effort| format!("reasoning: {effort}"));
                let reasoning_width = reasoning_text
                    .as_ref()
                    .map(|text| text.len() + 4)
                    .unwrap_or(0);
                let branch_char_offset =
                    4 + g.model.len() + reasoning_width + 4 + g.agent_profile.len() + 4;
                let branch_text = if g.current_branch.is_empty() {
                    String::new()
                } else {
                    format!("⎇ {}", g.current_branch)
                };
                let branch_span_style = Style::default()
                    .fg(theme::TOOL)
                    .add_modifier(Modifier::UNDERLINED);

                // Store the branch chip bounds for click hit-testing.
                if status_rect.width > branch_char_offset as u16 && !branch_text.is_empty() {
                    let chip_len = branch_text.len() as u16;
                    g.branch_chip_bounds = Some(Rect::new(
                        status_rect.x + branch_char_offset as u16,
                        status_rect.y,
                        chip_len.min(status_rect.width - branch_char_offset as u16),
                        1,
                    ));
                } else {
                    g.branch_chip_bounds = None;
                }

                let mut status_spans = vec![busy, activity_span, approval_hint, q_hint];
                status_spans.extend(model_identity_spans(
                    &g.model,
                    g.reasoning_effort.as_deref(),
                    &g.agent_profile,
                ));
                status_spans.extend([
                    Span::raw(" │ "),
                    // branch_text borrow ends before next mutable use of `g` below
                    Span::styled(branch_text, branch_span_style),
                    Span::raw(" │ "),
                    perm_span,
                ]);
                // Sidebar is hidden on narrow terminals — put session/tokens/cost back on the bar.
                if sidebar_opt.is_none() {
                    status_spans.push(Span::raw(" │ "));
                    status_spans.push(Span::styled(
                        g.session_id[..8.min(g.session_id.len())].to_string(),
                        Style::default().fg(theme::MUTED),
                    ));
                    status_spans.extend([
                        Span::raw(" │ in:"),
                        Span::styled(
                            format!("{}", g.input_tokens),
                            Style::default().fg(theme::TEXT),
                        ),
                        Span::raw(" out:"),
                        Span::styled(
                            format!("{}", g.output_tokens),
                            Style::default().fg(theme::TEXT),
                        ),
                        Span::raw(" │ $"),
                        Span::styled(
                            format!("{:.4}", g.cost_usd),
                            Style::default().fg(theme::SUCCESS),
                        ),
                    ]);
                }
                status_spans.push(Span::raw(" │ "));
                status_spans.push(time_span);
                if let Some(host) = &g.custom_provider_host {
                    status_spans.insert(
                        5,
                        Span::styled(
                            format!("Custom · {host}"),
                            Style::default().fg(theme::USER),
                        ),
                    );
                    status_spans.insert(6, Span::raw(" │ "));
                }
                let status = Line::from(status_spans);
                let bar = Paragraph::new(status).style(Style::default().bg(theme::SURFACE));
                frame.render_widget(bar, status_rect);
                if let Some(cancel_hint) = cancel_hint {
                    let hint_width = cancel_hint_text.len() as u16;
                    if st_r.width > hint_width {
                        let hint_rect = Rect::new(
                            st_r.x + st_r.width.saturating_sub(hint_width),
                            st_r.y,
                            hint_width,
                            1,
                        );
                        let hint_bar = Paragraph::new(Line::from(cancel_hint))
                            .style(Style::default().bg(theme::SURFACE));
                        frame.render_widget(hint_bar, hint_rect);
                    }
                }

                if let Some(sr) = slash_opt {
                    if !history_navigating
                        && slash_panel_visible(&g.input_buffer)
                        && !slash_filtered.is_empty()
                    {
                        let n_show = slash_filtered.len().min(SLASH_PANEL_MAX_ROWS);
                        let max_scroll = slash_filtered.len().saturating_sub(n_show);
                        let list_scroll = g
                            .slash_menu_index
                            .saturating_sub(n_show.saturating_sub(1))
                            .min(max_scroll);
                        let mut slash_lines: Vec<Line> = Vec::new();
                        for (i, entry) in slash_filtered[list_scroll..list_scroll + n_show]
                            .iter()
                            .enumerate()
                        {
                            let global = list_scroll + i;
                            let st = if global == g.slash_menu_index {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::USER)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::TEXT)
                            };
                            slash_lines.push(Line::from(Span::styled(entry.display_text(), st)));
                        }
                        if slash_filtered.len() > n_show {
                            slash_lines.push(Line::from(Span::styled(
                                format!(
                                    " ─ {}/{} · ↑↓",
                                    g.slash_menu_index + 1,
                                    slash_filtered.len()
                                ),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                        let slash_w = Paragraph::new(Text::from(slash_lines))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(Style::default().fg(theme::BORDER))
                                    .title(Span::styled(
                                        " commands (↑↓ Tab complete) ",
                                        Style::default().fg(theme::MUTED),
                                    )),
                            )
                            .style(Style::default().bg(theme::SURFACE));
                        frame.render_widget(slash_w, sr);
                    } else if !at_matches.is_empty() {
                        let n_show = at_matches.len().min(SLASH_PANEL_MAX_ROWS);
                        let max_scroll = at_matches.len().saturating_sub(n_show);
                        let pick = g.at_menu_index.min(at_matches.len().saturating_sub(1));
                        let list_scroll =
                            pick.saturating_sub(n_show.saturating_sub(1)).min(max_scroll);
                        let mut lines: Vec<Line> = Vec::new();
                        for (i, path) in at_matches[list_scroll..list_scroll + n_show]
                            .iter()
                            .enumerate()
                        {
                            let global = list_scroll + i;
                            let st = if global == pick {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::USER)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::TEXT)
                            };
                            // Show path without @ prefix since @ is already in the buffer
                            lines.push(Line::from(Span::styled(format!(" {path}"), st)));
                        }
                        if at_matches.len() > n_show {
                            lines.push(Line::from(Span::styled(
                                format!(" ─ {}/{} · ↑↓ Tab", pick + 1, at_matches.len()),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                        let at_w = Paragraph::new(Text::from(lines))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(Style::default().fg(theme::BORDER))
                                    .title(Span::styled(
                                        " files (@ mention) ",
                                        Style::default().fg(theme::MUTED),
                                    )),
                            )
                            .style(Style::default().bg(theme::SURFACE));
                        frame.render_widget(at_w, sr);
                    } else if !dollar_matches.is_empty() {
                        let n_show = dollar_matches.len().min(SLASH_PANEL_MAX_ROWS);
                        let max_scroll = dollar_matches.len().saturating_sub(n_show);
                        let pick = g
                            .at_menu_index
                            .min(dollar_matches.len().saturating_sub(1));
                        let list_scroll = pick
                            .saturating_sub(n_show.saturating_sub(1))
                            .min(max_scroll);
                        let mut lines = Vec::new();
                        for (i, command) in dollar_matches[list_scroll..list_scroll + n_show]
                            .iter()
                            .enumerate()
                        {
                            let global = list_scroll + i;
                            let style = if global == pick {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::USER)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::TEXT)
                            };
                            lines.push(Line::from(Span::styled(format!(" {command}"), style)));
                        }
                        if dollar_matches.len() > n_show {
                            lines.push(Line::from(Span::styled(
                                format!(" ─ {}/{} · ↑↓ Tab", pick + 1, dollar_matches.len()),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                        let dollar_panel = Paragraph::new(Text::from(lines))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(Style::default().fg(theme::BORDER))
                                    .title(Span::styled(
                                        " skills ($ reference) ",
                                        Style::default().fg(theme::MUTED),
                                    )),
                            )
                            .style(Style::default().bg(theme::SURFACE));
                        frame.render_widget(dollar_panel, sr);
                    }
                }

                let render_model = composer_render_model_with_width(
                    &g.input_buffer,
                    g.cursor_char_idx,
                    inp_r.height
                        .saturating_sub(2 + auxiliary_rows as u16)
                        .max(1) as usize,
                    inp_r.width as usize,
                );

                let hint = if g.active_approval.is_some() {
                    Line::from(Span::styled(
                        "Approval: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow · /approve · /deny · other /commands still work",
                        Style::default().fg(theme::ERROR),
                    ))
                } else if g.active_question.is_some() && !g.question_modal_open() {
                    Line::from(Span::styled(
                        "Enter / 0 = suggested · 1–n = option · click underlined line · /auto-answer · End = transcript bottom (empty input)",
                        Style::default().fg(theme::WARN),
                    ))
                } else if history_navigating {
                    Line::from(Span::styled(
                        "History: ↑↓ navigate · Enter send · edit or move to detach · PageUp/PageDown transcript",
                        Style::default().fg(theme::MUTED),
                    ))
                } else if slash_panel_visible(&g.input_buffer) {
                    let hint_msg = if slash_filtered.is_empty() {
                        "No matching /command — try /help or Ctrl+P (palette)"
                    } else {
                        "Commands above · ↑↓ select · Tab complete · Ctrl+P palette"
                    };
                    Line::from(Span::styled(hint_msg, Style::default().fg(theme::MUTED)))
                } else if g.input_buffer.is_empty() {
                    Line::from(Span::styled(
                        "Enter send · ↑↓ history · PageUp/PageDown transcript · Shift+Enter/Alt+Enter/Ctrl+J newline · Tab agent · Ctrl+V image · Ctrl+P palette · Ctrl+X Q exit · Ctrl+L clear",
                        Style::default().fg(theme::MUTED),
                    ))
                } else {
                    Line::default()
                };

                let input_title = if g.active_approval.is_some() {
                    Span::styled(
                        " approve ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::WARN)
                            .add_modifier(Modifier::BOLD),
                    )
                } else if g.active_question.is_some() {
                    Span::styled(
                        " answer ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::ASSISTANT)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(" message ", Style::default().fg(theme::MUTED))
                };
                let mut input_lines = render_model.rows;
                if !g.staged_image_attachments.is_empty() {
                    input_lines.push(Line::from(Span::styled(
                        format!(
                            "  {} image(s) staged · Enter to send · /image clear",
                            g.staged_image_attachments.len()
                        ),
                        Style::default().fg(theme::SUCCESS),
                    )));
                }
                input_lines.push(hint);
                let input_block = Paragraph::new(Text::from(input_lines))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(if g.active_approval.is_some() {
                                theme::WARN
                            } else if g.active_question.is_some() {
                                theme::ASSISTANT
                            } else {
                                theme::BORDER
                            }))
                            .title(input_title),
                    )
                    .style(Style::default().bg(theme::SURFACE));

                frame.render_widget(input_block, inp_r);

                if g.command_palette_open() {
                    render_command_palette(frame, area, &g);
                }

                if g.branch_picker_open() {
                    render_branch_picker(frame, area, &g);
                }

                // LLM provider picker (default provider or API-key target).
                if g.provider_picker_open() {
                    let all = ProviderKind::ALL;
                    let n_builtin = all.len();
                    let n = g.provider_picker_visible_row_count();
                    let cap = crate::tui::state::TuiSessionState::PROVIDER_PICKER_VISIBLE_ROWS.min(n.max(1));
                    let scroll = g.provider_picker_scroll().min(n.saturating_sub(1));
                    let end = (scroll + cap).min(n);
                    let rows = (cap as u16).saturating_add(9).max(10);
                    let popup_area = centered_rect(area, 52, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            if g.provider_picker_for_api_key() {
                                " Select provider for API key "
                            } else {
                                " Default LLM provider "
                            },
                            Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    if scroll > 0 {
                        lines.push(Line::from(Span::styled(
                            "  More above (Up)",
                            Style::default().fg(theme::MUTED),
                        )));
                    }
                    let row_labels: Vec<String> = (0..n)
                        .map(|i| {
                            if i < n_builtin {
                                let p = all[i];
                                let name = p.display_name();
                                if p == ProviderKind::Custom {
                                    format!("{name} (BYO endpoint)")
                                } else {
                                    name.to_string()
                                }
                            } else {
                                "Add custom provider…".to_string()
                            }
                        })
                        .collect();
                    for (i, label) in row_labels
                        .iter()
                        .enumerate()
                        .skip(scroll)
                        .take(end.saturating_sub(scroll))
                    {
                        let st = if i == g.provider_picker_index() {
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::USER)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::TEXT)
                        };
                        lines.push(Line::from(Span::styled(format!(" {label}"), st)));
                    }
                    if end < n {
                        lines.push(Line::from(Span::styled(
                            "  More below (Down)",
                            Style::default().fg(theme::MUTED),
                        )));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter confirm · Esc cancel · c slash-command help ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(" settings ", Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Add custom provider wizard (`/provider` → Add custom provider…).
                if g.custom_provider_setup_open() {
                    let rows = 18u16;
                    let popup_area = centered_rect(area, 72, rows);
                    let step_title = match g.custom_provider_setup_step() {
                        CustomProviderSetupStep::Compatibility => "Step 1/4 — API compatibility",
                        CustomProviderSetupStep::BaseUrl => "Step 2/4 — Base URL",
                        CustomProviderSetupStep::ApiKey => "Step 3/4 — API key",
                        CustomProviderSetupStep::Model => "Step 4/4 — Model id",
                        CustomProviderSetupStep::Probe => "Check endpoint",
                    };
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            step_title,
                            Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    match g.custom_provider_setup_step() {
                        CustomProviderSetupStep::Compatibility => {
                            let opts = [
                                ("OpenAI-compatible", "POST …/v1/chat/completions (Bearer)"),
                                ("OpenAI Responses", "POST …/v1/responses (Bearer)"),
                                ("Anthropic-compatible", "POST …/v1/messages (x-api-key)"),
                            ];
                            for (j, (a, b)) in opts.iter().enumerate() {
                                let st = if j == g.custom_setup_compat_index() {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::USER)
                                        .add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme::TEXT)
                                };
                                lines.push(Line::from(vec![
                                    Span::styled(format!(" {a}"), st),
                                    Span::styled(
                                        format!(" - {b}"),
                                        Style::default().fg(theme::MUTED),
                                    ),
                                ]));
                            }
                        }
                        CustomProviderSetupStep::BaseUrl => {
                            lines.push(Line::from(Span::styled(
                                " Example: https://api.example.com or https://api.example.com/zen/v1",
                                Style::default().fg(theme::MUTED),
                            )));
                            lines.push(Line::default());
                            lines.push(Line::from(Span::styled(
                                format!(" {}", g.custom_setup_input()),
                                Style::default().fg(theme::TEXT),
                            )));
                        }
                        CustomProviderSetupStep::ApiKey => {
                            lines.push(Line::from(Span::styled(
                                " API-key environment variable (Tab toggles secret):",
                                Style::default().fg(theme::MUTED),
                            )));
                            lines.push(Line::default());
                            let env = if g.custom_setup_credential_env_focus() {
                                g.custom_setup_input()
                            } else {
                                g.custom_setup_api_key_env()
                            };
                            lines.push(Line::from(Span::styled(
                                format!(" env: {env}"),
                                Style::default().fg(theme::TEXT),
                            )));
                            let masked = if g.custom_setup_credential_env_focus()
                                || g.custom_setup_input().is_empty()
                            {
                                String::new()
                            } else {
                                "*".repeat(g.custom_setup_input().chars().count().min(48))
                            };
                            lines.push(Line::from(Span::styled(
                                format!(" secret: {masked}"),
                                Style::default().fg(theme::TEXT),
                            )));
                        }
                        CustomProviderSetupStep::Model => {
                            lines.push(Line::from(Span::styled(
                                " Model name your host expects (e.g. gpt-4o-mini).",
                                Style::default().fg(theme::MUTED),
                            )));
                            lines.push(Line::default());
                            lines.push(Line::from(Span::styled(
                                format!(" {}", g.custom_setup_input()),
                                Style::default().fg(theme::TEXT),
                            )));
                        }
                        CustomProviderSetupStep::Probe => {
                            if let Some(error) = g.custom_provider_probe_error() {
                                lines.push(Line::from(Span::styled(
                                    format!(" Probe failed: {error}"),
                                    Style::default().fg(theme::WARN),
                                )));
                                let labels = if g.custom_provider_probe_retryable() {
                                    ["Retry", "Save anyway", "Cancel"].as_slice()
                                } else {
                                    ["Cancel"].as_slice()
                                };
                                for (idx, label) in labels.iter().enumerate() {
                                    let style = if idx == g.custom_provider_probe_index() {
                                        Style::default()
                                            .fg(Color::Black)
                                            .bg(theme::USER)
                                            .add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default().fg(theme::TEXT)
                                    };
                                    lines.push(Line::from(Span::styled(
                                        format!(" {label}"),
                                        style,
                                    )));
                                }
                            } else {
                                lines.push(Line::from(Span::styled(
                                    " Checking endpoint…",
                                    Style::default().fg(theme::MUTED),
                                )));
                            }
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        match g.custom_provider_setup_step() {
                            CustomProviderSetupStep::Compatibility => {
                                " Enter confirm · Up/Down · Esc cancel "
                            }
                            CustomProviderSetupStep::Probe if g.custom_provider_probe_error().is_none() => {
                                " Checking endpoint · Esc cancel "
                            }
                            CustomProviderSetupStep::Probe => " Enter choose · Up/Down · Esc cancel ",
                            _ => " Enter confirm · Tab next field · Esc cancel · Backspace edit ",
                        },
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(" custom provider ", Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.permission_picker_open() {
                    const PERM_LABELS: &[&str] = &["Default", "Plan", "AcceptEdits", "DontAsk", "BypassPermissions"];
                    let rows = (PERM_LABELS.len() as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 40, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " Permission mode ",
                            Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    for (i, name) in PERM_LABELS.iter().enumerate() {
                        let st = if i == g.permission_picker_index() {
                            Style::default().fg(Color::Black).bg(theme::USER).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::TEXT)
                        };
                        lines.push(Line::from(Span::styled(format!(" {name}"), st)));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter apply · Esc cancel ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(" permissions ", Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.agent_picker_open() {
                    const AGENT_LABELS: &[(&str, &str)] = &[
                        ("@build", "Full-access agent for development"),
                        ("@plan", "Read-only analysis and planning"),
                        ("@review", "Focused code review"),
                        ("@fix", "Bug diagnosis and minimal fixes"),
                        ("@test", "Testing and validation"),
                    ];
                    let rows = (AGENT_LABELS.len() as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 52, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " Agent profile ",
                            Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    for (i, (name, desc)) in AGENT_LABELS.iter().enumerate() {
                        let st = if i == g.agent_picker_index() {
                            Style::default().fg(Color::Black).bg(theme::USER).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::TEXT)
                        };
                        let desc_st = if i == g.agent_picker_index() {
                            Style::default().fg(Color::Black).bg(theme::USER)
                        } else {
                            Style::default().fg(theme::MUTED)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!(" {name:<10}"), st),
                            Span::styled(format!(" {desc}"), desc_st),
                        ]));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter apply · Esc cancel ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(" agent ", Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Question modal popup (arrow-key option picker).
                if g.question_modal_open() && g.active_question.is_some() {
                    render_question_popup(frame, area, &mut g);
                }

                if g.session_picker_open() {
                    let filter = g.session_picker_search().to_ascii_lowercase();
                    let filtered_indices: Vec<usize> = g.session_picker_entries().iter().enumerate()
                        .filter(|(_, s)| filter.is_empty() || s.to_ascii_lowercase().contains(&filter))
                        .map(|(i, _)| i)
                        .collect();
                    const SESSION_PICKER_MAX_ROWS: usize = 16;
                    let n_filtered = filtered_indices.len();
                    let viewport_rows = n_filtered.min(SESSION_PICKER_MAX_ROWS);
                    let rows = (viewport_rows as u16).saturating_add(8).max(10);
                    let popup_area = centered_rect(area, 56, rows);
                    let pick = g.session_picker_index().min(n_filtered.saturating_sub(1));

                    if pick < g.session_picker_scroll() {
                        *g.session_picker_scroll_mut().unwrap() = pick;
                    } else if viewport_rows > 0 && pick >= g.session_picker_scroll() + viewport_rows {
                        *g.session_picker_scroll_mut().unwrap() = pick.saturating_sub(viewport_rows - 1);
                    }
                    *g.session_picker_scroll_mut().unwrap() = g.session_picker_scroll().min(n_filtered.saturating_sub(viewport_rows));
                    let list_start = g.session_picker_scroll();
                    let list_end = (list_start + viewport_rows).min(n_filtered);

                    let search_display = if g.session_picker_search().is_empty() {
                        "type to filter".to_string()
                    } else {
                        g.session_picker_search().to_string()
                    };
                    let mut lines: Vec<Line> = vec![
                        Line::from(vec![
                            Span::styled(" Search ", Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD)),
                            Span::styled(search_display, Style::default().fg(theme::TEXT)),
                        ]),
                        Line::default(),
                    ];
                    if filtered_indices.is_empty() {
                        lines.push(Line::from(Span::styled(" No matching sessions", Style::default().fg(theme::MUTED))));
                    } else {
                        if list_start > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▲ {} more", list_start),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                        let current_session_id = g.session_id.clone();
                        for (vis_idx, &filt_idx) in filtered_indices
                            .iter()
                            .enumerate()
                            .skip(list_start)
                            .take(list_end.saturating_sub(list_start))
                        {
                            let id = &g.session_picker_entries()[filt_idx];
                            let is_current = id == &current_session_id;
                            let marker = if is_current { " *" } else { "" };
                            let st = if vis_idx == pick {
                                Style::default().fg(Color::Black).bg(theme::USER).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::TEXT)
                            };
                            lines.push(Line::from(Span::styled(format!(" {id}{marker}"), st)));
                        }
                        let remaining_below = n_filtered.saturating_sub(list_end);
                        if remaining_below > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▼ {} more", remaining_below),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter resume · Esc close ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(" sessions ", Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.api_key_modal_open() {
                    let provider = g
                        .api_key_target_provider()
                        .map(|p| p.display_name())
                        .unwrap_or("provider");
                    let popup_area = centered_rect(area, 66, 12);
                    let headline = if g.api_key_connect_after_save() {
                        " Connect provider "
                    } else {
                        " API key "
                    };
                    let hint = if g.api_key_target_has_existing() {
                        " Press Enter to keep current key, or paste a new key to replace it. "
                    } else {
                        " Paste API key, then press Enter. "
                    };
                    let masked = if g.api_key_input().is_empty() {
                        String::new()
                    } else {
                        "*".repeat(g.api_key_input().chars().count())
                    };
                    let validation_line = if g.onboarding_mode {
                        match &g.validation_status {
                            Some(crate::tui::state::OnboardingValidation::Validating) => {
                                Some(Line::from(Span::styled(
                                    " Validating...",
                                    Style::default().fg(Color::Yellow),
                                )))
                            }
                            Some(crate::tui::state::OnboardingValidation::Failed(msg)) => {
                                Some(Line::from(Span::styled(
                                    format!(" {}", msg),
                                    Style::default().fg(Color::Red),
                                )))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let mut lines = vec![
                        Line::from(vec![
                            Span::styled(
                                format!(" Provider: {provider}"),
                                Style::default()
                                    .fg(theme::TEXT)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::default(),
                        Line::from(vec![
                            Span::styled(" API key ", Style::default().fg(theme::MUTED)),
                            Span::styled(masked, Style::default().fg(theme::USER)),
                        ]),
                        Line::default(),
                        Line::from(Span::styled(hint, Style::default().fg(theme::MUTED))),
                        Line::from(Span::styled(
                            " Enter confirm · Esc cancel ",
                            Style::default().fg(theme::MUTED),
                        )),
                    ];
                    if let Some(vline) = validation_line {
                        lines.push(vline);
                    }
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(headline, Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Generic info modal (read-only scrollable popup).
                if g.info_modal_open() {
                    let max_vis = 16usize;
                    let n_lines = g.info_modal_lines().len();
                    let popup_h = (n_lines.min(max_vis) as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 70, popup_h);
                    let n_show = n_lines.min(max_vis);
                    let max_scroll = n_lines.saturating_sub(n_show);
                    *g.info_modal_scroll_mut().unwrap() = g.info_modal_scroll().min(max_scroll);
                    let start = g.info_modal_scroll();
                    let end = (start + n_show).min(n_lines);
                    let mut lines: Vec<Line> = Vec::new();
                    for line in &g.info_modal_lines()[start..end] {
                        lines.push(Line::from(Span::styled(
                            format!(" {line}"),
                            Style::default().fg(theme::TEXT),
                        )));
                    }
                    if n_lines > max_vis {
                        lines.push(Line::from(Span::styled(
                            format!(" ─ {}/{} · ↑↓ scroll", start + 1, n_lines),
                            Style::default().fg(theme::MUTED),
                        )));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Esc close ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let title = format!(" {} ", g.info_modal_title());
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(title, Style::default().fg(theme::MUTED))),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.skill_picker_open() {
                    let filtered = filtered_skill_indices(&g);
                    let popup_area = centered_rect(
                        area,
                        92,
                        skill_picker_popup_height(filtered.len()),
                    );
                    render_skill_picker(frame, popup_area, &mut g);
                }

                // Model picker popup.
                if g.model_picker_open() {
                    let filter = g.model_picker_search().to_ascii_lowercase();

                    // Pre-compute indices for visible/selectable items and scroll
                    // without holding an immutable borrow on `g` that conflicts
                    // with the scroll update.
                    let vis_indices: Vec<usize> = g
                        .model_picker_entries()
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| {
                            e.is_header
                                || filter.is_empty()
                                || e.label.to_ascii_lowercase().contains(&filter)
                                || e.detail.to_ascii_lowercase().contains(&filter)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    let selectable_vis: Vec<usize> = vis_indices
                        .iter()
                        .enumerate()
                        .filter(|&(_, &orig)| !g.model_picker_entries()[orig].is_header)
                        .map(|(vi, _)| vi)
                        .collect();
                    let n_sel = selectable_vis.len();
                    let pick = if n_sel > 0 {
                        g.model_picker_index().min(n_sel - 1)
                    } else {
                        0
                    };
                    let selected_vis_idx = selectable_vis.get(pick).copied().unwrap_or(0);

                    const MODEL_PICKER_MAX_ROWS: usize = 18;
                    let n_visible = vis_indices.len();
                    let viewport_rows = n_visible.min(MODEL_PICKER_MAX_ROWS);
                    let popup_h = (viewport_rows as u16).saturating_add(7).max(10);
                    let popup_area = centered_rect(area, 62, popup_h);

                    // Keep the selected item visible within the viewport.
                    if selected_vis_idx < g.model_picker_scroll() {
                        *g.model_picker_scroll_mut().unwrap() = selected_vis_idx;
                    } else if viewport_rows > 0 && selected_vis_idx >= g.model_picker_scroll() + viewport_rows {
                        *g.model_picker_scroll_mut().unwrap() = selected_vis_idx.saturating_sub(viewport_rows - 1);
                    }
                    *g.model_picker_scroll_mut().unwrap() = g.model_picker_scroll().min(n_visible.saturating_sub(viewport_rows));
                    let list_start = g.model_picker_scroll();
                    let list_end = (list_start + viewport_rows).min(n_visible);

                    let search_display = if g.model_picker_search().is_empty() {
                        "type to filter…".to_string()
                    } else {
                        g.model_picker_search().to_string()
                    };
                    let mut lines: Vec<Line> = vec![
                        Line::from(vec![
                            Span::styled(
                                "Search ",
                                Style::default()
                                    .fg(theme::MUTED)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                search_display,
                                Style::default().fg(theme::TEXT),
                            ),
                        ]),
                        Line::default(),
                    ];
                    if vis_indices.is_empty() {
                        lines.push(Line::from(Span::styled(
                            " No models match",
                            Style::default().fg(theme::MUTED),
                        )));
                    } else {
                        if list_start > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▲ {} more", list_start),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                        for (vi, &model_idx) in vis_indices
                            .iter()
                            .enumerate()
                            .skip(list_start)
                            .take(list_end.saturating_sub(list_start))
                        {
                            let entry = &g.model_picker_entries()[model_idx];
                            if entry.is_header {
                                lines.push(Line::from(Span::styled(
                                    format!(" {}", entry.label),
                                    Style::default()
                                        .fg(theme::ASSISTANT)
                                        .add_modifier(Modifier::BOLD),
                                )));
                            } else {
                                let is_sel = selected_vis_idx == vi;
                                let main_st = if is_sel {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::USER)
                                        .add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme::TEXT)
                                };
                                let sub_st = if is_sel {
                                    main_st
                                } else {
                                    Style::default().fg(theme::MUTED)
                                };
                                lines.push(Line::from(vec![
                                    Span::styled(format!("   {}", entry.label), main_st),
                                    Span::styled(format!("  {}", entry.detail), sub_st),
                                ]));
                            }
                        }
                        let remaining_below = n_visible.saturating_sub(list_end);
                        if remaining_below > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▼ {} more", remaining_below),
                                Style::default().fg(theme::MUTED),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " ↑↓ select · Enter apply · Esc close ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(Span::styled(
                                    " models ",
                                    Style::default().fg(theme::MUTED),
                                )),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // OpenCode-style "Connect a provider" (`/connect`).
                if g.connect_modal_open() {
                    let rows = build_connect_rows(g.connect_search());
                    let sel = clamp_selection(g.connect_menu_index(), &rows);
                    let selected_row = row_index_for_selection(&rows, sel);
                    let body_lines = rows.len().max(1);
                    let popup_h = (body_lines as u16).saturating_add(9).clamp(11, 24);
                    let popup_area = centered_rect(area, 58, popup_h);
                    let mut lines: Vec<Line> = vec![
                        Line::from(vec![
                            Span::styled(
                                "Search ",
                                Style::default()
                                    .fg(theme::MUTED)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                if g.connect_search().is_empty() {
                                    "type to filter…"
                                } else {
                                    g.connect_search()
                                },
                                Style::default().fg(theme::TEXT),
                            ),
                        ]),
                        Line::default(),
                    ];
                    if rows.is_empty() {
                        lines.push(Line::from(Span::styled(
                            " No providers match",
                            Style::default().fg(theme::MUTED),
                        )));
                    } else {
                        for (i, row) in rows.iter().enumerate() {
                            match row {
                                ConnectRow::SectionHeader(h) => {
                                    lines.push(Line::from(Span::styled(
                                        format!(" {h}"),
                                        Style::default()
                                            .fg(theme::ASSISTANT)
                                            .add_modifier(Modifier::BOLD),
                                    )));
                                }
                                ConnectRow::Provider {
                                    title,
                                    subtitle,
                                    ..
                                } => {
                                    let is_sel = selected_row == Some(i);
                                    let main_st = if is_sel {
                                        Style::default()
                                            .fg(Color::Black)
                                            .bg(theme::USER)
                                            .add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default().fg(theme::TEXT)
                                    };
                                    let sub_st = if is_sel {
                                        main_st
                                    } else {
                                        Style::default().fg(theme::MUTED)
                                    };
                                    lines.push(Line::from(vec![
                                        Span::styled(format!(" {title}"), main_st),
                                        Span::styled(format!(" — {subtitle}"), sub_st),
                                    ]));
                                }
                            }
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " ↑↓ select · Enter connect · Esc close ",
                        Style::default().fg(theme::MUTED),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let title = Line::from(vec![
                        Span::styled(
                            " Connect a provider ",
                            Style::default().fg(theme::MUTED),
                        ),
                        Span::styled(" esc ", Style::default().fg(theme::MUTED)),
                    ]);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::BORDER))
                                .title(title),
                        )
                        .style(Style::default().bg(theme::SURFACE))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }
            })?;

                last_rendered_version = g.state_version;
                last_rendered_size = cur_size;
                if busy_tick_due {
                    last_busy_tick = now;
                }
            }
        }

        // Adaptive poll: quick ticks (~66ms) while the agent is busy or
        // streaming so the spinner stays lively; otherwise 250ms to keep
        // idle CPU <1% per the research doc.
        let poll_ms = {
            let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            if matches!(
                g.current_busy_state,
                BusyState::Thinking
                    | BusyState::Streaming
                    | BusyState::ToolRunning
                    | BusyState::ApprovalPending
            ) {
                66u64
            } else {
                250u64
            }
        };
        if poll(Duration::from_millis(poll_ms))? {
            let ev = read()?;
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            // Any user input triggers a redraw on the next tick (typing, mouse,
            // resize, etc). This avoids having to sprinkle `mark_dirty()` through
            // every branch of the huge `match ev` below.
            g.mark_dirty();
            let _ = version_tx.send(g.state_version);

            match ev {
                Event::Mouse(_) if g.command_palette_open() => continue,
                Event::Mouse(_) if g.info_modal_open() => continue,
                Event::Mouse(_) if g.model_picker_open() => continue,
                Event::Mouse(_) if g.connect_modal_open() => continue,
                Event::Mouse(_) if g.api_key_modal_open() => continue,
                Event::Mouse(_) if g.custom_provider_setup_open() => continue,
                Event::Mouse(_) if g.provider_picker_open() => continue,
                Event::Mouse(_) if g.permission_picker_open() => continue,
                Event::Mouse(_) if g.agent_picker_open() => continue,
                Event::Mouse(_) if g.session_picker_open() => continue,
                Event::Mouse(m) if g.skill_picker_open() => {
                    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                        let sz = terminal.size()?;
                        let area = Rect::new(0, 0, sz.width, sz.height);
                        let filtered = filtered_skill_indices(&g);
                        let popup_area =
                            centered_rect(area, 92, skill_picker_popup_height(filtered.len()));
                        if rect_contains(popup_area, m.column, m.row) {
                            handle_skill_picker_mouse(&mut g, popup_area, m.row);
                        }
                    }
                    continue;
                }
                Event::Mouse(_) if g.question_modal_open() => continue,
                Event::Paste(text) => {
                    dispatch_paste(&mut g, &text);
                }
                Event::Mouse(m) => {
                    let sz = terminal.size()?;
                    let area = Rect::new(0, 0, sz.width, sz.height);
                    let (main_area, _) = layout_with_sidebar(area);
                    let history_navigating = g.prompt_history_is_navigating();
                    let slash_filtered = if history_navigating {
                        Vec::new()
                    } else {
                        filter_slash_entries(&slash_entries, &g.input_buffer)
                    };
                    let at_matches = if history_navigating {
                        Vec::new()
                    } else {
                        at_completion_matches(&workspace_files, &g.input_buffer, g.cursor_char_idx)
                    };
                    let dollar_matches = if history_navigating {
                        Vec::new()
                    } else {
                        matching_skill_reference_commands(
                            &g.input_buffer,
                            cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                            &dollar_skill_commands,
                        )
                    };
                    let sh = if history_navigating {
                        0
                    } else {
                        composer_chrome_height(
                            &slash_entries,
                            &workspace_files,
                            &g.input_buffer,
                            g.cursor_char_idx,
                        )
                        .max(at_panel_height(dollar_matches.len()))
                    };
                    let input_h = composer_input_height_with_width(
                        &g.input_buffer,
                        usize::from(!g.staged_image_attachments.is_empty()) + 1,
                        main_area.width as usize,
                    );
                    let (tr, _, slash_r, _) = layout_chunks(main_area, sh, input_h);

                    if rect_contains(tr, m.column, m.row) {
                        let inner_w = tr.width.saturating_sub(2);
                        let cache = ensure_transcript_cache(&mut g, inner_w);
                        let total = cache.lines.len();
                        let th = tr.height.saturating_sub(2) as usize;
                        let max_scroll = total.saturating_sub(th);
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                g.transcript_follow_tail = false;
                                g.scroll_lines = g.scroll_lines.saturating_sub(MOUSE_SCROLL_LINES);
                                g.mark_dirty();
                            }
                            MouseEventKind::ScrollDown => {
                                g.scroll_lines =
                                    (g.scroll_lines + MOUSE_SCROLL_LINES).min(max_scroll);
                                if g.scroll_lines >= max_scroll {
                                    g.transcript_follow_tail = true;
                                }
                                g.mark_dirty();
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                // Inner content starts below top border (y+1).
                                let inner_top = tr.y.saturating_add(1);
                                if m.row >= inner_top {
                                    let row_in_area = (m.row - inner_top) as usize;
                                    if row_in_area < th {
                                        let gline = g.scroll_lines + row_in_area;
                                        let hits_len = g
                                            .transcript_cache
                                            .as_ref()
                                            .map(|c| c.hits.len())
                                            .unwrap_or(0);
                                        let hit = if gline < hits_len {
                                            g.transcript_cache
                                                .as_ref()
                                                .and_then(|c| c.hits.get(gline).cloned())
                                                .flatten()
                                        } else {
                                            None
                                        };
                                        if let Some((qid, sel)) = hit {
                                            drop(g);
                                            if let Some(ref tx) = question_answer_tx {
                                                let _ = tx.try_send((qid, sel));
                                            } else {
                                                let _ =
                                                    cmd_tx.try_send(TuiCmd::QuestionAnswer(sel));
                                            }
                                            continue;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    if let Some(sr) = slash_r
                        && rect_contains(sr, m.column, m.row)
                        && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                    {
                        let inner_y = m.row.saturating_sub(sr.y).saturating_sub(1);
                        if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
                            let n_show = slash_filtered.len().min(SLASH_PANEL_MAX_ROWS);
                            let max_scroll = slash_filtered.len().saturating_sub(n_show);
                            let list_scroll = g
                                .slash_menu_index
                                .saturating_sub(n_show.saturating_sub(1))
                                .min(max_scroll);
                            if (inner_y as usize) < n_show {
                                let idx = list_scroll + inner_y as usize;
                                if idx < slash_filtered.len() {
                                    g.input_buffer = slash_filtered[idx].command_str();
                                    g.cursor_char_idx = g.input_buffer.chars().count();
                                    g.slash_menu_index = idx;
                                }
                            }
                        } else if !at_matches.is_empty() {
                            let n_show = at_matches.len().min(SLASH_PANEL_MAX_ROWS);
                            let max_scroll = at_matches.len().saturating_sub(n_show);
                            let pick = g.at_menu_index.min(at_matches.len().saturating_sub(1));
                            let list_scroll = pick
                                .saturating_sub(n_show.saturating_sub(1))
                                .min(max_scroll);
                            if (inner_y as usize) < n_show {
                                let idx = list_scroll + inner_y as usize;
                                if let Some(choice) = at_matches.get(idx) {
                                    let cur = g.cursor_char_idx;
                                    let (buf, cidx) =
                                        apply_at_completion(&g.input_buffer, cur, choice);
                                    g.input_buffer = buf;
                                    g.cursor_char_idx = cidx;
                                }
                            }
                        } else if !dollar_matches.is_empty() {
                            let n_show = dollar_matches.len().min(SLASH_PANEL_MAX_ROWS);
                            let max_scroll = dollar_matches.len().saturating_sub(n_show);
                            let pick = g.at_menu_index.min(dollar_matches.len().saturating_sub(1));
                            let list_scroll = pick
                                .saturating_sub(n_show.saturating_sub(1))
                                .min(max_scroll);
                            if (inner_y as usize) < n_show {
                                let idx = list_scroll + inner_y as usize;
                                if let Some(choice) = dollar_matches.get(idx)
                                    && let Some((buf, cidx)) =
                                        apply_skill_reference_completion_with_space(
                                            &g.input_buffer,
                                            cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                                            choice,
                                        )
                                {
                                    g.input_buffer = buf;
                                    g.cursor_char_idx = cidx;
                                    g.at_menu_index = idx;
                                }
                            }
                        }
                    }

                    // Check click on branch chip in status bar.
                    if let Some(bounds) = g.branch_chip_bounds
                        && rect_contains(bounds, m.column, m.row)
                        && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                    {
                        let _ = cmd_tx.try_send(TuiCmd::OpenBranchPicker);
                    }
                }
                Event::Key(key) => {
                    let dollar_matches = matching_skill_reference_commands(
                        &g.input_buffer,
                        cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                        &dollar_skill_commands,
                    );
                    if g.command_palette_open() {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                                g.close_command_palette();
                                g.command_palette_query_mut().unwrap().clear();
                                *g.palette_index_mut().unwrap() = 0;
                            }
                            (KeyCode::Up, _) => {
                                if g.palette_index() > 0 {
                                    *g.palette_index_mut().unwrap() -= 1;
                                }
                            }
                            (KeyCode::Down, _) => {
                                let filtered = filter_palette_rows(g.command_palette_query());
                                let selectable = palette_selectable_indices(&filtered);
                                if !selectable.is_empty() {
                                    *g.palette_index_mut().unwrap() = (g.palette_index() + 1)
                                        .min(selectable.len().saturating_sub(1));
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let filtered = filter_palette_rows(g.command_palette_query());
                                let selectable = palette_selectable_indices(&filtered);
                                let pick =
                                    g.palette_index().min(selectable.len().saturating_sub(1));
                                let command = if let Some(&abs_idx) = selectable.get(pick)
                                    && let PaletteRow::Entry { label, .. } = filtered[abs_idx]
                                {
                                    Some(palette_command_for_label(label).to_string())
                                } else {
                                    None
                                };
                                g.close_command_palette();
                                g.command_palette_query_mut().unwrap().clear();
                                *g.palette_index_mut().unwrap() = 0;
                                if let Some(command) = command {
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::Submit(command));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.command_palette_query_mut().unwrap().pop();
                                let filtered = filter_palette_rows(g.command_palette_query());
                                let selectable = palette_selectable_indices(&filtered);
                                *g.palette_index_mut().unwrap() =
                                    g.palette_index().min(selectable.len().saturating_sub(1));
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.command_palette_query_mut().unwrap().push(c);
                                let filtered = filter_palette_rows(g.command_palette_query());
                                let selectable = palette_selectable_indices(&filtered);
                                *g.palette_index_mut().unwrap() =
                                    g.palette_index().min(selectable.len().saturating_sub(1));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Info modal (read-only scrollable popup).
                    if g.info_modal_open() {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                                g.close_info_modal();
                            }
                            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                                *g.info_modal_scroll_mut().unwrap() =
                                    g.info_modal_scroll().saturating_sub(1);
                            }
                            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                                let max_vis = 16usize;
                                let max_scroll = g.info_modal_lines().len().saturating_sub(max_vis);
                                *g.info_modal_scroll_mut().unwrap() =
                                    (g.info_modal_scroll() + 1).min(max_scroll);
                            }
                            (KeyCode::Home, _) => {
                                *g.info_modal_scroll_mut().unwrap() = 0;
                            }
                            (KeyCode::End, _) => {
                                let max_vis = 16usize;
                                *g.info_modal_scroll_mut().unwrap() =
                                    g.info_modal_lines().len().saturating_sub(max_vis);
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Enter in the skill picker only edits the draft. The
                    // resulting command is submitted later through Chat.
                    if g.skill_picker_open() {
                        handle_skill_picker_key(&mut g, key);
                        continue;
                    }

                    // Model picker popup.
                    if g.model_picker_open() {
                        let filter = g.model_picker_search().to_ascii_lowercase();
                        let selectable_count = g
                            .model_picker_entries()
                            .iter()
                            .filter(|e| {
                                !e.is_header
                                    && (filter.is_empty()
                                        || e.label.to_ascii_lowercase().contains(&filter)
                                        || e.detail.to_ascii_lowercase().contains(&filter))
                            })
                            .count();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_model_picker();
                            }
                            (KeyCode::Up, _) => {
                                if selectable_count > 0 {
                                    *g.model_picker_index_mut().unwrap() = g
                                        .model_picker_index()
                                        .saturating_sub(1)
                                        .min(selectable_count - 1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                if selectable_count > 0 {
                                    *g.model_picker_index_mut().unwrap() =
                                        (g.model_picker_index() + 1).min(selectable_count - 1);
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let selectable: Vec<&ModelPickerEntry> = g
                                    .model_picker_entries()
                                    .iter()
                                    .filter(|e| {
                                        !e.is_header
                                            && (filter.is_empty()
                                                || e.label.to_ascii_lowercase().contains(&filter)
                                                || e.detail.to_ascii_lowercase().contains(&filter))
                                    })
                                    .collect();
                                let pick = g
                                    .model_picker_index()
                                    .min(selectable.len().saturating_sub(1));
                                if let Some(entry) = selectable.get(pick) {
                                    let action = entry.action.clone();
                                    g.close_model_picker();
                                    drop(g);
                                    match action {
                                        ModelPickerAction::SwitchProvider(p) => {
                                            let _ = cmd_tx.try_send(TuiCmd::ApplyModelProvider(p));
                                        }
                                        ModelPickerAction::ApplyModel(m) => {
                                            let _ = cmd_tx.try_send(TuiCmd::ApplyModel(m));
                                        }
                                    }
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.model_picker_search_mut().unwrap().pop();
                                *g.model_picker_index_mut().unwrap() = 0;
                                *g.model_picker_scroll_mut().unwrap() = 0;
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.model_picker_search_mut().unwrap().push(c);
                                *g.model_picker_index_mut().unwrap() = 0;
                                *g.model_picker_scroll_mut().unwrap() = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Connect provider (OpenCode-style `/connect`) and custom setup.
                    if dispatch_custom_provider_key(&mut g, key, &cmd_tx) {
                        continue;
                    }

                    if g.api_key_modal_open() {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_api_key_modal();
                                if g.onboarding_mode {
                                    // Go back to connect modal instead of closing entirely
                                    g.open_connect_modal();
                                }
                            }
                            (KeyCode::Enter, _) => {
                                if g.onboarding_mode {
                                    // Block input while validation is in flight
                                    if matches!(
                                        g.validation_status,
                                        Some(crate::tui::state::OnboardingValidation::Validating)
                                    ) {
                                        // Already validating — ignore
                                    } else if let Some(provider) = g.api_key_target_provider() {
                                        let key = g.api_key_input().trim().to_string();
                                        if key.is_empty() {
                                            // Don't submit empty keys during onboarding
                                        } else {
                                            g.validation_status = Some(
                                                crate::tui::state::OnboardingValidation::Validating,
                                            );
                                            drop(g);
                                            let _ = cmd_tx
                                                .try_send(TuiCmd::ValidateApiKey(provider, key));
                                        }
                                    }
                                } else {
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::Submit(String::new()));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.api_key_input_mut().unwrap().pop();
                                if g.onboarding_mode {
                                    g.validation_status = None;
                                }
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.api_key_input_mut().unwrap().push(c);
                                if g.onboarding_mode {
                                    g.validation_status = None; // Clear stale error on new input
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Provider picker (settings).
                    if g.provider_picker_open() {
                        let n = g.provider_picker_visible_row_count();
                        let n_builtin = ProviderKind::ALL.len();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_provider_picker();
                            }
                            (KeyCode::Up, _) => {
                                if n > 0 {
                                    *g.provider_picker_index_mut().unwrap() =
                                        g.provider_picker_index().saturating_sub(1);
                                }
                                g.sync_provider_picker_scroll();
                            }
                            (KeyCode::Down, _) => {
                                if n > 0 {
                                    *g.provider_picker_index_mut().unwrap() =
                                        (g.provider_picker_index() + 1) % n;
                                }
                                g.sync_provider_picker_scroll();
                            }
                            (KeyCode::Char('c'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.close_provider_picker();
                                g.open_info_modal(
                                    "custom API",
                                    vec![
                                        "Use a custom base URL that speaks an OpenAI- or Anthropic-compatible HTTP API."
                                            .to_string(),
                                        String::new(),
                                        "Option A — use this wizard: /provider then choose \"Add custom provider…\"."
                                            .to_string(),
                                        String::new(),
                                        "Option B — composer commands:".to_string(),
                                        "  /custom openai <base-url> [api-key] [model]".to_string(),
                                        "  /custom anthropic <base-url> [api-key] [model]".to_string(),
                                        String::new(),
                                        "Then pick \"Custom\" in /provider or run /provider custom.".to_string(),
                                    ],
                                );
                            }
                            (KeyCode::Enter, _) => {
                                if n == 0 {
                                    g.close_provider_picker();
                                    continue;
                                }
                                let for_key = g.provider_picker_for_api_key();
                                if g.provider_picker_include_add_row()
                                    && *g.provider_picker_index_mut().unwrap() == n_builtin
                                {
                                    g.close_provider_picker();
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::ConfigureCustomProvider);
                                    continue;
                                }
                                let p =
                                    ProviderKind::ALL[g.provider_picker_index().min(n_builtin - 1)];
                                g.close_provider_picker();
                                if for_key {
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::PromptApiKey(p, false));
                                } else {
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::ApplyDefaultProvider(p));
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Branch picker keyboard handling.
                    if g.branch_picker_open() {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_branch_picker();
                            }
                            (KeyCode::Up, _) => {
                                if !filtered_branch_indices(
                                    g.branch_picker_branches(),
                                    g.branch_picker_query(),
                                )
                                .is_empty()
                                {
                                    *g.branch_picker_index_mut().unwrap() =
                                        g.branch_picker_index().saturating_sub(1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                let n = filtered_branch_indices(
                                    g.branch_picker_branches(),
                                    g.branch_picker_query(),
                                )
                                .len();
                                if n > 0 {
                                    *g.branch_picker_index_mut().unwrap() =
                                        (g.branch_picker_index() + 1).min(n - 1);
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let cmd = branch_picker_enter_command(
                                    g.branch_picker_branches(),
                                    g.branch_picker_query(),
                                    g.branch_picker_index(),
                                );
                                g.close_branch_picker();
                                if let Some(c) = cmd {
                                    drop(g);
                                    let _ = cmd_tx.try_send(c);
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.branch_picker_query_mut().unwrap().pop();
                                let filtered = filtered_branch_indices(
                                    g.branch_picker_branches(),
                                    g.branch_picker_query(),
                                );
                                *g.branch_picker_index_mut().unwrap() = g
                                    .branch_picker_index()
                                    .min(filtered.len().saturating_sub(1));
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.branch_picker_query_mut().unwrap().push(c);
                                *g.branch_picker_index_mut().unwrap() = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Phase 0 invariant: approval hotkeys own Ctrl+Y/N/U even
                    // when a question modal is also active.
                    let primary_mode =
                        primary_input_mode(g.active_approval.is_some(), g.question_modal_open());
                    if matches!(primary_mode, PrimaryInputMode::Approval)
                        && approval_shortcut_action(key.code, key.modifiers).is_some()
                        && let Some(answer) = handle_approval_key(&mut g, key)
                    {
                        g.mark_transcript_dirty();
                        drop(g);
                        if let Some(ref tx) = approval_answer_tx {
                            let _ = tx.try_send(answer);
                        }
                        continue;
                    }

                    // Question modal keyboard handling.
                    if matches!(primary_mode, PrimaryInputMode::QuestionModal) {
                        if let Some(ref q) = g.active_question.clone() {
                            // Total items: 1 (suggested) + options.len() + (1 if allow_custom for "Chat about this")
                            let total = 1 + q.options.len() + if q.allow_custom { 1 } else { 0 };
                            match (key.code, key.modifiers) {
                                (KeyCode::Esc, _) => {
                                    if q.allow_custom {
                                        // Fall back to inline text input
                                        g.close_question_modal();
                                    }
                                    // If !allow_custom, Esc is a no-op
                                }
                                (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                                    *g.question_modal_index_mut().unwrap() =
                                        g.question_modal_index().saturating_sub(1);
                                }
                                (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                                    *g.question_modal_index_mut().unwrap() =
                                        (g.question_modal_index() + 1).min(total - 1);
                                }
                                (KeyCode::Enter, _) => {
                                    let idx = g.question_modal_index();
                                    let sel = if idx == 0 {
                                        // Suggested answer
                                        Some(QuestionSelection::Suggested)
                                    } else if idx <= q.options.len() {
                                        // Regular option (1-based → 0-based)
                                        Some(QuestionSelection::Option {
                                            option_id: q.options[idx - 1].id.clone(),
                                        })
                                    } else {
                                        // "Chat about this" — fall back to inline text input
                                        None
                                    };

                                    if let Some(sel) = sel {
                                        let qid = q.question_id.clone();
                                        g.close_question_modal();
                                        drop(g);
                                        let sent = if let Some(ref tx) = question_answer_tx {
                                            tx.try_send((qid.clone(), sel.clone())).is_ok()
                                        } else {
                                            cmd_tx.try_send(TuiCmd::QuestionAnswer(sel)).is_ok()
                                        };
                                        if !sent && let Ok(mut g) = state.lock() {
                                            // Restore so the user can retry if the side-channel dropped.
                                            g.active_question = Some(q.clone());
                                            g.open_question_modal();
                                            g.push_error(
                                                "failed to submit question answer; try again"
                                                    .into(),
                                            );
                                        }
                                    } else {
                                        // "Chat about this" — close modal, keep active_question
                                        g.close_question_modal();
                                    }
                                }
                                _ => {}
                            }
                        }
                        continue;
                    }

                    // Permission picker keyboard handling.
                    if g.permission_picker_open() {
                        const PERM_COUNT: usize = 5;
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_permission_picker();
                            }
                            (KeyCode::Up, _) => {
                                *g.permission_picker_index_mut().unwrap() =
                                    g.permission_picker_index().saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                *g.permission_picker_index_mut().unwrap() =
                                    (g.permission_picker_index() + 1).min(PERM_COUNT - 1);
                            }
                            (KeyCode::Enter, _) => {
                                let idx = g.permission_picker_index();
                                g.close_permission_picker();
                                drop(g);
                                let _ = cmd_tx.try_send(TuiCmd::ApplyPermission(idx));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Agent profile picker keyboard handling.
                    if g.agent_picker_open() {
                        const AGENT_COUNT: usize = 5;
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_agent_picker();
                            }
                            (KeyCode::Up, _) => {
                                *g.agent_picker_index_mut().unwrap() =
                                    g.agent_picker_index().saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                *g.agent_picker_index_mut().unwrap() =
                                    (g.agent_picker_index() + 1).min(AGENT_COUNT - 1);
                            }
                            (KeyCode::Enter, _) => {
                                let idx = g.agent_picker_index();
                                g.close_agent_picker();
                                drop(g);
                                let _ = cmd_tx.try_send(TuiCmd::SwitchAgent(idx));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Session picker keyboard handling.
                    if g.session_picker_open() {
                        let filter = g.session_picker_search().to_ascii_lowercase();
                        let count = g
                            .session_picker_entries()
                            .iter()
                            .filter(|s| {
                                filter.is_empty() || s.to_ascii_lowercase().contains(&filter)
                            })
                            .count();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_session_picker();
                            }
                            (KeyCode::Up, _) => {
                                *g.session_picker_index_mut().unwrap() =
                                    g.session_picker_index().saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                if count > 0 {
                                    *g.session_picker_index_mut().unwrap() =
                                        (g.session_picker_index() + 1).min(count.saturating_sub(1));
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let filtered: Vec<&String> = g
                                    .session_picker_entries()
                                    .iter()
                                    .filter(|s| {
                                        filter.is_empty()
                                            || s.to_ascii_lowercase().contains(&filter)
                                    })
                                    .collect();
                                let pick = g
                                    .session_picker_index()
                                    .min(filtered.len().saturating_sub(1));
                                if let Some(id) = filtered.get(pick) {
                                    let id = (*id).clone();
                                    g.close_session_picker();
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::ResumeSession(id));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.session_picker_search_mut().unwrap().pop();
                                *g.session_picker_index_mut().unwrap() = 0;
                                *g.session_picker_scroll_mut().unwrap() = 0;
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.session_picker_search_mut().unwrap().push(c);
                                *g.session_picker_index_mut().unwrap() = 0;
                                *g.session_picker_scroll_mut().unwrap() = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Ctrl+X leader key dispatch.
                    if g.leader_pending {
                        dispatch_leader_key(&mut g, key, &cmd_tx);
                        continue;
                    }

                    if handle_newline_key(&mut g, key) {
                        continue;
                    }

                    if handle_prompt_history_key(&mut g, key) {
                        continue;
                    }

                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) if escape_cancels_active_turn(&g) => {
                            if let Some(ref flag) = cancel_flag {
                                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                            g.dismiss_interactive_prompts();
                            g.blocks
                                .push(DisplayBlock::System("Cancelling current run...".into()));
                            let _ = cmd_tx.try_send(TuiCmd::CancelTurn);
                        }
                        (KeyCode::Char('c' | 'C'), mods)
                            if mods.contains(KeyModifiers::CONTROL)
                                && mods.contains(KeyModifiers::SHIFT) =>
                        {
                            drop(g);
                            let _ = cmd_tx.try_send(TuiCmd::CopyLastAssistant);
                        }
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            if let Some(ref flag) = cancel_flag {
                                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                            g.dismiss_interactive_prompts();
                            let _ = cmd_tx.try_send(TuiCmd::CancelTurn);
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            g.blocks.clear();
                            g.streaming_assistant = None;
                            g.scroll_lines = 0;
                            g.transcript_follow_tail = true;
                            g.mark_transcript_dirty();
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            g.open_command_palette();
                            g.command_palette_query_mut().unwrap().clear();
                            *g.palette_index_mut().unwrap() = 0;
                        }
                        (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                            g.leader_pending = true;
                        }
                        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                            if g.active_approval.is_some() || g.active_question.is_some() {
                                continue;
                            }
                            g.blocks
                                .push(DisplayBlock::System("[image] reading clipboard…".into()));
                            g.mark_transcript_dirty();
                            drop(g);
                            let _ = cmd_tx.try_send(TuiCmd::PasteClipboard);
                        }
                        (KeyCode::Tab, _) => {
                            if !g.prompt_history_is_navigating()
                                && !workspace_files_indexing
                                && let Some((buf, cidx)) = apply_selected_at_completion(
                                    &workspace_files,
                                    &g.input_buffer,
                                    g.cursor_char_idx,
                                    g.at_menu_index,
                                    false,
                                )
                            {
                                g.detach_prompt_history();
                                g.input_buffer = buf;
                                g.cursor_char_idx = cidx;
                            } else if !g.prompt_history_is_navigating()
                                && let Some(choice) = dollar_matches.get(
                                    g.at_menu_index.min(dollar_matches.len().saturating_sub(1)),
                                )
                                && let Some((buf, cidx)) =
                                    apply_skill_reference_completion_with_space(
                                        &g.input_buffer,
                                        cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                                        choice,
                                    )
                            {
                                g.detach_prompt_history();
                                g.input_buffer = buf;
                                g.cursor_char_idx = cidx;
                            } else {
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    let pick = g.slash_menu_index % slash_filtered.len();
                                    g.input_buffer = slash_filtered[pick].command_str();
                                    g.cursor_char_idx = g.input_buffer.chars().count();
                                } else {
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::CycleAgent);
                                }
                            }
                        }
                        (KeyCode::F(2), KeyModifiers::NONE) => {
                            drop(g);
                            let _ = cmd_tx.try_send(TuiCmd::CycleModel(true));
                        }
                        (KeyCode::F(2), KeyModifiers::SHIFT) => {
                            drop(g);
                            let _ = cmd_tx.try_send(TuiCmd::CycleModel(false));
                        }
                        (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.input_buffer.clear();
                                g.cursor_char_idx = 0;
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.try_send(ApprovalAnswer::Verdict {
                                        call_id,
                                        approved: true,
                                    });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.input_buffer.clear();
                                g.cursor_char_idx = 0;
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.try_send(ApprovalAnswer::Verdict {
                                        call_id,
                                        approved: false,
                                    });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let input_json: serde_json::Value =
                                    serde_json::from_str(&req.input).unwrap_or_default();
                                let pattern = suggest_allow_pattern(&req.tool, &input_json);
                                let call_id = req.call_id.clone();
                                g.input_buffer.clear();
                                g.cursor_char_idx = 0;
                                g.blocks.push(DisplayBlock::System(format!(
                                    "Always allowing: {pattern}"
                                )));
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.try_send(ApprovalAnswer::AllowPattern {
                                        call_id,
                                        pattern,
                                    });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Enter, _) => {
                            if !g.prompt_history_is_navigating()
                                && !workspace_files_indexing
                                && let Some((buf, cidx)) = apply_selected_at_completion(
                                    &workspace_files,
                                    &g.input_buffer,
                                    g.cursor_char_idx,
                                    g.at_menu_index,
                                    true,
                                )
                            {
                                g.detach_prompt_history();
                                g.input_buffer = buf;
                                g.cursor_char_idx = cidx;
                                continue;
                            }
                            if !g.prompt_history_is_navigating()
                                && let Some(choice) = dollar_matches.get(
                                    g.at_menu_index.min(dollar_matches.len().saturating_sub(1)),
                                )
                                && let Some((buf, cidx)) =
                                    apply_skill_reference_completion_with_space(
                                        &g.input_buffer,
                                        cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                                        choice,
                                    )
                            {
                                g.input_buffer = buf;
                                g.cursor_char_idx = cidx;
                                continue;
                            }
                            let line = std::mem::take(&mut g.input_buffer);
                            g.cursor_char_idx = 0;
                            g.slash_menu_index = 0;
                            let active_approval = g.active_approval.clone();
                            let active_q = g.active_question.clone();
                            if let Some(req) = active_approval {
                                let t = line.trim();
                                if t.is_empty() {
                                    g.blocks.push(DisplayBlock::System(
                                        "Empty line — type y or n (or yes/no, ok, deny). Ctrl+Y = approve, Ctrl+N = deny."
                                            .into(),
                                    ));
                                    continue;
                                }
                                if t.starts_with('/') {
                                    let lower = t.to_lowercase();
                                    let slash_verdict = match lower.as_str() {
                                        "/approve" | "/y" | "/yes" | "/ok" => Some(true),
                                        "/deny" | "/n" | "/no" => Some(false),
                                        _ => None,
                                    };
                                    if let Some(approved) = slash_verdict {
                                        let call_id = req.call_id.clone();
                                        drop(g);
                                        if let Some(ref tx) = approval_answer_tx {
                                            let _ = tx.try_send(ApprovalAnswer::Verdict {
                                                call_id,
                                                approved,
                                            });
                                        } else {
                                            let _ = cmd_tx.try_send(TuiCmd::CancelTurn);
                                        }
                                        continue;
                                    }
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::Submit(line));
                                    continue;
                                }
                                if let Some(approved) = parse_approval_verdict(t) {
                                    let call_id = req.call_id.clone();
                                    drop(g);
                                    if let Some(ref tx) = approval_answer_tx {
                                        let _ = tx.try_send(ApprovalAnswer::Verdict {
                                            call_id,
                                            approved,
                                        });
                                    } else {
                                        let _ = cmd_tx.try_send(TuiCmd::CancelTurn);
                                    }
                                    continue;
                                }
                                g.blocks.push(DisplayBlock::System(
                                    "Could not parse approval — try y, n, yes, no, ok, deny, or Ctrl+Y / Ctrl+N."
                                        .into(),
                                ));
                                continue;
                            }
                            if let Some(ref q) = active_q {
                                let t = line.trim();
                                // `/auto-answer` must go through the side channel: `run_turn` is often
                                // blocked on this question, so `cmd_rx` is not polled for Submit.
                                if t == "/auto-answer" {
                                    let qid = q.question_id.clone();
                                    drop(g);
                                    if let Some(ref tx) = question_answer_tx {
                                        let _ = tx.try_send((qid, QuestionSelection::Suggested));
                                    } else {
                                        let _ = cmd_tx.try_send(TuiCmd::QuestionAnswer(
                                            QuestionSelection::Suggested,
                                        ));
                                    }
                                    continue;
                                }
                                if t.starts_with('/') {
                                    drop(g);
                                    let _ = cmd_tx.try_send(TuiCmd::Submit(line));
                                    continue;
                                }
                                if let Some(sel) = parse_tui_question_answer(&line, q) {
                                    let qid = q.question_id.clone();
                                    drop(g);
                                    if let Some(ref tx) = question_answer_tx {
                                        let _ = tx.try_send((qid, sel));
                                    } else {
                                        let _ = cmd_tx.try_send(TuiCmd::QuestionAnswer(sel));
                                    }
                                    continue;
                                }
                                g.blocks.push(DisplayBlock::System(
                                    "Invalid answer: use Enter/0 for suggested, 1–n for an option, or custom text."
                                        .into(),
                                ));
                                continue;
                            }
                            drop(g);
                            let _ = cmd_tx.try_send(TuiCmd::Submit(line));
                        }
                        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                            g.detach_prompt_history();
                            g.cursor_char_idx = 0;
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            g.detach_prompt_history();
                            g.cursor_char_idx = g.input_buffer.chars().count();
                        }
                        (KeyCode::Home, _) => {
                            if !g.input_buffer.is_empty() {
                                g.detach_prompt_history();
                                g.cursor_char_idx =
                                    move_cursor_home(&g.input_buffer, g.cursor_char_idx);
                            }
                        }
                        (KeyCode::PageUp, _) => {
                            let size = terminal.size()?;
                            let area = Rect::new(0, 0, size.width, size.height);
                            let (max_scroll, page) = transcript_scroll_metrics(
                                &mut g,
                                area,
                                &slash_entries,
                                &workspace_files,
                                &dollar_skill_commands,
                            );
                            apply_transcript_page_scroll(&mut g, max_scroll, page, false);
                        }
                        (KeyCode::PageDown, _) => {
                            let size = terminal.size()?;
                            let area = Rect::new(0, 0, size.width, size.height);
                            let (max_scroll, page) = transcript_scroll_metrics(
                                &mut g,
                                area,
                                &slash_entries,
                                &workspace_files,
                                &dollar_skill_commands,
                            );
                            apply_transcript_page_scroll(&mut g, max_scroll, page, true);
                        }
                        (KeyCode::End, _) => {
                            if !g.input_buffer.is_empty() {
                                g.detach_prompt_history();
                                g.cursor_char_idx =
                                    move_cursor_end(&g.input_buffer, g.cursor_char_idx);
                            } else {
                                let sz = terminal.size().ok();
                                if let Some(sz) = sz {
                                    let area = Rect::new(0, 0, sz.width, sz.height);
                                    let (main_area, _) = layout_with_sidebar(area);
                                    let sh = composer_chrome_height(
                                        &slash_entries,
                                        &workspace_files,
                                        &g.input_buffer,
                                        g.cursor_char_idx,
                                    );
                                    let input_h = composer_input_height_with_width(
                                        &g.input_buffer,
                                        usize::from(!g.staged_image_attachments.is_empty()) + 1,
                                        main_area.width as usize,
                                    );
                                    let (tr, _, _, _) = layout_chunks(main_area, sh, input_h);
                                    let total =
                                        transcript_lines(&g, tr.width.saturating_sub(2)).len();
                                    let th = tr.height.saturating_sub(2) as usize;
                                    let max_scroll = total.saturating_sub(th);
                                    g.transcript_follow_tail = true;
                                    g.scroll_lines = max_scroll;
                                }
                            }
                        }
                        (KeyCode::Left, _) => {
                            g.detach_prompt_history();
                            g.cursor_char_idx = g.cursor_char_idx.saturating_sub(1);
                        }
                        (KeyCode::Right, _) => {
                            g.detach_prompt_history();
                            let max = g.input_buffer.chars().count();
                            g.cursor_char_idx = (g.cursor_char_idx + 1).min(max);
                        }
                        (KeyCode::Up, _) => {
                            // A recalled entry owns the arrow keys until the
                            // user edits or moves the cursor. On an empty
                            // composer, Up enters the newest history entry;
                            // an empty history is a safe no-op.
                            if g.prompt_history_is_navigating() || g.input_buffer.is_empty() {
                                if let Some((buffer, cursor)) = g.prompt_history_previous() {
                                    g.input_buffer = buffer;
                                    g.cursor_char_idx = cursor;
                                }
                                continue;
                            }
                            let at_matches = at_completion_matches(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                            );
                            if !at_matches.is_empty()
                                && at_completion_active(&g.input_buffer, g.cursor_char_idx)
                            {
                                g.at_menu_index = g.at_menu_index.saturating_sub(1);
                            } else {
                                let dollar_matches = matching_skill_reference_commands(
                                    &g.input_buffer,
                                    cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                                    &dollar_skill_commands,
                                );
                                if !dollar_matches.is_empty() {
                                    g.at_menu_index = g.at_menu_index.saturating_sub(1);
                                    continue;
                                }
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    g.slash_menu_index = g.slash_menu_index.saturating_sub(1);
                                } else if !g.input_buffer.is_empty() {
                                    let composer_width = terminal
                                        .size()
                                        .ok()
                                        .map(|sz| {
                                            let area = Rect::new(0, 0, sz.width, sz.height);
                                            layout_with_sidebar(area).0.width as usize
                                        })
                                        .unwrap_or(usize::MAX);
                                    g.cursor_char_idx = move_cursor_vertical_with_width(
                                        &g.input_buffer,
                                        g.cursor_char_idx,
                                        false,
                                        composer_width,
                                    );
                                } else {
                                    g.transcript_follow_tail = false;
                                    g.scroll_lines = g.scroll_lines.saturating_sub(1);
                                }
                            }
                        }
                        (KeyCode::Down, _) => {
                            // While recalling, Down advances toward newer
                            // entries and then restores the pre-history draft.
                            // Empty non-history input no longer scrolls the
                            // transcript; PageDown owns that action.
                            if g.prompt_history_is_navigating() || g.input_buffer.is_empty() {
                                if let Some((buffer, cursor)) = g.prompt_history_next() {
                                    g.input_buffer = buffer;
                                    g.cursor_char_idx = cursor;
                                }
                                continue;
                            }
                            let at_matches = at_completion_matches(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                            );
                            if !at_matches.is_empty()
                                && at_completion_active(&g.input_buffer, g.cursor_char_idx)
                            {
                                let n = at_matches.len();
                                g.at_menu_index = (g.at_menu_index + 1) % n;
                            } else {
                                let dollar_matches = matching_skill_reference_commands(
                                    &g.input_buffer,
                                    cursor_byte_index(&g.input_buffer, g.cursor_char_idx),
                                    &dollar_skill_commands,
                                );
                                if !dollar_matches.is_empty() {
                                    let n = dollar_matches.len();
                                    g.at_menu_index = (g.at_menu_index + 1) % n;
                                    continue;
                                }
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    let n = slash_filtered.len();
                                    g.slash_menu_index = (g.slash_menu_index + 1) % n;
                                } else if !g.input_buffer.is_empty() {
                                    let composer_width = terminal
                                        .size()
                                        .ok()
                                        .map(|sz| {
                                            let area = Rect::new(0, 0, sz.width, sz.height);
                                            layout_with_sidebar(area).0.width as usize
                                        })
                                        .unwrap_or(usize::MAX);
                                    g.cursor_char_idx = move_cursor_vertical_with_width(
                                        &g.input_buffer,
                                        g.cursor_char_idx,
                                        true,
                                        composer_width,
                                    );
                                } else {
                                    let sz = terminal.size().ok();
                                    if let Some(sz) = sz {
                                        let area = Rect::new(0, 0, sz.width, sz.height);
                                        let (main_area, _) = layout_with_sidebar(area);
                                        let sh = composer_chrome_height(
                                            &slash_entries,
                                            &workspace_files,
                                            &g.input_buffer,
                                            g.cursor_char_idx,
                                        );
                                        let input_h = composer_input_height_with_width(
                                            &g.input_buffer,
                                            usize::from(!g.staged_image_attachments.is_empty()) + 1,
                                            main_area.width as usize,
                                        );
                                        let (tr, _, _, _) = layout_chunks(main_area, sh, input_h);
                                        let lines =
                                            transcript_lines(&g, tr.width.saturating_sub(2));
                                        let total = lines.len();
                                        let th = tr.height.saturating_sub(2) as usize;
                                        let max_scroll = total.saturating_sub(th);
                                        g.scroll_lines = (g.scroll_lines + 1).min(max_scroll);
                                        if g.scroll_lines >= max_scroll {
                                            g.transcript_follow_tail = true;
                                        }
                                    }
                                }
                            }
                        }
                        (KeyCode::Backspace, _) => {
                            if g.cursor_char_idx > 0 {
                                g.detach_prompt_history();
                                if let Some((buf, cidx)) =
                                    delete_completed_at_mention(&g.input_buffer, g.cursor_char_idx)
                                {
                                    g.input_buffer = buf;
                                    g.cursor_char_idx = cidx;
                                } else {
                                    let idx = g.cursor_char_idx;
                                    let mut cs: Vec<char> = g.input_buffer.chars().collect();
                                    cs.remove(idx - 1);
                                    g.input_buffer = cs.into_iter().collect();
                                    g.cursor_char_idx -= 1;
                                }
                                if slash_panel_visible(&g.input_buffer) {
                                    let f = filter_slash_entries(&slash_entries, &g.input_buffer);
                                    if !f.is_empty() {
                                        g.slash_menu_index =
                                            g.slash_menu_index.min(f.len().saturating_sub(1));
                                    } else {
                                        g.slash_menu_index = 0;
                                    }
                                }
                            }
                        }
                        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                            g.detach_prompt_history();
                            let idx = g.cursor_char_idx;
                            let mut cs: Vec<char> = g.input_buffer.chars().collect();
                            cs.insert(idx, c);
                            g.input_buffer = cs.into_iter().collect();
                            g.cursor_char_idx += 1;
                            if slash_panel_visible(&g.input_buffer) {
                                let f = filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !f.is_empty() {
                                    g.slash_menu_index =
                                        g.slash_menu_index.min(f.len().saturating_sub(1));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    restore_terminal();
    terminal_restore_guard.disarm();
    let _ = execute!(stdout(), MoveToColumn(0));
    Ok(())
}

#[cfg(test)]
mod approval_parse_tests {
    use super::{
        ApprovalShortcutAction, PrimaryInputMode, QUESTION_POPUP_MAX_WIDTH, QuestionPopupLayout,
        TuiCmd, apply_selected_at_completion, apply_transcript_page_scroll,
        approval_shortcut_action, branch_picker_enter_command, delete_completed_at_mention,
        dispatch_custom_provider_key, dispatch_leader_key, dispatch_paste,
        escape_cancels_active_turn, filter_slash_entries, filtered_branch_indices,
        handle_newline_key, handle_prompt_history_key, load_slash_entries,
        matching_skill_reference_commands, model_identity_spans, primary_input_mode,
        question_popup_layout, render_question_popup, sidebar_footer_lines,
        transcript_scroll_metrics, workspace_sidebar_line,
    };
    use crate::skill_references::apply_skill_reference_completion_with_space;
    use crate::tui::composer::cursor_byte_index;
    use crate::tui::composer::{completed_at_mention_range_before_cursor, composer_line};
    use crate::tui::state::{CustomProviderSetupStep, TuiSessionState};
    use crate::tui::transcript::parse_approval_verdict;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nca_common::config::CustomProviderConfig;
    use nca_common::event::{AgentEvent, BusyState, InteractiveQuestionPayload, QuestionOption};
    use ratatui::{
        Terminal,
        backend::TestBackend,
        layout::Rect,
        text::{Line, Text},
        widgets::Paragraph,
    };
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    fn question(prompt: &str, options: usize, allow_custom: bool) -> InteractiveQuestionPayload {
        InteractiveQuestionPayload {
            question_id: "q".into(),
            call_id: "call".into(),
            prompt: prompt.into(),
            options: (0..options)
                .map(|i| QuestionOption {
                    id: format!("id-{i}"),
                    label: format!("Option {i}"),
                })
                .collect(),
            allow_custom,
            suggested_answer: "Suggested answer".into(),
        }
    }

    #[test]
    fn reasoning_status_segment_renders_only_when_present_at_wide_and_narrow_widths() {
        for (width, effort, expected) in [(64, Some("medium"), true), (24, None, false)] {
            let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
            let completed = terminal
                .draw(|frame| {
                    let line = Line::from(model_identity_spans("model", effort, "@build"));
                    frame.render_widget(Paragraph::new(line), frame.area());
                })
                .unwrap();
            let rendered = (0..width)
                .map(|x| {
                    completed
                        .buffer
                        .cell((x, 0))
                        .expect("cell in test backend")
                        .symbol()
                        .to_string()
                })
                .collect::<String>();
            assert_eq!(rendered.contains("reasoning: medium"), expected);
        }
    }

    #[test]
    fn workspace_sidebar_line_preserves_empty_fallback() {
        assert_eq!(workspace_sidebar_line(""), "—");
        let rendered = workspace_sidebar_line("/workspace/project/very-long-leaf");
        assert!(rendered.contains('…'));
        assert!(rendered.starts_with('/'));
        assert!(rendered.ends_with("leaf"));
    }

    #[test]
    fn sidebar_footer_renders_path_independent_navigation_hints() {
        let mut terminal = Terminal::new(TestBackend::new(32, 8)).unwrap();
        let completed = terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(Text::from(sidebar_footer_lines())),
                    frame.area(),
                );
            })
            .unwrap();
        let rendered = (0..8)
            .map(|y| {
                (0..32)
                    .map(|x| {
                        completed
                            .buffer
                            .cell((x, y))
                            .expect("cell in test backend")
                            .symbol()
                            .to_string()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let rendered = rendered.join("\n");
        assert!(rendered.contains("Ctrl+P commands"));
        assert!(rendered.contains("/skills browse skills"));
        assert!(rendered.contains("$skill inline guidance"));
        assert!(!rendered.contains("~/.local/share/ncacli"));
    }

    #[test]
    fn question_popup_grows_to_all_options_and_caps_width() {
        let q = question("Choose one", 5, true);
        let layout = question_popup_layout(Rect::new(0, 0, 120, 40), &q, 0, 0);

        assert_eq!(layout.option_start, 0);
        assert_eq!(layout.option_end, 7);
        assert!(layout.popup_area.width <= QUESTION_POPUP_MAX_WIDTH);
        assert!(layout.popup_area.height < 40);
        assert!(layout.indicator_lines.is_empty());
    }

    #[test]
    fn question_popup_wraps_long_prompt_and_option_labels() {
        let mut q = question(
            "A prompt with enough words to wrap in a narrow question popup",
            1,
            false,
        );
        q.options[0].label = "A deliberately long option label that must wrap".into();
        let layout = question_popup_layout(Rect::new(0, 0, 46, 24), &q, 0, 0);

        assert!(layout.prompt_lines.len() > 1);
        assert!(layout.option_line_counts[1] > 1);
        assert!(
            layout
                .footer_lines
                .iter()
                .any(|line| line.contains("Enter"))
        );
    }

    #[test]
    fn question_popup_scroll_keeps_selected_item_visible_with_indicators() {
        let q = question("Pick a value", 12, true);
        let layout = question_popup_layout(Rect::new(0, 0, 50, 10), &q, 13, 0);

        assert!(layout.option_start > 0);
        assert!(layout.option_start <= 13);
        assert!(layout.option_end > 13);
        assert!(
            layout
                .indicator_lines
                .iter()
                .any(|line| line.contains("above"))
        );
        assert!(!layout.footer_lines.is_empty());
    }

    #[test]
    fn question_popup_resize_recalculates_viewport() {
        let q = question("Pick a value", 12, true);
        let short = question_popup_layout(Rect::new(0, 0, 60, 10), &q, 0, 0);
        let tall = question_popup_layout(Rect::new(0, 0, 60, 30), &q, 0, 0);

        assert!(short.option_end < tall.option_end);
        assert_eq!(tall.option_end, 14);
        assert!(short.popup_area.height <= 10);
    }

    #[test]
    fn question_popup_custom_contract_controls_logical_items() {
        let without_chat = question("Pick", 3, false);
        let without = question_popup_layout(Rect::new(0, 0, 60, 30), &without_chat, 3, 0);
        assert_eq!(without.option_end, 4);
        assert_eq!(without.option_line_counts.len(), 4);

        let with_chat = question("Pick", 3, true);
        let with = question_popup_layout(Rect::new(0, 0, 60, 30), &with_chat, 4, 0);
        assert_eq!(with.option_end, 5);
        assert_eq!(with.option_line_counts.len(), 5);
    }

    fn render_question_for_test(
        width: u16,
        height: u16,
        q: InteractiveQuestionPayload,
        selected: usize,
    ) -> (Vec<String>, QuestionPopupLayout) {
        let mut state = state();
        state.active_question = Some(q);
        state.open_question_modal();
        *state.question_modal_index_mut().unwrap() = selected;

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut rendered_layout = None;
        let completed = terminal
            .draw(|frame| {
                let area = frame.area();
                rendered_layout = render_question_popup(frame, area, &mut state);
            })
            .unwrap();
        let layout = rendered_layout.expect("question renderer should return a layout");
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| {
                        completed
                            .buffer
                            .cell((x, y))
                            .expect("cell in test backend")
                            .symbol()
                            .to_string()
                    })
                    .collect::<String>()
            })
            .collect();
        (lines, layout)
    }

    #[test]
    fn production_question_renderer_draws_all_visible_options_and_footer() {
        let q = question("Choose one", 3, true);
        let (lines, layout) = render_question_for_test(80, 24, q, 0);

        assert_eq!(layout.option_start, 0);
        assert_eq!(layout.option_end, 5);
        let rendered = lines.join("\\n");
        assert!(rendered.contains("Suggested: Suggested answer"));
        assert!(rendered.contains("Option 0"));
        assert!(rendered.contains("Option 2"));
        assert!(rendered.contains("Chat about this"));
        assert!(rendered.contains("Enter"));
    }

    #[test]
    fn production_question_renderer_keeps_scrolled_footer_inside_popup() {
        let q = question("Pick a value", 12, true);
        let (lines, layout) = render_question_for_test(52, 10, q, 13);

        assert!(layout.option_start > 0);
        assert!(layout.option_end > 13);
        assert!(layout.popup_area.bottom() <= 10);
        let popup_lines =
            &lines[usize::from(layout.popup_area.y)..usize::from(layout.popup_area.bottom())];
        let rendered = popup_lines.join("\\n");
        assert!(rendered.contains("More above"));
        assert!(rendered.contains("Chat about this"));
        assert!(rendered.contains("Enter confirm"));
    }

    #[test]
    fn production_question_renderer_bounds_long_wrapped_options_before_footer() {
        let mut q = question("A long prompt that wraps", 1, false);
        q.options[0].label =
            "This is a deliberately long option label that wraps across many display rows".into();
        let (lines, layout) = render_question_for_test(46, 8, q, 0);

        assert!(layout.option_line_counts[1] > layout.option_row_budget);
        assert!(layout.popup_area.bottom() <= 8);
        let popup_lines =
            &lines[usize::from(layout.popup_area.y)..usize::from(layout.popup_area.bottom())];
        let rendered = popup_lines.join("\\n");
        assert!(rendered.contains("Enter") || rendered.contains("confirm"));
    }

    #[test]
    fn production_question_renderer_recalculates_on_resize() {
        let q = question("Pick a value", 12, true);
        let (short_lines, short) = render_question_for_test(60, 10, q.clone(), 0);
        let (tall_lines, tall) = render_question_for_test(60, 30, q, 0);

        assert!(short.option_end < tall.option_end);
        assert!(short.popup_area.height < tall.popup_area.height);
        assert!(short_lines.join("\\n").contains("Enter"));
        assert!(tall_lines.join("\\n").contains("Chat about this"));
    }

    #[test]
    fn question_popup_renderer_honors_custom_contract_and_escape() {
        let without_chat = question("Pick", 1, false);
        let (lines, layout) = render_question_for_test(60, 20, without_chat, 0);
        let rendered = lines.join("\\n");
        assert_eq!(layout.option_end, 2);
        assert!(!rendered.contains("Chat about this"));

        let with_chat = question("Pick", 1, true);
        let (lines, layout) = render_question_for_test(60, 20, with_chat, 0);
        assert_eq!(layout.option_end, 3);
        assert!(lines.join("\\n").contains("Chat about this"));
    }

    fn state() -> TuiSessionState {
        TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("/tmp"),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn composer_history_application_seam_preserves_completion_and_scratch_ownership() {
        let mut state = state();
        state.set_prompt_history(vec!["older".into(), "newest\n終わり  ".into()]);
        state.input_buffer.clear();
        state.cursor_char_idx = 0;

        assert!(handle_prompt_history_key(&mut state, key(KeyCode::Up)));
        assert_eq!(state.input_buffer, "newest\n終わり  ");
        assert_eq!(state.cursor_char_idx, state.input_buffer.chars().count());
        assert!(state.prompt_history_is_navigating());
        assert!(handle_prompt_history_key(&mut state, key(KeyCode::Up)));
        assert_eq!(state.input_buffer, "older");
        assert!(handle_prompt_history_key(&mut state, key(KeyCode::Down)));
        assert_eq!(state.input_buffer, "newest\n終わり  ");
        assert!(handle_prompt_history_key(&mut state, key(KeyCode::Down)));
        assert!(state.input_buffer.is_empty());
        assert_eq!(state.cursor_char_idx, 0);
        assert!(!state.prompt_history_is_navigating());

        state.input_buffer = "/he".into();
        state.cursor_char_idx = 3;
        state.slash_menu_index = 2;
        assert!(!handle_prompt_history_key(&mut state, key(KeyCode::Up)));
        assert_eq!(state.slash_menu_index, 2);
    }

    #[test]
    fn empty_history_arrows_are_noops_and_page_scroll_is_viewport_sized() {
        let mut state = state();
        state.input_buffer = "".into();
        state.scroll_lines = 7;
        assert!(handle_prompt_history_key(&mut state, key(KeyCode::Up)));
        assert_eq!(state.scroll_lines, 7);
        assert!(handle_prompt_history_key(&mut state, key(KeyCode::Down)));
        assert_eq!(state.scroll_lines, 7);

        state
            .blocks
            .extend((0..40).map(|i| crate::tui::state::DisplayBlock::User(format!("line {i}"))));
        let (max_scroll, page) =
            transcript_scroll_metrics(&mut state, Rect::new(0, 0, 100, 20), &[], &[], &[]);
        assert!(max_scroll > page);
        apply_transcript_page_scroll(&mut state, max_scroll, page, false);
        assert_eq!(state.scroll_lines, 0);
        assert!(!state.transcript_follow_tail);
        apply_transcript_page_scroll(&mut state, max_scroll, page, true);
        assert_eq!(state.scroll_lines, page);
        apply_transcript_page_scroll(&mut state, max_scroll, max_scroll, true);
        assert_eq!(state.scroll_lines, max_scroll);
        assert!(state.transcript_follow_tail);
    }

    fn type_text(state: &mut TuiSessionState, tx: &mpsc::Sender<TuiCmd>, text: &str) {
        for c in text.chars() {
            assert!(dispatch_custom_provider_key(
                state,
                key(KeyCode::Char(c)),
                tx
            ));
        }
    }

    fn clear_input(state: &mut TuiSessionState, tx: &mpsc::Sender<TuiCmd>) {
        let count = state.custom_setup_input().chars().count();
        for _ in 0..count {
            assert!(dispatch_custom_provider_key(
                state,
                key(KeyCode::Backspace),
                tx
            ));
        }
    }

    #[test]
    fn production_event_loop_routes_onboarding_custom_selection() {
        let mut state = state();
        state.onboarding_mode = true;
        state.open_connect_modal();
        state.connect_search_mut().unwrap().push_str("custom");
        let (tx, mut rx) = mpsc::channel(1);

        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert!(!state.connect_modal_open());
        assert!(matches!(rx.try_recv(), Ok(TuiCmd::ConfigureCustomProvider)));
    }

    #[test]
    fn production_event_loop_routes_connect_custom_selection() {
        let mut state = state();
        state.open_connect_modal();
        state.connect_search_mut().unwrap().push_str("custom");
        let (tx, mut rx) = mpsc::channel(1);

        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert!(matches!(rx.try_recv(), Ok(TuiCmd::ConfigureCustomProvider)));
    }

    #[test]
    fn production_event_loop_routes_unconfigured_custom_setup_to_probe() {
        let mut state = state();
        state.open_custom_provider_setup("gateway-model");
        let (tx, mut rx) = mpsc::channel(1);

        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert_eq!(
            state.custom_provider_setup_step(),
            CustomProviderSetupStep::BaseUrl
        );
        type_text(&mut state, &tx, "https://gateway.example/v1");
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        clear_input(&mut state, &tx);
        type_text(&mut state, &tx, "GATEWAY_API_KEY");
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Tab),
            &tx
        ));
        type_text(&mut state, &tx, "inline-secret");
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert_eq!(
            state.custom_provider_setup_step(),
            CustomProviderSetupStep::Model
        );
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));

        assert_eq!(
            state.custom_provider_setup_step(),
            CustomProviderSetupStep::Probe
        );
        let command = rx.try_recv().expect("custom setup should emit a command");
        assert!(matches!(
            command,
            TuiCmd::ApplyCustomProviderSetup {
                base_url,
                api_key_env,
                api_key: Some(api_key),
                model,
                ..
            } if base_url == "https://gateway.example/v1"
                && api_key_env == "GATEWAY_API_KEY"
                && api_key == "inline-secret"
                && model == "gateway-model"
        ));
    }

    #[test]
    fn production_event_loop_routes_configured_custom_edit_and_preserves_secret() {
        let mut state = state();
        state.open_custom_provider_setup_from_config(&CustomProviderConfig {
            base_url: "https://configured.example/v1".into(),
            api_key_env: "CONFIGURED_API_KEY".into(),
            api_key: Some("existing-secret".into()),
            model: "configured-model".into(),
            ..Default::default()
        });
        let (tx, mut rx) = mpsc::channel(1);

        assert_eq!(state.custom_setup_input(), "https://configured.example/v1");
        assert!(state.custom_setup_api_key().is_empty());
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        clear_input(&mut state, &tx);
        type_text(&mut state, &tx, "https://configured.example");
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));
        assert!(dispatch_custom_provider_key(
            &mut state,
            key(KeyCode::Enter),
            &tx
        ));

        let command = rx.try_recv().expect("custom setup should emit a command");
        assert!(matches!(
            command,
            TuiCmd::ApplyCustomProviderSetup {
                base_url,
                api_key_env,
                api_key: None,
                model,
                ..
            } if base_url == "https://configured.example"
                && api_key_env == "CONFIGURED_API_KEY"
                && model == "configured-model"
        ));
        assert_eq!(
            state.custom_provider_setup_step(),
            CustomProviderSetupStep::Probe
        );
    }

    #[test]
    fn parses_yes_with_punctuation_and_synonyms() {
        assert_eq!(parse_approval_verdict("yes"), Some(true));
        assert_eq!(parse_approval_verdict("Yes."), Some(true));
        assert_eq!(parse_approval_verdict("  OK! "), Some(true));
        assert_eq!(parse_approval_verdict("approve"), Some(true));
        assert_eq!(parse_approval_verdict("/approve"), Some(true));
        assert_eq!(parse_approval_verdict("/y"), Some(true));
    }

    #[test]
    fn parses_no_and_deny() {
        assert_eq!(parse_approval_verdict("n"), Some(false));
        assert_eq!(parse_approval_verdict("no."), Some(false));
        assert_eq!(parse_approval_verdict("deny"), Some(false));
        assert_eq!(parse_approval_verdict("/deny"), Some(false));
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(parse_approval_verdict("maybe"), None);
        assert_eq!(parse_approval_verdict("nope"), None);
        assert_eq!(parse_approval_verdict(""), None);
    }

    #[test]
    fn approval_priority_survives_question_modal() {
        assert_eq!(primary_input_mode(true, true), PrimaryInputMode::Approval);
        assert_eq!(
            approval_shortcut_action(KeyCode::Char('y'), KeyModifiers::CONTROL),
            Some(ApprovalShortcutAction::Approve)
        );
        assert_eq!(
            approval_shortcut_action(KeyCode::Char('n'), KeyModifiers::CONTROL),
            Some(ApprovalShortcutAction::Deny)
        );
        assert_eq!(
            approval_shortcut_action(KeyCode::Char('u'), KeyModifiers::CONTROL),
            Some(ApprovalShortcutAction::AllowPattern)
        );
    }

    #[test]
    fn branch_picker_switches_exact_match_from_typed_query() {
        let branches = vec![
            "interactive-question".into(),
            "main".into(),
            "self-autoresearch".into(),
        ];
        let cmd = branch_picker_enter_command(&branches, "main", 0);
        assert!(matches!(cmd, Some(TuiCmd::SwitchBranch(name)) if name == "main"));
    }

    #[test]
    fn branch_picker_creates_only_with_slash_prefix() {
        let branches = vec!["main".into()];
        let cmd = branch_picker_enter_command(&branches, "/feature-x", 0);
        assert!(matches!(cmd, Some(TuiCmd::CreateBranch(name)) if name == "feature-x"));
    }

    #[test]
    fn branch_picker_filters_case_insensitively() {
        let branches = vec!["Main".into(), "feature/login".into()];
        assert_eq!(filtered_branch_indices(&branches, "main"), vec![0]);
        assert_eq!(filtered_branch_indices(&branches, "LOGIN"), vec![1]);
    }

    #[test]
    fn branch_picker_switches_selected_filtered_branch_by_name() {
        let branches = vec!["alpha".into(), "main".into(), "main-fix".into()];
        let cmd = branch_picker_enter_command(&branches, "mai", 1);
        assert!(matches!(cmd, Some(TuiCmd::SwitchBranch(name)) if name == "main-fix"));
    }

    #[test]
    fn slash_panel_includes_supported_aliases_without_skill_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join(".agents/skills/custom-review");
        std::fs::create_dir_all(&skill_dir).expect("skill directory");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Custom Review\ncommand: custom-review\ndescription: Review\n---\nReview.\n",
        )
        .expect("skill file");

        let entries = load_slash_entries(dir.path(), &[PathBuf::from(".agents/skills")]);
        let quit = filter_slash_entries(&entries, "/qui");
        assert_eq!(quit.len(), 1);
        assert_eq!(quit[0].command_str(), "/quit");
        assert!(
            filter_slash_entries(&entries, "/q")
                .iter()
                .all(|entry| entry.command_str() != "/q")
        );
        let custom = filter_slash_entries(&entries, "/cus");
        assert!(custom.iter().any(|entry| entry.command_str() == "/custom"));
        assert!(
            custom
                .iter()
                .all(|entry| entry.command_str() != "/custom-review")
        );
        assert!(
            filter_slash_entries(&entries, "/con")
                .iter()
                .any(|entry| entry.command_str() == "/connect")
        );
    }

    #[test]
    fn enter_accepts_selected_at_mention_without_submitting() {
        let workspace_files = vec![
            "crates/cli/src/file_mentions.rs".into(),
            "crates/cli/src/tui/app.rs".into(),
        ];
        let buffer = "check @crates/cli/src/t";
        let cursor_char_idx = buffer.chars().count();

        let (next_buffer, next_cursor_char_idx) =
            apply_selected_at_completion(&workspace_files, buffer, cursor_char_idx, 0, true)
                .expect("active mention should be selectable");

        assert_eq!(next_buffer, "check @crates/cli/src/tui/app.rs ");
        assert_eq!(next_cursor_char_idx, next_buffer.chars().count());
    }

    #[test]
    fn backspace_deletes_completed_at_mention_and_space() {
        let buffer = "check @crates/cli/src/tui/app.rs ";
        let cursor_char_idx = buffer.chars().count();

        let (next_buffer, next_cursor_char_idx) =
            delete_completed_at_mention(buffer, cursor_char_idx)
                .expect("completed mention should delete as one token");

        assert_eq!(next_buffer, "check ");
        assert_eq!(next_cursor_char_idx, "check ".chars().count());
    }

    #[test]
    fn mention_range_includes_inserted_trailing_space() {
        let buffer = "check @crates/cli/src/tui/app.rs ";
        let cursor_char_idx = buffer.chars().count();

        assert_eq!(
            completed_at_mention_range_before_cursor(buffer, cursor_char_idx),
            Some((6, buffer.chars().count()))
        );
    }

    #[test]
    fn composer_line_styles_completed_mentions() {
        let line = composer_line("see @README.md ", 15);
        let mention_span = line
            .spans
            .iter()
            .find(|span| span.content.contains("@README.md"))
            .expect("mention span should exist");

        assert_eq!(mention_span.style.bg, Some(super::theme::MENTION_BG));
    }

    #[test]
    fn atomic_paste_preserves_chat_newlines_and_cursor_position() {
        let mut state = state();
        state.input_buffer = "αβγ".into();
        state.cursor_char_idx = 2;

        dispatch_paste(&mut state, "one\r\ntwo");

        assert_eq!(state.input_buffer, "αβone\ntwoγ");
        assert_eq!(state.cursor_char_idx, "αβone\ntwo".chars().count());
    }

    #[test]
    fn newline_shortcuts_insert_without_submitting() {
        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::CONTROL),
        ] {
            let mut state = state();
            state.input_buffer = "αdraft".into();
            state.cursor_char_idx = 1;

            assert!(handle_newline_key(&mut state, key));
            assert_eq!(state.input_buffer, "α\ndraft");
            assert_eq!(state.cursor_char_idx, 2);
        }
    }

    #[test]
    fn ordinary_enter_remains_submission_key() {
        let mut state = state();
        state.input_buffer = "draft".into();
        state.cursor_char_idx = state.input_buffer.chars().count();

        assert!(!handle_newline_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ));
        assert_eq!(state.input_buffer, "draft");
        assert_eq!(state.cursor_char_idx, 5);
    }

    #[test]
    fn ctrl_x_q_leader_requests_exit() {
        let mut state = state();
        state.leader_pending = true;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        dispatch_leader_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE),
            &tx,
        );

        assert!(!state.leader_pending);
        assert!(state.should_exit);
        assert!(matches!(rx.try_recv(), Ok(TuiCmd::Exit)));
    }

    #[test]
    fn atomic_paste_is_insert_only_and_does_not_submit_chat_input() {
        let mut state = state();
        dispatch_paste(&mut state, "first\nsecond");

        assert_eq!(state.input_buffer, "first\nsecond");
        assert!(!state.should_exit);
    }

    #[test]
    fn multiline_at_completion_remains_available_after_a_newline() {
        let workspace_files = vec!["src/main.rs".into(), "src/lib.rs".into()];
        let buffer = "first line\nopen @src/ma";
        let cursor = buffer.chars().count();

        assert_eq!(
            crate::tui::composer::at_completion_matches(&workspace_files, buffer, cursor),
            vec!["src/main.rs"]
        );
    }

    #[test]
    fn dollar_completion_dispatch_preserves_inline_text_and_whitespace() {
        let commands = vec![
            "manual-only".into(),
            "research-alpha".into(),
            "research-beta".into(),
        ];
        let line = "before $research after";
        let cursor = line.find("$research").unwrap() + "$research".len();
        let matches =
            matching_skill_reference_commands(line, cursor_byte_index(line, cursor), &commands);
        assert_eq!(matches, ["$research-alpha", "$research-beta"]);

        let (selected, selected_cursor) = apply_skill_reference_completion_with_space(
            line,
            cursor_byte_index(line, cursor),
            &matches[0],
        )
        .expect("active dollar token should be selectable");
        assert_eq!(selected, "before $research-alpha after");
        assert_eq!(selected_cursor, "before $research-alpha".chars().count());

        let end_line = "before $research";
        let end_cursor = end_line.chars().count();
        let end_matches = matching_skill_reference_commands(
            end_line,
            cursor_byte_index(end_line, end_cursor),
            &commands,
        );
        let (selected, selected_cursor) = apply_skill_reference_completion_with_space(
            end_line,
            cursor_byte_index(end_line, end_cursor),
            &end_matches[0],
        )
        .expect("line-end dollar token should be selectable");
        assert_eq!(selected, "before $research-alpha ");
        assert_eq!(selected_cursor, selected.chars().count());

        let escaped = r"before \$research";
        assert!(
            matching_skill_reference_commands(
                escaped,
                cursor_byte_index(escaped, escaped.chars().count()),
                &commands,
            )
            .is_empty()
        );
    }

    #[test]
    fn atomic_paste_sanitizes_single_line_api_key_input_without_submit() {
        let mut state = state();
        state.open_api_key_modal(nca_common::config::ProviderKind::OpenAi, false, false);

        dispatch_paste(&mut state, "secret\r\ncontinued");

        assert_eq!(state.api_key_input(), "secret continued");
        assert!(state.api_key_modal_open());
    }

    #[test]
    fn escape_only_cancels_active_turn_states() {
        let mut state = TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
        );
        assert!(!escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::Thinking);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::Streaming);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::ToolRunning);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::ApprovalPending);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::Idle);
        state.active_question = Some(nca_common::event::InteractiveQuestionPayload {
            question_id: "q".into(),
            call_id: "c".into(),
            prompt: "Pick".into(),
            options: vec![],
            allow_custom: true,
            suggested_answer: "a".into(),
        });
        assert!(
            escape_cancels_active_turn(&state),
            "stuck question while idle must still be dismissable with Esc"
        );
    }

    #[test]
    fn provider_error_hides_cancel_hint_and_allows_next_input() {
        let mut state = state();
        state.set_busy_state(BusyState::Thinking);
        state.apply_event(&AgentEvent::Error {
            message: "API request failed: provider unavailable".into(),
        });
        state.set_busy(false);

        assert_eq!(state.current_busy_state, BusyState::Error);
        assert!(!state.busy);
        assert!(!escape_cancels_active_turn(&state));

        state.apply_event(&AgentEvent::MessageReceived {
            role: "user".into(),
            content: "try again".into(),
        });
        assert_eq!(state.current_busy_state, BusyState::Thinking);
        assert!(escape_cancels_active_turn(&state));
    }

    #[test]
    fn dismiss_interactive_prompts_clears_question_and_approval() {
        let mut state = TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
        );
        state.active_question = Some(nca_common::event::InteractiveQuestionPayload {
            question_id: "q".into(),
            call_id: "c".into(),
            prompt: "Pick".into(),
            options: vec![],
            allow_custom: true,
            suggested_answer: "a".into(),
        });
        state.open_question_modal();
        state.dismiss_interactive_prompts();
        assert!(state.active_question.is_none());
        assert!(!state.question_modal_open());
    }
}
