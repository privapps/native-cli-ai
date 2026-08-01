//! Agent-facing autoresearch contract.
//!
//! Discovery and inspection are safe, non-executing operations.  Mutating or
//! executing operations require the host to authorize an explicit
//! `/autoresearch` invocation; the normal [`ApprovalPolicy`](crate::approval::ApprovalPolicy)
//! still decides whether the individual tool call may run.

use nca_autoresearch::agent::{
    AGENT_CONTRACT_VERSION, AgentResultRecord, ContinuationPolicy, append_agent_result_for_session,
    discover_programs, load_agent_results, summarize_program,
};
use nca_autoresearch::experiment::{ExperimentConfig, ExperimentRunner};
use nca_autoresearch::result::ResultsLogger;
use nca_autoresearch::{ExperimentDecision, GitManager, SessionStatus, SessionStore};
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ToolExecutor;

/// Manual skill command used by the host to authorize execution.
pub const AGENT_SKILL_COMMAND: &str = nca_autoresearch::AGENT_SKILL_COMMAND;

const OP_DISCOVER: &str = "discover";
const OP_START: &str = "start";
const OP_STATUS: &str = "status";
const OP_RESULTS: &str = "results";
const OP_CONTINUE: &str = "continue";
const OP_CANCEL: &str = "cancel";

/// Stable tool for program discovery and bounded session interaction.
pub struct AutoresearchTool {
    workspace_root: PathBuf,
    explicitly_authorized: Arc<AtomicBool>,
    cancelled_sessions: Arc<Mutex<HashSet<String>>>,
}

impl AutoresearchTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::new_with_authorization(workspace_root, Arc::new(AtomicBool::new(false)))
    }

    pub fn new_with_authorization(
        workspace_root: PathBuf,
        explicitly_authorized: Arc<AtomicBool>,
    ) -> Self {
        Self {
            workspace_root,
            explicitly_authorized,
            cancelled_sessions: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn authorization_capability(&self) -> Arc<AtomicBool> {
        self.explicitly_authorized.clone()
    }

    pub fn authorize_explicit_invocation(&self) {
        self.explicitly_authorized.store(true, Ordering::Release);
    }

    fn is_authorized(&self) -> bool {
        self.explicitly_authorized.load(Ordering::Acquire)
    }

    fn require_explicit_invocation(&self) -> Result<(), String> {
        if self.is_authorized() {
            Ok(())
        } else {
            Err(format!(
                "autoresearch execution requires an explicit user invocation; use /{AGENT_SKILL_COMMAND} or the nca autoresearch command first"
            ))
        }
    }

    fn mark_cancelled(&self, session_id: &str) -> Result<(), String> {
        self.cancelled_sessions
            .lock()
            .map_err(|_| "autoresearch cancellation state is unavailable".to_string())?
            .insert(session_id.to_string());
        Ok(())
    }

    fn is_cancelled(&self, session_id: &str) -> Result<bool, String> {
        Ok(self
            .cancelled_sessions
            .lock()
            .map_err(|_| "autoresearch cancellation state is unavailable".to_string())?
            .contains(session_id))
    }

    fn clear_cancelled(&self, session_id: &str) -> Result<(), String> {
        self.cancelled_sessions
            .lock()
            .map_err(|_| "autoresearch cancellation state is unavailable".to_string())?
            .remove(session_id);
        Ok(())
    }

    fn store(&self) -> SessionStore {
        SessionStore::for_workspace(&self.workspace_root)
    }

    fn required_string(input: &Value, key: &str) -> Result<String, String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("{key} is required"))
    }

    fn success(call: &ToolCall, output: Value) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_else(|error| error.to_string()),
            error: None,
        }
    }

    fn failure(call: &ToolCall, error: impl Into<String>) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(error.into()),
        }
    }

    async fn execute_continue(&self, call: &ToolCall) -> Result<Value, String> {
        self.require_explicit_invocation()?;
        let session_id = Self::required_string(&call.input, "session_id")?;
        let descriptions = call
            .input
            .get("descriptions")
            .and_then(Value::as_array)
            .ok_or_else(|| "descriptions must be a non-empty array".to_string())?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::trim)
                    .filter(|description| !description.is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        "each experiment description must be a non-empty string".to_string()
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if descriptions.is_empty() {
            return Err("descriptions must be a non-empty array".into());
        }

        let state = self
            .store()
            .load(&session_id)
            .map_err(|error| error.to_string())?;
        if state.status != SessionStatus::Running {
            return Err(format!(
                "autoresearch session '{session_id}' is not running (status: {})",
                state.status
            ));
        }
        let (_, program, program_summary) =
            summarize_program(&state.workspace, &state.program_path)
                .map_err(|error| error.to_string())?;
        let policy = call
            .input
            .get("policy")
            .cloned()
            .map(serde_json::from_value::<ContinuationPolicy>)
            .transpose()
            .map_err(|error| format!("invalid continuation policy: {error}"))?
            .unwrap_or_default()
            .validate(&program)
            .map_err(|error| error.to_string())?;
        if descriptions.len() as u64 > policy.max_iterations {
            return Err(format!(
                "{} descriptions exceed the continuation's {}-iteration limit",
                descriptions.len(),
                policy.max_iterations
            ));
        }

        let command = Self::required_string(&call.input, "command")?;
        let args = call
            .input
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(ToOwned::to_owned)
                            .ok_or_else(|| "experiment args must be strings".to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();

        self.clear_cancelled(&session_id)?;
        let store = self.store();
        let git = GitManager::new(&state.workspace);
        let logger = ResultsLogger::new(store.root().join(format!("{session_id}.experiments.tsv")));
        let experiments_root = store.root().join("experiments").join(&session_id);
        let started_at = Instant::now();
        let mut records = Vec::new();
        let mut stopped_reason = "completed";
        let mut baseline = state.best_result.as_ref().map(|result| result.metric_value);

        for (index, description) in descriptions.iter().enumerate() {
            if self.is_cancelled(&session_id)? {
                stopped_reason = "cancelled";
                break;
            }
            if started_at.elapsed() >= Duration::from_secs(policy.max_total_seconds) {
                stopped_reason = "time_budget_exhausted";
                break;
            }

            let experiment_id = if index == 0 {
                store
                    .next_experiment_id(&session_id)
                    .map_err(|error| error.to_string())?
            } else {
                format!("{session_id}-{}", state.iteration_count + index as u64 + 1)
            };
            let config = ExperimentConfig {
                working_dir: state.workspace.clone(),
                command: command.clone(),
                args: args.clone(),
                time_budget_seconds: program.time_budget_seconds.max(1),
                log_file: None,
                memory_limit_gb: program.max_memory_gb,
                kill_timeout_factor: 2,
            };
            let runner = ExperimentRunner::new(config);
            let remaining = policy
                .max_total_seconds
                .saturating_sub(started_at.elapsed().as_secs())
                .max(1);
            let isolated = tokio::time::timeout(
                Duration::from_secs(remaining),
                runner.run_isolated_with_program(
                    &git,
                    &logger,
                    &experiments_root,
                    &experiment_id,
                    &program
                        .editable_files
                        .iter()
                        .map(|file| file.path.clone())
                        .collect::<Vec<_>>(),
                    &program,
                    baseline,
                    true,
                    description.clone(),
                ),
            )
            .await
            .map_err(|_| "autoresearch continuation time budget exhausted".to_string())?
            .map_err(|error| error.to_string())?;

            let record = AgentResultRecord::from_isolated(&isolated);
            let result = isolated.result.clone();
            store
                .record_result(&session_id, result)
                .map_err(|error| error.to_string())?;
            append_agent_result_for_session(&store, &session_id, &record)
                .map_err(|error| error.to_string())?;
            baseline = if isolated.decision == ExperimentDecision::Keep {
                isolated.metric_value
            } else {
                baseline
            };
            records.push(record);
        }

        if self.is_cancelled(&session_id)? {
            stopped_reason = "cancelled";
        }
        let final_state = store.load(&session_id).map_err(|error| error.to_string())?;
        Ok(json!({
            "contract_version": AGENT_CONTRACT_VERSION,
            "operation": OP_CONTINUE,
            "program": program_summary,
            "session": final_state,
            "results": records,
            "continuation": {
                "requested_iterations": descriptions.len(),
                "executed_iterations": records.len(),
                "max_iterations": policy.max_iterations,
                "max_total_seconds": policy.max_total_seconds,
                "stopped_reason": stopped_reason,
                "approval_required": true,
            }
        }))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for AutoresearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "autoresearch".into(),
            description: "Discover and inspect research programs, or run explicitly authorized bounded autoresearch experiments. Discovery never executes a program; execution requires explicit user invocation and normal approval.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": [OP_DISCOVER, OP_START, OP_STATUS, OP_RESULTS, OP_CONTINUE, OP_CANCEL],
                        "description": "Operation to perform; defaults to discover"
                    },
                    "program": { "type": "string", "description": "Workspace-relative Markdown program path for start" },
                    "session_id": { "type": "string" },
                    "command": { "type": "string", "description": "Direct executable for a bounded experiment" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "descriptions": { "type": "array", "items": { "type": "string" }, "description": "Agent-selected changes to try, in order" },
                    "policy": {
                        "type": "object",
                        "properties": {
                            "max_iterations": { "type": "integer", "minimum": 1, "maximum": 8 },
                            "max_total_seconds": { "type": "integer", "minimum": 1, "maximum": 3600 }
                        }
                    }
                },
                "required": ["operation"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let operation = call
            .input
            .get("operation")
            .and_then(Value::as_str)
            .unwrap_or(OP_DISCOVER);
        let result = match operation {
            OP_DISCOVER => discover_programs(&self.workspace_root)
                .map(|programs| {
                    json!({
                        "contract_version": AGENT_CONTRACT_VERSION,
                        "operation": OP_DISCOVER,
                        "execution_started": false,
                        "programs": programs,
                    })
                })
                .map_err(|error| error.to_string()),
            OP_START => {
                if let Err(error) = self.require_explicit_invocation() {
                    Err(error)
                } else {
                    let program_path = Self::required_string(&call.input, "program");
                    match program_path {
                        Ok(program_path) => {
                            summarize_program(&self.workspace_root, &PathBuf::from(&program_path))
                                .and_then(|(_, _, summary)| {
                                    self.store()
                                        .start(
                                            call.input.get("session_id").and_then(Value::as_str),
                                            PathBuf::from(program_path),
                                            &self.workspace_root,
                                        )
                                        .map(|state| {
                                            json!({
                                                "contract_version": AGENT_CONTRACT_VERSION,
                                                "operation": OP_START,
                                                "program": summary,
                                                "session": state,
                                                "execution_started": false,
                                                "approval_required": true,
                                            })
                                        })
                                })
                                .map_err(|error| error.to_string())
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            OP_STATUS => self
                .store()
                .resolve(call.input.get("session_id").and_then(Value::as_str))
                .map(|state| {
                    json!({
                        "contract_version": AGENT_CONTRACT_VERSION,
                        "operation": OP_STATUS,
                        "session": state,
                    })
                })
                .map_err(|error| error.to_string()),
            OP_RESULTS => self
                .store()
                .resolve(call.input.get("session_id").and_then(Value::as_str))
                .and_then(|state| {
                    let results = load_agent_results(&self.store(), &state)?;
                    Ok(json!({
                        "contract_version": AGENT_CONTRACT_VERSION,
                        "operation": OP_RESULTS,
                        "session": state,
                        "results": results,
                    }))
                })
                .map_err(|error| error.to_string()),
            OP_CANCEL => {
                if let Err(error) = self.require_explicit_invocation() {
                    Err(error)
                } else {
                    let session_id = Self::required_string(&call.input, "session_id");
                    match session_id {
                        Ok(session_id) => self
                            .mark_cancelled(&session_id)
                            .and_then(|()| {
                                self.store()
                                    .stop(&session_id)
                                    .map_err(|error| error.to_string())
                            })
                            .map(|state| {
                                json!({
                                    "contract_version": AGENT_CONTRACT_VERSION,
                                    "operation": OP_CANCEL,
                                    "session": state,
                                    "stopped_reason": "cancelled",
                                })
                            })
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error),
                    }
                }
            }
            OP_CONTINUE => self.execute_continue(call).await,
            _ => Err(format!("unknown autoresearch operation '{operation}'")),
        };

        match result {
            Ok(output) => Self::success(call, output),
            Err(error) => Self::failure(call, error),
        }
    }
}
