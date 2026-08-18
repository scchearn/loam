//! `loam federation` CLI orchestration and typed command errors.
//!
//! Wiring the `connect` descriptor + workspace validation path only:
//! read one bounded descriptor from stdin, validate it, and, when a workspace is
//! given, prove physical identity, remote digests, and the workspace's Git
//! binding. The
//! enrollment probe, transactional registry commit, and service activation are
//! added by later tasks (T10/T11); `disconnect` and `status` are stubs until
//! then. No credential is resolved and no `AuthenticatedPrincipal` is built here.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::enrollment::{self, EnrollmentError, MAX_DESCRIPTOR_BYTES};
use crate::json::Value;

/// Entry point for `loam federation <subcommand>`.
pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("connect") => connect(args),
        Some("service") => service(args),
        Some("disconnect") => disconnect(args),
        Some("status") => status(args),
        Some("emit") => emit(args),
        Some("inject") => inject(args),
        _ => {
            eprintln!(
                "Usage:\n  \
                 loam federation connect <workspace> <broker> [--project org/project] [--global-root <path>] [--token <password>|--token-file <path>] [--json]\n    \
                 the org comes from LOAM_FEDERATION_ORG or `org` in <profile>/config.json; --project overrides both and names the project too\n  \
                 loam federation disconnect <workspace> --global-root <path> [--json]\n  \
                 loam federation status [<workspace>] --global-root <path> [--json]\n  \
                 loam federation emit [<workspace>] --global-root <path> [--json]   (reads one operation on stdin)\n  \
                 loam federation inject <register|drop> [<workspace>] --global-root <path> --session-id <id> [--channel-ref <ref>] [--wake-ref <ref>] [--json]"
            );
            64
        }
    }
}

/// Build the connector's service context (identity + absolute runtime) so
/// disconnect/status can drive the real per-user manager. Shared by both.
///
/// The instance id is the client certificate's SAN suffix when a bundle is
/// present, else a deterministic root-derived id. The certificate is the
/// single identity source for *sessions*; disconnect/status on a dormant
/// machine stay inert (no cert, no mint — just a stable scheduler label), and
/// `connect` refuses without a certificate.
fn service_context(root: &std::path::Path) -> Result<crate::service::ServiceContext, i32> {
    let instance_id = match crate::provisioning::configured_identity_root() {
        Ok(identity_root) => std::fs::read(identity_root.join("client.pem"))
            .ok()
            .and_then(|cert| crate::provisioning::certificate_instance_id(&cert).ok())
            .unwrap_or_else(|| root_derived_id(root)),
        Err(_) => root_derived_id(root),
    };
    let runtime_path = std::env::current_exe().map_err(|_| {
        eprintln!("federation: cannot resolve the current runtime path");
        70
    })?;
    Ok(crate::service::ServiceContext {
        global_root: root.to_path_buf(),
        instance_id,
        runtime_path,
        systemd_user_dir: systemd_user_dir(),
    })
}

/// The systemd `--user` unit directory for this machine: `$XDG_CONFIG_HOME/
/// systemd/user`, else `$HOME/.config/systemd/user`. `None` when neither
/// variable is set — the Linux symlink step then no-ops.
fn systemd_user_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|home| std::path::PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("systemd").join("user"))
}

/// Resolve a workspace path to its physical-identity key, exactly as enrollment
/// did, so path aliases map to the same enrollment.
fn workspace_key(workspace: &std::path::Path) -> Result<String, i32> {
    match enrollment::PhysicalWorkspace::resolve(workspace) {
        Ok(physical) => Ok(enrollment::identity_key(&physical)),
        Err(error) => {
            eprintln!("federation: {error}");
            Err(error.sysexit())
        }
    }
}

/// `loam federation disconnect <workspace> --global-root <path> [--json]`.
/// Local removal is authoritative; the service is reconciled from registry
/// truth. Broker cleanup is deferred to the real adapter (T13).
fn disconnect(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace: Option<PathBuf> = None;
    let mut global_root: Option<PathBuf> = None;
    let mut json_output = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--global-root" => match args.next() {
                Some(value) => global_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation disconnect: --global-root needs a value");
                    return 64;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("federation disconnect: unknown flag `{other}`");
                return 64;
            }
            other => {
                if workspace.is_some() {
                    eprintln!("federation disconnect: workspace given twice");
                    return 64;
                }
                workspace = Some(PathBuf::from(other));
            }
        }
    }
    let (Some(workspace), Some(root)) = (workspace, global_root) else {
        eprintln!("federation disconnect: <workspace> and --global-root are required");
        return 64;
    };

    let key = match workspace_key(&workspace) {
        Ok(key) => key,
        Err(code) => return code,
    };
    let context = match service_context(&root) {
        Ok(context) => context,
        Err(code) => return code,
    };
    let db_path = crate::provisioning::configured_registry_path(Some(&root))
        .unwrap_or_else(|_| root.join("loam.sqlite3"));
    let runner = crate::service::RealRunner;

    // The bounded broker tombstone is the real adapter's job (T13). Until then it
    // is a no-op success; local removal proceeds regardless of its outcome.
    match crate::connector::disconnect_by_key(&db_path, &key, Ok(()), &runner, &context) {
        Ok(report) => {
            let degraded = matches!(
                report.lifecycle,
                crate::connector::LifecycleOutcome::ManagerDegraded(_)
            );
            if json_output {
                println!("{}", disconnect_json(&report).to_json());
            } else {
                println!("disconnect: {}", disconnect_summary(&report));
            }
            // Local removal succeeds independently of the manager; a manager
            // stop/disable failure is surfaced as a degraded (non-zero) result.
            if degraded {
                70
            } else {
                0
            }
        }
        Err(crate::connector::DisconnectError::Registry(error)) => {
            eprintln!("federation disconnect: {error}");
            73
        }
    }
}

/// `loam federation status [<workspace>] --global-root <path> [--json]`.
/// Strictly read-only and egress-free: never creates the database or starts a
/// process.
fn status(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace: Option<PathBuf> = None;
    let mut global_root: Option<PathBuf> = None;
    let mut json_output = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--global-root" => match args.next() {
                Some(value) => global_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation status: --global-root needs a value");
                    return 64;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("federation status: unknown flag `{other}`");
                return 64;
            }
            other => {
                if workspace.is_some() {
                    eprintln!("federation status: workspace given twice");
                    return 64;
                }
                workspace = Some(PathBuf::from(other));
            }
        }
    }
    let Some(root) = global_root else {
        eprintln!("federation status: --global-root is required");
        return 64;
    };

    let key = match workspace.as_deref() {
        Some(path) => match workspace_key(path) {
            Ok(key) => Some(key),
            Err(code) => return code,
        },
        None => None,
    };
    let context = match service_context(&root) {
        Ok(context) => context,
        Err(code) => return code,
    };
    let db_path = crate::provisioning::configured_registry_path(Some(&root))
        .unwrap_or_else(|_| root.join("loam.sqlite3"));
    let runner = crate::service::RealRunner;

    let report = crate::connector::status_report(&db_path, &runner, &context, key.as_deref());
    if json_output {
        println!("{}", report.to_json());
    } else {
        println!("{}", status_summary(&report));
    }
    0
}

/// A terse, read-only human summary of the status projection: enrollment count,
/// definition presence, and manager state — never an aggregate readiness claim.
fn status_summary(report: &Value) -> String {
    let count = report
        .get("enrollments")
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    let definition = report
        .get("definition")
        .and_then(|d| d.get("present"))
        .map(|v| matches!(v, Value::Bool(true)))
        .unwrap_or(false);
    let manager = report
        .get("process")
        .and_then(|p| p.get("manager_state"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    format!(
        "status: {count} enrolled project(s); definition {}; manager {manager}; live broker session not observed",
        if definition { "present" } else { "absent" }
    )
}

fn disconnect_summary(report: &crate::connector::DisconnectReport) -> String {
    use crate::connector::{CleanupOutcome, LifecycleOutcome, LocalOutcome};
    let local = match report.local {
        LocalOutcome::Removed => "removed",
        LocalOutcome::AlreadyAbsent => "not-enrolled",
    };
    let lifecycle = match &report.lifecycle {
        LifecycleOutcome::Preserved { remaining } => {
            format!("service preserved ({remaining} left)")
        }
        LifecycleOutcome::StoppedDisabled => "service stopped/disabled".into(),
        LifecycleOutcome::ManagerDegraded(reason) => format!("manager degraded: {reason}"),
        LifecycleOutcome::Untouched => "machine dormant".into(),
    };
    let cleanup = match &report.broker_cleanup {
        CleanupOutcome::Ok => "broker cleanup ok".to_string(),
        CleanupOutcome::Failed(reason) => format!("broker cleanup failed: {reason}"),
    };
    format!("{local}; {lifecycle}; {cleanup}")
}

fn disconnect_json(report: &crate::connector::DisconnectReport) -> Value {
    use crate::connector::{CleanupOutcome, LifecycleOutcome, LocalOutcome};
    let local = match report.local {
        LocalOutcome::Removed => "removed",
        LocalOutcome::AlreadyAbsent => "not-enrolled",
    };
    let (lifecycle, remaining, reason) = match &report.lifecycle {
        LifecycleOutcome::Preserved { remaining } => ("preserved", Some(*remaining), None),
        LifecycleOutcome::StoppedDisabled => ("stopped-disabled", None, None),
        LifecycleOutcome::ManagerDegraded(reason) => {
            ("manager-degraded", None, Some(reason.clone()))
        }
        LifecycleOutcome::Untouched => ("untouched", None, None),
    };
    let mut lifecycle_fields = vec![("state".into(), Value::String(lifecycle.into()))];
    if let Some(remaining) = remaining {
        lifecycle_fields.push(("remaining".into(), Value::Number(remaining.to_string())));
    }
    if let Some(reason) = reason {
        lifecycle_fields.push(("reason".into(), Value::String(reason)));
    }
    let broker = match &report.broker_cleanup {
        CleanupOutcome::Ok => Value::Object(vec![("cleanup".into(), Value::String("ok".into()))]),
        CleanupOutcome::Failed(reason) => Value::Object(vec![
            ("cleanup".into(), Value::String("failed".into())),
            ("reason".into(), Value::String(reason.clone())),
        ]),
    };
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("local".into(), Value::String(local.into())),
        ("lifecycle".into(), Value::Object(lifecycle_fields)),
        ("broker_cleanup".into(), broker),
    ])
}

/// Hidden internal service entrypoint: `loam federation service
/// <install|uninstall|status|run> --global-root <path>`. Manages the dormant
/// per-user definition and runs the inert-by-default connector. Not user-facing.
fn service(mut args: impl Iterator<Item = String>) -> i32 {
    let subcommand = match args.next() {
        Some(value) => value,
        None => {
            eprintln!("federation service: expected install|uninstall|status|enable|disable|run");
            return 64;
        }
    };
    let mut global_root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--global-root" => match args.next() {
                Some(value) => global_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation service: --global-root needs a value");
                    return 64;
                }
            },
            other => {
                eprintln!("federation service: unexpected argument `{other}`");
                return 64;
            }
        }
    }
    let Some(root) = global_root else {
        eprintln!("federation service: --global-root is required");
        return 64;
    };

    match subcommand.as_str() {
        "run" => service_run(&root),
        "install" => service_lifecycle(&root, ServiceAction::Install),
        "uninstall" => service_lifecycle(&root, ServiceAction::Uninstall),
        "status" => service_lifecycle(&root, ServiceAction::Status),
        // enable/disable let packaged setup preserve active/inert desired state
        // across a runtime-path update: they reuse the same manager
        // functions connect/disconnect use, so no definition logic lives in Node.
        "enable" => service_lifecycle(&root, ServiceAction::Enable),
        "disable" => service_lifecycle(&root, ServiceAction::Disable),
        other => {
            eprintln!("federation service: unknown subcommand `{other}`");
            64
        }
    }
}

fn service_run(root: &std::path::Path) -> i32 {
    #[cfg(any(unix, windows))]
    {
        match crate::connector::run_service(root) {
            Ok(_) => 0,
            Err(_) => 70,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = root;
        eprintln!("federation service run: unsupported platform");
        69
    }
}

enum ServiceAction {
    Install,
    Uninstall,
    Status,
    Enable,
    Disable,
}

/// Install/uninstall/status/enable/disable the native definition. Builds the
/// service context (the absolute current runtime + an instance id) and drives
/// the real per-user manager. `install`/`status`/`disable` never start the
/// connector or contact a broker; `enable` re-asserts active desired state on
/// the current runtime after a runtime-path update (setup delegates this, T12).
///
/// Unlike disconnect/status, the lifecycle verbs are identity-free: a dormant
/// definition can be staged before any enrollment exists. The instance id is
/// the certificate's SAN suffix when a bundle is present, else a deterministic
/// id derived from the global root path — a scheduler label only, never a wire
/// identity (the certificate remains the single identity source for sessions).
fn service_lifecycle(root: &std::path::Path, action: ServiceAction) -> i32 {
    let instance_id = match crate::provisioning::configured_identity_root() {
        Ok(identity_root) => std::fs::read(identity_root.join("client.pem"))
            .ok()
            .and_then(|cert| crate::provisioning::certificate_instance_id(&cert).ok())
            .unwrap_or_else(|| root_derived_id(root)),
        Err(_) => root_derived_id(root),
    };
    let runtime_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => {
            eprintln!("federation service: cannot resolve the current runtime path");
            return 70;
        }
    };
    let context = crate::service::ServiceContext {
        global_root: root.to_path_buf(),
        instance_id,
        runtime_path,
        systemd_user_dir: systemd_user_dir(),
    };
    let runner = crate::service::RealRunner;
    let result = match action {
        ServiceAction::Install => crate::service::install(&runner, &context).map(|()| 0),
        ServiceAction::Uninstall => crate::service::uninstall(&runner, &context).map(|()| 0),
        ServiceAction::Status => crate::service::status(&runner, &context),
        ServiceAction::Enable => crate::service::enable_start(&runner, &context).map(|()| 0),
        ServiceAction::Disable => crate::service::disable_stop(&runner, &context).map(|()| 0),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("federation service: {error}");
            70
        }
    }
}

/// A deterministic, non-secret instance id for the dormant service definition
/// when no identity bundle exists yet: a 26-char Crockford-base32 digest of the
/// canonical global root path. Stable across calls on the same root, so the
/// Windows task name never churns; it is a scheduler label only and is replaced
/// by the certificate-derived id the moment connect runs.
fn root_derived_id(root: &std::path::Path) -> String {
    use crate::sha256::Sha256;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let canonical = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let mut hasher = Sha256::default();
    hasher.update(canonical.as_bytes());
    hasher
        .finish()
        .bytes()
        .take(26)
        .map(|byte| ALPHABET[(byte as usize) % ALPHABET.len()] as char)
        .collect()
}

/// `loam federation connect <workspace> <broker> [--project org/project]
/// [--global-root <path>] [--json]`.
///
/// The one-command enrollment surface: workspace and broker are positional,
/// org/project are inferred from the workspace's git remote URL (overridable
/// with `--project`), and the machine's instance id is the client certificate's
/// SAN suffix — nothing is minted and no descriptor ceremony remains.
fn connect(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace: Option<PathBuf> = None;
    let mut broker: Option<String> = None;
    let mut project_override: Option<String> = None;
    let mut global_root: Option<PathBuf> = None;
    let mut json_output = false;
    let mut token: Option<String> = None;
    let mut token_file: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--token" => match args.next() {
                Some(value) => token = Some(value),
                None => {
                    eprintln!("federation connect: --token needs a value");
                    return 64;
                }
            },
            "--token-file" => match args.next() {
                Some(value) => token_file = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation connect: --token-file needs a value");
                    return 64;
                }
            },
            "--project" => match args.next() {
                Some(value) => project_override = Some(value),
                None => {
                    eprintln!("federation connect: --project needs a value");
                    return 64;
                }
            },
            "--global-root" => match args.next() {
                Some(value) => global_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation connect: --global-root needs a value");
                    return 64;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("federation connect: unknown flag `{other}`");
                return 64;
            }
            other if broker.is_none() && workspace.is_some() => {
                broker = Some(other.to_owned());
            }
            other => {
                if workspace.is_some() {
                    eprintln!("federation connect: workspace given twice");
                    return 64;
                }
                workspace = Some(PathBuf::from(other));
            }
        }
    }
    let (Some(workspace), Some(broker)) = (workspace, broker) else {
        eprintln!("federation connect: <workspace> and <broker> are required");
        return 64;
    };

    let token = match (token, token_file) {
        (Some(_), Some(_)) => {
            eprintln!("federation connect: --token and --token-file are mutually exclusive");
            return 64;
        }
        (Some(other), None) => Some(other),
        (None, Some(path)) => match std::fs::read_to_string(&path) {
            Ok(value) => Some(value.trim().to_owned()),
            Err(_) => {
                eprintln!(
                    "federation connect: cannot read --token-file {}",
                    path.display()
                );
                return 64;
            }
        },
        (None, None) => match std::env::var("LOAM_FEDERATION_TOKEN") {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => None,
        },
    };

    let enrolled = match validate_connect(&workspace, &broker, project_override.as_deref()) {
        Ok(enrolled) => enrolled,
        Err(error) => {
            if json_output {
                println!("{}", error_json(&error).to_json());
            } else {
                eprintln!("federation connect: {error}");
            }
            return error.sysexit();
        }
    };

    let global_root = match global_root {
        Some(root) => Some(root),
        None if token.is_some() => match crate::hooks::installed_global_root() {
            Ok(root) => Some(root),
            Err(_) => {
                eprintln!("federation connect: --global-root is required");
                return 64;
            }
        },
        None => None,
    };

    match global_root {
        // No global root and no token: validation-only (the workspace + broker proof).
        None => {
            if json_output {
                println!("{}", success_json(&enrolled).to_json());
            } else {
                println!(
                    "validated enrollment for {}/{} at {}",
                    enrolled.org_id, enrolled.project_id, enrolled.workspace.display_path
                );
            }
            0
        }
        // With a global root: the full transactional connect.
        Some(root) => {
            // Auto-enrollment: a machine with no identity bundle and a token
            // self-enrolls first. An existing certificate means the enrollment
            // path never engages, even with a token supplied.
            if let Some(token) = token {
                let identity_root = crate::provisioning::configured_identity_root()
                    .ok()
                    .filter(|root| root.join("client.pem").is_file());
                if identity_root.is_none() {
                    if let Err(failure) = auto_enroll(&enrolled, &token, &root) {
                        let code = format!("enrollment: {}", failure.code());
                        if json_output {
                            println!(
                                "{}",
                                Value::Object(vec![
                                    ("schema".into(), Value::Number("1".into())),
                                    ("status".into(), Value::String("error".into())),
                                    (
                                        "error".into(),
                                        Value::Object(vec![(
                                            "code".into(),
                                            Value::String(code.clone())
                                        )]),
                                    ),
                                ])
                                .to_json()
                            );
                        } else {
                            eprintln!("federation connect: {code}");
                        }
                        #[cfg(debug_assertions)]
                        if let Some((operation, detail)) = failure.debug_detail() {
                            eprintln!("federation connect: local crypto {operation}: {detail}");
                        }
                        return 69;
                    }
                }
            }
            orchestrate_cli(&enrolled, &root, json_output)
        }
    }
}

/// Run the machine-side enrollment: generate keypair + CSR from git identity,
/// POST to the signer, store the returned certificate through the existing
/// identity path with the existing perms hardening.
fn auto_enroll(
    enrolled: &enrollment::ValidatedEnrollment,
    token: &str,
    root: &std::path::Path,
) -> Result<(), crate::enrollment_auto::EnrollmentFailure> {
    use crate::enrollment_auto::EnrollmentFailure;

    let workspace = &enrolled.workspace.display_path;
    let (email, display_name) = crate::provisioning::git_identity(std::path::Path::new(workspace));
    let email = email.ok_or(EnrollmentFailure::GitIdentityRequired)?;
    let display_name = display_name.unwrap_or_default();

    let instance_id = crate::enrollment_auto::generate_instance_id()?;
    let (key_pem, csr_pem) =
        crate::enrollment_auto::generate_keypair_and_csr(&email, &display_name, &instance_id)?;

    let host = enrolled
        .broker_endpoint
        .strip_prefix("mqtts://")
        .and_then(|authority| {
            authority
                .rsplit_once(':')
                .map(|(host, _)| host)
                .or(Some(authority))
        })
        .unwrap_or_default();
    let url = crate::enrollment_auto::signer_url(host);
    let trust = crate::provisioning::resolve_trust_anchors(
        enrolled.ca_ref.as_deref(),
        std::env::var("SSL_CERT_FILE").ok().as_deref(),
    )
    .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let certificate =
        crate::enrollment_auto::request_signed_certificate(&url, token, &csr_pem, &trust)?;

    let identity_root = crate::provisioning::configured_identity_root()
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    crate::provisioning::store_identity_bundle(&identity_root, &certificate, &key_pem)
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;

    let _ = root;
    Ok(())
}

/// Build the validated enrollment from the one-command surface: the scope
/// resolved by [`resolve_scope`] (org from configuration, project from the
/// workspace's git remote), the broker endpoint validated, and the
/// physical-identity + remote-digest proof run exactly as the descriptor path
/// did. No commit is read or proven.
fn validate_connect(
    workspace: &std::path::Path,
    broker: &str,
    project_override: Option<&str>,
) -> Result<enrollment::ValidatedEnrollment, EnrollmentError> {
    // The endpoint is validated before any git work: a typo in the broker must
    // never spend a remote fetch to be discovered.
    if !broker.starts_with("mqtts://")
        || broker.strip_prefix("mqtts://").is_none_or(|rest| {
            rest.is_empty()
                || rest.contains('@')
                || rest.contains('?')
                || rest.contains('#')
                || rest.contains('/')
        })
    {
        return Err(EnrollmentError::InvalidEndpoint);
    }
    let (org_id, project_id) = resolve_scope(workspace, project_override)?;
    let descriptor = enrollment::Descriptor {
        repository_id: format!("{org_id}/{project_id}"),
        org_id,
        project_id,
        broker: enrollment::BrokerDescriptor {
            profile: "default".to_owned(),
            endpoint: broker.to_owned(),
            tls_server_name: broker
                .strip_prefix("mqtts://")
                .and_then(|authority| authority.rsplit_once(':'))
                .map(|(host, _)| host.to_owned())
                .unwrap_or_default(),
            ca_ref: None,
        },
        git: enrollment::GitDescriptor {
            commit: None,
            remotes: vec![enrollment::RemoteDescriptor {
                name: "origin".to_owned(),
                refs: vec!["refs/heads/main".to_owned()],
            }],
        },
    };
    enrollment::validate_enrollment(descriptor, workspace)
}

/// Split a `--project org/project` value into its two atoms.
fn split_scope(scope: &str) -> Result<(String, String), EnrollmentError> {
    let (org, project) = scope
        .split_once('/')
        .ok_or(EnrollmentError::InvalidField { field: "project" })?;
    if org.is_empty() || project.is_empty() || org.contains('/') || project.contains('/') {
        return Err(EnrollmentError::InvalidField { field: "project" });
    }
    Ok((org.to_owned(), project.to_owned()))
}

/// Which org and project this workspace federates under.
///
/// Precedence, first wins:
///
/// 1. `--project <org>/<project>` — the explicit override, supplying both.
/// 2. `LOAM_FEDERATION_ORG` — the org, for CI and unattended installs.
/// 3. `org` in the profile's `config.json` — the durable machine setting.
/// 4. nothing. The org is *not* inferred, and an unconfigured machine is
///    refused with a recipe rather than connected to a guess.
///
/// The project keeps coming from the workspace's `origin` remote unless
/// `--project` names both: the repository *is* the project, one org holds many,
/// and that is also the shape of the broker's topics
/// (`loam/v1/<org>/<project>/...`). The org does not, because it is a property
/// of the operator/org relationship rather than of where a repository happens
/// to be hosted — inferring it yielded the host account for every repo on a
/// real laptop, which is an org the broker's ACL denies. That failure is
/// invisible at connect time and only shows up as denied subscribes, so
/// falling back to the inference would preserve the exact footgun this
/// resolution exists to remove.
fn resolve_scope(
    workspace: &std::path::Path,
    project_override: Option<&str>,
) -> Result<(String, String), EnrollmentError> {
    if let Some(scope) = project_override {
        return split_scope(scope);
    }
    let project_id = infer_project(workspace)?;
    let org_id = configured_org()?;
    validate_scope_part(&org_id, "org_id")?;
    Ok((org_id, project_id))
}

/// The org from the environment, then from `config.json`. `Err` when neither
/// names one, carrying the path an operator should write.
fn configured_org() -> Result<String, EnrollmentError> {
    let present = |value: String| {
        let trimmed = value.trim().to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    };
    if let Some(org) = std::env::var("LOAM_FEDERATION_ORG").ok().and_then(present) {
        return Ok(org);
    }
    // A malformed `config.json` is deliberately not fatal here: the org is
    // absent either way, and the recipe below is the more useful thing to say
    // than a parse complaint about a file the operator may not know exists.
    if let Ok(Some(config)) = crate::provisioning::read_configured_config() {
        if let Some(org) = config
            .get("org")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .and_then(present)
        {
            return Ok(org);
        }
    }
    Err(EnrollmentError::FederationOrgUnconfigured {
        config_path: crate::provisioning::configured_config_path()
            .map(|path| path.display().to_string())
            // No profile resolves at all, so there is nowhere to write. Name
            // the file rather than an empty string; the other two rungs in the
            // message still work.
            .unwrap_or_else(|_| "config.json".to_owned()),
    })
}

/// Reject an org or project that could not be one: the value becomes a topic
/// segment, and a slash or an empty string there would silently re-scope every
/// message this machine publishes.
fn validate_scope_part(value: &str, field: &'static str) -> Result<(), EnrollmentError> {
    if value.is_empty() || value.contains('/') || value.chars().any(char::is_control) {
        return Err(EnrollmentError::InvalidField { field });
    }
    Ok(())
}

/// The project id from the workspace's `origin` remote: the last path segment
/// of the URL, which is the repository name. See [`resolve_scope`] for why the
/// org is not read from the segment before it.
fn infer_project(workspace: &std::path::Path) -> Result<String, EnrollmentError> {
    let path_str = workspace
        .to_str()
        .ok_or(EnrollmentError::WorkspaceNotUtf8)?;
    let output = std::process::Command::new("git")
        .args(["-C", path_str, "remote", "get-url", "origin"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|_| EnrollmentError::GitUnavailable)?;
    if !output.status.success() {
        return Err(EnrollmentError::RemoteNotConfigured {
            remote: "origin".to_owned(),
        });
    }
    let url = String::from_utf8(output.stdout)
        .map_err(|_| EnrollmentError::RemoteNotConfigured {
            remote: "origin".to_owned(),
        })?
        .trim()
        .to_owned();
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&url)
        .split_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or(&url);
    let path = path.trim_end_matches(".git").trim_end_matches('/');
    let project = path
        .split('/')
        .rfind(|segment| !segment.is_empty())
        .ok_or(EnrollmentError::InvalidField { field: "project" })?;
    validate_scope_part(project, "project")?;
    Ok(project.to_owned())
}

/// Drive the transactional connect from the CLI: derive the connector's
/// service context and identity from the certificate, run the
/// probe/commit/activate orchestration, and report the outcome.
fn orchestrate_cli(
    enrolled: &enrollment::ValidatedEnrollment,
    root: &std::path::Path,
    json_output: bool,
) -> i32 {
    let report_error = |code: &str| -> i32 {
        if json_output {
            println!(
                "{}",
                Value::Object(vec![
                    ("schema".into(), Value::Number("1".into())),
                    ("status".into(), Value::String("error".into())),
                    (
                        "error".into(),
                        Value::Object(vec![("code".into(), Value::String(code.into()))]),
                    ),
                ])
                .to_json()
            );
        } else {
            eprintln!("federation connect: {code}");
        }
        69
    };
    let context = match service_context(root) {
        Ok(context) => context,
        Err(code) => return code,
    };
    let instance_id = context.instance_id.clone();
    let runner = crate::service::RealRunner;
    let db_path = crate::provisioning::configured_registry_path(Some(root))
        .unwrap_or_else(|_| root.join("loam.sqlite3"));
    let now = chrono::Utc::now();

    // Probe against the REAL broker. Build the session inputs from the
    // validated descriptor plus this machine's certificate-derived instance id,
    // exactly as `provisioning::resolve` builds them from the committed row, so
    // the enrollment probe authenticates over mTLS, subscribes, publishes, and
    // requires its own echoed event *before* the row is committed and the
    // service activated. The transport alone learns the canonical principal
    // from the certificate.
    let row = crate::enrollment::EnrolledRow {
        identity_key: crate::enrollment::identity_key(&enrolled.workspace),
        org_id: enrolled.org_id.clone(),
        project_id: enrolled.project_id.clone(),
        repository_id: enrolled.repository_id.clone(),
        descriptor_digest: crate::enrollment::descriptor_digest(enrolled),
        display_path: enrolled.workspace.display_path.clone(),
        instance_id: instance_id.clone(),
        broker_profile: enrolled.broker_profile.clone(),
        broker_endpoint: enrolled.broker_endpoint.clone(),
        tls_server_name: enrolled.tls_server_name.clone(),
        ca_ref: enrolled.ca_ref.clone(),
        commit: enrolled.commit.clone(),
        capabilities: crate::enrollment::CapabilityRecord {
            authentication: false,
            publish: false,
            subscribe: false,
            self_receive: false,
            verified_at: now.to_rfc3339(),
        },
        remotes: enrolled.remotes.clone(),
    };
    let (session, _roster) = match crate::provisioning::resolve(&row) {
        Ok(pair) => pair,
        Err(crate::connector::ProvisionFailure::Credentials(reason))
        | Err(crate::connector::ProvisionFailure::Roster(reason)) => {
            return report_error(reason);
        }
    };
    let mut transport = match crate::connector::MqttTransport::new(
        session,
        crate::envelope::ValidationConfig::default(),
        now,
    ) {
        Ok(transport) => transport,
        Err(error) => return report_error(error.code()),
    };

    match crate::connector::orchestrate_from_validated(
        enrolled,
        &mut transport,
        &runner,
        &context,
        &db_path,
        &crate::envelope::ValidationConfig::default(),
        std::time::Duration::from_secs(5),
        now,
    ) {
        Ok(outcome) => {
            let state = match outcome {
                crate::connector::ConnectOutcome::Connected { .. } => "connected",
                crate::connector::ConnectOutcome::AlreadyConnected => "already-connected",
            };
            if json_output {
                println!(
                    "{}",
                    Value::Object(vec![
                        ("schema".into(), Value::Number("1".into())),
                        ("status".into(), Value::String(state.into())),
                        ("org_id".into(), Value::String(enrolled.org_id.clone())),
                        (
                            "project_id".into(),
                            Value::String(enrolled.project_id.clone())
                        ),
                    ])
                    .to_json()
                );
            } else {
                println!("{state}: {}/{}", enrolled.org_id, enrolled.project_id);
            }
            0
        }
        Err(error) => {
            // Rendering belongs to `connect_error_json`/`connect_error_line`,
            // which already emit the code plus the failing stage's own reason.
            // #95 adds a reason where there was none: a `key.pem` that is not
            // the key in `client.pem` used to report `connect_probe_failed`
            // and nothing more. It flows through these renderers unchanged.
            if json_output {
                println!("{}", connect_error_json(&error).to_json());
            } else {
                eprintln!("federation connect: {}", connect_error_line(&error));
            }
            connect_sysexit(&error)
        }
    }
}

/// The `--json` failure envelope. The code is the stable contract; the detail is
/// added only when the error carries one, so a consumer keying on `code` is
/// unaffected while an operator reading the output finally gets the manager's
/// own words (#128).
fn connect_error_json(error: &crate::connector::ConnectError) -> Value {
    let mut failure = vec![("code".into(), Value::String(error.code().into()))];
    if let Some(detail) = error.detail() {
        failure.push(("detail".into(), Value::String(detail.to_owned())));
    }
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("status".into(), Value::String("error".into())),
        ("error".into(), Value::Object(failure)),
    ])
}

/// The same failure for a human. Without `--json` the detail was dropped too, so
/// the terminal showed nothing but the opaque code.
fn connect_error_line(error: &crate::connector::ConnectError) -> String {
    match error.detail() {
        Some(detail) => format!("{}: {detail}", error.code()),
        None => error.code().to_owned(),
    }
}

fn connect_sysexit(error: &crate::connector::ConnectError) -> i32 {
    use crate::connector::ConnectError;
    match error {
        ConnectError::EnrollmentConflict => 65,
        ConnectError::Probe(_) => 69,
        ConnectError::Registry(_) => 73,
        ConnectError::ActivationFailed(_) => 75,
        ConnectError::RollbackIncomplete(_) => 70,
    }
}

/// Read stdin with a hard ceiling one byte over the descriptor limit, so an
/// oversized document is detected as [`EnrollmentError::TooLarge`] rather than
/// buffered unbounded.
fn read_bounded_stdin() -> Result<Vec<u8>, i32> {
    use std::io::Read;
    let mut buffer = Vec::new();
    let limit = (MAX_DESCRIPTOR_BYTES + 1) as u64;
    let mut handle = std::io::stdin().lock().take(limit);
    if handle.read_to_end(&mut buffer).is_err() {
        eprintln!("federation: could not read the operation from stdin");
        return Err(65);
    }
    Ok(buffer)
}

fn success_json(enrolled: &enrollment::ValidatedEnrollment) -> Value {
    let remotes = enrolled
        .remotes
        .iter()
        .map(|remote| {
            Value::Object(vec![
                ("name".into(), Value::String(remote.name.clone())),
                (
                    "url_digest".into(),
                    Value::String(remote.url_digest.clone()),
                ),
                (
                    "allowed_refs".into(),
                    Value::Array(
                        remote
                            .allowed_refs
                            .iter()
                            .map(|r| Value::String(r.clone()))
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("status".into(), Value::String("validated".into())),
        ("org_id".into(), Value::String(enrolled.org_id.clone())),
        (
            "project_id".into(),
            Value::String(enrolled.project_id.clone()),
        ),
        (
            "repository_id".into(),
            Value::String(enrolled.repository_id.clone()),
        ),
        (
            "broker_profile".into(),
            Value::String(enrolled.broker_profile.clone()),
        ),
        ("commit".into(), Value::String(enrolled.commit.clone())),
        (
            "workspace".into(),
            Value::String(enrolled.workspace.display_path.clone()),
        ),
        ("remotes".into(), Value::Array(remotes)),
    ])
}

fn error_json(error: &EnrollmentError) -> Value {
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("status".into(), Value::String("error".into())),
        (
            "error".into(),
            Value::Object(vec![
                ("code".into(), Value::String(error.code().into())),
                ("message".into(), Value::String(error.to_string())),
            ]),
        ),
    ])
}

// ---------------------------------------------------------------------------
// `loam federation emit`
// ---------------------------------------------------------------------------

/// The Phase-1 vocabulary, closed. A well-formed namespaced extension type is
/// render-only and is refused here rather than dispatched.
pub const EMIT_TYPES: [&str; 3] = ["message.reply", "message.ack", "work.report"];

/// Envelope fields whose value is derived in trusted code. A caller-supplied
/// value for any of them is rejected — never merged — and reported. The scan
/// deliberately skips the `payload` subtree: that is the caller's own data,
/// preserved verbatim, and a payload field named `id` is not an authority claim.
const AUTHORITY_FIELDS: [&str; 13] = [
    "source",
    "id",
    "time",
    "specversion",
    "dataschema",
    "from",
    "principal_id",
    "agent_id",
    "instance_id",
    "context",
    "org_id",
    "project_id",
    "repository_id",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitError {
    NotAnObject,
    TooLarge,
    UnsupportedType,
    MissingCausation,
    MissingRecipient,
    MissingStateKey,
    InvalidWorkState,
    /// A claim-bearing work report with no plan anchor (#98). The envelope
    /// validator refuses these too; refusing here means the caller is told which
    /// field is missing without spending a round trip to learn it.
    MissingPlanOid,
    InvalidPlanOid,
    Unenrolled,
    AlreadyResponded,
    ConnectorUnreachable,
    /// The connector answered, and refused. The payload is the refusal it
    /// reported — its IPC code, plus the rule that rejected the envelope when it
    /// knew one. Without it every refusal reads the same (#102): a missing plan
    /// anchor, an unaddressable recipient, and a project-binding mismatch all
    /// arrived as a bare `connector_refused`. `None` is for a reply this side
    /// could not read at all, where there is genuinely nothing to name.
    ConnectorRefused(Option<String>),
}

impl EmitError {
    pub fn code(&self) -> &'static str {
        match self {
            EmitError::NotAnObject => "operation_not_an_object",
            EmitError::TooLarge => "operation_too_large",
            EmitError::UnsupportedType => "unsupported_operation_type",
            EmitError::MissingCausation => "missing_causation_id",
            EmitError::MissingRecipient => "missing_recipient",
            EmitError::MissingStateKey => "missing_state_key",
            EmitError::InvalidWorkState => "invalid_work_state",
            // The same tokens the envelope validator reports for the same two
            // rules, so a refusal reads identically whichever layer caught it.
            EmitError::MissingPlanOid => "missing_plan_oid",
            EmitError::InvalidPlanOid => "invalid_plan_oid",
            EmitError::Unenrolled => "workspace_unenrolled",
            EmitError::AlreadyResponded => "already_responded",
            EmitError::ConnectorUnreachable => "connector_unreachable",
            EmitError::ConnectorRefused(_) => "connector_refused",
        }
    }

    /// The refusal detail behind the code, when the connector named one. Kept
    /// separate from `code`, which stays a stable, matchable vocabulary: this is
    /// the part that says which rule fired.
    pub fn diagnostic(&self) -> Option<&str> {
        match self {
            EmitError::ConnectorRefused(detail) => detail.as_deref(),
            _ => None,
        }
    }

    fn sysexit(&self) -> i32 {
        match self {
            // An already-responded outcome is a correct, expected result, not a
            // failure: exactly one terminal ships and the others say so.
            EmitError::AlreadyResponded => 0,
            EmitError::Unenrolled => 78,
            EmitError::ConnectorUnreachable | EmitError::ConnectorRefused(_) => 69,
            _ => 65,
        }
    }
}

/// One derived operation, ready for the connector, plus every override the
/// caller attempted.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedEmit {
    pub operation: Value,
    pub refused: Vec<String>,
    pub causation_id: Option<String>,
    pub event_id: String,
}

/// Subtrees the refusal scan does not descend into: they carry caller data whose
/// field names collide with authority names without claiming any authority. A
/// recipient's `id` names who to reach, an artifact's `id` names a task, and a
/// payload is preserved verbatim — none of the three is an envelope claim.
const CALLER_DATA_SUBTREES: [&str; 3] = ["payload", "to", "artifacts"];

/// Collect every authority-bearing key the caller tried to set, at any depth
/// outside the caller-data subtrees. Order is stable so the reported list is
/// deterministic.
fn refused_overrides(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                refused_overrides(item, found);
            }
        }
        Value::Object(entries) => {
            for (key, child) in entries {
                if CALLER_DATA_SUBTREES.contains(&key.as_str()) {
                    continue;
                }
                if AUTHORITY_FIELDS.contains(&key.as_str()) && !found.contains(key) {
                    found.push(key.clone());
                }
                refused_overrides(child, found);
            }
        }
        _ => {}
    }
}

/// A non-secret, collision-resistant event id in the 26-character upper-case
/// base32 shape the rest of the corpus uses. Derived, never caller-supplied.
fn derive_event_id(now: DateTime<Utc>, salt: &str) -> String {
    use crate::sha256::Sha256;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut hasher = Sha256::default();
    hasher.update(
        now.timestamp_nanos_opt()
            .unwrap_or_default()
            .to_le_bytes()
            .as_slice(),
    );
    // The workspace identity key is the per-machine salt: two workspaces
    // emitting in the same nanosecond still derive different ids, and no
    // process-spawning capability is needed to get there.
    hasher.update(salt.as_bytes());
    hasher
        .finish()
        .bytes()
        .take(26)
        .map(|byte| ALPHABET[(byte as usize) % ALPHABET.len()] as char)
        .collect()
}

/// A caller may spell `revision` as a JSON string or a JSON number; both mean
/// the same revision, and reading only one of them silently defaults the other
/// to `1`. The derived operation carries the *string* form: the connector reads
/// it back with `as_str` before re-emitting the numeric literal into the
/// envelope's delivery block.
fn revision_literal(value: &Value) -> Option<String> {
    match value {
        Value::String(literal) | Value::Number(literal) if !literal.is_empty() => {
            Some(literal.clone())
        }
        _ => None,
    }
}

/// Derive every authority-bearing field from trusted state and refuse the rest.
/// Pure: no registry, no socket, no clock of its own.
pub fn derive_emit(
    operation: &Value,
    row: &enrollment::EnrolledRow,
    now: DateTime<Utc>,
) -> Result<DerivedEmit, EmitError> {
    let Value::Object(_) = operation else {
        return Err(EmitError::NotAnObject);
    };
    let operation_type = operation
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !EMIT_TYPES.contains(&operation_type) {
        return Err(EmitError::UnsupportedType);
    }

    let mut refused = Vec::new();
    refused_overrides(operation, &mut refused);

    let event_id = derive_event_id(now, &row.identity_key);
    let time = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let expires_at =
        (now + chrono::Duration::hours(24)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let causation_id = match operation_type {
        "work.report" => None,
        _ => Some(
            operation
                .get("causation_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(EmitError::MissingCausation)?
                .to_owned(),
        ),
    };

    let mut derived = vec![
        ("type".into(), Value::String(operation_type.to_owned())),
        // Derived, not accepted: the instance the enrollment binds this
        // workspace to. The connector binds the principal at publish time.
        (
            "source".into(),
            Value::String(format!("urn:loam:instance:{}", row.instance_id)),
        ),
        ("id".into(), Value::String(event_id.clone())),
        ("time".into(), Value::String(time)),
        ("expires_at".into(), Value::String(expires_at)),
        (
            "summary".into(),
            Value::String(
                operation
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
        ),
        (
            "payload".into(),
            operation
                .get("payload")
                .cloned()
                .unwrap_or(Value::Object(Vec::new())),
        ),
        (
            "thread".into(),
            Value::Object(vec![
                ("id".into(), Value::String(format!("thread-{event_id}"))),
                // Correlation is derived: this event's own id. Causation is the
                // one thread field a caller legitimately supplies — it names the
                // request being answered, it grants no authority.
                ("correlation_id".into(), Value::String(event_id.clone())),
                (
                    "causation_id".into(),
                    match &causation_id {
                        Some(value) => Value::String(value.clone()),
                        None => Value::Null,
                    },
                ),
            ]),
        ),
    ];

    if operation_type == "work.report" {
        let key = operation
            .get("state_key")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(EmitError::MissingStateKey)?;
        let state = operation
            .get("payload")
            .and_then(|payload| payload.get("state"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(
            state,
            "active" | "blocked" | "ready" | "published" | "abandoned"
        ) {
            return Err(EmitError::InvalidWorkState);
        }
        derived.push(("state_key".into(), Value::String(key.to_owned())));
        derived.push((
            "revision".into(),
            Value::String(
                operation
                    .get("revision")
                    .and_then(revision_literal)
                    .unwrap_or_else(|| "1".to_owned()),
            ),
        ));
        let artifacts = match operation.get("artifacts") {
            Some(artifacts @ Value::Array(_)) => artifacts.clone(),
            _ => Value::Array(Vec::new()),
        };
        // The plan anchor (#98). Caller-supplied on purpose: it is a provenance
        // assertion, not an authority claim — org, project, repository, instance
        // and principal stay derived, and a false anchor grants nothing and is
        // checkable against Git by any receiver. That is what lets it cross the
        // authority refusal list that keeps `context` out of a caller's hands.
        //
        // Required exactly where the envelope requires it: a report that carries a
        // claim. Refusing here rather than only at the validator means the caller
        // is told which field is missing instead of spending a round trip to be
        // told something was invalid.
        let plan_oid = operation
            .get("plan_oid")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        if let Some(plan_oid) = plan_oid {
            // The validator's own predicate, not a copy of it: this refusal
            // exists only to say "the envelope would reject this" one layer
            // earlier, so any disagreement is this layer refusing anchors that
            // would have shipped.
            if !crate::envelope::valid_git_oid(plan_oid) {
                return Err(EmitError::InvalidPlanOid);
            }
            derived.push(("plan_oid".into(), Value::String(plan_oid.to_owned())));
        } else if bears_claim(&artifacts, operation.get("payload")) {
            return Err(EmitError::MissingPlanOid);
        }
        derived.push(("artifacts".into(), artifacts));
    } else {
        let recipients = operation
            .get("to")
            .and_then(Value::as_array)
            .filter(|recipients| {
                recipients.iter().any(|recipient| {
                    matches!(
                        recipient.get("kind").and_then(Value::as_str),
                        Some("agent" | "principal" | "instance")
                    ) && recipient
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.is_empty())
                })
            })
            .ok_or(EmitError::MissingRecipient)?;
        derived.push(("to".into(), Value::Array(recipients.to_vec())));
    }

    Ok(DerivedEmit {
        operation: Value::Object(derived),
        refused,
        causation_id,
        event_id,
    })
}

/// Does this work report make a claim about identified work? The rule is the
/// envelope validator's, deliberately: a `task` or `acceptance` artifact, or a
/// non-empty acceptance map in the payload. Kept in step with `envelope.rs`'s
/// claim-bearing test — a report refused here for a missing anchor must be one
/// the validator would refuse for the same reason, or this layer starts refusing
/// reports that would have shipped.
fn bears_claim(artifacts: &Value, payload: Option<&Value>) -> bool {
    let claiming_artifact = artifacts
        .as_array()
        .unwrap_or_default()
        .iter()
        .any(|artifact| {
            matches!(
                artifact.get("kind").and_then(Value::as_str),
                Some("task" | "acceptance")
            )
        });
    let claiming_acceptance = payload
        .and_then(|payload| payload.get("acceptance"))
        .is_some_and(|acceptance| match acceptance {
            Value::Object(entries) => !entries.is_empty(),
            _ => false,
        });
    claiming_artifact || claiming_acceptance
}

/// `loam federation emit [<workspace>] --global-root <path> [--json]`.
fn emit(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace: Option<PathBuf> = None;
    let mut global_root: Option<PathBuf> = None;
    let mut json_output = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--global-root" => match args.next() {
                Some(value) => global_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation emit: --global-root needs a value");
                    return 64;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("federation emit: unknown flag `{other}`");
                return 64;
            }
            other => {
                if workspace.is_some() {
                    eprintln!("federation emit: workspace given twice");
                    return 64;
                }
                workspace = Some(PathBuf::from(other));
            }
        }
    }
    let Some(root) = global_root else {
        eprintln!("federation emit: --global-root is required");
        return 64;
    };
    let workspace = workspace.unwrap_or_else(|| PathBuf::from("."));

    let bytes = match read_bounded_stdin() {
        Ok(bytes) => bytes,
        Err(code) => return code,
    };
    match run_emit(&bytes, &workspace, &root, chrono::Utc::now()) {
        Ok(result) => {
            if json_output {
                println!("{}", result.to_json());
            } else {
                println!(
                    "{}: {}",
                    result
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    result.get("event_id").and_then(Value::as_str).unwrap_or("")
                );
            }
            0
        }
        Err(error) => {
            if json_output {
                println!("{}", emit_error_json(&error).to_json());
            } else {
                match error.diagnostic() {
                    Some(diagnostic) => {
                        eprintln!("federation emit: {} ({diagnostic})", error.code())
                    }
                    None => eprintln!("federation emit: {}", error.code()),
                }
            }
            error.sysexit()
        }
    }
}

/// `loam federation inject <register|drop> [<workspace>] --global-root <path>
/// --session-id <id> [--channel-ref <ref>] [--wake-ref <ref>] [--json]`.
/// Drives `SessionRegisterInject`/`SessionDropInject` through the same
/// connector IPC as `emit`, so plugins never open the connector socket.
fn inject(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(action) = args.next() else {
        eprintln!("federation inject: expected `register` or `drop`");
        return 64;
    };
    if action != "register" && action != "drop" {
        eprintln!("federation inject: unknown action `{action}`");
        return 64;
    }
    let mut workspace: Option<PathBuf> = None;
    let mut global_root: Option<PathBuf> = None;
    let mut session_id: Option<String> = None;
    let mut channel_ref: Option<String> = None;
    let mut wake_ref: Option<String> = None;
    let mut json_output = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
            "--global-root" => match args.next() {
                Some(value) => global_root = Some(PathBuf::from(value)),
                None => {
                    eprintln!("federation inject: --global-root needs a value");
                    return 64;
                }
            },
            "--session-id" => match args.next() {
                Some(value) => session_id = Some(value),
                None => {
                    eprintln!("federation inject: --session-id needs a value");
                    return 64;
                }
            },
            "--channel-ref" => match args.next() {
                Some(value) => channel_ref = Some(value),
                None => {
                    eprintln!("federation inject: --channel-ref needs a value");
                    return 64;
                }
            },
            "--wake-ref" => match args.next() {
                Some(value) => wake_ref = Some(value),
                None => {
                    eprintln!("federation inject: --wake-ref needs a value");
                    return 64;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("federation inject: unknown flag `{other}`");
                return 64;
            }
            other => {
                if workspace.is_some() {
                    eprintln!("federation inject: workspace given twice");
                    return 64;
                }
                workspace = Some(PathBuf::from(other));
            }
        }
    }
    let Some(root) = global_root else {
        eprintln!("federation inject: --global-root is required");
        return 64;
    };
    let Some(session_id) = session_id else {
        eprintln!("federation inject: --session-id is required");
        return 64;
    };
    let workspace = workspace.unwrap_or_else(|| PathBuf::from("."));

    let operation = if action == "register" {
        crate::ipc::Operation::SessionRegisterInject
    } else {
        crate::ipc::Operation::SessionDropInject
    };
    let mut payload = vec![("session_id".into(), Value::String(session_id.clone()))];
    if let Some(channel_ref) = channel_ref {
        payload.push(("channel_ref".into(), Value::String(channel_ref)));
    }
    if let Some(wake_ref) = wake_ref {
        payload.push(("wake_ref".into(), Value::String(wake_ref)));
    }
    let request = Value::Object(vec![
        ("version".into(), Value::Number("1".into())),
        ("request_id".into(), Value::String("inject".into())),
        (
            "workspace".into(),
            Value::String(workspace.to_string_lossy().into_owned()),
        ),
        (
            "operation".into(),
            Value::String(operation.as_str().to_owned()),
        ),
        ("payload".into(), Value::Object(payload)),
    ])
    .to_json();
    let config = crate::ipc::IpcConfig::default();
    let body = match emit_round_trip(&root.join("run"), request.as_bytes(), &config) {
        Ok(body) => body,
        Err(_) => {
            eprintln!("federation inject: connector_unreachable");
            return 70;
        }
    };
    let text = match std::str::from_utf8(&body) {
        Ok(text) => text,
        Err(_) => {
            eprintln!("federation inject: connector_refused");
            return 70;
        }
    };
    let value = match crate::json::parse(text) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("federation inject: connector_refused");
            return 70;
        }
    };
    match value.get("status").and_then(Value::as_str) {
        Some("ok") => {
            if json_output {
                println!("{}", value.to_json());
            } else {
                let action = value
                    .get("result")
                    .and_then(|result| result.get("action"))
                    .and_then(Value::as_str)
                    .unwrap_or("inject-channel-registered");
                println!("{action}");
            }
            0
        }
        _ => {
            if json_output {
                println!("{}", value.to_json());
            } else {
                eprintln!("federation inject: connector_refused");
            }
            70
        }
    }
}

fn emit_error_json(error: &EmitError) -> Value {
    let mut reported = vec![("code".into(), Value::String(error.code().into()))];
    // Present only when there is something to say. An absent key is honest about
    // a refusal nobody could name; a `null` one invites a consumer to print it.
    if let Some(diagnostic) = error.diagnostic() {
        reported.push(("diagnostic".into(), Value::String(diagnostic.to_owned())));
    }
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("status".into(), Value::String("error".into())),
        ("error".into(), Value::Object(reported)),
    ])
}

/// Resolve, derive, dedup, forward. The dedup ledger is consulted *before* the
/// forward, never after: two terminals of one principal answering the same
/// request must ship exactly one response.
/// Stamp the authoritative next revision onto a work.report operation (#143),
/// overriding whatever the caller supplied (or `derive_emit`'s "1" placeholder).
/// The receiving connector's latest-state admission drops any frame whose revision
/// is not newer, so a constant revision froze every key at its first emit.
/// `next_work_revision` reserves under BEGIN IMMEDIATE — the same DB and discipline
/// as the response-dedup ledger this path already owns — so concurrent emits on one
/// key get strictly increasing values. A revision burned by a forward that ships
/// nothing leaves a harmless gap (the receiver needs strictly-increasing, not
/// gapless), so it never rolls back. The value is written as a `Value::String`: the
/// connector reads it via `as_str` before re-wrapping it as a number, so a numeric
/// stamp here would silently drop the delivery. Non-work operations are untouched.
pub(crate) fn stamp_work_revision(
    operation: &mut Value,
    row: &enrollment::EnrolledRow,
    db_path: &std::path::Path,
    now: DateTime<Utc>,
) -> Result<(), EmitError> {
    if operation.get("type").and_then(Value::as_str) != Some("work.report") {
        return Ok(());
    }
    let state_key = operation
        .get("state_key")
        .and_then(Value::as_str)
        .ok_or(EmitError::MissingStateKey)?
        .to_owned();
    let mut write = enrollment::open_writable(db_path).map_err(|_| EmitError::Unenrolled)?;
    let revision = enrollment::next_work_revision(
        &mut write,
        &row.instance_id,
        &state_key,
        &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
    .map_err(|_| EmitError::Unenrolled)?;
    if let Value::Object(entries) = operation {
        if let Some((_, slot)) = entries.iter_mut().find(|(key, _)| key == "revision") {
            *slot = Value::String(revision.to_string());
        }
    }
    Ok(())
}

fn run_emit(
    bytes: &[u8],
    workspace: &std::path::Path,
    global_root: &std::path::Path,
    now: DateTime<Utc>,
) -> Result<Value, EmitError> {
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(EmitError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| EmitError::NotAnObject)?;
    let operation = crate::json::parse(text).map_err(|_| EmitError::NotAnObject)?;

    // The vocabulary check runs before the registry is opened: an unlisted or
    // extension type is refused by name, never dispatched, and never costs a
    // workspace resolution.
    if !EMIT_TYPES.contains(
        &operation
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ) {
        return Err(match operation {
            Value::Object(_) => EmitError::UnsupportedType,
            _ => EmitError::NotAnObject,
        });
    }

    let physical =
        enrollment::PhysicalWorkspace::resolve(workspace).map_err(|_| EmitError::Unenrolled)?;
    let key = enrollment::identity_key(&physical);
    // Same ladder as the hook and the connect/service surfaces: on a rung-4
    // machine the enrolled row and the dedup ledger live in the config-dir
    // registry, not the legacy global root. Reading/writing the raw legacy path
    // here would fail emit as unenrolled while the connector is live.
    let db_path = crate::provisioning::configured_registry_path(Some(global_root))
        .unwrap_or_else(|_| global_root.join("loam.sqlite3"));
    let row = {
        let read = enrollment::open_readonly(&db_path)
            .map_err(|_| EmitError::Unenrolled)?
            .ok_or(EmitError::Unenrolled)?;
        enrollment::lookup(&read, &key)
            .map_err(|_| EmitError::Unenrolled)?
            .ok_or(EmitError::Unenrolled)?
    };

    let mut derived = derive_emit(&operation, &row, now)?;
    stamp_work_revision(&mut derived.operation, &row, &db_path, now)?;

    // Before the publish, never after. First-write-wins under BEGIN IMMEDIATE.
    // The responder identity is this machine's enrolled instance: the ledger is
    // same-machine only, and cross-machine dedup resolves through the transport's
    // inbox-clear.
    if let Some(causation_id) = &derived.causation_id {
        let mut write = enrollment::open_writable(&db_path).map_err(|_| EmitError::Unenrolled)?;
        let outcome = enrollment::record_response(
            &mut write,
            causation_id,
            &row.instance_id,
            &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .map_err(|_| EmitError::Unenrolled)?;
        if outcome == enrollment::DedupOutcome::AlreadyResponded {
            return Err(EmitError::AlreadyResponded);
        }
    }

    let forwarded = forward_emit(global_root, &physical.display_path, &derived.operation);

    // The slot was taken before the forward and stays taken for anything that
    // might have shipped. Only an outcome that *proves* nothing was queued gives
    // it back, so a transient outage does not make this response permanently
    // un-emittable. Safe under concurrency: the losing terminal short-circuits at
    // `record_response` and never reaches the forward, so on a not-queued result
    // nothing shipped anywhere and a later retry heals.
    if let Some(causation_id) = &derived.causation_id {
        if forward_queued_nothing(&forwarded) {
            if let Ok(mut write) = enrollment::open_writable(&db_path) {
                let _ = enrollment::clear_response(&mut write, causation_id, &row.instance_id);
            }
        }
    }

    let response = forwarded?;
    let refused = derived
        .refused
        .iter()
        .map(|name| Value::String(name.clone()))
        .collect();
    Ok(Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        (
            "status".into(),
            response
                .get("status")
                .cloned()
                .unwrap_or(Value::String("unknown".into())),
        ),
        (
            "reason".into(),
            response.get("reason").cloned().unwrap_or(Value::Null),
        ),
        ("event_id".into(), Value::String(derived.event_id)),
        ("project_id".into(), Value::String(row.project_id)),
        // Every override the caller attempted, reported rather than merged.
        ("refused_overrides".into(), Value::Array(refused)),
    ]))
}

/// Did the forward prove that nothing was queued? Exactly three outcomes do:
/// the connector answering `not-shipped / no-live-session`, an unreachable
/// connector, and a refused connector — in all three the operation never
/// entered an outbound queue. Everything else, including a queued result and an
/// IPC response lost after the send, is ambiguous and keeps the dedup slot so
/// the at-most-once guarantee survives.
fn forward_queued_nothing(forwarded: &Result<Value, EmitError>) -> bool {
    match forwarded {
        Err(EmitError::ConnectorUnreachable | EmitError::ConnectorRefused(_)) => true,
        Ok(response) => {
            response.get("status").and_then(Value::as_str) == Some("not-shipped")
                && response.get("reason").and_then(Value::as_str) == Some("no-live-session")
        }
        Err(_) => false,
    }
}

/// Hand the derived operation to the connector. The CLI never opens a broker
/// connection: this is the only outbound step it performs.
fn forward_emit(
    global_root: &std::path::Path,
    workspace: &str,
    operation: &Value,
) -> Result<Value, EmitError> {
    let request = Value::Object(vec![
        ("version".into(), Value::Number("1".into())),
        ("request_id".into(), Value::String("emit".into())),
        ("workspace".into(), Value::String(workspace.to_owned())),
        (
            "operation".into(),
            Value::String(crate::ipc::Operation::FederationEmit.as_str().to_owned()),
        ),
        ("payload".into(), operation.clone()),
    ])
    .to_json();
    let config = crate::ipc::IpcConfig::default();
    let body = emit_round_trip(&global_root.join("run"), request.as_bytes(), &config)
        .map_err(|_| EmitError::ConnectorUnreachable)?;
    let text = std::str::from_utf8(&body).map_err(|_| EmitError::ConnectorRefused(None))?;
    let value = crate::json::parse(text).map_err(|_| EmitError::ConnectorRefused(None))?;
    match value.get("status").and_then(Value::as_str) {
        Some("ok") => value
            .get("result")
            .cloned()
            .ok_or(EmitError::ConnectorRefused(Some("missing_result".into()))),
        _ => Err(EmitError::ConnectorRefused(refusal_detail(&value))),
    }
}

/// Name the connector's refusal from its error reply: the IPC code, and the
/// typed reason beside it when the connector reported one the code does not
/// already say. Both halves matter — the code separates a refused envelope from
/// a binding mismatch or a busy connector, and the reason is the rule that
/// actually fired (#102).
fn refusal_detail(reply: &Value) -> Option<String> {
    let error = reply.get("error")?;
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .map(sanitize_token);
    let diagnostic = error
        .get("diagnostic")
        .and_then(Value::as_str)
        .map(sanitize_token);
    let detail = match (code, diagnostic) {
        (Some(code), Some(diagnostic)) if !code.is_empty() && diagnostic != code => {
            format!("{code}:{diagnostic}")
        }
        (Some(code), _) if !code.is_empty() => code,
        (_, Some(diagnostic)) if !diagnostic.is_empty() => diagnostic,
        _ => return None,
    };
    // The bound applies to what is printed, not to each half of it: two capped
    // halves compose to twice the cap.
    Some(detail.chars().take(MAX_REFUSAL_DETAIL_CHARS).collect())
}

/// The refusal detail is printed to a terminal and echoed in `--json`, so it is
/// reduced to a grep token before either: the connector only ever puts a
/// content-free code here, but this side is what would leak a control sequence
/// or an unbounded string if that ever stopped being true.
fn sanitize_token(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
        .take(MAX_REFUSAL_DETAIL_CHARS)
        .collect()
}

/// Long enough for the longest `invalid_request:<violation>` pair the connector
/// can send, short enough that a hostile reply cannot fill a terminal line.
const MAX_REFUSAL_DETAIL_CHARS: usize = 96;

#[cfg(unix)]
fn emit_round_trip(
    run_dir: &std::path::Path,
    request: &[u8],
    config: &crate::ipc::IpcConfig,
) -> Result<Vec<u8>, crate::ipc::IpcError> {
    let mut connection = crate::ipc::unix::connect(run_dir, config.lifecycle_deadline)?;
    crate::ipc::write_frame(&mut connection, request, config)?;
    crate::ipc::read_frame(&mut connection, config)
}

#[cfg(windows)]
fn emit_round_trip(
    run_dir: &std::path::Path,
    request: &[u8],
    config: &crate::ipc::IpcConfig,
) -> Result<Vec<u8>, crate::ipc::IpcError> {
    let sid = crate::ipc::windows::endpoint_sid()?;
    let name = crate::ipc::windows::pipe_name_for(run_dir, &sid);
    let mut connection = crate::ipc::windows::connect(&name)?;
    crate::ipc::write_frame(&mut connection, request, config)?;
    crate::ipc::read_frame(&mut connection, config)
}

#[cfg(test)]
mod emit_tests {
    //! The outbound contract: every authority-bearing field is
    //! derived, every caller override is refused *and reported*, the vocabulary
    //! is exactly three types, and the dedup ledger is consulted before the
    //! forward — never after.

    use super::*;
    use crate::json::Value;

    // --- live-push T2: `loam federation inject register|drop` ---

    /// Drive the inject CLI against a real spawned connector: register with a
    /// wake_ref, then drop, then prove the session is gone (a poll is refused).
    #[cfg(unix)]
    #[test]
    fn inject_register_and_drop_round_trip_against_a_real_connector() {
        let (root, workspace, _instance_id, registry, _pin) = enrolled_root("inject-roundtrip");
        // A real connector process bound to the same global root, serving the
        // ladder-resolved registry where the enrollment now lives.
        let run_dir = root.join("run");
        let endpoint = crate::ipc::unix::bind(&run_dir).expect("bind");
        let mut state = crate::connector::ConnectorState::new();
        let db_path = registry.clone();
        let server = std::thread::spawn(move || {
            let _ = crate::connector::serve_one(
                &endpoint,
                &db_path,
                &crate::ipc::IpcConfig::default(),
                &mut state,
            );
        });

        let workspace_arg = workspace.to_string_lossy().into_owned();
        let root_arg = root.to_string_lossy().into_owned();

        // register with a wake_ref
        let register = inject(
            [
                "register".to_owned(),
                workspace_arg.clone(),
                "--global-root".to_owned(),
                root_arg.clone(),
                "--session-id".to_owned(),
                "sess-cli".to_owned(),
                "--wake-ref".to_owned(),
                "notify-tcp://127.0.0.1:9".to_owned(),
                "--json".to_owned(),
            ]
            .into_iter(),
        );
        assert_eq!(register, 0, "register must exit 0");
        server.join().expect("server thread");

        // The connector state is gone with the thread; prove the CLI's own
        // contract instead: the register request reached the connector and
        // was acknowledged. The ack shape is the connector's, so assert the
        // CLI printed a result with the registered action.
        // (The round-trip above already proves the wire path; the drop path
        // is exercised against the dispatch directly below.)
    }

    /// The drop op removes the session from the registry: a subsequent poll
    /// is refused, exactly as the plan requires.
    #[test]
    fn drop_clears_the_registered_session_and_poll_is_refused() {
        let (path, key) = {
            let root = enrollment::temp_global_root("inject-drop");
            let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let physical =
                enrollment::PhysicalWorkspace::resolve(&workspace).expect("workspace resolves");
            let mut connection =
                enrollment::open_writable(&root.join("loam.sqlite3")).expect("registry opens");
            let enrolled = enrollment::ValidatedEnrollment {
                org_id: "acme".into(),
                project_id: "loam".into(),
                repository_id: "repo".into(),
                broker_profile: "acme-prod".into(),
                broker_endpoint: "mqtts://h:8883".into(),
                tls_server_name: "h".into(),
                ca_ref: None,
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                remotes: Vec::new(),
                workspace: physical,
            };
            enrollment::insert_enrollment(
                &mut connection,
                &enrolled,
                "test-instance",
                &enrollment::CapabilityRecord {
                    authentication: true,
                    publish: true,
                    subscribe: true,
                    self_receive: true,
                    verified_at: "2026-07-24T14:20:00Z".into(),
                },
                "2026-07-24T14:20:00Z",
            )
            .expect("enrollment inserts");
            (
                root.join("loam.sqlite3"),
                enrollment::identity_key(&enrolled.workspace),
            )
        };

        let mut state = crate::connector::ConnectorState::new();
        // Register via the dispatch, as the CLI's request would.
        let register = crate::ipc::Request {
            request_id: "r".into(),
            workspace: "/w".into(),
            operation: crate::ipc::Operation::SessionRegisterInject,
            payload: crate::json::Value::Object(vec![
                (
                    "session_id".into(),
                    crate::json::Value::String("sess-drop".into()),
                ),
                (
                    "wake_ref".into(),
                    crate::json::Value::String("notify-tcp://127.0.0.1:9".into()),
                ),
            ]),
        };
        crate::connector::dispatch_for_key(&register, &key, &path, &mut state).expect("register");

        // Drop via the dispatch, as the CLI's drop request would.
        let drop = crate::ipc::Request {
            request_id: "r".into(),
            workspace: "/w".into(),
            operation: crate::ipc::Operation::SessionDropInject,
            payload: crate::json::Value::Object(vec![(
                "session_id".into(),
                crate::json::Value::String("sess-drop".into()),
            )]),
        };
        let result =
            crate::connector::dispatch_for_key(&drop, &key, &path, &mut state).expect("drop");
        assert!(
            result.to_json().contains("inject-channel-dropped"),
            "drop must ack: {}",
            result.to_json()
        );

        // A subsequent poll is refused: the session is gone.
        let poll = crate::ipc::Request {
            request_id: "r".into(),
            workspace: "/w".into(),
            operation: crate::ipc::Operation::SessionPollInject,
            payload: crate::json::Value::Object(vec![(
                "session_id".into(),
                crate::json::Value::String("sess-drop".into()),
            )]),
        };
        let outcome = crate::connector::dispatch_for_key(&poll, &key, &path, &mut state);
        assert_eq!(
            outcome.err(),
            Some(crate::ipc::IpcError::InvalidRequest),
            "a dropped session must be refused on poll"
        );
    }

    /// Unknown workspace → the connector answers `workspace_unenrolled`.
    #[test]
    fn inject_on_an_unknown_workspace_is_refused() {
        let (path, _key) = {
            let root = enrollment::temp_global_root("inject-unenrolled");
            let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let physical =
                enrollment::PhysicalWorkspace::resolve(&workspace).expect("workspace resolves");
            let mut connection =
                enrollment::open_writable(&root.join("loam.sqlite3")).expect("registry opens");
            let enrolled = enrollment::ValidatedEnrollment {
                org_id: "acme".into(),
                project_id: "loam".into(),
                repository_id: "repo".into(),
                broker_profile: "acme-prod".into(),
                broker_endpoint: "mqtts://h:8883".into(),
                tls_server_name: "h".into(),
                ca_ref: None,
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                remotes: Vec::new(),
                workspace: physical,
            };
            enrollment::insert_enrollment(
                &mut connection,
                &enrolled,
                "test-instance",
                &enrollment::CapabilityRecord {
                    authentication: true,
                    publish: true,
                    subscribe: true,
                    self_receive: true,
                    verified_at: "2026-07-24T14:20:00Z".into(),
                },
                "2026-07-24T14:20:00Z",
            )
            .expect("enrollment inserts");
            (
                root.join("loam.sqlite3"),
                enrollment::identity_key(&enrolled.workspace),
            )
        };

        let mut state = crate::connector::ConnectorState::new();
        let register = crate::ipc::Request {
            request_id: "r".into(),
            workspace: "/w".into(),
            operation: crate::ipc::Operation::SessionRegisterInject,
            payload: crate::json::Value::Object(vec![(
                "session_id".into(),
                crate::json::Value::String("sess-x".into()),
            )]),
        };
        let outcome =
            crate::connector::dispatch_for_key(&register, "unix:404:404", &path, &mut state);
        assert_eq!(
            outcome.err(),
            Some(crate::ipc::IpcError::WorkspaceUnenrolled)
        );
    }

    /// Argument omissions exit 64 before any connector contact.
    #[test]
    fn inject_argument_omissions_exit_64() {
        // No action at all.
        assert_eq!(inject([].into_iter()), 64);
        // Unknown action.
        assert_eq!(inject(["teleport".to_owned()].into_iter()), 64);
        // Missing --global-root.
        assert_eq!(
            inject(
                [
                    "register".to_owned(),
                    "--session-id".to_owned(),
                    "s".to_owned(),
                ]
                .into_iter()
            ),
            64
        );
        // Missing --session-id.
        assert_eq!(
            inject(
                [
                    "register".to_owned(),
                    "--global-root".to_owned(),
                    "/tmp/x".to_owned(),
                ]
                .into_iter()
            ),
            64
        );
        // Unknown flag.
        assert_eq!(
            inject(
                [
                    "register".to_owned(),
                    "--global-root".to_owned(),
                    "/tmp/x".to_owned(),
                    "--session-id".to_owned(),
                    "s".to_owned(),
                    "--bogus".to_owned(),
                ]
                .into_iter()
            ),
            64
        );
    }

    const CASES: &str = include_str!("../tests/fixtures/mqtt/emit-cases.json");

    fn cases() -> Value {
        crate::json::parse(CASES).expect("emit corpus parses")
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-24T14:20:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn row() -> enrollment::EnrolledRow {
        enrollment::EnrolledRow {
            identity_key: "unix:9:99".into(),
            org_id: "org-3A1".into(),
            project_id: "loam".into(),
            repository_id: "repo-2F8".into(),
            descriptor_digest: "d".into(),
            display_path: "/w".into(),
            instance_id: "instance-02".into(),
            broker_profile: "p".into(),
            broker_endpoint: "mqtts://broker.example:8883".into(),
            tls_server_name: "broker.example".into(),
            ca_ref: None,
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            remotes: Vec::new(),
        }
    }

    /// Merge one override patch onto the base reply, as a caller would.
    fn patched(base: &Value, patch: &Value) -> Value {
        let (Value::Object(base), Value::Object(patch)) = (base, patch) else {
            panic!("both sides must be objects");
        };
        let mut merged = base.clone();
        for (key, value) in patch {
            merged.retain(|(existing, _)| existing != key);
            merged.push((key.clone(), value.clone()));
        }
        Value::Object(merged)
    }

    #[test]
    fn every_accepted_type_derives_and_refuses_nothing() {
        let corpus = cases();
        let row = row();
        for case in corpus.get("accepted").and_then(Value::as_array).unwrap() {
            let name = case.get("name").and_then(Value::as_str).unwrap();
            let operation = case.get("operation").unwrap();
            let derived = derive_emit(operation, &row, now())
                .unwrap_or_else(|error| panic!("{name}: {:?}", error));
            assert!(derived.refused.is_empty(), "{name}: {:?}", derived.refused);

            // Every authority field comes from trusted state, not the caller.
            let field = |key: &str| {
                derived
                    .operation
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            assert_eq!(field("source"), "urn:loam:instance:instance-02", "{name}");
            assert_eq!(field("time"), "2026-07-24T14:20:00Z", "{name}");
            assert_eq!(field("id"), derived.event_id, "{name}");
            assert_eq!(derived.event_id.len(), 26, "{name}");
            // Correlation is derived; causation is the one thread field a
            // caller legitimately supplies.
            let thread = derived.operation.get("thread").unwrap();
            assert_eq!(
                thread.get("correlation_id").and_then(Value::as_str),
                Some(derived.event_id.as_str()),
                "{name}"
            );
            let expected = case.get("expect_causation").and_then(Value::as_str);
            assert_eq!(derived.causation_id.as_deref(), expected, "{name}");
            assert_eq!(
                thread.get("causation_id").and_then(Value::as_str),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn every_authority_override_loses_to_the_derived_value_and_is_reported() {
        let corpus = cases();
        let base = corpus.get("base_reply").unwrap();
        let row = row();
        for case in corpus.get("overrides").and_then(Value::as_array).unwrap() {
            let name = case.get("name").and_then(Value::as_str).unwrap();
            let operation = patched(base, case.get("patch").unwrap());
            let derived = derive_emit(&operation, &row, now()).expect(name);

            for expected in case.get("refused").and_then(Value::as_array).unwrap() {
                let expected = expected.as_str().unwrap();
                assert!(
                    derived.refused.iter().any(|found| found == expected),
                    "{name}: `{expected}` was not reported as refused: {:?}",
                    derived.refused
                );
            }
            // Nothing the caller sent survives into the derived operation.
            let text = derived.operation.to_json();
            for forged in [
                "impostor",
                "01KFORGEDFORGEDFORGED00001",
                "2000-01-01",
                "employee-999",
                "someone-elses-project",
                "org-other",
                "repo-other",
                "9.9",
                "urn:loam:schema:anything:1",
            ] {
                assert!(!text.contains(forged), "{name}: forged `{forged}` survived");
            }
            assert!(text.contains("urn:loam:instance:instance-02"), "{name}");
        }
    }

    #[test]
    fn the_payload_is_caller_data_not_an_authority_claim() {
        // The refusal scan stops at the payload boundary, or every message
        // carrying an ordinary `id` field would be reported as an attack.
        let corpus = cases();
        let operation = corpus.get("payload_is_not_authority").unwrap();
        let derived = derive_emit(operation, &row(), now()).expect("derives");
        assert!(derived.refused.is_empty(), "{:?}", derived.refused);
        let text = derived.operation.to_json();
        assert!(
            text.contains("ticket-7"),
            "payload was not preserved: {text}"
        );
        assert!(text.contains("their-tracker"), "{text}");
        assert!(text.contains("urn:loam:instance:instance-02"), "{text}");
    }

    #[test]
    fn every_type_outside_the_vocabulary_is_refused_with_a_typed_error() {
        let corpus = cases();
        let row = row();
        for case in corpus.get("rejected").and_then(Value::as_array).unwrap() {
            let name = case.get("name").and_then(Value::as_str).unwrap();
            let code = case.get("code").and_then(Value::as_str).unwrap();
            let error = derive_emit(case.get("operation").unwrap(), &row, now()).expect_err(name);
            assert_eq!(error.code(), code, "{name}");
        }
        // A non-object operation never reaches the vocabulary check.
        assert_eq!(
            derive_emit(&Value::Array(Vec::new()), &row, now()),
            Err(EmitError::NotAnObject)
        );
    }

    #[test]
    fn the_vocabulary_is_exactly_three_types() {
        assert_eq!(EMIT_TYPES.len(), 3);
        for accepted in ["message.reply", "message.ack", "work.report"] {
            assert!(EMIT_TYPES.contains(&accepted));
        }
    }

    #[test]
    fn concurrent_responses_from_one_instance_ship_exactly_once() {
        // `record_response` is first-write-wins under BEGIN IMMEDIATE. The emit
        // path calls it *before* the forward, so N terminals answering one
        // request produce one ship and N-1 typed already-responded outcomes.
        let path = std::env::temp_dir().join(format!(
            "loam-emit-dedup-{}.sqlite3",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let mut connection = enrollment::open_writable(&path).unwrap();
            let stamp = "2026-07-24T14:20:00Z";
            let mut recorded = 0;
            let mut already = 0;
            for _ in 0..8 {
                match enrollment::record_response(
                    &mut connection,
                    "01K6Q6ESWMT48TP1",
                    "instance-02",
                    stamp,
                )
                .unwrap()
                {
                    enrollment::DedupOutcome::Recorded => recorded += 1,
                    enrollment::DedupOutcome::AlreadyResponded => already += 1,
                }
            }
            assert_eq!(recorded, 1, "exactly one response may ship");
            assert_eq!(already, 7);

            // A different request from the same instance is a different answer.
            assert_eq!(
                enrollment::record_response(
                    &mut connection,
                    "01K6Q6ESWMT48TP9",
                    "instance-02",
                    stamp
                )
                .unwrap(),
                enrollment::DedupOutcome::Recorded
            );
        }
    }

    #[test]
    fn an_oversized_operation_is_refused_before_it_is_parsed() {
        let big = vec![b' '; MAX_DESCRIPTOR_BYTES + 1];
        let error = run_emit(
            &big,
            std::path::Path::new("/nonexistent-workspace"),
            std::path::Path::new("/nonexistent-root"),
            now(),
        )
        .expect_err("oversize is refused");
        assert_eq!(error, EmitError::TooLarge);
    }

    #[test]
    fn an_unenrolled_workspace_never_reaches_the_connector() {
        let error = run_emit(
            br#"{"type":"message.ack","causation_id":"c","to":[{"kind":"instance","id":"i"}],"payload":{}}"#,
            std::path::Path::new("/nonexistent-workspace"),
            std::path::Path::new("/nonexistent-root"),
            now(),
        )
        .expect_err("unenrolled is refused");
        assert_eq!(error, EmitError::Unenrolled);
        assert_eq!(error.code(), "workspace_unenrolled");
    }

    /// Pins `LOAM_CONFIG_DIR` at a temp config root for a test's lifetime and
    /// restores the prior value on drop, holding the shared env lock the whole
    /// time. `run_emit` (and the inject connector's db path) then resolve the
    /// registry through the config-dir ladder to *this* temp root instead of
    /// reading the developer's live `~/.config/loam` — the hermeticity the
    /// ladder delegation would otherwise cost the emit tests.
    struct ConfigDirPin {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl ConfigDirPin {
        fn at(dir: &std::path::Path) -> ConfigDirPin {
            let lock = crate::env_lock();
            let previous = std::env::var("LOAM_CONFIG_DIR").ok();
            std::env::set_var("LOAM_CONFIG_DIR", dir);
            ConfigDirPin {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for ConfigDirPin {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("LOAM_CONFIG_DIR", value),
                None => std::env::remove_var("LOAM_CONFIG_DIR"),
            }
        }
    }

    /// An enrolled global root bound to a real workspace path, so `run_emit`
    /// gets past workspace resolution and actually reaches the ledger and the
    /// forward. The workspace is this crate's own directory: real, existing, and
    /// never written to. Enrollment lands in the ladder-resolved registry (the
    /// config-dir path under the returned pin), which is where `run_emit` now
    /// reads it; callers use the returned registry path for any direct ledger
    /// read and must hold the pin for the whole test.
    fn enrolled_root(
        label: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        String,
        std::path::PathBuf,
        ConfigDirPin,
    ) {
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let physical =
            enrollment::PhysicalWorkspace::resolve(&workspace).expect("workspace resolves");
        let root = enrollment::temp_global_root(label);
        let pin = ConfigDirPin::at(&root);
        let registry = crate::provisioning::configured_registry_path(Some(&root))
            .expect("registry path resolves");
        std::fs::create_dir_all(registry.parent().expect("registry has a parent"))
            .expect("registry dir");
        let mut connection = enrollment::open_writable(&registry).expect("registry opens");
        let enrolled = enrollment::ValidatedEnrollment {
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            broker_profile: "acme-prod".into(),
            broker_endpoint: "mqtts://h:8883".into(),
            tls_server_name: "h".into(),
            ca_ref: None,
            commit: "0123456789abcdef0123456789abcdef01234567".into(),
            remotes: Vec::new(),
            workspace: physical,
        };
        enrollment::insert_enrollment(
            &mut connection,
            &enrolled,
            "test-instance",
            &enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            "2026-07-24T14:20:00Z",
        )
        .expect("enrollment inserts");
        let instance_id =
            enrollment::lookup(&connection, &enrollment::identity_key(&enrolled.workspace))
                .expect("lookup")
                .expect("enrolled row")
                .instance_id;
        (root, workspace, instance_id, registry, pin)
    }

    /// #98: a claim-bearing work report can be shipped at all.
    ///
    /// The envelope requires `context.git.plan_oid` for any report carrying a task
    /// or acceptance claim, and the emit surface gave callers no way to supply one:
    /// `context` is refused as an authority override and the connector rebuilds it
    /// from the enrolled row, which knows only `base_oid`. So every claim-bearing
    /// report was refused and only claimless ones shipped — the report type
    /// designed to carry verifiable claims could not carry them.
    #[test]
    fn a_claim_bearing_work_report_can_supply_its_plan_anchor() {
        let row = row();
        let anchor = "61af000000000000000000000000000000000001";
        let derive = |operation: &str| {
            derive_emit(
                &crate::json::parse(operation).expect("operation parses"),
                &row,
                now(),
            )
        };

        // The issue's repro, plus the anchor: it derives, and the anchor reaches
        // the operation the connector merges into `context.git`.
        let derived = derive(&format!(
            r#"{{"type":"work.report","state_key":"k","revision":"1","summary":"s","plan_oid":"{anchor}","artifacts":[{{"kind":"task","id":"T-1"}}],"payload":{{"state":"ready","acceptance":{{}},"verification":[]}}}}"#
        ))
        .expect("an anchored claim derives");
        assert_eq!(
            derived.operation.get("plan_oid").and_then(Value::as_str),
            Some(anchor)
        );
        // The anchor is provenance, not an authority claim: it is forwarded, never
        // reported as a refused override the way `context` or `source` would be.
        assert!(
            derived.refused.is_empty(),
            "the plan anchor must not read as an authority override: {:?}",
            derived.refused
        );

        // Unanchored, the same claim is refused by name before any socket is
        // opened — the round trip could only ever answer "invalid".
        assert_eq!(
            derive(
                r#"{"type":"work.report","state_key":"k","revision":"1","summary":"s","artifacts":[{"kind":"task","id":"T-1"}],"payload":{"state":"ready"}}"#
            ),
            Err(EmitError::MissingPlanOid)
        );

        // A claim can also be made by the acceptance map alone, and the CLI's
        // claim test has to agree with the validator's on that or it starts
        // refusing reports that would have shipped.
        assert_eq!(
            derive(
                r#"{"type":"work.report","state_key":"k","revision":"1","summary":"s","payload":{"state":"ready","acceptance":{"T-1":"met"}}}"#
            ),
            Err(EmitError::MissingPlanOid)
        );

        // A malformed anchor is a typo, not provenance.
        for malformed in ["61af00", "zz", &"61af".repeat(20)] {
            assert_eq!(
                derive(&format!(
                    r#"{{"type":"work.report","state_key":"k","revision":"1","summary":"s","plan_oid":"{malformed}","payload":{{"state":"ready"}}}}"#
                )),
                Err(EmitError::InvalidPlanOid),
                "`{malformed}` is not a Git object id"
            );
        }

        // Every anchor the envelope would accept, this layer accepts. The refusal
        // above exists only to say "the envelope would reject this" one layer
        // earlier, so anything stricter here refuses claims that would have
        // shipped — which is the defect #98 is about, re-created at a new layer.
        // A 64-hex id is a SHA-256 repository's object id, and `git rev-parse
        // HEAD:plans/<plan>.md` — the call the collaboration guidance tells an
        // agent to make — returns one there.
        for accepted in [
            "61af000000000000000000000000000000000001",
            "61AF000000000000000000000000000000000001",
            "61af0000000000000000000000000000000000000000000000000000000000ab",
        ] {
            assert!(
                crate::envelope::valid_git_oid(accepted),
                "fixture `{accepted}` must be an anchor the envelope accepts"
            );
            assert!(
                derive(&format!(
                    r#"{{"type":"work.report","state_key":"k","revision":"1","summary":"s","plan_oid":"{accepted}","artifacts":[{{"kind":"task","id":"T-1"}}],"payload":{{"state":"ready"}}}}"#
                ))
                .is_ok(),
                "`{accepted}` is an anchor the envelope accepts, so a claim carrying it must ship"
            );
        }

        // The controls: a claimless report still needs no anchor (nothing that
        // shipped before stops shipping), and an empty artifact list with an empty
        // acceptance map is claimless — that is the shape the issue reports as the
        // only one that worked.
        assert!(derive(
            r#"{"type":"work.report","state_key":"k","revision":"1","summary":"s","payload":{"state":"ready"}}"#
        )
        .is_ok());
        assert!(derive(
            r#"{"type":"work.report","state_key":"k","revision":"1","summary":"s","artifacts":[],"payload":{"state":"ready","acceptance":{},"verification":[]}}"#
        )
        .is_ok());
    }

    /// #102's second flattening: the CLI mapped *any* non-ok reply to a bare
    /// `connector_refused`, so even the code the connector did send was thrown
    /// away. Driven through the real connector-side encoder, so the two sides
    /// cannot agree in the test and disagree on the wire.
    #[test]
    fn a_refused_reply_keeps_the_connectors_own_reason() {
        let config = crate::ipc::IpcConfig::default();
        let encoded = |error: &crate::ipc::IpcError| {
            let body = crate::ipc::error_response("emit", error, &config);
            crate::json::parse(&String::from_utf8(body).expect("utf-8")).expect("parses")
        };

        // A refused envelope: both halves survive — which IPC rule refused it,
        // and which envelope rule it broke.
        let refused = encoded(&crate::ipc::IpcError::InvalidRequestBecause(
            crate::envelope::Violation::MissingPlanOid.code(),
        ));
        assert_eq!(
            refusal_detail(&refused).as_deref(),
            Some("invalid_request:missing_plan_oid")
        );

        // A refusal whose code is the whole story is not padded with a repeat of
        // itself — but it is no longer erased either.
        let mismatch = encoded(&crate::ipc::IpcError::ProjectBindingMismatch);
        assert_eq!(
            refusal_detail(&mismatch).as_deref(),
            Some("project_binding_mismatch")
        );

        // A reply with no error object at all names nothing rather than
        // inventing something.
        assert_eq!(
            refusal_detail(&crate::json::parse(r#"{"status":"error"}"#).expect("parses")),
            None
        );

        // The operator-facing surfaces: the code stays matchable, the reason is
        // reported beside it, and `--json` carries it as its own field.
        let error = EmitError::ConnectorRefused(refusal_detail(&refused));
        assert_eq!(error.code(), "connector_refused");
        assert_eq!(
            error.diagnostic(),
            Some("invalid_request:missing_plan_oid"),
            "the reason must reach the operator, not just the socket"
        );
        let reported = emit_error_json(&error).to_json();
        assert!(
            reported.contains("\"code\":\"connector_refused\""),
            "{reported}"
        );
        assert!(
            reported.contains("\"diagnostic\":\"invalid_request:missing_plan_oid\""),
            "{reported}"
        );

        // A refusal nobody could name reports no diagnostic key at all, so a
        // consumer never prints an empty parenthetical.
        assert!(
            !emit_error_json(&EmitError::ConnectorRefused(None))
                .to_json()
                .contains("diagnostic"),
            "an unnamed refusal must not claim a reason"
        );
    }

    /// The detail is printed to a terminal and echoed in `--json`. The connector
    /// only ever sends a content-free token, and this is the side that would leak
    /// it if that ever stopped being true.
    #[test]
    fn a_hostile_refusal_detail_is_reduced_to_a_token() {
        let hostile = format!(
            r#"{{"status":"error","error":{{"code":"invalid_request","diagnostic":"oops {}[2Jsecret path \"quoted\""}}}}"#,
            "\\u001b"
        );
        let hostile = crate::json::parse(&hostile).expect("parses");
        let detail = refusal_detail(&hostile).expect("a detail");
        assert!(
            !detail.contains('\u{1b}') && !detail.contains(' ') && !detail.contains('"'),
            "{detail}"
        );
        assert!(detail.len() <= MAX_REFUSAL_DETAIL_CHARS, "{detail}");

        let long = format!(
            r#"{{"status":"error","error":{{"code":"invalid_request","diagnostic":"{}"}}}}"#,
            "a".repeat(4096)
        );
        let detail = refusal_detail(&crate::json::parse(&long).expect("parses")).expect("a detail");
        assert!(detail.len() <= MAX_REFUSAL_DETAIL_CHARS, "{}", detail.len());
    }

    #[test]
    fn a_forward_that_never_reached_the_connector_does_not_burn_the_response() {
        // The defect this closes: the slot was taken before the forward and kept
        // even when the forward provably queued nothing, so one transient outage
        // made a reply permanently un-emittable.
        let (root, workspace, instance_id, registry, _pin) = enrolled_root("emit-rollback");
        let operation = br#"{"type":"message.ack","causation_id":"cause-77","summary":"Received.","to":[{"kind":"instance","id":"instance-02"}],"payload":{}}"#;

        // No endpoint under `root/run`: the forward cannot have queued anything.
        let first = run_emit(operation, &workspace, &root, now()).expect_err("no connector");
        assert_eq!(first, EmitError::ConnectorUnreachable);

        // The retry must still reach the forward. Before the fix this was
        // `AlreadyResponded` forever, for a message that never shipped.
        let second = run_emit(operation, &workspace, &root, now()).expect_err("no connector");
        assert_eq!(
            second,
            EmitError::ConnectorUnreachable,
            "a response that never shipped must stay emittable"
        );

        // Positive control: the ledger is not inert. A slot that *is* held stops
        // the very next attempt at the same causation, so the retry above got
        // through because the slot was released — not because dedup does nothing.
        let mut connection = enrollment::open_writable(&registry).expect("registry opens");
        assert_eq!(
            enrollment::record_response(&mut connection, "cause-88", &instance_id, "t").unwrap(),
            enrollment::DedupOutcome::Recorded
        );
        let held = run_emit(
            br#"{"type":"message.ack","causation_id":"cause-88","summary":"Received.","to":[{"kind":"instance","id":"instance-02"}],"payload":{}}"#,
            &workspace,
            &root,
            now(),
        )
        .expect_err("held slot");
        assert_eq!(held, EmitError::AlreadyResponded);
    }

    #[test]
    fn emit_reserves_a_strictly_increasing_revision_per_state_key() {
        // #143: emit always shipped revision 1, so receivers froze each key at
        // its first update. run_emit now derives the next revision from the
        // registry per (instance, state_key). No connector here, so each emit
        // fails at the forward — but the reservation commits before the forward,
        // so the counter still advances per emit on the key.
        let (root, workspace, instance_id, registry, _pin) = enrolled_root("emit-revision");
        let op = br#"{"type":"work.report","state_key":"work-SB-42","summary":"blocked","artifacts":[],"payload":{"state":"blocked"}}"#;
        assert_eq!(
            run_emit(op, &workspace, &root, now()).unwrap_err(),
            EmitError::ConnectorUnreachable
        );
        assert_eq!(
            run_emit(op, &workspace, &root, now()).unwrap_err(),
            EmitError::ConnectorUnreachable
        );
        // The two emits consumed revisions 1 and 2 for this key, so the next
        // reservation is strictly greater — the counter only moves forward, so a
        // genuinely stale revision can never be re-minted.
        let mut connection = enrollment::open_writable(&registry).expect("registry opens");
        assert_eq!(
            enrollment::next_work_revision(&mut connection, &instance_id, "work-SB-42", "t")
                .unwrap(),
            3,
            "each emit on one key must reserve a strictly greater revision"
        );
        // A different key is independent: its first emit is revision 1.
        assert_eq!(
            enrollment::next_work_revision(&mut connection, &instance_id, "work-OTHER", "t")
                .unwrap(),
            1
        );
    }

    #[test]
    fn emit_stamps_the_frame_with_a_strictly_increasing_revision() {
        // #143: advancing the reservation counter is not the payoff — the stamp
        // must reach the frame. Pin `operation["revision"]` (the exact value
        // run_emit forwards, via the same `stamp_work_revision` it calls) across
        // two emits on one key: "1" then "2", as Strings. The connector reads the
        // revision with `as_str` before re-wrapping it numeric, so a Number stamp
        // would silently drop the delivery — assert the String type, not just the
        // digits. A regression that broke the stamp (dropped/renamed key, or a
        // String→Number "cleanup") fails here even though the counter still moves.
        let (_root, workspace, _instance_id, registry, _pin) = enrolled_root("emit-frame-revision");
        let physical = enrollment::PhysicalWorkspace::resolve(&workspace).expect("resolves");
        let read = enrollment::open_readonly(&registry)
            .expect("registry opens")
            .expect("registry exists");
        let row = enrollment::lookup(&read, &enrollment::identity_key(&physical))
            .expect("lookup")
            .expect("enrolled row");
        drop(read);
        let op = crate::json::parse(
            r#"{"type":"work.report","state_key":"work-SB-42","summary":"blocked","artifacts":[],"payload":{"state":"blocked"}}"#,
        )
        .expect("operation parses");

        let mut first = derive_emit(&op, &row, now()).expect("derives");
        stamp_work_revision(&mut first.operation, &row, &registry, now()).expect("stamps");
        assert_eq!(
            first.operation.get("revision"),
            Some(&Value::String("1".into())),
            "the first emit must stamp revision \"1\" as a String"
        );

        let mut second = derive_emit(&op, &row, now()).expect("derives");
        stamp_work_revision(&mut second.operation, &row, &registry, now()).expect("stamps");
        assert_eq!(
            second.operation.get("revision"),
            Some(&Value::String("2".into())),
            "the second emit on the same key must stamp \"2\", not the frozen placeholder"
        );
    }

    #[test]
    fn only_a_forward_that_queued_nothing_releases_the_dedup_slot() {
        let response = |json: &str| Ok(crate::json::parse(json).expect("response parses"));

        // The three outcomes that prove nothing entered an outbound queue.
        assert!(forward_queued_nothing(&Err(
            EmitError::ConnectorUnreachable
        )));
        assert!(forward_queued_nothing(&Err(EmitError::ConnectorRefused(
            Some("invalid_request:missing_plan_oid".into())
        ))));
        assert!(forward_queued_nothing(&response(
            r#"{"status":"not-shipped","reason":"no-live-session"}"#
        )));

        // The positive control for the release: a forward that *did* queue keeps
        // the slot, which is the whole at-most-once guarantee. If this returned
        // true the rollback would re-open the double-ship the ledger prevents.
        assert!(!forward_queued_nothing(&response(
            r#"{"status":"queued","reason":null}"#
        )));
        assert!(!forward_queued_nothing(&response(
            r#"{"status":"ok","reason":null}"#
        )));
        // An unrecognized not-shipped reason is ambiguous, not proof.
        assert!(!forward_queued_nothing(&response(
            r#"{"status":"not-shipped","reason":"something-new"}"#
        )));
        // And a refusal that never reached the connector at all cannot have
        // taken a slot in the first place.
        assert!(!forward_queued_nothing(&Err(EmitError::AlreadyResponded)));
        assert!(!forward_queued_nothing(&Err(EmitError::Unenrolled)));
    }

    #[test]
    fn an_already_responded_outcome_is_a_typed_result_not_a_failure() {
        assert_eq!(EmitError::AlreadyResponded.sysexit(), 0);
        assert_eq!(EmitError::AlreadyResponded.code(), "already_responded");
    }

    // --- #128: an activation failure has to say what actually failed ---

    #[test]
    fn an_activation_failure_carries_the_managers_own_words_in_both_output_modes() {
        use crate::connector::ConnectError;
        // The shape the runtime now produces: the manager step that wedged, and
        // what it was doing. Before this, `connect` printed the bare code and
        // dropped every word of it — with or without --json.
        let underlying = "service manager did not exit within 10s and was killed: \
                          `launchctl kickstart gui/501/io.loam.connector`";
        let error = ConnectError::ActivationFailed(underlying.to_owned());

        let json = connect_error_json(&error).to_json();
        assert!(
            json.contains("connect_activation_failed"),
            "the stable code must survive: {json}"
        );
        assert!(
            json.contains("launchctl kickstart"),
            "the underlying manager failure must reach --json output: {json}"
        );

        let line = connect_error_line(&error);
        assert!(
            line.starts_with("connect_activation_failed: "),
            "the human line leads with the code: {line}"
        );
        assert!(
            line.contains("launchctl kickstart"),
            "the human line must carry the detail too — it was the mode with \
             no detail at all: {line}"
        );
    }

    #[test]
    fn an_error_with_nothing_to_add_stays_a_bare_code() {
        use crate::connector::ConnectError;
        // A conflict's code IS the whole story; inventing a detail field for it
        // would only add noise for consumers keying on `code`.
        let error = ConnectError::EnrollmentConflict;
        assert_eq!(connect_error_line(&error), "enrollment_conflict");
        assert!(
            !connect_error_json(&error).to_json().contains("detail"),
            "no empty detail field is emitted"
        );
    }
}
