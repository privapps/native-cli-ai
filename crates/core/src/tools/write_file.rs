use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

pub struct WriteFileTool {
    workspace_root: std::path::PathBuf,
}

impl WriteFileTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or overwrite a file inside the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let content = call.input["content"].as_str().unwrap_or("");

        let full_path = self.workspace_root.join(path);

        let parent = match full_path.parent() {
            Some(parent) => parent,
            None => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("Invalid write path".into()),
                };
            }
        };

        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create parent directories: {err}")),
            };
        }

        let canonical_parent = match parent.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve parent path: {err}")),
                };
            }
        };

        if !canonical_parent.starts_with(&self.workspace_root) {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Path is outside the workspace".into()),
            };
        }

        match tokio::fs::write(&full_path, content).await {
            Ok(()) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output: format!("Wrote {}", full_path.display()),
                error: None,
            },
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write file: {err}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            input,
        }
    }

    #[tokio::test]
    async fn generic_write_does_not_classify_financial_content() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().canonicalize().unwrap();
        let tool = WriteFileTool::new(workspace_root);

        let result = tool
            .execute(&make_call(json!({
                "path": "report.md",
                "content": "# Microsoft Data\n\nFY2026 revenue and net income were reported."
            })))
            .await;

        assert!(result.success);
        assert!(dir.path().join("report.md").exists());
    }

    #[tokio::test]
    async fn generic_write_accepts_structured_financial_text() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://investor.example.com/results";
        let tool = WriteFileTool::new(dir.path().canonicalize().unwrap());

        let result = tool
            .execute(&make_call(json!({
                "path": "report.md",
                "content": format!("# Microsoft Annual Financial Report\n\nAs of: 2026-07-29T12:00:00Z\n\nIssuer: Microsoft\nReporting calendar: fiscal\nReport type: annual\nPeriod end: 2025-06-30\nPublication status: reported\nPublication date: 2026-07-29T12:00:00Z\nSource retrieved at: 2026-07-29T12:00:00Z\n\nFY2025 revenue and net income were reported.\n\nSource: {url}")
            })))
            .await;

        assert!(result.success, "{result:?}");
        assert!(dir.path().join("report.md").is_file());
    }
}
