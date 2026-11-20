# Date format drift

Detail for the `### Check date format drift` step of `loam::linting-memory`.

Run `datecheck` to scan all markdown files for date-format drift:

```bash
<native-runtime-command> datecheck check "$WIKI_ROOT"
```

The script reports drift as JSON: front matter point-in-time fields missing timezone offsets, legacy TZ labels (`SAST`, `GMT+N`, `UTC`), and decisions-log entries using non-em-dash separators.

Canonical formats are defined in `loam-using/references/date-formats.md`.

If drift is found:
1. Run `<native-runtime-command> datecheck fix "$WIKI_ROOT"`. `--offset` is optional and defaults to the machine's local offset, so no human input is needed.
2. Re-run `datecheck check "$WIKI_ROOT"` to confirm zero drift.
3. Record it in `<wiki root>/log.md` and report under "Fixed now".

`check` never writes. `fix` is idempotent and loses no information — run it directly, never report date drift as needing approval.
