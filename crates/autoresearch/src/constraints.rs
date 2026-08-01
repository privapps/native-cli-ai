//! Secondary resource and quality constraints for research candidates.
//!
//! A primary metric answers "did this candidate improve the objective?".
//! [`ConstraintSet`] answers the independent policy question "is the
//! candidate admissible?".  Keeping those answers separate means a result
//! can explain that it was discarded for a memory or quality violation even
//! when its primary metric improved.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::program::{MetricGoal, ResearchProgram};
use crate::result::ExperimentDecision;

/// Whether a secondary value describes a resource budget or a quality floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    Resource,
    Quality,
}

/// Comparison used by a secondary constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintOperator {
    LessThanOrEqual,
    GreaterThanOrEqual,
}

impl ConstraintOperator {
    fn accepts(self, observed: f64, limit: f64) -> bool {
        match self {
            Self::LessThanOrEqual => observed <= limit,
            Self::GreaterThanOrEqual => observed >= limit,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::LessThanOrEqual => "<=",
            Self::GreaterThanOrEqual => ">=",
        }
    }
}

/// A named secondary constraint declared by a research program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecondaryConstraint {
    /// Canonical key used to look up a value in an observation map.
    pub name: String,
    pub kind: ConstraintKind,
    pub operator: ConstraintOperator,
    pub limit: f64,
    /// Optional human-readable unit, such as `GB`, `ms`, or `%`.
    pub unit: Option<String>,
}

impl SecondaryConstraint {
    pub fn new(
        name: impl Into<String>,
        kind: ConstraintKind,
        operator: ConstraintOperator,
        limit: f64,
    ) -> Self {
        Self {
            name: canonical_name(&name.into()),
            kind,
            operator,
            limit,
            unit: None,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }
}

/// A constraint that could not be satisfied by a candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintViolation {
    pub name: String,
    pub kind: ConstraintKind,
    pub operator: ConstraintOperator,
    pub limit: f64,
    pub observed: Option<f64>,
    pub unit: Option<String>,
    pub reason: String,
}

/// Result of evaluating all secondary constraints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintEvaluation {
    pub violations: Vec<ConstraintViolation>,
}

impl ConstraintEvaluation {
    pub fn satisfied(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Primary-metric decision plus independent secondary-constraint evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstrainedDecision {
    pub primary_decision: ExperimentDecision,
    pub constraints: ConstraintEvaluation,
    pub final_decision: ExperimentDecision,
}

/// The declared constraints for one research program.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConstraintSet {
    pub primary_goal: MetricGoal,
    pub constraints: Vec<SecondaryConstraint>,
}

impl ConstraintSet {
    pub fn new(primary_goal: MetricGoal) -> Self {
        Self {
            primary_goal,
            constraints: Vec::new(),
        }
    }

    pub fn with_constraint(mut self, constraint: SecondaryConstraint) -> Self {
        self.push_unique(constraint);
        self
    }

    pub fn push_unique(&mut self, constraint: SecondaryConstraint) {
        if !self
            .constraints
            .iter()
            .any(|existing| existing.name == constraint.name)
        {
            self.constraints.push(constraint);
        }
    }

    /// Translate the existing program representation into typed constraints.
    ///
    /// `max_memory_gb` is retained as a first-class compatibility field by
    /// the original program API.  Free-form entries in `extra_constraints`
    /// provide the extensible resource/quality declaration syntax.
    pub fn from_program(program: &ResearchProgram) -> Self {
        let mut set = Self::new(program.metric_goal);
        if let Some(memory) = program.max_memory_gb {
            set.push_unique(
                SecondaryConstraint::new(
                    "memory_gb",
                    ConstraintKind::Resource,
                    ConstraintOperator::LessThanOrEqual,
                    memory,
                )
                .with_unit("GB"),
            );
        }

        for declaration in &program.extra_constraints {
            if let Some(constraint) = parse_declaration(declaration) {
                set.push_unique(constraint);
            }
        }
        set
    }

    pub fn evaluate(&self, observations: &BTreeMap<String, f64>) -> ConstraintEvaluation {
        let violations = self
            .constraints
            .iter()
            .filter_map(|constraint| {
                let observed = find_observation(observations, &constraint.name);
                match observed {
                    Some(value)
                        if value.is_finite()
                            && constraint.operator.accepts(value, constraint.limit) =>
                    {
                        None
                    }
                    Some(value) => Some(ConstraintViolation {
                        name: constraint.name.clone(),
                        kind: constraint.kind,
                        operator: constraint.operator,
                        limit: constraint.limit,
                        observed: Some(value),
                        unit: constraint.unit.clone(),
                        reason: format!(
                            "{}={} violates {} {}{}",
                            constraint.name,
                            value,
                            constraint.operator.symbol(),
                            constraint.limit,
                            format_unit(constraint.unit.as_deref())
                        ),
                    }),
                    None => Some(ConstraintViolation {
                        name: constraint.name.clone(),
                        kind: constraint.kind,
                        operator: constraint.operator,
                        limit: constraint.limit,
                        observed: None,
                        unit: constraint.unit.clone(),
                        reason: format!("missing observation for {}", constraint.name),
                    }),
                }
            })
            .collect();

        ConstraintEvaluation { violations }
    }

    /// Apply constraints after the primary metric decision.
    ///
    /// A violated constraint turns a metric `Keep` or `Discard` into a
    /// `Discard`; a malformed primary result remains a `Failure`.  The
    /// violation list stays available for audit/UI output.
    pub fn decide(
        &self,
        primary_decision: ExperimentDecision,
        observations: &BTreeMap<String, f64>,
    ) -> ConstrainedDecision {
        let constraints = self.evaluate(observations);
        let final_decision = if primary_decision == ExperimentDecision::Failure {
            ExperimentDecision::Failure
        } else if constraints.satisfied() {
            primary_decision
        } else {
            ExperimentDecision::Discard
        };

        ConstrainedDecision {
            primary_decision,
            constraints,
            final_decision,
        }
    }
}

/// Parse the small, deliberately human-friendly constraint syntax accepted
/// in `ResearchProgram.extra_constraints`.
///
/// Examples: `max_memory_gb <= 50`, `accuracy >= 0.90`,
/// `latency_ms: 100`, and `quality.accuracy >= 0.9`.
pub fn parse_declaration(input: &str) -> Option<SecondaryConstraint> {
    let cleaned = input
        .trim()
        .trim_start_matches(['-', '*'])
        .trim()
        .replace('`', "")
        .replace("**", "");
    if cleaned.is_empty() {
        return None;
    }

    let (lhs, rhs, operator) = if let Some((lhs, rhs)) = cleaned.split_once("<=") {
        (lhs, rhs, ConstraintOperator::LessThanOrEqual)
    } else if let Some((lhs, rhs)) = cleaned.split_once(">=") {
        (lhs, rhs, ConstraintOperator::GreaterThanOrEqual)
    } else if let Some((lhs, rhs)) = cleaned.split_once(':') {
        let inferred = infer_operator(lhs);
        (lhs, rhs, inferred)
    } else if let Some((lhs, rhs)) = cleaned.split_once('<') {
        (lhs, rhs, ConstraintOperator::LessThanOrEqual)
    } else if let Some((lhs, rhs)) = cleaned.split_once('>') {
        (lhs, rhs, ConstraintOperator::GreaterThanOrEqual)
    } else {
        return None;
    };

    let (limit, unit) = parse_number_and_unit(rhs)?;
    let name = canonical_name(lhs);
    if name.is_empty() || !limit.is_finite() {
        return None;
    }

    Some(SecondaryConstraint {
        kind: infer_kind(&name),
        name,
        operator,
        limit,
        unit,
    })
}

fn infer_operator(name: &str) -> ConstraintOperator {
    let lower = name.to_ascii_lowercase();
    if lower.contains("max")
        || lower.contains("memory")
        || lower.contains("latency")
        || lower.contains("time")
        || lower.contains("flop")
        || lower.contains("token")
    {
        ConstraintOperator::LessThanOrEqual
    } else {
        ConstraintOperator::GreaterThanOrEqual
    }
}

fn infer_kind(name: &str) -> ConstraintKind {
    if name.contains("memory")
        || name.contains("latency")
        || name.contains("time")
        || name.contains("flop")
        || name.contains("token")
        || name.contains("cost")
        || name.contains("resource")
    {
        ConstraintKind::Resource
    } else {
        ConstraintKind::Quality
    }
}

fn canonical_name(raw: &str) -> String {
    let mut name = raw
        .trim()
        .to_ascii_lowercase()
        .replace(['.', '/', '-'], "_");
    name.retain(|character| character.is_ascii_alphanumeric() || character == '_');
    while name.contains("__") {
        name = name.replace("__", "_");
    }
    name = name.trim_matches('_').to_string();

    for prefix in ["max_", "min_", "limit_", "required_"] {
        if let Some(stripped) = name.strip_prefix(prefix) {
            name = stripped.to_string();
            break;
        }
    }
    name
}

fn parse_number_and_unit(raw: &str) -> Option<(f64, Option<String>)> {
    let trimmed = raw.trim().trim_matches(['`', '*']);
    let mut number_end = 0;
    for (index, character) in trimmed.char_indices() {
        if character.is_ascii_digit()
            || matches!(character, '.' | '+' | '-' | 'e' | 'E')
            || (character == ' ' && number_end > 0)
        {
            number_end = index + character.len_utf8();
        } else {
            break;
        }
    }
    let number_text = trimmed[..number_end].trim();
    let number = number_text.parse().ok()?;
    let suffix = trimmed[number_end..]
        .trim()
        .trim_matches(|character: char| !character.is_ascii_alphabetic() && character != '%');
    let unit = (!suffix.is_empty()).then(|| suffix.to_string());
    Some((number, unit))
}

fn find_observation(observations: &BTreeMap<String, f64>, name: &str) -> Option<f64> {
    observations
        .iter()
        .find_map(|(key, value)| (canonical_name(key) == name).then_some(*value))
}

fn format_unit(unit: Option<&str>) -> String {
    unit.map(|value| format!(" {value}")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{EditableFile, MetricCommand};

    fn observations(values: &[(&str, f64)]) -> BTreeMap<String, f64> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value))
            .collect()
    }

    #[test]
    fn parses_resource_and_quality_declarations() {
        let memory = parse_declaration("max_memory_gb <= 50GB").unwrap();
        assert_eq!(memory.name, "memory_gb");
        assert_eq!(memory.kind, ConstraintKind::Resource);
        assert_eq!(memory.operator, ConstraintOperator::LessThanOrEqual);
        assert_eq!(memory.limit, 50.0);
        assert_eq!(memory.unit.as_deref(), Some("GB"));

        let quality = parse_declaration("accuracy >= 0.90").unwrap();
        assert_eq!(quality.name, "accuracy");
        assert_eq!(quality.kind, ConstraintKind::Quality);
        assert_eq!(quality.operator, ConstraintOperator::GreaterThanOrEqual);
    }

    #[test]
    fn violations_are_separate_and_override_keep() {
        let set = ConstraintSet::new(MetricGoal::Minimize)
            .with_constraint(SecondaryConstraint::new(
                "memory_gb",
                ConstraintKind::Resource,
                ConstraintOperator::LessThanOrEqual,
                8.0,
            ))
            .with_constraint(SecondaryConstraint::new(
                "accuracy",
                ConstraintKind::Quality,
                ConstraintOperator::GreaterThanOrEqual,
                0.9,
            ));
        let decision = set.decide(
            ExperimentDecision::Keep,
            &observations(&[("memory_gb", 10.0), ("accuracy", 0.95)]),
        );

        assert_eq!(decision.primary_decision, ExperimentDecision::Keep);
        assert_eq!(decision.constraints.violations.len(), 1);
        assert_eq!(decision.constraints.violations[0].name, "memory_gb");
        assert_eq!(decision.final_decision, ExperimentDecision::Discard);
    }

    #[test]
    fn program_memory_and_free_form_constraints_are_typed() {
        let program = ResearchProgram {
            name: "bounded".into(),
            description: String::new(),
            editable_files: vec![EditableFile::new("train.py")],
            fixed_files: Vec::new(),
            metric_command: MetricCommand {
                command: "echo metric".into(),
                parse_regex: "metric".into(),
            },
            metric_goal: MetricGoal::Maximize,
            time_budget_seconds: 10,
            extra_constraints: vec!["accuracy >= 0.9".into()],
            max_memory_gb: Some(16.0),
            instructions: String::new(),
        };
        let set = ConstraintSet::from_program(&program);
        assert_eq!(set.primary_goal, MetricGoal::Maximize);
        assert_eq!(set.constraints.len(), 2);
        assert!(
            set.evaluate(&observations(&[("memory_gb", 8.0), ("accuracy", 0.92)]))
                .satisfied()
        );
    }

    #[test]
    fn missing_and_non_finite_values_are_constraint_failures() {
        let set =
            ConstraintSet::new(MetricGoal::Minimize).with_constraint(SecondaryConstraint::new(
                "quality",
                ConstraintKind::Quality,
                ConstraintOperator::GreaterThanOrEqual,
                0.9,
            ));
        let missing = set.evaluate(&BTreeMap::new());
        assert_eq!(missing.violations[0].observed, None);
        let nan = set.evaluate(&observations(&[("quality", f64::NAN)]));
        assert!(nan.violations[0].observed.is_some_and(f64::is_nan));
    }
}
