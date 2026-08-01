//! Experiment execution with fixed time budget
//!
//! Runs experiments with a hard timeout, captures output, and extracts metrics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::constraints::ConstraintSet;
use crate::git_integration::{
    ExperimentWorkspace, GitManager, WorktreeDecision, WorktreeDisposition,
};
use crate::result::{ExperimentDecision, ExperimentRecord, ResultsLogger, decide_metric};

pub use crate::metric_parser::MetricParser;
pub use crate::program::{MetricGoal, ResearchProgram};
pub use crate::result::{ExperimentResult, ExperimentStatus, ExperimentTermination};

/// Configuration for experiment execution
#[derive(Debug, Clone)]
pub struct ExperimentConfig {
    /// Working directory for the experiment
    pub working_dir: PathBuf,
    /// Command to run
    pub command: String,
    /// Arguments for the command
    pub args: Vec<String>,
    /// Time budget in seconds
    pub time_budget_seconds: u64,
    /// Log file path
    pub log_file: Option<PathBuf>,
    /// Memory limit in GB (soft constraint)
    pub memory_limit_gb: Option<f64>,
    /// Kill timeout factor (how many times the budget before SIGKILL)
    pub kill_timeout_factor: u64,
}

impl Default for ExperimentConfig {
    fn default() -> Self {
        Self {
            working_dir: PathBuf::from("."),
            command: "python".to_string(),
            args: vec!["train.py".to_string()],
            time_budget_seconds: 300,
            log_file: None,
            memory_limit_gb: None,
            kill_timeout_factor: 2,
        }
    }
}

impl ExperimentConfig {
    /// Run a training script (like karpathy's train.py)
    pub fn training_script(script: impl AsRef<Path>, time_budget_seconds: u64) -> Self {
        let script_path = script.as_ref();
        let script_name = script_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("train.py");

        Self {
            working_dir: script_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
            command: "python".to_string(),
            args: vec![script_name.to_string()],
            time_budget_seconds,
            log_file: Some(PathBuf::from("run.log")),
            memory_limit_gb: None,
            kill_timeout_factor: 2,
        }
    }
}

/// Experiment runner that executes runs with timeout and metric extraction
pub struct ExperimentRunner {
    config: ExperimentConfig,
    metric_parser: MetricParser,
}

impl ExperimentRunner {
    /// Create a new experiment runner
    pub fn new(config: ExperimentConfig) -> Self {
        Self {
            config,
            metric_parser: MetricParser::default(),
        }
    }

    /// Set a custom metric parser
    pub fn with_metric_parser(mut self, parser: MetricParser) -> Self {
        self.metric_parser = parser;
        self
    }

    /// Run a single experiment with the configured time budget
    ///
    /// Returns the experiment result with parsed metrics.
    pub async fn run(&self) -> Result<ExperimentOutput> {
        self.run_with_description(String::new()).await
    }

    /// Run an experiment with a description of what was changed
    pub async fn run_with_description(&self, description: String) -> Result<ExperimentOutput> {
        let start_time = Instant::now();

        // Set up log file
        let log_path = self.config.log_file.as_ref().map(|p| {
            if p.is_relative() {
                self.config.working_dir.join(p)
            } else {
                p.clone()
            }
        });

        // Clear/create log file
        if let Some(ref log_path) = log_path {
            tokio::fs::create_dir_all(log_path.parent().unwrap_or(&self.config.working_dir))
                .await?;
            tokio::fs::write(log_path, b"").await?;
        }

        // Spawn the process. The runner never mutates the caller's process
        // environment and all file mutations therefore remain inside the
        // configured working directory (isolated callers use a worktree).
        let mut child = Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(&self.config.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn: {} {:?}",
                    self.config.command, self.config.args
                )
            })?;

        // Capture stdout and stderr independently so both remain auditable.
        let stdout = child.stdout.take().expect("stdout captured");
        let stderr = child.stderr.take().expect("stderr captured");

        let mut stdout_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let mut reader = stdout;
            reader.read_to_end(&mut bytes).await.map(|_| bytes)
        });
        let mut stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            let mut reader = stderr;
            reader.read_to_end(&mut bytes).await.map(|_| bytes)
        });

        // Wait for process with timeout
        let timeout_duration = Duration::from_secs(self.config.time_budget_seconds.max(1));

        let wait_result = timeout(timeout_duration, child.wait()).await;

        // Check if we timed out
        let (exit_status, termination) = match wait_result {
            Ok(Ok(status)) => {
                // Process exited normally before timeout
                (Some(status), ExperimentTermination::Completed)
            }
            Ok(Err(e)) => {
                // Process spawn failed
                return Err(anyhow::anyhow!("Process wait failed: {}", e));
            }
            Err(_) => {
                // Timeout - kill the process
                tracing::warn!(
                    "Experiment timed out after {} seconds, killing process",
                    self.config.time_budget_seconds
                );

                // Kill synchronously
                let _ = child.kill().await;

                // Wait only for the configured cleanup grace; kill_on_drop
                // remains the final safety net if a child refuses to exit.
                let _ = timeout(
                    Duration::from_secs(self.config.kill_timeout_factor.max(1)),
                    child.wait(),
                )
                .await;

                (None, ExperimentTermination::TimedOut)
            }
        };

        // Wait for output streams with a bounded cleanup window as well.
        let captured = timeout(
            Duration::from_secs(self.config.kill_timeout_factor.max(1)),
            async {
                let stdout = (&mut stdout_task)
                    .await
                    .context("stdout capture task failed")??;
                let stderr = (&mut stderr_task)
                    .await
                    .context("stderr capture task failed")??;
                Ok::<_, anyhow::Error>((stdout, stderr))
            },
        )
        .await;
        let (stdout, stderr) = match captured {
            Ok(captured) => captured?,
            Err(_) => {
                stdout_task.abort();
                stderr_task.abort();
                (Vec::new(), Vec::new())
            }
        };
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        let output = if stderr.is_empty() {
            stdout.clone()
        } else if stdout.is_empty() {
            format!("[stderr] {stderr}")
        } else {
            format!("{stdout}\n[stderr] {stderr}")
        };

        if let Some(ref log_path) = log_path {
            let audit_output = if stderr.is_empty() {
                stdout.clone()
            } else {
                format!("{stdout}\n[stderr] {stderr}")
            };
            tokio::fs::write(log_path, audit_output.as_bytes()).await?;
        }

        let elapsed = start_time.elapsed();

        // Determine success and extract metrics
        let (status, parsed_metrics) = if let Some(code) = exit_status.and_then(|s| s.code()) {
            if code == 0 {
                (
                    ExperimentStatus::Keep,
                    self.metric_parser.extract_all(&output),
                )
            } else {
                (ExperimentStatus::Crash, None)
            }
        } else {
            (ExperimentStatus::Crash, None)
        };

        let termination = if termination == ExperimentTermination::Completed
            && status == ExperimentStatus::Crash
        {
            ExperimentTermination::Crashed
        } else {
            termination
        };

        // Extract values from parsed_metrics
        let peak_vram_mb = parsed_metrics.as_ref().and_then(|m| m.peak_vram_mb);
        let mfu_percent = parsed_metrics.as_ref().and_then(|m| m.mfu_percent);
        let total_tokens_m = parsed_metrics.as_ref().and_then(|m| m.total_tokens_m);
        let num_steps = parsed_metrics.as_ref().and_then(|m| m.num_steps);
        let num_params_m = parsed_metrics.as_ref().and_then(|m| m.num_params_m);
        let training_seconds = parsed_metrics
            .as_ref()
            .and_then(|m| m.training_seconds)
            .unwrap_or(elapsed.as_secs_f64());

        Ok(ExperimentOutput {
            status,
            output,
            stdout,
            stderr,
            termination,
            elapsed_seconds: elapsed.as_secs_f64(),
            training_seconds,
            peak_vram_mb,
            mfu_percent,
            total_tokens_m,
            num_steps,
            num_params_m,
            memory_gb: peak_vram_mb.map(|mb| mb / 1024.0).unwrap_or(0.0),
            description,
            log_path,
        })
    }

    /// Run with a specific metric extraction
    pub async fn run_with_metric(
        &self,
        _metric_command: &str,
        regex: &str,
    ) -> Result<(ExperimentOutput, Option<f64>)> {
        self.run_with_metric_and_description(regex, String::new())
            .await
    }

    /// Run an experiment and mark a missing/non-finite metric as malformed.
    pub async fn run_with_metric_and_description(
        &self,
        regex: &str,
        description: String,
    ) -> Result<(ExperimentOutput, Option<f64>)> {
        let mut output = self.run_with_description(description).await?;
        let metric_value = self.metric_parser.extract_with_regex(&output.output, regex);
        if output.termination == ExperimentTermination::Completed
            && metric_value.filter(|value| value.is_finite()).is_none()
        {
            output.status = ExperimentStatus::Crash;
            output.termination = ExperimentTermination::MalformedMetrics;
        }
        Ok((output, metric_value))
    }

    /// Run one bounded experiment in a detached worktree, enforce its file
    /// policy, decide keep/discard/failure, and persist a complete audit row.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_isolated_with_metric(
        &self,
        git: &GitManager,
        logger: &ResultsLogger,
        experiments_root: &Path,
        experiment_id: &str,
        permitted_files: &[PathBuf],
        metric_regex: &str,
        baseline_metric: Option<f64>,
        goal: MetricGoal,
        description: impl Into<String>,
    ) -> Result<IsolatedExperimentResult> {
        self.run_isolated_with_metric_and_constraints(
            git,
            logger,
            experiments_root,
            experiment_id,
            permitted_files,
            metric_regex,
            baseline_metric,
            &ConstraintSet::new(goal),
            false,
            description,
        )
        .await
    }

    /// Run an isolated experiment using every constraint declared by a
    /// research program.  Recovery is opt-in: when enabled, an existing
    /// worktree is resumed only after its durable identity marker is checked.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_isolated_with_program(
        &self,
        git: &GitManager,
        logger: &ResultsLogger,
        experiments_root: &Path,
        experiment_id: &str,
        permitted_files: &[PathBuf],
        program: &ResearchProgram,
        baseline_metric: Option<f64>,
        resume_interrupted: bool,
        description: impl Into<String>,
    ) -> Result<IsolatedExperimentResult> {
        self.run_isolated_with_metric_and_constraints(
            git,
            logger,
            experiments_root,
            experiment_id,
            permitted_files,
            &program.metric_command.parse_regex,
            baseline_metric,
            &ConstraintSet::from_program(program),
            resume_interrupted,
            description,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_isolated_with_metric_and_constraints(
        &self,
        git: &GitManager,
        logger: &ResultsLogger,
        experiments_root: &Path,
        experiment_id: &str,
        permitted_files: &[PathBuf],
        metric_regex: &str,
        baseline_metric: Option<f64>,
        constraints: &ConstraintSet,
        resume_interrupted: bool,
        description: impl Into<String>,
    ) -> Result<IsolatedExperimentResult> {
        let workspace = if resume_interrupted && experiments_root.join(experiment_id).exists() {
            git.recover_isolated_worktree(experiments_root, experiment_id)
                .await?
        } else {
            git.create_isolated_worktree(experiments_root, experiment_id)
                .await?
        };
        let description = description.into();
        let mut config = self.config.clone();
        config.working_dir = workspace.path.clone();
        // The JSONL audit record is the isolated run's log. Keeping a
        // relative run.log in the worktree would turn instrumentation into an
        // unauthorized candidate-file change.
        config.log_file = None;
        let runner = ExperimentRunner {
            config,
            metric_parser: self.metric_parser.clone(),
        };
        let (mut output, mut metric_value) = match runner
            .run_with_metric_and_description(metric_regex, description.clone())
            .await
        {
            Ok(result) => result,
            Err(error) => (
                ExperimentOutput::failed(description.clone(), error.to_string()),
                None,
            ),
        };

        let changed_files = git.workspace_changed_files(&workspace).await?;
        if let Err(error) = git
            .validate_permitted_files(&workspace, permitted_files)
            .await
        {
            output.status = ExperimentStatus::Crash;
            output.termination = ExperimentTermination::PolicyViolation;
            output.output.push_str(&format!("\n[policy] {error}"));
            metric_value = None;
        }

        let primary_decision = if output.status == ExperimentStatus::Crash {
            ExperimentDecision::Failure
        } else {
            decide_metric(constraints.primary_goal, baseline_metric, metric_value)
        };
        let constrained = constraints.decide(primary_decision, &output.observations());
        let decision = constrained.final_decision;
        let worktree_decision = match decision {
            ExperimentDecision::Keep => WorktreeDecision::Keep,
            ExperimentDecision::Discard => WorktreeDecision::Discard,
            ExperimentDecision::Failure => WorktreeDecision::Failure,
        };
        let disposition = git
            .finalize_experiment(&workspace, worktree_decision)
            .await?;

        let mut result = output.to_result(&workspace.base_commit);
        result.metric_value = metric_value.unwrap_or(0.0);
        result.status = match decision {
            ExperimentDecision::Keep => ExperimentStatus::Keep,
            ExperimentDecision::Discard => ExperimentStatus::Discard,
            ExperimentDecision::Failure => ExperimentStatus::Crash,
        };
        let record = ExperimentRecord {
            experiment_id: experiment_id.to_string(),
            commit: workspace.base_commit.clone(),
            metric_value,
            status: result.status,
            termination: output.termination,
            primary_decision,
            decision,
            constraint_violations: constrained.constraints.violations.clone(),
            memory_gb: output.memory_gb,
            training_seconds: output.training_seconds,
            total_seconds: output.elapsed_seconds,
            peak_vram_mb: output.peak_vram_mb,
            mfu_percent: output.mfu_percent,
            total_tokens_m: output.total_tokens_m,
            num_steps: output.num_steps,
            num_params_m: output.num_params_m,
            description,
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
            changed_files: changed_files
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            workspace: workspace.path.display().to_string(),
            timestamp: result.timestamp,
        };
        logger.append(&result)?;
        logger.append_record(&record)?;

        Ok(IsolatedExperimentResult {
            output,
            metric_value,
            decision,
            result,
            record,
            workspace,
            disposition,
        })
    }
}

/// Result of a complete isolated run, including its policy decision and
/// durable audit data.
#[derive(Debug, Clone)]
pub struct IsolatedExperimentResult {
    pub output: ExperimentOutput,
    pub metric_value: Option<f64>,
    pub decision: ExperimentDecision,
    pub result: ExperimentResult,
    pub record: ExperimentRecord,
    pub workspace: ExperimentWorkspace,
    pub disposition: WorktreeDisposition,
}

/// Output from an experiment run
#[derive(Debug, Clone)]
pub struct ExperimentOutput {
    /// Experiment status
    pub status: ExperimentStatus,
    /// Captured stdout/stderr
    pub output: String,
    /// Captured stdout without stderr framing
    pub stdout: String,
    /// Captured stderr without stdout framing
    pub stderr: String,
    /// Distinguishes timeout, crash, malformed metrics, and policy failure.
    pub termination: ExperimentTermination,
    /// Total elapsed time
    pub elapsed_seconds: f64,
    /// Training time (may be extracted from output)
    pub training_seconds: f64,
    /// Peak VRAM in MB
    pub peak_vram_mb: Option<f64>,
    /// Model FLOPs utilization
    pub mfu_percent: Option<f64>,
    /// Total tokens in millions
    pub total_tokens_m: Option<f64>,
    /// Number of training steps
    pub num_steps: Option<u64>,
    /// Number of parameters in millions
    pub num_params_m: Option<f64>,
    /// Memory usage in GB
    pub memory_gb: f64,
    /// Description of the experiment
    pub description: String,
    /// Path to log file
    pub log_path: Option<PathBuf>,
}

impl ExperimentOutput {
    /// Construct a durable crash-shaped output for failures before a child
    /// process can produce normal output (for example, an unknown command).
    pub fn failed(description: String, error: String) -> Self {
        Self {
            status: ExperimentStatus::Crash,
            output: format!("[runner] {error}"),
            stdout: String::new(),
            stderr: error,
            termination: ExperimentTermination::Crashed,
            elapsed_seconds: 0.0,
            training_seconds: 0.0,
            peak_vram_mb: None,
            mfu_percent: None,
            total_tokens_m: None,
            num_steps: None,
            num_params_m: None,
            memory_gb: 0.0,
            description,
            log_path: None,
        }
    }

    /// Convert to ExperimentResult for logging
    pub fn to_result(&self, commit: &str) -> ExperimentResult {
        let mut result = ExperimentResult::new(
            commit.to_string(),
            0.0, // Will be set by the caller
            self.memory_gb,
            self.training_seconds,
            self.elapsed_seconds,
            self.status,
            self.description.clone(),
        );
        result.peak_vram_mb = self.peak_vram_mb;
        result.mfu_percent = self.mfu_percent;
        result.total_tokens_m = self.total_tokens_m;
        result.num_steps = self.num_steps;
        result.num_params_m = self.num_params_m;
        result
    }

    /// Return the named observations available to secondary constraints.
    /// Values emitted as `name: value` or `name=value` are included in
    /// addition to the structured metrics parsed by the runner.
    pub fn observations(&self) -> BTreeMap<String, f64> {
        let mut observations = BTreeMap::new();
        if let Some(value) = self.peak_vram_mb {
            observations.insert("peak_vram_mb".into(), value);
            observations.insert("memory_gb".into(), value / 1024.0);
        }
        if let Some(value) = self.mfu_percent {
            observations.insert("mfu_percent".into(), value);
        }
        if let Some(value) = self.total_tokens_m {
            observations.insert("total_tokens_m".into(), value);
        }
        if let Some(value) = self.num_steps {
            observations.insert("num_steps".into(), value as f64);
        }
        if let Some(value) = self.num_params_m {
            observations.insert("num_params_m".into(), value);
        }
        observations.insert("training_seconds".into(), self.training_seconds);
        observations.insert("total_seconds".into(), self.elapsed_seconds);

        for line in self.output.lines() {
            let Some((name, raw_value)) = line.split_once(':').or_else(|| line.split_once('='))
            else {
                continue;
            };
            let Some(value) = raw_value.split_whitespace().next().and_then(|value| {
                value
                    .trim_matches(|character: char| ",;`".contains(character))
                    .parse::<f64>()
                    .ok()
            }) else {
                continue;
            };
            let name = name.trim().trim_start_matches("[stderr]").trim();
            observations.insert(name.to_string(), value);
        }
        observations
    }
}

/// Run an experiment and extract metrics in one go
pub async fn run_experiment(
    config: ExperimentConfig,
    metric_regex: &str,
) -> Result<(ExperimentOutput, Option<f64>)> {
    let runner = ExperimentRunner::new(config);
    runner.run_with_metric("", metric_regex).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = ExperimentConfig::default();
        assert_eq!(config.time_budget_seconds, 300);
        assert_eq!(config.command, "python");
    }

    #[tokio::test]
    async fn test_successful_experiment() {
        let config = ExperimentConfig {
            command: "echo".to_string(),
            args: vec!["val_bpb: 1.234\npeak_vram_mb: 4096.0".to_string()],
            time_budget_seconds: 5,
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let output = runner.run().await.unwrap();

        assert_eq!(output.status, ExperimentStatus::Keep);
        assert!(output.output.contains("val_bpb"));
        assert_eq!(output.peak_vram_mb, Some(4096.0));
        assert_eq!(output.memory_gb, 4.0);
    }

    #[tokio::test]
    async fn runner_retains_separate_streams_and_description() {
        let config = ExperimentConfig {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "printf 'val_bpb: 0.8'; printf 'warning' >&2".into(),
            ],
            time_budget_seconds: 5,
            ..Default::default()
        };

        let (output, metric) = ExperimentRunner::new(config)
            .run_with_metric_and_description(r"val_bpb:\s*([0-9.]+)", "stream capture".into())
            .await
            .unwrap();

        assert_eq!(metric, Some(0.8));
        assert_eq!(output.stdout, "val_bpb: 0.8");
        assert_eq!(output.stderr, "warning");
        assert_eq!(output.description, "stream capture");
        assert_eq!(output.termination, ExperimentTermination::Completed);
    }

    #[tokio::test]
    async fn test_failed_experiment() {
        let config = ExperimentConfig {
            command: "bash".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            time_budget_seconds: 5,
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let output = runner.run().await.unwrap();

        assert_eq!(output.status, ExperimentStatus::Crash);
        assert_eq!(output.termination, ExperimentTermination::Crashed);
    }

    #[tokio::test]
    async fn test_timeout() {
        let config = ExperimentConfig {
            command: "sleep".to_string(),
            args: vec!["10".to_string()],
            time_budget_seconds: 1,
            kill_timeout_factor: 1, // Aggressive kill
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let start = Instant::now();
        let output = runner.run().await.unwrap();
        let elapsed = start.elapsed();

        // Should timeout well before 10 seconds
        assert!(elapsed < Duration::from_secs(5));
        assert_eq!(output.status, ExperimentStatus::Crash);
        assert_eq!(output.termination, ExperimentTermination::TimedOut);
    }

    #[tokio::test]
    async fn malformed_metric_is_distinct_from_a_process_crash() {
        let config = ExperimentConfig {
            command: "echo".to_string(),
            args: vec!["no metric here".to_string()],
            time_budget_seconds: 5,
            ..Default::default()
        };

        let runner = ExperimentRunner::new(config);
        let (output, metric) = runner
            .run_with_metric("val_bpb:\\s*([0-9.]+)", "missing metric")
            .await
            .unwrap();

        assert_eq!(metric, None);
        assert_eq!(output.status, ExperimentStatus::Crash);
        assert_eq!(output.termination, ExperimentTermination::MalformedMetrics);
    }

    #[tokio::test]
    async fn isolated_runner_records_and_discards_regressions_without_touching_main_repo() {
        use crate::git_integration::GitManager;
        use crate::result::{ExperimentDecision, ResultsLogger};
        use std::process::Command as StdCommand;

        let temp = tempfile::tempdir().unwrap();
        let run_git_checked = |args: &[&str]| {
            let output = StdCommand::new("git")
                .current_dir(temp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        };

        run_git_checked(&["init", "-q"]);
        run_git_checked(&["config", "user.email", "test@example.com"]);
        run_git_checked(&["config", "user.name", "Test"]);
        run_git_checked(&["config", "commit.gpgsign", "false"]);
        std::fs::write(temp.path().join("train.py"), "print('baseline')").unwrap();
        run_git_checked(&["add", "train.py"]);
        run_git_checked(&["commit", "-qm", "initial"]);
        std::fs::write(temp.path().join("user.txt"), "unrelated").unwrap();

        let config = ExperimentConfig {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "printf 'val_bpb: 1.1\\n'; printf candidate > train.py".into(),
            ],
            time_budget_seconds: 5,
            ..Default::default()
        };
        let logger = ResultsLogger::new(temp.path().join("results.tsv"));
        let result = ExperimentRunner::new(config)
            .run_isolated_with_metric(
                &GitManager::new(temp.path()),
                &logger,
                &temp.path().join("experiments"),
                "exp-regression",
                &["train.py".into()],
                r"val_bpb:\s*([0-9.]+)",
                Some(1.0),
                MetricGoal::Minimize,
                "regression",
            )
            .await
            .unwrap();

        assert_eq!(result.decision, ExperimentDecision::Discard);
        assert_eq!(result.metric_value, Some(1.1));
        assert!(!result.workspace.path.exists());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("user.txt")).unwrap(),
            "unrelated"
        );
        assert_eq!(logger.load_records().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn isolated_program_runner_applies_constraints_to_production_decision_and_audit() {
        use crate::git_integration::GitManager;
        use crate::program::{EditableFile, MetricCommand, ResearchProgram};
        use crate::result::{ExperimentDecision, ResultsLogger};
        use std::process::Command as StdCommand;

        let temp = tempfile::tempdir().unwrap();
        let run_git_checked = |args: &[&str]| {
            let output = StdCommand::new("git")
                .current_dir(temp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?} failed");
        };
        run_git_checked(&["init", "-q"]);
        run_git_checked(&["config", "user.email", "test@example.com"]);
        run_git_checked(&["config", "user.name", "Test"]);
        run_git_checked(&["config", "commit.gpgsign", "false"]);
        std::fs::write(temp.path().join("train.py"), "baseline").unwrap();
        run_git_checked(&["add", "train.py"]);
        run_git_checked(&["commit", "-qm", "initial"]);

        let program = ResearchProgram {
            name: "constrained".into(),
            description: String::new(),
            editable_files: vec![EditableFile::new("train.py")],
            fixed_files: Vec::new(),
            metric_command: MetricCommand {
                command: "unused".into(),
                parse_regex: r"val_bpb:\s*([0-9.]+)".into(),
            },
            metric_goal: MetricGoal::Minimize,
            time_budget_seconds: 5,
            extra_constraints: vec!["accuracy >= 0.9".into()],
            max_memory_gb: None,
            instructions: String::new(),
        };
        let logger = ResultsLogger::new(temp.path().join("results.tsv"));
        let result = ExperimentRunner::new(ExperimentConfig {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "printf 'val_bpb: 0.5\\naccuracy: 0.8\\n'; printf candidate > train.py".into(),
            ],
            time_budget_seconds: 5,
            ..Default::default()
        })
        .run_isolated_with_program(
            &GitManager::new(temp.path()),
            &logger,
            &temp.path().join("experiments"),
            "constrained-1",
            &[PathBuf::from("train.py")],
            &program,
            Some(1.0),
            false,
            "violating but faster",
        )
        .await
        .unwrap();

        assert_eq!(result.record.primary_decision, ExperimentDecision::Keep);
        assert_eq!(result.decision, ExperimentDecision::Discard);
        assert_eq!(result.result.status, ExperimentStatus::Discard);
        assert_eq!(result.record.constraint_violations.len(), 1);
        assert_eq!(result.record.constraint_violations[0].name, "accuracy");
        assert_eq!(logger.load_records().unwrap()[0], result.record);
    }

    #[tokio::test]
    async fn isolated_program_runner_explicitly_resumes_an_interrupted_worktree() {
        use crate::git_integration::GitManager;
        use crate::program::{EditableFile, MetricCommand, ResearchProgram};
        use crate::result::ResultsLogger;
        use std::process::Command as StdCommand;

        let temp = tempfile::tempdir().unwrap();
        let run_git_checked = |args: &[&str]| {
            let output = StdCommand::new("git")
                .current_dir(temp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?} failed");
        };
        run_git_checked(&["init", "-q"]);
        run_git_checked(&["config", "user.email", "test@example.com"]);
        run_git_checked(&["config", "user.name", "Test"]);
        run_git_checked(&["config", "commit.gpgsign", "false"]);
        std::fs::write(temp.path().join("train.py"), "baseline").unwrap();
        run_git_checked(&["add", "train.py"]);
        run_git_checked(&["commit", "-qm", "initial"]);

        let git = GitManager::new(temp.path());
        let experiments = temp.path().join("experiments");
        let interrupted = git
            .create_isolated_worktree(&experiments, "session-1")
            .await
            .unwrap();
        std::fs::write(interrupted.path.join("train.py"), "partial").unwrap();
        let program = ResearchProgram {
            name: "recover".into(),
            description: String::new(),
            editable_files: vec![EditableFile::new("train.py")],
            fixed_files: Vec::new(),
            metric_command: MetricCommand {
                command: "unused".into(),
                parse_regex: r"val_bpb:\s*([0-9.]+)".into(),
            },
            metric_goal: MetricGoal::Minimize,
            time_budget_seconds: 5,
            extra_constraints: Vec::new(),
            max_memory_gb: None,
            instructions: String::new(),
        };
        let result = ExperimentRunner::new(ExperimentConfig {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                "printf 'val_bpb: 0.5\\n'; printf resumed > train.py".into(),
            ],
            time_budget_seconds: 5,
            ..Default::default()
        })
        .run_isolated_with_program(
            &git,
            &ResultsLogger::new(temp.path().join("results.tsv")),
            &experiments,
            "session-1",
            &[PathBuf::from("train.py")],
            &program,
            Some(1.0),
            true,
            "resume",
        )
        .await
        .unwrap();

        assert_eq!(result.workspace.path, interrupted.path);
        assert!(result.workspace.path.exists());
        git.finalize_experiment(&result.workspace, WorktreeDecision::Failure)
            .await
            .unwrap();
        assert!(!result.workspace.path.exists());
    }
}
