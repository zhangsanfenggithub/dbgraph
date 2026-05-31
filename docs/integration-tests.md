# Docker Integration Tests

The repository includes a PostgreSQL teashop example that can be run with Docker.

```powershell
docker compose -f examples/postgres-teashop/docker-compose.yml up -d
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
powershell -ExecutionPolicy Bypass -File scripts/integration/postgres-smoke.ps1
docker compose -f examples/postgres-teashop/docker-compose.yml down -v
```

The smoke script initializes a temporary DbGraph project, captures a PostgreSQL snapshot, runs graph queries, and validates a SQL artifact.
