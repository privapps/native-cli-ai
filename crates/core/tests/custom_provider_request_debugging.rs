use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use nca_common::config::{NcaConfig, ProviderCompatibility};
use nca_common::message::Message;
use nca_core::provider::custom::CustomProvider;
use nca_core::provider::{Provider, StreamChunk};
use serde_json::Value;
use tiny_http::{Header, Request, Response, Server, StatusCode};

const CHILD_MODE: &str = "NCA_CUSTOM_PROVIDER_DEBUG_TEST_CHILD";
const CHILD_PROTOCOL: &str = "NCA_CUSTOM_PROVIDER_DEBUG_TEST_PROTOCOL";
const CHILD_BASE_URL: &str = "NCA_CUSTOM_PROVIDER_DEBUG_TEST_BASE_URL";
const API_KEY: &str = "request-secret";
const RESPONSE_MARKER: &str = "response-only-marker";
const TEST_NAME: &str = "custom_provider_request_diagnostics_through_public_seam";

#[derive(Debug)]
struct RequestSnapshot {
    method: String,
    url: String,
    body: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
}

fn spawn_fixture(protocol: &str) -> (String, Receiver<RequestSnapshot>) {
    let server = Server::http("127.0.0.1:0").expect("start fixture server");
    let origin = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => format!("http://{address}"),
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let base_url = format!("{origin}/{API_KEY}/v1");
    let body = response_body(protocol);
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let mut request = server.recv().expect("receive provider request");
        let mut request_body = String::new();
        request
            .as_reader()
            .read_to_string(&mut request_body)
            .expect("read provider request body");

        let snapshot = RequestSnapshot {
            method: request.method().as_str().to_string(),
            url: request.url().to_string(),
            body: request_body,
            authorization: header_value(&request, "authorization"),
            x_api_key: header_value(&request, "x-api-key"),
        };
        request_tx.send(snapshot).expect("send request snapshot");

        request
            .respond(
                Response::from_string(body)
                    .with_status_code(StatusCode(200))
                    .with_header(
                        Header::from_bytes("Content-Type", "text/event-stream")
                            .expect("content type header"),
                    ),
            )
            .expect("send fixture response");
    });

    (base_url, request_rx)
}

fn header_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| match name {
            "authorization" => header.field.equiv("authorization"),
            "x-api-key" => header.field.equiv("x-api-key"),
            _ => false,
        })
        .map(|header| header.value.as_str().to_string())
}

fn response_body(protocol: &str) -> String {
    match protocol {
        "chat" => format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{RESPONSE_MARKER}\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"
        ),
        "responses" => format!(
            "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":\"{RESPONSE_MARKER}\"}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{}}}}\n\n"
        ),
        "anthropic" => format!(
            "event: content_block_delta\ndata: {{\"delta\":{{\"type\":\"text_delta\",\"text\":\"{RESPONSE_MARKER}\"}}}}\n\nevent: message_stop\ndata: {{}}\n\n"
        ),
        other => panic!("unsupported fixture protocol: {other}"),
    }
}

fn run_child(
    protocol: &str,
    base_url: &str,
    working_directory: &Path,
    debug_value: Option<&str>,
    request_rx: Receiver<RequestSnapshot>,
) -> (Output, RequestSnapshot) {
    let mut command = Command::new(env::current_exe().expect("current test executable"));
    command
        .args(["--exact", TEST_NAME, "--nocapture"])
        .current_dir(working_directory)
        .env(CHILD_MODE, "1")
        .env(CHILD_PROTOCOL, protocol)
        .env(CHILD_BASE_URL, base_url);
    match debug_value {
        Some(value) => {
            command.env("NCA_DEBUG_REQUEST", value);
        }
        None => {
            command.env_remove("NCA_DEBUG_REQUEST");
        }
    }

    let output = command.output().expect("run public-provider child");
    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let request = request_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("provider fixture request");
    (output, request)
}

async fn run_provider_child() {
    let protocol = env::var(CHILD_PROTOCOL).expect("child protocol");
    let base_url = env::var(CHILD_BASE_URL).expect("child base URL");
    let compatibility = match protocol.as_str() {
        "chat" => ProviderCompatibility::OpenAi,
        "responses" => ProviderCompatibility::OpenAiResponses,
        "anthropic" => ProviderCompatibility::Anthropic,
        other => panic!("unsupported child protocol: {other}"),
    };

    let mut config = NcaConfig::default();
    config.provider.custom.api_key = Some(API_KEY.into());
    config.provider.custom.base_url = base_url;
    config.provider.custom.compatibility = compatibility;
    config.provider.custom.model = "diagnostic-model".into();

    let provider = CustomProvider::from_config(&config).expect("construct custom provider");
    let mut stream = provider
        .chat(
            &[Message::user(format!("request body contains {API_KEY}"))],
            &[],
            "",
            Path::new("."),
        )
        .await
        .expect("send custom provider request");

    let mut text = String::new();
    let mut completed = false;
    while let Some(chunk) = stream.recv().await {
        match chunk {
            StreamChunk::TextDelta(delta) => text.push_str(&delta),
            StreamChunk::Done => {
                completed = true;
                break;
            }
            StreamChunk::Error(error) => panic!("provider stream failed: {error}"),
            StreamChunk::ToolUse(_) | StreamChunk::Usage { .. } => {}
        }
    }

    assert!(completed, "provider stream did not complete");
    assert_eq!(text, RESPONSE_MARKER);
    println!("CHILD_RESULT={text}");
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_chat_request(request: &RequestSnapshot) {
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, format!("/{API_KEY}/v1/chat/completions"));
    assert_eq!(
        request.authorization.as_deref(),
        Some("Bearer request-secret")
    );
    let body: Value = serde_json::from_str(&request.body).expect("Chat Completions JSON");
    assert_eq!(body["model"], "diagnostic-model");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"][0]["content"],
        format!("request body contains {API_KEY}")
    );
}

fn assert_responses_request(request: &RequestSnapshot) {
    assert_eq!(request.method, "POST");
    assert_eq!(request.url, format!("/{API_KEY}/v1/responses"));
    assert_eq!(
        request.authorization.as_deref(),
        Some("Bearer request-secret")
    );
    let body: Value = serde_json::from_str(&request.body).expect("Responses JSON");
    assert_eq!(body["model"], "diagnostic-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(
        body["input"][0]["content"],
        format!("request body contains {API_KEY}")
    );
}

#[tokio::test]
async fn custom_provider_request_diagnostics_through_public_seam() {
    if env::var_os(CHILD_MODE).is_some() {
        run_provider_child().await;
        return;
    }

    let chat_dir = tempfile::tempdir().expect("chat working directory");
    let (chat_url, chat_requests) = spawn_fixture("chat");
    let (chat_output, chat_request) =
        run_child("chat", &chat_url, chat_dir.path(), Some("1"), chat_requests);
    let chat_stderr = stderr(&chat_output);
    assert_chat_request(&chat_request);
    assert!(stdout(&chat_output).contains("CHILD_RESULT=response-only-marker"));
    assert!(chat_stderr.contains("custom OpenAI-compatible request"));
    assert!(chat_stderr.starts_with('['));
    assert!(chat_stderr.contains("method: POST"));
    assert!(chat_stderr.contains("url: http://127.0.0.1:"));
    assert!(chat_stderr.contains("/[REDACTED]/v1/chat/completions"));
    assert!(chat_stderr.contains("authorization: [REDACTED]"));
    assert!(chat_stderr.contains("\"model\": \"diagnostic-model\""));
    assert!(chat_stderr.contains("\"messages\": ["));
    assert!(!chat_stderr.contains(API_KEY));
    assert!(!chat_stderr.contains(RESPONSE_MARKER));
    let chat_log = fs::read_to_string(chat_dir.path().join("debug.log")).expect("Chat log");
    assert!(chat_log.contains("custom OpenAI-compatible request"));
    assert!(!chat_log.contains(API_KEY));
    assert!(!chat_log.contains(RESPONSE_MARKER));

    let responses_dir = tempfile::tempdir().expect("Responses working directory");
    let (responses_url, responses_requests) = spawn_fixture("responses");
    let (responses_output, responses_request) = run_child(
        "responses",
        &responses_url,
        responses_dir.path(),
        Some("1"),
        responses_requests,
    );
    let responses_stderr = stderr(&responses_output);
    assert_responses_request(&responses_request);
    assert!(stdout(&responses_output).contains("CHILD_RESULT=response-only-marker"));
    assert!(responses_stderr.contains("custom OpenAI Responses request"));
    assert!(responses_stderr.starts_with('['));
    assert!(responses_stderr.contains("method: POST"));
    assert!(responses_stderr.contains("/[REDACTED]/v1/responses"));
    assert!(responses_stderr.contains("authorization: [REDACTED]"));
    assert!(responses_stderr.contains("\"input\": ["));
    assert!(!responses_stderr.contains(API_KEY));
    assert!(!responses_stderr.contains(RESPONSE_MARKER));
    let responses_log =
        fs::read_to_string(responses_dir.path().join("debug.log")).expect("Responses log");
    assert!(responses_log.contains("custom OpenAI Responses request"));
    assert!(!responses_log.contains(API_KEY));
    assert!(!responses_log.contains(RESPONSE_MARKER));

    let append_dir = tempfile::tempdir().expect("append working directory");
    let (first_url, first_requests) = spawn_fixture("chat");
    let _ = run_child(
        "chat",
        &first_url,
        append_dir.path(),
        Some("1"),
        first_requests,
    );
    let (second_url, second_requests) = spawn_fixture("responses");
    let _ = run_child(
        "responses",
        &second_url,
        append_dir.path(),
        Some("1"),
        second_requests,
    );
    let append_log = fs::read_to_string(append_dir.path().join("debug.log")).expect("append log");
    assert_eq!(
        append_log
            .matches("custom OpenAI-compatible request")
            .count(),
        1
    );
    assert_eq!(
        append_log
            .matches("custom OpenAI Responses request")
            .count(),
        1
    );
    assert!(!append_log.contains(API_KEY));
    assert!(!append_log.contains(RESPONSE_MARKER));
    for debug_value in [
        None,
        Some(""),
        Some("0"),
        Some("true"),
        Some(" 1"),
        Some("01"),
    ] {
        let disabled_dir = tempfile::tempdir().expect("disabled working directory");
        let (url, requests) = spawn_fixture("chat");
        let (output, request) = run_child("chat", &url, disabled_dir.path(), debug_value, requests);
        assert_chat_request(&request);
        assert!(stdout(&output).contains("CHILD_RESULT=response-only-marker"));
        let output_stderr = stderr(&output);
        assert!(!output_stderr.contains("custom OpenAI-compatible request"));
        assert!(!output_stderr.contains("failed to write custom provider request diagnostics"));
        assert!(!disabled_dir.path().join("debug.log").exists());
    }

    let anthropic_dir = tempfile::tempdir().expect("Anthropic working directory");
    let (anthropic_url, anthropic_requests) = spawn_fixture("anthropic");
    let (anthropic_output, anthropic_request) = run_child(
        "anthropic",
        &anthropic_url,
        anthropic_dir.path(),
        Some("1"),
        anthropic_requests,
    );
    assert_eq!(anthropic_request.url, format!("/{API_KEY}/v1/messages"));
    assert_eq!(anthropic_request.x_api_key.as_deref(), Some(API_KEY));
    assert!(stdout(&anthropic_output).contains("CHILD_RESULT=response-only-marker"));
    assert!(!stderr(&anthropic_output).contains("custom Anthropic-compatible request"));
    assert!(
        !stderr(&anthropic_output).contains("failed to write custom provider request diagnostics")
    );
    assert!(!anthropic_dir.path().join("debug.log").exists());
    let write_failure_dir = tempfile::tempdir().expect("write-failure working directory");
    fs::create_dir(write_failure_dir.path().join("debug.log")).expect("block debug.log path");
    let (failure_url, failure_requests) = spawn_fixture("chat");
    let (failure_output, failure_request) = run_child(
        "chat",
        &failure_url,
        write_failure_dir.path(),
        Some("1"),
        failure_requests,
    );
    assert_chat_request(&failure_request);
    assert!(stdout(&failure_output).contains("CHILD_RESULT=response-only-marker"));
    assert!(
        stderr(&failure_output).contains("failed to write custom provider request diagnostics")
    );
    assert!(write_failure_dir.path().join("debug.log").is_dir());
}
