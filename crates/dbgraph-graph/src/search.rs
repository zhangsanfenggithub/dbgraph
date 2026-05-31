//! Search query parsing and snapshot search.

use dbgraph_core::model::{DbObject, DbSnapshot};
use serde::{Deserialize, Serialize};

/// Parsed structured search query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedSearchQuery {
    /// Free text terms.
    pub text: String,
    /// Kind filters.
    pub kinds: Vec<String>,
    /// Schema filters.
    pub schema_filters: Vec<String>,
    /// Name filters.
    pub name_filters: Vec<String>,
}

/// Search options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    /// Maximum number of results.
    pub limit: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self { limit: 20 }
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// Object id.
    pub id: String,
    /// Object kind.
    pub kind: String,
    /// Object short name.
    pub name: String,
    /// Fully qualified name.
    pub full_name: String,
    /// Summary text.
    pub summary: String,
    /// Deterministic score.
    pub score: f64,
}

/// Parses field-qualified search syntax.
#[must_use]
pub fn parse_search_query(raw: &str) -> ParsedSearchQuery {
    let mut parsed = ParsedSearchQuery::default();
    let mut text = Vec::new();
    for token in tokenize(raw) {
        let Some((key, value)) = token.split_once(':') else {
            text.push(token);
            continue;
        };
        let value = unquote(value);
        if value.is_empty() {
            text.push(token);
            continue;
        }
        match key.to_ascii_lowercase().as_str() {
            "kind" => parsed.kinds.push(value.to_ascii_lowercase()),
            "schema" => parsed.schema_filters.push(value.to_ascii_lowercase()),
            "name" => parsed.name_filters.push(value),
            _ => text.push(token),
        }
    }
    text.join(" ").trim().clone_into(&mut parsed.text);
    parsed
}

/// Searches snapshot objects with deterministic ranking.
#[must_use]
pub fn search_snapshot(
    snapshot: &DbSnapshot,
    query: &str,
    options: &SearchOptions,
) -> Vec<SearchResult> {
    let parsed = parse_search_query(query);
    let terms = extract_search_terms(&parsed.text);
    let mut results = snapshot
        .objects
        .iter()
        .filter(|object| matches_filters(object, &parsed))
        .filter_map(|object| {
            let haystack = object_haystack(object);
            let score = score_object(object, &haystack, &terms, &parsed);
            (score > 0.0 || terms.is_empty()).then(|| SearchResult {
                id: object.id.clone(),
                kind: object.kind.as_str().to_owned(),
                name: object.name.clone(),
                full_name: object.full_name.clone(),
                summary: object_summary(object),
                score,
            })
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.full_name.cmp(&right.full_name))
    });
    results.truncate(options.limit);
    results
}

/// Extracts useful terms from natural-language text.
#[must_use]
pub fn extract_search_terms(query: &str) -> Vec<String> {
    let expanded = query
        .replace(['_', '.'], " ")
        .chars()
        .flat_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                vec![ch]
            } else {
                vec![' ']
            }
        })
        .collect::<String>();
    let mut terms = Vec::new();
    for word in expanded.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        if lower.len() < 3 || is_stop_word(&lower) {
            continue;
        }
        terms.push(lower.clone());
        if let Some(stripped) = lower.strip_suffix('s') {
            if stripped.len() >= 3 {
                terms.push(stripped.to_owned());
            }
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in raw.chars() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                current.push(ch);
            }
            ch if ch.is_whitespace() && !in_quote => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned()
}

fn matches_filters(object: &DbObject, parsed: &ParsedSearchQuery) -> bool {
    (parsed.kinds.is_empty()
        || parsed
            .kinds
            .iter()
            .any(|kind| object.kind.as_str() == kind.as_str()))
        && (parsed.schema_filters.is_empty()
            || object.schema_name.as_ref().is_some_and(|schema| {
                let schema = schema.to_ascii_lowercase();
                parsed.schema_filters.contains(&schema)
            }))
        && (parsed.name_filters.is_empty()
            || parsed.name_filters.iter().any(|filter| {
                let filter = filter.to_ascii_lowercase();
                object.name.to_ascii_lowercase().contains(&filter)
                    || object.full_name.to_ascii_lowercase().contains(&filter)
            }))
}

fn score_object(
    object: &DbObject,
    haystack: &str,
    terms: &[String],
    parsed: &ParsedSearchQuery,
) -> f64 {
    let mut score = 0.0;
    let mut matched = terms.is_empty();
    for term in terms {
        if object.full_name.eq_ignore_ascii_case(term) || object.name.eq_ignore_ascii_case(term) {
            score += 50.0;
            matched = true;
        } else if object.name.to_ascii_lowercase().starts_with(term) {
            score += 25.0;
            matched = true;
        } else if object.name.to_ascii_lowercase().contains(term) {
            score += 15.0;
            matched = true;
        }
        if haystack.contains(term) {
            score += 10.0;
            matched = true;
        }
    }
    if !matched {
        return 0.0;
    }
    score += kind_bonus(object.kind.as_str());
    if !parsed.kinds.is_empty() {
        score += 5.0;
    }
    score
}

fn kind_bonus(kind: &str) -> f64 {
    match kind {
        "table" | "view" | "materialized_view" => 12.0,
        "column" => 8.0,
        "query" | "sql_artifact" => 6.0,
        "foreign_key" | "primary_key" | "unique_constraint" | "check_constraint" => 5.0,
        _ => 1.0,
    }
}

fn object_haystack(object: &DbObject) -> String {
    format!(
        "{} {} {}",
        object.name,
        object.full_name,
        serde_json::to_string(&object.metadata).unwrap_or_default()
    )
    .to_ascii_lowercase()
}

fn object_summary(object: &DbObject) -> String {
    if let Some(comment) = object
        .metadata
        .get("comment")
        .and_then(|value| value.as_str())
    {
        return comment.to_owned();
    }
    if let Some(comment) = object
        .table
        .as_ref()
        .and_then(|table| table.comment.clone())
    {
        return comment;
    }
    if let Some(comment) = object
        .column
        .as_ref()
        .and_then(|column| column.comment.clone())
    {
        return comment;
    }
    object
        .metadata
        .get("normalizedSql")
        .and_then(|value| value.as_str())
        .map_or_else(
            || format!("{} {}", object.kind.as_str(), object.full_name),
            |sql| sql.chars().take(160).collect(),
        )
}

fn is_stop_word(value: &str) -> bool {
    matches!(
        value,
        "the" | "and" | "for" | "with" | "from" | "this" | "that" | "show" | "give" | "about"
    )
}

#[cfg(test)]
mod tests {
    use crate::search::{parse_search_query, search_snapshot, SearchOptions};
    use dbgraph_core::model::{DbObject, DbObjectKind, DbSnapshot};

    #[test]
    fn parses_structured_filters_and_free_text() {
        let parsed = parse_search_query(r#"kind:table name:"payment events" schema:public refund"#);

        assert_eq!(parsed.text, "refund");
        assert_eq!(parsed.kinds, vec!["table"]);
        assert_eq!(parsed.name_filters, vec!["payment events"]);
        assert_eq!(parsed.schema_filters, vec!["public"]);
    }

    #[test]
    fn search_finds_payment_comments_filters_kind_and_sorts_stably() {
        let snapshot = sample_snapshot();

        let results = search_snapshot(&snapshot, "kind:table payment", &SearchOptions::default());

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "table");
        assert_eq!(results[0].full_name, "public.payments");
        assert!(results[0].summary.contains("payment"));
    }

    fn sample_snapshot() -> DbSnapshot {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        let mut payments = DbObject::new("table:payments", DbObjectKind::Table, "public.payments");
        payments.schema_name = Some("public".to_owned());
        payments.metadata.insert(
            "comment".to_owned(),
            serde_json::Value::String("payment ledger records".to_owned()),
        );
        let mut orders = DbObject::new("table:orders", DbObjectKind::Table, "public.orders");
        orders.schema_name = Some("public".to_owned());
        let mut status = DbObject::new(
            "column:payments.status",
            DbObjectKind::Column,
            "public.payments.status",
        );
        status.schema_name = Some("public".to_owned());
        status.table_name = Some("payments".to_owned());
        snapshot.objects = vec![orders, status, payments];
        snapshot
    }
}
