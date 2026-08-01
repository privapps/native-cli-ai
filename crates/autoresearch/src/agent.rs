//! Stable, bounded contract used by an nca agent to drive autoresearch.
//!
//! This module deliberately contains no model or tool-loop code.  It defines
//! the data exchanged at the agent boundary, discovers Markdown programs
//! without executing them, and persists the additional failure context that a
//! model needs when selecting the next experiment.

use crate::constraints::ConstraintViolation;
use crate::experiment::IsolatedExperimentResult;
use crate::program::ResearchProgram;
use crate::result::{ExperimentDecision, ExperimentResult, ExperimentTermination};
use crate::session::{SessionState, SessionStore};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Version of the JSON contract exposed by the agent-facing autoresearch
/// tool.  Additive fields may be added without changing this number; a
/// breaking change requires a new contract version.
pub const AGENT_CONTRACT_VERSION: u32 = 1;

/// The manual skill name that authorizes model-driven execution.
pub const AGENT_SKILL_COMMAND: &str = "autoresearch";

/// Hard upper bound for one agent continuation request.
pub const MAX_CONTINUATION_ITERATIONS: u64 = 8;

/// Hard upper bound for one agent continuation request in seconds.
pub const MAX_CONTINUATION_SECONDS: u64 = 3_600;

/// Agent-supplied continuation limits.  The tool validates these against the
/// hard caps above and the selected program's per-experiment budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContinuationPolicy {
    /// Number of descriptions/experiments accepted in this continuation.
    pub max_iterations: u64,
    /// Wall-clock budget for this continuation request.
    pub max_total_seconds: u64,
}

impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self {
            max_iterations: 1,
            max_total_seconds: 300,
        }
    }
}

impl ContinuationPolicy {
    /// Validate and clamp the request to the explicit product safety caps.
    pub fn validate(self, program: &ResearchProgram) -> Result<Self> {
        if self.max_iterations == 0 {
            bail!("autoresearch continuation requires at least one iteration");
        }
        if self.max_iterations > MAX_CONTINUATION_ITERATIONS {
            bail!(
                "autoresearch continuation exceeds the {}-iteration cap",
                MAX_CONTINUATION_ITERATIONS
            );
        }
        if self.max_total_seconds == 0 {
            bail!("autoresearch continuation requires a positive time budget");
        }
        if self.max_total_seconds > MAX_CONTINUATION_SECONDS {
            bail!(
                "autoresearch continuation exceeds the {}-second cap",
                MAX_CONTINUATION_SECONDS
            );
        }

        let program_budget = program.time_budget_seconds.max(1);
        let minimum_budget = self.max_iterations.saturating_mul(program_budget);
        if self.max_total_seconds < program_budget {
            bail!(
                "continuation time budget must cover at least one {}-second experiment",
                program_budget
            );
        }

        // The runner enforces the per-experiment program budget.  This check
        // prevents a request from claiming more iterations than its total
        // wall-clock budget can possibly accommodate while preserving the
        // caller's explicit limits.
        let possible_iterations = (self.max_total_seconds / program_budget).max(1);
        if self.max_iterations > possible_iterations && minimum_budget > self.max_total_seconds {
            bail!(
                "continuation allows {} iterations but only {} seconds",
                self.max_iterations,
                self.max_total_seconds
            );
        }

        Ok(self)
    }
}

/// A discoverable summary of a Markdown research program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramSummary {
    pub contract_version: u32,
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    pub editable_files: Vec<PathBuf>,
    pub fixed_files: Vec<PathBuf>,
    pub metric_command: String,
    pub metric_regex: String,
    pub metric_goal: crate::program::MetricGoal,
    pub time_budget_seconds: u64,
    pub max_memory_gb: Option<f64>,
    pub constraints: Vec<String>,
}

impl ProgramSummary {
    pub fn from_program(path: impl Into<PathBuf>, program: &ResearchProgram) -> Self {
        Self {
            contract_version: AGENT_CONTRACT_VERSION,
            path: path.into(),
            name: program.name.clone(),
            description: program.description.clone(),
            editable_files: program
                .editable_files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            fixed_files: program
                .fixed_files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            metric_command: program.metric_command.command.clone(),
            metric_regex: program.metric_command.parse_regex.clone(),
            metric_goal: program.metric_goal,
            time_budget_seconds: program.time_budget_seconds,
            max_memory_gb: program.max_memory_gb,
            constraints: program.extra_constraints.clone(),
        }
    }
}

/// Durable result context returned to the agent.  The lifecycle result file
/// remains compatible with ticket 01; this sidecar preserves termination,
/// process streams, and policy evidence for agent decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResultRecord {
    pub contract_version: u32,
    pub result: ExperimentResult,
    pub termination: ExperimentTermination,
    #[serde(default)]
    pub primary_decision: ExperimentDecision,
    pub decision: ExperimentDecision,
    #[serde(default)]
    pub constraint_violations: Vec<ConstraintViolation>,
    pub stdout: String,
    pub stderr: String,
    pub changed_files: Vec<PathBuf>,
    pub experiment_workspace: PathBuf,
}

impl AgentResultRecord {
    pub fn from_isolated(result: &IsolatedExperimentResult) -> Self {
        Self {
            contract_version: AGENT_CONTRACT_VERSION,
            result: result.result.clone(),
            termination: result.output.termination,
            primary_decision: result.record.primary_decision,
            decision: result.decision,
            constraint_violations: result.record.constraint_violations.clone(),
            stdout: result.output.stdout.clone(),
            stderr: result.output.stderr.clone(),
            changed_files: result
                .record
                .changed_files
                .iter()
                .map(PathBuf::from)
                .collect(),
            experiment_workspace: result.workspace.path.clone(),
        }
    }
}

/// Return the sidecar used for rich agent-facing experiment records.
pub fn agent_results_path(store: &SessionStore, session_id: &str) -> PathBuf {
    store
        .root()
        .join(format!("{session_id}.agent-results.jsonl"))
}

/// Append rich result context for a known session id.
pub fn append_agent_result_for_session(
    store: &SessionStore,
    session_id: &str,
    record: &AgentResultRecord,
) -> Result<()> {
    fs::create_dir_all(store.root())?;
    append_json_line(&agent_results_path(store, session_id), record)
}

/// Load rich result context for a session, or synthesize a compatible view
/// from the lifecycle JSONL when the sidecar predates agent integration.
pub fn load_agent_results(
    store: &SessionStore,
    state: &SessionState,
) -> Result<Vec<AgentResultRecord>> {
    let path = agent_results_path(store, &state.session_id);
    if path.exists() {
        let file =
            File::open(&path).with_context(|| format!("open agent result records {:?}", path))?;
        return BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(line_number, line)| match line {
                Ok(line) if line.trim().is_empty() => None,
                Ok(line) => Some(
                    serde_json::from_str(&line)
                        .with_context(|| format!("parse agent result line {}", line_number + 1)),
                ),
                Err(error) => Some(Err(error.into())),
            })
            .collect();
    }

    Ok(store
        .results(&state.session_id)?
        .into_iter()
        .map(|result| {
            let (termination, decision) = match result.status {
                crate::result::ExperimentStatus::Keep => {
                    (ExperimentTermination::Completed, ExperimentDecision::Keep)
                }
                crate::result::ExperimentStatus::Discard => (
                    ExperimentTermination::Completed,
                    ExperimentDecision::Discard,
                ),
                crate::result::ExperimentStatus::Crash => {
                    (ExperimentTermination::Crashed, ExperimentDecision::Failure)
                }
            };
            AgentResultRecord {
                contract_version: AGENT_CONTRACT_VERSION,
                result,
                termination,
                primary_decision: decision,
                decision,
                constraint_violations: Vec::new(),
                stdout: String::new(),
                stderr: String::new(),
                changed_files: Vec::new(),
                experiment_workspace: PathBuf::new(),
            }
        })
        .collect())
}

fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open agent result records {:?}", path))?;
    serde_json::to_writer(&mut file, value).context("serialize agent result record")?;
    file.write_all(b"\n")
        .context("terminate agent result record")?;
    file.sync_data().context("flush agent result record")?;
    Ok(())
}

/// Discover valid research programs below a workspace without running any
/// command in them.  Invalid Markdown files are ignored during discovery and
/// are reported when explicitly selected for execution.
pub fn discover_programs(workspace_root: &Path) -> Result<Vec<ProgramSummary>> {
    let root = workspace_root
        .canonicalize()
        .with_context(|| format!("canonicalize autoresearch workspace {:?}", workspace_root))?;
    if !root.is_dir() {
        bail!(
            "autoresearch workspace is not a directory: {:?}",
            workspace_root
        );
    }

    let mut files = Vec::new();
    collect_markdown(&root, &root, 0, &mut files)?;
    let mut programs = files
        .into_iter()
        .filter_map(|path| {
            let program = ResearchProgram::from_file(&path).ok()?;
            program.validate().ok()?;
            let relative = path.strip_prefix(&root).ok()?.to_path_buf();
            Some(ProgramSummary::from_program(relative, &program))
        })
        .collect::<Vec<_>>();
    programs.sort_by(|left, right| left.path.cmp(&right.path));
    programs.dedup_by(|left, right| left.path == right.path);
    Ok(programs)
}

/// Parse one explicitly selected program and return a workspace-relative
/// summary.  This is intentionally separate from discovery so a user can
/// select a program that is outside the default scan directories but still
/// inside the workspace.
pub fn summarize_program(
    workspace_root: &Path,
    program_path: &Path,
) -> Result<(PathBuf, ResearchProgram, ProgramSummary)> {
    let root = workspace_root.canonicalize()?;
    let candidate = if program_path.is_absolute() {
        program_path.to_path_buf()
    } else {
        root.join(program_path)
    };
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("canonicalize research program {:?}", candidate))?;
    if !canonical.starts_with(&root) {
        bail!("research program must remain inside the workspace");
    }
    let program = ResearchProgram::from_file(&canonical)
        .with_context(|| format!("parse research program {:?}", canonical))?;
    program
        .validate()
        .with_context(|| format!("validate research program {:?}", canonical))?;
    let relative = canonical.strip_prefix(&root)?.to_path_buf();
    let summary = ProgramSummary::from_program(relative.clone(), &program);
    Ok((canonical, program, summary))
}

fn collect_markdown(
    root: &Path,
    current: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > 6 || output.len() >= 256 {
        return Ok(());
    }
    for entry in
        fs::read_dir(current).with_context(|| format!("read program directory {:?}", current))?
    {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            collect_markdown(root, &path, depth + 1, output)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("md")
            && path.starts_with(root)
        {
            output.push(path);
        }
        if output.len() >= 256 {
            break;
        }
    }
    Ok(())
}
