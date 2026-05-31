//! Context candidate retrieval and formatting.

use std::collections::{BTreeMap, HashSet};

use dbgraph_core::model::{DbObject, DbSnapshot};
use serde::{Deserialize, Serialize};

use crate::search::{extract_search_terms, search_snapshot, SearchOptions};
use crate::workload::WorkloadModel;

/// Configurable ranking weights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingWeights {
    /// Name/text match weight.
    pub text_match: f64,
    /// Graph-neighbor weight.
    pub graph_neighbor: f64,
    /// Table/profile weight.
    pub statistics: f64,
    /// SQL workload frequency weight.
    pub workload: f64,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            text_match: 1.0,
            graph_neighbor: 0.35,
            statistics: 0.1,
            workload: 0.2,
        }
    }
}

/// Context builder options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextOptions {
    /// Rough token budget.
    pub token_budget: usize,
    /// Maximum objects.
    pub max_objects: usize,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            token_budget: 800,
            max_objects: 12,
        }
    }
}

/// Context package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPackage {
    /// Original query.
    pub query: String,
    /// Relevant objects.
    pub objects: Vec<ContextObject>,
    /// Relation paths/edges among selected objects.
    pub relation_paths: Vec<String>,
    /// Risk notes.
    pub risks: Vec<String>,
    /// Suggested next tools.
    pub suggested_next_tools: Vec<String>,
    /// Ranking notes.
    pub ranking_notes: Vec<String>,
    /// Rough token estimate.
    pub estimated_tokens: usize,
}

/// Context object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextObject {
    /// Object kind.
    pub kind: String,
    /// Full name.
    pub full_name: String,
    /// Summary.
    pub summary: String,
    /// Score.
    pub score: f64,
}

/// Builds AI database context from a snapshot.
pub struct ContextBuilder {
    weights: RankingWeights,
}

impl ContextBuilder {
    /// Creates a new builder.
    #[must_use]
    pub const fn new(weights: RankingWeights) -> Self {
        Self { weights }
    }

    /// Builds a compact context package.
    #[must_use]
    pub fn build(
        &self,
        snapshot: &DbSnapshot,
        query: &str,
        options: &ContextOptions,
    ) -> ContextPackage {
        let terms = extract_search_terms(query);
        let mut scored = self.seed_scored_objects(snapshot, query, options.max_objects);
        self.expand_graph_neighbors(snapshot, &mut scored);
        self.apply_statistics_boost(snapshot, &mut scored);
        let workload = WorkloadModel::from_snapshot(snapshot);
        self.apply_workload_boost(&workload, &mut scored);
        let (context_objects, estimated_tokens) = select_context_objects(scored, options);
        let relation_paths = relation_paths(snapshot, &context_objects);

        ContextPackage {
            query: query.to_owned(),
            objects: context_objects,
            relation_paths,
            risks: vec![
                "Read-only context: verify SQL against your local graph before executing changes."
                    .to_owned(),
            ],
            suggested_next_tools: vec![
                "dbgraph table <table>".to_owned(),
                "dbgraph relations <object>".to_owned(),
                "dbgraph validate-sql --sql <SQL>".to_owned(),
            ],
            ranking_notes: vec![format!("terms: {}", terms.join(", "))],
            estimated_tokens,
        }
    }

    fn seed_scored_objects(
        &self,
        snapshot: &DbSnapshot,
        query: &str,
        max_objects: usize,
    ) -> BTreeMap<String, (DbObject, f64)> {
        let mut scored = BTreeMap::<String, (DbObject, f64)>::new();
        for result in search_snapshot(
            snapshot,
            query,
            &SearchOptions {
                limit: max_objects * 2,
            },
        ) {
            if let Some(object) = snapshot
                .objects
                .iter()
                .find(|object| object.id == result.id)
            {
                scored.insert(
                    object.id.clone(),
                    (object.clone(), result.score * self.weights.text_match),
                );
            }
        }
        scored
    }

    fn expand_graph_neighbors(
        &self,
        snapshot: &DbSnapshot,
        scored: &mut BTreeMap<String, (DbObject, f64)>,
    ) {
        let seed_ids = scored.keys().cloned().collect::<HashSet<_>>();
        for edge in &snapshot.edges {
            if seed_ids.contains(&edge.from_object_id) || seed_ids.contains(&edge.to_object_id) {
                for object_id in [&edge.from_object_id, &edge.to_object_id] {
                    if let Some(object) = snapshot
                        .objects
                        .iter()
                        .find(|object| object.id == *object_id)
                    {
                        let entry = scored
                            .entry(object.id.clone())
                            .or_insert((object.clone(), 0.0));
                        entry.1 += 15.0 * self.weights.graph_neighbor;
                    }
                }
            }
        }
    }

    fn apply_statistics_boost(
        &self,
        snapshot: &DbSnapshot,
        scored: &mut BTreeMap<String, (DbObject, f64)>,
    ) {
        for profile in &snapshot.table_profiles {
            if let Some(entry) = scored.get_mut(&profile.object_id) {
                if profile.row_estimate.unwrap_or_default() > 0 {
                    entry.1 += self.weights.statistics;
                }
            }
        }
    }

    fn apply_workload_boost(
        &self,
        workload: &WorkloadModel,
        scored: &mut BTreeMap<String, (DbObject, f64)>,
    ) {
        for (object_id, (_, score)) in scored {
            let frequency = workload.frequency(object_id);
            if frequency > 0 {
                *score += f64::from(frequency) * self.weights.workload;
            }
        }
    }
}

fn select_context_objects(
    scored: BTreeMap<String, (DbObject, f64)>,
    options: &ContextOptions,
) -> (Vec<ContextObject>, usize) {
    let mut objects = scored.into_values().collect::<Vec<_>>();
    objects.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.full_name.cmp(&right.0.full_name))
    });

    let mut context_objects = Vec::new();
    let mut estimated_tokens = 0_usize;
    for (object, score) in objects {
        let summary = object_summary(&object);
        let estimate = estimate_tokens(&object.full_name) + estimate_tokens(&summary) + 4;
        if context_objects.len() >= options.max_objects
            || estimated_tokens.saturating_add(estimate) > options.token_budget
        {
            break;
        }
        estimated_tokens += estimate;
        context_objects.push(ContextObject {
            kind: object.kind.as_str().to_owned(),
            full_name: object.full_name,
            summary,
            score,
        });
    }
    (context_objects, estimated_tokens)
}

fn relation_paths(snapshot: &DbSnapshot, context_objects: &[ContextObject]) -> Vec<String> {
    let selected = context_objects
        .iter()
        .map(|object| object.full_name.as_str())
        .collect::<HashSet<_>>();
    snapshot
        .edges
        .iter()
        .filter_map(|edge| {
            let from = snapshot
                .objects
                .iter()
                .find(|object| object.id == edge.from_object_id)?;
            let to = snapshot
                .objects
                .iter()
                .find(|object| object.id == edge.to_object_id)?;
            (selected.contains(from.full_name.as_str()) && selected.contains(to.full_name.as_str()))
                .then(|| {
                    format!(
                        "{} -{}-> {}",
                        from.full_name,
                        edge.kind.as_str(),
                        to.full_name
                    )
                })
        })
        .collect()
}

fn object_summary(object: &DbObject) -> String {
    object
        .metadata
        .get("comment")
        .and_then(|value| value.as_str())
        .map_or_else(
            || format!("{} {}", object.kind.as_str(), object.full_name),
            ToOwned::to_owned,
        )
}

fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace().count().max(text.len() / 4)
}

#[cfg(test)]
mod tests {
    use crate::context::{ContextBuilder, ContextOptions, RankingWeights};
    use dbgraph_core::model::{DbEdge, DbEdgeKind, DbObject, DbObjectKind, DbSnapshot};

    #[test]
    fn context_builder_returns_ranked_refund_payment_order_context_with_budget() {
        let snapshot = sample_snapshot();
        let context = ContextBuilder::new(RankingWeights::default()).build(
            &snapshot,
            "refund payment order",
            &ContextOptions {
                token_budget: 80,
                max_objects: 5,
            },
        );

        let names = context
            .objects
            .iter()
            .map(|object| object.full_name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"public.refunds"));
        assert!(names.contains(&"public.payments"));
        assert!(names.contains(&"public.orders"));
        assert!(context.estimated_tokens <= 80);
        assert!(!context.ranking_notes.is_empty());
    }

    fn sample_snapshot() -> DbSnapshot {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        for (id, name, comment) in [
            ("table:refunds", "public.refunds", "refund request records"),
            (
                "table:payments",
                "public.payments",
                "payment ledger records",
            ),
            ("table:orders", "public.orders", "customer order records"),
            ("table:users", "public.users", "login records"),
        ] {
            let mut object = DbObject::new(id, DbObjectKind::Table, name);
            object.metadata.insert(
                "comment".to_owned(),
                serde_json::Value::String(comment.to_owned()),
            );
            snapshot.objects.push(object);
        }
        snapshot.edges.push(DbEdge::explicit(
            "edge:refunds.payments",
            DbEdgeKind::References,
            "table:refunds",
            "table:payments",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:payments.orders",
            DbEdgeKind::References,
            "table:payments",
            "table:orders",
        ));
        snapshot
    }
}
