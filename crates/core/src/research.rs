use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuthority {
    Official,
    Secondary,
    Unknown,
}

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

    fn merge_missing_from(&mut self, incoming: Self) {
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

impl SourceAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Secondary => "secondary",
            Self::Unknown => "unknown",
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

    fn accepts(self, report_type: ReportType) -> bool {
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

    fn is_eligible(self) -> bool {
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
    pub as_of: DateTime<Utc>,
    pub source_authority: SourceAuthority,
    pub source_retrieved_at: DateTime<Utc>,
    pub source_http_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Resolved,
    Fallback,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinancialReportResolution {
    pub issuer: String,
    pub requested_cadence: ReportCadence,
    pub as_of: DateTime<Utc>,
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
    #[error("source publication date {publication_date} is after the as-of time {as_of}")]
    PublicationInFuture {
        publication_date: DateTime<Utc>,
        as_of: DateTime<Utc>,
    },
    #[error("source URL was not observed during this research turn")]
    SourceNotObserved,
    #[error("source is not an authoritative financial source")]
    SourceNotOfficial,
    #[error("source publication date is unavailable")]
    PublicationDateUnknown,
    #[error("source publication date does not match the observed source")]
    PublicationDateMismatch,
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
struct ResearchState {
    as_of: DateTime<Utc>,
    evidence: Vec<EvidenceRecord>,
    validated_report: Option<ValidatedFinancialReport>,
    resolution: Option<FinancialReportResolution>,
    conflicts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResearchContext {
    state: Arc<Mutex<ResearchState>>,
}

impl ResearchContext {
    pub fn new(as_of: DateTime<Utc>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ResearchState {
                as_of,
                evidence: Vec::new(),
                validated_report: None,
                resolution: None,
                conflicts: Vec::new(),
            })),
        }
    }

    pub fn begin_turn(&self, as_of: DateTime<Utc>) {
        if let Ok(mut state) = self.state.lock() {
            state.as_of = as_of;
            state.evidence.clear();
            state.validated_report = None;
            state.resolution = None;
            state.conflicts.clear();
        }
    }

    pub fn as_of(&self) -> DateTime<Utc> {
        self.state
            .lock()
            .map(|state| state.as_of)
            .unwrap_or_else(|_| Utc::now())
    }

    pub fn record_evidence(&self, mut evidence: EvidenceRecord) {
        evidence.url = canonical_url(&evidence.url);
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(index) = state
            .evidence
            .iter()
            .position(|existing| existing.url == evidence.url)
        {
            let previous = state.evidence[index].clone();
            record_evidence_conflicts(&mut state.conflicts, &previous, &evidence);
            let existing = &mut state.evidence[index];
            if existing.published_at.is_none() {
                existing.published_at = evidence.published_at;
            }
            if existing.response_status.is_none() {
                existing.response_status = evidence.response_status;
            }
            if existing.http_date.is_none() {
                existing.http_date = evidence.http_date;
            }
            if let Some(incoming) = evidence.report_metadata {
                if let Some(metadata) = existing.report_metadata.as_mut() {
                    metadata.merge_missing_from(incoming);
                } else {
                    existing.report_metadata = Some(incoming);
                }
            }
            if existing.title.is_none() {
                existing.title = evidence.title;
            }
            if existing.snippet.is_none() {
                existing.snippet = evidence.snippet;
            }
            if existing.authority == SourceAuthority::Unknown {
                existing.authority = evidence.authority;
            }
            return;
        }
        state.evidence.push(evidence);
    }

    pub fn evidence(&self) -> Vec<EvidenceRecord> {
        self.state
            .lock()
            .map(|state| state.evidence.clone())
            .unwrap_or_default()
    }

    pub fn validate_candidate(
        &self,
        mut candidate: FinancialReportCandidate,
    ) -> Result<ValidatedFinancialReport, ReportValidationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReportValidationError::SourceNotObserved)?;
        let as_of = state.as_of;

        if !candidate.status.is_eligible() {
            return Err(ReportValidationError::IneligibleStatus(
                candidate.status.as_str().into(),
            ));
        }
        if candidate.period_end > as_of.date_naive() {
            return Err(ReportValidationError::PeriodInFuture {
                period_end: candidate.period_end,
                as_of: as_of.date_naive(),
            });
        }
        if candidate.publication_date > as_of {
            return Err(ReportValidationError::PublicationInFuture {
                publication_date: candidate.publication_date,
                as_of,
            });
        }

        let url = canonical_url(&candidate.publication_url);
        let evidence = state
            .evidence
            .iter()
            .find(|evidence| evidence.url == url)
            .ok_or(ReportValidationError::SourceNotObserved)?;
        if evidence.authority != SourceAuthority::Official {
            return Err(ReportValidationError::SourceNotOfficial);
        }
        let observed_date = evidence
            .published_at
            .ok_or(ReportValidationError::PublicationDateUnknown)?;
        if observed_date != candidate.publication_date {
            return Err(ReportValidationError::PublicationDateMismatch);
        }
        let metadata = evidence.report_metadata.as_ref().ok_or(
            ReportValidationError::SourceMetadataUnavailable("reporting period"),
        )?;
        let observed_issuer = metadata
            .issuer
            .as_deref()
            .ok_or(ReportValidationError::SourceMetadataUnavailable("issuer"))?;
        if !same_issuer(observed_issuer, &candidate.issuer) {
            return Err(ReportValidationError::SourceMetadataMismatch("issuer"));
        }
        let period_label = metadata.period_label.as_deref().ok_or(
            ReportValidationError::SourceMetadataUnavailable("reporting period"),
        )?;
        if !period_label.eq_ignore_ascii_case(&candidate.period_label) {
            return Err(ReportValidationError::SourceMetadataMismatch(
                "reporting period",
            ));
        }
        if metadata.period_end != Some(candidate.period_end) {
            return Err(ReportValidationError::SourceMetadataMismatch("period end"));
        }
        if metadata.report_type != Some(candidate.report_type) {
            return Err(ReportValidationError::SourceMetadataMismatch("report type"));
        }
        if metadata.calendar != Some(candidate.calendar) {
            return Err(ReportValidationError::SourceMetadataMismatch(
                "reporting calendar",
            ));
        }
        if metadata.status != Some(candidate.status) {
            return Err(ReportValidationError::SourceMetadataMismatch("status"));
        }

        candidate.publication_url = url;
        let validated = ValidatedFinancialReport {
            candidate,
            as_of,
            source_authority: evidence.authority,
            source_retrieved_at: evidence.retrieved_at,
            source_http_date: evidence.http_date,
        };

        let should_promote = state
            .validated_report
            .as_ref()
            .map(|current| is_newer_report(&validated, current))
            .unwrap_or(true);
        if should_promote {
            state.validated_report = Some(validated.clone());
        }
        state.resolution = None;
        Ok(validated)
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
        let issuer = issuer.trim().to_string();
        let as_of = self.as_of();
        let requested_candidates = self.eligible_candidates(&issuer, requested_cadence);
        let mut conflicts = self.conflicts();
        append_period_conflicts(&mut conflicts, &requested_candidates);
        let requested_selected = self.validate_first(requested_candidates);
        let newer_annual_blocked = requested_cadence == ReportCadence::Annual
            && requested_selected.as_ref().is_some_and(|selected| {
                self.has_ineligible_newer_annual(&issuer, selected.candidate.period_end)
            });
        let selected = requested_selected;
        let mut status = if selected.is_some() {
            ResolutionStatus::Resolved
        } else {
            ResolutionStatus::Unavailable
        };
        let mut limitation = None;

        if newer_annual_blocked {
            status = ResolutionStatus::Fallback;
            limitation = Some(format!(
                "The newest observed annual period was not eligible as of {}; the newest eligible prior annual result is shown as a fallback.",
                as_of.to_rfc3339()
            ));
        }

        let selected = if selected.is_none() && requested_cadence == ReportCadence::Annual {
            let fallback =
                self.validate_first(self.eligible_candidates(&issuer, ReportCadence::Quarterly));
            if fallback.is_some() {
                status = ResolutionStatus::Fallback;
                limitation = Some(format!(
                    "No eligible annual report was observed as of {}; the newest eligible quarterly result is shown as a fallback and must not be treated as annual.",
                    as_of.to_rfc3339()
                ));
            }
            fallback
        } else {
            selected
        };

        if selected.is_none() && limitation.is_none() {
            limitation = Some(match requested_cadence {
                ReportCadence::Annual => format!(
                    "No eligible annual or quarterly result was observed as of {}.",
                    as_of.to_rfc3339()
                ),
                ReportCadence::Quarterly => format!(
                    "No eligible quarterly result was observed as of {}.",
                    as_of.to_rfc3339()
                ),
                ReportCadence::Latest => format!(
                    "No eligible official reported result was observed as of {}.",
                    as_of.to_rfc3339()
                ),
            });
        }

        let resolution = FinancialReportResolution {
            issuer,
            requested_cadence,
            as_of,
            status,
            selected,
            limitation,
            conflicts,
        };
        if let Ok(mut state) = self.state.lock() {
            state.validated_report = resolution.selected.clone();
            state.resolution = Some(resolution.clone());
        }
        resolution
    }

    fn conflicts(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.conflicts.clone())
            .unwrap_or_default()
    }

    fn eligible_candidates(
        &self,
        issuer: &str,
        requested_cadence: ReportCadence,
    ) -> Vec<FinancialReportCandidate> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        let as_of = state.as_of;
        let mut candidates = state
            .evidence
            .iter()
            .filter_map(|evidence| {
                if evidence.authority != SourceAuthority::Official {
                    return None;
                }
                let metadata = evidence.report_metadata.as_ref()?;
                let publication_date = evidence.published_at?;
                let observed_issuer = metadata.issuer.as_deref()?;
                if !same_issuer(observed_issuer, issuer) {
                    return None;
                }
                let period_label = metadata.period_label.clone()?;
                let period_end = metadata.period_end?;
                let report_type = metadata.report_type?;
                let calendar = metadata.calendar?;
                let status = metadata.status?;
                if !status.is_eligible()
                    || period_end > as_of.date_naive()
                    || publication_date > as_of
                    || !requested_cadence.accepts(report_type)
                {
                    return None;
                }
                Some(FinancialReportCandidate {
                    issuer: issuer.to_string(),
                    report_type,
                    period_label,
                    period_end,
                    calendar,
                    status,
                    publication_url: canonical_url(&evidence.url),
                    publication_date,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(compare_candidate_order_desc);
        candidates
    }

    fn has_ineligible_newer_annual(&self, issuer: &str, selected_period_end: NaiveDate) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        state.evidence.iter().any(|evidence| {
            if evidence.authority != SourceAuthority::Official {
                return false;
            }
            let Some(metadata) = evidence.report_metadata.as_ref() else {
                return false;
            };
            let Some(observed_issuer) = metadata.issuer.as_deref() else {
                return false;
            };
            let Some(period_end) = metadata.period_end else {
                return false;
            };
            if !same_issuer(observed_issuer, issuer)
                || metadata.report_type != Some(ReportType::Annual)
                || period_end <= selected_period_end
            {
                return false;
            }
            let status_eligible = metadata.status.is_some_and(ReportStatus::is_eligible);
            let period_completed = period_end <= state.as_of.date_naive();
            let publication_available = evidence
                .published_at
                .is_some_and(|published_at| published_at <= state.as_of);
            !(status_eligible && period_completed && publication_available)
        })
    }

    fn validate_first(
        &self,
        candidates: Vec<FinancialReportCandidate>,
    ) -> Option<ValidatedFinancialReport> {
        candidates
            .into_iter()
            .find_map(|candidate| self.validate_candidate(candidate).ok())
    }

    pub fn validated_report(&self) -> Option<ValidatedFinancialReport> {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.validated_report.clone())
    }

    pub fn validate_report_output(&self, output: &str) -> Result<(), ReportValidationError> {
        if !looks_like_financial_report(output) {
            return Ok(());
        }
        let Some(validated) = self.validated_report() else {
            return Err(ReportValidationError::OutputNotValidated);
        };
        if !contains_as_of(output, validated.as_of) {
            return Err(ReportValidationError::OutputMissingAsOf);
        }
        if !output.contains(&validated.candidate.period_label) {
            return Err(ReportValidationError::OutputPeriodMismatch(
                validated.candidate.period_label,
            ));
        }
        let candidate = &validated.candidate;
        let period_end = candidate.period_end.to_string();
        let output_lower = output.to_ascii_lowercase();
        let required_fields = [
            (candidate.issuer.as_str(), "issuer"),
            (candidate.report_type.as_str(), "report type"),
            (candidate.calendar.as_str(), "reporting calendar"),
            (period_end.as_str(), "period end"),
            (candidate.status.as_str(), "publication status"),
        ];
        for (value, field) in required_fields {
            if !output_lower.contains(&value.to_ascii_lowercase()) {
                return Err(ReportValidationError::OutputMetadataMissing(field));
            }
        }
        if !contains_datetime(output, candidate.publication_date) {
            return Err(ReportValidationError::OutputMetadataMissing(
                "publication date",
            ));
        }
        if !contains_datetime(output, validated.source_retrieved_at) {
            return Err(ReportValidationError::OutputMetadataMissing(
                "source retrieval timestamp",
            ));
        }
        if !output.contains(&candidate.publication_url) {
            return Err(ReportValidationError::OutputMissingSource);
        }
        if self
            .state
            .lock()
            .ok()
            .and_then(|state| state.resolution.clone())
            .is_some_and(|resolution| {
                resolution.status == ResolutionStatus::Fallback
                    && !contains_fallback_disclosure(output)
            })
        {
            return Err(ReportValidationError::OutputMissingFallbackDisclosure);
        }
        Ok(())
    }

    pub fn final_response_warning(&self, output: &str) -> Option<String> {
        if !looks_like_financial_report(output) || self.validate_report_output(output).is_ok() {
            return None;
        }
        let mut warning = "Verification status: unverified. This financial report did not pass the as-of, source, and reporting-period checks; treat its figures as unverified until an authoritative source is confirmed.".to_string();
        if let Ok(state) = self.state.lock()
            && let Some(limitation) = state
                .resolution
                .as_ref()
                .and_then(|resolution| resolution.limitation.as_deref())
        {
            warning.push_str(" Research limitation: ");
            warning.push_str(limitation);
        }
        Some(warning)
    }

    pub fn annotate_final_response(&self, output: &str) -> String {
        let Some(warning) = self.final_response_warning(output) else {
            return output.to_string();
        };

        if let Ok(mut value) = serde_json::from_str::<Value>(output)
            && let Value::Object(object) = &mut value
        {
            object.insert(
                "verification_status".into(),
                Value::String("unverified".into()),
            );
            object.insert("verification_warning".into(), Value::String(warning));
            return serde_json::to_string_pretty(&value).unwrap_or_else(|_| output.to_string());
        }

        let lines: Vec<&str> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.len() > 1
            && lines
                .iter()
                .all(|line| serde_json::from_str::<Value>(line).is_ok())
        {
            let warning_record = serde_json::json!({
                "verification_status": "unverified",
                "verification_warning": warning,
            });
            return format!("{}\n{}", output.trim_end(), warning_record);
        }

        format!("{warning}\n\n{output}")
    }
}

pub fn canonical_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn record_evidence_conflicts(
    conflicts: &mut Vec<String>,
    existing: &EvidenceRecord,
    incoming: &EvidenceRecord,
) {
    if let (Some(existing_date), Some(incoming_date)) =
        (existing.published_at, incoming.published_at)
        && existing_date != incoming_date
    {
        conflicts.push(format!(
            "Conflicting publication dates were observed for {}: {} and {}.",
            existing.url,
            existing_date.to_rfc3339(),
            incoming_date.to_rfc3339()
        ));
    }

    let Some(existing_metadata) = existing.report_metadata.as_ref() else {
        return;
    };
    let Some(incoming_metadata) = incoming.report_metadata.as_ref() else {
        return;
    };
    if let (Some(existing_period), Some(incoming_period)) = (
        &existing_metadata.period_label,
        &incoming_metadata.period_label,
    ) && existing_period != incoming_period
    {
        conflicts.push(format!(
            "Conflicting reporting periods were observed for {}: {} and {}.",
            existing.url, existing_period, incoming_period
        ));
    }
    if let (Some(existing_issuer), Some(incoming_issuer)) =
        (&existing_metadata.issuer, &incoming_metadata.issuer)
        && !same_issuer(existing_issuer, incoming_issuer)
    {
        conflicts.push(format!(
            "Conflicting issuers were observed for {}: {} and {}.",
            existing.url, existing_issuer, incoming_issuer
        ));
    }
    if let (Some(existing_end), Some(incoming_end)) =
        (existing_metadata.period_end, incoming_metadata.period_end)
        && existing_end != incoming_end
    {
        conflicts.push(format!(
            "Conflicting period ends were observed for {}: {} and {}.",
            existing.url, existing_end, incoming_end
        ));
    }
}

fn append_period_conflicts(conflicts: &mut Vec<String>, candidates: &[FinancialReportCandidate]) {
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(other) = candidates[index + 1..]
            .iter()
            .find(|other| other.period_end == candidate.period_end)
        else {
            continue;
        };
        let message = format!(
            "Multiple eligible official sources cover period end {}; selected the newest publication among {} and {}.",
            candidate.period_end, candidate.publication_url, other.publication_url
        );
        if !conflicts.contains(&message) {
            conflicts.push(message);
        }
    }
}

fn is_newer_report(
    candidate: &ValidatedFinancialReport,
    current: &ValidatedFinancialReport,
) -> bool {
    (
        candidate.candidate.period_end,
        candidate.candidate.publication_date,
        source_authority_rank(candidate.source_authority),
    ) > (
        current.candidate.period_end,
        current.candidate.publication_date,
        source_authority_rank(current.source_authority),
    )
}

fn compare_candidate_order_desc(
    left: &FinancialReportCandidate,
    right: &FinancialReportCandidate,
) -> Ordering {
    right
        .period_end
        .cmp(&left.period_end)
        .then_with(|| right.publication_date.cmp(&left.publication_date))
        .then_with(|| right.report_type.as_str().cmp(left.report_type.as_str()))
        .then_with(|| right.publication_url.cmp(&left.publication_url))
}

fn source_authority_rank(authority: SourceAuthority) -> u8 {
    match authority {
        SourceAuthority::Official => 2,
        SourceAuthority::Secondary => 1,
        SourceAuthority::Unknown => 0,
    }
}

pub fn classify_source_authority(url: &str) -> SourceAuthority {
    let lower = url.to_ascii_lowercase();
    let Some((_, host_and_path)) = lower.split_once("://") else {
        return SourceAuthority::Unknown;
    };
    let host = host_and_path.split('/').next().unwrap_or_default();
    let path = host_and_path
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or_default();
    if host == "sec.gov"
        || host.ends_with(".gov")
        || host.starts_with("investor.")
        || host.starts_with("ir.")
        || path.contains("investor-relations")
        || path.contains("/investors/")
    {
        SourceAuthority::Official
    } else if !host.is_empty() {
        SourceAuthority::Secondary
    } else {
        SourceAuthority::Unknown
    }
}

pub fn parse_publication_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Some(value.with_timezone(&Utc));
    }
    if let Ok(value) = DateTime::parse_from_rfc2822(value) {
        return Some(value.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| DateTime::<Utc>::from_naive_utc_and_offset(date, Utc))
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
        if lower_text.contains(&lower_issuer) {
            metadata.issuer = Some(issuer.to_string());
        }
    }
    Some(metadata)
}

fn same_issuer(left: &str, right: &str) -> bool {
    left.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .eq_ignore_ascii_case(&right.split_whitespace().collect::<Vec<_>>().join(" "))
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

fn contains_as_of(output: &str, as_of: DateTime<Utc>) -> bool {
    let date = as_of.date_naive().to_string();
    let timestamp = as_of.to_rfc3339();
    let zulu_timestamp = as_of.to_rfc3339_opts(SecondsFormat::Secs, true);
    output.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        (lower.contains("as of")
            || lower.contains("as-of")
            || lower.contains("as_of")
            || lower.contains("as-of:"))
            && line.contains(&date)
            && (line.contains(&timestamp) || line.contains(&zulu_timestamp))
    })
}

fn contains_datetime(output: &str, value: DateTime<Utc>) -> bool {
    output.contains(&value.to_rfc3339())
        || output.contains(&value.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn contains_fallback_disclosure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    let identifies_annual = lower.contains("annual");
    let explains_limitation = [
        "not available",
        "not published",
        "not yet",
        "unavailable",
        "no eligible",
        "fallback",
        "instead",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    identifies_annual && explains_limitation
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
        let publication_date = as_of() + chrono::Duration::hours(1);
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
        assert_eq!(resolution.status, ResolutionStatus::Resolved);
        assert!(
            resolution
                .conflicts
                .iter()
                .any(|conflict| conflict.contains("Multiple eligible official sources"))
        );
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
