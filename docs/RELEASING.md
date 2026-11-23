# Releasing loam

Loam has two independent release tracks. A plugin release and a native runtime
release may happen separately; do not bump both versions just to make the
numbers match.

## The two release tracks

| Track | Version surfaces | Tag | Published result |
| --- | --- | --- | --- |
| Plugin | `package.json`, `package-lock.json`, the Claude marketplace manifest, the Codex plugin manifest, and the Cursor plugin manifest | `v<version>` | npm package; prereleases use the `next` npm channel |
| Runtime | `cli/Cargo.toml`, the Loam entry in `Cargo.lock`, and `setup/constants.mjs` `RUNTIME_VERSION` | `cli-v<version>` | Raw per-platform binaries plus `loam-runtime-manifest.json` on a GitHub release |

The plugin track changes skills, setup, adapters, and integration behavior. The
runtime track changes the native binary. A plugin-only change leaves
`RUNTIME_VERSION` alone; a runtime-only change does not churn the plugin version.

Use the checked-in bump command. It edits only one track, refuses a dirty
working tree, verifies that every file in that track agrees before editing, and
checks the result afterward:

```bash
bin/bump-release.sh --plugin 1.0.0-next.24
bin/bump-release.sh --runtime 0.11.0-next.21
```

The versions above are examples. Choose the next version in the appropriate
track; do not copy them blindly.

## Version rules

- Use SemVer `MAJOR.MINOR.PATCH`, optionally followed by a prerelease such as
  `-next.0` or `-rc.1`. Build metadata (`+build`) is not accepted.
- Each track must agree with itself. The plugin files must carry one plugin
  version, and the runtime files must carry one runtime version. The two values
  do **not** need to be equal.
- Prereleases are ordered by their numeric suffix: `1.0.0-next.0` comes before
  `1.0.0-next.1`, and every `1.0.0-next.N` comes before stable `1.0.0`.
- A plugin tag containing `-` is published to npm with the `next` dist-tag. A
  plugin tag without `-` is published as `latest`. Runtime prereleases are
  marked as prereleases on GitHub.
- `RUNTIME_VERSION` must name a published `cli-v<version>` runtime before a
  package that depends on that value is offered as ready. If a runtime bump and
  a plugin bump are coordinated, publish and verify the runtime first, then
  publish the plugin that points to it.

## Required changelog work

Update [`CHANGELOG.md`](../CHANGELOG.md) before creating a release tag. The
changelog update is part of the release change, not a follow-up task.

1. Write the user-visible outcome and why it matters. Do not paste commit
   titles or internal task names.
2. For a release, move the shipped entries from `[Unreleased]` into a dated
   heading for the exact plugin or runtime version. If both tracks ship
   together, name both versions in the heading or its first paragraph.
3. Leave a new `[Unreleased]` heading at the top for work that has not shipped.

## Version gates

Run the gates that match the track, plus the full CI checks for a coordinated
release.

### Before bumping

The bump script needs the native checker. Build it once, then confirm the
working tree is clean:

```bash
cargo +1.94.1 build --release --workspace --locked
git status --short
```

`bin/bump-release.sh` refuses to run if the tree is dirty or if the selected
track already disagrees. It also refuses malformed versions, missing files, and
partial replacement counts.

### Plugin track

After `bin/bump-release.sh --plugin <version>` and the changelog edit:

```bash
npm ci
npm test
npm pack --dry-run
```

The plugin workflow checks that `package.json` matches the `v<version>` tag
before publishing. It publishes prereleases with `--tag next`, so a prerelease
cannot silently move the production `latest` channel.

### Runtime track

After `bin/bump-release.sh --runtime <version>` and the changelog edit:

```bash
cargo +1.94.1 fmt --all -- --check
cargo +1.94.1 clippy --locked --workspace --all-targets -- -D warnings
cargo +1.94.1 test --workspace --locked
cargo +1.94.1 build --release --workspace --locked
bin/check-release-resolution.sh --versions-only
```

The offline resolution check confirms that the runtime files agree and that
`RUNTIME_VERSION` can be read. After the runtime tag is published, run the full
network check:

```bash
bin/check-release-resolution.sh
```

That check fetches the published `cli-v<version>` manifest and verifies all
five supported targets are present. It should pass before a plugin release
points at a newly bumped runtime.

## Tag and publish

Review the complete diff, including `CHANGELOG.md`, then commit the release
change. Push the release tag that matches the track:

```bash
git tag v<plugin-version>
git push origin v<plugin-version>

git tag cli-v<runtime-version>
git push origin cli-v<runtime-version>
```

The `v*` tag starts the plugin publish workflow, which publishes to npm and
creates the repository's GitHub release; the plugin release owns the
"Latest" slot. The `cli-v*` tag starts the five-target runtime build, creates
the runtime's GitHub release (never marked latest), attaches the raw binaries
and manifest, and runs the published-resolution check. A coordinated
release may create the two tags in either order only when the plugin does not
point at an unpublished runtime; otherwise use the runtime-first order above.

## References

- [Plugin release workflow](../.github/workflows/plugin-release.yml)
- [Runtime release workflow](../.github/workflows/runtime-release.yml)
- [Release-resolution check](../bin/check-release-resolution.sh)
- [Release bump command](../bin/bump-release.sh)

[PR #123]: https://github.com/scchearn/loam/pull/123
