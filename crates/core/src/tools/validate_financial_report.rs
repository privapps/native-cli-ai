use crate::research::{
    FinancialReportCandidate, ReportStatus, ReportType, ReportingCalendar, ResearchContext,
    parse_publication_date,
};
use chrono::NaiveDate;
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::json;
use std::sync::Arc;

pub struct ValidateFinancialReportTool {
    context: Arc<ResearchContext>,
}

impl ValidateFinancialReportTool {
    pub fn new(context: Arc<ResearchContext>) -> Self {
        Self { context }
    }
}

#[async_trait::async_trait]
impl super::ToolExecutor for ValidateFinancialReportTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "validate_financial_report".into(),
            description: "Validate a financial report candidate against the current as-of time and observed authoritative web evidence before presenting or writing it".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "issuer": { "type": "string" },
                    "report_type": {
                        "type": "string",
                        "enum": ["annual", "quarterly", "earnings_release", "filing"]
                    },
                    "period_label": { "type": "string" },
                    "period_end": {
                        "type": "string",
                        "description": "Completed reporting-period end date in YYYY-MM-DD form"
                    },
                    "reporting_calendar": {
                        "type": "string",
                        "enum": ["fiscal", "calendar"]
                    },
                    "status": {
                        "type": "string",
                        "enum": ["reported", "filed", "guidance", "estimate", "unverified"]
                    },
                    "publication_url": { "type": "string" },
                    "publication_date": {
                        "type": "string",
                        "description": "Publication timestamp in RFC3339 form or date in YYYY-MM-DD form"
                    }
                },
                "required": [
                    "issuer",
                    "report_type",
                    "period_label",
                    "period_end",
                    "reporting_calendar",
                    "status",
                    "publication_url",
                    "publication_date"
                ]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let result = (|| {
            let issuer = required_string(call, "issuer")?;
            let report_type =
                ReportType::parse(required_string(call, "report_type")?).ok_or_else(|| {
                    RequestError::InvalidField {
                        field: "report_type",
                        value: call.input["report_type"].to_string(),
                    }
                })?;
            let period_label = required_string(call, "period_label")?;
            let period_end =
                NaiveDate::parse_from_str(required_string(call, "period_end")?, "%Y-%m-%d")
                    .map_err(|_| RequestError::InvalidField {
                        field: "period_end",
                        value: call.input["period_end"].to_string(),
                    })?;
            let calendar = ReportingCalendar::parse(required_string(call, "reporting_calendar")?)
                .ok_or_else(|| RequestError::InvalidField {
                field: "reporting_calendar",
                value: call.input["reporting_calendar"].to_string(),
            })?;
            let status =
                ReportStatus::parse(required_string(call, "status")?).ok_or_else(|| {
                    RequestError::InvalidField {
                        field: "status",
                        value: call.input["status"].to_string(),
                    }
                })?;
            let publication_url = required_string(call, "publication_url")?;
            let publication_date =
                parse_publication_date(required_string(call, "publication_date")?).ok_or_else(
                    || RequestError::InvalidField {
                        field: "publication_date",
                        value: call.input["publication_date"].to_string(),
                    },
                )?;

            let candidate = FinancialReportCandidate {
                issuer: issuer.to_string(),
                report_type,
                period_label: period_label.to_string(),
                period_end,
                calendar,
                status,
                publication_url: publication_url.to_string(),
                publication_date,
            };
            self.context
                .validate_candidate(candidate)
                .map_err(RequestError::Validation)
        })();

        match result {
            Ok(validated) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "status": "verified",
                    "as_of": validated.as_of,
                    "source_authority": validated.source_authority,
                    "report": validated.candidate,
                }))
                .unwrap_or_else(|_| "{\"status\":\"verified\"}".into()),
                error: None,
            },
            Err(error) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RequestError {
    #[error("missing required field {0}")]
    MissingField(&'static str),
    #[error("invalid field {field} value: {value}")]
    InvalidField { field: &'static str, value: String },
    #[error("{0}")]
    Validation(#[from] crate::research::ReportValidationError),
}

fn required_string<'a>(call: &'a ToolCall, field: &'static str) -> Result<&'a str, RequestError> {
    call.input[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(RequestError::MissingField(field))
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
            id: "call-1".into(),
            name: "validate_financial_report".into(),
            input,
        }
    }

    fn context() -> Arc<ResearchContext> {
        let context = Arc::new(ResearchContext::new(as_of()));
        context.record_evidence(EvidenceRecord {
            url: "https://investor.example.com/results".into(),
            title: Some("Microsoft FY2025 annual results".into()),
            snippet: None,
            retrieved_at: as_of(),
            response_status: Some(200),
            http_date: Some(as_of()),
            published_at: Some(as_of()),
            authority: SourceAuthority::Official,
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2025".into()),
                period_end: Some(chrono::NaiveDate::from_ymd_opt(2025, 6, 30).unwrap()),
                report_type: Some(ReportType::Annual),
                calendar: Some(ReportingCalendar::Fiscal),
                status: Some(ReportStatus::Reported),
            }),
        });
        context
    }

    fn input() -> serde_json::Value {
        serde_json::json!({
            "issuer": "Microsoft",
            "report_type": "annual",
            "period_label": "FY2025",
            "period_end": "2025-06-30",
            "reporting_calendar": "fiscal",
            "status": "reported",
            "publication_url": "https://investor.example.com/results",
            "publication_date": "2026-07-29T12:00:00Z"
        })
    }

    #[tokio::test]
    async fn returns_machine_readable_verified_metadata() {
        let tool = ValidateFinancialReportTool::new(context());
        let result = tool.execute(&call(input())).await;

        assert!(result.success, "{result:?}");
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "verified");
        assert_eq!(output["report"]["period_label"], "FY2025");
    }

    #[tokio::test]
    async fn returns_a_tool_failure_for_a_future_period() {
        let tool = ValidateFinancialReportTool::new(context());
        let mut input = input();
        input["period_end"] = serde_json::json!("2026-12-31");
        let result = tool.execute(&call(input)).await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("after the as-of date"));
    }
}
