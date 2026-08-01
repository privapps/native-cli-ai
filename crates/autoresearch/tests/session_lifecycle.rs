use nca_autoresearch::{ExperimentResult, ExperimentStatus, SessionStatus, SessionStore};
use std::fs;
use tempfile::tempdir;

fn write_program(workspace: &std::path::Path) -> std::path::PathBuf {
    let path = workspace.join("research.md");
    fs::write(
        &path,
        "# Durable research\n\n## Metric\n- Command: `printf 'metric: 0.42\\n'`\n- Regex: `metric:\\s*([0-9.]+)`\n- Goal: minimize\n",
    )
    .expect("write research program");
    path
}

#[test]
fn session_state_and_results_survive_store_reload_and_lifecycle_transitions() {
    let temp = tempdir().expect("temporary workspace");
    let program = write_program(temp.path());
    let store = SessionStore::for_workspace(temp.path());

    let started = store
        .start(Some("durable"), &program, temp.path())
        .expect("start session");
    assert_eq!(started.session_id, "durable");
    assert_eq!(started.status, SessionStatus::Running);
    assert_eq!(started.program_name, "Durable research");
    assert_eq!(started.iteration_count, 0);
    assert!(started.baseline_metric.is_none());
    assert!(started.best_result.is_none());

    let reloaded = SessionStore::for_workspace(temp.path())
        .load("durable")
        .expect("reload session");
    assert_eq!(reloaded, started);

    let result = ExperimentResult::new(
        "abc1234".into(),
        0.42,
        1.0,
        2.0,
        2.5,
        ExperimentStatus::Keep,
        "baseline".into(),
    );
    let with_result = store
        .record_result("durable", result.clone())
        .expect("record result");
    assert_eq!(with_result.iteration_count, 1);
    assert_eq!(with_result.baseline_metric, Some(0.42));
    assert_eq!(with_result.best_result, Some(result));

    let reloaded = SessionStore::for_workspace(temp.path())
        .load("durable")
        .expect("reload result state");
    assert_eq!(reloaded.iteration_count, 1);
    assert_eq!(
        SessionStore::for_workspace(temp.path())
            .results("durable")
            .expect("load results")
            .len(),
        1
    );

    let stopped = store.stop("durable").expect("stop running session");
    assert_eq!(stopped.status, SessionStatus::Stopped);
    assert!(
        store.stop("durable").is_err(),
        "stopping inactive must fail"
    );

    let resumed = store.resume("durable").expect("resume stopped session");
    assert_eq!(resumed.status, SessionStatus::Running);
    assert_eq!(resumed.iteration_count, 1);

    assert!(
        store.start(Some("durable"), &program, temp.path()).is_err(),
        "starting an existing session must fail"
    );
}

#[test]
fn next_experiment_id_repeats_after_restart_until_result_is_durable() {
    let temp = tempdir().expect("temporary workspace");
    let program = write_program(temp.path());
    let store = SessionStore::for_workspace(temp.path());
    store
        .start(Some("recoverable"), &program, temp.path())
        .expect("start session");

    assert_eq!(
        store.next_experiment_id("recoverable").unwrap(),
        "recoverable-1"
    );
    let restarted = SessionStore::for_workspace(temp.path());
    assert_eq!(
        restarted.next_experiment_id("recoverable").unwrap(),
        "recoverable-1",
        "an interrupted run must be resumed by identity, not renamed"
    );

    restarted
        .record_result(
            "recoverable",
            ExperimentResult::new(
                "abc1234".into(),
                0.42,
                1.0,
                2.0,
                2.5,
                ExperimentStatus::Keep,
                "completed".into(),
            ),
        )
        .unwrap();
    assert_eq!(
        restarted.next_experiment_id("recoverable").unwrap(),
        "recoverable-2"
    );
}

#[test]
fn session_start_rejects_malformed_metric_regex_before_persisting_state() {
    let temp = tempdir().expect("temporary workspace");
    let program = temp.path().join("invalid-regex.md");
    fs::write(
        &program,
        "# Invalid regex\n\n## Metric\n- Command: `printf 'metric: 0.42\\n'`\n- Regex: `metric: ([`\n",
    )
    .expect("write invalid research program");

    let store = SessionStore::for_workspace(temp.path());
    let error = store
        .start(Some("invalid-regex"), &program, temp.path())
        .expect_err("malformed regex must reject session start");

    assert!(error.to_string().contains("invalid metric regex"));
    assert!(
        !store.root().exists(),
        "invalid sessions must not persist state"
    );
}

#[test]
fn session_start_rejects_metric_regex_without_capture_group() {
    let temp = tempdir().expect("temporary workspace");
    let program = temp.path().join("missing-capture.md");
    fs::write(
        &program,
        "# Missing capture\n\n## Metric\n- Command: `printf 'metric: 0.42\\n'`\n- Regex: `metric: [0-9.]+`\n",
    )
    .expect("write research program");

    let error = SessionStore::for_workspace(temp.path())
        .start(Some("missing-capture"), &program, temp.path())
        .expect_err("regex without capture group must reject session start");

    assert!(
        error
            .to_string()
            .contains("metric regex must contain a capture group")
    );
}
