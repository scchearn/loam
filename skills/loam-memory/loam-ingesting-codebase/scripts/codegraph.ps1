#!/usr/bin/env pwsh
# codegraph.ps1 — PowerShell twin of codegraph.sh
# Helper for loam::ingesting-codebase and loam::syncing-code-graph on Windows.
#
# Subcommands:
#   index <wiki-root>           Emit JSON of code-ingested entity pages in the wiki
#   walk  <codebase-root>       Emit JSON of candidate code files under the codebase root
#
# Exit codes: 0 success, 1 bad args, 2 root not found, 3 exclusions file missing.

param(
  [Parameter(Position = 0)]
  [string]$Subcommand = '',
  [Parameter(Position = 1, ValueFromRemainingArguments)]
  [string[]$Rest = @()
)

$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$SkillDir = Split-Path -Parent $ScriptDir
$DefaultExclusions = Join-Path $SkillDir 'references/ingestion-exclusions.md'

function Format-JsonDate([datetime]$dt) {
  return $dt.ToString('yyyy-MM-dd')
}

function Show-Usage {
  Write-Host @"
Usage:
  codegraph.ps1 index <wiki-root>
  codegraph.ps1 walk  <codebase-root> [--exclusions <exclusions.md>]

  index  - Globs <wiki-root>/entities/*.md, parses front matter (source_path, ingested_at),
           stats each source_path for current mtime, emits JSON.

  walk   - Walks <codebase-root> recursively, applies exclusion globs, lists candidate
           code files (by extension) with mtime. --exclusions defaults to the bundled
           references/ingestion-exclusions.md.

Exit codes: 0 ok, 1 bad args, 2 root not found, 3 exclusions file missing.
"@
  exit 1
}

function Invoke-Index {
  param([string]$WikiRoot)

  if (-not $WikiRoot -or -not (Test-Path $WikiRoot -PathType Container)) {
    Write-Error "Error: wiki root not found: $WikiRoot"; exit 2
  }

  $entitiesDir = Join-Path $WikiRoot 'entities'
  if (-not (Test-Path $entitiesDir -PathType Container)) {
    Write-Host '[]'; exit 0
  }

  $entries = @()

  Get-ChildItem -Path $entitiesDir -Filter '*.md' -File | ForEach-Object {
    $content = Get-Content $_.FullName -Raw
    $sourcePath = ''
    $ingestedAt = ''

    # Parse front matter (between first two --- lines)
    $lines = $content -split "`n"
    $inFm = $false
    foreach ($line in $lines) {
      $trimmed = $line.Trim()
      if ($trimmed -eq '---') {
        if ($inFm) { break } else { $inFm = $true; continue }
      }
      if ($inFm) {
        if ($trimmed -match '^source_path:\s*(.*)$') { $sourcePath = $matches[1].Trim('"') }
        if ($trimmed -match '^ingested_at:\s*(.*)$') { $ingestedAt = $matches[1].Trim('"') }
      }
    }

    # Skip prose entity pages without code-graph front matter
    if (-not $sourcePath -or -not $ingestedAt) { return }

    $slug = [System.IO.Path]::GetFileNameWithoutExtension($_.Name)

    $exists = $false
    $mtimeStr = ''
    if (Test-Path $sourcePath -PathType Leaf) {
      $mtime = (Get-Item $sourcePath).LastWriteTime
      $mtimeStr = Format-JsonDate $mtime
      $exists = $true
    }

    $entries += [PSCustomObject]@{
      source_path = $sourcePath
      slug = $slug
      ingested_at = $ingestedAt
      mtime = $mtimeStr
      exists = $exists
    }
  }

  $json = $entries | ConvertTo-Json -Depth 2 -Compress
  if (-not $json -or $json -eq '') { $json = '[]' }
  elseif ($entries.Count -eq 1) { $json = "[$json]" }
  Write-Host $json
}

function Invoke-Walk {
  param(
    [string]$CodebaseRoot,
    [string]$ExclusionsFile
  )

  if (-not $CodebaseRoot -or -not (Test-Path $CodebaseRoot -PathType Container)) {
    Write-Error "Error: codebase root not found: $CodebaseRoot"; exit 2
  }
  if (-not (Test-Path $ExclusionsFile -PathType Leaf)) {
    Write-Error "Error: exclusions file not found: $ExclusionsFile"; exit 3
  }

  # Parse exclusions
  $excludePatterns = @()
  $includeExts = @()
  $section = ''
  $inCode = $false

  foreach ($line in (Get-Content $ExclusionsFile)) {
    $trimmed = $line.Trim()
    if ($trimmed -eq '') { continue }

    # Toggle code-block state
    if ($trimmed -eq '```') { $inCode = -not $inCode; continue }

    # Section headers (always processed)
    if ($trimmed -match '^##\s*(.*)$') { $section = $matches[1]; continue }

    # Only process lines inside code blocks
    if (-not $inCode) { continue }

    # Strip inline comments (but not ## headings)
    if ($trimmed -notmatch '^##') {
      $hashIdx = $trimmed.IndexOf('#')
      if ($hashIdx -ge 0) { $trimmed = $trimmed.Substring(0, $hashIdx).Trim() }
      if ($trimmed -eq '') { continue }
    }

    if ($section -match 'Include') {
      foreach ($ext in ($trimmed -split '\s+')) {
        $cleanExt = $ext -replace '^\.', ''
        if ($cleanExt) { $includeExts += $cleanExt }
      }
    } else {
      $excludePatterns += $trimmed
    }
  }

  # Collect candidate files
  $entries = @()
  $allFiles = Get-ChildItem -Path $CodebaseRoot -Recurse -File
  foreach ($file in $allFiles) {
    $relPath = $file.FullName.Substring($CodebaseRoot.Length).TrimStart('\', '/')

    # Check extension
    $ext = $file.Extension.TrimStart('.')
    if ($includeExts -notcontains $ext) { continue }

    # Apply exclusion patterns
    $skip = $false
    foreach ($pat in $excludePatterns) {
      $matchPat = $pat -replace '\*\*', '*'
      if ($relPath -like $matchPat) { $skip = $true; break }
      $matchPat2 = $pat -replace '\*\*', '\*'
      if ($relPath -like $matchPat2) { $skip = $true; break }
    }
    if ($skip) { continue }

    $mtimeStr = Format-JsonDate $file.LastWriteTime
    $entries += [PSCustomObject]@{
      path = $relPath
      mtime = $mtimeStr
    }
  }

  $json = $entries | ConvertTo-Json -Depth 2 -Compress
  if (-not $json -or $json -eq '') { $json = '[]' }
  elseif ($entries.Count -eq 1) { $json = "[$json]" }
  Write-Host $json
}

# --- main ---

if (-not $Subcommand) { Show-Usage }

switch ($Subcommand) {
  'index' {
    if ($Rest.Count -lt 1) { Show-Usage }
    Invoke-Index -WikiRoot $Rest[0]
  }
  'walk' {
    if ($Rest.Count -lt 1) { Show-Usage }
    $codebaseRoot = $Rest[0]
    $exclusions = $DefaultExclusions
    for ($i = 1; $i -lt $Rest.Count; $i++) {
      if ($Rest[$i] -eq '--exclusions' -and ($i + 1) -lt $Rest.Count) {
        $exclusions = $Rest[$i + 1]; $i++
      }
    }
    Invoke-Walk -CodebaseRoot $codebaseRoot -ExclusionsFile $exclusions
  }
  default { Show-Usage }
}