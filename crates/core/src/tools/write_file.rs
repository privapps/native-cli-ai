use crate::research::ResearchContext;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::sync::Arc;

use super::ToolExecutor;

pub struct WriteFileTool {
    workspace_root: std::path::PathBuf,
    research_context: Arc<ResearchContext>,
}

impl WriteFileTool {
    pub fn new(workspace_root: std::path::PathBuf, research_context: Arc<ResearchContext>) -> Self {
        Self {
            workspace_root,
            research_context,
        }
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

        if let Err(error) = self.research_context.validate_report_output(content) {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Refusing to write financial report: {error}")),
            };
        }

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
    use crate::research::{
        EvidenceRecord, FinancialReportCandidate, ReportStatus, ReportType, ReportingCalendar,
        SourceAuthority,
    };
    use chrono::{NaiveDate, TimeZone, Utc};
    use serde_json::json;

    fn make_call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            input,
        }
    }

    fn as_of() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap()
    }

    fn candidate() -> FinancialReportCandidate {
        FinancialReportCandidate {
            issuer: "Microsoft".into(),
            report_type: ReportType::Annual,
            period_label: "FY2025".into(),
            period_end: NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            calendar: ReportingCalendar::Fiscal,
            status: ReportStatus::Reported,
            publication_url: "https://investor.example.com/results".into(),
            publication_date: as_of(),
        }
    }

    fn validated_context() -> Arc<ResearchContext> {
        let context = Arc::new(ResearchContext::new(as_of()));
        let candidate = candidate();
        context.record_evidence(EvidenceRecord {
            url: candidate.publication_url.clone(),
            title: Some("FY2025 results".into()),
            snippet: None,
            retrieved_at: as_of(),
            response_status: Some(200),
            http_date: Some(as_of()),
            published_at: Some(candidate.publication_date),
            authority: SourceAuthority::Official,
            report_metadata: Some(crate::research::ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2025".into()),
                period_end: Some(NaiveDate::from_ymd_opt(2025, 6, 30).unwrap()),
                report_type: Some(ReportType::Annual),
                calendar: Some(ReportingCalendar::Fiscal),
                status: Some(ReportStatus::Reported),
            }),
        });
        context.validate_candidate(candidate).unwrap();
        context
    }

    #[tokio::test]
    async fn refuses_to_write_an_unvalidated_financial_report() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().canonicalize().unwrap();
        let tool = WriteFileTool::new(workspace_root, Arc::new(ResearchContext::new(as_of())));

        let result = tool
            .execute(&make_call(json!({
                "path": "report.md",
                "content": "# Microsoft Data\n\nFY2026 revenue and net income were reported."
            })))
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not passed"));
        assert!(!dir.path().join("report.md").exists());
    }

    #[tokio::test]
    async fn writes_a_validated_financial_report() {
        let dir = tempfile::tempdir().unwrap();
        let url = "https://investor.example.com/results";
        let tool = WriteFileTool::new(dir.path().canonicalize().unwrap(), validated_context());

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
