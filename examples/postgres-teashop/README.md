# PostgreSQL Teashop Example

This example provides a small commerce schema for DbGraph demos.

```powershell
docker compose -f examples/postgres-teashop/docker-compose.yml up -d
$env:DATABASE_URL="postgres://postgres:postgres@localhost:55432/teashop"
dbgraph init -i --yes
dbgraph snapshot --profile stats
dbgraph search order --kind table
dbgraph context "customer order payment"
dbgraph analyze --scope all --format markdown --output teashop-analysis.md
```

The `sql/orders.sql` file is discovered during snapshot and appears as SQL artifact/query graph context.
The analysis report should include sensitive column findings for customer email and payment provider tokens, plus a performance finding for `public.orders.status`.

Shut it down with:

```powershell
docker compose -f examples/postgres-teashop/docker-compose.yml down -v
```
