pub mod apply_patch;
pub mod ask_question;
pub mod autoresearch;
pub mod bash;
pub mod code_intel_tool;
pub mod copy_path;
pub mod create_directory;
pub mod delete_path;
pub mod edit_file;
pub mod fetch_url;
pub mod filesystem;
pub mod git;
pub mod invoke_skill;
pub mod list_directory;
pub mod mcp;
pub mod move_path;
pub mod rename_path;
pub mod replace_match;
pub mod resolve_latest_financial_report;
pub mod run_validation;
pub mod search;
pub mod skill_hints;
pub mod spawn_subagent;
pub mod types;
pub mod update_todos;
pub mod validate_financial_report;
pub mod web_search;
pub mod write_file;
pub mod write_validated_financial_report;

pub use ask_question::AskQuestionTool;
pub use autoresearch::AutoresearchTool;
pub use invoke_skill::InvokeSkillTool;
pub use resolve_latest_financial_report::ResolveLatestFinancialReportTool;
pub use skill_hints::RecentSkillHints;
pub use update_todos::{TodoStore, UpdateTodosTool, validate_todos};
pub use validate_financial_report::ValidateFinancialReportTool;
pub use write_validated_financial_report::WriteValidatedFinancialReportTool;

use crate::research::ResearchContext;
use chrono::Utc;
use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

tokio::task_local! {
    static EXECUTION_YOLO: bool;
}

pub const FINANCIAL_RESEARCH_SKILL_COMMAND: &str = "financial-research";

/// Returns whether the current tool invocation is running under yolo.
/// Tool implementations use this to bypass nca-level guards while retaining
/// actual OS and subprocess errors.
pub fn yolo_enabled() -> bool {
    EXECUTION_YOLO.try_with(|value| *value).unwrap_or(false)
}

/// Registry of available tools the agent can invoke.
pub struct ToolRegistry {
    tools: Vec<Box<dyn ToolExecutor>>,
    research_context: Arc<ResearchContext>,
    financial_research_enabled: Arc<AtomicBool>,
    autoresearch_authorized: Arc<AtomicBool>,
    yolo: bool,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            research_context: Arc::new(ResearchContext::new(Utc::now().date_naive())),
            financial_research_enabled: Arc::new(AtomicBool::new(false)),
            autoresearch_authorized: Arc::new(AtomicBool::new(false)),
            yolo: false,
        }
    }

    pub fn research_context(&self) -> Arc<ResearchContext> {
        self.research_context.clone()
    }

    /// Return the generic evidence ledger shared by the registry's research
    /// tools. Financial validation remains behind `ResearchContext`; generic
    /// collectors can observe this ledger without depending on that policy.
    pub fn evidence_ledger(&self) -> Arc<crate::evidence::EvidenceLedger> {
        self.research_context.evidence_ledger()
    }

    pub fn financial_research_capability(&self) -> Arc<AtomicBool> {
        self.financial_research_enabled.clone()
    }

    pub fn register(&mut self, tool: Box<dyn ToolExecutor>) {
        self.tools.push(tool);
    }

    pub fn enable_financial_research(&self) {
        self.financial_research_enabled
            .store(true, Ordering::Release);
    }

    pub fn financial_research_enabled(&self) -> bool {
        self.financial_research_enabled.load(Ordering::Acquire)
    }

    pub fn set_yolo(&mut self, yolo: bool) {
        self.yolo = yolo;
        if yolo {
            self.enable_financial_research();
        }
    }

    /// Mark autoresearch execution as explicitly selected by the user or a
    /// parent session.  Discovery and inspection remain available without
    /// this capability; execution tools enforce it at their own boundary.
    pub fn authorize_autoresearch(&self) {
        self.autoresearch_authorized.store(true, Ordering::Release);
    }

    pub fn autoresearch_authorized(&self) -> Arc<AtomicBool> {
        self.autoresearch_authorized.clone()
    }

    pub fn is_yolo(&self) -> bool {
        self.yolo
    }

    pub fn with_default_readonly_tools(
        workspace_root: std::path::PathBuf,
        web_config: WebConfig,
    ) -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(AutoresearchTool::new_with_authorization(
            workspace_root.clone(),
            registry.autoresearch_authorized.clone(),
        )));
        registry.register(Box::new(filesystem::ReadFileTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(search::SearchCodeTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(list_directory::ListDirectoryTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(git::GitStatusTool::new(workspace_root.clone())));
        registry.register(Box::new(git::GitDiffTool::new(workspace_root)));
        let research_context = registry.research_context();
        registry.register(Box::new(web_search::WebSearchTool::new(
            web_config.clone(),
            research_context.clone(),
        )));
        registry.register(Box::new(fetch_url::FetchUrlTool::new(
            web_config,
            research_context.clone(),
        )));
        registry.register(Box::new(ValidateFinancialReportTool::new(research_context)));
        registry.register(Box::new(ResolveLatestFinancialReportTool::new(
            registry.research_context(),
        )));
        registry
    }

    pub fn with_default_full_tools(
        workspace_root: std::path::PathBuf,
        web_config: WebConfig,
    ) -> Self {
        let mut registry = Self::with_default_readonly_tools(workspace_root.clone(), web_config);
        registry.register(Box::new(code_intel_tool::CodeIntelTool::new(
            crate::code_intel::FastLocalCodeIntel::new(workspace_root.clone()),
        )));
        registry.register(Box::new(write_file::WriteFileTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(WriteValidatedFinancialReportTool::new(
            workspace_root.clone(),
            registry.research_context(),
        )));
        registry.register(Box::new(create_directory::CreateDirectoryTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(apply_patch::ApplyPatchTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(edit_file::EditFileTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(replace_match::ReplaceMatchTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(rename_path::RenamePathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(move_path::MovePathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(copy_path::CopyPathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(delete_path::DeletePathTool::new(
            workspace_root.clone(),
        )));
        registry.register(Box::new(run_validation::RunValidationTool::new(
            workspace_root,
        )));
        registry
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .filter(|tool| {
                (self.yolo || !is_financial_tool(&tool.definition()))
                    || self.financial_research_enabled.load(Ordering::Acquire)
            })
            .map(|t| t.definition())
            .collect()
    }

    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        if !self.yolo
            && is_financial_tool_name(&call.name)
            && !self.financial_research_enabled.load(Ordering::Acquire)
        {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(
                    "financial research is opt-in; invoke the `financial-research` skill first"
                        .into(),
                ),
            };
        }
        for tool in &self.tools {
            if tool.definition().name == call.name {
                return EXECUTION_YOLO.scope(self.yolo, tool.execute(call)).await;
            }
        }

        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(format!("Unknown tool: {}", call.name)),
        }
    }
}

fn is_financial_tool(definition: &ToolDefinition) -> bool {
    is_financial_tool_name(&definition.name)
}

fn is_financial_tool_name(name: &str) -> bool {
    matches!(
        name,
        "validate_financial_report"
            | "resolve_latest_financial_report"
            | "write_validated_financial_report"
    )
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait implemented by each tool.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, call: &ToolCall) -> ToolResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn yolo_allows_registry_tools_to_use_external_paths() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("created.txt");
        let mut registry = ToolRegistry::with_default_full_tools(
            workspace.path().to_path_buf(),
            WebConfig::default(),
        );
        registry.set_yolo(true);
        let result = registry
            .execute(&ToolCall {
                id: "yolo-write".into(),
                name: "write_file".into(),
                input: serde_json::json!({
                    "path": target,
                    "content": "yolo",
                }),
            })
            .await;
        assert!(result.success, "{result:?}");
        assert_eq!(tokio::fs::read_to_string(target).await.unwrap(), "yolo");
    }
}
