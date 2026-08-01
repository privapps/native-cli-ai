//! Read-only analysis of completed autoresearch results.
//!
//! The analysis layer never updates a logger, session, Git checkout, or
//! result status.  It computes best-known progress, primary-metric
//! regressions, stalled runs, and diminishing returns from an immutable slice
//! of [`ExperimentResult`] values.

use serde::{Deserialize, Serialize};

use crate::program::MetricGoal;
use crate::result::{ExperimentResult, ExperimentStatus};

/// Tuning knobs for read-only progress analysis.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnalysisOptions {
    /// Number of trailing results without a new best that marks a stall.
    pub stall_window: usize,
    /// Maximum number of early/recent gains compared for diminishing returns.
    pub diminishing_window: usize,
    /// Ignore gains below this relative size when deciding whether progress
    /// has meaningfully slowed.
    pub min_relative_improvement: f64,
    /// Recent gains below this fraction of early gains are diminishing.
    pub diminishing_ratio: f64,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            stall_window: 3,
            diminishing_window: 3,
            min_relative_improvement: 0.001,
            diminishing_ratio: 0.5,
        }
    }
}

/// The best accepted result observed so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BestKnown {
    pub ordinal: usize,
    pub metric: f64,
    pub commit: String,
    pub description: String,
}

/// A candidate whose primary metric did not beat the best known value before
/// it, independent of whether a secondary constraint also failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regression {
    pub ordinal: usize,
    pub metric: f64,
    pub best_before: f64,
    pub status: ExperimentStatus,
}

/// Immutable findings from a result sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub goal: MetricGoal,
    pub total_results: usize,
    pub completed_results: usize,
    pub best_known: Option<BestKnown>,
    pub regressions: Vec<Regression>,
    pub stalled: bool,
    pub diminishing_returns: bool,
    pub trailing_results_without_improvement: usize,
    pub improvement_gains: Vec<f64>,
}

/// Analyze results without changing the input slice or any persisted state.
pub fn analyze_results(
    results: &[ExperimentResult],
    goal: MetricGoal,
    options: AnalysisOptions,
) -> AnalysisReport {
    let mut best_known = None;
    let mut regressions = Vec::new();
    let mut completed_results = 0;
    let mut improvement_gains = Vec::new();
    let mut last_improvement_index = None;

    for (index, result) in results.iter().enumerate() {
        if result.status == ExperimentStatus::Crash {
            continue;
        }
        completed_results += 1;
        if !result.metric_value.is_finite() {
            continue;
        }

        match best_known.as_ref() {
            None => {
                best_known = Some(BestKnown {
                    ordinal: index + 1,
                    metric: result.metric_value,
                    commit: result.commit.clone(),
                    description: result.description.clone(),
                });
                last_improvement_index = Some(index);
            }
            Some(best) if better(goal, result.metric_value, best.metric) => {
                improvement_gains.push(relative_gain(goal, best.metric, result.metric_value));
                best_known = Some(BestKnown {
                    ordinal: index + 1,
                    metric: result.metric_value,
                    commit: result.commit.clone(),
                    description: result.description.clone(),
                });
                last_improvement_index = Some(index);
            }
            Some(best) => {
                regressions.push(Regression {
                    ordinal: index + 1,
                    metric: result.metric_value,
                    best_before: best.metric,
                    status: result.status,
                });
            }
        }
    }

    let trailing_results_without_improvement = match last_improvement_index {
        Some(index) => results.len().saturating_sub(index + 1),
        None => results.len(),
    };
    let stalled = options.stall_window > 0
        && results.len() >= options.stall_window
        && trailing_results_without_improvement >= options.stall_window;

    let diminishing_returns = has_diminishing_returns(&improvement_gains, options);

    AnalysisReport {
        goal,
        total_results: results.len(),
        completed_results,
        best_known,
        regressions,
        stalled,
        diminishing_returns,
        trailing_results_without_improvement,
        improvement_gains,
    }
}

fn has_diminishing_returns(gains: &[f64], options: AnalysisOptions) -> bool {
    if gains.len() < 2 || options.diminishing_window == 0 {
        return false;
    }
    let window = options.diminishing_window.min(gains.len() / 2).max(1);
    let early = average(&gains[..window]);
    let recent = average(&gains[gains.len() - window..]);
    early > options.min_relative_improvement
        && recent < early * options.diminishing_ratio
        && recent >= 0.0
}

fn average(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn better(goal: MetricGoal, candidate: f64, current: f64) -> bool {
    match goal {
        MetricGoal::Minimize => candidate < current,
        MetricGoal::Maximize => candidate > current,
    }
}

fn relative_gain(goal: MetricGoal, previous: f64, candidate: f64) -> f64 {
    let absolute = match goal {
        MetricGoal::Minimize => previous - candidate,
        MetricGoal::Maximize => candidate - previous,
    };
    let scale = previous.abs().max(f64::EPSILON);
    (absolute / scale).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn result(metric: f64, status: ExperimentStatus, commit: &str) -> ExperimentResult {
        ExperimentResult {
            commit: commit.into(),
            metric_value: metric,
            memory_gb: 1.0,
            training_seconds: 1.0,
            total_seconds: 1.0,
            status,
            description: commit.into(),
            timestamp: Utc::now(),
            peak_vram_mb: None,
            mfu_percent: None,
            total_tokens_m: None,
            num_steps: None,
            num_params_m: None,
        }
    }

    #[test]
    fn identifies_best_regressions_stalls_and_diminishing_returns() {
        let results = vec![
            result(1.0, ExperimentStatus::Keep, "baseline"),
            result(0.8, ExperimentStatus::Keep, "large improvement"),
            result(0.79, ExperimentStatus::Keep, "small improvement"),
            result(0.789, ExperimentStatus::Keep, "tiny improvement"),
            result(1.2, ExperimentStatus::Discard, "regression"),
            result(0.0, ExperimentStatus::Crash, "crash"),
        ];
        let report = analyze_results(
            &results,
            MetricGoal::Minimize,
            AnalysisOptions {
                stall_window: 2,
                diminishing_window: 2,
                ..Default::default()
            },
        );

        assert_eq!(
            report.best_known.as_ref().unwrap().commit,
            "tiny improvement"
        );
        assert_eq!(report.regressions.len(), 1);
        assert_eq!(report.regressions[0].status, ExperimentStatus::Discard);
        assert!(report.stalled);
        assert!(report.diminishing_returns);
        assert_eq!(report.completed_results, 5);
    }

    #[test]
    fn maximize_and_empty_inputs_have_correct_edge_behavior() {
        let empty = analyze_results(&[], MetricGoal::Maximize, AnalysisOptions::default());
        assert!(empty.best_known.is_none());
        assert!(!empty.stalled);
        assert!(!empty.diminishing_returns);

        let results = vec![
            result(0.5, ExperimentStatus::Keep, "baseline"),
            result(0.6, ExperimentStatus::Keep, "best"),
            result(0.55, ExperimentStatus::Discard, "regression"),
        ];
        let report = analyze_results(&results, MetricGoal::Maximize, AnalysisOptions::default());
        assert_eq!(report.best_known.unwrap().metric, 0.6);
        assert_eq!(report.regressions[0].metric, 0.55);
    }

    #[test]
    fn analysis_does_not_mutate_results() {
        let results = vec![result(1.0, ExperimentStatus::Keep, "baseline")];
        let before = results.clone();
        let _ = analyze_results(&results, MetricGoal::Minimize, AnalysisOptions::default());
        assert_eq!(results, before);
    }
}
