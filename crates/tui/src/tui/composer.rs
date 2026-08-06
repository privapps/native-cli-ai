//! Composer input rendering, slash-command panel, `@`-mention completion, and
//! the categorised command palette.
//!
//! Extracted from `tui/app.rs` in Phase 2.2. Everything here operates on raw
//! buffer strings + character/byte offsets; no ratatui `Frame` / `Terminal`
//! knowledge, which keeps these helpers trivially unit-testable.

use crate::file_mentions;
use crate::slash_commands::{CommandCategory, command_surface_entries, visible_commands};
use crate::tui::app::TuiCmd;
use crate::tui::theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthChar;

pub const SLASH_PANEL_MAX_ROWS: usize = 8;
pub const COMPOSER_MAX_DRAFT_ROWS: usize = 8;

/// The display and insertion contract for a paste payload.
pub fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn sanitize_single_line_paste(text: &str) -> String {
    normalize_paste(text).replace('\n', " ")
}

/// Insert text at a character-index cursor, returning the new buffer and
/// character-index cursor. Newline normalization is intentionally centralized
/// here so paste and any future programmatic insertion share the same rules.
pub fn insert_text_at_cursor(buffer: &str, cursor_char_idx: usize, text: &str) -> (String, usize) {
    let cursor_char_idx = cursor_char_idx.min(buffer.chars().count());
    let cursor_byte = cursor_byte_index(buffer, cursor_char_idx);
    let inserted = normalize_paste(text);
    let mut result = String::with_capacity(buffer.len() + inserted.len());
    result.push_str(&buffer[..cursor_byte]);
    result.push_str(&inserted);
    result.push_str(&buffer[cursor_byte..]);
    (result, cursor_char_idx + inserted.chars().count())
}

/// Return the character-index line and column for a cursor in a newline-
/// delimited draft.
pub fn cursor_line_column(buffer: &str, cursor_char_idx: usize) -> (usize, usize) {
    let cursor_char_idx = cursor_char_idx.min(buffer.chars().count());
    let mut line = 0;
    let mut column = 0;
    for ch in buffer.chars().take(cursor_char_idx) {
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Move vertically while preserving the current logical-line column.
///
/// This compatibility wrapper retains the original newline-based behavior for
/// callers that do not have terminal width available. Full-screen input should
/// use [`move_cursor_vertical_with_width`] so wrapped rows participate in
/// movement.
pub fn move_cursor_vertical(buffer: &str, cursor_char_idx: usize, down: bool) -> usize {
    let (line, column) = cursor_line_column(buffer, cursor_char_idx);
    let lines: Vec<&str> = buffer.split('\n').collect();
    let target_line = if down {
        (line + 1).min(lines.len().saturating_sub(1))
    } else {
        line.saturating_sub(1)
    };
    let target_column = column.min(lines[target_line].chars().count());
    lines
        .iter()
        .take(target_line)
        .map(|line| line.chars().count() + 1)
        .sum::<usize>()
        + target_column
}

/// Move vertically through display rows while preserving the current display
/// column as far as the target row allows. The cursor stays at the nearest
/// edge at the first/last row.
pub fn move_cursor_vertical_with_width(
    buffer: &str,
    cursor_char_idx: usize,
    down: bool,
    max_width: usize,
) -> usize {
    let content_width = max_width.saturating_sub(2).max(1);
    let rows = visual_rows(buffer, content_width);
    let cursor_char_idx = cursor_char_idx.min(buffer.chars().count());
    let current_row = visual_row_for_cursor(&rows, cursor_char_idx);
    let target_row = if down {
        (current_row + 1).min(rows.len().saturating_sub(1))
    } else {
        current_row.saturating_sub(1)
    };
    if target_row == current_row {
        return cursor_char_idx;
    }

    let current_col = cursor_display_column(&rows[current_row], cursor_char_idx);
    cursor_at_display_column(&rows[target_row], current_col)
}

pub fn move_cursor_home(buffer: &str, cursor_char_idx: usize) -> usize {
    let (_, column) = cursor_line_column(buffer, cursor_char_idx);
    cursor_char_idx
        .min(buffer.chars().count())
        .saturating_sub(column)
}

pub fn move_cursor_end(buffer: &str, cursor_char_idx: usize) -> usize {
    let (line, column) = cursor_line_column(buffer, cursor_char_idx);
    let line_length = buffer
        .split('\n')
        .nth(line)
        .unwrap_or_default()
        .chars()
        .count();
    cursor_char_idx.min(buffer.chars().count()) + line_length.saturating_sub(column)
}

/// Slash and shell prefixes are commands only for a single-line draft.
pub fn is_single_line_command(buffer: &str) -> bool {
    if buffer.contains('\n') {
        return false;
    }
    let trimmed = buffer.trim_start();
    trimmed.starts_with('/') || trimmed.starts_with('!')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerRenderModel {
    pub rows: Vec<Line<'static>>,
    pub draft_height: usize,
    pub total_rows: usize,
    pub first_row: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,
}

/// Text used for `/command` detection (ignores leading spaces in the composer).
pub fn slash_command_buffer(buffer: &str) -> &str {
    buffer.trim_start()
}

pub fn slash_panel_visible(buffer: &str) -> bool {
    if buffer.contains('\n') {
        return false;
    }
    let s = slash_command_buffer(buffer);
    s.starts_with('/') && !s.chars().any(char::is_whitespace)
}

pub fn cursor_byte_index(line: &str, cursor_char_idx: usize) -> usize {
    line.char_indices()
        .nth(cursor_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

pub fn at_panel_height(n: usize) -> u16 {
    if n == 0 {
        return 0;
    }
    (n.min(SLASH_PANEL_MAX_ROWS) as u16).saturating_add(2)
}

pub fn at_completion_active(buffer: &str, cursor_char_idx: usize) -> bool {
    if slash_panel_visible(buffer) {
        return false;
    }
    let b = cursor_byte_index(buffer, cursor_char_idx);
    file_mentions::at_token_before_cursor(buffer, b).is_some()
}

pub fn at_completion_matches(
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
) -> Vec<String> {
    if !at_completion_active(buffer, cursor_char_idx) {
        return Vec::new();
    }
    let b = cursor_byte_index(buffer, cursor_char_idx);
    let Some((_, prefix)) = file_mentions::at_token_before_cursor(buffer, b) else {
        return Vec::new();
    };
    file_mentions::filter_paths_prefix(workspace_files, &prefix)
}

pub fn composer_chrome_height(
    slash_entries: &[SlashEntry],
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
) -> u16 {
    let slash_filtered = filter_slash_entries(slash_entries, buffer);
    let at_matches = at_completion_matches(workspace_files, buffer, cursor_char_idx);
    let slash_h = if slash_panel_visible(buffer) {
        slash_panel_height(slash_filtered.len())
    } else {
        0
    };
    let at_h = if !at_matches.is_empty() {
        at_panel_height(at_matches.len())
    } else {
        0
    };
    slash_h.max(at_h)
}

/// Replace `@prefix` before cursor with `@choice` (relative path).
pub fn apply_at_completion(buffer: &str, cursor_char_idx: usize, choice: &str) -> (String, usize) {
    let b = cursor_byte_index(buffer, cursor_char_idx);
    let Some((at_byte, _prefix)) = file_mentions::at_token_before_cursor(buffer, b) else {
        return (buffer.to_string(), cursor_char_idx);
    };
    let before = &buffer[..at_byte.saturating_add(1)];
    let after = &buffer[b..];
    let new_buf = format!("{before}{choice}{after}");
    let new_byte = at_byte + 1 + choice.len();
    let new_char = new_buf[..new_byte.min(new_buf.len())].chars().count();
    (new_buf, new_char)
}

pub fn apply_selected_at_completion(
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
    at_menu_index: usize,
    append_space: bool,
) -> Option<(String, usize)> {
    let at_matches = at_completion_matches(workspace_files, buffer, cursor_char_idx);
    if at_matches.is_empty() || !at_completion_active(buffer, cursor_char_idx) {
        return None;
    }

    let pick = at_menu_index.min(at_matches.len().saturating_sub(1));
    let choice = at_matches.get(pick)?;
    let (mut new_buf, mut new_cursor_char_idx) =
        apply_at_completion(buffer, cursor_char_idx, choice);

    if append_space {
        let insert_at = cursor_byte_index(&new_buf, new_cursor_char_idx);
        new_buf.insert(insert_at, ' ');
        new_cursor_char_idx += 1;
    }

    Some((new_buf, new_cursor_char_idx))
}

pub fn at_mention_char_ranges(buffer: &str) -> Vec<(usize, usize)> {
    file_mentions::parse_at_mentions(buffer)
        .into_iter()
        .map(|(start, end, _)| {
            let start_char = buffer[..start].chars().count();
            let end_char = buffer[..end].chars().count();
            (start_char, end_char)
        })
        .collect()
}

pub fn completed_at_mention_range_before_cursor(
    buffer: &str,
    cursor_char_idx: usize,
) -> Option<(usize, usize)> {
    let chars: Vec<char> = buffer.chars().collect();
    for (start_char, end_char) in at_mention_char_ranges(buffer) {
        if end_char == cursor_char_idx {
            return Some((start_char, end_char));
        }
        if end_char < chars.len()
            && end_char + 1 == cursor_char_idx
            && chars.get(end_char) == Some(&' ')
        {
            return Some((start_char, end_char + 1));
        }
    }
    None
}

pub fn remove_char_range(buffer: &str, start_char_idx: usize, end_char_idx: usize) -> String {
    let mut chars: Vec<char> = buffer.chars().collect();
    chars.drain(start_char_idx..end_char_idx);
    chars.into_iter().collect()
}

pub fn delete_completed_at_mention(
    buffer: &str,
    cursor_char_idx: usize,
) -> Option<(String, usize)> {
    let (start_char, end_char) = completed_at_mention_range_before_cursor(buffer, cursor_char_idx)?;
    Some((remove_char_range(buffer, start_char, end_char), start_char))
}

fn push_styled_run(
    spans: &mut Vec<Span<'static>>,
    text: &mut String,
    current_style: &mut Option<Style>,
    style: Style,
    ch: char,
) {
    if current_style.as_ref() != Some(&style) && !text.is_empty() {
        spans.push(Span::styled(
            std::mem::take(text),
            current_style.unwrap_or_default(),
        ));
    }
    *current_style = Some(style);
    text.push(ch);
}

pub fn composer_line(buffer: &str, cursor_char_idx: usize) -> Line<'static> {
    composer_line_at(buffer, buffer, 0, cursor_char_idx, true)
}

fn composer_line_at(
    full_buffer: &str,
    line: &str,
    line_start_char_idx: usize,
    cursor_char_idx: usize,
    first_line: bool,
) -> Line<'static> {
    let prompt = Span::styled("❯ ", Style::default().fg(theme::USER).bold());
    let chars: Vec<char> = line.chars().collect();
    let mention_ranges = at_mention_char_ranges(full_buffer);
    let cursor_char_idx = cursor_char_idx.min(line_start_char_idx + chars.len());
    let mut spans = if first_line {
        vec![prompt]
    } else {
        vec![Span::raw("  ")]
    };
    let mut run = String::new();
    let mut run_style: Option<Style> = None;

    for idx in 0..=chars.len() {
        let global_idx = line_start_char_idx + idx;
        if global_idx == cursor_char_idx {
            let cursor_char = chars.get(idx).copied().unwrap_or(' ');
            let in_mention = idx < chars.len()
                && mention_ranges
                    .iter()
                    .any(|(start, end)| *start <= global_idx && global_idx < *end);
            let cursor_style = if in_mention {
                Style::default()
                    .bg(theme::USER)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(theme::MUTED)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            };
            push_styled_run(
                &mut spans,
                &mut run,
                &mut run_style,
                cursor_style,
                cursor_char,
            );
            if idx == chars.len() {
                break;
            }
            continue;
        }

        let Some(ch) = chars.get(idx).copied() else {
            break;
        };
        let in_mention = mention_ranges
            .iter()
            .any(|(start, end)| *start <= global_idx && global_idx < *end);
        let style = if in_mention {
            Style::default()
                .fg(theme::TEXT)
                .bg(theme::MENTION_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };
        push_styled_run(&mut spans, &mut run, &mut run_style, style, ch);
    }

    if !run.is_empty() {
        spans.push(Span::styled(run, run_style.unwrap_or_default()));
    }

    Line::from(spans)
}

/// Build the visible draft rows and cursor viewport for the full-screen TUI.
pub fn composer_render_model(
    buffer: &str,
    cursor_char_idx: usize,
    max_visible_rows: usize,
) -> ComposerRenderModel {
    composer_render_model_with_width(buffer, cursor_char_idx, max_visible_rows, usize::MAX)
}

/// Build the visible draft rows using terminal display-cell width.
///
/// `max_width` is the full composer width, including the two-cell prompt or
/// continuation prefix. A zero width uses the smallest practical content
/// width so a wide character is never dropped.
pub fn composer_render_model_with_width(
    buffer: &str,
    cursor_char_idx: usize,
    max_visible_rows: usize,
    max_width: usize,
) -> ComposerRenderModel {
    let content_width = max_width.saturating_sub(2).max(1);
    let visual_rows = visual_rows(buffer, content_width);
    let total_rows = visual_rows.len().max(1);
    let cursor_char_idx = cursor_char_idx.min(buffer.chars().count());
    let cursor_row = visual_row_for_cursor(&visual_rows, cursor_char_idx);
    let cursor_col = visual_rows
        .get(cursor_row)
        .map(|row| 2 + cursor_display_column(row, cursor_char_idx))
        .unwrap_or(2);
    let draft_height = total_rows.min(max_visible_rows.max(1));
    let first_row = cursor_row
        .saturating_sub(draft_height.saturating_sub(1))
        .min(total_rows.saturating_sub(draft_height));
    let rows = visual_rows
        .iter()
        .enumerate()
        .skip(first_row)
        .take(draft_height)
        .map(|(row, visual)| {
            composer_line_at(
                buffer,
                visual.text,
                visual.start_char,
                cursor_char_idx,
                row == first_row,
            )
        })
        .collect();

    ComposerRenderModel {
        rows,
        draft_height,
        total_rows,
        first_row,
        cursor_row,
        cursor_col,
        cursor_visible: cursor_row >= first_row && cursor_row < first_row + draft_height,
    }
}

#[derive(Debug, Clone)]
struct VisualRow<'a> {
    text: &'a str,
    start_char: usize,
    end_char: usize,
    logical_end_char: usize,
}

fn visual_row_for_cursor(rows: &[VisualRow<'_>], cursor_char_idx: usize) -> usize {
    rows.iter()
        .position(|row| {
            cursor_char_idx >= row.start_char
                && cursor_char_idx <= row.logical_end_char
                && (cursor_char_idx < row.end_char || row.end_char == row.logical_end_char)
        })
        .unwrap_or_else(|| rows.len().saturating_sub(1))
}

fn cursor_display_column(row: &VisualRow<'_>, cursor_char_idx: usize) -> usize {
    row.text
        .chars()
        .take(cursor_char_idx.saturating_sub(row.start_char))
        .map(|ch| ch.width().unwrap_or(0))
        .sum()
}

fn cursor_at_display_column(row: &VisualRow<'_>, desired_col: usize) -> usize {
    let mut best_idx = row.start_char;
    let mut best_distance = desired_col;
    let mut display_col: usize = 0;

    for (offset, ch) in row.text.chars().enumerate() {
        display_col = display_col.saturating_add(ch.width().unwrap_or(0));
        let distance = display_col.abs_diff(desired_col);
        if distance < best_distance {
            best_distance = distance;
            best_idx = row.start_char + offset + 1;
        }
    }

    best_idx
}

fn visual_rows(buffer: &str, content_width: usize) -> Vec<VisualRow<'_>> {
    let mut rows = Vec::new();
    let mut logical_start = 0;

    for line in buffer.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let logical_end = logical_start + chars.len();
        if chars.is_empty() {
            rows.push(VisualRow {
                text: line,
                start_char: logical_start,
                end_char: logical_end,
                logical_end_char: logical_end,
            });
        } else {
            let mut chunk_start = 0;
            while chunk_start < chars.len() {
                let mut chunk_end = chunk_start;
                let mut width: usize = 0;
                while chunk_end < chars.len() {
                    let char_width = chars[chunk_end].width().unwrap_or(0);
                    if chunk_end > chunk_start && width.saturating_add(char_width) > content_width {
                        break;
                    }
                    width = width.saturating_add(char_width);
                    chunk_end += 1;
                }
                if chunk_end == chunk_start {
                    chunk_end += 1;
                }
                let start_byte = chars[..chunk_start].iter().map(|ch| ch.len_utf8()).sum();
                let end_byte = chars[..chunk_end].iter().map(|ch| ch.len_utf8()).sum();
                rows.push(VisualRow {
                    text: &line[start_byte..end_byte],
                    start_char: logical_start + chunk_start,
                    end_char: logical_start + chunk_end,
                    logical_end_char: logical_end,
                });
                chunk_start = chunk_end;
            }
        }
        logical_start = logical_end + 1;
    }

    rows
}

/// Height of the bordered composer, including its hint and optional auxiliary
/// rows such as staged-image status.
pub fn composer_input_height(buffer: &str, auxiliary_rows: usize) -> u16 {
    composer_input_height_with_width(buffer, auxiliary_rows, usize::MAX)
}

/// Height of the bordered composer using terminal display-cell rows.
pub fn composer_input_height_with_width(
    buffer: &str,
    auxiliary_rows: usize,
    max_width: usize,
) -> u16 {
    let content_width = max_width.saturating_sub(2).max(1);
    let draft_rows = visual_rows(buffer, content_width).len().max(1);
    (draft_rows.min(COMPOSER_MAX_DRAFT_ROWS) + auxiliary_rows + 3) as u16
}

// ---------------------------------------------------------------------------
// Slash-command entries
// ---------------------------------------------------------------------------

/// Entry for the slash panel: one canonical built-in command spelling or
/// one of its supported aliases.
#[derive(Clone, Copy)]
pub enum SlashEntry {
    Command(&'static str),
}

impl SlashEntry {
    pub fn command_str(&self) -> String {
        match self {
            SlashEntry::Command(spelling) => spelling.to_string(),
        }
    }

    pub fn display_text(&self) -> String {
        self.command_str()
    }
}

/// Load the built-in slash-command surface.
///
/// Skill commands intentionally do not belong here: `$<command>` completion
/// and `/skills` are the dedicated skill surfaces. The arguments remain part
/// of this seam so callers can continue to pass the runtime discovery context
/// without creating a second command-loading path.
pub fn load_slash_entries(_workspace_root: &Path, _skill_dirs: &[PathBuf]) -> Vec<SlashEntry> {
    let mut entries: Vec<SlashEntry> = command_surface_entries()
        .map(|(_, spelling)| SlashEntry::Command(spelling))
        .collect();

    entries.sort_by_key(|entry| entry.command_str().to_ascii_lowercase());
    entries.dedup_by(|left, right| {
        left.command_str()
            .eq_ignore_ascii_case(&right.command_str())
    });
    entries
}

/// Filter built-in slash entries by buffer prefix.
pub fn filter_slash_entries<'a>(entries: &'a [SlashEntry], buffer: &str) -> Vec<&'a SlashEntry> {
    if !slash_panel_visible(buffer) {
        return Vec::new();
    }
    let s = slash_command_buffer(buffer);
    let needle = s.trim_start_matches('/').to_lowercase();
    entries
        .iter()
        .filter(|e| {
            e.command_str()
                .trim_start_matches('/')
                .to_lowercase()
                .starts_with(&needle)
        })
        .collect()
}

pub fn slash_panel_height(filtered_len: usize) -> u16 {
    if filtered_len == 0 {
        return 0;
    }
    let rows = filtered_len.min(SLASH_PANEL_MAX_ROWS);
    let footer = if filtered_len > SLASH_PANEL_MAX_ROWS {
        1
    } else {
        0
    };
    (rows as u16)
        .saturating_add(footer)
        .saturating_add(2)
        .min(14)
}

// ---------------------------------------------------------------------------
// Branch picker
// ---------------------------------------------------------------------------

pub fn branch_filter_text(query: &str) -> &str {
    query.trim().strip_prefix('/').unwrap_or(query.trim())
}

pub fn filtered_branch_indices(branches: &[String], query: &str) -> Vec<usize> {
    let filter = branch_filter_text(query).to_ascii_lowercase();
    if filter.is_empty() {
        return (0..branches.len()).collect();
    }
    branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| branch.to_ascii_lowercase().contains(&filter))
        .map(|(idx, _)| idx)
        .collect()
}

pub fn branch_picker_enter_command(
    branches: &[String],
    query: &str,
    selected_filtered_idx: usize,
) -> Option<TuiCmd> {
    let raw_query = query.trim();
    let branch_name = branch_filter_text(raw_query).trim();
    let filtered = filtered_branch_indices(branches, raw_query);

    if raw_query.starts_with('/') {
        return (!branch_name.is_empty()).then(|| TuiCmd::CreateBranch(branch_name.to_string()));
    }

    if !branch_name.is_empty()
        && let Some((idx, _)) = branches
            .iter()
            .enumerate()
            .find(|(_, branch)| branch.eq_ignore_ascii_case(branch_name))
    {
        return Some(TuiCmd::SwitchBranch(branches[idx].clone()));
    }

    filtered
        .get(selected_filtered_idx)
        .copied()
        .map(|idx| TuiCmd::SwitchBranch(branches[idx].clone()))
}

// ---------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------

/// A row in the categorised command palette.
#[derive(Clone)]
pub enum PaletteRow {
    Section(&'static str),
    Entry {
        command: &'static str,
        label: &'static str,
        shortcut: &'static str,
    },
}

static PALETTE_CATALOG: std::sync::LazyLock<Vec<PaletteRow>> = std::sync::LazyLock::new(|| {
    let mut rows = Vec::new();
    for category in CommandCategory::ALL {
        rows.push(PaletteRow::Section(category.label()));
        rows.extend(
            visible_commands()
                .filter(|spec| spec.category == category)
                .map(|spec| PaletteRow::Entry {
                    command: spec.name,
                    label: spec.description,
                    shortcut: spec.shortcut,
                }),
        );
    }
    rows
});

pub fn palette_command_for_label(label: &str) -> &'static str {
    visible_commands()
        .find(|spec| spec.description == label)
        .map(|spec| spec.name)
        .unwrap_or("/help")
}

pub fn filter_palette_rows(query: &str) -> Vec<&'static PaletteRow> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return PALETTE_CATALOG.iter().collect();
    }
    let mut result: Vec<&'static PaletteRow> = Vec::new();
    let mut pending_section: Option<&'static PaletteRow> = None;
    for row in PALETTE_CATALOG.iter() {
        match row {
            PaletteRow::Section(_) => {
                pending_section = Some(row);
            }
            PaletteRow::Entry {
                command,
                label,
                shortcut,
            } => {
                if label.to_ascii_lowercase().contains(&needle)
                    || shortcut.to_ascii_lowercase().contains(&needle)
                    || command.contains(&needle)
                {
                    if let Some(s) = pending_section.take() {
                        result.push(s);
                    }
                    result.push(row);
                }
            }
        }
    }
    result
}

pub fn palette_selectable_indices(rows: &[&PaletteRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, PaletteRow::Entry { .. }).then_some(i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash_commands::resolve_command;

    #[test]
    fn visible_palette_entries_map_to_registered_commands() {
        for row in PALETTE_CATALOG.iter() {
            if let PaletteRow::Entry { command, .. } = row {
                assert!(
                    resolve_command(command).is_some(),
                    "{command} is not registered"
                );
            }
        }
    }

    #[test]
    fn connect_provider_is_not_duplicated() {
        let count = PALETTE_CATALOG
            .iter()
            .filter(|row| {
                matches!(
                    row,
                    PaletteRow::Entry {
                        command: "/connect",
                        ..
                    }
                )
            })
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn paste_normalizes_line_endings_without_collapsing_paragraphs() {
        assert_eq!(
            normalize_paste("one\r\ntwo\rthree\n\nfour\n"),
            "one\ntwo\nthree\n\nfour\n"
        );
    }

    #[test]
    fn paste_inserts_at_unicode_cursor_and_advances_by_chars() {
        let (buffer, cursor) = insert_text_at_cursor("αβγ", 2, "\r\n你好");

        assert_eq!(buffer, "αβ\n你好γ");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn vertical_cursor_movement_clamps_to_destination_line() {
        assert_eq!(move_cursor_vertical("first\n二\nthird", 4, true), 7);
        assert_eq!(move_cursor_vertical("first\n二\nthird", 7, true), 9);
    }

    #[test]
    fn vertical_cursor_movement_follows_wrapped_display_rows() {
        let draft = "abcdef\nxy";
        // Full width 4 leaves two content cells, so the logical first line is
        // displayed as `ab`, `cd`, `ef`.
        assert_eq!(move_cursor_vertical_with_width(draft, 4, true, 4), 7);
        assert_eq!(move_cursor_vertical_with_width(draft, 2, true, 4), 4);
        assert_eq!(move_cursor_vertical_with_width(draft, 4, false, 4), 2);
    }

    #[test]
    fn home_and_end_move_within_the_current_line() {
        let buffer = "first\nsecond\nthird";
        assert_eq!(move_cursor_home(buffer, 9), 6);
        assert_eq!(move_cursor_end(buffer, 8), 12);
    }

    #[test]
    fn multiline_command_prefix_is_not_classified_as_a_command() {
        assert!(is_single_line_command(" /help"));
        assert!(is_single_line_command("!echo hi"));
        assert!(!is_single_line_command("/help\nmore"));
        assert!(!is_single_line_command("!echo\nmore"));
        assert!(!slash_panel_visible("\n/help"));
        assert!(!is_single_line_command("\n/help"));
    }

    #[test]
    fn multiline_at_completion_uses_the_token_before_the_cursor() {
        let files = vec!["src/main.rs".into(), "src/lib.rs".into()];
        let buffer = "intro\nsee @src/ma";
        let cursor = buffer.chars().count();

        assert!(at_completion_active(buffer, cursor));
        assert_eq!(
            at_completion_matches(&files, buffer, cursor),
            vec!["src/main.rs"]
        );
        assert!(!slash_panel_visible("/help\n@src/ma"));
    }

    #[test]
    fn render_model_caps_rows_and_keeps_cursor_visible() {
        let model = composer_render_model("one\ntwo\nthree\nfour", 14, 2);

        assert_eq!(model.total_rows, 4);
        assert_eq!(model.draft_height, 2);
        assert_eq!(model.first_row, 2);
        assert_eq!(model.cursor_row, 3);
        assert!(model.cursor_visible);
        assert_eq!(model.rows.len(), 2);
        assert!(model.rows[0].spans[0].content.contains('❯'));
    }

    #[test]
    fn display_width_wraps_long_lines_without_losing_logical_text() {
        let model = composer_render_model_with_width("abcdefgh", 8, 8, 4);
        let rendered = model
            .rows
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(model.total_rows, 4);
        assert_eq!(rendered, "❯ ab   cd   ef   gh ");
    }

    #[test]
    fn display_width_accounts_for_wide_unicode_and_cursor_following() {
        let draft = "a界bc";
        let model = composer_render_model_with_width(draft, draft.chars().count(), 1, 4);

        assert_eq!(model.total_rows, 3);
        assert_eq!(model.first_row, 2);
        assert_eq!(model.cursor_row, 2);
        assert_eq!(model.cursor_col, 4);
        let rendered = model.rows[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "❯ bc ");
    }

    #[test]
    fn display_width_keeps_combining_marks_and_empty_drafts_usable() {
        let draft = "e\u{301}xyz";
        let model = composer_render_model_with_width(draft, 2, 8, 4);
        assert_eq!(model.total_rows, 2);
        assert_eq!(model.cursor_row, 0);
        assert_eq!(model.cursor_col, 3);

        let empty = composer_render_model_with_width("", 0, 8, 1);
        assert_eq!(empty.total_rows, 1);
        assert!(empty.cursor_visible);
    }

    #[test]
    fn composer_height_counts_visual_rows() {
        assert_eq!(composer_input_height_with_width("abcdefgh", 0, 4), 7);
        assert_eq!(composer_input_height_with_width("one\ntwo", 0, 8), 5);
    }

    #[test]
    fn generic_slash_surface_contains_builtins_and_aliases_but_no_skills() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".agents/skills/review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Review Changes\ncommand: agents-review\ndescription: Inspect a diff\n---\nReview.\n",
        )
        .unwrap();

        let entries = load_slash_entries(dir.path(), &[PathBuf::from(".agents/skills")]);
        assert!(entries.iter().any(|entry| entry.command_str() == "/exit"));
        assert!(entries.iter().any(|entry| entry.command_str() == "/quit"));
        assert!(!entries.iter().any(|entry| entry.command_str() == "/q"));
        assert!(entries.iter().any(|entry| entry.command_str() == "/skills"));
        assert!(
            !entries
                .iter()
                .any(|entry| entry.command_str() == "/agents-review")
        );
        assert!(
            entries
                .iter()
                .any(|entry| matches!(entry, SlashEntry::Command("/help")))
        );
    }

    #[test]
    fn generic_slash_surface_aliases_filter_and_insert_as_their_own_spelling() {
        let entries = load_slash_entries(Path::new("/tmp"), &[]);
        let quit = filter_slash_entries(&entries, "/qui");
        assert_eq!(quit.len(), 1);
        assert_eq!(quit[0].command_str(), "/quit");
        assert_eq!(quit[0].display_text(), "/quit");

        let q = filter_slash_entries(&entries, "/q");
        assert!(q.iter().all(|entry| entry.command_str() != "/q"));
    }

    #[test]
    fn configured_skill_is_not_in_generic_slash_surface() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("configured-skills/custom-review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Custom Review\ncommand: custom-review\ndescription: Inspect configured roots\n---\nReview.\n",
        )
        .unwrap();

        // The configured skill remains available through `$` completion; the
        // generic slash surface intentionally does not load it.
        let entries = load_slash_entries(dir.path(), &[PathBuf::from("configured-skills")]);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.command_str() == "/custom-review")
        );
    }
}
