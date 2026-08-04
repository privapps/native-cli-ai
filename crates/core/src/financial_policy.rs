//! Financial interpretation of the generic research evidence ledger.
//!
//! Collectors write provenance-only records to [`EvidenceLedger`].  This
//! module is the financial policy boundary: it attaches financial metadata,
//! applies issuer/period/cadence/source rules, and owns the verified report
//! capability consumed by the financial tools.

use crate::evidence::{EvidenceLedger, GenericEvidenceRecord, SourceAuthority, canonical_url};
use crate::research::{
    EvidenceRecord, FinancialReportCandidate, FinancialReportResolution, ReportCadence,
    ReportEvidenceMetadata, ReportStatus, ReportType, ReportValidationError, ResolutionStatus,
    ValidatedFinancialReport, infer_report_metadata_for_issuer, looks_like_financial_report,
};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct FinancialPolicyState {
    financial_metadata_by_url: HashMap<String, ReportEvidenceMetadata>,
    observation_metadata: HashMap<usize, Option<ReportEvidenceMetadata>>,
    validated_report: Option<ValidatedFinancialReport>,
    resolution: Option<FinancialReportResolution>,
    conflicts: Vec<String>,
}

/// Applies financial interpretation to the generic evidence collected during
/// one immutable-as-of research turn.
#[derive(Debug, Clone)]
pub struct FinancialResearchPolicy {
    ledger: Arc<EvidenceLedger>,
    state: Arc<Mutex<FinancialPolicyState>>,
}

impl FinancialResearchPolicy {
    pub fn with_ledger(ledger: Arc<EvidenceLedger>) -> Self {
        Self {
            ledger,
            state: Arc::new(Mutex::new(FinancialPolicyState {
                financial_metadata_by_url: HashMap::new(),
                observation_metadata: HashMap::new(),
                validated_report: None,
                resolution: None,
                conflicts: Vec::new(),
            })),
        }
    }

    pub fn begin_turn(&self, as_of: NaiveDate) {
        self.ledger.begin_turn(as_of);
        if let Ok(mut state) = self.state.lock() {
            state.financial_metadata_by_url.clear();
            state.observation_metadata.clear();
            state.validated_report = None;
            state.resolution = None;
            state.conflicts.clear();
        }
    }

    pub fn as_of(&self) -> NaiveDate {
        self.ledger.as_of()
    }

    /// Record provenance-only evidence collected by a generic tool.
    ///
    /// The ledger remains domain-neutral; this policy hook exists so a
    /// collector cannot leave a previously issued financial capability valid
    /// after adding evidence that conflicts with it.
    pub fn record_generic_evidence(&self, evidence: GenericEvidenceRecord) {
        if let Some(index) = self.ledger.record_evidence_with_index(evidence)
            && let Ok(mut state) = self.state.lock()
        {
            state.observation_metadata.insert(index, None);
        }
        self.invalidate_validated_report_if_conflicted();
    }

    /// Record a legacy financial observation while preserving its generic
    /// provenance in the shared ledger. New collectors can write directly to
    /// the ledger and hand their records to the policy for interpretation.
    pub fn record_evidence(&self, mut evidence: EvidenceRecord) {
        evidence.url = canonical_url(&evidence.url);
        let previous = self
            .evidence()
            .into_iter()
            .find(|existing| existing.url == evidence.url);
        if let Some(previous) = previous.as_ref() {
            self.record_metadata_conflicts(previous, &evidence);
        }

        let observation_index = self
            .ledger
            .record_evidence_with_index(GenericEvidenceRecord {
                url: evidence.url.clone(),
                title: evidence.title.clone(),
                snippet: evidence.snippet.clone(),
                content: None,
                retrieved_at: evidence.retrieved_at,
                response_status: evidence.response_status,
                http_date: evidence.http_date,
                published_at: evidence.published_at,
                authority: evidence.authority,
            });

        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(index) = observation_index {
            state
                .observation_metadata
                .insert(index, evidence.report_metadata.clone());
        }
        if let Some(incoming) = evidence.report_metadata {
            if let Some(existing) = state.financial_metadata_by_url.get_mut(&evidence.url) {
                existing.merge_missing_from(incoming);
            } else {
                state
                    .financial_metadata_by_url
                    .insert(evidence.url, incoming);
            }
        }
        drop(state);
        self.invalidate_validated_report_if_conflicted();
    }

    pub fn evidence(&self) -> Vec<EvidenceRecord> {
        let metadata = self
            .state
            .lock()
            .map(|state| state.financial_metadata_by_url.clone())
            .unwrap_or_default();
        self.ledger
            .evidence()
            .into_iter()
            .map(|evidence| EvidenceRecord {
                report_metadata: metadata.get(&evidence.url).cloned(),
                url: evidence.url,
                title: evidence.title,
                snippet: evidence.snippet,
                retrieved_at: evidence.retrieved_at,
                response_status: evidence.response_status,
                http_date: evidence.http_date,
                published_at: evidence.published_at,
                authority: evidence.authority,
            })
            .collect()
    }

    pub fn observations(&self) -> Vec<EvidenceRecord> {
        let metadata = self
            .state
            .lock()
            .map(|state| state.observation_metadata.clone())
            .unwrap_or_default();
        self.ledger
            .observations()
            .into_iter()
            .enumerate()
            .map(|(index, evidence)| EvidenceRecord {
                report_metadata: metadata.get(&index).cloned().flatten(),
                url: evidence.url,
                title: evidence.title,
                snippet: evidence.snippet,
                retrieved_at: evidence.retrieved_at,
                response_status: evidence.response_status,
                http_date: evidence.http_date,
                published_at: evidence.published_at,
                authority: evidence.authority,
            })
            .collect()
    }

    pub fn validate_candidate(
        &self,
        mut candidate: FinancialReportCandidate,
    ) -> Result<ValidatedFinancialReport, ReportValidationError> {
        if let Some(reason) = self.conflict_reason_for_candidate(&candidate) {
            return Err(ReportValidationError::ConflictingEvidence(reason));
        }
        let as_of = self.as_of();
        if !candidate.status.is_eligible() {
            return Err(ReportValidationError::IneligibleStatus(
                candidate.status.as_str().into(),
            ));
        }
        if candidate.period_end > as_of {
            return Err(ReportValidationError::PeriodInFuture {
                period_end: candidate.period_end,
                as_of,
            });
        }
        if candidate.publication_date.date_naive() > as_of {
            return Err(ReportValidationError::PublicationInFuture {
                publication_date: candidate.publication_date.date_naive(),
                as_of,
            });
        }

        let url = canonical_url(&candidate.publication_url);
        let evidence = self
            .evidence_for_issuer(&candidate.issuer)
            .into_iter()
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

        let mut state = self
            .state
            .lock()
            .map_err(|_| ReportValidationError::SourceNotObserved)?;
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

    /// Resolve the newest eligible report without crossing cadences except
    /// for the explicit annual-to-quarterly fallback.
    pub fn resolve_latest_report(
        &self,
        issuer: &str,
        requested_cadence: ReportCadence,
    ) -> FinancialReportResolution {
        let issuer = issuer.trim().to_string();
        let as_of = self.as_of();
        let requested_candidates = self.eligible_candidates(&issuer, requested_cadence);
        let mut conflicts = self.conflicts_for_issuer(&issuer);
        append_period_conflicts(&mut conflicts, &requested_candidates);
        let mut selected = None;
        let mut status = ResolutionStatus::Unavailable;
        let mut limitation = None;

        if conflicts.is_empty() {
            selected = self.validate_first(requested_candidates);
            status = if selected.is_some() {
                ResolutionStatus::Resolved
            } else {
                ResolutionStatus::Unavailable
            };

            let newer_annual_blocked = requested_cadence == ReportCadence::Annual
                && selected.as_ref().is_some_and(|selected| {
                    self.has_ineligible_newer_annual(&issuer, selected.candidate.period_end)
                });
            if newer_annual_blocked {
                status = ResolutionStatus::Fallback;
                limitation = Some(format!(
                    "The newest observed annual period was not eligible as of {}; the newest eligible prior annual result is shown as a fallback.",
                    as_of
                ));
            }

            if selected.is_none() && requested_cadence == ReportCadence::Annual {
                let fallback_candidates =
                    self.eligible_candidates(&issuer, ReportCadence::Quarterly);
                append_period_conflicts(&mut conflicts, &fallback_candidates);
                if conflicts.is_empty() {
                    selected = self.validate_first(fallback_candidates);
                    if selected.is_some() {
                        status = ResolutionStatus::Fallback;
                        limitation = Some(format!(
                            "No eligible annual report was observed as of {}; the newest eligible quarterly result is shown as a fallback and must not be treated as annual.",
                            as_of
                        ));
                    }
                }
            }
        }

        if !conflicts.is_empty() {
            selected = None;
            status = ResolutionStatus::Conflict;
            limitation = Some(
                "Conflicting official evidence was observed; the financial report remains unverified until the sources are reconciled."
                    .into(),
            );
        } else if selected.is_none() && limitation.is_none() {
            limitation = Some(match requested_cadence {
                ReportCadence::Annual => format!(
                    "No eligible annual or quarterly result was observed as of {}.",
                    as_of
                ),
                ReportCadence::Quarterly => {
                    format!("No eligible quarterly result was observed as of {}.", as_of)
                }
                ReportCadence::Latest => format!(
                    "No eligible official reported result was observed as of {}.",
                    as_of
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
            state.validated_report = if resolution.status == ResolutionStatus::Conflict {
                None
            } else {
                resolution.selected.clone()
            };
            state.resolution = Some(resolution.clone());
        }
        resolution
    }

    pub fn validated_report(&self) -> Option<ValidatedFinancialReport> {
        // Keep the capability safe even for callers that hold the generic
        // ledger directly. Generic collection normally uses
        // `record_generic_evidence`, but the capability must not survive a
        // conflicting ledger mutation through any supported seam.
        self.invalidate_validated_report_if_conflicted();
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

    fn record_metadata_conflicts(&self, existing: &EvidenceRecord, incoming: &EvidenceRecord) {
        let mut conflicts = Vec::new();
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
        if let (Some(existing_type), Some(incoming_type)) =
            (existing_metadata.report_type, incoming_metadata.report_type)
            && existing_type != incoming_type
        {
            conflicts.push(format!(
                "Conflicting report types were observed for {}: {} and {}.",
                existing.url,
                existing_type.as_str(),
                incoming_type.as_str()
            ));
        }
        if let (Some(existing_calendar), Some(incoming_calendar)) =
            (existing_metadata.calendar, incoming_metadata.calendar)
            && existing_calendar != incoming_calendar
        {
            conflicts.push(format!(
                "Conflicting reporting calendars were observed for {}: {} and {}.",
                existing.url,
                existing_calendar.as_str(),
                incoming_calendar.as_str()
            ));
        }
        if let (Some(existing_status), Some(incoming_status)) =
            (existing_metadata.status, incoming_metadata.status)
            && existing_status != incoming_status
        {
            conflicts.push(format!(
                "Conflicting report statuses were observed for {}: {} and {}.",
                existing.url,
                existing_status.as_str(),
                incoming_status.as_str()
            ));
        }
        if let Ok(mut state) = self.state.lock() {
            state.conflicts.extend(conflicts);
        }
    }

    fn conflicts(&self) -> Vec<String> {
        let mut conflicts = self.ledger.conflicts();
        conflicts.extend(
            self.state
                .lock()
                .map(|state| state.conflicts.clone())
                .unwrap_or_default(),
        );
        conflicts
    }

    fn conflict_reason_for_candidate(
        &self,
        candidate: &FinancialReportCandidate,
    ) -> Option<String> {
        let candidate_url = canonical_url(&candidate.publication_url);
        let mut conflicts = self
            .conflicts()
            .into_iter()
            .filter(|conflict| conflict.contains(&candidate_url))
            .collect::<Vec<_>>();
        let eligible_candidates =
            self.eligible_candidates(&candidate.issuer, ReportCadence::Latest);
        let mut period_conflicts = Vec::new();
        append_period_conflicts(&mut period_conflicts, &eligible_candidates);
        conflicts.extend(period_conflicts.into_iter().filter(|conflict| {
            conflict.contains(&candidate_url)
                || conflict.contains(&candidate.period_end.to_string())
        }));
        if let Ok(state) = self.state.lock()
            && let Some(resolution) = state.resolution.as_ref()
            && same_issuer(&resolution.issuer, &candidate.issuer)
            && resolution.status == ResolutionStatus::Conflict
        {
            conflicts.extend(resolution.conflicts.iter().cloned());
        }
        conflicts.sort();
        conflicts.dedup();
        (!conflicts.is_empty()).then(|| conflicts.join(" "))
    }

    fn invalidate_validated_report_if_conflicted(&self) {
        let candidate = self
            .state
            .lock()
            .ok()
            .and_then(|state| state.validated_report.clone())
            .map(|validated| validated.candidate);
        let Some(candidate) = candidate else {
            return;
        };
        if self.conflict_reason_for_candidate(&candidate).is_none() {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state.validated_report = None;
            state.resolution = None;
        }
    }

    fn eligible_candidates(
        &self,
        issuer: &str,
        requested_cadence: ReportCadence,
    ) -> Vec<FinancialReportCandidate> {
        let as_of = self.as_of();
        let mut candidates = self
            .evidence_for_issuer(issuer)
            .into_iter()
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
                    || period_end > as_of
                    || publication_date.date_naive() > as_of
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
        let as_of = self.as_of();
        self.evidence_for_issuer(issuer).iter().any(|evidence| {
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
            let metadata_complete = metadata.period_label.is_some() && metadata.calendar.is_some();
            let period_completed = period_end <= as_of;
            let publication_available = evidence
                .published_at
                .is_some_and(|published_at| published_at.date_naive() <= as_of);
            !(status_eligible && metadata_complete && period_completed && publication_available)
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

    fn evidence_for_issuer(&self, issuer: &str) -> Vec<EvidenceRecord> {
        let metadata = self
            .state
            .lock()
            .map(|state| state.financial_metadata_by_url.clone())
            .unwrap_or_default();
        self.ledger
            .evidence()
            .into_iter()
            .map(|evidence| {
                let report_metadata = metadata.get(&evidence.url).cloned().or_else(|| {
                    let text = [
                        evidence.title.as_deref(),
                        evidence.snippet.as_deref(),
                        evidence.content.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                    infer_report_metadata_for_issuer(&text, Some(issuer))
                });
                EvidenceRecord {
                    report_metadata,
                    url: evidence.url,
                    title: evidence.title,
                    snippet: evidence.snippet,
                    retrieved_at: evidence.retrieved_at,
                    response_status: evidence.response_status,
                    http_date: evidence.http_date,
                    published_at: evidence.published_at,
                    authority: evidence.authority,
                }
            })
            .collect()
    }

    fn conflicts_for_issuer(&self, issuer: &str) -> Vec<String> {
        let relevant_urls = self
            .evidence_for_issuer(issuer)
            .into_iter()
            .filter(|evidence| {
                evidence
                    .report_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.issuer.as_deref())
                    .is_some_and(|observed| same_issuer(observed, issuer))
            })
            .map(|evidence| evidence.url)
            .collect::<Vec<_>>();
        self.conflicts()
            .into_iter()
            .filter(|conflict| relevant_urls.iter().any(|url| conflict.contains(url)))
            .collect()
    }
}

fn same_issuer(left: &str, right: &str) -> bool {
    left.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .eq_ignore_ascii_case(&right.split_whitespace().collect::<Vec<_>>().join(" "))
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

fn contains_as_of(output: &str, as_of: NaiveDate) -> bool {
    let date = as_of.to_string();
    output.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        (lower.contains("as of")
            || lower.contains("as-of")
            || lower.contains("as_of")
            || lower.contains("as-of:"))
            && line.contains(&date)
    })
}

fn contains_datetime(output: &str, value: DateTime<Utc>) -> bool {
    output.contains(&value.to_rfc3339())
        || output.contains(&value.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        || output.contains(&value.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn contains_fallback_disclosure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("annual")
        && [
            "not available",
            "not published",
            "not yet",
            "unavailable",
            "no eligible",
            "fallback",
            "instead",
        ]
        .iter()
        .any(|phrase| lower.contains(phrase))
}
