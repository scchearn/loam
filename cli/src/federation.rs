//! `loam federation` CLI orchestration and typed command errors.
//!
//! Wiring the `connect` descriptor + workspace validation path only:
//! read one bounded descriptor from stdin, validate it, and, when a workspace is
//! given, prove physical identity, remote digests, and commit reachability. The
//! enrollment probe, transactional registry commit, and service activation are
//! added by later tasks (T10/T11); `disconnect` and `status` are stubs until
//! then. No credential is resolved and no `AuthenticatedPrincipal` is built here.

use std::io::Read;
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
        _ => {
            eprintln!(
                "Usage:\n  \
                 loam federation connect [<workspace>] --json   (reads one descriptor on stdin)\n  \
                 loam federation disconnect <workspace> --global-root <path> [--json]\n  \
                 loam federation status [<workspace>] --global-root <path> [--json]\n  \
                 loam federation emit [<workspace>] --global-root <path> [--json]   (reads one operation on stdin)"
            );
            64
        }
    }
}

/// Build the connector's service context (stable identity + absolute runtime) so
/// disconnect/status can drive the real per-user manager. Shared by both.
fn service_context(root: &std::path::Path) -> Result<crate::service::ServiceContext, i32> {
    let instance_id = crate::service::ensure_instance_id(root).map_err(|error| {
        eprintln!("federation: {error}");
        70
    })?;
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
    let db_path = root.join("loam.sqlite3");
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
    let db_path = root.join("loam.sqlite3");
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
/// service context (stable instance identity + the absolute current runtime) and
/// drives the real per-user manager. `install`/`status`/`disable` never start the
/// connector or contact a broker; `enable` re-asserts active desired state on the
/// current runtime after a runtime-path update (setup delegates this, T12).
fn service_lifecycle(root: &std::path::Path, action: ServiceAction) -> i32 {
    let instance_id = match crate::service::ensure_instance_id(root) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("federation service: {error}");
            return 70;
        }
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

fn connect(mut args: impl Iterator<Item = String>) -> i32 {
    let mut workspace: Option<PathBuf> = None;
    let mut global_root: Option<PathBuf> = None;
    let mut json_output = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json_output = true,
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
            other => {
                if workspace.is_some() {
                    eprintln!("federation connect: workspace given twice");
                    return 64;
                }
                workspace = Some(PathBuf::from(other));
            }
        }
    }

    let descriptor_bytes = match read_bounded_stdin() {
        Ok(bytes) => bytes,
        Err(code) => return code,
    };

    let enrolled = match validate(&descriptor_bytes, workspace.as_deref()) {
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

    match global_root {
        // No global root: validation-only (the descriptor + workspace proof).
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
        // With a global root: the full transactional connect (T10). The transport
        // is the deterministic stub until the real broker adapter lands (T13).
        Some(root) => orchestrate_cli(&enrolled, &root, json_output),
    }
}

/// Drive the T10 transactional connect from the CLI: derive the connector's
/// service context and identity, run the probe/commit/activate orchestration
/// against the stub transport, and report the outcome.
fn orchestrate_cli(
    enrolled: &enrollment::ValidatedEnrollment,
    root: &std::path::Path,
    json_output: bool,
) -> i32 {
    let instance_id = match crate::service::ensure_instance_id(root) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("federation connect: {error}");
            return 70;
        }
    };
    let runtime_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => {
            eprintln!("federation connect: cannot resolve the current runtime path");
            return 70;
        }
    };
    let context = crate::service::ServiceContext {
        global_root: root.to_path_buf(),
        instance_id: instance_id.clone(),
        runtime_path,
        systemd_user_dir: systemd_user_dir(),
    };
    let runner = crate::service::RealRunner;
    let db_path = root.join("loam.sqlite3");
    let now = chrono::Utc::now();

    // T13 — probe against the REAL broker, not the stub. Build the session inputs
    // from the validated descriptor plus this machine's instance id, exactly as
    // `provisioning::resolve` builds them from the committed row, so the enrollment
    // probe authenticates over mTLS, subscribes, publishes, and requires its own
    // echoed event *before* the row is committed and the service activated. The
    // transport alone learns the canonical principal from the certificate.
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
        credential_ref: enrolled.credential_ref.clone(),
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
                                Value::String(error.code().into())
                            )]),
                        ),
                    ])
                    .to_json()
                );
            } else {
                eprintln!("federation connect: {}", error.code());
            }
            connect_sysexit(&error)
        }
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

/// Parse the descriptor, then — only when a workspace path is supplied — run the
/// full physical-identity, remote-digest, and reachability proof. Descriptor
/// validation always runs first so a malformed descriptor is rejected before any
/// filesystem or Git access.
fn validate(
    bytes: &[u8],
    workspace: Option<&std::path::Path>,
) -> Result<enrollment::ValidatedEnrollment, EnrollmentError> {
    let descriptor = enrollment::parse_descriptor(bytes)?;
    let workspace = workspace.unwrap_or_else(|| std::path::Path::new("."));
    enrollment::validate_enrollment(descriptor, workspace)
}

/// Read stdin with a hard ceiling one byte over the descriptor limit, so an
/// oversized document is detected as [`EnrollmentError::TooLarge`] rather than
/// buffered unbounded.
fn read_bounded_stdin() -> Result<Vec<u8>, i32> {
    let mut buffer = Vec::new();
    let limit = (MAX_DESCRIPTOR_BYTES + 1) as u64;
    let mut handle = std::io::stdin().lock().take(limit);
    if handle.read_to_end(&mut buffer).is_err() {
        eprintln!("federation connect: could not read descriptor from stdin");
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
    Unenrolled,
    AlreadyResponded,
    ConnectorUnreachable,
    ConnectorRefused,
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
            EmitError::Unenrolled => "workspace_unenrolled",
            EmitError::AlreadyResponded => "already_responded",
            EmitError::ConnectorUnreachable => "connector_unreachable",
            EmitError::ConnectorRefused => "connector_refused",
        }
    }

    fn sysexit(&self) -> i32 {
        match self {
            // An already-responded outcome is a correct, expected result, not a
            // failure: exactly one terminal ships and the others say so.
            EmitError::AlreadyResponded => 0,
            EmitError::Unenrolled => 78,
            EmitError::ConnectorUnreachable | EmitError::ConnectorRefused => 69,
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
        derived.push((
            "artifacts".into(),
            match operation.get("artifacts") {
                Some(artifacts @ Value::Array(_)) => artifacts.clone(),
                _ => Value::Array(Vec::new()),
            },
        ));
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
                eprintln!("federation emit: {}", error.code());
            }
            error.sysexit()
        }
    }
}

fn emit_error_json(error: &EmitError) -> Value {
    Value::Object(vec![
        ("schema".into(), Value::Number("1".into())),
        ("status".into(), Value::String("error".into())),
        (
            "error".into(),
            Value::Object(vec![("code".into(), Value::String(error.code().into()))]),
        ),
    ])
}

/// Resolve, derive, dedup, forward. The dedup ledger is consulted *before* the
/// forward, never after: two terminals of one principal answering the same
/// request must ship exactly one response.
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
    let db_path = global_root.join("loam.sqlite3");
    let row = {
        let read = enrollment::open_readonly(&db_path)
            .map_err(|_| EmitError::Unenrolled)?
            .ok_or(EmitError::Unenrolled)?;
        enrollment::lookup(&read, &key)
            .map_err(|_| EmitError::Unenrolled)?
            .ok_or(EmitError::Unenrolled)?
    };

    let derived = derive_emit(&operation, &row, now)?;

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
        Err(EmitError::ConnectorUnreachable | EmitError::ConnectorRefused) => true,
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
    let text = std::str::from_utf8(&body).map_err(|_| EmitError::ConnectorRefused)?;
    let value = crate::json::parse(text).map_err(|_| EmitError::ConnectorRefused)?;
    match value.get("status").and_then(Value::as_str) {
        Some("ok") => value
            .get("result")
            .cloned()
            .ok_or(EmitError::ConnectorRefused),
        _ => Err(EmitError::ConnectorRefused),
    }
}

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
            credential_ref: "loam/test/credential".into(),
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

    /// An enrolled global root bound to a real workspace path, so `run_emit`
    /// gets past workspace resolution and actually reaches the ledger and the
    /// forward. The workspace is this crate's own directory: real, existing, and
    /// never written to.
    fn enrolled_root(label: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let physical =
            enrollment::PhysicalWorkspace::resolve(&workspace).expect("workspace resolves");
        let root = enrollment::temp_global_root(label);
        let mut connection =
            enrollment::open_writable(&root.join("loam.sqlite3")).expect("registry opens");
        let enrolled = enrollment::ValidatedEnrollment {
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            broker_profile: "acme-prod".into(),
            broker_endpoint: "mqtts://h:8883".into(),
            tls_server_name: "h".into(),
            credential_ref: "vault://c".into(),
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
        (root, workspace, instance_id)
    }

    #[test]
    fn a_forward_that_never_reached_the_connector_does_not_burn_the_response() {
        // The defect this closes: the slot was taken before the forward and kept
        // even when the forward provably queued nothing, so one transient outage
        // made a reply permanently un-emittable.
        let (root, workspace, instance_id) = enrolled_root("emit-rollback");
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
        let mut connection =
            enrollment::open_writable(&root.join("loam.sqlite3")).expect("registry opens");
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
    fn only_a_forward_that_queued_nothing_releases_the_dedup_slot() {
        let response = |json: &str| Ok(crate::json::parse(json).expect("response parses"));

        // The three outcomes that prove nothing entered an outbound queue.
        assert!(forward_queued_nothing(&Err(
            EmitError::ConnectorUnreachable
        )));
        assert!(forward_queued_nothing(&Err(EmitError::ConnectorRefused)));
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
}
