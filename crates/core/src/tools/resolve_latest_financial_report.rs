use crate::research::{ReportCadence, ResearchContext};
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::json;
use std::sync::Arc;

pub struct ResolveLatestFinancialReportTool {
    context: Arc<ResearchContext>,
}

impl ResolveLatestFinancialReportTool {
    pub fn new(context: Arc<ResearchContext>) -> Self {
        Self { context }
    }
}

#[async_trait::async_trait]
impl super::ToolExecutor for ResolveLatestFinancialReportTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "resolve_latest_financial_report".into(),
            description: "Resolve the newest eligible observed financial result for an issuer and cadence; use after web_search or fetch_url before composing the report".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "issuer": { "type": "string" },
                    "cadence": {
                        "type": "string",
                        "enum": ["latest", "annual", "quarterly"],
                        "description": "Use latest for an unspecified cadence, annual for an annual-only request, or quarterly for a quarterly-only request"
                    }
                },
                "required": ["issuer", "cadence"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let issuer = match required_string(call, "issuer") {
            Ok(issuer) => issuer,
            Err(error) => return failure(call, error),
        };
        let cadence = match required_string(call, "cadence").and_then(|value| {
            ReportCadence::parse(value)
                .ok_or_else(|| format!("invalid field cadence value: {}", call.input["cadence"]))
        }) {
            Ok(cadence) => cadence,
            Err(error) => return failure(call, error),
        };

        let resolution = self.context.resolve_latest_report(issuer, cadence);
        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: serde_json::to_string_pretty(&resolution)
                .unwrap_or_else(|_| "{\"status\":\"unavailable\"}".into()),
            error: None,
        }
    }
}

fn required_string<'a>(call: &'a ToolCall, field: &'static str) -> Result<&'a str, String> {
    call.input[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing required field {field}"))
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
    use crate::research::{
        EvidenceRecord, ReportEvidenceMetadata, ReportStatus, ReportType, ReportingCalendar,
        SourceAuthority,
    };
    use crate::tools::ToolExecutor;
    use chrono::{TimeZone, Utc};

    fn as_of() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap()
    }

    fn call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "resolve-1".into(),
            name: "resolve_latest_financial_report".into(),
            input,
        }
    }

    #[tokio::test]
    async fn returns_an_explicit_quarterly_fallback_when_annual_is_unavailable() {
        let context = Arc::new(ResearchContext::new(as_of()));
        context.record_evidence(EvidenceRecord {
            url: "https://investor.example.com/q3-2026".into(),
            title: Some("Microsoft FY2026 Q3 reported results".into()),
            snippet: None,
            retrieved_at: as_of(),
            response_status: Some(200),
            http_date: None,
            published_at: Some(as_of() - chrono::Duration::days(30)),
            authority: SourceAuthority::Official,
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2026 Q3".into()),
                period_end: Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()),
                report_type: Some(ReportType::Quarterly),
                calendar: Some(ReportingCalendar::Fiscal),
                status: Some(ReportStatus::Reported),
            }),
        });
        let tool = ResolveLatestFinancialReportTool::new(context);

        let result = tool
            .execute(&call(json!({
                "issuer": "Microsoft",
                "cadence": "annual"
            })))
            .await;

        assert!(result.success, "{result:?}");
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "fallback");
        assert_eq!(output["selected"]["candidate"]["report_type"], "quarterly");
        assert!(
            output["limitation"]
                .as_str()
                .is_some_and(|limitation| limitation.contains("annual"))
        );
    }
}
