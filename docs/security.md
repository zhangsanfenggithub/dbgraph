# DbGraph Security Notes

DbGraph is designed to be schema-first and local by default.

## Defaults

- Raw business rows are not stored.
- Raw samples are not stored.
- PII masking is enabled.
- `snapshot.profilingMode` defaults to `schema`.
- Sampling requires explicit `profilingMode: "sample"` or `dbgraph snapshot --profile sample`.

## PII Detection

PII scoring uses column name, type, comments, metadata comments, and custom terms:

```json
{
  "security": {
    "storeRawData": false,
    "storeRawSamples": false,
    "maskPii": true,
    "customSensitiveTerms": ["tax_id", "national_id"]
  }
}
```

Sensitive columns receive a `piiScore` in column profiles. Sample summaries mask sensitive values even when raw sample storage is explicitly enabled.

## Sampling Policy

Safe sampling is bounded by `snapshot.maxRowsPerTable`. Providers should use deterministic limit sampling or random sampling with a statement timeout. Stored sample summaries should contain counts, masked examples, and no sensitive raw values.

## Local Files To Protect

- `.dbgraph/dbgraph.config.json` may contain connection details if `connectionString` is used.
- `.dbgraph/snapshots/*.json` contains schema metadata and profile summaries.
- `.dbgraph/dbgraph.db` contains the local graph index.

Prefer `connectionEnv` over plaintext `connectionString` when possible.
