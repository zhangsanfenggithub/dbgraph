//! Snapshot diff engine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::{DbObject, DbObjectKind, DbSnapshot};

/// Object-level change kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectChangeKind {
    /// Object exists only in the latest snapshot.
    Added,
    /// Object exists only in the previous snapshot.
    Removed,
    /// Object exists in both snapshots but relevant metadata changed.
    Changed,
}

/// One changed object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectChange {
    /// Change kind.
    pub kind: ObjectChangeKind,
    /// Object kind.
    pub object_kind: DbObjectKind,
    /// Fully qualified object name.
    pub full_name: String,
    /// Human-readable details.
    pub details: Vec<String>,
}

/// Possible rename. This is deliberately a candidate, not a definitive claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameCandidate {
    /// Removed object name.
    pub from_full_name: String,
    /// Added object name.
    pub to_full_name: String,
    /// Reason why this looks like a possible rename.
    pub reason: String,
}

/// Snapshot diff report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDiff {
    /// Previous snapshot id.
    pub previous_snapshot_id: String,
    /// Latest snapshot id.
    pub latest_snapshot_id: String,
    /// Whether schema hash changed.
    pub schema_hash_changed: bool,
    /// Object changes.
    pub changes: Vec<ObjectChange>,
    /// Rename candidates.
    pub rename_candidates: Vec<RenameCandidate>,
}

/// Compares two snapshots.
pub struct DiffEngine;

impl DiffEngine {
    /// Compares a previous snapshot with a latest snapshot.
    #[must_use]
    pub fn compare(previous: &DbSnapshot, latest: &DbSnapshot) -> SchemaDiff {
        let previous_by_name = object_map(previous);
        let latest_by_name = object_map(latest);
        let previous_names = previous_by_name.keys().cloned().collect::<BTreeSet<_>>();
        let latest_names = latest_by_name.keys().cloned().collect::<BTreeSet<_>>();
        let mut changes = Vec::new();

        for name in latest_names.difference(&previous_names) {
            if let Some(object) = latest_by_name.get(name) {
                changes.push(ObjectChange {
                    kind: ObjectChangeKind::Added,
                    object_kind: object.kind,
                    full_name: (*name).clone(),
                    details: vec!["object added".to_owned()],
                });
            }
        }

        for name in previous_names.difference(&latest_names) {
            if let Some(object) = previous_by_name.get(name) {
                changes.push(ObjectChange {
                    kind: ObjectChangeKind::Removed,
                    object_kind: object.kind,
                    full_name: (*name).clone(),
                    details: vec!["object removed".to_owned()],
                });
            }
        }

        for name in previous_names.intersection(&latest_names) {
            if let (Some(previous_object), Some(latest_object)) =
                (previous_by_name.get(name), latest_by_name.get(name))
            {
                let details = change_details(previous_object, latest_object);
                if !details.is_empty() {
                    changes.push(ObjectChange {
                        kind: ObjectChangeKind::Changed,
                        object_kind: latest_object.kind,
                        full_name: (*name).clone(),
                        details,
                    });
                }
            }
        }

        changes.sort_by(|left, right| {
            left.kind
                .cmp_key()
                .cmp(right.kind.cmp_key())
                .then_with(|| left.full_name.cmp(&right.full_name))
        });

        SchemaDiff {
            previous_snapshot_id: previous.id.clone(),
            latest_snapshot_id: latest.id.clone(),
            schema_hash_changed: previous.schema_hash != latest.schema_hash,
            rename_candidates: rename_candidates(&previous_by_name, &latest_by_name),
            changes,
        }
    }
}

impl ObjectChangeKind {
    fn cmp_key(self) -> &'static str {
        match self {
            Self::Added => "1-added",
            Self::Removed => "2-removed",
            Self::Changed => "3-changed",
        }
    }
}

fn object_map(snapshot: &DbSnapshot) -> BTreeMap<String, &DbObject> {
    snapshot
        .objects
        .iter()
        .map(|object| (object.full_name.clone(), object))
        .collect()
}

fn change_details(previous: &DbObject, latest: &DbObject) -> Vec<String> {
    let mut details = Vec::new();
    if previous.kind != latest.kind {
        details.push(format!(
            "kind changed from {} to {}",
            previous.kind.as_str(),
            latest.kind.as_str()
        ));
    }
    if previous
        .column
        .as_ref()
        .and_then(|column| column.data_type.as_deref())
        != latest
            .column
            .as_ref()
            .and_then(|column| column.data_type.as_deref())
    {
        details.push("column type changed".to_owned());
    }
    if previous.column.as_ref().and_then(|column| column.nullable)
        != latest.column.as_ref().and_then(|column| column.nullable)
    {
        details.push("column nullability changed".to_owned());
    }
    if previous
        .column
        .as_ref()
        .and_then(|column| column.default.as_deref())
        != latest
            .column
            .as_ref()
            .and_then(|column| column.default.as_deref())
    {
        details.push("column default changed".to_owned());
    }
    if previous.constraint != latest.constraint {
        details.push("constraint metadata changed".to_owned());
    }
    if previous.index != latest.index {
        details.push("index metadata changed".to_owned());
    }
    if previous.table != latest.table {
        details.push("table metadata changed".to_owned());
    }
    if previous.metadata != latest.metadata {
        details.push("object metadata changed".to_owned());
    }
    details
}

fn rename_candidates(
    previous: &BTreeMap<String, &DbObject>,
    latest: &BTreeMap<String, &DbObject>,
) -> Vec<RenameCandidate> {
    let latest_names = latest.keys().cloned().collect::<BTreeSet<_>>();
    let previous_names = previous.keys().cloned().collect::<BTreeSet<_>>();
    let removed = previous_names
        .difference(&latest_names)
        .filter_map(|name| previous.get(name).copied())
        .collect::<Vec<_>>();
    let added = latest_names
        .difference(&previous_names)
        .filter_map(|name| latest.get(name).copied())
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for old in removed {
        for new in &added {
            if old.kind == new.kind && old.schema_name == new.schema_name && similar_shape(old, new)
            {
                candidates.push(RenameCandidate {
                    from_full_name: old.full_name.clone(),
                    to_full_name: new.full_name.clone(),
                    reason: "same object kind and schema; review as possible rename".to_owned(),
                });
            }
        }
    }
    candidates.sort_by(|left, right| {
        left.from_full_name
            .cmp(&right.from_full_name)
            .then_with(|| left.to_full_name.cmp(&right.to_full_name))
    });
    candidates
}

fn similar_shape(left: &DbObject, right: &DbObject) -> bool {
    if left.kind == DbObjectKind::Table {
        return true;
    }
    left.table_name == right.table_name || left.column_name == right.column_name
}

#[cfg(test)]
mod tests {
    use crate::diff::{DiffEngine, ObjectChangeKind};
    use crate::model::{ColumnMetadata, DbObject, DbObjectKind, DbSnapshot};

    #[test]
    fn diff_detects_added_removed_changed_and_rename_candidates() {
        let mut previous = DbSnapshot::new("prev", "postgres", "app", 1);
        previous.schema_hash = Some("old".to_owned());
        previous
            .objects
            .push(table("table:public.users", "public.users"));
        previous
            .objects
            .push(table("table:public.orders", "public.orders"));
        previous.objects.push(column(
            "column:public.orders.status",
            "public.orders.status",
            "orders",
            "status",
            "text",
            true,
            None,
        ));
        previous
            .objects
            .push(table("table:public.people", "public.people"));

        let mut latest = DbSnapshot::new("latest", "postgres", "app", 2);
        latest.schema_hash = Some("new".to_owned());
        latest
            .objects
            .push(table("table:public.users", "public.users"));
        latest
            .objects
            .push(table("table:public.payments", "public.payments"));
        latest.objects.push(column(
            "column:public.orders.status",
            "public.orders.status",
            "orders",
            "status",
            "varchar",
            false,
            Some("'pending'"),
        ));
        latest
            .objects
            .push(table("table:public.customers", "public.customers"));

        let diff = DiffEngine::compare(&previous, &latest);

        assert!(diff.schema_hash_changed);
        assert!(diff
            .changes
            .iter()
            .any(|change| change.kind == ObjectChangeKind::Added
                && change.full_name == "public.payments"));
        assert!(diff
            .changes
            .iter()
            .any(|change| change.kind == ObjectChangeKind::Removed
                && change.full_name == "public.people"));
        assert!(diff.changes.iter().any(|change| {
            change.kind == ObjectChangeKind::Changed
                && change.full_name == "public.orders.status"
                && change.details.iter().any(|detail| detail.contains("type"))
                && change
                    .details
                    .iter()
                    .any(|detail| detail.contains("nullability"))
                && change
                    .details
                    .iter()
                    .any(|detail| detail.contains("default"))
        }));
        assert!(diff.rename_candidates.iter().any(|candidate| {
            candidate.from_full_name == "public.people"
                && candidate.to_full_name == "public.customers"
        }));
    }

    fn table(id: &str, full_name: &str) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Table, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(object.name.clone());
        object
    }

    fn column(
        id: &str,
        full_name: &str,
        table_name: &str,
        column_name: &str,
        data_type: &str,
        nullable: bool,
        default: Option<&str>,
    ) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Column, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.column_name = Some(column_name.to_owned());
        object.column = Some(ColumnMetadata {
            data_type: Some(data_type.to_owned()),
            data_type_family: Some(data_type.to_owned()),
            nullable: Some(nullable),
            default: default.map(ToOwned::to_owned),
            comment: None,
        });
        object
    }
}
