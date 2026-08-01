//! Durable autoresearch session lifecycle and result persistence.

use crate::program::{MetricGoal, ResearchProgram};
use crate::result::{ExperimentResult, ExperimentStatus};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Durable lifecycle state for an autoresearch session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// The session has been started and may accept research results.
    Running,
    /// The session was deliberately stopped and can be resumed.
    Stopped,
    /// The session finished all work.
    Completed,
    /// The session stopped because of an unrecoverable error.
    Failed,
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Stopped => write!(f, "stopped"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// JSON-persisted metadata for one autoresearch session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// Stable identifier used by the CLI lifecycle commands.
    pub session_id: String,
    /// The program file selected when the session was started.
    pub program_path: PathBuf,
    /// Human-readable program name parsed from the program file.
    pub program_name: String,
    /// Workspace in which the session operates.
    pub workspace: PathBuf,
    /// Metric goal copied from the selected program for recovery.
    pub metric_goal: MetricGoal,
    /// First successful metric recorded by this session.
    pub baseline_metric: Option<f64>,
    /// Best non-crash result recorded by this session.
    pub best_result: Option<ExperimentResult>,
    /// Number of durable results recorded by this session.
    pub iteration_count: u64,
    /// Current durable lifecycle state.
    pub status: SessionStatus,
    /// Time at which the session was created.
    pub created_at: DateTime<Utc>,
    /// Time at which the session metadata was last changed.
    pub updated_at: DateTime<Utc>,
}

/// Durable storage for autoresearch sessions belonging to one workspace.
#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    /// Create a store rooted at an explicit session directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create the canonical store for a workspace.
    pub fn for_workspace(workspace: impl AsRef<Path>) -> Self {
        Self::new(
            workspace
                .as_ref()
                .join(".nca")
                .join("autoresearch")
                .join("sessions"),
        )
    }

    /// Return the directory containing the session state and result files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Start and durably persist a new session.
    pub fn start(
        &self,
        requested_id: Option<&str>,
        program_path: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
    ) -> Result<SessionState> {
        let workspace = canonical_directory(workspace.as_ref())?;
        let program_path = resolve_program_path(program_path.as_ref(), &workspace)?;
        let program = ResearchProgram::from_file(&program_path).map_err(|error| {
            anyhow::anyhow!("invalid research program {:?}: {error}", program_path)
        })?;
        validate_program(&program)?;

        let session_id = requested_id
            .map(validate_session_id)
            .transpose()?
            .unwrap_or_else(generate_session_id);
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create autoresearch session directory {:?}", self.root))?;

        let state_path = self.state_path(&session_id);
        if state_path.exists() {
            bail!(
                "autoresearch session '{session_id}' already exists; use resume or choose a new session id"
            );
        }

        let now = Utc::now();
        let state = SessionState {
            session_id: session_id.clone(),
            program_path,
            program_name: program.name,
            workspace,
            metric_goal: program.metric_goal,
            baseline_metric: None,
            best_result: None,
            iteration_count: 0,
            status: SessionStatus::Running,
            created_at: now,
            updated_at: now,
        };

        write_new_json(&state_path, &state)?;
        File::create(self.results_path(&session_id))
            .with_context(|| format!("create autoresearch results for session '{session_id}'"))?;
        Ok(state)
    }

    /// Load a durable session by id.
    pub fn load(&self, session_id: &str) -> Result<SessionState> {
        validate_session_id(session_id)?;
        let path = self.state_path(session_id);
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("load autoresearch session '{session_id}'"))?;
        let state: SessionState = serde_json::from_str(&contents)
            .with_context(|| format!("parse autoresearch session '{session_id}'"))?;
        if state.session_id != session_id {
            bail!(
                "autoresearch session file {:?} contains id '{}' instead of '{session_id}'",
                path,
                state.session_id
            );
        }
        Ok(state)
    }

    /// List durable sessions, newest metadata first.
    pub fn list(&self) -> Result<Vec<SessionState>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.root)
            .with_context(|| format!("list autoresearch sessions in {:?}", self.root))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .context("autoresearch session file has no valid id")?;
            sessions.push(self.load(id)?);
        }
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        Ok(sessions)
    }

    /// Find a session by id, or the newest session when no id is supplied.
    pub fn resolve(&self, session_id: Option<&str>) -> Result<SessionState> {
        match session_id {
            Some(id) => self.load(id),
            None => self
                .list()?
                .into_iter()
                .next()
                .context("no autoresearch sessions found"),
        }
    }

    /// Return the stable id for the next experiment in a session.
    ///
    /// The id is derived from the last durably recorded iteration. If a
    /// process is interrupted before recording its result, the same id is
    /// deliberately produced after restart so the execution layer can make
    /// an explicit, identity-checked recovery attempt instead of silently
    /// allocating a different workspace.
    pub fn next_experiment_id(&self, session_id: &str) -> Result<String> {
        let state = self.load(session_id)?;
        Ok(format!(
            "{}-{}",
            state.session_id,
            state.iteration_count.saturating_add(1)
        ))
    }

    /// Stop a running session without deleting its state or results.
    pub fn stop(&self, session_id: &str) -> Result<SessionState> {
        let mut state = self.load(session_id)?;
        if state.status != SessionStatus::Running {
            bail!(
                "cannot stop inactive autoresearch session '{session_id}' (status: {})",
                state.status
            );
        }
        state.status = SessionStatus::Stopped;
        state.updated_at = Utc::now();
        self.save(&state)?;
        Ok(state)
    }

    /// Recover a stopped session. Resuming a running session is idempotent so
    /// a process restart cannot create duplicate execution.
    pub fn resume(&self, session_id: &str) -> Result<SessionState> {
        let mut state = self.load(session_id)?;
        match state.status {
            SessionStatus::Running => Ok(state),
            SessionStatus::Stopped => {
                state.status = SessionStatus::Running;
                state.updated_at = Utc::now();
                self.save(&state)?;
                Ok(state)
            }
            status => bail!("cannot resume autoresearch session '{session_id}' (status: {status})"),
        }
    }

    /// Append a result and update the session's durable counters and best result.
    pub fn record_result(
        &self,
        session_id: &str,
        result: ExperimentResult,
    ) -> Result<SessionState> {
        let mut state = self.load(session_id)?;
        if state.status != SessionStatus::Running {
            bail!(
                "cannot record a result for inactive autoresearch session '{session_id}' (status: {})",
                state.status
            );
        }

        let results_path = self.results_path(session_id);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&results_path)
            .with_context(|| format!("open autoresearch results {:?}", results_path))?;
        serde_json::to_writer(&mut file, &result).context("serialize autoresearch result")?;
        file.write_all(b"\n")
            .context("terminate autoresearch result record")?;

        state.iteration_count += 1;
        if result.status != ExperimentStatus::Crash {
            if state.baseline_metric.is_none() {
                state.baseline_metric = Some(result.metric_value);
            }
            let is_better = state
                .best_result
                .as_ref()
                .map(|best| {
                    better_metric(state.metric_goal, result.metric_value, best.metric_value)
                })
                .unwrap_or(true);
            if is_better {
                state.best_result = Some(result);
            }
        }
        state.updated_at = Utc::now();
        self.save(&state)?;
        Ok(state)
    }

    /// Load all persisted results without executing the research program.
    pub fn results(&self, session_id: &str) -> Result<Vec<ExperimentResult>> {
        self.load(session_id)?;
        let path = self.results_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file =
            File::open(&path).with_context(|| format!("open autoresearch results {:?}", path))?;
        let mut results = Vec::new();
        for (line_number, line) in BufReader::new(file).lines().enumerate() {
            let line =
                line.with_context(|| format!("read autoresearch result line {}", line_number + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            results.push(
                serde_json::from_str(&line).with_context(|| {
                    format!("parse autoresearch result line {}", line_number + 1)
                })?,
            );
        }
        Ok(results)
    }

    fn save(&self, state: &SessionState) -> Result<()> {
        write_atomic_json(&self.state_path(&state.session_id), state)
    }

    fn state_path(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.json"))
    }

    fn results_path(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.results.jsonl"))
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        bail!(
            "autoresearch workspace does not exist or is not a directory: {:?}",
            path
        );
    }
    path.canonicalize()
        .with_context(|| format!("canonicalize autoresearch workspace {:?}", path))
}

fn resolve_program_path(path: &Path, workspace: &Path) -> Result<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let from_workspace = workspace.join(path);
        if from_workspace.is_file() {
            from_workspace
        } else {
            std::env::current_dir()
                .context("read current directory")?
                .join(path)
        }
    };
    if !candidate.is_file() {
        bail!(
            "research program does not exist or is not a file: {:?}",
            candidate
        );
    }
    candidate
        .canonicalize()
        .with_context(|| format!("canonicalize research program {:?}", candidate))
}

fn validate_program(program: &ResearchProgram) -> Result<()> {
    program.validate()
}

fn validate_session_id(session_id: &str) -> Result<String> {
    if session_id.is_empty()
        || session_id == "."
        || session_id == ".."
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        bail!("invalid autoresearch session id '{session_id}'");
    }
    Ok(session_id.to_string())
}

fn generate_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("research-{nanos}")
}

fn better_metric(goal: MetricGoal, candidate: f64, current: f64) -> bool {
    match goal {
        MetricGoal::Minimize => candidate < current,
        MetricGoal::Maximize => candidate > current,
    }
}

fn write_new_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create autoresearch session state {:?}", path))?;
    serde_json::to_writer_pretty(&mut file, value).context("serialize autoresearch session")?;
    file.write_all(b"\n")
        .context("terminate autoresearch session state")?;
    file.sync_all()
        .context("flush autoresearch session state")?;
    Ok(())
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    {
        let mut file = File::create(&temporary)
            .with_context(|| format!("create temporary session state {:?}", temporary))?;
        serde_json::to_writer_pretty(&mut file, value)
            .context("serialize autoresearch session state")?;
        file.write_all(b"\n")
            .context("terminate autoresearch session state")?;
        file.sync_all()
            .context("flush autoresearch session state")?;
    }
    fs::rename(&temporary, path).with_context(|| {
        format!(
            "replace autoresearch session state {:?} with {:?}",
            path, temporary
        )
    })?;
    Ok(())
}
