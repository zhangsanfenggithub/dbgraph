//! MCP server integration for `DbGraph`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use dbgraph_core::diff::{DiffEngine, SchemaDiff};
use dbgraph_core::model::{ColumnProfile, DbEdge, DbObject, DbObjectKind, DbSnapshot};
use dbgraph_core::project::ProjectContext;
use dbgraph_core::snapshot::SnapshotStore;
use dbgraph_core::{DbGraphError, Result, VERSION};
use dbgraph_graph::analysis::{AnalysisAnalyzer, AnalysisOptions, AnalysisScope};
use dbgraph_graph::context::{ContextBuilder, ContextOptions, RankingWeights};
use dbgraph_graph::impact::{ImpactAnalyzer, ImpactOptions, ImpactReport};
use dbgraph_graph::relations::{relations_for, Direction, RelationsOptions};
use dbgraph_graph::search::{search_snapshot, SearchOptions, SearchResult};
use dbgraph_sql::{analyze_sql, SqlDialect};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Describes the current implementation status of the MCP crate.
#[must_use]
pub fn crate_status() -> &'static str {
    "mcp stdio skeleton"
}

const PROTOCOL_VERSION: &str = "2024-11-05";

/// MCP server metadata returned during `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Server name.
    pub name: &'static str,
    /// Server version.
    pub version: &'static str,
}

/// MCP tool definition.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON schema for tool arguments.
    pub input_schema: Value,
}

/// Minimal tool registry. DBG-0601 intentionally exposes no tools yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRegistry;

impl ToolRegistry {
    /// Returns all registered tools.
    #[must_use]
    pub fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            status_tool(),
            search_tool(),
            table_tool(),
            context_tool(),
            relations_tool(),
            impact_tool(),
            analyze_tool(),
            diff_tool(),
            validate_sql_tool(),
        ]
    }

    fn execute(name: &str, arguments: &Value) -> ToolCallResult {
        match name {
            "dbgraph_status" => {
                let root = project_path(arguments);
                tool_result(status_project(&root))
            }
            "dbgraph_search" => tool_result(search_project(arguments)),
            "dbgraph_table" => tool_result(table_project(arguments)),
            "dbgraph_context" => tool_result(context_project(arguments)),
            "dbgraph_relations" => tool_result(relations_project(arguments)),
            "dbgraph_impact" => tool_result(impact_project(arguments)),
            "dbgraph_analyze" => tool_result(analyze_project(arguments)),
            "dbgraph_diff" => tool_result(diff_project(arguments)),
            "dbgraph_validate_sql" => tool_result(validate_sql_project(arguments)),
            _ => ToolCallResult::error(
                "unknown_tool",
                format!("unknown MCP tool `{name}`"),
                Some("Call tools/list to inspect available DbGraph tools."),
            ),
        }
    }

    fn execute_call(params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return ToolCallResult::error(
                "invalid_params",
                "tools/call requires params.name",
                Some("Pass {\"name\":\"dbgraph_status\",\"arguments\":{...}}."),
            )
            .into_json();
        };
        let empty = json!({});
        let arguments = params.get("arguments").unwrap_or(&empty);
        Self::execute(name, arguments).into_json()
    }
}

fn status_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_status".to_owned(),
        description: "Inspect DbGraph project initialization, snapshots, and graph index state. Use this before search/table when unsure whether the project is initialized.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn search_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_search".to_owned(),
        description: "Search the latest local DbGraph snapshot for database objects by keyword. Supports optional object kind filtering such as table, column, view, foreign_key, index, or sql_artifact.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "query": {
                    "type": "string",
                    "description": "Keyword query, for example payment order or kind:table refund."
                },
                "kind": {
                    "type": "string",
                    "description": "Optional object kind filter, for example table, column, view, index, foreign_key, sql_artifact."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Maximum number of results. Defaults to 20."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn table_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_table".to_owned(),
        description: "Show one table's columns, types, nullability, defaults, constraints, indexes, profile, and direct relations from the latest local DbGraph snapshot.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["table"],
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "table": {
                    "type": "string",
                    "description": "Table name or fully-qualified table name, for example payments or public.payments."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn context_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_context".to_owned(),
        description: "Build compact read-only database context for an AI task. Pass the user's task text directly as query; DbGraph will return relevant objects, relation paths, risks, and suggested next tools without executing SQL.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "query": {
                    "type": "string",
                    "description": "Natural language database task, for example refund payment order investigation."
                },
                "tokenBudget": {
                    "type": "integer",
                    "minimum": 100,
                    "maximum": 8000,
                    "description": "Approximate output token budget. Defaults to 800."
                },
                "mode": {
                    "type": "string",
                    "enum": ["compact", "verbose"],
                    "description": "compact returns fewer objects; verbose allows a larger object set within the token budget."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn relations_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_relations".to_owned(),
        description: "Traverse incoming, outgoing, explicit, and inferred relations for a database object. Returns confidence and evidence so agents do not guess relationships.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["object"],
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "object": {
                    "type": "string",
                    "description": "Object name, id, or fully-qualified name, for example public.orders."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 2,
                    "description": "Traversal depth. Defaults to 1; maximum is 2."
                },
                "direction": {
                    "type": "string",
                    "enum": ["incoming", "outgoing", "both"],
                    "description": "Traversal direction. Defaults to both."
                },
                "mode": {
                    "type": "string",
                    "enum": ["compact", "verbose"],
                    "description": "compact limits path count; verbose returns all paths within depth."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn impact_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_impact".to_owned(),
        description: "Read-only impact analysis before changing database-related code. Traverses local graph dependencies and returns affected objects plus risk notes; it never writes to or queries the business database.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["object"],
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "object": {
                    "type": "string",
                    "description": "Object name, id, or fully-qualified name to analyze, for example public.orders.status."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 2,
                    "description": "Traversal depth. Defaults to 2; maximum is 2."
                },
                "maxObjects": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum impacted objects to return before truncating."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn analyze_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_analyze".to_owned(),
        description: "Run read-only risk, quality, and performance analysis against the latest local DbGraph snapshot. Returns deterministic findings with severity and evidence.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "scope": {
                    "type": "string",
                    "enum": ["all", "risk", "quality", "performance"],
                    "description": "Analysis scope. Defaults to all."
                },
                "maxFindings": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum findings to return before truncating."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn diff_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_diff".to_owned(),
        description: "Read-only schema diff between the latest and previous local DbGraph snapshots. Use before database-related edits to avoid guessing what changed.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "maxObjects": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum changed objects to return before truncating."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn validate_sql_tool() -> ToolDefinition {
    ToolDefinition {
        name: "dbgraph_validate_sql".to_owned(),
        description: "Parse and validate SQL references against the local graph. This tool does not execute SQL, does not apply migrations, and does not write to the business database.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["sql"],
            "properties": {
                "projectPath": {
                    "type": "string",
                    "description": "Optional project directory. Defaults to the MCP server current working directory."
                },
                "sql": {
                    "type": "string",
                    "description": "SQL text to parse and validate. It is never executed."
                },
                "dialect": {
                    "type": "string",
                    "enum": ["postgres", "mysql", "generic"],
                    "description": "SQL dialect. Defaults to postgres."
                },
                "maxObjects": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum unresolved references to return before truncating."
                }
            },
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallResult {
    content: Vec<TextContent>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_error: bool,
}

impl ToolCallResult {
    fn success(payload: &Value) -> Self {
        Self {
            content: vec![TextContent::json(payload)],
            is_error: false,
        }
    }

    fn error(kind: &str, message: impl Into<String>, suggestion: Option<&str>) -> Self {
        Self {
            content: vec![TextContent::json(&json!({
                "error": {
                    "kind": kind,
                    "message": message.into(),
                    "suggestion": suggestion,
                }
            }))],
            is_error: true,
        }
    }

    fn into_json(self) -> Value {
        serde_json::to_value(self).expect("tool call result should serialize")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

impl TextContent {
    fn json(payload: &Value) -> Self {
        Self {
            content_type: "text",
            text: serde_json::to_string_pretty(&payload).expect("json payload should serialize"),
        }
    }
}

fn tool_result(result: Result<Value>) -> ToolCallResult {
    match result {
        Ok(payload) => ToolCallResult::success(&payload),
        Err(error) => ToolCallResult::error(
            error_kind(&error),
            error.to_string(),
            Some(error_suggestion(&error)),
        ),
    }
}

fn error_kind(error: &DbGraphError) -> &'static str {
    match error {
        DbGraphError::InvalidArgument { .. } => "invalid_argument",
        DbGraphError::ConfigNotFound { .. } => "config_not_found",
        DbGraphError::InvalidConfig { .. } => "invalid_config",
        DbGraphError::Io { .. } => "io",
        DbGraphError::Internal { .. } => "internal",
    }
}

fn error_suggestion(error: &DbGraphError) -> &'static str {
    match error {
        DbGraphError::ConfigNotFound { .. } => "Run `dbgraph init` in this project.",
        DbGraphError::InvalidConfig { .. } => "Run `dbgraph snapshot` after initialization.",
        DbGraphError::InvalidArgument { .. } => "Check the tool arguments and try again.",
        DbGraphError::Io { .. } => "Check project path and local filesystem permissions.",
        DbGraphError::Internal { .. } => "Retry with `RUST_LOG=debug` and inspect stderr.",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReport {
    project_root: PathBuf,
    dbgraph_dir: PathBuf,
    initialized: bool,
    config_present: bool,
    snapshot_count: usize,
    latest_snapshot: Option<String>,
    graph_db_present: bool,
    suggestion: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchReport {
    query: String,
    results: Vec<SearchResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TableReport {
    table: String,
    columns: Vec<ColumnReport>,
    constraints: Vec<ObjectSummary>,
    indexes: Vec<ObjectSummary>,
    profile: Option<dbgraph_core::model::TableProfile>,
    incoming_relations: Vec<EdgeSummary>,
    outgoing_relations: Vec<EdgeSummary>,
    suggestions: Vec<String>,
    response_budget: ResponseBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBudget {
    truncated: bool,
    omitted: BudgetOmitted,
    suggested_follow_up_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct BudgetOmitted {
    columns: usize,
    constraints: usize,
    indexes: usize,
    relations: usize,
    objects: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ColumnReport {
    name: String,
    full_name: String,
    data_type: Option<String>,
    nullable: Option<bool>,
    default: Option<String>,
    profile: Option<ColumnProfile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObjectSummary {
    kind: String,
    full_name: String,
    summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EdgeSummary {
    kind: String,
    from: String,
    to: String,
    confidence: f64,
    evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidateSqlReport {
    valid: bool,
    executed: bool,
    dialect: String,
    diagnostics: Vec<String>,
    references: Vec<String>,
    unresolved: Vec<UnresolvedReference>,
    safety: String,
    response_budget: ResponseBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnresolvedReference {
    name: String,
    kind: String,
    suggestions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResponseBudgeter {
    columns: usize,
    edges: usize,
    objects: usize,
}

impl ResponseBudgeter {
    fn from_arguments(arguments: &Value) -> Self {
        Self {
            columns: bounded_usize(arguments, "maxColumns", 50, 1, 500),
            edges: bounded_usize(arguments, "maxEdges", 50, 1, 500),
            objects: bounded_usize(arguments, "maxObjects", 50, 1, 500),
        }
    }

    fn apply_table(self, report: &mut TableReport) {
        let incoming_omitted = truncate_vec(&mut report.incoming_relations, self.edges);
        let outgoing_omitted = truncate_vec(&mut report.outgoing_relations, self.edges);
        let omitted = BudgetOmitted {
            columns: truncate_vec(&mut report.columns, self.columns),
            constraints: truncate_vec(&mut report.constraints, self.objects),
            indexes: truncate_vec(&mut report.indexes, self.objects),
            relations: incoming_omitted + outgoing_omitted,
            ..BudgetOmitted::default()
        };
        report.response_budget = ResponseBudget::from_omitted(
            omitted,
            vec!["dbgraph_table with a higher maxColumns/maxEdges limit".to_owned()],
        );
    }

    fn apply_impact(self, report: &mut ImpactReport) -> ResponseBudget {
        let omitted = BudgetOmitted {
            objects: truncate_vec(&mut report.items, self.objects),
            ..BudgetOmitted::default()
        };
        ResponseBudget::from_omitted(
            omitted,
            vec!["dbgraph_impact with a higher maxObjects limit".to_owned()],
        )
    }

    fn apply_diff(self, report: &mut SchemaDiff) -> ResponseBudget {
        let omitted = BudgetOmitted {
            objects: truncate_vec(&mut report.changes, self.objects),
            ..BudgetOmitted::default()
        };
        ResponseBudget::from_omitted(
            omitted,
            vec!["dbgraph_diff with a higher maxObjects limit".to_owned()],
        )
    }

    fn apply_unresolved(self, unresolved: &mut Vec<UnresolvedReference>) -> ResponseBudget {
        let omitted = BudgetOmitted {
            objects: truncate_vec(unresolved, self.objects),
            ..BudgetOmitted::default()
        };
        ResponseBudget::from_omitted(
            omitted,
            vec!["dbgraph_validate_sql with a higher maxObjects limit".to_owned()],
        )
    }
}

impl ResponseBudget {
    fn empty() -> Self {
        Self::from_omitted(BudgetOmitted::default(), Vec::new())
    }

    fn from_omitted(omitted: BudgetOmitted, suggested_follow_up_tools: Vec<String>) -> Self {
        let truncated = omitted.columns > 0
            || omitted.constraints > 0
            || omitted.indexes > 0
            || omitted.relations > 0
            || omitted.objects > 0;
        Self {
            truncated,
            omitted,
            suggested_follow_up_tools: if truncated {
                suggested_follow_up_tools
            } else {
                Vec::new()
            },
        }
    }
}

fn truncate_vec<T>(items: &mut Vec<T>, max_len: usize) -> usize {
    let omitted = items.len().saturating_sub(max_len);
    if omitted > 0 {
        items.truncate(max_len);
    }
    omitted
}

fn project_path(arguments: &Value) -> PathBuf {
    arguments
        .get("projectPath")
        .and_then(Value::as_str)
        .map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            PathBuf::from,
        )
}

fn status_project(root: &Path) -> Result<Value> {
    let context = ProjectContext::discover_from(root)?
        .unwrap_or_else(|| ProjectContext::from_project_root(root));
    let snapshots = snapshot_files(&context)?;
    let initialized = context.dbgraph_dir().is_dir() && context.config_path().is_file();
    let report = StatusReport {
        project_root: context.project_root().to_path_buf(),
        dbgraph_dir: context.dbgraph_dir().to_path_buf(),
        initialized,
        config_present: context.config_path().is_file(),
        snapshot_count: snapshots.len(),
        latest_snapshot: snapshots
            .last()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned()),
        graph_db_present: context.graph_db_path().is_file(),
        suggestion: if initialized {
            "Run `dbgraph snapshot` to refresh the local graph before querying.".to_owned()
        } else {
            "Run `dbgraph init` in this project, then `dbgraph snapshot`.".to_owned()
        },
    };
    to_value(report)
}

fn search_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let query = required_string(arguments, "query")?;
    let kind = arguments.get("kind").and_then(Value::as_str);
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(20_usize, |value| value.clamp(1, 50) as usize);
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let query = kind.map_or_else(|| query.to_owned(), |kind| format!("kind:{kind} {query}"));
    to_value(SearchReport {
        results: search_snapshot(&snapshot, &query, &SearchOptions { limit }),
        query,
    })
}

fn table_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let table_name = required_string(arguments, "table")?;
    let budget = ResponseBudgeter::from_arguments(arguments);
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let mut report = table_report(&snapshot, table_name);
    budget.apply_table(&mut report);
    to_value(report)
}

fn context_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let query = required_string(arguments, "query")?;
    let token_budget = bounded_usize(arguments, "tokenBudget", 800, 100, 8000);
    let max_objects = if mode(arguments) == ResponseMode::Verbose {
        24
    } else {
        12
    };
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let package = ContextBuilder::new(RankingWeights::default()).build(
        &snapshot,
        query,
        &ContextOptions {
            token_budget,
            max_objects,
        },
    );
    to_value(package)
}

fn relations_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let object = required_string(arguments, "object")?;
    let depth = bounded_usize(arguments, "depth", 1, 1, 2);
    let direction = direction(arguments)?;
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let mut report = relations_for(&snapshot, object, &RelationsOptions { depth, direction })?;
    if mode(arguments) == ResponseMode::Compact {
        report.paths.truncate(8);
    }
    to_value(report)
}

fn table_report(snapshot: &DbSnapshot, table_name: &str) -> TableReport {
    let Some(table) = resolve_table(snapshot, table_name) else {
        return TableReport {
            table: table_name.to_owned(),
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
            profile: None,
            incoming_relations: Vec::new(),
            outgoing_relations: Vec::new(),
            suggestions: table_suggestions(snapshot, table_name),
            response_budget: ResponseBudget::empty(),
        };
    };
    let table_key = table.table_name.as_deref().unwrap_or(table.name.as_str());
    TableReport {
        table: table.full_name.clone(),
        columns: snapshot
            .objects
            .iter()
            .filter(|object| {
                object.kind == DbObjectKind::Column
                    && object.table_name.as_deref() == Some(table_key)
            })
            .map(|object| ColumnReport {
                name: object
                    .column_name
                    .clone()
                    .unwrap_or_else(|| object.name.clone()),
                full_name: object.full_name.clone(),
                data_type: object
                    .column
                    .as_ref()
                    .and_then(|column| column.data_type.clone()),
                nullable: object.column.as_ref().and_then(|column| column.nullable),
                default: object
                    .column
                    .as_ref()
                    .and_then(|column| column.default.clone()),
                profile: snapshot
                    .column_profiles
                    .iter()
                    .find(|profile| profile.object_id == object.id)
                    .cloned(),
            })
            .collect(),
        constraints: related_objects(snapshot, table_key, constraint_kind),
        indexes: related_objects(snapshot, table_key, |kind| kind == DbObjectKind::Index),
        profile: snapshot
            .table_profiles
            .iter()
            .find(|profile| profile.object_id == table.id)
            .cloned(),
        incoming_relations: snapshot
            .edges
            .iter()
            .filter(|edge| edge.to_object_id == table.id)
            .map(|edge| edge_summary(snapshot, edge))
            .collect(),
        outgoing_relations: snapshot
            .edges
            .iter()
            .filter(|edge| edge.from_object_id == table.id)
            .map(|edge| edge_summary(snapshot, edge))
            .collect(),
        suggestions: Vec::new(),
        response_budget: ResponseBudget::empty(),
    }
}

fn impact_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let object = required_string(arguments, "object")?;
    let depth = bounded_usize(arguments, "depth", 2, 1, 2);
    let budget = ResponseBudgeter::from_arguments(arguments);
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let mut report = ImpactAnalyzer::new().analyze(&snapshot, object, &ImpactOptions { depth })?;
    let response_budget = budget.apply_impact(&mut report);
    to_value(json!({
        "target": report.target,
        "items": report.items,
        "risks": report.risks,
        "responseBudget": response_budget,
        "safety": "Read-only impact analysis; DbGraph does not execute SQL or write to the business database."
    }))
}

fn analyze_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let scope = arguments
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .parse::<AnalysisScope>()
        .map_err(DbGraphError::invalid_argument)?;
    let max_findings = bounded_usize(arguments, "maxFindings", 50, 1, 200);
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let mut report = AnalysisAnalyzer::new().analyze(&snapshot, &AnalysisOptions { scope });
    let omitted = truncate_vec(&mut report.findings, max_findings);
    to_value(json!({
        "snapshotId": report.snapshot_id,
        "scope": report.scope,
        "findings": report.findings,
        "severityCounts": report.severity_counts,
        "overview": report.overview,
        "sections": report.sections,
        "topFindings": report.top_findings,
        "riskScore": report.risk_score,
        "responseBudget": ResponseBudget::from_omitted(
            BudgetOmitted {
                objects: omitted,
                ..BudgetOmitted::default()
            },
            vec!["dbgraph_analyze with a higher maxFindings limit".to_owned()],
        ),
    }))
}

fn diff_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let budget = ResponseBudgeter::from_arguments(arguments);
    let context = discover_context(&root)?;
    let store = SnapshotStore::new(&context);
    let latest = store.read_latest()?.ok_or_else(|| {
        DbGraphError::invalid_config("no snapshots found; run `dbgraph snapshot` first")
    })?;
    let previous_path = store.previous_snapshot_path()?.ok_or_else(|| {
        DbGraphError::invalid_config(
            "no previous snapshot found; run `dbgraph snapshot` at least twice",
        )
    })?;
    let previous = store.read_snapshot(previous_path)?;
    let mut report = DiffEngine::compare(&previous, &latest);
    let response_budget = budget.apply_diff(&mut report);
    to_value(json!({
        "previousSnapshotId": report.previous_snapshot_id,
        "latestSnapshotId": report.latest_snapshot_id,
        "schemaHashChanged": report.schema_hash_changed,
        "changes": report.changes,
        "renameCandidates": report.rename_candidates,
        "responseBudget": response_budget,
        "safety": "Read-only schema diff over local snapshots."
    }))
}

fn validate_sql_project(arguments: &Value) -> Result<Value> {
    let root = project_path(arguments);
    let sql = required_string(arguments, "sql")?;
    let dialect = parse_dialect(arguments)?;
    let budget = ResponseBudgeter::from_arguments(arguments);
    let context = discover_context(&root)?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let analysis = analyze_sql(sql, dialect)?;
    let valid_parse = analysis.diagnostics.is_empty();
    let references = analysis
        .references
        .iter()
        .map(|reference| reference.object_name.clone())
        .collect::<Vec<_>>();
    let mut unresolved = analysis
        .references
        .iter()
        .filter(|reference| !reference_exists(&snapshot, &reference.object_name))
        .map(|reference| UnresolvedReference {
            name: reference.object_name.clone(),
            kind: reference.kind.as_str().to_owned(),
            suggestions: suggest_objects(&snapshot, &reference.object_name),
        })
        .collect::<Vec<_>>();
    let response_budget = budget.apply_unresolved(&mut unresolved);
    to_value(ValidateSqlReport {
        valid: valid_parse && unresolved.is_empty(),
        executed: false,
        dialect: dialect.as_str().to_owned(),
        diagnostics: analysis
            .diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect(),
        references,
        unresolved,
        safety: "Parse-only validation. DbGraph does not execute SQL, apply migrations, or write to the business database.".to_owned(),
        response_budget,
    })
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| DbGraphError::invalid_argument(format!("`{key}` is required")))
}

fn bounded_usize(arguments: &Value, key: &str, default: usize, min: usize, max: usize) -> usize {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map_or(default, |value| value.clamp(min, max))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseMode {
    Compact,
    Verbose,
}

fn mode(arguments: &Value) -> ResponseMode {
    match arguments.get("mode").and_then(Value::as_str) {
        Some("verbose") => ResponseMode::Verbose,
        _ => ResponseMode::Compact,
    }
}

fn direction(arguments: &Value) -> Result<Direction> {
    match arguments.get("direction").and_then(Value::as_str) {
        None | Some("both") => Ok(Direction::Both),
        Some("incoming") => Ok(Direction::Incoming),
        Some("outgoing") => Ok(Direction::Outgoing),
        Some(value) => Err(DbGraphError::invalid_argument(format!(
            "`direction` must be incoming, outgoing, or both, got `{value}`"
        ))),
    }
}

fn parse_dialect(arguments: &Value) -> Result<SqlDialect> {
    arguments
        .get("dialect")
        .and_then(Value::as_str)
        .unwrap_or("postgres")
        .parse()
        .map_err(DbGraphError::invalid_argument)
}

fn discover_context(start: &Path) -> Result<ProjectContext> {
    Ok(ProjectContext::discover_from(start)?
        .unwrap_or_else(|| ProjectContext::from_project_root(start)))
}

fn require_graph_index(context: &ProjectContext) -> Result<()> {
    if context.graph_db_path().is_file() {
        Ok(())
    } else {
        Err(DbGraphError::invalid_config(
            "graph index is missing; run `dbgraph snapshot` first",
        ))
    }
}

fn latest_snapshot(context: &ProjectContext) -> Result<DbSnapshot> {
    SnapshotStore::new(context).read_latest()?.ok_or_else(|| {
        DbGraphError::invalid_config("no snapshots found; run `dbgraph snapshot` first")
    })
}

fn snapshot_files(context: &ProjectContext) -> Result<Vec<PathBuf>> {
    if !context.snapshots_dir().is_dir() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(context.snapshots_dir())
        .map_err(|source| DbGraphError::io(context.snapshots_dir(), source))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn resolve_table<'a>(snapshot: &'a DbSnapshot, table_name: &str) -> Option<&'a DbObject> {
    let normalized = table_name.to_ascii_lowercase();
    snapshot.objects.iter().find(|object| {
        object.kind == DbObjectKind::Table
            && (object.full_name.eq_ignore_ascii_case(table_name)
                || object.name.eq_ignore_ascii_case(table_name)
                || object
                    .full_name
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{normalized}")))
    })
}

fn table_suggestions(snapshot: &DbSnapshot, table_name: &str) -> Vec<String> {
    let normalized = normalize_name(table_name);
    let mut suggestions = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Table)
        .filter(|object| {
            let name = normalize_name(&object.name);
            name.contains(&normalized) || edit_distance(&name, &normalized) <= 2
        })
        .map(|object| object.full_name.clone())
        .collect::<Vec<_>>();
    suggestions.sort();
    suggestions.truncate(5);
    suggestions
}

fn reference_exists(snapshot: &DbSnapshot, reference: &str) -> bool {
    let normalized = normalize_name(reference);
    snapshot.objects.iter().any(|object| {
        object.full_name.eq_ignore_ascii_case(reference)
            || object.name.eq_ignore_ascii_case(reference)
            || object
                .full_name
                .to_ascii_lowercase()
                .ends_with(&format!(".{normalized}"))
    })
}

fn suggest_objects(snapshot: &DbSnapshot, reference: &str) -> Vec<String> {
    let normalized = normalize_name(reference);
    let mut suggestions = snapshot
        .objects
        .iter()
        .filter(|object| {
            let name = normalize_name(&object.name);
            name.contains(&normalized)
                || normalized.contains(&name)
                || edit_distance(&name, &normalized) <= 2
        })
        .map(|object| object.full_name.clone())
        .collect::<Vec<_>>();
    suggestions.sort();
    suggestions.dedup();
    suggestions.truncate(5);
    suggestions
}

fn related_objects(
    snapshot: &DbSnapshot,
    table_key: &str,
    kind_matches: impl Fn(DbObjectKind) -> bool,
) -> Vec<ObjectSummary> {
    snapshot
        .objects
        .iter()
        .filter(|object| {
            kind_matches(object.kind) && object.table_name.as_deref() == Some(table_key)
        })
        .map(object_summary)
        .collect()
}

fn constraint_kind(kind: DbObjectKind) -> bool {
    matches!(
        kind,
        DbObjectKind::PrimaryKey
            | DbObjectKind::ForeignKey
            | DbObjectKind::UniqueConstraint
            | DbObjectKind::CheckConstraint
    )
}

fn object_summary(object: &DbObject) -> ObjectSummary {
    ObjectSummary {
        kind: object.kind.as_str().to_owned(),
        full_name: object.full_name.clone(),
        summary: object
            .metadata
            .get("comment")
            .and_then(Value::as_str)
            .map_or_else(
                || format!("{} {}", object.kind.as_str(), object.full_name),
                ToOwned::to_owned,
            ),
    }
}

fn edge_summary(snapshot: &DbSnapshot, edge: &DbEdge) -> EdgeSummary {
    let object_name = |id: &str| {
        snapshot
            .objects
            .iter()
            .find(|object| object.id == id)
            .map_or_else(|| id.to_owned(), |object| object.full_name.clone())
    };
    EdgeSummary {
        kind: edge.kind.as_str().to_owned(),
        from: object_name(&edge.from_object_id),
        to: object_name(&edge.to_object_id),
        confidence: edge.confidence,
        evidence: edge
            .evidence
            .iter()
            .map(|evidence| evidence.detail.clone())
            .collect(),
    }
}

fn normalize_name(value: &str) -> String {
    value
        .rsplit('.')
        .next()
        .unwrap_or(value)
        .trim_matches('"')
        .trim_matches('`')
        .to_ascii_lowercase()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (i, left_ch) in left.chars().enumerate() {
        current[0] = i + 1;
        for (j, right_ch) in right.chars().enumerate() {
            let cost = usize::from(left_ch != right_ch);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn to_value(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|source| DbGraphError::Internal {
        message: format!("failed to serialize MCP tool response: {source}"),
    })
}

/// Runs a newline-delimited JSON-RPC MCP server over stdio-compatible streams.
///
/// # Errors
///
/// Returns an error if reading an input line or writing a response fails.
pub fn run_stdio(input: impl Read, mut output: impl Write, mut _logs: impl Write) -> Result<()> {
    let registry = ToolRegistry;
    let mut reader = BufReader::new(input);
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|source| DbGraphError::io("<mcp-stdin>", source))?;
        if bytes == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(line.trim_end(), &registry);
        if let Some(response) = response {
            write_response(&mut output, &response)?;
        }
    }
    Ok(())
}

fn handle_line(line: &str, registry: &ToolRegistry) -> Option<JsonRpcResponse> {
    let Ok(message) = serde_json::from_str::<JsonRpcMessage>(line) else {
        return Some(JsonRpcResponse::error(
            None,
            ErrorCode::ParseError,
            "Parse error: invalid JSON",
        ));
    };
    if message.jsonrpc != "2.0" || message.method.is_empty() {
        return message.id.map(|id| {
            JsonRpcResponse::error(Some(id), ErrorCode::InvalidRequest, "Invalid Request")
        });
    }
    let id = message.id?;
    Some(match message.method.as_str() {
        "initialize" => JsonRpcResponse::result(id, initialize_result()),
        "tools/list" => JsonRpcResponse::result(id, json!({ "tools": registry.tools() })),
        "tools/call" => JsonRpcResponse::result(id, ToolRegistry::execute_call(&message.params)),
        "ping" => JsonRpcResponse::result(id, json!({})),
        method => JsonRpcResponse::error(
            Some(id),
            ErrorCode::MethodNotFound,
            format!("Method not found: {method}"),
        ),
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": server_info(),
        "instructions": "DbGraph is local-first and read-only by default. This MCP skeleton only exposes server metadata and an empty tool registry."
    })
}

fn server_info() -> ServerInfo {
    ServerInfo {
        name: "dbgraph",
        version: VERSION,
    }
}

fn write_response(output: &mut impl Write, response: &JsonRpcResponse) -> Result<()> {
    serde_json::to_writer(&mut *output, response).map_err(|source| DbGraphError::Internal {
        message: format!("failed to serialize MCP response: {source}"),
    })?;
    output
        .write_all(b"\n")
        .map_err(|source| DbGraphError::io("<mcp-stdout>", source))?;
    output
        .flush()
        .map_err(|source| DbGraphError::io("<mcp-stdout>", source))
}

#[derive(Debug, Deserialize)]
struct JsonRpcMessage {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code: code as i64,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Clone, Copy)]
enum ErrorCode {
    ParseError = -32700,
    InvalidRequest = -32600,
    MethodNotFound = -32601,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn status_is_stable() {
        assert_eq!(crate_status(), "mcp stdio skeleton");
    }

    #[test]
    fn stdio_server_responds_to_initialize_and_tools_list() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n"
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        run_stdio(Cursor::new(input), &mut stdout, &mut stderr).expect("stdio server should run");

        assert!(
            stderr.is_empty(),
            "server should not log during normal handshake"
        );
        let lines = String::from_utf8(stdout).expect("stdout should be utf8");
        let responses = lines
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid json"))
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "dbgraph");
        assert!(responses[0]["result"]["capabilities"]["tools"].is_object());
        assert_eq!(responses[1]["id"], 2);
        let tools = responses[1]["result"]["tools"]
            .as_array()
            .expect("tools should be array");
        assert_eq!(tools.len(), 9);
        let names = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "dbgraph_status",
                "dbgraph_search",
                "dbgraph_table",
                "dbgraph_context",
                "dbgraph_relations",
                "dbgraph_impact",
                "dbgraph_analyze",
                "dbgraph_diff",
                "dbgraph_validate_sql"
            ]
        );
        assert!(tools.iter().all(|tool| tool["inputSchema"].is_object()));
        assert!(tools.iter().any(|tool| {
            tool["name"] == "dbgraph_validate_sql"
                && tool["description"]
                    .as_str()
                    .is_some_and(|description| description.contains("does not execute SQL"))
        }));
    }

    #[test]
    fn stdio_server_reports_invalid_json_as_json_rpc_error() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        run_stdio(Cursor::new("not-json\n"), &mut stdout, &mut stderr)
            .expect("parse errors are protocol responses");

        assert!(stderr.is_empty());
        let response = serde_json::from_slice::<serde_json::Value>(&stdout).expect("valid json");
        assert_eq!(response["id"], serde_json::Value::Null);
        assert_eq!(response["error"]["code"], -32700);
    }

    #[test]
    fn status_tool_reports_uninitialized_project_with_suggestion() {
        let temp = TempProject::new();
        std::fs::create_dir_all(&temp.root).expect("temp root should exist");
        let input = format!(
            "{}\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_status",
                    "arguments": { "projectPath": temp.root }
                }
            })
        );
        let response = run_one_response(&input);
        let payload = tool_payload(&response);

        assert_eq!(payload["initialized"], false);
        assert!(payload["suggestion"]
            .as_str()
            .expect("suggestion")
            .contains("dbgraph init"));
    }

    #[test]
    fn search_and_table_tools_return_json_from_latest_snapshot() {
        let temp = TempProject::new();
        let snapshot = sample_snapshot();
        write_indexed_snapshot(&temp.root, &snapshot);
        let input = format!(
            "{}\n{}\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_search",
                    "arguments": {
                        "projectPath": temp.root,
                        "query": "payment",
                        "kind": "table"
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_table",
                    "arguments": {
                        "projectPath": temp.root,
                        "table": "payments"
                    }
                }
            })
        );
        let responses = run_responses(&input);
        let search = tool_payload(&responses[0]);
        let table = tool_payload(&responses[1]);

        assert_eq!(search["results"][0]["fullName"], "public.payments");
        assert_eq!(search["results"][0]["kind"], "table");
        assert_eq!(table["table"], "public.payments");
        assert_eq!(table["columns"][0]["name"], "order_id");
        assert_eq!(table["columns"][0]["dataType"], "bigint");
        assert_eq!(table["constraints"][0]["kind"], "foreign_key");
    }

    #[test]
    fn context_and_relations_tools_return_ai_context_and_evidence() {
        let temp = TempProject::new();
        let snapshot = sample_snapshot();
        write_indexed_snapshot(&temp.root, &snapshot);
        let input = format!(
            "{}\n{}\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_context",
                    "arguments": {
                        "projectPath": temp.root,
                        "query": "refund payment order",
                        "tokenBudget": 120,
                        "mode": "compact"
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_relations",
                    "arguments": {
                        "projectPath": temp.root,
                        "object": "public.orders",
                        "depth": 2,
                        "direction": "incoming",
                        "mode": "verbose"
                    }
                }
            })
        );

        let responses = run_responses(&input);
        let context = tool_payload(&responses[0]);
        let relations = tool_payload(&responses[1]);

        assert!(context["objects"]
            .as_array()
            .expect("objects")
            .iter()
            .any(|object| object["fullName"] == "public.payments"));
        assert!(context["suggestedNextTools"]
            .as_array()
            .expect("suggested next tools")
            .iter()
            .any(|tool| tool
                .as_str()
                .is_some_and(|tool| tool.contains("dbgraph table"))));
        assert!(context["estimatedTokens"].as_u64().expect("tokens") <= 120);
        assert_eq!(relations["target"], "public.orders");
        assert!(relations["paths"]
            .as_array()
            .expect("paths")
            .iter()
            .flat_map(|path| path["edges"].as_array().into_iter().flatten())
            .any(|edge| edge["confidence"].as_f64().unwrap_or_default() > 0.0
                && !edge["evidence"].as_array().expect("evidence").is_empty()));
    }

    #[test]
    fn impact_diff_and_validate_sql_tools_return_read_only_reports() {
        let temp = TempProject::new();
        let previous = sample_snapshot();
        let latest = sample_snapshot_with_extra_status();
        write_snapshot_only(&temp.root, &previous);
        write_indexed_snapshot(&temp.root, &latest);
        let input = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_impact",
                    "arguments": {
                        "projectPath": temp.root,
                        "object": "public.orders.status",
                        "depth": 2
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_diff",
                    "arguments": {
                        "projectPath": temp.root
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_validate_sql",
                    "arguments": {
                        "projectPath": temp.root,
                        "sql": "select * from public.payments where order_id = 1",
                        "dialect": "postgres"
                    }
                }
            })
        );

        let responses = run_responses(&input);
        let impact = tool_payload(&responses[0]);
        let diff = tool_payload(&responses[1]);
        let validate = tool_payload(&responses[2]);

        assert_eq!(impact["target"], "public.orders.status");
        assert!(impact["risks"]
            .as_array()
            .expect("risks")
            .iter()
            .any(|risk| risk["message"]
                .as_str()
                .is_some_and(|message| message.contains("status"))));
        assert!(diff["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .any(|change| change["fullName"] == "public.orders.status"));
        assert_eq!(validate["executed"], false);
        assert_eq!(validate["valid"], true);
        assert!(validate["unresolved"]
            .as_array()
            .expect("unresolved")
            .is_empty());
    }

    #[test]
    fn analyze_tool_returns_findings_from_latest_snapshot() {
        let temp = TempProject::new();
        let mut snapshot = sample_snapshot();
        snapshot
            .column_profiles
            .push(dbgraph_core::model::ColumnProfile {
                object_id: "column:payments.order_id".to_owned(),
                data_type_family: Some("integer".to_owned()),
                null_fraction: None,
                distinct_estimate: None,
                pii_score: Some(0.6),
                profile: dbgraph_core::model::Metadata::new(),
            });
        write_indexed_snapshot(&temp.root, &snapshot);
        let input = format!(
            "{}\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_analyze",
                    "arguments": {
                        "projectPath": temp.root,
                        "scope": "risk",
                        "maxFindings": 5
                    }
                }
            })
        );

        let response = run_one_response(&input);
        let payload = tool_payload(&response);

        assert_eq!(payload["scope"], "risk");
        assert!(payload["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(|finding| {
                finding["ruleId"] == "risk.sensitive_column"
                    && finding["title"] == "Sensitive column detected"
                    && finding["suggestedFix"]
                        .as_str()
                        .is_some_and(|fix| fix.contains("mask"))
                    && finding["confidence"].as_f64().unwrap_or_default() > 0.0
            }));
        assert!(payload["sections"]
            .as_array()
            .expect("sections")
            .iter()
            .any(|section| section["id"] == "security_privacy"));
        assert!(
            payload["topFindings"]
                .as_array()
                .expect("top findings")
                .len()
                <= 5
        );
        assert_eq!(payload["responseBudget"]["truncated"], false);
    }

    #[test]
    fn table_tool_marks_truncated_columns_and_suggests_follow_up() {
        let temp = TempProject::new();
        let snapshot = sample_large_table_snapshot(12);
        write_indexed_snapshot(&temp.root, &snapshot);
        let input = format!(
            "{}\n",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "dbgraph_table",
                    "arguments": {
                        "projectPath": temp.root,
                        "table": "wide_events",
                        "maxColumns": 3
                    }
                }
            })
        );

        let response = run_one_response(&input);
        let table = tool_payload(&response);

        assert_eq!(table["columns"].as_array().expect("columns").len(), 3);
        assert_eq!(table["responseBudget"]["truncated"], true);
        assert_eq!(table["responseBudget"]["omitted"]["columns"], 9);
        assert!(table["responseBudget"]["suggestedFollowUpTools"]
            .as_array()
            .expect("follow ups")
            .iter()
            .any(|tool| tool
                .as_str()
                .is_some_and(|tool| tool.contains("dbgraph_table"))));
    }

    fn run_one_response(input: &str) -> serde_json::Value {
        let responses = run_responses(input);
        assert_eq!(responses.len(), 1);
        responses.into_iter().next().expect("one response")
    }

    fn run_responses(input: &str) -> Vec<serde_json::Value> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_stdio(Cursor::new(input), &mut stdout, &mut stderr).expect("stdio server should run");
        assert!(stderr.is_empty());
        String::from_utf8(stdout)
            .expect("stdout should be utf8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid json response"))
            .collect()
    }

    fn tool_payload(response: &serde_json::Value) -> serde_json::Value {
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text content");
        serde_json::from_str(text).expect("tool text should be json")
    }

    fn write_indexed_snapshot(root: &std::path::Path, snapshot: &dbgraph_core::model::DbSnapshot) {
        let context = dbgraph_core::project::ProjectContext::from_project_root(root);
        std::fs::create_dir_all(context.dbgraph_dir()).expect("dbgraph dir should exist");
        std::fs::create_dir_all(context.snapshots_dir()).expect("snapshots dir should exist");
        std::fs::write(context.config_path(), "{}\n").expect("config marker should write");
        dbgraph_core::snapshot::SnapshotStore::new(&context)
            .write_snapshot(snapshot, true)
            .expect("snapshot should write");
        let mut repo =
            dbgraph_storage::GraphRepository::open(context.graph_db_path()).expect("repo open");
        repo.rebuild_snapshot(snapshot)
            .expect("graph index should rebuild");
    }

    fn write_snapshot_only(root: &std::path::Path, snapshot: &dbgraph_core::model::DbSnapshot) {
        let context = dbgraph_core::project::ProjectContext::from_project_root(root);
        std::fs::create_dir_all(context.dbgraph_dir()).expect("dbgraph dir should exist");
        std::fs::create_dir_all(context.snapshots_dir()).expect("snapshots dir should exist");
        std::fs::write(context.config_path(), "{}\n").expect("config marker should write");
        dbgraph_core::snapshot::SnapshotStore::new(&context)
            .write_snapshot(snapshot, true)
            .expect("snapshot should write");
    }

    fn sample_snapshot() -> dbgraph_core::model::DbSnapshot {
        use dbgraph_core::model::{
            ColumnMetadata, ConstraintMetadata, DbEdge, DbEdgeKind, DbObject, DbObjectKind,
            DbSnapshot, Evidence,
        };

        let mut snapshot = DbSnapshot::new("s1", "postgres", "app", 1);
        let mut orders = DbObject::new("table:orders", DbObjectKind::Table, "public.orders");
        orders.schema_name = Some("public".to_owned());
        orders.table_name = Some("orders".to_owned());
        orders.metadata.insert(
            "comment".to_owned(),
            serde_json::Value::String("customer order records".to_owned()),
        );
        let mut payments = DbObject::new("table:payments", DbObjectKind::Table, "public.payments");
        payments.schema_name = Some("public".to_owned());
        payments.table_name = Some("payments".to_owned());
        payments.metadata.insert(
            "comment".to_owned(),
            serde_json::Value::String("payment ledger records".to_owned()),
        );
        let mut refunds = DbObject::new("table:refunds", DbObjectKind::Table, "public.refunds");
        refunds.schema_name = Some("public".to_owned());
        refunds.table_name = Some("refunds".to_owned());
        refunds.metadata.insert(
            "comment".to_owned(),
            serde_json::Value::String("refund request records".to_owned()),
        );
        let mut column = DbObject::new(
            "column:payments.order_id",
            DbObjectKind::Column,
            "public.payments.order_id",
        );
        column.schema_name = Some("public".to_owned());
        column.table_name = Some("payments".to_owned());
        column.column_name = Some("order_id".to_owned());
        column.column = Some(ColumnMetadata {
            data_type: Some("bigint".to_owned()),
            data_type_family: Some("integer".to_owned()),
            nullable: Some(false),
            default: None,
            comment: None,
        });
        let mut foreign_key = DbObject::new(
            "fk:payments.orders",
            DbObjectKind::ForeignKey,
            "public.payments_order_id_fkey",
        );
        foreign_key.schema_name = Some("public".to_owned());
        foreign_key.table_name = Some("payments".to_owned());
        foreign_key.constraint = Some(ConstraintMetadata {
            columns: vec!["order_id".to_owned()],
            referenced_table: Some("public.orders".to_owned()),
            referenced_columns: vec!["id".to_owned()],
        });
        snapshot.objects = vec![orders, payments, refunds, column, foreign_key];
        let mut edge = DbEdge::explicit(
            "edge:payments.orders",
            DbEdgeKind::References,
            "table:payments",
            "table:orders",
        );
        edge.evidence.push(Evidence {
            source: "test".to_owned(),
            detail: "foreign key payments.order_id".to_owned(),
        });
        snapshot.edges.push(edge);
        snapshot
    }

    fn sample_snapshot_with_extra_status() -> dbgraph_core::model::DbSnapshot {
        use dbgraph_core::model::{ColumnMetadata, DbObject, DbObjectKind};

        let mut snapshot = sample_snapshot();
        snapshot.id = "s2".to_owned();
        snapshot.created_at_unix_ms = 2;
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
            default: Some("'pending'".to_owned()),
            comment: None,
        });
        snapshot.objects.push(status);
        snapshot
    }

    fn sample_large_table_snapshot(column_count: usize) -> dbgraph_core::model::DbSnapshot {
        use dbgraph_core::model::{ColumnMetadata, DbObject, DbObjectKind, DbSnapshot};

        let mut snapshot = DbSnapshot::new("wide", "postgres", "app", 1);
        let mut table = DbObject::new(
            "table:wide_events",
            DbObjectKind::Table,
            "public.wide_events",
        );
        table.schema_name = Some("public".to_owned());
        table.table_name = Some("wide_events".to_owned());
        snapshot.objects.push(table);
        for idx in 0..column_count {
            let mut column = DbObject::new(
                format!("column:wide_events.c{idx}"),
                DbObjectKind::Column,
                format!("public.wide_events.c{idx}"),
            );
            column.schema_name = Some("public".to_owned());
            column.table_name = Some("wide_events".to_owned());
            column.column_name = Some(format!("c{idx}"));
            column.column = Some(ColumnMetadata {
                data_type: Some("text".to_owned()),
                data_type_family: Some("text".to_owned()),
                nullable: Some(true),
                default: None,
                comment: None,
            });
            snapshot.objects.push(column);
        }
        snapshot
    }

    struct TempProject {
        root: std::path::PathBuf,
    }

    impl TempProject {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be valid")
                .as_nanos();
            Self {
                root: std::env::temp_dir()
                    .join(format!("dbgraph-mcp-test-{}-{unique}", std::process::id())),
            }
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            if self.root.exists() {
                std::fs::remove_dir_all(&self.root).expect("temp root should be removed");
            }
        }
    }
}
