## Overview

Malformed fixture: exercises the `artifact-parse` signal. One wiki topic
page has unparseable YAML front matter; one goal has malformed timestamp
fields. Both artifacts must still appear in inventory with a recorded
`parse_errors` entry rather than being dropped.

## Topics

- [[topics/bad-frontmatter|Bad frontmatter]]
