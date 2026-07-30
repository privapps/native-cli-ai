use serde::{Deserialize, Serialize};

/// Invocation-scoped authorization context.
///
/// This is deliberately not part of persisted configuration. A caller may
/// carry it through a session and its child sessions, but a later launch must
/// opt in again explicitly.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContext {
    #[serde(default)]
    pub yolo: bool,
}

impl ExecutionContext {
    pub const fn normal() -> Self {
        Self { yolo: false }
    }

    pub const fn yolo() -> Self {
        Self { yolo: true }
    }
}
