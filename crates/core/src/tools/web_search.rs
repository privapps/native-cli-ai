use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::time::Duration;

use super::ToolExecutor;
use super::fetch_url::{
    first_html_element, first_html_element_with_class, first_html_element_with_class_any_tag,
    html_attribute, html_elements_with_class_any_tag, html_fragment_text,
};

pub struct WebSearchTool {
    client: reqwest::Client,
    config: WebConfig,
}

impl WebSearchTool {
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
impl ToolExecutor for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".into(),
            description: "Search the public web and return titles, URLs, and snippets".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let query = call.input["query"].as_str().unwrap_or("").trim();
        let limit = call.input["limit"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or(self.config.default_search_limit)
            .clamp(1, 10);

        if query.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("query is required".into()),
            };
        }

        let response = self
            .client
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", query)])
            .send()
            .await;

        let body = match response {
            Ok(response) => match response.text().await {
                Ok(body) => body,
                Err(err) => {
                    return ToolResult {
                        call_id: call.id.clone(),
                        success: false,
                        output: String::new(),
                        error: Some(format!("failed to read search response: {err}")),
                    };
                }
            },
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("search request failed: {err}")),
                };
            }
        };

        let rows = parse_search_results(&body, limit);

        if rows.is_empty() {
            let fallback = html_fragment_text(&body);
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: fallback.chars().take(self.config.max_fetch_chars).collect(),
                error: Some("no structured search results parsed".into()),
            };
        }

        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: rows.join("\n"),
            error: None,
        }
    }
}

fn clean_href(href: &str) -> String {
    if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href.to_string()
    }
}

fn parse_search_results(body: &str, limit: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for result in classed_divisions(body, "result").into_iter().take(limit) {
        let title_node = first_html_element_with_class(&result, "a", "result__a").or_else(|| {
            let (_, title_container) =
                first_html_element_with_class_any_tag(&result, "result__title")?;
            first_html_element(&title_container, "a")
        });
        let Some((title_attributes, title_body)) = title_node else {
            continue;
        };
        let title = html_fragment_text(&title_body);
        let url = html_attribute(&title_attributes, "href")
            .map(|href| clean_href(&href))
            .unwrap_or_default();
        let snippet = first_html_element_with_class_any_tag(&result, "result__snippet")
            .map(|(_, snippet_body)| html_fragment_text(&snippet_body))
            .unwrap_or_default();
        if !title.is_empty() && !url.is_empty() {
            rows.push(format!("- {title}\n  URL: {url}\n  Snippet: {snippet}"));
        }
    }
    rows
}

fn classed_divisions(html: &str, class_name: &str) -> Vec<String> {
    html_elements_with_class_any_tag(html, class_name)
}

#[cfg(test)]
mod tests {
    use super::{classed_divisions, parse_search_results};

    #[test]
    fn finds_nested_search_result_divisions() {
        let html = r#"<div class="result"><div class="result__body"><a class="result__a" href="/one">One</a></div></div><div class="result"><a class="result__a" href="/two">Two</a></div>"#;
        let results = classed_divisions(html, "result");
        assert_eq!(results.len(), 2);
        assert!(results[0].contains("result__body"));
        assert!(results[1].contains("/two"));
    }

    #[test]
    fn finds_a_final_result_without_a_following_division() {
        let results = classed_divisions(
            r#"<div class="result"><a class="result__a" href="/only">Only</a></div>"#,
            "result",
        );

        assert_eq!(results.len(), 1);
        assert!(results[0].contains("/only"));
    }

    #[test]
    fn matches_result_class_as_an_exact_token_with_flexible_syntax() {
        let html = r#"<div class="result--ad">Ad</div><DIV CLASS = 'result other'>Hit</DIV>"#;
        assert_eq!(classed_divisions(html, "result"), vec!["Hit"]);
    }

    #[test]
    fn accepts_result_containers_other_than_divisions() {
        let html = r#"<article class="result"><a class="result__a" href="/one">One</a></article>"#;
        assert_eq!(classed_divisions(html, "result").len(), 1);
    }

    #[test]
    fn parses_both_duckduckgo_title_shapes_and_any_snippet_element() {
        let html = r#"
            <div class="result">
                <h2 class="result__title"><a href="//example.com/one">One</a></h2>
                <p class="result__snippet">First &#x2026;</p>
            </div>
            <div class="result">
                <a class="result__a" href="/two">Two</a>
                <span class="result__snippet"><b>Second</b></span>
            </div>
        "#;

        assert_eq!(
            parse_search_results(html, 10),
            vec![
                "- One\n  URL: https://example.com/one\n  Snippet: First …",
                "- Two\n  URL: /two\n  Snippet: Second",
            ]
        );
    }
}
