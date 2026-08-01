//! Stable progress views for terminal and file-based autoresearch review.
//!
//! [`ProgressReport::render_markdown`] is the documented file representation:
//! it contains a summary followed by one row per result with the ordinal,
//! status, candidate metric, best-so-far metric, and description.  The
//! terminal rendering uses the same points and fields, so neither view
//! performs a fresh experiment or mutates the results log.

use serde::{Deserialize, Serialize};

use crate::program::MetricGoal;
use crate::result::{ExperimentResult, ExperimentStatus};

/// One immutable point in a progress report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressPoint {
    pub ordinal: usize,
    pub status: ExperimentStatus,
    pub metric: f64,
    pub best_metric: Option<f64>,
    pub description: String,
}

/// An immutable, renderable view of a result sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressReport {
    pub goal: MetricGoal,
    pub points: Vec<ProgressPoint>,
    pub total: usize,
    pub kept: usize,
    pub discarded: usize,
    pub crashed: usize,
    pub best_metric: Option<f64>,
}

impl ProgressReport {
    pub fn from_results(results: &[ExperimentResult], goal: MetricGoal) -> Self {
        let mut best_metric = None;
        let mut points = Vec::with_capacity(results.len());
        let mut kept = 0;
        let mut discarded = 0;
        let mut crashed = 0;

        for (ordinal, result) in results.iter().enumerate() {
            match result.status {
                ExperimentStatus::Keep => {
                    kept += 1;
                    if result.metric_value.is_finite()
                        && best_metric.is_none_or(|best| better(goal, result.metric_value, best))
                    {
                        best_metric = Some(result.metric_value);
                    }
                }
                ExperimentStatus::Discard => discarded += 1,
                ExperimentStatus::Crash => crashed += 1,
            }
            points.push(ProgressPoint {
                ordinal: ordinal + 1,
                status: result.status,
                metric: result.metric_value,
                best_metric,
                description: result.description.clone(),
            });
        }

        Self {
            goal,
            total: results.len(),
            kept,
            discarded,
            crashed,
            best_metric,
            points,
        }
    }

    /// Render a compact, ANSI-free view suitable for a terminal or log.
    pub fn render_terminal(&self) -> String {
        let mut output = format!(
            "Autoresearch progress ({})\nexperiments: {} | kept: {} | discarded: {} | crashed: {}\n",
            self.goal, self.total, self.kept, self.discarded, self.crashed
        );
        output.push_str(&format!("best: {}\n", format_metric(self.best_metric)));
        for point in &self.points {
            output.push_str(&format!(
                "{:>4} {:>8} metric={} best={} {}\n",
                point.ordinal,
                point.status,
                format_metric(Some(point.metric)),
                format_metric(point.best_metric),
                point.description.replace(['\n', '\r'], " ")
            ));
        }
        output
    }

    /// Render a stable Markdown artifact for audit and file-based review.
    pub fn render_markdown(&self) -> String {
        let mut output = format!(
            "# Autoresearch Progress\n\n- Goal: `{}`\n- Experiments: {}\n- Kept: {}\n- Discarded: {}\n- Crashed: {}\n- Best metric: `{}`\n\n| # | Status | Metric | Best so far | Description |\n| ---: | --- | ---: | ---: | --- |\n",
            self.goal,
            self.total,
            self.kept,
            self.discarded,
            self.crashed,
            format_metric(self.best_metric)
        );
        for point in &self.points {
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                point.ordinal,
                point.status,
                format_metric(Some(point.metric)),
                format_metric(point.best_metric),
                escape_markdown(&point.description)
            ));
        }
        output
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

fn better(goal: MetricGoal, candidate: f64, current: f64) -> bool {
    match goal {
        MetricGoal::Minimize => candidate < current,
        MetricGoal::Maximize => candidate > current,
    }
}

fn format_metric(metric: Option<f64>) -> String {
    metric
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.6}"))
        .unwrap_or_else(|| "—".to_string())
}

fn escape_markdown(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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
    fn progress_preserves_best_so_far_and_renders_both_surfaces() {
        let results = vec![
            result(1.0, ExperimentStatus::Keep, "baseline"),
            result(1.2, ExperimentStatus::Discard, "| regression"),
            result(0.8, ExperimentStatus::Keep, "improvement"),
            result(f64::NAN, ExperimentStatus::Crash, "crash"),
        ];
        let report = ProgressReport::from_results(&results, MetricGoal::Minimize);

        assert_eq!(report.best_metric, Some(0.8));
        assert_eq!(report.points[1].best_metric, Some(1.0));
        assert_eq!(report.points[3].best_metric, Some(0.8));
        assert!(report.render_terminal().contains("experiments: 4"));
        let markdown = report.render_markdown();
        assert!(markdown.contains("# Autoresearch Progress"));
        assert!(markdown.contains("\\| regression"));
        let json = report.to_json().unwrap();
        assert!(json.contains("\"best_metric\": 0.8"));
    }

    #[test]
    fn maximize_progress_does_not_promote_discarded_values() {
        let results = vec![
            result(0.8, ExperimentStatus::Keep, "baseline"),
            result(0.9, ExperimentStatus::Discard, "discarded"),
            result(0.85, ExperimentStatus::Keep, "keep"),
        ];
        let report = ProgressReport::from_results(&results, MetricGoal::Maximize);
        assert_eq!(report.best_metric, Some(0.85));
        assert_eq!(report.points[1].best_metric, Some(0.8));
    }
}
