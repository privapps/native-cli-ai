use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::time::Duration;

use super::ToolExecutor;

pub struct FetchUrlTool {
    client: reqwest::Client,
    config: WebConfig,
}

impl FetchUrlTool {
    pub fn new(config: WebConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .user_agent(config.user_agent.clone())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client, config }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for FetchUrlTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fetch_url".into(),
            description: "Fetch and normalize the text content of a URL".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let url = call.input["url"].as_str().unwrap_or("").trim();
        if url.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("url is required".into()),
            };
        }

        let response = self.client.get(url).send().await;
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("fetch failed: {err}")),
                };
            }
        };

        let status = response.status();
        if !status.is_success() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("unexpected status: {status}")),
            };
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = match response.text().await {
            Ok(body) => body,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("failed to read response body: {err}")),
                };
            }
        };

        let normalized = if content_type.contains("html") || body.contains("<html") {
            normalize_html(&body)
        } else {
            normalize_plain_text(&body)
        };

        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: normalized
                .chars()
                .take(self.config.max_fetch_chars)
                .collect(),
            error: None,
        }
    }
}

fn normalize_html(body: &str) -> String {
    let title = first_html_element(body, "title").map(|(_, content)| html_fragment_text(&content));
    let content = first_html_element(body, "body")
        .map(|(_, content)| html_fragment_text(&content))
        .unwrap_or_else(|| html_fragment_text(body));
    let title = title.filter(|text| !text.is_empty());

    match title {
        Some(title) if !content.is_empty() => format!("Title: {title}\n\n{content}"),
        Some(title) => title,
        None => content,
    }
}

fn normalize_plain_text(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove markup from a small HTML fragment without pulling in a full DOM
/// parser. The web tools only need readable text and a few class/attribute
/// lookups; keeping this parser deliberately narrow avoids a large dependency
/// tree for the core crate.
pub(crate) fn html_fragment_text(fragment: &str) -> String {
    let mut text = String::with_capacity(fragment.len());
    let mut cursor = 0;

    while let Some(markup) = next_html_markup(fragment, cursor) {
        text.push_str(&fragment[cursor..markup.start()]);
        text.push(' ');

        if let HtmlMarkup::Tag(tag) = markup
            && !tag.closing
            && !tag.self_closing
            && is_hidden_text_element(tag.name)
            && let Some((_, element_end)) = matching_element_bounds(fragment, tag)
        {
            cursor = element_end;
            continue;
        }

        cursor = markup.end();
    }
    text.push_str(&fragment[cursor..]);

    normalize_plain_text(&decode_html_entities(&text))
}

pub(crate) fn first_html_element(fragment: &str, tag: &str) -> Option<(String, String)> {
    first_html_element_matching(fragment, Some(tag), None)
}

pub(crate) fn first_html_element_with_class(
    fragment: &str,
    tag: &str,
    class_name: &str,
) -> Option<(String, String)> {
    first_html_element_matching(fragment, Some(tag), Some(class_name))
}

pub(crate) fn first_html_element_with_class_any_tag(
    fragment: &str,
    class_name: &str,
) -> Option<(String, String)> {
    first_html_element_matching(fragment, None, Some(class_name))
}

#[allow(dead_code)]
pub(crate) fn html_elements_with_class(
    fragment: &str,
    tag_name: &str,
    class_name: &str,
) -> Vec<String> {
    html_elements_with_class_matching(fragment, Some(tag_name), class_name)
}

pub(crate) fn html_elements_with_class_any_tag(fragment: &str, class_name: &str) -> Vec<String> {
    html_elements_with_class_matching(fragment, None, class_name)
}

fn html_elements_with_class_matching(
    fragment: &str,
    tag_name: Option<&str>,
    class_name: &str,
) -> Vec<String> {
    let mut elements = Vec::new();
    let mut cursor = 0;

    while let Some(markup) = next_html_markup(fragment, cursor) {
        cursor = markup.end();
        let HtmlMarkup::Tag(tag) = markup else {
            continue;
        };
        if tag.closing
            || tag.self_closing
            || tag_name.is_some_and(|name| !tag.name.eq_ignore_ascii_case(name))
            || !has_class(tag.attributes, class_name)
        {
            continue;
        }
        let Some((body_end, element_end)) = matching_element_bounds(fragment, tag) else {
            continue;
        };
        elements.push(fragment[tag.end..body_end].to_string());
        cursor = element_end;
    }

    elements
}

pub(crate) fn html_attribute(attributes: &str, name: &str) -> Option<String> {
    let bytes = attributes.as_bytes();
    let mut cursor = 0;

    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        let attribute_start = cursor;
        while cursor < bytes.len()
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        if attribute_start == cursor {
            cursor += 1;
            continue;
        }
        let attribute_name = &attributes[attribute_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        let (value_start, value_end) = match bytes.get(cursor).copied() {
            Some(quote @ (b'\'' | b'"')) => {
                cursor += 1;
                let start = cursor;
                while cursor < bytes.len() && bytes[cursor] != quote {
                    cursor += 1;
                }
                let end = cursor;
                cursor += usize::from(cursor < bytes.len());
                (start, end)
            }
            Some(_) => {
                let start = cursor;
                while cursor < bytes.len()
                    && !bytes[cursor].is_ascii_whitespace()
                    && bytes[cursor] != b'>'
                {
                    cursor += 1;
                }
                (start, cursor)
            }
            None => (cursor, cursor),
        };

        if attribute_name.eq_ignore_ascii_case(name) {
            return Some(decode_html_entities(&attributes[value_start..value_end]));
        }
    }

    None
}

fn decode_html_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(relative_ampersand) = text[cursor..].find('&') {
        let ampersand = cursor + relative_ampersand;
        decoded.push_str(&text[cursor..ampersand]);
        let Some(relative_semicolon) = text[ampersand + 1..].find(';') else {
            decoded.push_str(&text[ampersand..]);
            return decoded;
        };
        if relative_semicolon > 31 {
            decoded.push('&');
            cursor = ampersand + 1;
            continue;
        }

        let semicolon = ampersand + 1 + relative_semicolon;
        let entity = &text[ampersand + 1..semicolon];
        if let Some(character) = decode_numeric_entity(entity) {
            decoded.push(character);
        } else if let Some(replacement) = decode_named_entity(entity) {
            decoded.push_str(replacement);
        } else {
            decoded.push_str(&text[ampersand..=semicolon]);
        }
        cursor = semicolon + 1;
    }
    decoded.push_str(&text[cursor..]);
    decoded
}

fn decode_numeric_entity(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let value = if let Some(hex) = digits
        .strip_prefix('x')
        .or_else(|| digits.strip_prefix('X'))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse().ok()?
    };
    char::from_u32(value).filter(|character| *character != '\0')
}

fn decode_named_entity(entity: &str) -> Option<&'static str> {
    Some(match entity {
        "amp" => "&",
        "apos" => "'",
        "copy" => "©",
        "gt" => ">",
        "hellip" => "…",
        "laquo" => "«",
        "lt" => "<",
        "mdash" => "—",
        "nbsp" | "ensp" | "emsp" | "thinsp" => " ",
        "ndash" => "–",
        "quot" => "\"",
        "raquo" => "»",
        "reg" => "®",
        "trade" => "™",
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct HtmlTag<'a> {
    start: usize,
    end: usize,
    name: &'a str,
    attributes: &'a str,
    closing: bool,
    self_closing: bool,
}

#[derive(Clone, Copy)]
enum HtmlMarkup<'a> {
    Tag(HtmlTag<'a>),
    Other { start: usize, end: usize },
}

impl HtmlMarkup<'_> {
    fn start(self) -> usize {
        match self {
            Self::Tag(tag) => tag.start,
            Self::Other { start, .. } => start,
        }
    }

    fn end(self) -> usize {
        match self {
            Self::Tag(tag) => tag.end,
            Self::Other { end, .. } => end,
        }
    }
}

fn next_html_markup(fragment: &str, mut cursor: usize) -> Option<HtmlMarkup<'_>> {
    let bytes = fragment.as_bytes();
    while cursor < bytes.len() {
        let start = cursor + fragment[cursor..].find('<')?;
        if fragment[start..].starts_with("<!--") {
            let end = fragment[start + 4..]
                .find("-->")
                .map(|relative| start + 4 + relative + 3)
                .unwrap_or(fragment.len());
            return Some(HtmlMarkup::Other { start, end });
        }

        let mut name_start = start + 1;
        let closing = bytes.get(name_start) == Some(&b'/');
        name_start += usize::from(closing);
        if matches!(bytes.get(name_start), Some(b'!' | b'?')) {
            let end = find_tag_end(fragment, name_start + 1).unwrap_or(fragment.len());
            return Some(HtmlMarkup::Other { start, end });
        }
        let mut name_end = name_start;
        while name_end < bytes.len()
            && (bytes[name_end].is_ascii_alphanumeric()
                || matches!(bytes[name_end], b':' | b'-' | b'_'))
        {
            name_end += 1;
        }
        if name_end == name_start {
            cursor = start + 1;
            continue;
        }

        let Some(end) = find_tag_end(fragment, name_end) else {
            // Treat an unterminated tag as markup through EOF so malformed
            // responses do not leak raw tag syntax into normalized text.
            return Some(HtmlMarkup::Other {
                start,
                end: fragment.len(),
            });
        };
        let attributes = &fragment[name_end..end - 1];
        return Some(HtmlMarkup::Tag(HtmlTag {
            start,
            end,
            name: &fragment[name_start..name_end],
            attributes,
            closing,
            self_closing: !closing && attributes.trim_end().ends_with('/'),
        }));
    }
    None
}

fn find_tag_end(fragment: &str, mut cursor: usize) -> Option<usize> {
    let bytes = fragment.as_bytes();
    let mut quote = None;
    while cursor < bytes.len() {
        match (quote, bytes[cursor]) {
            (Some(expected), current) if current == expected => quote = None,
            (None, current @ (b'\'' | b'"')) => quote = Some(current),
            (None, b'>') => return Some(cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn first_html_element_matching(
    fragment: &str,
    tag_name: Option<&str>,
    class_name: Option<&str>,
) -> Option<(String, String)> {
    let mut cursor = 0;
    while let Some(markup) = next_html_markup(fragment, cursor) {
        cursor = markup.end();
        let HtmlMarkup::Tag(tag) = markup else {
            continue;
        };
        if tag.closing
            || tag.self_closing
            || tag_name.is_some_and(|name| !tag.name.eq_ignore_ascii_case(name))
            || class_name.is_some_and(|class| !has_class(tag.attributes, class))
        {
            continue;
        }
        let Some((body_end, _)) = matching_element_bounds(fragment, tag) else {
            continue;
        };
        return Some((
            tag.attributes.to_string(),
            fragment[tag.end..body_end].to_string(),
        ));
    }
    None
}

fn matching_element_bounds(fragment: &str, opening: HtmlTag<'_>) -> Option<(usize, usize)> {
    let mut cursor = opening.end;
    let mut depth = 1usize;
    while let Some(markup) = next_html_markup(fragment, cursor) {
        cursor = markup.end();
        let HtmlMarkup::Tag(tag) = markup else {
            continue;
        };
        if !tag.name.eq_ignore_ascii_case(opening.name) {
            continue;
        }
        if tag.closing {
            depth -= 1;
            if depth == 0 {
                return Some((tag.start, tag.end));
            }
        } else if !tag.self_closing {
            depth += 1;
        }
    }
    None
}

fn has_class(attributes: &str, class_name: &str) -> bool {
    html_attribute(attributes, "class").is_some_and(|classes| {
        classes
            .split_ascii_whitespace()
            .any(|class| class == class_name)
    })
}

fn is_hidden_text_element(tag_name: &str) -> bool {
    ["script", "style", "noscript", "template"]
        .iter()
        .any(|hidden| tag_name.eq_ignore_ascii_case(hidden))
}

#[cfg(test)]
mod tests {
    use super::{
        first_html_element, first_html_element_with_class, html_attribute, html_fragment_text,
        normalize_html,
    };

    #[test]
    fn normalizes_html_without_a_dom_parser() {
        let normalized = normalize_html(
            "<html><head><title> A &amp; B </title></head><body><script>ignore()</script><p>Hello <b>world</b>.</p></body></html>",
        );
        assert_eq!(normalized, "Title: A & B\n\nHello world .");
    }

    #[test]
    fn extracts_classed_elements_and_attributes() {
        let html = r#"<div class="result"><a class="result__a" href="/item?a=1&amp;b=2">Title</a><a class="result__snippet">Snippet</a></div>"#;
        let (attrs, title) = first_html_element_with_class(html, "a", "result__a").unwrap();
        assert_eq!(
            html_attribute(&attrs, "href").as_deref(),
            Some("/item?a=1&b=2")
        );
        assert_eq!(html_fragment_text(&title), "Title");
    }

    #[test]
    fn handles_nested_elements_and_gt_inside_quoted_attributes() {
        let html = r#"<div class="result" data-label="1 > 0"><div>Inner</div> tail</div>"#;
        let (attrs, body) = first_html_element_with_class(html, "div", "result").unwrap();

        assert_eq!(
            html_attribute(&attrs, "data-label").as_deref(),
            Some("1 > 0")
        );
        assert_eq!(html_fragment_text(&body), "Inner tail");
    }

    #[test]
    fn matches_class_tokens_and_flexible_attribute_syntax() {
        let html =
            r#"<a class="result__a--ad">Wrong</a><a CLASS = result__a href = '/right'>Right</a>"#;
        let (attrs, body) = first_html_element_with_class(html, "a", "result__a").unwrap();

        assert_eq!(html_fragment_text(&body), "Right");
        assert_eq!(html_attribute(&attrs, "HREF").as_deref(), Some("/right"));
    }

    #[test]
    fn preserves_slashes_in_unquoted_attribute_values() {
        let (attrs, _) = first_html_element("<a href=https://example.test/a/b>Link</a>", "a")
            .expect("anchor should parse");
        assert_eq!(
            html_attribute(&attrs, "href").as_deref(),
            Some("https://example.test/a/b")
        );
    }

    #[test]
    fn decodes_decimal_hex_and_common_named_entities() {
        assert_eq!(
            html_fragment_text("A&#160;B &#x2014; C &hellip; D &unknown;"),
            "A B — C … D &unknown;"
        );
    }

    #[test]
    fn element_matching_is_ascii_case_insensitive() {
        let (_, body) = first_html_element("<BODY><P>Hello</P></BODY>", "body").unwrap();
        assert_eq!(html_fragment_text(&body), "Hello");
    }

    #[test]
    fn malformed_unterminated_tags_do_not_leak_into_text() {
        assert_eq!(html_fragment_text("Before <a href='broken"), "Before");
    }
}
