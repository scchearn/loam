# Ingestion Exclusions

Applied during the `walk` phase to filter the codebase tree. The ingestion skill loads this file and excludes any path matching a pattern below. Files not matched by any exclusion and whose extension is in the include list become ingestion candidates.

The helper also applies low-signal filters before emitting candidates: `.gitignore` ignored paths when Git is available, zero-byte files, whitespace-only files, binary/non-text files, likely generated files by header (`generated`, `auto-generated`, `do not edit`, `@generated`, `Code generated`, `This file was generated`), and files over the default large-file guard.

## Format

- One glob per line.
- `#` starts a comment.
- Patterns match against the full path relative to the codebase root.
- `*` matches within a path segment; `**` matches across segments.

## Always exclude

### Build outputs

```
**/dist/**
**/build/**
**/out/**
**/target/**
**/bin/**
**/obj/**
**/__pycache__/**
**/.next/**
**/.nuxt/**
**/.cache/**
```

### Dependencies

```
**/node_modules/**
**/vendor/**
**/.venv/**
**/venv/**
**/Pods/**
**/.gradle/**
```

### Lock files

```
package-lock.json
yarn.lock
pnpm-lock.yaml
Gemfile.lock
go.sum
Cargo.lock
poetry.lock
uv.lock
bun.lockb
```

### Config and metadata noise

```
.git/**
.github/**
.gitignore
.env*
.eslintrc*
.prettierrc*
tsconfig.json
jsconfig.json
*.config.js
*.config.ts
*.config.mjs
*.config.cjs
webpack.config.*
vite.config.*
rollup.config.*
babel.config.*
jest.config.*
vitest.config.*
Makefile
CMakeLists.txt
Dockerfile
docker-compose*
```

### OS and editor noise

```
.DS_Store
.vscode/**
.idea/**
*.swp
*.swo
*~
```

### Minified and generated

```
*.min.js
*.min.css
*.generated.*
*.gen.*
```

### Loam and wiki artifacts (never ingest the wiki into itself)

```
wiki/**
.wiki-metadata.json
.claude-plugin/**
.opencode/**
.claude/**
```

## Include by extension

Files with these extensions are ingestion candidates (after exclusions pass):

```
.ts .tsx .js .jsx .mjs .cjs
.py
.java
.go
.rb
.rs
.c .cpp .cc .h .hpp .hh
.cs
.php
.swift
.kt .kts
.scala
.sql
.graphql .gql
.proto
.sh
```

## Notes

- If unsure whether a file is code or config, include it. Downstream summarization handles misclassification gracefully (the role template captures whatever is there).
- SQL migrations and GraphQL schemas are code — include them.
- Generated code (e.g. `*.generated.*`) is excluded by default. If generated code is semantically important (e.g. generated protobuf types that other code depends on), the user can override by removing the pattern from their local exclusions or ingesting those files explicitly.
- Git ignore: when the codebase root is inside a Git worktree, ignored files are skipped by default. Use `--no-gitignore` only when intentionally ingesting ignored local source.
- Empty or whitespace-only files are skipped before classification. Non-empty package markers, barrel files, and re-export files remain candidates because they can describe meaningful graph edges.
- Monorepo roots: walk all sub-projects. The 200-file cap handles large monorepos; the user re-invokes to continue.
