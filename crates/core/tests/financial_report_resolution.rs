use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use nca_common::config::{NcaConfig, PermissionConfig};
use nca_common::event::AgentEvent;
use nca_common::message::Message;
use nca_common::tool::{ToolCall, ToolDefinition};
use nca_core::agent::AgentLoop;
use nca_core::approval::ApprovalPolicy;
use nca_core::harness::{HarnessSnapshot, build_system_prompt};
use nca_core::provider::{Provider, ProviderError, StreamChunk};
use nca_core::tools::ToolRegistry;
use nca_core::tools::fetch_url::FetchUrlTool;
use nca_core::tools::resolve_latest_financial_report::ResolveLatestFinancialReportTool;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tiny_http::{Header, Response, Server, StatusCode};

struct ScriptedProvider {
    calls: AtomicUsize,
    fixture_url: String,
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_number == 0 {
            assert!(
                messages[0]
                    .content
                    .to_summary_text()
                    .contains("as_of: 2026-07-29")
            );
        }

        let chunks = match call_number {
            0 => vec![StreamChunk::ToolUse(ToolCall {
                id: "fetch-1".into(),
                name: "fetch_url".into(),
                input: serde_json::json!({
                    "url": self.fixture_url,
                    "issuer": "Microsoft"
                }),
            })],
            1 => vec![StreamChunk::ToolUse(ToolCall {
                id: "resolve-1".into(),
                name: "resolve_latest_financial_report".into(),
                input: serde_json::json!({
                    "issuer": "Microsoft",
                    "cadence": "annual"
                }),
            })],
            2 => {
                let resolution: serde_json::Value = serde_json::from_str(
                    &messages
                        .last()
                        .expect("resolver result")
                        .content
                        .to_summary_text(),
                )
                .expect("resolver JSON");
                let report = &resolution["selected"];
                vec![StreamChunk::TextDelta(format!(
                    "# Microsoft Financial Results\n\nAs of: 2026-07-29\n\nIssuer: Microsoft\nReporting calendar: fiscal\nReport type: quarterly\nPeriod end: 2026-03-31\nPublication status: reported\nPublication date: {}\nSource retrieved at: {}\n\nFY2026 Q3 revenue was reported.\n\nSource: {}\n\nThe requested annual report was not available, so this quarterly fallback is shown instead.",
                    report["candidate"]["publication_date"],
                    report["source_retrieved_at"],
                    report["candidate"]["publication_url"],
                ))]
            }
            _ => {
                return Err(ProviderError::Other(
                    "scripted provider called too many times".into(),
                ));
            }
        };
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        for chunk in chunks {
            tx.send(chunk)
                .await
                .map_err(|_| ProviderError::Other("scripted stream closed".into()))?;
        }
        tx.send(StreamChunk::Done)
            .await
            .map_err(|_| ProviderError::Other("scripted stream closed".into()))?;
        Ok(rx)
    }
}

#[tokio::test]
async fn fake_clock_agent_turn_resolves_a_verified_quarterly_fallback() {
    let server = Server::http("127.0.0.1:0").expect("start fixture server");
    let address = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => address,
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let fixture_url = format!("http://investor.example.com:{}/q3-2026", address.port());
    let response_body = r#"<html><head><meta property="article:published_time" content="2026-04-28T12:00:00Z"><title>Microsoft FY2026 Q3 reported results</title></head><body>Microsoft FY2026 Q3 quarterly results, period ended 2026-03-31; reported revenue.</body></html>"#;
    std::thread::spawn(move || {
        let request = server.recv().expect("fixture request");
        request
            .respond(
                Response::from_string(response_body)
                    .with_status_code(StatusCode(200))
                    .with_header(
                        Header::from_bytes("Content-Type", "text/html")
                            .expect("content type header"),
                    ),
            )
            .expect("fixture response");
    });

    let client = reqwest::Client::builder()
        .resolve("investor.example.com", address)
        .build()
        .expect("fixture client");
    let config = NcaConfig::default();
    let mut tools = ToolRegistry::new();
    let context = tools.research_context();
    tools.register(Box::new(FetchUrlTool::with_client(
        config.web.clone(),
        context.clone(),
        client,
    )));
    tools.register(Box::new(ResolveLatestFinancialReportTool::new(
        context.clone(),
    )));
    tools.enable_financial_research();

    let as_of = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
    let workspace = tempfile::tempdir().expect("workspace");
    let snapshot = HarnessSnapshot {
        workspace_root: workspace.path().to_path_buf(),
        as_of: as_of.date_naive(),
        cwd_display: workspace.path().display().to_string(),
        model: "test-model".into(),
        permission_mode: "default".into(),
        ..Default::default()
    };
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);
    let provider = ScriptedProvider {
        calls: AtomicUsize::new(0),
        fixture_url: fixture_url.clone(),
    };
    let mut agent = AgentLoop::new(
        Box::new(provider),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()),
        "test-model".into(),
        event_tx,
        6,
        4,
        1,
        None,
    );
    agent.set_system_prompt(build_system_prompt(&config, &snapshot, None));
    agent.begin_research_turn(as_of.date_naive());

    let output = agent
        .run_turn(
            "Find the latest annual financial report for Microsoft.",
            workspace.path(),
            &[],
        )
        .await
        .expect("scripted research turn");
    assert!(output.contains("Report type: quarterly"));
    assert!(output.contains("FY2026 Q3"));
    assert!(output.contains("Publication date:"));
    assert!(output.contains("Source retrieved at:"));
    assert!(output.contains("quarterly fallback"));
    assert!(!output.contains("Report type: annual"));
    let selected = agent
        .tools
        .research_context()
        .validated_report()
        .expect("resolver should select a report");
    assert_eq!(
        selected.candidate.report_type,
        nca_core::research::ReportType::Quarterly
    );
    assert_eq!(selected.candidate.publication_url, fixture_url);
}
