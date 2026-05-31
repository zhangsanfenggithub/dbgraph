//! Installer, release, and agent configuration support for `DbGraph`.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Start marker for managed instruction fragments.
pub const DBGRAPH_SECTION_START: &str = "<!-- DBGRAPH_START -->";

/// End marker for managed instruction fragments.
pub const DBGRAPH_SECTION_END: &str = "<!-- DBGRAPH_END -->";

/// Start marker for managed MCP configuration blocks.
pub const DBGRAPH_MCP_SECTION_START: &str = "<!-- DBGRAPH_MCP_START -->";

/// End marker for managed MCP configuration blocks.
pub const DBGRAPH_MCP_SECTION_END: &str = "<!-- DBGRAPH_MCP_END -->";

/// Supported release operating systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOs {
    /// Apple macOS.
    Macos,
    /// Linux using the GNU target.
    Linux,
    /// Microsoft Windows.
    Windows,
}

/// Supported release CPU architectures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseArch {
    /// `x86_64` / `amd64`.
    X64,
    /// `arm64` / `aarch64`.
    Arm64,
}

/// One prebuilt binary target in the `DbGraph` release matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseTarget {
    /// Operating system.
    pub os: ReleaseOs,
    /// CPU architecture.
    pub arch: ReleaseArch,
    /// Rust target triple used in release artifact names.
    pub triple: &'static str,
    /// Archive extension for the artifact.
    pub archive_extension: &'static str,
}

impl ReleaseTarget {
    /// Returns the release artifact file name for a version.
    #[must_use]
    pub fn artifact_name(self, version: &str) -> String {
        format!(
            "dbgraph-{}-{}.{}",
            normalize_release_tag(version),
            self.triple,
            self.archive_extension
        )
    }

    /// Returns the checksum sidecar file name for a version.
    #[must_use]
    pub fn checksum_name(self, version: &str) -> String {
        format!("{}.sha256", self.artifact_name(version))
    }

    /// Returns the GitHub release download URL for this artifact.
    #[must_use]
    pub fn download_url(self, repository: &str, version: &str) -> String {
        let repo = repository.trim_end_matches('/');
        let tag = normalize_release_tag(version);
        format!(
            "{repo}/releases/download/{tag}/{}",
            self.artifact_name(version)
        )
    }

    /// Returns the checksum sidecar download URL for this artifact.
    #[must_use]
    pub fn checksum_url(self, repository: &str, version: &str) -> String {
        let repo = repository.trim_end_matches('/');
        let tag = normalize_release_tag(version);
        format!(
            "{repo}/releases/download/{tag}/{}",
            self.checksum_name(version)
        )
    }
}

/// Returns all supported `DbGraph` release targets.
#[must_use]
pub const fn release_targets() -> [ReleaseTarget; 6] {
    [
        ReleaseTarget {
            os: ReleaseOs::Macos,
            arch: ReleaseArch::X64,
            triple: "x86_64-apple-darwin",
            archive_extension: "tar.gz",
        },
        ReleaseTarget {
            os: ReleaseOs::Macos,
            arch: ReleaseArch::Arm64,
            triple: "aarch64-apple-darwin",
            archive_extension: "tar.gz",
        },
        ReleaseTarget {
            os: ReleaseOs::Linux,
            arch: ReleaseArch::X64,
            triple: "x86_64-unknown-linux-gnu",
            archive_extension: "tar.gz",
        },
        ReleaseTarget {
            os: ReleaseOs::Linux,
            arch: ReleaseArch::Arm64,
            triple: "aarch64-unknown-linux-gnu",
            archive_extension: "tar.gz",
        },
        ReleaseTarget {
            os: ReleaseOs::Windows,
            arch: ReleaseArch::X64,
            triple: "x86_64-pc-windows-msvc",
            archive_extension: "zip",
        },
        ReleaseTarget {
            os: ReleaseOs::Windows,
            arch: ReleaseArch::Arm64,
            triple: "aarch64-pc-windows-msvc",
            archive_extension: "zip",
        },
    ]
}

/// Returns a release tag in `vX.Y.Z` form.
#[must_use]
pub fn normalize_release_tag(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed == "latest" || trimmed.starts_with('v') {
        trimmed.to_owned()
    } else {
        format!("v{trimmed}")
    }
}

/// Supported agent configuration targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Codex CLI/app configuration.
    Codex,
    /// Claude Desktop/Code style configuration.
    Claude,
    /// Cursor MCP configuration.
    Cursor,
    /// Gemini CLI configuration.
    Gemini,
    /// opencode configuration.
    Opencode,
}

impl AgentKind {
    /// Returns the stable CLI name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
        }
    }

    /// Parses a stable target name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "cursor" => Some(Self::Cursor),
            "gemini" => Some(Self::Gemini),
            "opencode" | "open-code" => Some(Self::Opencode),
            _ => None,
        }
    }

    /// Returns every supported target.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Codex,
            Self::Claude,
            Self::Cursor,
            Self::Gemini,
            Self::Opencode,
        ]
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Abstraction for an agent configuration target.
pub trait AgentTarget {
    /// Returns this target's kind.
    fn kind(&self) -> AgentKind;

    /// Detects the configuration file path.
    fn config_path(&self, location: Option<&Path>) -> PathBuf;

    /// Renders the managed MCP configuration block.
    fn render_block(&self, binary: &str) -> String {
        render_mcp_managed_block(self.kind(), binary)
    }
}

impl AgentTarget for AgentKind {
    fn kind(&self) -> AgentKind {
        *self
    }

    fn config_path(&self, location: Option<&Path>) -> PathBuf {
        let base = location.map_or_else(default_config_base, Path::to_path_buf);
        match self {
            Self::Codex => base.join(".codex").join("dbgraph-mcp.json"),
            Self::Claude => base.join(".claude").join("dbgraph-mcp.json"),
            Self::Cursor => base.join(".cursor").join("mcp.json"),
            Self::Gemini => base.join(".gemini").join("settings.json"),
            Self::Opencode => base.join(".opencode").join("mcp.json"),
        }
    }
}

/// Result of an agent config write/remove operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallEdit {
    /// Target kind that was edited.
    pub target: AgentKind,
    /// Config path affected by the operation.
    pub path: PathBuf,
    /// Whether file content changed.
    pub changed: bool,
    /// Backup path created before modifying an existing file.
    pub backup_path: Option<PathBuf>,
    /// Whether this was a dry-run operation.
    pub dry_run: bool,
}

/// Parses a comma-separated list of agent targets.
///
/// # Errors
///
/// Returns an error string when any target name is unknown.
pub fn parse_agent_kinds(value: Option<&str>) -> std::result::Result<Vec<AgentKind>, String> {
    let Some(value) = value else {
        return Ok(AgentKind::all().to_vec());
    };
    let mut parsed = Vec::new();
    for raw in value.split(',').filter(|item| !item.trim().is_empty()) {
        let kind = AgentKind::parse(raw)
            .ok_or_else(|| format!("unknown agent target `{}`", raw.trim()))?;
        if !parsed.contains(&kind) {
            parsed.push(kind);
        }
    }
    if parsed.is_empty() {
        return Err("at least one target is required".to_owned());
    }
    Ok(parsed)
}

/// Renders the MCP JSON payload for an agent target.
#[must_use]
pub fn render_mcp_config(kind: AgentKind, binary: &str) -> String {
    format!(
        "{{\n  \"mcpServers\": {{\n    \"dbgraph\": {{\n      \"command\": \"{}\",\n      \"args\": [\"serve\", \"--mcp\"],\n      \"description\": \"DbGraph read-only database context for {}\"\n    }}\n  }}\n}}\n",
        escape_json(binary),
        kind
    )
}

/// Renders the managed MCP block inserted into agent config files.
#[must_use]
pub fn render_mcp_managed_block(kind: AgentKind, binary: &str) -> String {
    format!(
        "{DBGRAPH_MCP_SECTION_START}\n{}\n{DBGRAPH_MCP_SECTION_END}\n",
        render_mcp_config(kind, binary).trim_end()
    )
}

/// Inserts or replaces the `DbGraph` managed MCP block.
#[must_use]
pub fn upsert_managed_block(existing: &str, block: &str) -> String {
    if let Some((before, after_start)) = existing.split_once(DBGRAPH_MCP_SECTION_START) {
        if let Some((_, after)) = after_start.split_once(DBGRAPH_MCP_SECTION_END) {
            return format!(
                "{}{}{}",
                before.trim_end(),
                if before.trim().is_empty() { "" } else { "\n\n" },
                [block.trim_end(), after.trim_start()].join("\n")
            )
            .trim_end()
            .to_owned()
                + "\n";
        }
    }

    format!(
        "{}{}{}\n",
        existing.trim_end(),
        if existing.trim().is_empty() {
            ""
        } else {
            "\n\n"
        },
        block.trim_end()
    )
}

/// Removes only the `DbGraph` managed MCP block.
#[must_use]
pub fn remove_managed_block(existing: &str) -> String {
    if let Some((before, after_start)) = existing.split_once(DBGRAPH_MCP_SECTION_START) {
        if let Some((_, after)) = after_start.split_once(DBGRAPH_MCP_SECTION_END) {
            return format!(
                "{}{}",
                before.trim_end(),
                if after.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{}", after.trim_start())
                }
            )
            .trim_end()
            .to_owned()
                + "\n";
        }
    }
    existing.to_owned()
}

/// Writes or updates a target's managed MCP configuration block.
///
/// # Errors
///
/// Returns an IO error when reading, backing up, creating directories, or
/// writing the config file fails.
pub fn install_agent_config(
    target: AgentKind,
    location: Option<&Path>,
    binary: &str,
    dry_run: bool,
) -> io::Result<InstallEdit> {
    let path = target.config_path(location);
    let block = target.render_block(binary);
    let existing = read_optional(&path)?;
    let next = upsert_managed_block(&existing, &block);
    write_changed(target, path, &existing, next, dry_run)
}

/// Removes a target's managed MCP configuration block.
///
/// # Errors
///
/// Returns an IO error when reading, backing up, or writing the config file
/// fails.
pub fn uninstall_agent_config(
    target: AgentKind,
    location: Option<&Path>,
    dry_run: bool,
) -> io::Result<InstallEdit> {
    let path = target.config_path(location);
    let existing = read_optional(&path)?;
    let next = remove_managed_block(&existing);
    write_changed(target, path, &existing, next, dry_run)
}

/// Instruction targets supported by the template renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionTarget {
    /// Generic `AGENTS.md` fragment.
    AgentsMd,
    /// `CLAUDE.md` fragment.
    ClaudeMd,
    /// Cursor `.mdc` rule fragment.
    CursorRule,
}

impl InstructionTarget {
    /// Returns the default file name for the target.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::AgentsMd => "AGENTS.md.fragment",
            Self::ClaudeMd => "CLAUDE.md.fragment",
            Self::CursorRule => "dbgraph.mdc",
        }
    }

    fn cursor_frontmatter(self) -> &'static str {
        match self {
            Self::CursorRule => {
                "---\ndescription: DbGraph database context rules\nalwaysApply: true\n---\n\n"
            }
            Self::AgentsMd | Self::ClaudeMd => "",
        }
    }
}

/// Render a stable instruction fragment for an agent target.
#[must_use]
pub fn render_instruction_fragment(target: InstructionTarget) -> String {
    format!(
        "{frontmatter}{start}\n## DbGraph\n\n{body}\n{end}\n",
        frontmatter = target.cursor_frontmatter(),
        start = DBGRAPH_SECTION_START,
        body = INSTRUCTION_BODY,
        end = DBGRAPH_SECTION_END
    )
}

/// Render all supported instruction fragments.
#[must_use]
pub fn render_all_instruction_fragments() -> Vec<(InstructionTarget, String)> {
    [
        InstructionTarget::AgentsMd,
        InstructionTarget::ClaudeMd,
        InstructionTarget::CursorRule,
    ]
    .into_iter()
    .map(|target| (target, render_instruction_fragment(target)))
    .collect()
}

const INSTRUCTION_BODY: &str = r#"This project can use DbGraph database context through CLI/MCP tools.

### When To Use DbGraph

Use DbGraph for database-structure questions, especially before editing SQL,
migrations, ORM models, data-access code, or API behavior that depends on
tables, columns, keys, indexes, views, triggers, or query workload.

| Question | Tool |
|---|---|
| "What tables or columns match X?" | `dbgraph_search` |
| "Show me table X" | `dbgraph_table` |
| "What references or depends on X?" | `dbgraph_relations` |
| "What context do I need for this DB task?" | `dbgraph_context` |
| "What could break if this schema changes?" | `dbgraph_impact` |

### Rules

- Do not guess table names, column names, relation directions, or constraint
  behavior when DbGraph context is available. Query DbGraph first.
- Treat explicit database constraints as authoritative. Treat inferred
  relations as hints and label them as inferred.
- DbGraph is read-only for target business databases by default. Do not execute
  DDL, DML, or AI-generated write SQL through DbGraph.
- DbGraph must not store raw business row data by default. Sampling must be
  explicit opt-in and masked according to project configuration.
- Use native file search only for literal source text; use DbGraph for database
  structure, relationships, context, and impact.
"#;

fn default_config_base() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn escape_json(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn read_optional(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err),
    }
}

fn write_changed(
    target: AgentKind,
    path: PathBuf,
    existing: &str,
    next: String,
    dry_run: bool,
) -> io::Result<InstallEdit> {
    if existing == next {
        return Ok(InstallEdit {
            target,
            path,
            changed: false,
            backup_path: None,
            dry_run,
        });
    }
    if dry_run {
        return Ok(InstallEdit {
            target,
            path,
            changed: true,
            backup_path: None,
            dry_run,
        });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let backup_path = if path.exists() {
        let backup = path.with_extension("dbgraph.bak");
        fs::copy(&path, &backup)?;
        Some(backup)
    } else {
        None
    };

    fs::write(&path, next)?;
    Ok(InstallEdit {
        target,
        path,
        changed: true,
        backup_path,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_fragment_contains_safety_boundaries() {
        let fragment = render_instruction_fragment(InstructionTarget::AgentsMd);

        assert!(fragment.contains(DBGRAPH_SECTION_START));
        assert!(fragment.contains("Do not guess table names"));
        assert!(fragment.contains("read-only"));
        assert!(fragment.contains("must not store raw business row data"));
    }

    #[test]
    fn cursor_fragment_has_frontmatter() {
        let fragment = render_instruction_fragment(InstructionTarget::CursorRule);

        assert!(fragment.starts_with("---\n"));
        assert!(fragment.contains("alwaysApply: true"));
        assert!(fragment.contains(DBGRAPH_SECTION_END));
    }

    #[test]
    fn all_fragments_have_stable_file_names() {
        let rendered = render_all_instruction_fragments();
        let names = rendered
            .iter()
            .map(|(target, _)| target.file_name())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec!["AGENTS.md.fragment", "CLAUDE.md.fragment", "dbgraph.mdc"]
        );
    }

    #[test]
    fn release_matrix_has_six_unambiguous_targets_and_checksum_urls() {
        let targets = release_targets();
        let triples = targets
            .iter()
            .map(|target| target.triple)
            .collect::<Vec<_>>();

        assert_eq!(
            triples,
            vec![
                "x86_64-apple-darwin",
                "aarch64-apple-darwin",
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "x86_64-pc-windows-msvc",
                "aarch64-pc-windows-msvc",
            ]
        );

        for target in targets {
            let artifact = target.artifact_name("0.1.0");
            assert!(artifact.starts_with("dbgraph-v0.1.0-"));
            assert!(artifact.contains(target.triple));
            assert_eq!(target.checksum_name("0.1.0"), format!("{artifact}.sha256"));
            assert_eq!(
                target.download_url("https://github.com/zhangsanfenggithub/dbgraph", "0.1.0"),
                format!(
                    "https://github.com/zhangsanfenggithub/dbgraph/releases/download/v0.1.0/{artifact}"
                )
            );
        }
    }

    #[test]
    fn agent_target_config_is_idempotent_and_backed_up() {
        let temp = TempDir::new("agent-config");
        let path = AgentKind::Codex.config_path(Some(&temp.root));
        fs::create_dir_all(path.parent().expect("path has parent")).expect("dir should create");
        fs::write(&path, "{ \"user\": true }\n").expect("existing config should write");

        let first = install_agent_config(AgentKind::Codex, Some(&temp.root), "dbgraph", false)
            .expect("install should write");
        let second = install_agent_config(AgentKind::Codex, Some(&temp.root), "dbgraph", false)
            .expect("install should be idempotent");
        let stored = fs::read_to_string(&path).expect("config should read");

        assert!(first.changed);
        assert!(first.backup_path.is_some());
        assert!(!second.changed);
        assert!(stored.contains("\"command\": \"dbgraph\""));
        assert!(stored.contains("\"args\": [\"serve\", \"--mcp\"]"));
        assert_eq!(stored.matches(DBGRAPH_MCP_SECTION_START).count(), 1);
    }

    #[test]
    fn uninstall_removes_only_managed_block() {
        let user_config = "{ \"user\": true }\n";
        let block = render_mcp_managed_block(AgentKind::Cursor, "dbgraph");
        let combined = upsert_managed_block(user_config, &block);

        let removed = remove_managed_block(&combined);

        assert!(removed.contains("{ \"user\": true }"));
        assert!(!removed.contains(DBGRAPH_MCP_SECTION_START));
        assert!(!removed.contains("\"dbgraph\""));
    }

    #[test]
    fn install_artifacts_have_static_contracts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crate lives under workspace");
        let sh = fs::read_to_string(root.join("install.sh")).expect("install.sh should exist");
        let ps1 = fs::read_to_string(root.join("install.ps1")).expect("install.ps1 should exist");
        let package_json =
            fs::read_to_string(root.join("npm").join("package.json")).expect("npm package exists");
        let npm_bin = fs::read_to_string(root.join("npm").join("bin").join("dbgraph.js"))
            .expect("npm bin exists");

        assert!(sh.contains("--install-dir"));
        assert!(sh.contains("--version"));
        assert!(sh.contains("sha256"));
        assert!(sh.contains("x86_64-unknown-linux-gnu"));
        assert!(ps1.contains("InstallDir"));
        assert!(ps1.contains("Get-FileHash"));
        assert!(ps1.contains("Expand-Archive"));
        assert!(package_json.contains("\"name\": \"@dbgraph/cli\""));
        assert!(package_json.contains("\"dbgraph\": \"bin/dbgraph.js\""));
        assert!(npm_bin.contains("childProcess.spawn"));
        assert!(npm_bin.contains("process.argv.slice(2)"));
        assert!(npm_bin.contains("x86_64-pc-windows-msvc"));
    }

    struct TempDir {
        root: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be valid")
                .as_nanos();
            let root = env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
            Self { root }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if self.root.exists() {
                fs::remove_dir_all(&self.root).expect("temp dir should remove");
            }
        }
    }
}
