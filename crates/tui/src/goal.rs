//! Shared prompt construction for the interactive autonomous goal command.

use nca_common::todo::{AgentTodo, TodoStatus};

/// Build the first visible user turn for a fresh autonomous objective.
pub fn initial_prompt(objective: &str) -> String {
    format!(
        "Work autonomously toward this objective: {objective}\n\n\
Before doing substantial work, replace the current checklist with a fresh,\
complete checklist for this objective using update_todos. Execute the work,\
keep every todo status current, and verify each item before marking it\
completed. Do not claim the objective is complete while any checklist item\
remains incomplete. When the objective has independent workstreams, create\
sub-agents with spawn_subagent and start those tasks in parallel. Use parallel\
delegation for independent investigation, implementation, or verification;\
keep dependent steps ordered, avoid duplicate work, and integrate each result\
before claiming completion."
    )
}

/// Build the first visible user turn for continuing a saved incomplete goal.
pub fn continuation_prompt() -> &'static str {
    "Continue the existing objective autonomously. Inspect the current checklist, work on the next incomplete item, keep all todo statuses current, and verify concrete progress. When the remaining work has independent workstreams, create sub-agents with spawn_subagent and start those tasks in parallel. Keep dependent steps ordered, avoid duplicate work, and integrate each result. Do not claim completion while any checklist item remains incomplete."
}

/// Build the bounded final verification request after the checklist is complete.
pub fn final_verification_prompt() -> &'static str {
    "The checklist is fully completed, but the completion handshake is still missing. Perform final verification now. If the final checks are independent, create sub-agents with spawn_subagent and run them in parallel, then integrate their results. Call complete_goal with a concise summary, concrete evidence, and verification exactly `passed`. Do not rely on prose alone."
}

/// A checklist is a valid successful goal only when it has work and every
/// item is completed.
pub fn is_complete(todos: &[AgentTodo]) -> bool {
    !todos.is_empty()
        && todos
            .iter()
            .all(|todo| todo.status == TodoStatus::Completed)
}

pub fn has_cancelled_item(todos: &[AgentTodo]) -> bool {
    todos
        .iter()
        .any(|todo| todo.status == TodoStatus::Cancelled)
}

/// Update the consecutive unchanged-snapshot counter. `None` is the first
/// post-turn snapshot and therefore cannot be a stall yet.
pub fn unchanged_snapshot_count(
    previous: Option<&[AgentTodo]>,
    current: &[AgentTodo],
    count: u32,
) -> u32 {
    if previous == Some(current) {
        count.saturating_add(1)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_prompt_requires_fresh_verified_checklist() {
        let prompt = initial_prompt("ship the feature");
        assert!(prompt.contains("ship the feature"));
        assert!(prompt.contains("fresh"));
        assert!(prompt.contains("update_todos"));
        assert!(prompt.contains("verify"));
        assert!(prompt.contains("spawn_subagent"));
        assert!(prompt.contains("in parallel"));
    }

    #[test]
    fn continuation_prompt_requires_next_incomplete_item() {
        let prompt = continuation_prompt();
        assert!(prompt.contains("next incomplete item"));
        assert!(prompt.contains("todo statuses"));
        assert!(prompt.contains("spawn_subagent"));
        assert!(prompt.contains("in parallel"));
    }

    #[test]
    fn final_verification_prompt_requires_handshake_evidence() {
        let prompt = final_verification_prompt();
        assert!(prompt.contains("complete_goal"));
        assert!(prompt.contains("concrete evidence"));
        assert!(prompt.contains("passed"));
        assert!(prompt.contains("spawn_subagent"));
        assert!(prompt.contains("in parallel"));
    }

    fn todo(id: &str, status: TodoStatus) -> AgentTodo {
        AgentTodo {
            id: id.into(),
            content: format!("item {id}"),
            status,
            source: None,
        }
    }

    #[test]
    fn completion_requires_non_empty_all_completed_snapshot() {
        assert!(!is_complete(&[]));
        assert!(!is_complete(&[todo("1", TodoStatus::Cancelled)]));
        assert!(!is_complete(&[todo("1", TodoStatus::InProgress)]));
        assert!(is_complete(&[todo("1", TodoStatus::Completed)]));
    }

    #[test]
    fn unchanged_counter_resets_when_any_snapshot_field_changes() {
        let first = vec![todo("1", TodoStatus::Pending)];
        let second = first.clone();
        assert_eq!(unchanged_snapshot_count(None, &first, 0), 0);
        assert_eq!(unchanged_snapshot_count(Some(&first), &second, 0), 1);

        let changed = vec![todo("1", TodoStatus::Completed)];
        assert_eq!(unchanged_snapshot_count(Some(&second), &changed, 1), 0);
    }
}
