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
  & $Bin federation service uninstall --global-root $Root
  if ($LASTEXITCODE -ne 0) { throw "uninstall exited $LASTEXITCODE" }
  schtasks /Query /TN $Task 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { throw "task not removed" }
  Write-Host "windows service smoke OK"
} finally {
  if ($Task) { schtasks /Delete /TN $Task /F 2>$null | Out-Null }
  Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue
}
