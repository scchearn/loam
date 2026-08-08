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
        Some("disconnect") | Some("status") => {
            eprintln!("federation: disconnect and status arrive in Slice C T11");
            69
        }
        _ => {
            eprintln!(
                "Usage:\n  loam federation connect [<workspace>] --json   (reads one descriptor on stdin)"
            );
            64
        }
    }
}

fn connect(args: impl Iterator<Item = String>) -> i32 {
    let mut workspace: Option<PathBuf> = None;
    let mut json_output = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json_output = true,
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

    match validate(&descriptor_bytes, workspace.as_deref()) {
        Ok(enrolled) => {
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
        Err(error) => {
            if json_output {
                println!("{}", error_json(&error).to_json());
            } else {
                eprintln!("federation connect: {error}");
            }
            error.sysexit()
        }
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
