use async_trait::async_trait;
use chrono::Utc;
use nca_common::config::{PermissionConfig, WebConfig};
use nca_common::event::AgentEvent;
use nca_common::message::Message;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use nca_core::agent::AgentLoop;
use nca_core::approval::ApprovalPolicy;
use nca_core::provider::{Provider, ProviderError, StreamChunk};
use nca_core::research::ResearchContext;
use nca_core::tools::web_search::{SearchLimiter, WebSearchTool};
use nca_core::tools::{ToolExecutor, ToolRegistry};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tiny_http::{Response, Server, StatusCode};

fn call(input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "fixture-search".into(),
        name: "web_search".into(),
        input,
    }
}

fn fixture_tool(
    body: &'static str,
    status: u16,
) -> (
    WebSearchTool,
    Arc<ResearchContext>,
    std::thread::JoinHandle<()>,
) {
    let server = Server::http("127.0.0.1:0").expect("start fixture server");
    let address = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => address,
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let handle = std::thread::spawn(move || {
        let request = server.recv().expect("fixture request");
        request
            .respond(
                Response::from_string(body)
                    .with_status_code(StatusCode(status))
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "text/html")
                            .expect("content type header"),
                    ),
            )
            .expect("fixture response");
    });
    let context = Arc::new(ResearchContext::new(Utc::now().date_naive()));
    let client = reqwest::Client::builder().build().expect("fixture client");
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 0,
        search_max_cooldown_ms: 0,
        search_challenge_retries: 0,
        ..WebConfig::default()
    };
    let tool = WebSearchTool::with_client_and_endpoint(
        config,
        context.clone(),
        client,
        format!("http://{address}/html/"),
    );
    (tool, context, handle)
}

const FIXTURE_RESULTS: &str = r#"
    <div class="result">
      <a class="result__a" href="https://example.com/result">Fixture result</a>
      <p class="result__snippet">A deterministic fixture result.</p>
    </div>
"#;
const FIXTURE_CHALLENGE: &str =
    r#"<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>"#;

struct PlannedResponse {
    body: &'static str,
    status: u16,
    delay: Duration,
}

#[derive(Default)]
struct FixtureLog {
    request_starts: Vec<Instant>,
    response_statuses: Vec<u16>,
    request_completions: Vec<Instant>,
    active_requests: usize,
    max_in_flight: usize,
}

struct SearchFixture {
    endpoint: String,
    log: Arc<Mutex<FixtureLog>>,
    handle: std::thread::JoinHandle<()>,
}

fn search_fixture(responses: Vec<PlannedResponse>) -> SearchFixture {
    let server = Server::http("127.0.0.1:0").expect("start search fixture");
    let address = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => address,
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let log = Arc::new(Mutex::new(FixtureLog::default()));
    let thread_log = Arc::clone(&log);
    let handle = std::thread::spawn(move || {
        let mut workers = Vec::with_capacity(responses.len());
        for planned in responses {
            let request = server.recv().expect("fixture request");
            let worker_log = Arc::clone(&thread_log);
            workers.push(std::thread::spawn(move || {
                {
                    let mut log = worker_log.lock().expect("fixture log");
                    log.request_starts.push(Instant::now());
                    log.active_requests += 1;
                    let active_requests = log.active_requests;
                    log.max_in_flight = log.max_in_flight.max(active_requests);
                }
                std::thread::sleep(planned.delay);
                request
                    .respond(
                        Response::from_string(planned.body)
                            .with_status_code(StatusCode(planned.status))
                            .with_header(
                                tiny_http::Header::from_bytes("Content-Type", "text/html")
                                    .expect("content type header"),
                            ),
                    )
                    .expect("fixture response");
                let mut log = worker_log.lock().expect("fixture log");
                log.response_statuses.push(planned.status);
                log.request_completions.push(Instant::now());
                log.active_requests -= 1;
            }));
        }
        for worker in workers {
            worker.join().expect("fixture worker");
        }
    });
    SearchFixture {
        endpoint: format!("http://{address}/html/"),
        log,
        handle,
    }
}

fn fixture_tool_with_config(
    fixture: &SearchFixture,
    config: WebConfig,
    limiter: Arc<SearchLimiter>,
) -> WebSearchTool {
    let context = Arc::new(ResearchContext::new(Utc::now().date_naive()));
    let client = reqwest::Client::builder().build().expect("fixture client");
    WebSearchTool::with_client_and_endpoint_and_limiter(
        config,
        context,
        client,
        fixture.endpoint.clone(),
        limiter,
    )
}

fn fixture_log(log: &Arc<Mutex<FixtureLog>>) -> MutexGuard<'_, FixtureLog> {
    log.lock().expect("fixture log")
}

struct SingleSearchCallProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl Provider for SingleSearchCallProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_number > 0 {
            return Err(ProviderError::Other(
                "provider was called again after a definitive search failure".into(),
            ));
        }

        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(StreamChunk::ToolUse(ToolCall {
            id: "search-1".into(),
            name: "web_search".into(),
            input: serde_json::json!({"query": "blocked query"}),
        }))
        .await
        .expect("test provider receiver is alive");
        tx.send(StreamChunk::Done)
            .await
            .expect("test provider receiver is alive");
        Ok(rx)
    }
}

struct BlockedSearchTool;

#[async_trait]
impl ToolExecutor for BlockedSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".into(),
            description: "test search".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(
                "search provider blocked the request (HTTP 202): DuckDuckGo returned an anti-bot challenge"
                    .into(),
            ),
        }
    }
}

#[tokio::test]
async fn definitive_search_block_stops_before_equivalent_retry() {
    let provider = SingleSearchCallProvider {
        calls: AtomicUsize::new(0),
    };
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(BlockedSearchTool));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
    let mut agent = AgentLoop::new(
        Box::new(provider),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        4,
        4,
        1,
        None,
    );

    let output = agent
        .run_turn("search", Path::new("."), &[])
        .await
        .expect("definitive search failure should finish the turn");

    assert!(output.contains("search provider blocked the request (HTTP 202)"));
    assert!(output.contains("try a different search provider or adjust the query"));
    let events = std::iter::from_fn(|| event_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::Error { message } if message.contains("Search stopped:"))
    }));
}

struct RepeatingToolProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for RepeatingToolProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(StreamChunk::ToolUse(ToolCall {
            id: format!("search-{call_number}"),
            name: "web_search".into(),
            input: serde_json::json!({"query": "temporary failure"}),
        }))
        .await
        .expect("test provider receiver is alive");
        tx.send(StreamChunk::Done)
            .await
            .expect("test provider receiver is alive");
        Ok(rx)
    }
}

struct TransientSearchTool;

#[async_trait]
impl ToolExecutor for TransientSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".into(),
            description: "test search".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some("search request failed: connection reset".into()),
        }
    }
}

#[tokio::test]
async fn transient_search_failure_keeps_the_existing_retry_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = RepeatingToolProvider {
        calls: calls.clone(),
    };
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(TransientSearchTool));
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(32);
    let mut agent = AgentLoop::new(
        Box::new(provider),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        4,
        4,
        1,
        None,
    );

    let output = agent
        .run_turn("search", Path::new("."), &[])
        .await
        .expect("retry limit should finish the turn");

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(output.contains("Tool `web_search` failed 3 times consecutively"));
}

#[tokio::test]
async fn challenge_fixture_reports_upstream_blocking_with_status() {
    let (tool, _context, server) = fixture_tool(
        r#"<div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>"#,
        202,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "blocked"})))
        .await;
    server.join().expect("fixture thread");

    assert!(!result.success);
    let error = result.error.expect("blocking error");
    assert!(error.contains("blocked the request (HTTP 202)"));
    assert!(!error.contains("no structured search results parsed"));
}

#[tokio::test]
async fn empty_results_fixture_reports_no_results() {
    let (tool, _context, server) = fixture_tool(
        r#"<div class="no-results"><div class="no-results__title">No results found</div></div>"#,
        200,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "nothing"})))
        .await;
    server.join().expect("fixture thread");

    assert!(!result.success);
    assert_eq!(
        result.error.as_deref(),
        Some("no search results found for the query")
    );
}

#[tokio::test]
async fn malformed_results_fixture_reports_parser_failure() {
    let (tool, _context, server) = fixture_tool(
        "<html><body><div class=\"unexpected-layout\">Not results</div></body></html>",
        200,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "malformed"})))
        .await;
    server.join().expect("fixture thread");

    assert!(!result.success);
    let error = result.error.expect("parser error");
    assert!(error.contains("no recognized structured results"));
    assert!(!error.contains("blocked the request"));
    assert!(!error.contains("no search results found"));
}

#[tokio::test]
async fn structured_results_fixture_returns_json_and_records_evidence() {
    let body = r#"
        <div class="result results_links">
          <h2 class="result__title"><a class="result__a" href="https://example.com/report">Example report</a></h2>
          <a class="result__snippet">A structured search result.</a>
        </div>
    "#;
    let (tool, context, server) = fixture_tool(body, 200);
    let result = tool
        .execute(&call(serde_json::json!({"query": "report", "limit": 1})))
        .await;
    server.join().expect("fixture thread");

    assert!(result.success, "{result:?}");
    let output: serde_json::Value = serde_json::from_str(&result.output).expect("search JSON");
    assert_eq!(output["results"][0]["title"], "Example report");
    assert_eq!(output["results"][0]["url"], "https://example.com/report");
    let evidence = context.evidence();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].url, "https://example.com/report");
}

#[tokio::test]
async fn concurrent_searches_are_serialized_and_paced_across_sessions() {
    let fixture = search_fixture(vec![
        PlannedResponse {
            body: FIXTURE_RESULTS,
            status: 200,
            delay: Duration::from_millis(20),
        },
        PlannedResponse {
            body: FIXTURE_RESULTS,
            status: 200,
            delay: Duration::from_millis(20),
        },
    ]);
    let config = WebConfig {
        search_min_interval_ms: 35,
        search_challenge_retries: 0,
        ..WebConfig::default()
    };
    let limiter = SearchLimiter::new(&config);
    let first_tool = fixture_tool_with_config(&fixture, config.clone(), Arc::clone(&limiter));
    let second_tool = fixture_tool_with_config(&fixture, config, limiter);
    let first_call = call(serde_json::json!({"query": "first"}));
    let second_call = call(serde_json::json!({"query": "second"}));

    let (first, second) = tokio::join!(
        first_tool.execute(&first_call),
        second_tool.execute(&second_call)
    );
    fixture.handle.join().expect("fixture thread");

    assert!(first.success, "{first:?}");
    assert!(second.success, "{second:?}");
    let log = fixture_log(&fixture.log);
    assert_eq!(log.request_starts.len(), 2);
    assert_eq!(log.max_in_flight, 1);
    assert!(
        log.request_starts[1].duration_since(log.request_starts[0]) >= Duration::from_millis(30)
    );
}

#[tokio::test]
async fn http_202_cooldown_blocks_follow_up_and_allows_recovery() {
    let fixture = search_fixture(vec![
        PlannedResponse {
            body: FIXTURE_CHALLENGE,
            status: 202,
            delay: Duration::ZERO,
        },
        PlannedResponse {
            body: FIXTURE_RESULTS,
            status: 200,
            delay: Duration::ZERO,
        },
    ]);
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 45,
        search_max_cooldown_ms: 45,
        search_challenge_retries: 0,
        ..WebConfig::default()
    };
    let tool = fixture_tool_with_config(&fixture, config.clone(), SearchLimiter::new(&config));

    let blocked = tool
        .execute(&call(serde_json::json!({"query": "blocked"})))
        .await;
    let recovered = tool
        .execute(&call(serde_json::json!({"query": "recovery"})))
        .await;
    fixture.handle.join().expect("fixture thread");

    assert!(!blocked.success, "{blocked:?}");
    assert!(
        blocked
            .error
            .as_deref()
            .is_some_and(|error| error.contains("blocked the request (HTTP 202)"))
    );
    assert!(recovered.success, "{recovered:?}");
    let log = fixture_log(&fixture.log);
    assert_eq!(log.max_in_flight, 1);
    assert_eq!(log.response_statuses, vec![202, 200]);
    assert_eq!(log.request_completions.len(), 2);
    assert!(
        log.request_starts[1].duration_since(log.request_starts[0]) >= Duration::from_millis(35)
    );
}

#[tokio::test]
async fn repeated_202_challenges_use_bounded_backoff_and_retry_to_recovery() {
    let fixture = search_fixture(vec![
        PlannedResponse {
            body: FIXTURE_CHALLENGE,
            status: 202,
            delay: Duration::ZERO,
        },
        PlannedResponse {
            body: FIXTURE_CHALLENGE,
            status: 202,
            delay: Duration::ZERO,
        },
        PlannedResponse {
            body: FIXTURE_RESULTS,
            status: 200,
            delay: Duration::ZERO,
        },
    ]);
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 8,
        search_max_cooldown_ms: 20,
        search_challenge_retries: 2,
        ..WebConfig::default()
    };
    let tool = fixture_tool_with_config(&fixture, config.clone(), SearchLimiter::new(&config));

    let result = tool
        .execute(&call(serde_json::json!({"query": "retry"})))
        .await;
    fixture.handle.join().expect("fixture thread");

    assert!(result.success, "{result:?}");
    let log = fixture_log(&fixture.log);
    assert_eq!(log.request_starts.len(), 3);
    assert!(
        log.request_starts[1].duration_since(log.request_starts[0]) >= Duration::from_millis(6)
    );
    assert!(
        log.request_starts[2].duration_since(log.request_starts[1]) >= Duration::from_millis(14)
    );
}

#[tokio::test]
async fn default_three_challenge_retries_exhaust_as_an_explicit_failure() {
    let fixture = search_fixture(
        (0..4)
            .map(|_| PlannedResponse {
                body: FIXTURE_CHALLENGE,
                status: 202,
                delay: Duration::ZERO,
            })
            .collect(),
    );
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 2,
        search_max_cooldown_ms: 8,
        ..WebConfig::default()
    };
    assert_eq!(config.search_challenge_retries, 3);
    let tool = fixture_tool_with_config(&fixture, config.clone(), SearchLimiter::new(&config));

    let result = tool
        .execute(&call(serde_json::json!({"query": "exhaust"})))
        .await;
    fixture.handle.join().expect("fixture thread");

    assert!(!result.success, "{result:?}");
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("blocked the request (HTTP 202)"))
    );
    assert_eq!(fixture_log(&fixture.log).request_starts.len(), 4);
}

#[tokio::test]
async fn unrelated_tools_do_not_wait_for_the_duckduckgo_limiter() {
    let fixture = search_fixture(vec![PlannedResponse {
        body: FIXTURE_RESULTS,
        status: 200,
        delay: Duration::from_millis(80),
    }]);
    let config = WebConfig {
        search_min_interval_ms: 100,
        search_challenge_retries: 0,
        ..WebConfig::default()
    };
    let tool = fixture_tool_with_config(&fixture, config.clone(), SearchLimiter::new(&config));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(tool));
    registry.register(Box::new(FastFixtureTool));

    let search_call = call(serde_json::json!({"query": "slow"}));
    let fast_call = ToolCall {
        id: "fast".into(),
        name: "fast_fixture".into(),
        input: serde_json::json!({}),
    };
    let search = registry.execute(&search_call);
    let fast = tokio::time::timeout(Duration::from_millis(35), registry.execute(&fast_call));
    let (search, fast) = tokio::join!(search, fast);
    fixture.handle.join().expect("fixture thread");

    assert!(search.success, "{search:?}");
    assert!(fast.expect("unrelated tool timed out").success);
}

struct FastFixtureTool;

#[async_trait]
impl ToolExecutor for FastFixtureTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fast_fixture".into(),
            description: "fixture tool unrelated to web search".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: "fast".into(),
            error: None,
        }
    }
}
