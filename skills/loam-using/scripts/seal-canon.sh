#!/bin/sh
# seal-canon.sh: print the loam.canon/v1 bytes of a seal span, with no loam
# runtime. The trust floor: it must agree byte for byte with `loam seal hash`.
#
#   sh seal-canon.sh [--digest] <file> <anchor>
#
# Prints the canonical bytes of the content strictly between the marker lines
# of the seal span `<anchor>` (a leading `^` or `#` is accepted), or with
# --digest only the lowercase sha256 hex of those bytes.
#
# loam.canon/v1: strict UTF-8; CRLF and lone CR become LF; trailing spaces and
# tabs are stripped per line; leading and trailing blank lines are dropped;
# exactly one final LF; an all-blank span is the empty byte string. No NFC,
# no reflow, no re-escaping.
#
# Span location mirrors the runtime marker scanner: whole-line HTML comments
# `<!-- loam:seal s-<slug> -->` ... `<!-- /loam:seal -->` outside fenced code,
# indented code and front matter; a malformed, nested, unclosed or duplicated
# span, or one holding a question record, is not found.
#
# This is not a CommonMark parser, so it refuses (exit 2) rather than guess:
# a fence opener inside a list item or block quote, or code indented inside a
# list, is ambiguous, and when the target id is mentioned in a file holding
# one the span is refused. So is any marker or question line carrying
# whitespace beyond space and tab (form feed, NBSP, U+2000..): the runtime
# trims Unicode white space, this script does not. NUL bytes are refused
# on purpose as invalid text. The floor may refuse a span the runtime
# accepts; it must never print a digest the runtime would not.
#
# Exit: 0 ok, 2 usage, missing file, missing/ambiguous anchor, invalid UTF-8.
# Needs only POSIX sh and awk, plus sha256sum or `shasum -a 256` for --digest.

set -u

usage() {
    echo "usage: seal-canon.sh [--digest] <file> <anchor>" >&2
    exit 2
}

digest=0
if [ "${1:-}" = "--digest" ]; then
    digest=1
    shift
fi
[ "$#" -eq 2 ] || usage
file=$1
anchor=$2
[ -f "$file" ] && [ -r "$file" ] || {
    echo "seal-canon: cannot read $file" >&2
    exit 2
}

# The runtime trims every leading '#', then every leading '^' (so "^#id" stays
# "#id" and is not found).
while :; do
    case $anchor in
        '#'*) anchor=${anchor#'#'} ;;
        *) break ;;
    esac
done
while :; do
    case $anchor in
        '^'*) anchor=${anchor#'^'} ;;
        *) break ;;
    esac
done
[ -n "$anchor" ] || usage

run_awk() {
    LC_ALL=C SEAL_ID=$anchor awk '
function width(s,   k, w, c) {
    w = 0
    for (k = 1; k <= length(s); k++) {
        c = substr(s, k, 1)
        if (c == " ") w++
        else if (c == "\t") w += 4 - (w % 4)
        else break
    }
    return w
}
function lead(s,   k, c) {
    for (k = 1; k <= length(s); k++) {
        c = substr(s, k, 1)
        if (c != " " && c != "\t") break
    }
    return k
}
function trim(s) {
    sub(/^[ \t]+/, "", s)
    sub(/[ \t]+$/, "", s)
    return s
}
function run(s, ch,   k) {
    k = 0
    while (substr(s, k + 1, 1) == ch) k++
    return k
}
function valid_id(prefix, id,   slug, k, nw, w) {
    if (substr(id, 1, 2) != prefix) return 0
    slug = substr(id, 3)
    nw = split(slug, w, "-")
    if (nw < 2 || nw > 6) return 0
    for (k = 1; k <= nw; k++) if (w[k] !~ /^[a-z0-9]+$/) return 0
    return 1
}
# Strip block-quote and list-item prefixes; sets the global pre when any went.
function unprefix(s,   b, guard) {
    pre = 0
    for (guard = 0; guard < 32; guard++) {
        b = substr(s, lead(s))
        if (substr(b, 1, 1) == ">") { s = substr(b, 2); pre = 1; continue }
        if (match(b, /^([-+*]|[0-9]+[.)])([ \t]|$)/)) { s = substr(b, RLENGTH + 1); pre = 1; continue }
        break
    }
    return s
}
# A question record the runtime nests-checks: "- [x] Q: ... ^q-<slug>".
function question_anchor(s,   tt, rest, c, l, nf, qw, tok, id) {
    tt = s
    sub(/^[ \t]+/, "", tt)
    if (substr(tt, 1, 3) != "- [" && substr(tt, 1, 3) != "* [") return 0
    rest = substr(tt, 4)
    c = substr(rest, 1, 1)
    if (c == "") return 0
    if (c < "\200") l = 1
    else if (c < "\340") l = 2
    else if (c < "\360") l = 3
    else l = 4
    if (substr(rest, l + 1, 5) != "] Q: ") return 0
    sub(/[ \t]+$/, "", tt)
    nf = split(tt, qw, /[ \t]+/)
    tok = qw[nf]
    if (substr(tok, 1, 1) != "^") return 0
    id = substr(tok, 2)
    return valid_id("q-", id)
}
BEGIN {
    uws = "[\013\014]|\302[\205\240]|\341\232\200|\342\200[\200-\212\250\251\257]|\342\201\237|\343\200\200"
    utf8 = "^([\001-\177]|[\302-\337][\200-\277]|\340[\240-\277][\200-\277]|[\341-\354\356\357][\200-\277][\200-\277]|\355[\200-\237][\200-\277]|\360[\220-\277][\200-\277][\200-\277]|[\361-\363][\200-\277][\200-\277][\200-\277]|\364[\200-\217][\200-\277][\200-\277])*$"
    n = 0
}
{
    rec = $0
    if (rec !~ utf8) { bad = 1; exit }
    # CRLF, lone CR and LF all end a line.
    sub(/\r$/, "", rec)
    k = split(rec, parts, "\r")
    if (k == 0) parts[k = 1] = ""
    for (j = 1; j <= k; j++) lines[++n] = parts[j]
}
END {
    if (bad) exit 3
    id = ENVIRON["SEAL_ID"]
    # A leading BOM is content-neutral: it never sits inside a span.
    if (substr(lines[1], 1, 3) == "\357\273\277") lines[1] = substr(lines[1], 4)

    fm_end = 0
    if (lines[1] ~ /^---[ \t]*$/ && lines[2] !~ /^[ \t]*$/) {
        for (i = 2; i <= n; i++) {
            if (lines[i] ~ /^(---|\.\.\.)[ \t]*$/) { fm_end = i; break }
        }
    }
    sp = 0
    fence = 0
    icode = 0
    pb = 1
    cont = 0
    amb = 0
    mention = 0
    badws = 0
    for (i = fm_end + 1; i <= n; i++) {
        L = lines[i]
        if (index(L, "loam:seal") && index(L, id)) mention = 1
        if ((index(L, "loam:") || index(L, "] Q: ")) && L ~ uws) badws = 1
        blank = (L ~ /^[ \t]*$/)
        ind = width(L)
        if (fence) {
            body = substr(L, lead(L))
            pb = 0
            if (ind <= 3 && run(body, fch) >= flen && trim(substr(body, run(body, fch) + 1)) == "") { fence = 0; pb = 1 }
            continue
        }
        if (blank) { pb = 1; continue }
        if (ind >= 4 && (pb || icode)) { icode = 1; pb = 0; if (cont) amb = 1; continue }
        icode = 0
        pb = 0
        body = substr(L, lead(L))
        c = substr(body, 1, 1)
        s2 = unprefix(L)
        b2 = substr(s2, lead(s2))
        c2 = substr(b2, 1, 1)
        fl = 0
        if (c2 == "`" || c2 == "~") {
            r2 = run(b2, c2)
            if (r2 >= 3 && !(c2 == "`" && index(substr(b2, r2 + 1), "`"))) fl = 1
        }
        if (fl && (pre || (ind >= 1 && cont))) amb = 1
        if (pre) cont = 1
        if (ind <= 3 && !pre && fl) {
            fence = 1; fch = c2; flen = r2
            continue
        }
        t2 = body
        gsub(/[ \t]/, "", t2)
        if (ind <= 3 && ((body ~ /^#+([ \t]|$)/ && run(body, "#") <= 6) || (length(t2) >= 3 && t2 ~ /^(-+|\*+|_+)$/) || t2 ~ /^(=+|-+)$/)) { pb = 1; continue }

        t = trim(L)
        if (substr(t, 1, 4) == "<!--" && substr(t, length(t) - 2) == "-->") pb = 1
        if (sp && question_anchor(L)) {
            for (s = sp; s >= 1; s--) if (sk_kind[s] == "seal") { sk_ex[s] = 1; break }
            continue
        }
        if (length(t) < 7 || substr(t, 1, 4) != "<!--" || substr(t, length(t) - 2) != "-->") continue
        inner = trim(substr(t, 5, length(t) - 7))
        if (inner == "") continue
        nw = split(inner, w, /[ \t]+/)
        head = w[1]
        closing = (substr(head, 1, 1) == "/")
        name = closing ? substr(head, 2) : head
        if (name == "loam:seal") kind = "seal"
        else if (name == "loam:class") kind = "class"
        else continue
        prefix = (kind == "seal") ? "s-" : "c-"

        if (!closing) {
            oid = (nw >= 2) ? w[2] : ""
            ex = 0
            if (valid_id(prefix, oid)) {
                if (++seen[oid] > 1) dup[oid] = 1
            } else ex = 1
            if (kind == "seal" && nw > 2) ex = 1
            if (kind == "class" && !(nw == 3 && (w[3] == "local-only" || w[3] == "project" || w[3] == "hub-promotable"))) ex = 1
            conflict = 0
            for (s = sp; s >= 1; s--) if (sk_kind[s] == "seal") { conflict = s; break }
            if (!conflict && kind == "class")
                for (s = sp; s >= 1; s--) if (sk_kind[s] == "class") { conflict = s; break }
            if (conflict) { sk_ex[conflict] = 1; ex = 1 }
            sp++
            sk_kind[sp] = kind; sk_id[sp] = oid; sk_start[sp] = i + 1; sk_ex[sp] = ex
        } else {
            if (sp == 0) continue
            if (sk_kind[sp] != kind) { sk_ex[sp] = 1; continue }
            ex = sk_ex[sp]
            if (nw > 1) ex = 1
            if (!ex && kind == "seal") { cstart[sk_id[sp]] = sk_start[sp]; cend[sk_id[sp]] = i }
            sp--
        }
    }
    if (badws) exit 5
    if (amb && mention) exit 6
    if (!(id in cstart) || (id in dup)) exit 4

    first = 0
    last = 0
    for (i = cstart[id]; i < cend[id]; i++) {
        s = lines[i]
        sub(/[ \t]+$/, "", s)
        lines[i] = s
        if (s != "") { if (!first) first = i; last = i }
    }
    for (i = first; first && i <= last; i++) printf "%s\n", lines[i]
}
' "$file"
}

report() {
    case $1 in
        3) echo "seal-canon: $file is not valid UTF-8" >&2 ;;
        4) echo "seal-canon: no well-formed seal span ^$anchor in $file" >&2 ;;
        5) echo "seal-canon: unsupported whitespace in a marker line of $file; use the loam runtime" >&2 ;;
        6) echo "seal-canon: ambiguous list or block-quote structure around ^$anchor in $file; use the loam runtime" >&2 ;;
        *) echo "seal-canon: awk failed on $file (status $1)" >&2 ;;
    esac
    exit 2
}

if [ "$digest" -eq 0 ]; then
    run_awk
    status=$?
    [ "$status" -eq 0 ] || report "$status"
    exit 0
fi

if command -v sha256sum >/dev/null 2>&1; then
    hasher="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    hasher="shasum -a 256"
else
    echo "seal-canon: --digest needs sha256sum or shasum" >&2
    exit 2
fi
tmp=$(mktemp "${TMPDIR:-/tmp}/loam-seal-canon.XXXXXX") || exit 2
trap 'rm -f "$tmp"' EXIT
trap 'exit 2' HUP INT TERM
run_awk >"$tmp"
status=$?
[ "$status" -eq 0 ] || report "$status"
# shellcheck disable=SC2086 # $hasher is a fixed, intentionally split command
sum=$($hasher <"$tmp") || {
    echo "seal-canon: $hasher failed" >&2
    exit 2
}
hex=${sum%% *}
case $hex in
    *[!0-9a-f]*) hex= ;;
esac
if [ "${#hex}" -ne 64 ]; then
    echo "seal-canon: $hasher gave no sha256 digest" >&2
    exit 2
fi
printf '%s\n' "$hex"
