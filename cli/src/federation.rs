//! `loam federation` CLI orchestration and typed command errors.
//!
//! Slice C T2 wires the `connect` descriptor + workspace validation path only:
//! read one bounded descriptor from stdin, validate it, and, when a workspace is
//! given, prove physical identity, remote digests, and commit reachability. The
//! enrollment probe, transactional registry commit, and service activation are
//! added by later tasks (T10/T11); `disconnect` and `status` are stubs until
//! then. No credential is resolved and no `AuthenticatedPrincipal` is built here.

use std::io::Read;
use std::path::PathBuf;

use crate::enrollment::{self, EnrollmentError, MAX_DESCRIPTOR_BYTES};
use crate::json::Value;

/// Entry point for `loam federation <subcommand>`.
pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("connect") => connect(args),
        Some("service") => service(args),
        Some("disconnect") => disconnect(args),
        Some("status") => status(args),
        _ => {
            eprintln!(
                "Usage:\n  \
                 loam federation connect [<workspace>] --json   (reads one descriptor on stdin)\n  \
                 loam federation disconnect <workspace> --global-root <path> [--json]\n  \
                 loam federation status [<workspace>] --global-root <path> [--json]"
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
    })
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
        // across a runtime-path update (T12): they reuse the same T8 manager
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
    };
    // The connector's own authenticated identity is supplied by the transport in
    // production (T13). The stub reports this derived identity.
    let identity = crate::connector::SessionIdentity {
        principal_id: format!("connector-{instance_id}"),
        agent_id: format!("agent-{instance_id}"),
        instance_id,
        allowed_claims: vec![],
    };
    let mut transport = crate::connector::StubTransport::healthy(identity);
    let runner = crate::service::RealRunner;
    let db_path = root.join("loam.sqlite3");
    let now = chrono::Utc::now();

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
