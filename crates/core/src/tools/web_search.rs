use crate::research::{
    EvidenceRecord, ResearchContext, classify_source_authority, infer_report_metadata_for_issuer,
    parse_publication_date,
};
use chrono::{DateTime, Utc};
use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use super::ToolExecutor;
use super::fetch_url::{
    first_html_element, first_html_element_with_class, first_html_element_with_class_any_tag,
    html_attribute, html_elements_with_class_any_tag, html_fragment_text,
};

pub struct WebSearchTool {
    client: reqwest::Client,
    config: WebConfig,
    context: Arc<ResearchContext>,
}

impl WebSearchTool {
    pub fn new(config: WebConfig, context: Arc<ResearchContext>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .user_agent(config.user_agent.clone())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            config,
            context,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".into(),
            description: "Search the public web and return source-attributed titles, URLs, snippets, and available publication metadata".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer" },
                    "domains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional domains to prefer, such as an issuer investor-relations site or sec.gov"
                    },
                    "issuer": {
                        "type": "string",
                        "description": "Optional issuer name; provide this for financial-report resolution so result metadata is tied to the named issuer"
                    },
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

        let query = add_domain_hints(query, &call.input["domains"]);
        let response = self
            .client
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", query.as_str())])
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

        let retrieved_at = Utc::now();
        let as_of = self.context.as_of();
        let issuer = call.input["issuer"].as_str();
        let results = rows
            .into_iter()
            .map(|row| {
                let authority = classify_source_authority(&row.url);
                let report_metadata = infer_report_metadata_for_issuer(
                    &format!("{} {}", row.title, row.snippet),
                    issuer,
                );
                self.context.record_evidence(EvidenceRecord {
                    url: row.url.clone(),
                    title: Some(row.title.clone()),
                    snippet: Some(row.snippet.clone()),
                    retrieved_at,
                    response_status: None,
                    http_date: None,
                    published_at: row.published_at,
                    authority,
                    report_metadata: report_metadata.clone(),
                });
                json!({
                    "title": row.title,
                    "url": row.url,
                    "snippet": row.snippet,
                    "published_at": row.published_at,
                    "retrieved_at": retrieved_at,
                    "source_authority": authority,
                    "report_metadata": report_metadata,
                    "eligible_as_of": row.published_at.map(|date| date.date_naive() <= as_of),
                })
            })
            .collect::<Vec<_>>();

        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "query": query,
                "as_of": as_of,
                "retrieved_at": retrieved_at,
                "results": results,
            }))
            .unwrap_or_else(|_| "{\"results\":[]}".into()),
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    published_at: Option<DateTime<Utc>>,
}

fn add_domain_hints(query: &str, domains: &serde_json::Value) -> String {
    let mut query = query.to_string();
    if let Some(domains) = domains.as_array() {
        for domain in domains.iter().filter_map(|domain| domain.as_str()) {
            let domain = domain.trim();
            if !domain.is_empty() {
                query.push_str(" site:");
                query.push_str(domain);
            }
        }
    }
    query
}

fn clean_href(href: &str) -> String {
    if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href.to_string()
    }
}

fn parse_search_results(body: &str, limit: usize) -> Vec<SearchResult> {
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
            let published_at = first_html_element_with_class_any_tag(&result, "result__timestamp")
                .and_then(|(_, timestamp)| parse_publication_date(&html_fragment_text(&timestamp)));
            rows.push(SearchResult {
                title,
                url,
                snippet,
                published_at,
            });
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
                super::SearchResult {
                    title: "One".into(),
                    url: "https://example.com/one".into(),
                    snippet: "First …".into(),
                    published_at: None,
                },
                super::SearchResult {
                    title: "Two".into(),
                    url: "/two".into(),
                    snippet: "Second".into(),
                    published_at: None,
                },
            ]
        );
    }

    #[test]
    fn parses_a_structured_publication_date_from_a_search_result() {
        let html = r#"
            <div class="result">
                <a class="result__a" href="https://investor.example.com/results">Results</a>
                <span class="result__timestamp">2026-07-29</span>
                <p class="result__snippet">Annual earnings release</p>
            </div>
        "#;

        let result = parse_search_results(html, 1).pop().expect("result");
        assert_eq!(
            result.published_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-07-29T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
    }
}
