# Date and Time Formats — Canonical Reference

This file is the single source of truth for how loam writes dates and times.
Every skill that writes or reads a timestamp points here instead of inlining
its own format.

## Design principles

1. **Daily-granularity surfaces stay TZ-free.** A log heading records which day
   a thing happened — a few hours of offset doesn't change the day for the
   author, and the day is the unit. Adding TZ noise to `## [2026-06-26]` buys
   nothing.

2. **Point-in-time surfaces carry a timezone offset.** `created_at`, `Captured:`,
   `started_at` — these record *when* something happened to the minute. A
   collaborator in another timezone needs the offset to interpret the time.
   ISO 8601 numeric offset (`+02:00`) is unambiguous and machine-parseable;
   local labels (`SAST`, `GMT+2`) are not.

3. **Epoch surfaces are TZ-independent by definition.** Unix epoch seconds
   are absolute — no offset needed, no offset wanted.

4. **Filenames stay local-time for sortability.** The offset lives in the
   body, not the filename.

5. **One separator for decisions log entries: em-dash (`—`).** No hyphen-minus,
   no colon, no time component. The date is the unit.

## Format table

| Surface | Format | TZ? | Example | Shell one-liner |
|---------|--------|-----|---------|-----------------|
| Log entry heading | `YYYY-MM-DD` | no | `## [2026-06-26] add (file) \| ...` | `date '+%Y-%m-%d'` |
| Log archive filename | `YYYY-MM` | no | `log-archive/2026-06.md` | `date '+%Y-%m'` |
| Checkpoint filename | `YYYY-MM-DD-HHMM` | no (local) | `checkpoint-2026-06-26-1107.md` | `date '+%Y-%m-%d-%H%M'` |
| Checkpoint `Captured:` body | `YYYY-MM-DD HH:MM ±HH:MM` | **yes** | `2026-06-26 11:07 +02:00` | `date '+%Y-%m-%d %H:%M %z'` |
| Spec/plan front matter | `YYYY-MM-DD HH:MM ±HH:MM` | **yes** | `2026-06-26 11:07 +02:00` | `date '+%Y-%m-%d %H:%M %z'` |
| `ingested_at` | Unix epoch seconds | n/a | `1719407600` | `stat -c %Y file` (Linux) / `stat -f %m file` (macOS) |
| `last_verified` | `YYYY-MM-DD` | no | `2026-06-26` | `date '+%Y-%m-%d'` |
| Decisions log entry | `YYYY-MM-DD — <text>` | no | `2026-06-26 — decided...` | `date '+%Y-%m-%d'` |
| Inline body dates | `YYYY-MM-DD` | no | `... on 2026-06-26 ...` | `date '+%Y-%m-%d'` |
| hcom thread suffix | `YYYYMMDDHHMMSS` | no | `20260626110700` | `date '+%Y%m%d%H%M%S'` |
| Vault registration | epoch ms | n/a | `1719407600000` | `date '+%s%3N'` |

## Front matter fields

Spec and plan front matter uses `YYYY-MM-DD HH:MM ±HH:MM` for all
point-in-time fields:

```yaml
created_at: 2026-06-26 11:07 +02:00
updated_at: 2026-06-26 14:30 +02:00
approved_at: 2026-06-26 16:00 +02:00  # or null when draft
started_at: 2026-06-26 09:00 +02:00   # or null
completed_at: 2026-06-26 17:30 +02:00  # or null
```

`null` is valid for `approved_at`, `started_at`, and `completed_at` when
the state hasn't been reached yet.

## Checkpoint `Captured:` field

```markdown
- Captured: 2026-06-26 11:07 +02:00
```

The offset is mandatory. `SAST`, `GMT+2`, and other named labels are
legacy. The native datecheck command normalizes them to numeric offsets.

## Decisions log entries

```markdown
- 2026-06-26 — Decided to use SQLite for the activity log.
```

Separator is always em-dash (`—`, U+2014). Not hyphen-minus (`-`), not colon
(`:`), and no time component — the date is the unit.

## `ingested_at` on code entity pages

```yaml
ingested_at: 1719407600
```

Unix epoch seconds from the source file's mtime. This is a numeric field,
not a date string. The sync gate compares `file.mtime > ingested_at`
numerically; date strings break this comparison.

Legacy pages with `ingested_at: 2026-06-24` (date-only) are stale and
migrate to epoch on the next `loam::syncing-code-graph` run.

## Enforcement

`<native-runtime-command> datecheck check <wiki-root>`
scans all `*.md` files for drift from these formats and reports findings as
JSON. `loam::linting-memory` calls this during its health check. Use
`<native-runtime-command> datecheck fix <wiki-root> --offset <local-offset>` only after approval;
normalization is idempotent and skips already-canonical values.
