//! Impact analysis helpers.

use std::collections::{HashMap, HashSet, VecDeque};

use dbgraph_core::model::{DbObject, DbObjectKind, DbSnapshot};
use dbgraph_core::{DbGraphError, Result};
use serde::{Deserialize, Serialize};

use crate::relations::resolve_object;

/// Impact traversal options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpactOptions {
    /// Maximum dependency depth.
    pub depth: usize,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        Self { depth: 2 }
    }
}

/// Direct or indirect impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactScope {
    /// One edge away.
    Direct,
    /// More than one edge away.
    Indirect,
}

/// Impact report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactReport {
    /// Target object.
    pub target: String,
    /// Impacted items.
    pub items: Vec<ImpactItem>,
    /// Risk notes.
    pub risks: Vec<ImpactRisk>,
}

/// Impacted object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactItem {
    /// Scope.
    pub scope: ImpactScope,
    /// Object kind.
    pub kind: String,
    /// Full name.
    pub full_name: String,
    /// Evidence.
    pub evidence: String,
}

/// Risk note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactRisk {
    /// Message.
    pub message: String,
    /// Evidence.
    pub evidence: String,
}

/// Impact analyzer.
pub struct ImpactAnalyzer;

impl ImpactAnalyzer {
    /// Creates a new analyzer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Analyzes impact of changing an object.
    ///
    /// # Errors
    ///
    /// Returns an error when the target object is not present.
    pub fn analyze(
        &self,
        snapshot: &DbSnapshot,
        target: &str,
        options: &ImpactOptions,
    ) -> Result<ImpactReport> {
        let Some(target_object) = resolve_object(snapshot, target) else {
            return Err(DbGraphError::invalid_argument(format!(
                "object `{target}` was not found in latest snapshot"
            )));
        };
        let object_by_id = snapshot
            .objects
            .iter()
            .map(|object| (object.id.as_str(), object))
            .collect::<HashMap<_, _>>();
        let mut items = Vec::new();
        let mut seen = HashSet::from([target_object.id.clone()]);
        let mut queue = VecDeque::from([(target_object.id.clone(), 0_usize, String::new())]);
        if let Some(parent_table) = parent_table(snapshot, target_object) {
            seen.insert(parent_table.id.clone());
            items.push(ImpactItem {
                scope: ImpactScope::Direct,
                kind: parent_table.kind.as_str().to_owned(),
                full_name: parent_table.full_name.clone(),
                evidence: "column belongs to table".to_owned(),
            });
            queue.push_back((
                parent_table.id.clone(),
                1_usize,
                "column belongs to table".to_owned(),
            ));
        }
        while let Some((object_id, depth, evidence)) = queue.pop_front() {
            if depth >= options.depth {
                continue;
            }
            for edge in snapshot
                .edges
                .iter()
                .filter(|edge| edge.to_object_id == object_id || edge.from_object_id == object_id)
            {
                let next_id = if edge.to_object_id == object_id {
                    edge.from_object_id.clone()
                } else {
                    edge.to_object_id.clone()
                };
                if !seen.insert(next_id.clone()) {
                    continue;
                }
                let Some(object) = object_by_id.get(next_id.as_str()) else {
                    continue;
                };
                let next_evidence = if evidence.is_empty() {
                    edge.kind.as_str().to_owned()
                } else {
                    format!("{evidence} -> {}", edge.kind.as_str())
                };
                items.push(ImpactItem {
                    scope: if depth == 0 {
                        ImpactScope::Direct
                    } else {
                        ImpactScope::Indirect
                    },
                    kind: object.kind.as_str().to_owned(),
                    full_name: object.full_name.clone(),
                    evidence: next_evidence.clone(),
                });
                queue.push_back((next_id, depth + 1, next_evidence));
            }
        }
        items.sort_by(|left, right| {
            left.scope
                .cmp_key()
                .cmp(right.scope.cmp_key())
                .then_with(|| left.full_name.cmp(&right.full_name))
        });
        Ok(ImpactReport {
            target: target_object.full_name.clone(),
            risks: risk_notes(target_object),
            items,
        })
    }
}

impl Default for ImpactAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl ImpactScope {
    fn cmp_key(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Indirect => "indirect",
        }
    }
}

fn risk_notes(object: &DbObject) -> Vec<ImpactRisk> {
    let mut risks = Vec::new();
    let full_name = object.full_name.to_ascii_lowercase();
    if full_name.ends_with(".tenant_id") || object.name.eq_ignore_ascii_case("tenant_id") {
        risks.push(ImpactRisk {
            message: "tenant_id changes can affect multi-tenant isolation".to_owned(),
            evidence: object.full_name.clone(),
        });
    }
    if full_name.ends_with(".status") || object.name.eq_ignore_ascii_case("status") {
        risks.push(ImpactRisk {
            message: "status changes can affect workflow filters and state machines".to_owned(),
            evidence: object.full_name.clone(),
        });
    }
    if matches!(
        object.kind,
        DbObjectKind::UniqueConstraint | DbObjectKind::ForeignKey | DbObjectKind::PrimaryKey
    ) {
        risks.push(ImpactRisk {
            message: format!(
                "{} changes can affect relational integrity",
                object.kind.as_str()
            ),
            evidence: object.full_name.clone(),
        });
    }
    if let Some(row_estimate) = object
        .metadata
        .get("rowEstimate")
        .and_then(serde_json::Value::as_i64)
    {
        if row_estimate > 1_000_000 {
            risks.push(ImpactRisk {
                message: "large table changes may be operationally expensive".to_owned(),
                evidence: format!("{} row estimate {row_estimate}", object.full_name),
            });
        }
    }
    risks
}

fn parent_table<'a>(snapshot: &'a DbSnapshot, object: &DbObject) -> Option<&'a DbObject> {
    let table_name = object.table_name.as_deref()?;
    snapshot.objects.iter().find(|candidate| {
        candidate.kind == DbObjectKind::Table
            && candidate
                .table_name
                .as_deref()
                .unwrap_or(candidate.name.as_str())
                == table_name
            && (object.schema_name.is_none() || candidate.schema_name == object.schema_name)
    })
}

#[cfg(test)]
mod tests {
    use crate::impact::{ImpactAnalyzer, ImpactOptions, ImpactScope};
    use dbgraph_core::model::{
        ColumnMetadata, DbEdge, DbEdgeKind, DbObject, DbObjectKind, DbSnapshot,
    };

    #[test]
    fn impact_finds_dependent_sql_views_related_tables_and_risks() {
        let snapshot = sample_snapshot();

        let report = ImpactAnalyzer::new()
            .analyze(
                &snapshot,
                "public.payments.status",
                &ImpactOptions { depth: 2 },
            )
            .expect("impact target should resolve");

        assert!(report
            .items
            .iter()
            .any(|item| item.scope == ImpactScope::Direct
                && item.full_name.contains("payment_status_query")));
        assert!(report
            .items
            .iter()
            .any(|item| item.scope == ImpactScope::Indirect && item.full_name == "public.orders"));
        assert!(report.risks.iter().any(
            |risk| risk.message.contains("status") && risk.evidence.contains("payments.status")
        ));
    }

    fn sample_snapshot() -> DbSnapshot {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        snapshot.objects.push(DbObject::new(
            "table:payments",
            DbObjectKind::Table,
            "public.payments",
        ));
        snapshot.objects.push(DbObject::new(
            "table:orders",
            DbObjectKind::Table,
            "public.orders",
        ));
        let mut status = DbObject::new(
            "column:payments.status",
            DbObjectKind::Column,
            "public.payments.status",
        );
        status.table_name = Some("payments".to_owned());
        status.column_name = Some("status".to_owned());
        status.column = Some(ColumnMetadata {
            data_type: Some("text".to_owned()),
            data_type_family: Some("text".to_owned()),
            nullable: Some(false),
            default: None,
            comment: None,
        });
        snapshot.objects.push(status);
        snapshot.objects.push(DbObject::new(
            "query:payment_status_query",
            DbObjectKind::Query,
            "sql.payment_status_query",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:query.status",
            DbEdgeKind::FiltersBy,
            "query:payment_status_query",
            "column:payments.status",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:orders.payments",
            DbEdgeKind::References,
            "table:orders",
            "table:payments",
        ));
        snapshot
    }
}
