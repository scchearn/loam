# Slice C T7 Windows named-pipe owner smoke.
#
# Proves on a real Windows host what no in-process test can: a second local
# user, in its own logon session, cannot open the connector's endpoint, while
# the owning session round-trips a frame through it. The denial comes from the
# pipe's protected DACL, so it happens at open time — before any frame byte.
#
# Requires administrator rights to create and remove the temporary local user
# (the hosted windows runner has them). A missing prerequisite is a BLOCKER, not
# a pass: the smoke never reports success it did not observe.
$ErrorActionPreference = "Stop"

$User = "loamsmoke"
$Password = "L0am-Smoke-" + [guid]::NewGuid().ToString("N").Substring(0, 12) + "!"
$Root = Join-Path $env:TEMP ("loam-ipc-" + [guid]::NewGuid().ToString("N"))
$Log = Join-Path $Root "server.log"
$ChildScript = Join-Path $Root "deny.ps1"
$ChildOut = Join-Path $Root "deny.out"
$Server = $null
$Created = $false

function Fail([string]$message) { throw "windows ipc owner smoke: $message" }

New-Item -ItemType Directory -Path $Root | Out-Null
try {
  # 1. Build and launch the endpoint fixture, then learn its pipe name.
  & cargo +1.94.1 test --locked --test ipc_owner --no-run 2>&1 | Out-Null
  if ($LASTEXITCODE -ne 0) { Fail "building the ipc_owner test binary exited $LASTEXITCODE" }
  $exe = Get-ChildItem -Path "target\debug\deps" -Filter "ipc_owner-*.exe" |
    Where-Object { $_.Name -notlike "*.d" } |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
  if (-not $exe) { Fail "no ipc_owner test binary was produced" }

  $env:LOAM_IPC_SMOKE_ROOT = $Root
  $env:LOAM_IPC_SMOKE_SECONDS = "60"
  $Server = Start-Process -FilePath $exe.FullName `
    -ArgumentList "--ignored", "--nocapture", "--exact", "windows_owner::windows_endpoint_serves_the_alternate_user_smoke" `
    -RedirectStandardOutput $Log -PassThru -WindowStyle Hidden

  $pipeName = $null
  $deadline = (Get-Date).AddSeconds(60)
  while ((Get-Date) -lt $deadline -and -not $pipeName) {
    Start-Sleep -Milliseconds 200
    if (Test-Path $Log) {
      $match = Select-String -Path $Log -Pattern "LOAM_PIPE_NAME=(.+)$" | Select-Object -First 1
      if ($match) { $pipeName = $match.Matches[0].Groups[1].Value.Trim() }
    }
  }
  if (-not $pipeName) { Fail "the endpoint fixture never reported its pipe name" }
  if (-not $pipeName.StartsWith("\\.\pipe\")) { Fail "endpoint name is not local: $pipeName" }
  $short = $pipeName.Substring("\\.\pipe\".Length)

  # 2. Positive control: the owning session round-trips one frame. Without this
  #    the denial below would prove nothing (a broken pipe also "denies").
  $client = New-Object System.IO.Pipes.NamedPipeClientStream(".", $short, [System.IO.Pipes.PipeDirection]::InOut)
  try {
    $client.Connect(10000)
    $body = [Text.Encoding]::ASCII.GetBytes("ping")
    $frame = New-Object byte[] (4 + $body.Length)
    [Array]::Copy([BitConverter]::GetBytes([int]$body.Length), 0, $frame, 0, 4)
    if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($frame, 0, 4) }
    [Array]::Copy($body, 0, $frame, 4, $body.Length)
    $client.Write($frame, 0, $frame.Length)
    $client.Flush()
    $header = New-Object byte[] 4
    if ($client.Read($header, 0, 4) -ne 4) { Fail "same-user control read no response header" }
    if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($header) }
    $length = [BitConverter]::ToInt32($header, 0)
    if ($length -le 0 -or $length -gt 1024) { Fail "same-user control read an implausible length $length" }
    $payload = New-Object byte[] $length
    if ($client.Read($payload, 0, $length) -ne $length) { Fail "same-user control read a short body" }
    $answer = [Text.Encoding]::ASCII.GetString($payload)
    if ($answer -ne "pong") { Fail "same-user control got '$answer', expected 'pong'" }
  } finally {
    $client.Dispose()
  }
  Write-Host "same-user positive control OK"

  # 3. Alternate user, alternate logon session: opening the pipe must be denied.
  New-LocalUser -Name $User -Password (ConvertTo-SecureString $Password -AsPlainText -Force) `
    -AccountNeverExpires -PasswordNeverExpires -UserMayNotChangePassword | Out-Null
  $Created = $true

  @"
`$ErrorActionPreference = 'Stop'
try {
  `$stream = New-Object System.IO.Pipes.NamedPipeClientStream('.', '$short', [System.IO.Pipes.PipeDirection]::InOut)
  `$stream.Connect(5000)
  `$stream.Dispose()
  exit 0
} catch [System.UnauthorizedAccessException] {
  exit 3
} catch {
  if (`$_.Exception.Message -match 'Access is denied') { exit 3 }
  Set-Content -Path '$ChildOut' -Value `$_.Exception.GetType().FullName
  Add-Content -Path '$ChildOut' -Value `$_.Exception.Message
  exit 4
}
"@ | Set-Content -Path $ChildScript -Encoding ASCII

  $credential = New-Object System.Management.Automation.PSCredential(
    $User, (ConvertTo-SecureString $Password -AsPlainText -Force))
  $denied = Start-Process -FilePath "powershell.exe" -Credential $credential `
    -ArgumentList "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $ChildScript `
    -WorkingDirectory $env:TEMP -PassThru -Wait
  switch ($denied.ExitCode) {
    3 { Write-Host "alternate-user denial OK (access denied at pipe open)" }
    0 { Fail "another user opened the connector endpoint" }
    default {
      $detail = if (Test-Path $ChildOut) { Get-Content $ChildOut -Raw } else { "no detail" }
      Fail "alternate-user attempt was inconclusive (exit $($denied.ExitCode)): $detail"
    }
  }

  Write-Host "windows ipc owner smoke OK"
} catch {
  # The endpoint fixture's own output is the only view of the server side, so a
  # red run is diagnosable instead of just "pipe is broken".
  if (Test-Path $Log) {
    Write-Host "--- endpoint fixture output ---"
    Get-Content $Log | Write-Host
  }
  throw
} finally {
  if ($Server -and -not $Server.HasExited) { Stop-Process -Id $Server.Id -Force -ErrorAction SilentlyContinue }
  if ($Created) { Remove-LocalUser -Name $User -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue
}
