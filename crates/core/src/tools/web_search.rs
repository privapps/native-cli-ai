use crate::research::{
    EvidenceRecord, ResearchContext, classify_source_authority, infer_report_metadata_for_issuer,
    parse_publication_date,
};
use chrono::{DateTime, Utc};
use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use super::ToolExecutor;
use super::fetch_url::{
    first_html_element, first_html_element_with_class, first_html_element_with_class_any_tag,
    html_attribute, html_elements_with_class_any_tag, html_fragment_text,
};

const MAX_SEARCH_CHALLENGE_RETRIES: u32 = 10;

/// Process-local policy and state for DuckDuckGo requests.
///
/// Runtime-created search tools receive the process-wide instance. Fixture
/// constructors may create an isolated instance so tests do not share state.
pub struct SearchLimiter {
    in_flight: Arc<Semaphore>,
    state: Mutex<SearchLimiterState>,
    min_interval: Duration,
    initial_cooldown: Duration,
    max_cooldown: Duration,
}

struct SearchLimiterState {
    last_started: Option<Instant>,
    cooldown_until: Option<Instant>,
    challenge_count: u32,
}

impl SearchLimiter {
    /// Create an isolated limiter, intended for deterministic fixture tests.
    pub fn new(config: &WebConfig) -> Arc<Self> {
        Arc::new(Self {
            in_flight: Arc::new(Semaphore::new(1)),
            state: Mutex::new(SearchLimiterState {
                last_started: None,
                cooldown_until: None,
                challenge_count: 0,
            }),
            min_interval: Duration::from_millis(config.search_min_interval_ms),
            initial_cooldown: Duration::from_millis(config.search_cooldown_ms),
            max_cooldown: Duration::from_millis(config.search_max_cooldown_ms)
                .max(Duration::from_millis(config.search_cooldown_ms)),
        })
    }

    async fn acquire(&self) -> OwnedSemaphorePermit {
        self.in_flight
            .clone()
            .acquire_owned()
            .await
            .expect("DuckDuckGo limiter semaphore should never close")
    }

    async fn wait_until_allowed(&self) {
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let interval_wait = state
                    .last_started
                    .and_then(|last| self.min_interval.checked_sub(last.elapsed()));
                let cooldown_wait = state
                    .cooldown_until
                    .and_then(|until| until.checked_duration_since(now));

                match (interval_wait, cooldown_wait) {
                    (Some(interval), Some(cooldown)) => Some(interval.max(cooldown)),
                    (Some(wait), None) | (None, Some(wait)) => Some(wait),
                    (None, None) => {
                        state.last_started = Some(now);
                        None
                    }
                }
            };

            match wait {
                Some(wait) => tokio::time::sleep(wait).await,
                None => return,
            }
        }
    }

    async fn record_challenge(&self) {
        let mut state = self.state.lock().await;
        state.challenge_count = state.challenge_count.saturating_add(1).min(16);
        let exponent = state.challenge_count.saturating_sub(1).min(16);
        let multiplier = 1_u32 << exponent;
        let cooldown = self
            .initial_cooldown
            .saturating_mul(multiplier)
            .min(self.max_cooldown);
        state.cooldown_until = Some(Instant::now() + cooldown);
    }

    async fn record_success(&self) {
        let mut state = self.state.lock().await;
        state.challenge_count = 0;
        state.cooldown_until = None;
    }
}

fn process_wide_search_limiter(config: &WebConfig) -> Arc<SearchLimiter> {
    static LIMITER: OnceLock<Arc<SearchLimiter>> = OnceLock::new();
    LIMITER.get_or_init(|| SearchLimiter::new(config)).clone()
}

pub struct WebSearchTool {
    client: reqwest::Client,
    config: WebConfig,
    context: Arc<ResearchContext>,
    search_url: String,
    limiter: Arc<SearchLimiter>,
}

/// Returns whether a search error is a definitive provider outcome rather than
/// a transient request or parser failure that may be worth retrying.
pub(crate) fn is_non_retryable_search_failure(error: Option<&str>) -> bool {
    error.is_some_and(|error| {
        error.starts_with("search provider blocked the request")
            || error == "no search results found for the query"
    })
}

impl WebSearchTool {
    pub fn new(config: WebConfig, context: Arc<ResearchContext>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .user_agent(config.user_agent.clone())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self::with_client_and_endpoint_and_limiter(
            config.clone(),
            context,
            client,
            "https://html.duckduckgo.com/html/",
            process_wide_search_limiter(&config),
        )
    }

    /// Construct a search tool with an injected HTTP client and endpoint.
    ///
    /// The runtime uses [`Self::new`]. This seam keeps fixture tests local and
    /// deterministic while exercising the same HTTP and parsing boundary.
    pub fn with_client_and_endpoint(
        config: WebConfig,
        context: Arc<ResearchContext>,
        client: reqwest::Client,
        search_url: impl Into<String>,
    ) -> Self {
        Self::with_client_and_endpoint_and_limiter(
            config.clone(),
            context,
            client,
            search_url,
            SearchLimiter::new(&config),
        )
    }

    /// Construct a search tool with an explicitly shared limiter.
    pub fn with_client_and_endpoint_and_limiter(
        config: WebConfig,
        context: Arc<ResearchContext>,
        client: reqwest::Client,
        search_url: impl Into<String>,
        limiter: Arc<SearchLimiter>,
    ) -> Self {
        Self {
            client,
            config,
            context,
            search_url: search_url.into(),
            limiter,
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
        let retries = self
            .config
            .search_challenge_retries
            .min(MAX_SEARCH_CHALLENGE_RETRIES);
        let mut retry_index = 0;
        let rows = loop {
            let _permit = self.limiter.acquire().await;
            self.limiter.wait_until_allowed().await;
            let response = self
                .client
                .get(&self.search_url)
                .query(&[("q", query.as_str())])
                .send()
                .await;

            let (status, body) = match response {
                Ok(response) => {
                    let status = response.status();
                    match response.text().await {
                        Ok(body) => (status, body),
                        Err(err) => {
                            return ToolResult {
                                call_id: call.id.clone(),
                                success: false,
                                output: String::new(),
                                error: Some(format!("failed to read search response: {err}")),
                            };
                        }
                    }
                }
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
            if is_retryable_duckduckgo_challenge(status, &body) {
                self.limiter.record_challenge().await;
                if retry_index < retries {
                    retry_index += 1;
                    tracing::debug!(
                        retry = retry_index,
                        max_retries = retries,
                        "DuckDuckGo anti-bot challenge; waiting for limiter cooldown before retry"
                    );
                    continue;
                }
            }

            if let Some(error) = classify_search_failure(status, &body, rows.is_empty()) {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(error),
                };
            }

            self.limiter.record_success().await;
            break rows;
        };

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

fn classify_search_failure(
    status: StatusCode,
    body: &str,
    no_structured_results: bool,
) -> Option<String> {
    if !no_structured_results {
        if !status.is_success() {
            return Some(format!(
                "search provider returned HTTP status {}",
                status.as_u16()
            ));
        }
        return None;
    }

    if is_duckduckgo_challenge_body(body) {
        return Some(format!(
            "search provider blocked the request (HTTP {}): DuckDuckGo returned an anti-bot challenge",
            status.as_u16()
        ));
    }

    let body_lower = body.to_ascii_lowercase();

    if !status.is_success() {
        return Some(format!(
            "search provider returned HTTP status {}",
            status.as_u16()
        ));
    }

    if body_lower.contains("no results found") {
        return Some("no search results found for the query".into());
    }

    Some(format!(
        "search response contained no recognized structured results (HTTP {})",
        status.as_u16()
    ))
}

fn is_duckduckgo_challenge_body(body: &str) -> bool {
    let body_lower = body.to_ascii_lowercase();
    body_lower.contains("anomaly-modal")
        || body_lower.contains("challenge-form")
        || body_lower.contains("unfortunately, bots use duckduckgo too")
}

fn is_retryable_duckduckgo_challenge(status: StatusCode, body: &str) -> bool {
    status == StatusCode::ACCEPTED && is_duckduckgo_challenge_body(body)
}

#[cfg(test)]
mod tests {
    use super::{classed_divisions, classify_search_failure, parse_search_results};
    use reqwest::StatusCode;

    #[test]
    fn classifies_duckduckgo_antibot_challenge_with_status() {
        let error = classify_search_failure(
            StatusCode::ACCEPTED,
            r#"<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>"#,
            true,
        )
        .expect("challenge should be classified");

        assert_eq!(
            error,
            "search provider blocked the request (HTTP 202): DuckDuckGo returned an anti-bot challenge"
        );
    }

    #[test]
    fn classifies_legitimate_empty_results_separately() {
        let error = classify_search_failure(
            StatusCode::OK,
            "No results found for the requested query",
            true,
        )
        .expect("empty results should be classified");

        assert_eq!(error, "no search results found for the query");
    }

    #[test]
    fn classifies_unrecognized_result_markup_separately() {
        let error = classify_search_failure(
            StatusCode::OK,
            "<html><body>Unexpected layout</body></html>",
            true,
        )
        .expect("unrecognized markup should be classified");

        assert_eq!(
            error,
            "search response contained no recognized structured results (HTTP 200)"
        );
    }

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
