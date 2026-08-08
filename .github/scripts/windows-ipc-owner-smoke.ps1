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
$ErrLog = Join-Path $Root "server.err.log"
# The alternate user cannot reach our profile's TEMP — the path is not even
# traversable for it — so everything the second logon session has to read or
# write lives under the all-users public root instead, with an explicit grant
# added once the account exists.
$Shared = Join-Path $env:PUBLIC ("loam-ipc-" + [guid]::NewGuid().ToString("N"))
$ChildScript = Join-Path $Shared "deny.ps1"
$ChildOut = Join-Path $Shared "deny.out"
$Server = $null
$Created = $false

function Fail([string]$message) { throw "windows ipc owner smoke: $message" }

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
  $env:LOAM_IPC_SMOKE_SECONDS = "60"
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
  $sddl = Select-String -Path $Log -Pattern "LOAM_ENDPOINT_SDDL=(.+)$" | Select-Object -First 1
  if ($sddl) { Write-Host ("endpoint dacl: " + $sddl.Matches[0].Groups[1].Value.Trim()) }
  if (-not $pipeName.StartsWith("\\.\pipe\")) { Fail "endpoint name is not local: $pipeName" }
  $short = $pipeName.Substring("\\.\pipe\".Length)

  # 2. Positive control: the owning session round-trips one frame. Without this
  #    the denial below would prove nothing (a broken pipe also "denies").
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
    # Control for the probe itself: if this session's own groups show no
    # S-1-5-5-* either, then the API hides logon SIDs and the child's missing
    # one proves nothing about sessions.
    $me = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    Write-Host ("control groups: " + (($me.Groups | ForEach-Object { $_.Value }) -join ","))
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
  # Diagnostic only: the descriptor the endpoint actually carries, so the
  # denial verdict below can be read against it instead of assumed.
  try {
    Write-Host ("endpoint sddl: " + (Get-Acl -Path "\\.\pipe\$short" -ErrorAction Stop).Sddl)
  } catch {
    Write-Host "endpoint sddl unavailable: $($_.Exception.Message)"
  }

  # 3. Alternate user, alternate logon session: opening the pipe must be denied.
  New-LocalUser -Name $User -Password (ConvertTo-SecureString $Password -AsPlainText -Force) `
    -AccountNeverExpires -PasswordNeverExpires -UserMayNotChangePassword | Out-Null
  $Created = $true
  # The account only ever touches this one directory; the endpoint stays
  # protected by its own DACL, which is the thing under test.
  & icacls $Shared /grant "${User}:(OI)(CI)(F)" | Out-Null
  if ($LASTEXITCODE -ne 0) { Fail "granting $User access to the shared directory exited $LASTEXITCODE" }

  # The child writes its own verdict, including the identity it actually ran
  # under. A cross-session exit code is not evidence — `Start-Process
  # -Credential` reports one the parent cannot always read back — and this gate
  # decides a security question, so it decides it on what the child observed,
  # not on a number that defaults to zero. Only an access denial passes; every
  # other exception is an inconclusive run, not a denial.
  @"
`$ErrorActionPreference = 'Stop'
`$id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
# The endpoint's ACE names a logon SESSION, not a user. Report the session this
# child actually landed in, so 'a second user opened it' can be told apart from
# 'a second user in the same logon session opened it' — only the first is a
# failure of the descriptor.
`$groups = ((`$id.Groups | ForEach-Object { `$_.Value }) -join ',')
`$who = `$id.Name + ' (' + `$id.User.Value + ', groups ' + `$groups + ')'
try {
  `$stream = New-Object System.IO.Pipes.NamedPipeClientStream('.', '$short', [System.IO.Pipes.PipeDirection]::InOut)
  `$stream.Connect(5000)
  # Read the descriptor from the handle that should not exist. If this open is
  # wrong, the object's own DACL is the evidence for why it succeeded.
  `$sddl = 'unavailable'
  try { `$sddl = `$stream.GetAccessControl().GetSecurityDescriptorSddlForm('Access') } catch { `$sddl = 'unavailable: ' + `$_.Exception.Message }
  `$stream.Dispose()
  Set-Content -Path '$ChildOut' -Value "opened as `$who :: dacl `$sddl"
  exit 0
} catch {
  `$denial = (`$_.Exception -is [System.UnauthorizedAccessException]) -or (`$_.Exception.Message -match 'Access is denied')
  `$kind = if (`$denial) { 'denied' } else { 'error' }
  Set-Content -Path '$ChildOut' -Value "`$kind as `$who :: `$(`$_.Exception.GetType().FullName) :: `$(`$_.Exception.Message)"
  if (`$denial) { exit 3 } else { exit 4 }
}
"@ | Set-Content -Path $ChildScript -Encoding ASCII

  $credential = New-Object System.Management.Automation.PSCredential(
    $User, (ConvertTo-SecureString $Password -AsPlainText -Force))
  $denied = Start-Process -FilePath "powershell.exe" -Credential $credential `
    -ArgumentList "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", $ChildScript `
    -WorkingDirectory $Shared -PassThru -Wait
  $verdict = if (Test-Path $ChildOut) { (Get-Content $ChildOut -Raw).Trim() } else { "" }
  if (-not $verdict) { Fail "the alternate-user child left no verdict (exit $($denied.ExitCode))" }
  if ($verdict.StartsWith("denied")) {
    Write-Host "alternate-user denial OK (access denied at pipe open): $verdict"
  } elseif ($verdict.StartsWith("opened")) {
    Fail "another user opened the connector endpoint: $verdict"
  } else {
    Fail "alternate-user attempt was inconclusive (exit $($denied.ExitCode)): $verdict"
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
  if ($Created) { Remove-LocalUser -Name $User -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $Root -ErrorAction SilentlyContinue
  Remove-Item -Recurse -Force $Shared -ErrorAction SilentlyContinue
}
