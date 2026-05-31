param(
  [string]$Version = $(if ($env:DBG_VERSION) { $env:DBG_VERSION } else { "latest" }),
  [string]$InstallDir = $(if ($env:DBG_INSTALL_DIR) { $env:DBG_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".dbgraph\bin" }),
  [string]$Repo = $(if ($env:DBG_REPO) { $env:DBG_REPO } else { "https://github.com/zhangsanfenggithub/dbgraph" }),
  [switch]$Help
)

$ErrorActionPreference = "Stop"

if ($Help) {
  Write-Host "Usage: install.ps1 [-Version VERSION] [-InstallDir DIR]"
  exit 0
}

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
switch ($arch) {
  "X64" { $target = "x86_64-pc-windows-msvc" }
  "Arm64" { $target = "aarch64-pc-windows-msvc" }
  default { throw "dbgraph: unsupported architecture $arch" }
}

$tag = $Version
if ($tag -eq "latest") {
  $response = Invoke-WebRequest -Uri "$Repo/releases/latest" -MaximumRedirection 0 -ErrorAction SilentlyContinue
  $location = $response.Headers.Location
  if (-not $location) {
    $location = (Invoke-WebRequest -Uri "$Repo/releases/latest" -MaximumRedirection 5).BaseResponse.ResponseUri.AbsoluteUri
  }
  $tag = Split-Path $location -Leaf
}
if (-not $tag.StartsWith("v")) { $tag = "v$tag" }

$asset = "dbgraph-$tag-$target.zip"
$base = "$Repo/releases/download/$tag"
$url = "$base/$asset"
$checksumUrl = "$url.sha256"
$tmp = Join-Path $env:TEMP ("dbgraph-" + [guid]::NewGuid().ToString())
$zip = Join-Path $tmp $asset
$sum = Join-Path $tmp "$asset.sha256"
$extract = Join-Path $tmp "extract"

try {
  New-Item -ItemType Directory -Force -Path $tmp, $extract, $InstallDir | Out-Null
  Write-Host "Installing DbGraph $tag ($target)..."
  Invoke-WebRequest -Uri $url -OutFile $zip
  Invoke-WebRequest -Uri $checksumUrl -OutFile $sum

  $expected = ((Get-Content -Raw $sum).Trim() -split "\s+")[0].ToLowerInvariant()
  $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
  if ($expected -ne $actual) { throw "dbgraph: sha256 checksum mismatch" }

  Expand-Archive -Path $zip -DestinationPath $extract -Force
  $binary = Get-ChildItem -Path $extract -Filter "dbgraph.exe" -Recurse | Select-Object -First 1
  if (-not $binary) { throw "dbgraph: archive did not contain dbgraph.exe" }

  $dest = Join-Path $InstallDir "dbgraph.exe"
  $staged = Join-Path $InstallDir "dbgraph.exe.tmp"
  Copy-Item -LiteralPath $binary.FullName -Destination $staged -Force
  Move-Item -LiteralPath $staged -Destination $dest -Force

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if (($userPath -split ";") -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$userPath", "User")
    Write-Host "Added $InstallDir to your user PATH. Restart your terminal to pick it up."
  }

  Write-Host "Installed $dest"
  Write-Host "Run: dbgraph --help"
  Write-Host "Uninstall hint: remove $dest and delete $InstallDir from your user PATH."
}
finally {
  if (Test-Path $tmp) {
    Remove-Item -LiteralPath $tmp -Recurse -Force
  }
}
