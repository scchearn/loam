#!/usr/bin/env pwsh
# datecheck.ps1 — PowerShell twin of datecheck.sh
# Scans wiki markdown for date-format drift, optionally fixes.
#
# Usage:
#   datecheck.ps1 check <wiki-root>              # report drift as JSON
#   datecheck.ps1 fix   <wiki-root> [--offset +02:00]  # apply normalizations
#
# Exit codes: 0 (no drift / fixes applied), 1 (bad args), 2 (drift found in check mode)

param(
  [Parameter(Position = 0)]
  [string]$Mode = '',
  [Parameter(Position = 1)]
  [string]$WikiRoot = '',
  [string]$Offset = ''
)

$ErrorActionPreference = 'Stop'

if (-not $Mode -or -not $WikiRoot) {
  Write-Host 'Usage: datecheck.ps1 <check|fix> <wiki-root> [--offset +HH:MM]'
  exit 1
}

if (-not (Test-Path $WikiRoot -PathType Container)) {
  Write-Host "{`"error`":`"wiki root not found: $WikiRoot`"}"
  exit 1
}

if (-not $Offset) {
  $tz = [TimeZoneInfo]::Local
  $offsetHours = $tz.GetUtcOffset([DateTime]::Now).TotalHours
  $sign = if ($offsetHours -ge 0) { '+' } else { '-' }
  $absHours = [Math]::Abs($offsetHours)
  $h = [int][Math]::Floor($absHours)
  $m = [int](($absHours - $h) * 60)
  $Offset = "$sign$($h.ToString('D2')):$($m.ToString('D2'))"
}

# Legacy TZ labels
$LegacyTzPattern = ' (SAST|GMT[+-]\d+|UTC|UT)$'

# Point-in-time front matter fields
$TzFields = @('created_at','updated_at','approved_at','started_at','completed_at')

function ConvertTo-JsonLine {
  param([hashtable]$obj)
  $pairs = @()
  foreach ($key in $obj.Keys) {
    $val = $obj[$key] -replace '\\','\\' -replace '"','\"'
    $pairs += "`"$key`":`"$val`""
  }
  return '{' + ($pairs -join ',') + '}'
}

function Invoke-ScanFile {
  param([string]$file)
  $rel = $file.Substring($WikiRoot.Length).TrimStart('\','/')
  $lines = Get-Content $file -Encoding UTF8
  $inFrontmatter = $false
  $foundDrift = $false

  for ($i = 0; $i -lt $lines.Length; $i++) {
    $lineNum = $i + 1
    $line = $lines[$i]

    if ($i -eq 0 -and $line -eq '---') { $inFrontmatter = $true; continue }
    if ($inFrontmatter -and $line -eq '---') { $inFrontmatter = $false; continue }

    # Check front matter TZ fields
    if ($inFrontmatter) {
      foreach ($field in $TzFields) {
        if ($line -match "^$field`:\s+(.+)$") {
          $value = $Matches[1].Trim()
          if ($value -eq 'null' -or -not $value) { continue }

          if ($value -match $LegacyTzPattern) {
            $obj = @{ file=$rel; line=$lineNum; field=$field; value=$value; issue='legacy_tz'; fix="replace with $Offset" }
            Write-Host (ConvertTo-JsonLine $obj)
            $foundDrift = $true
          } elseif ($value -match '^\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}$') {
            $obj = @{ file=$rel; line=$lineNum; field=$field; value=$value; issue='missing_offset'; fix="add $Offset" }
            Write-Host (ConvertTo-JsonLine $obj)
            $foundDrift = $true
          }
        }
      }
    }

    # Check Captured: lines
    if ($line -match '^-\s*Captured:\s+(.+)$') {
      $value = $Matches[1]
      if ($value -match $LegacyTzPattern) {
        $obj = @{ file=$rel; line=$lineNum; field='Captured'; value=$value; issue='legacy_tz'; fix="replace with $Offset" }
        Write-Host (ConvertTo-JsonLine $obj)
        $foundDrift = $true
      } elseif ($value -match '^\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}$') {
        $obj = @{ file=$rel; line=$lineNum; field='Captured'; value=$value; issue='missing_offset'; fix="add $Offset" }
        Write-Host (ConvertTo-JsonLine $obj)
        $foundDrift = $true
      }
    }

    # Check decisions-log separators
    if ($line -match '^-\s+\d{4}-\d{2}-\d{2}(.*?)$') {
      $rest = $Matches[1]
      if ($rest -notmatch '^\s*\u2014' -and $rest -match '^(\s*[-:]\s*)') {
        $sep = $Matches[1]
        $obj = @{ file=$rel; line=$lineNum; field='decisions_log'; value=$sep; issue='wrong_separator'; fix='use em-dash —' }
        Write-Host (ConvertTo-JsonLine $obj)
        $foundDrift = $true
      }
    }
  }

  return -not $foundDrift
}

function Invoke-FixFile {
  param([string]$file)
  $rel = $file.Substring($WikiRoot.Length).TrimStart('\','/')
  $before = Get-Content $file -Raw -Encoding UTF8

  $content = $before

  # 1. Add offset to bare date-times in front matter fields
  foreach ($field in $TzFields) {
    $content = $content -replace "(?m)^$field`: (\d{4}-\d{2}-\d{2} \d{2}:\d{2})$", "$field`: `$1 $Offset"
  }

  # 2. Replace legacy TZ labels in front matter fields
  foreach ($field in $TzFields) {
    $content = $content -replace "(?m)^$field`: (\d{4}-\d{2}-\d{2} \d{2}:\d{2}) (SAST|GMT[+-]\d+|UTC|UT)$", "$field`: `$1 $Offset"
  }

  # 3. Add offset to bare Captured: timestamps
  $content = $content -replace '(?m)^(- Captured: \d{4}-\d{2}-\d{2} \d{2}:\d{2})$', "`$1 $Offset"

  # 4. Replace legacy TZ labels in Captured: lines
  $content = $content -replace '(?m)^(- Captured: \d{4}-\d{2}-\d{2} \d{2}:\d{2}) (SAST|GMT[+-]\d+|UTC|UT)$', "`$1 $Offset"

  # 5. Fix decisions-log separators
  $content = $content -replace '(?m)^(- \d{4}-\d{2}-\d{2}) - ', '$1 — '
  $content = $content -replace '(?m)^(- \d{4}-\d{2}-\d{2}): ', '$1 — '

  if ($content -ne $before) {
    Set-Content -Path $file -Value $content -Encoding UTF8 -NoNewline
    Write-Host $rel
    return $true
  }
  return $false
}

# --- main ---

$files = Get-ChildItem $WikiRoot -Recurse -Filter '*.md' -File | Sort-Object FullName

switch ($Mode) {
  'check' {
    $drift = $false
    foreach ($file in $files) {
      $ok = Invoke-ScanFile $file.FullName
      if (-not $ok) { $drift = $true }
    }
    if ($drift) { exit 2 }
    exit 0
  }
  'fix' {
    $fixedCount = 0
    foreach ($file in $files) {
      $ok = Invoke-ScanFile $file.FullName 2>&1 | Out-Null
      # Re-check: scan returns $true if no drift; we want to fix only if drift exists
      $scanOutput = Invoke-ScanFile $file.FullName 6>&1
      # If scan produced output, there's drift — fix it
      $tempFile = [System.IO.Path]::GetTempFileName()
      Invoke-ScanFile $file.FullName *>&1 | Out-File $tempFile
      $scanResult = (Get-Content $tempFile -Raw).Trim()
      Remove-Item $tempFile -Force
      if ($scanResult) {
        if (Invoke-FixFile $file.FullName) { $fixedCount++ }
      }
    }
    Write-Host "{`"mode`":`"fix`",`"offset`":`"$Offset`",`"files_fixed`":$fixedCount}"
    exit 0
  }
  default {
    Write-Host 'Usage: datecheck.ps1 <check|fix> <wiki-root> [--offset +HH:MM]'
    exit 1
  }
}