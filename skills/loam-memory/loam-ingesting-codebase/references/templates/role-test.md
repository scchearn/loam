# Role Template: test

Use this template for test and spec files. Captures what is being tested and which cases are covered — not the test code itself. Replace `<...>` placeholders with extracted content.

```md
---
source_path: <relative-path-from-codebase-root>
ingested_at: <YYYY-MM-DD>
---

# <TestSuiteName>

## Tests

<which module, function, or component is being tested>
e.g. Tests for `[[validate-token]]` utility.

## Coverage

<bullet list of test cases — describe what each case verifies, not the assertion code>
- <case: e.g. "returns user when token is valid and not expired">
- <case: e.g. "returns null when token is expired">
- <case: e.g. "returns null when token signature is tampered">
- <case: e.g. "throws on malformed token header">

## Dependencies

- [[<target-module-slug>]] — the module under test
- [[<dependency-slug>]] — test helpers, fixtures, or mocks
- <external-dependency> (external) — <e.g. test framework>
```

## Extraction notes

- **Name**: use the test suite name (describe block name, file name, or the module-under-test name + "Tests").
- **Tests**: the module/function/component being tested. Link to it as `[[slug]]`.
- **Coverage**: describe each test case in one line. Focus on what is verified, not how. Group similar cases if there are many (e.g. "5 cases for expired token variants" instead of listing all 5).
- **Dependencies**: the module under test (linked), test helpers, fixtures, mocks. Test framework (jest, vitest, pytest) is `(external)`.
- Do not reproduce test assertions or setup code. The coverage description is the value, not the implementation.