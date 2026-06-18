# Add Checklist

Use this checklist before marking an add pass complete.

## Before editing

- Confirm the target source is local and unambiguous (file mode), or confirm the conversation topic is clear (chat-context mode).
- If chat-context mode: confirm the synthesized summary is faithful to what was discussed. Flag any points that were mentioned but not confirmed.
- Confirm the wiki root and schema.
- Read `index.md`.
- Read the latest relevant log entries.
- Read related topic, entity, and concept pages.

## During ingest

- Synthesize source content directly into the most relevant topic, entity, concept, or analysis pages.
  - File mode: note the source path in relevant pages where it materially aids retrieval.
  - Chat-context mode: mark conversation-sourced claims with appropriate uncertainty (discussed, suggested, agreed in conversation).
- Create a new entity or concept page only when it is important enough to reuse.
- Only touch pages materially affected by the source.
- Create a new entity or concept page only when it is important enough to reuse.
- Preserve uncertainty.
  - File mode: distinguish facts from claims in the source.
  - Chat-context mode: mark conversation-sourced claims with appropriate uncertainty (discussed, suggested, agreed in conversation).
- Make contradictions explicit instead of silently flattening them.

## Before finishing

- Update `index.md`.
- Append `log.md`.
  - File mode: `## [YYYY-MM-DD] add (file) | <source title>`
  - Chat-context mode: `## [YYYY-MM-DD] add (chat) | <topic>`
- Check that every touched durable page is discoverable from the index or inbound links.
- Confirm raw-source files were not modified.

## Good add outcomes

- The wiki is richer than before.
- The path from raw source to synthesis is traceable through log entries and relevant page references.
- A future session can answer related questions faster by reading memory first.
- In chat-context mode: uncertainty is explicit, not overstated.