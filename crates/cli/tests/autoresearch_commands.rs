use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

fn write_program(workspace: &std::path::Path) {
    fs::write(
        workspace.join("research.md"),
        "# CLI research\n\n## Metric\n- Command: printf 'metric: 0.42\\n'\n- Regex: metric:\\s*([0-9.]+)\n- Goal: minimize\n",
    )
    .expect("write research program");
}

fn nca(workspace: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("nca").expect("nca binary");
    command
        .current_dir(workspace)
        .env("HOME", workspace)
        .env("NCA_HOME", workspace.join("nca-home"));
    command
}

#[test]
fn autoresearch_cli_lifecycle_survives_process_restarts() {
    let temp = tempdir().expect("temporary workspace");
    write_program(temp.path());

    let started = nca(temp.path())
        .args([
            "autoresearch",
            "start",
            "--program",
            "research.md",
            "--workspace",
            ".",
            "--session-id",
            "cli-session",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let started: Value = serde_json::from_slice(&started).expect("start JSON");
    assert_eq!(started["session_id"], "cli-session");
    assert_eq!(started["status"], "running");
    assert_eq!(started["iteration_count"], 0);
    assert_eq!(started["program_name"], "CLI research");

    let status = nca(temp.path())
        .args([
            "autoresearch",
            "status",
            "cli-session",
            "--workspace",
            ".",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status).expect("status JSON");
    assert_eq!(status["status"], "running");
    assert_eq!(
        status["program_path"].as_str(),
        fs::canonicalize(temp.path().join("research.md"))
            .unwrap()
            .to_str()
    );

    nca(temp.path())
        .args(["autoresearch", "status", "cli-session", "--workspace", "."])
        .assert()
        .success()
        .stdout(predicates::str::contains("status=running"))
        .stdout(predicates::str::contains("iterations=0"));

    let stopped = nca(temp.path())
        .args([
            "autoresearch",
            "stop",
            "cli-session",
            "--workspace",
            ".",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stopped: Value = serde_json::from_slice(&stopped).expect("stop JSON");
    assert_eq!(stopped["status"], "stopped");

    nca(temp.path())
        .args(["autoresearch", "stop", "cli-session", "--workspace", "."])
        .assert()
        .failure()
        .stderr(predicates::str::contains("inactive"));

    let resumed = nca(temp.path())
        .args([
            "autoresearch",
            "resume",
            "cli-session",
            "--workspace",
            ".",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let resumed: Value = serde_json::from_slice(&resumed).expect("resume JSON");
    assert_eq!(resumed["status"], "running");
    assert_eq!(resumed["iteration_count"], 0);

    let results = nca(temp.path())
        .args([
            "autoresearch",
            "results",
            "cli-session",
            "--workspace",
            ".",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let results: Value = serde_json::from_slice(&results).expect("results JSON");
    assert_eq!(results["session"]["session_id"], "cli-session");
    assert_eq!(results["session"]["status"], "running");
    assert_eq!(results["results"].as_array().unwrap().len(), 0);

    nca(temp.path())
        .args([
            "autoresearch",
            "start",
            "--program",
            "research.md",
            "--workspace",
            ".",
            "--session-id",
            "cli-session",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already exists"));
}

#[test]
fn autoresearch_cli_rejects_missing_workspace_and_invalid_program() {
    let temp = tempdir().expect("temporary workspace");
    nca(temp.path())
        .args([
            "autoresearch",
            "start",
            "--program",
            "missing.md",
            "--workspace",
            "missing-workspace",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("workspace"));

    fs::write(
        temp.path().join("invalid.md"),
        "# Invalid research\n\n## Metric\n- Goal: minimize\n",
    )
    .expect("write invalid research program");
    nca(temp.path())
        .args([
            "autoresearch",
            "start",
            "--program",
            "invalid.md",
            "--workspace",
            ".",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("metric command"));
}
