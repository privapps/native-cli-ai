use async_trait::async_trait;
use nca_common::config::{NcaConfig, PermissionConfig};
use nca_common::event::{AgentEvent, BusyState};
use nca_common::message::Message;
use nca_common::tool::ToolDefinition;
use nca_core::agent::AgentLoop;
use nca_core::approval::ApprovalPolicy;
use nca_core::provider::custom::CustomProvider;
use nca_core::provider::{Provider, ProviderError, StreamChunk};
use nca_core::tools::ToolRegistry;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tiny_http::{Response, Server, StatusCode};

struct PendingProvider;

#[async_trait]
impl Provider for PendingProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        std::future::pending().await
    }
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        Err(ProviderError::RequestFailed("provider unavailable".into()))
    }
}

fn agent(
    provider: Box<dyn Provider>,
    event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
) -> AgentLoop {
    AgentLoop::new(
        provider,
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        4,
        4,
        1,
        None,
    )
}

fn drain_events(event_rx: &mut tokio::sync::mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn esc_cancels_a_turn_while_provider_request_is_pending() {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
    let mut agent = agent(Box::new(PendingProvider), event_tx);
    let cancel = agent.cancel_handle();
    let turn = tokio::spawn(async move { agent.run_turn("hello", Path::new("."), &[]).await });

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

    cancel.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_millis(150), turn)
        .await
        .expect("Esc cancellation should terminate a pending provider request")
        .expect("agent task should not panic");
    assert!(result.is_err(), "cancelled turn should return an error");

    let events = drain_events(&mut event_rx);
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::Error { message } if message.contains("Run cancelled"))
    }));
    assert!(matches!(
        result,
        Err(ProviderError::Other(message))
            if message == "run cancelled while waiting for model"
    ));
}

#[tokio::test]
async fn esc_cancels_a_pending_custom_provider_request() {
    let server = Server::http("127.0.0.1:0").expect("mock provider listener");
    let base_url = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => format!("http://{address}"),
        other => panic!("unsupported mock provider address: {other:?}"),
    };
    let (request_started_tx, request_started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let request = server.recv().expect("custom provider request");
        let _ = request_started_tx.send(());
        let _ = release_rx.recv();
        let _ = request.respond(Response::from_string("").with_status_code(StatusCode(200)));
    });

    let mut config = NcaConfig::default();
    config.provider.custom.api_key = Some("custom-test-key".into());
    config.provider.custom.base_url = base_url;
    let provider = CustomProvider::from_config(&config).expect("custom provider");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
    let mut agent = agent(Box::new(provider), event_tx);
    let cancel = agent.cancel_handle();
    let turn = tokio::spawn(async move { agent.run_turn("hello", Path::new("."), &[]).await });

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
        .expect("custom provider request should start")
        .expect("mock provider should receive the request");

    cancel.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_millis(150), turn)
        .await
        .expect("Esc cancellation should terminate the custom provider request")
        .expect("agent task should not panic");
    assert!(matches!(
        result,
        Err(ProviderError::Other(message))
            if message == "run cancelled while waiting for model"
    ));

    release_tx.send(()).expect("release mock provider");
}

#[tokio::test]
async fn provider_request_errors_are_emitted_for_tui_cleanup() {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
    let mut agent = agent(Box::new(FailingProvider), event_tx);

    let result = agent.run_turn("hello", Path::new("."), &[]).await;
    assert!(
        matches!(result, Err(ProviderError::RequestFailed(message)) if message == "provider unavailable")
    );

    let events = drain_events(&mut event_rx);
    assert!(events.iter().any(|event| {
        matches!(event, AgentEvent::Error { message } if message == "API request failed: provider unavailable")
    }));
}
