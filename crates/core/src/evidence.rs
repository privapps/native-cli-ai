use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAuthority {
    Official,
    Secondary,
    Unknown,
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

/// Evidence captured by the reusable research boundary.
///
/// This record deliberately contains retrieval and provenance fields only.
/// Financial interpretation belongs to the compatibility/policy layer that
/// consumes the ledger, not to the generic evidence boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenericEvidenceRecord {
    pub url: String,
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub content: Option<String>,
    pub retrieved_at: DateTime<Utc>,
    pub response_status: Option<u16>,
    pub http_date: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub authority: SourceAuthority,
}

#[derive(Debug, Clone)]
struct EvidenceLedgerState {
    as_of: NaiveDate,
    observations: Vec<GenericEvidenceRecord>,
    evidence: Vec<GenericEvidenceRecord>,
    conflicts: Vec<String>,
}

/// Runtime-owned, turn-scoped store for generic evidence and provenance.
///
/// `as_of` is supplied when a turn begins and is not part of an evidence
/// record, so collectors cannot accidentally choose a different date for an
/// individual observation. Starting another turn clears all prior evidence
/// and conflicts before installing the new date.
#[derive(Debug, Clone)]
pub struct EvidenceLedger {
    state: Arc<Mutex<EvidenceLedgerState>>,
}

impl EvidenceLedger {
    pub fn new(as_of: NaiveDate) -> Self {
        Self {
            state: Arc::new(Mutex::new(EvidenceLedgerState {
                as_of,
                observations: Vec::new(),
                evidence: Vec::new(),
                conflicts: Vec::new(),
            })),
        }
    }

    pub fn begin_turn(&self, as_of: NaiveDate) {
        if let Ok(mut state) = self.state.lock() {
            state.as_of = as_of;
            state.observations.clear();
            state.evidence.clear();
            state.conflicts.clear();
        }
    }

    pub fn as_of(&self) -> NaiveDate {
        self.state
            .lock()
            .map(|state| state.as_of)
            .unwrap_or_else(|_| Utc::now().date_naive())
    }

    pub fn record_evidence(&self, evidence: GenericEvidenceRecord) {
        let _ = self.record_evidence_with_index(evidence);
    }

    pub(crate) fn record_evidence_with_index(
        &self,
        mut evidence: GenericEvidenceRecord,
    ) -> Option<usize> {
        evidence.url = canonical_url(&evidence.url);
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        state.observations.push(evidence.clone());
        let observation_index = state.observations.len() - 1;
        if let Some(index) = state
            .evidence
            .iter()
            .position(|existing| existing.url == evidence.url)
        {
            let previous = state.evidence[index].clone();
            record_generic_conflicts(&mut state.conflicts, &previous, &evidence);
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
            if existing.title.is_none() {
                existing.title = evidence.title;
            }
            if existing.snippet.is_none() {
                existing.snippet = evidence.snippet;
            }
            if existing.content.is_none() {
                existing.content = evidence.content;
            }
            if source_authority_rank(evidence.authority) > source_authority_rank(existing.authority)
            {
                existing.authority = evidence.authority;
            }
            return Some(observation_index);
        }
        state.evidence.push(evidence);
        Some(observation_index)
    }

    pub fn evidence(&self) -> Vec<GenericEvidenceRecord> {
        self.state
            .lock()
            .map(|state| state.evidence.clone())
            .unwrap_or_default()
    }

    pub fn observations(&self) -> Vec<GenericEvidenceRecord> {
        self.state
            .lock()
            .map(|state| state.observations.clone())
            .unwrap_or_default()
    }

    pub fn conflicts(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.conflicts.clone())
            .unwrap_or_default()
    }
}

pub fn classify_source_authority(url: &str) -> SourceAuthority {
    let lower = url.to_ascii_lowercase();
    let Some((_, host_and_path)) = lower.split_once("://") else {
        return SourceAuthority::Unknown;
    };
    let host_and_path = host_and_path.split('#').next().unwrap_or_default();
    let host = host_and_path
        .split('/')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();
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

pub fn canonical_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn record_generic_conflicts(
    conflicts: &mut Vec<String>,
    existing: &GenericEvidenceRecord,
    incoming: &GenericEvidenceRecord,
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
    if let (Some(existing_status), Some(incoming_status)) =
        (existing.response_status, incoming.response_status)
        && existing_status != incoming_status
    {
        conflicts.push(format!(
            "Conflicting response statuses were observed for {}: {} and {}.",
            existing.url, existing_status, incoming_status
        ));
    }
}

fn source_authority_rank(authority: SourceAuthority) -> u8 {
    match authority {
        SourceAuthority::Official => 2,
        SourceAuthority::Secondary => 1,
        SourceAuthority::Unknown => 0,
    }
}
