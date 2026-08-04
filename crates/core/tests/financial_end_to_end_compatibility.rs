use async_trait::async_trait;
use chrono::NaiveDate;
use nca_common::config::{NcaConfig, PermissionConfig};
use nca_common::event::AgentEvent;
use nca_common::message::Message;
use nca_common::tool::{ToolCall, ToolDefinition};
use nca_core::agent::AgentLoop;
use nca_core::approval::ApprovalPolicy;
use nca_core::harness::{HarnessSnapshot, build_system_prompt};
use nca_core::provider::{Provider, ProviderError, StreamChunk};
use nca_core::skills::{SkillCatalog, SkillSource};
use nca_core::tools::fetch_url::FetchUrlTool;
use nca_core::tools::invoke_skill::InvokeSkillTool;
use nca_core::tools::resolve_latest_financial_report::ResolveLatestFinancialReportTool;
use nca_core::tools::validate_financial_report::ValidateFinancialReportTool;
use nca_core::tools::write_file::WriteFileTool;
use nca_core::tools::write_validated_financial_report::WriteValidatedFinancialReportTool;
use nca_core::tools::{ToolExecutor, ToolRegistry};
use reqwest::Client;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tiny_http::{Header, Response, Server, StatusCode};

type ProviderStep = Box<dyn Fn(&[Message], &[ToolDefinition]) -> Vec<StreamChunk> + Send + Sync>;

struct ScriptedProvider {
    calls: AtomicUsize,
    steps: Vec<ProviderStep>,
}

impl ScriptedProvider {
    fn new(steps: Vec<ProviderStep>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            steps,
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _model: &str,
        _workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let Some(step) = self.steps.get(call) else {
            return Err(ProviderError::Other(format!(
                "scripted compatibility provider called too many times: {call}"
            )));
        };
        let chunks = step(messages, tools);
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        for chunk in chunks {
            sender
                .send(chunk)
                .await
                .map_err(|_| ProviderError::Other("scripted stream closed".into()))?;
        }
        sender
            .send(StreamChunk::Done)
            .await
            .map_err(|_| ProviderError::Other("scripted stream closed".into()))?;
        Ok(receiver)
    }
}

fn tool(name: &str, input: Value) -> StreamChunk {
    StreamChunk::ToolUse(ToolCall {
        id: format!("compatibility-{name}"),
        name: name.into(),
        input,
    })
}

fn fixture_server(responses: Vec<&'static str>) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let server = Server::http("127.0.0.1:0").expect("start compatibility fixture server");
    let address = match server.server_addr() {
        tiny_http::ListenAddr::IP(address) => address,
        other => panic!("unsupported fixture address: {other:?}"),
    };
    let handle = std::thread::spawn(move || {
        for body in responses {
            let request = server.recv().expect("compatibility fixture request");
            request
                .respond(
                    Response::from_string(body)
                        .with_status_code(StatusCode(200))
                        .with_header(
                            Header::from_bytes("Content-Type", "text/html")
                                .expect("content type header"),
                        ),
                )
                .expect("compatibility fixture response");
        }
    });
    (address, handle)
}

fn fixture_client(address: SocketAddr) -> Client {
    Client::builder()
        .resolve("investor.example.com", address)
        .build()
        .expect("fixture client")
}

fn make_agent(
    provider: ScriptedProvider,
    tools: ToolRegistry,
    workspace: &Path,
    as_of: NaiveDate,
) -> AgentLoop {
    let config = NcaConfig::default();
    let snapshot = HarnessSnapshot {
        workspace_root: workspace.to_path_buf(),
        as_of,
        cwd_display: workspace.display().to_string(),
        model: "compatibility-test-model".into(),
        permission_mode: "default".into(),
        ..Default::default()
    };
    let (event_sender, _event_receiver) = tokio::sync::mpsc::channel::<AgentEvent>(128);
    let mut agent = AgentLoop::new(
        Box::new(provider),
        tools,
        ApprovalPolicy::new(PermissionConfig::default()).with_yolo(true),
        "compatibility-test-model".into(),
        event_sender,
        14,
        8,
        1,
        None,
    );
    agent.set_system_prompt(build_system_prompt(&config, &snapshot, None));
    agent.begin_research_turn(as_of);
    agent
}

fn resolution_from(messages: &[Message]) -> Value {
    let message = messages
        .iter()
        .rev()
        .find(|message| message.content.to_summary_text().contains("\"status\""))
        .expect("financial resolution in provider history");
    serde_json::from_str(&message.content.to_summary_text()).expect("financial resolution JSON")
}

fn report_content(resolution: &Value, disclose_fallback: bool) -> String {
    let selected = &resolution["selected"];
    let candidate = &selected["candidate"];
    let fallback_note = if disclose_fallback {
        "\n\nThe requested annual report was unavailable, so this quarterly fallback is shown instead."
    } else {
        ""
    };
    format!(
        "# Microsoft Financial Report\n\nAs of: {}\n\nIssuer: Microsoft\nReporting period: {}\nReporting calendar: {}\nReport type: {}\nPeriod end: {}\nPublication status: {}\nPublication date: {}\nSource retrieved at: {}\n\nMicrosoft revenue was reported.\n\nSource: {}{}",
        resolution["as_of"].as_str().expect("resolution as_of"),
        candidate["period_label"].as_str().expect("period label"),
        candidate["calendar"].as_str().expect("calendar"),
        candidate["report_type"].as_str().expect("report type"),
        candidate["period_end"].as_str().expect("period end"),
        candidate["status"].as_str().expect("report status"),
        candidate["publication_date"]
            .as_str()
            .expect("publication date"),
        selected["source_retrieved_at"]
            .as_str()
            .expect("retrieval timestamp"),
        candidate["publication_url"]
            .as_str()
            .expect("publication URL"),
        fallback_note,
    )
}

fn full_research_tools(workspace: &Path, client: Client) -> ToolRegistry {
    let config = NcaConfig::default();
    let workspace_root = workspace.canonicalize().expect("canonical workspace");
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
    tools.register(Box::new(ValidateFinancialReportTool::new(context.clone())));
    tools.register(Box::new(WriteFileTool::new(workspace_root.clone())));
    tools.register(Box::new(WriteValidatedFinancialReportTool::new(
        workspace_root,
        context,
    )));
    tools.enable_financial_research();
    tools
}

#[tokio::test]
async fn verified_agent_flow_uses_one_as_of_and_protected_persistence() {
    let as_of = NaiveDate::from_ymd_opt(2026, 7, 29).expect("as_of");
    let (address, server) = fixture_server(vec![
        r#"<html><head><meta property="article:published_time" content="2026-07-29T10:00:00Z"><title>Microsoft FY2025 annual results</title></head><body>Microsoft FY2025 annual results, period ended 2025-06-30; reported revenue.</body></html>"#,
    ]);
    let fixture_url = format!("http://investor.example.com:{}/fy2025", address.port());
    let workspace = tempfile::tempdir().expect("workspace");
    let output_path = workspace.path().join("verified-report.md");
    let tools = full_research_tools(workspace.path(), fixture_client(address));

    let fetch_url = fixture_url.clone();
    let steps: Vec<ProviderStep> = vec![
        Box::new(move |_, definitions| {
            assert!(
                definitions
                    .iter()
                    .any(|definition| definition.name == "fetch_url")
            );
            assert!(
                definitions
                    .iter()
                    .any(|definition| definition.name == "resolve_latest_financial_report")
            );
            assert!(
                definitions
                    .iter()
                    .any(|definition| definition.name == "write_validated_financial_report")
            );
            vec![tool("fetch_url", serde_json::json!({"url": fetch_url}))]
        }),
        Box::new(|messages, _| {
            let fetched = messages
                .last()
                .expect("fetch result")
                .content
                .to_summary_text();
            assert!(fetched.contains("\"as_of\": \"2026-07-29\""));
            vec![tool(
                "resolve_latest_financial_report",
                serde_json::json!({"issuer": "Microsoft", "cadence": "annual"}),
            )]
        }),
        Box::new(|messages, _| {
            let resolution = resolution_from(messages);
            assert_eq!(resolution["status"], "resolved");
            assert_eq!(resolution["as_of"], "2026-07-29");
            assert_eq!(resolution["selected"]["candidate"]["report_type"], "annual");
            vec![tool(
                "write_validated_financial_report",
                serde_json::json!({
                    "path": "verified-report.md",
                    "content": report_content(&resolution, false),
                }),
            )]
        }),
        Box::new(|messages, _| {
            let written = messages
                .last()
                .expect("writer result")
                .content
                .to_summary_text();
            assert!(
                written.contains("Wrote"),
                "protected writer failed: {written}"
            );
            let resolution = resolution_from(messages);
            vec![StreamChunk::TextDelta(report_content(&resolution, false))]
        }),
    ];

    let mut agent = make_agent(ScriptedProvider::new(steps), tools, workspace.path(), as_of);
    let output = agent
        .run_turn(
            "Find and save Microsoft's latest annual financial report.",
            workspace.path(),
            &[],
        )
        .await
        .expect("verified compatibility turn");
    server.join().expect("fixture thread");

    assert!(output.contains("Report type: annual"));
    assert!(output.contains("As of: 2026-07-29"));
    assert!(!output.contains("Verification status: unverified"));
    assert_eq!(
        tokio::fs::read_to_string(&output_path)
            .await
            .expect("protected financial report"),
        output
    );
    assert_eq!(agent.tools.evidence_ledger().as_of(), as_of);
    assert_eq!(
        agent
            .tools
            .research_context()
            .validated_report()
            .expect("validated report")
            .as_of,
        as_of
    );
}

#[tokio::test]
async fn fallback_without_disclosure_is_explicitly_unverified() {
    let as_of = NaiveDate::from_ymd_opt(2026, 7, 29).expect("as_of");
    let (address, server) = fixture_server(vec![
        r#"<html><head><meta property="article:published_time" content="2026-04-28T12:00:00Z"><title>Microsoft FY2026 Q3 reported results</title></head><body>Microsoft FY2026 Q3 quarterly results, period ended 2026-03-31; reported revenue.</body></html>"#,
    ]);
    let fixture_url = format!("http://investor.example.com:{}/q3-2026", address.port());
    let workspace = tempfile::tempdir().expect("workspace");
    let tools = full_research_tools(workspace.path(), fixture_client(address));
    let fetch_url = fixture_url.clone();
    let steps: Vec<ProviderStep> = vec![
        Box::new(move |_, _| vec![tool("fetch_url", serde_json::json!({"url": fetch_url}))]),
        Box::new(|_, _| {
            vec![tool(
                "resolve_latest_financial_report",
                serde_json::json!({"issuer": "Microsoft", "cadence": "annual"}),
            )]
        }),
        Box::new(|messages, _| {
            let resolution = resolution_from(messages);
            assert_eq!(resolution["status"], "fallback");
            assert_eq!(resolution["as_of"], "2026-07-29");
            assert_eq!(
                resolution["selected"]["candidate"]["report_type"],
                "quarterly"
            );
            vec![StreamChunk::TextDelta(report_content(&resolution, false))]
        }),
    ];

    let mut agent = make_agent(ScriptedProvider::new(steps), tools, workspace.path(), as_of);
    let output = agent
        .run_turn(
            "Find Microsoft's latest annual financial report.",
            workspace.path(),
            &[],
        )
        .await
        .expect("fallback compatibility turn");
    server.join().expect("fixture thread");

    assert!(output.contains("Verification status: unverified"));
    assert!(output.contains("quarterly"));
    assert!(output.contains("fallback"));
    assert!(output.contains("must not be treated as annual"));
    assert_eq!(agent.tools.evidence_ledger().as_of(), as_of);
}

#[tokio::test]
async fn recovery_flow_surfaces_mismatch_missing_metadata_conflict_and_errors() {
    let as_of = NaiveDate::from_ymd_opt(2026, 7, 29).expect("as_of");
    let (address, server) = fixture_server(vec![
        r#"<html><head><meta property="article:published_time" content="2026-07-20T12:00:00Z"><title>Apple FY2026 annual results</title></head><body>Apple FY2026 annual results, period ended 2026-06-30; reported revenue.</body></html>"#,
        r#"<html><head><meta property="article:published_time" content="2026-07-21T12:00:00Z"><title>Microsoft reported revenue</title></head><body>Microsoft reported revenue without a reporting period.</body></html>"#,
        r#"<html><head><meta property="article:published_time" content="2026-07-22T12:00:00Z"><title>Microsoft FY2025 annual results</title></head><body>Microsoft FY2025 annual results, period ended 2025-06-30; reported revenue.</body></html>"#,
        r#"<html><head><meta property="article:published_time" content="2026-07-23T12:00:00Z"><title>Microsoft FY2025 annual results correction</title></head><body>Microsoft FY2025 annual results, period ended 2025-06-30; reported revenue.</body></html>"#,
    ]);
    let base_url = format!("http://investor.example.com:{}", address.port());
    let urls = [
        format!("{base_url}/apple"),
        format!("{base_url}/missing-period"),
        format!("{base_url}/primary"),
        format!("{base_url}/correction"),
    ];
    let workspace = tempfile::tempdir().expect("workspace");
    let tools = full_research_tools(workspace.path(), fixture_client(address));
    let mismatch_url = urls[0].clone();
    let missing_url = urls[1].clone();
    let primary_url = urls[2].clone();
    let correction_url = urls[3].clone();
    let validation_url = primary_url.clone();
    let initial_validation_url = validation_url.clone();
    let steps: Vec<ProviderStep> = vec![
        Box::new(move |_, _| vec![tool("fetch_url", serde_json::json!({"url": mismatch_url}))]),
        Box::new(|_, _| {
            vec![tool(
                "resolve_latest_financial_report",
                serde_json::json!({"issuer": "Microsoft", "cadence": "annual"}),
            )]
        }),
        Box::new(move |messages, _| {
            let resolution = resolution_from(messages);
            assert_eq!(resolution["status"], "unavailable");
            assert!(
                resolution["limitation"]
                    .as_str()
                    .expect("mismatch limitation")
                    .contains("No eligible")
            );
            vec![tool("fetch_url", serde_json::json!({"url": missing_url}))]
        }),
        Box::new(|_, _| {
            vec![tool(
                "resolve_latest_financial_report",
                serde_json::json!({"issuer": "Microsoft", "cadence": "annual"}),
            )]
        }),
        Box::new(move |messages, _| {
            let resolution = resolution_from(messages);
            assert_eq!(resolution["status"], "unavailable");
            assert!(
                resolution["limitation"]
                    .as_str()
                    .expect("metadata limitation")
                    .contains("No eligible")
            );
            vec![tool("fetch_url", serde_json::json!({"url": primary_url}))]
        }),
        Box::new(move |_, _| {
            vec![tool(
                "validate_financial_report",
                serde_json::json!({
                    "issuer": "Microsoft",
                    "report_type": "annual",
                    "period_label": "FY2025",
                    "period_end": "2025-06-30",
                    "reporting_calendar": "fiscal",
                    "status": "reported",
                    "publication_url": initial_validation_url,
                    "publication_date": "2026-07-22T12:00:00Z",
                }),
            )]
        }),
        Box::new(move |_, _| {
            vec![tool(
                "fetch_url",
                serde_json::json!({"url": correction_url}),
            )]
        }),
        Box::new(|_, _| {
            vec![tool(
                "write_validated_financial_report",
                serde_json::json!({
                    "path": "protected-after-collector-conflict.md",
                    "content": "# Microsoft Financial Report\n\nRevenue remains unverified.",
                }),
            )]
        }),
        Box::new(|messages, _| {
            let error = messages
                .last()
                .expect("protected write after collector conflict")
                .content
                .to_summary_text();
            assert!(error.contains("no validated financial report"));
            vec![tool(
                "resolve_latest_financial_report",
                serde_json::json!({"issuer": "Microsoft", "cadence": "annual"}),
            )]
        }),
        Box::new(move |messages, _| {
            let resolution = resolution_from(messages);
            assert_eq!(resolution["status"], "conflict");
            assert!(resolution["selected"].is_null());
            assert!(
                !resolution["conflicts"]
                    .as_array()
                    .expect("conflicts")
                    .is_empty()
            );
            vec![tool(
                "validate_financial_report",
                serde_json::json!({
                    "issuer": "Microsoft",
                    "report_type": "annual",
                    "period_label": "FY2025",
                    "period_end": "2025-06-30",
                    "reporting_calendar": "fiscal",
                    "status": "reported",
                    "publication_url": validation_url,
                    "publication_date": "2026-07-22T12:00:00Z",
                }),
            )]
        }),
        Box::new(|messages, _| {
            let error = messages
                .last()
                .expect("conflict validation error")
                .content
                .to_summary_text();
            assert!(error.contains("conflicting official evidence"));
            vec![tool(
                "write_validated_financial_report",
                serde_json::json!({
                    "path": "protected-after-conflict.md",
                    "content": "# Microsoft Financial Report\n\nRevenue remains unverified.",
                }),
            )]
        }),
        Box::new(|messages, _| {
            let error = messages
                .last()
                .expect("protected write error")
                .content
                .to_summary_text();
            assert!(error.contains("no validated financial report"));
            vec![tool(
                "resolve_latest_financial_report",
                serde_json::json!({"issuer": "Microsoft", "cadence": "monthly"}),
            )]
        }),
        Box::new(|messages, _| {
            let error = messages
                .last()
                .expect("cadence error")
                .content
                .to_summary_text();
            assert!(error.contains("invalid field cadence"));
            vec![StreamChunk::TextDelta(
                "Recovery required: conflicting official evidence, a protected persistence refusal, and unsupported cadence left the Microsoft financial report revenue unverified.".into(),
            )]
        }),
    ];

    let mut agent = make_agent(ScriptedProvider::new(steps), tools, workspace.path(), as_of);
    let output = agent
        .run_turn(
            "Investigate Microsoft's annual financial report and recover from bad evidence.",
            workspace.path(),
            &[],
        )
        .await
        .expect("recovery compatibility turn");
    server.join().expect("fixture thread");

    assert!(output.contains("Verification status: unverified"));
    assert!(output.contains("conflicting official evidence"));
    assert!(output.contains("unsupported cadence"));
    assert!(
        !workspace
            .path()
            .join("protected-after-conflict.md")
            .exists()
    );
    assert!(agent.tools.research_context().validated_report().is_none());
    assert_eq!(agent.tools.evidence_ledger().as_of(), as_of);
    assert_eq!(agent.tools.evidence_ledger().observations().len(), 4);
}

#[tokio::test]
async fn generic_writing_and_skill_capability_boundaries_are_explicit() {
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let registry = ToolRegistry::with_default_full_tools(
        workspace_root.clone(),
        NcaConfig::default().web.clone(),
    );
    let generic_content = "# Microsoft Financial Report\n\nRevenue: draft and unverified.";
    let generic = registry
        .execute(&ToolCall {
            id: "generic-write".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path": "generic.md", "content": generic_content}),
        })
        .await;
    assert!(generic.success, "generic writer failed: {generic:?}");
    assert_eq!(
        tokio::fs::read_to_string(workspace.path().join("generic.md"))
            .await
            .expect("generic output"),
        generic_content
    );
    assert!(
        !registry
            .definitions()
            .iter()
            .any(|definition| definition.name == "write_validated_financial_report")
    );

    registry.enable_financial_research();
    let protected = registry
        .execute(&ToolCall {
            id: "protected-write".into(),
            name: "write_validated_financial_report".into(),
            input: serde_json::json!({"path": "protected.md", "content": generic_content}),
        })
        .await;
    assert!(!protected.success);
    assert!(
        protected
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no validated financial report"))
    );
    assert!(!workspace.path().join("protected.md").exists());

    let clean_discovery = SkillCatalog::discover(workspace.path(), &[]).expect("clean discovery");
    let financial = clean_discovery
        .iter()
        .find(|skill| skill.command == "financial-research")
        .expect("built-in financial skill");
    assert_eq!(financial.source, SkillSource::BuiltIn);
    assert!(!financial.allow_implicit_invocation);
    assert!(
        !SkillCatalog::discover_for_model(workspace.path(), &[])
            .expect("model discovery")
            .iter()
            .any(|skill| skill.command == "financial-research")
    );

    let capability_registry = ToolRegistry::with_default_full_tools(
        workspace_root.clone(),
        NcaConfig::default().web.clone(),
    );
    let capability = capability_registry.financial_research_capability();
    let implicit = InvokeSkillTool::new_with_financial_capability(
        workspace_root.clone(),
        vec![],
        Default::default(),
        capability.clone(),
    );
    let implicit_result = implicit
        .execute(&ToolCall {
            id: "implicit-skill".into(),
            name: "invoke_skill".into(),
            input: serde_json::json!({"skill_name": "financial-research"}),
        })
        .await;
    assert!(!implicit_result.success);
    assert!(!capability.load(Ordering::Acquire));
    assert!(
        !capability_registry
            .definitions()
            .iter()
            .any(|definition| definition.name == "resolve_latest_financial_report")
    );

    let explicit = InvokeSkillTool::new_with_financial_capability_and_explicit_skills(
        workspace_root,
        vec![],
        Default::default(),
        capability,
        vec!["financial-research".into()],
    );
    let explicit_result = explicit
        .execute(&ToolCall {
            id: "explicit-skill".into(),
            name: "invoke_skill".into(),
            input: serde_json::json!({"skill_name": "financial-research"}),
        })
        .await;
    assert!(
        explicit_result.success,
        "explicit skill failed: {explicit_result:?}"
    );
    assert!(
        capability_registry
            .definitions()
            .iter()
            .any(|definition| definition.name == "resolve_latest_financial_report")
    );
}
