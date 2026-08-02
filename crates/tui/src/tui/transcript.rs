//! Transcript rendering: converting session state into styled `ratatui` lines.
//!
//! Extracted from `tui/app.rs` in Phase 2.2. These helpers are the hot path
//! every TUI frame hits on dirty redraws; see `docs/research/baselines.md`
//! for `wrap_text` / `parse_md_line` numbers.

use crate::tui::busy_indicator;
use crate::tui::state::{ApprovalRequest, DisplayBlock, TranscriptCache, TuiSessionState};
use crate::tui::theme;
use nca_common::event::{BusyState, QuestionSelection};
use pulldown_cmark::{Alignment, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SynStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use unicode_width::UnicodeWidthStr;

/// Per flattened transcript line: click selects this answer for `question_id`
/// (same indices as `transcript_lines_and_hits`).
pub type LineAnswerHit = Option<(String, QuestionSelection)>;

#[inline]
pub fn push_transcript_line(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    line: Line<'static>,
    hit: LineAnswerHit,
) {
    lines.push(line);
    hits.push(hit);
}

/// Build scrollable transcript lines + optional mouse/click targets per line
/// straight from state (no caching). The draw path still uses this to keep the
/// closure simple; callers outside draw (mouse handlers, benchmarks) should go
/// through [`ensure_transcript_cache`] to avoid rebuilding.
pub fn transcript_lines_and_hits(
    state: &TuiSessionState,
    width: u16,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();

    for block in &state.blocks {
        match block {
            DisplayBlock::User(content) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![Span::styled(
                        " YOU ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::USER)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                for text_line in wrap_text(content, w) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(text_line, Style::default().fg(theme::TEXT))),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::Assistant(content) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![Span::styled(
                        " nca ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::ASSISTANT)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                for text_line in render_markdown_block(content, w) {
                    push_transcript_line(&mut lines, &mut hits, text_line, None);
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolRunning { name, input, .. } => {
                let summary = tool_running_summary(name, input);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(" ⚡ ", Style::default().fg(theme::TOOL)),
                        Span::styled(
                            format!("{name} "),
                            Style::default()
                                .fg(theme::TOOL)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("{summary} "), Style::default().fg(theme::TEXT)),
                        Span::styled("…", Style::default().fg(theme::MUTED)),
                    ]),
                    None,
                );
            }
            DisplayBlock::ApprovalPending(req) => {
                render_approval_block(&mut lines, &mut hits, req, w);
            }
            DisplayBlock::ApprovalResolved { tool, approved } => {
                let (label, style) = if *approved {
                    (
                        " approved ",
                        Style::default().fg(Color::Black).bg(theme::SUCCESS),
                    )
                } else {
                    (
                        " denied ",
                        Style::default().fg(Color::Black).bg(theme::ERROR),
                    )
                };
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(label, style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" {tool}"), Style::default().fg(theme::TEXT)),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolDone { name, ok, detail } => {
                let (icon, st) = if *ok {
                    ("✓", Style::default().fg(theme::SUCCESS))
                } else {
                    ("✗", Style::default().fg(theme::ERROR))
                };
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(format!(" {icon} "), st),
                        Span::styled(
                            name.to_string(),
                            Style::default()
                                .fg(theme::TOOL)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" — {}", truncate_chars(detail, 100)),
                            Style::default().fg(theme::MUTED),
                        ),
                    ]),
                    None,
                );
            }
            DisplayBlock::System(s) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(Span::styled(
                        format!(" ‣ {s}"),
                        Style::default().fg(theme::WARN),
                    )),
                    None,
                );
            }
            DisplayBlock::Question(q) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            " ? ",
                            Style::default().fg(Color::Black).bg(theme::WARN).bold(),
                        ),
                        Span::styled(
                            " question ",
                            Style::default()
                                .fg(theme::WARN)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                for text_line in wrap_text(&q.prompt, w) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(text_line, Style::default().fg(theme::TEXT))),
                        None,
                    );
                }
                // When the modal is open, skip inline options — the popup handles selection.
                if !state.question_modal_open() {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled(
                                format!("  [0] suggested: {} ", q.suggested_answer),
                                Style::default()
                                    .fg(theme::SUCCESS)
                                    .add_modifier(Modifier::UNDERLINED),
                            ),
                            Span::styled("(click)", Style::default().fg(theme::MUTED)),
                        ]),
                        Some((q.question_id.clone(), QuestionSelection::Suggested)),
                    );
                    for (i, o) in q.options.iter().enumerate() {
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(vec![
                                Span::styled(
                                    format!("  [{}] ({}) {} ", i + 1, o.id, o.label),
                                    Style::default()
                                        .fg(theme::TEXT)
                                        .add_modifier(Modifier::UNDERLINED),
                                ),
                                Span::styled("(click)", Style::default().fg(theme::MUTED)),
                            ]),
                            Some((
                                q.question_id.clone(),
                                QuestionSelection::Option {
                                    option_id: o.id.clone(),
                                },
                            )),
                        );
                    }
                    if q.allow_custom {
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(Span::styled(
                                "  [c] type your own answer below, then Enter",
                                Style::default().fg(theme::MUTED),
                            )),
                            None,
                        );
                    }
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            "  Tip: /auto-answer or Enter on empty = suggested · click an option above",
                            Style::default().fg(theme::MUTED),
                        )),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ErrorLine(s) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(Span::styled(
                        format!(" ✗ {s}"),
                        Style::default().fg(theme::ERROR),
                    )),
                    None,
                );
            }
        }
    }

    if let Some(stream) = &state.streaming_assistant
        && !stream.is_empty()
    {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    " nca ",
                    Style::default().fg(Color::Black).bg(theme::ASSISTANT),
                ),
                Span::styled(" streaming", Style::default().fg(theme::MUTED)),
            ]),
            None,
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        for text_line in render_markdown_block(stream, w) {
            push_transcript_line(&mut lines, &mut hits, text_line, None);
        }
    }

    if lines.is_empty() {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    "nca",
                    Style::default()
                        .fg(theme::ASSISTANT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — session ready", Style::default().fg(theme::MUTED)),
            ]),
            None,
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(Span::styled(
                "Tab  agent   Ctrl+V  image   Ctrl+Shift+C  copy   Ctrl+P  commands   !cmd  shell   @path  search   /  inline   wheel  scroll",
                Style::default().fg(theme::MUTED),
            )),
            None,
        );
    }

    (lines, hits)
}

/// Ensure `state.transcript_cache` is populated for the given width + current
/// `transcript_version`, rebuilding only when invalidated. Used on paths that
/// don't draw (mouse clicks, scrolls, benches) so we don't re-wrap the whole
/// transcript per event.
pub fn ensure_transcript_cache(state: &mut TuiSessionState, width: u16) -> &TranscriptCache {
    let needs_build = state
        .transcript_cache
        .as_ref()
        .map(|c| !c.is_valid(state.transcript_version, width))
        .unwrap_or(true);
    if needs_build {
        let (lines, hits) = transcript_lines_and_hits(state, width);
        state.transcript_cache = Some(TranscriptCache {
            built_for_version: state.transcript_version,
            built_for_width: width,
            lines,
            hits,
        });
    }
    state
        .transcript_cache
        .as_ref()
        .expect("cache was just ensured")
}

#[allow(dead_code)]
pub fn transcript_lines(state: &TuiSessionState, width: u16) -> Vec<Line<'static>> {
    transcript_lines_and_hits(state, width).0
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// One-line path/command summary for an in-flight tool call.
pub fn tool_running_summary(name: &str, input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return "…".into();
    }
    // Prefer the first non-empty line (one-liners from format_tool_input_for_display).
    let first = trimmed.lines().next().unwrap_or(trimmed).trim();
    if (name == "write_file" || name == "edit_file" || name == "read_file")
        && !first.starts_with('{')
    {
        return truncate_chars(first, 72);
    }
    truncate_chars(first, 72)
}

/// Live footer lines appended outside the transcript cache so the elapsed
/// timer keeps ticking on busy animation frames without rebuilding the cache.
pub fn live_activity_lines(state: &TuiSessionState) -> Vec<Line<'static>> {
    let secs = state.busy_state_since.elapsed().as_secs();
    let elapsed_ms = state.busy_state_since.elapsed().as_millis();
    let frame = busy_indicator::frame_for_state(state.current_busy_state, elapsed_ms);
    let color = busy_indicator::color_for_state(state.current_busy_state);

    let detail = match state.current_busy_state {
        BusyState::Thinking => format!("{frame} waiting for model · {secs}s"),
        BusyState::Streaming => {
            let n = state
                .streaming_assistant
                .as_ref()
                .map(|s| s.chars().count())
                .unwrap_or(0);
            format!("{frame} streaming · {n} chars · {secs}s")
        }
        BusyState::ToolRunning => {
            let summary = state.blocks.iter().rev().find_map(|b| match b {
                DisplayBlock::ToolRunning { name, input, .. } => {
                    Some(format!("{name} · {}", tool_running_summary(name, input)))
                }
                _ => None,
            });
            match summary {
                Some(s) => format!("{frame} {s} · {secs}s"),
                None => format!("{frame} running tool · {secs}s"),
            }
        }
        BusyState::ApprovalPending => format!("{frame} waiting for approval · {secs}s"),
        BusyState::Error | BusyState::Idle => return Vec::new(),
    };

    vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  {detail}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
    ]
}

pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width < 8 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    for paragraph in s.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line = word.to_string();
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(line);
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() && !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

pub fn wrap_preformatted_line(line: &str, width: usize) -> Vec<String> {
    if width < 4 || line.is_empty() {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for ch in line.chars() {
        if current_len >= width {
            out.push(current);
            current = String::new();
            current_len = 0;
        }
        current.push(ch);
        current_len += 1;
    }
    if out.is_empty() || !current.is_empty() {
        out.push(current);
    }
    out
}

pub fn push_wrapped_plain_lines(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    text: &str,
    width: usize,
    style: Style,
) {
    for source_line in text.lines() {
        let wrapped = wrap_preformatted_line(source_line, width);
        for line in wrapped {
            push_transcript_line(lines, hits, Line::from(Span::styled(line, style)), None);
        }
        if source_line.is_empty() {
            push_transcript_line(lines, hits, Line::default(), None);
        }
    }
}

pub fn render_approval_block(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    req: &ApprovalRequest,
    width: usize,
) {
    push_transcript_line(
        lines,
        hits,
        Line::from(vec![
            Span::styled(
                " approve ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", pretty_tool_label(&req.tool)),
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        None,
    );
    push_transcript_line(lines, hits, Line::default(), None);
    if !req.description.is_empty()
        && req.description != format!("Tool `{}` requires approval", req.tool)
    {
        for text_line in wrap_text(&req.description, width) {
            push_transcript_line(
                lines,
                hits,
                Line::from(Span::styled(text_line, Style::default().fg(theme::TEXT))),
                None,
            );
        }
        push_transcript_line(lines, hits, Line::default(), None);
    }

    let preview = pretty_approval_input(&req.tool, &req.input);
    for text_line in wrap_preformatted_line(&preview, width) {
        push_transcript_line(
            lines,
            hits,
            Line::from(Span::styled(
                format!("  {text_line}"),
                Style::default().fg(theme::TEXT),
            )),
            None,
        );
    }
    push_transcript_line(lines, hits, Line::default(), None);
    push_transcript_line(
        lines,
        hits,
        Line::from(vec![
            Span::styled(
                " y ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" approve  ", Style::default().fg(theme::MUTED)),
            Span::styled(
                " n ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" deny  ", Style::default().fg(theme::MUTED)),
            Span::styled(
                " Ctrl+U ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::ASSISTANT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" always allow", Style::default().fg(theme::MUTED)),
        ]),
        None,
    );
    push_transcript_line(lines, hits, Line::default(), None);
}

fn pretty_tool_label(tool: &str) -> String {
    match tool {
        "execute_bash" => "run command".into(),
        "delete_path" => "delete path".into(),
        other => other.replace('_', " "),
    }
}

fn pretty_approval_input(tool: &str, raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.trim().to_string();
    };
    if tool == "execute_bash"
        && let Some(cmd) = value.get("command").and_then(|v| v.as_str())
    {
        return format!("$ {cmd}");
    }
    if let Some(path) = value
        .get("path")
        .or_else(|| value.get("file_path"))
        .and_then(|v| v.as_str())
    {
        return path.to_string();
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string())
}

/// Parse user approval input (flexible: punctuation, synonyms, `/approve` style).
pub fn parse_approval_verdict(line: &str) -> Option<bool> {
    let mut s = line.trim().to_lowercase();
    while matches!(
        s.chars().last(),
        Some('.' | '!' | '?' | ',' | ';' | ':' | '"' | '\'')
    ) {
        s.pop();
    }
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Slash commands (handled before this in caller for passthrough; bare forms here too)
    match s {
        "/approve" | "/y" | "/yes" | "/ok" => return Some(true),
        "/deny" | "/n" | "/no" => return Some(false),
        _ => {}
    }
    let word = s.split_whitespace().next()?;
    match word {
        "y" | "yes" | "ok" | "okay" | "approve" | "approved" | "allow" | "1" | "true" => Some(true),
        "n" | "no" | "deny" | "denied" | "reject" | "rejected" | "decline" | "declined" | "0"
        | "false" => Some(false),
        _ => None,
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

fn syntect_to_ratatui(style: SynStyle) -> Style {
    Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ))
}

/// Render a markdown fragment to wrapped styled lines (headings, lists, code).
pub fn render_markdown_block(text: &str, width: usize) -> Vec<Line<'static>> {
    if has_unclosed_fenced_block(text) {
        return render_unclosed_fenced_fallback(text, width.max(1));
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, options);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current = Line::from(Vec::<Span<'static>>::new());
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let mut list_stack: Vec<ListState> = Vec::new();
    let mut quote_depth = 0usize;
    let mut style_stack: Vec<Style> = Vec::new();
    let mut current_style = Style::default().fg(theme::TEXT);
    let mut pending_space = false;
    let mut table: Option<TableBuilder> = None;

    let flush_line = |out: &mut Vec<Line<'static>>, current: &mut Line<'static>| {
        if !current.spans.is_empty() || !current.style.add_modifier.is_empty() {
            out.push(current.clone());
            *current = Line::from(Vec::<Span<'static>>::new());
        }
    };

    for event in parser {
        if table.is_some() {
            if let Some(table_lines) = handle_table_event(&mut table, event, width) {
                out.extend(table_lines);
                pending_space = false;
            }
            continue;
        }

        match event {
            MdEvent::Start(Tag::Table(alignments)) => {
                flush_line(&mut out, &mut current);
                table = Some(TableBuilder::new(alignments.to_vec()));
                pending_space = false;
            }
            MdEvent::Start(Tag::CodeBlock(kind)) => {
                flush_line(&mut out, &mut current);
                pending_space = false;
                in_code = true;
                code_buf.clear();
                code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
            }
            MdEvent::End(TagEnd::CodeBlock) => {
                in_code = false;
                let highlighted = highlight_code_block(&code_lang, &code_buf, width);
                out.extend(highlighted);
                code_buf.clear();
                pending_space = false;
            }
            MdEvent::Code(text) => {
                append_quote_prefix(&mut current, quote_depth, width);
                append_markdown_text(
                    &mut out,
                    &mut current,
                    &text,
                    width,
                    current_style.fg(theme::TOOL),
                    &mut pending_space,
                );
            }
            MdEvent::Start(Tag::Heading { .. }) => {
                flush_line(&mut out, &mut current);
                style_stack.push(current_style);
                current_style = current_style
                    .fg(theme::ASSISTANT)
                    .add_modifier(Modifier::BOLD);
                pending_space = false;
            }
            MdEvent::End(TagEnd::Heading(_)) => {
                flush_line(&mut out, &mut current);
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
                pending_space = false;
            }
            MdEvent::Start(Tag::List(start)) => list_stack.push(ListState {
                ordered: start.is_some(),
                next: start.unwrap_or(1),
            }),
            MdEvent::End(TagEnd::List(_)) => {
                flush_line(&mut out, &mut current);
                list_stack.pop();
                pending_space = false;
            }
            MdEvent::Start(Tag::Item) => {
                flush_line(&mut out, &mut current);
                pending_space = false;
                append_quote_prefix(&mut current, quote_depth, width);
                let (prefix, item_style) = if let Some(list) = list_stack.last_mut() {
                    let prefix = if list.ordered {
                        let prefix = format!("{}. ", list.next);
                        list.next = list.next.saturating_add(1);
                        prefix
                    } else {
                        "- ".to_string()
                    };
                    (
                        format!(
                            "{}{}",
                            "  ".repeat(list_stack.len().saturating_sub(1)),
                            prefix
                        ),
                        current_style.fg(theme::MUTED),
                    )
                } else {
                    ("- ".to_string(), current_style.fg(theme::MUTED))
                };
                let available = width.max(1).saturating_sub(current.width());
                current
                    .spans
                    .push(Span::styled(bounded_prefix(&prefix, available), item_style));
            }
            MdEvent::End(TagEnd::Item) => {
                flush_line(&mut out, &mut current);
                pending_space = false;
            }
            MdEvent::Start(Tag::Strong) => {
                style_stack.push(current_style);
                current_style = current_style.add_modifier(Modifier::BOLD);
            }
            MdEvent::End(TagEnd::Strong) => {
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
            }
            MdEvent::Start(Tag::Emphasis) => {
                style_stack.push(current_style);
                current_style = current_style.add_modifier(Modifier::ITALIC);
            }
            MdEvent::End(TagEnd::Emphasis) => {
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
            }
            MdEvent::Start(Tag::Strikethrough) => {
                style_stack.push(current_style);
                current_style = current_style.add_modifier(Modifier::CROSSED_OUT);
            }
            MdEvent::End(TagEnd::Strikethrough) => {
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
            }
            MdEvent::Start(Tag::Link { .. }) => {
                style_stack.push(current_style);
                current_style = current_style
                    .fg(theme::USER)
                    .add_modifier(Modifier::UNDERLINED);
            }
            MdEvent::End(TagEnd::Link) => {
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
            }
            MdEvent::Start(Tag::Image { .. }) => {
                style_stack.push(current_style);
                current_style = current_style
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC);
                append_markdown_text(
                    &mut out,
                    &mut current,
                    "[image: ",
                    width,
                    current_style,
                    &mut pending_space,
                );
            }
            MdEvent::End(TagEnd::Image) => {
                pending_space = false;
                append_markdown_text(
                    &mut out,
                    &mut current,
                    "]",
                    width,
                    current_style,
                    &mut pending_space,
                );
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
            }
            MdEvent::Start(Tag::BlockQuote(_)) => {
                flush_line(&mut out, &mut current);
                style_stack.push(current_style);
                current_style = current_style
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC);
                quote_depth = quote_depth.saturating_add(1);
                pending_space = false;
            }
            MdEvent::End(TagEnd::BlockQuote(_)) => {
                flush_line(&mut out, &mut current);
                quote_depth = quote_depth.saturating_sub(1);
                current_style = style_stack
                    .pop()
                    .unwrap_or_else(|| Style::default().fg(theme::TEXT));
                pending_space = false;
            }
            MdEvent::Rule => {
                flush_line(&mut out, &mut current);
                append_quote_prefix(&mut current, quote_depth, width);
                let rule_width = width.max(1).saturating_sub(current.width()).max(1);
                current.spans.push(Span::styled(
                    "─".repeat(rule_width),
                    Style::default().fg(theme::MUTED),
                ));
                flush_line(&mut out, &mut current);
                pending_space = false;
            }
            MdEvent::TaskListMarker(checked) => {
                let available = width.max(1).saturating_sub(current.width());
                current.spans.push(Span::styled(
                    bounded_prefix(if checked { "[x] " } else { "[ ] " }, available),
                    current_style.fg(theme::MUTED),
                ));
                pending_space = false;
            }
            MdEvent::Text(t) if in_code => code_buf.push_str(&t),
            MdEvent::Text(t) => {
                append_quote_prefix(&mut current, quote_depth, width);
                append_markdown_text(
                    &mut out,
                    &mut current,
                    &t,
                    width,
                    current_style,
                    &mut pending_space,
                );
            }
            MdEvent::SoftBreak => {
                flush_line(&mut out, &mut current);
                pending_space = false;
            }
            MdEvent::HardBreak => {
                flush_line(&mut out, &mut current);
                pending_space = false;
            }
            MdEvent::End(TagEnd::Paragraph) => {
                flush_line(&mut out, &mut current);
                pending_space = false;
            }
            MdEvent::Html(html) | MdEvent::InlineHtml(html) => {
                append_quote_prefix(&mut current, quote_depth, width);
                append_markdown_text(
                    &mut out,
                    &mut current,
                    &html,
                    width,
                    current_style.fg(theme::MUTED),
                    &mut pending_space,
                );
            }
            MdEvent::InlineMath(math) => append_markdown_text(
                &mut out,
                &mut current,
                &format!("$ {} $", math),
                width,
                current_style,
                &mut pending_space,
            ),
            MdEvent::DisplayMath(math) => {
                flush_line(&mut out, &mut current);
                append_markdown_text(
                    &mut out,
                    &mut current,
                    &format!("$$ {} $$", math),
                    width,
                    current_style,
                    &mut pending_space,
                );
                flush_line(&mut out, &mut current);
            }
            MdEvent::FootnoteReference(label) => append_markdown_text(
                &mut out,
                &mut current,
                &format!("[^{}]", label),
                width,
                current_style.fg(theme::MUTED),
                &mut pending_space,
            ),
            _ => {}
        }
    }
    if let Some(table) = table.take() {
        out.extend(table.render(width));
    }
    flush_line(&mut out, &mut current);
    if out.is_empty() {
        out.push(Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(theme::TEXT),
        )));
    }
    out
}

fn has_unclosed_fenced_block(text: &str) -> bool {
    let fence = char::from(96).to_string().repeat(3);
    text.lines()
        .filter(|line| line.trim_start().starts_with(&fence))
        .count()
        % 2
        == 1
}

#[derive(Clone, Copy)]
struct ListState {
    ordered: bool,
    next: u64,
}

struct TableBuilder {
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
    current_row: Option<TableRow>,
    current_cell: Option<TableCellBuilder>,
    in_header: bool,
    style_stack: Vec<Style>,
    current_style: Style,
    pending_space: bool,
}

struct TableRow {
    header: bool,
    cells: Vec<TableCell>,
}

struct TableCell {
    lines: Vec<Line<'static>>,
}

struct TableCellBuilder {
    lines: Vec<Line<'static>>,
    current: Line<'static>,
}

impl TableBuilder {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            rows: Vec::new(),
            current_row: None,
            current_cell: None,
            in_header: false,
            style_stack: Vec::new(),
            current_style: Style::default().fg(theme::TEXT),
            pending_space: false,
        }
    }

    fn finish_cell(&mut self) {
        let Some(mut cell) = self.current_cell.take() else {
            return;
        };
        if !cell.current.spans.is_empty() || cell.lines.is_empty() {
            cell.lines.push(cell.current);
        }
        if let Some(row) = self.current_row.as_mut() {
            row.cells.push(TableCell { lines: cell.lines });
        }
        self.pending_space = false;
    }

    fn finish_row(&mut self) {
        self.finish_cell();
        if let Some(row) = self.current_row.take() {
            self.rows.push(row);
        }
        self.pending_space = false;
    }

    fn flush_cell_line(&mut self) {
        let Some(cell) = self.current_cell.as_mut() else {
            return;
        };
        if !cell.current.spans.is_empty() || cell.lines.is_empty() {
            let current =
                std::mem::replace(&mut cell.current, Line::from(Vec::<Span<'static>>::new()));
            cell.lines.push(current);
        }
        self.pending_space = false;
    }

    fn append_text(&mut self, text: &str, style: Style) {
        let Some(cell) = self.current_cell.as_mut() else {
            return;
        };
        let mut saw_word = false;
        for word in text.split_whitespace() {
            if (self.pending_space || saw_word) && !cell.current.spans.is_empty() {
                cell.current
                    .spans
                    .push(Span::styled(" ".to_string(), style));
            }
            cell.current
                .spans
                .push(Span::styled(word.to_string(), style));
            self.pending_space = false;
            saw_word = true;
        }
        if saw_word {
            self.pending_space = text.chars().last().is_some_and(char::is_whitespace);
        }
    }

    fn render(mut self, width: usize) -> Vec<Line<'static>> {
        self.finish_row();
        if self.rows.is_empty() {
            return vec![Line::from(Span::styled(
                "(empty table)",
                Style::default().fg(theme::MUTED),
            ))];
        }

        let columns = self
            .rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0)
            .max(self.alignments.len());
        if columns == 0 {
            return vec![Line::from(Span::styled(
                "(empty table)",
                Style::default().fg(theme::MUTED),
            ))];
        }

        let width = width.max(1);
        let natural_widths = table_natural_widths(&self.rows, columns);
        let spaced_separator = " │ ";
        let tight_separator = "│";
        let spaced_total = natural_widths.iter().sum::<usize>()
            + spaced_separator.width() * columns.saturating_sub(1);
        let separator = if columns == 1 || spaced_total <= width {
            spaced_separator
        } else {
            tight_separator
        };
        let separator_width = separator.width();
        let available = width.saturating_sub(separator_width * columns.saturating_sub(1));
        if available < columns {
            return render_table_fallback(&self.rows, width);
        }
        let column_widths = table_column_widths(&natural_widths, available);
        if table_requires_fallback(&self.rows, &column_widths) {
            return render_table_fallback(&self.rows, width);
        }

        let mut out = Vec::new();
        for row in &self.rows {
            let cells: Vec<Vec<Line<'static>>> = (0..columns)
                .map(|column| {
                    row.cells
                        .get(column)
                        .map(|cell| wrap_table_cell(cell, column_widths[column]))
                        .unwrap_or_else(|| vec![Line::default()])
                })
                .collect();
            let row_height = cells.iter().map(Vec::len).max().unwrap_or(1);

            for line_index in 0..row_height {
                let mut line = Line::from(Vec::<Span<'static>>::new());
                for column in 0..columns {
                    if column > 0 {
                        line.spans.push(Span::styled(
                            separator.to_string(),
                            Style::default().fg(theme::MUTED),
                        ));
                    }
                    append_aligned_table_cell(
                        &mut line,
                        cells[column].get(line_index),
                        column_widths[column],
                        self.alignments
                            .get(column)
                            .copied()
                            .unwrap_or(Alignment::None),
                    );
                }
                out.push(line);
            }

            if row.header {
                let divider = if separator == spaced_separator {
                    "─┼─"
                } else {
                    "┼"
                };
                let mut line = Line::from(Vec::<Span<'static>>::new());
                for (column, column_width) in column_widths.iter().enumerate() {
                    if column > 0 {
                        line.spans.push(Span::styled(
                            divider.to_string(),
                            Style::default().fg(theme::MUTED),
                        ));
                    }
                    line.spans.push(Span::styled(
                        "─".repeat(*column_width),
                        Style::default().fg(theme::MUTED),
                    ));
                }
                out.push(line);
            }
        }
        out
    }
}

impl TableCellBuilder {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current: Line::from(Vec::<Span<'static>>::new()),
        }
    }
}

fn handle_table_event(
    table: &mut Option<TableBuilder>,
    event: MdEvent<'_>,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    if matches!(event, MdEvent::End(TagEnd::Table)) {
        return table.take().map(|table| table.render(width));
    }

    let builder = table.as_mut()?;
    match event {
        MdEvent::Start(Tag::TableHead) => {
            builder.in_header = true;
            builder.current_row = Some(TableRow {
                header: true,
                cells: Vec::new(),
            });
        }
        MdEvent::End(TagEnd::TableHead) => {
            builder.finish_row();
            builder.in_header = false;
        }
        MdEvent::Start(Tag::TableRow) => {
            builder.current_row = Some(TableRow {
                header: builder.in_header,
                cells: Vec::new(),
            });
        }
        MdEvent::End(TagEnd::TableRow) => builder.finish_row(),
        MdEvent::Start(Tag::TableCell) => {
            builder.finish_cell();
            builder.current_cell = Some(TableCellBuilder::new());
            builder.pending_space = false;
        }
        MdEvent::End(TagEnd::TableCell) => builder.finish_cell(),
        MdEvent::Start(Tag::Strong) => {
            builder.style_stack.push(builder.current_style);
            builder.current_style = builder.current_style.add_modifier(Modifier::BOLD);
        }
        MdEvent::End(TagEnd::Strong) => {
            builder.current_style = builder
                .style_stack
                .pop()
                .unwrap_or_else(|| Style::default().fg(theme::TEXT));
        }
        MdEvent::Start(Tag::Emphasis) => {
            builder.style_stack.push(builder.current_style);
            builder.current_style = builder.current_style.add_modifier(Modifier::ITALIC);
        }
        MdEvent::End(TagEnd::Emphasis) => {
            builder.current_style = builder
                .style_stack
                .pop()
                .unwrap_or_else(|| Style::default().fg(theme::TEXT));
        }
        MdEvent::Start(Tag::Strikethrough) => {
            builder.style_stack.push(builder.current_style);
            builder.current_style = builder.current_style.add_modifier(Modifier::CROSSED_OUT);
        }
        MdEvent::End(TagEnd::Strikethrough) => {
            builder.current_style = builder
                .style_stack
                .pop()
                .unwrap_or_else(|| Style::default().fg(theme::TEXT));
        }
        MdEvent::Start(Tag::Link { .. }) => {
            builder.style_stack.push(builder.current_style);
            builder.current_style = builder
                .current_style
                .fg(theme::USER)
                .add_modifier(Modifier::UNDERLINED);
        }
        MdEvent::End(TagEnd::Link) => {
            builder.current_style = builder
                .style_stack
                .pop()
                .unwrap_or_else(|| Style::default().fg(theme::TEXT));
        }
        MdEvent::Start(Tag::Image { .. }) => {
            builder.style_stack.push(builder.current_style);
            builder.current_style = builder
                .current_style
                .fg(theme::MUTED)
                .add_modifier(Modifier::ITALIC);
            builder.append_text("[image:", builder.current_style);
        }
        MdEvent::End(TagEnd::Image) => {
            builder.append_text("]", builder.current_style);
            builder.current_style = builder
                .style_stack
                .pop()
                .unwrap_or_else(|| Style::default().fg(theme::TEXT));
        }
        MdEvent::Code(text) => {
            builder.append_text(&text, builder.current_style.fg(theme::TOOL));
        }
        MdEvent::Text(text) => builder.append_text(&text, builder.current_style),
        MdEvent::Html(html) | MdEvent::InlineHtml(html) => {
            builder.append_text(&html, builder.current_style.fg(theme::MUTED));
        }
        MdEvent::InlineMath(math) => {
            builder.append_text(&format!("$ {} $", math), builder.current_style);
        }
        MdEvent::SoftBreak | MdEvent::HardBreak => builder.flush_cell_line(),
        MdEvent::TaskListMarker(checked) => {
            builder.append_text(if checked { "[x]" } else { "[ ]" }, builder.current_style);
        }
        _ => {}
    }
    None
}

fn table_natural_widths(rows: &[TableRow], columns: usize) -> Vec<usize> {
    (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.cells.get(column))
                .flat_map(|cell| cell.lines.iter().map(Line::width))
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect()
}

fn table_column_widths(natural: &[usize], available: usize) -> Vec<usize> {
    if natural.iter().sum::<usize>() <= available {
        return natural.to_vec();
    }

    let fair_share = (available / natural.len()).max(1);
    let mut widths: Vec<usize> = natural
        .iter()
        .map(|width| (*width).min(fair_share).max(1))
        .collect();
    let mut remaining = available.saturating_sub(widths.iter().sum::<usize>());
    while remaining > 0 {
        let column = (0..natural.len())
            .max_by_key(|&index| natural[index].saturating_sub(widths[index]))
            .unwrap_or(0);
        widths[column] += 1;
        remaining -= 1;
    }
    widths
}

fn table_requires_fallback(rows: &[TableRow], column_widths: &[usize]) -> bool {
    rows.iter().any(|row| {
        row.cells.iter().enumerate().any(|(column, cell)| {
            let width = column_widths.get(column).copied().unwrap_or(1);
            cell.lines.iter().any(|line| {
                line.spans.iter().any(|span| {
                    span.content
                        .chars()
                        .any(|ch| Span::raw(ch.to_string()).width() > width)
                })
            })
        })
    })
}

fn wrap_table_cell(cell: &TableCell, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for source in &cell.lines {
        let mut current = Line::from(Vec::<Span<'static>>::new());
        for span in &source.spans {
            let mut chunk = String::new();
            let mut chunk_width = 0;
            for ch in span.content.chars() {
                let char_width = Span::raw(ch.to_string()).width();
                let (rendered, rendered_width) = if char_width > width {
                    ("?".to_string(), 1)
                } else {
                    (ch.to_string(), char_width)
                };
                if rendered_width > 0 && current.width() + chunk_width + rendered_width > width {
                    if !chunk.is_empty() {
                        current
                            .spans
                            .push(Span::styled(std::mem::take(&mut chunk), span.style));
                        chunk_width = 0;
                    }
                    if current.width() + rendered_width > width {
                        wrapped.push(current);
                        current = Line::from(Vec::<Span<'static>>::new());
                    }
                }
                chunk.push_str(&rendered);
                chunk_width += rendered_width;
            }
            if !chunk.is_empty() {
                current.spans.push(Span::styled(chunk, span.style));
            }
        }
        wrapped.push(current);
    }
    if wrapped.is_empty() {
        wrapped.push(Line::default());
    }
    wrapped
}

fn append_aligned_table_cell(
    line: &mut Line<'static>,
    cell: Option<&Line<'static>>,
    width: usize,
    alignment: Alignment,
) {
    let cell_width = cell.map(Line::width).unwrap_or(0).min(width);
    let padding = width.saturating_sub(cell_width);
    let (left, right) = match alignment {
        Alignment::Right => (padding, 0),
        Alignment::Center => (padding / 2, padding - padding / 2),
        Alignment::Left | Alignment::None => (0, padding),
    };
    if left > 0 {
        line.spans.push(Span::raw(" ".repeat(left)));
    }
    if let Some(cell) = cell {
        line.spans.extend(cell.spans.iter().cloned());
    }
    if right > 0 {
        line.spans.push(Span::raw(" ".repeat(right)));
    }
}

fn render_table_fallback(rows: &[TableRow], width: usize) -> Vec<Line<'static>> {
    rows.iter()
        .flat_map(|row| {
            row.cells
                .iter()
                .flat_map(|cell| wrap_table_cell(cell, width.max(1)).into_iter())
        })
        .collect()
}

fn render_unclosed_fenced_fallback(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut current = Line::from(Vec::<Span<'static>>::new());
    let mut pending_space = false;
    let style = Style::default().fg(theme::TEXT);

    for source_line in text.split('\n') {
        if source_line.is_empty() {
            if !current.spans.is_empty() {
                out.push(current.clone());
                current = Line::from(Vec::<Span<'static>>::new());
            }
            out.push(Line::default());
            pending_space = false;
            continue;
        }
        append_markdown_text(
            &mut out,
            &mut current,
            source_line,
            width,
            style,
            &mut pending_space,
        );
        if !current.spans.is_empty() {
            out.push(current.clone());
            current = Line::from(Vec::<Span<'static>>::new());
        }
        pending_space = false;
    }
    if !current.spans.is_empty() {
        out.push(current);
    }
    out
}

fn bounded_prefix(prefix: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if Span::raw(prefix).width() <= width {
        return prefix.to_string();
    }
    let marker = prefix
        .chars()
        .rev()
        .find(|ch| !ch.is_whitespace())
        .unwrap_or('?');
    if Span::raw(marker.to_string()).width() <= width {
        marker.to_string()
    } else {
        "?".to_string()
    }
}

fn append_quote_prefix(current: &mut Line<'static>, quote_depth: usize, width: usize) {
    if quote_depth > 0 && current.spans.is_empty() {
        let full_prefix = format!("{}│ ", "  ".repeat(quote_depth.saturating_sub(1)));
        let prefix = bounded_prefix(&full_prefix, width.max(1));
        current
            .spans
            .push(Span::styled(prefix, Style::default().fg(theme::MUTED)));
    }
}

/// Append one Markdown text event to the current terminal line.
///
/// `pulldown-cmark` splits a paragraph into separate events around inline
/// markup (`Text("before ")`, `Code("inside")`, `Text(" after")`, etc.).
/// Those event boundaries are not display line boundaries, so wrapping has to
/// happen while the current line is shared across events.
fn append_markdown_text(
    out: &mut Vec<Line<'static>>,
    current: &mut Line<'static>,
    text: &str,
    width: usize,
    style: Style,
    pending_space: &mut bool,
) {
    let starts_with_space = text.chars().next().is_some_and(char::is_whitespace);
    let mut first_word = true;
    for word in text.split_whitespace() {
        let needs_space = if first_word {
            *pending_space || starts_with_space
        } else {
            true
        };
        append_markdown_word(out, current, word, width, style, needs_space);
        first_word = false;
    }
    *pending_space = text.chars().last().is_some_and(char::is_whitespace);
}

fn append_markdown_word(
    out: &mut Vec<Line<'static>>,
    current: &mut Line<'static>,
    word: &str,
    width: usize,
    style: Style,
    needs_space: bool,
) {
    let width = width.max(1);
    let separator = needs_space && !current.spans.is_empty() && !line_ends_with_whitespace(current);
    let separator_width = usize::from(separator);
    let word_width = Span::raw(word).width();

    if current.width() + separator_width + word_width <= width {
        if separator {
            current.spans.push(Span::styled(" ".to_string(), style));
        }
        current.spans.push(Span::styled(word.to_string(), style));
        return;
    }

    if !current.spans.is_empty() {
        out.push(current.clone());
        *current = Line::from(Vec::<Span<'static>>::new());
    }
    append_long_markdown_word(out, current, word, width, style);
}

fn append_long_markdown_word(
    out: &mut Vec<Line<'static>>,
    current: &mut Line<'static>,
    word: &str,
    width: usize,
    style: Style,
) {
    let mut remaining = word;
    while !remaining.is_empty() {
        let available = width.saturating_sub(current.width());
        if available == 0 {
            out.push(current.clone());
            *current = Line::from(Vec::<Span<'static>>::new());
            continue;
        }

        let mut used = 0usize;
        let mut end = 0usize;
        for (idx, ch) in remaining.char_indices() {
            let char_width = Span::raw(ch.to_string()).width();
            if used + char_width > available {
                break;
            }
            used += char_width;
            end = idx + ch.len_utf8();
        }

        if end == 0 {
            if current.spans.is_empty() {
                let ch_len = remaining
                    .chars()
                    .next()
                    .expect("remaining is non-empty")
                    .len_utf8();
                current.spans.push(Span::styled("?".to_string(), style));
                remaining = &remaining[ch_len..];
            } else {
                out.push(current.clone());
                *current = Line::from(Vec::<Span<'static>>::new());
            }
            continue;
        }

        current
            .spans
            .push(Span::styled(remaining[..end].to_string(), style));
        remaining = &remaining[end..];
        if !remaining.is_empty() {
            out.push(current.clone());
            *current = Line::from(Vec::<Span<'static>>::new());
        }
    }
}

fn line_ends_with_whitespace(line: &Line<'_>) -> bool {
    line.spans
        .last()
        .and_then(|span| span.content.chars().last())
        .is_some_and(char::is_whitespace)
}

fn highlight_code_block(lang: &str, code: &str, width: usize) -> Vec<Line<'static>> {
    let ss = syntax_set();
    let ts = theme_set();
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let theme = &ts.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    let code_width = width.max(1);
    for line in LinesWithEndings::from(code) {
        let mut fragments = Vec::new();
        if let Ok(parts) = h.highlight_line(line, ss) {
            fragments.extend(parts.into_iter().map(|(style, text)| {
                (
                    syntect_to_ratatui(style),
                    text.trim_end_matches(&['\r', '\n'][..]).to_string(),
                )
            }));
        }
        if fragments.is_empty() {
            fragments.push((
                Style::default().fg(theme::MUTED),
                line.trim_end().to_string(),
            ));
        }
        append_wrapped_code_fragments(&mut lines, &fragments, code_width);
    }
    if code.is_empty() {
        append_wrapped_code_fragments(
            &mut lines,
            &[(Style::default().fg(theme::MUTED), String::new())],
            code_width,
        );
    }
    lines
}

fn append_wrapped_code_fragments(
    lines: &mut Vec<Line<'static>>,
    fragments: &[(Style, String)],
    width: usize,
) {
    let prefix = if width >= 3 { "  " } else { "" };
    let mut current = Line::from(Span::styled(
        prefix.to_string(),
        Style::default().fg(theme::MUTED),
    ));
    let mut wrote_content = false;

    for (style, text) in fragments {
        for ch in text.chars() {
            let char_width = Span::raw(ch.to_string()).width();
            if char_width == 0 {
                current.spans.push(Span::styled(ch.to_string(), *style));
                continue;
            }
            if current.width().saturating_add(char_width) > width {
                if wrote_content {
                    lines.push(current);
                    current = Line::from(Span::styled(
                        prefix.to_string(),
                        Style::default().fg(theme::MUTED),
                    ));
                }
                if current.width().saturating_add(char_width) > width {
                    current.spans.push(Span::styled("?".to_string(), *style));
                    wrote_content = true;
                    continue;
                }
            }
            current.spans.push(Span::styled(ch.to_string(), *style));
            wrote_content = true;
        }
    }
    lines.push(current);
}

pub fn parse_md_line(line: &str) -> Line<'static> {
    render_markdown_block(line, 120)
        .into_iter()
        .next()
        .unwrap_or_else(|| Line::from(Span::raw(line.to_string())))
}
