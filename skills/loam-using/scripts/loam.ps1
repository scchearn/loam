# loam.ps1 — PowerShell scope resolver, bootstrapper, and launcher for the
# native loam runtime. Twin of loam.sh.
#
# Invoke exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File loam.ps1 <args>
#
# Written for in-box Windows PowerShell 5.1: no ternaries, no null-coalescing,
# no PowerShell 7 cmdlets. pwsh works but is not required.
#
# Exit codes: the runtime's own when it runs; 75 when the runtime is not yet
# available; 78 for invalid CLI_VERSION or an unsupported target.

param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Arguments = @()
)

$ErrorActionPreference = 'Stop'

$RepoReleaseBase = 'https://github.com/scchearn/loam/releases/download'
$SupportedTargets = @(
  'x86_64-apple-darwin',
  'aarch64-apple-darwin',
  'x86_64-pc-windows-msvc',
  'x86_64-unknown-linux-musl',
  'aarch64-unknown-linux-musl'
)
$MarkerStaleSeconds = 600

# Logical launcher directory. $PSScriptRoot follows the invocation path rather
# than a junction/symlink target, which is exactly the scope contract.
$ScriptDir = $PSScriptRoot

function Get-AgentsRoot {
  $dir = $ScriptDir
  while ($dir) {
    if ((Split-Path $dir -Leaf) -eq '.agents') { return $dir }
    $parent = Split-Path $dir -Parent
    if ($parent -eq $dir) { break }
    $dir = $parent
  }
  return (Join-Path $env:USERPROFILE '.agents')
}

$AgentsRoot = Get-AgentsRoot
$RuntimeRoot = Join-Path $AgentsRoot 'loam'
$InstallLog = Join-Path $RuntimeRoot 'install.log'

function Initialize-AgentsGitignore {
  $file = Join-Path $AgentsRoot '.gitignore'
  if (Test-Path $file) { return }
  try {
    if (-not (Test-Path $AgentsRoot)) { New-Item -ItemType Directory -Path $AgentsRoot -Force | Out-Null }
    Set-Content -Path $file -Value '*' -Encoding ASCII
  } catch { }
}

function Get-CliVersion {
  $file = Join-Path $ScriptDir 'CLI_VERSION'
  if (-not (Test-Path $file -PathType Leaf)) { return $null }
  $value = (Get-Content $file -Raw).Trim()
  if ($value -notmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { return $null }
  return $value
}

function Get-Target {
  if ($env:LOAM_TARGET) { return $env:LOAM_TARGET }
  # PowerShell 5.1 is Windows-only in practice; PowerShell 7 on Unix still
  # reports a usable platform through $IsWindows/$IsMacOS.
  if (($PSVersionTable.PSVersion.Major -lt 6) -or $IsWindows) { return 'x86_64-pc-windows-msvc' }
  $machine = (uname -m)
  if ($IsMacOS) {
    if ($machine -eq 'arm64' -or $machine -eq 'aarch64') { return 'aarch64-apple-darwin' }
    return 'x86_64-apple-darwin'
  }
  if ($machine -eq 'aarch64' -or $machine -eq 'arm64') { return 'aarch64-unknown-linux-musl' }
  return 'x86_64-unknown-linux-musl'
}

function Get-RuntimeBinary($version, $target) {
  $name = 'loam'
  if ($target -like '*windows*') { $name = 'loam.exe' }
  return (Join-Path (Join-Path (Join-Path (Join-Path $RuntimeRoot 'bin') $version) $target) $name)
}

function Get-RemoteFile($url, $destination) {
  if ($url.StartsWith('file://')) {
    $source = $url.Substring(7)
    if (-not (Test-Path $source -PathType Leaf)) { return $false }
    Copy-Item -Path $source -Destination $destination -Force
    return $true
  }
  try {
    # TLS 1.2 is not the 5.1 default on older hosts and GitHub requires it.
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $destination -UseBasicParsing -TimeoutSec 120
    return $true
  } catch {
    Write-Error "loam: download failed: $url"
    return $false
  }
}

function Invoke-Bootstrap($version, $target) {
  $binary = Get-RuntimeBinary $version $target
  if (Test-Path $binary -PathType Leaf) { return 0 }

  $versionDir = Join-Path (Join-Path $RuntimeRoot 'bin') $version
  New-Item -ItemType Directory -Path $versionDir -Force | Out-Null
  $marker = Join-Path $versionDir ($target + '.installing')
  if (Test-Path $marker) {
    $age = ((Get-Date) - (Get-Item $marker).LastWriteTime).TotalSeconds
    if ($age -lt $MarkerStaleSeconds) { return 0 }
    Remove-Item $marker -Force -ErrorAction SilentlyContinue
  }
  try {
    # CreateNew fails if another installer won the race: the atomic primitive.
    $stream = [System.IO.File]::Open($marker, [System.IO.FileMode]::CreateNew)
    $stream.Close()
  } catch { return 0 }

  $base = $env:LOAM_RELEASE_BASE_URL
  if (-not $base) { $base = "$RepoReleaseBase/cli-v$version" }
  $staging = Join-Path ([System.IO.Path]::GetTempPath()) ('loam-install-' + [System.Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $staging -Force | Out-Null
  $status = 1

  try {
    $manifestPath = Join-Path $staging 'manifest.json'
    if (Get-RemoteFile "$base/loam-runtime-manifest.json" $manifestPath) {
      $manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json
      $entry = $manifest.runtimes | Where-Object { $_.target -eq $target } | Select-Object -First 1
      if (-not $entry) {
        Write-Error "loam: manifest has no runtime for target $target"
      } else {
        $artifact = Join-Path $staging $entry.file
        if (Get-RemoteFile "$base/$($entry.file)" $artifact) {
          $actual = (Get-FileHash -Path $artifact -Algorithm SHA256).Hash.ToLower()
          if ($actual -ne $entry.sha256.ToLower()) {
            Write-Error "loam: checksum mismatch for $($entry.file) (expected $($entry.sha256), got $actual)"
          } else {
            New-Item -ItemType Directory -Path (Split-Path $binary -Parent) -Force | Out-Null
            if (Publish-Runtime $artifact $binary) { $status = 0 }
            else { Write-Error "loam: could not publish runtime to $binary" }
          }
        }
      }
    }
  } finally {
    Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $marker -Force -ErrorAction SilentlyContinue
  }
  return $status
}

function Publish-Runtime($source, $destination) {
  # Windows holds a sharing lock on a running executable: retry with bounded
  # backoff and fail closed. The existing runtime is never removed first.
  for ($attempt = 1; $attempt -le 5; $attempt++) {
    try {
      Move-Item -Path $source -Destination $destination -Force -ErrorAction Stop
      return $true
    } catch {
      if (Test-Path $destination -PathType Leaf) { return $true }
      Start-Sleep -Seconds $attempt
    }
  }
  return $false
}

function Start-BackgroundBootstrap {
  if ($env:LOAM_NO_BOOTSTRAP) { return }
  try {
    New-Item -ItemType Directory -Path $RuntimeRoot -Force | Out-Null
    if ((Test-Path $InstallLog) -and ((Get-Item $InstallLog).Length -gt 1MB)) {
      Remove-Item $InstallLog -Force -ErrorAction SilentlyContinue
    }
    $self = Join-Path $ScriptDir 'loam.ps1'
    Start-Process -FilePath 'powershell.exe' `
      -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $self, '--loam-bootstrap') `
      -WindowStyle Hidden -RedirectStandardOutput $InstallLog -RedirectStandardError "$InstallLog.err" | Out-Null
  } catch { }
}

# --- main ---------------------------------------------------------------------
Initialize-AgentsGitignore

$version = Get-CliVersion
if (-not $version) {
  Write-Error "loam: CLI_VERSION is missing, empty, or not valid SemVer at $ScriptDir\CLI_VERSION"
  exit 78
}
$target = Get-Target
if ($SupportedTargets -notcontains $target) {
  Write-Error "loam: unsupported platform target: $target"
  exit 78
}
$binary = Get-RuntimeBinary $version $target

if ($Arguments.Count -gt 0 -and $Arguments[0] -eq '--loam-runtime-path') {
  Write-Output $binary
  exit 0
}
if ($Arguments.Count -gt 0 -and $Arguments[0] -eq '--loam-bootstrap') {
  exit (Invoke-Bootstrap $version $target)
}

if (Test-Path $binary -PathType Leaf) {
  & $binary @Arguments
  exit $LASTEXITCODE
}

Start-BackgroundBootstrap
Write-Error "loam: native runtime $version ($target) is temporarily unavailable; retry shortly"
exit 75
