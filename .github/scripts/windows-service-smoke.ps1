# Slice C T8 dormant-lifecycle service smoke (Windows/Task Scheduler).
# Proves our CLI drives real schtasks: install creates a disabled current-user
# logon task, status/query finds it, uninstall removes it. No start, no broker.
$ErrorActionPreference = "Stop"
$Bin = $args[0]
if (-not $Bin) { throw "path to loam.exe required" }
$Root = Join-Path $env:TEMP ("loam-svc-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Root | Out-Null
$Task = $null
try {
  & $Bin federation service install --global-root $Root
  if ($LASTEXITCODE -ne 0) { throw "install exited $LASTEXITCODE" }
  $id = (Get-Content (Join-Path $Root "instance_id")).Trim()
  $Task = "Loam\connector-$id"
  schtasks /Query /TN $Task | Out-Null
  if ($LASTEXITCODE -ne 0) { throw "task not created" }
  if (Test-Path (Join-Path $Root "loam.sqlite3")) { throw "database created by install" }
  # Confirm it is disabled (not Ready/Running).
  $info = schtasks /Query /TN $Task /FO LIST /V | Out-String
  if ($info -notmatch "Disabled") { throw "task is not disabled: $info" }
  # Read-only status (the verb packaged setup delegates to) must not create a DB.
  & $Bin federation service status --global-root $Root | Out-Null
  if (Test-Path (Join-Path $Root "loam.sqlite3")) { throw "database created by status" }
  # enable/disable are what setup uses to preserve active desired state across a
  # runtime update; exercise them against real schtasks. The empty registry keeps
  # the connector inert (the Windows endpoint itself lands in T7), so no daemon
  # and no database result.
  & $Bin federation service enable --global-root $Root
  if ($LASTEXITCODE -ne 0) { throw "enable exited $LASTEXITCODE" }
  $enabled = schtasks /Query /TN $Task /FO LIST /V | Out-String
  if ($enabled -match "Scheduled Task State:\s+Disabled") { throw "task still disabled after enable: $enabled" }
  if (Test-Path (Join-Path $Root "loam.sqlite3")) { throw "database created by enable (inert violated)" }
  & $Bin federation service disable --global-root $Root
  if ($LASTEXITCODE -ne 0) { throw "disable exited $LASTEXITCODE" }
  & $Bin federation service uninstall --global-root $Root
  if ($LASTEXITCODE -ne 0) { throw "uninstall exited $LASTEXITCODE" }
  schtasks /Query /TN $Task 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { throw "task not removed" }
  Write-Host "windows service smoke OK"
} finally {
  if ($Task) { schtasks /Delete /TN $Task /F 2>$null | Out-Null }
  Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue
}
