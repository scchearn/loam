---
title: "Bad Frontmatter
tags: [unterminated, list
---

# Bad frontmatter

This page's YAML front matter is deliberately malformed: an unterminated
quoted string on `title` and an unterminated flow sequence on `tags`. Per
the snapshot spec, malformed front matter must not drop the artifact from
inventory -- it should still appear, with a `parse_errors` entry recorded.
