# Contract test for loam.ps1 — the Windows twin of loam-launcher-contract-test.
#
# Run exactly as:
#   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File loam-launcher-contract-test.ps1
#
# Covers scope resolution, CLI_VERSION validation, unsupported targets, the
# ready-runtime path, bootstrap integrity, and the concurrency marker.

$ErrorActionPreference = 'Stop'

$launcherSource = Join-Path $PSScriptRoot 'loam.ps1'
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('loam-ps-contract-' + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
$checks = 0

function Fail($message) {
  Write-Host "loam.ps1 contract: FAIL: $message"
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
  exit 1
}

function Add-Check { $script:checks++ }

# Copy the launcher plus a CLI_VERSION into an isolated .agents tree.
function Install-Tree($base, $version) {
  $dir = Join-Path $base '.agents\skills\loam-using\scripts'
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  Copy-Item $launcherSource (Join-Path $dir 'loam.ps1') -Force
  Set-Content -Path (Join-Path $dir 'CLI_VERSION') -Value $version -Encoding ASCII
  return (Join-Path $dir 'loam.ps1')
}

function Invoke-Launcher($launcher, $launcherArgs) {
  # Windows PowerShell 5.1 wraps native-command stderr in ErrorRecord objects.
  # Under the script's $ErrorActionPreference = 'Stop', merging with 2>&1 turns
  # the first stderr line into a terminating error — so a launcher correctly
  # exiting nonzero *with* a diagnostic (invalid CLI_VERSION, unsupported
  # target, missing release) aborted this script instead of being asserted on.
  #
  # Capture stderr through a file rather than the merged stream, and drop the
  # preference to 'Continue' for the call. Assigning the preference inside the
  # function creates a function-scoped copy that is discarded on return, so the
  # caller's 'Stop' is restored automatically.
  $ErrorActionPreference = 'Continue'
  $all = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $launcher) + $launcherArgs
  $errorFile = [System.IO.Path]::GetTempFileName()
  $stdout = $null
  $stderr = ''
  $code = $null
  try {
    $stdout = & powershell.exe @all 2> $errorFile
    $code = $LASTEXITCODE
    if (Test-Path -LiteralPath $errorFile) {
      $stderr = Get-Content -LiteralPath $errorFile -Raw
    }
  } finally {
    Remove-Item -LiteralPath $errorFile -Force -ErrorAction SilentlyContinue
  }
  # A launcher that never ran leaves $LASTEXITCODE untouched; -1 keeps the
  # exit-code assertions meaningful instead of comparing against $null.
  if ($null -eq $code) { $code = -1 }

  $lines = @()
  if ($null -ne $stdout) { $lines += @($stdout | ForEach-Object { [string]$_ }) }
  if (-not [string]::IsNullOrEmpty($stderr)) { $lines += @($stderr -split "`r?`n") }
  $combined = ($lines -join "`n")
  return @{ Output = $combined.TrimEnd("`r", "`n"); ExitCode = $code }
}

$env:LOAM_TARGET = 'x86_64-pc-windows-msvc'

# --- 1. Project scope: runtime path derives from the nearest .agents ancestor.
$project = Join-Path $tmp 'project'
$launcher = Install-Tree $project '1.2.3'
$result = Invoke-Launcher $launcher @('--loam-runtime-path')
$expected = Join-Path $project '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe'
if ($result.Output.Trim() -ne $expected) { Fail "1: project scope resolved to $($result.Output)" }
Add-Check

# --- 2. Bootstrap writes .agents\.gitignore so a clean worktree stays clean.
$gitignore = Join-Path $project '.agents\.gitignore'
if (-not (Test-Path $gitignore)) { Fail '2: .agents\.gitignore not created' }
if ((Get-Content $gitignore -Raw).Trim() -ne '*') { Fail '2: .agents\.gitignore does not ignore the tree' }
Add-Check

# --- 3. Global scope: no .agents ancestor -> %USERPROFILE%\.agents.
$global = Join-Path $tmp 'global\skills\loam-using\scripts'
New-Item -ItemType Directory -Path $global -Force | Out-Null
Copy-Item $launcherSource (Join-Path $global 'loam.ps1') -Force
Set-Content -Path (Join-Path $global 'CLI_VERSION') -Value '1.2.3' -Encoding ASCII
$result = Invoke-Launcher (Join-Path $global 'loam.ps1') @('--loam-runtime-path')
$expected = Join-Path $env:USERPROFILE '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe'
if ($result.Output.Trim() -ne $expected) { Fail "3: global scope resolved to $($result.Output)" }
Add-Check

# --- 4. Missing / empty / malformed CLI_VERSION -> exit 78, no network.
foreach ($bad in @('', 'not-semver', '1.2')) {
  $broken = Join-Path $tmp ('broken-' + [System.Guid]::NewGuid().ToString('N'))
  $launcher = Install-Tree $broken $bad
  $result = Invoke-Launcher $launcher @('state', '--fast', $broken)
  if ($result.ExitCode -ne 78) { Fail "4: CLI_VERSION '$bad' did not exit 78" }
}
$absent = Join-Path $tmp 'absent'
$launcher = Install-Tree $absent '1.2.3'
Remove-Item (Join-Path (Split-Path $launcher -Parent) 'CLI_VERSION') -Force
$result = Invoke-Launcher $launcher @('state', '--fast', $absent)
if ($result.ExitCode -ne 78) { Fail '4: absent CLI_VERSION did not exit 78' }
Add-Check

# --- 5. Unsupported target -> exit 78 with a clear diagnostic.
$unsupported = Join-Path $tmp 'unsupported'
$launcher = Install-Tree $unsupported '1.2.3'
$env:LOAM_TARGET = 'sparc64-unknown-haiku'
$result = Invoke-Launcher $launcher @('state', '--fast', $unsupported)
$env:LOAM_TARGET = 'x86_64-pc-windows-msvc'
if ($result.ExitCode -ne 78) { Fail '5: unsupported target did not exit 78' }
if ($result.Output -notmatch 'unsupported') { Fail "5: expected unsupported diagnostic, got $($result.Output)" }
Add-Check

# --- 6. Runtime absent -> exit 75 with a temporary-unavailable message.
$pending = Join-Path $tmp 'pending'
$launcher = Install-Tree $pending '1.2.3'
$env:LOAM_NO_BOOTSTRAP = '1'
$result = Invoke-Launcher $launcher @('state', '--fast', $pending)
Remove-Item Env:\LOAM_NO_BOOTSTRAP
if ($result.ExitCode -ne 75) { Fail "6: absent runtime did not exit 75 (got $($result.ExitCode))" }
if ($result.Output -notmatch 'temporarily unavailable') { Fail "6: expected retry message, got $($result.Output)" }
Add-Check

# --- 7. Ready runtime is executed by absolute path with the caller's arguments.
$ready = Join-Path $tmp 'ready'
$launcher = Install-Tree $ready '1.2.3'
$runtimeDir = Join-Path $ready '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc'
New-Item -ItemType Directory -Path $runtimeDir -Force | Out-Null
# A .cmd stub stands in for the native executable so the test needs no build.
$stub = Join-Path $runtimeDir 'loam.exe'
Set-Content -Path (Join-Path $runtimeDir 'stub.cmd') -Value '@echo stub:%*' -Encoding ASCII
Copy-Item (Join-Path $runtimeDir 'stub.cmd') $stub -Force
$result = Invoke-Launcher $launcher @('state', '--fast', 'C:\some\workspace')
if ($result.Output -notmatch 'stub:') { Fail "7: runtime not invoked, got $($result.Output)" }
Add-Check

# --- 8. Bootstrap verifies the manifest SHA-256 and publishes a runnable binary.
$release = Join-Path $tmp 'release'
New-Item -ItemType Directory -Path $release -Force | Out-Null
$artifact = Join-Path $release 'loam-x86_64-pc-windows-msvc.exe'
Set-Content -Path $artifact -Value '@echo installed' -Encoding ASCII
$digest = (Get-FileHash -Path $artifact -Algorithm SHA256).Hash.ToLower()
Set-Content -Path (Join-Path $release 'loam-runtime-manifest.json') -Encoding ASCII -Value @"
{"version":"1.2.3","runtimes":[{"target":"x86_64-pc-windows-msvc","file":"loam-x86_64-pc-windows-msvc.exe","sha256":"$digest"}]}
"@
$install = Join-Path $tmp 'install'
$launcher = Install-Tree $install '1.2.3'
$env:LOAM_RELEASE_BASE_URL = "file://$release"
$result = Invoke-Launcher $launcher @('--loam-bootstrap')
if ($result.ExitCode -ne 0) { Fail "8: bootstrap exited $($result.ExitCode): $($result.Output)" }
if (-not (Test-Path (Join-Path $install '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe'))) {
  Fail '8: runtime was not published'
}
Add-Check

# --- 9. Checksum mismatch never publishes an executable.
$badRelease = Join-Path $tmp 'bad-release'
New-Item -ItemType Directory -Path $badRelease -Force | Out-Null
Set-Content -Path (Join-Path $badRelease 'loam-x86_64-pc-windows-msvc.exe') -Value '@echo tampered' -Encoding ASCII
Set-Content -Path (Join-Path $badRelease 'loam-runtime-manifest.json') -Encoding ASCII -Value @"
{"version":"1.2.3","runtimes":[{"target":"x86_64-pc-windows-msvc","file":"loam-x86_64-pc-windows-msvc.exe","sha256":"0000000000000000000000000000000000000000000000000000000000000000"}]}
"@
$tampered = Join-Path $tmp 'tampered'
$launcher = Install-Tree $tampered '1.2.3'
$env:LOAM_RELEASE_BASE_URL = "file://$badRelease"
$result = Invoke-Launcher $launcher @('--loam-bootstrap')
if ($result.ExitCode -eq 0) { Fail '9: checksum mismatch reported success' }
if (Test-Path (Join-Path $tampered '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe')) {
  Fail '9: checksum mismatch published an executable'
}
Add-Check

# --- 10. A fresh .installing marker suppresses a duplicate concurrent download.
$concurrent = Join-Path $tmp 'concurrent'
$launcher = Install-Tree $concurrent '1.2.3'
$markerDir = Join-Path $concurrent '.agents\loam\bin\1.2.3'
New-Item -ItemType Directory -Path $markerDir -Force | Out-Null
Set-Content -Path (Join-Path $markerDir 'x86_64-pc-windows-msvc.installing') -Value '' -Encoding ASCII
$env:LOAM_RELEASE_BASE_URL = "file://$release"
Invoke-Launcher $launcher @('--loam-bootstrap') | Out-Null
if (Test-Path (Join-Path $concurrent '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe')) {
  Fail '10: fresh marker did not suppress the duplicate install'
}
Add-Check

# --- 11. Updating CLI_VERSION installs beside the old version, never over it.
$upgrade = Join-Path $tmp 'upgrade'
$launcher = Install-Tree $upgrade '1.2.3'
$oldDir = Join-Path $upgrade '.agents\loam\bin\1.0.0\x86_64-pc-windows-msvc'
New-Item -ItemType Directory -Path $oldDir -Force | Out-Null
Set-Content -Path (Join-Path $oldDir 'loam.exe') -Value '@echo old' -Encoding ASCII
$result = Invoke-Launcher $launcher @('--loam-bootstrap')
if ($result.ExitCode -ne 0) { Fail "11: upgrade bootstrap exited $($result.ExitCode)" }
if ((Get-Content (Join-Path $oldDir 'loam.exe') -Raw).Trim() -ne '@echo old') {
  Fail '11: previous version was overwritten'
}
if (-not (Test-Path (Join-Path $upgrade '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe'))) {
  Fail '11: new version was not installed'
}
Add-Check

# --- 12. Missing release leaves no runtime behind.
$missing = Join-Path $tmp 'missing'
$launcher = Install-Tree $missing '1.2.3'
$env:LOAM_RELEASE_BASE_URL = "file://$tmp\no-such-release"
$result = Invoke-Launcher $launcher @('--loam-bootstrap')
if ($result.ExitCode -eq 0) { Fail '12: missing release reported success' }
if (Test-Path (Join-Path $missing '.agents\loam\bin\1.2.3\x86_64-pc-windows-msvc\loam.exe')) {
  Fail '12: missing release published an executable'
}
Add-Check

Remove-Item Env:\LOAM_RELEASE_BASE_URL -ErrorAction SilentlyContinue
Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "loam.ps1 contract: PASS ($checks checks)"
