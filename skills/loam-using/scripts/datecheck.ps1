# datecheck.ps1 — compatibility forwarder to `loam datecheck`.
#
# Invoke exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File datecheck.ps1 <check|fix> <wiki-root> [--offset +02:00]
#
# Exit codes are the native runtime's. The full PowerShell implementation this
# replaced lives in datecheck-legacy.ps1 as parity evidence.
param(
  [Parameter(ValueFromRemainingArguments = $true)]
  [string[]]$Arguments = @()
)
$launcher = Join-Path $PSScriptRoot 'loam.ps1'
& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $launcher datecheck @Arguments
exit $LASTEXITCODE
