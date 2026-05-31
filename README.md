<div align="center">

# DbGraph

**Local-first database context for AI coding agents**

Build a local graph of database schema objects, SQL artifacts, and relationships so agents can search, validate, analyze, and reason about database changes without storing business row data by default.

[![Rust](https://img.shields.io/badge/Rust-workspace-orange)](Cargo.toml)
[![CLI](https://img.shields.io/badge/interface-CLI%20%2B%20MCP-blue)](#cli-reference)
[![Docs](https://img.shields.io/badge/docs-English%20%7C%20中文-lightgrey)](#documentation)

</div>

---

## Documentation

| Guide | Description |
| --- | --- |
| [English Usage Guide](docs/usage.md) | Complete workflow, provider setup, CLI usage, MCP, and smoke tests. |
| [中文使用说明](docs/usage.zh-CN.md) | 中文完整使用流程、配置、常用命令和安全说明。 |
| [Quickstart](docs/quickstart.md) | Short path for getting a project initialized and indexed. |

## Contents

- [Why DbGraph](#why-dbgraph)
- [Status](#status)
- [Installation](#installation)
- [Quickstart](#quickstart)
- [Core Features](#core-features)
- [CLI Reference](#cli-reference)
- [MCP Tools](#mcp-tools)
- [Example: PostgreSQL Teashop](#example-postgresql-teashop)
- [Safety Defaults](#safety-defaults)
- [Development](#development)
- [Inspiration](#inspiration)
- [Roadmap](#roadmap)

## Why DbGraph

| Need | DbGraph Approach |
| --- | --- |
| Give agents database context | Captures schema, SQL artifacts, and relations into local project state. |
| Avoid unsafe exploratory SQL | Validates SQL references against the local graph without executing SQL. |
| Understand change impact | Traverses explicit and inferred dependencies across tables, columns, views, and SQL artifacts. |
| Review database risk | Reports security, quality, workload, and performance findings with evidence and suggested fixes. |
| Integrate with agents | Exposes both CLI commands and MCP stdio tools. |

## Status

DbGraph currently supports:

| Area | Available Now |
| --- | --- |
| Project setup | `.dbgraph/` initialization, config, snapshots, instruction fragments |
| Providers | PostgreSQL snapshots, SQLite snapshots |
| SQL | SQL file scanning, parsing, fingerprinting, lineage extraction |
| Graph | Local SQLite graph storage, object search, inferred relationships |
| Analysis | SQL validation, diff, impact, context retrieval, structured analysis reports |
| Agent interface | CLI plus MCP stdio server |

### Database Support

| Provider | Status | Notes |
| --- | --- | --- |
| PostgreSQL | Supported | Schemas, tables, columns, constraints, indexes, views, materialized views, routines, triggers, enums, sequences, and available statistics. |
| SQLite | Supported | Local business database files with tables, columns, primary keys, foreign keys, unique constraints, indexes, views, and exact table counts. |
| MySQL | Registered, skipped | Waiting for local/containerized service fixtures. |
| SQL Server | Registered, skipped | Waiting for local/containerized service fixtures. |

Provider details are documented in [docs/provider-capabilities.md](docs/provider-capabilities.md).

## Installation

From this repository:

```bash
cargo build --workspace
cargo run -p dbgraph-cli -- --version
```

If `dbgraph` is already installed on your `PATH`:

```bash
dbgraph --version
```

## Quickstart

| Step | Command |
| --- | --- |
| Initialize project | `dbgraph init -i --yes` |
| Capture snapshot | `dbgraph snapshot --profile stats` |
| Search graph | `dbgraph search orders --kind table` |
| Inspect table | `dbgraph table public.orders` |
| Traverse relations | `dbgraph relations public.orders --depth 2` |
| Validate SQL | `dbgraph validate-sql --sql "select * from orders"` |
| Generate report | `dbgraph analyze --scope all --format markdown --output dbgraph-analysis.md` |

For a complete walkthrough, see [docs/usage.md](docs/usage.md) or [docs/usage.zh-CN.md](docs/usage.zh-CN.md).

## Core Features

### Database Snapshots

- `dbgraph snapshot [PATH] [--json]` captures schema metadata through the configured provider.
- Snapshot JSON is written under `.dbgraph/snapshots/`.
- The local graph index is rebuilt into `.dbgraph/dbgraph.db`.
- Sensitive connection strings are not printed in provider errors.

### SQL Artifacts

- Scans `.sql` files under `migrations/`, `sql/`, and `db/` by default.
- Ignores noisy directories such as `node_modules/`, `target/`, `bin/`, and `obj/`.
- Preserves raw SQL, normalized SQL, fingerprints, statement summaries, and diagnostics.
- Extracts reads, writes, joins, filters, groups, ordering, and CTE dependencies where supported.
- Stores SQL artifacts as query objects in the graph.

### Search, Context, and Impact

| Command | Purpose |
| --- | --- |
| `dbgraph search` | Search local schema and SQL graph objects. |
| `dbgraph table` | Summarize columns, constraints, indexes, profile, and relations for a table. |
| `dbgraph relations` | Traverse explicit and inferred graph relations. |
| `dbgraph context` | Build compact, read-only context for AI database work. |
| `dbgraph diff` | Compare the latest snapshot with the previous snapshot. |
| `dbgraph impact` | Report direct and indirect affected objects plus risk notes. |

### Analysis Reports

`dbgraph analyze` produces structured review reports with overview, risk score, section summaries, top findings, severity counts, evidence, impact, confidence, suggested fixes, and related objects.

| Section | Rules |
| --- | --- |
| Security & Privacy | Sensitive columns, SQL references to sensitive columns, broad `SELECT *`. |
| Data Integrity & Schema Quality | Missing primary keys, probable missing foreign keys. |
| SQL Workload & Safety | `UPDATE` or `DELETE` statements without `WHERE`. |
| Performance | Filters or joins without supporting indexes. |

Example:

```bash
dbgraph analyze --scope all --format markdown --output dbgraph-analysis.md
```

## CLI Reference

```bash
dbgraph --version
dbgraph --help
dbgraph init [PATH] [--force] [-i|--interactive] [--yes]
dbgraph status [PATH] [--json]
dbgraph snapshot [PATH] [--profile schema|stats|sample] [--max-rows-per-table N] [--store-raw-samples] [--json]
dbgraph sync [PATH] [--json]
dbgraph benchmark [--tables N] [--columns-per-table N] [--json]
dbgraph validate-sql [PATH] (--sql SQL | --file FILE) [--dialect postgres|mysql|generic] [--json]
dbgraph search [PATH] QUERY [--kind KIND] [--json]
dbgraph table [PATH] TABLE [--json]
dbgraph relations [PATH] OBJECT [--depth 1|2] [--direction incoming|outgoing|both] [--json]
dbgraph context [PATH] QUERY [--tokens N] [--json]
dbgraph diff [PATH] [--json]
dbgraph impact [PATH] OBJECT [--depth 1|2] [--json]
dbgraph analyze [PATH] [--scope all|risk|quality|performance] [--format text|json|markdown] [--output FILE] [--json]
dbgraph install [--target codex,cursor,claude] [--location DIR] [--yes] [--dry-run] [--print-config]
dbgraph uninstall [--target codex,cursor,claude] [--location DIR] [--dry-run]
dbgraph serve --mcp
```

## MCP Tools

Start the stdio MCP server:

```bash
dbgraph serve --mcp
```

| Tool | Purpose |
| --- | --- |
| `dbgraph_status` | Inspect initialization, snapshots, and graph index state. |
| `dbgraph_search` | Search graph objects. |
| `dbgraph_table` | Inspect one table. |
| `dbgraph_context` | Build compact AI task context. |
| `dbgraph_relations` | Traverse graph relations. |
| `dbgraph_impact` | Analyze affected objects. |
| `dbgraph_analyze` | Return structured risk, quality, and performance findings. |
| `dbgraph_diff` | Compare latest and previous snapshots. |
| `dbgraph_validate_sql` | Validate SQL references without executing SQL. |

MCP responses are JSON text content. Large responses include response budget metadata and suggested follow-up tools.

## Example: PostgreSQL Teashop

```bash
docker compose -f examples/postgres-teashop/docker-compose.yml up -d
export DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
dbgraph init -i --yes
dbgraph snapshot --profile stats
dbgraph analyze --scope all --format markdown --output teashop-analysis.md
docker compose -f examples/postgres-teashop/docker-compose.yml down -v
```

On PowerShell:

```powershell
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
```

Smoke test:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/integration/postgres-smoke.ps1
```

## Safety Defaults

| Safety Boundary | Behavior |
| --- | --- |
| Local-first | Project state is stored under `.dbgraph/`. |
| Database access | `dbgraph snapshot` is the command that connects to the configured database. |
| SQL validation | `dbgraph validate-sql` never executes SQL. |
| Analysis | `dbgraph analyze` works from the local snapshot and graph index. |
| Samples | Raw sample storage is off by default; sensitive samples are masked when sampling is explicitly enabled. |
| Secrets | Sensitive connection strings are not printed in provider errors. |
| Internal storage | SQLite provider refuses to snapshot `.dbgraph/dbgraph.db`. |

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

## Inspiration

DbGraph is inspired by the developer experience of [CodeGraph](https://github.com/colbymchenry/codegraph): fast local graph context, agent-friendly lookup, and structured answers instead of repeated grep/read loops. DbGraph applies that idea to database work by indexing schema objects, SQL artifacts, inferred relationships, impact paths, and review findings.

## Roadmap

- Add broader integration fixtures from real PostgreSQL schemas.
- Enable MySQL and SQL Server providers when local/containerized services are available.
- Improve ranking quality with persisted FTS-backed retrieval when needed.
- Expand SQL impact detection across more statement shapes.
