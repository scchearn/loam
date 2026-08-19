# Loam View

Loam View is a local, read-only window onto one loam workspace. It runs
`loam state --view` once, serves the resulting snapshot on loopback, and
renders five areas over it:

| Area | Question it answers |
| --- | --- |
| Pulse | What needs attention now, with the evidence behind it |
| Atlas | How code, concepts, work, and memory fit together |
| Work Stream | Why work exists, how far it has progressed, what it touches |
| Chronicle | How project understanding changed, event by event |
| Stewardship | Whether this memory can be trusted, and what to fix first |

A Reader opens any inventoried document, an Inspector explains any single
artifact, and a Query palette (`Ctrl`/`Cmd` + `K`) searches memory, code, and
work.

## Requirements

- **Node.js >= 22.** The launcher refuses to start on anything older and says so.
- **The installed native loam runtime.** View shells out to it for every
  snapshot. If it is missing, the launcher fails with
  `Recovery: npx @scchearn/loam setup`.

## Opening it

### Through your agent harness (primary)

Ask for it in plain language — "open loam view", "show me the project view" —
and the **Using** router takes it from there: the agent background-spawns the
launcher, reads the URL off its first line of output, and hands you the URL.
The harness owns the child process; closing the session ends the server.

### From a terminal (fallback)

```bash
npx @scchearn/loam view [workspace-root]
```

With no argument it uses the current directory as the workspace root. The
process stays in the foreground and opens your browser. `Ctrl-C` stops it.

Flags:

| Flag | Effect |
| --- | --- |
| `--no-open` | Serve, print the URL, and skip the browser opener |

## The background-spawn pattern

This is how a harness agent should open View, and why each step exists.

1. **Spawn the launcher in the background with `--no-open`.** View runs in the
   foreground until interrupted, so a blocking call never returns. `--no-open`
   matters because a headless agent has no browser to hand the URL to.
2. **Capture the URL line.** The launcher writes
   `Loam View: http://127.0.0.1:<port>/` to stdout as its *first* output, before
   any browser-open attempt, precisely so it can be parsed without waiting.
3. **Report the URL to the human.** They open it themselves.
4. **Leave the process alone.** The harness owns its lifecycle. View never
   daemonizes and never writes a pid file; when the session ends, so does the
   server.

The port is always ephemeral (the launcher listens on `0`), so never hardcode
one — read it from the URL line every time.

## Boundaries

These are contract, not configuration. None of them can be turned off.

- **Read-only.** The HTTP surface is `GET /api/snapshot`,
  `POST /api/refresh`, `GET /api/document`, and `GET /api/search`. `refresh` is
  a POST only because it re-runs the producer; nothing in View writes to your
  workspace. There is no edit affordance anywhere in the UI.
- **Loopback only.** The server binds `127.0.0.1` on an ephemeral port. It is
  not reachable from another machine, and there is no flag to make it so.
- **No export.** There is no download, no "export report", no file the page can
  hand you. Everything stays in the browser.
- **Bounded to one workspace.** Document reads are resolved against the
  workspace root and rejected if they escape it.

## Refresh, and what happens when it fails

`Refresh` re-runs `loam state --view` and re-reads the snapshot. If the producer
fails, **the previous snapshot stays on screen** and the error is surfaced in a
live region above the view — you never lose your render because a refresh went
wrong. The message names what failed, when the snapshot you are looking at was
taken, and what to do; the freshness chip marks itself stale so nothing on
screen reads as current. The keyboard stays on the Refresh control throughout.
The message clears on the next successful read, not before.

## Keyboard and assistive technology

- Every control is reachable by `Tab`, with a visible focus ring that is never
  covered by the chrome — including the bottom rail on narrow viewports.
- "Skip to content" is the first tab stop and jumps to the workspace region
  without changing which area you are in.
- `Ctrl`/`Cmd` + `K` opens Query from anywhere; `Escape` closes it. Results are
  walked with the arrow keys and chosen with `Enter` — the input keeps focus and
  points at the active option, so results are not separate tab stops.
- Opening the Inspector or Reader makes everything behind it `inert` — the skip
  link included, since it lives inside the shell — moves focus in, and returns
  focus to the control you came from on close, including when leaving the view
  re-renders that control.
- Severity is never carried by colour alone; every state has a text badge.
- Atlas ships a list beside the graph. The list is the accessible interface and
  carries the same nodes, kinds, and relationship counts.
- Reader wikilinks resolve the way loam writes them: as given, relative to the
  linking document, or relative to the wiki root. A target that matches more
  than one artifact stays unresolved and is labelled as such — ambiguity is a
  diagnostic, never a guess.
- `prefers-reduced-motion: reduce` is honoured.

## Empty and degraded workspaces

View never fakes completeness. A workspace with no wiki, no goals, or no
recorded events says exactly what is missing and names the loam command that
would fix it, rather than rendering an empty panel or inventing a number. A
missing metric reads `—`, not `0`.

"Nothing was read" and "nothing is there" are different claims, and View keeps
them apart. If the snapshot cannot be read at all, the areas are not rendered:
one panel says so and points at Refresh. An area's own empty state only ever
speaks for a workspace that was actually read.

## Development

To drive View from a checkout before the runtime it builds is installed
globally, point `LOAM_NATIVE_BIN` at an absolute path to a `loam` binary:

```bash
LOAM_NATIVE_BIN=/abs/path/to/loam node view/launch.mjs <workspace-root> --no-open
```

Static assets are served with immutable caching, so restart the server after
changing anything under `view/public/`. It will come up on a new port.
