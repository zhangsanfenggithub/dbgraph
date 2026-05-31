//! Database provider abstractions and `PostgreSQL` catalog extraction.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use dbgraph_core::model::{
    CapabilityStatus, ColumnMetadata, ColumnProfile, ConstraintMetadata, DbEdge, DbEdgeKind,
    DbObject, DbObjectKind, DbSnapshot, Evidence, IndexMetadata, Metadata, ProviderCapabilities,
    TableMetadata, TableProfile,
};
use dbgraph_core::{DbGraphError, Result};
use postgres::{Client, NoTls};
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

/// Runtime connection settings for provider snapshot extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConnectionConfig {
    /// `PostgreSQL` connection URL.
    pub url: String,
    /// TCP connection timeout.
    pub connect_timeout_ms: u64,
    /// Per-statement timeout configured after connecting.
    pub statement_timeout_ms: u64,
}

impl ProviderConnectionConfig {
    /// Creates settings from a connection URL with conservative defaults.
    #[must_use]
    pub fn from_url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            connect_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
        }
    }

    /// Returns a redacted URL suitable for diagnostics.
    #[must_use]
    pub fn redacted_url(&self) -> String {
        redact_connection_url(&self.url)
    }

    /// Extracts a best-effort database name from the URL path.
    #[must_use]
    pub fn database_name_hint(&self) -> Option<String> {
        Url::parse(&self.url).ok().and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|segment| !segment.is_empty())
                .map(ToOwned::to_owned)
        })
    }
}

/// Metadata returned after opening a provider connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    /// Current database user.
    pub current_user: String,
    /// Current database name.
    pub database_name: String,
    /// `PostgreSQL` version string.
    pub version: String,
    /// Whether a read-only transaction probe succeeded.
    pub read_only_transaction_supported: bool,
}

/// Database provider interface.
pub trait DatabaseProvider {
    /// Stable provider id.
    fn id(&self) -> &'static str;
    /// Capability matrix.
    fn capabilities(&self) -> ProviderCapabilities;
    /// Opens and validates a read-only introspection connection.
    ///
    /// # Errors
    ///
    /// Returns a user-readable error when the connection or setup fails.
    fn connect(&self, config: &ProviderConnectionConfig) -> Result<ConnectionInfo>;
    /// Captures a canonical snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when connection or catalog extraction fails.
    fn snapshot(&self, config: &ProviderConnectionConfig) -> Result<DbSnapshot>;
}

/// Provider registry.
#[derive(Debug, Default)]
pub struct ProviderRegistry;

impl ProviderRegistry {
    /// Returns a provider by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Box<dyn DatabaseProvider>> {
        match id {
            "postgres" => Some(Box::new(PostgresProvider)),
            "mysql" => Some(Box::new(crate::MysqlProvider)),
            "sql-server" => Some(Box::new(crate::SqlServerProvider)),
            "sqlite" => Some(Box::new(crate::SqliteProvider)),
            _ => None,
        }
    }
}

/// `PostgreSQL` provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresProvider;

impl DatabaseProvider for PostgresProvider {
    fn id(&self) -> &'static str {
        "postgres"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            schema_metadata: CapabilityStatus::Supported,
            constraints: CapabilityStatus::Supported,
            indexes: CapabilityStatus::Supported,
            views: CapabilityStatus::Supported,
            routines: CapabilityStatus::Supported,
            triggers: CapabilityStatus::Supported,
            statistics: CapabilityStatus::Supported,
            sampling: CapabilityStatus::Unsupported,
        }
    }

    fn connect(&self, config: &ProviderConnectionConfig) -> Result<ConnectionInfo> {
        let mut client = connect_postgres(config)?;
        setup_read_only_session(&mut client, config)?;
        read_connection_info(&mut client)
    }

    fn snapshot(&self, config: &ProviderConnectionConfig) -> Result<DbSnapshot> {
        let mut client = connect_postgres(config)?;
        setup_read_only_session(&mut client, config)?;
        let info = read_connection_info(&mut client)?;
        let raw = extract_raw_schema(&mut client, self.capabilities(), &info.database_name)?;
        Ok(canonicalize_raw_snapshot(raw))
    }
}

/// Raw provider snapshot before canonical graph mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSchemaSnapshot {
    /// Provider id.
    pub provider: String,
    /// Database name.
    pub database_name: String,
    /// Provider capabilities.
    pub capabilities: ProviderCapabilities,
    /// Schemas.
    pub schemas: Vec<RawSchema>,
    /// Tables.
    pub tables: Vec<RawTable>,
    /// Columns.
    pub columns: Vec<RawColumn>,
    /// Constraints.
    pub constraints: Vec<RawConstraint>,
    /// Indexes.
    pub indexes: Vec<RawIndex>,
    /// Views.
    pub views: Vec<RawView>,
    /// Routines.
    pub routines: Vec<RawRoutine>,
    /// Triggers.
    pub triggers: Vec<RawTrigger>,
    /// Enums.
    pub enums: Vec<RawEnum>,
    /// Sequences.
    pub sequences: Vec<RawSequence>,
    /// Statistics.
    pub statistics: RawStatisticsSnapshot,
}

impl Default for RawSchemaSnapshot {
    fn default() -> Self {
        Self {
            provider: "postgres".to_owned(),
            database_name: String::new(),
            capabilities: ProviderCapabilities::default(),
            schemas: Vec::new(),
            tables: Vec::new(),
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
            views: Vec::new(),
            routines: Vec::new(),
            triggers: Vec::new(),
            enums: Vec::new(),
            sequences: Vec::new(),
            statistics: RawStatisticsSnapshot::default(),
        }
    }
}

/// Raw schema row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSchema {
    pub name: String,
    pub system: bool,
}

/// Raw table row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTable {
    pub schema: String,
    pub name: String,
    pub table_type: String,
    pub comment: Option<String>,
}

/// Raw column row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawColumn {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub data_type: String,
    pub data_type_family: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    pub ordinal: i32,
}

/// Raw constraint kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawConstraintKind {
    PrimaryKey,
    ForeignKey,
    Unique,
    Check,
}

/// Raw constraint row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawConstraint {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub kind: RawConstraintKind,
    pub columns: Vec<String>,
    pub referenced_schema: Option<String>,
    pub referenced_table: Option<String>,
    pub referenced_columns: Vec<String>,
    pub check_expression: Option<String>,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

/// Raw index row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawIndex {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
    pub expression: Option<String>,
    pub predicate: Option<String>,
}

/// Raw view row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawView {
    pub schema: String,
    pub name: String,
    pub definition: String,
    pub materialized: bool,
}

/// Raw routine kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawRoutineKind {
    Function,
    Procedure,
}

/// Raw routine row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRoutine {
    pub schema: String,
    pub name: String,
    pub identity_arguments: String,
    pub routine_kind: RawRoutineKind,
    pub return_type: Option<String>,
    pub language: Option<String>,
}

/// Raw trigger row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTrigger {
    pub schema: String,
    pub table: String,
    pub name: String,
    pub function_schema: String,
    pub function_name: String,
    pub function_identity_arguments: String,
    pub enabled: bool,
}

/// Raw enum row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEnum {
    pub schema: String,
    pub name: String,
    pub labels: Vec<String>,
}

/// Raw sequence row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSequence {
    pub schema: String,
    pub name: String,
    pub owner_table: Option<String>,
    pub owner_column: Option<String>,
}

/// Raw statistics snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStatisticsSnapshot {
    pub tables: Vec<RawTableStatistics>,
    pub columns: Vec<RawColumnStatistics>,
}

/// Raw table stats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTableStatistics {
    pub schema: String,
    pub table: String,
    pub row_estimate: Option<i64>,
    pub row_count_kind: Option<String>,
    pub size_bytes: Option<i64>,
    pub source: String,
}

/// Raw column stats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawColumnStatistics {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub data_type_family: Option<String>,
    pub null_fraction: Option<f64>,
    pub distinct_estimate: Option<f64>,
    pub avg_width: Option<i32>,
    pub histogram_bounds_count: Option<u64>,
    pub most_common_value_count: Option<u64>,
    pub source: String,
}

/// Converts raw provider output into the canonical snapshot model.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn canonicalize_raw_snapshot(raw: RawSchemaSnapshot) -> DbSnapshot {
    let mut snapshot = DbSnapshot::new(
        format!("{}:{}", raw.provider, raw.database_name),
        raw.provider,
        raw.database_name,
        0,
    );
    snapshot.capabilities = raw.capabilities;
    snapshot.metadata.insert(
        "provider".to_owned(),
        json!({
            "capabilities": snapshot.capabilities,
        }),
    );

    let system_schemas = raw
        .schemas
        .iter()
        .filter(|schema| schema.system || is_system_schema(&schema.name))
        .map(|schema| schema.name.as_str())
        .collect::<HashSet<_>>();

    for schema in raw
        .schemas
        .iter()
        .filter(|schema| !system_schemas.contains(schema.name.as_str()))
    {
        snapshot.objects.push(DbObject::new(
            schema_object_id(&schema.name),
            DbObjectKind::Schema,
            &schema.name,
        ));
    }

    for table in raw
        .tables
        .iter()
        .filter(|table| !system_schemas.contains(table.schema.as_str()))
    {
        let mut object = DbObject::new(
            table_object_id(&table.schema, &table.name),
            DbObjectKind::Table,
            qualified(&table.schema, &table.name),
        );
        object.schema_name = Some(table.schema.clone());
        object.table_name = Some(table.name.clone());
        object.table = Some(TableMetadata {
            table_type: Some(table.table_type.clone()),
            comment: table.comment.clone(),
        });
        if let Some(comment) = &table.comment {
            object.metadata.insert("comment".to_owned(), json!(comment));
        }
        snapshot.objects.push(object);
    }

    for column in raw
        .columns
        .iter()
        .filter(|column| !system_schemas.contains(column.schema.as_str()))
    {
        let mut object = DbObject::new(
            column_object_id(&column.schema, &column.table, &column.name),
            DbObjectKind::Column,
            format!(
                "{}.{}",
                qualified(&column.schema, &column.table),
                column.name
            ),
        );
        object.schema_name = Some(column.schema.clone());
        object.table_name = Some(column.table.clone());
        object.column_name = Some(column.name.clone());
        object.column = Some(ColumnMetadata {
            data_type: Some(column.data_type.clone()),
            data_type_family: Some(column.data_type_family.clone()),
            nullable: Some(column.nullable),
            default: column.default.clone(),
            comment: column.comment.clone(),
        });
        if let Some(comment) = &column.comment {
            object.metadata.insert("comment".to_owned(), json!(comment));
        }
        snapshot.edges.push(DbEdge::explicit(
            format!(
                "has_column:{}->{}",
                table_object_id(&column.schema, &column.table),
                column_object_id(&column.schema, &column.table, &column.name)
            ),
            DbEdgeKind::HasColumn,
            table_object_id(&column.schema, &column.table),
            column_object_id(&column.schema, &column.table, &column.name),
        ));
        snapshot.objects.push(object);
    }

    for constraint in raw
        .constraints
        .iter()
        .filter(|constraint| !system_schemas.contains(constraint.schema.as_str()))
    {
        let kind = match constraint.kind {
            RawConstraintKind::PrimaryKey => DbObjectKind::PrimaryKey,
            RawConstraintKind::ForeignKey => DbObjectKind::ForeignKey,
            RawConstraintKind::Unique => DbObjectKind::UniqueConstraint,
            RawConstraintKind::Check => DbObjectKind::CheckConstraint,
        };
        let constraint_id =
            constraint_object_id(&constraint.schema, &constraint.table, &constraint.name);
        let mut object = DbObject::new(
            constraint_id.clone(),
            kind,
            format!(
                "{}.{}",
                qualified(&constraint.schema, &constraint.table),
                constraint.name
            ),
        );
        object.schema_name = Some(constraint.schema.clone());
        object.table_name = Some(constraint.table.clone());
        object.constraint = Some(ConstraintMetadata {
            columns: constraint.columns.clone(),
            referenced_table: constraint
                .referenced_schema
                .as_ref()
                .zip(constraint.referenced_table.as_ref())
                .map(|(schema, table)| qualified(schema, table)),
            referenced_columns: constraint.referenced_columns.clone(),
        });
        object.metadata.insert(
            "constraint".to_owned(),
            json!({
                "checkExpression": constraint.check_expression,
                "onDelete": constraint.on_delete,
                "onUpdate": constraint.on_update,
            }),
        );
        snapshot.edges.push(DbEdge::explicit(
            format!(
                "has_constraint:{}->{constraint_id}",
                table_object_id(&constraint.schema, &constraint.table)
            ),
            DbEdgeKind::HasConstraint,
            table_object_id(&constraint.schema, &constraint.table),
            constraint_id.clone(),
        ));
        if constraint.kind == RawConstraintKind::ForeignKey {
            if let (Some(target_schema), Some(target_table)) = (
                constraint.referenced_schema.as_deref(),
                constraint.referenced_table.as_deref(),
            ) {
                let mut edge = DbEdge::explicit(
                    format!(
                        "references:{constraint_id}->{}",
                        table_object_id(target_schema, target_table)
                    ),
                    DbEdgeKind::References,
                    constraint_id.clone(),
                    table_object_id(target_schema, target_table),
                );
                edge.evidence.push(Evidence {
                    source: "postgres.pg_constraint".to_owned(),
                    detail: format!(
                        "{} references {}({})",
                        constraint.name,
                        qualified(target_schema, target_table),
                        constraint.referenced_columns.join(", ")
                    ),
                });
                snapshot.edges.push(edge);
            }
        }
        snapshot.objects.push(object);
    }

    for index in raw
        .indexes
        .iter()
        .filter(|index| !system_schemas.contains(index.schema.as_str()))
    {
        let index_id = index_object_id(&index.schema, &index.table, &index.name);
        let mut object = DbObject::new(
            index_id.clone(),
            DbObjectKind::Index,
            format!("{}.{}", qualified(&index.schema, &index.table), index.name),
        );
        object.schema_name = Some(index.schema.clone());
        object.table_name = Some(index.table.clone());
        object.index = Some(IndexMetadata {
            unique: Some(index.unique),
            columns: index.columns.clone(),
            expression: index.expression.clone(),
        });
        object
            .metadata
            .insert("predicate".to_owned(), json!(index.predicate));
        if index.expression.is_some() && index.columns.is_empty() {
            object
                .metadata
                .insert("expressionIndex".to_owned(), json!(true));
        }
        snapshot.edges.push(DbEdge::explicit(
            format!(
                "has_index:{}->{index_id}",
                table_object_id(&index.schema, &index.table)
            ),
            DbEdgeKind::HasIndex,
            table_object_id(&index.schema, &index.table),
            index_id,
        ));
        snapshot.objects.push(object);
    }

    for view in raw
        .views
        .iter()
        .filter(|view| !system_schemas.contains(view.schema.as_str()))
    {
        let kind = if view.materialized {
            DbObjectKind::MaterializedView
        } else {
            DbObjectKind::View
        };
        let mut object = DbObject::new(
            view_object_id(&view.schema, &view.name),
            kind,
            qualified(&view.schema, &view.name),
        );
        object.schema_name = Some(view.schema.clone());
        object
            .metadata
            .insert("definition".to_owned(), json!(view.definition));
        snapshot.objects.push(object);
    }

    for routine in raw
        .routines
        .iter()
        .filter(|routine| !system_schemas.contains(routine.schema.as_str()))
    {
        let kind = match routine.routine_kind {
            RawRoutineKind::Function => DbObjectKind::Function,
            RawRoutineKind::Procedure => DbObjectKind::Procedure,
        };
        let mut object = DbObject::new(
            routine_object_id(&routine.schema, &routine.name, &routine.identity_arguments),
            kind,
            routine_full_name(routine),
        );
        object.schema_name = Some(routine.schema.clone());
        object.metadata.insert(
            "routine".to_owned(),
            json!({
                "returnType": routine.return_type,
                "language": routine.language,
                "identityArguments": routine.identity_arguments,
            }),
        );
        snapshot.objects.push(object);
    }

    for trigger in raw
        .triggers
        .iter()
        .filter(|trigger| !system_schemas.contains(trigger.schema.as_str()))
    {
        let trigger_id = trigger_object_id(&trigger.schema, &trigger.table, &trigger.name);
        let mut object = DbObject::new(
            trigger_id.clone(),
            DbObjectKind::Trigger,
            format!(
                "{}.{}",
                qualified(&trigger.schema, &trigger.table),
                trigger.name
            ),
        );
        object.schema_name = Some(trigger.schema.clone());
        object.table_name = Some(trigger.table.clone());
        object
            .metadata
            .insert("enabled".to_owned(), json!(trigger.enabled));
        snapshot.edges.push(DbEdge::explicit(
            format!(
                "triggered_by:{}->{trigger_id}",
                table_object_id(&trigger.schema, &trigger.table)
            ),
            DbEdgeKind::TriggeredBy,
            table_object_id(&trigger.schema, &trigger.table),
            trigger_id.clone(),
        ));
        snapshot.edges.push(DbEdge::explicit(
            format!(
                "depends_on:{trigger_id}->{}",
                routine_object_id(
                    &trigger.function_schema,
                    &trigger.function_name,
                    &trigger.function_identity_arguments
                )
            ),
            DbEdgeKind::DependsOn,
            trigger_id.clone(),
            routine_object_id(
                &trigger.function_schema,
                &trigger.function_name,
                &trigger.function_identity_arguments,
            ),
        ));
        snapshot.objects.push(object);
    }

    for enum_type in raw
        .enums
        .iter()
        .filter(|enum_type| !system_schemas.contains(enum_type.schema.as_str()))
    {
        let mut object = DbObject::new(
            enum_object_id(&enum_type.schema, &enum_type.name),
            DbObjectKind::Enum,
            qualified(&enum_type.schema, &enum_type.name),
        );
        object.schema_name = Some(enum_type.schema.clone());
        object
            .metadata
            .insert("labels".to_owned(), json!(enum_type.labels));
        snapshot.objects.push(object);
    }

    for sequence in raw
        .sequences
        .iter()
        .filter(|sequence| !system_schemas.contains(sequence.schema.as_str()))
    {
        let mut object = DbObject::new(
            sequence_object_id(&sequence.schema, &sequence.name),
            DbObjectKind::Sequence,
            qualified(&sequence.schema, &sequence.name),
        );
        object.schema_name = Some(sequence.schema.clone());
        object.metadata.insert(
            "owner".to_owned(),
            json!({
                "table": sequence.owner_table,
                "column": sequence.owner_column,
            }),
        );
        snapshot.objects.push(object);
    }

    let table_profile_map = raw
        .statistics
        .tables
        .iter()
        .map(|stats| {
            (
                table_object_id(&stats.schema, &stats.table),
                TableProfile {
                    object_id: table_object_id(&stats.schema, &stats.table),
                    row_estimate: stats.row_estimate,
                    row_count_kind: stats
                        .row_count_kind
                        .clone()
                        .or_else(|| stats.row_estimate.map(|_| "estimate".to_owned())),
                    size_bytes: stats.size_bytes,
                    profile: metadata([("source", json!(stats.source))]),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    snapshot.table_profiles = table_profile_map.into_values().collect();

    let column_profile_map = raw
        .statistics
        .columns
        .iter()
        .map(|stats| {
            let mut profile = metadata([
                ("source", json!(stats.source)),
                ("avgWidth", json!(stats.avg_width)),
                ("histogramBoundsCount", json!(stats.histogram_bounds_count)),
            ]);
            if let Some(count) = stats.most_common_value_count {
                profile.insert("mostCommonValueCount".to_owned(), json!(count));
            }
            (
                column_object_id(&stats.schema, &stats.table, &stats.column),
                ColumnProfile {
                    object_id: column_object_id(&stats.schema, &stats.table, &stats.column),
                    data_type_family: stats.data_type_family.clone(),
                    null_fraction: stats.null_fraction,
                    distinct_estimate: stats.distinct_estimate,
                    pii_score: None,
                    profile,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    snapshot.column_profiles = column_profile_map.into_values().collect();

    snapshot.objects.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.full_name.cmp(&right.full_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    snapshot.edges.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.from_object_id.cmp(&right.from_object_id))
            .then_with(|| left.to_object_id.cmp(&right.to_object_id))
            .then_with(|| left.id.cmp(&right.id))
    });

    snapshot
}

fn connect_postgres(config: &ProviderConnectionConfig) -> Result<Client> {
    let mut pg_config = config.url.parse::<postgres::Config>().map_err(|source| {
        DbGraphError::invalid_config(format!("invalid PostgreSQL connection URL: {source}"))
    })?;
    pg_config.connect_timeout(Duration::from_millis(config.connect_timeout_ms));
    pg_config
        .connect(NoTls)
        .map_err(|source| DbGraphError::Internal {
            message: format!(
                "failed to connect to PostgreSQL at {}: {source}",
                config.redacted_url()
            ),
        })
}

fn setup_read_only_session(client: &mut Client, config: &ProviderConnectionConfig) -> Result<()> {
    client
        .batch_execute(&format!(
            "SET statement_timeout = {}",
            config.statement_timeout_ms
        ))
        .map_err(pg_error)?;
    client
        .batch_execute("BEGIN READ ONLY; COMMIT;")
        .map_err(pg_error)
}

fn read_connection_info(client: &mut Client) -> Result<ConnectionInfo> {
    let row = client
        .query_one(
            "SELECT current_user, current_database(), version(), current_setting('transaction_read_only')",
            &[],
        )
        .map_err(pg_error)?;
    Ok(ConnectionInfo {
        current_user: row.get(0),
        database_name: row.get(1),
        version: row.get(2),
        read_only_transaction_supported: true,
    })
}

#[allow(clippy::too_many_lines)]
fn extract_raw_schema(
    client: &mut Client,
    capabilities: ProviderCapabilities,
    database_name: &str,
) -> Result<RawSchemaSnapshot> {
    let schemas = client
        .query(
            "SELECT nspname, nspname LIKE 'pg_%' OR nspname = 'information_schema' AS system
             FROM pg_namespace
             ORDER BY nspname",
            &[],
        )
        .map_err(pg_error)?
        .into_iter()
        .map(|row| RawSchema {
            name: row.get(0),
            system: row.get(1),
        })
        .collect();

    let tables = client.query(
        "SELECT n.nspname, c.relname,
                CASE c.relkind WHEN 'p' THEN 'PARTITIONED TABLE' ELSE 'BASE TABLE' END,
                obj_description(c.oid, 'pg_class')
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r','p') AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, c.relname",
        &[],
    ).map_err(pg_error)?.into_iter().map(|row| RawTable {
        schema: row.get(0),
        name: row.get(1),
        table_type: row.get(2),
        comment: row.get(3),
    }).collect();

    let columns = client
        .query(
            "SELECT n.nspname, c.relname, a.attname, t.typname,
                CASE
                  WHEN t.typcategory = 'N' THEN 'numeric'
                  WHEN t.typcategory = 'S' THEN 'string'
                  WHEN t.typcategory = 'B' THEN 'boolean'
                  WHEN t.typcategory = 'D' THEN 'datetime'
                  WHEN t.typcategory = 'E' THEN 'enum'
                  WHEN t.typcategory = 'A' THEN 'array'
                  ELSE 'other'
                END,
                a.attnotnull = false,
                pg_get_expr(d.adbin, d.adrelid),
                col_description(c.oid, a.attnum),
                a.attnum::int
         FROM pg_attribute a
         JOIN pg_class c ON c.oid = a.attrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_type t ON t.oid = a.atttypid
         LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
         WHERE c.relkind IN ('r','p') AND a.attnum > 0 AND NOT a.attisdropped
           AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, c.relname, a.attnum",
            &[],
        )
        .map_err(pg_error)?
        .into_iter()
        .map(|row| RawColumn {
            schema: row.get(0),
            table: row.get(1),
            name: row.get(2),
            data_type: row.get(3),
            data_type_family: row.get(4),
            nullable: row.get(5),
            default: row.get(6),
            comment: row.get(7),
            ordinal: row.get(8),
        })
        .collect();

    let constraints = client.query(
        "WITH keys AS (
           SELECT con.oid, COALESCE(array_agg(a.attname ORDER BY key.ordinality) FILTER (WHERE a.attname IS NOT NULL), ARRAY[]::text[]) AS columns
           FROM pg_constraint con
           LEFT JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS key(attnum, ordinality) ON true
           LEFT JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = key.attnum
           GROUP BY con.oid
         ), fkeys AS (
           SELECT con.oid, COALESCE(array_agg(a.attname ORDER BY key.ordinality) FILTER (WHERE a.attname IS NOT NULL), ARRAY[]::text[]) AS referenced_columns
           FROM pg_constraint con
           LEFT JOIN LATERAL unnest(con.confkey) WITH ORDINALITY AS key(attnum, ordinality) ON true
           LEFT JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = key.attnum
           GROUP BY con.oid
         )
         SELECT n.nspname, rel.relname, con.conname, con.contype::text,
                COALESCE(keys.columns, ARRAY[]::text[]),
                rn.nspname, rrel.relname, COALESCE(fkeys.referenced_columns, ARRAY[]::text[]),
                pg_get_constraintdef(con.oid),
                con.confdeltype::text, con.confupdtype::text
         FROM pg_constraint con
         JOIN pg_class rel ON rel.oid = con.conrelid
         JOIN pg_namespace n ON n.oid = rel.relnamespace
         LEFT JOIN pg_class rrel ON rrel.oid = con.confrelid
         LEFT JOIN pg_namespace rn ON rn.oid = rrel.relnamespace
         LEFT JOIN keys ON keys.oid = con.oid
         LEFT JOIN fkeys ON fkeys.oid = con.oid
         WHERE con.contype IN ('p','f','u','c') AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, rel.relname, con.conname",
        &[],
    ).map_err(pg_error)?.into_iter().filter_map(|row| {
        let raw_kind: String = row.get(3);
        let kind = match raw_kind.as_str() {
            "p" => RawConstraintKind::PrimaryKey,
            "f" => RawConstraintKind::ForeignKey,
            "u" => RawConstraintKind::Unique,
            "c" => RawConstraintKind::Check,
            _ => return None,
        };
        Some(RawConstraint {
            schema: row.get(0),
            table: row.get(1),
            name: row.get(2),
            kind,
            columns: row.get::<_, Vec<String>>(4),
            referenced_schema: row.get(5),
            referenced_table: row.get(6),
            referenced_columns: row.get::<_, Vec<String>>(7),
            check_expression: row.get(8),
            on_delete: row.get::<_, Option<String>>(9).map(|value| action_code(&value).to_owned()),
            on_update: row.get::<_, Option<String>>(10).map(|value| action_code(&value).to_owned()),
        })
    }).collect();

    let indexes = client.query(
        "SELECT n.nspname, tbl.relname, idx.relname, i.indisunique,
                ARRAY(
                  SELECT a.attname FROM unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord)
                  JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
                  WHERE k.attnum > 0 ORDER BY k.ord
                ),
                CASE WHEN pg_get_indexdef(i.indexrelid) LIKE '%(%' THEN pg_get_expr(i.indexprs, i.indrelid) END,
                pg_get_expr(i.indpred, i.indrelid)
         FROM pg_index i
         JOIN pg_class idx ON idx.oid = i.indexrelid
         JOIN pg_class tbl ON tbl.oid = i.indrelid
         JOIN pg_namespace n ON n.oid = tbl.relnamespace
         WHERE NOT i.indisprimary AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, tbl.relname, idx.relname",
        &[],
    ).map_err(pg_error)?.into_iter().map(|row| RawIndex {
        schema: row.get(0),
        table: row.get(1),
        name: row.get(2),
        unique: row.get(3),
        columns: row.get(4),
        expression: row.get(5),
        predicate: row.get(6),
    }).collect();

    let views = client
        .query(
            "SELECT schemaname, viewname, definition, false FROM pg_views
         WHERE NOT (schemaname LIKE 'pg_%' OR schemaname = 'information_schema')
         UNION ALL
         SELECT schemaname, matviewname, definition, true FROM pg_matviews
         WHERE NOT (schemaname LIKE 'pg_%' OR schemaname = 'information_schema')
         ORDER BY 1, 2",
            &[],
        )
        .map_err(pg_error)?
        .into_iter()
        .map(|row| RawView {
            schema: row.get(0),
            name: row.get(1),
            definition: row.get(2),
            materialized: row.get(3),
        })
        .collect();

    let routines = client.query(
        "SELECT n.nspname, p.proname, pg_get_function_identity_arguments(p.oid),
                CASE WHEN p.prokind = 'p' THEN 'procedure' ELSE 'function' END,
                pg_get_function_result(p.oid), l.lanname
         FROM pg_proc p
         JOIN pg_namespace n ON n.oid = p.pronamespace
         JOIN pg_language l ON l.oid = p.prolang
         WHERE p.prokind IN ('f','p') AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, p.proname",
        &[],
    ).map_err(pg_error)?.into_iter().map(|row| {
        let kind: String = row.get(3);
        RawRoutine {
            schema: row.get(0),
            name: row.get(1),
            identity_arguments: row.get(2),
            routine_kind: if kind == "procedure" { RawRoutineKind::Procedure } else { RawRoutineKind::Function },
            return_type: row.get(4),
            language: row.get(5),
        }
    }).collect();

    let triggers = client.query(
        "SELECT n.nspname, tbl.relname, t.tgname, fnn.nspname, p.proname,
                pg_get_function_identity_arguments(p.oid), t.tgenabled <> 'D'
         FROM pg_trigger t
         JOIN pg_class tbl ON tbl.oid = t.tgrelid
         JOIN pg_namespace n ON n.oid = tbl.relnamespace
         JOIN pg_proc p ON p.oid = t.tgfoid
         JOIN pg_namespace fnn ON fnn.oid = p.pronamespace
         WHERE NOT t.tgisinternal AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, tbl.relname, t.tgname",
        &[],
    ).map_err(pg_error)?.into_iter().map(|row| RawTrigger {
        schema: row.get(0),
        table: row.get(1),
        name: row.get(2),
        function_schema: row.get(3),
        function_name: row.get(4),
        function_identity_arguments: row.get(5),
        enabled: row.get(6),
    }).collect();

    let enums = client
        .query(
            "SELECT n.nspname, t.typname, array_agg(e.enumlabel ORDER BY e.enumsortorder)
         FROM pg_type t
         JOIN pg_namespace n ON n.oid = t.typnamespace
         JOIN pg_enum e ON e.enumtypid = t.oid
         WHERE NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         GROUP BY n.nspname, t.typname
         ORDER BY n.nspname, t.typname",
            &[],
        )
        .map_err(pg_error)?
        .into_iter()
        .map(|row| RawEnum {
            schema: row.get(0),
            name: row.get(1),
            labels: row.get(2),
        })
        .collect();

    let sequences = client
        .query(
            "SELECT n.nspname, seq.relname, tbl.relname, a.attname
         FROM pg_class seq
         JOIN pg_namespace n ON n.oid = seq.relnamespace
         LEFT JOIN pg_depend dep ON dep.objid = seq.oid AND dep.deptype = 'a'
         LEFT JOIN pg_class tbl ON tbl.oid = dep.refobjid
         LEFT JOIN pg_attribute a ON a.attrelid = dep.refobjid AND a.attnum = dep.refobjsubid
         WHERE seq.relkind = 'S' AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, seq.relname",
            &[],
        )
        .map_err(pg_error)?
        .into_iter()
        .map(|row| RawSequence {
            schema: row.get(0),
            name: row.get(1),
            owner_table: row.get(2),
            owner_column: row.get(3),
        })
        .collect();

    let table_stats = client.query(
        "SELECT n.nspname, c.relname, c.reltuples::bigint, pg_total_relation_size(c.oid)::bigint
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r','p') AND NOT (n.nspname LIKE 'pg_%' OR n.nspname = 'information_schema')
         ORDER BY n.nspname, c.relname",
        &[],
    ).map_err(pg_error)?.into_iter().map(|row| RawTableStatistics {
        schema: row.get(0),
        table: row.get(1),
        row_estimate: row.get(2),
        row_count_kind: Some("estimate".to_owned()),
        size_bytes: row.get(3),
        source: "pg_class.reltuples".to_owned(),
    }).collect();

    let column_stats = client.query(
        "SELECT schemaname, tablename, attname, null_frac::float8, n_distinct::float8, avg_width::int,
                array_length(histogram_bounds, 1),
                array_length(most_common_vals, 1)
         FROM pg_stats
         WHERE NOT (schemaname LIKE 'pg_%' OR schemaname = 'information_schema')
         ORDER BY schemaname, tablename, attname",
        &[],
    ).map_err(pg_error)?.into_iter().map(|row| RawColumnStatistics {
        schema: row.get(0),
        table: row.get(1),
        column: row.get(2),
        data_type_family: None,
        null_fraction: row.get(3),
        distinct_estimate: row.get(4),
        avg_width: row.get(5),
        histogram_bounds_count: row.get::<_, Option<i32>>(6).and_then(|value| u64::try_from(value).ok()),
        most_common_value_count: row.get::<_, Option<i32>>(7).and_then(|value| u64::try_from(value).ok()),
        source: "pg_stats".to_owned(),
    }).collect();

    Ok(RawSchemaSnapshot {
        provider: "postgres".to_owned(),
        database_name: database_name.to_owned(),
        capabilities,
        schemas,
        tables,
        columns,
        constraints,
        indexes,
        views,
        routines,
        triggers,
        enums,
        sequences,
        statistics: RawStatisticsSnapshot {
            tables: table_stats,
            columns: column_stats,
        },
    })
}

fn metadata<const N: usize>(pairs: [(&str, serde_json::Value); N]) -> Metadata {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn qualified(schema: &str, name: &str) -> String {
    format!("{schema}.{name}")
}

fn schema_object_id(schema: &str) -> String {
    format!("schema:{schema}")
}

fn table_object_id(schema: &str, table: &str) -> String {
    format!("table:{schema}.{table}")
}

fn column_object_id(schema: &str, table: &str, column: &str) -> String {
    format!("column:{schema}.{table}.{column}")
}

fn constraint_object_id(schema: &str, table: &str, constraint: &str) -> String {
    format!("constraint:{schema}.{table}.{constraint}")
}

fn index_object_id(schema: &str, table: &str, index: &str) -> String {
    format!("index:{schema}.{table}.{index}")
}

fn view_object_id(schema: &str, view: &str) -> String {
    format!("view:{schema}.{view}")
}

fn routine_object_id(schema: &str, name: &str, args: &str) -> String {
    format!("routine:{schema}.{name}({args})")
}

fn routine_full_name(routine: &RawRoutine) -> String {
    format!(
        "{}.{}({})",
        routine.schema, routine.name, routine.identity_arguments
    )
}

fn trigger_object_id(schema: &str, table: &str, trigger: &str) -> String {
    format!("trigger:{schema}.{table}.{trigger}")
}

fn enum_object_id(schema: &str, enum_type: &str) -> String {
    format!("enum:{schema}.{enum_type}")
}

fn sequence_object_id(schema: &str, sequence: &str) -> String {
    format!("sequence:{schema}.{sequence}")
}

fn is_system_schema(schema: &str) -> bool {
    schema == "information_schema" || schema.starts_with("pg_")
}

fn action_code(value: &str) -> &'static str {
    match value {
        "a" => "NO ACTION",
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "UNKNOWN",
    }
}

fn redact_connection_url(value: &str) -> String {
    if let Ok(mut url) = Url::parse(value) {
        if url.password().is_some() {
            let _ = url.set_password(Some("***"));
        }
        url.to_string()
    } else {
        "<invalid-postgres-url>".to_owned()
    }
}

#[allow(clippy::needless_pass_by_value)]
fn pg_error(source: postgres::Error) -> DbGraphError {
    DbGraphError::Internal {
        message: format!("PostgreSQL introspection error: {source}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgraph_core::model::{CapabilityStatus, DbEdgeKind, DbObjectKind};

    #[test]
    fn registry_resolves_postgres_without_exposing_concrete_type() {
        let registry = ProviderRegistry;
        let provider = registry
            .get("postgres")
            .expect("postgres provider should exist");

        assert_eq!(provider.id(), "postgres");
        assert_eq!(
            provider.capabilities().schema_metadata,
            CapabilityStatus::Supported
        );
    }

    #[test]
    fn raw_snapshot_capabilities_serialize_to_metadata() {
        let snapshot = RawSchemaSnapshot {
            provider: "postgres".to_owned(),
            database_name: "app".to_owned(),
            capabilities: PostgresProvider.capabilities(),
            ..RawSchemaSnapshot::default()
        };

        let json = serde_json::to_string(&snapshot).expect("raw snapshot should serialize");

        assert!(json.contains("capabilities"));
        assert!(json.contains("schemaMetadata"));
    }

    #[test]
    fn connection_error_does_not_include_password() {
        let config = ProviderConnectionConfig {
            url: "postgres://user:secret@127.0.0.1:1/missing".to_owned(),
            connect_timeout_ms: 50,
            statement_timeout_ms: 1_000,
        };

        let err = PostgresProvider
            .connect(&config)
            .expect_err("connect should fail");
        let message = err.to_string();

        assert!(message.contains("failed to connect to PostgreSQL"));
        assert!(!message.contains("secret"));
    }

    #[test]
    fn database_name_is_read_from_connection_url_without_password_leak() {
        let config = ProviderConnectionConfig::from_url("postgres://user:secret@localhost/app_db");

        assert_eq!(config.database_name_hint().as_deref(), Some("app_db"));
        assert!(!config.redacted_url().contains("secret"));
    }

    #[test]
    fn maps_schema_tables_columns_and_comments() {
        let raw = raw_fixture();

        let snapshot = canonicalize_raw_snapshot(raw);

        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Schema && object.full_name == "public"));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Schema && object.full_name == "tenant"));
        assert!(!snapshot
            .objects
            .iter()
            .any(|object| object.full_name == "pg_catalog"));
        let user_id = snapshot
            .objects
            .iter()
            .find(|object| object.full_name == "public.users.id")
            .expect("id column should exist");
        assert_eq!(
            user_id
                .column
                .as_ref()
                .and_then(|column| column.data_type.as_deref()),
            Some("int8")
        );
        assert_eq!(
            user_id
                .column
                .as_ref()
                .and_then(|column| column.data_type_family.as_deref()),
            Some("integer")
        );
        assert!(snapshot
            .objects
            .iter()
            .find(|object| object.full_name == "public.users")
            .and_then(|object| object.table.as_ref())
            .and_then(|table| table.comment.as_deref())
            .is_some_and(|comment| comment.contains("Application users")));
    }

    #[test]
    fn maps_constraints_and_foreign_key_edges() {
        let snapshot = canonicalize_raw_snapshot(raw_fixture());

        let pk = snapshot
            .objects
            .iter()
            .find(|object| object.full_name == "public.users.users_pkey")
            .expect("pk should exist");
        assert_eq!(pk.kind, DbObjectKind::PrimaryKey);
        assert_eq!(
            pk.constraint
                .as_ref()
                .map(|constraint| constraint.columns.as_slice()),
            Some(&["id".to_owned()][..])
        );
        let fk_edge = snapshot
            .edges
            .iter()
            .find(|edge| edge.kind == DbEdgeKind::References)
            .expect("fk reference edge should exist");
        assert!(fk_edge.from_object_id.contains("orders_user_id_fkey"));
        assert!(fk_edge.to_object_id.contains("public.users"));
        assert!((fk_edge.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn maps_indexes_views_routines_triggers_enums_sequences() {
        let snapshot = canonicalize_raw_snapshot(raw_fixture());

        let index = snapshot
            .objects
            .iter()
            .find(|object| object.full_name == "public.orders.idx_orders_lower_status")
            .expect("index should exist");
        assert_eq!(index.kind, DbObjectKind::Index);
        assert_eq!(
            index
                .index
                .as_ref()
                .and_then(|index| index.expression.as_deref()),
            Some("lower(status)")
        );
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::View
                && object.full_name == "public.active_orders"));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Function
                && object.full_name == "public.touch_updated_at()"));
        assert!(snapshot
            .edges
            .iter()
            .any(|edge| edge.kind == DbEdgeKind::TriggeredBy));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Enum
                && object.full_name == "public.order_status"));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Sequence
                && object.full_name == "public.order_id_seq"));
    }

    #[test]
    fn maps_statistics_without_sensitive_values() {
        let snapshot = canonicalize_raw_snapshot(raw_fixture());

        let profile = snapshot
            .table_profiles
            .iter()
            .find(|profile| profile.object_id == "table:public.orders")
            .expect("orders table profile should exist");
        assert_eq!(profile.row_estimate, Some(42));
        assert_eq!(profile.row_count_kind.as_deref(), Some("estimate"));
        assert_eq!(
            profile
                .profile
                .get("source")
                .and_then(serde_json::Value::as_str),
            Some("pg_class.reltuples")
        );
        let column_profile = snapshot
            .column_profiles
            .iter()
            .find(|profile| profile.object_id == "column:public.orders.status")
            .expect("status profile should exist");
        assert_eq!(column_profile.null_fraction, Some(0.1));
        assert_eq!(column_profile.distinct_estimate, Some(3.0));
        assert!(!column_profile.profile.contains_key("most_common_vals"));
        assert_eq!(
            column_profile
                .profile
                .get("mostCommonValueCount")
                .and_then(serde_json::Value::as_u64),
            Some(3)
        );
    }

    #[test]
    fn live_postgres_snapshot_when_env_is_set() {
        let Ok(url) = std::env::var("DBG_TEST_DATABASE_URL") else {
            return;
        };
        let schema = format!("dbgraph_phase03_{}", std::process::id());
        let config = ProviderConnectionConfig::from_url(url);
        let mut client = connect_postgres(&config).expect("live postgres should connect");
        client
            .batch_execute(&format!(
                "
                DROP SCHEMA IF EXISTS {schema} CASCADE;
                CREATE SCHEMA {schema};
                CREATE TYPE {schema}.order_status AS ENUM ('pending', 'active');
                CREATE TABLE {schema}.users (
                    id BIGINT PRIMARY KEY,
                    name TEXT NOT NULL
                );
                CREATE TABLE {schema}.orders (
                    id BIGINT PRIMARY KEY,
                    user_id BIGINT NOT NULL REFERENCES {schema}.users(id) ON DELETE CASCADE,
                    status {schema}.order_status NOT NULL DEFAULT 'pending'
                );
                CREATE INDEX idx_orders_status ON {schema}.orders(status);
                CREATE VIEW {schema}.active_orders AS SELECT id, user_id FROM {schema}.orders WHERE status = 'active';
                COMMENT ON TABLE {schema}.orders IS 'Customer orders';
                COMMENT ON COLUMN {schema}.orders.user_id IS 'Owning user';
                "
            ))
            .expect("fixture schema should be created");

        let snapshot_result = PostgresProvider.snapshot(&config);

        client
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE;"))
            .expect("fixture schema should be dropped");

        let snapshot = snapshot_result.expect("live snapshot should succeed");
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.full_name == format!("{schema}.orders")));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.full_name == format!("{schema}.orders.user_id")));
        assert!(snapshot
            .edges
            .iter()
            .any(|edge| edge.kind == DbEdgeKind::References));
        assert!(!snapshot.table_profiles.is_empty());
    }

    #[allow(clippy::too_many_lines)]
    fn raw_fixture() -> RawSchemaSnapshot {
        RawSchemaSnapshot {
            provider: "postgres".to_owned(),
            database_name: "app".to_owned(),
            capabilities: PostgresProvider.capabilities(),
            schemas: vec![
                RawSchema {
                    name: "public".to_owned(),
                    system: false,
                },
                RawSchema {
                    name: "tenant".to_owned(),
                    system: false,
                },
                RawSchema {
                    name: "pg_catalog".to_owned(),
                    system: true,
                },
            ],
            tables: vec![
                RawTable {
                    schema: "public".to_owned(),
                    name: "users".to_owned(),
                    table_type: "BASE TABLE".to_owned(),
                    comment: Some("Application users".to_owned()),
                },
                RawTable {
                    schema: "public".to_owned(),
                    name: "orders".to_owned(),
                    table_type: "BASE TABLE".to_owned(),
                    comment: Some("Customer orders".to_owned()),
                },
            ],
            columns: vec![
                RawColumn {
                    schema: "public".to_owned(),
                    table: "users".to_owned(),
                    name: "id".to_owned(),
                    data_type: "int8".to_owned(),
                    data_type_family: "integer".to_owned(),
                    nullable: false,
                    default: None,
                    comment: Some("User id".to_owned()),
                    ordinal: 1,
                },
                RawColumn {
                    schema: "public".to_owned(),
                    table: "orders".to_owned(),
                    name: "user_id".to_owned(),
                    data_type: "int8".to_owned(),
                    data_type_family: "integer".to_owned(),
                    nullable: false,
                    default: None,
                    comment: None,
                    ordinal: 2,
                },
                RawColumn {
                    schema: "public".to_owned(),
                    table: "orders".to_owned(),
                    name: "status".to_owned(),
                    data_type: "order_status".to_owned(),
                    data_type_family: "enum".to_owned(),
                    nullable: false,
                    default: Some("'pending'::order_status".to_owned()),
                    comment: None,
                    ordinal: 3,
                },
            ],
            constraints: vec![
                RawConstraint {
                    schema: "public".to_owned(),
                    table: "users".to_owned(),
                    name: "users_pkey".to_owned(),
                    kind: RawConstraintKind::PrimaryKey,
                    columns: vec!["id".to_owned()],
                    referenced_schema: None,
                    referenced_table: None,
                    referenced_columns: Vec::new(),
                    check_expression: None,
                    on_delete: None,
                    on_update: None,
                },
                RawConstraint {
                    schema: "public".to_owned(),
                    table: "orders".to_owned(),
                    name: "orders_user_id_fkey".to_owned(),
                    kind: RawConstraintKind::ForeignKey,
                    columns: vec!["user_id".to_owned()],
                    referenced_schema: Some("public".to_owned()),
                    referenced_table: Some("users".to_owned()),
                    referenced_columns: vec!["id".to_owned()],
                    check_expression: None,
                    on_delete: Some("CASCADE".to_owned()),
                    on_update: Some("NO ACTION".to_owned()),
                },
            ],
            indexes: vec![RawIndex {
                schema: "public".to_owned(),
                table: "orders".to_owned(),
                name: "idx_orders_lower_status".to_owned(),
                unique: false,
                columns: vec!["status".to_owned()],
                expression: Some("lower(status)".to_owned()),
                predicate: Some("status IS NOT NULL".to_owned()),
            }],
            views: vec![RawView {
                schema: "public".to_owned(),
                name: "active_orders".to_owned(),
                definition: "select * from public.orders where status = 'active'".to_owned(),
                materialized: false,
            }],
            routines: vec![RawRoutine {
                schema: "public".to_owned(),
                name: "touch_updated_at".to_owned(),
                identity_arguments: String::new(),
                routine_kind: RawRoutineKind::Function,
                return_type: Some("trigger".to_owned()),
                language: Some("plpgsql".to_owned()),
            }],
            triggers: vec![RawTrigger {
                schema: "public".to_owned(),
                table: "orders".to_owned(),
                name: "orders_touch".to_owned(),
                function_schema: "public".to_owned(),
                function_name: "touch_updated_at".to_owned(),
                function_identity_arguments: String::new(),
                enabled: true,
            }],
            enums: vec![RawEnum {
                schema: "public".to_owned(),
                name: "order_status".to_owned(),
                labels: vec!["pending".to_owned(), "active".to_owned()],
            }],
            sequences: vec![RawSequence {
                schema: "public".to_owned(),
                name: "order_id_seq".to_owned(),
                owner_table: Some("orders".to_owned()),
                owner_column: Some("id".to_owned()),
            }],
            statistics: RawStatisticsSnapshot {
                tables: vec![RawTableStatistics {
                    schema: "public".to_owned(),
                    table: "orders".to_owned(),
                    row_estimate: Some(42),
                    row_count_kind: None,
                    size_bytes: Some(4096),
                    source: "pg_class.reltuples".to_owned(),
                }],
                columns: vec![RawColumnStatistics {
                    schema: "public".to_owned(),
                    table: "orders".to_owned(),
                    column: "status".to_owned(),
                    data_type_family: Some("enum".to_owned()),
                    null_fraction: Some(0.1),
                    distinct_estimate: Some(3.0),
                    avg_width: Some(8),
                    histogram_bounds_count: Some(2),
                    most_common_value_count: Some(3),
                    source: "pg_stats".to_owned(),
                }],
            },
        }
    }
}
