# Download and execute a published Loam runtime in an isolated Windows scope.
# Invoked by windows-runtime-download-smoke.yml with Windows PowerShell 5.1.

[CmdletBinding()]
param(
  [string]$RuntimeVersion = $env:LOAM_RUNTIME_VERSION
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($RuntimeVersion) -or $RuntimeVersion -notmatch '^\d+\.\d+\.\d+$') {
  throw "Runtime version must be SemVer X.Y.Z; got '$RuntimeVersion'"
}

$repoLauncher = (Resolve-Path (Join-Path $PSScriptRoot '..\..\skills\loam-using\scripts\loam.ps1')).Path
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ('loam-live-' + [System.Guid]::NewGuid().ToString('N'))
$scriptDir = Join-Path $tempRoot '.agents\skills\loam-using\scripts'
$launcher = Join-Path $scriptDir 'loam.ps1'
$versionFile = Join-Path $scriptDir 'CLI_VERSION'
$target = 'x86_64-pc-windows-msvc'
$runtime = Join-Path $tempRoot ".agents\loam\bin\$RuntimeVersion\$target\loam.exe"

New-Item -ItemType Directory -Path $scriptDir -Force | Out-Null

try {
  Copy-Item $repoLauncher $launcher -Force
  Set-Content -Path $versionFile -Value $RuntimeVersion -Encoding ASCII
  $env:LOAM_TARGET = $target

  $resolvedPath = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher --loam-runtime-path
  if ($LASTEXITCODE -ne 0 -or $resolvedPath.Trim() -ne $runtime) {
    throw "Launcher resolved an unexpected runtime path: $resolvedPath"
  }

  & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher --loam-bootstrap
  if ($LASTEXITCODE -ne 0) {
    throw "Runtime bootstrap failed with exit code $LASTEXITCODE"
  }
  if (-not (Test-Path -LiteralPath $runtime -PathType Leaf)) {
    throw "Runtime was not installed at $runtime"
  }

  $stateOutput = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher state --fast $tempRoot
  if ($LASTEXITCODE -ne 0) {
    throw "Downloaded runtime failed state --fast with exit code $LASTEXITCODE"
  }
  $state = ($stateOutput -join [Environment]::NewLine) | ConvertFrom-Json
  if ($null -eq $state) {
    throw 'Downloaded runtime did not return JSON state'
  }

  Write-Host "Windows runtime smoke test passed for cli-v$RuntimeVersion"
  Write-Host "Installed runtime: $runtime"
} finally {
  Remove-Item $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
