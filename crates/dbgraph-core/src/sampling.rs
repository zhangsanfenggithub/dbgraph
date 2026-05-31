//! Safe sample summary helpers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::DbObject;
use crate::security::{mask_value, MaskingStrategy, PiiDetector};

/// Sample extraction strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingStrategy {
    /// Use deterministic limit sampling.
    Limit,
    /// Use database random sampling when a provider supports it.
    Random,
}

/// Safe sampler options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplingOptions {
    /// Maximum rows per table.
    pub max_rows_per_table: u32,
    /// Sampling strategy.
    pub strategy: SamplingStrategy,
    /// Optional statement timeout in milliseconds.
    pub statement_timeout_ms: Option<u64>,
    /// Whether raw non-sensitive values may be retained.
    pub store_raw_samples: bool,
    /// Masking strategy for sensitive values.
    pub masking_strategy: MaskingStrategy,
}

/// Per-column sample summary that avoids sensitive raw values by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSampleSummary {
    /// Column full name.
    pub column: String,
    /// Number of observed non-null values.
    pub observed_non_null: usize,
    /// Number of observed null values.
    pub observed_null: usize,
    /// Whether this column was considered sensitive.
    pub sensitive: bool,
    /// Masked or raw examples according to policy.
    pub examples: Vec<Value>,
}

/// Summarizes already-fetched row values with masking policy.
#[must_use]
pub fn summarize_column_values(
    column: &DbObject,
    values: &[Value],
    detector: &PiiDetector,
    options: &SamplingOptions,
) -> ColumnSampleSummary {
    let finding = detector.detect_column(column);
    let sensitive = finding.score >= 0.4;
    let mut observed_non_null = 0;
    let mut observed_null = 0;
    let mut examples = Vec::new();

    for value in values.iter().take(options.max_rows_per_table as usize) {
        if value.is_null() {
            observed_null += 1;
            continue;
        }
        observed_non_null += 1;
        if examples.len() >= 5 {
            continue;
        }
        let text = value_to_string(value);
        let stored = if sensitive || !options.store_raw_samples {
            Value::String(mask_value(&text, options.masking_strategy))
        } else {
            value.clone()
        };
        examples.push(stored);
    }

    ColumnSampleSummary {
        column: column.full_name.clone(),
        observed_non_null,
        observed_null,
        sensitive,
        examples,
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColumnMetadata, DbObject, DbObjectKind};
    use crate::security::PiiRuleConfig;

    #[test]
    fn sensitive_samples_are_masked_even_when_raw_samples_are_enabled() {
        let detector = PiiDetector::new(&PiiRuleConfig::default());
        let mut column = DbObject::new(
            "column:users.email",
            DbObjectKind::Column,
            "public.users.email",
        );
        column.column_name = Some("email".to_owned());
        column.column = Some(ColumnMetadata {
            data_type: Some("text".to_owned()),
            data_type_family: Some("text".to_owned()),
            nullable: Some(false),
            default: None,
            comment: None,
        });

        let summary = summarize_column_values(
            &column,
            &[Value::String("a@example.test".to_owned())],
            &detector,
            &SamplingOptions {
                max_rows_per_table: 10,
                strategy: SamplingStrategy::Limit,
                statement_timeout_ms: Some(1_000),
                store_raw_samples: true,
                masking_strategy: MaskingStrategy::Redact,
            },
        );

        assert!(summary.sensitive);
        assert_eq!(
            summary.examples,
            vec![Value::String("[REDACTED]".to_owned())]
        );
    }
}
