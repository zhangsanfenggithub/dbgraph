//! Incremental sync planning helpers based on schema hashes.

use serde::{Deserialize, Serialize};

use crate::model::DbSnapshot;
use crate::snapshot::compute_schema_hash;
use crate::Result;

/// Incremental sync plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPlan {
    /// Schema hash is unchanged, so index rebuild can be skipped.
    Unchanged { schema_hash: String },
    /// Schema changed and the graph index should be rebuilt.
    Changed {
        previous_hash: Option<String>,
        next_hash: String,
    },
}

impl SyncPlan {
    /// Returns true when the index can be skipped.
    #[must_use]
    pub const fn can_skip_rebuild(&self) -> bool {
        matches!(self, Self::Unchanged { .. })
    }
}

/// Compares snapshots by stable schema hash.
///
/// # Errors
///
/// Returns an error if either hash cannot be computed.
pub fn plan_incremental_sync(previous: Option<&DbSnapshot>, next: &DbSnapshot) -> Result<SyncPlan> {
    let next_hash = compute_schema_hash(next)?;
    let previous_hash = previous
        .map(|snapshot| {
            snapshot
                .schema_hash
                .clone()
                .map_or_else(|| compute_schema_hash(snapshot), Ok)
        })
        .transpose()?;

    if previous_hash.as_deref() == Some(next_hash.as_str()) {
        Ok(SyncPlan::Unchanged {
            schema_hash: next_hash,
        })
    } else {
        Ok(SyncPlan::Changed {
            previous_hash,
            next_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DbObject, DbObjectKind, DbSnapshot};

    #[test]
    fn same_schema_skips_rebuild_even_with_new_timestamp() {
        let mut previous = sample("s1", 1);
        previous.schema_hash = Some(compute_schema_hash(&previous).unwrap());
        let next = sample("s2", 2);

        let plan = plan_incremental_sync(Some(&previous), &next).unwrap();

        assert!(plan.can_skip_rebuild());
    }

    fn sample(id: &str, created_at_unix_ms: u64) -> DbSnapshot {
        let mut snapshot = DbSnapshot::new(id, "postgres", "app", created_at_unix_ms);
        snapshot.objects.push(DbObject::new(
            "table:public.users",
            DbObjectKind::Table,
            "public.users",
        ));
        snapshot
    }
}
