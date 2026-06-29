# Output Contract

## Required outputs

The skill always produces a prepared debate plan. If the user approves running it, the convergence result is appended on-thread.

### Prepared debate plan (always produced)

- the goal or deliverable
- the tension axis and the positions being argued
- the agents (by tag or name), their assigned positions, and their role briefs
- the hub stance (neutral / partisan-for-synthesis / participant)
- the round count and the stopping rule
- the convergence prompt
- the forcing-field deliverable spec
- scope facts the debate must respect
- assumptions, if defaults were chosen
- the **exact first hcom action** that would fire if approved — a verbatim `hcom send` or spawn command, not a placeholder

If the skill cannot produce any of these, it has not finished gathering parameters — go back to the interview, do not emit a partial plan.

### Convergence result (only if approved)

- the forcing-field deliverable, produced by the hub or the synthesizer role
- reported on-thread with `--intent inform`

## Approval gate

The prepared plan is the proposal. Present it, then ask explicit approval to run. The gate is per-debate, not per-session.

- If approved: run the rounds, converge, report on-thread.
- If denied or absent: the prepared plan is the deliverable. Stop. Do not send, spawn, seed, or launch anything.

"No" means stop, not renegotiate. Renegotiation is a fresh invocation with new `$ARGUMENTS`.

## Defaulting rule

If one preference is missing but the tension axis and agent roster are still clear, choose the recommended default and label it as an assumption.

If the same model family is available from multiple providers, surface the top exact provider-qualified IDs and choose one explicit assumption.
If `opencode models` was not actually run, label provider-qualified IDs as assumptions or likely candidates instead of confirmed local availability.

Ask a follow-up only when the missing information would materially change the tension axis, agent roster, convergence rule, or forcing-field deliverable.

## Formatting

Place the prepared debate plan under a literal heading line `## Prepared debate plan`, followed by the plan block. Place the approval prompt under a literal heading line `## Approval gate`. If approved, place the convergence result under `## Convergence result`.

## Forbidden substitutions

Do not replace the prepared debate plan with:

- prose-only design notes without the exact first hcom action
- a saved `agents/<slug>.toml` or `agents/<slug>.md` — that contract is retired
- shell scripts (unless the user explicitly asked for executable automation, and even then only alongside the prepared plan)
- repo setup tasks
- build output
- implementation claims

Do not emit `agents/<slug>` artifacts. The saved-config contract is retired.

## Retired contract

The previous output contract produced `agents/<slug>.toml` and `agents/<slug>.md` plus a saved-config loader. That contract is retired. Do not generate those artifacts, do not provide loader instructions, and do not treat a request for saved configs as a request for this skill. Compose the debate inline instead.

## Scope control

The prepared plan is a debate/consensus artifact. The convergence result is the debate output. Neither may claim the project work has already been done. The skill runs a debate about the goal; it does not implement the goal.