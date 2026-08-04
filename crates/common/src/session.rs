use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use crate::execution::ExecutionContext;
use crate::message::Message;

/// Metadata for a persisted session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub workspace: PathBuf,
    pub model: String,
    pub status: SessionStatus,
    pub pid: Option<u32>,
    pub socket_path: Option<PathBuf>,
    /// Git worktree path if the session runs in an isolated worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    /// Branch name the session operates on (e.g. `nca/<session-id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Base branch the worktree was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Parent session id if this is a child/sub-agent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// IDs of child sessions spawned from this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_session_ids: Vec<String>,
    /// Summary inherited from parent session for context continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_summary: Option<String>,
    /// Why this child session was spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_reason: Option<String>,
    /// Persisted compact summary for resume and memory surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_summary: Option<String>,
    /// External orchestration metadata for headless worker runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationContext>,
    /// Authorization context active for this invocation. This is metadata,
    /// not a persisted launch default.
    #[serde(default)]
    pub execution: ExecutionContext,
}

pub const MAX_PROMPT_HISTORY_ENTRIES: usize = 100;

/// Keep prompt history bounded while preserving literal text and duplicate entries.
pub fn normalize_prompt_history(entries: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut history: Vec<String> = entries
        .into_iter()
        .filter(|entry| !entry.trim().is_empty())
        .collect();
    if history.len() > MAX_PROMPT_HISTORY_ENTRIES {
        let first = history.len() - MAX_PROMPT_HISTORY_ENTRIES;
        history.drain(..first);
    }
    history
}

/// Derive best-effort prompt history for sessions written before the dedicated
/// history field existed. Existing user messages are already canonical (and
/// may contain expanded file/skill context), so this is intentionally only a
/// compatibility fallback.
pub fn legacy_prompt_history(messages: &[Message]) -> Vec<String> {
    normalize_prompt_history(
        messages
            .iter()
            .filter(|message| message.role == crate::message::Role::User)
            .map(|message| message.content.to_summary_text()),
    )
}
/// Full session state, including conversation history and cost tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    /// Literal ordinary chat drafts, in submission order. Older session JSON
    /// may omit this field; callers should use [`Self::prompt_history_or_legacy`]
    /// when loading a session for interactive recall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_history: Vec<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub estimated_cost_usd: f64,
    /// Current session todo list (authoritative snapshot; also event-sourced).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<crate::todo::AgentTodo>,
}

/// Lightweight session summary for machine-readable orchestration surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub workspace: PathBuf,
    pub model: String,
    pub status: SessionStatus,
    pub pid: Option<u32>,
    pub socket_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationContext>,
    #[serde(default)]
    pub execution: ExecutionContext,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub estimated_cost_usd: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<crate::todo::AgentTodo>,
}

/// Optional metadata injected by an external orchestrator for headless runs.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct OrchestrationContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl SessionState {
    /// Return persisted prompt history, or derive a capped compatibility
    /// fallback from existing user messages when the field was absent/empty.
    pub fn prompt_history_or_legacy(&self) -> Vec<String> {
        if self.prompt_history.is_empty() {
            legacy_prompt_history(&self.messages)
        } else {
            normalize_prompt_history(self.prompt_history.clone())
        }
    }
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.meta.id.clone(),
            created_at: self.meta.created_at,
            updated_at: self.meta.updated_at,
            workspace: self.meta.workspace.clone(),
            model: self.meta.model.clone(),
            status: self.meta.status.clone(),
            pid: self.meta.pid,
            socket_path: self.meta.socket_path.clone(),
            worktree_path: self.meta.worktree_path.clone(),
            branch: self.meta.branch.clone(),
            base_branch: self.meta.base_branch.clone(),
            parent_session_id: self.meta.parent_session_id.clone(),
            child_session_ids: self.meta.child_session_ids.clone(),
            inherited_summary: self.meta.inherited_summary.clone(),
            spawn_reason: self.meta.spawn_reason.clone(),
            session_summary: self.meta.session_summary.clone(),
            orchestration: self.meta.orchestration.clone(),
            execution: self.meta.execution,
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            estimated_cost_usd: self.estimated_cost_usd,
            todos: self.todos.clone(),
        }
    }
}

impl OrchestrationContext {
    pub fn from_env() -> Option<Self> {
        let mut metadata = BTreeMap::new();
        for (key, value) in env::vars() {
            if let Some(meta_key) = key.strip_prefix("NCA_ORCH_META_")
                && !value.trim().is_empty()
            {
                metadata.insert(meta_key.to_ascii_lowercase(), value);
            }
        }

        let ctx = Self {
            orchestrator: non_empty_env("NCA_ORCH_NAME"),
            run_id: non_empty_env("NCA_ORCH_RUN_ID"),
            task_id: non_empty_env("NCA_ORCH_TASK_ID"),
            task_ref: non_empty_env("NCA_ORCH_TASK_REF"),
            parent_run_id: non_empty_env("NCA_ORCH_PARENT_RUN_ID"),
            callback_url: non_empty_env("NCA_ORCH_CALLBACK_URL"),
            metadata,
        };

        if ctx.is_empty() { None } else { Some(ctx) }
    }

    pub fn is_empty(&self) -> bool {
        self.orchestrator.is_none()
            && self.run_id.is_none()
            && self.task_id.is_none()
            && self.task_ref.is_none()
            && self.parent_run_id.is_none()
            && self.callback_url.is_none()
            && self.metadata.is_empty()
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Completed,
    Error,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::OrchestrationContext;
    use super::{
        MAX_PROMPT_HISTORY_ENTRIES, SessionMeta, SessionState, SessionStatus,
        legacy_prompt_history, normalize_prompt_history,
    };
    use crate::message::Message;
    use crate::todo::{AgentTodo, TodoStatus};
    use chrono::Utc;
    use std::env;
    use std::path::PathBuf;

    #[test]
    fn prompt_history_normalization_preserves_literal_duplicates_and_cap() {
        let entries = (0..102)
            .map(|index| {
                if index == 1 {
                    " \n".to_string()
                } else {
                    format!("entry-{index}\n  ")
                }
            })
            .collect::<Vec<_>>();
        let normalized = normalize_prompt_history(entries);

        assert_eq!(normalized.len(), MAX_PROMPT_HISTORY_ENTRIES);
        assert_eq!(normalized.first().map(String::as_str), Some("entry-2\n  "));
        assert_eq!(normalized.last().map(String::as_str), Some("entry-101\n  "));
        assert_eq!(
            normalize_prompt_history(vec!["duplicate".into(), "duplicate".into()]),
            vec!["duplicate", "duplicate"]
        );
    }

    #[test]
    fn legacy_prompt_history_uses_newest_user_messages_only() {
        let messages = vec![
            Message::user("old"),
            Message::assistant("answer"),
            Message::user("new\ntext"),
        ];
        assert_eq!(
            legacy_prompt_history(&messages),
            vec!["old".to_string(), "new\ntext".to_string()]
        );
    }

    #[test]
    fn orchestration_context_reads_env_contract() {
        let vars = [
            ("NCA_ORCH_NAME", "paperclip-wrapper"),
            ("NCA_ORCH_RUN_ID", "run-123"),
            ("NCA_ORCH_TASK_ID", "task-456"),
            ("NCA_ORCH_TASK_REF", "issue/99"),
            ("NCA_ORCH_PARENT_RUN_ID", "run-122"),
            ("NCA_ORCH_CALLBACK_URL", "http://localhost/callback"),
            ("NCA_ORCH_META_CHANNEL", "ticket"),
        ];

        for (key, value) in vars {
            unsafe { env::set_var(key, value) };
        }

        let ctx = OrchestrationContext::from_env().expect("context from env");
        assert_eq!(ctx.orchestrator.as_deref(), Some("paperclip-wrapper"));
        assert_eq!(ctx.run_id.as_deref(), Some("run-123"));
        assert_eq!(ctx.task_id.as_deref(), Some("task-456"));
        assert_eq!(ctx.task_ref.as_deref(), Some("issue/99"));
        assert_eq!(ctx.parent_run_id.as_deref(), Some("run-122"));
        assert_eq!(
            ctx.callback_url.as_deref(),
            Some("http://localhost/callback")
        );
        assert_eq!(
            ctx.metadata.get("channel").map(String::as_str),
            Some("ticket")
        );

        for (key, _) in vars {
            unsafe { env::remove_var(key) };
        }
    }

    #[test]
    fn old_session_json_loads_with_empty_todos() {
        let raw = r#"{
            "meta": {
                "id": "s1",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "workspace": "/tmp",
                "model": "m",
                "status": "running"
            },
            "messages": [
                {"role": "user", "content": "legacy prompt"},
                {"role": "assistant", "content": "answer"}
            ],
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "estimated_cost_usd": 0.0
        }"#;
        let state: SessionState = serde_json::from_str(raw).expect("deserialize");
        assert!(state.todos.is_empty());
        assert_eq!(state.prompt_history, Vec::<String>::new());
        assert_eq!(state.prompt_history_or_legacy(), vec!["legacy prompt"]);
        assert!(state.snapshot().todos.is_empty());
        assert!(!state.meta.execution.yolo);
    }

    #[test]
    fn session_todos_roundtrip() {
        let now = Utc::now();
        let state = SessionState {
            meta: SessionMeta {
                id: "s1".into(),
                created_at: now,
                updated_at: now,
                workspace: PathBuf::from("/tmp"),
                model: "m".into(),
                status: SessionStatus::Running,
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
            messages: Vec::new(),
            prompt_history: vec![
                "  café\n\n終わり  \n".into(),
                "duplicate".into(),
                "duplicate".into(),
            ],
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost_usd: 0.0,
            todos: vec![AgentTodo {
                id: "1".into(),
                content: "Ship it".into(),
                status: TodoStatus::InProgress,
                source: None,
            }],
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.todos.len(), 1);
        assert_eq!(back.prompt_history, state.prompt_history);
        assert_eq!(back.snapshot().todos[0].content, "Ship it");
    }

    #[test]
    fn yolo_metadata_roundtrips_without_becoming_a_launch_default() {
        let now = Utc::now();
        let state = SessionState {
            meta: SessionMeta {
                id: "yolo".into(),
                created_at: now,
                updated_at: now,
                workspace: PathBuf::from("/tmp"),
                model: "m".into(),
                status: SessionStatus::Completed,
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
                execution: crate::execution::ExecutionContext::yolo(),
            },
            messages: Vec::new(),
            prompt_history: Vec::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            estimated_cost_usd: 0.0,
            todos: Vec::new(),
        };
        let encoded = serde_json::to_string(&state).expect("serialize");
        let decoded: SessionState = serde_json::from_str(&encoded).expect("deserialize");
        assert!(decoded.meta.execution.yolo);
        assert!(!crate::execution::ExecutionContext::default().yolo);
    }
}
