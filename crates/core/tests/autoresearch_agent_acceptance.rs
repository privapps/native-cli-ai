use nca_common::config::{PermissionConfig, PermissionMode};
use nca_common::tool::{PermissionTier, ToolCall};
use nca_core::approval::ApprovalPolicy;
use nca_core::tools::ToolExecutor;
use nca_core::tools::autoresearch::AutoresearchTool;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

fn call(operation: &str, input: Value) -> ToolCall {
    let mut input = input;
    input["operation"] = Value::String(operation.to_string());
    ToolCall {
        id: format!("autoresearch-{operation}"),
        name: "autoresearch".into(),
        input,
    }
}

fn program_text() -> &'static str {
    r#"# Agent fixture

Improve a deterministic metric.

## Files
- Editable: `src/allowed.txt`
- Fixed: `src/fixed.txt`

## Metric
- Command: `grep "metric:" run.log`
- Regex: `metric:\s*([0-9.]+)`
- Goal: minimize

## Constraints
- Time budget: 5 seconds
- Max memory: 1GB
- no network access

## Instructions
Change only the editable file.
"#
}

fn git(workspace: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(workspace)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {:?} failed", args);
}

fn setup_workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::create_dir(workspace.path().join("src")).expect("src");
    fs::write(workspace.path().join("program.md"), program_text()).expect("program");
    fs::write(workspace.path().join("src/allowed.txt"), "baseline\n").expect("allowed");
    fs::write(workspace.path().join("src/fixed.txt"), "fixed\n").expect("fixed");
    git(workspace.path(), &["init", "-q"]);
    git(
        workspace.path(),
        &["config", "user.email", "agent@example.com"],
    );
    git(workspace.path(), &["config", "user.name", "Agent Fixture"]);
    git(workspace.path(), &["config", "commit.gpgsign", "false"]);
    git(workspace.path(), &["add", "."]);
    git(workspace.path(), &["commit", "-qm", "fixture"]);
    workspace
}

fn parse_output(result: &nca_common::tool::ToolResult) -> Value {
    assert!(result.success, "tool failed: {result:?}");
    serde_json::from_str(&result.output).expect("tool JSON output")
}

#[tokio::test]
async fn discovery_is_read_only_and_execution_requires_explicit_invocation() {
    let workspace = setup_workspace();
    let tool = AutoresearchTool::new(workspace.path().to_path_buf());

    let discovered = parse_output(&tool.execute(&call("discover", serde_json::json!({}))).await);
    assert_eq!(discovered["contract_version"], 1);
    assert_eq!(discovered["execution_started"], false);
    assert_eq!(discovered["programs"][0]["path"], "program.md");
    assert!(!workspace.path().join(".nca").exists());

    let refused = tool
        .execute(&call(
            "start",
            serde_json::json!({"program": "program.md", "session_id": "refused"}),
        ))
        .await;
    assert!(!refused.success);
    assert!(refused.error.unwrap().contains("explicit user invocation"));
}

#[tokio::test]
async fn explicit_launch_continuation_returns_failure_context_and_cancel_is_durable() {
    let workspace = setup_workspace();
    let tool = AutoresearchTool::new(workspace.path().to_path_buf());
    tool.authorize_explicit_invocation();

    let started = parse_output(
        &tool
            .execute(&call(
                "start",
                serde_json::json!({"program": "program.md", "session_id": "agent-1"}),
            ))
            .await,
    );
    assert_eq!(started["session"]["status"], "running");
    assert_eq!(started["execution_started"], false);

    let continued = parse_output(
        &tool
            .execute(&call(
                "continue",
                serde_json::json!({
                    "session_id": "agent-1",
                    "command": "sh",
                    "args": ["-c", "printf 'metric: 0.5\\npeak_vram_mb: 512\\n'; printf 'candidate\\n' > src/allowed.txt"],
                    "descriptions": ["write the deterministic candidate"],
                    "policy": {"max_iterations": 1, "max_total_seconds": 30}
                }),
            ))
            .await,
    );
    assert_eq!(continued["continuation"]["executed_iterations"], 1);
    assert_eq!(continued["results"][0]["result"]["status"], "keep");
    assert_eq!(continued["results"][0]["termination"], "completed");
    assert!(
        continued["results"][0]["stdout"]
            .as_str()
            .unwrap()
            .contains("metric: 0.5")
    );
    assert_eq!(continued["session"]["iteration_count"], 1);

    let policy_refused = tool
        .execute(&call(
            "continue",
            serde_json::json!({
                "session_id": "agent-1",
                "command": "true",
                "descriptions": ["one", "two"],
                "policy": {"max_iterations": 1, "max_total_seconds": 30}
            }),
        ))
        .await;
    assert!(!policy_refused.success);
    assert!(
        policy_refused
            .error
            .unwrap()
            .contains("exceed the continuation's")
    );

    let results = parse_output(
        &tool
            .execute(&call(
                "results",
                serde_json::json!({"session_id": "agent-1"}),
            ))
            .await,
    );
    assert_eq!(results["results"].as_array().unwrap().len(), 1);

    let cancelled = parse_output(
        &tool
            .execute(&call(
                "cancel",
                serde_json::json!({"session_id": "agent-1"}),
            ))
            .await,
    );
    assert_eq!(cancelled["session"]["status"], "stopped");

    let refused_continuation = tool
        .execute(&call(
            "continue",
            serde_json::json!({
                "session_id": "agent-1",
                "command": "true",
                "descriptions": ["must not run after cancellation"]
            }),
        ))
        .await;
    assert!(!refused_continuation.success);
    assert!(refused_continuation.error.unwrap().contains("not running"));
}

#[tokio::test]
async fn failed_experiment_delivers_termination_and_stderr_context() {
    let workspace = setup_workspace();
    let tool = AutoresearchTool::new(workspace.path().to_path_buf());
    tool.authorize_explicit_invocation();

    parse_output(
        &tool
            .execute(&call(
                "start",
                serde_json::json!({"program": "program.md", "session_id": "agent-failure"}),
            ))
            .await,
    );
    let failure = parse_output(
        &tool
            .execute(&call(
                "continue",
                serde_json::json!({
                    "session_id": "agent-failure",
                    "command": "sh",
                    "args": ["-c", "printf 'training failed' >&2; exit 2"],
                    "descriptions": ["run a deliberately failing fixture"],
                    "policy": {"max_iterations": 1, "max_total_seconds": 30}
                }),
            ))
            .await,
    );
    assert_eq!(failure["results"][0]["result"]["status"], "crash");
    assert_eq!(failure["results"][0]["termination"], "crashed");
    assert_eq!(failure["results"][0]["decision"], "failure");
    assert!(
        failure["results"][0]["stderr"]
            .as_str()
            .unwrap()
            .contains("training failed")
    );
}

#[test]
fn autoresearch_skill_is_discoverable_but_manual_only() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory")
        .parent()
        .expect("repository directory");
    let skills = nca_core::skills::SkillCatalog::discover(
        repo,
        &[std::path::PathBuf::from(".agents/skills")],
    )
    .expect("discover repository skills");
    let skill = skills
        .iter()
        .find(|skill| skill.command == "autoresearch")
        .expect("autoresearch skill");
    assert!(!skill.allow_implicit_invocation);
}

#[test]
fn autoresearch_execution_is_an_approval_boundary() {
    let policy = ApprovalPolicy::new(PermissionConfig {
        mode: PermissionMode::Default,
        ..Default::default()
    });
    assert_eq!(
        policy.check("autoresearch", r#"{"operation":"continue"}"#),
        PermissionTier::Ask
    );
}
