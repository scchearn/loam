# Slice C T7 Windows named-pipe owner smoke.
#
# Proves on a real Windows host what no in-process test can, against the two
# barriers the endpoint actually has:
#
#   1. The pipe's protected DACL grants one logon SESSION. A peer in a different
#      logon session is refused at open time, before a handle exists.
#   2. The client-token SID proof grants one USER. A peer that shares the logon
#      session opens the handle and is then refused before the codec reads a
#      byte, so it is never served.
#
# Both are exercised with the same second local account, because the difference
# between them is the logon session, not the user: `Start-Process -Credential`
# copies the caller's logon SID into the new token (that is how a runas'd
# process reaches the caller's desktop), while a Task Scheduler batch logon
# carries its own. Asserting "another user is denied at open" would be asserting
# something the descriptor does not say.
#
# Requires administrator rights to create and remove the temporary local user
# (the hosted windows runner has them). A missing prerequisite is a BLOCKER, not
# a pass: the smoke never reports success it did not observe.
$ErrorActionPreference = "Stop"

$User = "loamsmoke"
$Password = "L0am-Smoke-" + [guid]::NewGuid().ToString("N").Substring(0, 12) + "!"
$Root = Join-Path $env:TEMP ("loam-ipc-" + [guid]::NewGuid().ToString("N"))
$Log = Join-Path $Root "server.log"
$ErrLog = Join-Path $Root "server.err.log"
# The alternate user cannot reach our profile's TEMP — the path is not even
# traversable for it — so everything the second logon session has to read or
# write lives under the all-users public root instead, with an explicit grant
# added once the account exists.
# Short names on purpose: schtasks caps /tr at 261 characters, and these paths
# are spelled out inside it.
$Shared = Join-Path $env:PUBLIC ("loam-ipc-" + [guid]::NewGuid().ToString("N").Substring(0, 12))
$ChildScript = Join-Path $Shared "client.ps1"
$TaskScript = Join-Path $Shared "task.ps1"
$SessionOut = Join-Path $Shared "session.out"
$BatchOut = Join-Path $Shared "batch.out"
$Task = "loam-ipc-owner-smoke"
$Server = $null
$Created = $false
$Scheduled = $false

function Fail([string]$message) { throw "windows ipc owner smoke: $message" }

function Read-Verdict([string]$path, [int]$seconds) {
  $deadline = (Get-Date).AddSeconds($seconds)
  while ((Get-Date) -lt $deadline) {
    if (Test-Path $path) {
      $text = (Get-Content $path -Raw).Trim()
      if ($text) { return $text }
    }
    Start-Sleep -Milliseconds 250
  }
  return ""
}

New-Item -ItemType Directory -Path $Root | Out-Null
New-Item -ItemType Directory -Path $Shared | Out-Null
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
  $env:LOAM_IPC_SMOKE_SECONDS = "300"
  $Server = Start-Process -FilePath $exe.FullName `
    -ArgumentList "--ignored", "--nocapture", "--exact", "windows_owner::windows_endpoint_serves_the_alternate_user_smoke" `
    -RedirectStandardOutput $Log -RedirectStandardError $ErrLog -PassThru -WindowStyle Hidden

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
  $sddl = Select-String -Path $Log -Pattern "LOAM_ENDPOINT_SDDL=(.+)$" | Select-Object -First 1
  if ($sddl) { Write-Host ("endpoint dacl: " + $sddl.Matches[0].Groups[1].Value.Trim()) }

  # 2. Positive control: the owning session round-trips one frame. Without this
  #    every denial below would prove nothing (a broken pipe also "denies").
  #    The control must open the pipe the way the real client does. .NET's
  #    three-argument constructor defaults to TokenImpersonationLevel.None,
  #    which omits SECURITY_SQOS_PRESENT from CreateFile; the server then
  #    impersonates an anonymous token and the SID proof correctly rejects it.
  #    `connect` in cli/src/ipc/windows.rs asks for SECURITY_IMPERSONATION, so
  #    the control asks for the same level.
  $client = [System.IO.Pipes.NamedPipeClientStream]::new(
    ".",
    $short,
    [System.IO.Pipes.PipeDirection]::InOut,
    [System.IO.Pipes.PipeOptions]::None,
    [System.Security.Principal.TokenImpersonationLevel]::Impersonation)
  try {
    $client.Connect(10000)
    # Read the descriptor while the connection is live: the server disconnects
    # the instance as soon as it has answered, and a second open cannot read it
    # because the single instance is busy. This is the only window there is.
    try {
      $live = [System.IO.Pipes.PipesAclExtensions]::GetAccessControl($client)
      Write-Host ("live endpoint dacl: " + $live.GetSecurityDescriptorSddlForm("Access"))
    } catch {
      Write-Host "live endpoint dacl unavailable: $($_.Exception.Message)"
    }
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
  Write-Host ("same-user positive control OK (logon " + ((whoami /logonid) | Select-Object -Last 1).Trim() + ")")

  # The one client both cases run. It reports what it observed — refused at
  # open, opened but never served, or opened and served — and the identity and
  # logon session it observed it from. `whoami /logonid` is used deliberately:
  # WindowsIdentity.Groups does not surface the logon SID, so the .NET view
  # cannot tell these two cases apart.
  @'
param([string]$PipeName, [string]$Out)
$ErrorActionPreference = 'Stop'
$id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$who = $id.Name + ' (' + $id.User.Value + ') logon ' + ((whoami /logonid) | Select-Object -Last 1).Trim()
function Say([string]$outcome, [string]$detail) {
  Set-Content -Path $Out -Value "$outcome as $who :: $detail"
}
try {
  $stream = New-Object System.IO.Pipes.NamedPipeClientStream('.', $PipeName, [System.IO.Pipes.PipeDirection]::InOut)
  $stream.Connect(5000)
} catch {
  if (($_.Exception -is [System.UnauthorizedAccessException]) -or ($_.Exception.Message -match 'Access is denied')) {
    Say 'denied-at-open' $_.Exception.GetType().FullName
    exit 3
  }
  Say 'error' ($_.Exception.GetType().FullName + ' :: ' + $_.Exception.Message)
  exit 4
}
# The handle exists, so the DACL admitted this logon session. Everything from
# here tests the second barrier: whether the connector will SERVE this user.
$sddl = 'unavailable'
try { $sddl = $stream.GetAccessControl().GetSecurityDescriptorSddlForm('Access') } catch { }
try {
  $body = [Text.Encoding]::ASCII.GetBytes('ping')
  $frame = New-Object byte[] (4 + $body.Length)
  [Array]::Copy([BitConverter]::GetBytes([int]$body.Length), 0, $frame, 0, 4)
  if ([BitConverter]::IsLittleEndian) { [Array]::Reverse($frame, 0, 4) }
  [Array]::Copy($body, 0, $frame, 4, $body.Length)
  $stream.Write($frame, 0, $frame.Length)
  $stream.Flush()
  $buffer = New-Object byte[] 8
  $read = $stream.ReadAsync($buffer, 0, 8)
  if (-not $read.Wait(15000)) { Say 'unserved' "no response before the deadline; dacl $sddl"; exit 0 }
  if ($read.Result -eq 0) { Say 'unserved' "server closed without answering; dacl $sddl"; exit 0 }
  Say 'served' "server answered $($read.Result) bytes; dacl $sddl"
  exit 5
} catch {
  Say 'unserved' ('connection failed after open: ' + $_.Exception.GetType().FullName + '; dacl ' + $sddl)
  exit 0
} finally {
  $stream.Dispose()
}
'@ | Set-Content -Path $ChildScript -Encoding ASCII

  New-LocalUser -Name $User -Password (ConvertTo-SecureString $Password -AsPlainText -Force) `
    -AccountNeverExpires -PasswordNeverExpires -UserMayNotChangePassword | Out-Null
  $Created = $true
  # The account only ever touches this one directory; the endpoint stays
  # protected by its own DACL, which is the thing under test.
  & icacls $Shared /grant "${User}:(OI)(CI)(F)" | Out-Null
  if ($LASTEXITCODE -ne 0) { Fail "granting $User access to the shared directory exited $LASTEXITCODE" }

  # 3. Second USER, same logon session. The DACL admits it; the SID proof must
  #    refuse to serve it, before the codec reads a byte.
  $credential = New-Object System.Management.Automation.PSCredential(
    $User, (ConvertTo-SecureString $Password -AsPlainText -Force))
  Start-Process -FilePath "powershell.exe" -Credential $credential `
    -ArgumentList "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $ChildScript, "-PipeName", $short, "-Out", $SessionOut `
    -WorkingDirectory $Shared -PassThru -Wait | Out-Null
  $verdict = Read-Verdict $SessionOut 30
  if (-not $verdict) { Fail "the same-session client left no verdict" }
  Write-Host "same-session second user: $verdict"
  if ($verdict.StartsWith("served")) { Fail "the connector served a second user: $verdict" }
  if (-not ($verdict.StartsWith("unserved") -or $verdict.StartsWith("denied-at-open"))) {
    Fail "the same-session attempt was inconclusive: $verdict"
  }

  # 4. Second logon session. A Task Scheduler batch logon carries its own logon
  #    SID, so the DACL must refuse it at open — no handle, no frame, nothing to
  #    reject later.
  # The arguments are baked into a wrapper rather than spelled out in /tr, which
  # schtasks caps at 261 characters.
  "& '$ChildScript' -PipeName '$short' -Out '$BatchOut'" | Set-Content -Path $TaskScript -Encoding ASCII
  $command = "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $TaskScript"
  & schtasks /create /tn $Task /tr $command /sc once /st 23:59 /ru $User /rp $Password /f | Out-Null
  if ($LASTEXITCODE -ne 0) { Fail "registering the batch-logon task exited $LASTEXITCODE" }
  $Scheduled = $true
  & schtasks /run /tn $Task | Out-Null
  if ($LASTEXITCODE -ne 0) { Fail "starting the batch-logon task exited $LASTEXITCODE" }
  $verdict = Read-Verdict $BatchOut 90
  if (-not $verdict) { Fail "the batch-logon client left no verdict (it may never have run)" }
  Write-Host "second logon session: $verdict"
  if (-not $verdict.StartsWith("denied-at-open")) {
    Fail "a different logon session was not refused at open: $verdict"
  }

  Write-Host "windows ipc owner smoke OK"
} catch {
  # The endpoint fixture's own output is the only view of the server side, so a
  # red run is diagnosable instead of just "pipe is broken".
  if (Test-Path $Log) {
    Write-Host "--- endpoint fixture output ---"
    Get-Content $Log | Write-Host
  }
  # Peer rejections are reported on stderr (`reject_peer` names the stage and
  # the win32 code), so a red run says why the proof failed.
  if (Test-Path $ErrLog) {
    Write-Host "--- endpoint fixture stderr ---"
    Get-Content $ErrLog | Write-Host
  }
  throw
} finally {
  if ($Server -and -not $Server.HasExited) { Stop-Process -Id $Server.Id -Force -ErrorAction SilentlyContinue }
  if ($Scheduled) { & schtasks /delete /tn $Task /f 2>&1 | Out-Null }
  if ($Created) { Remove-LocalUser -Name $User -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue
  Remove-Item -Recurse -Force $Shared -ErrorAction SilentlyContinue
}
