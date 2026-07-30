pub mod apply_patch;
pub mod ask_question;
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

pub use ask_question::AskQuestionTool;
pub use invoke_skill::InvokeSkillTool;
pub use resolve_latest_financial_report::ResolveLatestFinancialReportTool;
pub use skill_hints::RecentSkillHints;
pub use update_todos::{TodoStore, UpdateTodosTool, validate_todos};
pub use validate_financial_report::ValidateFinancialReportTool;

use crate::research::ResearchContext;
use chrono::Utc;
use nca_common::config::WebConfig;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::sync::Arc;

/// Registry of available tools the agent can invoke.
pub struct ToolRegistry {
    tools: Vec<Box<dyn ToolExecutor>>,
    research_context: Arc<ResearchContext>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            research_context: Arc::new(ResearchContext::new(Utc::now())),
        }
    }

    pub fn research_context(&self) -> Arc<ResearchContext> {
        self.research_context.clone()
    }

    pub fn register(&mut self, tool: Box<dyn ToolExecutor>) {
        self.tools.push(tool);
    }

    pub fn with_default_readonly_tools(
        workspace_root: std::path::PathBuf,
        web_config: WebConfig,
    ) -> Self {
        let mut registry = Self::new();
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
        self.tools.iter().map(|t| t.definition()).collect()
    }

    pub async fn execute(&self, call: &ToolCall) -> ToolResult {
        for tool in &self.tools {
            if tool.definition().name == call.name {
                return tool.execute(call).await;
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
