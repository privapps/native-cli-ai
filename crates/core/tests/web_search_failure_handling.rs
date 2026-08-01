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
use nca_core::tools::web_search::{ManualSearchClock, SearchLimiter, WebSearchTool};
use nca_core::tools::{ToolExecutor, ToolRegistry};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tiny_http::{Response, Server, StatusCode};
use tokio::sync::Notify;

const BING_RESULT: &str = r#"<rss><channel><item><title>Bing result</title><link>https://example.com/bing</link><description>Bing snippet</description><pubDate>Wed, 29 Jul 2026 12:00:00 GMT</pubDate></item></channel></rss>"#;
const DDG_RESULT: &str = r#"<div class="result results_links"><h2 class="result__title"><a class="result__a" href="/l/?uddg=https%3A%2F%2Fexample.com%2Fddg">DDG result</a></h2><p class="result__snippet">DDG snippet</p></div>"#;

fn call(input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "fixture-search".into(),
        name: "web_search".into(),
        input,
    }
}

#[derive(Clone)]
struct FixtureResponse {
    body: &'static str,
    status: u16,
    delay: Duration,
}

#[derive(Debug, Default)]
struct RequestRecord {
    method: String,
    url: String,
    body: String,
    user_agent: Option<String>,
}

#[derive(Debug, Default)]
struct RequestLog {
    requests: Vec<RequestRecord>,
    max_in_flight: usize,
    request_starts: Vec<Instant>,
    response_statuses: Vec<u16>,
    request_completions: Vec<Instant>,
}

struct FixtureServer {
    endpoint: String,
    log: Arc<Mutex<RequestLog>>,
    request_notify: Arc<Notify>,
    completion_notify: Arc<Notify>,
    handle: JoinHandle<()>,
}

fn fixture_server(responses: Vec<FixtureResponse>) -> FixtureServer {
    let server = Server::http("127.0.0.1:0").expect("start fixture server");
    let address = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => address,
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let response_count = responses.len();
    let queued = Arc::new(Mutex::new(VecDeque::from(responses)));
    let log = Arc::new(Mutex::new(RequestLog::default()));
    let request_notify = Arc::new(Notify::new());
    let completion_notify = Arc::new(Notify::new());
    let active = Arc::new(AtomicUsize::new(0));
    let recorded_log = log.clone();
    let recorded_active = active.clone();
    let recorded_request_notify = request_notify.clone();
    let recorded_completion_notify = completion_notify.clone();
    let handle = std::thread::spawn(move || {
        let mut response_threads = Vec::new();
        for _ in 0..response_count {
            let mut request = server.recv().expect("fixture request");
            let mut body = String::new();
            request
                .as_reader()
                .read_to_string(&mut body)
                .expect("read fixture request body");
            let user_agent = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("User-Agent"))
                .map(|header| header.value.to_string());
            let record = RequestRecord {
                method: request.method().to_string(),
                url: request.url().to_string(),
                body,
                user_agent,
            };
            let response = queued
                .lock()
                .expect("fixture response queue")
                .pop_front()
                .expect("fixture response");
            let in_flight = recorded_active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut log = recorded_log.lock().expect("fixture request log");
            log.max_in_flight = log.max_in_flight.max(in_flight);
            log.request_starts.push(Instant::now());
            log.response_statuses.push(response.status);
            log.requests.push(record);
            drop(log);
            recorded_request_notify.notify_one();
            let active_for_response = recorded_active.clone();
            let completed_log = recorded_log.clone();
            let completed_notify = recorded_completion_notify.clone();
            response_threads.push(std::thread::spawn(move || {
                std::thread::sleep(response.delay);
                request
                    .respond(
                        Response::from_string(response.body)
                            .with_status_code(StatusCode(response.status))
                            .with_header(
                                tiny_http::Header::from_bytes("Content-Type", "text/plain")
                                    .expect("content type header"),
                            ),
                    )
                    .expect("fixture response");
                active_for_response.fetch_sub(1, Ordering::SeqCst);
                completed_log
                    .lock()
                    .expect("fixture request log")
                    .request_completions
                    .push(Instant::now());
                completed_notify.notify_one();
            }));
        }
        for response_thread in response_threads {
            response_thread.join().expect("fixture response thread");
        }
    });
    FixtureServer {
        endpoint: format!("http://{address}/search"),
        log,
        request_notify,
        completion_notify,
        handle,
    }
}

fn tool_with_servers(
    bing: Vec<FixtureResponse>,
    duckduckgo: Vec<FixtureResponse>,
    retry_attempts: u32,
) -> (WebSearchTool, FixtureServer, FixtureServer) {
    let (tool, _context, bing_server, duckduckgo_server) =
        tool_with_servers_and_context(bing, duckduckgo, retry_attempts);
    (tool, bing_server, duckduckgo_server)
}

fn tool_with_servers_and_manual_clock(
    bing: Vec<FixtureResponse>,
    duckduckgo: Vec<FixtureResponse>,
    retry_attempts: u32,
) -> (
    WebSearchTool,
    FixtureServer,
    FixtureServer,
    Arc<ManualSearchClock>,
) {
    let bing_server = fixture_server(bing);
    let duckduckgo_server = fixture_server(duckduckgo);
    let clock = ManualSearchClock::new();
    let context = Arc::new(ResearchContext::new(Utc::now().date_naive()));
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 5,
        search_max_cooldown_ms: 20,
        search_retry_attempts: retry_attempts,
        ..WebConfig::default()
    };
    let client = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .build()
        .expect("fixture client");
    let limiter = SearchLimiter::new_with_clock(&config, clock.clone());
    let tool = WebSearchTool::with_clients_and_endpoints_and_limiter(
        config,
        context,
        client.clone(),
        bing_server.endpoint.clone(),
        client,
        duckduckgo_server.endpoint.clone(),
        limiter,
    );
    (tool, bing_server, duckduckgo_server, clock)
}

fn tool_with_servers_and_context(
    bing: Vec<FixtureResponse>,
    duckduckgo: Vec<FixtureResponse>,
    retry_attempts: u32,
) -> (
    WebSearchTool,
    Arc<ResearchContext>,
    FixtureServer,
    FixtureServer,
) {
    let bing_server = fixture_server(bing);
    let duckduckgo_server = fixture_server(duckduckgo);
    let context = Arc::new(ResearchContext::new(Utc::now().date_naive()));
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 5,
        search_max_cooldown_ms: 20,
        search_retry_attempts: retry_attempts,
        ..WebConfig::default()
    };
    let client = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .build()
        .expect("fixture client");
    let tool = WebSearchTool::with_clients_and_endpoints(
        config,
        context.clone(),
        client.clone(),
        bing_server.endpoint.clone(),
        client,
        duckduckgo_server.endpoint.clone(),
    );
    (tool, context, bing_server, duckduckgo_server)
}

fn response(body: &'static str, status: u16) -> FixtureResponse {
    FixtureResponse {
        body,
        status,
        delay: Duration::ZERO,
    }
}

#[tokio::test]
async fn bing_success_preserves_public_output_and_skips_fallback() {
    let (tool, bing, duckduckgo) = tool_with_servers(vec![response(BING_RESULT, 200)], vec![], 1);
    let result = tool
        .execute(&call(serde_json::json!({"query": "report", "limit": 1})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    let output: serde_json::Value = serde_json::from_str(&result.output).expect("search JSON");
    assert_eq!(output["results"][0]["title"], "Bing result");
    assert_eq!(output["results"][0]["url"], "https://example.com/bing");
    let bing_log = bing.log.lock().expect("Bing log");
    assert_eq!(bing_log.requests.len(), 1);
    assert!(bing_log.requests[0].url.contains("q=report"));
    assert!(bing_log.requests[0].url.contains("format=rss"));
    assert!(
        bing_log.requests[0]
            .user_agent
            .as_deref()
            .is_some_and(|agent| agent == "nca/0.5 (+https://github.com/user/native-cli-ai)")
    );
    assert!(duckduckgo.log.lock().expect("DDG log").requests.is_empty());
}

#[tokio::test]
async fn malformed_bing_response_falls_back_to_duckduckgo() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("<html>not RSS</html>", 200)],
        vec![response(DDG_RESULT, 200)],
        1,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "fallback"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    let output: serde_json::Value = serde_json::from_str(&result.output).expect("search JSON");
    assert_eq!(output["results"][0]["url"], "https://example.com/ddg");
    let request = &duckduckgo.log.lock().expect("DDG log").requests[0];
    assert_eq!(request.method, "POST");
    assert!(request.body.contains("q=fallback"));
    assert!(request.body.contains("kl=us-en"));
}

#[tokio::test]
async fn empty_bing_response_falls_back_to_duckduckgo() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("<rss><channel></channel></rss>", 200)],
        vec![response(DDG_RESULT, 200)],
        1,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "empty"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    assert_eq!(bing.log.lock().expect("Bing log").requests.len(), 1);
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 1);
}

#[tokio::test]
async fn non_retryable_bing_http_failure_falls_back_immediately() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("bad request", 400)],
        vec![response(DDG_RESULT, 200)],
        5,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "bad request"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    assert_eq!(bing.log.lock().expect("Bing log").requests.len(), 1);
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 1);
}

#[tokio::test]
async fn retryable_bing_failure_is_retried_inside_search_tool() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("temporary", 599), response(BING_RESULT, 200)],
        vec![],
        1,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "retry"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    assert_eq!(bing.log.lock().expect("Bing log").requests.len(), 2);
}

#[tokio::test]
async fn concurrent_bing_requests_are_not_serialized_by_search_limiting() {
    let mut first = response(BING_RESULT, 200);
    first.delay = Duration::from_millis(100);
    let mut second = response(BING_RESULT, 200);
    second.delay = Duration::from_millis(100);
    let (tool, bing, duckduckgo) = tool_with_servers(vec![first, second], vec![], 0);
    let first_call = call(serde_json::json!({"query": "one"}));
    let second_call = call(serde_json::json!({"query": "two"}));
    let (first_result, second_result) =
        tokio::join!(tool.execute(&first_call), tool.execute(&second_call));
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(first_result.success, "{first_result:?}");
    assert!(second_result.success, "{second_result:?}");
    assert_eq!(bing.log.lock().expect("Bing log").max_in_flight, 2);
}

#[tokio::test]
async fn concurrent_process_tools_do_not_share_a_bing_gate() {
    let mut first = response(BING_RESULT, 200);
    first.delay = Duration::from_millis(100);
    let mut second = response(BING_RESULT, 200);
    second.delay = Duration::from_millis(100);
    let bing = fixture_server(vec![first, second]);
    let duckduckgo = fixture_server(vec![]);
    let context_a = Arc::new(ResearchContext::new(Utc::now().date_naive()));
    let context_b = Arc::new(ResearchContext::new(Utc::now().date_naive()));
    let config = WebConfig {
        search_min_interval_ms: 0,
        search_cooldown_ms: 1,
        search_max_cooldown_ms: 5,
        search_retry_attempts: 0,
        ..WebConfig::default()
    };
    let client_a = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .build()
        .expect("client A");
    let client_b = client_a.clone();
    let tool_a = WebSearchTool::with_clients_and_endpoints_using_process_gates(
        config.clone(),
        context_a,
        client_a,
        bing.endpoint.clone(),
        reqwest::Client::new(),
        duckduckgo.endpoint.clone(),
    );
    let tool_b = WebSearchTool::with_clients_and_endpoints_using_process_gates(
        config,
        context_b,
        client_b,
        bing.endpoint.clone(),
        reqwest::Client::new(),
        duckduckgo.endpoint.clone(),
    );
    let first_call = call(serde_json::json!({"query": "process-one"}));
    let second_call = call(serde_json::json!({"query": "process-two"}));
    let (first_result, second_result) =
        tokio::join!(tool_a.execute(&first_call), tool_b.execute(&second_call));
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(first_result.success, "{first_result:?}");
    assert!(second_result.success, "{second_result:?}");
    assert_eq!(bing.log.lock().expect("Bing log").max_in_flight, 2);
}

#[tokio::test]
async fn concurrent_process_tools_share_the_duckduckgo_gate() {
    let bing = fixture_server(vec![
        response("<html>not RSS</html>", 200),
        response("<html>not RSS</html>", 200),
    ]);
    let mut first = response(DDG_RESULT, 200);
    first.delay = Duration::from_millis(15);
    let mut second = response(DDG_RESULT, 200);
    second.delay = Duration::from_millis(15);
    let duckduckgo = fixture_server(vec![first, second]);
    let config = WebConfig {
        search_min_interval_ms: 20,
        search_cooldown_ms: 0,
        search_max_cooldown_ms: 0,
        search_challenge_retries: 0,
        search_retry_attempts: 0,
        ..WebConfig::default()
    };
    let client = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .build()
        .expect("fixture client");
    let tool_a = WebSearchTool::with_clients_and_endpoints_using_process_gates(
        config.clone(),
        Arc::new(ResearchContext::new(Utc::now().date_naive())),
        client.clone(),
        bing.endpoint.clone(),
        client.clone(),
        duckduckgo.endpoint.clone(),
    );
    let tool_b = WebSearchTool::with_clients_and_endpoints_using_process_gates(
        config,
        Arc::new(ResearchContext::new(Utc::now().date_naive())),
        client.clone(),
        bing.endpoint.clone(),
        client,
        duckduckgo.endpoint.clone(),
    );
    let first_call = call(serde_json::json!({"query": "process-a"}));
    let second_call = call(serde_json::json!({"query": "process-b"}));
    let (first_result, second_result) =
        tokio::join!(tool_a.execute(&first_call), tool_b.execute(&second_call));
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(first_result.success, "{first_result:?}");
    assert!(second_result.success, "{second_result:?}");
    let log = duckduckgo.log.lock().expect("DDG log");
    assert_eq!(log.max_in_flight, 1);
    assert!(
        log.request_starts[1].duration_since(log.request_starts[0]) >= Duration::from_millis(10)
    );
}

#[tokio::test]
async fn anti_bot_bing_failure_is_not_retried_and_uses_fallback() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("captcha verify you are human", 202)],
        vec![response(DDG_RESULT, 200)],
        5,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "blocked"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    assert_eq!(bing.log.lock().expect("Bing log").requests.len(), 1);
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 1);
}

#[tokio::test]
async fn duckduckgo_retry_is_local_to_the_fallback_provider() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("<html>not RSS</html>", 200)],
        vec![response("temporary", 599), response(DDG_RESULT, 200)],
        1,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "ddg retry"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 2);
}

#[tokio::test]
async fn duckduckgo_http_202_challenge_retries_and_recovers() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("<html>not RSS</html>", 200)],
        vec![
            response(
                "<div class=\"anomaly-modal__title\">Unfortunately, bots use DuckDuckGo too.</div>",
                202,
            ),
            response(DDG_RESULT, 200),
        ],
        5,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "ddg blocked"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 2);
}

#[tokio::test]
async fn duckduckgo_challenge_records_status_and_cooldown_before_retry() {
    let (tool, bing, duckduckgo, clock) = tool_with_servers_and_manual_clock(
        vec![response("<html>not RSS</html>", 200)],
        vec![
            response(
                "<div class=\"anomaly-modal__title\">Unfortunately, bots use DuckDuckGo too.</div>",
                202,
            ),
            response(DDG_RESULT, 200),
        ],
        0,
    );
    let tool = Arc::new(tool);
    let search_call = call(serde_json::json!({"query": "cooldown"}));
    let search = tokio::spawn({
        let tool = Arc::clone(&tool);
        async move { tool.execute(&search_call).await }
    });
    duckduckgo.request_notify.notified().await;
    duckduckgo.completion_notify.notified().await;
    clock.wait_for_sleep().await;
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 1);
    let retry_request = duckduckgo.request_notify.notified();
    clock.advance(Duration::from_millis(5));
    retry_request.await;
    let result = search.await.expect("search task");
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    let log = duckduckgo.log.lock().expect("DDG log");
    assert_eq!(log.response_statuses, vec![202, 200]);
    assert_eq!(log.request_completions.len(), 2);
}

#[tokio::test]
async fn repeated_duckduckgo_challenges_use_bounded_shared_backoff() {
    let challenge = response(
        "<div class=\"anomaly-modal__title\">Unfortunately, bots use DuckDuckGo too.</div>",
        202,
    );
    let (tool, bing, duckduckgo, clock) = tool_with_servers_and_manual_clock(
        vec![response("<html>not RSS</html>", 200)],
        vec![
            challenge.clone(),
            challenge.clone(),
            challenge,
            response(DDG_RESULT, 200),
        ],
        0,
    );
    let tool = Arc::new(tool);
    let search_call = call(serde_json::json!({"query": "backoff"}));
    let mut next_request = duckduckgo.request_notify.notified();
    let search = tokio::spawn({
        let tool = Arc::clone(&tool);
        async move { tool.execute(&search_call).await }
    });
    for delay in [5, 10, 20] {
        next_request.await;
        duckduckgo.completion_notify.notified().await;
        clock.wait_for_sleep().await;
        next_request = duckduckgo.request_notify.notified();
        clock.advance(Duration::from_millis(delay));
    }
    next_request.await;
    let result = search.await.expect("search task");
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    let log = duckduckgo.log.lock().expect("DDG log");
    assert_eq!(log.response_statuses, vec![202, 202, 202, 200]);
}

#[tokio::test]
async fn duckduckgo_http_202_challenge_exhaustion_is_explicit() {
    let challenge = response(
        "<div class=\"anomaly-modal__title\">Unfortunately, bots use DuckDuckGo too.</div>",
        202,
    );
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![response("<html>not RSS</html>", 200)],
        vec![
            challenge.clone(),
            challenge.clone(),
            challenge.clone(),
            challenge,
        ],
        5,
    );
    let result = tool
        .execute(&call(serde_json::json!({"query": "ddg blocked"})))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(!result.success, "{result:?}");
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|error| { error.contains("duckduckgo") && error.contains("anti-bot") })
    );
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 4);
}

#[tokio::test]
async fn concurrent_fallbacks_share_one_duckduckgo_slot() {
    let mut first = response(DDG_RESULT, 200);
    first.delay = Duration::from_millis(100);
    let mut second = response(DDG_RESULT, 200);
    second.delay = Duration::from_millis(100);
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![
            response("<html>not RSS</html>", 200),
            response("<html>not RSS</html>", 200),
        ],
        vec![first, second],
        0,
    );
    let first_call = call(serde_json::json!({"query": "fallback-one"}));
    let second_call = call(serde_json::json!({"query": "fallback-two"}));
    let (first_result, second_result) =
        tokio::join!(tool.execute(&first_call), tool.execute(&second_call));
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(first_result.success, "{first_result:?}");
    assert!(second_result.success, "{second_result:?}");
    assert_eq!(duckduckgo.log.lock().expect("DDG log").max_in_flight, 1);
}

#[tokio::test]
async fn independent_sessions_share_duckduckgo_limiter_and_request_pacing() {
    let bing = fixture_server(vec![
        response("<html>not RSS</html>", 200),
        response("<html>not RSS</html>", 200),
    ]);
    let first = response(DDG_RESULT, 200);
    let second = response(DDG_RESULT, 200);
    let duckduckgo = fixture_server(vec![first, second]);
    let config = WebConfig {
        search_min_interval_ms: 20,
        search_cooldown_ms: 0,
        search_max_cooldown_ms: 0,
        search_challenge_retries: 0,
        search_retry_attempts: 0,
        ..WebConfig::default()
    };
    let clock = ManualSearchClock::new();
    let limiter = SearchLimiter::new_with_clock(&config, clock.clone());
    let client = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .build()
        .expect("fixture client");
    let tool_a = WebSearchTool::with_clients_and_endpoints_and_limiter(
        config.clone(),
        Arc::new(ResearchContext::new(Utc::now().date_naive())),
        client.clone(),
        bing.endpoint.clone(),
        client.clone(),
        duckduckgo.endpoint.clone(),
        Arc::clone(&limiter),
    );
    let tool_b = WebSearchTool::with_clients_and_endpoints_and_limiter(
        config,
        Arc::new(ResearchContext::new(Utc::now().date_naive())),
        client.clone(),
        bing.endpoint.clone(),
        client,
        duckduckgo.endpoint.clone(),
        limiter,
    );
    let first_call = call(serde_json::json!({"query": "session-a"}));
    let second_call = call(serde_json::json!({"query": "session-b"}));
    let first_search = tokio::spawn({
        let tool_a = Arc::new(tool_a);
        async move { tool_a.execute(&first_call).await }
    });
    let second_search = tokio::spawn({
        let tool_b = Arc::new(tool_b);
        async move { tool_b.execute(&second_call).await }
    });
    duckduckgo.request_notify.notified().await;
    assert_eq!(duckduckgo.log.lock().expect("DDG log").requests.len(), 1);
    let second_request = duckduckgo.request_notify.notified();
    clock.advance(Duration::from_millis(20));
    second_request.await;
    let first_result = first_search.await.expect("first search task");
    let second_result = second_search.await.expect("second search task");
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(first_result.success, "{first_result:?}");
    assert!(second_result.success, "{second_result:?}");
    let log = duckduckgo.log.lock().expect("DDG log");
    assert_eq!(log.response_statuses, vec![200, 200]);
    assert_eq!(log.max_in_flight, 1);
}

#[tokio::test]
async fn fallback_results_record_evidence_and_metadata() {
    let (tool, context, bing, duckduckgo) = tool_with_servers_and_context(
        vec![response("<html>not RSS</html>", 200)],
        vec![response(DDG_RESULT, 200)],
        0,
    );
    let result = tool
        .execute(&call(serde_json::json!({
            "query": "financial report",
            "issuer": "Example"
        })))
        .await;
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert!(result.success, "{result:?}");
    let evidence = context.evidence();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].url, "https://example.com/ddg");
    let output: serde_json::Value = serde_json::from_str(&result.output).expect("search JSON");
    assert!(output["results"][0]["retrieved_at"].is_string());
    assert!(output["results"][0]["source_authority"].is_string());
}

#[tokio::test]
async fn both_provider_failures_are_explicit_and_agent_does_not_retry_search() {
    let (tool, bing, duckduckgo) = tool_with_servers(
        vec![
            response("bing unavailable", 500),
            response("bing unavailable", 500),
        ],
        vec![
            response("ddg unavailable", 500),
            response("ddg unavailable", 500),
        ],
        1,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = SingleSearchCallProvider {
        calls: calls.clone(),
    };
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(tool));
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
        .expect("exhausted search should finish the turn");
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(output.contains("web search failed: bing:"), "{output}");
    assert!(output.contains("duckduckgo:"), "{output}");
    assert!(
        std::iter::from_fn(|| event_rx.try_recv().ok()).any(|event| {
            matches!(event, AgentEvent::Error { message } if message.contains("Search stopped:"))
        })
    );
}

struct SingleSearchCallProvider {
    calls: Arc<AtomicUsize>,
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
                "provider called more than once".into(),
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

#[tokio::test]
async fn unrelated_tools_remain_available_while_search_is_paced() {
    let slow = response("<html>not RSS</html>", 200);
    let mut duckduckgo_response = response(DDG_RESULT, 200);
    duckduckgo_response.delay = Duration::from_millis(100);
    let (tool, bing, duckduckgo) = tool_with_servers(vec![slow], vec![duckduckgo_response], 0);
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(tool));
    registry.register(Box::new(FastTool));
    let fast_call = ToolCall {
        id: "fast-call".into(),
        name: "fast_tool".into(),
        input: serde_json::json!({}),
    };
    let search_call = call(serde_json::json!({"query": "slow"}));
    let search = registry.execute(&search_call);
    let fast = tokio::time::timeout(Duration::from_millis(50), registry.execute(&fast_call));
    let (search, fast) = tokio::join!(search, fast);
    bing.handle.join().expect("Bing fixture thread");
    duckduckgo.handle.join().expect("DDG fixture thread");
    assert!(search.success, "{search:?}");
    assert!(fast.expect("fast tool should not be paced").success);
}

struct FastTool;

#[async_trait]
impl ToolExecutor for FastTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fast_tool".into(),
            description: "test unrelated tool".into(),
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
