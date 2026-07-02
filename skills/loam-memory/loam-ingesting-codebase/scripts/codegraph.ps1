#!/usr/bin/env pwsh
# PowerShell twin of codegraph.sh.

param(
  [Parameter(Position = 0)] [string]$Subcommand = '',
  [Parameter(Position = 1, ValueFromRemainingArguments)] [string[]]$Rest = @()
)

$ErrorActionPreference = 'Stop'
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$SkillDir = Split-Path -Parent $ScriptDir
$DefaultExclusions = Join-Path $SkillDir 'references/ingestion-exclusions.md'
$MaxBytes = 500 * 1024

function Show-Usage {
  Write-Host @"
Usage:
  codegraph.ps1 index <wiki-root> [--codebase-root <codebase-root>]
  codegraph.ps1 walk  <codebase-root> [--exclusions <exclusions.md>] [--summary] [--no-gitignore]
  codegraph.ps1 diff  <codebase-root> <wiki-root> [--exclusions <exclusions.md>] [--no-gitignore] [--strict]
"@
  exit 1
}

function Format-JsonMtime([datetime]$Date) {
  ([DateTimeOffset]$Date.ToUniversalTime()).ToUnixTimeSeconds().ToString()
}

function Test-EpochString([string]$Value) { $Value -match '^\d+$' }

function Assert-WikiRoot([string]$WikiRoot) {
  if (-not $WikiRoot -or -not (Test-Path $WikiRoot -PathType Container)) {
    Write-Error "Error: wiki root not found: $WikiRoot"; exit 2
  }
  if ((Test-Path (Join-Path $WikiRoot 'SCHEMA.md') -PathType Leaf) -or (Test-Path (Join-Path $WikiRoot 'index.md') -PathType Leaf) -or (Test-Path (Join-Path $WikiRoot 'log.md') -PathType Leaf)) { return }

  $nestedWiki = Join-Path $WikiRoot 'wiki'
  if ((Test-Path (Join-Path $nestedWiki 'SCHEMA.md') -PathType Leaf) -or (Test-Path (Join-Path $nestedWiki 'index.md') -PathType Leaf) -or (Test-Path (Join-Path $nestedWiki 'log.md') -PathType Leaf)) {
    Write-Error "Error: wiki root contract not found: $WikiRoot; did you mean: $nestedWiki"; exit 2
  }

  Write-Error "Error: wiki root contract not found: $WikiRoot"; exit 2
}

function Read-Exclusions([string]$Path) {
  if (-not (Test-Path $Path -PathType Leaf)) { Write-Error "Error: exclusions file not found: $Path"; exit 3 }
  $exclude = @()
  $include = @()
  $section = ''
  $inCode = $false
  foreach ($raw in Get-Content $Path) {
    $line = $raw
    if ($line -notmatch '^##') {
      $hash = $line.IndexOf('#')
      if ($hash -ge 0) { $line = $line.Substring(0, $hash) }
    }
    $line = $line.Trim()
    if (-not $line) { continue }
    if ($line -eq '```') { $inCode = -not $inCode; continue }
    if ($line -match '^##\s*(.*)$') { $section = $matches[1]; continue }
    if (-not $inCode) { continue }
    if ($section -match 'Include') {
      foreach ($ext in ($line -split '\s+')) { if ($ext) { $include += $ext.TrimStart('.') } }
    } else { $exclude += $line }
  }
  @{ Exclude = $exclude; Include = $include }
}

function Test-Excluded([string]$RelPath, [string[]]$Patterns) {
  $base = Split-Path -Leaf $RelPath
  foreach ($pat in $Patterns) {
    if (-not $pat) { continue }
    $match = $pat -replace '\*\*', '*'
    if ($RelPath -like $match -or $base -like $match) { return $true }
    # ponytail: **/ prefix means "any depth incl root"; */X/* misses root-level X/*, so also test stripped
    if ($pat.StartsWith('**/')) {
      $root = $match.Substring(2)
      if ($RelPath -like $root) { return $true }
    }
  }
  $false
}

function Test-GitIgnored([string]$Root, [string]$RelPath) {
  & git -C $Root check-ignore --quiet -- $RelPath 2>$null
  $LASTEXITCODE -eq 0
}

function Test-BinaryFile([string]$Path) {
  $bytes = [System.IO.File]::ReadAllBytes($Path)
  foreach ($b in $bytes) { if ($b -eq 0) { return $true } }
  $false
}

function Test-WhitespaceOnly([string]$Path) {
  $text = Get-Content $Path -Raw -ErrorAction SilentlyContinue
  if ($null -eq $text) { return $true }
  $text -notmatch '\S'
}

function Test-GeneratedHeader([string]$Path) {
  $header = (Get-Content $Path -TotalCount 5 -ErrorAction SilentlyContinue) -join "`n"
  $header -match '(?i)generated|auto-generated|do not edit|@generated|Code generated|This file was generated'
}

function Collect-Walk([string]$CodebaseRoot, [string]$ExclusionsFile, [bool]$RespectGitignore) {
  if (-not $CodebaseRoot -or -not (Test-Path $CodebaseRoot -PathType Container)) { Write-Error "Error: codebase root not found: $CodebaseRoot"; exit 2 }
  $rules = Read-Exclusions $ExclusionsFile
  $entries = @()
  $byExt = @{}
  $excluded = [ordered]@{ pattern = 0; gitignore = 0; empty = 0; large = 0; generated_header = 0; binary = 0 }
  $useGit = $false
  if ($RespectGitignore -and (Get-Command git -ErrorAction SilentlyContinue)) {
    & git -C $CodebaseRoot rev-parse --is-inside-work-tree *> $null
    $useGit = ($LASTEXITCODE -eq 0)
  }

  foreach ($file in Get-ChildItem -Path $CodebaseRoot -Recurse -File -Force) {
    $rel = $file.FullName.Substring($CodebaseRoot.Length).TrimStart('\', '/') -replace '\\', '/'
    $ext = $file.Extension.TrimStart('.')
    if ($rules.Include -notcontains $ext) { continue }
    if (Test-Excluded $rel $rules.Exclude) { $excluded.pattern++; continue }
    if ($useGit -and (Test-GitIgnored $CodebaseRoot $rel)) { $excluded.gitignore++; continue }
    if ($file.Length -eq 0) { $excluded.empty++; continue }
    if (Test-WhitespaceOnly $file.FullName) { $excluded.empty++; continue }
    if (Test-BinaryFile $file.FullName) { $excluded.binary++; continue }
    if ($file.Length -gt $MaxBytes) { $excluded.large++; continue }
    if (Test-GeneratedHeader $file.FullName) { $excluded.generated_header++; continue }

    $entries += [PSCustomObject]@{ path = $rel; mtime = (Format-JsonMtime $file.LastWriteTime); size = $file.Length }
    $byExt[$ext] = 1 + ($byExt[$ext] ?? 0)
  }
  @{ Entries = $entries; ByExt = $byExt; Excluded = $excluded }
}

function Resolve-Source([string]$SourcePath, [string]$CodebaseRoot) {
  if ([System.IO.Path]::IsPathRooted($SourcePath)) { return $SourcePath }
  if ($CodebaseRoot) { return (Join-Path $CodebaseRoot $SourcePath) }
  $SourcePath
}

function Collect-Index([string]$WikiRoot, [string]$CodebaseRoot = '') {
  Assert-WikiRoot $WikiRoot
  $entries = @()
  # Dual scan: code/ (primary) and entities/ (legacy transition for stranded source_path: pages)
  foreach ($scanDir in @('code', 'entities')) {
    $entitiesDir = Join-Path $WikiRoot $scanDir
    if (-not (Test-Path $entitiesDir -PathType Container)) { continue }
    foreach ($page in Get-ChildItem -Path $entitiesDir -Filter '*.md' -File) {
      $sourcePath = ''
      $ingestedAt = ''
      $sourceSize = ''
      $contentHash = ''
      $inFm = $false
      foreach ($line in (Get-Content $page.FullName)) {
        $trimmed = $line.Trim()
        if ($trimmed -eq '---') { if ($inFm) { break } else { $inFm = $true; continue } }
        if ($inFm) {
          if ($trimmed -match '^source_path:\s*(.*)$') { $sourcePath = $matches[1].Trim('"') }
          if ($trimmed -match '^ingested_at:\s*(.*)$') { $ingestedAt = $matches[1].Trim('"') }
          if ($trimmed -match '^source_size:\s*(.*)$') { $sourceSize = $matches[1].Trim('"') }
          if ($trimmed -match '^content_hash:\s*(.*)$') { $contentHash = $matches[1].Trim('"').ToLowerInvariant() }
        }
      }
      if (-not $sourcePath -or -not $ingestedAt) { continue }
      $skip = $false
      foreach ($e in $entries) { if ($e.source_path -eq $sourcePath) { $skip = $true; break } }
      if ($skip) { continue }
      $resolved = Resolve-Source $sourcePath $CodebaseRoot
      $exists = Test-Path $resolved -PathType Leaf
      $mtime = ''
      if ($exists) { $mtime = Format-JsonMtime (Get-Item $resolved).LastWriteTime }
      $entries += [PSCustomObject]@{ source_path = $sourcePath; slug = [System.IO.Path]::GetFileNameWithoutExtension($page.Name); ingested_at = $ingestedAt; source_size = $sourceSize; content_hash = $contentHash; mtime = $mtime; exists = $exists }
    }
  }
  $entries
}

function Write-Json($Value) {
  $json = $Value | ConvertTo-Json -Depth 5 -Compress
  if (-not $json) { $json = '[]' }
  if ($Value -is [array] -and $Value.Count -eq 1) { $json = "[$json]" }
  Write-Host $json
}

switch ($Subcommand) {
  'index' {
    if ($Rest.Count -lt 1) { Show-Usage }
    $wikiRoot = $Rest[0]
    $codebaseRoot = ''
    for ($i = 1; $i -lt $Rest.Count; $i++) {
      if ($Rest[$i] -eq '--codebase-root' -and ($i + 1) -lt $Rest.Count) { $codebaseRoot = $Rest[$i + 1]; $i++ }
      else { Write-Error "Error: unknown flag: $($Rest[$i])"; exit 1 }
    }
    Write-Json @(Collect-Index $wikiRoot $codebaseRoot)
  }
  'walk' {
    if ($Rest.Count -lt 1) { Show-Usage }
    $codebaseRoot = $Rest[0]
    $exclusions = $DefaultExclusions
    $summary = $false
    $respectGitignore = $true
    for ($i = 1; $i -lt $Rest.Count; $i++) {
      switch ($Rest[$i]) {
        '--exclusions' { $exclusions = $Rest[$i + 1]; $i++ }
        '--summary' { $summary = $true }
        '--no-gitignore' { $respectGitignore = $false }
        default { Write-Error "Error: unknown flag: $($Rest[$i])"; exit 1 }
      }
    }
    $walk = Collect-Walk $codebaseRoot $exclusions $respectGitignore
    if ($summary) { Write-Json ([PSCustomObject]@{ total = $walk.Entries.Count; by_ext = $walk.ByExt; excluded = $walk.Excluded }) }
    else { Write-Json @($walk.Entries) }
  }
  'diff' {
    if ($Rest.Count -lt 2) { Show-Usage }
    $codebaseRoot = $Rest[0]
    $wikiRoot = $Rest[1]
    $exclusions = $DefaultExclusions
    $respectGitignore = $true
    $strict = $false
    for ($i = 2; $i -lt $Rest.Count; $i++) {
      switch ($Rest[$i]) {
        '--exclusions' { $exclusions = $Rest[$i + 1]; $i++ }
        '--no-gitignore' { $respectGitignore = $false }
        '--strict' { $strict = $true }
        default { Write-Error "Error: unknown flag: $($Rest[$i])"; exit 1 }
      }
    }
    $walk = Collect-Walk $codebaseRoot $exclusions $respectGitignore
    $index = @{}
    foreach ($entry in (Collect-Index $wikiRoot $codebaseRoot)) { $index[$entry.source_path] = $entry }
    $diff = @()
    foreach ($entry in $walk.Entries) {
      if (-not $index.ContainsKey($entry.path)) {
        $diff += [PSCustomObject]@{ path = $entry.path; mtime = $entry.mtime; reason = 'new' }
        continue
      }
      $idx = $index[$entry.path]
      $reason = ''
      if ($strict) {
        if ($idx.content_hash) {
          $fileHash = (Get-FileHash -Path (Join-Path $codebaseRoot $entry.path) -Algorithm SHA256).Hash.ToLower()
          if ($fileHash -eq $idx.content_hash) { $reason = '' } else { $reason = 'stale' }
        } else { $reason = 'stale' }
      } elseif (-not (Test-EpochString $idx.ingested_at)) {
        $reason = 'stale'
      } elseif ([int64]$entry.mtime -gt [int64]$idx.ingested_at) {
        if ($idx.source_size -and ($idx.source_size -match '^\d+$')) {
          if ([string]$entry.size -ne $idx.source_size) {
            $reason = 'stale'
          } elseif ($idx.content_hash) {
            $fileHash = (Get-FileHash -Path (Join-Path $codebaseRoot $entry.path) -Algorithm SHA256).Hash.ToLower()
            if ($fileHash -eq $idx.content_hash) { $reason = '' } else { $reason = 'stale' }
          } else { $reason = 'stale' }
        } else { $reason = 'stale' }
      }
      if ($reason) { $diff += [PSCustomObject]@{ path = $entry.path; mtime = $entry.mtime; reason = $reason; slug = $idx.slug } }
    }
    Write-Json @($diff)
  }
  default { Show-Usage }
}
