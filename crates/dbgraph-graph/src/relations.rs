//! Relation traversal helpers.

use std::collections::{HashMap, HashSet, VecDeque};

use dbgraph_core::model::{DbEdge, DbObject, DbSnapshot};
use dbgraph_core::{DbGraphError, Result};
use serde::{Deserialize, Serialize};

/// Traversal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow outgoing edges.
    Outgoing,
    /// Follow incoming edges.
    Incoming,
    /// Follow both incoming and outgoing edges.
    Both,
}

/// Relation traversal options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationsOptions {
    /// Maximum edge depth.
    pub depth: usize,
    /// Direction.
    pub direction: Direction,
}

impl Default for RelationsOptions {
    fn default() -> Self {
        Self {
            depth: 1,
            direction: Direction::Both,
        }
    }
}

/// Relation report for one object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationsReport {
    /// Target object full name.
    pub target: String,
    /// Relation paths.
    pub paths: Vec<RelationPath>,
}

/// One path from the target to another object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationPath {
    /// Objects in path order.
    pub objects: Vec<String>,
    /// Edges in path order.
    pub edges: Vec<RelationEdge>,
}

/// Edge rendered in relation path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationEdge {
    /// Edge kind.
    pub kind: String,
    /// From object.
    pub from: String,
    /// To object.
    pub to: String,
    /// Confidence.
    pub confidence: f64,
    /// Evidence detail strings.
    pub evidence: Vec<String>,
}

/// Builds a relation report for an object name.
///
/// # Errors
///
/// Returns an error if the object cannot be resolved.
pub fn relations_for(
    snapshot: &DbSnapshot,
    object_name: &str,
    options: &RelationsOptions,
) -> Result<RelationsReport> {
    let objects_by_id = snapshot
        .objects
        .iter()
        .map(|object| (object.id.as_str(), object))
        .collect::<HashMap<_, _>>();
    let Some(start) = resolve_object(snapshot, object_name) else {
        return Err(DbGraphError::invalid_argument(format!(
            "object `{object_name}` was not found in latest snapshot"
        )));
    };
    let mut paths = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::from([(start.id.clone(), Vec::<DbEdge>::new())]);
    visited.insert(start.id.clone());
    while let Some((object_id, edge_path)) = queue.pop_front() {
        if edge_path.len() >= options.depth {
            continue;
        }
        for edge in adjacent_edges(snapshot, &object_id, options.direction) {
            let next_id = if edge.from_object_id == object_id {
                edge.to_object_id.clone()
            } else {
                edge.from_object_id.clone()
            };
            if !objects_by_id.contains_key(next_id.as_str()) {
                continue;
            }
            let mut next_path = edge_path.clone();
            next_path.push(edge.clone());
            paths.push(render_path(&objects_by_id, &start.id, &next_path));
            if visited.insert(next_id.clone()) {
                queue.push_back((next_id, next_path));
            }
        }
    }
    paths.sort_by(|left, right| {
        left.objects
            .join(">")
            .cmp(&right.objects.join(">"))
            .then_with(|| left.edges.len().cmp(&right.edges.len()))
    });
    Ok(RelationsReport {
        target: start.full_name.clone(),
        paths,
    })
}

/// Resolves an object by full name, id, or short name.
#[must_use]
pub fn resolve_object<'a>(snapshot: &'a DbSnapshot, object_name: &str) -> Option<&'a DbObject> {
    let normalized = object_name.to_ascii_lowercase();
    snapshot.objects.iter().find(|object| {
        object.id.eq_ignore_ascii_case(object_name)
            || object.full_name.eq_ignore_ascii_case(object_name)
            || object.name.eq_ignore_ascii_case(object_name)
            || object
                .full_name
                .to_ascii_lowercase()
                .ends_with(&format!(".{normalized}"))
    })
}

fn adjacent_edges(snapshot: &DbSnapshot, object_id: &str, direction: Direction) -> Vec<DbEdge> {
    snapshot
        .edges
        .iter()
        .filter(|edge| match direction {
            Direction::Outgoing => edge.from_object_id == object_id,
            Direction::Incoming => edge.to_object_id == object_id,
            Direction::Both => edge.from_object_id == object_id || edge.to_object_id == object_id,
        })
        .cloned()
        .collect()
}

fn render_path(
    objects_by_id: &HashMap<&str, &DbObject>,
    start_id: &str,
    edges: &[DbEdge],
) -> RelationPath {
    let mut object_ids = vec![start_id.to_owned()];
    let mut current = start_id.to_owned();
    for edge in edges {
        current = if edge.from_object_id == current {
            edge.to_object_id.clone()
        } else {
            edge.from_object_id.clone()
        };
        object_ids.push(current.clone());
    }
    RelationPath {
        objects: object_ids
            .iter()
            .filter_map(|id| {
                objects_by_id
                    .get(id.as_str())
                    .map(|object| object.full_name.clone())
            })
            .collect(),
        edges: edges
            .iter()
            .map(|edge| RelationEdge {
                kind: edge.kind.as_str().to_owned(),
                from: objects_by_id.get(edge.from_object_id.as_str()).map_or_else(
                    || edge.from_object_id.clone(),
                    |object| object.full_name.clone(),
                ),
                to: objects_by_id.get(edge.to_object_id.as_str()).map_or_else(
                    || edge.to_object_id.clone(),
                    |object| object.full_name.clone(),
                ),
                confidence: edge.confidence,
                evidence: edge
                    .evidence
                    .iter()
                    .map(|evidence| evidence.detail.clone())
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use crate::relations::{relations_for, Direction, RelationsOptions};
    use dbgraph_core::model::{DbEdge, DbEdgeKind, DbObject, DbObjectKind, DbSnapshot};

    #[test]
    fn relations_report_incoming_outgoing_and_inferred_with_depth() {
        let snapshot = sample_snapshot();

        let report = relations_for(
            &snapshot,
            "public.users",
            &RelationsOptions {
                depth: 2,
                direction: Direction::Both,
            },
        )
        .expect("relations should resolve table");

        assert!(report
            .paths
            .iter()
            .any(|path| path.edges.iter().any(|edge| edge.kind == "references")));
        assert!(report.paths.iter().any(|path| path
            .edges
            .iter()
            .any(|edge| edge.kind == "inferred_reference")));
        assert!(report.paths.iter().all(|path| path.edges.len() <= 2));
    }

    fn sample_snapshot() -> DbSnapshot {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        snapshot.objects.push(DbObject::new(
            "table:users",
            DbObjectKind::Table,
            "public.users",
        ));
        snapshot.objects.push(DbObject::new(
            "table:orders",
            DbObjectKind::Table,
            "public.orders",
        ));
        snapshot.objects.push(DbObject::new(
            "table:payments",
            DbObjectKind::Table,
            "public.payments",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "fk:orders.users",
            DbEdgeKind::References,
            "table:orders",
            "table:users",
        ));
        snapshot.edges.push(DbEdge {
            id: "inf:payments.orders".to_owned(),
            kind: DbEdgeKind::InferredReference,
            from_object_id: "table:payments".to_owned(),
            to_object_id: "table:orders".to_owned(),
            confidence: 0.82,
            evidence: vec![],
            metadata: std::collections::BTreeMap::new(),
        });
        snapshot
    }
}
