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
use nca_core::tools::web_search::WebSearchTool;
use nca_core::tools::{ToolExecutor, ToolRegistry};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    let tool = WebSearchTool::with_client_and_endpoint(
        WebConfig::default(),
        context.clone(),
        client,
        format!("http://{address}/html/"),
    );
    (tool, context, handle)
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
