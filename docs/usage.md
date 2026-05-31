# DbGraph Usage Guide

DbGraph is a local-first database context tool. The normal workflow is:

1. initialize a project
2. configure a database provider
3. capture a snapshot
4. use the local graph for search, validation, impact analysis, and review reports

DbGraph stores schema metadata, SQL artifacts, graph edges, and profile summaries. It does not execute SQL during validation, and it does not store business row data by default.

## Run From Source

When developing from this repository, prefix CLI commands with Cargo:

```powershell
cargo run -p dbgraph-cli -- --version
cargo run -p dbgraph-cli -- init -i --yes
```

If `dbgraph` is already installed on your `PATH`, use the shorter form:

```powershell
dbgraph --version
dbgraph init -i --yes
```

## Initialize A Project

Run this from the application or database-project directory you want DbGraph to index:

```powershell
dbgraph init -i --yes
```

This creates:

- `.dbgraph/dbgraph.config.json`
- `.dbgraph/snapshots/`
- `.dbgraph/instructions/`
- `.dbgraph/dbgraph.db` after the first successful snapshot

Check project state:

```powershell
dbgraph status
dbgraph status --json
```

## Configure A Database

DbGraph reads configuration from `.dbgraph/dbgraph.config.json`.

### PostgreSQL

The default interactive config uses `DATABASE_URL`.

```powershell
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
dbgraph snapshot --profile stats
```

### SQLite

SQLite does not require an external service. Set the provider and connection string in `.dbgraph/dbgraph.config.json`:

```json
{
  "version": 1,
  "database": {
    "provider": "sqlite",
    "connectionEnv": null,
    "connectionString": "C:/path/to/app.sqlite"
  },
  "snapshot": {
    "prettyJson": true,
    "profilingMode": "schema",
    "maxRowsPerTable": 20,
    "sampleRows": false
  },
  "security": {
    "storeRawData": false,
    "storeRawSamples": false,
    "maskPii": true,
    "customSensitiveTerms": []
  },
  "mcp": {
    "enabled": true,
    "maxResponseChars": 15000
  }
}
```

MySQL and SQL Server are registered as providers but are intentionally skipped in this build until local/container fixtures are available.

## Capture A Snapshot

```powershell
dbgraph snapshot
dbgraph snapshot --profile schema
dbgraph snapshot --profile stats
dbgraph snapshot --profile sample --max-rows-per-table 20
```

Profile modes:

- `schema`: schema-only metadata; safest default.
- `stats`: provider/catalog statistics such as row estimates.
- `sample`: explicit opt-in sampling; masked by default.

Snapshot output is written under `.dbgraph/snapshots/`, and the local graph index is rebuilt into `.dbgraph/dbgraph.db`.

## SQL Artifacts

During snapshot, DbGraph scans SQL files under these directories by default:

- `migrations/`
- `sql/`
- `db/`

It ignores noisy directories such as `node_modules/`, `target/`, `bin/`, and `obj/`.

SQL artifacts become query objects in the graph. DbGraph extracts read/write/filter/join dependencies where supported.

## Daily CLI Commands

Search graph objects:

```powershell
dbgraph search customer
dbgraph search orders --kind table
dbgraph search email --kind column --json
```

Inspect a table:

```powershell
dbgraph table public.orders
dbgraph table orders --json
```

Traverse relationships:

```powershell
dbgraph relations public.orders --depth 2
dbgraph relations public.orders --direction incoming
```

Build compact AI context:

```powershell
dbgraph context "refund payment order" --tokens 800
dbgraph context "which tables are touched by order status changes" --json
```

Validate SQL without executing it:

```powershell
dbgraph validate-sql --sql "select * from orders"
dbgraph validate-sql --file sql/orders.sql --dialect postgres --json
```

Compare latest snapshot with the previous snapshot:

```powershell
dbgraph diff
dbgraph diff --json
```

Check impact before changing an object:

```powershell
dbgraph impact public.orders.status
dbgraph impact public.orders.status --depth 2 --json
```

## Analysis Reports

Run the structured analyzer:

```powershell
dbgraph analyze --scope all
dbgraph analyze --scope risk
dbgraph analyze --scope quality
dbgraph analyze --scope performance
```

Output formats:

```powershell
dbgraph analyze --scope all --format text
dbgraph analyze --scope all --format json
dbgraph analyze --scope all --json
dbgraph analyze --scope all --format markdown --output dbgraph-analysis.md
```

The report includes:

- overview and risk score
- section summaries
- top findings
- severity counts
- evidence
- impact
- confidence
- suggested fixes
- related SQL/schema objects where available

Current rule groups:

- Security & Privacy: sensitive columns, SQL references to sensitive columns, broad `SELECT *`.
- Data Integrity & Schema Quality: missing primary keys, probable missing foreign keys.
- SQL Workload & Safety: `UPDATE` or `DELETE` statements without `WHERE`.
- Performance: filters or joins without supporting indexes.

## MCP Server

Start the stdio MCP server:

```powershell
dbgraph serve --mcp
```

Available MCP tools:

- `dbgraph_status`
- `dbgraph_search`
- `dbgraph_table`
- `dbgraph_context`
- `dbgraph_relations`
- `dbgraph_impact`
- `dbgraph_analyze`
- `dbgraph_diff`
- `dbgraph_validate_sql`

MCP responses are JSON text content. Large responses include `responseBudget` metadata and suggested follow-up calls.

## PostgreSQL Teashop Smoke Test

Run the example database:

```powershell
docker compose -f examples/postgres-teashop/docker-compose.yml up -d
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
powershell -ExecutionPolicy Bypass -File scripts/integration/postgres-smoke.ps1
docker compose -f examples/postgres-teashop/docker-compose.yml down -v
```

The smoke test initializes a temporary project, captures a Postgres snapshot, runs search, validates SQL, and verifies the structured analysis report contains expected risk/performance findings and suggested fixes.

## Safety Notes

- `dbgraph validate-sql` never executes SQL.
- `dbgraph analyze` works from the local snapshot and graph index.
- `dbgraph snapshot` is the command that connects to the configured database.
- Raw sample storage is off by default.
- Sensitive samples are masked when sampling is explicitly enabled.
- SQLite provider refuses to snapshot `.dbgraph/dbgraph.db`, so DbGraph's internal graph database is not confused with a business SQLite database.
