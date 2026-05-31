//! SQL parsing, source scanning, lineage extraction, and graph artifact helpers.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use dbgraph_core::model::{DbEdge, DbEdgeKind, DbObject, DbObjectKind, Evidence, Metadata};
use dbgraph_core::{DbGraphError, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlparser::ast::{
    Delete, Expr, FromTable, GroupByExpr, Ident, JoinConstraint, JoinOperator, ObjectName, Query,
    Select, SetExpr, Statement, TableFactor, TableWithJoins, UpdateTableFromKind,
};
use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use walkdir::{DirEntry, WalkDir};

/// SQL dialect used for parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqlDialect {
    /// `PostgreSQL` dialect.
    Postgres,
    /// `MySQL` dialect.
    MySql,
    /// Generic ANSI-ish dialect.
    Generic,
}

impl SqlDialect {
    /// Returns the stable storage string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Generic => "generic",
        }
    }
}

impl std::str::FromStr for SqlDialect {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => Ok(Self::Postgres),
            "mysql" => Ok(Self::MySql),
            "generic" | "ansi" => Ok(Self::Generic),
            _ => Err(format!("unsupported SQL dialect `{value}`")),
        }
    }
}

/// Parse status for a SQL artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    /// SQL parsed into at least one statement.
    Parsed,
    /// SQL failed to parse; diagnostics are preserved.
    Failed,
}

/// Diagnostic emitted by the parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseDiagnostic {
    /// Human-readable parser message.
    pub message: String,
}

/// Parsed statement representation that is stable to serialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedStatement {
    /// Statement kind, such as select, insert, update, delete, or other.
    pub kind: String,
    /// Canonical SQL emitted by `sqlparser-rs`.
    pub sql: String,
}

/// SQL parser result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseResult {
    /// Parse status.
    pub status: ParseStatus,
    /// Original SQL text.
    pub raw_sql: String,
    /// Normalized SQL text.
    pub normalized_sql: String,
    /// Stable SHA-256 fingerprint of normalized SQL.
    pub fingerprint: String,
    /// Parsed statements.
    pub statements: Vec<ParsedStatement>,
    /// Parse diagnostics.
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// SQL parser facade.
#[derive(Debug, Clone, Copy)]
pub struct SqlParser {
    dialect: SqlDialect,
}

impl SqlParser {
    /// Creates a parser for the selected dialect.
    #[must_use]
    pub const fn new(dialect: SqlDialect) -> Self {
        Self { dialect }
    }

    /// Parses SQL while preserving diagnostics on failure.
    ///
    /// # Errors
    ///
    /// Returns only internal serialization/hash errors; SQL syntax errors are encoded in
    /// [`ParseResult::diagnostics`].
    pub fn parse(self, sql: &str) -> Result<ParseResult> {
        parse_sql(sql, self.dialect)
    }
}

/// Source SQL artifact found in a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlArtifactSource {
    /// Path relative to project root.
    pub source_path: PathBuf,
    /// Source kind. Phase 04 scans files only.
    pub source_kind: String,
    /// Raw SQL text.
    pub raw_sql: String,
}

/// SQL scanner options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    /// Include globs relative to project root.
    pub include: Vec<String>,
    /// Exclude globs relative to project root.
    pub exclude: Vec<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            include: vec![
                "migrations/**/*.sql".to_owned(),
                "sql/**/*.sql".to_owned(),
                "db/**/*.sql".to_owned(),
            ],
            exclude: default_exclude_globs(),
        }
    }
}

/// Lineage reference discovered in a SQL statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageReference {
    /// Graph edge kind represented by this reference.
    pub kind: DbEdgeKind,
    /// Referenced table/column/CTE name.
    pub object_name: String,
    /// Optional table alias used in SQL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Whether the reference was resolved to a known table alias.
    pub resolved: bool,
}

/// Lineage analysis for a parsed SQL artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageAnalysis {
    /// SQL dialect used.
    pub dialect: SqlDialect,
    /// Parse diagnostics, if any.
    pub diagnostics: Vec<ParseDiagnostic>,
    /// Extracted references.
    pub references: Vec<LineageReference>,
}

/// SQL artifact row that can be persisted by storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlArtifactRecord {
    /// Artifact id.
    pub id: String,
    /// Snapshot id.
    pub snapshot_id: String,
    /// Source kind.
    pub source_kind: String,
    /// Source path.
    pub source_path: String,
    /// SQL dialect.
    pub dialect: String,
    /// SQL fingerprint.
    pub fingerprint: String,
    /// Normalized SQL.
    pub normalized_sql: String,
    /// Serialized AST summary.
    pub ast_json: String,
    /// Serialized lineage analysis.
    pub analysis_json: String,
}

/// Graph objects/edges derived from a SQL artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlArtifactGraph {
    /// Query object inserted into the graph index.
    pub object: DbObject,
    /// SQL artifact row.
    pub artifact: SqlArtifactRecord,
    /// Conservative unresolved lineage edges.
    pub edges: Vec<DbEdge>,
}

/// Parses SQL using the requested dialect.
///
/// # Errors
///
/// Returns an internal error if the parse result cannot be serialized or hashed.
pub fn parse_sql(sql: &str, dialect: SqlDialect) -> Result<ParseResult> {
    let parsed = match dialect {
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        SqlDialect::MySql => Parser::parse_sql(&MySqlDialect {}, sql),
        SqlDialect::Generic => Parser::parse_sql(&GenericDialect {}, sql),
    };
    match parsed {
        Ok(statements) => {
            let statements = statements
                .into_iter()
                .map(|statement| ParsedStatement {
                    kind: statement_kind(&statement).to_owned(),
                    sql: compact_whitespace(&statement.to_string()),
                })
                .collect::<Vec<_>>();
            let normalized_sql = statements
                .iter()
                .map(|statement| statement.sql.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            Ok(ParseResult {
                status: ParseStatus::Parsed,
                raw_sql: sql.to_owned(),
                fingerprint: fingerprint(&normalized_sql),
                normalized_sql,
                statements,
                diagnostics: Vec::new(),
            })
        }
        Err(source) => {
            let normalized_sql = compact_whitespace(sql);
            Ok(ParseResult {
                status: ParseStatus::Failed,
                raw_sql: sql.to_owned(),
                fingerprint: fingerprint(&normalized_sql),
                normalized_sql,
                statements: Vec::new(),
                diagnostics: vec![ParseDiagnostic {
                    message: source.to_string(),
                }],
            })
        }
    }
}

/// Scans project SQL files in stable order.
///
/// # Errors
///
/// Returns an error when globs are invalid or a matched file cannot be read.
pub fn scan_sql_files(
    project_root: impl AsRef<Path>,
    options: &ScanOptions,
) -> Result<Vec<SqlArtifactSource>> {
    let project_root = project_root.as_ref();
    let include = compile_globs(&options.include)?;
    let exclude = compile_globs(&options.exclude)?;
    let mut artifacts = Vec::new();

    for entry in WalkDir::new(project_root)
        .into_iter()
        .filter_entry(should_descend)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let absolute = entry.path();
        if absolute
            .extension()
            .map_or(true, |extension| extension != "sql")
        {
            continue;
        }
        let relative = absolute
            .strip_prefix(project_root)
            .unwrap_or(absolute)
            .to_path_buf();
        if exclude.is_match(&relative) || !include.is_match(&relative) {
            continue;
        }
        let raw_sql =
            fs::read_to_string(absolute).map_err(|source| DbGraphError::io(absolute, source))?;
        artifacts.push(SqlArtifactSource {
            source_path: relative,
            source_kind: "file".to_owned(),
            raw_sql,
        });
    }

    artifacts.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    Ok(artifacts)
}

/// Parses SQL and extracts conservative lineage.
///
/// # Errors
///
/// Returns an error if parsing cannot be attempted.
pub fn analyze_sql(sql: &str, dialect: SqlDialect) -> Result<LineageAnalysis> {
    let parsed = parse_sql(sql, dialect)?;
    if parsed.status == ParseStatus::Failed {
        return Ok(LineageAnalysis {
            dialect,
            diagnostics: parsed.diagnostics,
            references: Vec::new(),
        });
    }
    let mut analysis = LineageBuilder::default();
    let statements = parse_statements_for_analysis(sql, dialect)?;
    for statement in &statements {
        analysis.visit_statement(statement);
    }
    Ok(LineageAnalysis {
        dialect,
        diagnostics: Vec::new(),
        references: analysis.finish(),
    })
}

/// Builds graph/storage records for one SQL artifact.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub fn sql_artifact_to_graph(
    snapshot_id: &str,
    source_path: &str,
    parsed: &ParseResult,
    analysis: &LineageAnalysis,
) -> Result<SqlArtifactGraph> {
    let object_id = format!("query:{}", parsed.fingerprint);
    let mut object = DbObject::new(
        object_id.clone(),
        DbObjectKind::Query,
        format!("sql.{source_path}:{}", parsed.fingerprint),
    );
    object.metadata = Metadata::from([
        (
            "sourcePath".to_owned(),
            serde_json::Value::String(source_path.to_owned()),
        ),
        (
            "normalizedSql".to_owned(),
            serde_json::Value::String(parsed.normalized_sql.clone()),
        ),
        (
            "fingerprint".to_owned(),
            serde_json::Value::String(parsed.fingerprint.clone()),
        ),
    ]);

    let mut edges = Vec::new();
    for (idx, reference) in analysis.references.iter().enumerate() {
        edges.push(DbEdge {
            id: format!(
                "sql:{}:{}:{idx}",
                parsed.fingerprint,
                reference.kind.as_str()
            ),
            kind: reference.kind,
            from_object_id: object_id.clone(),
            to_object_id: reference.object_name.clone(),
            confidence: if reference.resolved { 0.9 } else { 0.45 },
            evidence: vec![Evidence {
                source: "sqlparser".to_owned(),
                detail: format!("{} {}", reference.kind.as_str(), reference.object_name),
            }],
            metadata: Metadata::from([(
                "unresolved".to_owned(),
                serde_json::Value::Bool(!reference.resolved),
            )]),
        });
    }

    Ok(SqlArtifactGraph {
        object,
        artifact: SqlArtifactRecord {
            id: format!("sql:{}", parsed.fingerprint),
            snapshot_id: snapshot_id.to_owned(),
            source_kind: "file".to_owned(),
            source_path: source_path.to_owned(),
            dialect: analysis.dialect.as_str().to_owned(),
            fingerprint: parsed.fingerprint.clone(),
            normalized_sql: parsed.normalized_sql.clone(),
            ast_json: serde_json::to_string(&parsed.statements).map_err(json_error)?,
            analysis_json: serde_json::to_string(analysis).map_err(json_error)?,
        },
        edges,
    })
}

/// Resolves SQL artifact edge targets against known snapshot objects.
pub fn resolve_sql_edge_targets(snapshot: &dbgraph_core::model::DbSnapshot, edges: &mut [DbEdge]) {
    for edge in edges {
        if let Some(target) = resolve_reference(snapshot, &edge.to_object_id) {
            edge.to_object_id = target.id.clone();
            edge.confidence = edge.confidence.max(0.9);
            edge.metadata
                .insert("unresolved".to_owned(), serde_json::Value::Bool(false));
            edge.metadata.insert(
                "resolvedName".to_owned(),
                serde_json::Value::String(target.full_name.clone()),
            );
        }
    }
}

fn resolve_reference<'a>(
    snapshot: &'a dbgraph_core::model::DbSnapshot,
    reference: &str,
) -> Option<&'a DbObject> {
    let normalized = reference.trim_matches('"').trim_matches('`');
    snapshot
        .objects
        .iter()
        .find(|object| object.id == normalized || object.full_name == normalized)
        .or_else(|| resolve_column_reference(snapshot, normalized))
        .or_else(|| resolve_table_reference(snapshot, normalized))
}

fn resolve_column_reference<'a>(
    snapshot: &'a dbgraph_core::model::DbSnapshot,
    reference: &str,
) -> Option<&'a DbObject> {
    let parts = reference.split('.').collect::<Vec<_>>();
    match parts.as_slice() {
        [table, column] => snapshot.objects.iter().find(|object| {
            object.kind == DbObjectKind::Column
                && object.table_name.as_deref() == Some(*table)
                && object
                    .column_name
                    .as_deref()
                    .unwrap_or(object.name.as_str())
                    == *column
        }),
        [schema, table, column] => snapshot.objects.iter().find(|object| {
            object.kind == DbObjectKind::Column
                && object.schema_name.as_deref() == Some(*schema)
                && object.table_name.as_deref() == Some(*table)
                && object
                    .column_name
                    .as_deref()
                    .unwrap_or(object.name.as_str())
                    == *column
        }),
        _ => None,
    }
}

fn resolve_table_reference<'a>(
    snapshot: &'a dbgraph_core::model::DbSnapshot,
    reference: &str,
) -> Option<&'a DbObject> {
    let parts = reference.split('.').collect::<Vec<_>>();
    match parts.as_slice() {
        [table] => snapshot.objects.iter().find(|object| {
            matches!(object.kind, DbObjectKind::Table | DbObjectKind::View)
                && object.table_name.as_deref().unwrap_or(object.name.as_str()) == *table
        }),
        [schema, table] => snapshot.objects.iter().find(|object| {
            matches!(object.kind, DbObjectKind::Table | DbObjectKind::View)
                && object.schema_name.as_deref() == Some(*schema)
                && object.table_name.as_deref().unwrap_or(object.name.as_str()) == *table
        }),
        _ => None,
    }
}

fn parse_statements_for_analysis(sql: &str, dialect: SqlDialect) -> Result<Vec<Statement>> {
    let parsed = match dialect {
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        SqlDialect::MySql => Parser::parse_sql(&MySqlDialect {}, sql),
        SqlDialect::Generic => Parser::parse_sql(&GenericDialect {}, sql),
    };
    parsed.map_err(|source| DbGraphError::invalid_argument(source.to_string()))
}

#[derive(Default)]
struct LineageBuilder {
    refs: Vec<LineageReference>,
    aliases: HashMap<String, String>,
    ctes: BTreeSet<String>,
}

impl LineageBuilder {
    fn visit_statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Query(query) => self.visit_query(query),
            Statement::Insert(insert) => {
                self.push(DbEdgeKind::WritesTo, &insert.table.to_string(), None, true);
                if let Some(source) = insert.source.as_deref() {
                    self.visit_query(source);
                }
            }
            Statement::Update {
                table,
                selection,
                from,
                ..
            } => {
                self.visit_table_with_joins(table, DbEdgeKind::WritesTo);
                if let Some(selection) = selection {
                    self.visit_expr(selection, DbEdgeKind::FiltersBy);
                }
                if let Some(from) = from {
                    for table in update_from_tables(from) {
                        self.visit_table_with_joins(table, DbEdgeKind::ReadsFrom);
                    }
                }
            }
            Statement::Delete(delete) => self.visit_delete(delete),
            _ => {}
        }
    }

    fn visit_query(&mut self, query: &Query) {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let alias = cte.alias.name.value.clone();
                self.ctes.insert(alias.clone());
                self.push(DbEdgeKind::DependsOn, &alias, None, true);
                self.visit_query(&cte.query);
            }
        }
        self.visit_set_expr(&query.body);
        if let Some(order_by) = &query.order_by {
            if let sqlparser::ast::OrderByKind::Expressions(expressions) = &order_by.kind {
                for expression in expressions {
                    self.visit_expr(&expression.expr, DbEdgeKind::OrdersBy);
                }
            }
        }
    }

    fn visit_set_expr(&mut self, expr: &SetExpr) {
        match expr {
            SetExpr::Select(select) => self.visit_select(select),
            SetExpr::Query(query) => self.visit_query(query),
            SetExpr::SetOperation { left, right, .. } => {
                self.visit_set_expr(left);
                self.visit_set_expr(right);
            }
            SetExpr::Insert(statement)
            | SetExpr::Update(statement)
            | SetExpr::Delete(statement) => {
                self.visit_statement(statement);
            }
            _ => {}
        }
    }

    fn visit_select(&mut self, select: &Select) {
        for table in &select.from {
            self.visit_table_with_joins(table, DbEdgeKind::ReadsFrom);
        }
        if let Some(selection) = &select.selection {
            self.visit_expr(selection, DbEdgeKind::FiltersBy);
        }
        match &select.group_by {
            GroupByExpr::Expressions(expressions, _) => {
                for expression in expressions {
                    self.visit_expr(expression, DbEdgeKind::GroupsBy);
                }
            }
            GroupByExpr::All(_) => {}
        }
        for expression in &select.sort_by {
            self.visit_expr(&expression.expr, DbEdgeKind::OrdersBy);
        }
        if let Some(having) = &select.having {
            self.visit_expr(having, DbEdgeKind::FiltersBy);
        }
    }

    fn visit_delete(&mut self, delete: &Delete) {
        for table in from_tables(&delete.from) {
            self.visit_table_with_joins(table, DbEdgeKind::WritesTo);
        }
        if let Some(using) = &delete.using {
            for table in using {
                self.visit_table_with_joins(table, DbEdgeKind::ReadsFrom);
            }
        }
        if let Some(selection) = &delete.selection {
            self.visit_expr(selection, DbEdgeKind::FiltersBy);
        }
    }

    fn visit_table_with_joins(&mut self, table: &TableWithJoins, base_kind: DbEdgeKind) {
        self.visit_table_factor(&table.relation, base_kind);
        for join in &table.joins {
            self.visit_table_factor(&join.relation, DbEdgeKind::JoinsOn);
            match join_constraint(&join.join_operator) {
                Some(JoinConstraint::On(expr)) => self.visit_expr(expr, DbEdgeKind::JoinsOn),
                Some(JoinConstraint::Using(columns)) => {
                    for column in columns {
                        self.push(DbEdgeKind::JoinsOn, &object_name(column), None, false);
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_table_factor(&mut self, factor: &TableFactor, kind: DbEdgeKind) {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let table_name = object_name(name);
                if let Some(alias) = alias {
                    self.aliases
                        .insert(alias.name.value.clone(), table_name.clone());
                }
                let is_cte = self.ctes.contains(&table_name);
                self.push(
                    kind,
                    &table_name,
                    alias.as_ref().map(|alias| alias.name.value.clone()),
                    !is_cte,
                );
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                if let Some(alias) = alias {
                    self.push(DbEdgeKind::DependsOn, &alias.name.value, None, true);
                }
                self.visit_query(subquery);
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.visit_table_with_joins(table_with_joins, kind);
            }
            _ => self.visit_display_refs(factor, kind),
        }
    }

    fn visit_expr(&mut self, expr: &Expr, kind: DbEdgeKind) {
        match expr {
            Expr::Identifier(ident) => {
                self.push(kind, &ident.value, None, false);
            }
            Expr::CompoundIdentifier(parts) => self.visit_compound_identifier(parts, kind),
            Expr::BinaryOp { left, right, .. } => {
                self.visit_expr(left, kind);
                self.visit_expr(right, kind);
            }
            Expr::Nested(inner) | Expr::UnaryOp { expr: inner, .. } => self.visit_expr(inner, kind),
            Expr::Between {
                expr, low, high, ..
            } => {
                self.visit_expr(expr, kind);
                self.visit_expr(low, kind);
                self.visit_expr(high, kind);
            }
            Expr::InList { expr, list, .. } => {
                self.visit_expr(expr, kind);
                for item in list {
                    self.visit_expr(item, kind);
                }
            }
            Expr::IsNull(expr) | Expr::IsNotNull(expr) => self.visit_expr(expr, kind),
            _ => {}
        }
    }

    fn visit_compound_identifier(&mut self, parts: &[Ident], kind: DbEdgeKind) {
        let values = parts
            .iter()
            .map(|part| part.value.clone())
            .collect::<Vec<_>>();
        match values.as_slice() {
            [alias, column] => {
                if let Some(table) = self.aliases.get(alias).cloned() {
                    self.push(
                        kind,
                        &format!("{table}.{column}"),
                        Some(alias.clone()),
                        true,
                    );
                } else {
                    self.push(kind, &values.join("."), Some(alias.clone()), false);
                }
            }
            _ => self.push(kind, &values.join("."), None, false),
        }
    }

    fn visit_display_refs(&mut self, value: &impl std::fmt::Display, kind: DbEdgeKind) {
        let text = value.to_string();
        for token in identifier_tokens(&text) {
            self.push(kind, &token, None, false);
        }
    }

    fn push(&mut self, kind: DbEdgeKind, object_name: &str, alias: Option<String>, resolved: bool) {
        let object_name = trim_identifier(object_name);
        if object_name.is_empty() || is_literal_or_keyword(&object_name) {
            return;
        }
        self.refs.push(LineageReference {
            kind,
            object_name,
            alias,
            resolved,
        });
    }

    fn finish(self) -> Vec<LineageReference> {
        let mut seen = BTreeSet::new();
        let mut refs = Vec::new();
        for reference in self.refs {
            let key = (
                reference.kind.as_str().to_owned(),
                reference.object_name.clone(),
                reference.alias.clone(),
                reference.resolved,
            );
            if seen.insert(key) {
                refs.push(reference);
            }
        }
        refs
    }
}

fn join_constraint(operator: &JoinOperator) -> Option<&JoinConstraint> {
    match operator {
        JoinOperator::Join(value)
        | JoinOperator::Inner(value)
        | JoinOperator::Left(value)
        | JoinOperator::LeftOuter(value)
        | JoinOperator::Right(value)
        | JoinOperator::RightOuter(value)
        | JoinOperator::FullOuter(value)
        | JoinOperator::CrossJoin(value)
        | JoinOperator::Semi(value)
        | JoinOperator::LeftSemi(value)
        | JoinOperator::RightSemi(value)
        | JoinOperator::Anti(value)
        | JoinOperator::LeftAnti(value)
        | JoinOperator::RightAnti(value)
        | JoinOperator::StraightJoin(value) => Some(value),
        JoinOperator::AsOf { constraint, .. } => Some(constraint),
        JoinOperator::CrossApply | JoinOperator::OuterApply => None,
    }
}

fn from_tables(from: &FromTable) -> &[TableWithJoins] {
    match from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
    }
}

fn update_from_tables(from: &UpdateTableFromKind) -> &[TableWithJoins] {
    match from {
        UpdateTableFromKind::BeforeSet(tables) | UpdateTableFromKind::AfterSet(tables) => tables,
    }
}

fn object_name(name: &ObjectName) -> String {
    name.to_string()
}

fn statement_kind(statement: &Statement) -> &'static str {
    match statement {
        Statement::Query(_) => "select",
        Statement::Insert(_) => "insert",
        Statement::Update { .. } => "update",
        Statement::Delete(_) => "delete",
        _ => "other",
    }
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut out, "{byte:02x}").expect("write to String cannot fail");
    }
    out
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|source| {
            DbGraphError::invalid_argument(format!("invalid glob `{pattern}`: {source}"))
        })?);
    }
    builder.build().map_err(|source| {
        DbGraphError::invalid_argument(format!("failed to build glob set: {source}"))
    })
}

fn should_descend(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    !matches!(
        name,
        "node_modules" | "target" | "bin" | "obj" | ".git" | ".dbgraph" | "dist" | "build"
    )
}

fn default_exclude_globs() -> Vec<String> {
    [
        "node_modules/**",
        "target/**",
        "bin/**",
        "obj/**",
        "dist/**",
        "build/**",
        ".git/**",
        ".dbgraph/**",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

fn trim_identifier(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .trim_matches('\'')
        .to_owned()
}

fn identifier_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
        .map(trim_identifier)
        .filter(|token| !token.is_empty() && !is_literal_or_keyword(token))
        .collect()
}

fn is_literal_or_keyword(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.parse::<f64>().is_ok()
        || matches!(
            lower.as_str(),
            "select"
                | "from"
                | "where"
                | "join"
                | "inner"
                | "left"
                | "right"
                | "outer"
                | "full"
                | "on"
                | "and"
                | "or"
                | "as"
                | "group"
                | "by"
                | "order"
                | "desc"
                | "asc"
                | "update"
                | "insert"
                | "delete"
                | "set"
                | "into"
                | "values"
                | "with"
                | "paid"
        )
}

#[allow(clippy::needless_pass_by_value)]
fn json_error(source: serde_json::Error) -> DbGraphError {
    DbGraphError::Internal {
        message: format!("JSON serialization error: {source}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use dbgraph_core::model::{ColumnMetadata, DbEdgeKind, DbObject, DbObjectKind, DbSnapshot};
    use tempfile::TempDir;

    use crate::{
        analyze_sql, scan_sql_files, sql_artifact_to_graph, ParseStatus, ScanOptions, SqlDialect,
        SqlParser,
    };

    #[test]
    fn parser_preserves_raw_normalized_ast_and_diagnostics() {
        let parser = SqlParser::new(SqlDialect::Postgres);

        let ok = parser
            .parse(" select * from public.users where id = 1 ")
            .unwrap();
        let bad = parser.parse("select from").unwrap();

        assert_eq!(ok.status, ParseStatus::Parsed);
        assert_eq!(ok.raw_sql, " select * from public.users where id = 1 ");
        assert_eq!(ok.normalized_sql, "SELECT * FROM public.users WHERE id = 1");
        assert!(!ok.statements.is_empty());
        assert!(ok.diagnostics.is_empty());
        assert_eq!(bad.status, ParseStatus::Failed);
        assert!(!bad.diagnostics.is_empty());
        assert_eq!(bad.raw_sql, "select from");
    }

    #[test]
    fn parser_accepts_common_dml_statements() {
        let parser = SqlParser::new(SqlDialect::Postgres);

        for (sql, kind) in [
            ("select * from users", "select"),
            ("insert into users(id) values (1)", "insert"),
            ("update users set id = 2 where id = 1", "update"),
            ("delete from users where id = 1", "delete"),
        ] {
            let parsed = parser.parse(sql).unwrap();

            assert_eq!(parsed.status, ParseStatus::Parsed);
            assert_eq!(parsed.statements[0].kind, kind);
        }
    }

    #[test]
    fn scanner_uses_project_sql_roots_ignores_noise_and_sorts_results() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path().join("migrations/002.sql"),
            "select * from orders;",
        );
        write(temp.path().join("sql/001.sql"), "select * from users;");
        write(
            temp.path().join("db/schema.sql"),
            "create table users(id int);",
        );
        write(
            temp.path().join("target/ignored.sql"),
            "select * from target_table;",
        );
        write(
            temp.path().join("node_modules/pkg/ignored.sql"),
            "select * from packages;",
        );

        let artifacts = scan_sql_files(temp.path(), &ScanOptions::default()).unwrap();
        let paths = artifacts
            .iter()
            .map(|artifact| artifact.source_path.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec!["db/schema.sql", "migrations/002.sql", "sql/001.sql"]
        );
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.source_kind == "file"));
    }

    #[test]
    fn lineage_extracts_join_reads_writes_clauses_and_unresolved_columns() {
        let sql = "\
            WITH recent AS (
                SELECT user_id, max(created_at) AS newest FROM orders GROUP BY user_id
            )
            SELECT u.id, o.total
            FROM public.users u
            JOIN orders o ON u.id = o.user_id
            WHERE o.status = 'paid'
            GROUP BY u.id, o.total
            ORDER BY o.total DESC";

        let analysis = analyze_sql(sql, SqlDialect::Postgres).unwrap();
        let kinds = analysis
            .references
            .iter()
            .map(|reference| reference.kind)
            .collect::<Vec<_>>();

        assert!(analysis
            .references
            .iter()
            .any(|reference| reference.kind == DbEdgeKind::ReadsFrom
                && reference.object_name == "public.users"));
        assert!(analysis
            .references
            .iter()
            .any(|reference| reference.kind == DbEdgeKind::ReadsFrom
                && reference.object_name == "orders"));
        assert!(kinds.contains(&DbEdgeKind::JoinsOn));
        assert!(kinds.contains(&DbEdgeKind::FiltersBy));
        assert!(kinds.contains(&DbEdgeKind::GroupsBy));
        assert!(kinds.contains(&DbEdgeKind::OrdersBy));
        assert!(kinds.contains(&DbEdgeKind::DependsOn));
        assert!(analysis
            .references
            .iter()
            .any(|reference| reference.object_name == "user_id" && !reference.resolved));

        let write = analyze_sql(
            "update orders set status = 'paid' where id = 1",
            SqlDialect::Postgres,
        )
        .unwrap();
        assert!(write.references.iter().any(|reference| {
            reference.kind == DbEdgeKind::WritesTo && reference.object_name == "orders"
        }));
    }

    #[test]
    fn sql_artifact_builds_query_object_and_searchable_edges() {
        let parser = SqlParser::new(SqlDialect::Postgres);
        let parsed = parser
            .parse("select * from public.users where id = 1")
            .unwrap();
        let analysis = analyze_sql(&parsed.raw_sql, SqlDialect::Postgres).unwrap();

        let graph =
            sql_artifact_to_graph("snapshot:1", "sql/users.sql", &parsed, &analysis).unwrap();

        assert_eq!(graph.object.kind, DbObjectKind::Query);
        assert!(graph
            .object
            .metadata
            .get("normalizedSql")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("public.users"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == DbEdgeKind::ReadsFrom));
        assert_eq!(graph.artifact.snapshot_id, "snapshot:1");
        assert_eq!(graph.artifact.fingerprint, parsed.fingerprint);
    }

    #[test]
    fn sql_edges_resolve_table_and_column_references_to_snapshot_object_ids() {
        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        let mut orders = DbObject::new("table:orders", DbObjectKind::Table, "public.orders");
        orders.schema_name = Some("public".to_owned());
        orders.table_name = Some("orders".to_owned());
        let mut status = DbObject::new(
            "column:orders.status",
            DbObjectKind::Column,
            "public.orders.status",
        );
        status.schema_name = Some("public".to_owned());
        status.table_name = Some("orders".to_owned());
        status.column_name = Some("status".to_owned());
        status.column = Some(ColumnMetadata {
            data_type: Some("text".to_owned()),
            data_type_family: Some("text".to_owned()),
            nullable: Some(false),
            default: None,
            comment: None,
        });
        snapshot.objects = vec![orders, status];
        let parser = SqlParser::new(SqlDialect::Postgres);
        let parsed = parser
            .parse("select * from orders o where o.status = 'paid'")
            .unwrap();
        let analysis = analyze_sql(&parsed.raw_sql, SqlDialect::Postgres).unwrap();
        let mut graph =
            sql_artifact_to_graph("snapshot:1", "sql/orders.sql", &parsed, &analysis).unwrap();

        crate::resolve_sql_edge_targets(&snapshot, &mut graph.edges);

        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == DbEdgeKind::ReadsFrom && edge.to_object_id == "table:orders"));
        assert!(graph
            .edges
            .iter()
            .any(|edge| edge.kind == DbEdgeKind::FiltersBy
                && edge.to_object_id == "column:orders.status"
                && edge.metadata.get("unresolved") == Some(&serde_json::Value::Bool(false))));
    }

    fn write(path: impl AsRef<std::path::Path>, content: &str) {
        let path = path.as_ref();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}
