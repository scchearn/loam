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
matter; a malformed, nested, unclosed or duplicated span, or one holding a
question record, is not found.

It is not a CommonMark parser, so it refuses (exit 2) rather than guesses: a
fence opener inside a list item or block quote, or code indented inside a list,
is ambiguous, as are an HTML block and a footnote definition; the target opener
appearing on more than one line is refused outright, and when the target id is mentioned in a file holding one the
span is refused. So is any marker or question line carrying whitespace beyond
space and tab (the runtime trims Unicode white space). The floor may refuse a
span the runtime accepts; it must never print a digest the runtime would not.

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
# The runtime trims every leading '#', then every leading '^'.
$Anchor = $Anchor.TrimStart('#').TrimStart('^')
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

# Strip block-quote and list-item prefixes; sets $script:pre when any went.
function Remove-Prefix([string]$s) {
    $script:pre = $false
    for ($g = 0; $g -lt 32; $g++) {
        $b = $s.TrimStart($ws)
        if ($b.StartsWith('>', [StringComparison]::Ordinal)) { $s = $b.Substring(1); $script:pre = $true; continue }
        $m = [regex]::Match($b, '^([-+*]|[0-9]+[.)])([ \t]|$)')
        if ($m.Success) { $s = $b.Substring($m.Length); $script:pre = $true; continue }
        break
    }
    return $s
}

# A question record the runtime nests-checks: "- [x] Q: ... ^q-<slug>".
function Test-QuestionAnchor([string]$s) {
    $tt = $s.TrimStart($ws)
    if (-not ($tt.StartsWith('- [', [StringComparison]::Ordinal) -or $tt.StartsWith('* [', [StringComparison]::Ordinal))) { return $false }
    $rest = $tt.Substring(3)
    if ($rest.Length -eq 0) { return $false }
    $l = if ([char]::IsHighSurrogate($rest[0])) { 2 } else { 1 }
    if ($rest.Length -lt $l) { return $false }
    if (-not $rest.Substring($l).StartsWith('] Q: ', [StringComparison]::Ordinal)) { return $false }
    $tok = @($tt.TrimEnd($ws) -split '[ \t]+')[-1]
    if (-not $tok.StartsWith('^', [StringComparison]::Ordinal)) { return $false }
    return (Test-ValidId 'q-' $tok.Substring(1))
}

$uws = '[\u000B\u000C\u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000]'

$fmEnd = -1
if ($n -gt 1 -and $lines[0] -match '^---[ \t]*$' -and $lines[1].Trim($ws).Length -gt 0) {
    for ($i = 1; $i -lt $n; $i++) {
        if ($lines[$i] -match '^(---|\.\.\.)[ \t]*$') { $fmEnd = $i; break }
    }
}

$stack = New-Object System.Collections.ArrayList
# Ordinal: the runtime matches ids exactly, and a plain @{} ignores case.
$seen = New-Object System.Collections.Hashtable ([StringComparer]::Ordinal)
$dup = New-Object System.Collections.Hashtable ([StringComparer]::Ordinal)
$found = New-Object System.Collections.Hashtable ([StringComparer]::Ordinal)
$fence = $false
$fch = [char]'`'
$flen = 0
$icode = $false
$pb = $true
$cont = $false
$amb = $false
$mention = $false
$badws = $false
$pre = $false
$mcount = 0
foreach ($l in $lines) { if ($l.Contains('loam:seal') -and $l.Contains($Anchor)) { $mcount++ } }

for ($i = $fmEnd + 1; $i -lt $n; $i++) {
    $line = $lines[$i]
    if ($line.Contains('loam:seal') -and $line.Contains($Anchor)) { $mention = $true }
    if (($line.Contains('loam:') -or $line.Contains('] Q: ')) -and $line -match $uws) { $badws = $true }
    $blank = ($line.Trim($ws).Length -eq 0)
    $ind = Get-Width $line
    $body = $line.TrimStart($ws)
    if ($fence) {
        $r = Get-Run $body $fch
        $pb = $false
        if ($ind -le 3 -and $r -ge $flen -and $body.Substring($r).Trim($ws).Length -eq 0) { $fence = $false; $pb = $true }
        continue
    }
    if ($blank) { $pb = $true; continue }
    if ($ind -ge 4 -and ($pb -or $icode)) { $icode = $true; $pb = $false; if ($cont) { $amb = $true }; continue }
    $icode = $false
    $pb = $false
    $s2 = Remove-Prefix $line
    $b2 = $s2.TrimStart($ws)
    if ($b2.StartsWith('<', [StringComparison]::Ordinal)) {
        $t3 = $b2.Trim($ws)
        if (-not ($t3.Length -ge 7 -and $t3.StartsWith('<!--', [StringComparison]::Ordinal) -and $t3.EndsWith('-->', [StringComparison]::Ordinal))) { $amb = $true }
    }
    if ($b2 -match '^\[\^[^\]]+\]:') { $amb = $true }
    $fl = $false
    if ($b2.Length -gt 0 -and ($b2[0] -eq '`' -or $b2[0] -eq '~')) {
        $c2 = $b2[0]
        $r2 = Get-Run $b2 $c2
        if ($r2 -ge 3 -and -not ($c2 -eq '`' -and $b2.Substring($r2).Contains('`'))) { $fl = $true }
    }
    if ($fl -and ($pre -or ($ind -ge 1 -and $cont))) { $amb = $true }
    if ($pre) { $cont = $true }
    if ($ind -le 3 -and -not $pre -and $fl) {
        $fence = $true; $fch = $c2; $flen = $r2
        continue
    }
    $t2 = $body -replace '[ \t]', ''
    $isHeading = ($body -match '^#+([ \t]|$)') -and ((Get-Run $body '#') -le 6)
    $isBreak = ($t2.Length -ge 3 -and $t2 -match '^(-+|\*+|_+)$') -or ($t2 -match '^(=+|-+)$')
    if ($ind -le 3 -and ($isHeading -or $isBreak)) { $pb = $true; continue }

    $t = $line.Trim($ws)
    if ($t.StartsWith('<!--', [StringComparison]::Ordinal) -and $t.EndsWith('-->', [StringComparison]::Ordinal)) { $pb = $true }
    if ($stack.Count -gt 0 -and (Test-QuestionAnchor $line)) {
        for ($s = $stack.Count - 1; $s -ge 0; $s--) {
            if ($stack[$s].Kind -eq 'seal') { $stack[$s].Ex = $true; break }
        }
        continue
    }
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

if ($badws) { Fail "unsupported whitespace in a marker line of $File; use the loam runtime" }
if ($mcount -gt 1) { Fail "^$Anchor opens on more than one line of $File; use the loam runtime" }
if ($amb -and $mention) { Fail "ambiguous list or block-quote structure around ^$Anchor in $File; use the loam runtime" }
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
