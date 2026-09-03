# Runtime, workspace state, and federation

Read when a skill needs a native command, must satisfy or surface a hint, or handles federation. Referenced from `loam::using`.

## Global installation boundary

Skills are installed once globally under `<home>/.agents/skills/` and are
authoritative; the current workspace owns only its memory, goals, specs,
plans, and checkpoint data. The native runtime binary lives in the config-dir
runtime store — `<config-root>/runtime/<version>/<target>/loam[.exe]`,
selected by `<config-root>/runtime/ledger.json` — never on `PATH` and never
inside a project. Do not install, update, or execute a project-local Loam
runtime or skill copy.

The injected `Native runtime command:` line is the only runtime command for
skills. Use its quoted absolute native executable prefix directly; on Windows
the value includes the PowerShell call operator and `.exe` path. The shared
Node integration entry is reserved for status; adapters own hook spawning.
`npx @scchearn/loam install` owns first install, repair, downloads, updates,
and migration; `setup` only configures an existing install. Startup and the
integration are read-only, network-free, and must not poll for updates.

## Runtime and workspace state

The shared integration invokes native `state --fast` and may emit advisory
`hints[]` with `maintenance` or `workflow` signals. Hints point at the
relevant loam skill; they never authorize bypassing that skill or auto-running
commands. Missing hints mean "no cheap signal," not "nothing to do."

When a fresh state block is needed, run the native state operation through the
injected native runtime command. `<native-runtime-command>` means the exact
quoted command shown in the `Native runtime command:` context line:

```text
<native-runtime-command> state --fast "$(pwd)"
```

The harness hook operation is reserved for adapters and session startup; skills
must not use it to resolve workspace state.

## Background code ingestion

Background code ingestion is enabled by default and boundary-triggered only.
Set `LOAM_INGEST_BACKGROUND=0` to opt out unconditionally, or set
`background_ingest.enabled` to `false` in global config;
`LOAM_INGEST_BACKGROUND=1` overrides a disabled config. Claude Stop, Codex
Stop, and OpenCode `session.idle` may start a same-harness worker; Cursor has no
idle worker. The worker acquires a workspace lease before probing, requires
full state and `code_ingest_pending.evidence.pending_count > 0`, resolves the
installed ingestion exclusions, and needs a complete actionable fingerprint.
Missing or unreadable exclusions, uncertain process identity, live workers,
and stat/read/deadline races fail closed. It never changes source files,
commits, or pushes. Inspect it with the installed integration's
`ingest-status --workspace <path> --json` command.

For native operations, invoke the same native command directly:

```text
<native-runtime-command> <native-loam-args>
```

## Native command surface

Skill-relevant subcommands (all run through `<native-runtime-command>`):

- `state [--fast] <workspace-root>` — workspace state JSON: `wiki_root`,
  qmd readiness, checkpoints, drift, signals. `--fast` skips expensive
  aggregation; prefer the injected block before re-running it.
- `lint [--only guidance|markdown|memory|work] [--fix] <workspace-root>` —
  four-domain lint; default runs all four, `--only` runs exactly one. `--fix`
  regenerates the `AGENTS.md` `loam:memory-map` region and writes nothing
  outside its markers.
- `datecheck <check|fix> <wiki-root> [--offset +HH:MM]` — timestamp lint and
  repair; canonical offsets are `±HH:MM`.
- `codegraph index <wiki-root> [--codebase-root <dir>]`, `walk
  <codebase-root>`, `diff <codebase-root> [<wiki-root>]` — build, list, and
  drift-check the code graph.
- `checkpoint state [--window <minutes>] [<workspace-root>]` — digest of
  recently touched files; `checkpoint verify <note.md>` validates a checkpoint
  note and always exits 0.

`hooks`, `hook`, and `check versions` are adapter, setup, and release
surfaces — not skill-callable state sources.

## Federation

The injected `## Federation` line is authoritative: `unenrolled`, `degraded
(reason)`, or `live · project · items`. Degraded means collaboration state is
unavailable this turn — the local context above it stays complete and current.
Do not probe the broker, and never fabricate federation state; teammate items
arrive as wake tips and are informational unless they request you.

To join a project on this machine (agent-initiated, user-approved):
`<native-runtime-command> federation connect <workspace>
mqtts://<broker-host>:<port> --project <org/project> --token-file <path>`.
The enrollment token comes from the broker operator — prefer `--token-file`;
never place tokens in shell history or committed files. Git `user.email` must
be set first. Read-only inventory: `federation list` and `federation status`
(add `--json` for machine-readable output). Refusals map to fixes:
`bad-token` → obtain a fresh token from the operator;
`git-identity-required` → set `git config --global user.email`;
`signer-unreachable` / `signer-timeout` → broker-side network problem,
report to the operator. `federation emit` and `inject` are connector/adapter
surfaces, not skill operations.

If the integration reports `Loam is unavailable` or does not provide real
state, you may run `npx @scchearn/loam install` to install or repair Loam.
Install is agent-initiated — if the recovery command is in your context, use
it. If install succeeds, continue the task. If install fails, retry once. If
it fails again, report the failure output to the user and stop — do not loop.
Never fabricate workspace state or hints and never fall back to a
project-local launcher.

## Reuse before probing

The injected `## Workspace state` block is a compact native-state result from
startup. Reuse it when its `Workspace` matches the current
workspace, it contains the fields the active skill needs, and no later
operation changed the relevant wiki, qmd, checkpoint, or metadata state. Do
not rerun the integration merely to rediscover the same state.

The `hcom:` line is the availability answer for the optional hcom integration
(agent messaging and delegation). Read it instead of probing for the binary —
no `which hcom`, no `hcom --version`, no equivalent. It is workspace-independent,
so it is present whether or not a wiki is. `not installed` means every
hcom-dependent branch takes its documented fallback; it is never an error and
never a reason to stop.

Run a fresh native command when the block is absent, belongs to another
workspace, lacks required fields, or relevant state changed after injection.
Run the native command directly when omitted checks such as date drift or
`code_ingest_pending` are required. When a skill performs a newer authoritative
check itself, use that result instead of rerunning the startup context.

The injected block uses these stable line forms; checkpoint and signal lines are optional:

```text
Workspace: <absolute workspace> · Probe: state --fast
Wiki: <absolute wiki root> · qmd: <ready|not installed> [· collection: <name>]
Wiki: none
hcom: <ready|not installed>
Checkpoints: <count> (latest: "<title>" — <captured_at>)
Signals:
- [loam:hint] <kind> — <message> [(<evidence key>: <value>, ...)] [→ <command>]
```

The native runtime command is resolved once and quoted for the host operating
system. Do not add a shell or PowerShell twin, a Node command proxy, or a
project-local runtime for runtime access.

## Consuming hints

After completing the primary task of any loam skill that consumed injected or freshly probed native state, scan its hints and surface unsatisfied hints to the user as suggested next actions. This is mandatory — hints that go unread are signals wasted.

For each hint, emit one Markdown list item in this form:

```text
- [loam:hint] <kind> — <message> [(<evidence summary>)] [→ <command>]
```

Rules:

- **`plan_reconcilable`** — aggregate workflow hint for completed plans with open acceptance criteria; route listed plans to `/loam::amending-plan`.
- **Suppress satisfied hints.** Skip any hint whose `kind` your skill body names as one it satisfies. A skill may satisfy more than one (e.g. `linting-memory` satisfies `memory_lint_stale`, `date_drift_pending`, `log_rotation_due`, and `legacy_structure_pending`); suppress all of them.
- **Only surface hints with a non-null `command`.** Hints without a command (e.g. `retrieval_not_ready`) are informational; mention them only if the user asks for state.
- **Do not auto-run the suggested skill.** Hints are advisory; the user decides whether to act. End your turn or hand back to the user after surfacing.
- **Empty `hints[]` → say nothing.** Do not invent suggestions or pad the report.
- **`evidence` summary.** When the hint's `evidence` object carries a count (e.g. `pending_count`, `drift_count`, `log_lines`, `age_minutes`), include it parenthetically: `- [loam:hint] code_ingest_pending — 3 source file(s) new or changed (pending_count: 3)`. Omit the parenthetical when `evidence` is empty.

This makes the integration state a closed loop: native state signals, the skill
acts, and the next-most-pressing signal surfaces to the user instead of dying
in JSON.
