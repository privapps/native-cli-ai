//! Snapshot tests for transcript rendering helpers.

use std::path::PathBuf;

use nca_common::event::{AgentEvent, EventEnvelope};
use nca_tui::tui::replay_event_log_into_state;
use nca_tui::tui::state::{DisplayBlock, TuiSessionState};
use nca_tui::tui::transcript::{
    ensure_transcript_cache, parse_md_line, render_markdown_block, transcript_lines,
    transcript_lines_and_hits, wrap_text,
};
use ratatui::style::Modifier;
use ratatui::text::Line;
use std::sync::{Arc, Mutex};

fn line_to_plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn lines_to_plain(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcript_body_plain(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .skip(2)
        .take_while(|line| !line.spans.is_empty())
        .map(line_to_plain)
        .collect()
}

#[test]
fn wrap_text_handles_unicode_and_long_words() {
    let input = "Rust is a systems programming language focused on safety, speed, and concurrency — featuring zero-cost abstractions and a friendly compiler.";
    let wrapped = wrap_text(input, 40);
    insta::assert_debug_snapshot!("wrap_text_40", wrapped);

    let wrapped_narrow = wrap_text(input, 20);
    insta::assert_debug_snapshot!("wrap_text_20", wrapped_narrow);
}

#[test]
fn wrap_text_preserves_explicit_newlines() {
    let input = "line one\nline two is a bit longer and should wrap\nline three";
    let wrapped = wrap_text(input, 24);
    insta::assert_debug_snapshot!("wrap_text_newlines_24", wrapped);
}

#[test]
fn parse_md_line_styles_common_markdown() {
    let samples = [
        "# Heading",
        "## Subheading",
        "- bullet item",
        "1. numbered item",
        "> quoted text",
        "plain paragraph",
        "`inline code`",
        "```rust",
        "    indented code block",
    ];

    let rendered: Vec<String> = samples
        .iter()
        .map(|s| format!("{:>30} => {}", s, line_to_plain(&parse_md_line(s))))
        .collect();

    insta::assert_debug_snapshot!("parse_md_line_samples", rendered);
}

#[test]
fn markdown_block_renders_list_items() {
    let lines = render_markdown_block("- alpha\n- beta", 40);
    let plain: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join("");
    insta::assert_snapshot!("markdown_list_block", plain);
}

#[test]
fn inline_markdown_does_not_insert_display_line_breaks() {
    let lines = render_markdown_block("I'm **nca**, a general-purpose assistant.", 80);

    assert_eq!(
        lines.len(),
        1,
        "inline Markdown should remain one display line"
    );
    assert_eq!(
        lines_to_plain(&lines),
        "I'm nca, a general-purpose assistant."
    );
}

#[test]
fn common_inline_markdown_has_visible_styles_without_delimiters() {
    let lines = render_markdown_block(
        "**bold** *italic* ~~removed~~ `code` [link](https://example.com)",
        120,
    );
    let plain = lines_to_plain(&lines);

    assert_eq!(plain, "bold italic removed code link");
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(lines[0].spans.iter().any(|span| {
        span.content == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
    }));
    assert!(lines[0].spans.iter().any(|span| {
        span.content == "removed" && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
    }));
    assert!(lines[0].spans.iter().any(|span| {
        span.content == "link" && span.style.add_modifier.contains(Modifier::UNDERLINED)
    }));
}

#[test]
fn common_block_markdown_preserves_structure() {
    let lines = render_markdown_block(
        "# Heading\n\n> quoted\n\n1. first\n2. second\n\n- [x] done\n- [ ] todo\n\n---\n\nafter  \nbreak",
        40,
    );
    let plain: Vec<String> = lines.iter().map(line_to_plain).collect();

    assert_eq!(plain[0], "Heading");
    assert!(
        lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(plain.iter().any(|line| line == "│ quoted"));
    assert!(plain.iter().any(|line| line == "1. first"));
    assert!(plain.iter().any(|line| line == "2. second"));
    assert!(plain.iter().any(|line| line == "- [x] done"));
    assert!(plain.iter().any(|line| line == "- [ ] todo"));
    assert!(plain.iter().any(|line| line.chars().all(|ch| ch == '─')));
    assert!(plain.iter().any(|line| line == "after"));
    assert!(plain.iter().any(|line| line == "break"));
}

#[test]
fn markdown_soft_and_hard_breaks_remain_display_lines() {
    let soft = render_markdown_block("soft\nbreak", 80);
    let hard = render_markdown_block("hard  \nbreak", 80);

    assert_eq!(
        soft.iter().map(line_to_plain).collect::<Vec<_>>(),
        vec!["soft", "break"]
    );
    assert_eq!(
        hard.iter().map(line_to_plain).collect::<Vec<_>>(),
        vec!["hard", "break"]
    );
}

#[test]
fn highlighted_code_is_bounded_and_keeps_content() {
    let lines = render_markdown_block(
        "```rust\nfn a_very_long_function_name() { println!(\"hello\"); }\n```",
        20,
    );
    let plain = lines_to_plain(&lines);

    let code_without_line_prefixes = plain.lines().map(str::trim_start).collect::<String>();
    assert!(code_without_line_prefixes.contains("fn a_very_long_function_name()"));
    assert!(code_without_line_prefixes.contains("println!(\"hello\");"));
    assert!(lines.iter().all(|line| line.width() <= 20));
    assert!(
        lines
            .iter()
            .any(|line| { line.spans.iter().any(|span| !span.style.fg.is_none()) })
    );
}

#[test]
fn unsupported_markdown_has_readable_fallbacks() {
    let lines = render_markdown_block("![diagram](image.png)\n\nraw <widget> markup", 80);
    let plain = lines_to_plain(&lines);

    assert!(plain.contains("diagram"));
    assert!(plain.contains("[image:"));
    assert!(plain.contains("<widget>"));
}

#[test]
fn markdown_table_preserves_rows_columns_and_header_divider() {
    let lines = render_markdown_block(
        "| Name | Role |\n| :--- | ---: |\n| Ada | **Engineer** |\n| Lin | Reviewer |",
        60,
    );
    let plain: Vec<String> = lines.iter().map(line_to_plain).collect();

    assert_eq!(plain.len(), 4);
    assert!(plain[0].contains("Name") && plain[0].contains("Role"));
    assert!(plain[0].contains("│"));
    assert!(plain[1].contains("┼"), "header divider: {:?}", plain[1]);
    assert!(plain[2].contains("Ada") && plain[2].contains("Engineer"));
    assert!(plain[3].contains("Lin") && plain[3].contains("Reviewer"));
    assert_eq!(plain[2].matches('│').count(), 1);
}

#[test]
fn markdown_table_applies_left_center_and_right_alignment() {
    let lines = render_markdown_block(
        "| Left | Center | Right |\n| :--- | :----: | ---: |\n| a | b | c |",
        60,
    );
    let plain: Vec<String> = lines.iter().map(line_to_plain).collect();

    assert_eq!(plain.len(), 3);
    assert!(
        plain[2].starts_with('a'),
        "left aligned row: {:?}",
        plain[2]
    );
    assert!(
        plain[2].contains("│   b"),
        "center aligned row: {:?}",
        plain[2]
    );
    assert!(plain[2].ends_with('c'), "right aligned row: {:?}", plain[2]);
}

#[test]
fn markdown_table_keeps_inline_styles_inside_cells() {
    let lines = render_markdown_block(
        "| Content | Link |\n| --- | --- |\n| **bold** *italic* ~~gone~~ `code` | [docs](https://example.com) |",
        100,
    );
    let row = &lines[2];

    assert!(row.spans.iter().any(|span| {
        span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    assert!(row.spans.iter().any(|span| {
        span.content == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
    }));
    assert!(row.spans.iter().any(|span| {
        span.content == "gone" && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
    }));
    assert!(
        row.spans
            .iter()
            .any(|span| { span.content == "code" && span.style.fg.is_some() })
    );
    assert!(row.spans.iter().any(|span| {
        span.content == "docs" && span.style.add_modifier.contains(Modifier::UNDERLINED)
    }));
}

#[test]
fn markdown_table_wraps_narrow_cells_without_losing_content() {
    let lines = render_markdown_block(
        "| Name | Description |\n| --- | --- |\n| Ada | A long description that must wrap in a narrow terminal |",
        24,
    );
    let plain = lines_to_plain(&lines);

    assert!(plain.contains("Ada"));
    assert!(
        plain.contains("A long description"),
        "wrapped table: {plain}"
    );
    assert!(plain.contains("narrow terminal"));
    assert!(lines.iter().all(|line| line.width() <= 24));
    assert!(lines.len() > 3, "wrapped table: {:?}", plain);
}

#[test]
fn malformed_table_like_input_remains_readable() {
    let lines = render_markdown_block("| incomplete | table", 24);
    let plain = lines_to_plain(&lines);

    assert!(plain.contains("incomplete"));
    assert!(plain.contains("table"));
    assert!(lines.iter().all(|line| line.width() <= 24));
}

#[test]
fn narrow_many_column_table_falls_back_without_losing_cell_content() {
    let lines = render_markdown_block(
        "| A | B | C | D | E | F | G | H |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n| one | two | three | four | five | six | seven | eight |",
        12,
    );
    let plain = lines_to_plain(&lines);

    for value in [
        "one", "two", "three", "four", "five", "six", "seven", "eight",
    ] {
        assert!(
            plain.contains(value),
            "missing {value} in fallback: {plain}"
        );
    }
    assert!(lines.iter().all(|line| line.width() <= 12));
}

#[test]
fn wide_unicode_in_a_narrow_column_uses_bounded_fallback() {
    let lines = render_markdown_block("| Glyph | Value |\n| --- | --- |\n| 界 | visible |", 4);
    let plain = lines_to_plain(&lines);
    let compact = plain.replace('\n', "");

    assert!(compact.contains('界'), "wide glyph was lost: {plain}");
    assert!(
        compact.contains("visible"),
        "cell content was lost: {plain}"
    );
    assert!(lines.iter().all(|line| line.width() <= 4));
}

#[test]
fn long_unbroken_markdown_words_are_bounded() {
    let lines = render_markdown_block("supercalifragilistic", 6);
    let plain = lines_to_plain(&lines);

    assert_eq!(plain.replace('\n', ""), "supercalifragilistic");
    assert!(lines.iter().all(|line| line.width() <= 6));
}

#[test]
fn wide_unicode_text_in_a_single_column_is_bounded() {
    let lines = render_markdown_block("界", 1);

    assert_eq!(lines_to_plain(&lines), "?");
    assert!(lines.iter().all(|line| line.width() <= 1));
}

#[test]
fn wide_unicode_code_in_a_narrow_block_is_bounded() {
    let lines = render_markdown_block("```text\n界\n```", 3);
    let plain = lines_to_plain(&lines);

    assert!(plain.contains('?'));
    assert!(lines.iter().all(|line| line.width() <= 3));
}

#[test]
fn narrow_markdown_prefixes_remain_width_bounded() {
    for markdown in ["> 界", "- item", "1. item", "- [ ] task"] {
        let lines = render_markdown_block(markdown, 1);
        assert!(
            lines.iter().all(|line| line.width() <= 1),
            "unbounded prefix for {markdown:?}: {lines:?}"
        );
    }
}

#[test]
fn unclosed_fenced_fallback_remains_width_bounded() {
    let lines = render_markdown_block("```\n界", 1);

    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| line.width() <= 1));
}

#[test]
fn completed_and_streaming_assistant_use_the_same_renderer() {
    let markdown = "# Results\n\n| Name | Status |\n| --- | --- |\n| **Ada** | ready |";
    let mut completed = TuiSessionState::new(
        "completed".into(),
        "model".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp/workspace"),
    );
    completed
        .blocks
        .push(DisplayBlock::Assistant(markdown.into()));

    let mut streaming = TuiSessionState::new(
        "streaming".into(),
        "model".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp/workspace"),
    );
    streaming.streaming_assistant = Some(markdown.into());

    let expected = render_markdown_block(markdown, 60)
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>();
    let completed_body = transcript_body_plain(&transcript_lines(&completed, 60));
    let streaming_body = transcript_body_plain(&transcript_lines(&streaming, 60));

    assert_eq!(completed_body, expected);
    assert_eq!(streaming_body, expected);
}

#[test]
fn transcript_cache_reuses_corrected_markdown_without_changing_raw_text() {
    let markdown = "| Name | Role |\n| --- | --- |\n| Ada | **Engineer** |";
    let mut state = TuiSessionState::new(
        "cached".into(),
        "model".into(),
        "@build".into(),
        "default".into(),
        PathBuf::from("/tmp/workspace"),
    );
    state.apply_event(&AgentEvent::MessageReceived {
        role: "assistant".into(),
        content: markdown.into(),
    });

    let uncached = transcript_lines_and_hits(&state, 60).0;
    let cached = ensure_transcript_cache(&mut state, 60).lines.clone();

    assert_eq!(
        uncached.iter().map(line_to_plain).collect::<Vec<_>>(),
        cached.iter().map(line_to_plain).collect::<Vec<_>>()
    );
    assert_eq!(state.last_assistant_text(), Some(markdown));
    assert!(cached.iter().any(|line| line_to_plain(line).contains('│')));
    assert!(cached.iter().any(|line| line_to_plain(line).contains('┼')));
}

#[tokio::test]
async fn replay_preserves_raw_assistant_markdown_and_renders_tables() {
    let temp = tempfile::tempdir().expect("event-log directory");
    let path = temp.path().join("session.events.jsonl");
    let markdown = "| Name | Role |\n| --- | --- |\n| Ada | **Engineer** |";
    let envelope = serde_json::to_string(&EventEnvelope::new(
        1,
        AgentEvent::MessageReceived {
            role: "assistant".into(),
            content: markdown.into(),
        },
    ))
    .expect("serialize assistant event");
    tokio::fs::write(&path, format!("{envelope}\n"))
        .await
        .expect("write event log");

    let state = Arc::new(Mutex::new(TuiSessionState::new(
        "replay".into(),
        "model".into(),
        "@build".into(),
        "default".into(),
        temp.path().to_path_buf(),
    )));
    replay_event_log_into_state(&path, &state).await;

    let state = state.lock().expect("state lock");
    assert_eq!(state.last_assistant_text(), Some(markdown));
    let lines = transcript_lines(&state, 60);
    let plain = lines_to_plain(&lines);
    assert!(plain.contains("Ada"));
    assert!(plain.contains("Engineer"));
    assert!(plain.contains('│'));
    assert!(plain.contains('┼'));
}

#[test]
fn transcript_lines_full_session_snapshot() {
    let mut state = TuiSessionState::new(
        "sess-snapshot".into(),
        "MiniMax-M2.5".into(),
        "default".into(),
        "default".into(),
        PathBuf::from("/tmp/workspace"),
    );

    state.blocks.push(DisplayBlock::User(
        "Please read src/main.rs and summarize.".into(),
    ));
    state.blocks.push(DisplayBlock::ToolRunning {
        name: "read_file".into(),
        call_id: "call-1".into(),
        input: "{\"path\":\"src/main.rs\"}".into(),
    });
    state.blocks.push(DisplayBlock::ToolDone {
        name: "read_file".into(),
        ok: true,
        detail: "42 lines".into(),
    });
    state.blocks.push(DisplayBlock::Assistant(
        "The file defines the `main` function and a helper.\n\n- Loads config.\n- Starts runtime.\n- Blocks on Ctrl-C.".into(),
    ));
    state
        .blocks
        .push(DisplayBlock::System("Session saved.".into()));

    let lines = transcript_lines(&state, 60);
    insta::assert_snapshot!("transcript_lines_full", lines_to_plain(&lines));
}
