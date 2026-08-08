//! Slice C enrollment: descriptor validation, physical Git workspace identity,
//! remote-URL digests, and isolated commit-reachability proof.
//!
//! This module turns a bounded, non-secret stdin descriptor into a typed
//! [`ValidatedEnrollment`] candidate. It performs every trust-boundary check
//! before any registry, service-manager, credential, or transport work happens
//! elsewhere: exact schema and field inventory, no secret- or authority-shaped
//! field, a `mqtts://` endpoint without userinfo, a physical workspace identity
//! that path aliases cannot duplicate, remote URLs resolved from local Git and
//! reduced to SHA-256 digests, and proof that the declared commit is reachable
//! from an allowed ref — proven in an isolated temporary repository that never
//! touches the enrolled worktree.
//!
//! It constructs no `AuthenticatedPrincipal` and resolves no credential; those
//! belong to the transport adapter (Slice B seam, Slice C T4/T10).

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

/// The only keys a descriptor may carry, top level.
const DESCRIPTOR_KEYS: &[&str] = &[
    "schema",
    "org_id",
    "project_id",
    "repository_id",
    "broker",
    "git",
];
const BROKER_KEYS: &[&str] = &[
    "profile",
    "endpoint",
    "tls_server_name",
    "credential_ref",
    "ca_ref",
];
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
    CommitUnreachable,
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
            EnrollmentError::CommitUnreachable => "commit_unreachable",
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
            EnrollmentError::CommitUnreachable => {
                write!(f, "git.commit is not reachable from any allowed ref")
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
    pub credential_ref: String,
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
    Unix { device: u64, inode: u64 },
    /// Windows carries only the canonical final path in Slice C T2; the
    /// volume-serial/file-index refinement lands in T7 alongside the other
    /// Win32 handle FFI, where the hosted Windows CI leg proves it.
    // ponytail: Windows volume/file-index deferred to T7 (all Win32 raw FFI in one CI-verified place)
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
    pub credential_ref: String,
    pub ca_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDescriptor {
    pub commit: String,
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
    let credential_ref = required_bounded_string(entries, "credential_ref")?;
    let ca_ref = optional_bounded_string(entries, "ca_ref")?;
    Ok(BrokerDescriptor {
        profile,
        endpoint,
        tls_server_name,
        credential_ref,
        ca_ref,
    })
}

fn parse_git(entries: &[(String, crate::json::Value)]) -> Result<GitDescriptor, EnrollmentError> {
    reject_forbidden_and_check_keys(entries, GIT_KEYS)?;
    let commit = required_bounded_string(entries, "commit")?;
    validate_commit(&commit)?;

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
    // Windows uses the canonical final path as identity in T2; T7 adds the
    // volume-serial/file-index refinement under the reviewed Win32 FFI.
    Ok(PlatformIdentity::WindowsPath)
}

// ---------------------------------------------------------------------------
// Remote resolution and commit reachability
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

/// Prove the declared commit is reachable from one of the allowed refs, in an
/// isolated temporary bare repository. Fetches only the exact named refs from
/// the workspace's remotes into private temp refs, then checks ancestry. Never
/// fetches into or mutates the enrolled worktree.
fn prove_commit_reachable(
    workspace: &Path,
    remotes: &[ValidatedRemote],
    descriptor_remotes: &[RemoteDescriptor],
    commit: &str,
) -> Result<(), EnrollmentError> {
    let temp = TempRepo::init()?;
    let workspace_str = workspace
        .to_str()
        .ok_or(EnrollmentError::WorkspaceNotUtf8)?;

    let mut any_ref = false;
    for (validated, descriptor) in remotes.iter().zip(descriptor_remotes.iter()) {
        // Resolve the remote's URL from the *workspace* config (already digest
        // matched) so the isolated repo fetches from the same place the enrolled
        // repo would, without copying credentials into the temp config.
        let url = remote_url(workspace, &validated.name)?;
        for (index, refspec) in descriptor.refs.iter().enumerate() {
            let dest = format!("refs/enroll/{}/{index}", validated.name);
            let output = Command::new("git")
                .args([
                    "-C",
                    temp.path_str()?,
                    "-c",
                    // Never prompt for credentials; a private ref fetch must fail
                    // closed rather than block or read the user's helper.
                    "credential.helper=",
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    &url,
                    &format!("{refspec}:{dest}"),
                ])
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map_err(|_| EnrollmentError::GitUnavailable)?;
            if output.status.success() {
                any_ref = true;
                if is_ancestor(&temp, commit, &dest)? {
                    return Ok(());
                }
            }
        }
    }
    let _ = workspace_str;
    if any_ref {
        Err(EnrollmentError::CommitUnreachable)
    } else {
        // No allowed ref could be fetched at all.
        Err(EnrollmentError::CommitUnreachable)
    }
}

fn is_ancestor(temp: &TempRepo, commit: &str, ref_name: &str) -> Result<bool, EnrollmentError> {
    let output = Command::new("git")
        .args([
            "-C",
            temp.path_str()?,
            "merge-base",
            "--is-ancestor",
            commit,
            ref_name,
        ])
        .output()
        .map_err(|_| EnrollmentError::GitUnavailable)?;
    Ok(output.status.success())
}

/// An isolated temporary bare repository that cleans itself up on drop.
struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn init() -> Result<TempRepo, EnrollmentError> {
        let path = std::env::temp_dir().join(format!(
            "loam-enroll-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).map_err(|_| EnrollmentError::GitUnavailable)?;
        let output = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&path)
            .output()
            .map_err(|_| EnrollmentError::GitUnavailable)?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&path);
            return Err(EnrollmentError::GitUnavailable);
        }
        Ok(TempRepo { path })
    }

    fn path_str(&self) -> Result<&str, EnrollmentError> {
        self.path.to_str().ok_or(EnrollmentError::WorkspaceNotUtf8)
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Validate a descriptor against a physical workspace end to end: structural
/// validation, physical identity, remote resolution + digest + userinfo
/// rejection, and isolated commit-reachability proof. Returns the non-secret
/// [`ValidatedEnrollment`] projection or the first typed violation.
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

    prove_commit_reachable(
        &canonical,
        &remotes,
        &descriptor.git.remotes,
        &descriptor.git.commit,
    )?;

    Ok(ValidatedEnrollment {
        org_id: descriptor.org_id,
        project_id: descriptor.project_id,
        repository_id: descriptor.repository_id,
        broker_profile: descriptor.broker.profile,
        broker_endpoint: descriptor.broker.endpoint,
        tls_server_name: descriptor.broker.tls_server_name,
        credential_ref: descriptor.broker.credential_ref,
        ca_ref: descriptor.broker.ca_ref,
        commit: descriptor.git.commit,
        remotes,
        workspace,
    })
}

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
        let json = r#"{"schema":1,"org_id":"a","org_id":"b","project_id":"p","repository_id":"r","broker":{"profile":"x","endpoint":"mqtts://h:8883","tls_server_name":"h","credential_ref":"vault://c"},"git":{"commit":"0123456789abcdef0123456789abcdef01234567","remotes":[{"name":"origin","refs":["refs/heads/main"]}]}}"#;
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
