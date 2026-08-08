# Tier-9 — systemd operational smoke

Manual/operational gate for the Slice B MQTT transport. It runs a real
Mosquitto broker as an **isolated, uniquely named transient systemd _user_
unit** and proves the operational properties the transport depends on on an
Arch-compatible host. It is not a CI job, not a package installer, and not part
of any automated gate.

## Prerequisites

- `mosquitto`, `mosquitto_passwd`, `mosquitto_pub`, `mosquitto_sub`, `openssl`
- `systemd-run` and a working `systemctl --user` session bus (linger enabled, or
  an active login session)

Each binary can be overridden with `LOAM_MOSQUITTO_BIN`,
`LOAM_MOSQUITTO_PASSWD_BIN`, `LOAM_MOSQUITTO_PUB_BIN`, `LOAM_MOSQUITTO_SUB_BIN`,
`LOAM_OPENSSL_BIN`. A missing prerequisite is reported as a **blocker (exit 2)**,
never a fabricated pass.

## Run

```sh
bash -n cli/tests/mqtt/tier-9-systemd-smoke.sh   # syntax check
cli/tests/mqtt/tier-9-systemd-smoke.sh           # execute on this host
```

Exit codes: `0` pass, `1` one or more checks failed (broker log preserved at
`$TMPDIR/loam-tier9-<run>.log`), `2` prerequisite blocker.

## What it proves

| # | Check | Observable |
|---|-------|------------|
| 1 | TLS certificate validity | server and CA certs pass `openssl x509 -checkend 0` |
| 2 | Transient user unit starts | `systemctl --user is-active` on a uniquely named `systemd-run` unit |
| 3 | Retained round-trip over TLS | a retained sentinel published and observed over the TLS listener |
| 4 | No anonymous listener | an unauthenticated publish is refused |
| 5 | Restart persistence | after `systemctl --user restart`, the sentinel persists and MainPID changed |
| 6 | Backup source is real | wiping the persistence DB and restarting loses the sentinel (not a vacuous copy) |
| 7 | Restore works | restoring the backed-up DB and restarting brings the sentinel back, with a fresh MainPID |

Restart evidence is a changed `MainPID`; backup/restore is non-vacuous because
the wiped-DB step proves the database is the sentinel's only source before the
restore step proves recovery.

## Isolation guarantees

- All broker state (certs, credentials, ACL, config, persistence, backup) lives
  under one `mktemp -d` directory, removed on exit.
- Never edits `/etc/mosquitto`, never enables a system-wide service, never
  overwrites an existing persistence database, never requires Docker.
- The transient unit is stopped and `reset-failed` on exit; the retained
  sentinel is cleared before teardown, and the namespace is a fresh
  `loam/v1/tier9-<run-id>/…` per run.
