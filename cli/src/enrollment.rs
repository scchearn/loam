//! Enrollment: descriptor validation, physical Git workspace identity, and
//! remote-URL digests.
//!
//! This module turns a bounded, non-secret stdin descriptor into a typed
//! [`ValidatedEnrollment`] candidate. It performs every trust-boundary check
//! before any registry, service-manager, credential, or transport work happens
//! elsewhere: exact schema and field inventory, no secret- or authority-shaped
//! field, a `mqtts://` endpoint without userinfo, a physical workspace identity
//! that path aliases cannot duplicate, and remote URLs resolved from local Git
//! and reduced to SHA-256 digests (config re-checks each remote's digest before
//! any later fetch). No commit-reachability proof is performed: the workspace's
//! git state changes after enrollment anyway, and the remote URL is enough to
//! prove the workspace is a git repo and infer org/project.
//!
//! It constructs no `AuthenticatedPrincipal` and resolves no credential; those
//! belong to the transport adapter.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sha256::Sha256;

/// Enrollment descriptors are bounded so a hostile or corrupt document cannot
/// exhaust memory before validation. Matches the spec's 64 KiB stdin ceiling.
pub const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
/// Bounded, control-free identifier length. Long enough for real org/project
/// names and opaque references; short enough that nothing unbounded is stored.
const MAX_FIELD_LEN: usize = 256;
const MAX_REMOTES: usize = 8;
const MIN_REMOTES: usize = 1;
const MAX_REFS_PER_REMOTE: usize = 32;
const MIN_REFS_PER_REMOTE: usize = 1;

/// Git's canonical zero object ID. Used as the enrollment commit placeholder
/// when the descriptor records none: it is a valid 40-hex OID (so the probe's
/// git anchors and the stored `commit_oid` stay well-formed) while honestly
/// meaning "no commit recorded". Commit reachability is not proven at
/// enrollment, so a stored commit is descriptive, never a gate.
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// The only keys a descriptor may carry, top level.
const DESCRIPTOR_KEYS: &[&str] = &[
    "schema",
    "org_id",
    "project_id",
    "repository_id",
    "broker",
    "git",
];
const BROKER_KEYS: &[&str] = &["profile", "endpoint", "tls_server_name", "ca_ref"];
const GIT_KEYS: &[&str] = &["commit", "remotes"];
const REMOTE_KEYS: &[&str] = &["name", "refs"];

/// Field-name fragments that would carry authority or a secret. A key whose
/// lowercased name contains any of these is rejected outright, at any level, so
/// a descriptor can never smuggle a credential or a caller-declared principal.
const FORBIDDEN_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "private_key",
    "privatekey",
    "principal",
    "agent_id",
    "instance_id",
    "source",
];

/// A typed rejection. Each variant names one specific violation so tests assert
/// error identity rather than matching message text, and so the CLI can map it
/// to a stable JSON code and a sysexits process class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentError {
    TooLarge { bytes: usize },
    NotUtf8,
    InvalidJson(String),
    NotAnObject { at: &'static str },
    DuplicateField { key: String },
    UnknownField { key: String },
    ForbiddenField { key: String },
    MissingField { key: &'static str },
    UnsupportedSchema,
    InvalidField { field: &'static str },
    InvalidEndpoint,
    InvalidCommit,
    TooManyRemotes,
    TooFewRemotes,
    TooManyRefs { remote: String },
    TooFewRefs { remote: String },
    InvalidRef { value: String },
    WorkspaceNotGit,
    WorkspaceNotUtf8,
    RemoteNotConfigured { remote: String },
    CredentialBearingRemote { remote: String },
    GitUnavailable,
}

impl EnrollmentError {
    /// Stable machine code for JSON output. Never includes untrusted input.
    pub fn code(&self) -> &'static str {
        match self {
            EnrollmentError::TooLarge { .. } => "descriptor_too_large",
            EnrollmentError::NotUtf8 => "descriptor_not_utf8",
            EnrollmentError::InvalidJson(_) => "descriptor_invalid_json",
            EnrollmentError::NotAnObject { .. } => "descriptor_not_object",
            EnrollmentError::DuplicateField { .. } => "descriptor_duplicate_field",
            EnrollmentError::UnknownField { .. } => "descriptor_unknown_field",
            EnrollmentError::ForbiddenField { .. } => "descriptor_forbidden_field",
            EnrollmentError::MissingField { .. } => "descriptor_missing_field",
            EnrollmentError::UnsupportedSchema => "descriptor_unsupported_schema",
            EnrollmentError::InvalidField { .. } => "descriptor_invalid_field",
            EnrollmentError::InvalidEndpoint => "descriptor_invalid_endpoint",
            EnrollmentError::InvalidCommit => "descriptor_invalid_commit",
            EnrollmentError::TooManyRemotes => "descriptor_too_many_remotes",
            EnrollmentError::TooFewRemotes => "descriptor_too_few_remotes",
            EnrollmentError::TooManyRefs { .. } => "descriptor_too_many_refs",
            EnrollmentError::TooFewRefs { .. } => "descriptor_too_few_refs",
            EnrollmentError::InvalidRef { .. } => "descriptor_invalid_ref",
            EnrollmentError::WorkspaceNotGit => "workspace_not_git",
            EnrollmentError::WorkspaceNotUtf8 => "workspace_not_utf8",
            EnrollmentError::RemoteNotConfigured { .. } => "remote_not_configured",
            EnrollmentError::CredentialBearingRemote { .. } => "credential_bearing_remote",
            EnrollmentError::GitUnavailable => "git_unavailable",
        }
    }

    /// sysexits-style process class, matching the crate's CLI convention:
    /// usage/input 64/65, unavailable 69, internal 70.
    pub fn sysexit(&self) -> i32 {
        match self {
            EnrollmentError::GitUnavailable => 69,
            _ => 65,
        }
    }
}

impl std::fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Diagnostics name the field/remote key (descriptor-supplied, bounded,
        // control-free by the time we format) but never echo values, URLs, or
        // secrets.
        match self {
            EnrollmentError::TooLarge { bytes } => {
                write!(
                    f,
                    "descriptor is {bytes} bytes, over the {MAX_DESCRIPTOR_BYTES} limit"
                )
            }
            EnrollmentError::NotUtf8 => write!(f, "descriptor is not valid UTF-8"),
            EnrollmentError::InvalidJson(why) => write!(f, "descriptor is not valid JSON: {why}"),
            EnrollmentError::NotAnObject { at } => write!(f, "{at} must be a JSON object"),
            EnrollmentError::DuplicateField { key } => write!(f, "duplicate field `{key}`"),
            EnrollmentError::UnknownField { key } => write!(f, "unknown field `{key}`"),
            EnrollmentError::ForbiddenField { key } => {
                write!(
                    f,
                    "field `{key}` may not carry a secret or a caller-declared identity"
                )
            }
            EnrollmentError::MissingField { key } => write!(f, "missing field `{key}`"),
            EnrollmentError::UnsupportedSchema => write!(f, "descriptor schema must be 1"),
            EnrollmentError::InvalidField { field } => write!(f, "field `{field}` is invalid"),
            EnrollmentError::InvalidEndpoint => {
                write!(
                    f,
                    "broker endpoint must be mqtts:// with no userinfo, query, or fragment"
                )
            }
            EnrollmentError::InvalidCommit => {
                write!(f, "git.commit must be 40 or 64 lowercase hex")
            }
            EnrollmentError::TooManyRemotes => {
                write!(f, "git.remotes may list at most {MAX_REMOTES}")
            }
            EnrollmentError::TooFewRemotes => {
                write!(f, "git.remotes must list at least {MIN_REMOTES}")
            }
            EnrollmentError::TooManyRefs { remote } => {
                write!(
                    f,
                    "remote `{remote}` may list at most {MAX_REFS_PER_REMOTE} refs"
                )
            }
            EnrollmentError::TooFewRefs { remote } => {
                write!(
                    f,
                    "remote `{remote}` must list at least {MIN_REFS_PER_REMOTE} ref"
                )
            }
            EnrollmentError::InvalidRef { value } => {
                write!(f, "ref `{value}` must be a full refs/ ref")
            }
            EnrollmentError::WorkspaceNotGit => {
                write!(f, "workspace is not a Git top-level directory")
            }
            EnrollmentError::WorkspaceNotUtf8 => {
                write!(f, "workspace path is not representable as UTF-8")
            }
            EnrollmentError::RemoteNotConfigured { remote } => {
                write!(f, "remote `{remote}` is not configured in the workspace")
            }
            EnrollmentError::CredentialBearingRemote { remote } => {
                write!(f, "remote `{remote}` URL embeds credentials")
            }
            EnrollmentError::GitUnavailable => write!(f, "git is unavailable"),
        }
    }
}

/// A validated, non-secret enrollment projection. It holds no credential, no
/// raw remote URL, and no message content — only bounded validated identifiers,
/// remote name→digest→allowed-refs mappings, and the physical workspace
/// identity that prevents alias duplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEnrollment {
    pub org_id: String,
    pub project_id: String,
    pub repository_id: String,
    pub broker_profile: String,
    pub broker_endpoint: String,
    pub tls_server_name: String,
    pub ca_ref: Option<String>,
    pub commit: String,
    pub remotes: Vec<ValidatedRemote>,
    pub workspace: PhysicalWorkspace,
}

/// One remote reduced to its name, the SHA-256 of its normalized URL (never the
/// URL itself), and the exact allowed refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRemote {
    pub name: String,
    pub url_digest: String,
    pub allowed_refs: Vec<String>,
}

/// Canonical display path plus a platform physical identity so symlink, case,
/// or bind aliases of one directory cannot enroll twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalWorkspace {
    pub display_path: String,
    pub identity: PlatformIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformIdentity {
    /// Unix device + inode: robust against symlink, case, and bind-mount aliases.
    #[cfg_attr(not(unix), allow(dead_code))]
    Unix { device: u64, inode: u64 },
    /// Windows carries only the canonical final path currently; the
    /// volume-serial/file-index refinement is deferred alongside the other
    /// Win32 handle FFI, where the hosted Windows CI leg proves it.
    // ponytail: Windows volume/file-index deferred (all Win32 raw FFI in one CI-verified place)
    #[cfg_attr(unix, allow(dead_code))]
    WindowsPath,
}

// ---------------------------------------------------------------------------
// Descriptor parsing and validation
// ---------------------------------------------------------------------------

/// Parse a bounded stdin byte string into a validated [`Descriptor`]. Performs
/// size, UTF-8, JSON, field-inventory, secret-shape, and per-field checks with
/// no filesystem or network access.
pub fn parse_descriptor(bytes: &[u8]) -> Result<Descriptor, EnrollmentError> {
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(EnrollmentError::TooLarge { bytes: bytes.len() });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| EnrollmentError::NotUtf8)?;
    let value = crate::json::parse(text).map_err(EnrollmentError::InvalidJson)?;
    let entries = object_entries(&value, "descriptor")?;
    reject_forbidden_and_check_keys(entries, DESCRIPTOR_KEYS)?;

    require_schema_one(entries)?;
    let org_id = required_bounded_id(entries, "org_id")?;
    let project_id = required_bounded_id(entries, "project_id")?;
    let repository_id = required_bounded_id(entries, "repository_id")?;
    let broker = parse_broker(required_object(entries, "broker")?)?;
    let git = parse_git(required_object(entries, "git")?)?;

    Ok(Descriptor {
        org_id,
        project_id,
        repository_id,
        broker,
        git,
    })
}

/// The parsed, structurally validated descriptor before workspace/remote proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub org_id: String,
    pub project_id: String,
    pub repository_id: String,
    pub broker: BrokerDescriptor,
    pub git: GitDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerDescriptor {
    pub profile: String,
    pub endpoint: String,
    pub tls_server_name: String,
    pub ca_ref: Option<String>,
}

/// The workspace Git binding: remote names + exact allowed refs for the
/// workspace-identity check. `commit` is optional — it is descriptive
/// provenance at best, never a reachability gate, so the descriptor need not
/// carry one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDescriptor {
    pub commit: Option<String>,
    pub remotes: Vec<RemoteDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDescriptor {
    pub name: String,
    pub refs: Vec<String>,
}

fn parse_broker(
    entries: &[(String, crate::json::Value)],
) -> Result<BrokerDescriptor, EnrollmentError> {
    reject_forbidden_and_check_keys(entries, BROKER_KEYS)?;
    let profile = required_bounded_id(entries, "profile")?;
    let endpoint = required_bounded_string(entries, "endpoint")?;
    validate_endpoint(&endpoint)?;
    let tls_server_name = required_bounded_id(entries, "tls_server_name")?;
    let ca_ref = optional_bounded_string(entries, "ca_ref")?;
    Ok(BrokerDescriptor {
        profile,
        endpoint,
        tls_server_name,
        ca_ref,
    })
}

fn parse_git(entries: &[(String, crate::json::Value)]) -> Result<GitDescriptor, EnrollmentError> {
    reject_forbidden_and_check_keys(entries, GIT_KEYS)?;
    let commit = match optional_bounded_string(entries, "commit")? {
        Some(commit) => {
            validate_commit(&commit)?;
            Some(commit)
        }
        None => None,
    };

    let remotes_value = entries
        .iter()
        .find(|(k, _)| k == "remotes")
        .map(|(_, v)| v)
        .ok_or(EnrollmentError::MissingField { key: "remotes" })?;
    let items = remotes_value
        .as_array()
        .ok_or(EnrollmentError::InvalidField { field: "remotes" })?;
    if items.len() > MAX_REMOTES {
        return Err(EnrollmentError::TooManyRemotes);
    }
    if items.len() < MIN_REMOTES {
        return Err(EnrollmentError::TooFewRemotes);
    }
    let mut remotes = Vec::with_capacity(items.len());
    for item in items {
        remotes.push(parse_remote(object_entries(item, "remote")?)?);
    }
    Ok(GitDescriptor { commit, remotes })
}

fn parse_remote(
    entries: &[(String, crate::json::Value)],
) -> Result<RemoteDescriptor, EnrollmentError> {
    reject_forbidden_and_check_keys(entries, REMOTE_KEYS)?;
    let name = required_bounded_id(entries, "name")?;
    let refs_value = entries
        .iter()
        .find(|(k, _)| k == "refs")
        .map(|(_, v)| v)
        .ok_or(EnrollmentError::MissingField { key: "refs" })?;
    let items = refs_value
        .as_array()
        .ok_or(EnrollmentError::InvalidField { field: "refs" })?;
    if items.len() > MAX_REFS_PER_REMOTE {
        return Err(EnrollmentError::TooManyRefs { remote: name });
    }
    if items.len() < MIN_REFS_PER_REMOTE {
        return Err(EnrollmentError::TooFewRefs { remote: name });
    }
    let mut refs = Vec::with_capacity(items.len());
    for item in items {
        let value = item
            .as_str()
            .ok_or(EnrollmentError::InvalidField { field: "refs" })?;
        validate_ref(value)?;
        refs.push(value.to_owned());
    }
    Ok(RemoteDescriptor { name, refs })
}

// --- field helpers ---------------------------------------------------------

fn object_entries<'a>(
    value: &'a crate::json::Value,
    at: &'static str,
) -> Result<&'a [(String, crate::json::Value)], EnrollmentError> {
    match value {
        crate::json::Value::Object(entries) => Ok(entries),
        _ => Err(EnrollmentError::NotAnObject { at }),
    }
}

/// Reject any forbidden (secret/authority-shaped) key, any duplicate key, and
/// any key outside the allowed set — in that order, so a smuggled credential is
/// refused before an "unknown field" verdict could mask it.
fn reject_forbidden_and_check_keys(
    entries: &[(String, crate::json::Value)],
    allowed: &[&str],
) -> Result<(), EnrollmentError> {
    for (index, (key, _)) in entries.iter().enumerate() {
        let lowered = key.to_ascii_lowercase();
        if FORBIDDEN_FRAGMENTS
            .iter()
            .any(|frag| lowered.contains(frag))
        {
            return Err(EnrollmentError::ForbiddenField { key: key.clone() });
        }
        if entries[..index].iter().any(|(prior, _)| prior == key) {
            return Err(EnrollmentError::DuplicateField { key: key.clone() });
        }
        if !allowed.contains(&key.as_str()) {
            return Err(EnrollmentError::UnknownField { key: key.clone() });
        }
    }
    Ok(())
}

fn required_object<'a>(
    entries: &'a [(String, crate::json::Value)],
    key: &'static str,
) -> Result<&'a [(String, crate::json::Value)], EnrollmentError> {
    let value = entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or(EnrollmentError::MissingField { key })?;
    object_entries(value, key)
}

fn require_schema_one(entries: &[(String, crate::json::Value)]) -> Result<(), EnrollmentError> {
    let value = entries
        .iter()
        .find(|(k, _)| k == "schema")
        .map(|(_, v)| v)
        .ok_or(EnrollmentError::MissingField { key: "schema" })?;
    match value {
        crate::json::Value::Number(literal) if literal == "1" => Ok(()),
        _ => Err(EnrollmentError::UnsupportedSchema),
    }
}

fn required_bounded_string(
    entries: &[(String, crate::json::Value)],
    field: &'static str,
) -> Result<String, EnrollmentError> {
    let value = entries
        .iter()
        .find(|(k, _)| k == field)
        .map(|(_, v)| v)
        .ok_or(EnrollmentError::MissingField { key: field })?;
    let text = value
        .as_str()
        .ok_or(EnrollmentError::InvalidField { field })?;
    if text.is_empty() || text.len() > MAX_FIELD_LEN {
        return Err(EnrollmentError::InvalidField { field });
    }
    Ok(text.to_owned())
}

/// A bounded identifier: non-empty, within length, and free of control
/// characters. Used for ids, names, profiles, and TLS server names.
fn required_bounded_id(
    entries: &[(String, crate::json::Value)],
    field: &'static str,
) -> Result<String, EnrollmentError> {
    let text = required_bounded_string(entries, field)?;
    if text.chars().any(|c| c.is_control()) {
        return Err(EnrollmentError::InvalidField { field });
    }
    Ok(text)
}

fn optional_bounded_string(
    entries: &[(String, crate::json::Value)],
    field: &'static str,
) -> Result<Option<String>, EnrollmentError> {
    match entries.iter().find(|(k, _)| k == field).map(|(_, v)| v) {
        None => Ok(None),
        Some(crate::json::Value::Null) => Ok(None),
        Some(value) => {
            let text = value
                .as_str()
                .ok_or(EnrollmentError::InvalidField { field })?;
            if text.is_empty() || text.len() > MAX_FIELD_LEN {
                return Err(EnrollmentError::InvalidField { field });
            }
            Ok(Some(text.to_owned()))
        }
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), EnrollmentError> {
    let rest = endpoint
        .strip_prefix("mqtts://")
        .ok_or(EnrollmentError::InvalidEndpoint)?;
    // No userinfo (`@`), query (`?`), or fragment (`#`); a plain host[:port]
    // authority only. Production never accepts plaintext mqtt://.
    if rest.is_empty()
        || rest.contains('@')
        || rest.contains('?')
        || rest.contains('#')
        || rest.contains('/')
    {
        return Err(EnrollmentError::InvalidEndpoint);
    }
    Ok(())
}

fn validate_commit(commit: &str) -> Result<(), EnrollmentError> {
    let ok = matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(EnrollmentError::InvalidCommit)
    }
}

fn validate_ref(value: &str) -> Result<(), EnrollmentError> {
    let ok = value.starts_with("refs/")
        && value.len() <= MAX_FIELD_LEN
        && !value.contains("..")
        && !value.ends_with('/')
        && value.chars().all(|c| !c.is_control() && c != ' ');
    if ok {
        Ok(())
    } else {
        Err(EnrollmentError::InvalidRef {
            value: value.chars().take(64).collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Physical workspace identity
// ---------------------------------------------------------------------------

impl PhysicalWorkspace {
    /// Resolve `path` to the Git top level, canonicalize it, and derive a
    /// physical identity. Never fetches, never mutates the repo.
    pub fn resolve(path: &Path) -> Result<PhysicalWorkspace, EnrollmentError> {
        let top = git_toplevel(path)?;
        let canonical =
            std::fs::canonicalize(&top).map_err(|_| EnrollmentError::WorkspaceNotGit)?;
        let display_path = canonical
            .to_str()
            .ok_or(EnrollmentError::WorkspaceNotUtf8)?
            .to_owned();
        let identity = platform_identity(&canonical)?;
        Ok(PhysicalWorkspace {
            display_path,
            identity,
        })
    }
}

fn git_toplevel(path: &Path) -> Result<PathBuf, EnrollmentError> {
    let path_str = path.to_str().ok_or(EnrollmentError::WorkspaceNotUtf8)?;
    let output = Command::new("git")
        .args(["-C", path_str, "rev-parse", "--show-toplevel"])
        .output()
        .map_err(|_| EnrollmentError::GitUnavailable)?;
    if !output.status.success() {
        return Err(EnrollmentError::WorkspaceNotGit);
    }
    let text = String::from_utf8(output.stdout).map_err(|_| EnrollmentError::WorkspaceNotUtf8)?;
    let top = text.trim_end_matches('\n');
    if top.is_empty() {
        return Err(EnrollmentError::WorkspaceNotGit);
    }
    Ok(PathBuf::from(top))
}

#[cfg(unix)]
fn platform_identity(canonical: &Path) -> Result<PlatformIdentity, EnrollmentError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(canonical).map_err(|_| EnrollmentError::WorkspaceNotGit)?;
    Ok(PlatformIdentity::Unix {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(not(unix))]
fn platform_identity(_canonical: &Path) -> Result<PlatformIdentity, EnrollmentError> {
    // Windows uses the canonical final path as identity currently; a later change adds the
    // volume-serial/file-index refinement under the reviewed Win32 FFI.
    Ok(PlatformIdentity::WindowsPath)
}

// ---------------------------------------------------------------------------
// Remote resolution
// ---------------------------------------------------------------------------

/// Resolve one configured remote's URL from local Git config. Returns the raw
/// URL string; the caller reduces it to a digest and rejects userinfo.
fn remote_url(workspace: &Path, name: &str) -> Result<String, EnrollmentError> {
    let path_str = workspace
        .to_str()
        .ok_or(EnrollmentError::WorkspaceNotUtf8)?;
    let output = Command::new("git")
        .args(["-C", path_str, "remote", "get-url", name])
        .output()
        .map_err(|_| EnrollmentError::GitUnavailable)?;
    if !output.status.success() {
        return Err(EnrollmentError::RemoteNotConfigured {
            remote: name.to_owned(),
        });
    }
    let url = String::from_utf8(output.stdout)
        .map_err(|_| EnrollmentError::RemoteNotConfigured {
            remote: name.to_owned(),
        })?
        .trim_end_matches('\n')
        .to_owned();
    if url.is_empty() {
        return Err(EnrollmentError::RemoteNotConfigured {
            remote: name.to_owned(),
        });
    }
    Ok(url)
}

/// Reject a URL that embeds credentials (`userinfo@` in the authority of an
/// `scheme://` URL), then return the SHA-256 hex of the URL. `scp`-style
/// `user@host:path` remotes are permitted (the `user` is an SSH login, not an
/// embedded password) and are not treated as credential-bearing.
fn digest_remote_url(url: &str, name: &str) -> Result<String, EnrollmentError> {
    if let Some((_scheme, after)) = url.split_once("://") {
        let authority = after.split('/').next().unwrap_or(after);
        if authority.contains('@') {
            return Err(EnrollmentError::CredentialBearingRemote {
                remote: name.to_owned(),
            });
        }
    }
    let mut hasher = Sha256::default();
    hasher.update(url.as_bytes());
    Ok(hasher.finish())
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Validate a descriptor against a physical workspace end to end: structural
/// validation, physical identity, and remote resolution + digest + userinfo
/// rejection. Returns the non-secret [`ValidatedEnrollment`] projection or the
/// first typed violation. No commit-reachability proof is performed.
pub fn validate_enrollment(
    descriptor: Descriptor,
    workspace_path: &Path,
) -> Result<ValidatedEnrollment, EnrollmentError> {
    let workspace = PhysicalWorkspace::resolve(workspace_path)?;
    let canonical = PathBuf::from(&workspace.display_path);

    let mut remotes = Vec::with_capacity(descriptor.git.remotes.len());
    for remote in &descriptor.git.remotes {
        let url = remote_url(&canonical, &remote.name)?;
        let url_digest = digest_remote_url(&url, &remote.name)?;
        remotes.push(ValidatedRemote {
            name: remote.name.clone(),
            url_digest,
            allowed_refs: remote.refs.clone(),
        });
    }

    Ok(ValidatedEnrollment {
        org_id: descriptor.org_id,
        project_id: descriptor.project_id,
        repository_id: descriptor.repository_id,
        broker_profile: descriptor.broker.profile,
        broker_endpoint: descriptor.broker.endpoint,
        tls_server_name: descriptor.broker.tls_server_name,
        ca_ref: descriptor.broker.ca_ref,
        commit: descriptor.git.commit.unwrap_or_else(|| ZERO_OID.to_owned()),
        remotes,
        workspace,
    })
}

// ---------------------------------------------------------------------------
// Transactional federation registry
// ---------------------------------------------------------------------------
//
// Federation enrollments live in federation-owned tables inside the existing
// `<global-root>/loam.sqlite3`, alongside the hook tables. The registry keeps
// its own `federation_schema(version)` marker and never reads or writes the
// hook-owned `PRAGMA user_version`. Reads open read-only and return empty when
// the database does not exist — a read never creates the store. Writes use a
// 5-second busy ceiling and `BEGIN IMMEDIATE`. Physical-workspace identity is the
// uniqueness key, so symlink/case/bind aliases cannot enroll twice.

// Re-exported for the connect/disconnect/status orchestration (T10/T11); unused
// in the binary until then.
#[allow(unused_imports)]
pub(crate) use registry::*;

/// A unique existing directory for a test that needs a global root. It lives
/// here because this module is the crate's admitted filesystem surface — a
/// caller such as `federation.rs` is deliberately barred from `std::fs`, so it
/// cannot make one itself. Leaked on purpose, like every other temp path in
/// these tests.
#[cfg(test)]
pub fn temp_global_root(label: &str) -> std::path::PathBuf {
    // A short base on Unix: tests bind a Unix-domain socket under this root, and
    // macOS caps sun_path at ~104 bytes — the default TMPDIR (`/var/folders/…`)
    // is long enough to overflow it. `/tmp` is short and always present on Unix;
    // Windows keeps the standard temp dir.
    #[cfg(unix)]
    let base = std::path::PathBuf::from("/tmp");
    #[cfg(not(unix))]
    let base = std::env::temp_dir();
    let path = base.join(format!(
        "loam-root-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("temp global root");
    path
}

pub mod registry {
    //! Consumed by the connect/disconnect/status orchestration in T10/T11, which
    //! retires this module allow once the registry is wired to the CLI surface.
    #![allow(dead_code)]

    use std::path::Path;

    use rusqlite::{Connection, OpenFlags, TransactionBehavior};

    use crate::enrollment::{
        PhysicalWorkspace, PlatformIdentity, ValidatedEnrollment, ValidatedRemote,
    };
    use crate::sha256::Sha256;

    // The schema marker stays at 2 across the addition of `federation_session`:
    // every federation table is created together under one version via
    // `CREATE TABLE IF NOT EXISTS`, and the wake-ref table is a purely additive,
    // optional cache that both older and newer runtimes tolerate (an old binary
    // ignores it; a new binary creates it on the next writable open and treats its
    // absence as "no persisted wakes yet"). Bumping the marker would make the
    // strict version check reject a v2 registry from an older runtime for no
    // compatibility gain — the connector-self-healing reload/prune contract, not a
    // marker, is what matters.
    const FEDERATION_SCHEMA_VERSION: i64 = 2;
    const REGISTRY_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5_000);

    const CREATE_FEDERATION_TABLES: &str = "\
    CREATE TABLE IF NOT EXISTS federation_schema (version INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS federation_enrollment (
        id INTEGER PRIMARY KEY,
        identity_key TEXT NOT NULL UNIQUE,
        org_id TEXT NOT NULL,
        project_id TEXT NOT NULL,
        repository_id TEXT NOT NULL,
        descriptor_digest TEXT NOT NULL,
        display_path TEXT NOT NULL,
        instance_id TEXT NOT NULL,
        broker_profile TEXT NOT NULL,
        broker_endpoint TEXT NOT NULL,
        tls_server_name TEXT NOT NULL,
        ca_ref TEXT,
        commit_oid TEXT NOT NULL,
        cap_authentication INTEGER NOT NULL,
        cap_publish INTEGER NOT NULL,
        cap_subscribe INTEGER NOT NULL,
        cap_self_receive INTEGER NOT NULL,
        verified_at TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS federation_remote (
        enrollment_id INTEGER NOT NULL REFERENCES federation_enrollment(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        url_digest TEXT NOT NULL,
        allowed_refs TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS response_dedup (
        causation_id TEXT NOT NULL,
        responder_principal_id TEXT NOT NULL,
        recorded_at TEXT NOT NULL,
        PRIMARY KEY (causation_id, responder_principal_id)
    );
    CREATE TABLE IF NOT EXISTS federation_session (
        session_id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        wake_ref TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );";

    /// A backend or schema failure. Distinct from the descriptor rejections above so
    /// callers map it to a persistence sysexit rather than a usage error.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RegistryError {
        Backend(String),
        SchemaUnsupported { version: i64 },
    }

    impl std::fmt::Display for RegistryError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RegistryError::Backend(why) => {
                    write!(f, "federation registry backend error: {why}")
                }
                RegistryError::SchemaUnsupported { version } => {
                    write!(f, "unsupported federation schema version {version}")
                }
            }
        }
    }

    impl RegistryError {
        fn backend<E: std::fmt::Display>(error: E) -> RegistryError {
            RegistryError::Backend(error.to_string())
        }
    }

    /// The result of an idempotent enrollment insert.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum InsertOutcome {
        /// A new enrollment was committed.
        Inserted,
        /// The same physical workspace with the same descriptor digest already
        /// exists; nothing changed (and no healthy probe is republished).
        AlreadyEnrolled,
        /// The same physical workspace exists with a different descriptor/binding;
        /// neither row changed. The caller must disconnect first.
        Conflict,
    }

    /// One stored enrollment, read back for status and lifecycle. Carries no secret,
    /// no raw remote URL, and no message content.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EnrolledRow {
        pub identity_key: String,
        pub org_id: String,
        pub project_id: String,
        pub repository_id: String,
        pub descriptor_digest: String,
        pub display_path: String,
        pub instance_id: String,
        pub broker_profile: String,
        /// The stored `mqtts://host:port` the connector dials. Read back so a
        /// session can be provisioned from the row alone.
        pub broker_endpoint: String,
        pub tls_server_name: String,
        /// Absent means "use the bundled Mozilla roots"; present means a PEM
        /// trust file pinned at this path.
        pub ca_ref: Option<String>,
        pub commit: String,
        pub capabilities: CapabilityRecord,
        pub remotes: Vec<ValidatedRemote>,
    }

    /// The historical capability evidence a successful probe recorded.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CapabilityRecord {
        pub authentication: bool,
        pub publish: bool,
        pub subscribe: bool,
        pub self_receive: bool,
        pub verified_at: String,
    }

    /// The uniqueness key for a physical workspace. Two aliases of one directory
    /// resolve to the same key and therefore cannot enroll twice.
    pub fn identity_key(workspace: &PhysicalWorkspace) -> String {
        match &workspace.identity {
            PlatformIdentity::Unix { device, inode } => format!("unix:{device}:{inode}"),
            PlatformIdentity::WindowsPath => format!("path:{}", workspace.display_path),
        }
    }

    /// A stable digest of the validated enrollment's binding, used to decide whether
    /// a repeated connect is idempotent or a conflict. Covers org/project/repository,
    /// broker, commit, and each remote's name/digest/refs — never a secret.
    pub fn descriptor_digest(enrolled: &ValidatedEnrollment) -> String {
        let mut hasher = Sha256::default();
        for part in [
            &enrolled.org_id,
            &enrolled.project_id,
            &enrolled.repository_id,
            &enrolled.broker_profile,
            &enrolled.broker_endpoint,
            &enrolled.tls_server_name,
            &enrolled.commit,
        ] {
            hasher.update(part.as_bytes());
            hasher.update(b"\x1f");
        }
        for remote in &enrolled.remotes {
            hasher.update(remote.name.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(remote.url_digest.as_bytes());
            hasher.update(b"\x1f");
            for allowed in &remote.allowed_refs {
                hasher.update(allowed.as_bytes());
                hasher.update(b"\x1e");
            }
            hasher.update(b"\x1d");
        }
        hasher.finish()
    }

    /// Open the registry for writing, creating the database and the federation
    /// tables if absent. Never touches the hook-owned `PRAGMA user_version`.
    pub fn open_writable(db_path: &Path) -> Result<Connection, RegistryError> {
        let connection = Connection::open(db_path).map_err(RegistryError::backend)?;
        connection
            .busy_timeout(REGISTRY_BUSY_TIMEOUT)
            .map_err(RegistryError::backend)?;
        connection
            .execute_batch(CREATE_FEDERATION_TABLES)
            .map_err(RegistryError::backend)?;
        ensure_schema_marker(&connection)?;
        Ok(connection)
    }

    /// Open the registry read-only. Returns `None` when the database does not exist,
    /// so a read never creates the store.
    pub fn open_readonly(db_path: &Path) -> Result<Option<Connection>, RegistryError> {
        if !db_path.is_file() {
            return Ok(None);
        }
        let connection = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(RegistryError::backend)?;
        connection
            .busy_timeout(REGISTRY_BUSY_TIMEOUT)
            .map_err(RegistryError::backend)?;
        // A database that predates federation has no marker yet; treat absent as
        // "no enrollments" rather than an error.
        if federation_tables_present(&connection)? {
            let version = schema_marker(&connection)?;
            if version != FEDERATION_SCHEMA_VERSION {
                return Err(RegistryError::SchemaUnsupported { version });
            }
        }
        Ok(Some(connection))
    }

    fn ensure_schema_marker(connection: &Connection) -> Result<(), RegistryError> {
        let count: i64 = connection
            .query_row("SELECT count(*) FROM federation_schema", [], |row| {
                row.get(0)
            })
            .map_err(RegistryError::backend)?;
        if count == 0 {
            connection
                .execute(
                    "INSERT INTO federation_schema (version) VALUES (?1)",
                    [FEDERATION_SCHEMA_VERSION],
                )
                .map_err(RegistryError::backend)?;
        } else {
            let version = schema_marker(connection)?;
            if version != FEDERATION_SCHEMA_VERSION {
                return Err(RegistryError::SchemaUnsupported { version });
            }
        }
        Ok(())
    }

    fn schema_marker(connection: &Connection) -> Result<i64, RegistryError> {
        connection
            .query_row("SELECT version FROM federation_schema LIMIT 1", [], |row| {
                row.get(0)
            })
            .map_err(RegistryError::backend)
    }

    fn federation_tables_present(connection: &Connection) -> Result<bool, RegistryError> {
        let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='federation_schema'",
            [],
            |row| row.get(0),
        )
        .map_err(RegistryError::backend)?;
        Ok(count == 1)
    }

    /// Insert an enrollment idempotently. Existing identical bindings are
    /// `AlreadyEnrolled`; a changed binding for the same physical workspace is a
    /// `Conflict`; neither mutates an existing row.
    pub fn insert_enrollment(
        connection: &mut Connection,
        enrolled: &ValidatedEnrollment,
        instance_id: &str,
        capabilities: &CapabilityRecord,
        now_rfc3339: &str,
    ) -> Result<InsertOutcome, RegistryError> {
        let key = identity_key(&enrolled.workspace);
        let digest = descriptor_digest(enrolled);

        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RegistryError::backend)?;

        let existing: Option<String> = transaction
            .query_row(
                "SELECT descriptor_digest FROM federation_enrollment WHERE identity_key = ?1",
                [&key],
                |row| row.get(0),
            )
            .ok();
        if let Some(existing_digest) = existing {
            let outcome = if existing_digest == digest {
                InsertOutcome::AlreadyEnrolled
            } else {
                InsertOutcome::Conflict
            };
            transaction.rollback().map_err(RegistryError::backend)?;
            return Ok(outcome);
        }

        transaction
            .execute(
                "INSERT INTO federation_enrollment (
                identity_key, org_id, project_id, repository_id, descriptor_digest,
                display_path, instance_id, broker_profile, broker_endpoint,
                tls_server_name, ca_ref, commit_oid,
                cap_authentication, cap_publish, cap_subscribe, cap_self_receive,
                verified_at, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18
            )",
                rusqlite::params![
                    key,
                    enrolled.org_id,
                    enrolled.project_id,
                    enrolled.repository_id,
                    digest,
                    enrolled.workspace.display_path,
                    instance_id,
                    enrolled.broker_profile,
                    enrolled.broker_endpoint,
                    enrolled.tls_server_name,
                    enrolled.ca_ref,
                    enrolled.commit,
                    capabilities.authentication as i64,
                    capabilities.publish as i64,
                    capabilities.subscribe as i64,
                    capabilities.self_receive as i64,
                    capabilities.verified_at,
                    now_rfc3339,
                ],
            )
            .map_err(RegistryError::backend)?;
        let enrollment_id = transaction.last_insert_rowid();
        for remote in &enrolled.remotes {
            transaction
                .execute(
                    "INSERT INTO federation_remote (enrollment_id, name, url_digest, allowed_refs)
                 VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        enrollment_id,
                        remote.name,
                        remote.url_digest,
                        remote.allowed_refs.join("\n"),
                    ],
                )
                .map_err(RegistryError::backend)?;
        }
        transaction.commit().map_err(RegistryError::backend)?;
        Ok(InsertOutcome::Inserted)
    }

    /// Look up an enrollment by physical-workspace identity key.
    pub fn lookup(
        connection: &Connection,
        key: &str,
    ) -> Result<Option<EnrolledRow>, RegistryError> {
        let row = connection
            .query_row(
                "SELECT id, identity_key, org_id, project_id, repository_id, descriptor_digest,
                    display_path, instance_id, broker_profile, broker_endpoint,
                    tls_server_name, ca_ref, commit_oid,
                    cap_authentication, cap_publish, cap_subscribe, cap_self_receive, verified_at
             FROM federation_enrollment WHERE identity_key = ?1",
                [key],
                row_to_enrolled,
            )
            .ok();
        match row {
            Some((id, mut enrolled)) => {
                enrolled.remotes = remotes_for(connection, id)?;
                Ok(Some(enrolled))
            }
            None => Ok(None),
        }
    }

    /// List every enrollment, each with its remotes.
    pub fn list_enrollments(connection: &Connection) -> Result<Vec<EnrolledRow>, RegistryError> {
        let mut statement = connection
            .prepare(
                "SELECT id, identity_key, org_id, project_id, repository_id, descriptor_digest,
                    display_path, instance_id, broker_profile, broker_endpoint,
                    tls_server_name, ca_ref, commit_oid,
                    cap_authentication, cap_publish, cap_subscribe, cap_self_receive, verified_at
             FROM federation_enrollment ORDER BY id",
            )
            .map_err(RegistryError::backend)?;
        let rows = statement
            .query_map([], row_to_enrolled)
            .map_err(RegistryError::backend)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, mut enrolled) = row.map_err(RegistryError::backend)?;
            enrolled.remotes = remotes_for(connection, id)?;
            out.push(enrolled);
        }
        Ok(out)
    }

    /// Remove an enrollment by identity key. Returns whether a row was removed.
    pub fn delete_enrollment(
        connection: &mut Connection,
        key: &str,
    ) -> Result<bool, RegistryError> {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RegistryError::backend)?;
        let removed = transaction
            .execute(
                "DELETE FROM federation_enrollment WHERE identity_key = ?1",
                [key],
            )
            .map_err(RegistryError::backend)?;
        // Remotes cascade via the foreign key only when enforcement is on; delete
        // explicitly so the projection is clean regardless of the pragma.
        transaction
            .execute(
                "DELETE FROM federation_remote WHERE enrollment_id NOT IN
                (SELECT id FROM federation_enrollment)",
                [],
            )
            .map_err(RegistryError::backend)?;
        transaction.commit().map_err(RegistryError::backend)?;
        Ok(removed > 0)
    }

    /// One persisted session wake reference, reloaded after a connector restart so
    /// an idle session (one that takes no turns and so never re-registers) is still
    /// woken and mailbox-delivered.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct PersistedWake {
        pub session_id: String,
        pub project_id: String,
        pub wake_ref: String,
    }

    /// Persist (or refresh) one session's wake reference — the idempotent upsert
    /// the connector runs on every registration, so a re-register with the same
    /// session id updates rather than duplicates. Only wake-bearing registrations
    /// are stored; the channel ref is live-only and never persisted.
    pub fn upsert_session_wake(
        connection: &Connection,
        session_id: &str,
        project_id: &str,
        wake_ref: &str,
        now_rfc3339: &str,
    ) -> Result<(), RegistryError> {
        connection
            .execute(
                "INSERT INTO federation_session (session_id, project_id, wake_ref, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(session_id) DO UPDATE SET
                     project_id = excluded.project_id,
                     wake_ref = excluded.wake_ref,
                     updated_at = excluded.updated_at",
                rusqlite::params![session_id, project_id, wake_ref, now_rfc3339],
            )
            .map_err(RegistryError::backend)?;
        Ok(())
    }

    /// Remove one session's persisted wake reference — on an explicit drop or when
    /// a wake target proved unreachable, so a restart never reloads a dead wake.
    /// A missing row (or missing table) is not an error.
    pub fn delete_session_wake(
        connection: &Connection,
        session_id: &str,
    ) -> Result<(), RegistryError> {
        if !session_table_present(connection)? {
            return Ok(());
        }
        connection
            .execute(
                "DELETE FROM federation_session WHERE session_id = ?1",
                [session_id],
            )
            .map_err(RegistryError::backend)?;
        Ok(())
    }

    /// Every persisted wake reference, for the startup reload. Tolerates the table
    /// being absent (a registry last written by a runtime that predates the wake
    /// table) by returning an empty list.
    pub fn list_session_wakes(
        connection: &Connection,
    ) -> Result<Vec<PersistedWake>, RegistryError> {
        if !session_table_present(connection)? {
            return Ok(Vec::new());
        }
        let mut statement = connection
            .prepare("SELECT session_id, project_id, wake_ref FROM federation_session")
            .map_err(RegistryError::backend)?;
        let rows = statement
            .query_map([], |row| {
                Ok(PersistedWake {
                    session_id: row.get(0)?,
                    project_id: row.get(1)?,
                    wake_ref: row.get(2)?,
                })
            })
            .map_err(RegistryError::backend)?;
        let mut wakes = Vec::new();
        for wake in rows {
            wakes.push(wake.map_err(RegistryError::backend)?);
        }
        Ok(wakes)
    }

    fn session_table_present(connection: &Connection) -> Result<bool, RegistryError> {
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='federation_session'",
                [],
                |row| row.get(0),
            )
            .map_err(RegistryError::backend)?;
        Ok(count == 1)
    }

    fn row_to_enrolled(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, EnrolledRow)> {
        let id: i64 = row.get(0)?;
        Ok((
            id,
            EnrolledRow {
                identity_key: row.get(1)?,
                org_id: row.get(2)?,
                project_id: row.get(3)?,
                repository_id: row.get(4)?,
                descriptor_digest: row.get(5)?,
                display_path: row.get(6)?,
                instance_id: row.get(7)?,
                broker_profile: row.get(8)?,
                broker_endpoint: row.get(9)?,
                tls_server_name: row.get(10)?,
                ca_ref: row.get(11)?,
                commit: row.get(12)?,
                capabilities: CapabilityRecord {
                    authentication: row.get::<_, i64>(13)? != 0,
                    publish: row.get::<_, i64>(14)? != 0,
                    subscribe: row.get::<_, i64>(15)? != 0,
                    self_receive: row.get::<_, i64>(16)? != 0,
                    verified_at: row.get(17)?,
                },
                remotes: Vec::new(),
            },
        ))
    }

    fn remotes_for(
        connection: &Connection,
        enrollment_id: i64,
    ) -> Result<Vec<ValidatedRemote>, RegistryError> {
        let mut statement = connection
        .prepare("SELECT name, url_digest, allowed_refs FROM federation_remote WHERE enrollment_id = ?1 ORDER BY name")
        .map_err(RegistryError::backend)?;
        let rows = statement
            .query_map([enrollment_id], |row| {
                let name: String = row.get(0)?;
                let url_digest: String = row.get(1)?;
                let allowed: String = row.get(2)?;
                Ok(ValidatedRemote {
                    name,
                    url_digest,
                    allowed_refs: allowed.split('\n').map(str::to_owned).collect(),
                })
            })
            .map_err(RegistryError::backend)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(RegistryError::backend)?);
        }
        Ok(out)
    }

    // -------------------------------------------------------------------------
    // Response-dedup ledger (2026-08-08 amendment, T17)
    // -------------------------------------------------------------------------
    //
    // The multi-terminal single-response contract's HARD layer: exactly one
    // response per `(causation_id, responder principal_id)` ships. The `emit`
    // path calls `record_response` before shipping; the first write for a pair
    // wins under `BEGIN IMMEDIATE`, and every later attempt is `AlreadyResponded`.
    // The ledger stores correlation identity only — never a message body,
    // summary, or payload. Cross-connector races are out of scope and resolve
    // through the transport's inbox-clear-after-first-response (eventually consistent).

    /// The outcome of a check-and-record against the response-dedup ledger.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DedupOutcome {
        /// This responder's response for this request was recorded; ship it.
        Recorded,
        /// A response for this `(causation_id, principal)` already exists; do not
        /// ship a duplicate.
        AlreadyResponded,
    }

    /// Record that `responder_principal_id` is responding to the request
    /// identified by `causation_id`, first-write-wins. Same-machine only; the
    /// caller ships the response only on [`DedupOutcome::Recorded`].
    pub fn record_response(
        connection: &mut Connection,
        causation_id: &str,
        responder_principal_id: &str,
        now_rfc3339: &str,
    ) -> Result<DedupOutcome, RegistryError> {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(RegistryError::backend)?;
        let existing: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM response_dedup
                 WHERE causation_id = ?1 AND responder_principal_id = ?2",
                rusqlite::params![causation_id, responder_principal_id],
                |row| row.get(0),
            )
            .ok();
        if existing.is_some() {
            transaction.rollback().map_err(RegistryError::backend)?;
            return Ok(DedupOutcome::AlreadyResponded);
        }
        transaction
            .execute(
                "INSERT INTO response_dedup (causation_id, responder_principal_id, recorded_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![causation_id, responder_principal_id, now_rfc3339],
            )
            .map_err(RegistryError::backend)?;
        transaction.commit().map_err(RegistryError::backend)?;
        Ok(DedupOutcome::Recorded)
    }

    /// Release a slot taken by [`record_response`] when the forward it was taken
    /// for provably queued nothing. Without this a transient connector outage
    /// makes a response permanently un-emittable: the slot is held forever for a
    /// message that never shipped. The caller must only reach here on an outcome
    /// that proves nothing was queued — an ambiguous result keeps the slot, so
    /// at-most-once survives.
    pub fn clear_response(
        connection: &mut Connection,
        causation_id: &str,
        responder_principal_id: &str,
    ) -> Result<(), RegistryError> {
        connection
            .execute(
                "DELETE FROM response_dedup
                 WHERE causation_id = ?1 AND responder_principal_id = ?2",
                rusqlite::params![causation_id, responder_principal_id],
            )
            .map_err(RegistryError::backend)?;
        Ok(())
    }
} // mod registry

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../tests/fixtures/mqtt/connector-descriptor-cases.json");

    fn golden_valid() -> String {
        let value = crate::json::parse(GOLDEN).expect("fixture parses");
        value.get("valid").expect("valid case present").to_json()
    }

    #[test]
    fn golden_valid_descriptor_parses() {
        let descriptor = parse_descriptor(golden_valid().as_bytes()).expect("valid descriptor");
        assert_eq!(descriptor.org_id, "acme");
        assert_eq!(descriptor.project_id, "loam");
        assert_eq!(
            descriptor.broker.endpoint,
            "mqtts://broker.acme.example:8883"
        );
        assert_eq!(
            descriptor.broker.ca_ref.as_deref(),
            Some("vault://acme/loam/org-ca")
        );
        assert_eq!(descriptor.git.remotes.len(), 1);
        assert_eq!(descriptor.git.remotes[0].refs.len(), 2);
    }

    #[test]
    fn sha256_commit_is_accepted() {
        let value = crate::json::parse(GOLDEN).expect("fixture parses");
        let json = value.get("valid_sha256_commit").unwrap().to_json();
        assert!(parse_descriptor(json.as_bytes()).is_ok());
    }

    #[test]
    fn oversize_descriptor_is_rejected_before_parse() {
        let big = vec![b' '; MAX_DESCRIPTOR_BYTES + 1];
        assert_eq!(
            parse_descriptor(&big),
            Err(EnrollmentError::TooLarge {
                bytes: MAX_DESCRIPTOR_BYTES + 1
            })
        );
    }

    #[test]
    fn non_utf8_is_rejected() {
        assert_eq!(
            parse_descriptor(&[0xff, 0xfe]),
            Err(EnrollmentError::NotUtf8)
        );
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert!(matches!(
            parse_descriptor(b"{not json"),
            Err(EnrollmentError::InvalidJson(_))
        ));
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let json = with_extra(r#""note":"hi""#);
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::UnknownField { key: "note".into() })
        );
    }

    #[test]
    fn duplicate_field_is_rejected() {
        // Two org_id keys; the parser preserves both in order so we can detect it.
        let json = r#"{"schema":1,"org_id":"a","org_id":"b","project_id":"p","repository_id":"r","broker":{"profile":"x","endpoint":"mqtts://h:8883","tls_server_name":"h"},"git":{"commit":"0123456789abcdef0123456789abcdef01234567","remotes":[{"name":"origin","refs":["refs/heads/main"]}]}}"#;
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::DuplicateField {
                key: "org_id".into()
            })
        );
    }

    #[test]
    fn secret_shaped_field_is_rejected() {
        for shape in [
            "password",
            "token",
            "private_key",
            "principal_id",
            "agent_id",
            "source",
        ] {
            let json = with_extra(&format!(r#""{shape}":"x""#));
            assert_eq!(
                parse_descriptor(json.as_bytes()),
                Err(EnrollmentError::ForbiddenField { key: shape.into() }),
                "field {shape} must be forbidden"
            );
        }
    }

    #[test]
    fn wrong_schema_is_rejected() {
        let json = golden_valid().replace("\"schema\":1", "\"schema\":2");
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::UnsupportedSchema)
        );
    }

    #[test]
    fn plaintext_endpoint_is_rejected() {
        let json = golden_valid().replace("mqtts://", "mqtt://");
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidEndpoint)
        );
    }

    #[test]
    fn endpoint_with_userinfo_is_rejected() {
        let json = golden_valid().replace(
            "mqtts://broker.acme.example:8883",
            "mqtts://user:pass@broker.acme.example:8883",
        );
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidEndpoint)
        );
    }

    #[test]
    fn endpoint_with_query_is_rejected() {
        let json = golden_valid().replace(
            "mqtts://broker.acme.example:8883",
            "mqtts://broker.acme.example:8883?x=1",
        );
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidEndpoint)
        );
    }

    #[test]
    fn bad_commit_is_rejected() {
        let json = golden_valid().replace(
            "0123456789abcdef0123456789abcdef01234567",
            "0123456789ABCDEF0123456789abcdef01234567",
        );
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidCommit)
        );
    }

    #[test]
    fn non_full_ref_is_rejected() {
        let json = golden_valid().replace("refs/heads/main", "main");
        assert!(matches!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidRef { .. })
        ));
    }

    #[test]
    fn empty_id_is_rejected() {
        let json = golden_valid().replace("\"org_id\":\"acme\"", "\"org_id\":\"\"");
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidField { field: "org_id" })
        );
    }

    #[test]
    fn control_character_id_is_rejected() {
        let json = golden_valid().replace("\"org_id\":\"acme\"", "\"org_id\":\"a\\u0007c\"");
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::InvalidField { field: "org_id" })
        );
    }

    #[test]
    fn too_many_remotes_is_rejected() {
        let many = (0..9)
            .map(|i| format!(r#"{{"name":"r{i}","refs":["refs/heads/main"]}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let json = golden_valid().replace(
            r#""remotes":[{"name":"origin","refs":["refs/heads/main","refs/heads/federation"]}]"#,
            &format!(r#""remotes":[{many}]"#),
        );
        assert_eq!(
            parse_descriptor(json.as_bytes()),
            Err(EnrollmentError::TooManyRemotes)
        );
    }

    #[test]
    fn digest_rejects_credential_bearing_url() {
        assert_eq!(
            digest_remote_url("https://user:pass@example.com/repo.git", "origin"),
            Err(EnrollmentError::CredentialBearingRemote {
                remote: "origin".into()
            })
        );
    }

    #[test]
    fn digest_allows_scp_style_and_is_stable() {
        let a = digest_remote_url("git@github.com:acme/loam.git", "origin").expect("scp ok");
        let b = digest_remote_url("git@github.com:acme/loam.git", "origin").expect("scp ok");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    // --- helpers ---

    fn with_extra(extra_pair: &str) -> String {
        // Insert an extra top-level pair after the opening brace of the golden
        // valid descriptor.
        let valid = golden_valid();
        format!("{{{extra_pair},{}", &valid[1..])
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use rusqlite::{Connection, TransactionBehavior};

    fn temp_db(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "loam-registry-{label}-{}.sqlite3",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn sample(device: u64, inode: u64, commit: &str) -> ValidatedEnrollment {
        ValidatedEnrollment {
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            broker_profile: "acme-prod".into(),
            broker_endpoint: "mqtts://broker.acme.example:8883".into(),
            tls_server_name: "broker.acme.example".into(),
            ca_ref: None,
            commit: commit.into(),
            remotes: vec![ValidatedRemote {
                name: "origin".into(),
                url_digest: "a".repeat(64),
                allowed_refs: vec!["refs/heads/main".into()],
            }],
            workspace: PhysicalWorkspace {
                display_path: "/w/proj".into(),
                identity: PlatformIdentity::Unix { device, inode },
            },
        }
    }

    fn caps() -> CapabilityRecord {
        CapabilityRecord {
            authentication: true,
            publish: true,
            subscribe: true,
            self_receive: true,
            verified_at: "2026-08-08T10:00:00Z".into(),
        }
    }

    #[test]
    fn a_read_never_creates_the_store() {
        let path = temp_db("no-create");
        assert!(open_readonly(&path).unwrap().is_none());
        assert!(!path.is_file(), "read must not create the database");
    }

    #[test]
    fn insert_lookup_and_list_round_trip() {
        let path = temp_db("round-trip");
        let mut connection = open_writable(&path).unwrap();
        let enrolled = sample(1, 10, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(
            insert_enrollment(
                &mut connection,
                &enrolled,
                "instance-under-test",
                &caps(),
                "2026-08-08T10:00:00Z"
            )
            .unwrap(),
            InsertOutcome::Inserted
        );
        let key = identity_key(&enrolled.workspace);
        let row = lookup(&connection, &key).unwrap().expect("present");
        assert_eq!(row.org_id, "acme");
        assert_eq!(row.commit, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(row.remotes.len(), 1);
        assert_eq!(row.remotes[0].url_digest, "a".repeat(64));
        assert!(row.capabilities.self_receive);
        assert_eq!(list_enrollments(&connection).unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_read_projection_carries_the_provisioning_fields() {
        let path = temp_db("provisioning-projection");
        let mut connection = open_writable(&path).unwrap();

        let mut pinned = sample(4, 40, "0123456789abcdef0123456789abcdef01234567");
        pinned.ca_ref = Some("vault://acme/loam/ca".into());
        insert_enrollment(
            &mut connection,
            &pinned,
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();
        let row = lookup(&connection, &identity_key(&pinned.workspace))
            .unwrap()
            .expect("present");
        assert_eq!(row.broker_endpoint, "mqtts://broker.acme.example:8883");
        assert_eq!(row.tls_server_name, "broker.acme.example");
        assert_eq!(row.ca_ref.as_deref(), Some("vault://acme/loam/ca"));
        // The instance id is the single source of session identity downstream,
        // so a projection that reads it back empty would be a silent defect.
        assert!(!row.instance_id.is_empty());

        // Both readers share one projection: widening the SELECT in `lookup` and
        // not in `list_enrollments` is exactly the half-fix that reads clean here.
        let listed = list_enrollments(&connection).unwrap();
        assert_eq!(listed[0].broker_endpoint, row.broker_endpoint);
        assert_eq!(listed[0].ca_ref, row.ca_ref);

        // Control: an absent CA reference reads back absent, never an empty
        // string — "use system roots" and "pinned to nothing" must not collapse.
        let mut system_roots = sample(5, 50, "0123456789abcdef0123456789abcdef01234567");
        system_roots.ca_ref = None;
        insert_enrollment(
            &mut connection,
            &system_roots,
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();
        let absent = lookup(&connection, &identity_key(&system_roots.workspace))
            .unwrap()
            .expect("present");
        assert!(absent.ca_ref.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn same_physical_workspace_is_idempotent_and_conflict_aware() {
        let path = temp_db("idempotent");
        let mut connection = open_writable(&path).unwrap();
        let enrolled = sample(2, 20, "0123456789abcdef0123456789abcdef01234567");
        insert_enrollment(
            &mut connection,
            &enrolled,
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();

        // Same identity, same digest -> AlreadyEnrolled, no duplicate row.
        assert_eq!(
            insert_enrollment(
                &mut connection,
                &enrolled,
                "instance-under-test",
                &caps(),
                "t"
            )
            .unwrap(),
            InsertOutcome::AlreadyEnrolled
        );
        assert_eq!(list_enrollments(&connection).unwrap().len(), 1);

        // Same identity, different binding (commit) -> Conflict, still one row.
        let changed = sample(2, 20, "ffffffffffffffffffffffffffffffffffffffff");
        assert_eq!(
            insert_enrollment(
                &mut connection,
                &changed,
                "instance-under-test",
                &caps(),
                "t"
            )
            .unwrap(),
            InsertOutcome::Conflict
        );
        let row = lookup(&connection, &identity_key(&enrolled.workspace))
            .unwrap()
            .unwrap();
        assert_eq!(
            row.commit, "0123456789abcdef0123456789abcdef01234567",
            "conflict must not overwrite"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn delete_removes_then_reports_absent() {
        let path = temp_db("delete");
        let mut connection = open_writable(&path).unwrap();
        let enrolled = sample(3, 30, "0123456789abcdef0123456789abcdef01234567");
        insert_enrollment(
            &mut connection,
            &enrolled,
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();
        let key = identity_key(&enrolled.workspace);
        assert!(delete_enrollment(&mut connection, &key).unwrap());
        assert!(!delete_enrollment(&mut connection, &key).unwrap());
        assert!(lookup(&connection, &key).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn federation_never_touches_hook_user_version() {
        let path = temp_db("user-version");
        let connection = open_writable(&path).unwrap();
        // A fresh federation-only database leaves user_version at its default 0.
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
        // Simulate the hook store claiming version 2, then run a federation write.
        connection
            .execute_batch("PRAGMA user_version = 2;")
            .unwrap();
        drop(connection);
        let mut connection = open_writable(&path).unwrap();
        insert_enrollment(
            &mut connection,
            &sample(4, 40, "0123456789abcdef0123456789abcdef01234567"),
            "instance-under-test",
            &caps(),
            "t",
        )
        .unwrap();
        let after: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            after, 2,
            "federation must not alter hook-owned user_version"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_busy_writer_times_out_rather_than_corrupting() {
        // A second immediate transaction against a write-locked database must
        // fail (busy) within its short timeout rather than block forever or
        // corrupt state — proving `BEGIN IMMEDIATE` + busy ceiling behave.
        let path = temp_db("busy");
        let mut holder = open_writable(&path).unwrap();
        let held = holder
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        held.execute_batch("CREATE TABLE IF NOT EXISTS lock_marker(a)")
            .unwrap(); // acquire the write lock

        let second = Connection::open(&path).unwrap();
        second
            .busy_timeout(std::time::Duration::from_millis(20))
            .unwrap();
        let result = second.execute_batch("BEGIN IMMEDIATE; CREATE TABLE x(a); COMMIT;");
        assert!(
            result.is_err(),
            "a write-locked database must refuse the second immediate writer"
        );

        drop(held);
        let _ = std::fs::remove_file(&path);
    }

    // --- T17 response-dedup ledger ---

    #[test]
    fn first_response_wins_and_duplicate_is_already_responded() {
        let path = temp_db("dedup-basic");
        let mut connection = open_writable(&path).unwrap();
        assert_eq!(
            record_response(&mut connection, "cause-1", "employee-184", "t1").unwrap(),
            DedupOutcome::Recorded
        );
        assert_eq!(
            record_response(&mut connection, "cause-1", "employee-184", "t2").unwrap(),
            DedupOutcome::AlreadyResponded
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_cleared_slot_can_be_taken_again_but_an_uncleared_one_cannot() {
        // The slot is taken before the forward, so a forward
        // that provably queued nothing must give it back — otherwise one
        // transient outage makes that response permanently un-emittable.
        let path = temp_db("dedup-clear");
        let mut connection = open_writable(&path).unwrap();
        assert_eq!(
            record_response(&mut connection, "cause-9", "instance-01", "t1").unwrap(),
            DedupOutcome::Recorded
        );
        clear_response(&mut connection, "cause-9", "instance-01").unwrap();
        assert_eq!(
            record_response(&mut connection, "cause-9", "instance-01", "t2").unwrap(),
            DedupOutcome::Recorded,
            "a released slot must be takeable again"
        );

        // Positive control: without the release the slot stays held, so the
        // clear above is what changed the outcome — not a ledger that never
        // held anything.
        assert_eq!(
            record_response(&mut connection, "cause-9", "instance-01", "t3").unwrap(),
            DedupOutcome::AlreadyResponded
        );
        // Clearing a slot that was never taken is a no-op, not an error.
        clear_response(&mut connection, "cause-absent", "instance-01").unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn different_principal_or_causation_are_not_blocked() {
        let path = temp_db("dedup-distinct");
        let mut connection = open_writable(&path).unwrap();
        assert_eq!(
            record_response(&mut connection, "cause-1", "employee-184", "t").unwrap(),
            DedupOutcome::Recorded
        );
        // Same request, a *different* principal still records.
        assert_eq!(
            record_response(&mut connection, "cause-1", "employee-999", "t").unwrap(),
            DedupOutcome::Recorded
        );
        // Same principal, a *different* request still records.
        assert_eq!(
            record_response(&mut connection, "cause-2", "employee-184", "t").unwrap(),
            DedupOutcome::Recorded
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_ledger_stores_no_message_body() {
        let path = temp_db("dedup-bodyfree");
        let mut connection = open_writable(&path).unwrap();
        record_response(
            &mut connection,
            "cause-1",
            "employee-184",
            "2026-08-08T10:00:00Z",
        )
        .unwrap();
        // The table's columns are correlation identity + timestamp only.
        let columns: Vec<String> = {
            let mut statement = connection
                .prepare("SELECT name FROM pragma_table_info('response_dedup')")
                .unwrap();
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(
            columns,
            vec![
                "causation_id".to_owned(),
                "responder_principal_id".to_owned(),
                "recorded_at".to_owned()
            ],
            "the ledger must carry no body/summary/payload column"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concurrent_emits_for_one_pair_ship_exactly_one() {
        use std::sync::{Arc, Barrier};
        let path = Arc::new(temp_db("dedup-race"));
        // Create the schema up front so every worker opens an existing DB.
        drop(open_writable(&path).unwrap());

        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::new();
        for _ in 0..workers {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut connection = open_writable(&path).unwrap();
                barrier.wait();
                record_response(&mut connection, "cause-race", "employee-184", "t").unwrap()
            }));
        }
        let outcomes: Vec<DedupOutcome> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let recorded = outcomes
            .iter()
            .filter(|o| **o == DedupOutcome::Recorded)
            .count();
        assert_eq!(recorded, 1, "exactly one concurrent responder may record");
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == DedupOutcome::AlreadyResponded)
                .count(),
            workers - 1
        );
        let _ = std::fs::remove_file(&*path);
    }
}
