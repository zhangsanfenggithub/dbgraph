//! Repository-wide database analysis rules.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use dbgraph_core::model::{DbEdgeKind, DbObject, DbObjectKind, DbSnapshot};
use serde::{Deserialize, Serialize};

/// Analysis scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisScope {
    /// All scopes.
    #[default]
    All,
    /// Security and operational risk.
    Risk,
    /// Schema quality.
    Quality,
    /// Query/index performance hints.
    Performance,
}

impl FromStr for AnalysisScope {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "risk" => Ok(Self::Risk),
            "quality" => Ok(Self::Quality),
            "performance" | "perf" => Ok(Self::Performance),
            _ => Err("analysis scope must be all, risk, quality, or performance".to_owned()),
        }
    }
}

/// Finding severity sorted from highest to lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    /// Highest priority.
    Critical,
    /// High priority.
    High,
    /// Medium priority.
    Medium,
    /// Low priority.
    Low,
}

impl PartialOrd for FindingSeverity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FindingSeverity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl FindingSeverity {
    const fn rank(self) -> u8 {
        match self {
            Self::Critical => 4,
            Self::High => 3,
            Self::Medium => 2,
            Self::Low => 1,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

/// Analysis options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnalysisOptions {
    /// Scope filter.
    pub scope: AnalysisScope,
}

/// Database analysis report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisReport {
    /// Snapshot id analyzed.
    pub snapshot_id: String,
    /// Scope filter.
    pub scope: AnalysisScope,
    /// Findings sorted by severity, rule id, and object.
    pub findings: Vec<AnalysisFinding>,
    /// Count by severity label.
    pub severity_counts: BTreeMap<String, usize>,
    /// High-level review summary.
    pub overview: AnalysisOverview,
    /// Findings grouped into review sections.
    pub sections: Vec<AnalysisSection>,
    /// Highest-priority findings for quick review.
    pub top_findings: Vec<AnalysisFinding>,
    /// Numeric risk score derived from severity weights.
    pub risk_score: u32,
}

/// High-level analysis summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisOverview {
    /// Total finding count.
    pub total_findings: usize,
    /// Numeric risk score derived from severity weights.
    pub risk_score: u32,
    /// Short summary suitable for report headers.
    pub summary: String,
}

/// Group of findings in an audit-style report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSection {
    /// Stable section id.
    pub id: String,
    /// Human-readable section title.
    pub title: String,
    /// Section summary.
    pub summary: String,
    /// Finding count in this section.
    pub finding_count: usize,
    /// Count by severity label for this section.
    pub severity_counts: BTreeMap<String, usize>,
}

/// One analysis finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisFinding {
    /// Scope.
    pub scope: AnalysisScope,
    /// Severity.
    pub severity: FindingSeverity,
    /// Stable rule id.
    pub rule_id: String,
    /// Object full name.
    pub object: String,
    /// Human-readable message.
    pub message: String,
    /// Evidence.
    pub evidence: String,
    /// Short finding title.
    pub title: String,
    /// Detailed description.
    pub description: String,
    /// Expected impact if the finding is ignored.
    pub impact: String,
    /// Suggested next action.
    pub suggested_fix: String,
    /// Confidence from 0.0 to 1.0.
    pub confidence: f64,
    /// Searchable finding tags.
    pub tags: Vec<String>,
    /// Related object names, such as SQL artifacts that reference the finding object.
    pub related_objects: Vec<String>,
}

/// Rule-based snapshot analyzer.
pub struct AnalysisAnalyzer;

impl AnalysisAnalyzer {
    /// Creates a new analyzer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Analyzes a snapshot with deterministic rule output.
    #[must_use]
    pub fn analyze(&self, snapshot: &DbSnapshot, options: &AnalysisOptions) -> AnalysisReport {
        let mut findings = Vec::new();
        if includes(options.scope, AnalysisScope::Risk) {
            risk_findings(snapshot, &mut findings);
        }
        if includes(options.scope, AnalysisScope::Quality) {
            quality_findings(snapshot, &mut findings);
        }
        if includes(options.scope, AnalysisScope::Performance) {
            performance_findings(snapshot, &mut findings);
        }
        findings.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then_with(|| left.rule_id.cmp(&right.rule_id))
                .then_with(|| left.object.cmp(&right.object))
        });
        let mut severity_counts = BTreeMap::new();
        for finding in &findings {
            *severity_counts
                .entry(finding.severity.label().to_owned())
                .or_insert(0) += 1;
        }
        let risk_score = risk_score(&findings);
        let overview = AnalysisOverview {
            total_findings: findings.len(),
            risk_score,
            summary: overview_summary(findings.len(), risk_score),
        };
        let sections = build_sections(&findings);
        let top_findings = findings.iter().take(5).cloned().collect();
        AnalysisReport {
            snapshot_id: snapshot.id.clone(),
            scope: options.scope,
            findings,
            severity_counts,
            overview,
            sections,
            top_findings,
            risk_score,
        }
    }
}

impl Default for AnalysisAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

fn includes(selected: AnalysisScope, target: AnalysisScope) -> bool {
    selected == AnalysisScope::All || selected == target
}

fn risk_findings(snapshot: &DbSnapshot, findings: &mut Vec<AnalysisFinding>) {
    let sensitive_columns = snapshot
        .column_profiles
        .iter()
        .filter(|profile| profile.pii_score.unwrap_or_default() >= 0.4)
        .map(|profile| profile.object_id.as_str())
        .collect::<BTreeSet<_>>();
    for profile in &snapshot.column_profiles {
        if profile.pii_score.unwrap_or_default() >= 0.4 {
            if let Some(object) = object_by_id(snapshot, &profile.object_id) {
                findings.push(finding(
                    AnalysisScope::Risk,
                    FindingSeverity::High,
                    "risk.sensitive_column",
                    object,
                    "Sensitive column detected by PII profiling",
                    format!("piiScore={}", profile.pii_score.unwrap_or_default()),
                    Vec::new(),
                ));
            }
        }
    }

    for edge in snapshot.edges.iter().filter(|edge| {
        matches!(
            edge.kind,
            DbEdgeKind::ReadsFrom | DbEdgeKind::FiltersBy | DbEdgeKind::JoinsOn
        )
    }) {
        if !sensitive_columns.contains(edge.to_object_id.as_str()) {
            continue;
        }
        let Some(column) = object_by_id(snapshot, &edge.to_object_id) else {
            continue;
        };
        findings.push(finding(
            AnalysisScope::Risk,
            FindingSeverity::High,
            "risk.query_reads_sensitive_column",
            column,
            "SQL artifact references a sensitive column",
            format!("{} edge from {}", edge.kind.as_str(), edge.from_object_id),
            related_object_names(snapshot, &[edge.from_object_id.as_str()]),
        ));
    }

    for query in snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Query)
    {
        let sql = normalized_sql(query);
        let upper = sql.to_ascii_uppercase();
        if upper.contains("SELECT *") {
            findings.push(finding(
                AnalysisScope::Risk,
                FindingSeverity::Medium,
                "risk.select_star",
                query,
                "SQL artifact selects all columns",
                preview_sql(&sql),
                Vec::new(),
            ));
        }
        if (upper.starts_with("UPDATE ") || upper.contains("; UPDATE "))
            && !statement_has_where(&upper, "UPDATE")
        {
            findings.push(finding(
                AnalysisScope::Risk,
                FindingSeverity::High,
                "risk.update_without_where",
                query,
                "SQL artifact updates rows without a WHERE clause",
                preview_sql(&sql),
                Vec::new(),
            ));
        }
        if (upper.starts_with("DELETE ") || upper.contains("; DELETE "))
            && !statement_has_where(&upper, "DELETE")
        {
            findings.push(finding(
                AnalysisScope::Risk,
                FindingSeverity::High,
                "risk.delete_without_where",
                query,
                "SQL artifact deletes rows without a WHERE clause",
                preview_sql(&sql),
                Vec::new(),
            ));
        }
    }
}

fn quality_findings(snapshot: &DbSnapshot, findings: &mut Vec<AnalysisFinding>) {
    let primary_key_tables = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::PrimaryKey)
        .filter_map(|object| object.table_name.as_deref())
        .collect::<BTreeSet<_>>();
    for table in snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Table)
    {
        if !primary_key_tables.contains(table.name.as_str())
            && !table
                .table_name
                .as_deref()
                .is_some_and(|table_name| primary_key_tables.contains(table_name))
        {
            findings.push(finding(
                AnalysisScope::Quality,
                FindingSeverity::Medium,
                "quality.missing_primary_key",
                table,
                "Table has no primary key constraint in the snapshot",
                "no primary_key object found for table".to_owned(),
                Vec::new(),
            ));
        }
    }

    let fk_columns = foreign_key_columns(snapshot);
    let reference_edge_columns = snapshot
        .edges
        .iter()
        .filter(|edge| edge.kind == DbEdgeKind::References)
        .flat_map(|edge| [&edge.from_object_id, &edge.to_object_id])
        .cloned()
        .collect::<BTreeSet<_>>();
    for column in snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Column)
    {
        let Some(column_name) = column.column_name.as_deref() else {
            continue;
        };
        if column_name == "id" || !column_name.ends_with("_id") {
            continue;
        }
        if is_local_identifier_name(column) {
            continue;
        }
        let key = table_column_key(column);
        if !fk_columns.contains(&key) && !reference_edge_columns.contains(&column.id) {
            findings.push(finding(
                AnalysisScope::Quality,
                FindingSeverity::Medium,
                "quality.probable_missing_fk",
                column,
                "Column looks like a foreign key but has no FK constraint",
                format!("column name `{column_name}` ends with _id"),
                Vec::new(),
            ));
        }
    }
}

fn performance_findings(snapshot: &DbSnapshot, findings: &mut Vec<AnalysisFinding>) {
    let indexed_columns = indexed_columns(snapshot);
    let mut seen = BTreeSet::new();
    for edge in &snapshot.edges {
        if !matches!(edge.kind, DbEdgeKind::FiltersBy | DbEdgeKind::JoinsOn) {
            continue;
        }
        let Some(column) = object_by_id(snapshot, &edge.to_object_id) else {
            continue;
        };
        if column.kind != DbObjectKind::Column {
            continue;
        }
        let key = table_column_key(column);
        if indexed_columns.contains(&key) || !seen.insert((edge.kind.as_str(), key)) {
            continue;
        }
        findings.push(finding(
            AnalysisScope::Performance,
            FindingSeverity::Medium,
            if edge.kind == DbEdgeKind::FiltersBy {
                "performance.filter_without_index"
            } else {
                "performance.join_without_index"
            },
            column,
            "SQL workload uses this column without a supporting index",
            format!("{} edge from {}", edge.kind.as_str(), edge.from_object_id),
            related_object_names(snapshot, &[edge.from_object_id.as_str()]),
        ));
    }
}

fn risk_score(findings: &[AnalysisFinding]) -> u32 {
    findings
        .iter()
        .map(|finding| match finding.severity {
            FindingSeverity::Critical => 25,
            FindingSeverity::High => 10,
            FindingSeverity::Medium => 5,
            FindingSeverity::Low => 1,
        })
        .sum()
}

fn overview_summary(total_findings: usize, risk_score: u32) -> String {
    if total_findings == 0 {
        "No risk, quality, or performance findings were detected.".to_owned()
    } else {
        format!("{total_findings} findings detected with risk score {risk_score}.")
    }
}

fn build_sections(findings: &[AnalysisFinding]) -> Vec<AnalysisSection> {
    [
        (
            "security_privacy",
            "Security & Privacy",
            "sensitive data exposure and risky SQL access patterns.",
        ),
        (
            "data_integrity_schema_quality",
            "Data Integrity & Schema Quality",
            "Schema constraints and relationship quality issues.",
        ),
        (
            "sql_workload_safety",
            "SQL Workload & Safety",
            "SQL write patterns that can affect more rows than intended.",
        ),
        (
            "performance",
            "Performance",
            "Query workload patterns that may need supporting indexes.",
        ),
    ]
    .into_iter()
    .map(|(id, title, summary)| {
        let section_findings = findings
            .iter()
            .filter(|finding| finding_belongs_to_section(finding, id));
        let mut severity_counts = BTreeMap::new();
        let mut finding_count = 0;
        for finding in section_findings {
            finding_count += 1;
            *severity_counts
                .entry(finding.severity.label().to_owned())
                .or_insert(0) += 1;
        }
        AnalysisSection {
            id: id.to_owned(),
            title: title.to_owned(),
            summary: summary.to_owned(),
            finding_count,
            severity_counts,
        }
    })
    .collect()
}

fn finding_belongs_to_section(finding: &AnalysisFinding, section_id: &str) -> bool {
    match section_id {
        "security_privacy" => {
            finding.scope == AnalysisScope::Risk
                && (finding.rule_id.contains("sensitive") || finding.rule_id == "risk.select_star")
        }
        "data_integrity_schema_quality" => finding.scope == AnalysisScope::Quality,
        "sql_workload_safety" => matches!(
            finding.rule_id.as_str(),
            "risk.update_without_where" | "risk.delete_without_where"
        ),
        "performance" => finding.scope == AnalysisScope::Performance,
        _ => false,
    }
}

fn foreign_key_columns(snapshot: &DbSnapshot) -> BTreeSet<String> {
    snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::ForeignKey)
        .flat_map(|object| {
            object
                .constraint
                .as_ref()
                .into_iter()
                .flat_map(|constraint| constraint.columns.iter())
                .map(|column| {
                    format!(
                        "{}.{}",
                        object.table_name.as_deref().unwrap_or_default(),
                        column
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn indexed_columns(snapshot: &DbSnapshot) -> BTreeSet<String> {
    let mut columns = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Index)
        .flat_map(|object| {
            object
                .index
                .as_ref()
                .into_iter()
                .flat_map(|index| index.columns.iter())
                .map(|column| {
                    format!(
                        "{}.{}",
                        object.table_name.as_deref().unwrap_or_default(),
                        column
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    columns.extend(
        snapshot
            .objects
            .iter()
            .filter(|object| {
                matches!(
                    object.kind,
                    DbObjectKind::PrimaryKey | DbObjectKind::UniqueConstraint
                )
            })
            .flat_map(|object| {
                object
                    .constraint
                    .as_ref()
                    .into_iter()
                    .flat_map(|constraint| constraint.columns.iter())
                    .map(|column| {
                        format!(
                            "{}.{}",
                            object.table_name.as_deref().unwrap_or_default(),
                            column
                        )
                    })
                    .collect::<Vec<_>>()
            }),
    );
    columns
}

fn object_by_id<'a>(snapshot: &'a DbSnapshot, id: &str) -> Option<&'a DbObject> {
    snapshot.objects.iter().find(|object| object.id == id)
}

fn related_object_names(snapshot: &DbSnapshot, ids: &[&str]) -> Vec<String> {
    ids.iter()
        .map(|id| {
            object_by_id(snapshot, id)
                .map_or_else(|| (*id).to_owned(), |object| object.full_name.clone())
        })
        .collect()
}

fn table_column_key(column: &DbObject) -> String {
    format!(
        "{}.{}",
        column.table_name.as_deref().unwrap_or_default(),
        column
            .column_name
            .as_deref()
            .unwrap_or(column.name.as_str())
    )
}

fn is_local_identifier_name(column: &DbObject) -> bool {
    let Some(column_name) = column.column_name.as_deref() else {
        return false;
    };
    let Some(prefix) = column_name.strip_suffix("_id") else {
        return false;
    };
    let Some(table_name) = column.table_name.as_deref() else {
        return false;
    };
    table_name
        .rsplit('_')
        .next()
        .is_some_and(|last_segment| singularize(last_segment) == singularize(prefix))
}

fn singularize(value: &str) -> String {
    value.strip_suffix("ies").map_or_else(
        || value.trim_end_matches('s').to_owned(),
        |prefix| format!("{prefix}y"),
    )
}

fn normalized_sql(query: &DbObject) -> String {
    query
        .metadata
        .get("normalizedSql")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn statement_has_where(sql: &str, keyword: &str) -> bool {
    sql.split(';')
        .filter(|statement| statement.trim_start().starts_with(keyword))
        .all(|statement| statement.contains(" WHERE "))
}

fn preview_sql(sql: &str) -> String {
    sql.chars().take(160).collect()
}

fn finding(
    scope: AnalysisScope,
    severity: FindingSeverity,
    rule_id: &str,
    object: &DbObject,
    message: &str,
    evidence: String,
    related_objects: Vec<String>,
) -> AnalysisFinding {
    let metadata = rule_metadata(rule_id);
    AnalysisFinding {
        scope,
        severity,
        rule_id: rule_id.to_owned(),
        object: object.full_name.clone(),
        message: message.to_owned(),
        evidence,
        title: metadata.title.to_owned(),
        description: metadata.description.to_owned(),
        impact: metadata.impact.to_owned(),
        suggested_fix: metadata.suggested_fix.to_owned(),
        confidence: metadata.confidence,
        tags: metadata.tags.iter().map(|tag| (*tag).to_owned()).collect(),
        related_objects,
    }
}

struct RuleMetadata {
    title: &'static str,
    description: &'static str,
    impact: &'static str,
    suggested_fix: &'static str,
    confidence: f64,
    tags: &'static [&'static str],
}

fn rule_metadata(rule_id: &str) -> RuleMetadata {
    match rule_id {
        "risk.sensitive_column" => RuleMetadata {
            title: "Sensitive column detected",
            description: "Column profiling indicates likely PII or secret-bearing data.",
            impact: "Uncontrolled reads can create privacy, compliance, or credential exposure risk.",
            suggested_fix: "Review access controls, mask or redact this value in downstream outputs, and avoid broad SELECT * projections.",
            confidence: 0.9,
            tags: &["risk", "pii", "privacy"],
        },
        "risk.query_reads_sensitive_column" => RuleMetadata {
            title: "SQL reads sensitive column",
            description: "A SQL artifact references a column with elevated PII score.",
            impact: "Application or analytics code may propagate sensitive data beyond its intended boundary.",
            suggested_fix: "Project only required columns, mask sensitive values where possible, and review the related SQL artifact.",
            confidence: 0.85,
            tags: &["risk", "pii", "sql"],
        },
        "risk.select_star" => RuleMetadata {
            title: "SELECT * query detected",
            description: "The SQL artifact selects all columns from at least one source.",
            impact: "Future schema changes can silently expose new columns or increase query cost.",
            suggested_fix: "Replace SELECT * with explicit column projection and exclude sensitive or unused fields.",
            confidence: 0.8,
            tags: &["risk", "sql", "projection"],
        },
        "risk.update_without_where" => RuleMetadata {
            title: "UPDATE without WHERE",
            description: "The SQL artifact contains an UPDATE statement without a WHERE clause.",
            impact: "A broad update can unintentionally modify every row in the target table.",
            suggested_fix: "Add a WHERE clause, batch limit, transaction guard, or explicit operator confirmation before execution.",
            confidence: 0.9,
            tags: &["risk", "sql", "write"],
        },
        "risk.delete_without_where" => RuleMetadata {
            title: "DELETE without WHERE",
            description: "The SQL artifact contains a DELETE statement without a WHERE clause.",
            impact: "A broad delete can unintentionally remove every row in the target table.",
            suggested_fix: "Add a WHERE clause, batch limit, transaction guard, or explicit operator confirmation before execution.",
            confidence: 0.9,
            tags: &["risk", "sql", "write"],
        },
        "quality.missing_primary_key" => RuleMetadata {
            title: "Missing primary key",
            description: "The table has no primary key constraint in the snapshot.",
            impact: "Rows may be hard to address reliably, and downstream sync or update logic can become ambiguous.",
            suggested_fix: "Add a primary key, or document the table as append-only, staging, or intentionally keyless.",
            confidence: 0.85,
            tags: &["quality", "constraint", "primary_key"],
        },
        "quality.probable_missing_fk" => RuleMetadata {
            title: "Probable missing foreign key",
            description: "A column name looks like a foreign key but no FK constraint or reference edge was found.",
            impact: "Relationship integrity may rely on application code and can drift over time.",
            suggested_fix: "Add an explicit foreign key if the relationship is required, or document the denormalized design choice.",
            confidence: 0.7,
            tags: &["quality", "constraint", "foreign_key"],
        },
        "performance.filter_without_index" => RuleMetadata {
            title: "Filter without supporting index",
            description: "SQL workload filters by this column but no supporting index was found in the snapshot.",
            impact: "Frequent filters can degrade into table scans as data volume grows.",
            suggested_fix: "Validate selectivity with EXPLAIN, then consider CREATE INDEX CONCURRENTLY on the filtered column for Postgres.",
            confidence: 0.75,
            tags: &["performance", "index", "filter"],
        },
        "performance.join_without_index" => RuleMetadata {
            title: "Join without supporting index",
            description: "SQL workload joins on this column but no supporting index was found in the snapshot.",
            impact: "Join-heavy paths can become slower as related tables grow.",
            suggested_fix: "Validate join cardinality with EXPLAIN, then consider CREATE INDEX CONCURRENTLY on the join column for Postgres.",
            confidence: 0.75,
            tags: &["performance", "index", "join"],
        },
        _ => RuleMetadata {
            title: "Analysis finding",
            description: "DbGraph detected a database graph condition that needs review.",
            impact: "The finding may affect data safety, quality, or performance.",
            suggested_fix: "Review the object, evidence, and related schema or SQL artifacts before changing production systems.",
            confidence: 0.5,
            tags: &["analysis"],
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::analysis::{AnalysisAnalyzer, AnalysisOptions, AnalysisScope, FindingSeverity};
    use dbgraph_core::model::{
        ColumnMetadata, ColumnProfile, ConstraintMetadata, DbEdge, DbEdgeKind, DbObject,
        DbObjectKind, DbSnapshot, IndexMetadata,
    };

    #[test]
    fn analysis_reports_risk_quality_and_performance_findings() {
        let snapshot = sample_snapshot();

        let report = AnalysisAnalyzer::new().analyze(&snapshot, &AnalysisOptions::default());

        assert!(report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "risk.sensitive_column"
                && finding.object == "public.customers.email"
                && finding.severity == FindingSeverity::High));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "risk.select_star"
                && finding.object.contains("sql/orders.sql")));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "quality.missing_primary_key"
                && finding.object == "public.audit_events"));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "quality.probable_missing_fk"
                && finding.object == "public.orders.customer_id"));
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "quality.probable_missing_fk"
                && finding.object == "public.audit_events.event_id"));
        assert!(report.findings.iter().any(|finding| finding.rule_id
            == "risk.query_reads_sensitive_column"
            && finding.object == "public.customers.email"));
        assert!(report.findings.iter().any(|finding| finding.rule_id
            == "performance.filter_without_index"
            && finding.object == "public.orders.status"));
        assert!(!report.findings.iter().any(|finding| finding.rule_id
            == "performance.join_without_index"
            && finding.object == "public.customers.id"));
        assert!(report
            .findings
            .windows(2)
            .all(|pair| pair[0].severity >= pair[1].severity));
        assert_eq!(report.overview.total_findings, report.findings.len());
        assert!(report.overview.risk_score > 0);
        assert!(report
            .sections
            .iter()
            .any(|section| section.id == "security_privacy"
                && section.finding_count > 0
                && section.summary.contains("sensitive")));
        assert!(report
            .sections
            .iter()
            .any(|section| section.id == "data_integrity_schema_quality"
                && section.finding_count > 0));
        assert!(report
            .sections
            .iter()
            .any(|section| section.id == "performance" && section.finding_count > 0));
        assert!(report.top_findings.len() <= 5);

        let sensitive = report
            .findings
            .iter()
            .find(|finding| {
                finding.rule_id == "risk.sensitive_column"
                    && finding.object == "public.customers.email"
            })
            .expect("sensitive finding should exist");
        assert_eq!(sensitive.title, "Sensitive column detected");
        assert!(sensitive.description.contains("PII"));
        assert!(sensitive.impact.contains("privacy"));
        assert!(sensitive.suggested_fix.contains("mask"));
        assert!(sensitive.confidence >= 0.8);
        assert!(sensitive.tags.contains(&"pii".to_owned()));

        let sql_sensitive = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "risk.query_reads_sensitive_column")
            .expect("SQL sensitive reference should exist");
        assert!(sql_sensitive
            .related_objects
            .contains(&"sql.sql/orders.sql:fingerprint".to_owned()));

        let missing_index = report
            .findings
            .iter()
            .find(|finding| finding.rule_id == "performance.filter_without_index")
            .expect("missing index finding should exist");
        assert!(missing_index
            .suggested_fix
            .contains("CREATE INDEX CONCURRENTLY"));
    }

    #[test]
    fn analysis_scope_filters_findings() {
        let snapshot = sample_snapshot();

        let report = AnalysisAnalyzer::new().analyze(
            &snapshot,
            &AnalysisOptions {
                scope: AnalysisScope::Quality,
            },
        );

        assert!(!report.findings.is_empty());
        assert!(report
            .findings
            .iter()
            .all(|finding| finding.scope == AnalysisScope::Quality));
    }

    #[allow(clippy::too_many_lines)]
    fn sample_snapshot() -> DbSnapshot {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "teashop", 1);
        snapshot
            .objects
            .push(table("table:customers", "public.customers", "customers"));
        snapshot
            .objects
            .push(table("table:orders", "public.orders", "orders"));
        snapshot.objects.push(table(
            "table:audit_events",
            "public.audit_events",
            "audit_events",
        ));
        snapshot.objects.push(column(
            "column:customers.id",
            "public.customers.id",
            "customers",
            "id",
            "bigint",
        ));
        snapshot.objects.push(column(
            "column:customers.email",
            "public.customers.email",
            "customers",
            "email",
            "text",
        ));
        snapshot.objects.push(column(
            "column:orders.id",
            "public.orders.id",
            "orders",
            "id",
            "bigint",
        ));
        snapshot.objects.push(column(
            "column:orders.customer_id",
            "public.orders.customer_id",
            "orders",
            "customer_id",
            "bigint",
        ));
        snapshot.objects.push(column(
            "column:audit_events.event_id",
            "public.audit_events.event_id",
            "audit_events",
            "event_id",
            "bigint",
        ));
        snapshot.objects.push(column(
            "column:orders.status",
            "public.orders.status",
            "orders",
            "status",
            "text",
        ));
        snapshot.objects.push(primary_key(
            "pk:customers",
            "public.customers_pkey",
            "customers",
        ));
        snapshot
            .objects
            .push(primary_key("pk:orders", "public.orders_pkey", "orders"));
        snapshot.objects.push(index(
            "index:orders.customer_id",
            "public.idx_orders_customer_id",
            "orders",
            &["customer_id"],
        ));
        let mut query = DbObject::new(
            "query:orders",
            DbObjectKind::Query,
            "sql.sql/orders.sql:fingerprint",
        );
        query.metadata.insert(
            "normalizedSql".to_owned(),
            serde_json::Value::String(
                "SELECT * FROM orders WHERE status = 'paid'; UPDATE orders SET status = 'x'"
                    .to_owned(),
            ),
        );
        snapshot.objects.push(query);
        snapshot.edges.push(DbEdge::explicit(
            "edge:query.orders",
            DbEdgeKind::ReadsFrom,
            "query:orders",
            "table:orders",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:query.email",
            DbEdgeKind::ReadsFrom,
            "query:orders",
            "column:customers.email",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:query.customer_id",
            DbEdgeKind::JoinsOn,
            "query:orders",
            "column:customers.id",
        ));
        snapshot.edges.push(DbEdge::explicit(
            "edge:query.status",
            DbEdgeKind::FiltersBy,
            "query:orders",
            "column:orders.status",
        ));
        snapshot.column_profiles.push(ColumnProfile {
            object_id: "column:customers.email".to_owned(),
            data_type_family: Some("text".to_owned()),
            null_fraction: None,
            distinct_estimate: None,
            pii_score: Some(0.9),
            profile: dbgraph_core::model::Metadata::new(),
        });
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
        data_type: &str,
    ) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Column, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.column_name = Some(column_name.to_owned());
        object.column = Some(ColumnMetadata {
            data_type: Some(data_type.to_owned()),
            data_type_family: Some(data_type.to_owned()),
            nullable: Some(false),
            default: None,
            comment: None,
        });
        object
    }

    fn primary_key(id: &str, full_name: &str, table_name: &str) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::PrimaryKey, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.constraint = Some(ConstraintMetadata {
            columns: vec!["id".to_owned()],
            referenced_table: None,
            referenced_columns: Vec::new(),
        });
        object
    }

    fn index(id: &str, full_name: &str, table_name: &str, columns: &[&str]) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Index, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.index = Some(IndexMetadata {
            unique: Some(false),
            columns: columns.iter().map(ToString::to_string).collect(),
            expression: None,
        });
        object
    }
}
