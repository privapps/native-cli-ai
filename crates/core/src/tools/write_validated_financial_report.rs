use crate::research::{ResearchContext, looks_like_financial_report};
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::path::PathBuf;
use std::sync::Arc;

use super::{ToolExecutor, write_file::WriteFileTool};

/// Domain-specific persistence for reports that have passed the research seam.
/// Generic `write_file` deliberately does not inspect content or infer a domain.
pub struct WriteValidatedFinancialReportTool {
    context: Arc<ResearchContext>,
    generic_writer: WriteFileTool,
}

impl WriteValidatedFinancialReportTool {
    pub fn new(workspace_root: PathBuf, context: Arc<ResearchContext>) -> Self {
        Self {
            context,
            generic_writer: WriteFileTool::new(workspace_root),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for WriteValidatedFinancialReportTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_validated_financial_report".into(),
            description: "Write a financial report only after the current turn has produced a validated report and the content discloses its verification metadata".into(),
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
        let Some(validated) = self.context.validated_report() else {
            return refusal(
                call,
                "no validated financial report capability exists for this turn",
            );
        };
        let content = call.input["content"].as_str().unwrap_or("");
        if !looks_like_financial_report(content) {
            return refusal(call, "content is not a financial report");
        }
        if let Err(error) = self.context.validate_report_output(content) {
            return refusal(call, &error.to_string());
        }
        if validated.source_authority != crate::research::SourceAuthority::Official {
            return refusal(call, "validated report source is not official");
        }
        self.generic_writer.execute(call).await
    }
}

fn refusal(call: &ToolCall, reason: &str) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(format!("Refusing to write financial report: {reason}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{
        EvidenceRecord, FinancialReportCandidate, ReportEvidenceMetadata, ReportStatus, ReportType,
        ReportingCalendar, SourceAuthority,
    };
    use chrono::{NaiveDate, TimeZone, Utc};
    use serde_json::json;

    fn as_of() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap()
    }

    fn call(content: &str) -> ToolCall {
        ToolCall {
            id: "financial-write-1".into(),
            name: "write_validated_financial_report".into(),
            input: json!({"path":"report.md", "content":content}),
        }
    }

    fn validated_context() -> Arc<ResearchContext> {
        let context = Arc::new(ResearchContext::new(as_of()));
        let candidate = FinancialReportCandidate {
            issuer: "Microsoft".into(),
            report_type: ReportType::Annual,
            period_label: "FY2025".into(),
            period_end: NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            calendar: ReportingCalendar::Fiscal,
            status: ReportStatus::Reported,
            publication_url: "https://investor.example.com/results".into(),
            publication_date: as_of(),
        };
        context.record_evidence(EvidenceRecord {
            url: candidate.publication_url.clone(),
            title: Some("FY2025 results".into()),
            snippet: None,
            retrieved_at: as_of(),
            response_status: Some(200),
            http_date: None,
            published_at: Some(candidate.publication_date),
            authority: SourceAuthority::Official,
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2025".into()),
                period_end: Some(candidate.period_end),
                report_type: Some(candidate.report_type),
                calendar: Some(candidate.calendar),
                status: Some(candidate.status),
            }),
        });
        context.validate_candidate(candidate).unwrap();
        context
    }

    #[tokio::test]
    async fn refuses_before_touching_target_without_validation() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteValidatedFinancialReportTool::new(
            dir.path().canonicalize().unwrap(),
            Arc::new(ResearchContext::new(as_of())),
        );
        let result = tool
            .execute(&call("# Microsoft Financial Report\nRevenue: unknown"))
            .await;
        assert!(!result.success);
        assert!(!dir.path().join("report.md").exists());
    }

    #[tokio::test]
    async fn writes_only_complete_validated_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteValidatedFinancialReportTool::new(
            dir.path().canonicalize().unwrap(),
            validated_context(),
        );
        let content = "# Microsoft Financial Report\n\nAs of: 2026-07-29\nIssuer: Microsoft\nReporting calendar: fiscal\nReport type: annual\nPeriod end: 2025-06-30\nPublication status: reported\nPublication date: 2026-07-29T12:00:00Z\nSource retrieved at: 2026-07-29T12:00:00Z\nFY2025 revenue was reported.\nSource: https://investor.example.com/results";
        let result = tool.execute(&call(content)).await;
        assert!(result.success, "{result:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("report.md")).unwrap(),
            content
        );
    }
}
