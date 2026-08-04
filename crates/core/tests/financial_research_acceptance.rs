use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use nca_common::config::{NcaConfig, PermissionConfig};
use nca_common::event::AgentEvent;
use nca_common::message::Message;
use nca_common::tool::ToolCall;
use nca_common::tool::ToolDefinition;
use nca_core::agent::AgentLoop;
use nca_core::approval::ApprovalPolicy;
use nca_core::harness::{HarnessSnapshot, build_system_prompt};
use nca_core::provider::{Provider, ProviderError, StreamChunk};
use nca_core::research::{
    EvidenceRecord, FinancialReportCandidate, ReportEvidenceMetadata, ReportStatus, ReportType,
    ReportingCalendar, ResearchContext, SourceAuthority,
};
use nca_core::skills::{SkillCatalog, SkillSource};
use nca_core::tools::fetch_url::FetchUrlTool;
use nca_core::tools::invoke_skill::InvokeSkillTool;
use nca_core::tools::resolve_latest_financial_report::ResolveLatestFinancialReportTool;
use nca_core::tools::web_search::WebSearchTool;
use nca_core::tools::{RecentSkillHints, ToolExecutor, ToolRegistry};
use reqwest::Client;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tiny_http::{Header, Response, Server, StatusCode};

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("{name}-acceptance"),
        name: name.into(),
        input,
    }
}

fn fixture_server(body: &'static str) -> (SocketAddr, std::thread::JoinHandle<()>) {
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
                    .with_status_code(StatusCode(200))
                    .with_header(
                        Header::from_bytes("Content-Type", "text/html")
                            .expect("content type header"),
                    ),
            )
            .expect("fixture response");
    });
    (address, handle)
}

fn quarterly_evidence(as_of: NaiveDate) -> EvidenceRecord {
    EvidenceRecord {
        url: "https://investor.example.com/q3-2026".into(),
        title: Some("Microsoft FY2026 Q3 reported results".into()),
        snippet: None,
        retrieved_at: as_of.and_hms_opt(12, 0, 0).unwrap().and_utc(),
        response_status: Some(200),
        http_date: None,
        published_at: Some(
            as_of
                .pred_opt()
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc(),
        ),
        authority: SourceAuthority::Official,
        report_metadata: Some(ReportEvidenceMetadata {
            issuer: Some("Microsoft".into()),
            period_label: Some("FY2026 Q3".into()),
            period_end: Some(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()),
            report_type: Some(ReportType::Quarterly),
            calendar: Some(ReportingCalendar::Fiscal),
            status: Some(ReportStatus::Reported),
        }),
    }
}

#[tokio::test]
async fn same_day_publication_is_eligible_and_fetch_output_has_date_only_as_of() {
    let as_of = NaiveDate::from_ymd_opt(2026, 7, 29).unwrap();
    let publication = "2026-07-29T23:59:59Z";
    let body = r#"<html><head><meta property="article:published_time" content="2026-07-29T23:59:59Z"><title>Microsoft FY2025 results</title></head><body>Microsoft FY2025 annual results, period ended 2025-06-30; reported revenue.</body></html>"#;
    let (address, server) = fixture_server(body);
    let client = Client::builder()
        .resolve("investor.example.com", address)
        .build()
        .expect("fixture client");
    let context = Arc::new(ResearchContext::new(as_of));
    let tool = FetchUrlTool::with_client(
        nca_common::config::WebConfig::default(),
        context.clone(),
        client,
    );
    assert!(
        tool.definition().parameters["properties"]
            .get("issuer")
            .is_none()
    );

    let result = tool
        .execute(&call(
            "fetch_url",
            serde_json::json!({
                "url": format!("http://investor.example.com:{}/fy2025", address.port())
            }),
        ))
        .await;
    server.join().expect("fixture thread");

    assert!(result.success, "{result:?}");
    let output: serde_json::Value = serde_json::from_str(&result.output).expect("fetch JSON");
    assert_eq!(output["source"]["as_of"], "2026-07-29");
    assert_eq!(output["source"]["published_at"], publication);
    assert_eq!(output["source"]["eligible_as_of"], true);
    assert!(output["source"].get("report_metadata").is_none());
    assert_eq!(context.as_of(), as_of);

    let evidence = context.evidence().pop().expect("fetch evidence");
    let candidate = FinancialReportCandidate {
        issuer: "Microsoft".into(),
        report_type: ReportType::Annual,
        period_label: "FY2025".into(),
        period_end: NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
        calendar: ReportingCalendar::Fiscal,
        status: ReportStatus::Reported,
        publication_url: evidence.url,
        publication_date: Utc.with_ymd_and_hms(2026, 7, 29, 23, 59, 59).unwrap(),
    };
    let validated = context
        .validate_candidate(candidate)
        .expect("same-day report");
    assert_eq!(validated.as_of, as_of);
}

#[test]
fn web_search_takes_as_of_from_turn_context_instead_of_tool_input() {
    let context = Arc::new(ResearchContext::new(
        NaiveDate::from_ymd_opt(2026, 7, 29).unwrap(),
    ));
    let definition =
        WebSearchTool::new(nca_common::config::WebConfig::default(), context).definition();

    assert!(definition.parameters["properties"].get("as_of").is_none());
    assert!(definition.parameters["properties"].get("issuer").is_none());
    assert!(definition.description.contains("publication metadata"));
}

#[tokio::test]
async fn fallback_and_unverified_results_remain_structured_and_explicit() {
    let as_of = NaiveDate::from_ymd_opt(2026, 7, 29).unwrap();
    let context = Arc::new(ResearchContext::new(as_of));
    context.record_evidence(quarterly_evidence(as_of));

    let fallback = ResolveLatestFinancialReportTool::new(context.clone())
        .execute(&call(
            "resolve_latest_financial_report",
            serde_json::json!({"issuer": "Microsoft", "cadence": "annual"}),
        ))
        .await;
    assert!(fallback.success, "{fallback:?}");
    let fallback_json: serde_json::Value =
        serde_json::from_str(&fallback.output).expect("fallback JSON");
    assert_eq!(fallback_json["status"], "fallback");
    assert_eq!(fallback_json["as_of"], "2026-07-29");
    assert_eq!(
        fallback_json["selected"]["candidate"]["report_type"],
        "quarterly"
    );
    assert!(
        fallback_json["limitation"]
            .as_str()
            .is_some_and(|value| value.contains("must not be treated as annual"))
    );

    let unverified = context.annotate_final_response(
        r#"{"financial_report":"Microsoft","as_of":"2026-07-29","revenue":1}"#,
    );
    let unverified_json: serde_json::Value =
        serde_json::from_str(&unverified).expect("unverified JSON");
    assert_eq!(unverified_json["verification_status"], "unverified");
    assert!(
        unverified_json["verification_warning"]
            .as_str()
            .is_some_and(|value| value.contains("as-of"))
    );
}

struct MismatchedIssuerProvider {
    calls: AtomicUsize,
    fixture_url: String,
}

#[async_trait]
impl Provider for MismatchedIssuerProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = match call_number {
            0 => vec![StreamChunk::ToolUse(ToolCall {
                id: "mismatched-fetch".into(),
                name: "fetch_url".into(),
                input: serde_json::json!({
                    "url": self.fixture_url
                }),
            })],
            1 => vec![StreamChunk::ToolUse(ToolCall {
                id: "mismatched-resolve".into(),
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
                assert_eq!(resolution["status"], "unavailable");
                vec![StreamChunk::TextDelta(
                    "# Microsoft Financial Results\n\nMicrosoft revenue was reported without authoritative matching evidence."
                        .into(),
                )]
            }
            _ => {
                return Err(ProviderError::Other(
                    "mismatched issuer provider called too many times".into(),
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
async fn application_flow_marks_mismatched_issuer_evidence_unverified() {
    let body = r#"<html><head><meta property="article:published_time" content="2026-04-28T12:00:00Z"><title>Apple FY2026 annual results</title></head><body>Apple FY2026 annual results, period ended 2026-06-30; reported revenue.</body></html>"#;
    let (address, server) = fixture_server(body);
    let fixture_url = format!("http://investor.example.com:{}/fy2026", address.port());
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
    let mut agent = AgentLoop::new(
        Box::new(MismatchedIssuerProvider {
            calls: AtomicUsize::new(0),
            fixture_url,
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
        .run_turn(
            "Find the latest annual financial report for Microsoft.",
            workspace.path(),
            &[],
        )
        .await
        .expect("scripted mismatched-evidence turn");
    server.join().expect("fixture thread");

    assert!(output.contains("Verification status: unverified"));
    assert!(output.contains("No eligible annual or quarterly result"));
    assert!(agent.tools.research_context().validated_report().is_none());
}

#[tokio::test]
async fn financial_skill_is_embedded_manual_only_and_explicitly_enables_financial_tools() {
    let workspace = tempfile::tempdir().expect("create skill fixture workspace");

    let workspace_root = workspace.path().to_path_buf();
    let config = NcaConfig::default();
    let discovered = SkillCatalog::discover(workspace.path(), &config.harness.skill_directories)
        .expect("discover shipped skills");
    let financial = discovered
        .iter()
        .find(|skill| skill.command == "financial-research")
        .expect("embedded financial skill");
    assert_eq!(financial.source, SkillSource::BuiltIn);
    assert!(!financial.allow_implicit_invocation);
    assert!(
        !SkillCatalog::discover_for_model(workspace.path(), &config.harness.skill_directories)
            .expect("discover model skills")
            .iter()
            .any(|skill| skill.command == "financial-research")
    );

    let registry =
        ToolRegistry::with_default_readonly_tools(workspace_root.clone(), config.web.clone());
    let capability = registry.financial_research_capability();
    assert!(
        !registry
            .definitions()
            .iter()
            .any(|definition| definition.name == "resolve_latest_financial_report")
    );

    let denied = InvokeSkillTool::new_with_financial_capability(
        workspace_root.clone(),
        config.harness.skill_directories,
        RecentSkillHints::default(),
        capability.clone(),
    );
    let denied_result = denied
        .execute(&call(
            "invoke_skill",
            serde_json::json!({"skill_name": "financial-research"}),
        ))
        .await;
    assert!(!denied_result.success);
    assert!(!capability.load(Ordering::Acquire));

    let explicit = InvokeSkillTool::new_with_financial_capability_and_explicit_skills(
        workspace_root,
        vec![],
        RecentSkillHints::default(),
        capability,
        vec!["financial-research".into()],
    );
    let result = explicit
        .execute(&call(
            "invoke_skill",
            serde_json::json!({"skill_name": "financial-research"}),
        ))
        .await;

    assert!(result.success, "{result:?}");
    assert!(result.output.contains("Skill `financial-research` loaded"));
    assert!(result.output.contains("immutable UTC calendar date"));
    assert!(
        registry
            .definitions()
            .iter()
            .any(|definition| definition.name == "resolve_latest_financial_report")
    );
}
