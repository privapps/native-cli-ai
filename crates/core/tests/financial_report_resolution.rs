use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
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
use nca_core::tools::write_file::WriteFileTool;
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
                    "url": self.fixture_url
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

    let generic_evidence = agent.tools.evidence_ledger().evidence();
    assert_eq!(generic_evidence.len(), 1);
    assert_eq!(generic_evidence[0].url, fixture_url);
    assert_eq!(generic_evidence[0].response_status, Some(200));
    assert!(generic_evidence[0].retrieved_at.timestamp() > 0);
    assert!(generic_evidence[0].published_at.is_some());
    assert_eq!(
        generic_evidence[0].authority,
        nca_core::research::SourceAuthority::Official
    );
    assert_eq!(agent.tools.evidence_ledger().as_of(), as_of.date_naive());
}

struct GenericEvidenceProvider {
    calls: AtomicUsize,
    fixture_url: String,
}

struct FinancialLookingGenericProvider;

#[async_trait]
impl Provider for FinancialLookingGenericProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tx.send(StreamChunk::TextDelta(
            "The financial report draft contains revenue notes for a neutral example.".into(),
        ))
        .await
        .map_err(|_| ProviderError::Other("scripted stream closed".into()))?;
        tx.send(StreamChunk::Done)
            .await
            .map_err(|_| ProviderError::Other("scripted stream closed".into()))?;
        Ok(rx)
    }
}

#[tokio::test]
async fn generic_turn_does_not_infer_financial_verification_from_output_text() {
    let workspace = tempfile::tempdir().expect("workspace");
    let config = NcaConfig::default();
    let snapshot = HarnessSnapshot {
        workspace_root: workspace.path().to_path_buf(),
        as_of: NaiveDate::from_ymd_opt(2026, 7, 29).unwrap(),
        cwd_display: workspace.path().display().to_string(),
        model: "test-model".into(),
        permission_mode: "default".into(),
        ..Default::default()
    };
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);
    let mut agent = AgentLoop::new(
        Box::new(FinancialLookingGenericProvider),
        ToolRegistry::new(),
        ApprovalPolicy::new(PermissionConfig::default()).with_yolo(true),
        "test-model".into(),
        event_tx,
        2,
        1,
        1,
        None,
    );
    agent.set_system_prompt(build_system_prompt(&config, &snapshot, None));
    agent.begin_research_turn(snapshot.as_of);

    let output = agent
        .run_turn("Discuss this neutral draft.", workspace.path(), &[])
        .await
        .expect("generic turn");

    assert!(output.contains("financial report draft"));
    assert!(!output.contains("Verification status: unverified"));
}

#[async_trait]
impl Provider for GenericEvidenceProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call_number {
            0 => vec![StreamChunk::ToolUse(ToolCall {
                id: "generic-fetch".into(),
                name: "fetch_url".into(),
                input: serde_json::json!({"url": self.fixture_url}),
            })],
            1 => vec![StreamChunk::ToolUse(ToolCall {
                id: "generic-fetch-again".into(),
                name: "fetch_url".into(),
                input: serde_json::json!({"url": self.fixture_url}),
            })],
            2 => vec![StreamChunk::ToolUse(ToolCall {
                id: "generic-write".into(),
                name: "write_file".into(),
                input: serde_json::json!({
                    "path": "evidence.txt",
                    "content": "The collected evidence is domain-neutral."
                }),
            })],
            3 => vec![StreamChunk::TextDelta(
                "Collected generic evidence and saved it.".into(),
            )],
            _ => {
                return Err(ProviderError::Other(
                    "generic scripted provider called too many times".into(),
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
async fn generic_evidence_and_file_writing_work_without_financial_capability() {
    let server = Server::http("127.0.0.1:0").expect("start fixture server");
    let address = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => address,
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let fixture_url = format!("http://source.example.com:{}/evidence/", address.port());
    let response_bodies = [
        r#"<html><head><meta property="article:published_time" content="2026-07-28T12:00:00Z"><title>Collected notes</title></head><body>Evidence about a neutral research subject.</body></html>"#,
        r#"<html><head><meta property="article:published_time" content="2026-07-29T12:00:00Z"><title>Collected notes</title></head><body>Evidence about a neutral research subject.</body></html>"#,
    ];
    std::thread::spawn(move || {
        for response_body in response_bodies {
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
        }
    });

    let client = reqwest::Client::builder()
        .resolve("source.example.com", address)
        .build()
        .expect("fixture client");
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let mut tools = ToolRegistry::new();
    let context = tools.research_context();
    tools.register(Box::new(FetchUrlTool::with_client(
        NcaConfig::default().web.clone(),
        context.clone(),
        client,
    )));
    tools.register(Box::new(WriteFileTool::new(workspace_root)));
    assert!(
        tools
            .definitions()
            .iter()
            .any(|definition| definition.name == "write_file")
    );
    assert!(
        !tools
            .definitions()
            .iter()
            .any(|definition| definition.name == "resolve_latest_financial_report")
    );

    let as_of = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
    let config = NcaConfig::default();
    let snapshot = HarnessSnapshot {
        workspace_root: workspace.path().to_path_buf(),
        as_of: as_of.date_naive(),
        cwd_display: workspace.path().display().to_string(),
        model: "test-model".into(),
        permission_mode: "default".into(),
        ..Default::default()
    };
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);
    let mut agent = AgentLoop::new(
        Box::new(GenericEvidenceProvider {
            calls: AtomicUsize::new(0),
            fixture_url: fixture_url.clone(),
        }),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()).with_yolo(true),
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
        .run_turn("Collect and save the evidence.", workspace.path(), &[])
        .await
        .expect("generic evidence turn");
    assert!(output.contains("Collected generic evidence"));
    assert_eq!(
        tokio::fs::read_to_string(workspace.path().join("evidence.txt"))
            .await
            .expect("written evidence"),
        "The collected evidence is domain-neutral."
    );

    let ledger = agent.tools.evidence_ledger();
    let evidence = ledger.evidence();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].url, fixture_url.trim_end_matches('/'));
    assert_eq!(evidence[0].response_status, Some(200));
    assert_eq!(
        evidence[0].authority,
        nca_core::research::SourceAuthority::Secondary
    );
    assert_eq!(evidence.len(), 1);
    assert_eq!(ledger.observations().len(), 2);
    assert!(
        ledger
            .conflicts()
            .iter()
            .any(|conflict| conflict.contains("Conflicting publication dates"))
    );
    assert_eq!(
        evidence[0].published_at.map(|value| value.date_naive()),
        Some(NaiveDate::from_ymd_opt(2026, 7, 28).unwrap())
    );
    assert!(
        agent.tools.research_context().evidence()[0]
            .report_metadata
            .is_none()
    );
    assert_eq!(ledger.as_of(), as_of.date_naive());

    let next_as_of = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    agent.begin_research_turn(next_as_of);
    assert_eq!(ledger.as_of(), next_as_of);
    assert!(ledger.evidence().is_empty());
    assert!(ledger.observations().is_empty());
    assert!(ledger.conflicts().is_empty());
    assert!(agent.tools.research_context().evidence().is_empty());
}
