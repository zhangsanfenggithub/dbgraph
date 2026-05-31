//! `SQLite` schema extraction for local business database files.

use std::path::{Path, PathBuf};

use dbgraph_core::model::{CapabilityStatus, DbSnapshot, ProviderCapabilities};
use dbgraph_core::{DbGraphError, Result};
use rusqlite::Connection;
use url::Url;

use crate::postgres::{
    canonicalize_raw_snapshot, ConnectionInfo, DatabaseProvider, ProviderConnectionConfig,
    RawColumn, RawConstraint, RawConstraintKind, RawIndex, RawSchema, RawSchemaSnapshot,
    RawStatisticsSnapshot, RawTable, RawTableStatistics, RawView,
};

/// `SQLite` provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteProvider;

impl DatabaseProvider for SqliteProvider {
    fn id(&self) -> &'static str {
        "sqlite"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        sqlite_capabilities()
    }

    fn connect(&self, config: &ProviderConnectionConfig) -> Result<ConnectionInfo> {
        let path = sqlite_path(&config.url)?;
        reject_internal_dbgraph_storage(&path)?;
        let connection = open_read_only(&path)?;
        let version = connection
            .query_row("SELECT sqlite_version()", [], |row| row.get::<_, String>(0))
            .map_err(sqlite_error)?;
        Ok(ConnectionInfo {
            current_user: "local-file".to_owned(),
            database_name: database_name(&path),
            version,
            read_only_transaction_supported: true,
        })
    }

    fn snapshot(&self, config: &ProviderConnectionConfig) -> Result<DbSnapshot> {
        let path = sqlite_path(&config.url)?;
        reject_internal_dbgraph_storage(&path)?;
        let connection = open_read_only(&path)?;
        let raw = extract_raw_schema(&connection, &path, self.capabilities())?;
        Ok(canonicalize_raw_snapshot(raw))
    }
}

fn sqlite_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        schema_metadata: CapabilityStatus::Supported,
        constraints: CapabilityStatus::Supported,
        indexes: CapabilityStatus::Supported,
        views: CapabilityStatus::Supported,
        routines: CapabilityStatus::Unsupported,
        triggers: CapabilityStatus::Unsupported,
        statistics: CapabilityStatus::Supported,
        sampling: CapabilityStatus::Unsupported,
    }
}

fn sqlite_path(value: &str) -> Result<PathBuf> {
    if value.starts_with("file:") {
        return url_to_file_path(value);
    }
    if let Some(rest) = value.strip_prefix("sqlite:") {
        return url_to_file_path(&format!("file:{rest}"));
    }
    Ok(PathBuf::from(value))
}

fn url_to_file_path(value: &str) -> Result<PathBuf> {
    let url = Url::parse(value)
        .map_err(|source| DbGraphError::invalid_config(format!("invalid SQLite URI: {source}")))?;
    url.to_file_path().map_err(|()| {
        DbGraphError::invalid_config("SQLite URI must resolve to a local filesystem path")
    })
}

fn reject_internal_dbgraph_storage(path: &Path) -> Result<()> {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized == ".dbgraph/dbgraph.db"
        || normalized.ends_with("/.dbgraph/dbgraph.db")
        || normalized.contains("/.dbgraph/dbgraph.db")
    {
        return Err(DbGraphError::invalid_config(
            "SQLite provider refuses to snapshot DbGraph internal storage at .dbgraph/dbgraph.db",
        ));
    }
    Ok(())
}

fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(
        |source| DbGraphError::Internal {
            message: format!(
                "failed to open SQLite database {}: {source}",
                path.display()
            ),
        },
    )
}

fn extract_raw_schema(
    connection: &Connection,
    path: &Path,
    capabilities: ProviderCapabilities,
) -> Result<RawSchemaSnapshot> {
    let tables = read_tables(connection)?;
    let views = read_views(connection)?;
    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    let mut indexes = Vec::new();
    let mut table_stats = Vec::new();

    for table in &tables {
        let table_columns = read_columns(connection, &table.name)?;
        let primary_key_columns = table_columns
            .iter()
            .filter(|column| column.primary_key_ordinal > 0)
            .map(|column| column.raw.name.clone())
            .collect::<Vec<_>>();
        if !primary_key_columns.is_empty() {
            constraints.push(RawConstraint {
                schema: "main".to_owned(),
                table: table.name.clone(),
                name: format!("sqlite_auto_pk_{}", table.name),
                kind: RawConstraintKind::PrimaryKey,
                columns: primary_key_columns,
                referenced_schema: None,
                referenced_table: None,
                referenced_columns: Vec::new(),
                check_expression: None,
                on_delete: None,
                on_update: None,
            });
        }
        columns.extend(table_columns.into_iter().map(|column| column.raw));
        constraints.extend(read_foreign_keys(connection, &table.name)?);
        let (table_indexes, unique_constraints) = read_indexes(connection, &table.name)?;
        indexes.extend(table_indexes);
        constraints.extend(unique_constraints);
        table_stats.push(RawTableStatistics {
            schema: "main".to_owned(),
            table: table.name.clone(),
            row_estimate: Some(table_row_count(connection, &table.name)?),
            row_count_kind: Some("exact".to_owned()),
            size_bytes: None,
            source: "sqlite.count".to_owned(),
        });
    }

    Ok(RawSchemaSnapshot {
        provider: "sqlite".to_owned(),
        database_name: database_name(path),
        capabilities,
        schemas: vec![RawSchema {
            name: "main".to_owned(),
            system: false,
        }],
        tables,
        columns,
        constraints,
        indexes,
        views,
        statistics: RawStatisticsSnapshot {
            tables: table_stats,
            columns: Vec::new(),
        },
        ..RawSchemaSnapshot::default()
    })
}

fn read_tables(connection: &Connection) -> Result<Vec<RawTable>> {
    let mut statement = connection
        .prepare(
            "SELECT name, type, sql
             FROM sqlite_master
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(RawTable {
                schema: "main".to_owned(),
                name: row.get(0)?,
                table_type: row.get(1)?,
                comment: None,
            })
        })
        .map_err(sqlite_error)?;
    collect_rows(rows)
}

fn read_views(connection: &Connection) -> Result<Vec<RawView>> {
    let mut statement = connection
        .prepare(
            "SELECT name, COALESCE(sql, '')
             FROM sqlite_master
             WHERE type = 'view'
             ORDER BY name",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(RawView {
                schema: "main".to_owned(),
                name: row.get(0)?,
                definition: row.get(1)?,
                materialized: false,
            })
        })
        .map_err(sqlite_error)?;
    collect_rows(rows)
}

struct SqliteColumn {
    raw: RawColumn,
    primary_key_ordinal: i64,
}

fn read_columns(connection: &Connection, table: &str) -> Result<Vec<SqliteColumn>> {
    let sql = format!("PRAGMA table_info({})", quote_string(table));
    let mut statement = connection.prepare(&sql).map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            let data_type = row.get::<_, String>(2)?;
            let primary_key_ordinal = row.get::<_, i64>(5)?;
            let column = RawColumn {
                schema: "main".to_owned(),
                table: table.to_owned(),
                name: row.get(1)?,
                data_type: data_type.clone(),
                data_type_family: sqlite_type_family(&data_type).to_owned(),
                nullable: row.get::<_, i64>(3)? == 0 && primary_key_ordinal == 0,
                default: row.get(4)?,
                comment: None,
                ordinal: row.get(0)?,
            };
            Ok(SqliteColumn {
                raw: column,
                primary_key_ordinal,
            })
        })
        .map_err(sqlite_error)?;
    collect_rows(rows)
}

fn read_foreign_keys(connection: &Connection, table: &str) -> Result<Vec<RawConstraint>> {
    let sql = format!("PRAGMA foreign_key_list({})", quote_string(table));
    let mut statement = connection.prepare(&sql).map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            let id = row.get::<_, i64>(0)?;
            let target_table = row.get::<_, String>(2)?;
            let from_column = row.get::<_, String>(3)?;
            let target_column = row.get::<_, String>(4)?;
            Ok(RawConstraint {
                schema: "main".to_owned(),
                table: table.to_owned(),
                name: format!("sqlite_fk_{table}_{id}_{from_column}"),
                kind: RawConstraintKind::ForeignKey,
                columns: vec![from_column],
                referenced_schema: Some("main".to_owned()),
                referenced_table: Some(target_table),
                referenced_columns: vec![target_column],
                check_expression: None,
                on_update: row.get(5)?,
                on_delete: row.get(6)?,
            })
        })
        .map_err(sqlite_error)?;
    collect_rows(rows)
}

fn read_indexes(
    connection: &Connection,
    table: &str,
) -> Result<(Vec<RawIndex>, Vec<RawConstraint>)> {
    let sql = format!("PRAGMA index_list({})", quote_string(table));
    let mut statement = connection.prepare(&sql).map_err(sqlite_error)?;
    let index_rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut indexes = Vec::new();
    let mut constraints = Vec::new();
    for row in index_rows {
        let (name, unique, origin) = row.map_err(sqlite_error)?;
        let columns = read_index_columns(connection, &name)?;
        if origin == "pk" {
            continue;
        }
        let expression_index = columns.is_empty();
        let mut index = RawIndex {
            schema: "main".to_owned(),
            table: table.to_owned(),
            name: name.clone(),
            unique,
            columns: columns.clone(),
            expression: None,
            predicate: None,
        };
        if expression_index {
            index.expression = Some("unsupported sqlite expression index".to_owned());
        }
        indexes.push(index);
        if origin == "u" {
            constraints.push(RawConstraint {
                schema: "main".to_owned(),
                table: table.to_owned(),
                name,
                kind: RawConstraintKind::Unique,
                columns,
                referenced_schema: None,
                referenced_table: None,
                referenced_columns: Vec::new(),
                check_expression: None,
                on_delete: None,
                on_update: None,
            });
        }
    }
    Ok((indexes, constraints))
}

fn read_index_columns(connection: &Connection, index: &str) -> Result<Vec<String>> {
    let sql = format!("PRAGMA index_xinfo({})", quote_string(index));
    let mut statement = connection.prepare(&sql).map_err(sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            let key = row.get::<_, i64>(5)?;
            let column_name = row.get::<_, Option<String>>(2)?;
            Ok((key, column_name))
        })
        .map_err(sqlite_error)?;
    let pairs = collect_rows(rows)?;
    Ok(pairs
        .into_iter()
        .filter_map(|(key, column_name)| (key == 1).then_some(column_name).flatten())
        .collect())
}

fn table_row_count(connection: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_identifier(table));
    connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(sqlite_error)
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite_error)
}

fn sqlite_type_family(data_type: &str) -> &'static str {
    let upper = data_type.to_ascii_uppercase();
    if upper.contains("INT") {
        "integer"
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        "text"
    } else if upper.contains("BLOB") {
        "binary"
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        "float"
    } else if upper.contains("NUM") || upper.contains("DEC") || upper.contains("BOOL") {
        "numeric"
    } else {
        "other"
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn database_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("sqlite")
        .to_owned()
}

#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(source: rusqlite::Error) -> DbGraphError {
    DbGraphError::Internal {
        message: format!("SQLite introspection error: {source}"),
    }
}
