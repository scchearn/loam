//! Turning one enrolled row into the inputs a live broker session needs.
//!
//! This module exists to keep the secret-touching work out of `connector.rs`.
//! The connector holds the broker socket; giving it filesystem and subprocess
//! reach as well would concentrate exactly what the crate capability guard
//! exists to separate. `connector::provision_session` delegates here, and this
//! module is admitted to the guard's allowlists by two explicit named lines —
//! one for the files it reads, one for the secret-lookup subprocess.
//!
//! What crosses the boundary, and what never does:
//!
//! - **In:** opaque references from the enrollment row. They are lookup keys,
//!   never material, and they are passed to the backend verbatim — this module
//!   parses no scheme and splits no components out of them.
//! - **Out:** an `MqttSession` holding the material, and a `PeerRoster`. Nothing
//!   else. Every failure is a stable reason naming the *input* that failed, and
//!   no failure carries a byte of what it was looking for.
//!
//! The secret itself is read from the backend's standard output. It is never an
//! argument, so it never appears in the process table where any local user can
//! read it; the reference does appear in argv, which is the point of references.

use crate::connector::{reason, ProvisionFailure};

/// Where the resolved credential material came from and how it is shaped. Held
/// only long enough to build the session; a hand-written `Debug` keeps the
/// material out of any diagnostic that formats it.
pub struct CredentialMaterial {
    /// The client certificate chain, PEM.
    pub certificate: Vec<u8>,
    /// The private key, PEM.
    pub key: Vec<u8>,
    /// The trust anchors to verify the broker against: the pinned CA when the
    /// enrollment names one, the platform bundle when it does not.
    pub certificate_authority: Vec<u8>,
}

impl std::fmt::Debug for CredentialMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Lengths, never bytes. A derived `Debug` would print the private key
        // the first time a test unwrapped an error holding this value.
        formatter
            .debug_struct("CredentialMaterial")
            .field("certificate_bytes", &self.certificate.len())
            .field("key_bytes", &self.key.len())
            .field(
                "certificate_authority_bytes",
                &self.certificate_authority.len(),
            )
            .finish()
    }
}

/// Which platform secret store to ask, and with which program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Linux libsecret: `secret-tool lookup service loam-federation ref <ref>`.
    SecretTool(String),
    /// macOS keychain: `security find-generic-password -s loam-federation -a <ref> -w`.
    Security(String),
    /// Any other program, invoked as `<program> <ref>` and expected to write the
    /// secret to standard output. This is what an unusual host and the test tier
    /// use; it is reached only through an explicit `LOAM_SECRET_BACKEND`.
    Custom(String),
}

/// The keychain service every Loam federation secret is filed under.
const SERVICE: &str = "loam-federation";

/// Choose the backend from an explicit override, else from the platform.
///
/// Taking both as arguments rather than reading the environment inside keeps
/// this decidable in a test without mutating process-global state that a
/// concurrently running test would see.
pub fn select_backend(
    override_value: Option<&str>,
    operating_system: &str,
) -> Result<Backend, &'static str> {
    match override_value.map(str::trim).filter(|v| !v.is_empty()) {
        Some("secret-tool") => Ok(Backend::SecretTool("secret-tool".to_owned())),
        Some("security") => Ok(Backend::Security("security".to_owned())),
        Some(other) => Ok(Backend::Custom(other.to_owned())),
        None => match operating_system {
            "linux" => Ok(Backend::SecretTool("secret-tool".to_owned())),
            "macos" => Ok(Backend::Security("security".to_owned())),
            // Fail closed rather than guessing at a store: an unknown platform
            // with no override has no secret backend we can name.
            _ => Err(reason::CREDENTIAL_REF_UNRESOLVED),
        },
    }
}

/// The exact program and argument list one backend uses for one reference.
///
/// Built here rather than inline so the shape is pinned by a test: the
/// reference is passed **verbatim** as a single argument, because it is an
/// opaque key and any parsing here would silently disagree with whatever wrote
/// the secret. Note what is *not* in this list — the secret itself never
/// appears in an argument, so it never reaches the process table.
pub fn invocation(backend: &Backend, reference: &str) -> (String, Vec<String>) {
    let own = |values: [&str; 5]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    match backend {
        Backend::SecretTool(program) => (
            program.clone(),
            own(["lookup", "service", SERVICE, "ref", reference]),
        ),
        Backend::Security(program) => (
            program.clone(),
            vec![
                "find-generic-password".to_owned(),
                "-s".to_owned(),
                SERVICE.to_owned(),
                "-a".to_owned(),
                reference.to_owned(),
                "-w".to_owned(),
            ],
        ),
        Backend::Custom(program) => (program.clone(), vec![reference.to_owned()]),
    }
}

/// Ask the backend for one reference. `None` covers every failure — program
/// missing, non-zero exit, empty answer — because the caller's answer is the
/// same in all three and naming which would leak whether a reference exists.
pub fn lookup(backend: &Backend, reference: &str) -> Option<Vec<u8>> {
    let (program, arguments) = invocation(backend, reference);
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    // The secret arrives on stdout. Inherited stderr would put a backend's
    // diagnostic on the connector's own error stream, so it is discarded.
    command.stdin(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    let output = command.output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(output.stdout)
}

/// Split the stored `mqtts://host:port` into the host and port the transport
/// takes. The same authority rules enrollment validated on the way in: no
/// userinfo, no path, no query, no fragment, and never plaintext `mqtt://`.
pub fn split_endpoint(endpoint: &str) -> Result<(String, u16), &'static str> {
    let malformed = reason::ENDPOINT_MALFORMED;
    let authority = endpoint.strip_prefix("mqtts://").ok_or(malformed)?;
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains('?')
        || authority.contains('#')
        || authority.contains('/')
    {
        return Err(malformed);
    }
    let (host, port) = authority.rsplit_once(':').ok_or(malformed)?;
    if host.is_empty() {
        return Err(malformed);
    }
    let port: u16 = port.parse().map_err(|_| malformed)?;
    if port == 0 {
        return Err(malformed);
    }
    Ok((host.to_owned(), port))
}

/// One PEM block located in a blob: its label and the byte range it occupies.
struct PemBlock {
    label: String,
    start: usize,
    end: usize,
}

/// Locate every well-formed PEM block, in order. A `BEGIN` with no matching
/// `END`, or an `END` whose label differs, ends the scan: a half-read block is
/// never handed on.
fn pem_blocks(blob: &str) -> Vec<PemBlock> {
    let mut blocks = Vec::new();
    let mut open: Option<(String, usize)> = None;
    let mut offset = 0usize;
    for line in blob.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(label) = trimmed
            .strip_prefix("-----BEGIN ")
            .and_then(|rest| rest.strip_suffix("-----"))
        {
            if open.is_some() {
                // A second BEGIN inside an open block: the blob is not a
                // sequence of PEM objects, so stop rather than guess.
                return blocks;
            }
            open = Some((label.to_owned(), offset));
        } else if let Some(label) = trimmed
            .strip_prefix("-----END ")
            .and_then(|rest| rest.strip_suffix("-----"))
        {
            match open.take() {
                Some((opened, start)) if opened == label => blocks.push(PemBlock {
                    label: opened,
                    start,
                    end: offset + line.len(),
                }),
                _ => return blocks,
            }
        }
        offset += line.len();
    }
    blocks
}

/// Split one resolved blob into the client certificate chain and its key.
///
/// The contract is one reference resolving to one blob holding the certificate
/// first and the key second. The order is checked rather than assumed: a
/// swapped blob would otherwise fail deep inside the TLS handshake with a
/// diagnostic that names neither input.
pub fn split_credential(blob: &[u8]) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let unresolved = reason::CREDENTIAL_REF_UNRESOLVED;
    let text = std::str::from_utf8(blob).map_err(|_| unresolved)?;
    let blocks = pem_blocks(text);
    let Some((key_index, key_block)) = blocks
        .iter()
        .enumerate()
        .find(|(_, block)| block.label.contains("KEY"))
    else {
        return Err(unresolved);
    };
    // Certificates first, key second, nothing after: anything else is a blob
    // this reader does not understand, and half-understanding it is worse.
    if key_index == 0 || key_index + 1 != blocks.len() {
        return Err(unresolved);
    }
    if !blocks[..key_index]
        .iter()
        .all(|block| block.label.contains("CERTIFICATE"))
    {
        return Err(unresolved);
    }
    // "Then nothing" is enforced rather than described. Every byte outside an
    // accepted block must be whitespace, so trailing junk, a stray marker, and
    // an unclosed block at the end are all refused instead of quietly ignored —
    // `pem_blocks` stops scanning at the first thing it does not understand, and
    // without this check whatever followed would simply disappear.
    let mut covered = 0usize;
    for block in &blocks {
        if !text[covered..block.start].trim().is_empty() {
            return Err(unresolved);
        }
        covered = block.end;
    }
    if !text[covered..].trim().is_empty() {
        return Err(unresolved);
    }
    let certificate = &text.as_bytes()[blocks[0].start..blocks[key_index - 1].end];
    let key = &text.as_bytes()[key_block.start..key_block.end];
    Ok((certificate.to_owned(), key.to_owned()))
}

/// Well-known platform trust bundles, tried in order when an enrollment pins no
/// CA. `SSL_CERT_FILE` comes first because it is the conventional override and
/// is what a container or an unusual host already sets.
const SYSTEM_TRUST_BUNDLES: [&str; 4] = [
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/cert.pem",
    "/usr/local/etc/openssl/cert.pem",
];

/// The platform trust anchors, as PEM.
///
/// "Absent `ca_ref` means system roots" needs real bytes: the transport builds
/// its root store from the PEM it is handed, and an empty store refuses every
/// connection. Finding no bundle is therefore a refusal, not a quiet fallback
/// to trusting nothing — or, worse, to trusting everything.
pub fn system_trust_anchors(override_path: Option<&str>) -> Result<Vec<u8>, &'static str> {
    system_trust_anchors_among(override_path, &SYSTEM_TRUST_BUNDLES)
}

/// The same search over an explicit candidate list.
///
/// The list is a parameter so a test can point every candidate at a path that
/// does not exist and prove the refusal *unconditionally*. With the constant
/// baked in, any host carrying a real bundle — which is every Linux host — would
/// satisfy the search and the "found nothing, so refuse" branch would never run.
/// That branch is the one place a trust bypass could hide, so it is the one that
/// most needs a proof that does not depend on the machine it runs on.
pub fn system_trust_anchors_among(
    override_path: Option<&str>,
    candidates: &[&str],
) -> Result<Vec<u8>, &'static str> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(path) = override_path.map(str::trim).filter(|v| !v.is_empty()) {
        paths.push(path.to_owned());
    }
    paths.extend(candidates.iter().map(|path| (*path).to_owned()));
    for candidate in paths {
        if let Ok(bytes) = std::fs::read(&candidate) {
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }
    Err(reason::CA_UNRESOLVED)
}

/// Resolve the trust anchors for one enrollment: the pinned CA when it names
/// one, the platform bundle when it does not. Both branches must produce real
/// bytes; neither may fall through to an empty store.
pub fn resolve_trust_anchors(
    backend: &Backend,
    ca_ref: Option<&str>,
    system_override: Option<&str>,
) -> Result<Vec<u8>, &'static str> {
    match ca_ref {
        // Present but blank is a malformed reference, not an absent one. Reading
        // it as "no CA pinned" would turn a typo into a silently wider trust
        // decision — the same downgrade an unresolvable reference is refused for.
        Some(reference) if reference.trim().is_empty() => Err(reason::CA_UNRESOLVED),
        Some(reference) => lookup(backend, reference).ok_or(reason::CA_UNRESOLVED),
        None => system_trust_anchors(system_override),
    }
}

/// Resolve the credential material for one enrollment.
pub fn resolve_credentials(
    backend: &Backend,
    credential_ref: &str,
    ca_ref: Option<&str>,
    system_override: Option<&str>,
) -> Result<CredentialMaterial, ProvisionFailure> {
    if credential_ref.trim().is_empty() {
        // Refused before the backend is asked: a blank reference cannot resolve,
        // and asking is a subprocess spent to learn that.
        return Err(ProvisionFailure::Credentials(
            reason::CREDENTIAL_REF_UNRESOLVED,
        ));
    }
    let blob = lookup(backend, credential_ref).ok_or(ProvisionFailure::Credentials(
        reason::CREDENTIAL_REF_UNRESOLVED,
    ))?;
    let (certificate, key) = split_credential(&blob).map_err(ProvisionFailure::Credentials)?;
    let certificate_authority = resolve_trust_anchors(backend, ca_ref, system_override)
        .map_err(ProvisionFailure::Credentials)?;
    Ok(CredentialMaterial {
        certificate,
        key,
        certificate_authority,
    })
}

// ---------------------------------------------------------------------------
// Who the certificate says we are
// ---------------------------------------------------------------------------
//
// The authenticated identity is read from the client certificate's subject and
// from nowhere else. Two attributes matter: the common name, which is the
// operator's email and is the principal the broker authenticated, and the given
// name, which is the display name a colleague sees.
//
// The subject is located **positionally**. `tbsCertificate` carries the issuer
// Name before the subject Name, so a scan for the first common-name OID returns
// the *issuer* — and against this repository's own test CA, whose subject is
// `CN=Loam MQTT Test CA`, that scanner would pass every "does it find a common
// name" test while attributing every message to the certificate authority.

/// The subject attributes a session's identity is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateSubject {
    /// Subject common name, OID 2.5.4.3 — the authenticated principal.
    pub common_name: String,
    /// Subject given name, OID 2.5.4.42. Absent on most certificates, which is
    /// ordinary: the identity is the common name and the given name is shown.
    pub given_name: Option<String>,
}

/// OID 2.5.4.3, `id-at-commonName`, as its DER content bytes.
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
/// OID 2.5.4.42, `id-at-givenName`, as its DER content bytes.
const OID_GIVEN_NAME: &[u8] = &[0x55, 0x04, 0x2a];

/// One parsed DER element: its tag, its content range, and where the next
/// element starts. Every length is bounded against the buffer, so a truncated
/// or over-long encoding is a refusal and never a panic.
struct Element {
    tag: u8,
    content: (usize, usize),
    next: usize,
}

fn read_element(bytes: &[u8], from: usize) -> Option<Element> {
    let tag = *bytes.get(from)?;
    let first = *bytes.get(from + 1)?;
    let (length, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let count = (first & 0x7f) as usize;
        // Four bytes is already a 4 GiB element; anything longer is a hostile
        // or corrupt encoding rather than a certificate.
        if count == 0 || count > 4 {
            return None;
        }
        let mut length = 0usize;
        for offset in 0..count {
            length = (length << 8) | *bytes.get(from + 2 + offset)? as usize;
        }
        (length, 2 + count)
    };
    let start = from + header;
    let end = start.checked_add(length)?;
    if end > bytes.len() {
        return None;
    }
    Some(Element {
        tag,
        content: (start, end),
        next: end,
    })
}

/// Decode one PEM block body into DER. Written here rather than taken as a
/// dependency: the crate ships four dependencies and the guard checks that list.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for character in input.chars() {
        if character.is_whitespace() {
            continue;
        }
        if character == '=' {
            break;
        }
        let value = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => character as u32 - 'a' as u32 + 26,
            '0'..='9' => character as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((accumulator >> bits) as u8);
        }
    }
    Some(output)
}

/// The DER bytes of the first certificate in a PEM blob.
fn first_certificate_der(pem: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let block = pem_blocks(text)
        .into_iter()
        .find(|block| block.label.contains("CERTIFICATE"))?;
    let body: String = text[block.start..block.end]
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    base64_decode(&body)
}

/// Read the subject attributes out of one PEM client certificate.
///
/// The walk is: Certificate SEQUENCE, then tbsCertificate SEQUENCE, then skip
/// the optional explicit version and the serial number, then take the fourth
/// nested SEQUENCE — signature algorithm, issuer, validity, **subject**.
pub fn certificate_subject(pem: &[u8]) -> Result<CertificateSubject, &'static str> {
    let unresolved = reason::CREDENTIAL_REF_UNRESOLVED;
    let der = first_certificate_der(pem).ok_or(unresolved)?;
    let certificate = read_element(&der, 0).ok_or(unresolved)?;
    if certificate.tag != 0x30 {
        return Err(unresolved);
    }
    let tbs = read_element(&der, certificate.content.0).ok_or(unresolved)?;
    if tbs.tag != 0x30 {
        return Err(unresolved);
    }

    let mut cursor = tbs.content.0;
    let end = tbs.content.1;
    // The version is `[0] EXPLICIT` and optional; a v1 certificate simply has
    // no such element and the serial number comes first.
    let first = read_element(&der, cursor).ok_or(unresolved)?;
    if first.tag == 0xa0 {
        cursor = first.next;
    }
    // Serial number.
    let serial = read_element(&der, cursor).ok_or(unresolved)?;
    if serial.tag != 0x02 {
        return Err(unresolved);
    }
    cursor = serial.next;

    // signature algorithm, issuer, validity, subject — in that order. The
    // subject is the fourth, and taking the first common name instead would
    // read the issuer.
    let mut subject = None;
    for position in 0..4 {
        if cursor >= end {
            return Err(unresolved);
        }
        let element = read_element(&der, cursor).ok_or(unresolved)?;
        if element.tag != 0x30 {
            return Err(unresolved);
        }
        if position == 3 {
            subject = Some(element.content);
        }
        cursor = element.next;
    }
    let (start, finish) = subject.ok_or(unresolved)?;

    let mut common_name = None;
    let mut given_name = None;
    let mut rdn_cursor = start;
    while rdn_cursor < finish {
        let rdn = read_element(&der, rdn_cursor).ok_or(unresolved)?;
        rdn_cursor = rdn.next;
        if rdn.tag != 0x31 {
            continue;
        }
        let mut attribute_cursor = rdn.content.0;
        while attribute_cursor < rdn.content.1 {
            let attribute = read_element(&der, attribute_cursor).ok_or(unresolved)?;
            attribute_cursor = attribute.next;
            if attribute.tag != 0x30 {
                continue;
            }
            let oid = read_element(&der, attribute.content.0).ok_or(unresolved)?;
            if oid.tag != 0x06 {
                continue;
            }
            let value = read_element(&der, oid.next).ok_or(unresolved)?;
            // UTF8String, PrintableString, IA5String — the encodings a subject
            // attribute is written in. Anything else is left unread rather than
            // guessed at.
            if !matches!(value.tag, 0x0c | 0x13 | 0x16) {
                continue;
            }
            let text = std::str::from_utf8(&der[value.content.0..value.content.1])
                .map_err(|_| unresolved)?
                .to_owned();
            match &der[oid.content.0..oid.content.1] {
                OID_COMMON_NAME if common_name.is_none() => common_name = Some(text),
                OID_GIVEN_NAME if given_name.is_none() => given_name = Some(text),
                _ => {}
            }
        }
    }

    let common_name = common_name
        .filter(|value| !value.is_empty())
        .ok_or(unresolved)?;
    Ok(CertificateSubject {
        common_name,
        // An absent given name is the ordinary case: the identity is the common
        // name, and a missing optional attribute must never lock out a valid
        // principal.
        given_name: given_name.filter(|value| !value.is_empty()),
    })
}

/// The local Git identity of one workspace, as the operator configured it.
pub fn git_identity(workspace: &std::path::Path) -> (Option<String>, Option<String>) {
    let read = |key: &str| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["config", "--get", key])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    };
    (read("user.email"), read("user.name"))
}

/// The certificate is authoritative. A local Git email that disagrees with the
/// authenticated common name is refused rather than reconciled in either
/// direction: rewriting the identity from Git would let a local config claim a
/// principal, and rewriting Git from the certificate would edit the operator's
/// machine to make a mismatch disappear.
pub fn match_local_identity(
    subject: &CertificateSubject,
    local_email: Option<&str>,
) -> Result<(), &'static str> {
    match local_email {
        // No configured email is not a mismatch: the certificate still says who
        // this is, and a workspace with no Git identity is an ordinary state.
        None => Ok(()),
        Some(email) if email.eq_ignore_ascii_case(&subject.common_name) => Ok(()),
        Some(_) => Err(reason::IDENTITY_MISMATCH),
    }
}

// ---------------------------------------------------------------------------
// Whom a session admits
// ---------------------------------------------------------------------------
//
// The peer roster is an authorization boundary, so it defaults to admitting
// nobody and every unreadable shape is refused whole. A partially parsed roster
// is never half-admitted: the entry that did not parse might have been the one
// constraining the entries that did.

/// Where per-project rosters live, resolved through three rungs.
///
/// The connector never resolves the home directory itself — every entry point
/// takes an explicit global root — but `provision_session` keeps its
/// `(&EnrolledRow)` shape and so holds no root. Rung 2 exists for that gap: an
/// install whose global root is not the default would otherwise read an empty
/// directory forever and report `roster-absent` with nothing wrong.
pub fn roster_root(
    explicit: Option<&str>,
    loam_home: Option<&str>,
    home: Option<&str>,
) -> Result<std::path::PathBuf, &'static str> {
    let present = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    };
    if let Some(path) = present(explicit) {
        return Ok(path);
    }
    if let Some(path) = present(loam_home) {
        return Ok(path.join("federation").join("rosters"));
    }
    if let Some(path) = present(home) {
        return Ok(path
            .join(".agents")
            .join("loam")
            .join("federation")
            .join("rosters"));
    }
    Err(reason::ROSTER_ABSENT)
}

/// The roster root this process should use.
pub fn configured_roster_root() -> Result<std::path::PathBuf, &'static str> {
    roster_root(
        std::env::var("LOAM_FEDERATION_ROSTER_DIR").ok().as_deref(),
        std::env::var("LOAM_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// An entry that would admit everyone. `*` and `#` never occur in a principal
/// id or an instance id; a bare `+` is the MQTT single-level wildcard, but a `+`
/// *within* an entry is ordinary — `sam+loam@example.test` is one address, not
/// a pattern.
fn is_wildcard(entry: &str) -> bool {
    let trimmed = entry.trim();
    // Bare only, for the same reason `+` is: an entry that merely *contains*
    // one of these bytes is an id this reader does not recognize, not a pattern,
    // and refusing the whole roster for it is the mirror of the false positive
    // that would have locked out `sam+loam@example.test`.
    trimmed == "+" || trimmed == "*" || trimmed == "#"
}

/// One path atom: a name that can only ever resolve *inside* the root it is
/// joined to. An org or project id is caller-adjacent data that reaches this
/// reader from an enrollment row, and enrollment bounds its length and refuses
/// control characters — it does not refuse separators or `..`. Joining one
/// unchecked would let `../victim-org/their-project` read another tenant's
/// roster, and an absolute id would discard the root entirely, since `join`
/// silently replaces rather than appends. This is the authorization boundary,
/// so the scope is validated before it becomes a path.
fn is_path_atom(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

/// Read one list of bare ids. `None` means the file does not describe a roster
/// this reader understands, which is refused whole rather than partly read.
fn bare_ids(value: Option<&crate::json::Value>) -> Option<Vec<String>> {
    let entries = value?.as_array()?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        // Ids are passed through untransformed in both directions: nothing is
        // prefixed here and no `urn:loam:instance:` is stripped, because the
        // writer does not add one and a reader that stripped would disagree
        // with a writer that did not.
        let text = entry.as_str()?.trim();
        if text.is_empty() {
            return None;
        }
        out.push(text.to_owned());
    }
    Some(out)
}

/// Read the peer roster for one project.
///
/// Absent, empty, one-sided, wildcard, and malformed are five distinct answers
/// and one outcome: no session opens. The distinction is for the operator; the
/// default admits nobody either way.
pub fn read_roster(
    root: &std::path::Path,
    org_id: &str,
    project_id: &str,
) -> Result<crate::connector::PeerRoster, &'static str> {
    if !is_path_atom(org_id) || !is_path_atom(project_id) {
        return Err(reason::ROSTER_MALFORMED);
    }
    let path = root.join(org_id).join(format!("{project_id}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Err(reason::ROSTER_ABSENT);
    };
    let Ok(document) = crate::json::parse(&text) else {
        return Err(reason::ROSTER_MALFORMED);
    };
    // A repeated key is refused rather than first-wins. The shared parser keeps
    // every occurrence and reads the first, so a second `principals` narrowing
    // the first would be silently ignored — a half-admit wearing a whole
    // roster's shape.
    let crate::json::Value::Object(fields) = &document else {
        return Err(reason::ROSTER_MALFORMED);
    };
    let mut names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    let written = names.len();
    names.sort_unstable();
    names.dedup();
    if names.len() != written {
        return Err(reason::ROSTER_MALFORMED);
    }
    let (Some(principals), Some(origins)) = (
        bare_ids(document.get("principals")),
        bare_ids(document.get("origins")),
    ) else {
        return Err(reason::ROSTER_MALFORMED);
    };
    if principals
        .iter()
        .chain(origins.iter())
        .any(|e| is_wildcard(e))
    {
        return Err(reason::ROSTER_WILDCARD);
    }
    match (principals.is_empty(), origins.is_empty()) {
        (true, true) => Err(reason::ROSTER_EMPTY),
        // One-sided rosters open a session that looks connected and hears
        // nobody. The receive path admits an origin *and* a principal, so a
        // roster missing either half admits no colleague at all — and a live
        // session that silently hears nothing is worse than an honest refusal.
        (false, true) => Err(reason::ROSTER_NO_ORIGINS),
        (true, false) => Err(reason::ROSTER_NO_PRINCIPALS),
        (false, false) => Ok(crate::connector::PeerRoster {
            principals,
            origins,
        }),
    }
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/// Bounds the connector's own transport, matching the values the enrollment
/// probe already uses. Not a knob: a session that could be given a different
/// packet ceiling than the probe verified would pass enrollment and then fail
/// on the first large frame.
const REQUEST_CAPACITY: usize = 8;
const MAX_PACKET_BYTES: u32 = 400_000;

/// Resolve one enrolled row into a live session's inputs and the roster its
/// received frames are checked against.
///
/// Credentials are resolved before the roster on purpose: an operator whose
/// secret store is empty should be told that, not told their roster is missing.
pub fn resolve(
    row: &crate::enrollment::EnrolledRow,
) -> Result<(crate::connector::MqttSession, crate::connector::PeerRoster), ProvisionFailure> {
    let credentials = ProvisionFailure::Credentials;
    let (host, port) = split_endpoint(&row.broker_endpoint).map_err(credentials)?;
    let backend = configured_backend()?;
    let material = resolve_credentials(
        &backend,
        &row.credential_ref,
        row.ca_ref.as_deref(),
        // The conventional trust-bundle override, and the first rung the search
        // documents. Reading it here is what makes that rung real: passing
        // `None` left the documented override unconsulted, which the
        // two-instance tier caught by failing to trust its own fixture CA.
        std::env::var("SSL_CERT_FILE").ok().as_deref(),
    )?;

    // Who this session is, read from the certificate the broker will
    // authenticate and from nothing the caller can influence.
    let subject = certificate_subject(&material.certificate).map_err(credentials)?;
    let (local_email, _local_name) = git_identity(std::path::Path::new(&row.display_path));
    match_local_identity(&subject, local_email.as_deref()).map_err(credentials)?;

    let roster_root = configured_roster_root().map_err(ProvisionFailure::Roster)?;
    let roster = read_roster(&roster_root, &row.org_id, &row.project_id)
        .map_err(ProvisionFailure::Roster)?;

    // The client id is the **bare** instance id, with nothing prefixed or
    // derived. The broker's ACL scopes origin-write on the client id rather than
    // the user, because two machines belonging to one person share a
    // certificate and therefore a principal. A wrong client id is not an
    // authentication failure — the connection is accepted and every publish is
    // silently denied.
    let config = crate::transport::TransportConfig::new(
        &host,
        port,
        &row.instance_id,
        REQUEST_CAPACITY,
        MAX_PACKET_BYTES,
        crate::envelope::ValidationConfig::default(),
    )
    .map_err(|_| credentials(reason::ENDPOINT_MALFORMED))?;

    let session = crate::connector::MqttSession {
        config,
        // mTLS is the sole authentication: the effective username is the
        // certificate common name the broker assigns from what we present.
        username: None,
        password: None,
        ca_certificate: material.certificate_authority,
        client_authentication: Some((material.certificate, material.key)),
        claimed_identity: crate::connector::SessionIdentity {
            principal_id: subject.common_name,
            // One enrolled workspace is one agent. Nothing provisions a separate
            // agent identity, and minting one here would be a second source of
            // identity beside the enrolled instance id — the defect this slice
            // exists to close.
            agent_id: row.instance_id.clone(),
            // The single source. The connector reads this value and never mints,
            // derives, or defaults one, so an emit's `source` and its topic
            // origin cannot disagree.
            instance_id: row.instance_id.clone(),
            display_name: subject.given_name,
            allowed_claims: roster.principals.clone(),
        },
    };
    Ok((session, roster))
}

/// The backend this process should use, from the environment and the platform.
pub fn configured_backend() -> Result<Backend, ProvisionFailure> {
    select_backend(
        std::env::var("LOAM_SECRET_BACKEND").ok().as_deref(),
        std::env::consts::OS,
    )
    .map_err(ProvisionFailure::Credentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----\n";
    const INTERMEDIATE: &str = "-----BEGIN CERTIFICATE-----\nREVG\n-----END CERTIFICATE-----\n";
    const KEY: &str = "-----BEGIN PRIVATE KEY-----\nR0hJ\n-----END PRIVATE KEY-----\n";

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after the epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "loam-provisioning-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp directory is creatable");
        path
    }

    /// A backend that writes `body` to stdout, or exits non-zero when `body` is
    /// `None`. Stands in for libsecret without needing a keyring daemon.
    #[cfg(unix)]
    fn fake_backend(label: &str, body: Option<&str>) -> (std::path::PathBuf, Backend) {
        fake_backend_with_status(label, body, 1)
    }

    /// A backend that writes `body` to stdout and then exits with `status`. The
    /// two are separable on purpose: a program that fails *and* prints is the
    /// case where trusting stdout alone would hand a usage message to the trust
    /// store as though it were credential material.
    #[cfg(unix)]
    fn fake_backend_with_status(
        label: &str,
        body: Option<&str>,
        status: i32,
    ) -> (std::path::PathBuf, Backend) {
        use std::os::unix::fs::PermissionsExt;
        let directory = temp_dir(label);
        let script = directory.join("backend.sh");
        let contents = match body {
            Some(body) => format!("#!/bin/sh\nprintf '%s' '{body}'\n"),
            None => format!("#!/bin/sh\nexit {status}\n"),
        };
        let mut file = std::fs::File::create(&script).expect("script is writable");
        file.write_all(contents.as_bytes()).expect("script writes");
        drop(file);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("script is executable");
        wait_until_executable(&script);
        let backend = Backend::Custom(script.to_string_lossy().into_owned());
        (directory, backend)
    }

    /// Wait out `ETXTBSY`.
    ///
    /// The test runner is multithreaded, so another thread can `fork` while
    /// this one still holds a write descriptor on a freshly created script; the
    /// forked child carries that descriptor until it `exec`s, and Linux refuses
    /// to execute a file any process has open for writing. That is an artifact
    /// of creating and running a program in the same process, not a property of
    /// the resolver — production runs `secret-tool`, which nobody is writing.
    #[cfg(unix)]
    fn wait_until_executable(script: &std::path::Path) {
        for _ in 0..1000 {
            match std::process::Command::new(script)
                .arg("warmup")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Err(error) if error.raw_os_error() == Some(26) => std::thread::yield_now(),
                _ => return,
            }
        }
        panic!("the fake backend never became executable");
    }

    impl Backend {
        fn program_path(&self) -> &str {
            match self {
                Backend::SecretTool(program)
                | Backend::Security(program)
                | Backend::Custom(program) => program,
            }
        }
    }

    // --- synthetic certificates -------------------------------------------
    //
    // Built by hand rather than by `openssl` so the unit tier needs no external
    // program, and so the issuer and the subject can be made to disagree on
    // purpose — which is the whole point of the position-correctness control.

    fn der(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if body.len() < 0x80 {
            out.push(body.len() as u8);
        } else if body.len() < 0x100 {
            out.push(0x81);
            out.push(body.len() as u8);
        } else {
            out.push(0x82);
            out.push((body.len() >> 8) as u8);
            out.push((body.len() & 0xff) as u8);
        }
        out.extend_from_slice(body);
        out
    }

    fn attribute(oid: &[u8], value: &str) -> Vec<u8> {
        let mut body = der(0x06, oid);
        body.extend(der(0x0c, value.as_bytes()));
        der(0x31, &der(0x30, &body))
    }

    /// A `Name` carrying an optional given name and a common name.
    fn name(common: Option<&str>, given: Option<&str>) -> Vec<u8> {
        let mut body = Vec::new();
        if let Some(given) = given {
            body.extend(attribute(OID_GIVEN_NAME, given));
        }
        if let Some(common) = common {
            body.extend(attribute(OID_COMMON_NAME, common));
        }
        der(0x30, &body)
    }

    fn certificate_der(
        issuer_common: &str,
        subject_common: Option<&str>,
        subject_given: Option<&str>,
        versioned: bool,
    ) -> Vec<u8> {
        let mut tbs = Vec::new();
        if versioned {
            tbs.extend(der(0xa0, &der(0x02, &[0x02])));
        }
        tbs.extend(der(0x02, &[0x2a]));
        tbs.extend(der(0x30, &der(0x06, &[0x2a, 0x86, 0x48])));
        tbs.extend(name(Some(issuer_common), None));
        tbs.extend(der(0x30, &[]));
        tbs.extend(name(subject_common, subject_given));

        let mut body = der(0x30, &tbs);
        body.extend(der(0x30, &der(0x06, &[0x2a, 0x86, 0x48])));
        body.extend(der(0x03, &[0x00, 0x01]));
        der(0x30, &body)
    }

    fn base64_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let triple = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
            out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(triple >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[triple as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn pem(bytes: &[u8]) -> Vec<u8> {
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64_encode(bytes)
        )
        .into_bytes()
    }

    #[test]
    fn the_subject_is_read_positionally_and_never_by_scanning_for_the_first_common_name() {
        // The control this whole walk exists for. `tbsCertificate` carries the
        // issuer Name before the subject Name, so a scanner that returns the
        // first common-name OID returns the CA — and against this repository's
        // own test CA (`CN=Loam MQTT Test CA`) it would pass any "does it find a
        // common name" assertion while mis-attributing every message.
        let certificate = pem(&certificate_der(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            Some("Ada Lovelace"),
            true,
        ));
        let subject = certificate_subject(&certificate).expect("a well-formed certificate reads");
        assert_eq!(subject.common_name, "sam@example.test");
        assert_eq!(subject.given_name.as_deref(), Some("Ada Lovelace"));
        assert_ne!(subject.common_name, "Loam MQTT Test CA");
    }

    #[test]
    fn a_certificate_without_a_given_name_still_authenticates() {
        // The identity is the common name; the given name is shown. A missing
        // optional attribute must never lock out a valid principal, so this
        // degrades to an absent display name rather than refusing.
        let certificate = pem(&certificate_der(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
        ));
        let subject = certificate_subject(&certificate).expect("no given name is not a failure");
        assert_eq!(subject.common_name, "sam@example.test");
        assert!(subject.given_name.is_none());

        // A v1 certificate has no explicit version element, so the serial comes
        // first and the positional walk must still land on the subject.
        let v1 = pem(&certificate_der(
            "Loam MQTT Test CA",
            Some("v1@example.test"),
            None,
            false,
        ));
        assert_eq!(
            certificate_subject(&v1)
                .expect("a v1 certificate reads")
                .common_name,
            "v1@example.test"
        );
    }

    #[test]
    fn a_certificate_this_walk_cannot_read_is_refused_and_never_panics() {
        let unresolved = Err(reason::CREDENTIAL_REF_UNRESOLVED);
        // No subject common name at all: there is no principal to authenticate.
        let anonymous = pem(&certificate_der(
            "Loam MQTT Test CA",
            None,
            Some("Ada"),
            true,
        ));
        assert_eq!(
            certificate_subject(&anonymous).map(|s| s.common_name),
            unresolved
        );
        // Not a certificate.
        assert_eq!(
            certificate_subject(b"-----BEGIN PRIVATE KEY-----\nR0hJ\n-----END PRIVATE KEY-----\n")
                .map(|s| s.common_name),
            unresolved
        );
        assert_eq!(
            certificate_subject(b"not pem at all").map(|s| s.common_name),
            unresolved
        );

        // Every truncation of a valid certificate: each must refuse, and none
        // may panic. A bounded walk is the difference between a refusal and a
        // crashed connector on a malformed certificate.
        let whole = certificate_der("Loam MQTT Test CA", Some("sam@example.test"), None, true);
        for cut in 1..whole.len() {
            let truncated = pem(&whole[..cut]);
            let _ = certificate_subject(&truncated);
        }
        // A length header claiming *more* than the buffer holds. Asserted
        // unconditionally: an earlier form of this hedged behind `|| the cert is
        // long`, which is always true, so it proved nothing. The outer length is
        // long-form here, so raising its high byte claims roughly 64 KiB more
        // than exists.
        let mut lying = certificate_der("Loam MQTT Test CA", Some("sam@example.test"), None, true);
        if lying[1] < 0x80 {
            let claimed = 0x7f_usize;
            assert!(
                claimed > lying.len() - 2,
                "the doctored length must claim more than the buffer holds"
            );
            lying[1] = 0x7f;
        } else {
            lying[2] = lying[2].wrapping_add(1);
        }
        assert!(
            certificate_subject(&pem(&lying)).is_err(),
            "a length claiming more than the buffer holds must be refused"
        );
    }

    #[test]
    fn the_certificate_is_authoritative_and_a_local_email_never_overrides_it() {
        let subject = CertificateSubject {
            common_name: "sam@example.test".to_owned(),
            given_name: None,
        };
        assert_eq!(
            match_local_identity(&subject, Some("sam@example.test")),
            Ok(())
        );
        // Case is not identity: addresses differing only in case are the same
        // operator, and refusing them would be a false mismatch.
        assert_eq!(
            match_local_identity(&subject, Some("Sam@Example.Test")),
            Ok(())
        );
        // No configured Git identity is an ordinary state, not a mismatch.
        assert_eq!(match_local_identity(&subject, None), Ok(()));
        // A disagreement is surfaced, never resolved in either direction.
        assert_eq!(
            match_local_identity(&subject, Some("someone-else@example.test")),
            Err(reason::IDENTITY_MISMATCH)
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_local_git_identity_is_read_from_the_workspace_and_absence_is_not_an_error() {
        let directory = temp_dir("git-identity");
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&directory)
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            // No Git on this host: the absence half below still means something,
            // but the positive control cannot run, so skip rather than assert a
            // half-test that would pass for the wrong reason.
            let _ = std::fs::remove_dir_all(&directory);
            return;
        }

        // What the operator configured is what is read, and it is read for
        // *this* workspace: the local value differs from whatever global
        // identity this machine carries, and the local one wins.
        assert!(git(&["config", "user.email", "sam@example.test"]));
        assert!(git(&["config", "user.name", "Ada Lovelace"]));
        assert_eq!(
            git_identity(&directory),
            (
                Some("sam@example.test".to_owned()),
                Some("Ada Lovelace".to_owned())
            )
        );

        // Absence: a path that is not a workspace at all answers `None` rather
        // than failing. Note what is deliberately *not* asserted — a repository
        // with no local identity still resolves the machine's global config,
        // which is correct: that is where an operator's email normally lives.
        assert_eq!(
            git_identity(&directory.join("not-a-workspace")),
            (None, None)
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    // --- the peer roster ---------------------------------------------------

    fn write_roster(root: &std::path::Path, org: &str, project: &str, body: &str) {
        let directory = root.join(org);
        std::fs::create_dir_all(&directory).expect("roster directory is creatable");
        std::fs::write(directory.join(format!("{project}.json")), body)
            .expect("roster file is writable");
    }

    // --- the seam ----------------------------------------------------------

    fn enrolled_row(instance: &str, endpoint: &str) -> crate::enrollment::EnrolledRow {
        crate::enrollment::EnrolledRow {
            identity_key: "unix:1:1".into(),
            org_id: "acme".into(),
            project_id: "loam".into(),
            repository_id: "repo".into(),
            descriptor_digest: "d".into(),
            display_path: "/w".into(),
            instance_id: instance.into(),
            broker_profile: "p".into(),
            broker_endpoint: endpoint.into(),
            tls_server_name: "broker.example".into(),
            credential_ref: "acme/loam/mqtt".into(),
            ca_ref: None,
            commit: "84be000000000000000000000000000000000001".into(),
            capabilities: crate::enrollment::CapabilityRecord {
                authentication: true,
                publish: true,
                subscribe: true,
                self_receive: true,
                verified_at: "2026-07-24T14:20:00Z".into(),
            },
            remotes: Vec::new(),
        }
    }

    #[test]
    fn a_malformed_endpoint_refuses_before_any_secret_is_asked_for() {
        // Ordering is load-bearing, not incidental: the endpoint is checked
        // before the secret store is consulted, so a typo in an enrollment can
        // never spend a keyring lookup — or, on a desktop, provoke an unlock
        // prompt — to learn something already visible in the row.
        let row = enrolled_row("instance-01", "not-an-endpoint");
        assert_eq!(
            resolve(&row).err(),
            Some(ProvisionFailure::Credentials(reason::ENDPOINT_MALFORMED))
        );
    }

    #[test]
    fn the_session_identity_and_client_id_come_from_the_enrolled_row() {
        // The enrolled instance id is the single source. `federation emit`
        // derives `source` from the same field, so one reader of one column is
        // what makes the envelope's source and its topic origin agree — the
        // divergence that surfaces to a user as an unexplained refusal.
        let row = enrolled_row("instance-77", "mqtts://broker.acme.example:8883");
        let identity = crate::connector::SessionIdentity {
            principal_id: "sam@example.test".into(),
            agent_id: row.instance_id.clone(),
            instance_id: row.instance_id.clone(),
            display_name: None,
            allowed_claims: Vec::new(),
        };
        assert_eq!(identity.instance_id, row.instance_id);

        // The client id is the **bare** instance id. The broker's ACL scopes
        // origin-write on the client id rather than the user, because two
        // machines belonging to one person share a certificate and therefore a
        // principal. A wrong client id is not an authentication failure: the
        // connection is accepted and every publish is silently denied, which is
        // the hardest failure to diagnose from this side.
        let config = crate::transport::TransportConfig::new(
            "broker.acme.example",
            8883,
            &row.instance_id,
            REQUEST_CAPACITY,
            MAX_PACKET_BYTES,
            crate::envelope::ValidationConfig::default(),
        )
        .expect("the transport configuration is valid");
        // Asserted through the options that actually reach the wire, not
        // through the field we happened to store.
        assert_eq!(config.mqtt_options().client_id(), row.instance_id);
        assert!(!config.mqtt_options().client_id().starts_with("loam-"));
    }

    #[test]
    fn the_roster_root_walks_three_rungs_in_order() {
        // The explicit override wins.
        assert_eq!(
            roster_root(Some("/explicit"), Some("/loam-home"), Some("/home/op")).unwrap(),
            std::path::PathBuf::from("/explicit")
        );
        // Then the global root, which is why this rung exists: the connector's
        // root arrives as an argument and `provision_session` never sees it, so
        // a non-default install would otherwise read an empty directory forever.
        assert_eq!(
            roster_root(None, Some("/loam-home"), Some("/home/op")).unwrap(),
            std::path::PathBuf::from("/loam-home/federation/rosters")
        );
        // Then the default install location.
        assert_eq!(
            roster_root(None, None, Some("/home/op")).unwrap(),
            std::path::PathBuf::from("/home/op/.agents/loam/federation/rosters")
        );
        // Blank is not a value at any rung.
        assert_eq!(
            roster_root(Some("  "), Some(""), Some("/home/op")).unwrap(),
            std::path::PathBuf::from("/home/op/.agents/loam/federation/rosters")
        );
        // Nothing at all is an absent roster, not a path built from nothing.
        assert_eq!(roster_root(None, None, None), Err(reason::ROSTER_ABSENT));
    }

    #[test]
    fn a_well_formed_roster_is_admitted_exactly_as_written() {
        // The positive control every refusal below is measured against.
        let root = temp_dir("roster-ok");
        write_roster(
            &root,
            "acme",
            "loam",
            r#"{"principals":["ada@example.test"],"origins":["instance-02"]}"#,
        );
        let roster = read_roster(&root, "acme", "loam").expect("a well-formed roster is admitted");
        assert_eq!(roster.principals, vec!["ada@example.test".to_owned()]);
        assert_eq!(roster.origins, vec!["instance-02".to_owned()]);

        // Ids are passed through untransformed in both directions: no prefix is
        // added here and none is stripped, because the writer does neither.
        write_roster(
            &root,
            "acme",
            "prefixed",
            r#"{"principals":["urn:loam:principal:ada"],"origins":["urn:loam:instance:instance-02"]}"#,
        );
        let verbatim = read_roster(&root, "acme", "prefixed").expect("verbatim ids are admitted");
        assert_eq!(
            verbatim.origins,
            vec!["urn:loam:instance:instance-02".to_owned()]
        );

        // A roster is per (org, project): the same project name under another
        // org is a different file and does not leak across.
        assert_eq!(
            read_roster(&root, "other-org", "loam"),
            Err(reason::ROSTER_ABSENT)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_unusable_roster_refuses_with_its_own_reason_and_admits_nothing() {
        let root = temp_dir("roster-refuse");
        let cases: [(&str, &str, &str); 9] = [
            (
                "empty",
                r#"{"principals":[],"origins":[]}"#,
                reason::ROSTER_EMPTY,
            ),
            (
                "no-origins",
                r#"{"principals":["ada@example.test"],"origins":[]}"#,
                reason::ROSTER_NO_ORIGINS,
            ),
            (
                "no-principals",
                r#"{"principals":[],"origins":["instance-02"]}"#,
                reason::ROSTER_NO_PRINCIPALS,
            ),
            (
                "wildcard-star",
                r#"{"principals":["*"],"origins":["instance-02"]}"#,
                reason::ROSTER_WILDCARD,
            ),
            (
                "wildcard-hash",
                r##"{"principals":["ada@example.test"],"origins":["#"]}"##,
                reason::ROSTER_WILDCARD,
            ),
            (
                "wildcard-plus",
                r#"{"principals":["ada@example.test"],"origins":["+"]}"#,
                reason::ROSTER_WILDCARD,
            ),
            ("malformed-json", "{not json", reason::ROSTER_MALFORMED),
            (
                "malformed-entry",
                r#"{"principals":["ada@example.test",7],"origins":["instance-02"]}"#,
                reason::ROSTER_MALFORMED,
            ),
            (
                "malformed-missing-key",
                r#"{"principals":["ada@example.test"]}"#,
                reason::ROSTER_MALFORMED,
            ),
        ];
        for (project, body, expected) in cases {
            write_roster(&root, "acme", project, body);
            assert_eq!(
                read_roster(&root, "acme", project).map(|r| r.principals),
                Err(expected),
                "{project} must refuse with {expected}"
            );
        }

        // Absent is its own answer, distinct from every malformed one.
        assert_eq!(
            read_roster(&root, "acme", "never-written"),
            Err(reason::ROSTER_ABSENT)
        );

        // Discard, never half-admit: the well-formed entry in a file whose
        // other entry is unreadable is not admitted either. The entry that did
        // not parse might have been the one constraining the entry that did.
        write_roster(
            &root,
            "acme",
            "partial",
            r#"{"principals":["ada@example.test",{"id":"mallory"}],"origins":["instance-02"]}"#,
        );
        assert_eq!(
            read_roster(&root, "acme", "partial").map(|r| r.principals),
            Err(reason::ROSTER_MALFORMED)
        );

        // Cross-tenant traversal: an org or project id is enrollment data, and
        // enrollment bounds its length and refuses control characters but not
        // separators. Joined unchecked, each of these would read a roster
        // outside this project's scope — the first another tenant's, the second
        // and third somewhere else on the filesystem entirely.
        let elsewhere = temp_dir("roster-elsewhere");
        std::fs::create_dir_all(elsewhere.join("victim")).expect("victim scope is creatable");
        std::fs::write(
            elsewhere.join("victim").join("their-project.json"),
            r#"{"principals":["their-principal"],"origins":["their-instance"]}"#,
        )
        .expect("victim roster is writable");
        for (org, project) in [
            ("acme", "../victim/their-project"),
            ("..", "victim/their-project"),
            (elsewhere.to_string_lossy().as_ref(), "victim/their-project"),
            ("acme/..", "ordinary"),
            ("", "ordinary"),
            (".", "ordinary"),
        ] {
            let escaped = read_roster(&root, org, project);
            assert_eq!(
                escaped,
                Err(reason::ROSTER_MALFORMED),
                "{org}/{project} must not resolve outside this project's scope"
            );
        }
        // Positive control in the same run: an ordinary scope still resolves,
        // so the refusals above are the traversal and not a broken reader.
        write_roster(
            &root,
            "acme",
            "ordinary",
            r#"{"principals":["ada@example.test"],"origins":["instance-02"]}"#,
        );
        assert!(read_roster(&root, "acme", "ordinary").is_ok());
        let _ = std::fs::remove_dir_all(elsewhere);

        // A covering key is refused rather than first-wins: the shared parser
        // keeps both and reads the first, so a second `principals` that narrows
        // the first would be silently ignored.
        write_roster(
            &root,
            "acme",
            "covering",
            r#"{"principals":["broad@example.test"],"origins":["instance-02"],"principals":["narrow@example.test"]}"#,
        );
        assert_eq!(
            read_roster(&root, "acme", "covering").map(|r| r.principals),
            Err(reason::ROSTER_MALFORMED)
        );

        // An address containing a plus is an address, not a pattern: refusing
        // `sam+loam@example.test` would lock out a legitimate colleague.
        write_roster(
            &root,
            "acme",
            "plus-address",
            r#"{"principals":["sam+loam@example.test"],"origins":["instance-02"]}"#,
        );
        assert_eq!(
            read_roster(&root, "acme", "plus-address")
                .expect("a plus address is admitted")
                .principals,
            vec!["sam+loam@example.test".to_owned()]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_backend_follows_the_platform_and_the_override_wins() {
        assert_eq!(
            select_backend(None, "linux"),
            Ok(Backend::SecretTool("secret-tool".into()))
        );
        assert_eq!(
            select_backend(None, "macos"),
            Ok(Backend::Security("security".into()))
        );
        // An unknown platform has no store this code can name, so it refuses
        // rather than guessing at one.
        assert_eq!(
            select_backend(None, "plan9"),
            Err(reason::CREDENTIAL_REF_UNRESOLVED)
        );
        // The override names either a known store or a program to run.
        assert_eq!(
            select_backend(Some("security"), "linux"),
            Ok(Backend::Security("security".into()))
        );
        assert_eq!(
            select_backend(Some("/opt/vault-shim"), "linux"),
            Ok(Backend::Custom("/opt/vault-shim".into()))
        );
        // An empty override is not an override: it must not silently become a
        // custom backend named "".
        assert_eq!(
            select_backend(Some("   "), "linux"),
            Ok(Backend::SecretTool("secret-tool".into()))
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_secret_arrives_on_stdout_and_a_failing_backend_resolves_nothing() {
        let (directory, backend) = fake_backend("stdout", Some("s3cret-material"));
        assert_eq!(
            lookup(&backend, "acme/loam/mqtt").as_deref(),
            Some(b"s3cret-material".as_slice())
        );
        // Control: the same call shape against a backend that exits non-zero
        // resolves nothing, so the positive result above is the backend
        // answering rather than this code inventing an answer.
        let (failing_dir, failing) = fake_backend("stdout-fail", None);
        assert!(lookup(&failing, "acme/loam/mqtt").is_none());
        // And a backend that does not exist at all is the same answer.
        assert!(lookup(
            &Backend::Custom("/nonexistent/loam-secret-backend".into()),
            "acme/loam/mqtt"
        )
        .is_none());

        // The sharp one: a backend that *prints* and then fails. Trusting
        // stdout alone would take a usage message for credential material and
        // hand it to the trust store, so the exit status is checked too.
        let (noisy_dir, noisy) = fake_backend_with_status("stdout-noisy", None, 44);
        std::fs::write(
            noisy.program_path(),
            "#!/bin/sh\nprintf '%s' 'usage: security find-generic-password'\nexit 44\n",
        )
        .expect("script is rewritable");
        wait_until_executable(std::path::Path::new(noisy.program_path()));
        assert!(
            lookup(&noisy, "acme/loam/mqtt").is_none(),
            "a backend that prints and then fails must resolve nothing"
        );

        let _ = std::fs::remove_dir_all(directory);
        let _ = std::fs::remove_dir_all(failing_dir);
        let _ = std::fs::remove_dir_all(noisy_dir);
    }

    #[test]
    fn the_reference_is_passed_verbatim_as_one_argument_and_never_parsed() {
        // References are opaque keys. A resolver that "helpfully" stripped a
        // scheme would disagree with whatever wrote the secret, and the failure
        // would look like an absent secret rather than a mismatch.
        for reference in [
            "vault://acme/loam/mqtt",
            "acme/loam/mqtt",
            "loam-federation:acme:mqtt",
            "one with spaces",
        ] {
            let (program, argv) = invocation(&Backend::SecretTool("secret-tool".into()), reference);
            assert_eq!(program, "secret-tool");
            assert_eq!(argv, ["lookup", "service", SERVICE, "ref", reference]);

            let (program, argv) = invocation(&Backend::Security("security".into()), reference);
            assert_eq!(program, "security");
            assert_eq!(
                argv,
                [
                    "find-generic-password",
                    "-s",
                    SERVICE,
                    "-a",
                    reference,
                    "-w"
                ]
            );

            // Whole, not split: exactly one argument equals the reference, and
            // no argument is a fragment of it.
            for (_, argv) in [
                invocation(&Backend::SecretTool("secret-tool".into()), reference),
                invocation(&Backend::Security("security".into()), reference),
                invocation(&Backend::Custom("/opt/shim".into()), reference),
            ] {
                assert_eq!(
                    argv.iter().filter(|value| *value == reference).count(),
                    1,
                    "the reference must appear once, whole: {argv:?}"
                );
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_secret_never_reaches_the_process_table() {
        // The material is written by the backend to its own standard output, so
        // there is no argument list it could have been placed in. This proves
        // it against a backend that records exactly what it was invoked with.
        let directory = temp_dir("argv");
        let record = directory.join("argv.txt");
        let script = directory.join("backend.sh");
        {
            use std::os::unix::fs::PermissionsExt;
            let contents = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf '%s' 'BEGIN-SECRET-MATERIAL'\n",
                record.display()
            );
            let mut file = std::fs::File::create(&script).expect("script is writable");
            file.write_all(contents.as_bytes()).expect("script writes");
            drop(file);
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .expect("script is executable");
        }
        wait_until_executable(&script);
        let backend = Backend::Custom(script.to_string_lossy().into_owned());
        let secret = lookup(&backend, "acme/loam/mqtt").expect("the backend answers");
        assert_eq!(secret, b"BEGIN-SECRET-MATERIAL");

        let observed = std::fs::read_to_string(&record).expect("the argv record is readable");
        assert_eq!(observed.trim(), "acme/loam/mqtt");
        assert!(
            !observed.contains("BEGIN-SECRET-MATERIAL"),
            "the secret must never appear in an argument: {observed}"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn the_endpoint_splits_into_a_host_and_a_port() {
        assert_eq!(
            split_endpoint("mqtts://broker.acme.example:8883"),
            Ok(("broker.acme.example".to_owned(), 8883))
        );
        for malformed in [
            "broker.acme.example:8883",
            "mqtt://broker.acme.example:8883",
            "mqtts://broker.acme.example",
            "mqtts://:8883",
            "mqtts://broker.acme.example:0",
            "mqtts://broker.acme.example:99999",
            "mqtts://user@broker.acme.example:8883",
            "mqtts://broker.acme.example:8883/path",
            "mqtts://broker.acme.example:8883?a=b",
            "mqtts://",
        ] {
            assert_eq!(
                split_endpoint(malformed),
                Err(reason::ENDPOINT_MALFORMED),
                "{malformed} must not parse"
            );
        }
    }

    #[test]
    fn one_blob_splits_into_the_certificate_then_the_key() {
        let blob = format!("{CERTIFICATE}{KEY}");
        let (certificate, key) = split_credential(blob.as_bytes()).expect("a valid blob splits");
        assert_eq!(certificate, CERTIFICATE.as_bytes());
        assert_eq!(key, KEY.as_bytes());

        // A chain is still certificates-then-key.
        let chained = format!("{CERTIFICATE}{INTERMEDIATE}{KEY}");
        let (certificate, key) = split_credential(chained.as_bytes()).expect("a chain splits");
        assert_eq!(
            certificate,
            format!("{CERTIFICATE}{INTERMEDIATE}").as_bytes()
        );
        assert_eq!(key, KEY.as_bytes());
    }

    #[test]
    fn a_blob_this_reader_does_not_understand_resolves_nothing() {
        let unresolved = Err(reason::CREDENTIAL_REF_UNRESOLVED);
        // Order is checked, not assumed: a swapped blob would otherwise fail
        // deep in the handshake with a diagnostic naming neither input.
        assert_eq!(
            split_credential(format!("{KEY}{CERTIFICATE}").as_bytes()),
            unresolved
        );
        // Missing halves.
        assert_eq!(split_credential(CERTIFICATE.as_bytes()), unresolved);
        assert_eq!(split_credential(KEY.as_bytes()), unresolved);
        assert_eq!(split_credential(b""), unresolved);
        // Trailing material after the key: a blob carrying more than the
        // contract describes is refused whole rather than partly read.
        assert_eq!(
            split_credential(format!("{CERTIFICATE}{KEY}{CERTIFICATE}").as_bytes()),
            unresolved
        );
        // Anything outside an accepted block is refused rather than ignored:
        // trailing junk, a stray marker, and leading noise are all shapes a
        // scanner that only looked at markers would silently accept.
        assert_eq!(
            split_credential(format!("{CERTIFICATE}{KEY}trailing junk\n").as_bytes()),
            unresolved
        );
        assert_eq!(
            split_credential(format!("{CERTIFICATE}{KEY}-----BEGIN CERTIFICATE-----\n").as_bytes()),
            unresolved
        );
        assert_eq!(
            split_credential(format!("Bag Attributes\n{CERTIFICATE}{KEY}").as_bytes()),
            unresolved
        );
        // Whitespace between and around blocks is not content.
        assert!(split_credential(format!("\n\n{CERTIFICATE}\n{KEY}\n\n").as_bytes()).is_ok());
        // A block that opens and never closes.
        assert_eq!(
            split_credential(
                format!("{CERTIFICATE}-----BEGIN PRIVATE KEY-----\nR0hJ\n").as_bytes()
            ),
            unresolved
        );
        // A BEGIN nested inside an open block — the shape a hostile blob would
        // use to smuggle a second object past a naive scanner.
        assert_eq!(
            split_credential(
                b"-----BEGIN CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nR0hJ\n-----END PRIVATE KEY-----\n".as_slice()
            ),
            unresolved
        );
        // Mismatched END label.
        assert_eq!(
            split_credential(
                b"-----BEGIN CERTIFICATE-----\nQUJD\n-----END PRIVATE KEY-----\n".as_slice()
            ),
            unresolved
        );
        // Not UTF-8 at all.
        assert_eq!(split_credential(&[0xff, 0xfe, 0x00]), unresolved);
    }

    #[test]
    #[cfg(unix)]
    fn an_absent_ca_ref_means_the_platform_bundle_and_a_present_one_means_the_pinned_ca() {
        let (directory, backend) = fake_backend(
            "ca",
            Some("-----BEGIN CERTIFICATE-----\npinned\n-----END CERTIFICATE-----\n"),
        );

        // Present: the pinned CA comes from the backend, not from the platform.
        let pinned = resolve_trust_anchors(&backend, Some("acme/loam/ca"), None)
            .expect("a resolvable ca_ref pins");
        assert!(String::from_utf8_lossy(&pinned).contains("pinned"));

        // Absent: the platform bundle is read from disk instead. Both branches
        // must produce real bytes — the transport builds its root store from
        // this PEM, and an empty store refuses every connection, so "system
        // roots" cannot be spelled as an empty vector.
        let bundle = directory.join("bundle.pem");
        std::fs::write(
            &bundle,
            "-----BEGIN CERTIFICATE-----\nsystem\n-----END CERTIFICATE-----\n",
        )
        .expect("bundle is writable");
        let system = resolve_trust_anchors(&backend, None, Some(&bundle.to_string_lossy()))
            .expect("an absent ca_ref falls back to the platform bundle");
        assert!(String::from_utf8_lossy(&system).contains("system"));
        assert_ne!(pinned, system, "the two branches must be distinguishable");

        // A named CA that resolves to nothing is a refusal, never a silent
        // downgrade to the platform bundle — that downgrade would turn a
        // pinning failure into a quietly wider trust decision.
        let (failing_dir, failing) = fake_backend("ca-fail", None);
        assert_eq!(
            resolve_trust_anchors(
                &failing,
                Some("acme/loam/ca"),
                Some(&bundle.to_string_lossy())
            ),
            Err(reason::CA_UNRESOLVED)
        );

        // A present-but-blank reference is malformed, not absent: reading it as
        // "no CA pinned" would turn a typo into a silently wider trust decision.
        assert_eq!(
            resolve_trust_anchors(&backend, Some("   "), Some(&bundle.to_string_lossy())),
            Err(reason::CA_UNRESOLVED)
        );

        let _ = std::fs::remove_dir_all(directory);
        let _ = std::fs::remove_dir_all(failing_dir);
    }

    #[test]
    fn finding_no_trust_bundle_refuses_rather_than_trusting_an_empty_store() {
        // Every candidate points at a path that does not exist, so this holds on
        // any host. Baking the real list in would let a machine that happens to
        // carry `/etc/ssl/certs/ca-certificates.crt` — which is every Linux
        // host — satisfy the search and leave this branch unexercised.
        let directory = temp_dir("no-bundle");
        let absent = directory.join("absent.pem");
        let nowhere = [
            "/nonexistent/loam/ca-certificates.crt",
            "/nonexistent/loam/ca-bundle.crt",
        ];
        assert_eq!(
            system_trust_anchors_among(Some(&absent.to_string_lossy()), &nowhere),
            Err(reason::CA_UNRESOLVED)
        );

        // Positive control in the same run: the identical call finds a bundle
        // when one of the candidates exists, so the refusal above is the search
        // failing rather than the function always refusing.
        let present = directory.join("present.pem");
        std::fs::write(
            &present,
            "-----BEGIN CERTIFICATE-----\nfound\n-----END CERTIFICATE-----\n",
        )
        .expect("bundle is writable");
        let found = system_trust_anchors_among(None, &[&present.to_string_lossy(), nowhere[0]])
            .expect("an existing candidate resolves");
        assert!(String::from_utf8_lossy(&found).contains("found"));

        // An empty file is not a trust store: it would build a root store that
        // refuses every connection, which is a broken session rather than a
        // resolved one.
        let empty = directory.join("empty.pem");
        std::fs::write(&empty, "").expect("empty bundle is writable");
        assert_eq!(
            system_trust_anchors_among(Some(&empty.to_string_lossy()), &nowhere),
            Err(reason::CA_UNRESOLVED)
        );

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    #[cfg(unix)]
    fn resolution_produces_material_and_no_failure_carries_a_byte_of_it() {
        let (directory, backend) = fake_backend("resolve", Some("-----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nR0hJ\n-----END PRIVATE KEY-----\n"));
        let bundle = directory.join("bundle.pem");
        std::fs::write(
            &bundle,
            "-----BEGIN CERTIFICATE-----\nsystem\n-----END CERTIFICATE-----\n",
        )
        .expect("bundle is writable");

        let material = resolve_credentials(
            &backend,
            "acme/loam/mqtt",
            None,
            Some(&bundle.to_string_lossy()),
        )
        .expect("a resolvable reference produces material");
        assert!(!material.certificate.is_empty() && !material.key.is_empty());

        // The material is held, never described: `Debug` prints lengths so an
        // unwrapped error can never put a private key in a test log.
        let described = format!("{material:?}");
        assert!(
            !described.contains("R0hJ") && !described.contains("QUJD"),
            "{described}"
        );

        // Every failure names the input and nothing else.
        let (failing_dir, failing) = fake_backend("resolve-fail", None);
        let failure = resolve_credentials(&failing, "acme/loam/mqtt", None, None)
            .expect_err("an unresolvable reference refuses");
        assert_eq!(
            failure,
            ProvisionFailure::Credentials(reason::CREDENTIAL_REF_UNRESOLVED)
        );
        assert!(!format!("{failure:?}").contains("acme/loam/mqtt"));

        let _ = std::fs::remove_dir_all(directory);
        let _ = std::fs::remove_dir_all(failing_dir);
    }
}
