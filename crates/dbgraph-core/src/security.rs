//! PII detection and masking utilities for profile/sample data.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{ColumnProfile, DbObject, DbObjectKind, DbSnapshot, Metadata};

/// How a sensitive value should be represented.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskingStrategy {
    /// Preserve shape only.
    #[default]
    Mask,
    /// Stable one-way hash.
    Hash,
    /// Replace with a fixed marker.
    Redact,
}

/// Rule configuration for PII detection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PiiRuleConfig {
    /// Extra user-provided sensitive identifier fragments.
    pub custom_sensitive_terms: Vec<String>,
}

/// PII match details.
#[derive(Debug, Clone, PartialEq)]
pub struct PiiFinding {
    /// Score from 0.0 to 1.0.
    pub score: f64,
    /// Human-readable rule ids that matched.
    pub reasons: Vec<String>,
}

/// Lightweight deterministic detector for column metadata.
pub struct PiiDetector {
    terms: Vec<String>,
}

impl PiiDetector {
    /// Creates a detector with built-in and custom terms.
    #[must_use]
    pub fn new(config: &PiiRuleConfig) -> Self {
        let mut terms = [
            "email",
            "phone",
            "mobile",
            "password",
            "passwd",
            "token",
            "secret",
            "address",
            "ssn",
            "social_security",
            "credit_card",
            "card_number",
            "iban",
            "passport",
            "id_card",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        terms.extend(
            config
                .custom_sensitive_terms
                .iter()
                .map(|term| term.to_ascii_lowercase()),
        );
        terms.sort();
        terms.dedup();
        Self { terms }
    }

    /// Scores a column object.
    #[must_use]
    pub fn detect_column(&self, object: &DbObject) -> PiiFinding {
        let mut score = 0.0_f64;
        let mut reasons = Vec::new();
        let mut haystack = format!("{} {}", object.name, object.full_name).to_ascii_lowercase();
        if let Some(column) = &object.column {
            if let Some(data_type) = &column.data_type {
                haystack.push(' ');
                haystack.push_str(&data_type.to_ascii_lowercase());
            }
            if let Some(comment) = &column.comment {
                haystack.push(' ');
                haystack.push_str(&comment.to_ascii_lowercase());
            }
        }
        if let Some(comment) = object
            .metadata
            .get("comment")
            .and_then(|value| value.as_str())
        {
            haystack.push(' ');
            haystack.push_str(&comment.to_ascii_lowercase());
        }

        for term in &self.terms {
            if haystack.contains(term) {
                score += 0.45;
                reasons.push(format!("term:{term}"));
            }
        }
        if haystack.contains("varchar")
            || haystack.contains("text")
            || haystack.contains("char")
            || haystack.contains("string")
        {
            score += 0.05;
        }
        if object
            .column_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("id"))
        {
            score = score.min(0.2);
        }

        score = score.min(1.0);
        PiiFinding { score, reasons }
    }
}

/// Masks a scalar value with the requested strategy.
#[must_use]
pub fn mask_value(value: &str, strategy: MaskingStrategy) -> String {
    match strategy {
        MaskingStrategy::Mask => {
            if value.is_empty() {
                String::new()
            } else {
                "*".repeat(value.chars().count().clamp(4, 12))
            }
        }
        MaskingStrategy::Hash => {
            let digest = Sha256::digest(value.as_bytes());
            format!("sha256:{}", hex_prefix(&digest, 16))
        }
        MaskingStrategy::Redact => "[REDACTED]".to_owned(),
    }
}

/// Adds PII scores to column profiles, creating profiles for sensitive columns when needed.
#[must_use]
pub fn apply_pii_profiles(mut snapshot: DbSnapshot, detector: &PiiDetector) -> DbSnapshot {
    let column_ids = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Column)
        .map(|object| {
            let finding = detector.detect_column(object);
            (object.id.clone(), finding)
        })
        .collect::<Vec<_>>();

    for (object_id, finding) in column_ids {
        if finding.score <= 0.0 {
            continue;
        }
        if let Some(profile) = snapshot
            .column_profiles
            .iter_mut()
            .find(|profile| profile.object_id == object_id)
        {
            profile.pii_score = Some(finding.score);
            profile
                .profile
                .insert("piiReasons".to_owned(), serde_json::json!(finding.reasons));
        } else {
            let mut profile = Metadata::new();
            profile.insert("piiReasons".to_owned(), serde_json::json!(finding.reasons));
            snapshot.column_profiles.push(ColumnProfile {
                object_id,
                data_type_family: None,
                null_fraction: None,
                distinct_estimate: None,
                pii_score: Some(finding.score),
                profile,
            });
        }
    }
    snapshot
}

fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(chars);
    for byte in bytes {
        if out.len() >= chars {
            break;
        }
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        if out.len() >= chars {
            break;
        }
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnMetadata, DbObject};

    #[test]
    fn detector_scores_name_type_comment_and_custom_terms() {
        let detector = PiiDetector::new(&PiiRuleConfig {
            custom_sensitive_terms: vec!["tax_id".to_owned()],
        });
        let mut object = DbObject::new(
            "column:users.tax_id",
            DbObjectKind::Column,
            "public.users.tax_id",
        );
        object.column_name = Some("tax_id".to_owned());
        object.column = Some(ColumnMetadata {
            data_type: Some("text".to_owned()),
            data_type_family: Some("text".to_owned()),
            nullable: Some(false),
            default: None,
            comment: Some("government identifier".to_owned()),
        });

        let finding = detector.detect_column(&object);

        assert!(finding.score >= 0.45);
        assert!(finding.reasons.iter().any(|reason| reason == "term:tax_id"));
    }

    #[test]
    fn masking_never_returns_original_sensitive_value() {
        assert_ne!(
            mask_value("person@example.test", MaskingStrategy::Mask),
            "person@example.test"
        );
        assert!(mask_value("person@example.test", MaskingStrategy::Hash).starts_with("sha256:"));
        assert_eq!(
            mask_value("person@example.test", MaskingStrategy::Redact),
            "[REDACTED]"
        );
    }
}
