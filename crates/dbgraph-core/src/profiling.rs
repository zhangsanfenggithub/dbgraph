//! Profiling mode and profile post-processing helpers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::model::DbSnapshot;
use crate::{DbGraphError, Result};

/// Snapshot profiling depth.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilingMode {
    /// Schema metadata only. No table or column profile rows are retained.
    #[default]
    Schema,
    /// Provider/catalog statistics only.
    Stats,
    /// Explicit opt-in sample profiling.
    Sample,
}

impl fmt::Display for ProfilingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Schema => "schema",
            Self::Stats => "stats",
            Self::Sample => "sample",
        })
    }
}

impl FromStr for ProfilingMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "schema" => Ok(Self::Schema),
            "stats" => Ok(Self::Stats),
            "sample" => Ok(Self::Sample),
            _ => Err("profiling mode must be schema, stats, or sample".to_owned()),
        }
    }
}

/// Effective profiling options for a snapshot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilingOptions {
    /// Profiling depth.
    pub mode: ProfilingMode,
    /// Maximum sampled rows per table.
    pub max_rows_per_table: u32,
    /// Whether PII-like values should be masked.
    pub mask_pii: bool,
    /// Whether raw sample values may be stored.
    pub store_raw_samples: bool,
}

impl ProfilingOptions {
    /// Validates opt-in sample settings.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for unsafe or contradictory sampling settings.
    pub fn validate(&self) -> Result<()> {
        if self.mode == ProfilingMode::Sample && self.max_rows_per_table == 0 {
            return Err(DbGraphError::invalid_config(
                "snapshot.maxRowsPerTable must be greater than zero when profilingMode is sample",
            ));
        }
        if self.store_raw_samples && self.mode != ProfilingMode::Sample {
            return Err(DbGraphError::invalid_config(
                "security.storeRawSamples requires snapshot.profilingMode to be sample",
            ));
        }
        Ok(())
    }
}

/// Applies profiling mode semantics and records source metadata on the snapshot.
#[must_use]
pub fn apply_profiling_policy(mut snapshot: DbSnapshot, options: &ProfilingOptions) -> DbSnapshot {
    let profile_source = match options.mode {
        ProfilingMode::Schema => {
            snapshot.table_profiles.clear();
            snapshot.column_profiles.clear();
            "schema"
        }
        ProfilingMode::Stats => "catalog_statistics",
        ProfilingMode::Sample => "safe_sample",
    };

    for profile in &mut snapshot.table_profiles {
        profile
            .profile
            .insert("source".to_owned(), json!(profile_source));
    }
    for profile in &mut snapshot.column_profiles {
        profile
            .profile
            .insert("source".to_owned(), json!(profile_source));
    }
    snapshot.metadata.insert(
        "profiling".to_owned(),
        json!({
            "mode": options.mode.to_string(),
            "maxRowsPerTable": options.max_rows_per_table,
            "maskPii": options.mask_pii,
            "storeRawSamples": options.store_raw_samples,
            "source": profile_source
        }),
    );
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DbSnapshot, TableProfile};

    #[test]
    fn schema_mode_strips_provider_stats() {
        let mut snapshot = DbSnapshot::new("s1", "sqlite", "app", 1);
        snapshot.table_profiles.push(TableProfile {
            object_id: "table:users".to_owned(),
            row_estimate: Some(3),
            row_count_kind: Some("exact".to_owned()),
            size_bytes: None,
            profile: crate::model::Metadata::new(),
        });

        let next = apply_profiling_policy(
            snapshot,
            &ProfilingOptions {
                mode: ProfilingMode::Schema,
                max_rows_per_table: 20,
                mask_pii: true,
                store_raw_samples: false,
            },
        );

        assert!(next.table_profiles.is_empty());
        assert_eq!(
            next.metadata
                .get("profiling")
                .and_then(|value| value.get("source"))
                .and_then(serde_json::Value::as_str),
            Some("schema")
        );
    }

    #[test]
    fn raw_samples_require_sample_mode() {
        let err = ProfilingOptions {
            mode: ProfilingMode::Stats,
            max_rows_per_table: 20,
            mask_pii: true,
            store_raw_samples: true,
        }
        .validate()
        .expect_err("raw samples outside sample mode should fail");

        assert!(err.to_string().contains("storeRawSamples"));
    }
}
