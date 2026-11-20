<#
.SYNOPSIS
Print the loam.canon/v1 bytes of a seal span with no loam runtime (trust floor).

.DESCRIPTION
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File seal-canon.ps1 [-Digest] <file> <anchor>

Writes the canonical bytes of the content strictly between the marker lines of
seal span <anchor> (a leading ^ or # is accepted) to stdout, raw, with no BOM
and no newline translation. With -Digest it writes only the lowercase sha256
hex of those bytes, computed in-process.

loam.canon/v1: strict UTF-8; CRLF and lone CR become LF; trailing spaces and
tabs are stripped per line; leading and trailing blank lines are dropped;
exactly one final LF; an all-blank span is the empty byte string. No NFC, no
reflow, no re-escaping. It mirrors seal-canon.sh and the runtime scanner:
whole-line HTML comment markers outside fenced code, indented code and front
matter; a malformed, nested, unclosed or duplicated span is not found.

Exit: 0 ok; 2 usage, missing file, missing or malformed anchor, invalid UTF-8.
Targets Windows PowerShell 5.1.
#>
param(
    [switch]$Digest,
    [Parameter(Position = 0)][string]$File,
    [Parameter(Position = 1)][string]$Anchor
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("seal-canon: $Message")
    exit 2
}

if ([string]::IsNullOrEmpty($File) -or [string]::IsNullOrEmpty($Anchor)) {
    [Console]::Error.WriteLine('usage: seal-canon.ps1 [-Digest] <file> <anchor>')
    exit 2
}
$Anchor = $Anchor.TrimStart('#', '^')
if ($Anchor.Length -eq 0) { Fail 'empty anchor' }

try {
    $bytes = [IO.File]::ReadAllBytes($File)
} catch {
    Fail "cannot read $File"
}
try {
    $text = (New-Object System.Text.UTF8Encoding($false, $true)).GetString($bytes)
} catch {
    Fail "$File is not valid UTF-8"
}

# CRLF, lone CR and LF all end a line.
$lines = New-Object 'System.Collections.Generic.List[string]'
$sb = New-Object System.Text.StringBuilder
$len = $text.Length
for ($k = 0; $k -lt $len; $k++) {
    $c = $text[$k]
    if ($c -eq "`n") {
        $lines.Add($sb.ToString()); [void]$sb.Clear()
    } elseif ($c -eq "`r") {
        if ($k + 1 -lt $len -and $text[$k + 1] -eq "`n") { $k++ }
        $lines.Add($sb.ToString()); [void]$sb.Clear()
    } else {
        [void]$sb.Append($c)
    }
}
if ($sb.Length -gt 0) { $lines.Add($sb.ToString()) }
$n = $lines.Count
# A leading BOM never sits inside a span.
if ($n -gt 0 -and $lines[0].Length -gt 0 -and $lines[0][0] -eq [char]0xFEFF) {
    $lines[0] = $lines[0].Substring(1)
}

$ws = [char[]]@(' ', "`t")

function Get-Width([string]$s) {
    $w = 0
    foreach ($ch in $s.ToCharArray()) {
        if ($ch -eq ' ') { $w++ }
        elseif ($ch -eq "`t") { $w += 4 - ($w % 4) }
        else { break }
    }
    return $w
}

function Get-Run([string]$s, [char]$ch) {
    $r = 0
    while ($r -lt $s.Length -and $s[$r] -eq $ch) { $r++ }
    return $r
}

function Test-ValidId([string]$prefix, [string]$id) {
    if (-not $id.StartsWith($prefix, [StringComparison]::Ordinal)) { return $false }
    $words = $id.Substring(2).Split('-')
    if ($words.Count -lt 2 -or $words.Count -gt 6) { return $false }
    foreach ($word in $words) {
        if ($word -cnotmatch '^[a-z0-9]+$') { return $false }
    }
    return $true
}

$fmEnd = -1
if ($n -gt 0 -and $lines[0] -match '^---[ \t]*$') {
    for ($i = 1; $i -lt $n; $i++) {
        if ($lines[$i] -match '^(---|\.\.\.)[ \t]*$') { $fmEnd = $i; break }
    }
}

$stack = New-Object System.Collections.ArrayList
$seen = @{}
$dup = @{}
$found = @{}
$fence = $false
$fch = [char]'`'
$flen = 0
$icode = $false
$pb = $true

for ($i = $fmEnd + 1; $i -lt $n; $i++) {
    $line = $lines[$i]
    $blank = ($line.Trim($ws).Length -eq 0)
    $ind = Get-Width $line
    $body = $line.TrimStart($ws)
    if ($fence) {
        $r = Get-Run $body $fch
        if ($ind -le 3 -and $r -ge $flen -and $body.Substring($r).Trim($ws).Length -eq 0) { $fence = $false }
        $pb = $false
        continue
    }
    if ($blank) { $pb = $true; continue }
    if ($ind -ge 4 -and ($pb -or $icode)) { $icode = $true; $pb = $false; continue }
    $icode = $false
    $pb = $false
    $first = $body[0]
    if ($ind -le 3 -and ($first -eq '`' -or $first -eq '~')) {
        $r = Get-Run $body $first
        if ($r -ge 3 -and -not ($first -eq '`' -and $body.Substring($r).Contains('`'))) {
            $fence = $true; $fch = $first; $flen = $r
            continue
        }
    }

    $t = $line.Trim($ws)
    if ($t.Length -lt 7 -or -not $t.StartsWith('<!--', [StringComparison]::Ordinal) -or -not $t.EndsWith('-->', [StringComparison]::Ordinal)) { continue }
    $inner = $t.Substring(4, $t.Length - 7).Trim($ws)
    if ($inner.Length -eq 0) { continue }
    $w = @($inner -split '[ \t]+')
    $head = $w[0]
    $closing = $head.StartsWith('/', [StringComparison]::Ordinal)
    $name = if ($closing) { $head.Substring(1) } else { $head }
    if ($name -ceq 'loam:seal') { $kind = 'seal' }
    elseif ($name -ceq 'loam:class') { $kind = 'class' }
    else { continue }
    $prefix = if ($kind -eq 'seal') { 's-' } else { 'c-' }

    if (-not $closing) {
        $oid = if ($w.Count -ge 2) { $w[1] } else { '' }
        $ex = $false
        if (Test-ValidId $prefix $oid) {
            $seen[$oid] = 1 + [int]$seen[$oid]
            if ($seen[$oid] -gt 1) { $dup[$oid] = $true }
        } else { $ex = $true }
        if ($kind -eq 'seal' -and $w.Count -gt 2) { $ex = $true }
        if ($kind -eq 'class') {
            $okTok = ($w.Count -eq 3) -and (@('local-only', 'project', 'hub-promotable') -ccontains $w[2])
            if (-not $okTok) { $ex = $true }
        }
        $conflict = -1
        for ($s = $stack.Count - 1; $s -ge 0; $s--) {
            if ($stack[$s].Kind -eq 'seal') { $conflict = $s; break }
        }
        if ($conflict -lt 0 -and $kind -eq 'class') {
            for ($s = $stack.Count - 1; $s -ge 0; $s--) {
                if ($stack[$s].Kind -eq 'class') { $conflict = $s; break }
            }
        }
        if ($conflict -ge 0) { $stack[$conflict].Ex = $true; $ex = $true }
        [void]$stack.Add([pscustomobject]@{ Kind = $kind; Id = $oid; Start = $i + 1; Ex = $ex })
    } else {
        if ($stack.Count -eq 0) { continue }
        $top = $stack[$stack.Count - 1]
        if ($top.Kind -ne $kind) { $top.Ex = $true; continue }
        $ex = $top.Ex
        if ($w.Count -gt 1) { $ex = $true }
        if (-not $ex -and $kind -eq 'seal') { $found[$top.Id] = @($top.Start, $i) }
        $stack.RemoveAt($stack.Count - 1)
    }
}

if (-not $found.ContainsKey($Anchor) -or $dup.ContainsKey($Anchor)) {
    Fail "no well-formed seal span ^$Anchor in $File"
}
$from = $found[$Anchor][0]
$to = $found[$Anchor][1]
$kept = New-Object 'System.Collections.Generic.List[string]'
for ($i = $from; $i -lt $to; $i++) { $kept.Add($lines[$i].TrimEnd($ws)) }
$lo = 0
$hi = $kept.Count - 1
while ($lo -le $hi -and $kept[$lo].Length -eq 0) { $lo++ }
while ($hi -ge $lo -and $kept[$hi].Length -eq 0) { $hi-- }
$out = New-Object System.Text.StringBuilder
for ($i = $lo; $i -le $hi; $i++) { [void]$out.Append($kept[$i]).Append("`n") }
$canon = (New-Object System.Text.UTF8Encoding($false)).GetBytes($out.ToString())

if ($Digest) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $hash = $sha.ComputeHash($canon) } finally { $sha.Dispose() }
    $hex = (($hash | ForEach-Object { $_.ToString('x2') }) -join '')
    [Console]::Out.Write($hex + "`n")
} else {
    $stdout = [Console]::OpenStandardOutput()
    $stdout.Write($canon, 0, $canon.Length)
    $stdout.Flush()
}
exit 0
