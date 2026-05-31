//! Explicit placeholder providers for external databases skipped in local builds.

use dbgraph_core::model::{CapabilityStatus, DbSnapshot, ProviderCapabilities};
use dbgraph_core::{DbGraphError, Result};

use crate::postgres::{ConnectionInfo, DatabaseProvider, ProviderConnectionConfig};

/// `MySQL` provider placeholder.
#[derive(Debug, Clone, Copy, Default)]
pub struct MysqlProvider;

/// SQL Server provider placeholder.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlServerProvider;

impl DatabaseProvider for MysqlProvider {
    fn id(&self) -> &'static str {
        "mysql"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        external_capabilities()
    }

    fn connect(&self, _config: &ProviderConnectionConfig) -> Result<ConnectionInfo> {
        Err(not_implemented("MySQL"))
    }

    fn snapshot(&self, _config: &ProviderConnectionConfig) -> Result<DbSnapshot> {
        Err(not_implemented("MySQL"))
    }
}

impl DatabaseProvider for SqlServerProvider {
    fn id(&self) -> &'static str {
        "sql-server"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        external_capabilities()
    }

    fn connect(&self, _config: &ProviderConnectionConfig) -> Result<ConnectionInfo> {
        Err(not_implemented("SQL Server"))
    }

    fn snapshot(&self, _config: &ProviderConnectionConfig) -> Result<DbSnapshot> {
        Err(not_implemented("SQL Server"))
    }
}

fn external_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        schema_metadata: CapabilityStatus::Unsupported,
        constraints: CapabilityStatus::Unsupported,
        indexes: CapabilityStatus::Unsupported,
        views: CapabilityStatus::Unsupported,
        routines: CapabilityStatus::Unsupported,
        triggers: CapabilityStatus::Unsupported,
        statistics: CapabilityStatus::Unsupported,
        sampling: CapabilityStatus::Unsupported,
    }
}

fn not_implemented(provider: &str) -> DbGraphError {
    DbGraphError::invalid_config(format!(
        "{provider} provider is not implemented in this build; Phase08 skips external database services on this machine"
    ))
}
