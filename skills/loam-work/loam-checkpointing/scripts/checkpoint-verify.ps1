# checkpoint-verify.ps1 — compatibility forwarder to `loam checkpoint verify`.
#
# Invoke exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File checkpoint-verify.ps1 <note.md>
#
# There was never a PowerShell implementation of checkpoint verification; the
# native runtime is what makes this reachable on Windows at all. Exit codes are
# the native runtime's, and verification itself always exits 0.
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
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher checkpoint verify @Arguments
    exit $LASTEXITCODE
  }
}
Write-Error "Error: loam launcher not found near: $PSScriptRoot"
exit 1
