//! Synthetic schema generation for benchmark and smoke testing.

use crate::model::{ColumnMetadata, DbEdge, DbEdgeKind, DbObject, DbObjectKind, DbSnapshot};

/// Options for synthetic schema generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntheticSchemaOptions {
    /// Number of tables to create.
    pub table_count: usize,
    /// Number of columns per table.
    pub columns_per_table: usize,
}

impl Default for SyntheticSchemaOptions {
    fn default() -> Self {
        Self {
            table_count: 1_000,
            columns_per_table: 4,
        }
    }
}

/// Generates a deterministic large schema snapshot.
#[must_use]
pub fn synthetic_schema_snapshot(options: SyntheticSchemaOptions) -> DbSnapshot {
    let mut snapshot = DbSnapshot::new(
        format!("synthetic-{}", options.table_count),
        "synthetic",
        "benchmark",
        1,
    );
    for table_idx in 0..options.table_count {
        let table_id = format!("table:public.table_{table_idx}");
        let mut table = DbObject::new(
            table_id.clone(),
            DbObjectKind::Table,
            format!("public.table_{table_idx}"),
        );
        table.schema_name = Some("public".to_owned());
        table.table_name = Some(format!("table_{table_idx}"));
        snapshot.objects.push(table);

        for column_idx in 0..options.columns_per_table {
            let column_id = format!("{table_id}.column_{column_idx}");
            let mut column = DbObject::new(
                column_id.clone(),
                DbObjectKind::Column,
                format!("public.table_{table_idx}.column_{column_idx}"),
            );
            column.schema_name = Some("public".to_owned());
            column.table_name = Some(format!("table_{table_idx}"));
            column.column_name = Some(format!("column_{column_idx}"));
            column.column = Some(ColumnMetadata {
                data_type: Some("bigint".to_owned()),
                data_type_family: Some("integer".to_owned()),
                nullable: Some(column_idx != 0),
                default: None,
                comment: None,
            });
            snapshot.edges.push(DbEdge::explicit(
                format!("edge:{table_id}:{column_id}"),
                DbEdgeKind::HasColumn,
                table_id.clone(),
                column_id,
            ));
            snapshot.objects.push(column);
        }
    }
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_schema_can_generate_10k_tables_without_empty_ids() {
        let snapshot = synthetic_schema_snapshot(SyntheticSchemaOptions {
            table_count: 10_000,
            columns_per_table: 1,
        });

        assert_eq!(snapshot.objects.len(), 20_000);
        assert_eq!(snapshot.edges.len(), 10_000);
        assert!(snapshot.objects.iter().all(|object| !object.id.is_empty()));
    }
}
