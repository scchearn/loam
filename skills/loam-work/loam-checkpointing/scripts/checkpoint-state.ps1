# checkpoint-state.ps1 — compatibility forwarder to `loam checkpoint state`.
#
# Invoke exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File checkpoint-state.ps1 [--window <minutes>] [<workspace-root>]
#
# There was never a PowerShell implementation of the capture-side digest: the
# Bash original needed jq, GNU date, and find -mmin. Exit codes are the native
# runtime's.
param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Arguments = @()
)
$candidates = @(
  (Join-Path $PSScriptRoot '..\..\..\loam-using\scripts\loam.ps1'),
  (Join-Path $PSScriptRoot '..\..\loam-using\scripts\loam.ps1')
)
foreach ($launcher in $candidates) {
  if (Test-Path $launcher -PathType Leaf) {
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher checkpoint state @Arguments
    exit $LASTEXITCODE
  }
}
Write-Error "Error: loam launcher not found near: $PSScriptRoot"
exit 1
