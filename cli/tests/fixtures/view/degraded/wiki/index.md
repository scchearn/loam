## Overview

Degraded fixture: the workspace itself is readable, but
`wiki/code/corrupt.md` contains invalid UTF-8 bytes inside a `.md` file,
so a codegraph/code-page probe reading it must fail or return malformed
data for that one entry while every other artifact stays available.

## Topics

- [[topics/status|Status]]
