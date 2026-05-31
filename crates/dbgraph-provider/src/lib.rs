//! Database provider abstractions and concrete database integrations.

pub mod postgres;
pub mod sqlite;
pub mod unsupported;

pub use postgres::{
    canonicalize_raw_snapshot, ConnectionInfo, DatabaseProvider, PostgresProvider,
    ProviderConnectionConfig, ProviderRegistry, RawColumn, RawColumnStatistics, RawConstraint,
    RawConstraintKind, RawEnum, RawIndex, RawRoutine, RawRoutineKind, RawSchema, RawSchemaSnapshot,
    RawSequence, RawStatisticsSnapshot, RawTable, RawTableStatistics, RawTrigger, RawView,
};
pub use sqlite::SqliteProvider;
pub use unsupported::{MysqlProvider, SqlServerProvider};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use dbgraph_core::model::{CapabilityStatus, DbEdgeKind, DbObjectKind};
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn registry_resolves_phase08_providers_with_explicit_capabilities() {
        let registry = ProviderRegistry;

        for id in ["postgres", "mysql", "sql-server", "sqlite"] {
            let provider = registry.get(id).expect("provider should be registered");
            assert_eq!(provider.id(), id);
            assert_ne!(
                provider.capabilities().schema_metadata,
                CapabilityStatus::Unknown
            );
            assert_ne!(provider.capabilities().indexes, CapabilityStatus::Unknown);
            assert_eq!(
                provider.capabilities().sampling,
                CapabilityStatus::Unsupported
            );
        }
    }

    #[test]
    fn skipped_external_providers_fail_explicitly_without_local_services() {
        let registry = ProviderRegistry;

        for id in ["mysql", "sql-server"] {
            let provider = registry
                .get(id)
                .expect("stub provider should be registered");
            let err = provider
                .snapshot(&ProviderConnectionConfig::from_url("unused://localhost/db"))
                .expect_err("external provider should be skipped explicitly");
            assert!(err.to_string().contains("not implemented in this build"));
        }
    }

    #[test]
    fn sqlite_fixture_snapshots_tables_columns_fk_indexes_and_row_estimates() {
        let temp = TempProject::new();
        let db_path = temp.root.join("business.sqlite");
        create_sqlite_fixture(&db_path);
        let provider = ProviderRegistry
            .get("sqlite")
            .expect("sqlite provider should be registered");

        let snapshot = provider
            .snapshot(&ProviderConnectionConfig::from_url(
                db_path.display().to_string(),
            ))
            .expect("sqlite fixture should snapshot");

        assert_eq!(snapshot.provider, "sqlite");
        assert_eq!(
            snapshot.capabilities.schema_metadata,
            CapabilityStatus::Supported
        );
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Table && object.full_name == "main.users"));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Column
                && object.full_name == "main.orders.user_id"
                && object
                    .column
                    .as_ref()
                    .is_some_and(|column| column.nullable.is_some_and(|nullable| !nullable))));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::PrimaryKey
                && object.full_name == "main.users.sqlite_auto_pk_users"));
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.kind == DbObjectKind::Index
                && object.full_name == "main.orders.idx_orders_status"));
        assert!(snapshot
            .edges
            .iter()
            .any(|edge| edge.kind == DbEdgeKind::References
                && edge.to_object_id == "table:main.users"));
        assert!(snapshot
            .table_profiles
            .iter()
            .any(|profile| profile.object_id == "table:main.orders"
                && profile.row_estimate == Some(2)
                && profile.row_count_kind.as_deref() == Some("exact")));
    }

    #[test]
    fn sqlite_provider_rejects_internal_dbgraph_storage_file() {
        let temp = TempProject::new();
        let db_path = temp.root.join(".dbgraph").join("dbgraph.db");
        fs::create_dir_all(db_path.parent().expect("path has parent"))
            .expect("internal db dir should create");
        Connection::open(&db_path).expect("internal sqlite file should create");
        let provider = ProviderRegistry
            .get("sqlite")
            .expect("sqlite provider should be registered");

        let err = provider
            .snapshot(&ProviderConnectionConfig::from_url(
                db_path.display().to_string(),
            ))
            .expect_err("internal dbgraph storage should be rejected");

        assert!(err.to_string().contains(".dbgraph"));
    }

    #[test]
    fn sqlite_provider_rejects_relative_internal_dbgraph_storage_file() {
        let provider = ProviderRegistry
            .get("sqlite")
            .expect("sqlite provider should be registered");

        let err = provider
            .snapshot(&ProviderConnectionConfig::from_url(".dbgraph/dbgraph.db"))
            .expect_err("relative internal dbgraph storage should be rejected");

        assert!(err.to_string().contains(".dbgraph"));
    }

    #[test]
    fn sqlite_expression_indexes_do_not_abort_snapshot() {
        let temp = TempProject::new();
        let db_path = temp.root.join("business.sqlite");
        let connection = Connection::open(&db_path).expect("fixture sqlite db should open");
        connection
            .execute_batch(
                "
                CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    email TEXT NOT NULL
                );
                CREATE INDEX idx_users_lower_email ON users(lower(email));
                ",
            )
            .expect("expression index fixture should create");
        let provider = ProviderRegistry
            .get("sqlite")
            .expect("sqlite provider should be registered");

        let snapshot = provider
            .snapshot(&ProviderConnectionConfig::from_url(
                db_path.display().to_string(),
            ))
            .expect("expression index should not fail snapshot");

        let index = snapshot
            .objects
            .iter()
            .find(|object| object.full_name == "main.users.idx_users_lower_email")
            .expect("expression index should be represented");
        assert!(index
            .metadata
            .get("expressionIndex")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|value| value));
    }

    #[test]
    fn sqlite_provider_accepts_file_uri_connection_strings() {
        let temp = TempProject::new();
        let db_path = temp.root.join("business.sqlite");
        create_sqlite_fixture(&db_path);
        let file_url =
            url::Url::from_file_path(&db_path).expect("temp sqlite path should become file URL");
        let provider = ProviderRegistry
            .get("sqlite")
            .expect("sqlite provider should be registered");

        let snapshot = provider
            .snapshot(&ProviderConnectionConfig::from_url(file_url.to_string()))
            .expect("file URI sqlite fixture should snapshot");

        assert_eq!(snapshot.provider, "sqlite");
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.full_name == "main.users"));
    }

    #[test]
    fn sqlite_provider_accepts_sqlite_uri_connection_strings() {
        let temp = TempProject::new();
        let db_path = temp.root.join("business.sqlite");
        create_sqlite_fixture(&db_path);
        let file_url =
            url::Url::from_file_path(&db_path).expect("temp sqlite path should become file URL");
        let sqlite_url = file_url.as_str().replacen("file:", "sqlite:", 1);
        let provider = ProviderRegistry
            .get("sqlite")
            .expect("sqlite provider should be registered");

        let snapshot = provider
            .snapshot(&ProviderConnectionConfig::from_url(sqlite_url))
            .expect("sqlite URI fixture should snapshot");

        assert_eq!(snapshot.provider, "sqlite");
        assert!(snapshot
            .objects
            .iter()
            .any(|object| object.full_name == "main.orders"));
    }

    fn create_sqlite_fixture(path: &std::path::Path) {
        let connection = Connection::open(path).expect("fixture sqlite db should open");
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    email TEXT NOT NULL UNIQUE
                );
                CREATE TABLE orders (
                    id INTEGER PRIMARY KEY,
                    user_id INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'open',
                    FOREIGN KEY(user_id) REFERENCES users(id)
                );
                CREATE INDEX idx_orders_status ON orders(status);
                INSERT INTO users (id, email) VALUES (1, 'a@example.test');
                INSERT INTO orders (id, user_id, status) VALUES
                    (10, 1, 'open'),
                    (11, 1, 'paid');
                ",
            )
            .expect("fixture schema should create");
    }

    struct TempProject {
        root: PathBuf,
    }

    impl TempProject {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be valid")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "dbgraph-provider-phase08-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("temp root should create");
            Self { root }
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            if self.root.exists() {
                fs::remove_dir_all(&self.root).expect("temp root should remove");
            }
        }
    }
}
