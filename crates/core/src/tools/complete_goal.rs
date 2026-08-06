//! Accept an explicit, evidence-backed completion claim for the current todo list.

use nca_common::session::validate_goal_completion_claim;
use nca_common::todo::TodoStatus;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::update_todos::{CompletionClaimStore, TodoStore};

/// Tool that records completion only after validating the authoritative checklist.
pub struct CompleteGoalTool {
    todos: TodoStore,
    completion_claim: CompletionClaimStore,
}

impl CompleteGoalTool {
    pub fn new(todos: TodoStore, completion_claim: CompletionClaimStore) -> Self {
        Self {
            todos,
            completion_claim,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for CompleteGoalTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "complete_goal".into(),
            description: "Submit an explicit completion claim for the current goal. This is accepted only when the checklist is non-empty and every item is completed, with concrete evidence and verification exactly `passed`.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Concise summary of the completed objective."
                    },
                    "evidence": {
                        "type": "string",
                        "description": "Concrete evidence supporting completion, such as checks run and their results."
                    },
                    "verification": {
                        "type": "string",
                        "enum": ["passed"],
                        "description": "Must be exactly `passed`."
                    }
                },
                "required": ["summary", "evidence", "verification"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let string_field = |name: &str| {
            call.input
                .get(name)
                .and_then(|value| value.as_str())
                .ok_or_else(|| format!("{name} must be a string"))
        };
        let summary = match string_field("summary") {
            Ok(value) => value,
            Err(error) => return failure(call, error),
        };
        let evidence = match string_field("evidence") {
            Ok(value) => value,
            Err(error) => return failure(call, error),
        };
        let verification = match string_field("verification") {
            Ok(value) => value,
            Err(error) => return failure(call, error),
        };

        let todos = match self.todos.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return failure(call, "todo store lock poisoned".into()),
        };
        if todos.is_empty() {
            return failure(call, "cannot complete an empty checklist".into());
        }
        if todos
            .iter()
            .any(|todo| todo.status != TodoStatus::Completed)
        {
            return failure(call, "all checklist items must be completed".into());
        }

        let claim = match validate_goal_completion_claim(summary, evidence, verification) {
            Ok(claim) => claim,
            Err(error) => return failure(call, error),
        };
        let response_summary = claim.summary.clone();
        let mut guard = match self.completion_claim.lock() {
            Ok(guard) => guard,
            Err(_) => return failure(call, "completion claim store lock poisoned".into()),
        };
        *guard = Some(claim);

        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: format!("Goal completion verified: {response_summary}"),
            error: None,
        }
    }
}

fn failure(call: &ToolCall, error: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nca_common::todo::AgentTodo;
    use std::sync::{Arc, Mutex};

    fn completed_todos() -> TodoStore {
        Arc::new(Mutex::new(vec![AgentTodo {
            id: "1".into(),
            content: "Ship it".into(),
            status: TodoStatus::Completed,
            source: None,
        }]))
    }

    fn call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "claim-1".into(),
            name: "complete_goal".into(),
            input,
        }
    }

    #[tokio::test]
    async fn accepts_only_completed_checklist_with_concrete_passed_claim() {
        let todos = completed_todos();
        let claims = Arc::new(Mutex::new(None));
        let tool = CompleteGoalTool::new(todos, claims.clone());
        let result = tool
            .execute(&call(serde_json::json!({
                "summary": " shipped ",
                "evidence": " cargo test passed ",
                "verification": "passed"
            })))
            .await;
        assert!(result.success, "{result:?}");
        assert_eq!(claims.lock().unwrap().as_ref().unwrap().summary, "shipped");
        assert_eq!(
            claims.lock().unwrap().as_ref().unwrap().evidence,
            "cargo test passed"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_claims_without_creating_state() {
        let todos = completed_todos();
        let claims = Arc::new(Mutex::new(None));
        let tool = CompleteGoalTool::new(todos, claims.clone());
        for input in [
            serde_json::json!({"summary":"", "evidence":"test", "verification":"passed"}),
            serde_json::json!({"summary":"done", "evidence":"", "verification":"passed"}),
            serde_json::json!({"summary":"done", "evidence":"test", "verification":"failed"}),
        ] {
            assert!(!tool.execute(&call(input)).await.success);
            assert!(claims.lock().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn rejects_empty_incomplete_and_cancelled_checklists() {
        for todos in [
            Vec::new(),
            vec![AgentTodo {
                id: "1".into(),
                content: "Work".into(),
                status: TodoStatus::InProgress,
                source: None,
            }],
            vec![AgentTodo {
                id: "1".into(),
                content: "Abandon".into(),
                status: TodoStatus::Cancelled,
                source: None,
            }],
        ] {
            let todos = Arc::new(Mutex::new(todos));
            let claims = Arc::new(Mutex::new(None));
            let tool = CompleteGoalTool::new(todos, claims.clone());
            assert!(
                !tool
                    .execute(&call(serde_json::json!({
                        "summary":"done", "evidence":"test", "verification":"passed"
                    })))
                    .await
                    .success
            );
            assert!(claims.lock().unwrap().is_none());
        }
    }
}
