# DbGraph Quickstart

DbGraph captures database schema into a local graph index and exposes CLI/MCP tools for search, context, diff, impact, SQL validation, and structured analysis reports.

For the complete workflow, see [usage.md](usage.md).
中文完整说明见 [usage.zh-CN.md](usage.zh-CN.md)。

## Install From This Repository

```powershell
cargo build --workspace
cargo run -p dbgraph-cli -- --version
```

## Initialize A Project

```powershell
dbgraph init -i --yes
```

This creates `.dbgraph/`, a config file, local snapshot storage, and agent instruction fragments.

## Configure A Database

SQLite works without an external service:

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

PostgreSQL uses `DATABASE_URL` by default:

```powershell
$env:DATABASE_URL="postgres://postgres:postgres@localhost:5432/teashop"
dbgraph snapshot
```

## Profiling Modes

```powershell
dbgraph snapshot --profile schema
dbgraph snapshot --profile stats
dbgraph snapshot --profile sample --max-rows-per-table 20
```

`schema` is the default and keeps no profile rows. `stats` keeps provider/catalog profile data. `sample` is explicit opt-in and remains masked by default.

## Daily Commands

```powershell
dbgraph status
dbgraph search customer --kind table
dbgraph table public.orders
dbgraph relations public.orders --depth 2
dbgraph context "refund payment order" --tokens 800
dbgraph validate-sql --sql "select * from orders"
dbgraph diff
dbgraph impact public.orders.status
dbgraph analyze --scope all
dbgraph analyze --scope risk --json
dbgraph analyze --scope all --format markdown --output dbgraph-analysis.md
dbgraph sync
dbgraph benchmark --tables 10000 --columns-per-table 2
```

## Full Analysis Report

```powershell
dbgraph analyze --scope all --format markdown --output dbgraph-analysis.md
```

The markdown report includes an overview, section summaries, top findings, evidence, impact, confidence, related objects, and suggested fixes.
