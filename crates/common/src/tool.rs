use serde::{Deserialize, Serialize};

const HOSTED_TOOL_TYPE_PARAMETER: &str = "__nca_hosted_tool_type";
const HOSTED_TOOL_PARAMETERS_PARAMETER: &str = "__nca_hosted_tool_parameters";

/// Definition of a tool the agent can invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Construct a provider-hosted tool for adapters that understand it.
    ///
    /// The existing tool contract is intentionally retained so native tool
    /// definitions remain source-compatible. Hosted tools are tagged in the
    /// parameters value and adapters can reject them when their protocol does
    /// not support provider-managed execution.
    pub fn hosted(
        tool_type: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        let tool_type = tool_type.into();
        Self {
            name: tool_type.clone(),
            description: description.into(),
            parameters: serde_json::json!({
                HOSTED_TOOL_TYPE_PARAMETER: tool_type,
                HOSTED_TOOL_PARAMETERS_PARAMETER: parameters,
            }),
        }
    }

    /// Return the provider-hosted type, if this definition is hosted.
    pub fn hosted_tool_type(&self) -> Option<&str> {
        self.parameters
            .get(HOSTED_TOOL_TYPE_PARAMETER)
            .and_then(serde_json::Value::as_str)
            .filter(|tool_type| !tool_type.trim().is_empty())
    }

    pub fn is_hosted(&self) -> bool {
        self.hosted_tool_type().is_some()
    }
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// The result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub success: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Permission tier for a tool or command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTier {
    Allowed,
    Ask,
    Denied,
}
