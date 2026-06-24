#!/usr/bin/env pwsh
# loamstate.ps1 — PowerShell twin of loamstate.sh
# Probes wiki root and qmd readiness in one shot.
#
# Usage:
#   loamstate.ps1 <workspace-root>
#
# Exit codes: 0 (JSON always emitted), 1 bad args

param(
  [Parameter(Position = 0)]
  [string]$WorkspaceRoot = ''
)

$ErrorActionPreference = 'Stop'

if (-not $WorkspaceRoot) {
  Write-Host 'Usage: loamstate.ps1 <workspace-root>'
  exit 1
}

if (-not (Test-Path $WorkspaceRoot -PathType Container)) {
  Write-Host '{"error":"workspace not found"}'
  exit 0
}

# --- Resolve wiki root ---

$WikiRoot = ''
foreach ($candidate in @((Join-Path $WorkspaceRoot 'wiki'), $WorkspaceRoot)) {
  if ((Test-Path (Join-Path $candidate 'SCHEMA.md')) -or
      (Test-Path (Join-Path $candidate 'index.md')) -or
      (Test-Path (Join-Path $candidate 'log.md'))) {
    $WikiRoot = (Resolve-Path $candidate).Path
    break
  }
}

if (-not $WikiRoot) {
  Write-Host '{"wiki_root":"","exists":false,"qmd_ready":false}'
  exit 0
}

# --- Check contract files ---

$HasSchema   = Test-Path (Join-Path $WikiRoot 'SCHEMA.md')
$HasIndex    = Test-Path (Join-Path $WikiRoot 'index.md')
$HasLog      = Test-Path (Join-Path $WikiRoot 'log.md')
$HasOverview = Test-Path (Join-Path $WikiRoot 'overview.md')

# --- qmd readiness ---

$QmdReady = $false
$Collection = ''
$MetaStatus = ''
$MetaPath = ''

$MetaFile = Join-Path $WikiRoot '.wiki-metadata.json'
if (Test-Path $MetaFile -PathType Leaf) {
  $MetaPath = $MetaFile
  $meta = Get-Content $MetaFile -Raw | ConvertFrom-Json
  if ($meta.retrieval) {
    $MetaStatus = $meta.retrieval.status
    $Collection = $meta.retrieval.collection_name
    if ($MetaStatus -eq 'ready') { $QmdReady = $true }
  }
}

# Fallback: if not ready from metadata, try qmd CLI
if (-not $QmdReady) {
  $qmdCmd = Get-Command qmd -ErrorAction SilentlyContinue
  if ($qmdCmd) {
    try {
      $collections = qmd collection list 2>$null
      if ($collections) {
        foreach ($line in ($collections -split "`n")) {
          if ($line -match [regex]::Escape($WikiRoot)) {
            $QmdReady = $true
            if (-not $Collection) {
              $Collection = ($line -split '\s+')[0] -replace '[: ]',''
            }
            break
          }
        }
      }
    } catch { }
  }
}

# --- Emit JSON ---

$result = [PSCustomObject]@{
  wiki_root       = $WikiRoot
  exists          = $true
  has_schema      = $HasSchema
  has_index       = $HasIndex
  has_log         = $HasLog
  has_overview    = $HasOverview
  qmd_ready       = $QmdReady
  collection      = $Collection
  metadata_status = $MetaStatus
  metadata_path   = $MetaPath
}

$json = $result | ConvertTo-Json -Compress -Depth 2
Write-Host $json