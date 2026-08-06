use assert_cmd::Command;
use chrono::{Duration, Utc};
use nca_common::message::{Message, MessageToolCall};
use nca_common::session::{SessionMeta, SessionState, SessionStatus};
use nca_common::todo::{AgentTodo, TodoStatus};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command as ProcessCommand;
#[cfg(unix)]
use std::process::Stdio;
use tempfile::tempdir;

fn write_local_config(workspace: &Path) {
    write_local_config_contents(workspace, "[provider.minimax]\napi_key = \"test-key\"\n");
}

fn write_local_config_contents(workspace: &Path, contents: &str) {
    let config_dir = workspace.join(".nca");
    fs::create_dir_all(&config_dir).expect("create config dir");
    fs::write(config_dir.join("config.local.toml"), contents).expect("write local config");
}

fn write_skill(workspace: &Path, command: &str, contents: &str) {
    let directory = workspace.join(".agents/skills").join(command);
    fs::create_dir_all(&directory).expect("create skill directory");
    fs::write(directory.join("SKILL.md"), contents).expect("write skill");
}

fn write_session(
    workspace: &Path,
    id: &str,
    updated_at: chrono::DateTime<Utc>,
    model: &str,
    status: SessionStatus,
) {
    write_session_state(
        workspace,
        id,
        updated_at,
        model,
        status,
        vec![Message::user("hello")],
        Vec::new(),
        Vec::new(),
    );
}

#[allow(clippy::too_many_arguments)]
fn write_session_state(
    workspace: &Path,
    id: &str,
    updated_at: chrono::DateTime<Utc>,
    model: &str,
    status: SessionStatus,
    messages: Vec<Message>,
    prompt_history: Vec<String>,
    todos: Vec<AgentTodo>,
) {
    let sessions_dir = workspace.join(".nca").join("sessions");
    fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let session = SessionState {
        meta: SessionMeta {
            id: id.to_string(),
            created_at: updated_at - Duration::minutes(1),
            updated_at,
            workspace: workspace.to_path_buf(),
            model: model.to_string(),
            status,
            pid: None,
            socket_path: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            parent_session_id: None,
            child_session_ids: Vec::new(),
            inherited_summary: None,
            spawn_reason: None,
            session_summary: None,
            orchestration: None,
            execution: Default::default(),
        },
        messages,
        prompt_history,
        total_input_tokens: 0,
        total_output_tokens: 0,
        estimated_cost_usd: 0.0,
        todos,
        completion_claim: None,
    };

    let json = serde_json::to_string_pretty(&session).expect("serialize session");
    fs::write(sessions_dir.join(format!("{id}.json")), json).expect("write session");
}

fn write_event_log(workspace: &Path, id: &str, lines: &str) {
    let sessions_dir = workspace.join(".nca").join("sessions");
    fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    fs::write(sessions_dir.join(format!("{id}.events.jsonl")), lines).expect("write event log");
}

fn process_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        ProcessCommand::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    {
        let Ok(output) = ProcessCommand::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
        else {
            return false;
        };
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(&format!(",\"{pid}\","))
    }
}

#[test]
fn run_without_config_exits_nonzero() {
    let temp = tempdir().expect("tempdir");

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env_remove("MINIMAX_API_KEY")
        .arg("run")
        .arg("--prompt")
        .arg("hello")
        .arg("--stream")
        .arg("off")
        .assert()
        .failure()
        .code(10)
        .stderr(predicates::str::contains("missing MiniMax API key"));
}

#[test]
fn skills_lists_workspace_agents_directory_human_readably() {
    let temp = tempdir().expect("tempdir");
    write_skill(
        temp.path(),
        "cli-visible",
        "---\nname: CLI Visible\ncommand: cli-visible\ndescription: Check CLI discovery\n---\nUse this skill.\n",
    );

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NCA_HOME", temp.path().join("nca-home"))
        .arg("skills")
        .assert()
        .success()
        .stdout(predicates::str::contains("/cli-visible"))
        .stdout(predicates::str::contains("Check CLI discovery"));
}

#[test]
fn skills_json_lists_workspace_agents_directory_with_metadata() {
    let temp = tempdir().expect("tempdir");
    write_skill(
        temp.path(),
        "cli-json",
        "---\nname: CLI JSON\ncommand: cli-json\ndescription: Machine-readable discovery\n---\nUse this skill.\n",
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NCA_HOME", temp.path().join("nca-home"))
        .args(["skills", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let skills: Value = serde_json::from_slice(&output).expect("skills JSON");
    let skill = skills
        .as_array()
        .expect("skill array")
        .iter()
        .find(|skill| skill["command"] == "cli-json")
        .expect("workspace skill");
    assert_eq!(skill["source"], "filesystem");
    assert!(
        skill["directory"]
            .as_str()
            .unwrap()
            .ends_with(".agents/skills/cli-json")
    );
    assert_eq!(skill["description"], "Machine-readable discovery");
}

#[test]
fn sessions_lists_newest_saved_sessions_first_with_status() {
    let temp = tempdir().expect("tempdir");
    let now = Utc::now();

    write_session(
        temp.path(),
        "session-older",
        now - Duration::minutes(5),
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );
    write_session(
        temp.path(),
        "session-newer",
        now,
        "MiniMax-M2.5",
        SessionStatus::Cancelled,
    );

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("sessions")
        .assert()
        .success()
        .stdout(predicates::str::contains("session-newer  status=Cancelled"))
        .stdout(predicates::str::contains("session-older  status=Completed"));
}

/// Same selection rule as `nca --resume` (latest `updated_at`).
#[tokio::test]
async fn resume_targets_session_with_latest_updated_at() {
    let temp = tempdir().expect("tempdir");
    let now = Utc::now();

    write_local_config(temp.path());
    write_session(
        temp.path(),
        "session-older",
        now - Duration::minutes(5),
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );
    write_session(
        temp.path(),
        "session-newer",
        now,
        "MiniMax-M2.5-latest",
        SessionStatus::Completed,
    );

    let store =
        nca_runtime::session_store::SessionStore::new(temp.path().join(".nca").join("sessions"));
    let ids = store.list().await.expect("list");
    let mut latest: Option<(String, chrono::DateTime<Utc>)> = None;
    for id in ids {
        let Ok(session) = store.load(&id).await else {
            continue;
        };
        let replace = latest
            .as_ref()
            .map(|(_, t)| session.meta.updated_at > *t)
            .unwrap_or(true);
        if replace {
            latest = Some((session.meta.id, session.meta.updated_at));
        }
    }
    assert_eq!(latest.map(|(id, _)| id).as_deref(), Some("session-newer"));
}

#[test]
fn sessions_json_emits_sorted_machine_snapshots() {
    let temp = tempdir().expect("tempdir");
    let now = Utc::now();

    write_session(
        temp.path(),
        "session-older",
        now - Duration::minutes(5),
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );
    write_session(
        temp.path(),
        "session-newer",
        now,
        "MiniMax-M2.5",
        SessionStatus::Running,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("sessions")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    let sessions = payload["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions[0]["id"], "session-newer");
    assert_eq!(sessions[0]["status"], "running");
    assert_eq!(sessions[1]["id"], "session-older");
    assert!(
        payload["unreadable"]
            .as_array()
            .expect("unreadable array")
            .is_empty()
    );
}

#[test]
fn status_outputs_session_snapshot_json() {
    let temp = tempdir().expect("tempdir");
    let now = Utc::now();
    write_session(
        temp.path(),
        "session-status",
        now,
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("status")
        .arg("session-status")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["id"], "session-status");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["model"], "MiniMax-M2.5");
    assert_eq!(payload["estimated_cost_usd"], 0.0);
    assert!(
        payload["state_path"]
            .as_str()
            .is_some_and(|path| path.ends_with("session-status.json"))
    );
    assert!(
        payload["events_path"]
            .as_str()
            .is_some_and(|path| path.ends_with("session-status.events.jsonl"))
    );
}

#[test]
fn session_list_and_status_expose_authoritative_human_and_json_paths() {
    let workspace = tempdir().expect("temporary workspace");
    let nca_home = workspace.path().join("nca-home");
    let now = Utc::now();
    write_session(
        workspace.path(),
        "session-paths",
        now,
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );
    write_event_log(
        workspace.path(),
        "session-paths",
        "{\"id\":1,\"event\":{\"type\":\"SessionEnded\",\"reason\":\"Completed\"}}\n",
    );

    let human = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .env("NCA_HOME", &nca_home)
        .args(["sessions", "--workspace", "."])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8_lossy(&human);
    assert!(human.contains("state:"));
    assert!(human.contains("events:"));

    let json = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .env("NCA_HOME", &nca_home)
        .args(["status", "session-paths", "--workspace", ".", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&json).expect("status JSON");
    let state_path = Path::new(payload["state_path"].as_str().expect("state path"));
    let events_path = Path::new(payload["events_path"].as_str().expect("events path"));
    assert!(state_path.is_absolute());
    assert!(events_path.is_absolute());
    assert!(state_path.is_file());
    assert!(events_path.is_file());
    assert_eq!(
        state_path.file_name().and_then(|name| name.to_str()),
        Some("session-paths.json")
    );
    assert_eq!(
        events_path.file_name().and_then(|name| name.to_str()),
        Some("session-paths.events.jsonl")
    );
}

#[test]
fn sessions_searches_all_persisted_evidence_and_legacy_user_messages() {
    let workspace = tempdir().expect("temporary workspace");
    let nca_home = workspace.path().join("nca-home");
    let now = Utc::now();
    write_session_state(
        workspace.path(),
        "session-evidence",
        now,
        "MiniMax-M2.5",
        SessionStatus::Completed,
        vec![
            Message::assistant_with_tool_calls(
                "",
                vec![MessageToolCall {
                    id: "call-evidence".into(),
                    name: "evidence_tool_name".into(),
                    arguments: serde_json::json!({"needle": "evidence_tool_argument"}),
                }],
            ),
            Message::tool("call-evidence", "evidence_tool_result"),
            Message::assistant("evidence_assistant_message"),
        ],
        vec!["evidence_prompt_history".into()],
        vec![AgentTodo {
            id: "todo-evidence".into(),
            content: "evidence_todo_content".into(),
            status: TodoStatus::InProgress,
            source: None,
        }],
    );
    write_session_state(
        workspace.path(),
        "session-legacy",
        now - Duration::minutes(1),
        "MiniMax-M2.5",
        SessionStatus::Completed,
        vec![Message::user("evidence_legacy_user_message")],
        Vec::new(),
        Vec::new(),
    );

    for query in [
        "evidence_prompt_history",
        "evidence_todo_content",
        "in_progress",
        "evidence_assistant_message",
        "evidence_tool_name",
        "evidence_tool_argument",
        "evidence_tool_result",
    ] {
        let output = Command::cargo_bin("nca")
            .expect("binary")
            .current_dir(workspace.path())
            .env("HOME", workspace.path())
            .env("NCA_HOME", &nca_home)
            .args(["sessions", "--search", query, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let payload: Value = serde_json::from_slice(&output).expect("search JSON");
        let sessions = payload["sessions"].as_array().expect("sessions array");
        assert!(
            sessions
                .iter()
                .any(|session| session["id"] == "session-evidence"),
            "query {query:?}"
        );
    }

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .env("NCA_HOME", &nca_home)
        .args([
            "sessions",
            "--search",
            "evidence_legacy_user_message",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).expect("legacy search JSON");
    assert!(
        payload["sessions"]
            .as_array()
            .expect("sessions array")
            .iter()
            .any(|session| session["id"] == "session-legacy")
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .env("NCA_HOME", &nca_home)
        .args([
            "sessions",
            "--search",
            "empty_assistant_placeholder",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).expect("empty assistant search JSON");
    assert!(
        payload["sessions"]
            .as_array()
            .expect("sessions array")
            .is_empty()
    );
}

#[test]
fn workspace_option_selects_sessions_from_another_current_directory_and_rejects_invalid_paths() {
    let workspace = tempdir().expect("target workspace");
    let current = tempdir().expect("unrelated current directory");
    let nca_home = current.path().join("nca-home");
    write_session(
        workspace.path(),
        "session-explicit-workspace",
        Utc::now(),
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(current.path())
        .env("HOME", current.path())
        .env("NCA_HOME", &nca_home)
        .args([
            "sessions",
            "--workspace",
            workspace.path().to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).expect("workspace JSON");
    assert_eq!(payload["sessions"][0]["id"], "session-explicit-workspace");

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(current.path())
        .env("HOME", current.path())
        .env("NCA_HOME", &nca_home)
        .args([
            "sessions",
            "--workspace",
            current.path().join("missing").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid workspace"));
}

#[test]
fn cancel_json_updates_session_snapshot() {
    let temp = tempdir().expect("tempdir");
    let now = Utc::now();
    write_session(
        temp.path(),
        "session-cancel",
        now,
        "MiniMax-M2.5",
        SessionStatus::Running,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("cancel")
        .arg("session-cancel")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["cancelled"], true);
    assert_eq!(payload["session"]["status"], "cancelled");
}

#[test]
fn attach_falls_back_to_enveloped_event_log() {
    let temp = tempdir().expect("tempdir");
    let now = Utc::now();
    write_session(
        temp.path(),
        "session-log",
        now,
        "MiniMax-M2.5",
        SessionStatus::Completed,
    );
    write_event_log(
        temp.path(),
        "session-log",
        "{\"id\":1,\"ts\":\"2026-03-14T00:00:00Z\",\"event\":{\"type\":\"SessionEnded\",\"reason\":\"Completed\"}}\n",
    );

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("attach")
        .arg("session-log")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "\"event\":{\"type\":\"SessionEnded\"",
        ));
}

#[test]
fn spawn_json_reports_machine_paths() {
    let temp = tempdir().expect("tempdir");
    write_local_config(temp.path());
    let nca_home = temp.path().join("nca-home");
    let runtime_dir = temp.path().join("runtime");
    fs::create_dir_all(runtime_dir.join("nca")).expect("create isolated runtime directory");

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NCA_HOME", &nca_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env_remove("MINIMAX_API_KEY")
        .arg("spawn")
        .arg("--prompt")
        .arg("hello")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    let session_id = payload["session_id"].as_str().expect("session id");
    assert!(session_id.starts_with("session-"));
    assert!(
        payload["spawn_log_path"]
            .as_str()
            .expect("spawn log path")
            .ends_with(".spawn.log")
    );
    assert!(
        payload["event_log_path"]
            .as_str()
            .expect("event log path")
            .ends_with(".events.jsonl")
    );

    let status_path = payload["status_path"].as_str().expect("status path");
    let status: Value =
        serde_json::from_str(&fs::read_to_string(status_path).expect("spawned session metadata"))
            .expect("session metadata should be valid JSON");
    assert_eq!(payload["socket_path"], status["meta"]["socket_path"]);

    let pid = payload["pid"]
        .as_u64()
        .expect("spawned child pid")
        .try_into()
        .expect("pid should fit in u32");
    let socket_path = Path::new(
        payload["socket_path"]
            .as_str()
            .expect("socket path should be present"),
    )
    .to_path_buf();
    let cancel = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NCA_HOME", &nca_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .arg("cancel")
        .arg(session_id)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let cancelled: Value = serde_json::from_slice(&cancel).expect("cancel response should be JSON");
    assert_eq!(cancelled["cancelled"], true);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while (process_is_alive(pid) || socket_path.exists()) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(!process_is_alive(pid), "spawned child should be terminated");
    assert!(
        !socket_path.exists(),
        "spawned child IPC endpoint should be removed: {}",
        socket_path.display()
    );
}

#[test]
fn spawn_startup_failure_reports_ipc_context() {
    let temp = tempdir().expect("tempdir");
    write_local_config(temp.path());
    let nca_home = temp.path().join("nca-home");
    let runtime_dir = temp.path().join("runtime");
    fs::write(&runtime_dir, "runtime path is occupied by a file")
        .expect("runtime collision fixture should be created");

    let assertion = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("NCA_HOME", &nca_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env_remove("MINIMAX_API_KEY")
        .arg("spawn")
        .arg("--prompt")
        .arg("hello")
        .arg("--json")
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr);
    assert!(stderr.contains("spawned session session-"));
    assert!(stderr.contains("failed to prepare IPC directory"));
    assert!(stderr.contains(&runtime_dir.join("nca").display().to_string()));
    assert!(stderr.contains(".sock"));
    assert!(
        runtime_dir.is_file(),
        "runtime collision fixture should remain"
    );
    assert!(
        !runtime_dir.join("nca").exists(),
        "startup failure should not leave an IPC endpoint directory"
    );
}

#[test]
fn models_json_lists_all_provider_models() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "openai"

[provider.minimax]
api_key = "minimax-key"

[provider.openai]
api_key = "openai-key"
model = "gpt-4o"

[provider.anthropic]
api_key = "anthropic-key"
model = "claude-3-7-sonnet-latest"

[provider.openrouter]
api_key = "openrouter-key"
model = "openai/gpt-4o-mini"
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("models")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["default_provider"], "OpenAI");
    assert_eq!(payload["default_model"], "gpt-4o");
    let provider_models = payload["provider_models"]
        .as_array()
        .expect("provider_models array");
    assert_eq!(provider_models.len(), 5);
    assert!(provider_models.iter().any(|entry| {
        entry["provider"] == "OpenAI" && entry["model"] == "gpt-4o" && entry["selected"] == true
    }));
    assert!(provider_models.iter().any(|entry| {
        entry["provider"] == "Custom"
            && entry["model"] == "custom-model"
            && entry["selected"] == false
    }));
}

#[test]
fn models_json_reports_reasoning_effort_and_active_provider_scope() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "custom"

[provider.custom]
api_key = "custom-key"
base_url = "https://gateway.example"
compatibility = "anthropic"

[model]
reasoning_effort = "low"
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("models")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["reasoning_effort"], "low");
    assert_eq!(payload["reasoning_effort_scope"], "OpenAI-compatible only");
    assert_eq!(payload["reasoning_effort_active"], false);
}

#[test]
fn config_json_reports_reasoning_effort_and_active_provider_scope() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "custom"

[provider.custom]
api_key = "custom-key"
base_url = "https://gateway.example"
compatibility = "anthropic"

[model]
reasoning_effort = "low"
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("config")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["reasoning_effort"], "low");
    assert_eq!(payload["reasoning_effort_scope"], "OpenAI-compatible only");
    assert_eq!(payload["reasoning_effort_active"], false);
}

#[test]
fn config_json_preserves_configured_max_tokens_without_cli_override() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[model]
max_tokens = 64000
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env_remove("NCA_HOME")
        .env_remove("XDG_DATA_HOME")
        .arg("config")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["model"]["max_tokens"], 64000);
}

#[test]
fn max_tokens_cli_override_takes_precedence_over_config() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[model]
max_tokens = 64000
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env_remove("NCA_HOME")
        .env_remove("XDG_DATA_HOME")
        .args(["--max-tokens", "12345", "config", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["model"]["max_tokens"], 12345);
}

#[test]
fn reasoning_effort_cli_override_is_run_scoped() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "minimax"

[provider.minimax]
api_key = "minimax-key"

[model]
reasoning_effort = "low"
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("--reasoning-effort")
        .arg("nil")
        .arg("models")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["reasoning_effort"], "nil");
    let persisted =
        fs::read_to_string(temp.path().join(".nca/config.local.toml")).expect("persisted config");
    assert!(persisted.contains("reasoning_effort = \"low\""));
}

#[test]
fn non_nil_reasoning_effort_cli_override_is_run_scoped() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "minimax"

[provider.minimax]
api_key = "minimax-key"

[model]
reasoning_effort = "low"
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .arg("--reasoning-effort")
        .arg("high")
        .arg("models")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["reasoning_effort"], "high");
    let persisted =
        fs::read_to_string(temp.path().join(".nca/config.local.toml")).expect("persisted config");
    assert!(persisted.contains("reasoning_effort = \"low\""));
}

#[test]
fn doctor_json_reports_provider_readiness_for_all_backends() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "anthropic"

[provider.minimax]
api_key = "minimax-key"

[provider.anthropic]
api_key = "anthropic-key"
model = "claude-3-7-sonnet-latest"
"#,
    );

    let output = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env_remove("OPENAI_API_KEY")
        .arg("doctor")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let payload: Value = serde_json::from_slice(&output).expect("json");
    assert_eq!(payload["provider"], "Anthropic");
    assert_eq!(payload["default_model"], "claude-3-7-sonnet-latest");
    let providers = payload["providers"].as_array().expect("providers array");
    assert_eq!(providers.len(), 5);
    assert!(providers.iter().any(|entry| {
        entry["provider"] == "Anthropic"
            && entry["selected"] == true
            && entry["api_key_present"] == true
    }));
    assert!(providers.iter().any(|entry| {
        entry["provider"] == "OpenAI"
            && entry["selected"] == false
            && entry["api_key_present"] == false
    }));
    assert!(providers.iter().any(|entry| {
        entry["provider"] == "Custom"
            && entry["selected"] == false
            && entry["api_key_present"] == false
    }));
}

#[test]
fn index_build_writes_cli_index_json_under_nca_home() {
    let ws = tempdir().expect("ws");
    let home = tempdir().expect("home");
    write_local_config(ws.path());

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(ws.path())
        .env("HOME", home.path())
        .args(["index", "build"])
        .assert()
        .success()
        .stdout(predicates::str::contains("wrote"));

    let workspaces = home.path().join(".local/share/ncacli/workspaces");
    assert!(workspaces.is_dir(), "expected {:?}", workspaces);
    let mut index_path = None;
    for entry in fs::read_dir(&workspaces).expect("read workspaces") {
        let p = entry.expect("entry").path().join("cli-index.json");
        if p.is_file() {
            index_path = Some(p);
            break;
        }
    }
    let index_path = index_path.expect("cli-index.json under workspaces");
    let raw = fs::read_to_string(&index_path).expect("read index");
    let v: Value = serde_json::from_str(&raw).expect("parse index");
    assert_eq!(v["schema_version"], 1);
    let cmds = v["commands"].as_array().expect("commands");
    assert!(!cmds.is_empty());
    assert!(cmds.iter().any(|c| c["path"] == serde_json::json!(["run"])));
}

#[test]
fn index_show_and_build_json_status() {
    let ws = tempdir().expect("ws");
    let home = tempdir().expect("home");
    write_local_config(ws.path());

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(ws.path())
        .env("HOME", home.path())
        .args(["index", "build"])
        .assert()
        .success();

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(ws.path())
        .env("HOME", home.path())
        .args(["index", "show"])
        .assert()
        .success()
        .stdout(predicates::str::contains("CLI index"));

    Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(ws.path())
        .env("HOME", home.path())
        .args(["index", "show", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("schema_version"));

    let out = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(ws.path())
        .env("HOME", home.path())
        .args(["index", "build", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&out).expect("status json");
    assert!(status["path"].as_str().unwrap().contains("cli-index.json"));
    assert!(status["workspace_id"].as_str().unwrap().len() > 10);
}

#[test]
fn responses_compatibility_is_visible_in_status_models_and_doctor_diagnostics() {
    let temp = tempdir().expect("tempdir");
    write_local_config_contents(
        temp.path(),
        r#"
[provider]
default = "custom"

[provider.custom]
compatibility = "openai-responses"
api_key = "responses-key"
base_url = "https://gateway.example/v1"
model = "responses-model"
"#,
    );
    write_session(
        temp.path(),
        "responses-status",
        Utc::now(),
        "responses-model",
        SessionStatus::Completed,
    );

    let doctor = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .args(["doctor", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doctor: Value = serde_json::from_slice(&doctor).expect("doctor JSON");
    assert_eq!(doctor["provider"], "Custom");
    assert_eq!(doctor["compatibility"], "OpenAI Responses");
    assert!(
        doctor["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .any(|provider| provider["provider"] == "Custom"
                && provider["compatibility"] == "OpenAI Responses"
                && provider["selected"] == true)
    );

    let models = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .args(["models", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let models: Value = serde_json::from_slice(&models).expect("models JSON");
    assert!(
        models["provider_models"]
            .as_array()
            .expect("provider models")
            .iter()
            .any(|provider| provider["provider"] == "Custom"
                && provider["compatibility"] == "OpenAI Responses")
    );

    let status = Command::cargo_bin("nca")
        .expect("binary")
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .args(["status", "responses-status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status).expect("status JSON");
    assert_eq!(status["provider"], "Custom");
    assert_eq!(status["compatibility"], "OpenAI Responses");
}
