//! Turning one enrolled row into the inputs a live broker session needs.
//!
//! This module exists to keep the credential-touching work out of `connector.rs`.
//! The connector holds the broker socket; giving it filesystem reach as well
//! would concentrate exactly what the crate capability guard exists to separate.
//! `connector::provision_session` delegates here, and this module is admitted to
//! the guard's allowlists by name.
//!
//! What crosses the boundary, and what never does:
//!
//! - **In:** the enrolled row's broker endpoint, trust anchor, and the machine's
//!   identity directory (`<global-root>/federation/identity/`). The client
//!   certificate and key are read from PEM files there — the SSH-key model. The
//!   certificate is the single source of identity: the instance id is its SAN
//!   suffix, never a separate mint.
//! - **Out:** an `MqttSession` holding the material, and a `PeerRoster`. Nothing
//!   else. Every failure is a stable reason naming the *input* that failed, and
//!   no failure carries a byte of what it was looking for.

use crate::connector::{reason, ProvisionFailure};

/// Where the resolved credential material came from and how it is shaped. Held
/// only long enough to build the session; a hand-written `Debug` keeps the
/// material out of any diagnostic that formats it.
pub struct CredentialMaterial {
    /// The client certificate chain, PEM.
    pub certificate: Vec<u8>,
    /// The private key, PEM.
    pub key: Vec<u8>,
    /// The trust anchors to verify the broker against: parsed certificates from
    /// a pinned PEM file, or the bundled Mozilla roots as DER trust anchors.
    pub certificate_authority: rustls::RootCertStore,
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
                "certificate_authority_roots",
                &self.certificate_authority.len(),
            )
            .finish()
    }
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

/// The trust anchors as PEM for one enrollment: the pinned PEM trust file when
/// `ca_ref` names one, the bundled Mozilla roots otherwise.
///
/// The no-`ca_ref` default is the same compiled-in bundle on every OS —
/// Windows has no system CA bundle file, so the per-platform file search is
/// replaced by one code path that runs everywhere. `SSL_CERT_FILE`, the
/// conventional container override, is honored first when set. Both branches
/// must produce real bytes: the transport builds its root store from the PEM
/// it is handed, and an empty store refuses every connection.
///
/// The bundled roots are DER trust anchors, not PEM. `webpki-roots` ships them
/// that way; the PEM conversion here is the one place the two shapes meet, so
/// it is pinned by a unit test rather than assumed.
pub fn resolve_trust_anchors(
    ca_ref: Option<&str>,
    ssl_cert_file: Option<&str>,
) -> Result<rustls::RootCertStore, &'static str> {
    match ca_ref {
        // Present but blank is a malformed reference, not an absent one. Reading
        // it as "no CA pinned" would turn a typo into a silently wider trust
        // decision — the same downgrade an unresolvable reference is refused for.
        Some(reference) if reference.trim().is_empty() => Err(reason::CA_UNRESOLVED),
        Some(reference) => {
            let bytes = std::fs::read(reference.trim()).map_err(|_| reason::CA_UNRESOLVED)?;
            if bytes.is_empty() {
                return Err(reason::CA_UNRESOLVED);
            }
            build_root_store(&bytes)
        }
        None => bundled_trust_anchors(ssl_cert_file),
    }
}

/// The no-`ca_ref` trust path: `SSL_CERT_FILE` first, then the bundled Mozilla
/// roots. An empty override path is not an override — it must not silently
/// become "trust nothing".
fn bundled_trust_anchors(
    ssl_cert_file: Option<&str>,
) -> Result<rustls::RootCertStore, &'static str> {
    if let Some(path) = ssl_cert_file.map(str::trim).filter(|v| !v.is_empty()) {
        let bytes = std::fs::read(path).map_err(|_| reason::CA_UNRESOLVED)?;
        if !bytes.is_empty() {
            return build_root_store(&bytes);
        }
    }
    Ok(rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    })
}

pub fn build_root_store(pem: &[u8]) -> Result<rustls::RootCertStore, &'static str> {
    let der_certs: Vec<rustls::pki_types::CertificateDer<'static>> = pem_certificate_ders(pem)
        .into_iter()
        .map(rustls::pki_types::CertificateDer::from)
        .collect();
    let mut store = rustls::RootCertStore::empty();
    store.add_parsable_certificates(der_certs);
    if store.is_empty() {
        return Err(reason::CA_UNRESOLVED);
    }
    Ok(store)
}

fn base64_encode_bytes(bytes: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 63]);
        out.push(ALPHABET[(triple >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63]
        } else {
            b'='
        });
    }
    out
}

/// Resolve the credential material for one enrollment.
pub fn resolve_credentials(
    identity_root: &std::path::Path,
    ca_ref: Option<&str>,
    ssl_cert_file: Option<&str>,
) -> Result<CredentialMaterial, ProvisionFailure> {
    let credentials = ProvisionFailure::Credentials;
    let certificate_path = identity_root.join("client.pem");
    let key_path = identity_root.join("key.pem");
    // The credentials are the machine's private material, so the directory and
    // the two PEM files must be operator-private (`0700` dir, `0600` files on
    // Unix). Enforced on every read so an operator-placed bundle with looser
    // perms is hardened the moment it is used — not silently left world-readable.
    // Windows uses restrictive default ACLs; the icacls step is a smoke-leg
    // assertion, not a runtime behavior.
    let certificate =
        std::fs::read(&certificate_path).map_err(|_| credentials(reason::IDENTITY_REQUIRED))?;
    let key = std::fs::read(&key_path).map_err(|_| credentials(reason::IDENTITY_REQUIRED))?;
    harden_identity_permissions(identity_root, &certificate_path, &key_path)
        .map_err(ProvisionFailure::Credentials)?;
    let certificate_authority =
        resolve_trust_anchors(ca_ref, ssl_cert_file).map_err(ProvisionFailure::Credentials)?;
    Ok(CredentialMaterial {
        certificate,
        key,
        certificate_authority,
    })
}

/// Make the identity directory and its two PEM files operator-private:
/// `0700` on the directory, `0600` on `client.pem` and `key.pem` (Unix). The
/// exported copy that uninstall writes gets the same treatment. Declining
/// `no-op` on Windows, whose default per-user ACLs are already restrictive.
fn harden_identity_permissions(
    identity_root: &std::path::Path,
    certificate_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(identity_root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| reason::IDENTITY_REQUIRED)?;
        for path in [certificate_path, key_path] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| reason::IDENTITY_REQUIRED)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (identity_root, certificate_path, key_path);
    }
    Ok(())
}

/// Store the auto-enrolled identity bundle: `client.pem` + `key.pem` at the
/// identity root, created if absent, hardened with the same perms
/// (`0700` dir / `0600` files on Unix) as `resolve_credentials` enforces on
/// read. Returns `reason::IDENTITY_REQUIRED` on any failure so callers surface
/// a single typed path.
pub fn store_identity_bundle(
    identity_root: &std::path::Path,
    certificate_pem: &[u8],
    key_pem: &[u8],
) -> Result<(), &'static str> {
    std::fs::create_dir_all(identity_root).map_err(|_| reason::IDENTITY_REQUIRED)?;
    let certificate_path = identity_root.join("client.pem");
    let key_path = identity_root.join("key.pem");
    std::fs::write(&certificate_path, certificate_pem).map_err(|_| reason::IDENTITY_REQUIRED)?;
    std::fs::write(&key_path, key_pem).map_err(|_| reason::IDENTITY_REQUIRED)?;
    harden_identity_permissions(identity_root, &certificate_path, &key_path)
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

/// The DER bytes of every certificate block in a PEM blob, in order. Used by
/// the auto-enrollment HTTPS client to seed its root store from the same trust
/// material the MQTT session uses (bundled roots or a `ca_ref` PEM file).
pub fn pem_certificate_ders(pem: &[u8]) -> Vec<Vec<u8>> {
    let Ok(text) = std::str::from_utf8(pem) else {
        return Vec::new();
    };
    pem_blocks(text)
        .into_iter()
        .filter(|block| block.label.contains("CERTIFICATE"))
        .filter_map(|block| {
            let body: String = text[block.start..block.end]
                .lines()
                .filter(|line| !line.starts_with("-----"))
                .collect();
            base64_decode(&body)
        })
        .collect()
}

/// The base64 (standard alphabet, with padding) encoding of `bytes`.
pub fn base64_encode(bytes: &[u8]) -> Vec<u8> {
    base64_encode_bytes(bytes)
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

/// OID 2.5.29.17, `id-ce-subjectAltName`, as its DER content bytes.
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];

/// The `urn:loam:instance:` URI prefix a certificate's SAN must carry for its
/// suffix to be this machine's instance id.
const INSTANCE_SAN_PREFIX: &str = "urn:loam:instance:";

/// The instance id, derived from the client certificate's SAN and from nowhere
/// else. The SAN extension (OID 2.5.29.17) is read after the subject walk;
/// specifically the `urn:loam:instance:` URI — the SAN may also carry
/// `urn:loam:agent:`, but the URI is named, not assumed.
///
/// A SAN that is absent, not the URI, or whose suffix fails instance-id
/// validation is a typed refusal naming the SAN as the failing input.
pub fn certificate_instance_id(pem: &[u8]) -> Result<String, &'static str> {
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
    // Version (optional `[0] EXPLICIT`), serial, signature algorithm, issuer,
    // validity, subject — the same positional walk as the subject reader.
    let first = read_element(&der, cursor).ok_or(unresolved)?;
    if first.tag == 0xa0 {
        cursor = first.next;
    }
    let serial = read_element(&der, cursor).ok_or(unresolved)?;
    if serial.tag != 0x02 {
        return Err(unresolved);
    }
    cursor = serial.next;
    // signature algorithm, issuer, validity, subject — four SEQUENCEs.
    for _ in 0..4 {
        let element = read_element(&der, cursor).ok_or(unresolved)?;
        if element.tag != 0x30 {
            return Err(unresolved);
        }
        cursor = element.next;
    }
    // subjectPublicKeyInfo: one more SEQUENCE every real certificate carries
    // between the subject and the extensions. A walker that forgets it reads
    // the SPKI as the extensions element and refuses every real certificate.
    let spki = read_element(&der, cursor).ok_or(unresolved)?;
    if spki.tag != 0x30 {
        return Err(unresolved);
    }
    cursor = spki.next;
    // Optional issuer/subject unique IDs (`[1]`/`[2]` IMPLICIT) may sit between
    // the subject public key and the extensions; skip them rather than
    // misreading the extensions element.
    while let Some(element) = read_element(&der, cursor) {
        if element.tag == 0x81 || element.tag == 0x82 {
            cursor = element.next;
        } else {
            break;
        }
    }
    // Now at the extensions: `[3] EXPLICIT Extensions`, where Extensions is a
    // SEQUENCE OF Extension — the wrapper must be entered before any
    // extension's OID can be read.
    let explicit = read_element(&der, cursor).ok_or(unresolved)?;
    if explicit.tag != 0xa3 {
        return Err(unresolved);
    }
    let extensions = read_element(&der, explicit.content.0).ok_or(unresolved)?;
    if extensions.tag != 0x30 {
        return Err(unresolved);
    }
    let mut extension_cursor = extensions.content.0;
    while extension_cursor < extensions.content.1 {
        let extension = read_element(&der, extension_cursor).ok_or(unresolved)?;
        extension_cursor = extension.next;
        if extension.tag != 0x30 {
            continue;
        }
        let oid = read_element(&der, extension.content.0).ok_or(unresolved)?;
        if oid.tag != 0x06 || &der[oid.content.0..oid.content.1] != OID_SUBJECT_ALT_NAME {
            continue;
        }
        // Extension value: OCTET STRING wrapping a SEQUENCE of GeneralNames.
        let value = read_element(&der, oid.next).ok_or(unresolved)?;
        if value.tag != 0x04 {
            continue;
        }
        let names = read_element(&der, value.content.0).ok_or(unresolved)?;
        if names.tag != 0x30 {
            continue;
        }
        let mut name_cursor = names.content.0;
        while name_cursor < names.content.1 {
            let name = read_element(&der, name_cursor).ok_or(unresolved)?;
            name_cursor = name.next;
            if name.tag != 0x86 {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&der[name.content.0..name.content.1]) else {
                continue;
            };
            let Some(suffix) = text.strip_prefix(INSTANCE_SAN_PREFIX) else {
                continue;
            };
            if is_valid_instance_id(suffix) {
                return Ok(suffix.to_owned());
            }
            return Err(unresolved);
        }
        return Err(unresolved);
    }
    Err(unresolved)
}

/// The canonical instance id is the deployment's 26-character Crockford-base32
/// ULID (`provision-instance-id.sh` / `pki/issue-client.sh`):
/// `0123456789ABCDEFGHJKMNPQRSTVWXYZ`, case-insensitive, exact length. The
/// 32-hex form the old mint generated is dead with it.
pub fn is_valid_instance_id(value: &str) -> bool {
    value.len() == 26
        && value.bytes().all(|byte| {
            byte.is_ascii_digit()
                || matches!(byte, b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'T' | b'V'..=b'Z')
                || matches!(byte, b'a'..=b'h' | b'j'..=b'n' | b'p'..=b't' | b'v'..=b'z')
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

/// Where the federation profile lives: the directory holding `identity/`,
/// `rosters/`, `members/`, `loam.sqlite3`, and `config.json`. Rungs, first
/// wins:
///
/// 1. `LOAM_CONFIG_DIR` (names the loam config dir) → `<cfg>/federation`
/// 2. the platform config dir → `<config>/loam/federation`
/// 3. the legacy global root (`LOAM_HOME`) → `<root>/federation`
/// 4. the legacy default install (`HOME`) → `<home>/.agents/loam/federation`
///
/// Rungs 3-4 are pre-spec locations; [`migrate_legacy_profile`] copies a
/// legacy subtree into the config dir once.
///
/// The connector never resolves the home directory itself — every entry point
/// takes an explicit global root — but `provision_session` keeps its
/// `(&EnrolledRow)` shape and so holds no root. Rung 4 exists for that gap: an
/// install whose global root is not the default would otherwise read an empty
/// directory forever and report `roster-absent` with nothing wrong.
pub fn profile_root(
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
    loam_home: Option<&str>,
    home: Option<&str>,
) -> Result<std::path::PathBuf, &'static str> {
    let present = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    };
    if let Some(config) = present(config_dir) {
        return Ok(config.join("federation"));
    }
    if let Some(config) = config_root(config_dir, xdg_config_home, appdata, home) {
        return Ok(config.join("federation"));
    }
    if let Some(path) = present(loam_home) {
        return Ok(path.join("federation"));
    }
    if let Some(path) = present(home) {
        return Ok(path.join(".agents").join("loam").join("federation"));
    }
    Err(reason::PROFILE_ABSENT)
}

/// The platform-standard loam config directory (`<config>/loam`), or `None`
/// when no platform config basis resolves. Rung 1 of the ladder: `LOAM_CONFIG_DIR`.
fn config_root(
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
    home: Option<&str>,
) -> Option<std::path::PathBuf> {
    let present = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    };
    if let Some(config) = present(config_dir) {
        return Some(config);
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = present(home) {
            return Some(
                home.join("Library")
                    .join("Application Support")
                    .join("loam"),
            );
        }
        let _ = xdg_config_home;
        let _ = appdata;
        None
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = present(appdata) {
            return Some(appdata.join("loam"));
        }
        let _ = xdg_config_home;
        let _ = home;
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = present(xdg_config_home) {
            return Some(xdg.join("loam"));
        }
        if let Some(home) = present(home) {
            return Some(home.join(".config").join("loam"));
        }
        let _ = appdata;
        None
    }
}

/// The federation profile root this process should use.
pub fn configured_profile_root() -> Result<std::path::PathBuf, &'static str> {
    profile_root(
        std::env::var("LOAM_CONFIG_DIR").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("APPDATA").ok().as_deref(),
        std::env::var("LOAM_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The config root this process should use, when one resolves.
pub fn configured_config_root() -> Option<std::path::PathBuf> {
    config_root(
        std::env::var("LOAM_CONFIG_DIR").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("APPDATA").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The config-dir runtime store root (`<config>/loam/runtime`), the durable
/// versioned store the channel ledger records. Mirrors the Node resolver
/// (`integration/ledger.mjs` `runtimeStoreRoot`) so the runtime and connector
/// resolve the same path byte-for-byte. The store is new and never lived under
/// the legacy install root, so its ladder is `LOAM_CONFIG_DIR` → platform config
/// dir ONLY — no legacy `LOAM_HOME`/`~/.agents` rungs. `None` when nothing
/// resolves; callers must never write to a null path.
pub fn runtime_store_root(
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
    home: Option<&str>,
) -> Option<std::path::PathBuf> {
    config_root(config_dir, xdg_config_home, appdata, home).map(|config| config.join("runtime"))
}

/// The runtime store binary path
/// `<config>/loam/runtime/<version>/<target>/loam[.exe]`.
pub fn runtime_store_path(
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
    home: Option<&str>,
    version: &str,
    target: &str,
) -> Option<std::path::PathBuf> {
    let executable = if cfg!(target_os = "windows") {
        "loam.exe"
    } else {
        "loam"
    };
    runtime_store_root(config_dir, xdg_config_home, appdata, home)
        .map(|root| root.join(version).join(target).join(executable))
}

/// The runtime ledger path `<config>/loam/runtime/ledger.json`.
pub fn runtime_ledger_path(
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
    home: Option<&str>,
) -> Option<std::path::PathBuf> {
    runtime_store_root(config_dir, xdg_config_home, appdata, home)
        .map(|root| root.join("ledger.json"))
}

/// The runtime store root this process should use, when one resolves.
pub fn configured_runtime_store_root() -> Option<std::path::PathBuf> {
    configured_config_root().map(|config| config.join("runtime"))
}

/// Where per-project rosters live: `<profile>/rosters`. The explicit
/// per-resource override (`LOAM_FEDERATION_ROSTER_DIR`) names the directory
/// directly and wins.
pub fn roster_root(
    explicit: Option<&str>,
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
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
    profile_root(config_dir, xdg_config_home, appdata, loam_home, home)
        .map(|profile| profile.join("rosters"))
}

/// The roster root this process should use.
pub fn configured_roster_root() -> Result<std::path::PathBuf, &'static str> {
    roster_root(
        std::env::var("LOAM_FEDERATION_ROSTER_DIR").ok().as_deref(),
        std::env::var("LOAM_CONFIG_DIR").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("APPDATA").ok().as_deref(),
        std::env::var("LOAM_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The federation registry (`loam.sqlite3`) path: the enrollment store moved
/// into the config-dir profile (`<profile>/loam.sqlite3`) so it survives
/// uninstall with the rest of the identity. Resolution, first wins:
///
/// 1. `LOAM_CONFIG_DIR` → `<cfg>/federation/loam.sqlite3`
/// 2. an explicit `--global-root` (the operator's knob, and the hermetic-test
///    Root) → `<root>/loam.sqlite3`
/// 3. the platform config-dir profile → `<config>/loam/federation/loam.sqlite3`
/// 4. the legacy install root → `<global-root>/loam.sqlite3`
///
/// Rung 2 existing as it does keeps the CLI's `--global-root` authoritative
/// for non-default installs and tests (which would otherwise resolve the real
/// HOME profile and clobber it), while rung 1 lets new installs point the
/// registry at the surviving config dir explicitly.
pub fn configured_registry_path(
    legacy_root: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, &'static str> {
    if let Some(cfg) = std::env::var("LOAM_CONFIG_DIR")
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Ok(std::path::PathBuf::from(cfg)
            .join("federation")
            .join("loam.sqlite3"));
    }
    if let Some(root) = legacy_root {
        return Ok(root.join("loam.sqlite3"));
    }
    configured_profile_root().map(|profile| profile.join("loam.sqlite3"))
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
    // A valid scope atom is exactly one ordinary path component equal to itself.
    // This rejects empty, ".", "..", separators, and absolute roots — AND a
    // Windows drive prefix ("C:foo", "C:"), which `Path::join` treats as a root
    // and uses to REPLACE the roster root, escaping it into a cross-tenant read.
    // A character blacklist misses the drive prefix; `Component::Normal` does
    // not, on every platform. The `to_str` equality also rejects any value that
    // normalizes away (a trailing separator, an embedded "./").
    let mut components = std::path::Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(only)), None) => only.to_str() == Some(value),
        _ => false,
    }
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

/// Write the peer roster for one project, from broker-served membership.
///
/// The broker's retained membership payload (principals + origins) is the
/// author; the connector only subscribes and writes the local file from it
/// (`federation-enrollment-simplification.md`). The write applies the same
/// validation as the read — a payload that the read would refuse is never
/// written, so a half-admit can never be persisted. The write is atomic
/// (temp file + rename), so a crash mid-write never leaves a partial roster.
pub fn write_roster(
    root: &std::path::Path,
    org_id: &str,
    project_id: &str,
    body: &str,
) -> Result<(), &'static str> {
    // The payload is validated through the same rules the session build uses:
    // a payload that admits nobody (or admits everyone) must not become the
    // on-disk truth.
    if !is_path_atom(org_id) || !is_path_atom(project_id) {
        return Err(reason::ROSTER_MALFORMED);
    }
    let parsed = crate::json::parse(body).map_err(|_| reason::ROSTER_MALFORMED)?;
    let crate::json::Value::Object(fields) = &parsed else {
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
        bare_ids(parsed.get("principals")),
        bare_ids(parsed.get("origins")),
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
    if principals.is_empty() || origins.is_empty() {
        return Err(reason::ROSTER_EMPTY);
    }
    let directory = root.join(org_id);
    std::fs::create_dir_all(&directory).map_err(|_| reason::ROSTER_MALFORMED)?;
    let path = directory.join(format!("{project_id}.json"));
    let temporary = directory.join(format!("{project_id}.json.tmp"));
    std::fs::write(&temporary, body.as_bytes()).map_err(|_| reason::ROSTER_MALFORMED)?;
    std::fs::rename(&temporary, &path).map_err(|_| reason::ROSTER_MALFORMED)
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
// Member cards and roster assembly
// ---------------------------------------------------------------------------

/// One self-announced member card. Every connector publishes its own retained
/// card on `loam/v1/{org}/members/{instance_id}`; each project's roster is
/// assembled locally from the cards whose `projects` includes it. A card
/// carries no secret — no key, no credential — only the instance's identity
/// and the projects it is enrolled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCard {
    pub instance_id: String,
    pub principal_id: String,
    pub display_name: Option<String>,
    pub joined_at: String,
    pub projects: Vec<String>,
}

/// The member-card topic path: `loam/v1/{org}/members/{instance_id}`.
pub fn member_topic(org_id: &str, instance_id: &str) -> String {
    format!("loam/v1/{org_id}/members/{instance_id}")
}

/// The subscribe filter that captures every member card for an org.
pub fn member_filter(org_id: &str) -> String {
    format!("loam/v1/{org_id}/members/+")
}

/// Write one member card to the config-dir cache path:
/// `<profile>/rosters/{org}/members/{instance_id}.json`. The write applies the
/// same validation as the read (atomic temp-file + rename), so a malformed
/// card is never persisted.
pub fn write_member_card(
    root: &std::path::Path,
    org_id: &str,
    card: &MemberCard,
) -> Result<(), &'static str> {
    if !is_path_atom(org_id) || !is_path_atom(&card.instance_id) {
        return Err(reason::ROSTER_MALFORMED);
    }
    if !card
        .projects
        .iter()
        .all(|project| is_valid_project_listing(project))
    {
        return Err(reason::ROSTER_MALFORMED);
    }
    let body = member_card_json(card);
    if parse_member_card(&body).is_err() {
        return Err(reason::ROSTER_MALFORMED);
    }
    let directory = root.join(org_id).join("members");
    std::fs::create_dir_all(&directory).map_err(|_| reason::ROSTER_MALFORMED)?;
    let path = directory.join(format!("{}.json", card.instance_id));
    let temporary = directory.join(format!("{}.json.tmp", card.instance_id));
    std::fs::write(&temporary, body.as_bytes()).map_err(|_| reason::ROSTER_MALFORMED)?;
    std::fs::rename(&temporary, &path).map_err(|_| reason::ROSTER_MALFORMED)
}

/// Read one member card from the cache path. `None` means the card is absent;
/// `Err` means a present card is malformed.
pub fn read_member_card(
    root: &std::path::Path,
    org_id: &str,
    instance_id: &str,
) -> Result<Option<MemberCard>, &'static str> {
    if !is_path_atom(org_id) || !is_path_atom(instance_id) {
        return Err(reason::ROSTER_MALFORMED);
    }
    let path = root
        .join(org_id)
        .join("members")
        .join(format!("{instance_id}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    parse_member_card(&text).map(Some)
}

/// Reassemble one project's roster from the member-card cache: every card
/// whose `projects` includes the project contributes its instance as an origin
/// and its principal as a principal, deduplicated. Never written by
/// hand-authoring and never a single aggregate; the connector builds this
/// locally from the retained cards. A project with no matching card yields an
/// empty roster (the `no-peer-roster` gate fires downstream).
pub fn assemble_project_roster(
    root: &std::path::Path,
    org_id: &str,
    project_id: &str,
) -> Result<crate::connector::PeerRoster, &'static str> {
    if !is_path_atom(org_id) || !is_path_atom(project_id) {
        return Err(reason::ROSTER_MALFORMED);
    }
    let members_dir = root.join(org_id).join("members");
    let mut principals: Vec<String> = Vec::new();
    let mut origins: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(&members_dir) {
        Ok(entries) => entries,
        Err(_) => {
            return Ok(crate::connector::PeerRoster {
                principals,
                origins,
            })
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
            continue;
        };
        if !is_path_atom(stem) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(card) = parse_member_card(&text) else {
            continue;
        };
        if !card.projects.iter().any(|project| project == project_id) {
            continue;
        }
        if !principals.contains(&card.principal_id) {
            principals.push(card.principal_id);
        }
        if !origins.contains(&card.instance_id) {
            origins.push(card.instance_id);
        }
    }
    Ok(crate::connector::PeerRoster {
        principals,
        origins,
    })
}

/// A project listing in a member card must be a single ordinary path atom
/// (same rule as roster scopes), so it can never escape its directory.
fn is_valid_project_listing(project: &str) -> bool {
    is_path_atom(project)
}

/// Serialize a member card to JSON for the wire (retained publish) or the
/// cache file. Order is stable so a re-publish of an unchanged card writes
/// identical bytes (retained-card idempotence).
pub fn member_card_to_json(card: &MemberCard) -> String {
    member_card_json(card)
}

/// Parse a member-card payload from the wire (the pump's read path), exposing
/// the private validator so `connector.rs` (which holds the broker socket and
/// is barred from reading files) can validate + cache a received card without
/// reimplementing the shape rules.
pub fn parse_member_card_pub(text: &str) -> Result<MemberCard, &'static str> {
    parse_member_card(text)
}

/// Serialize an assembled `PeerRoster` to the B.7 roster JSON body the write
/// path validates before persisting. `PeerRoster` objects carry already-valid
/// id lists, so this is a straight projection; the write re-validates through
/// `write_roster` before anything is persisted.
pub fn roster_body(roster: &crate::connector::PeerRoster) -> String {
    let values = |ids: &[String]| {
        crate::json::Value::Array(
            ids.iter()
                .map(|id| crate::json::Value::String(id.clone()))
                .collect(),
        )
    };
    crate::json::Value::Object(vec![
        ("principals".into(), values(&roster.principals)),
        ("origins".into(), values(&roster.origins)),
    ])
    .to_json()
}

/// Serialize a member card to JSON for the wire (retained publish) or the
/// cache file. Order is stable so a re-publish of an unchanged card writes
/// identical bytes (retained-card idempotence).
fn member_card_json(card: &MemberCard) -> String {
    let value = crate::json::Value::Object(vec![
        (
            "instance_id".into(),
            crate::json::Value::String(card.instance_id.clone()),
        ),
        (
            "principal_id".into(),
            crate::json::Value::String(card.principal_id.clone()),
        ),
        (
            "display_name".into(),
            match &card.display_name {
                Some(name) => crate::json::Value::String(name.clone()),
                None => crate::json::Value::Null,
            },
        ),
        (
            "joined_at".into(),
            crate::json::Value::String(card.joined_at.clone()),
        ),
        (
            "projects".into(),
            crate::json::Value::Array(
                card.projects
                    .iter()
                    .map(|p| crate::json::Value::String(p.clone()))
                    .collect(),
            ),
        ),
    ]);
    value.to_json()
}

/// Parse a member-card payload with the same duck-typing the roster reader
/// uses: required fields with the right shapes, no wildcards, no control
/// characters, and a project list that is all path atoms. No secret-shaped
/// field is accepted (the reader refuses unknown keys only at the roster
/// level; a card carries no secret by construction).
fn parse_member_card(text: &str) -> Result<MemberCard, &'static str> {
    let document = crate::json::parse(text).map_err(|_| reason::ROSTER_MALFORMED)?;
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
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let instance_id = get("instance_id")
        .and_then(crate::json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let principal_id = get("principal_id")
        .and_then(crate::json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let joined_at = get("joined_at")
        .and_then(crate::json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let display_name = match get("display_name") {
        Some(crate::json::Value::Null) => None,
        Some(crate::json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let (Some(instance_id), Some(principal_id), Some(joined_at)) =
        (instance_id, principal_id, joined_at)
    else {
        return Err(reason::ROSTER_MALFORMED);
    };
    let projects = get("projects")
        .and_then(crate::json::Value::as_array)
        .ok_or(reason::ROSTER_MALFORMED)?;
    let mut project_list: Vec<String> = Vec::with_capacity(projects.len());
    for project in projects {
        let text = project.as_str().ok_or(reason::ROSTER_MALFORMED)?.trim();
        if text.is_empty() || !is_valid_project_listing(text) {
            return Err(reason::ROSTER_MALFORMED);
        }
        project_list.push(text.to_owned());
    }
    // instance_id and principal_id flow into MQTT topics/filesystem paths, so
    // they must carry no wildcard or control character. principal_id is an
    // email: a plus-addressed one (user+tag@host) trips the '+' guard and is
    // *consciously* refused — '+' is a topic wildcard, so a plus-addressed
    // principal could never be a safe topic atom. Broker CN policy may widen
    // one day, but the guard stays until topics can carry it.
    for value in [&instance_id, &principal_id] {
        if value
            .chars()
            .any(|c| c.is_control() || c == '+' || c == '#' || c == '*')
        {
            return Err(reason::ROSTER_MALFORMED);
        }
    }
    // joined_at never enters a topic or path — it is an RFC3339 timestamp, and
    // chrono's to_rfc3339() emits a "+00:00" offset the wildcard guard above
    // would reject (the connector's own cards fail their own parser otherwise).
    // Validate it as a real timestamp instead, still refusing control chars.
    if joined_at.chars().any(char::is_control)
        || chrono::DateTime::parse_from_rfc3339(&joined_at).is_err()
    {
        return Err(reason::ROSTER_MALFORMED);
    }
    Ok(MemberCard {
        instance_id,
        principal_id,
        display_name,
        joined_at,
        projects: project_list,
    })
}

/// The durable `config.json` in the profile: broker defaults and
/// org/project inference overrides, machine- and human-editable. Absent or
/// empty is read as `None` (all defaults); a present, malformed `config.json`
/// is an explicit error so a human edit that broke the file is surfaced, not
/// silently ignored.
pub fn read_config(mut root: &std::path::Path) -> Result<Option<crate::json::Value>, &'static str> {
    // `resolve` paths point inside the profile; `config.json` sits at the
    // profile root.
    if root.file_name() == Some(std::ffi::OsStr::new("federation")) {
        root = root.parent().unwrap_or(root);
    }
    let path = root.join("config.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    crate::json::parse(&text)
        .map(Some)
        .map_err(|_| reason::ROSTER_MALFORMED)
}

// ---------------------------------------------------------------------------
// One-time legacy migration
// ---------------------------------------------------------------------------

/// One-time migration of a pre-spec profile from the legacy global root into
/// the config dir. Copies the `federation/` subtree — identity, rosters,
/// registry, and any member cards — when the legacy location exists and the
/// config-dir profile does not yet hold one. The legacy files are copied, not
/// moved, so both resolve during the transition. Returns whether a copy
/// happened. A missing legacy profile is `Ok(false)`, never an error.
pub fn migrate_legacy_profile() -> Result<bool, &'static str> {
    // Destination: the config-dir profile root (rungs 1-2 of the ladder).
    let config_dir = std::env::var("LOAM_CONFIG_DIR").ok();
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let appdata = std::env::var("APPDATA").ok();
    let loam_home = std::env::var("LOAM_HOME").ok();
    let home = std::env::var("HOME").ok();
    let Some(config_root) = config_root(
        config_dir.as_deref(),
        xdg.as_deref(),
        appdata.as_deref(),
        home.as_deref(),
    ) else {
        return Ok(false);
    };
    let target = config_root.join("federation");
    if target.join("loam.sqlite3").exists() {
        return Ok(false);
    }
    // Source: the legacy subtree (rungs 3-4) when one exists.
    let present = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
    };
    let legacy_root = present(loam_home.as_deref())
        .map(|path| path.join("federation"))
        .or_else(|| {
            present(home.as_deref())
                .map(|path| path.join(".agents").join("loam").join("federation"))
        });
    let Some(source) = legacy_root else {
        return Ok(false);
    };
    if !source.is_dir() {
        return Ok(false);
    }
    copy_tree(&source, &target)?;
    Ok(true)
}

/// Recursively copy one directory tree into another. Only `std::fs`; symlinks
/// are copied as symlinks. Used for the one-time legacy migration.
fn copy_tree(source: &std::path::Path, target: &std::path::Path) -> Result<(), &'static str> {
    std::fs::create_dir_all(target).map_err(|_| reason::PROFILE_COPY_FAILED)?;
    for entry in std::fs::read_dir(source).map_err(|_| reason::PROFILE_COPY_FAILED)? {
        let entry = entry.map_err(|_| reason::PROFILE_COPY_FAILED)?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|_| reason::PROFILE_COPY_FAILED)?;
        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else if file_type.is_symlink() {
            let link = std::fs::read_link(&from).map_err(|_| reason::PROFILE_COPY_FAILED)?;
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(&link, &to);
            }
            #[cfg(windows)]
            {
                let _ = std::os::windows::fs::symlink_file(&link, &to);
            }
        } else {
            std::fs::copy(&from, &to).map_err(|_| reason::PROFILE_COPY_FAILED)?;
        }
    }
    Ok(())
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
/// identity directory is empty should be told that, not told their roster is
/// missing.
pub fn resolve(
    row: &crate::enrollment::EnrolledRow,
) -> Result<(crate::connector::MqttSession, crate::connector::PeerRoster), ProvisionFailure> {
    let credentials = ProvisionFailure::Credentials;
    let (host, port) = split_endpoint(&row.broker_endpoint).map_err(credentials)?;
    let identity_root = configured_identity_root().map_err(credentials)?;
    let material = resolve_credentials(
        &identity_root,
        row.ca_ref.as_deref(),
        // The conventional trust-bundle override, honored first. Reading it here
        // is what makes that rung real: passing `None` left the documented
        // override unconsulted, which the two-instance tier caught by failing
        // to trust its own fixture CA.
        std::env::var("SSL_CERT_FILE").ok().as_deref(),
    )?;

    // Who this session is, read from the certificate the broker will
    // authenticate and from nothing the caller can influence. The instance id
    // is the certificate's SAN suffix — the single identity source; a row whose
    // stored instance id disagrees with the certificate is refused rather than
    // silently resolved either way.
    let subject = certificate_subject(&material.certificate).map_err(credentials)?;
    let instance_id = certificate_instance_id(&material.certificate).map_err(credentials)?;
    if instance_id != row.instance_id {
        return Err(credentials(reason::IDENTITY_MISMATCH));
    }
    let (local_email, _local_name) = git_identity(std::path::Path::new(&row.display_path));
    match_local_identity(&subject, local_email.as_deref()).map_err(credentials)?;

    let roster_root = configured_roster_root().map_err(ProvisionFailure::Roster)?;
    let roster = match read_roster(&roster_root, &row.org_id, &row.project_id) {
        Ok(roster) => roster,
        // Self-announce re-scopes the roster gate: an enrolled machine always
        // admits itself (its own retained member card), so an absent or empty
        // assembled roster is the ordinary "first join / no colleagues yet"
        // state, not a refusal. The machine opens a self-only live session and
        // the pump assembles peers' cards into the file as they arrive. The
        // typed `no-peer-roster` refusal survives only for a genuinely unusable
        // roster — malformed or wildcard data — never for "nobody known yet".
        Err(reason::ROSTER_ABSENT)
        | Err(reason::ROSTER_EMPTY)
        | Err(reason::ROSTER_NO_ORIGINS)
        | Err(reason::ROSTER_NO_PRINCIPALS) => crate::connector::PeerRoster {
            principals: vec![subject.common_name.clone()],
            origins: vec![row.instance_id.clone()],
        },
        Err(other) => return Err(ProvisionFailure::Roster(other)),
    };

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
            // identity beside the certificate — the defect this slice exists to
            // close.
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

/// Where the machine's identity bundle lives: `<profile>/identity/`, resolved
/// through the profile ladder. The explicit per-resource override
/// (`LOAM_FEDERATION_IDENTITY_DIR`) names the directory directly and wins.
pub fn identity_root(
    explicit: Option<&str>,
    config_dir: Option<&str>,
    xdg_config_home: Option<&str>,
    appdata: Option<&str>,
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
    profile_root(config_dir, xdg_config_home, appdata, loam_home, home)
        .map(|profile| profile.join("identity"))
}

/// The identity root this process should use.
pub fn configured_identity_root() -> Result<std::path::PathBuf, &'static str> {
    identity_root(
        std::env::var("LOAM_FEDERATION_IDENTITY_DIR")
            .ok()
            .as_deref(),
        std::env::var("LOAM_CONFIG_DIR").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("APPDATA").ok().as_deref(),
        std::env::var("LOAM_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----\nQUJD\n-----END CERTIFICATE-----\n";
    const INTERMEDIATE: &str = "-----BEGIN CERTIFICATE-----\nREVG\n-----END CERTIFICATE-----\n";
    const KEY: &str = "-----BEGIN PRIVATE KEY-----\nR0hJ\n-----END PRIVATE KEY-----\n";

    /// A real, parseable self-signed EC (P-256) CA certificate. `CERTIFICATE`
    /// above is a stand-in body that the split-and-armor readers accept but
    /// `add_parsable_certificates` drops as non-DER; the trust-store rungs need
    /// a certificate that actually parses into a root, so they use this one.
    const REAL_ROOT_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBiDCCAS2gAwIBAgIUGTdvZg7AlDyozb88b5pPmypC42YwCgYIKoZIzj0EAwIw\nGTEXMBUGA1UEAwwObG9hbS10ZXN0LXJvb3QwHhcNMjYwODE0MDgwNzM5WhcNMzYw\nODExMDgwNzM5WjAZMRcwFQYDVQQDDA5sb2FtLXRlc3Qtcm9vdDBZMBMGByqGSM49\nAgEGCCqGSM49AwEHA0IABMF4yeUlY2aKHZWzELrMiGqnSXLwHhTiLbwvxJ4O40vM\n0UhRyNlsr0aii4fctuDvY9J6m2ZVNBJK3ErSN00vQ2GjUzBRMB0GA1UdDgQWBBTr\n6cr//tZbqKvYw52gq1Hi9n8uvTAfBgNVHSMEGDAWgBTr6cr//tZbqKvYw52gq1Hi\n9n8uvTAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0kAMEYCIQDoPSdWAVKz\nl+LzXM5pHyUpN+kgi5kei08hx7zrQcPeYgIhANqPWWBYGx/qRoS32iA6sZpCawF+\nJ5oTt4GkFclCoQpE\n-----END CERTIFICATE-----\n";

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
        certificate_der_with_san(
            issuer_common,
            subject_common,
            subject_given,
            versioned,
            None,
        )
    }

    /// A certificate with an optional SAN extension appended after the subject.
    /// The SAN is built as `[3] EXPLICIT` wrapping a SEQUENCE of GeneralNames;
    /// each name is a `[6]` (0x86) IA5String URI.
    fn certificate_der_with_san(
        issuer_common: &str,
        subject_common: Option<&str>,
        subject_given: Option<&str>,
        versioned: bool,
        san_uris: Option<&[&str]>,
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
        // Stand-in subjectPublicKeyInfo: real certificates carry an SPKI
        // SEQUENCE here, and the SAN walk must skip it to reach the extensions.
        tbs.extend(der(0x30, &der(0x03, &[0x00])));
        if let Some(uris) = san_uris {
            let mut names = Vec::new();
            for uri in uris {
                names.extend(der(0x86, uri.as_bytes()));
            }
            let extension = der(0x30, &{
                let mut body = der(0x06, OID_SUBJECT_ALT_NAME);
                body.extend(der(0x04, &der(0x30, &names)));
                body
            });
            // RFC 5280: [3] EXPLICIT wraps an Extensions SEQUENCE OF Extension.
            tbs.extend(der(0xa3, &der(0x30, &extension)));
        }

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
    fn a_malformed_endpoint_refuses_before_any_credential_is_read() {
        // Ordering is load-bearing, not incidental: the endpoint is checked
        // before the identity path is consulted, so a typo in an enrollment can
        // never spend a filesystem read to learn something already visible in
        // the row.
        let row = enrolled_row("instance-01", "not-an-endpoint");
        assert_eq!(
            resolve(&row).err(),
            Some(ProvisionFailure::Credentials(reason::ENDPOINT_MALFORMED))
        );
    }

    #[test]
    fn identity_divergence_is_impossible_the_row_must_match_the_certificate_san() {
        // Write a valid identity bundle whose SAN suffix is a known ULID, then
        // prove the seam refuses any row whose instance_id disagrees with it.
        // This is the connector-side SAN enforcement: the certificate is the
        // single identity source, so a stale or tampered row cannot silently
        // open a session under a different identity.
        let ulid = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let directory = temp_dir("identity-divergence");
        let identity = directory.join("identity");
        std::fs::create_dir_all(&identity).expect("identity directory is creatable");
        let certificate = pem(&certificate_der_with_san(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
            Some(&[&format!("urn:loam:instance:{ulid}")]),
        ));
        std::fs::write(identity.join("client.pem"), &certificate).expect("certificate is writable");
        std::fs::write(identity.join("key.pem"), KEY).expect("key is writable");
        std::env::set_var("LOAM_FEDERATION_IDENTITY_DIR", &identity);

        // A matching row gets past the identity gate; with self-announce, an
        // absent roster is the ordinary first-join state and resolves to the
        // machine's own self-admitted roster, not a refusal. It must NOT be
        // refused as an identity mismatch.
        let matching = enrolled_row(ulid, "mqtts://broker.acme.example:8883");
        let (_, roster) = resolve(&matching).expect("a matching row passes the SAN gate");
        assert_eq!(
            roster,
            crate::connector::PeerRoster {
                principals: vec!["sam@example.test".to_owned()],
                origins: vec![ulid.to_owned()],
            },
            "a matching row self-admits its own origin and principal"
        );

        // A row claiming a different instance id is refused outright: the cert
        // says who this machine is, and the row cannot override it.
        let divergent = enrolled_row(
            "01ARZ3NDEKTSV4RRFFQ69G5FBX",
            "mqtts://broker.acme.example:8883",
        );
        assert_eq!(
            resolve(&divergent).err(),
            Some(ProvisionFailure::Credentials(reason::IDENTITY_MISMATCH))
        );

        std::env::remove_var("LOAM_FEDERATION_IDENTITY_DIR");
        let _ = std::fs::remove_dir_all(directory);
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

    /// The default-vs-legacy part of the ladder is platform-specific: `~/.config`
    /// on Linux/macOS-variants, `Library/Application Support` on macOS, and
    /// `%APPDATA%` on Windows. The rung itself is the same everywhere.
    fn platform_roster_default_path() -> std::path::PathBuf {
        // Supply both a HOME and an APPDATA so the platform config rung resolves
        // on every OS: Linux/macOS read HOME, Windows reads APPDATA, and
        // config_root ignores whichever input its platform does not use.
        config_root(
            None,
            None,
            Some("C:/Users/op/AppData/Roaming"),
            Some("/home/op"),
        )
        .map(|config| config.join("federation").join("rosters"))
        .expect("the default config root resolves")
    }

    #[test]
    fn the_roster_root_walks_the_profile_ladder_in_order() {
        // The explicit per-resource override names the directory directly.
        assert_eq!(
            roster_root(Some("/explicit"), None, None, None, None, Some("/home/op"),).unwrap(),
            std::path::PathBuf::from("/explicit")
        );
        // Then the config-dir rungs: LOAM_CONFIG_DIR first, then the platform
        // config dir, then the legacy global root, then the legacy default
        // install. This is what makes the connector's `provision_session`
        // (which never sees the deployer's root) resolve the same profile the
        // CLI wrote to.
        assert_eq!(
            roster_root(None, Some("/cfg"), None, None, None, Some("/home/op")).unwrap(),
            std::path::PathBuf::from("/cfg/federation/rosters")
        );
        assert_eq!(
            roster_root(
                None,
                None,
                None,
                Some("C:/Users/op/AppData/Roaming"),
                None,
                Some("/home/op")
            )
            .unwrap(),
            platform_roster_default_path()
        );
        // The legacy global root is a fallback for when no config basis resolves
        // at all — `LOAM_HOME` outranks the default install, both below the
        // config dir.
        assert_eq!(
            roster_root(None, None, None, None, None, None),
            Err(reason::PROFILE_ABSENT)
        );
        assert_eq!(
            roster_root(None, None, Some(""), Some(""), Some("/loam-home"), None).unwrap(),
            std::path::PathBuf::from("/loam-home/federation/rosters")
        );
        assert_eq!(
            roster_root(
                None,
                None,
                None,
                Some("C:/Users/op/AppData/Roaming"),
                None,
                Some("/home/op")
            )
            .unwrap(),
            platform_roster_default_path()
        );
        // Blank is not a value at any rung → it falls through to the platform
        // default. On Windows that default reads APPDATA (blank here), so
        // blank-everything is a genuinely absent profile, not a default path.
        #[cfg(not(windows))]
        assert_eq!(
            roster_root(
                Some("  "),
                Some(""),
                Some(""),
                Some(""),
                Some(""),
                Some("/home/op"),
            )
            .unwrap(),
            platform_roster_default_path()
        );
        #[cfg(windows)]
        assert_eq!(
            roster_root(
                Some("  "),
                Some(""),
                Some(""),
                Some(""),
                Some(""),
                Some("/home/op"),
            )
            .unwrap(),
            // Windows has no APPDATA here, so the platform config rung is skipped
            // and the ladder falls to the legacy HOME default (~/.agents/loam).
            std::path::PathBuf::from("/home/op")
                .join(".agents")
                .join("loam")
                .join("federation")
                .join("rosters")
        );
        // Nothing at all is an absent profile, not a path built from nothing.
        assert_eq!(
            roster_root(None, None, None, None, None, None),
            Err(reason::PROFILE_ABSENT)
        );
    }

    #[test]
    fn the_runtime_store_resolves_under_the_config_dir_only() {
        let executable = if cfg!(target_os = "windows") {
            "loam.exe"
        } else {
            "loam"
        };
        // LOAM_CONFIG_DIR names the config root; the store is <config>/runtime.
        assert_eq!(
            runtime_store_path(
                Some("/cfg"),
                None,
                None,
                None,
                "0.11.0-next.15",
                "x86_64-unknown-linux-musl"
            )
            .unwrap(),
            std::path::Path::new("/cfg")
                .join("runtime")
                .join("0.11.0-next.15")
                .join("x86_64-unknown-linux-musl")
                .join(executable),
        );
        assert_eq!(
            runtime_ledger_path(Some("/cfg"), None, None, None).unwrap(),
            std::path::Path::new("/cfg")
                .join("runtime")
                .join("ledger.json"),
        );
        // The store never falls back to a legacy install root: with no config
        // basis at all it resolves to nothing (unlike the federation profile,
        // whose ladder keeps the legacy rungs). This is the byte-identical
        // config-dir-only ladder the Node resolver uses.
        assert!(runtime_store_root(None, None, None, None).is_none());
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
    fn the_broker_served_membership_payload_is_written_whole_and_validated_first() {
        let root = temp_dir("roster-write");
        // A usable membership payload is written and read back exactly as the
        // session build would admit it.
        let body =
            r#"{"principals":["ada@example.test"],"origins":["01ARZ3NDEKTSV4RRFFQ69G5FAV"]}"#;
        super::write_roster(&root, "acme", "loam", body).expect("a usable payload writes");
        let roster = read_roster(&root, "acme", "loam").expect("the written roster reads");
        assert_eq!(roster.principals, vec!["ada@example.test".to_owned()]);
        assert_eq!(
            roster.origins,
            vec!["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()]
        );

        // A payload that the read would refuse is never persisted: empty,
        // one-sided, wildcard, and malformed JSON.
        for (label, body) in [
            ("empty", r#"{"principals":[],"origins":[]}"#),
            (
                "no-origins",
                r#"{"principals":["ada@example.test"],"origins":[]}"#,
            ),
            (
                "wildcard",
                r#"{"principals":["*"],"origins":["01ARZ3NDEKTSV4RRFFQ69G5FAV"]}"#,
            ),
            ("malformed", "{not json"),
        ] {
            let before = read_roster(&root, "acme", label);
            assert!(
                super::write_roster(&root, "acme", label, body).is_err(),
                "{label} must not be written"
            );
            let after = read_roster(&root, "acme", label);
            assert_eq!(before, after, "{label} must leave no partial file");
        }
        // A traversal scope never escapes the roster root.
        assert!(
            super::write_roster(
                &root,
                "..",
                "victim",
                r#"{"principals":["a"],"origins":["b"]}"#
            )
            .is_err(),
            "a traversal org must be refused"
        );

        // The write is atomic: no temp file survives a successful write.
        assert!(!root.join("acme").join("loam.json.tmp").exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_card_the_writer_produces_parses_back_through_its_own_reader() {
        // The invariant the parser broke: a card the connector itself publishes
        // (own_member_card shape, with a REAL chrono to_rfc3339() joined_at that
        // carries a "+00:00" offset) MUST parse back. The old guard applied the
        // MQTT-wildcard '+' check to joined_at and rejected every self-published
        // card, so no roster ever materialized. Use the real writer path
        // (member_card_to_json) and a real timestamp — not a hand-fixed string —
        // so a future divergence between writer and reader fails here.
        let card = MemberCard {
            instance_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            principal_id: "ada@example.test".to_owned(),
            display_name: Some("Ada".to_owned()),
            joined_at: chrono::Utc::now().to_rfc3339(),
            projects: vec!["loam".to_owned()],
        };
        assert!(
            card.joined_at.contains('+'),
            "the writer's timestamp must carry the +00:00 offset this test guards"
        );
        let json = member_card_to_json(&card);
        let parsed = parse_member_card(&json).expect("a writer-produced card must parse back");
        assert_eq!(parsed.instance_id, card.instance_id);
        assert_eq!(parsed.principal_id, card.principal_id);
        assert_eq!(parsed.joined_at, card.joined_at);
        assert_eq!(parsed.projects, card.projects);

        // joined_at is validated as a timestamp, not by the wildcard guard: a
        // non-timestamp there still refuses, and a control character still does.
        let mut broken = card.clone();
        broken.joined_at = "not-a-timestamp".to_owned();
        assert!(
            parse_member_card(&member_card_to_json(&broken)).is_err(),
            "a non-RFC3339 joined_at must refuse"
        );

        // But the wildcard guard still bites instance_id/principal_id, which do
        // enter topics and paths.
        let mut wild = card.clone();
        wild.instance_id = "inst+ance".to_owned();
        assert!(
            parse_member_card(&member_card_to_json(&wild)).is_err(),
            "a wildcard in instance_id must still refuse"
        );
    }

    #[test]
    fn a_scope_atom_is_one_ordinary_path_component_equal_to_itself() {
        // The predicate under the roster path join, tested directly. Each of
        // these escapes or normalizes away on every platform and must refuse.
        for bad in [
            "",
            ".",
            "..",
            "/",
            "/abs",
            "a/b",
            "a/../b",
            "sub/dir",
            "trailing/",
        ] {
            assert!(!is_path_atom(bad), "{bad:?} must not be a scope atom");
        }
        for good in ["acme", "loam", "instance-01", "sam+loam@example.test"] {
            assert!(is_path_atom(good), "{good:?} must be a scope atom");
        }
        // A Windows drive prefix or UNC root escapes the roster root via
        // `Path::join` (push replaces self on a prefixed/rooted segment) — the
        // residual the Unix-form blacklist missed. It is a prefix only on
        // Windows; on Unix "C:foo" is an ordinary filename that stays in root,
        // so it is asserted where the escape can actually happen.
        #[cfg(windows)]
        {
            for rooted in ["C:secrets", "C:", r"C:\abs", r"\\server\share", r"a\b"] {
                assert!(
                    !is_path_atom(rooted),
                    "{rooted:?} is a drive/root/separator and must be refused on Windows"
                );
            }
        }
    }

    #[test]
    fn the_instance_id_is_the_certificate_san_suffix_and_nothing_else() {
        // The canonical form: a 26-char Crockford-base32 ULID, exactly as
        // `provision-instance-id.sh` / `pki/issue-client.sh` issue it.
        let ulid = "01K6Q6ESWMT48TPB9X4X4X4X4X";
        let certificate = pem(&certificate_der_with_san(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
            Some(&[&format!("urn:loam:instance:{ulid}")]),
        ));
        assert_eq!(
            certificate_instance_id(&certificate).expect("a SAN-bearing certificate reads"),
            ulid
        );

        // The SAN may also carry an agent URI; the instance URI is named, not
        // assumed, and the agent URI is never mistaken for it.
        let both = pem(&certificate_der_with_san(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
            Some(&[
                &format!("urn:loam:agent:{ulid}"),
                &format!("urn:loam:instance:{ulid}"),
            ]),
        ));
        assert_eq!(
            certificate_instance_id(&both).expect("the instance URI is selected"),
            ulid
        );
    }

    #[test]
    fn a_certificate_without_a_usable_san_refuses() {
        let unresolved = Err(reason::CREDENTIAL_REF_UNRESOLVED);
        // No SAN extension at all.
        let plain = pem(&certificate_der(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
        ));
        assert_eq!(certificate_instance_id(&plain), unresolved);
        // A SAN that is not the instance URI.
        let foreign = pem(&certificate_der_with_san(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
            Some(&["urn:loam:agent:01K6Q6ESWMT48TPB9X4X4X4X4X"]),
        ));
        assert_eq!(certificate_instance_id(&foreign), unresolved);
        // A SAN whose suffix fails instance-id validation.
        let bad_suffix = pem(&certificate_der_with_san(
            "Loam MQTT Test CA",
            Some("sam@example.test"),
            None,
            true,
            Some(&["urn:loam:instance:not-a-ulid"]),
        ));
        assert_eq!(certificate_instance_id(&bad_suffix), unresolved);
        // Not a certificate at all.
        assert_eq!(
            certificate_instance_id(
                b"-----BEGIN PRIVATE KEY-----\nR0hJ\n-----END PRIVATE KEY-----\n"
            ),
            unresolved
        );
    }

    #[test]
    fn the_instance_id_validator_accepts_only_the_canonical_ulid_form() {
        // The canonical 26-char Crockford-base32 ULID, upper and lower case.
        assert!(is_valid_instance_id("01K6Q6ESWMT48TPB9X4X4X4X4X"));
        assert!(is_valid_instance_id("01k6q6eswmt48tpb9x4x4x4x4x"));
        // The old 32-hex mint form is dead with the mint.
        assert!(!is_valid_instance_id("0123456789abcdef0123456789abcdef"));
        // Wrong length, and Crockford-excluded letters (I, L, O, U).
        assert!(!is_valid_instance_id("01K6Q6ESWMT48TPB9X4X4X4X4"));
        assert!(!is_valid_instance_id("01K6Q6ESWMT48TPB9X4X4X4X4XX"));
        assert!(!is_valid_instance_id("01K6Q6ESWMT48TPB9X4X4X4X4O"));
        assert!(!is_valid_instance_id(""));
    }

    #[test]
    fn the_identity_root_walks_three_rungs_in_order() {
        // The explicit per-resource override names the identity directory.
        assert_eq!(
            identity_root(Some("/explicit"), None, None, None, None, Some("/home/op"),).unwrap(),
            std::path::PathBuf::from("/explicit")
        );
        // Then the config-dir rungs.
        assert_eq!(
            identity_root(None, Some("/cfg"), None, None, None, Some("/home/op")).unwrap(),
            std::path::PathBuf::from("/cfg/federation/identity")
        );
        // The legacy global root is a fallback for when no config basis
        // resolves at all.
        assert_eq!(
            identity_root(None, None, Some(""), Some(""), Some("/loam-home"), None).unwrap(),
            std::path::PathBuf::from("/loam-home/federation/identity")
        );
        // Nothing at all is an unresolved identity, not a path from nothing.
        assert_eq!(
            identity_root(None, None, None, None, None, None),
            Err(reason::PROFILE_ABSENT)
        );
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
    fn an_absent_ca_ref_means_the_bundled_roots_and_a_present_one_means_the_pinned_file() {
        let directory = temp_dir("ca");

        // Absent: the bundled Mozilla roots. Both branches must produce
        // non-empty stores — the transport builds its TLS config from this store.
        let bundled = resolve_trust_anchors(None, None).expect("the bundle resolves");
        assert!(!bundled.is_empty(), "bundled store must not be empty");
        assert!(
            bundled.roots.len() > 100,
            "bundled store should have 100+ Mozilla roots"
        );

        // Present: a pinned `ca_ref` PEM file resolves to exactly the roots it
        // names — one here — never the bundle. The certificate must actually
        // parse into a root, so this rung uses a real self-signed cert.
        let pinned_path = directory.join("pinned.pem");
        std::fs::write(&pinned_path, REAL_ROOT_PEM).expect("pinned trust file is writable");
        let pinned = resolve_trust_anchors(Some(&pinned_path.to_string_lossy()), None)
            .expect("a resolvable ca_ref pins");
        assert_eq!(
            pinned.roots.len(),
            1,
            "the pinned store holds exactly its one named root"
        );
        assert_ne!(
            pinned.roots.len(),
            bundled.roots.len(),
            "the pinned branch is the file's roots, never the bundle"
        );

        // `SSL_CERT_FILE` is the no-`ca_ref` override: honored ahead of the
        // bundle, and — like the pinned rung — resolves to exactly the file's
        // roots, not the 100+ bundled ones.
        let override_path = directory.join("override.pem");
        std::fs::write(&override_path, REAL_ROOT_PEM).expect("override trust file is writable");
        let overridden = resolve_trust_anchors(None, Some(&override_path.to_string_lossy()))
            .expect("SSL_CERT_FILE is honored first");
        assert_eq!(
            overridden.roots.len(),
            1,
            "the override store holds exactly its one named root"
        );
        assert_ne!(
            overridden.roots.len(),
            bundled.roots.len(),
            "the override branch is the file's roots, never the bundle"
        );

        // A named CA that resolves to nothing is a refusal, never a silent
        // downgrade to the bundle — that downgrade would turn a pinning
        // failure into a quietly wider trust decision.
        assert!(matches!(
            resolve_trust_anchors(Some("/nonexistent/loam/ca.pem"), None),
            Err(reason::CA_UNRESOLVED)
        ));

        // A present-but-blank reference is malformed, not absent: reading it as
        // "no CA pinned" would turn a typo into a silently wider trust decision.
        assert!(matches!(
            resolve_trust_anchors(Some("   "), None),
            Err(reason::CA_UNRESOLVED)
        ));

        // An empty trust file is not a trust store: it would build a root store
        // that refuses every connection, which is a broken session rather than
        // a resolved one.
        let empty = directory.join("empty.pem");
        std::fs::write(&empty, "").expect("empty trust file is writable");
        assert!(matches!(
            resolve_trust_anchors(Some(&empty.to_string_lossy()), None),
            Err(reason::CA_UNRESOLVED)
        ));

        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn bundled_roots_resolve_to_a_non_empty_der_store_without_an_override() {
        let bundled = resolve_trust_anchors(None, None).expect("the bundle resolves");
        assert!(bundled.roots.len() > 100);
    }

    #[test]
    fn resolution_reads_the_identity_path_and_no_failure_carries_a_byte_of_it() {
        let directory = temp_dir("resolve");
        let identity = directory.join("identity");
        std::fs::create_dir_all(&identity).expect("identity directory is creatable");
        std::fs::write(identity.join("client.pem"), CERTIFICATE).expect("certificate is writable");
        std::fs::write(identity.join("key.pem"), KEY).expect("key is writable");

        let material =
            resolve_credentials(&identity, None, None).expect("an identity path produces material");
        assert!(!material.certificate.is_empty() && !material.key.is_empty());

        // The material is held, never described: `Debug` prints lengths so an
        // unwrapped error can never put a private key in a test log.
        let described = format!("{material:?}");
        assert!(
            !described.contains("R0hJ") && !described.contains("QUJD"),
            "{described}"
        );

        // Credentials are operator-private: the directory is 0700 and both PEM
        // files are 0600 after the read (Unix). A looser operator placement is
        // hardened the moment it is used.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&identity).unwrap().permissions().mode() & 0o777;
            let cert_mode = std::fs::metadata(identity.join("client.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let key_mode = std::fs::metadata(identity.join("key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700, "identity directory must be 0700");
            assert_eq!(cert_mode, 0o600, "client.pem must be 0600");
            assert_eq!(key_mode, 0o600, "key.pem must be 0600");
        }

        // Every failure names the input and nothing else.
        let empty = temp_dir("resolve-empty");
        let failure =
            resolve_credentials(&empty, None, None).expect_err("an empty identity path refuses");
        assert_eq!(
            failure,
            ProvisionFailure::Credentials(reason::IDENTITY_REQUIRED)
        );
        assert!(!format!("{failure:?}").contains("client.pem"));

        let _ = std::fs::remove_dir_all(directory);
        let _ = std::fs::remove_dir_all(empty);
    }
}
