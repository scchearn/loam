# loamstate.ps1 — PowerShell compatibility entry point for workspace state.
#
# Invoke exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File loamstate.ps1 [--fast] <workspace-root>
#
# Delegates to the native runtime through loam.ps1, so native Windows receives
# the same full state/hint contract as POSIX hosts. When the runtime is absent,
# installing, unsupported, or unavailable it emits minimal valid state and
# exits 0 rather than blocking session startup.
#
# Exit codes: 0 always for probe/environment failures; 1 for bad arguments.

param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Arguments = @()
)

$ErrorActionPreference = 'Stop'

$fast = $false
$workspace = ''
foreach ($argument in $Arguments) {
  if ($argument -eq '--fast') { $fast = $true }
  elseif ($argument.StartsWith('-')) {
    Write-Error 'Usage: loamstate.ps1 [--fast] <workspace-root>'
    exit 1
  } else { $workspace = $argument }
}

if (-not $workspace) {
  Write-Error 'Usage: loamstate.ps1 [--fast] <workspace-root>'
  exit 1
}

function Get-MinimalState($reason, $version, $target) {
  # Every field consumers read, degraded to neutral values, plus the canonical
  # runtime_unavailable maintenance hint.
  $hint = '{"kind":"runtime_unavailable","group":"maintenance","severity":"info",' +
    '"message":"Native loam runtime is unavailable; state is minimal until it installs.",' +
    '"command":null,"evidence":{"reason":"' + $reason + '","target":"' + $target + '","version":"' + $version + '"}}'
  return '{"wiki_root":"","exists":false,"qmd_ready":false,"latest_checkpoint":null,' +
    '"recent_checkpoints":[],"checkpoint_count":0,"git_status":null,"drift_count":null,' +
    '"hints":[' + $hint + ']}'
}

$launcher = Join-Path $PSScriptRoot 'loam.ps1'
$stateArgs = @('state')
if ($fast) { $stateArgs += '--fast' }
$stateArgs += $workspace

$output = & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher @stateArgs 2>$null
$status = $LASTEXITCODE

if ($status -eq 0 -and $output) {
  Write-Output ($output -join "`n")
  exit 0
}

$version = ''
$versionFile = Join-Path $PSScriptRoot 'CLI_VERSION'
if (Test-Path $versionFile -PathType Leaf) { $version = (Get-Content $versionFile -Raw).Trim() }
$target = $env:LOAM_TARGET
if (-not $target) { $target = 'x86_64-pc-windows-msvc' }

$reason = 'unavailable'
if ($status -eq 78) { $reason = 'configuration' }
elseif ($status -eq 75) { $reason = 'installing' }

Write-Output (Get-MinimalState $reason $version $target)
exit 0
