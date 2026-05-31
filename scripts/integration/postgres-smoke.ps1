$ErrorActionPreference = "Stop"

if (-not $env:DATABASE_URL) {
  $env:DATABASE_URL = "postgres://postgres:postgres@localhost:55432/teashop"
}

$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("dbgraph-postgres-smoke-" + [System.Guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $work | Out-Null
Copy-Item -Recurse -Path (Join-Path $root "examples/postgres-teashop/sql") -Destination (Join-Path $work "sql")

function Invoke-DbGraphCli {
  param([string[]]$Arguments)

  $output = & cargo run --manifest-path (Join-Path $root "Cargo.toml") -p dbgraph-cli -- @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "dbgraph command failed: dbgraph $($Arguments -join ' ')"
  }
  $output | Out-Host
  return ($output -join "`n")
}

try {
  Push-Location $work
  Invoke-DbGraphCli @("init", "-i", "--yes") | Out-Null
  Invoke-DbGraphCli @("snapshot", "--profile", "stats") | Out-Null
  Invoke-DbGraphCli @("search", "orders", "--kind", "table") | Out-Null
  Invoke-DbGraphCli @("validate-sql", "--sql", "select * from orders") | Out-Null
  $analysis = Invoke-DbGraphCli @("analyze", "--scope", "all", "--json")
  if ($analysis -notmatch "public\.customers\.email") {
    throw "analysis smoke missing expected customer email risk"
  }
  if ($analysis -notmatch "public\.payments\.provider_token") {
    throw "analysis smoke missing expected provider token risk"
  }
  if ($analysis -notmatch "public\.orders\.status") {
    throw "analysis smoke missing expected orders status performance finding"
  }
  if ($analysis -notmatch "suggestedFix") {
    throw "analysis smoke missing suggested fixes"
  }
}
finally {
  Pop-Location
  Remove-Item -Recurse -Force $work
}
