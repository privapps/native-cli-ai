use crate::research::{
    EvidenceRecord, ResearchContext, classify_source_authority, infer_report_metadata_for_issuer,
    parse_publication_date,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use quick_xml::de::from_str as from_xml_str;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::ToolExecutor;
use super::fetch_url::{
    first_html_element, first_html_element_with_class, first_html_element_with_class_any_tag,
    html_attribute, html_elements_with_class_any_tag, html_fragment_text,
};

const MAX_SEARCH_RETRY_ATTEMPTS: u32 = 10;
const BING_SEARCH_URL: &str = "https://www.bing.com/search";
const DUCKDUCKGO_SEARCH_URL: &str = "https://html.duckduckgo.com/html";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderErrorKind {
    Transport,
    Http { status: u16, retryable: bool },
    Empty,
    Blocked,
    Parse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderError {
    provider: &'static str,
    kind: ProviderErrorKind,
    message: String,
}

impl ProviderError {
    fn transport(provider: &'static str, message: impl Into<String>) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::Transport,
            message: message.into(),
        }
    }

    fn http(provider: &'static str, status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::Http {
                status: status.as_u16(),
                retryable: is_retryable_status(status),
            },
            message: message.into(),
        }
    }

    fn empty(provider: &'static str) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::Empty,
            message: "no usable results".into(),
        }
    }

    fn blocked(provider: &'static str, message: impl Into<String>) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::Blocked,
            message: message.into(),
        }
    }

    fn parse(provider: &'static str, message: impl Into<String>) -> Self {
        Self {
            provider,
            kind: ProviderErrorKind::Parse,
            message: message.into(),
        }
    }

    fn retryable(&self) -> bool {
        matches!(self.kind, ProviderErrorKind::Transport)
            || matches!(
                self.kind,
                ProviderErrorKind::Http {
                    retryable: true,
                    ..
                }
            )
    }
}

impl Display for ProviderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ProviderErrorKind::Http { status, .. } => {
                write!(
                    formatter,
                    "{} HTTP {}: {}",
                    self.provider, status, self.message
                )
            }
            _ => write!(formatter, "{}: {}", self.provider, self.message),
        }
    }
}

#[async_trait]
trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError>;
}

pub struct SearchLimiter {
    in_flight: Arc<Semaphore>,
    state: Mutex<GateState>,
    min_interval: Duration,
    cooldown: Duration,
    max_cooldown: Duration,
}

struct GateState {
    last_started: Option<Instant>,
    cooldown_until: Option<Instant>,
    blocked_count: u32,
}

impl SearchLimiter {
    pub fn new(config: &WebConfig) -> Arc<Self> {
        Arc::new(Self::from_config(config))
    }

    fn from_config(config: &WebConfig) -> Self {
        Self {
            in_flight: Arc::new(Semaphore::new(1)),
            state: Mutex::new(GateState {
                last_started: None,
                cooldown_until: None,
                blocked_count: 0,
            }),
            min_interval: Duration::from_millis(config.search_min_interval_ms),
            cooldown: Duration::from_millis(config.search_cooldown_ms),
            max_cooldown: Duration::from_millis(config.search_max_cooldown_ms)
                .max(Duration::from_millis(config.search_cooldown_ms)),
        }
    }

    async fn acquire(&self) -> OwnedSemaphorePermit {
        self.in_flight
            .clone()
            .acquire_owned()
            .await
            .expect("provider gate semaphore should never be closed")
    }

    async fn wait_for_start(&self) {
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
            if let Some(wait) = wait {
                tokio::time::sleep(wait).await;
            } else {
                return;
            }
        }
    }

    fn retry_delay(&self, retry_index: u32) -> Duration {
        self.cooldown
            .saturating_mul(1_u32 << retry_index.min(16))
            .min(self.max_cooldown)
    }

    async fn record_success(&self) {
        let mut state = self.state.lock().await;
        state.blocked_count = 0;
        state.cooldown_until = None;
    }

    async fn record_challenge(&self) {
        let mut state = self.state.lock().await;
        state.blocked_count = state.blocked_count.saturating_add(1).min(16);
        state.cooldown_until = Some(Instant::now() + self.retry_delay(state.blocked_count - 1));
    }

    async fn record_failure(&self, error: &ProviderError) {
        if !matches!(error.kind, ProviderErrorKind::Blocked) {
            return;
        }
        let mut state = self.state.lock().await;
        state.blocked_count = state.blocked_count.saturating_add(1).min(16);
        state.cooldown_until = Some(Instant::now() + self.retry_delay(state.blocked_count - 1));
    }
}

type ProviderGate = SearchLimiter;

fn shared_bing_gate(config: &WebConfig) -> Arc<ProviderGate> {
    static GATE: OnceLock<Arc<ProviderGate>> = OnceLock::new();
    GATE.get_or_init(|| SearchLimiter::new(config)).clone()
}

fn shared_duckduckgo_gate(config: &WebConfig) -> Arc<ProviderGate> {
    static GATE: OnceLock<Arc<ProviderGate>> = OnceLock::new();
    GATE.get_or_init(|| SearchLimiter::new(config)).clone()
}

struct BingSearchProvider {
    client: reqwest::Client,
    endpoint: String,
    config: WebConfig,
    gate: Arc<ProviderGate>,
}

impl BingSearchProvider {
    fn new(config: WebConfig, gate: Arc<ProviderGate>) -> Self {
        let client = build_search_client(&config);
        Self::with_client(config, client, BING_SEARCH_URL, gate)
    }

    fn with_client(
        config: WebConfig,
        client: reqwest::Client,
        endpoint: impl Into<String>,
        gate: Arc<ProviderGate>,
    ) -> Self {
        Self {
            client,
            endpoint: endpoint.into(),
            config,
            gate,
        }
    }

    async fn attempt(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError> {
        let response = self
            .client
            .get(&self.endpoint)
            .query(&[
                ("q", query),
                ("setlang", "en-US"),
                ("mkt", "en-US"),
                ("format", "rss"),
            ])
            .send()
            .await
            .map_err(|error| ProviderError::transport("bing", error.to_string()))?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            ProviderError::transport("bing", format!("failed to read response: {error}"))
        })?;
        if looks_like_antibot(&body) {
            return Err(ProviderError::blocked(
                "bing",
                format!(
                    "provider returned an anti-bot challenge (HTTP {})",
                    status.as_u16()
                ),
            ));
        }
        if !status.is_success() {
            return Err(ProviderError::http(
                "bing",
                status,
                "provider rejected the request",
            ));
        }
        parse_bing_results(&body, limit)
    }
}

#[async_trait]
impl SearchProvider for BingSearchProvider {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError> {
        let _permit = self.gate.acquire().await;
        let retries = self
            .config
            .search_retry_attempts
            .min(MAX_SEARCH_RETRY_ATTEMPTS);
        for retry_index in 0..=retries {
            self.gate.wait_for_start().await;
            match self.attempt(query, limit).await {
                Ok(rows) if !rows.is_empty() => {
                    self.gate.record_success().await;
                    return Ok(rows);
                }
                Ok(_) => {
                    let error = ProviderError::empty("bing");
                    self.gate.record_failure(&error).await;
                    return Err(error);
                }
                Err(error) if error.retryable() && retry_index < retries => {
                    tracing::debug!(
                        provider = error.provider,
                        attempt = retry_index + 1,
                        max_retries = retries,
                        error = %error,
                        "retrying transient search provider failure"
                    );
                    tokio::time::sleep(self.gate.retry_delay(retry_index)).await;
                }
                Err(error) => {
                    self.gate.record_failure(&error).await;
                    return Err(error);
                }
            }
        }
        unreachable!("provider retry loop always returns")
    }
}

struct DuckDuckGoSearchProvider {
    client: reqwest::Client,
    endpoint: String,
    config: WebConfig,
    gate: Arc<ProviderGate>,
}

impl DuckDuckGoSearchProvider {
    fn new(config: WebConfig, gate: Arc<ProviderGate>) -> Self {
        let client = build_search_client(&config);
        Self::with_client(config, client, DUCKDUCKGO_SEARCH_URL, gate)
    }

    fn with_client(
        config: WebConfig,
        client: reqwest::Client,
        endpoint: impl Into<String>,
        gate: Arc<ProviderGate>,
    ) -> Self {
        Self {
            client,
            endpoint: endpoint.into(),
            config,
            gate,
        }
    }

    async fn attempt(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError> {
        let response = self
            .client
            .post(&self.endpoint)
            .form(&[
                ("q", query),
                ("b", ""),
                ("df", ""),
                ("kf", "-1"),
                ("kh", "1"),
                ("kl", "us-en"),
                ("kp", "1"),
                ("k1", "-1"),
            ])
            .header("DNT", "1")
            .send()
            .await
            .map_err(|error| ProviderError::transport("duckduckgo", error.to_string()))?;
        let status = response.status();
        let body = response.text().await.map_err(|error| {
            ProviderError::transport("duckduckgo", format!("failed to read response: {error}"))
        })?;
        if is_duckduckgo_antibot_challenge(&body) {
            return Err(ProviderError::blocked(
                "duckduckgo",
                format!(
                    "provider returned an anti-bot challenge (HTTP {})",
                    status.as_u16()
                ),
            ));
        }
        if !status.is_success() {
            return Err(ProviderError::http(
                "duckduckgo",
                status,
                "provider rejected the request",
            ));
        }
        parse_duckduckgo_results(&body, limit)
    }
}

#[async_trait]
impl SearchProvider for DuckDuckGoSearchProvider {
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError> {
        let transient_retries = self
            .config
            .search_retry_attempts
            .min(MAX_SEARCH_RETRY_ATTEMPTS);
        let challenge_retries = self
            .config
            .search_challenge_retries
            .min(MAX_SEARCH_RETRY_ATTEMPTS);
        let mut transient_retry_index = 0;
        let mut challenge_retry_index = 0;
        loop {
            let permit = self.gate.acquire().await;
            self.gate.wait_for_start().await;
            match self.attempt(query, limit).await {
                Ok(rows) if !rows.is_empty() => {
                    self.gate.record_success().await;
                    return Ok(rows);
                }
                Ok(_) => {
                    let error = ProviderError::empty("duckduckgo");
                    self.gate.record_failure(&error).await;
                    return Err(error);
                }
                Err(error) if duckduckgo_challenge_is_retryable(&error) => {
                    self.gate.record_challenge().await;
                    if challenge_retry_index >= challenge_retries {
                        drop(permit);
                        return Err(error);
                    }
                    challenge_retry_index += 1;
                    drop(permit);
                    tracing::debug!(
                        provider = error.provider,
                        attempt = challenge_retry_index,
                        max_retries = challenge_retries,
                        error = %error,
                        "retrying DuckDuckGo anti-bot challenge after shared cooldown"
                    );
                }
                Err(error) if error.retryable() && transient_retry_index < transient_retries => {
                    transient_retry_index += 1;
                    drop(permit);
                    tracing::debug!(
                        provider = error.provider,
                        attempt = transient_retry_index,
                        max_retries = transient_retries,
                        error = %error,
                        "retrying DuckDuckGo search provider failure"
                    );
                    tokio::time::sleep(self.gate.retry_delay(transient_retry_index - 1)).await;
                }
                Err(error) => {
                    drop(permit);
                    return Err(error);
                }
            }
        }
    }
}

fn build_search_client(config: &WebConfig) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs))
        .user_agent(config.user_agent.clone())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

pub struct WebSearchTool {
    config: WebConfig,
    context: Arc<ResearchContext>,
    bing: Arc<dyn SearchProvider>,
    duckduckgo: Arc<dyn SearchProvider>,
}

/// Returns whether a search error is definitive and should not be retried by
/// the agent loop as an equivalent search request.
pub(crate) fn is_non_retryable_search_failure(error: Option<&str>) -> bool {
    error.is_some_and(|error| {
        error.starts_with("web search failed:")
            || error.contains("search provider blocked the request")
            || (error.contains("duckduckgo") && error.contains("anti-bot"))
            || error == "no search results found for the query"
    })
}

impl WebSearchTool {
    pub fn new(config: WebConfig, context: Arc<ResearchContext>) -> Self {
        let bing = BingSearchProvider::new(config.clone(), shared_bing_gate(&config));
        let duckduckgo =
            DuckDuckGoSearchProvider::new(config.clone(), shared_duckduckgo_gate(&config));
        Self {
            config,
            context,
            bing: Arc::new(bing),
            duckduckgo: Arc::new(duckduckgo),
        }
    }

    pub fn with_clients_and_endpoints(
        config: WebConfig,
        context: Arc<ResearchContext>,
        bing_client: reqwest::Client,
        bing_endpoint: impl Into<String>,
        duckduckgo_client: reqwest::Client,
        duckduckgo_endpoint: impl Into<String>,
    ) -> Self {
        let bing = BingSearchProvider::with_client(
            config.clone(),
            bing_client,
            bing_endpoint,
            SearchLimiter::new(&config),
        );
        let duckduckgo = DuckDuckGoSearchProvider::with_client(
            config.clone(),
            duckduckgo_client,
            duckduckgo_endpoint,
            SearchLimiter::new(&config),
        );
        Self {
            config,
            context,
            bing: Arc::new(bing),
            duckduckgo: Arc::new(duckduckgo),
        }
    }

    /// Construct fixture clients with an explicitly shared DuckDuckGo limiter.
    pub fn with_clients_and_endpoints_and_limiter(
        config: WebConfig,
        context: Arc<ResearchContext>,
        bing_client: reqwest::Client,
        bing_endpoint: impl Into<String>,
        duckduckgo_client: reqwest::Client,
        duckduckgo_endpoint: impl Into<String>,
        duckduckgo_limiter: Arc<SearchLimiter>,
    ) -> Self {
        let bing = BingSearchProvider::with_client(
            config.clone(),
            bing_client,
            bing_endpoint,
            SearchLimiter::new(&config),
        );
        let duckduckgo = DuckDuckGoSearchProvider::with_client(
            config.clone(),
            duckduckgo_client,
            duckduckgo_endpoint,
            duckduckgo_limiter,
        );
        Self {
            config,
            context,
            bing: Arc::new(bing),
            duckduckgo: Arc::new(duckduckgo),
        }
    }

    #[doc(hidden)]
    pub fn with_clients_and_endpoints_using_process_gates(
        config: WebConfig,
        context: Arc<ResearchContext>,
        bing_client: reqwest::Client,
        bing_endpoint: impl Into<String>,
        duckduckgo_client: reqwest::Client,
        duckduckgo_endpoint: impl Into<String>,
    ) -> Self {
        let bing = BingSearchProvider::with_client(
            config.clone(),
            bing_client,
            bing_endpoint,
            shared_bing_gate(&config),
        );
        let duckduckgo = DuckDuckGoSearchProvider::with_client(
            config.clone(),
            duckduckgo_client,
            duckduckgo_endpoint,
            shared_duckduckgo_gate(&config),
        );
        Self {
            config,
            context,
            bing: Arc::new(bing),
            duckduckgo: Arc::new(duckduckgo),
        }
    }

    pub fn with_client_and_endpoint(
        config: WebConfig,
        context: Arc<ResearchContext>,
        client: reqwest::Client,
        endpoint: impl Into<String>,
    ) -> Self {
        let endpoint = endpoint.into();
        Self::with_clients_and_endpoints(
            config,
            context,
            client.clone(),
            endpoint.clone(),
            client,
            endpoint,
        )
    }
}

#[async_trait]
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
        let raw_query = call.input["query"].as_str().unwrap_or("").trim();
        let limit = call.input["limit"]
            .as_u64()
            .map(|value| value as usize)
            .unwrap_or(self.config.default_search_limit)
            .clamp(1, 10);
        if raw_query.is_empty() {
            return failure(call, "query is required");
        }

        let query = add_domain_hints(raw_query, &call.input["domains"]);
        let rows = match self.bing.search(&query, limit).await {
            Ok(rows) => rows,
            Err(bing_error) => {
                tracing::debug!(error = %bing_error, "bing search failed; trying duckduckgo fallback");
                match self.duckduckgo.search(&query, limit).await {
                    Ok(rows) => rows,
                    Err(duckduckgo_error) => {
                        tracing::warn!(
                            bing = %bing_error,
                            duckduckgo = %duckduckgo_error,
                            "all web search providers failed"
                        );
                        return failure(
                            call,
                            &combined_provider_error(&bing_error, &duckduckgo_error),
                        );
                    }
                }
            }
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

fn failure(call: &ToolCall, error: &str) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(error.into()),
    }
}

fn combined_provider_error(bing: &ProviderError, duckduckgo: &ProviderError) -> String {
    format!("web search failed: bing: {bing}; duckduckgo: {duckduckgo}")
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

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

fn looks_like_antibot(body: &str) -> bool {
    let body_lower = body.to_ascii_lowercase();
    body_lower.contains("captcha")
        || body_lower.contains("unusual traffic")
        || body_lower.contains("verify you are human")
}

#[derive(Debug, Deserialize)]
struct BingFeed {
    channel: BingChannel,
}

#[derive(Debug, Deserialize)]
struct BingChannel {
    #[serde(default)]
    item: Vec<BingItem>,
}

#[derive(Debug, Deserialize)]
struct BingItem {
    title: Option<String>,
    link: Option<String>,
    description: Option<String>,
    #[serde(rename = "pubDate")]
    published_at: Option<String>,
}

fn parse_bing_results(body: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError> {
    let feed: BingFeed = from_xml_str(body)
        .map_err(|error| ProviderError::parse("bing", format!("invalid RSS response: {error}")))?;
    let rows = feed
        .channel
        .item
        .into_iter()
        .filter_map(|item| {
            let title = item.title?.trim().to_string();
            let url = item.link?.trim().to_string();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(SearchResult {
                title,
                url,
                snippet: item.description.unwrap_or_default().trim().to_string(),
                published_at: item
                    .published_at
                    .as_deref()
                    .and_then(parse_publication_date),
            })
        })
        .take(limit)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        Err(ProviderError::empty("bing"))
    } else {
        Ok(rows)
    }
}

fn parse_duckduckgo_results(body: &str, limit: usize) -> Result<Vec<SearchResult>, ProviderError> {
    let rows = classed_divisions(body, "result")
        .into_iter()
        .take(limit)
        .filter_map(|result| {
            let title_node =
                first_html_element_with_class(&result, "a", "result__a").or_else(|| {
                    let (_, title_container) =
                        first_html_element_with_class_any_tag(&result, "result__title")?;
                    first_html_element(&title_container, "a")
                })?;
            let (title_attributes, title_body) = title_node;
            let title = html_fragment_text(&title_body);
            let url = html_attribute(&title_attributes, "href")
                .map(resolve_duckduckgo_url)
                .unwrap_or_default();
            let snippet = first_html_element_with_class_any_tag(&result, "result__snippet")
                .map(|(_, snippet_body)| html_fragment_text(&snippet_body))
                .unwrap_or_default();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let published_at = first_html_element_with_class_any_tag(&result, "result__timestamp")
                .and_then(|(_, timestamp)| parse_publication_date(&html_fragment_text(&timestamp)));
            Some(SearchResult {
                title,
                url,
                snippet,
                published_at,
            })
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        if body.to_ascii_lowercase().contains("no results found") {
            Err(ProviderError::empty("duckduckgo"))
        } else {
            Err(ProviderError::parse(
                "duckduckgo",
                "response contained no recognized structured results",
            ))
        }
    } else {
        Ok(rows)
    }
}

fn resolve_duckduckgo_url(href: String) -> String {
    let candidate = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://duckduckgo.com{href}")
    } else {
        href.clone()
    };
    reqwest::Url::parse(&candidate)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "uddg")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or(candidate)
}

fn classed_divisions(html: &str, class_name: &str) -> Vec<String> {
    html_elements_with_class_any_tag(html, class_name)
}

fn is_duckduckgo_antibot_challenge(body: &str) -> bool {
    let body_lower = body.to_ascii_lowercase();
    body_lower.contains("anomaly-modal")
        || body_lower.contains("challenge-form")
        || body_lower.contains("unfortunately, bots use duckduckgo too")
}

fn duckduckgo_challenge_is_retryable(error: &ProviderError) -> bool {
    error.provider == "duckduckgo"
        && matches!(error.kind, ProviderErrorKind::Blocked)
        && error.message.contains("HTTP 202")
}

#[cfg(test)]
mod tests {
    use super::{
        classed_divisions, parse_bing_results, parse_duckduckgo_results, resolve_duckduckgo_url,
    };

    #[test]
    fn parses_bing_rss_items_and_publication_date() {
        let rows = parse_bing_results(
            r#"<rss><channel><item><title>One</title><link>https://example.com/one</link><description>Snippet</description><pubDate>Wed, 29 Jul 2026 12:00:00 GMT</pubDate></item></channel></rss>"#,
            5,
        )
        .expect("Bing RSS result");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "One");
        assert_eq!(rows[0].url, "https://example.com/one");
        assert_eq!(rows[0].snippet, "Snippet");
        assert!(rows[0].published_at.is_some());
    }

    #[test]
    fn resolves_ddg_redirect_urls() {
        let resolved =
            resolve_duckduckgo_url("/l/?uddg=https%3A%2F%2Fexample.com%2Fstory&rut=abc".into());
        assert_eq!(resolved, "https://example.com/story");
    }

    #[test]
    fn parses_duckduckgo_result_shapes() {
        let rows = parse_duckduckgo_results(
            r#"<div class="result results_links"><h2 class="result__title"><a class="result__a" href="//example.com/one">One</a></h2><p class="result__snippet">First</p></div>"#,
            5,
        )
        .expect("DuckDuckGo result");
        assert_eq!(rows[0].url, "https://example.com/one");
        assert_eq!(rows[0].snippet, "First");
    }

    #[test]
    fn matches_only_exact_result_class_tokens() {
        let html = r#"<div class="result--ad">Ad</div><DIV CLASS = 'result other'>Hit</DIV>"#;
        assert_eq!(classed_divisions(html, "result"), vec!["Hit"]);
    }
}
