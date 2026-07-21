# codegraph.ps1 — compatibility forwarder to `loam codegraph`.
#
# Invoke exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File codegraph.ps1 <index|walk|diff> ...
#
# Exit codes are the native runtime's. The full PowerShell implementation this
# replaced lives in codegraph-legacy.ps1 as parity evidence.
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
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher codegraph @Arguments
    exit $LASTEXITCODE
  }
}
Write-Error "Error: loam launcher not found near: $PSScriptRoot"
exit 1
