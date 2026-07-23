# Run the published Loam package setup in an isolated Windows scope.
# Invoked by windows-runtime-download-smoke.yml with Windows PowerShell 5.1.

[CmdletBinding()]
param(
  [string]$RuntimeVersion = $env:LOAM_RUNTIME_VERSION,
  [string]$PluginVersion = $env:LOAM_PLUGIN_VERSION
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($RuntimeVersion) -or $RuntimeVersion -notmatch '^\d+\.\d+\.\d+$') {
  throw "Runtime version must be SemVer X.Y.Z; got '$RuntimeVersion'"
}
if ([string]::IsNullOrWhiteSpace($PluginVersion) -or $PluginVersion -notmatch '^\d+\.\d+\.\d+$') {
  throw "Plugin version must be SemVer X.Y.Z; got '$PluginVersion'"
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('loam-packaged-' + [System.Guid]::NewGuid().ToString('N'))
$home = Join-Path $tempRoot 'home'
$workspace = Join-Path $tempRoot 'workspace'
$globalRoot = Join-Path $home '.agents\loam'

New-Item -ItemType Directory -Path $home, $workspace -Force | Out-Null

try {
  $env:HOME = $home
  $env:USERPROFILE = $home
  $env:LOAM_TARGET = 'x86_64-pc-windows-msvc'

  & npx.cmd --yes "@scchearn/loam@$PluginVersion" setup --yes
  if ($LASTEXITCODE -ne 0) {
    throw "Published setup failed with exit code $LASTEXITCODE"
  }

  $metadataPath = Join-Path $globalRoot 'install.json'
  if (-not (Test-Path -LiteralPath $metadataPath -PathType Leaf)) {
    throw "Published setup did not create $metadataPath"
  }
  $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
  if ($metadata.runtime_version -ne $RuntimeVersion) {
    throw "Setup selected runtime $($metadata.runtime_version), expected $RuntimeVersion"
  }
  if (-not (Test-Path -LiteralPath $metadata.runtime_path -PathType Leaf)) {
    throw "Installed runtime is missing at $($metadata.runtime_path)"
  }

  $stateOutput = & $metadata.runtime_path state --fast $workspace
  if ($LASTEXITCODE -ne 0) {
    throw "Installed runtime failed state --fast with exit code $LASTEXITCODE"
  }
  $state = ($stateOutput -join [Environment]::NewLine) | ConvertFrom-Json
  if ($null -eq $state) {
    throw 'Installed runtime did not return JSON state'
  }

  Write-Host "Windows packaged setup smoke test passed for @scchearn/loam@$PluginVersion and cli-v$RuntimeVersion"
  Write-Host "Installed runtime: $($metadata.runtime_path)"
} finally {
  Remove-Item $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
