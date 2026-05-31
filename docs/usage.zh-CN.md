# DbGraph 使用说明

DbGraph 是一个本地优先的数据库上下文工具。典型使用流程是：

1. 初始化项目
2. 配置数据库 provider
3. 生成数据库 snapshot
4. 基于本地图索引执行搜索、SQL 校验、影响分析和审查报告

DbGraph 默认保存的是 schema 元数据、SQL artifact、图关系和 profile 摘要。`validate-sql` 不会执行 SQL，默认也不会保存业务行数据。

## 从源码运行

如果你是在当前仓库里开发或测试，可以用 Cargo 前缀运行：

```powershell
cargo run -p dbgraph-cli -- --version
cargo run -p dbgraph-cli -- init -i --yes
```

如果已经把 `dbgraph` 安装到 `PATH`，可以直接运行：

```powershell
dbgraph --version
dbgraph init -i --yes
```

## 初始化项目

在你想让 DbGraph 索引的应用项目或数据库项目目录下运行：

```powershell
dbgraph init -i --yes
```

该命令会创建：

- `.dbgraph/dbgraph.config.json`
- `.dbgraph/snapshots/`
- `.dbgraph/instructions/`
- 第一次成功 snapshot 后会生成 `.dbgraph/dbgraph.db`

查看当前项目状态：

```powershell
dbgraph status
dbgraph status --json
```

## 配置数据库

DbGraph 从 `.dbgraph/dbgraph.config.json` 读取配置。

### PostgreSQL

交互式默认配置会使用 `DATABASE_URL`：

```powershell
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
dbgraph snapshot --profile stats
```

### SQLite

SQLite 不需要外部服务。把 `.dbgraph/dbgraph.config.json` 设置为类似下面的配置：

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

MySQL 和 SQL Server 目前已经注册为 provider，但当前构建里会显式跳过，等本地或容器化测试服务补齐后再启用。

## 生成 Snapshot

```powershell
dbgraph snapshot
dbgraph snapshot --profile schema
dbgraph snapshot --profile stats
dbgraph snapshot --profile sample --max-rows-per-table 20
```

Profile 模式：

- `schema`：只采集 schema 元数据，默认且最安全。
- `stats`：采集 provider/catalog 统计信息，例如行数估计。
- `sample`：显式开启采样；默认仍会进行敏感信息 mask。

Snapshot 会写入 `.dbgraph/snapshots/`，本地图索引会重建到 `.dbgraph/dbgraph.db`。

## SQL Artifact

生成 snapshot 时，DbGraph 默认扫描这些目录下的 `.sql` 文件：

- `migrations/`
- `sql/`
- `db/`

它会忽略噪声目录，例如：

- `node_modules/`
- `target/`
- `bin/`
- `obj/`

SQL artifact 会成为图里的 query object。DbGraph 会尽量提取读、写、过滤、JOIN 等依赖关系。

## 常用 CLI 命令

搜索图对象：

```powershell
dbgraph search customer
dbgraph search orders --kind table
dbgraph search email --kind column --json
```

查看表结构：

```powershell
dbgraph table public.orders
dbgraph table orders --json
```

查看关系：

```powershell
dbgraph relations public.orders --depth 2
dbgraph relations public.orders --direction incoming
```

为 AI 任务构建紧凑上下文：

```powershell
dbgraph context "refund payment order" --tokens 800
dbgraph context "which tables are touched by order status changes" --json
```

校验 SQL，但不执行 SQL：

```powershell
dbgraph validate-sql --sql "select * from orders"
dbgraph validate-sql --file sql/orders.sql --dialect postgres --json
```

比较最新 snapshot 和上一个 snapshot：

```powershell
dbgraph diff
dbgraph diff --json
```

修改对象前做影响分析：

```powershell
dbgraph impact public.orders.status
dbgraph impact public.orders.status --depth 2 --json
```

## 分析报告

运行结构化分析：

```powershell
dbgraph analyze --scope all
dbgraph analyze --scope risk
dbgraph analyze --scope quality
dbgraph analyze --scope performance
```

输出格式：

```powershell
dbgraph analyze --scope all --format text
dbgraph analyze --scope all --format json
dbgraph analyze --scope all --json
dbgraph analyze --scope all --format markdown --output dbgraph-analysis.md
```

分析报告包含：

- 总览和风险分数
- 分区摘要
- Top findings
- 严重程度计数
- 证据
- 影响说明
- 置信度
- 建议修复方式
- 可关联的 SQL/schema 对象

当前规则分组：

- Security & Privacy：敏感列、SQL 读取敏感列、宽泛的 `SELECT *`。
- Data Integrity & Schema Quality：缺失主键、疑似缺失外键。
- SQL Workload & Safety：没有 `WHERE` 的 `UPDATE` 或 `DELETE`。
- Performance：过滤或 JOIN 使用的列缺少支撑索引。

## MCP Server

启动 stdio MCP server：

```powershell
dbgraph serve --mcp
```

当前 MCP 工具：

- `dbgraph_status`
- `dbgraph_search`
- `dbgraph_table`
- `dbgraph_context`
- `dbgraph_relations`
- `dbgraph_impact`
- `dbgraph_analyze`
- `dbgraph_diff`
- `dbgraph_validate_sql`

MCP 响应是 JSON text content。较大的响应会包含 `responseBudget` 元数据和建议的后续调用。

## PostgreSQL Teashop Smoke Test

运行示例数据库：

```powershell
docker compose -f examples/postgres-teashop/docker-compose.yml up -d
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
powershell -ExecutionPolicy Bypass -File scripts/integration/postgres-smoke.ps1
docker compose -f examples/postgres-teashop/docker-compose.yml down -v
```

Smoke test 会在临时目录初始化 DbGraph 项目，生成 PostgreSQL snapshot，运行搜索，校验 SQL，并验证结构化分析报告里包含预期的风险、性能 finding 和建议修复方式。

## 安全说明

- `dbgraph validate-sql` 不会执行 SQL。
- `dbgraph analyze` 只基于本地 snapshot 和图索引工作。
- `dbgraph snapshot` 是会连接配置数据库的命令。
- 默认关闭 raw sample 存储。
- 显式开启采样时，敏感样本默认会被 mask。
- SQLite provider 会拒绝 snapshot `.dbgraph/dbgraph.db`，避免把 DbGraph 内部图数据库误当成业务 SQLite 数据库。

