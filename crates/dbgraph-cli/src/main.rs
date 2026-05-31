use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use dbgraph_agent_config::{
    install_agent_config, parse_agent_kinds, render_all_instruction_fragments, render_mcp_config,
    uninstall_agent_config, AgentKind, AgentTarget,
};
use dbgraph_core::benchmark::{synthetic_schema_snapshot, SyntheticSchemaOptions};
use dbgraph_core::config::{DatabaseConfig, DatabaseProviderKind, DbGraphConfig};
use dbgraph_core::diff::{DiffEngine, SchemaDiff};
use dbgraph_core::model::{
    ColumnProfile, DbEdge, DbObject, DbObjectKind, DbSnapshot, TableProfile,
};
use dbgraph_core::profiling::{apply_profiling_policy, ProfilingMode, ProfilingOptions};
use dbgraph_core::project::ProjectContext;
use dbgraph_core::security::{apply_pii_profiles, PiiDetector, PiiRuleConfig};
use dbgraph_core::snapshot::{now_unix_ms, SnapshotStore};
use dbgraph_core::sync::{plan_incremental_sync, SyncPlan};
use dbgraph_core::{init_logging, version_string, DbGraphError, LogVerbosity, Result};
use dbgraph_graph::analysis::{
    AnalysisAnalyzer, AnalysisFinding, AnalysisOptions, AnalysisReport, AnalysisScope,
};
use dbgraph_graph::context::{ContextBuilder, ContextOptions, ContextPackage, RankingWeights};
use dbgraph_graph::impact::{ImpactAnalyzer, ImpactOptions, ImpactReport};
use dbgraph_graph::rebuild_index;
use dbgraph_graph::relations::{relations_for, Direction, RelationsOptions, RelationsReport};
use dbgraph_graph::search::{search_snapshot, SearchOptions, SearchResult};
use dbgraph_mcp::run_stdio;
use dbgraph_provider::{ProviderConnectionConfig, ProviderRegistry};
use dbgraph_sql::{
    analyze_sql, resolve_sql_edge_targets, scan_sql_files, sql_artifact_to_graph, ScanOptions,
    SqlDialect, SqlParser,
};
use dbgraph_storage::{GraphRepository, SqlArtifactRecord as StoredSqlArtifactRecord};
use serde::Serialize;
use tracing::debug;

fn main() -> ExitCode {
    let outcome = run(env::args().skip(1));

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            print_error(&err);
            ExitCode::from(err.exit_code().code())
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run(args: impl IntoIterator<Item = String>) -> Result<()> {
    let parsed = parse_args(args)?;
    init_logging(parsed.verbosity)?;
    debug!(verbosity = ?parsed.verbosity, "CLI logging initialized");

    match parsed.command {
        Command::Version => {
            println!("{}", version_string());
            Ok(())
        }
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Init {
            path,
            force,
            interactive,
            yes,
        } => {
            let root = match path {
                Some(path) => path,
                None => env::current_dir().map_err(|source| DbGraphError::io(".", source))?,
            };
            let options = if interactive {
                if yes {
                    InitOptions::interactive_defaults()
                } else {
                    prompt_init_options()?
                }
            } else {
                InitOptions::default()
            };
            let summary = init_project_with_optional_snapshot(&root, force, &options, |path| {
                run_snapshot(path).map(Some)
            })?;
            println!(
                "Initialized DbGraph project at {}",
                summary.init.dbgraph_dir.display()
            );
            println!("Config: {}", summary.init.config_path.display());
            if options.configure_agent {
                println!(
                    "Instruction fragments: {}",
                    summary.init.instructions_dir.display()
                );
            }
            if let Some(snapshot) = &summary.snapshot {
                print_snapshot_summary(snapshot);
            }
            Ok(())
        }
        Command::Status { path, json } => {
            let root = match path {
                Some(path) => path,
                None => env::current_dir().map_err(|source| DbGraphError::io(".", source))?,
            };
            let status = read_status(&root)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status).map_err(|source| {
                        DbGraphError::Internal {
                            message: format!("failed to serialize status: {source}"),
                        }
                    })?
                );
            } else {
                print_status(&status);
            }
            Ok(())
        }
        Command::Snapshot {
            path,
            json,
            profile,
            max_rows_per_table,
            store_raw_samples,
        } => {
            let root = match path {
                Some(path) => path,
                None => env::current_dir().map_err(|source| DbGraphError::io(".", source))?,
            };
            let summary = run_snapshot_with_options(
                &root,
                SnapshotCliOptions {
                    profile,
                    max_rows_per_table,
                    store_raw_samples,
                },
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&summary).map_err(|source| {
                        DbGraphError::Internal {
                            message: format!("failed to serialize snapshot summary: {source}"),
                        }
                    })?
                );
            } else {
                print_snapshot_summary(&summary);
            }
            Ok(())
        }
        Command::Sync { path, json } => {
            let root = path_or_current(path)?;
            let summary = sync_project(&root)?;
            print_json_or(&summary, json, print_sync_summary)
        }
        Command::Benchmark {
            tables,
            columns_per_table,
            json,
        } => {
            let report = benchmark_project(tables, columns_per_table)?;
            print_json_or(&report, json, print_benchmark_report)
        }
        Command::ValidateSql {
            path,
            sql,
            file,
            dialect,
            json,
        } => {
            let root = match path {
                Some(path) => path,
                None => env::current_dir().map_err(|source| DbGraphError::io(".", source))?,
            };
            let sql = read_sql_input(sql, file.as_deref())?;
            let report = validate_sql(&root, &sql, dialect)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|source| {
                        DbGraphError::Internal {
                            message: format!("failed to serialize validate-sql report: {source}"),
                        }
                    })?
                );
            } else {
                print_validate_sql_report(&report);
            }
            Ok(())
        }
        Command::Search {
            path,
            query,
            kind,
            json,
        } => {
            let root = path_or_current(path)?;
            let report = search_project(&root, &query, kind.as_deref())?;
            print_json_or(&report, json, print_search_report)
        }
        Command::Table { path, table, json } => {
            let root = path_or_current(path)?;
            let report = table_project(&root, &table)?;
            print_json_or(&report, json, print_table_report)
        }
        Command::Relations {
            path,
            object,
            depth,
            direction,
            json,
        } => {
            let root = path_or_current(path)?;
            let report = relations_project(&root, &object, depth, direction)?;
            print_json_or(&report, json, print_relations_report)
        }
        Command::Context {
            path,
            query,
            token_budget,
            json,
        } => {
            let root = path_or_current(path)?;
            let report = context_project(&root, &query, token_budget)?;
            if json {
                print_json(&report)
            } else {
                print_context_report(&report);
                Ok(())
            }
        }
        Command::Diff { path, json } => {
            let root = path_or_current(path)?;
            let report = diff_project(&root)?;
            print_json_or(&report, json, print_diff_report)
        }
        Command::Impact {
            path,
            object,
            depth,
            json,
        } => {
            let root = path_or_current(path)?;
            let report = impact_project(&root, &object, depth)?;
            print_json_or(&report, json, print_impact_report)
        }
        Command::Analyze {
            path,
            scope,
            json,
            format,
            output,
        } => {
            let root = path_or_current(path)?;
            let report = analyze_project(&root, scope)?;
            let format = if json {
                AnalysisOutputFormat::Json
            } else {
                format
            };
            write_analysis_output(&report, format, output.as_deref())
        }
        Command::Install {
            targets,
            location,
            yes: _,
            dry_run,
            print_config,
        } => install_agents(&targets, location.as_deref(), dry_run, print_config),
        Command::Uninstall {
            targets,
            location,
            dry_run,
        } => uninstall_agents(&targets, location.as_deref(), dry_run),
        Command::ServeMcp => run_stdio(io::stdin(), io::stdout(), io::stderr()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedArgs {
    verbosity: LogVerbosity,
    command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Version,
    Help,
    Init {
        path: Option<PathBuf>,
        force: bool,
        interactive: bool,
        yes: bool,
    },
    Status {
        path: Option<PathBuf>,
        json: bool,
    },
    Snapshot {
        path: Option<PathBuf>,
        json: bool,
        profile: Option<ProfilingMode>,
        max_rows_per_table: Option<u32>,
        store_raw_samples: bool,
    },
    Sync {
        path: Option<PathBuf>,
        json: bool,
    },
    Benchmark {
        tables: usize,
        columns_per_table: usize,
        json: bool,
    },
    ValidateSql {
        path: Option<PathBuf>,
        sql: Option<String>,
        file: Option<PathBuf>,
        dialect: SqlDialect,
        json: bool,
    },
    Search {
        path: Option<PathBuf>,
        query: String,
        kind: Option<String>,
        json: bool,
    },
    Table {
        path: Option<PathBuf>,
        table: String,
        json: bool,
    },
    Relations {
        path: Option<PathBuf>,
        object: String,
        depth: usize,
        direction: Direction,
        json: bool,
    },
    Context {
        path: Option<PathBuf>,
        query: String,
        token_budget: usize,
        json: bool,
    },
    Diff {
        path: Option<PathBuf>,
        json: bool,
    },
    Impact {
        path: Option<PathBuf>,
        object: String,
        depth: usize,
        json: bool,
    },
    Analyze {
        path: Option<PathBuf>,
        scope: AnalysisScope,
        json: bool,
        format: AnalysisOutputFormat,
        output: Option<PathBuf>,
    },
    Install {
        targets: Vec<AgentKind>,
        location: Option<PathBuf>,
        yes: bool,
        dry_run: bool,
        print_config: bool,
    },
    Uninstall {
        targets: Vec<AgentKind>,
        location: Option<PathBuf>,
        dry_run: bool,
    },
    ServeMcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalysisOutputFormat {
    Text,
    Json,
    Markdown,
}

impl std::str::FromStr for AnalysisOutputFormat {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            _ => Err("analysis format must be text, json, or markdown".to_owned()),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<ParsedArgs> {
    let mut verbosity = LogVerbosity::Normal;
    let mut command = None;
    let mut pending_init = false;
    let mut pending_status = false;
    let mut pending_snapshot = false;
    let mut pending_sync = false;
    let mut pending_benchmark = false;
    let mut init_path = None;
    let mut init_force = false;
    let mut init_interactive = false;
    let mut init_yes = false;
    let mut status_path = None;
    let mut status_json = false;
    let mut snapshot_path = None;
    let mut snapshot_json = false;
    let mut snapshot_profile = None;
    let mut snapshot_max_rows_per_table = None;
    let mut snapshot_store_raw_samples = false;
    let mut sync_path = None;
    let mut sync_json = false;
    let mut benchmark_tables = 1_000_usize;
    let mut benchmark_columns_per_table = 4_usize;
    let mut benchmark_json = false;
    let mut pending_validate_sql = false;
    let mut validate_path = None;
    let mut validate_sql = None;
    let mut validate_file = None;
    let mut validate_dialect = SqlDialect::Postgres;
    let mut validate_json = false;
    let mut pending_search = false;
    let mut search_positionals = Vec::new();
    let mut search_kind = None;
    let mut search_json = false;
    let mut pending_table = false;
    let mut table_positionals = Vec::new();
    let mut table_json = false;
    let mut pending_relations = false;
    let mut relations_positionals = Vec::new();
    let mut relations_depth = 1_usize;
    let mut relations_direction = Direction::Both;
    let mut relations_json = false;
    let mut pending_context = false;
    let mut context_positionals = Vec::new();
    let mut context_budget = 800_usize;
    let mut context_json = false;
    let mut pending_diff = false;
    let mut diff_path = None;
    let mut diff_json = false;
    let mut pending_impact = false;
    let mut pending_analyze = false;
    let mut impact_positionals = Vec::new();
    let mut impact_depth = 2_usize;
    let mut impact_json = false;
    let mut analyze_path = None;
    let mut analyze_scope = AnalysisScope::All;
    let mut analyze_json = false;
    let mut analyze_format = AnalysisOutputFormat::Text;
    let mut analyze_output = None;
    let mut pending_install = false;
    let mut install_target_raw: Option<String> = None;
    let mut install_location = None;
    let mut install_yes = false;
    let mut install_dry_run = false;
    let mut install_print_config = false;
    let mut pending_uninstall = false;
    let mut uninstall_target_raw: Option<String> = None;
    let mut uninstall_location = None;
    let mut uninstall_dry_run = false;
    let mut pending_serve = false;
    let mut serve_mcp = false;
    let args = args.into_iter().collect::<Vec<_>>();
    let mut idx = 0;

    while idx < args.len() {
        let arg = args[idx].clone();
        match arg.as_str() {
            "--verbose" | "-v" => {
                if verbosity == LogVerbosity::Quiet {
                    return Err(DbGraphError::invalid_argument(
                        "`--verbose` cannot be used with `--quiet`",
                    ));
                }
                verbosity = LogVerbosity::Verbose;
            }
            "--quiet" | "-q" => {
                if verbosity == LogVerbosity::Verbose {
                    return Err(DbGraphError::invalid_argument(
                        "`--quiet` cannot be used with `--verbose`",
                    ));
                }
                verbosity = LogVerbosity::Quiet;
            }
            "--version" | "-V" | "version" => set_command(&mut command, Command::Version)?,
            "--help" | "-h" | "help" => set_command(&mut command, Command::Help)?,
            "init" => {
                set_command(
                    &mut command,
                    Command::Init {
                        path: None,
                        force: false,
                        interactive: false,
                        yes: false,
                    },
                )?;
                pending_init = true;
            }
            "status" => {
                set_command(
                    &mut command,
                    Command::Status {
                        path: None,
                        json: false,
                    },
                )?;
                pending_status = true;
            }
            "snapshot" => {
                set_command(
                    &mut command,
                    Command::Snapshot {
                        path: None,
                        json: false,
                        profile: None,
                        max_rows_per_table: None,
                        store_raw_samples: false,
                    },
                )?;
                pending_snapshot = true;
            }
            "sync" => {
                set_command(
                    &mut command,
                    Command::Sync {
                        path: None,
                        json: false,
                    },
                )?;
                pending_sync = true;
            }
            "benchmark" => {
                set_command(
                    &mut command,
                    Command::Benchmark {
                        tables: 1_000,
                        columns_per_table: 4,
                        json: false,
                    },
                )?;
                pending_benchmark = true;
            }
            "validate-sql" => {
                set_command(
                    &mut command,
                    Command::ValidateSql {
                        path: None,
                        sql: None,
                        file: None,
                        dialect: SqlDialect::Postgres,
                        json: false,
                    },
                )?;
                pending_validate_sql = true;
            }
            "search" => {
                set_command(
                    &mut command,
                    Command::Search {
                        path: None,
                        query: String::new(),
                        kind: None,
                        json: false,
                    },
                )?;
                pending_search = true;
            }
            "table" => {
                set_command(
                    &mut command,
                    Command::Table {
                        path: None,
                        table: String::new(),
                        json: false,
                    },
                )?;
                pending_table = true;
            }
            "relations" => {
                set_command(
                    &mut command,
                    Command::Relations {
                        path: None,
                        object: String::new(),
                        depth: 1,
                        direction: Direction::Both,
                        json: false,
                    },
                )?;
                pending_relations = true;
            }
            "context" => {
                set_command(
                    &mut command,
                    Command::Context {
                        path: None,
                        query: String::new(),
                        token_budget: 800,
                        json: false,
                    },
                )?;
                pending_context = true;
            }
            "diff" => {
                set_command(
                    &mut command,
                    Command::Diff {
                        path: None,
                        json: false,
                    },
                )?;
                pending_diff = true;
            }
            "impact" => {
                set_command(
                    &mut command,
                    Command::Impact {
                        path: None,
                        object: String::new(),
                        depth: 2,
                        json: false,
                    },
                )?;
                pending_impact = true;
            }
            "analyze" | "analyse" => {
                set_command(
                    &mut command,
                    Command::Analyze {
                        path: None,
                        scope: AnalysisScope::All,
                        json: false,
                        format: AnalysisOutputFormat::Text,
                        output: None,
                    },
                )?;
                pending_analyze = true;
            }
            "install" => {
                set_command(
                    &mut command,
                    Command::Install {
                        targets: AgentKind::all().to_vec(),
                        location: None,
                        yes: false,
                        dry_run: false,
                        print_config: false,
                    },
                )?;
                pending_install = true;
            }
            "uninstall" => {
                set_command(
                    &mut command,
                    Command::Uninstall {
                        targets: AgentKind::all().to_vec(),
                        location: None,
                        dry_run: false,
                    },
                )?;
                pending_uninstall = true;
            }
            "serve" => {
                set_command(&mut command, Command::ServeMcp)?;
                pending_serve = true;
            }
            "--mcp" if pending_serve => {
                serve_mcp = true;
            }
            "--force" | "-f" if pending_init => {
                init_force = true;
            }
            "--interactive" | "-i" if pending_init => {
                init_interactive = true;
            }
            "--yes" | "-y" if pending_init => {
                init_yes = true;
            }
            "--json" | "-j" if pending_status => {
                status_json = true;
            }
            "--json" | "-j" if pending_snapshot => {
                snapshot_json = true;
            }
            "--profile" if pending_snapshot => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--profile` requires a value")
                })?;
                snapshot_profile = Some(
                    value
                        .parse::<ProfilingMode>()
                        .map_err(DbGraphError::invalid_argument)?,
                );
            }
            "--max-rows-per-table" if pending_snapshot => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--max-rows-per-table` requires a value")
                })?;
                snapshot_max_rows_per_table = Some(parse_positive_u32(
                    value,
                    "`--max-rows-per-table` requires a positive integer",
                )?);
            }
            "--store-raw-samples" if pending_snapshot => {
                snapshot_store_raw_samples = true;
            }
            "--json" | "-j" if pending_sync => {
                sync_json = true;
            }
            "--json" | "-j" if pending_benchmark => {
                benchmark_json = true;
            }
            "--tables" if pending_benchmark => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| DbGraphError::invalid_argument("`--tables` requires a value"))?;
                benchmark_tables =
                    parse_positive_usize(value, "`--tables` requires a positive integer")?;
            }
            "--columns-per-table" if pending_benchmark => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--columns-per-table` requires a value")
                })?;
                benchmark_columns_per_table = parse_positive_usize(
                    value,
                    "`--columns-per-table` requires a positive integer",
                )?;
            }
            "--json" | "-j" if pending_validate_sql => {
                validate_json = true;
            }
            "--json" | "-j" if pending_search => search_json = true,
            "--json" | "-j" if pending_table => table_json = true,
            "--json" | "-j" if pending_relations => relations_json = true,
            "--json" | "-j" if pending_context => context_json = true,
            "--json" | "-j" if pending_diff => diff_json = true,
            "--json" | "-j" if pending_impact => impact_json = true,
            "--json" | "-j" if pending_analyze => {
                analyze_json = true;
                analyze_format = AnalysisOutputFormat::Json;
            }
            "--yes" | "-y" if pending_install => install_yes = true,
            "--dry-run" if pending_install => install_dry_run = true,
            "--dry-run" if pending_uninstall => uninstall_dry_run = true,
            "--print-config" if pending_install => install_print_config = true,
            "--target" | "--targets" if pending_install => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| DbGraphError::invalid_argument("`--target` requires a value"))?;
                install_target_raw = Some(value.clone());
            }
            "--target" | "--targets" if pending_uninstall => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| DbGraphError::invalid_argument("`--target` requires a value"))?;
                uninstall_target_raw = Some(value.clone());
            }
            "--location" if pending_install => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--location` requires a directory")
                })?;
                install_location = Some(PathBuf::from(value));
            }
            "--location" if pending_uninstall => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--location` requires a directory")
                })?;
                uninstall_location = Some(PathBuf::from(value));
            }
            value if pending_install && value.starts_with("--target=") => {
                install_target_raw = Some(value["--target=".len()..].to_owned());
            }
            value if pending_install && value.starts_with("--targets=") => {
                install_target_raw = Some(value["--targets=".len()..].to_owned());
            }
            value if pending_uninstall && value.starts_with("--target=") => {
                uninstall_target_raw = Some(value["--target=".len()..].to_owned());
            }
            value if pending_uninstall && value.starts_with("--targets=") => {
                uninstall_target_raw = Some(value["--targets=".len()..].to_owned());
            }
            value if pending_install && value.starts_with("--location=") => {
                install_location = Some(PathBuf::from(&value["--location=".len()..]));
            }
            value if pending_uninstall && value.starts_with("--location=") => {
                uninstall_location = Some(PathBuf::from(&value["--location=".len()..]));
            }
            "--kind" if pending_search => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| DbGraphError::invalid_argument("`--kind` requires a value"))?;
                search_kind = Some(value.clone());
            }
            "--depth" if pending_relations => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| DbGraphError::invalid_argument("`--depth` requires 1 or 2"))?;
                relations_depth = parse_depth(value)?;
            }
            "--direction" if pending_relations => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument(
                        "`--direction` requires incoming, outgoing, or both",
                    )
                })?;
                relations_direction = parse_direction(value)?;
            }
            "--tokens" | "--token-budget" if pending_context => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--tokens` requires a positive integer")
                })?;
                context_budget = value.parse::<usize>().map_err(|_| {
                    DbGraphError::invalid_argument("`--tokens` requires a positive integer")
                })?;
            }
            "--depth" if pending_impact => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| DbGraphError::invalid_argument("`--depth` requires 1 or 2"))?;
                impact_depth = parse_depth(value)?;
            }
            "--scope" if pending_analyze => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument(
                        "`--scope` requires all, risk, quality, or performance",
                    )
                })?;
                analyze_scope = value.parse().map_err(DbGraphError::invalid_argument)?;
            }
            "--format" if pending_analyze => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--format` requires text, json, or markdown")
                })?;
                analyze_format = value.parse().map_err(DbGraphError::invalid_argument)?;
            }
            "--output" if pending_analyze => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--output` requires a file path")
                })?;
                analyze_output = Some(PathBuf::from(value));
            }
            "--sql" if pending_validate_sql => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--sql` requires a SQL string")
                })?;
                validate_sql = Some(value.clone());
            }
            "--file" if pending_validate_sql => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument("`--file` requires a SQL file path")
                })?;
                validate_file = Some(PathBuf::from(value));
            }
            "--dialect" if pending_validate_sql => {
                idx += 1;
                let value = args.get(idx).ok_or_else(|| {
                    DbGraphError::invalid_argument(
                        "`--dialect` requires postgres, mysql, or generic",
                    )
                })?;
                validate_dialect = value.parse().map_err(DbGraphError::invalid_argument)?;
            }
            _ => {
                if pending_init && !arg.starts_with('-') && init_path.is_none() {
                    init_path = Some(PathBuf::from(arg));
                } else if pending_status && !arg.starts_with('-') && status_path.is_none() {
                    status_path = Some(PathBuf::from(arg));
                } else if pending_snapshot && !arg.starts_with('-') && snapshot_path.is_none() {
                    snapshot_path = Some(PathBuf::from(arg));
                } else if pending_sync && !arg.starts_with('-') && sync_path.is_none() {
                    sync_path = Some(PathBuf::from(arg));
                } else if pending_validate_sql && !arg.starts_with('-') && validate_path.is_none() {
                    validate_path = Some(PathBuf::from(arg));
                } else if pending_search && !arg.starts_with('-') {
                    search_positionals.push(arg);
                } else if pending_table && !arg.starts_with('-') {
                    table_positionals.push(arg);
                } else if pending_relations && !arg.starts_with('-') {
                    relations_positionals.push(arg);
                } else if pending_context && !arg.starts_with('-') {
                    context_positionals.push(arg);
                } else if pending_diff && !arg.starts_with('-') && diff_path.is_none() {
                    if arg != "latest" && arg != "previous" {
                        diff_path = Some(PathBuf::from(arg));
                    }
                } else if pending_impact && !arg.starts_with('-') {
                    impact_positionals.push(arg);
                } else if pending_analyze && !arg.starts_with('-') && analyze_path.is_none() {
                    analyze_path = Some(PathBuf::from(arg));
                } else if pending_install || pending_uninstall {
                    return Err(DbGraphError::invalid_argument(format!(
                        "unknown install option `{arg}`"
                    )));
                } else if pending_serve {
                    return Err(DbGraphError::invalid_argument(
                        "`serve` currently supports only `--mcp`",
                    ));
                } else {
                    return Err(DbGraphError::invalid_argument(format!(
                        "unknown command or option `{arg}`"
                    )));
                }
            }
        }
        idx += 1;
    }

    if pending_init {
        command = Some(Command::Init {
            path: init_path,
            force: init_force,
            interactive: init_interactive,
            yes: init_yes,
        });
    }
    if pending_status {
        command = Some(Command::Status {
            path: status_path,
            json: status_json,
        });
    }
    if pending_snapshot {
        command = Some(Command::Snapshot {
            path: snapshot_path,
            json: snapshot_json,
            profile: snapshot_profile,
            max_rows_per_table: snapshot_max_rows_per_table,
            store_raw_samples: snapshot_store_raw_samples,
        });
    }
    if pending_sync {
        command = Some(Command::Sync {
            path: sync_path,
            json: sync_json,
        });
    }
    if pending_benchmark {
        command = Some(Command::Benchmark {
            tables: benchmark_tables,
            columns_per_table: benchmark_columns_per_table,
            json: benchmark_json,
        });
    }
    if pending_validate_sql {
        if validate_sql.is_some() && validate_file.is_some() {
            return Err(DbGraphError::invalid_argument(
                "`validate-sql` accepts only one of `--sql` or `--file`",
            ));
        }
        if validate_sql.is_none() && validate_file.is_none() {
            return Err(DbGraphError::invalid_argument(
                "`validate-sql` requires `--sql <SQL>` or `--file <PATH>`",
            ));
        }
        command = Some(Command::ValidateSql {
            path: validate_path,
            sql: validate_sql,
            file: validate_file,
            dialect: validate_dialect,
            json: validate_json,
        });
    }
    if pending_search {
        let (path, query) = split_optional_path_and_query(&search_positionals, "search")?;
        command = Some(Command::Search {
            path,
            query,
            kind: search_kind,
            json: search_json,
        });
    }
    if pending_table {
        let (path, table) = split_optional_path_and_query(&table_positionals, "table")?;
        command = Some(Command::Table {
            path,
            table,
            json: table_json,
        });
    }
    if pending_relations {
        let (path, object) = split_optional_path_and_query(&relations_positionals, "relations")?;
        command = Some(Command::Relations {
            path,
            object,
            depth: relations_depth,
            direction: relations_direction,
            json: relations_json,
        });
    }
    if pending_context {
        let (path, query) = split_optional_path_and_query(&context_positionals, "context")?;
        command = Some(Command::Context {
            path,
            query,
            token_budget: context_budget,
            json: context_json,
        });
    }
    if pending_diff {
        command = Some(Command::Diff {
            path: diff_path,
            json: diff_json,
        });
    }
    if pending_impact {
        let (path, object) = split_optional_path_and_query(&impact_positionals, "impact")?;
        command = Some(Command::Impact {
            path,
            object,
            depth: impact_depth,
            json: impact_json,
        });
    }
    if pending_analyze {
        command = Some(Command::Analyze {
            path: analyze_path,
            scope: analyze_scope,
            json: analyze_json,
            format: analyze_format,
            output: analyze_output,
        });
    }
    if pending_install {
        command = Some(Command::Install {
            targets: parse_agent_kinds(install_target_raw.as_deref())
                .map_err(DbGraphError::invalid_argument)?,
            location: install_location,
            yes: install_yes,
            dry_run: install_dry_run,
            print_config: install_print_config,
        });
    }
    if pending_uninstall {
        command = Some(Command::Uninstall {
            targets: parse_agent_kinds(uninstall_target_raw.as_deref())
                .map_err(DbGraphError::invalid_argument)?,
            location: uninstall_location,
            dry_run: uninstall_dry_run,
        });
    }
    if pending_serve {
        if !serve_mcp {
            return Err(DbGraphError::invalid_argument(
                "`serve` requires `--mcp` in this release",
            ));
        }
        command = Some(Command::ServeMcp);
    }

    Ok(ParsedArgs {
        verbosity,
        command: command.unwrap_or(Command::Help),
    })
}

fn split_optional_path_and_query(
    positionals: &[String],
    command: &str,
) -> Result<(Option<PathBuf>, String)> {
    match positionals {
        [] => Err(DbGraphError::invalid_argument(format!(
            "`{command}` requires a query or object"
        ))),
        [query] => Ok((None, query.clone())),
        [first, rest @ ..] if looks_like_path(first) => {
            Ok((Some(PathBuf::from(first)), rest.join(" ")))
        }
        _ => Ok((None, positionals.join(" "))),
    }
}

fn looks_like_path(value: &str) -> bool {
    let path = Path::new(value);
    path.exists()
        || value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.contains('/')
        || value.contains('\\')
}

fn parse_depth(value: &str) -> Result<usize> {
    let depth = value
        .parse::<usize>()
        .map_err(|_| DbGraphError::invalid_argument("`--depth` requires 1 or 2"))?;
    if (1..=2).contains(&depth) {
        Ok(depth)
    } else {
        Err(DbGraphError::invalid_argument(
            "`--depth` supports only 1 or 2",
        ))
    }
}

fn parse_positive_usize(value: &str, message: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| DbGraphError::invalid_argument(message))?;
    if parsed == 0 {
        Err(DbGraphError::invalid_argument(message))
    } else {
        Ok(parsed)
    }
}

fn parse_positive_u32(value: &str, message: &str) -> Result<u32> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| DbGraphError::invalid_argument(message))?;
    if parsed == 0 {
        Err(DbGraphError::invalid_argument(message))
    } else {
        Ok(parsed)
    }
}

fn parse_direction(value: &str) -> Result<Direction> {
    match value.to_ascii_lowercase().as_str() {
        "incoming" => Ok(Direction::Incoming),
        "outgoing" => Ok(Direction::Outgoing),
        "both" => Ok(Direction::Both),
        _ => Err(DbGraphError::invalid_argument(
            "`--direction` requires incoming, outgoing, or both",
        )),
    }
}

fn set_command(slot: &mut Option<Command>, next: Command) -> Result<()> {
    if slot.replace(next).is_some() {
        return Err(DbGraphError::invalid_argument(
            "only one command can be supplied",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitSummary {
    dbgraph_dir: PathBuf,
    config_path: PathBuf,
    instructions_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct InitRunSummary {
    init: InitSummary,
    snapshot: Option<SnapshotSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct InitOptions {
    config: DbGraphConfig,
    configure_agent: bool,
    run_snapshot: bool,
}

impl InitOptions {
    fn interactive_defaults() -> Self {
        Self {
            configure_agent: true,
            run_snapshot: false,
            ..Self::default()
        }
    }
}

fn init_project(
    project_root: impl AsRef<Path>,
    force: bool,
    options: &InitOptions,
) -> Result<InitSummary> {
    let project_root = project_root.as_ref();
    fs::create_dir_all(project_root).map_err(|source| DbGraphError::io(project_root, source))?;

    let context = ProjectContext::from_project_root(project_root);
    fs::create_dir_all(context.dbgraph_dir())
        .map_err(|source| DbGraphError::io(context.dbgraph_dir(), source))?;
    fs::create_dir_all(context.snapshots_dir())
        .map_err(|source| DbGraphError::io(context.snapshots_dir(), source))?;
    fs::create_dir_all(context.instructions_dir())
        .map_err(|source| DbGraphError::io(context.instructions_dir(), source))?;

    let config_path = context.config_path();
    if config_path.exists() && !force {
        return Err(DbGraphError::invalid_config(format!(
            "{} already exists; re-run with `dbgraph init --force` to replace it",
            config_path.display()
        )));
    }

    options.config.save(&context)?;
    if options.configure_agent {
        write_instruction_fragments(&context)?;
    }

    Ok(InitSummary {
        dbgraph_dir: context.dbgraph_dir().to_path_buf(),
        instructions_dir: context.instructions_dir(),
        config_path,
    })
}

fn init_project_with_optional_snapshot(
    project_root: impl AsRef<Path>,
    force: bool,
    options: &InitOptions,
    snapshot_runner: impl FnOnce(&Path) -> Result<Option<SnapshotSummary>>,
) -> Result<InitRunSummary> {
    let project_root = project_root.as_ref();
    let init = init_project(project_root, force, options)?;
    let snapshot = if options.run_snapshot {
        snapshot_runner(project_root)?
    } else {
        None
    };
    Ok(InitRunSummary { init, snapshot })
}

fn write_instruction_fragments(context: &ProjectContext) -> Result<()> {
    fs::create_dir_all(context.instructions_dir())
        .map_err(|source| DbGraphError::io(context.instructions_dir(), source))?;

    for (target, content) in render_all_instruction_fragments() {
        let path = context.instructions_dir().join(target.file_name());
        fs::write(&path, content).map_err(|source| DbGraphError::io(path, source))?;
    }

    Ok(())
}

fn prompt_init_options() -> Result<InitOptions> {
    let mut stdout = io::stdout();
    let stdin = io::stdin();
    let mut stdin = stdin.lock().lines();

    let provider = prompt(
        &mut stdout,
        &mut stdin,
        "Database provider [postgres/mysql/sql-server/sqlite] (postgres): ",
        "postgres",
    )?;
    let provider_kind = provider
        .parse::<DatabaseProviderKind>()
        .map_err(DbGraphError::invalid_config)?;

    let source = prompt(
        &mut stdout,
        &mut stdin,
        "Connection source [DATABASE_URL/env/manual] (DATABASE_URL): ",
        "DATABASE_URL",
    )?;
    let mut database = DatabaseConfig {
        provider: provider_kind.to_string(),
        ..DatabaseConfig::default()
    };
    match source.as_str() {
        "DATABASE_URL" | "database_url" => {
            database.connection_env = Some("DATABASE_URL".to_owned());
            database.connection_string = None;
        }
        "env" | ".env" | "appsettings" => {
            let env_name = prompt(
                &mut stdout,
                &mut stdin,
                "Connection environment variable name (DATABASE_URL): ",
                "DATABASE_URL",
            )?;
            database.connection_env = Some(env_name);
            database.connection_string = None;
        }
        "manual" => {
            let connection_string = prompt(&mut stdout, &mut stdin, "Connection string: ", "")?;
            if connection_string.is_empty() {
                return Err(DbGraphError::invalid_config(
                    "manual connection source requires a connection string",
                ));
            }
            database.connection_env = None;
            database.connection_string = Some(connection_string);
        }
        _ => {
            return Err(DbGraphError::invalid_config(format!(
                "unsupported connection source `{source}`; supported values: DATABASE_URL, env, manual"
            )));
        }
    }

    let configure_agent = prompt_bool(
        &mut stdout,
        &mut stdin,
        "Generate agent instruction fragments? [Y/n]: ",
        true,
    )?;
    let run_snapshot = prompt_bool(&mut stdout, &mut stdin, "Run snapshot now? [y/N]: ", false)?;

    Ok(InitOptions {
        config: DbGraphConfig {
            database,
            ..DbGraphConfig::default()
        },
        configure_agent,
        run_snapshot,
    })
}

fn prompt(
    stdout: &mut impl Write,
    lines: &mut impl Iterator<Item = io::Result<String>>,
    message: &str,
    default: &str,
) -> Result<String> {
    stdout
        .write_all(message.as_bytes())
        .map_err(|source| DbGraphError::io("<stdout>", source))?;
    stdout
        .flush()
        .map_err(|source| DbGraphError::io("<stdout>", source))?;
    let value = lines
        .next()
        .transpose()
        .map_err(|source| DbGraphError::io("<stdin>", source))?
        .unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(value.to_owned())
    }
}

fn prompt_bool(
    stdout: &mut impl Write,
    lines: &mut impl Iterator<Item = io::Result<String>>,
    message: &str,
    default: bool,
) -> Result<bool> {
    let default_text = if default { "yes" } else { "no" };
    let value = prompt(stdout, lines, message, default_text)?;
    match value.to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" => Ok(true),
        "n" | "no" | "false" => Ok(false),
        _ => Err(DbGraphError::invalid_argument(format!(
            "expected yes or no, got `{value}`"
        ))),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusReport {
    project_root: PathBuf,
    dbgraph_dir: PathBuf,
    initialized: bool,
    config_path: PathBuf,
    config_present: bool,
    provider: Option<String>,
    snapshot_count: usize,
    latest_snapshot: Option<String>,
    graph_db_path: PathBuf,
    graph_db_present: bool,
    mcp_suggestion: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotSummary {
    project_root: PathBuf,
    snapshot_path: PathBuf,
    graph_db_path: PathBuf,
    provider: String,
    database_name: String,
    object_count: usize,
    table_count: usize,
    column_count: usize,
    edge_count: usize,
    table_profile_count: usize,
    column_profile_count: usize,
    sql_artifact_count: usize,
    profiling_mode: String,
    schema_hash: Option<String>,
}

fn run_snapshot(start: impl AsRef<Path>) -> Result<SnapshotSummary> {
    run_snapshot_with_options(start, SnapshotCliOptions::default())
}

#[derive(Debug, Clone, Copy, Default)]
struct SnapshotCliOptions {
    profile: Option<ProfilingMode>,
    max_rows_per_table: Option<u32>,
    store_raw_samples: bool,
}

fn run_snapshot_with_options(
    start: impl AsRef<Path>,
    cli_options: SnapshotCliOptions,
) -> Result<SnapshotSummary> {
    let start = start.as_ref();
    let context = ProjectContext::discover_from(start)?
        .unwrap_or_else(|| ProjectContext::from_project_root(start));
    let config = load_config_with_snapshot_overrides(&context, cli_options)?;
    let (snapshot, sql_artifacts, profiling_options) = capture_snapshot(&context, &config)?;
    write_snapshot_and_index(
        &context,
        &config,
        &snapshot,
        &sql_artifacts,
        &profiling_options,
    )
}

fn load_config_with_snapshot_overrides(
    context: &ProjectContext,
    cli_options: SnapshotCliOptions,
) -> Result<DbGraphConfig> {
    let mut config = DbGraphConfig::load(context)?;
    if let Some(profile) = cli_options.profile {
        config.snapshot.profiling_mode = profile;
        config.snapshot.sample_rows = profile == ProfilingMode::Sample;
    }
    if let Some(max_rows_per_table) = cli_options.max_rows_per_table {
        config.snapshot.max_rows_per_table = max_rows_per_table;
    }
    if cli_options.store_raw_samples {
        config.security.store_raw_samples = true;
    }
    config.validate()?;
    config.database.provider_kind()?;
    Ok(config)
}

fn effective_profiling_options(config: &DbGraphConfig) -> ProfilingOptions {
    ProfilingOptions {
        mode: config.snapshot.profiling_mode,
        max_rows_per_table: config.snapshot.max_rows_per_table,
        mask_pii: config.security.mask_pii,
        store_raw_samples: config.security.store_raw_samples,
    }
}

fn capture_snapshot(
    context: &ProjectContext,
    config: &DbGraphConfig,
) -> Result<(DbSnapshot, Vec<StoredSqlArtifactRecord>, ProfilingOptions)> {
    let profiling_options = effective_profiling_options(config);

    let registry = ProviderRegistry;
    let provider = registry.get(&config.database.provider).ok_or_else(|| {
        DbGraphError::invalid_config(format!(
            "provider `{}` is not registered",
            config.database.provider
        ))
    })?;
    let connection_url = resolve_connection_url(&config.database)?;
    let connection = ProviderConnectionConfig::from_url(connection_url);
    let mut snapshot = provider.snapshot(&connection)?;
    let timestamp = now_unix_ms()?;
    snapshot.created_at_unix_ms = timestamp;
    snapshot.id = format!(
        "{}:{}:{timestamp}",
        snapshot.provider, snapshot.database_name
    );

    let sql_artifacts = enrich_snapshot_with_sql(&mut snapshot, context)?;
    snapshot = apply_profiling_policy(snapshot, &profiling_options);
    if config.security.mask_pii {
        let detector = PiiDetector::new(&PiiRuleConfig {
            custom_sensitive_terms: config.security.custom_sensitive_terms.clone(),
        });
        snapshot = apply_pii_profiles(snapshot, &detector);
    }
    Ok((snapshot, sql_artifacts, profiling_options))
}

fn write_snapshot_and_index(
    context: &ProjectContext,
    config: &DbGraphConfig,
    snapshot: &DbSnapshot,
    sql_artifacts: &[StoredSqlArtifactRecord],
    profiling_options: &ProfilingOptions,
) -> Result<SnapshotSummary> {
    let snapshot_path =
        SnapshotStore::new(context).write_snapshot(snapshot, config.snapshot.pretty_json)?;
    let stored_snapshot = SnapshotStore::new(context).read_snapshot(&snapshot_path)?;
    let mut repository = GraphRepository::open(context.graph_db_path())?;
    let index_summary = rebuild_index(&mut repository, &stored_snapshot)?;
    repository.insert_sql_artifacts(sql_artifacts)?;

    Ok(SnapshotSummary {
        project_root: context.project_root().to_path_buf(),
        snapshot_path,
        graph_db_path: context.graph_db_path(),
        provider: stored_snapshot.provider.clone(),
        database_name: stored_snapshot.database_name.clone(),
        object_count: index_summary.object_count,
        table_count: stored_snapshot
            .objects
            .iter()
            .filter(|object| object.kind.as_str() == "table")
            .count(),
        column_count: stored_snapshot
            .objects
            .iter()
            .filter(|object| object.kind.as_str() == "column")
            .count(),
        edge_count: index_summary.edge_count,
        table_profile_count: index_summary.table_profile_count,
        column_profile_count: index_summary.column_profile_count,
        sql_artifact_count: sql_artifacts.len(),
        profiling_mode: profiling_options.mode.to_string(),
        schema_hash: stored_snapshot.schema_hash,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncSummary {
    project_root: PathBuf,
    plan: SyncPlan,
    snapshot: Option<SnapshotSummary>,
}

fn sync_project(start: impl AsRef<Path>) -> Result<SyncSummary> {
    let start = start.as_ref();
    let context = ProjectContext::discover_from(start)?
        .unwrap_or_else(|| ProjectContext::from_project_root(start));
    let previous = SnapshotStore::new(&context).read_latest()?;
    let config = load_config_with_snapshot_overrides(&context, SnapshotCliOptions::default())?;
    let (snapshot, sql_artifacts, profiling_options) = capture_snapshot(&context, &config)?;
    let plan = plan_incremental_sync(previous.as_ref(), &snapshot)?;
    let snapshot = if plan.can_skip_rebuild() {
        None
    } else {
        Some(write_snapshot_and_index(
            &context,
            &config,
            &snapshot,
            &sql_artifacts,
            &profiling_options,
        )?)
    };

    Ok(SyncSummary {
        project_root: context.project_root().to_path_buf(),
        snapshot,
        plan,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    tables: usize,
    columns_per_table: usize,
    object_count: usize,
    edge_count: usize,
    schema_hash: String,
}

fn benchmark_project(tables: usize, columns_per_table: usize) -> Result<BenchmarkReport> {
    let snapshot = synthetic_schema_snapshot(SyntheticSchemaOptions {
        table_count: tables,
        columns_per_table,
    });
    let schema_hash = dbgraph_core::snapshot::compute_schema_hash(&snapshot)?;
    Ok(BenchmarkReport {
        tables,
        columns_per_table,
        object_count: snapshot.objects.len(),
        edge_count: snapshot.edges.len(),
        schema_hash,
    })
}

fn enrich_snapshot_with_sql(
    snapshot: &mut dbgraph_core::model::DbSnapshot,
    context: &ProjectContext,
) -> Result<Vec<StoredSqlArtifactRecord>> {
    let sources = scan_sql_files(context.project_root(), &ScanOptions::default())?;
    let dialect = dialect_for_provider(&snapshot.provider);
    let mut artifacts = Vec::new();
    for source in sources {
        let parser = SqlParser::new(dialect);
        let parsed = parser.parse(&source.raw_sql)?;
        let analysis = analyze_sql(&source.raw_sql, dialect)?;
        let source_path = source.source_path.to_string_lossy().replace('\\', "/");
        let mut graph = sql_artifact_to_graph(&snapshot.id, &source_path, &parsed, &analysis)?;
        resolve_sql_edge_targets(snapshot, &mut graph.edges);
        snapshot.objects.push(graph.object);
        snapshot.edges.extend(graph.edges);
        artifacts.push(StoredSqlArtifactRecord {
            id: graph.artifact.id,
            snapshot_id: graph.artifact.snapshot_id,
            source_kind: graph.artifact.source_kind,
            source_path: graph.artifact.source_path,
            dialect: graph.artifact.dialect,
            fingerprint: graph.artifact.fingerprint,
            normalized_sql: graph.artifact.normalized_sql,
            ast_json: graph.artifact.ast_json,
            analysis_json: graph.artifact.analysis_json,
        });
    }
    Ok(artifacts)
}

fn dialect_for_provider(provider: &str) -> SqlDialect {
    match provider {
        "postgres" => SqlDialect::Postgres,
        "mysql" => SqlDialect::MySql,
        _ => SqlDialect::Generic,
    }
}

fn resolve_connection_url(config: &DatabaseConfig) -> Result<String> {
    if let Some(env_name) = config.connection_env.as_deref() {
        if let Ok(value) = env::var(env_name) {
            if !value.trim().is_empty() {
                return Ok(value);
            }
        }
    }
    config.connection_string.clone().ok_or_else(|| {
        DbGraphError::invalid_config(
            "database connection string is missing; set DATABASE_URL or database.connectionString",
        )
    })
}

fn print_snapshot_summary(summary: &SnapshotSummary) {
    println!("DbGraph snapshot complete");
    println!("Project: {}", summary.project_root.display());
    println!("Provider: {}", summary.provider);
    println!("Database: {}", summary.database_name);
    println!("Snapshot: {}", summary.snapshot_path.display());
    println!("Graph index: {}", summary.graph_db_path.display());
    println!("Objects: {}", summary.object_count);
    println!("Tables: {}", summary.table_count);
    println!("Columns: {}", summary.column_count);
    println!("Edges: {}", summary.edge_count);
    println!("Table profiles: {}", summary.table_profile_count);
    println!("Column profiles: {}", summary.column_profile_count);
    println!("SQL artifacts: {}", summary.sql_artifact_count);
    println!("Profiling mode: {}", summary.profiling_mode);
    if let Some(hash) = &summary.schema_hash {
        println!("Schema hash: {hash}");
    }
}

fn print_sync_summary(summary: &SyncSummary) {
    println!("DbGraph sync complete");
    println!("Project: {}", summary.project_root.display());
    match &summary.plan {
        SyncPlan::Unchanged { schema_hash } => {
            println!("Schema unchanged: {schema_hash}");
            println!("Skipped snapshot write and graph index rebuild");
        }
        SyncPlan::Changed {
            previous_hash,
            next_hash,
        } => {
            println!("Schema changed");
            println!(
                "Previous hash: {}",
                previous_hash.as_deref().unwrap_or("<none>")
            );
            println!("Next hash: {next_hash}");
        }
    }
}

fn print_benchmark_report(report: &BenchmarkReport) {
    println!("DbGraph benchmark schema");
    println!("Tables: {}", report.tables);
    println!("Columns per table: {}", report.columns_per_table);
    println!("Objects: {}", report.object_count);
    println!("Edges: {}", report.edge_count);
    println!("Schema hash: {}", report.schema_hash);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidateSqlReport {
    valid: bool,
    dialect: String,
    normalized_sql: String,
    diagnostics: Vec<String>,
    unresolved: Vec<UnresolvedSqlReference>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnresolvedSqlReference {
    kind: String,
    name: String,
    suggestions: Vec<String>,
}

fn read_sql_input(sql: Option<String>, file: Option<&Path>) -> Result<String> {
    match (sql, file) {
        (Some(sql), None) => Ok(sql),
        (None, Some(file)) => {
            fs::read_to_string(file).map_err(|source| DbGraphError::io(file, source))
        }
        (Some(_), Some(_)) => Err(DbGraphError::invalid_argument(
            "`validate-sql` accepts only one of `--sql` or `--file`",
        )),
        (None, None) => Err(DbGraphError::invalid_argument(
            "`validate-sql` requires `--sql <SQL>` or `--file <PATH>`",
        )),
    }
}

fn validate_sql(
    start: impl AsRef<Path>,
    sql: &str,
    dialect: SqlDialect,
) -> Result<ValidateSqlReport> {
    let parsed = SqlParser::new(dialect).parse(sql)?;
    let analysis = analyze_sql(sql, dialect)?;
    let context = ProjectContext::discover_from(start.as_ref())?
        .unwrap_or_else(|| ProjectContext::from_project_root(start.as_ref()));
    let latest = SnapshotStore::new(&context).read_latest()?;
    let mut unresolved = Vec::new();
    if let Some(snapshot) = latest {
        let repository = GraphRepository::open(context.graph_db_path())?;
        for reference in analysis.references {
            if !reference_exists(&snapshot, &reference.object_name) {
                unresolved.push(UnresolvedSqlReference {
                    kind: reference.kind.as_str().to_owned(),
                    name: reference.object_name.clone(),
                    suggestions: suggest_objects(&snapshot, &repository, &reference.object_name)?,
                });
            }
        }
    }

    Ok(ValidateSqlReport {
        valid: parsed.status == dbgraph_sql::ParseStatus::Parsed,
        dialect: dialect.as_str().to_owned(),
        normalized_sql: parsed.normalized_sql,
        diagnostics: parsed
            .diagnostics
            .into_iter()
            .chain(analysis.diagnostics)
            .map(|diagnostic| diagnostic.message)
            .collect(),
        unresolved,
    })
}

fn reference_exists(snapshot: &dbgraph_core::model::DbSnapshot, name: &str) -> bool {
    let normalized = normalize_sql_name(name);
    snapshot.objects.iter().any(|object| {
        matches!(
            object.kind,
            dbgraph_core::model::DbObjectKind::Table
                | dbgraph_core::model::DbObjectKind::View
                | dbgraph_core::model::DbObjectKind::MaterializedView
                | dbgraph_core::model::DbObjectKind::Column
        ) && (normalize_sql_name(&object.full_name) == normalized
            || normalize_sql_name(&object.name) == normalized)
    })
}

fn suggest_objects(
    snapshot: &dbgraph_core::model::DbSnapshot,
    repository: &GraphRepository,
    name: &str,
) -> Result<Vec<String>> {
    let normalized = normalize_sql_name(name);
    let singular = normalized.trim_end_matches('s').to_owned();
    let plural = format!("{singular}s");
    let (table_hint, column_hint) = table_column_hint(name);
    let mut suggestions = snapshot
        .objects
        .iter()
        .filter(|object| {
            matches!(
                object.kind,
                dbgraph_core::model::DbObjectKind::Table
                    | dbgraph_core::model::DbObjectKind::View
                    | dbgraph_core::model::DbObjectKind::MaterializedView
                    | dbgraph_core::model::DbObjectKind::Column
            )
        })
        .filter(|object| {
            let object_name = normalize_sql_name(&object.name);
            if object.kind == dbgraph_core::model::DbObjectKind::Column {
                let table_matches = table_hint.as_ref().map_or(true, |hint| {
                    object
                        .table_name
                        .as_deref()
                        .is_some_and(|table| normalize_sql_name(table) == *hint)
                        || object.full_name.to_ascii_lowercase().contains(hint)
                });
                let column = column_hint.as_deref().unwrap_or(&normalized);
                table_matches
                    && (object_name == column
                        || object_name.contains(column)
                        || edit_distance(&object_name, column) <= 2)
            } else {
                object_name == singular || object_name == plural || object_name.contains(&singular)
            }
        })
        .map(|object| object.full_name.clone())
        .collect::<Vec<_>>();
    for object in repository.search_objects(&normalized)? {
        if !suggestions.contains(&object.full_name) {
            suggestions.push(object.full_name);
        }
    }
    suggestions.sort();
    suggestions.dedup();
    suggestions.truncate(5);
    Ok(suggestions)
}

fn table_column_hint(name: &str) -> (Option<String>, Option<String>) {
    let parts = name
        .split('.')
        .map(normalize_sql_name)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [table, column] | [_, table, column] => (Some(table.clone()), Some(column.clone())),
        [column] => (None, Some(column.clone())),
        _ => (None, None),
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut costs = (0..=right.len()).collect::<Vec<_>>();
    for (left_idx, left_char) in left.chars().enumerate() {
        let mut previous = costs[0];
        costs[0] = left_idx + 1;
        for (right_idx, right_char) in right.chars().enumerate() {
            let insert = costs[right_idx + 1] + 1;
            let delete = costs[right_idx] + 1;
            let replace = previous + usize::from(left_char != right_char);
            previous = costs[right_idx + 1];
            costs[right_idx + 1] = insert.min(delete).min(replace);
        }
    }
    *costs.last().unwrap_or(&0)
}

fn normalize_sql_name(value: &str) -> String {
    value
        .rsplit('.')
        .next()
        .unwrap_or(value)
        .trim_matches('"')
        .trim_matches('`')
        .to_ascii_lowercase()
}

fn print_validate_sql_report(report: &ValidateSqlReport) {
    println!("SQL validation");
    println!("Dialect: {}", report.dialect);
    println!("Parse: {}", if report.valid { "valid" } else { "invalid" });
    if !report.diagnostics.is_empty() {
        println!("Diagnostics:");
        for diagnostic in &report.diagnostics {
            println!("  - {diagnostic}");
        }
    }
    if !report.unresolved.is_empty() {
        println!("Unresolved references:");
        for item in &report.unresolved {
            if item.suggestions.is_empty() {
                println!("  - {} {}", item.kind, item.name);
            } else {
                println!(
                    "  - {} {} (suggestions: {})",
                    item.kind,
                    item.name,
                    item.suggestions.join(", ")
                );
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchReport {
    query: String,
    results: Vec<SearchResult>,
}

fn search_project(
    start: impl AsRef<Path>,
    query: &str,
    kind: Option<&str>,
) -> Result<SearchReport> {
    let context = discover_context(start.as_ref())?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let query = kind.map_or_else(|| query.to_owned(), |kind| format!("kind:{kind} {query}"));
    Ok(SearchReport {
        results: search_snapshot(&snapshot, &query, &SearchOptions::default()),
        query,
    })
}

fn print_search_report(report: &SearchReport) {
    println!("DbGraph search: {}", report.query);
    for result in &report.results {
        println!(
            "- {} {} :: {}",
            result.kind, result.full_name, result.summary
        );
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TableReport {
    table: String,
    columns: Vec<ColumnReport>,
    constraints: Vec<ObjectSummary>,
    indexes: Vec<ObjectSummary>,
    profile: Option<TableProfile>,
    incoming_relations: Vec<EdgeSummary>,
    outgoing_relations: Vec<EdgeSummary>,
    suggestions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ColumnReport {
    name: String,
    full_name: String,
    data_type: Option<String>,
    nullable: Option<bool>,
    default: Option<String>,
    profile: Option<ColumnProfile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ObjectSummary {
    kind: String,
    full_name: String,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EdgeSummary {
    kind: String,
    from: String,
    to: String,
    confidence: f64,
    evidence: Vec<String>,
}

fn table_project(start: impl AsRef<Path>, table_name: &str) -> Result<TableReport> {
    let context = discover_context(start.as_ref())?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let Some(table) = resolve_table(&snapshot, table_name) else {
        return Ok(TableReport {
            table: table_name.to_owned(),
            columns: Vec::new(),
            constraints: Vec::new(),
            indexes: Vec::new(),
            profile: None,
            incoming_relations: Vec::new(),
            outgoing_relations: Vec::new(),
            suggestions: table_suggestions(&snapshot, table_name),
        });
    };
    let columns = snapshot
        .objects
        .iter()
        .filter(|object| {
            object.kind == DbObjectKind::Column
                && object.table_name.as_deref()
                    == table.table_name.as_deref().or(Some(table.name.as_str()))
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
        .collect::<Vec<_>>();
    let constraints = snapshot
        .objects
        .iter()
        .filter(|object| {
            matches!(
                object.kind,
                DbObjectKind::PrimaryKey
                    | DbObjectKind::ForeignKey
                    | DbObjectKind::UniqueConstraint
                    | DbObjectKind::CheckConstraint
            ) && object.table_name == table.table_name
        })
        .map(object_summary)
        .collect();
    let indexes = snapshot
        .objects
        .iter()
        .filter(|object| {
            object.kind == DbObjectKind::Index && object.table_name == table.table_name
        })
        .map(object_summary)
        .collect();
    Ok(TableReport {
        table: table.full_name.clone(),
        columns,
        constraints,
        indexes,
        profile: snapshot
            .table_profiles
            .iter()
            .find(|profile| profile.object_id == table.id)
            .cloned(),
        incoming_relations: snapshot
            .edges
            .iter()
            .filter(|edge| edge.to_object_id == table.id)
            .map(|edge| edge_summary(&snapshot, edge))
            .collect(),
        outgoing_relations: snapshot
            .edges
            .iter()
            .filter(|edge| edge.from_object_id == table.id)
            .map(|edge| edge_summary(&snapshot, edge))
            .collect(),
        suggestions: Vec::new(),
    })
}

fn print_table_report(report: &TableReport) {
    println!("Table: {}", report.table);
    if !report.suggestions.is_empty() {
        println!("Not found. Suggestions: {}", report.suggestions.join(", "));
        return;
    }
    println!("Columns:");
    for column in &report.columns {
        println!(
            "- {} {:?} nullable={:?} default={:?}",
            column.name, column.data_type, column.nullable, column.default
        );
    }
    println!("Constraints: {}", report.constraints.len());
    println!("Indexes: {}", report.indexes.len());
    println!("Incoming relations: {}", report.incoming_relations.len());
    println!("Outgoing relations: {}", report.outgoing_relations.len());
}

fn relations_project(
    start: impl AsRef<Path>,
    object: &str,
    depth: usize,
    direction: Direction,
) -> Result<RelationsReport> {
    let context = discover_context(start.as_ref())?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    relations_for(&snapshot, object, &RelationsOptions { depth, direction })
}

fn print_relations_report(report: &RelationsReport) {
    println!("Relations for {}", report.target);
    for path in &report.paths {
        println!("- {}", path.objects.join(" -> "));
        for edge in &path.edges {
            println!(
                "  {} {} -> {} confidence={}",
                edge.kind, edge.from, edge.to, edge.confidence
            );
        }
    }
}

fn context_project(
    start: impl AsRef<Path>,
    query: &str,
    token_budget: usize,
) -> Result<ContextPackage> {
    let context = discover_context(start.as_ref())?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    Ok(ContextBuilder::new(RankingWeights::default()).build(
        &snapshot,
        query,
        &ContextOptions {
            token_budget,
            max_objects: 12,
        },
    ))
}

fn print_context_report(report: &ContextPackage) {
    println!("## DbGraph Context");
    println!();
    println!("Query: {}", report.query);
    println!();
    println!("Relevant objects:");
    for object in &report.objects {
        println!(
            "- {} {} :: {}",
            object.kind, object.full_name, object.summary
        );
    }
    if !report.relation_paths.is_empty() {
        println!();
        println!("Relation paths:");
        for path in &report.relation_paths {
            println!("- {path}");
        }
    }
    println!();
    println!("Risks:");
    for risk in &report.risks {
        println!("- {risk}");
    }
    println!();
    println!("Suggested next tools:");
    for tool in &report.suggested_next_tools {
        println!("- {tool}");
    }
}

fn diff_project(start: impl AsRef<Path>) -> Result<SchemaDiff> {
    let context = discover_context(start.as_ref())?;
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
    Ok(DiffEngine::compare(&previous, &latest))
}

fn print_diff_report(report: &SchemaDiff) {
    println!(
        "Schema diff: {} -> {}",
        report.previous_snapshot_id, report.latest_snapshot_id
    );
    println!("Schema hash changed: {}", report.schema_hash_changed);
    for change in &report.changes {
        println!(
            "- {:?} {} {}",
            change.kind,
            change.object_kind.as_str(),
            change.full_name
        );
    }
    for candidate in &report.rename_candidates {
        println!(
            "- rename candidate: {} -> {} ({})",
            candidate.from_full_name, candidate.to_full_name, candidate.reason
        );
    }
}

fn impact_project(start: impl AsRef<Path>, object: &str, depth: usize) -> Result<ImpactReport> {
    let context = discover_context(start.as_ref())?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    match ImpactAnalyzer::new().analyze(&snapshot, object, &ImpactOptions { depth }) {
        Ok(report) => Ok(report),
        Err(err) => {
            let suggestions = table_suggestions(&snapshot, object);
            if suggestions.is_empty() {
                Err(err)
            } else {
                Err(DbGraphError::invalid_argument(format!(
                    "{err}. Suggestions: {}",
                    suggestions.join(", ")
                )))
            }
        }
    }
}

fn print_impact_report(report: &ImpactReport) {
    println!("Impact for {}", report.target);
    for item in &report.items {
        println!(
            "- {:?} {} {} ({})",
            item.scope, item.kind, item.full_name, item.evidence
        );
    }
    if !report.risks.is_empty() {
        println!("Risks:");
        for risk in &report.risks {
            println!("- {} ({})", risk.message, risk.evidence);
        }
    }
}

fn analyze_project(start: impl AsRef<Path>, scope: AnalysisScope) -> Result<AnalysisReport> {
    let context = discover_context(start.as_ref())?;
    require_graph_index(&context)?;
    let snapshot = latest_snapshot(&context)?;
    Ok(AnalysisAnalyzer::new().analyze(&snapshot, &AnalysisOptions { scope }))
}

fn write_analysis_output(
    report: &AnalysisReport,
    format: AnalysisOutputFormat,
    output: Option<&Path>,
) -> Result<()> {
    let rendered = match format {
        AnalysisOutputFormat::Text => render_analysis_text(report),
        AnalysisOutputFormat::Json => {
            serde_json::to_string_pretty(report).map_err(|source| DbGraphError::Internal {
                message: format!("failed to serialize analysis report: {source}"),
            })?
        }
        AnalysisOutputFormat::Markdown => render_analysis_markdown(report),
    };
    if let Some(path) = output {
        fs::write(path, format!("{rendered}\n")).map_err(|source| DbGraphError::io(path, source))
    } else {
        println!("{rendered}");
        Ok(())
    }
}

fn render_analysis_text(report: &AnalysisReport) -> String {
    let mut output = String::new();
    output.push_str("DbGraph analysis\n");
    let _ = writeln!(output, "Snapshot: {}", report.snapshot_id);
    let _ = writeln!(output, "Scope: {:?}", report.scope);
    let _ = writeln!(output, "Findings: {}", report.findings.len());
    let _ = writeln!(output, "Risk score: {}", report.risk_score);
    let _ = writeln!(output, "Summary: {}", report.overview.summary);
    for section in &report.sections {
        let _ = writeln!(
            output,
            "\n{}: {} findings\n",
            section.title, section.finding_count
        );
    }
    for finding in &report.findings {
        output.push_str(&render_analysis_finding(finding));
    }
    output
}

fn render_analysis_markdown(report: &AnalysisReport) -> String {
    let mut output = String::new();
    output.push_str("# DbGraph Analysis Report\n\n");
    let _ = writeln!(output, "- Snapshot: `{}`", report.snapshot_id);
    let _ = writeln!(output, "- Scope: `{:?}`", report.scope);
    let _ = writeln!(output, "- Findings: `{}`", report.findings.len());
    let _ = writeln!(output, "- Risk score: `{}`\n", report.risk_score);
    let _ = writeln!(output, "{}\n", report.overview.summary);
    output.push_str("## Sections\n\n");
    for section in &report.sections {
        let _ = writeln!(
            output,
            "- **{}**: {} findings. {}\n",
            section.title, section.finding_count, section.summary
        );
    }
    output.push_str("\n## Top Findings\n\n");
    for finding in &report.top_findings {
        output.push_str(&render_analysis_finding_markdown(finding));
    }
    output.push_str("\n## All Findings\n\n");
    for finding in &report.findings {
        output.push_str(&render_analysis_finding_markdown(finding));
    }
    output
}

fn render_analysis_finding(finding: &AnalysisFinding) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "\n[{:?}] {} {} :: {}\n",
        finding.severity, finding.rule_id, finding.object, finding.message
    );
    let _ = writeln!(output, "  title: {}", finding.title);
    let _ = writeln!(output, "  evidence: {}", finding.evidence);
    let _ = writeln!(output, "  impact: {}", finding.impact);
    let _ = writeln!(output, "  suggested fix: {}", finding.suggested_fix);
    if !finding.related_objects.is_empty() {
        let _ = writeln!(
            output,
            "  related objects: {}\n",
            finding.related_objects.join(", ")
        );
    }
    output
}

fn render_analysis_finding_markdown(finding: &AnalysisFinding) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "### {} `{}`\n", finding.title, finding.object);
    let _ = write!(
        output,
        "- Severity: `{:?}`\n- Rule: `{}`\n- Message: {}\n- Evidence: {}\n- Impact: {}\n- Suggested fix: {}\n- Confidence: `{:.2}`\n",
        finding.severity,
        finding.rule_id,
        finding.message,
        finding.evidence,
        finding.impact,
        finding.suggested_fix,
        finding.confidence,
    );
    if !finding.tags.is_empty() {
        let _ = writeln!(output, "- Tags: `{}`", finding.tags.join("`, `"));
    }
    if !finding.related_objects.is_empty() {
        let _ = writeln!(
            output,
            "- Related objects: `{}`\n",
            finding.related_objects.join("`, `")
        );
    }
    output.push('\n');
    output
}

fn discover_context(start: &Path) -> Result<ProjectContext> {
    Ok(ProjectContext::discover_from(start)?
        .unwrap_or_else(|| ProjectContext::from_project_root(start)))
}

fn path_or_current(path: Option<PathBuf>) -> Result<PathBuf> {
    path.map_or_else(
        || env::current_dir().map_err(|source| DbGraphError::io(".", source)),
        Ok,
    )
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

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|source| DbGraphError::Internal {
            message: format!("failed to serialize JSON output: {source}"),
        })?
    );
    Ok(())
}

fn print_json_or<T: Serialize>(value: &T, json: bool, printer: impl FnOnce(&T)) -> Result<()> {
    if json {
        print_json(value)
    } else {
        printer(value);
        Ok(())
    }
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
    let normalized = normalize_sql_name(table_name);
    let mut suggestions = snapshot
        .objects
        .iter()
        .filter(|object| object.kind == DbObjectKind::Table)
        .filter(|object| {
            let name = normalize_sql_name(&object.name);
            name.contains(&normalized) || edit_distance(&name, &normalized) <= 2
        })
        .map(|object| object.full_name.clone())
        .collect::<Vec<_>>();
    suggestions.sort();
    suggestions.truncate(5);
    suggestions
}

fn object_summary(object: &DbObject) -> ObjectSummary {
    ObjectSummary {
        kind: object.kind.as_str().to_owned(),
        full_name: object.full_name.clone(),
        summary: object
            .metadata
            .get("comment")
            .and_then(|value| value.as_str())
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

fn read_status(start: impl AsRef<Path>) -> Result<StatusReport> {
    let start = start.as_ref();
    let context = match ProjectContext::discover_from(start)? {
        Some(context) => context,
        None => ProjectContext::from_project_root(start),
    };
    let config_path = context.config_path();
    let config_present = config_path.is_file();
    let provider = if config_present {
        Some(DbGraphConfig::load(&context)?.database.provider)
    } else {
        None
    };
    let snapshots = snapshot_files(&context)?;
    let latest_snapshot = snapshots
        .last()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned());

    Ok(StatusReport {
        project_root: context.project_root().to_path_buf(),
        dbgraph_dir: context.dbgraph_dir().to_path_buf(),
        initialized: context.dbgraph_dir().is_dir() && config_present,
        config_path,
        config_present,
        provider,
        snapshot_count: snapshots.len(),
        latest_snapshot,
        graph_db_path: context.graph_db_path(),
        graph_db_present: context.graph_db_path().is_file(),
        mcp_suggestion: "Run `dbgraph serve --mcp` to start the MCP stdio server.".to_owned(),
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
    files.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs())
    });
    Ok(files)
}

fn print_status(status: &StatusReport) {
    if !status.initialized {
        println!("DbGraph is not initialized.");
        println!("Run `dbgraph init` in this project.");
        println!("Checked: {}", status.dbgraph_dir.display());
        return;
    }

    println!("DbGraph status");
    println!("Project: {}", status.project_root.display());
    println!(".dbgraph: present");
    println!("Config: {}", status.config_path.display());
    println!(
        "Provider: {}",
        status.provider.as_deref().unwrap_or("unknown")
    );
    println!("Snapshots: {}", status.snapshot_count);
    println!(
        "Latest snapshot: {}",
        status.latest_snapshot.as_deref().unwrap_or("none")
    );
    println!(
        "Graph index: {}",
        if status.graph_db_present {
            status.graph_db_path.display().to_string()
        } else {
            "missing".to_owned()
        }
    );
    println!("MCP: {}", status.mcp_suggestion);
}

fn print_error(err: &DbGraphError) {
    eprintln!("error: {err}");
    eprintln!("Run `dbgraph --help` for usage.");
    debug!(error = ?err, "command failed");
}

fn install_agents(
    targets: &[AgentKind],
    location: Option<&Path>,
    dry_run: bool,
    print_config: bool,
) -> Result<()> {
    if print_config {
        for target in targets {
            println!("# target: {target}");
            println!("{}", render_mcp_config(*target, "dbgraph").trim_end());
        }
        return Ok(());
    }

    for target in targets {
        let edit = install_agent_config(*target, location, "dbgraph", dry_run)
            .map_err(|source| DbGraphError::io(target.config_path(location), source))?;
        print_install_edit("install", &edit);
    }
    Ok(())
}

fn uninstall_agents(targets: &[AgentKind], location: Option<&Path>, dry_run: bool) -> Result<()> {
    for target in targets {
        let edit = uninstall_agent_config(*target, location, dry_run)
            .map_err(|source| DbGraphError::io(target.config_path(location), source))?;
        print_install_edit("uninstall", &edit);
    }
    Ok(())
}

fn print_install_edit(action: &str, edit: &dbgraph_agent_config::InstallEdit) {
    let mode = if edit.dry_run { "dry-run" } else { action };
    let status = if edit.changed { "changed" } else { "unchanged" };
    println!(
        "{mode} {target}: {status} {path}",
        target = edit.target,
        path = edit.path.display()
    );
    if let Some(backup) = &edit.backup_path {
        println!("backup: {}", backup.display());
    }
}

fn print_help() {
    println!(
        "\
DbGraph

Usage:
  dbgraph [OPTIONS] init [PATH] [--force] [-i|--interactive] [--yes]
  dbgraph [OPTIONS] status [PATH] [--json]
  dbgraph [OPTIONS] snapshot [PATH] [--profile schema|stats|sample] [--max-rows-per-table N] [--store-raw-samples] [--json]
  dbgraph [OPTIONS] sync [PATH] [--json]
  dbgraph [OPTIONS] benchmark [--tables N] [--columns-per-table N] [--json]
  dbgraph [OPTIONS] validate-sql [PATH] (--sql SQL | --file FILE) [--dialect postgres|mysql|generic] [--json]
  dbgraph [OPTIONS] search [PATH] QUERY [--kind KIND] [--json]
  dbgraph [OPTIONS] table [PATH] TABLE [--json]
  dbgraph [OPTIONS] relations [PATH] OBJECT [--depth 1|2] [--direction incoming|outgoing|both] [--json]
  dbgraph [OPTIONS] context [PATH] QUERY [--tokens N] [--json]
  dbgraph [OPTIONS] diff [PATH] [--json]
  dbgraph [OPTIONS] impact [PATH] OBJECT [--depth 1|2] [--json]
  dbgraph [OPTIONS] analyze [PATH] [--scope all|risk|quality|performance] [--format text|json|markdown] [--output FILE] [--json]
  dbgraph [OPTIONS] install [--target codex,cursor,claude] [--location DIR] [--yes] [--dry-run] [--print-config]
  dbgraph [OPTIONS] uninstall [--target codex,cursor,claude] [--location DIR] [--dry-run]
  dbgraph [OPTIONS] serve --mcp
  dbgraph [OPTIONS] --version
  dbgraph [OPTIONS] --help

Options:
  -v, --verbose      Show debug diagnostics
  -q, --quiet        Show errors only
  -V, --version      Print version
  -h, --help         Print help
  -f, --force        Replace existing init config when used with init
  -i, --interactive  Prompt for init options
  -y, --yes          Use interactive init defaults without prompts
  -j, --json         Print command output as JSON

Commands:
  init       Initialize .dbgraph project state
  status     Show local project status
  snapshot   Capture database schema into JSON and local SQLite index
  sync       Capture and compare schema hashes for incremental sync
  benchmark  Generate a synthetic schema benchmark report
  validate-sql Parse SQL and validate references against the local graph index
  search     Search schema and SQL graph objects
  table      Show table columns, constraints, profile, and relations
  relations  Traverse incoming, outgoing, explicit, and inferred relations
  context    Build compact read-only context for an AI database task
  diff       Compare the latest snapshot with the previous snapshot
  impact     Show downstream/upstream impact and risk notes for an object
  analyze    Report structured risk, quality, and performance findings
  install    Configure DbGraph MCP blocks for agent targets
  uninstall  Remove only DbGraph managed MCP blocks
  serve      Start the MCP stdio server"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbgraph_core::model::{
        ColumnMetadata, ConstraintMetadata, DbEdgeKind, Evidence, IndexMetadata,
    };

    fn parse(items: &[&str]) -> Result<ParsedArgs> {
        parse_args(items.iter().map(ToString::to_string))
    }

    #[test]
    fn parses_verbose_version() {
        let parsed = parse(&["--verbose", "--version"]).expect("args should parse");

        assert_eq!(parsed.verbosity, LogVerbosity::Verbose);
        assert_eq!(parsed.command, Command::Version);
    }

    #[test]
    fn parses_quiet_help() {
        let parsed = parse(&["--quiet", "help"]).expect("args should parse");

        assert_eq!(parsed.verbosity, LogVerbosity::Quiet);
        assert_eq!(parsed.command, Command::Help);
    }

    #[test]
    fn parses_init_defaults_to_current_directory() {
        let parsed = parse(&["init"]).expect("args should parse");

        assert_eq!(
            parsed.command,
            Command::Init {
                path: None,
                force: false,
                interactive: false,
                yes: false
            }
        );
    }

    #[test]
    fn parses_init_path_and_force() {
        let parsed = parse(&["init", "sample-project", "--force"]).expect("args should parse");

        assert_eq!(
            parsed.command,
            Command::Init {
                path: Some(PathBuf::from("sample-project")),
                force: true,
                interactive: false,
                yes: false
            }
        );
    }

    #[test]
    fn parses_interactive_yes_init() {
        let parsed = parse(&["init", "-i", "--yes"]).expect("args should parse");

        assert_eq!(
            parsed.command,
            Command::Init {
                path: None,
                force: false,
                interactive: true,
                yes: true
            }
        );
    }

    #[test]
    fn parses_status_json() {
        let parsed = parse(&["status", "--json", "sample-project"]).expect("args should parse");

        assert_eq!(
            parsed.command,
            Command::Status {
                path: Some(PathBuf::from("sample-project")),
                json: true
            }
        );
    }

    #[test]
    fn parses_snapshot_command() {
        let parsed = parse(&["snapshot", "--json"]).expect("args should parse");

        assert_eq!(
            parsed.command,
            Command::Snapshot {
                path: None,
                json: true,
                profile: None,
                max_rows_per_table: None,
                store_raw_samples: false,
            }
        );
    }

    #[test]
    fn parses_phase09_snapshot_sync_and_benchmark_commands() {
        let snapshot = parse(&[
            "snapshot",
            "--profile",
            "sample",
            "--max-rows-per-table",
            "7",
            "--store-raw-samples",
            "--json",
        ])
        .expect("snapshot args should parse");
        assert_eq!(
            snapshot.command,
            Command::Snapshot {
                path: None,
                json: true,
                profile: Some(ProfilingMode::Sample),
                max_rows_per_table: Some(7),
                store_raw_samples: true,
            }
        );

        let sync = parse(&["sync", "sample-project", "--json"]).expect("sync args should parse");
        assert_eq!(
            sync.command,
            Command::Sync {
                path: Some(PathBuf::from("sample-project")),
                json: true,
            }
        );

        let benchmark = parse(&["benchmark", "--tables", "10000", "--columns-per-table", "2"])
            .expect("benchmark args should parse");
        assert_eq!(
            benchmark.command,
            Command::Benchmark {
                tables: 10_000,
                columns_per_table: 2,
                json: false,
            }
        );

        let analyze = parse(&["analyze", "sample-project", "--scope", "quality", "--json"])
            .expect("analyze args should parse");
        assert_eq!(
            analyze.command,
            Command::Analyze {
                path: Some(PathBuf::from("sample-project")),
                scope: AnalysisScope::Quality,
                json: true,
                format: AnalysisOutputFormat::Json,
                output: None,
            }
        );

        let markdown = parse(&[
            "analyze",
            "sample-project",
            "--scope",
            "all",
            "--format",
            "markdown",
            "--output",
            "report.md",
        ])
        .expect("markdown analyze args should parse");
        assert_eq!(
            markdown.command,
            Command::Analyze {
                path: Some(PathBuf::from("sample-project")),
                scope: AnalysisScope::All,
                json: false,
                format: AnalysisOutputFormat::Markdown,
                output: Some(PathBuf::from("report.md")),
            }
        );
    }

    #[test]
    fn parses_validate_sql_from_string() {
        let parsed = parse(&[
            "validate-sql",
            "--sql",
            "select * from users",
            "--dialect",
            "postgres",
            "--json",
        ])
        .expect("args should parse");

        assert_eq!(
            parsed.command,
            Command::ValidateSql {
                path: None,
                sql: Some("select * from users".to_owned()),
                file: None,
                dialect: dbgraph_sql::SqlDialect::Postgres,
                json: true
            }
        );
    }

    #[test]
    fn parses_serve_mcp_command() {
        let parsed = parse(&["serve", "--mcp"]).expect("serve mcp args should parse");

        assert_eq!(parsed.command, Command::ServeMcp);
    }

    #[test]
    fn parses_install_and_uninstall_agent_targets() {
        let install = parse(&[
            "install",
            "--target=codex,cursor,claude",
            "--location",
            "sandbox",
            "--yes",
            "--dry-run",
            "--print-config",
        ])
        .expect("install args should parse");
        assert_eq!(
            install.command,
            Command::Install {
                targets: vec![AgentKind::Codex, AgentKind::Cursor, AgentKind::Claude],
                location: Some(PathBuf::from("sandbox")),
                yes: true,
                dry_run: true,
                print_config: true,
            }
        );

        let uninstall = parse(&["uninstall", "--target", "codex", "--location=sandbox"])
            .expect("uninstall args should parse");
        assert_eq!(
            uninstall.command,
            Command::Uninstall {
                targets: vec![AgentKind::Codex],
                location: Some(PathBuf::from("sandbox")),
                dry_run: false,
            }
        );
    }

    #[test]
    fn install_and_uninstall_preserve_user_config() {
        let temp = TempProject::new();
        let target_path = AgentKind::Codex.config_path(Some(&temp.root));
        fs::create_dir_all(target_path.parent().expect("config has parent"))
            .expect("config dir should create");
        fs::write(&target_path, "{ \"user\": true }\n").expect("user config should write");

        install_agents(&[AgentKind::Codex], Some(&temp.root), false, false)
            .expect("install should write managed block");
        install_agents(&[AgentKind::Codex], Some(&temp.root), false, false)
            .expect("second install should be idempotent");
        let installed = fs::read_to_string(&target_path).expect("config should read");
        assert!(installed.contains("{ \"user\": true }"));
        assert_eq!(
            installed
                .matches(dbgraph_agent_config::DBGRAPH_MCP_SECTION_START)
                .count(),
            1
        );
        assert!(target_path.with_extension("dbgraph.bak").is_file());

        uninstall_agents(&[AgentKind::Codex], Some(&temp.root), false)
            .expect("uninstall should remove managed block");
        let removed = fs::read_to_string(&target_path).expect("config should read");
        assert!(removed.contains("{ \"user\": true }"));
        assert!(!removed.contains(dbgraph_agent_config::DBGRAPH_MCP_SECTION_START));
    }

    #[test]
    fn parses_phase05_commands_without_quoted_query() {
        let search = parse(&["search", "refund", "payment", "--kind", "table", "--json"])
            .expect("search args should parse");
        assert_eq!(
            search.command,
            Command::Search {
                path: None,
                query: "refund payment".to_owned(),
                kind: Some("table".to_owned()),
                json: true
            }
        );

        let relations = parse(&[
            "relations",
            "public.payments",
            "--depth",
            "2",
            "--direction",
            "incoming",
        ])
        .expect("relations args should parse");
        assert_eq!(
            relations.command,
            Command::Relations {
                path: None,
                object: "public.payments".to_owned(),
                depth: 2,
                direction: Direction::Incoming,
                json: false
            }
        );

        let context = parse(&[
            "context", "refund", "payment", "order", "--tokens", "120", "--json",
        ])
        .expect("context args should parse");
        assert_eq!(
            context.command,
            Command::Context {
                path: None,
                query: "refund payment order".to_owned(),
                token_budget: 120,
                json: true
            }
        );

        let impact = parse(&["impact", "public.orders.status", "--depth", "1"])
            .expect("impact args should parse");
        assert_eq!(
            impact.command,
            Command::Impact {
                path: None,
                object: "public.orders.status".to_owned(),
                depth: 1,
                json: false
            }
        );
    }

    #[test]
    fn parses_phase05_commands_with_explicit_path() {
        let temp = TempProject::new();
        fs::create_dir_all(&temp.root).expect("temp root should exist");

        let parsed = parse(&[
            "search",
            temp.root.to_str().expect("path should be utf8"),
            "payment",
            "orders",
        ])
        .expect("search args should parse");

        assert_eq!(
            parsed.command,
            Command::Search {
                path: Some(temp.root.clone()),
                query: "payment orders".to_owned(),
                kind: None,
                json: false
            }
        );
    }

    #[test]
    fn validate_sql_suggests_known_table_without_database_connection() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);
        let mut snapshot = dbgraph_core::model::DbSnapshot::new("s1", "postgres", "app", 1);
        snapshot.objects.push(dbgraph_core::model::DbObject::new(
            "table:public.users",
            dbgraph_core::model::DbObjectKind::Table,
            "public.users",
        ));
        SnapshotStore::new(&context)
            .write_snapshot(&snapshot, true)
            .expect("snapshot should write");
        let mut repo = GraphRepository::open(context.graph_db_path()).expect("repo should open");
        repo.rebuild_snapshot(&snapshot)
            .expect("graph index should write");

        let report = validate_sql(
            &temp.root,
            "select * from user",
            dbgraph_sql::SqlDialect::Postgres,
        )
        .expect("validate should not require DB connection");

        assert!(report.valid);
        assert!(report.unresolved.iter().any(
            |item| item.name == "user" && item.suggestions.contains(&"public.users".to_owned())
        ));
    }

    #[test]
    fn validate_sql_reports_unresolved_columns_with_suggestions() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);
        let mut snapshot = dbgraph_core::model::DbSnapshot::new("s1", "postgres", "app", 1);
        snapshot.objects.push(dbgraph_core::model::DbObject::new(
            "table:public.users",
            dbgraph_core::model::DbObjectKind::Table,
            "public.users",
        ));
        let mut email = dbgraph_core::model::DbObject::new(
            "column:public.users.email",
            dbgraph_core::model::DbObjectKind::Column,
            "public.users.email",
        );
        email.table_name = Some("users".to_owned());
        email.column_name = Some("email".to_owned());
        snapshot.objects.push(email);
        SnapshotStore::new(&context)
            .write_snapshot(&snapshot, true)
            .expect("snapshot should write");
        let mut repo = GraphRepository::open(context.graph_db_path()).expect("repo should open");
        repo.rebuild_snapshot(&snapshot)
            .expect("graph index should write");

        let report = validate_sql(
            &temp.root,
            "select * from public.users u where u.emali = 'x'",
            dbgraph_sql::SqlDialect::Postgres,
        )
        .expect("validate should inspect local graph index");

        assert!(report
            .unresolved
            .iter()
            .any(|item| item.name == "public.users.emali"
                && item.suggestions.contains(&"public.users.email".to_owned())));
    }

    #[test]
    fn search_requires_graph_index_and_returns_stable_matches() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let snapshot = sample_phase05_snapshot("s1", 1);
        write_latest_snapshot_without_index(&temp.root, &snapshot);

        let err = search_project(&temp.root, "payment", Some("table"))
            .expect_err("search should require graph index");
        assert!(err.to_string().contains("dbgraph snapshot"));

        write_latest_snapshot_with_index(&temp.root, &snapshot);
        let report = search_project(&temp.root, "payment", Some("table"))
            .expect("search should use local snapshot");

        assert_eq!(report.results[0].full_name, "public.payments");
        assert!(report.results.iter().all(|result| result.kind == "table"));
    }

    #[test]
    fn table_reports_columns_constraints_profiles_and_relations() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let snapshot = sample_phase05_snapshot("s1", 1);
        write_latest_snapshot_with_index(&temp.root, &snapshot);

        let report = table_project(&temp.root, "payments").expect("table should resolve");

        assert_eq!(report.table, "public.payments");
        assert!(report.columns.iter().any(|column| column.name == "order_id"
            && column.data_type.as_deref() == Some("bigint")
            && column.nullable == Some(false)));
        assert!(report
            .constraints
            .iter()
            .any(|object| object.kind == "foreign_key"));
        assert!(report
            .indexes
            .iter()
            .any(|object| object.full_name == "public.idx_payments_order_id"));
        assert_eq!(
            report
                .profile
                .as_ref()
                .and_then(|profile| profile.row_estimate),
            Some(12_500)
        );
        assert!(report
            .incoming_relations
            .iter()
            .any(|edge| edge.from == "public.refunds"));
        assert!(report
            .outgoing_relations
            .iter()
            .any(|edge| edge.to == "public.orders"));

        let missing = table_project(&temp.root, "paymnts").expect("missing table should suggest");
        assert!(missing.suggestions.contains(&"public.payments".to_owned()));
    }

    #[test]
    fn relations_context_diff_and_impact_reports_are_built_from_snapshots() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let previous = sample_phase05_snapshot("s1", 1);
        let latest = sample_phase05_snapshot("s2", 2);
        write_snapshot_only(&temp.root, &previous);
        write_latest_snapshot_with_index(&temp.root, &latest);

        let relations = relations_project(&temp.root, "public.orders", 2, Direction::Incoming)
            .expect("relations should resolve");
        assert!(relations.paths.iter().any(|path| {
            path.objects
                .first()
                .is_some_and(|name| name == "public.orders")
                && path
                    .objects
                    .last()
                    .is_some_and(|name| name == "public.payments")
                && path
                    .edges
                    .iter()
                    .any(|edge| edge.from == "public.payments" && edge.to == "public.orders")
        }));

        let context =
            context_project(&temp.root, "refund payment order", 120).expect("context should build");
        let names = context
            .objects
            .iter()
            .map(|object| object.full_name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"public.refunds"));
        assert!(names.contains(&"public.payments"));
        assert!(context
            .risks
            .iter()
            .any(|risk| risk.to_ascii_lowercase().contains("read-only")));

        let diff = diff_project(&temp.root).expect("diff should compare latest and previous");
        assert!(diff
            .changes
            .iter()
            .any(|change| change.full_name == "public.refunds.status"));

        let impact =
            impact_project(&temp.root, "public.orders.status", 2).expect("impact should resolve");
        assert!(impact
            .items
            .iter()
            .any(|item| item.full_name == "public.active_orders"));
        assert!(impact
            .risks
            .iter()
            .any(|risk| risk.message.to_ascii_lowercase().contains("status")));
    }

    #[test]
    fn analyze_reports_quality_findings_from_latest_snapshot() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let snapshot = sample_phase05_snapshot("s1", 1);
        write_latest_snapshot_with_index(&temp.root, &snapshot);

        let report = analyze_project(&temp.root, AnalysisScope::Quality)
            .expect("analysis should use latest snapshot");

        assert!(report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "quality.missing_primary_key"
                && finding.object == "public.orders"));
        assert!(report
            .findings
            .iter()
            .all(|finding| finding.scope == AnalysisScope::Quality));
    }

    #[test]
    fn analyze_writes_markdown_report_to_output_path() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let snapshot = sample_phase05_snapshot("s1", 1);
        write_latest_snapshot_with_index(&temp.root, &snapshot);
        let report_path = temp.root.join("analysis.md");

        let report = analyze_project(&temp.root, AnalysisScope::All).expect("analysis should run");
        write_analysis_output(
            &report,
            AnalysisOutputFormat::Markdown,
            Some(report_path.as_path()),
        )
        .expect("markdown report should write");

        let markdown = fs::read_to_string(report_path).expect("markdown report should be readable");
        assert!(markdown.contains("# DbGraph Analysis Report"));
        assert!(markdown.contains("Data Integrity & Schema Quality"));
        assert!(markdown.contains("Suggested fix"));
    }

    #[test]
    fn snapshot_requires_initialized_project() {
        let temp = TempProject::new();
        fs::create_dir_all(&temp.root).expect("temp root should exist");

        let err = run(["snapshot".to_owned(), temp.root.display().to_string()])
            .expect_err("snapshot should require config");

        assert!(err.to_string().contains("Run `dbgraph init` first"));
    }

    #[test]
    fn init_creates_expected_layout() {
        let temp = TempProject::new();

        let summary =
            init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);

        assert_eq!(summary.dbgraph_dir, context.dbgraph_dir());
        assert_eq!(summary.instructions_dir, context.instructions_dir());
        assert!(context.dbgraph_dir().is_dir());
        assert!(context.snapshots_dir().is_dir());
        assert!(context.instructions_dir().is_dir());
        assert!(context.config_path().is_file());
    }

    #[test]
    fn init_does_not_overwrite_existing_config_without_force() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default())
            .expect("first init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);
        let custom = "{ \"custom\": true }\n";
        fs::write(context.config_path(), custom).expect("custom config should be written");

        let err = init_project(&temp.root, false, &InitOptions::default())
            .expect_err("second init should fail");
        let stored = fs::read_to_string(context.config_path()).expect("config should be readable");

        assert!(err.to_string().contains("--force"));
        assert_eq!(stored, custom);
    }

    #[test]
    fn init_force_replaces_existing_config() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default())
            .expect("first init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);
        fs::write(context.config_path(), "{ \"custom\": true }\n")
            .expect("custom config should be written");

        init_project(&temp.root, true, &InitOptions::default()).expect("force init should succeed");
        let config = DbGraphConfig::load(&context).expect("default config should load");

        assert!(!config.snapshot.sample_rows);
        assert!(!config.security.store_raw_data);
    }

    #[test]
    fn interactive_yes_writes_instruction_fragments() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::interactive_defaults())
            .expect("interactive init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);

        assert!(context
            .instructions_dir()
            .join("AGENTS.md.fragment")
            .is_file());
        assert!(context
            .instructions_dir()
            .join("CLAUDE.md.fragment")
            .is_file());
        assert!(context.instructions_dir().join("dbgraph.mdc").is_file());
        let config = DbGraphConfig::load(&context).expect("config should load");
        assert!(config.database.connection_string.is_none());
        assert_eq!(
            config.database.connection_env.as_deref(),
            Some("DATABASE_URL")
        );
    }

    #[test]
    fn init_with_run_snapshot_invokes_snapshot_after_project_files_exist() {
        let temp = TempProject::new();
        let options = InitOptions {
            run_snapshot: true,
            ..InitOptions::default()
        };

        let summary = init_project_with_optional_snapshot(&temp.root, false, &options, |root| {
            let context = ProjectContext::from_project_root(root);
            assert!(context.config_path().is_file());
            Ok(None)
        })
        .expect("init should succeed");

        assert!(summary.snapshot.is_none());
    }

    #[test]
    fn snapshot_supports_sqlite_provider_without_external_service() {
        let temp = TempProject::new();
        fs::create_dir_all(&temp.root).expect("temp root should exist");
        let sqlite_path = temp.root.join("business.sqlite");
        let connection = rusqlite::Connection::open(&sqlite_path).expect("sqlite should open");
        connection
            .execute_batch(
                "
                CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    email TEXT NOT NULL
                );
                INSERT INTO users (id, email) VALUES (1, 'a@example.test');
                ",
            )
            .expect("sqlite fixture should write");
        let context = ProjectContext::from_project_root(&temp.root);
        let config = DbGraphConfig {
            database: DatabaseConfig {
                provider: DatabaseProviderKind::Sqlite.to_string(),
                connection_env: None,
                connection_string: Some(sqlite_path.display().to_string()),
            },
            ..DbGraphConfig::default()
        };
        config.save(&context).expect("sqlite config should save");

        let summary = run_snapshot(&temp.root).expect("sqlite snapshot should run");

        assert_eq!(summary.provider, "sqlite");
        assert_eq!(summary.table_count, 1);
        assert_eq!(summary.column_count, 2);
        assert!(summary.snapshot_path.is_file());
        assert!(summary.graph_db_path.is_file());
    }

    #[test]
    fn status_reports_uninitialized_project() {
        let temp = TempProject::new();
        fs::create_dir_all(&temp.root).expect("temp root should exist");

        let status = read_status(&temp.root).expect("status should load");

        assert!(!status.initialized);
        assert!(!status.config_present);
        assert_eq!(status.snapshot_count, 0);
    }

    #[test]
    fn status_reports_initialized_project() {
        let temp = TempProject::new();
        init_project(&temp.root, false, &InitOptions::default()).expect("init should succeed");
        let context = ProjectContext::from_project_root(&temp.root);
        fs::write(context.snapshots_dir().join("2026-01-01.json"), "{}")
            .expect("snapshot should be written");

        let status = read_status(&temp.root).expect("status should load");

        assert!(status.initialized);
        assert_eq!(status.provider.as_deref(), Some("postgres"));
        assert_eq!(status.snapshot_count, 1);
        assert_eq!(status.latest_snapshot.as_deref(), Some("2026-01-01.json"));
        assert!(!status.graph_db_present);
    }

    #[test]
    fn rejects_conflicting_verbosity() {
        let err = parse(&["--quiet", "--verbose"]).expect_err("conflict should fail");

        assert_eq!(err.exit_code().code(), 2);
        assert!(err.to_string().contains("cannot be used"));
    }

    #[test]
    fn rejects_unknown_argument() {
        let err = parse(&["--bad"]).expect_err("unknown arg should fail");

        assert_eq!(err.exit_code().code(), 2);
        assert!(err.to_string().contains("--bad"));
    }

    fn write_latest_snapshot_without_index(root: &Path, snapshot: &DbSnapshot) {
        write_snapshot_only(root, snapshot);
    }

    fn write_latest_snapshot_with_index(root: &Path, snapshot: &DbSnapshot) {
        let context = ProjectContext::from_project_root(root);
        SnapshotStore::new(&context)
            .write_snapshot(snapshot, true)
            .expect("snapshot should write");
        let mut repo = GraphRepository::open(context.graph_db_path()).expect("repo should open");
        repo.rebuild_snapshot(snapshot)
            .expect("graph index should write");
    }

    fn write_snapshot_only(root: &Path, snapshot: &DbSnapshot) {
        let context = ProjectContext::from_project_root(root);
        SnapshotStore::new(&context)
            .write_snapshot(snapshot, true)
            .expect("snapshot should write");
    }

    #[allow(clippy::too_many_lines)]
    fn sample_phase05_snapshot(id: &str, created_at_unix_ms: u64) -> DbSnapshot {
        let mut snapshot = DbSnapshot::new(id, "postgres", "app", created_at_unix_ms);
        snapshot
            .objects
            .push(table("table:orders", "public.orders", "orders"));
        snapshot
            .objects
            .push(table("table:payments", "public.payments", "payments"));
        snapshot
            .objects
            .push(table("table:refunds", "public.refunds", "refunds"));
        snapshot.objects.push(column(
            "column:orders.status",
            "public.orders.status",
            "orders",
            "status",
            "text",
            false,
            Some("'pending'"),
        ));
        snapshot.objects.push(column(
            "column:payments.order_id",
            "public.payments.order_id",
            "payments",
            "order_id",
            "bigint",
            false,
            None,
        ));
        snapshot.objects.push(column(
            "column:refunds.payment_id",
            "public.refunds.payment_id",
            "refunds",
            "payment_id",
            "bigint",
            false,
            None,
        ));
        if id == "s2" {
            snapshot.objects.push(column(
                "column:refunds.status",
                "public.refunds.status",
                "refunds",
                "status",
                "text",
                false,
                Some("'open'"),
            ));
        }
        snapshot.objects.push(foreign_key(
            "fk:payments.orders",
            "public.payments_order_id_fkey",
            "payments",
            "public.orders",
        ));
        snapshot.objects.push(index(
            "index:payments.order_id",
            "public.idx_payments_order_id",
            "payments",
        ));
        snapshot.objects.push(view(
            "view:active_orders",
            "public.active_orders",
            "active order status view",
        ));
        snapshot.edges.push(edge(
            "edge:payments.orders",
            DbEdgeKind::References,
            "table:payments",
            "table:orders",
            1.0,
            "foreign key payments.order_id",
        ));
        snapshot.edges.push(edge(
            "edge:refunds.payments",
            DbEdgeKind::InferredReference,
            "table:refunds",
            "table:payments",
            0.74,
            "naming rule refunds.payment_id",
        ));
        snapshot.edges.push(edge(
            "edge:view.orders",
            DbEdgeKind::UsedByView,
            "view:active_orders",
            "table:orders",
            1.0,
            "view definition",
        ));
        snapshot.edges.push(edge(
            "edge:view.status",
            DbEdgeKind::FiltersBy,
            "view:active_orders",
            "column:orders.status",
            1.0,
            "where status = 'active'",
        ));
        snapshot.table_profiles.push(TableProfile {
            object_id: "table:payments".to_owned(),
            row_estimate: Some(12_500),
            row_count_kind: Some("estimate".to_owned()),
            size_bytes: Some(524_288),
            profile: std::collections::BTreeMap::new(),
        });
        snapshot.column_profiles.push(ColumnProfile {
            object_id: "column:payments.order_id".to_owned(),
            data_type_family: Some("integer".to_owned()),
            null_fraction: Some(0.0),
            distinct_estimate: Some(11_000.0),
            pii_score: Some(0.0),
            profile: std::collections::BTreeMap::new(),
        });
        snapshot
    }

    fn table(id: &str, full_name: &str, table_name: &str) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Table, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.metadata.insert(
            "comment".to_owned(),
            serde_json::Value::String(format!("{table_name} records")),
        );
        object
    }

    fn column(
        id: &str,
        full_name: &str,
        table_name: &str,
        column_name: &str,
        data_type: &str,
        nullable: bool,
        default: Option<&str>,
    ) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Column, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.column_name = Some(column_name.to_owned());
        object.column = Some(ColumnMetadata {
            data_type: Some(data_type.to_owned()),
            data_type_family: Some(data_type.to_owned()),
            nullable: Some(nullable),
            default: default.map(ToOwned::to_owned),
            comment: None,
        });
        object
    }

    fn foreign_key(
        id: &str,
        full_name: &str,
        table_name: &str,
        referenced_table: &str,
    ) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::ForeignKey, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.constraint = Some(ConstraintMetadata {
            columns: vec!["order_id".to_owned()],
            referenced_table: Some(referenced_table.to_owned()),
            referenced_columns: vec!["id".to_owned()],
        });
        object
    }

    fn index(id: &str, full_name: &str, table_name: &str) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::Index, full_name);
        object.schema_name = Some("public".to_owned());
        object.table_name = Some(table_name.to_owned());
        object.index = Some(IndexMetadata {
            unique: Some(false),
            columns: vec!["order_id".to_owned()],
            expression: None,
        });
        object
    }

    fn view(id: &str, full_name: &str, comment: &str) -> DbObject {
        let mut object = DbObject::new(id, DbObjectKind::View, full_name);
        object.schema_name = Some("public".to_owned());
        object.metadata.insert(
            "comment".to_owned(),
            serde_json::Value::String(comment.to_owned()),
        );
        object
    }

    fn edge(
        id: &str,
        kind: DbEdgeKind,
        from: &str,
        to: &str,
        confidence: f64,
        detail: &str,
    ) -> DbEdge {
        let mut edge = DbEdge::explicit(id, kind, from, to);
        edge.confidence = confidence;
        edge.evidence.push(Evidence {
            source: "test".to_owned(),
            detail: detail.to_owned(),
        });
        edge
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
            let root = env::temp_dir().join(format!(
                "dbgraph-cli-init-test-{}-{unique}",
                std::process::id()
            ));
            Self { root }
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            if self.root.exists() {
                fs::remove_dir_all(&self.root).expect("temp root should be removed");
            }
        }
    }
}
