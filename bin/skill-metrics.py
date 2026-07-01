#!/usr/bin/env python3
"""Emit per-skill metrics as TSV to stdout.

Input: list of SKILL.md paths as argv (or one path per line on stdin).
Output rows: name<TAB>desc_chars<TAB>desc_tokens<TAB>body_lines<TAB>body_tokens

Token counts use tiktoken's cl100k_base encoding (the de facto standard
for "tokens" in the agentskills.io spec context, since the spec doesn't
mandate a specific tokenizer).
"""

import sys
import re
import tiktoken

_ENC = tiktoken.get_encoding("cl100k_base")

_FM_FENCE = re.compile(r"^---\s*$")
_NAME = re.compile(r"^name:\s*(.*)$")
_DESC = re.compile(r"^description:\s*(.*)$")


def _strip_quotes(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == '"' and value[-1] == '"':
        value = value[1:-1]
    # YAML escapes \" -> "
    return value.replace('\\"', '"')


def parse_skill(path: str) -> tuple[str, str, str]:
    """Return (name, description, body_text) for a SKILL.md path."""
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()

    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        # No frontmatter; everything is body
        return "", "", text

    name = ""
    desc = ""
    i = 1
    body_start = len(lines)
    while i < len(lines):
        if _FM_FENCE.match(lines[i]):
            body_start = i + 1
            break
        m = _NAME.match(lines[i])
        if m:
            name = _strip_quotes(m.group(1))
        m = _DESC.match(lines[i])
        if m:
            # description might be single-line; handle multi-line only if value is empty
            # (we don't support multi-line YAML scalars here — loam skills use single-line quoted)
            desc = _strip_quotes(m.group(1))
        i += 1

    if not name:
        # Fallback to parent directory name
        parts = path.rstrip("/").split("/")
        if parts[-1] == "SKILL.md" and len(parts) >= 2:
            name = parts[-2]

    body = "\n".join(lines[body_start:])
    return name, desc, body


def count_tokens(text: str) -> int:
    return len(_ENC.encode(text))


def main() -> int:
    paths: list[str] = []
    if len(sys.argv) > 1:
        paths = sys.argv[1:]
    else:
        paths = [line.strip() for line in sys.stdin if line.strip()]

    for path in paths:
        name, desc, body = parse_skill(path)
        desc_chars = len(desc)
        desc_tokens = count_tokens(desc)
        # Body lines: count non-empty content lines (matches the awk version)
        body_lines = len(body.splitlines())
        body_tokens = count_tokens(body)
        print(f"{name}\t{desc_chars}\t{desc_tokens}\t{body_lines}\t{body_tokens}")
    return 0


if __name__ == "__main__":
    sys.exit(main())