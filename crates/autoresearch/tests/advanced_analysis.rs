use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use chrono::Utc;
use nca_autoresearch::{
    AnalysisOptions, ConstraintKind, ConstraintOperator, ConstraintSet, ExperimentDecision,
    ExperimentResult, ExperimentStatus, MetricGoal, ParallelScheduler, ProgressReport,
    ResearchProgram, analyze_results,
};

fn result(metric: f64, status: ExperimentStatus, description: &str) -> ExperimentResult {
    ExperimentResult {
        commit: description.into(),
        metric_value: metric,
        memory_gb: 1.0,
        training_seconds: 1.0,
        total_seconds: 1.0,
        status,
        description: description.into(),
        timestamp: Utc::now(),
        peak_vram_mb: None,
        mfu_percent: None,
        total_tokens_m: None,
        num_steps: None,
        num_params_m: None,
    }
}

#[test]
fn markdown_programs_retain_typed_secondary_constraints() {
    let program = ResearchProgram::from_markdown(
        "# bounded\n\n## Metric\n- Command: `echo metric`\n- Regex: `metric: ([0-9.]+)`\n- Goal: minimize\n\n## Constraints\n- Time budget: 30 seconds\n- Max memory: 8 GB\n- Accuracy >= 0.90\n",
    )
    .unwrap();
    let constraints = ConstraintSet::from_program(&program);

    assert_eq!(constraints.constraints.len(), 2);
    assert_eq!(constraints.constraints[1].name, "accuracy");
    assert_eq!(constraints.constraints[1].kind, ConstraintKind::Quality);
    assert_eq!(
        constraints.constraints[1].operator,
        ConstraintOperator::GreaterThanOrEqual
    );
}

#[test]
fn a_single_metric_program_keeps_the_sequential_no_constraint_path() {
    let program = ResearchProgram::from_markdown(
        "# sequential\n\n## Metric\n- Command: `echo metric`\n- Regex: `metric: ([0-9.]+)`\n- Goal: minimize\n",
    )
    .unwrap();
    let constraints = ConstraintSet::from_program(&program);
    assert!(constraints.constraints.is_empty());
    assert_eq!(constraints.primary_goal, MetricGoal::Minimize);
}

#[test]
fn constraint_decision_keeps_primary_and_policy_evidence_separate() {
    let set = ConstraintSet::new(MetricGoal::Minimize).with_constraint(
        nca_autoresearch::SecondaryConstraint::new(
            "accuracy",
            ConstraintKind::Quality,
            ConstraintOperator::GreaterThanOrEqual,
            0.9,
        ),
    );
    let observations = BTreeMap::from([(String::from("accuracy"), 0.8)]);
    let decision = set.decide(ExperimentDecision::Keep, &observations);

    assert_eq!(decision.primary_decision, ExperimentDecision::Keep);
    assert_eq!(decision.final_decision, ExperimentDecision::Discard);
    assert_eq!(decision.constraints.violations.len(), 1);
}

#[tokio::test]
async fn parallel_results_are_bounded_and_ordered_for_rendering() {
    let temp = tempfile::tempdir().unwrap();
    let scheduler = ParallelScheduler::new(temp.path(), 2).unwrap();
    let jobs = scheduler.prepare(["a", "b", "c", "d"]).unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let outcomes = scheduler
        .run(jobs, {
            let active = active.clone();
            let peak = peak.clone();
            move |job| {
                let active = active.clone();
                let peak = peak.clone();
                async move {
                    peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis((4 - job.ordinal) as u64 * 5)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, ()>(job.ordinal)
                }
            }
        })
        .await;

    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(
        outcomes
            .iter()
            .map(|outcome| *outcome.result.as_ref().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
}

#[test]
fn progress_and_analysis_are_read_only_and_file_friendly() {
    let results = vec![
        result(1.0, ExperimentStatus::Keep, "baseline"),
        result(0.8, ExperimentStatus::Keep, "improvement"),
        result(1.2, ExperimentStatus::Discard, "regression"),
    ];
    let original = results.clone();
    let report = ProgressReport::from_results(&results, MetricGoal::Minimize);
    assert!(report.render_markdown().contains("| # | Status | Metric |"));
    assert!(report.to_json().unwrap().contains("best_metric"));

    let analysis = analyze_results(
        &results,
        MetricGoal::Minimize,
        AnalysisOptions {
            stall_window: 1,
            ..Default::default()
        },
    );
    assert_eq!(analysis.best_known.unwrap().metric, 0.8);
    assert_eq!(analysis.regressions.len(), 1);
    assert_eq!(results, original);
}
