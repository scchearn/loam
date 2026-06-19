---
description: Save a resumable checkpoint for the current work
---

Use the `loam::checkpointing` skill now and follow it exactly.

If the `loam::checkpointing` skill is unavailable in this workspace, tell the user that `/checkpoint` requires that skill and stop. Do not attempt to recreate the checkpoint workflow yourself.

You were invoked via `/checkpoint`.
Treat `$ARGUMENTS` as optional guidance for the intended next step when I return.
If `$ARGUMENTS` is empty, infer the current work context from this session and proceed with the normal checkpoint workflow.

Do not summarize this command file. Execute the checkpoint workflow.