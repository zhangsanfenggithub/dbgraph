//! Core types, errors, logging, and shared metadata for `DbGraph`.

pub mod benchmark;
pub mod config;
pub mod diff;
pub mod model;
pub mod profiling;
pub mod project;
pub mod sampling;
pub mod security;
pub mod snapshot;
pub mod sync;

use std::path::PathBuf;
use std::{env, io};

use thiserror::Error;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Shared result type used by `DbGraph` crates.
pub type Result<T> = std::result::Result<T, DbGraphError>;

/// The package version shared by `DbGraph` crates.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stable process exit codes used by the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCodeKind {
    /// Command completed successfully.
    Success,
    /// User supplied invalid input or command-line arguments.
    Usage,
    /// Configuration is missing or invalid.
    Config,
    /// A filesystem operation failed.
    Io,
    /// Any unexpected internal failure.
    Internal,
}

impl ExitCodeKind {
    /// Returns the numeric process exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Usage => 2,
            Self::Config => 3,
            Self::Io => 4,
            Self::Internal => 1,
        }
    }
}

/// Core error type for user-visible failures.
#[derive(Debug, Error)]
pub enum DbGraphError {
    /// User supplied invalid command-line arguments or options.
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },

    /// Configuration could not be found.
    #[error("configuration not found at {path}. Run `dbgraph init` first.")]
    ConfigNotFound { path: PathBuf },

    /// Configuration exists but is invalid.
    #[error("invalid configuration: {message}")]
    InvalidConfig { message: String },

    /// Filesystem or operating-system I/O failed.
    #[error("I/O error while accessing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Catch-all for errors that do not yet have a narrower variant.
    #[error("internal error: {message}")]
    Internal { message: String },
}

impl DbGraphError {
    /// Creates an invalid argument error.
    #[must_use]
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            message: message.into(),
        }
    }

    /// Creates an invalid configuration error.
    #[must_use]
    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self::InvalidConfig {
            message: message.into(),
        }
    }

    /// Creates an I/O error with path context.
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Maps the error to a stable CLI exit code.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCodeKind {
        match self {
            Self::InvalidArgument { .. } => ExitCodeKind::Usage,
            Self::ConfigNotFound { .. } | Self::InvalidConfig { .. } => ExitCodeKind::Config,
            Self::Io { .. } => ExitCodeKind::Io,
            Self::Internal { .. } => ExitCodeKind::Internal,
        }
    }
}

/// Controls how much diagnostic output the CLI emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogVerbosity {
    /// Errors only.
    Quiet,
    /// Informational messages.
    #[default]
    Normal,
    /// Debug diagnostics.
    Verbose,
}

impl LogVerbosity {
    fn level_filter(self) -> LevelFilter {
        match self {
            Self::Quiet => LevelFilter::ERROR,
            Self::Normal => LevelFilter::INFO,
            Self::Verbose => LevelFilter::DEBUG,
        }
    }
}

/// Initializes process logging for CLI commands.
///
/// Logs are written to stderr so future MCP stdio output can keep stdout clean.
///
/// # Errors
///
/// Returns an error if the environment log filter is invalid or if another
/// tracing subscriber was already installed.
pub fn init_logging(verbosity: LogVerbosity) -> Result<()> {
    let default_filter = verbosity.level_filter().to_string();
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_filter))
        .map_err(|err| DbGraphError::Internal {
            message: format!("failed to configure log filter: {err}"),
        })?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_target(false)
        .try_init()
        .map_err(|err| DbGraphError::Internal {
            message: format!("failed to initialize logging: {err}"),
        })
}

/// Returns the stable CLI version string.
#[must_use]
pub fn version_string() -> String {
    format!("dbgraph {VERSION}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_contains_package_version() {
        assert_eq!(version_string(), format!("dbgraph {VERSION}"));
    }

    #[test]
    fn invalid_argument_is_user_readable_and_maps_to_usage() {
        let err = DbGraphError::invalid_argument("unknown option `--bad`");

        assert_eq!(err.to_string(), "invalid argument: unknown option `--bad`");
        assert_eq!(err.exit_code(), ExitCodeKind::Usage);
        assert_eq!(err.exit_code().code(), 2);
    }

    #[test]
    fn config_error_points_to_init_and_maps_to_config() {
        let err = DbGraphError::ConfigNotFound {
            path: PathBuf::from(".dbgraph/dbgraph.config.json"),
        };

        assert_eq!(err.exit_code(), ExitCodeKind::Config);
        assert!(err.to_string().contains("Run `dbgraph init` first"));
    }

    #[test]
    fn io_error_keeps_source_context_and_maps_to_io() {
        let source = io::Error::new(io::ErrorKind::NotFound, "missing file");
        let err = DbGraphError::io("missing.txt", source);

        assert_eq!(err.exit_code(), ExitCodeKind::Io);
        assert!(err.to_string().contains("missing.txt"));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn verbosity_maps_to_expected_filters() {
        assert_eq!(LogVerbosity::Quiet.level_filter(), LevelFilter::ERROR);
        assert_eq!(LogVerbosity::Normal.level_filter(), LevelFilter::INFO);
        assert_eq!(LogVerbosity::Verbose.level_filter(), LevelFilter::DEBUG);
    }
}
