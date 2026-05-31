# Provider Capability Matrix

Phase08 makes provider differences explicit. `SQLite` is implemented with a
local file fixture so it can be tested without installing an external database.
`MySQL` and SQL Server are registered as explicit skipped providers in this
build because this development machine does not have those services installed.

| Provider | Schema | Constraints | Indexes | Views | Routines | Triggers | Statistics | Sampling | Status |
|---|---|---|---|---|---|---|---|---|---|
| `postgres` | supported | supported | supported | supported | supported | supported | supported | unsupported | implemented |
| `sqlite` | supported | supported | supported | supported | unsupported | unsupported | supported | unsupported | implemented |
| `mysql` | unsupported | unsupported | unsupported | unsupported | unsupported | unsupported | unsupported | unsupported | skipped in this build |
| `sql-server` | unsupported | unsupported | unsupported | unsupported | unsupported | unsupported | unsupported | unsupported | skipped in this build |

Rules:

- Every registered provider must return a non-unknown capability matrix.
- Missing provider capabilities must be represented as `unsupported`, not by
  panics or missing registry entries.
- Context, search, relation, and impact code must tolerate unsupported optional
  capabilities.
- DbGraph still does not store raw business row data by default. SQLite table
  counts use `COUNT(*)` only and do not read row values into snapshots.
