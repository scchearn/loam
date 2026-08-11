# Federation Test Runbook — laptop ↔ MacBook

Goal: install the federation preview on both your machines, join them to one
project, and watch collaboration state cross between them ("Samuel Hearn did X").

Release line: **plugin 0.13.0-next.1 / runtime cli-v0.11.0-next.0** on the npm
`next` dist-tag (prerelease, off the stable line). Fully reversible.

---

## 0. Prerequisites (both machines)

- `git`, `node`, `npx` on PATH.
- Set your Git identity so both machines read as the same person:

  ```
  git config --global user.email scchearn@gmail.com
  git config --global user.name "Samuel Hearn"
  ```

- Start from a clean loam state. If loam is already installed and you hit a
  version mismatch, uninstall first: `npx --yes @scchearn/loam uninstall --yes`.

## 1. Install the federation client — run on BOTH machines

```
npx --yes @scchearn/loam@next setup --yes
```

That's the whole install: the `@next` package pins its own skills tag and its
own runtime. `latest` (production 0.12.0) is never touched.

**Verify** (must list connect/disconnect/status/emit):

```
loam federation --help
```

If it does NOT show `federation`, you installed over an existing loam and hit
the runtime/skills mismatch — uninstall (`npx --yes @scchearn/loam uninstall --yes`)
and re-run step 1 on the clean slate.

## 2. Stand up the broker (once, from your laptop)

The broker on `mqtt.aenon.io` is authored + dry-run-green but NOT yet deployed.
`pine` deploys it on your DIRECT go. From THIS laptop session, paste:

```
! hcom send @pine --name bigboss --intent request -- "Owner direct go. Finish the broker deploy per the OWNER-RUNBOOK — recon (host paths + both clients on @next, plugin 0.13.0-next.1 / runtime 0.11.0-next.0) first, stop and report on any divergence, then deploy + health/postflight. You are authorized to touch the box. Go now."
```

pine will: read-only recon → deploy (Cloudflare DNS → certbot cert → mosquitto +
org-CA → **store your laptop cert in secret-tool** → write the peer roster → run
`loam federation connect` on the laptop) → verify Matrix/Invoice-Ninja undisturbed.
It also produces a short **MacBook import runbook** (cert → Keychain) — run that on
the MacBook after step 1 there.

## 3. Connect the MacBook

After the broker is live and pine hands you the MacBook steps: on the MacBook,
import your client cert into the Keychain (pine's `security add-generic-password`
line), then `loam federation connect` that workspace into the same project.

## 4. Run it — what to DO

- Open a coding session (Claude Code / Codex / OpenCode) in the enrolled workspace
  on **machine A**.
- Do something that produces federation state — e.g. `loam federation emit` a
  `work.report` (state change) or a `message.reply`, or just work a turn.
- Open a session in the enrolled workspace on **machine B**.

---

## What you SHOULD see in the terminal

**On session start (both machines), injected into the model's context** — a
`<LOAM_IMPORTANT>` block ending with a `## Federation` section:

- Enrolled + broker reachable → your project's live collaboration snapshot
  (inbox items, colleagues' work-state), each item **sender-attributed** e.g.
  `- from Samuel Hearn <scchearn@gmail.com> · io.loam.work.state ready · …`
- Enrolled but broker down → `federation: degraded (connector_unreachable)` with
  the local baseline still present (never blocks the session).
- Not enrolled → `federation: unenrolled — run 'loam federation connect'`.

**When you work on machine A** (emit / a state change): A's connector publishes
that state to the broker.

**On machine B's next session-start or prompt turn**: B's hook pulls the new
state and injects it — you'll see A's activity attributed to **"Samuel Hearn"**
(the display name, cert-bound so it can't be spoofed), rendered as trusted-but-
inert text. That is the "employee B did something" you wanted, crossing machines.

**Work-claim trust marker**: a colleague's `work.state` shows as
`unverified — sender claim, not reconciled against Git` unless the connector has
reconciled it against Git (the `publication: verified` stamp). No stale claim is
ever shown as "current".

## What should NOT trigger (by design)

- **The read/hook path never publishes.** Just viewing context, opening a
  session, or seeing reply-shaped text triggers **zero** broker writes and
  **zero** actions — no tool calls, no commands, no fetches, worktree unchanged.
- **Injection is inert.** Hostile/unknown payloads render sanitized and
  attributed; they can enter model context but drive nothing and cannot forge
  Loam's own framing.
- **A duplicate** (QoS-1 redelivery) shows as **one** logical item, not two.
- **No auto-response.** The other machine seeing your message does not auto-reply;
  only an explicit `loam federation emit` sends anything.

## Known limits for this test

- The **per-machine origin isolation** (ACL `%c` on client-id) can only be proven
  against the **real broker** — the local test fixture couldn't. Your laptop↔Mac
  run against `mqtt.aenon.io` is where that final security property gets exercised.
- This is a **preview** build (0.13.0-next.1), the `federation` branch tip with
  `main` merged in — nothing missing from main, plus everything federation.
  Only the federation feature set differs from stable.

## Rollback (back to stable when done)

```
npx --yes @scchearn/loam uninstall --yes && npx --yes @scchearn/loam setup --yes
```

This restores the current **stable** loam (plugin 0.12.0 / runtime 0.10.0) —
no matter how many `-next.N` versions you cycled through.
