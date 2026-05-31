//! Graph index builder and inferred relation engine.

use std::collections::{HashMap, HashSet};

use dbgraph_core::model::{
    ColumnMetadata, DbEdge, DbEdgeKind, DbObject, DbObjectKind, DbSnapshot, Evidence,
};
use dbgraph_core::Result;
use dbgraph_storage::GraphRepository;

pub mod analysis;
pub mod context;
pub mod impact;
pub mod relations;
pub mod search;
pub mod workload;

/// Summary returned after rebuilding a snapshot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexBuildSummary {
    /// Number of objects written.
    pub object_count: usize,
    /// Number of edges written.
    pub edge_count: usize,
    /// Number of table profiles written.
    pub table_profile_count: usize,
    /// Number of column profiles written.
    pub column_profile_count: usize,
}

/// Rebuilds the local graph index for a snapshot.
///
/// # Errors
///
/// Returns an error if the repository transaction fails.
pub fn rebuild_index(
    repository: &mut GraphRepository,
    snapshot: &DbSnapshot,
) -> Result<IndexBuildSummary> {
    repository.rebuild_snapshot(snapshot)?;
    Ok(IndexBuildSummary {
        object_count: snapshot.objects.len(),
        edge_count: snapshot.edges.len(),
        table_profile_count: snapshot.table_profiles.len(),
        column_profile_count: snapshot.column_profiles.len(),
    })
}

/// Options for inferred relation generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InferenceOptions {
    /// Object ids or full names to ignore.
    pub ignored_columns: HashSet<String>,
}

/// Infer possible table relations from naming and type compatibility.
#[must_use]
pub fn infer_relations(snapshot: &DbSnapshot, options: &InferenceOptions) -> Vec<DbEdge> {
    let tables = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Table)
        .collect::<Vec<_>>();
    let columns = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Column)
        .collect::<Vec<_>>();

    let mut table_by_name = HashMap::new();
    for table in &tables {
        table_by_name.insert(normalize_identifier(&table.name), *table);
        table_by_name.insert(normalize_identifier(&table.full_name), *table);
    }

    let mut id_columns_by_table = HashMap::new();
    for column in &columns {
        if column.column_name.as_deref() == Some("id") || column.name == "id" {
            if let Some(table_name) = column.table_name.as_deref() {
                id_columns_by_table.insert(normalize_identifier(table_name), *column);
            }
        }
    }

    let explicit_pairs = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == DbEdgeKind::References)
        .map(|edge| (edge.from_object_id.as_str(), edge.to_object_id.as_str()))
        .collect::<HashSet<_>>();

    let mut inferred = Vec::new();
    for column in columns {
        if options.ignored_columns.contains(&column.id)
            || options.ignored_columns.contains(&column.full_name)
        {
            continue;
        }
        let Some(column_name) = column.column_name.as_deref().or(Some(column.name.as_str())) else {
            continue;
        };
        let Some(target_hint) = infer_target_table_hint(column_name) else {
            continue;
        };
        let Some(target_table) = find_target_table(&target_hint, &table_by_name) else {
            continue;
        };
        let Some(target_column) = id_columns_by_table
            .get(&normalize_identifier(&target_table.name))
            .copied()
        else {
            continue;
        };
        if explicit_pairs.contains(&(column.id.as_str(), target_column.id.as_str())) {
            continue;
        }
        if !types_compatible(column.column.as_ref(), target_column.column.as_ref()) {
            continue;
        }

        let confidence = confidence_for(column_name, &target_hint);
        inferred.push(DbEdge {
            id: format!("inferred:{}->{}", column.id, target_column.id),
            kind: DbEdgeKind::InferredReference,
            from_object_id: column.id.clone(),
            to_object_id: target_column.id.clone(),
            confidence,
            evidence: vec![Evidence {
                source: "naming_rule".to_owned(),
                detail: format!(
                    "{} looks like it references {}.id",
                    column.full_name, target_table.full_name
                ),
            }],
            metadata: std::collections::BTreeMap::new(),
        });
    }

    inferred.sort_by(|left, right| {
        left.from_object_id
            .cmp(&right.from_object_id)
            .then_with(|| left.to_object_id.cmp(&right.to_object_id))
    });
    inferred
}

/// Adds inferred relation edges to a cloned snapshot.
#[must_use]
pub fn snapshot_with_inferred_relations(
    snapshot: &DbSnapshot,
    options: &InferenceOptions,
) -> DbSnapshot {
    let mut next = snapshot.clone();
    next.edges.extend(infer_relations(snapshot, options));
    next
}

fn infer_target_table_hint(column_name: &str) -> Option<String> {
    let normalized = normalize_identifier(column_name);
    match normalized.as_str() {
        "tenant_id" => Some("tenants".to_owned()),
        "entity_id" => Some("entities".to_owned()),
        "created_by" | "updated_by" => Some("users".to_owned()),
        _ => normalized
            .strip_suffix("_id")
            .filter(|prefix| !prefix.is_empty())
            .map(pluralize),
    }
}

fn find_target_table<'a>(
    target_hint: &str,
    table_by_name: &HashMap<String, &'a DbObject>,
) -> Option<&'a DbObject> {
    let normalized = normalize_identifier(target_hint);
    table_by_name
        .get(&normalized)
        .or_else(|| table_by_name.get(&singularize(&normalized)))
        .or_else(|| table_by_name.get(&pluralize(&normalized)))
        .copied()
}

fn types_compatible(left: Option<&ColumnMetadata>, right: Option<&ColumnMetadata>) -> bool {
    match (
        left.and_then(|column| column.data_type_family.as_deref()),
        right.and_then(|column| column.data_type_family.as_deref()),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn confidence_for(column_name: &str, target_hint: &str) -> f64 {
    match normalize_identifier(column_name).as_str() {
        "tenant_id" | "created_by" | "updated_by" => 0.9,
        "entity_id" => 0.72,
        _ if !target_hint.is_empty() => 0.82,
        _ => 0.65,
    }
}

fn normalize_identifier(value: &str) -> String {
    value
        .rsplit('.')
        .next()
        .unwrap_or(value)
        .to_ascii_lowercase()
}

fn pluralize(value: &str) -> String {
    if value.ends_with('s') {
        value.to_owned()
    } else if value.ends_with('y') {
        format!("{}ies", value.trim_end_matches('y'))
    } else {
        format!("{value}s")
    }
}

fn singularize(value: &str) -> String {
    value.strip_suffix("ies").map_or_else(
        || value.trim_end_matches('s').to_owned(),
        |prefix| format!("{prefix}y"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgraph_core::model::{ColumnMetadata, DbObject};
    use dbgraph_storage::GraphRepository;

    #[test]
    fn rebuild_index_writes_snapshot_counts() {
        let snapshot = sample_snapshot();
        let mut repository = GraphRepository::open_in_memory().expect("repository should open");

        let summary = rebuild_index(&mut repository, &snapshot).expect("index should rebuild");

        assert_eq!(summary.object_count, snapshot.objects.len());
        assert_eq!(
            repository.object_count(&snapshot.id).unwrap(),
            i64::try_from(snapshot.objects.len()).unwrap()
        );
    }

    #[test]
    fn inferred_relation_has_confidence_and_evidence() {
        let snapshot = sample_snapshot();

        let edges = infer_relations(&snapshot, &InferenceOptions::default());

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, DbEdgeKind::InferredReference);
        assert!(edges[0].confidence > 0.0);
        assert!(edges[0].evidence[0].detail.contains("user_id"));
    }

    #[test]
    fn ignore_config_suppresses_false_positive() {
        let snapshot = sample_snapshot();
        let mut options = InferenceOptions::default();
        options
            .ignored_columns
            .insert("column:orders.user_id".to_owned());

        let edges = infer_relations(&snapshot, &options);

        assert!(edges.is_empty());
    }

    #[test]
    fn inferred_edges_are_distinct_from_explicit_edges() {
        let mut snapshot = sample_snapshot();
        snapshot.edges.push(DbEdge::explicit(
            "fk:orders.user_id",
            DbEdgeKind::References,
            "column:orders.user_id",
            "column:users.id",
        ));

        let edges = infer_relations(&snapshot, &InferenceOptions::default());

        assert!(edges.is_empty());
    }

    fn sample_snapshot() -> DbSnapshot {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        snapshot
            .objects
            .push(table("table:users", "public.users", "users"));
        snapshot
            .objects
            .push(table("table:orders", "public.orders", "orders"));
        snapshot.objects.push(column(
            "column:users.id",
            "public.users.id",
            "users",
            "id",
            "integer",
        ));
        snapshot.objects.push(column(
            "column:orders.user_id",
            "public.orders.user_id",
            "orders",
            "user_id",
            "integer",
        ));
        snapshot
    }

    fn table(id: &str, full_name: &str, table_name: &str) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Table, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object
    }

    fn column(
        id: &str,
        full_name: &str,
        table_name: &str,
        column_name: &str,
        family: &str,
    ) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Column, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.column_name = Some(column_name.to_owned());
        object.column = Some(ColumnMetadata {
            data_type: Some("bigint".to_owned()),
            data_type_family: Some(family.to_owned()),
            nullable: Some(false),
            default: None,
            comment: None,
        });
        object
    }
}
