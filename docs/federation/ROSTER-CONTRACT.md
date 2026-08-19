# Peer roster contract

The peer roster tells a connector which authenticated principals and machine
instances it may admit for one organization/project. The broker ACL controls
what can be published; the roster is the application-level allow-list used
when received frames are processed.

The optional provisioning helper,
[`provision-peer-roster.sh`](../../deploy/mqtt-broker/provision-peer-roster.sh),
writes the file. The runtime reads it as a separate file, not as an
enrollment-descriptor field.

## Location

The runtime reads one file at:

```text
<roster-root>/<org_id>/<project_id>.json
```

The runtime resolves `<roster-root>` in this order:

1. `LOAM_FEDERATION_ROSTER_DIR`, when set;
2. the federation profile under `LOAM_CONFIG_DIR`, the platform config
   directory, or the legacy profile;
3. the legacy `LOAM_HOME`/`~/.agents/loam` profile.

The `org_id` and `project_id` arguments used for this lookup are checked as
single ordinary path components before they are joined to the root. That is
path-traversal protection for the lookup path; the runtime does **not** compare
those arguments with same-named fields inside the JSON document.

On a current Linux install, the normal profile is
`$HOME/.config/loam/federation`, so the default runtime path is usually:

```text
$HOME/.config/loam/federation/rosters/<org_id>/<project_id>.json
```

The helper script historically defaults to
`$HOME/.agents/loam/federation/rosters`. Set
`LOAM_FEDERATION_ROSTER_DIR` explicitly when using the helper so its output
lands where the runtime reads.

## Runtime file admission

The enforced file reader is `read_roster` in
[`cli/src/provisioning.rs`](../../cli/src/provisioning.rs). It requires:

- a JSON object;
- `principals` and `origins` fields whose values are arrays;
- every array member to be a JSON string that is non-empty after surrounding
  whitespace is trimmed; and
- no duplicate object keys.

After trimming, principal and origin values are opaque strings. The runtime
does not enforce an email grammar, an instance-id length or alphabet, a URN
prefix, a case rule, or a relationship to `version`, `org_id`, or
`project_id`. It does not add or remove a prefix. Unknown document fields are
allowed, and `version`, `org_id`, and `project_id` are not required by the
runtime reader.

This is a valid runtime shape (the extra fields are useful to the helper, but
are not runtime admission requirements):

```json
{
  "version": 1,
  "org_id": "example-org",
  "project_id": "loam",
  "principals": ["person@example.org"],
  "origins": ["machine-a"]
}
```

The runtime wildcard check is equality-only. After trimming, it rejects these
three exact bare tokens in either array:

```text
*
#
+
```

It does not reject `**`, embedded wildcard characters, wildcard-looking
prefixes, or URN prefixes merely for looking like them. For example,
`sam+loam@example.test`, `urn:loam:instance:machine-a`, and `**` are not
rejected by this specific runtime wildcard check when the rest of the roster
is valid. Use concrete values that match the broker's actual topic identities;
the reader will not normalize mismatched forms for the operator.

The runtime `write_roster` path, used for broker-served membership payloads,
applies these same array, non-empty-string, duplicate-key, and exact-wildcard
checks before atomically writing a roster. It additionally requires both arrays
to be non-empty for that write. Neither runtime path enforces the document's
`version`, `org_id`, or `project_id` fields.

## Runtime outcomes and self-only fallback

`read_roster` reports distinct lower-layer reasons:

| Input | `read_roster` result | `resolve` behavior |
| --- | --- | --- |
| File absent or unreadable | `roster-absent` | Convert to a self-only roster. |
| Both arrays empty | `roster-empty` | Convert to a self-only roster. |
| Principals non-empty, origins empty | `roster-no-origins` | Convert to a self-only roster. |
| Origins non-empty, principals empty | `roster-no-principals` | Convert to a self-only roster. |
| Malformed JSON/object, missing or wrong-shaped arrays, duplicate keys, a non-string member, or a member empty after trimming | `roster-malformed` | Return `no-peer-roster`; do not open the project session. |
| A bare `*`, `#`, or `+` after trimming | `roster-wildcard` | Return `no-peer-roster`; do not open the project session. |
| Both arrays non-empty and otherwise valid | `PeerRoster` | Use the trimmed strings as the allow-list. |

The self-only conversion is in `resolve`, not in `read_roster`: it uses the
enrolled certificate subject common name as the principal and the enrolled
machine's instance id as the origin. Thus an absent, empty, or one-sided roster
does not broaden access, and a malformed or bare-wildcard roster remains a
hard refusal.

## Helper validation is a separate check

`provision-peer-roster.sh validate` is a convenience shape check, not the
runtime admission validator. It currently requires `jq`, a present file, a
truthy `.version`, string `.org_id` and `.project_id` fields, and array-shaped
`principals` and `origins`. It also requires the combined array lengths to be
greater than zero. Its wildcard helper rejects raw `""`, `"*"`, and `"**"`
entries, but does not reject `"+"` or `"#"`, does not trim before comparing,
does not require both arrays to be populated, and does not require array
members to be strings. It does not compare the document scope fields with the
path.

The helper's `write` command emits `version: 1`, the supplied scope fields,
and arrays made from comma-separated values (dropping empty CSV fields), then
runs that helper validation. It can therefore write a one-sided roster that
the helper accepts but `read_roster` reports as `roster-no-origins` or
`roster-no-principals`; `resolve` then uses the self-only fallback described
above. A helper `roster OK` message is not proof that the connector will admit
peers.

For a peer roster intended to admit colleagues, populate both arrays with
non-empty strings and avoid the runtime's exact bare `*`, `#`, and `+` tokens.
Use the same concrete origin strings that the broker puts in topic paths. For
example:

```sh
export LOAM_FEDERATION_ROSTER_DIR="$HOME/.config/loam/federation/rosters"
./provision-peer-roster.sh write \
  example-org loam \
  person@example.org \
  machine-a
./provision-peer-roster.sh validate \
  "$LOAM_FEDERATION_ROSTER_DIR/example-org/loam.json"
```

For multiple machines, pass comma-separated values to both list arguments:

```sh
./provision-peer-roster.sh write \
  example-org loam \
  person@example.org,other@example.org \
  machine-a,machine-b
```

## Identifier wire convention

The deployment convention uses two forms of an instance identifier, but the
roster reader does not enforce the form:

| Context | Usual form |
| --- | --- |
| Certificate SAN and envelope `source` | `urn:loam:instance:<instance_id>` |
| `data.from.instance_id`, MQTT topic origin, and roster `origins` | bare `<instance_id>` |

The runtime trims surrounding whitespace and otherwise compares the roster
strings as supplied. A prefixed roster origin is therefore not converted to a
bare topic origin. See [INSTANCE-ID-CONTRACT.md](INSTANCE-ID-CONTRACT.md) for
the deployment-wide identifier convention and
[IDENTITY-CONTRACT.md](IDENTITY-CONTRACT.md) for the certificate subject.

## Two-machine example

For one project used by two machines, list the principals and both concrete
origins:

```json
{
  "version": 1,
  "org_id": "example-org",
  "project_id": "loam",
  "principals": ["person@example.org"],
  "origins": ["machine-a", "machine-b"]
}
```

The checked-in
[`peer-roster.example.json`](../../deploy/mqtt-broker/peer-roster.example.json)
is a placeholder only; replace every value before using it.
