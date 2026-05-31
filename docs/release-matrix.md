# DbGraph Release Matrix

DbGraph publishes one prebuilt archive per supported Rust target triple.
All artifacts use an unambiguous versioned name:

```text
dbgraph-v{version}-{target}.{tar.gz|zip}
dbgraph-v{version}-{target}.{tar.gz|zip}.sha256
```

`latest` may be resolved through the GitHub release redirect or API, but pinned
downloads should use a `vX.Y.Z` tag.

| OS | Arch | Target | Archive |
|---|---|---|---|
| macOS | x64 | `x86_64-apple-darwin` | `tar.gz` |
| macOS | arm64 | `aarch64-apple-darwin` | `tar.gz` |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `tar.gz` |
| Linux | arm64 | `aarch64-unknown-linux-gnu` | `tar.gz` |
| Windows | x64 | `x86_64-pc-windows-msvc` | `zip` |
| Windows | arm64 | `aarch64-pc-windows-msvc` | `zip` |

Download URL convention:

```text
https://github.com/zhangsanfenggithub/dbgraph/releases/download/v{version}/dbgraph-v{version}-{target}.{ext}
https://github.com/zhangsanfenggithub/dbgraph/releases/download/v{version}/dbgraph-v{version}-{target}.{ext}.sha256
```

Installers and npm wrappers must derive URLs from this matrix rather than
maintaining separate platform-specific naming rules.
