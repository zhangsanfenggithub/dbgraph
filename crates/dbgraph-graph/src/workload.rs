//! Workload model derived from SQL artifact/query graph edges.

use std::collections::{BTreeMap, BTreeSet};

use dbgraph_core::model::{DbEdgeKind, DbObjectKind, DbSnapshot};
use serde::{Deserialize, Serialize};

/// Per-object workload counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectWorkload {
    /// Reads from this object.
    pub reads: u32,
    /// Writes to this object.
    pub writes: u32,
    /// Joins involving this object.
    pub joins: u32,
    /// Filters involving this object.
    pub filters: u32,
}

impl ObjectWorkload {
    /// Total frequency across all supported edge kinds.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.reads + self.writes + self.joins + self.filters
    }
}

/// Workload counters keyed by object id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadModel {
    /// Object counters.
    pub objects: BTreeMap<String, ObjectWorkload>,
    /// Number of unique SQL fingerprints included.
    pub unique_fingerprints: usize,
}

impl WorkloadModel {
    /// Builds a workload model from SQL query objects and their graph edges.
    #[must_use]
    pub fn from_snapshot(snapshot: &DbSnapshot) -> Self {
        let query_fingerprints = snapshot
            .objects
            .iter()
            .filter(|object| object.kind == DbObjectKind::Query)
            .map(|object| {
                let fingerprint = object
                    .metadata
                    .get("fingerprint")
                    .and_then(|value| value.as_str())
                    .unwrap_or(object.id.as_str())
                    .to_owned();
                (object.id.as_str(), fingerprint)
            })
            .collect::<BTreeMap<_, _>>();

        let mut seen_edges = BTreeSet::<(String, String, String)>::new();
        let mut counters = BTreeMap::<String, ObjectWorkload>::new();
        for edge in &snapshot.edges {
            let Some(fingerprint) = query_fingerprints.get(edge.from_object_id.as_str()) else {
                continue;
            };
            if !seen_edges.insert((
                fingerprint.clone(),
                edge.kind.as_str().to_owned(),
                edge.to_object_id.clone(),
            )) {
                continue;
            }

            let entry = counters.entry(edge.to_object_id.clone()).or_default();
            match edge.kind {
                DbEdgeKind::ReadsFrom => entry.reads += 1,
                DbEdgeKind::WritesTo => entry.writes += 1,
                DbEdgeKind::JoinsOn => entry.joins += 1,
                DbEdgeKind::FiltersBy => entry.filters += 1,
                _ => {}
            }
        }

        let unique_fingerprints = query_fingerprints
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
            .len();
        Self {
            objects: counters,
            unique_fingerprints,
        }
    }

    /// Returns workload frequency for an object.
    #[must_use]
    pub fn frequency(&self, object_id: &str) -> u32 {
        self.objects.get(object_id).map_or(0, ObjectWorkload::total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgraph_core::model::{DbEdge, DbObject};

    #[test]
    fn duplicate_sql_fingerprints_are_counted_once_per_target_and_kind() {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        for id in ["query:a", "query:b"] {
            let mut query = DbObject::new(id, DbObjectKind::Query, id);
            query.metadata.insert(
                "fingerprint".to_owned(),
                serde_json::Value::String("same-fp".to_owned()),
            );
            snapshot.objects.push(query);
        }
        snapshot.objects.push(DbObject::new(
            "table:orders",
            DbObjectKind::Table,
            "public.orders",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:a",
            DbEdgeKind::ReadsFrom,
            "query:a",
            "table:orders",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:b",
            DbEdgeKind::ReadsFrom,
            "query:b",
            "table:orders",
        ));

        let workload = WorkloadModel::from_snapshot(&snapshot);

        assert_eq!(workload.unique_fingerprints, 1);
        assert_eq!(workload.frequency("table:orders"), 1);
    }
}
