use crate::evidence::{EvidenceLedger, GenericEvidenceRecord};
pub use crate::evidence::{
    SourceAuthority, canonical_url, classify_source_authority, parse_publication_date,
};
use crate::financial_policy::FinancialResearchPolicy;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEvidenceMetadata {
    pub issuer: Option<String>,
    pub period_label: Option<String>,
    pub period_end: Option<NaiveDate>,
    pub report_type: Option<ReportType>,
    pub calendar: Option<ReportingCalendar>,
    pub status: Option<ReportStatus>,
}

impl ReportEvidenceMetadata {
    fn is_empty(&self) -> bool {
        self.issuer.is_none()
            && self.period_label.is_none()
            && self.period_end.is_none()
            && self.report_type.is_none()
            && self.calendar.is_none()
            && self.status.is_none()
    }

    pub(crate) fn merge_missing_from(&mut self, incoming: Self) {
        if self.issuer.is_none() {
            self.issuer = incoming.issuer;
        }
        if self.period_label.is_none() {
            self.period_label = incoming.period_label;
        }
        if self.period_end.is_none() {
            self.period_end = incoming.period_end;
        }
        if self.report_type.is_none() {
            self.report_type = incoming.report_type;
        }
        if self.calendar.is_none() {
            self.calendar = incoming.calendar;
        }
        if self.status.is_none() {
            self.status = incoming.status;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub url: String,
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub retrieved_at: DateTime<Utc>,
    pub response_status: Option<u16>,
    pub http_date: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub authority: SourceAuthority,
    pub report_metadata: Option<ReportEvidenceMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportType {
    Annual,
    Quarterly,
    EarningsRelease,
    Filing,
}

impl ReportType {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "annual" | "yearly" | "10-k" | "10k" => Some(Self::Annual),
            "quarterly" | "quarter" | "10-q" | "10q" => Some(Self::Quarterly),
            "earnings_release" | "earnings release" | "release" => Some(Self::EarningsRelease),
            "filing" => Some(Self::Filing),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Annual => "annual",
            Self::Quarterly => "quarterly",
            Self::EarningsRelease => "earnings_release",
            Self::Filing => "filing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportCadence {
    Latest,
    Annual,
    Quarterly,
}

impl ReportCadence {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "latest" | "current" | "any" => Some(Self::Latest),
            "annual" | "yearly" => Some(Self::Annual),
            "quarterly" | "quarter" => Some(Self::Quarterly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Annual => "annual",
            Self::Quarterly => "quarterly",
        }
    }

    pub(crate) fn accepts(self, report_type: ReportType) -> bool {
        match self {
            Self::Latest => true,
            Self::Annual => matches!(report_type, ReportType::Annual),
            Self::Quarterly => matches!(report_type, ReportType::Quarterly),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportingCalendar {
    Fiscal,
    Calendar,
}

impl ReportingCalendar {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fiscal" | "fy" => Some(Self::Fiscal),
            "calendar" | "cy" => Some(Self::Calendar),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fiscal => "fiscal",
            Self::Calendar => "calendar",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Reported,
    Filed,
    Guidance,
    Estimate,
    Unverified,
}

impl ReportStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "reported" | "actual" => Some(Self::Reported),
            "filed" | "filing" => Some(Self::Filed),
            "guidance" | "forecast" => Some(Self::Guidance),
            "estimate" | "estimated" | "analyst_estimate" => Some(Self::Estimate),
            "unverified" | "unknown" => Some(Self::Unverified),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Filed => "filed",
            Self::Guidance => "guidance",
            Self::Estimate => "estimate",
            Self::Unverified => "unverified",
        }
    }

    pub(crate) fn is_eligible(self) -> bool {
        matches!(self, Self::Reported | Self::Filed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinancialReportCandidate {
    pub issuer: String,
    pub report_type: ReportType,
    pub period_label: String,
    pub period_end: NaiveDate,
    pub calendar: ReportingCalendar,
    pub status: ReportStatus,
    pub publication_url: String,
    pub publication_date: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedFinancialReport {
    pub candidate: FinancialReportCandidate,
    pub as_of: NaiveDate,
    pub source_authority: SourceAuthority,
    pub source_retrieved_at: DateTime<Utc>,
    pub source_http_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Resolved,
    Fallback,
    Conflict,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinancialReportResolution {
    pub issuer: String,
    pub requested_cadence: ReportCadence,
    pub as_of: NaiveDate,
    pub status: ResolutionStatus,
    pub selected: Option<ValidatedFinancialReport>,
    pub limitation: Option<String>,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReportValidationError {
    #[error("report status `{0}` is not a reported or filed result")]
    IneligibleStatus(String),
    #[error("reporting period ended on {period_end}, after the as-of date {as_of}")]
    PeriodInFuture {
        period_end: NaiveDate,
        as_of: NaiveDate,
    },
    #[error("source publication date {publication_date} is after the as-of date {as_of}")]
    PublicationInFuture {
        publication_date: NaiveDate,
        as_of: NaiveDate,
    },
    #[error("source URL was not observed during this research turn")]
    SourceNotObserved,
    #[error("source is not an authoritative financial source")]
    SourceNotOfficial,
    #[error("source publication date is unavailable")]
    PublicationDateUnknown,
    #[error("source publication date does not match the observed source")]
    PublicationDateMismatch,
    #[error("conflicting official evidence prevents verification: {0}")]
    ConflictingEvidence(String),
    #[error("observed source metadata does not match the declared {0}")]
    SourceMetadataMismatch(&'static str),
    #[error("observed source metadata is unavailable for {0}")]
    SourceMetadataUnavailable(&'static str),
    #[error("financial report output is missing an as-of date")]
    OutputMissingAsOf,
    #[error("financial report output is missing a source URL")]
    OutputMissingSource,
    #[error("financial report output has not passed financial-period validation")]
    OutputNotValidated,
    #[error("validated period `{0}` is not named in the report output")]
    OutputPeriodMismatch(String),
    #[error("financial report output is missing {0}")]
    OutputMetadataMissing(&'static str),
    #[error("financial report output does not disclose the requested-cadence fallback")]
    OutputMissingFallbackDisclosure,
}

#[derive(Debug, Clone)]
pub struct ResearchContext {
    ledger: Arc<EvidenceLedger>,
    policy: Arc<FinancialResearchPolicy>,
}

pub trait IntoTurnDate {
    fn into_turn_date(self) -> NaiveDate;
}

impl IntoTurnDate for NaiveDate {
    fn into_turn_date(self) -> NaiveDate {
        self
    }
}

impl IntoTurnDate for DateTime<Utc> {
    fn into_turn_date(self) -> NaiveDate {
        self.date_naive()
    }
}

impl ResearchContext {
    pub fn new<T: IntoTurnDate>(as_of: T) -> Self {
        Self::with_ledger(Arc::new(EvidenceLedger::new(as_of.into_turn_date())))
    }

    pub fn with_ledger(ledger: Arc<EvidenceLedger>) -> Self {
        Self {
            policy: Arc::new(FinancialResearchPolicy::with_ledger(ledger.clone())),
            ledger,
        }
    }

    pub fn evidence_ledger(&self) -> Arc<EvidenceLedger> {
        self.ledger.clone()
    }

    /// Access the financial interpretation boundary used by specialized tools.
    pub fn financial_policy(&self) -> Arc<FinancialResearchPolicy> {
        self.policy.clone()
    }

    /// Record provenance-only evidence from a generic collector while
    /// preserving invalidation of any financial capability affected by it.
    pub fn record_generic_evidence(&self, evidence: GenericEvidenceRecord) {
        self.policy.record_generic_evidence(evidence);
    }

    pub fn begin_turn(&self, as_of: NaiveDate) {
        self.policy.begin_turn(as_of);
    }

    pub fn as_of(&self) -> NaiveDate {
        self.ledger.as_of()
    }

    pub fn record_evidence(&self, evidence: EvidenceRecord) {
        self.policy.record_evidence(evidence);
    }

    pub fn evidence(&self) -> Vec<EvidenceRecord> {
        self.policy.evidence()
    }

    /// Return every source observation, including repeated or conflicting
    /// observations that were merged into the normalized evidence view.
    pub fn observations(&self) -> Vec<EvidenceRecord> {
        self.policy.observations()
    }

    pub fn validate_candidate(
        &self,
        candidate: FinancialReportCandidate,
    ) -> Result<ValidatedFinancialReport, ReportValidationError> {
        self.policy.validate_candidate(candidate)
    }

    /// Select the newest eligible observed result for a requested cadence.
    ///
    /// `annual` is strict when an annual result exists. If no eligible annual
    /// result is available, the newest eligible quarter is returned as an
    /// explicitly labelled fallback so the caller cannot silently present it
    /// as an annual report. `latest` and `quarterly` do not cross cadences.
    pub fn resolve_latest_report(
        &self,
        issuer: &str,
        requested_cadence: ReportCadence,
    ) -> FinancialReportResolution {
        self.policy.resolve_latest_report(issuer, requested_cadence)
    }

    pub fn validated_report(&self) -> Option<ValidatedFinancialReport> {
        self.policy.validated_report()
    }

    pub fn validate_report_output(&self, output: &str) -> Result<(), ReportValidationError> {
        self.policy.validate_report_output(output)
    }

    pub fn final_response_warning(&self, output: &str) -> Option<String> {
        self.policy.final_response_warning(output)
    }

    pub fn annotate_final_response(&self, output: &str) -> String {
        self.policy.annotate_final_response(output)
    }
}

pub fn infer_report_metadata(text: &str) -> Option<ReportEvidenceMetadata> {
    let lower = text.to_ascii_lowercase();
    let metadata = ReportEvidenceMetadata {
        issuer: None,
        period_label: find_period_label(&lower),
        period_end: find_period_end(text, &lower),
        report_type: infer_report_type(&lower),
        calendar: infer_calendar(&lower),
        status: infer_report_status(&lower),
    };
    (!metadata.is_empty()).then_some(metadata)
}

/// Infer report metadata and attach an issuer only when the fetched evidence
/// visibly names the requested issuer. The resolver treats an unknown issuer
/// as ineligible rather than stamping the caller's issuer onto unrelated text.
pub fn infer_report_metadata_for_issuer(
    text: &str,
    issuer: Option<&str>,
) -> Option<ReportEvidenceMetadata> {
    let mut metadata = infer_report_metadata(text)?;
    if let Some(issuer) = issuer.map(str::trim).filter(|issuer| !issuer.is_empty()) {
        let lower_text = text.to_ascii_lowercase();
        let lower_issuer = issuer.to_ascii_lowercase();
        if contains_issuer_name(&lower_text, &lower_issuer) {
            metadata.issuer = Some(issuer.to_string());
        }
    }
    Some(metadata)
}

fn contains_issuer_name(text: &str, issuer: &str) -> bool {
    if issuer.is_empty() {
        return false;
    }
    let mut offset = 0;
    while let Some(relative) = text[offset..].find(issuer) {
        let start = offset + relative;
        let end = start + issuer.len();
        let starts_at_boundary = start == 0 || !text.as_bytes()[start - 1].is_ascii_alphanumeric();
        let ends_at_boundary = end == text.len() || !text.as_bytes()[end].is_ascii_alphanumeric();
        if starts_at_boundary && ends_at_boundary {
            return true;
        }
        offset = end.max(start + 1);
    }
    false
}

fn infer_calendar(lower: &str) -> Option<ReportingCalendar> {
    if let Some(period_label) = find_period_label(lower) {
        return if period_label.starts_with("FY") {
            Some(ReportingCalendar::Fiscal)
        } else {
            Some(ReportingCalendar::Calendar)
        };
    }
    if lower.contains("fiscal") {
        Some(ReportingCalendar::Fiscal)
    } else if lower.contains("calendar") {
        Some(ReportingCalendar::Calendar)
    } else {
        None
    }
}

fn find_period_label(lower: &str) -> Option<String> {
    for prefix in ["fy", "cy"] {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(prefix) {
            let start = offset + relative;
            let digits_start = start + prefix.len();
            let digits_end = digits_start + 4;
            if digits_end <= lower.len()
                && lower.as_bytes()[digits_start..digits_end]
                    .iter()
                    .all(u8::is_ascii_digit)
                && matches!(&lower[digits_start..digits_end][..2], "19" | "20")
            {
                return Some(format!(
                    "{}{}",
                    prefix.to_ascii_uppercase(),
                    &lower[digits_start..digits_end]
                ));
            }
            offset = digits_start.min(lower.len());
        }
    }
    None
}

fn find_period_end(text: &str, lower: &str) -> Option<NaiveDate> {
    for marker in [
        "period end",
        "period ended",
        "fiscal year ended",
        "year ended",
    ] {
        let Some(marker_start) = lower.find(marker) else {
            continue;
        };
        let scan_start = marker_start + marker.len();
        let scan_end = (scan_start + 80).min(text.len());
        for start in scan_start..scan_end {
            let end = start + 10;
            if end <= text.len()
                && text.as_bytes()[start..end]
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| {
                        if matches!(index, 4 | 7) {
                            *byte == b'-'
                        } else {
                            byte.is_ascii_digit()
                        }
                    })
                && let Ok(date) = NaiveDate::parse_from_str(&text[start..end], "%Y-%m-%d")
            {
                return Some(date);
            }
        }
    }
    None
}

fn infer_report_type(lower: &str) -> Option<ReportType> {
    if lower.contains("earnings release") {
        Some(ReportType::EarningsRelease)
    } else if lower.contains("10-k") || lower.contains("10k") || lower.contains("annual") {
        Some(ReportType::Annual)
    } else if lower.contains("10-q") || lower.contains("10q") || lower.contains("quarter") {
        Some(ReportType::Quarterly)
    } else if lower.contains("filing") {
        Some(ReportType::Filing)
    } else {
        None
    }
}

fn infer_report_status(lower: &str) -> Option<ReportStatus> {
    if lower.contains("guidance") || lower.contains("forecast") {
        Some(ReportStatus::Guidance)
    } else if lower.contains("estimate") || lower.contains("expected") {
        Some(ReportStatus::Estimate)
    } else if lower.contains("filed") || lower.contains("filing") {
        Some(ReportStatus::Filed)
    } else if lower.contains("reported") || lower.contains("results") || lower.contains("earnings")
    {
        Some(ReportStatus::Reported)
    } else {
        None
    }
}

pub fn looks_like_financial_report(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    let report_language = lower.contains("financial report")
        || lower.contains("financial_report")
        || lower.contains("earnings report")
        || lower.contains("earnings_report")
        || lower.contains("income statement")
        || lower.contains("annual performance")
        || lower.contains("financial results")
        || lower.contains("annual report")
        || lower.contains("quarterly report")
        || lower.contains("earnings release")
        || lower.contains("fiscal year");
    let financial_metric = lower.contains("revenue")
        || lower.contains("net income")
        || lower.contains("operating income")
        || lower.contains("earnings per share")
        || lower.contains(" eps ");
    let structured_report_metadata = lower.contains("issuer:")
        || lower.contains("period end")
        || lower.contains("publication status")
        || lower.contains("report type");
    let period_token = lower.contains("fy19")
        || lower.contains("fy20")
        || lower.contains("cy19")
        || lower.contains("cy20");
    financial_metric && (report_language || structured_report_metadata || period_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn as_of() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap()
    }

    fn evidence(url: &str, published_at: Option<DateTime<Utc>>) -> EvidenceRecord {
        EvidenceRecord {
            url: url.into(),
            title: Some("Official earnings release".into()),
            snippet: None,
            retrieved_at: as_of(),
            response_status: None,
            http_date: None,
            published_at,
            authority: SourceAuthority::Official,
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2025".into()),
                period_end: Some(NaiveDate::from_ymd_opt(2025, 6, 30).unwrap()),
                report_type: Some(ReportType::Annual),
                calendar: Some(ReportingCalendar::Fiscal),
                status: Some(ReportStatus::Reported),
            }),
        }
    }

    fn candidate(publication_date: DateTime<Utc>) -> FinancialReportCandidate {
        FinancialReportCandidate {
            issuer: "Microsoft".into(),
            report_type: ReportType::Annual,
            period_label: "FY2025".into(),
            period_end: NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            calendar: ReportingCalendar::Fiscal,
            status: ReportStatus::Reported,
            publication_url: "https://www.microsoft.com/investor-relations/earnings".into(),
            publication_date,
        }
    }

    #[test]
    fn rejects_a_period_ending_after_the_as_of_date() {
        let context = ResearchContext::new(as_of());
        let mut report = candidate(as_of());
        report.period_end = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        context.record_evidence(evidence(
            &report.publication_url,
            Some(report.publication_date),
        ));

        assert!(matches!(
            context.validate_candidate(report),
            Err(ReportValidationError::PeriodInFuture { .. })
        ));
    }

    #[test]
    fn rejects_a_source_published_after_the_as_of_time() {
        let context = ResearchContext::new(as_of());
        let publication_date = as_of() + chrono::Duration::days(1);
        let report = candidate(publication_date);
        context.record_evidence(evidence(&report.publication_url, Some(publication_date)));

        assert!(matches!(
            context.validate_candidate(report),
            Err(ReportValidationError::PublicationInFuture { .. })
        ));
    }

    #[test]
    fn rejects_guidance_even_when_the_period_and_source_are_current() {
        let context = ResearchContext::new(as_of());
        let mut report = candidate(as_of());
        report.status = ReportStatus::Guidance;
        context.record_evidence(evidence(
            &report.publication_url,
            Some(report.publication_date),
        ));

        assert!(matches!(
            context.validate_candidate(report),
            Err(ReportValidationError::IneligibleStatus(_))
        ));
    }

    #[test]
    fn requires_observed_official_evidence_and_a_known_publication_date() {
        let context = ResearchContext::new(as_of());
        let report = candidate(as_of());
        assert_eq!(
            context.validate_candidate(report.clone()),
            Err(ReportValidationError::SourceNotObserved)
        );

        context.record_evidence(EvidenceRecord {
            authority: SourceAuthority::Secondary,
            ..evidence(&report.publication_url, Some(report.publication_date))
        });
        assert_eq!(
            context.validate_candidate(report.clone()),
            Err(ReportValidationError::SourceNotOfficial)
        );

        let context = ResearchContext::new(as_of());
        context.record_evidence(evidence(&report.publication_url, None));
        assert_eq!(
            context.validate_candidate(report),
            Err(ReportValidationError::PublicationDateUnknown)
        );
    }

    #[test]
    fn validates_output_against_the_currently_validated_period_and_source() {
        let context = ResearchContext::new(as_of());
        let report = candidate(as_of());
        context.record_evidence(evidence(
            &report.publication_url,
            Some(report.publication_date),
        ));
        context.validate_candidate(report).unwrap();

        let output = "# Microsoft Annual Financial Report\n\nAs of: 2026-07-29T12:00:00Z\n\nIssuer: Microsoft\nReporting calendar: fiscal\nReport type: annual\nPeriod end: 2025-06-30\nPublication status: reported\nPublication date: 2026-07-29T12:00:00Z\nSource retrieved at: 2026-07-29T12:00:00Z\n\nFY2025 revenue was $1.\n\nSource: https://www.microsoft.com/investor-relations/earnings\nNet income was reported.";
        assert!(context.validate_report_output(output).is_ok());
    }

    #[test]
    fn keeps_the_newest_validated_period_when_candidates_arrive_out_of_order() {
        let context = ResearchContext::new(as_of());
        let older = candidate(as_of());
        let mut newer = older.clone();
        newer.period_label = "FY2026".into();
        newer.period_end = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        newer.publication_url = "https://www.microsoft.com/investor-relations/earnings-2026".into();

        context.record_evidence(evidence(
            &older.publication_url,
            Some(older.publication_date),
        ));
        context.record_evidence(EvidenceRecord {
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2026".into()),
                period_end: Some(newer.period_end),
                report_type: Some(ReportType::Annual),
                calendar: Some(ReportingCalendar::Fiscal),
                status: Some(ReportStatus::Reported),
            }),
            ..evidence(&newer.publication_url, Some(newer.publication_date))
        });
        context.validate_candidate(newer).unwrap();
        context.validate_candidate(older).unwrap();

        assert_eq!(
            context.validated_report().unwrap().candidate.period_label,
            "FY2026"
        );
    }

    #[test]
    fn resolves_latest_by_completed_period_and_reports_an_annual_fallback() {
        let context = ResearchContext::new(as_of());
        let annual = evidence_with_metadata(
            "https://investor.example.com/fy2025",
            as_of() - chrono::Duration::days(365),
            "FY2025",
            NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
            ReportType::Annual,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        );
        let quarter = evidence_with_metadata(
            "https://investor.example.com/q3-2026",
            as_of() - chrono::Duration::days(30),
            "FY2026 Q3",
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            ReportType::Quarterly,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        );
        let future_annual = evidence_with_metadata(
            "https://investor.example.com/fy2026",
            as_of() + chrono::Duration::days(2),
            "FY2026",
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            ReportType::Annual,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        );
        context.record_evidence(annual);
        context.record_evidence(quarter);
        context.record_evidence(future_annual);

        let prior_annual_resolution =
            context.resolve_latest_report("Microsoft", ReportCadence::Annual);
        assert_eq!(prior_annual_resolution.status, ResolutionStatus::Fallback);
        assert_eq!(
            prior_annual_resolution
                .selected
                .as_ref()
                .unwrap()
                .candidate
                .report_type,
            ReportType::Annual
        );
        assert!(
            prior_annual_resolution
                .limitation
                .as_deref()
                .is_some_and(|limitation| limitation.contains("prior annual"))
        );

        let annual_context = ResearchContext::new(as_of());
        annual_context.record_evidence(evidence_with_metadata(
            "https://investor.example.com/q3-2026",
            as_of() - chrono::Duration::days(30),
            "FY2026 Q3",
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            ReportType::Quarterly,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        ));
        annual_context.record_evidence(evidence_with_metadata(
            "https://investor.example.com/fy2026",
            as_of() + chrono::Duration::days(2),
            "FY2026",
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            ReportType::Annual,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        ));

        let annual_resolution =
            annual_context.resolve_latest_report("Microsoft", ReportCadence::Annual);
        assert_eq!(annual_resolution.status, ResolutionStatus::Fallback);
        assert_eq!(
            annual_resolution
                .selected
                .as_ref()
                .unwrap()
                .candidate
                .report_type,
            ReportType::Quarterly
        );
        assert_eq!(
            annual_resolution
                .selected
                .as_ref()
                .unwrap()
                .candidate
                .period_end,
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()
        );
        assert!(
            annual_resolution
                .limitation
                .as_deref()
                .is_some_and(|limitation| limitation.contains("annual"))
        );

        let latest_resolution = context.resolve_latest_report("Microsoft", ReportCadence::Latest);
        assert_eq!(latest_resolution.status, ResolutionStatus::Resolved);
        assert_eq!(
            latest_resolution
                .selected
                .as_ref()
                .unwrap()
                .candidate
                .period_label,
            "FY2026 Q3"
        );
    }

    #[test]
    fn does_not_stamp_a_requested_issuer_onto_unrelated_evidence() {
        let context = ResearchContext::new(as_of());
        let mut apple_evidence = evidence_with_metadata(
            "https://investor.example.com/apple-results",
            as_of() - chrono::Duration::days(30),
            "FY2026",
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            ReportType::Annual,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        );
        apple_evidence.report_metadata.as_mut().unwrap().issuer = Some("Apple".into());
        context.record_evidence(apple_evidence);

        let resolution = context.resolve_latest_report("Microsoft", ReportCadence::Latest);
        assert_eq!(resolution.status, ResolutionStatus::Unavailable);
        assert!(resolution.selected.is_none());
    }

    #[test]
    fn requires_a_fallback_disclosure_in_a_report_output() {
        let context = ResearchContext::new(as_of());
        let evidence = evidence_with_metadata(
            "https://investor.example.com/q3-2026",
            as_of() - chrono::Duration::days(30),
            "FY2026 Q3",
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            ReportType::Quarterly,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        );
        context.record_evidence(evidence);
        let resolution = context.resolve_latest_report("Microsoft", ReportCadence::Annual);
        let report = resolution.selected.as_ref().unwrap();
        let output = format!(
            "# Microsoft Financial Results\n\nAs of: 2026-07-29T12:00:00Z\n\nIssuer: Microsoft\nReporting calendar: fiscal\nReport type: quarterly\nPeriod end: 2026-03-31\nPublication status: reported\nPublication date: {}\nSource retrieved at: {}\n\n{} revenue was reported.\n\nSource: {}",
            report.candidate.publication_date.to_rfc3339(),
            report.source_retrieved_at.to_rfc3339(),
            report.candidate.period_label,
            report.candidate.publication_url
        );
        assert_eq!(
            context.validate_report_output(&output),
            Err(ReportValidationError::OutputMissingFallbackDisclosure)
        );
        let disclosed = format!(
            "{output}\n\nThe requested annual report was not available, so this quarterly fallback is shown instead."
        );
        assert!(context.validate_report_output(&disclosed).is_ok());
    }

    fn evidence_with_metadata(
        url: &str,
        published_at: DateTime<Utc>,
        period_label: &str,
        period_end: NaiveDate,
        report_type: ReportType,
        calendar: ReportingCalendar,
        status: ReportStatus,
    ) -> EvidenceRecord {
        EvidenceRecord {
            url: url.into(),
            title: Some(format!("Official {period_label} results")),
            snippet: None,
            retrieved_at: as_of(),
            response_status: Some(200),
            http_date: None,
            published_at: Some(published_at),
            authority: SourceAuthority::Official,
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some(period_label.into()),
                period_end: Some(period_end),
                report_type: Some(report_type),
                calendar: Some(calendar),
                status: Some(status),
            }),
        }
    }

    #[test]
    fn warns_when_a_financial_response_skips_validation() {
        let context = ResearchContext::new(as_of());
        let output = "# Earnings Report\nRevenue increased; net income increased.";
        assert!(context.final_response_warning(output).is_some());
    }

    #[test]
    fn annotates_unverified_json_without_prefixing_non_json_text() {
        let context = ResearchContext::new(as_of());
        let output = r#"{"financial_report":"Microsoft","as_of":"2026-07-29","revenue":1}"#;
        let annotated = context.annotate_final_response(output);
        let value: serde_json::Value = serde_json::from_str(&annotated).unwrap();
        assert_eq!(value["verification_status"], "unverified");
        assert!(value["verification_warning"].as_str().is_some());
    }

    #[test]
    fn classifies_common_official_source_shapes() {
        assert_eq!(
            classify_source_authority("https://www.sec.gov/Archives/x"),
            SourceAuthority::Official
        );
        assert_eq!(
            classify_source_authority("https://investor.example.com/results"),
            SourceAuthority::Official
        );
        assert_eq!(
            classify_source_authority("https://news.example.com/story"),
            SourceAuthority::Secondary
        );
        assert_eq!(
            classify_source_authority("https://sec.gov:443/Archives/x"),
            SourceAuthority::Official
        );
    }

    #[test]
    fn parses_rfc3339_rfc2822_and_date_only_publication_values() {
        assert_eq!(
            parse_publication_date("2026-07-29T12:00:00Z"),
            Some(as_of())
        );
        assert!(parse_publication_date("Wed, 29 Jul 2026 12:00:00 GMT").is_some());
        assert_eq!(
            parse_publication_date("2026-07-29"),
            Some(as_of().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc())
        );
    }

    #[test]
    fn extracts_period_metadata_without_fabricating_unknown_fields() {
        let metadata = infer_report_metadata(
            "Microsoft FY2025 annual results, period ended 2025-06-30; reported revenue.",
        )
        .unwrap();
        assert_eq!(metadata.period_label.as_deref(), Some("FY2025"));
        assert_eq!(
            metadata.period_end,
            Some(NaiveDate::from_ymd_opt(2025, 6, 30).unwrap())
        );
        assert_eq!(metadata.report_type, Some(ReportType::Annual));
        assert_eq!(metadata.calendar, Some(ReportingCalendar::Fiscal));
        assert_eq!(metadata.status, Some(ReportStatus::Reported));

        let unknown = infer_report_metadata("Issuer investor relations page").unwrap_or_default();
        assert_eq!(unknown.period_label, None);
        assert_eq!(unknown.period_end, None);
    }

    #[test]
    fn issuer_matching_uses_name_boundaries() {
        let metadata = infer_report_metadata_for_issuer(
            "Pineapple FY2025 annual results, period ended 2025-06-30; reported revenue.",
            Some("Apple"),
        )
        .expect("financial metadata");
        assert_eq!(metadata.issuer, None);

        let metadata = infer_report_metadata_for_issuer(
            "Apple Inc. FY2025 annual results, period ended 2025-06-30; reported revenue.",
            Some("Apple"),
        )
        .expect("financial metadata");
        assert_eq!(metadata.issuer.as_deref(), Some("Apple"));
    }

    #[test]
    fn generic_and_financial_observations_keep_their_metadata_alignment() {
        let context = ResearchContext::new(as_of());
        context.record_generic_evidence(GenericEvidenceRecord {
            url: "https://news.example.com/notes".into(),
            title: Some("Neutral notes".into()),
            snippet: None,
            content: None,
            retrieved_at: as_of(),
            response_status: Some(200),
            http_date: None,
            published_at: None,
            authority: SourceAuthority::Secondary,
        });
        context.record_evidence(evidence(
            "https://investor.example.com/results",
            Some(as_of()),
        ));

        let observations = context.observations();
        assert_eq!(observations.len(), 2);
        assert!(observations[0].report_metadata.is_none());
        assert!(observations[1].report_metadata.is_some());
    }

    #[test]
    fn merges_partial_search_metadata_with_richer_fetch_metadata() {
        let context = ResearchContext::new(as_of());
        let url = "https://investor.example.com/results";
        context.record_evidence(EvidenceRecord {
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2026".into()),
                ..Default::default()
            }),
            ..evidence(url, Some(as_of()))
        });
        context.record_evidence(EvidenceRecord {
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2026".into()),
                period_end: Some(NaiveDate::from_ymd_opt(2026, 6, 30).unwrap()),
                report_type: Some(ReportType::Annual),
                calendar: Some(ReportingCalendar::Fiscal),
                status: Some(ReportStatus::Reported),
            }),
            ..evidence(url, Some(as_of()))
        });

        let evidence = context.evidence();
        let metadata = evidence[0]
            .report_metadata
            .as_ref()
            .expect("merged metadata");
        assert_eq!(
            metadata.period_end,
            Some(NaiveDate::from_ymd_opt(2026, 6, 30).unwrap())
        );
        assert_eq!(metadata.report_type, Some(ReportType::Annual));
        assert_eq!(metadata.status, Some(ReportStatus::Reported));
    }

    #[test]
    fn surfaces_conflicting_publication_evidence_in_resolution() {
        let context = ResearchContext::new(as_of());
        context.record_evidence(evidence_with_metadata(
            "https://investor.example.com/results-primary",
            as_of() - chrono::Duration::days(10),
            "FY2026",
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            ReportType::Annual,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        ));
        let candidate = FinancialReportCandidate {
            issuer: "Microsoft".into(),
            report_type: ReportType::Annual,
            period_label: "FY2026".into(),
            period_end: NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            calendar: ReportingCalendar::Fiscal,
            status: ReportStatus::Reported,
            publication_url: "https://investor.example.com/results-primary".into(),
            publication_date: as_of() - chrono::Duration::days(10),
        };
        context.validate_candidate(candidate.clone()).unwrap();
        context.record_evidence(evidence_with_metadata(
            "https://investor.example.com/results-correction",
            as_of() - chrono::Duration::days(5),
            "FY2026",
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
            ReportType::Annual,
            ReportingCalendar::Fiscal,
            ReportStatus::Reported,
        ));

        let resolution = context.resolve_latest_report("Microsoft", ReportCadence::Latest);
        assert_eq!(resolution.status, ResolutionStatus::Conflict);
        assert!(resolution.selected.is_none());
        assert!(
            resolution
                .conflicts
                .iter()
                .any(|conflict| conflict.contains("Multiple eligible official sources"))
        );
        assert!(
            resolution
                .limitation
                .as_deref()
                .is_some_and(|limitation| limitation.contains("unverified"))
        );
        assert!(context.validated_report().is_none());
        assert!(matches!(
            context.validate_candidate(candidate),
            Err(ReportValidationError::ConflictingEvidence(_))
        ));
        assert!(context.validated_report().is_none());
    }

    #[test]
    fn rejects_a_candidate_that_conflicts_with_observed_period_metadata() {
        let context = ResearchContext::new(as_of());
        let report = candidate(as_of());
        context.record_evidence(EvidenceRecord {
            report_metadata: Some(ReportEvidenceMetadata {
                issuer: Some("Microsoft".into()),
                period_label: Some("FY2026".into()),
                ..Default::default()
            }),
            ..evidence(&report.publication_url, Some(report.publication_date))
        });

        assert_eq!(
            context.validate_candidate(report),
            Err(ReportValidationError::SourceMetadataMismatch(
                "reporting period"
            ))
        );
    }
}
