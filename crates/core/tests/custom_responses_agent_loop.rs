use async_trait::async_trait;
use nca_common::config::{NcaConfig, PermissionConfig, ProviderCompatibility};
use nca_common::event::{AgentEvent, BusyState};
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use nca_core::agent::AgentLoop;
use nca_core::approval::ApprovalPolicy;
use nca_core::provider::custom::CustomProvider;
use nca_core::tools::{ToolExecutor, ToolRegistry};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Duration;
use tiny_http::{Header, Request, Response, Server, StatusCode};

const FUNCTION_CALL_RESPONSE: &str = concat!(
    "event: response.output_item.added\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_item_1\",\"call_id\":\"call_1\",\"name\":\"echo\",\"arguments\":\"\"}}\n\n",
    "event: response.function_call_arguments.delta\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_item_1\",\"delta\":\"{\\\"value\\\":\\\"hello\\\"}\"}\n\n",
    "event: response.function_call_arguments.done\n",
    "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_item_1\",\"arguments\":\"{\\\"value\\\":\\\"hello\\\"}\"}\n\n",
    "event: response.output_item.done\n",
    "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_item_1\",\"name\":\"echo\",\"arguments\":\"{\\\"value\\\":\\\"hello\\\"}\"}}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
);

const FINAL_RESPONSE: &str = concat!(
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"tool loop complete\"}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
);

struct EchoTool;

#[async_trait]
impl ToolExecutor for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".into(),
            description: "Echo a value for the Responses tool-loop fixture".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: format!("echoed: {}", call.input["value"]),
            error: None,
        }
    }
}

#[derive(Debug)]
struct CapturedRequest {
    url: String,
    authorization: Option<String>,
    body: String,
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str().to_string())
}

fn spawn_responses_agent_fixture() -> (String, JoinHandle<Vec<CapturedRequest>>) {
    let server = Server::http("127.0.0.1:0").expect("start Responses agent fixture");
    let base_url = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => format!("http://{address}"),
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let handle = std::thread::spawn(move || {
        let mut captured = Vec::new();
        for response_body in [FUNCTION_CALL_RESPONSE, FINAL_RESPONSE] {
            let mut request = server.recv().expect("receive Responses agent request");
            let mut body = String::new();
            request
                .as_reader()
                .read_to_string(&mut body)
                .expect("read Responses request body");
            captured.push(CapturedRequest {
                url: request.url().to_string(),
                authorization: header(&request, "authorization"),
                body,
            });
            request
                .respond(
                    Response::from_string(response_body)
                        .with_status_code(StatusCode(200))
                        .with_header(
                            Header::from_bytes("content-type", "text/event-stream")
                                .expect("SSE content type"),
                        ),
                )
                .expect("send Responses agent response");
        }
        captured
    });
    (base_url, handle)
}

#[tokio::test]
async fn custom_responses_fixture_completes_agent_tool_result_loop() {
    let (base_url, fixture) = spawn_responses_agent_fixture();
    let mut config = NcaConfig::default();
    config.provider.custom.api_key = Some("responses-agent-secret".into());
    config.provider.custom.base_url = base_url;
    config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
    config.provider.custom.model = "responses-agent-model".into();
    let provider = CustomProvider::from_config(&config).expect("custom Responses provider");

    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
    let mut agent = AgentLoop::new(
        Box::new(provider),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()).with_yolo(true),
        "responses-agent-model".into(),
        event_tx,
        4,
        4,
        1,
        None,
    );
    let workspace = tempfile::tempdir().expect("agent workspace");

    let answer = agent
        .run_turn("echo hello", workspace.path(), &[])
        .await
        .expect("Responses agent tool loop");
    let requests = fixture.join().expect("Responses fixture thread");

    assert_eq!(answer, "tool loop complete");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "/v1/responses");
    assert_eq!(requests[1].url, "/v1/responses");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer responses-agent-secret")
    );

    let first: Value = serde_json::from_str(&requests[0].body).expect("first request JSON");
    assert_eq!(first["stream"], true);
    assert_eq!(first["store"], false);
    assert_eq!(first["tools"][0]["type"], "function");
    assert_eq!(first["tools"][0]["name"], "echo");
    assert!(first.get("previous_response_id").is_none());

    let second: Value = serde_json::from_str(&requests[1].body).expect("second request JSON");
    assert!(
        second["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["type"] == "function_call" && item["call_id"] == "call_1" })
    );
    assert!(second["input"].as_array().unwrap().iter().any(|item| {
        item["type"] == "function_call_output"
            && item["call_id"] == "call_1"
            && item["output"] == "echoed: \"hello\""
    }));
}

#[tokio::test]
async fn custom_responses_request_cancellation_is_prompt() {
    let server = Server::http("127.0.0.1:0").expect("start pending Responses fixture");
    let base_url = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => format!("http://{address}"),
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let (request_started_tx, request_started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let fixture = std::thread::spawn(move || {
        let request = server.recv().expect("receive pending Responses request");
        let _ = request_started_tx.send(());
        let _ = release_rx.recv();
        let _ = request.respond(Response::from_string("").with_status_code(StatusCode(200)));
    });

    let mut config = NcaConfig::default();
    config.provider.custom.api_key = Some("responses-cancel-secret".into());
    config.provider.custom.base_url = base_url;
    config.provider.custom.compatibility = ProviderCompatibility::OpenAiResponses;
    let provider = CustomProvider::from_config(&config).expect("custom Responses provider");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(16);
    let mut agent = AgentLoop::new(
        Box::new(provider),
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()).with_yolo(true),
        "responses-cancel-model".into(),
        event_tx,
        4,
        4,
        1,
        None,
    );
    let cancel = agent.cancel_handle();
    let workspace = tempfile::tempdir().expect("agent workspace");
    let turn = tokio::spawn(async move {
        agent
            .run_turn("cancel this request", workspace.path(), &[])
            .await
    });

    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("agent should enter Thinking")
            .expect("event channel should remain open");
        if matches!(
            event,
            AgentEvent::BusyStateChanged {
                state: BusyState::Thinking
            }
        ) {
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(1), request_started_rx)
        .await
        .expect("Responses request should start")
        .expect("fixture should receive the Responses request");

    cancel.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_millis(150), turn)
        .await
        .expect("Responses cancellation should terminate the pending request")
        .expect("agent task should not panic");
    assert!(matches!(
        result,
        Err(nca_core::provider::ProviderError::Other(message))
            if message == "run cancelled while waiting for model"
    ));

    release_tx
        .send(())
        .expect("release pending Responses fixture");
    fixture.join().expect("Responses fixture thread");
}
