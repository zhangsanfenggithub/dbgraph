# Performance And Scale

Use the built-in benchmark command to generate deterministic synthetic schemas:

```powershell
dbgraph benchmark --tables 1000 --columns-per-table 4 --json
dbgraph benchmark --tables 10000 --columns-per-table 2 --json
```

Storage rebuilds use a single SQLite transaction and batched prepared statements. The test suite includes a 10k-object storage rebuild and a 10k-table synthetic schema generation check.
