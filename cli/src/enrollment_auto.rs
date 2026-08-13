//! Machine-side auto-enrollment (specs/federation-auto-enrollment.md):
//! the machine generates its own keypair + CSR on first `connect --token`,
//! POSTs `{password, csr}` to the broker-host signer over HTTPS, and stores
//! the returned certificate through the existing identity path with the
//! existing perms hardening. The org CA ("the broker's keypair") is untouched;
//! only the signing moment is automated.
//!
//! The CSR writer is the mirror of the certificate reader in
//! `provisioning.rs`: the same hand-rolled DER craft, the same OIDs, the same
//! subject shape (`CN=<email>/emailAddress=<email>/GN=<display_name>`) and the
//! same SAN URI (`urn:loam:instance:<ulid>`), so a certificate issued from one
//! of these CSRs round-trips through `certificate_subject` and
//! `certificate_instance_id` exactly as a broker-issued cert does.
//!
//! The HTTPS client is minimal HTTP/1.1 over the already-locked `rustls`
//! dependency (promoted to direct; it was already in the tree via rumqttc's
//! `use-rustls`). Encryption is ECDSA P-256 via the already-locked `aws-lc-rs`
//! (rustls's default crypto provider here), so this module adds no new crate.

use std::io::{Read, Write};
use std::net::TcpStream;

/// OID 2.5.4.3 `id-at-commonName`, as DER content bytes. Mirrors the reader.
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
/// OID 2.5.4.42 `id-at-givenName`, as DER content bytes. Mirrors the reader.
const OID_GIVEN_NAME: &[u8] = &[0x55, 0x04, 0x2a];
/// OID 1.2.840.113549.1.9.1 `emailAddress`, as DER content bytes. The identity
/// contract binds the email in this conventional slot alongside the CN.
const OID_EMAIL_ADDRESS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x01];
/// OID 2.5.29.17 `id-ce-subjectAltName`, as DER content bytes. Mirrors the
/// reader's SAN extension.
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
/// OID 1.2.840.113549.1.9.14 `pkcs-9-at-extensionRequest`: the CSR attribute
/// that carries the requested extensions (here, the SAN).
const OID_EXTENSION_REQUEST: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x0e];
/// OID 1.2.840.10045.4.3.2 `ecdsa-with-SHA256`: the CSR's signature algorithm.
const OID_ECDSA_WITH_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];

/// The `urn:loam:instance:` URI prefix the SAN must carry. Mirrors the reader.
const INSTANCE_SAN_PREFIX: &str = "urn:loam:instance:";

/// Why an auto-enrollment attempt failed. Typed per the spec: a failed token
/// is distinct from an unreachable signer, which is distinct from a response
/// that is not a certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentFailure {
    /// The signer verifies the password exactly and a bad one gets a 401.
    BadToken,
    /// The signer could not be reached (DNS, connect, TLS, or empty reply).
    SignerUnreachable,
    /// The signer replied 2xx but the body is not one parseable certificate.
    MalformedSignerResponse,
    /// The machine has no git identity to name the CSR subject with.
    GitIdentityRequired,
}

impl EnrollmentFailure {
    pub fn code(&self) -> &'static str {
        match self {
            EnrollmentFailure::BadToken => "bad-token",
            EnrollmentFailure::SignerUnreachable => "signer-unreachable",
            EnrollmentFailure::MalformedSignerResponse => "malformed-signer-response",
            EnrollmentFailure::GitIdentityRequired => "git-identity-required",
        }
    }
}

/// The signer URL the machine post to, derived from the broker host on the
/// conventional port the spec fixes. `https://<host>:8443/v1/enroll` unless
/// `LOAM_FEDERATION_SIGNER` names the endpoint explicitly.
pub fn signer_url(broker_host: &str) -> String {
    match std::env::var("LOAM_FEDERATION_SIGNER") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => format!("https://{broker_host}:8443/v1/enroll"),
    }
}

/// Generate a fresh instance id (`urn:loam:instance:` SAN suffix) with no
/// dependency beyond the already-locked `aws-lc-rs` RNG. The 26-character
/// Crockford-base32 ULID form `is_valid_instance_id` accepts.
pub fn generate_instance_id() -> Result<String, EnrollmentFailure> {
    use crate::sha256::Sha256;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut entropy = [0u8; 32];
    aws_lc_rs::rand::fill(&mut entropy).map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let mut hasher = Sha256::default();
    hasher.update(&entropy);
    Ok(hasher
        .finish()
        .bytes()
        .take(26)
        .map(|byte| ALPHABET[(byte as usize) % ALPHABET.len()] as char)
        .collect())
}

/// Generate the machine's own ECDSA P-256 keypair and a PKCS#10 CSR naming the
/// local git identity, carrying the instance-id SAN. Returns the key and the
/// CSR, both as PEM.
pub fn generate_keypair_and_csr(
    email: &str,
    display_name: &str,
    instance_id: &str,
) -> Result<(Vec<u8>, Vec<u8>), EnrollmentFailure> {
    if email.is_empty()
        || email.len() > 1024
        || !email.contains('@')
        || !display_name.chars().all(|c| c != '\n' && c != '\0')
    {
        return Err(EnrollmentFailure::GitIdentityRequired);
    }
    let rng = aws_lc_rs::rand::SystemRandom::new();

    let key_document = aws_lc_rs::signature::EcdsaKeyPair::generate_pkcs8(
        &aws_lc_rs::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        &rng,
    )
    .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let keypair = aws_lc_rs::signature::EcdsaKeyPair::from_pkcs8(
        &aws_lc_rs::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        key_document.as_ref(),
    )
    .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let spki = der_as_der_of_public_key(&keypair)?;

    let csr = build_csr(&keypair, email, display_name, instance_id, &spki, &rng)?;

    // The generated PKCS#8 document IS the machine's private key: armoring the
    // document we generated (rather than re-serializing the parsed keypair)
    // keeps exactly the bytes that paired with the signed CSR's public key.
    let key_pem = pem_armor("PRIVATE KEY", key_document.as_ref());
    let csr_pem = pem_armor("CERTIFICATE REQUEST", &csr);
    Ok((key_pem, csr_pem))
}

fn der_as_der_of_public_key(
    keypair: &aws_lc_rs::signature::EcdsaKeyPair,
) -> Result<Vec<u8>, EnrollmentFailure> {
    use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
    use aws_lc_rs::signature::KeyPair;
    keypair
        .public_key()
        .as_der()
        .map(|der: PublicKeyX509Der<'_>| der.as_ref().to_vec())
        .map_err(|_| EnrollmentFailure::SignerUnreachable)
}

/// Build a PKCS#10 CertificationRequest carrying the SAN as an
/// `extensionRequest` attribute, signed by the machine's own key.
fn build_csr(
    keypair: &aws_lc_rs::signature::EcdsaKeyPair,
    email: &str,
    display_name: &str,
    instance_id: &str,
    spki: &[u8],
    rng: &aws_lc_rs::rand::SystemRandom,
) -> Result<Vec<u8>, EnrollmentFailure> {
    // subjectAltName GeneralNames: one URI GeneralName (`[6]` IA5String).
    let san_uri = format!("{INSTANCE_SAN_PREFIX}{instance_id}");
    let san_general_names = der_sequence(&der_implicit(6, san_uri.as_bytes()));
    // The SAN extension's value is an OCTET STRING wrapping those GeneralNames.
    let san_extension_value = der_octet_string(&san_general_names);
    let san_extension = der_sequence(&concat2(
        &der_oid(OID_SUBJECT_ALT_NAME),
        &san_extension_value,
    ));
    // extensionRequest attribute value: a SET whose one element is that
    // extension, and the attribute's value set wraps it.
    let extension_request = der_sequence(&concat2(
        &der_oid(OID_EXTENSION_REQUEST),
        &der_set(&der_sequence(&san_extension)),
    ));
    // CertificationRequestInfo attributes `[0] IMPLICIT Attributes`: IMPLICIT
    // tagging substitutes the SET-of-Attribute wrapper with the `[0]` tag, so
    // the content is the Attribute SEQUENCE directly (one attribute here), not
    // a sequence wrapping it. The tag is constructed (`0xa0`).
    let attributes = der_implicit_constructed(0, &extension_request);

    // Subject Name: three single-value RDNs, the identity contract's shape.
    let cn_rdn = der_set(&der_sequence(&concat2(
        &der_oid(OID_COMMON_NAME),
        &der_utf8(email.as_bytes()),
    )));
    let email_rdn = der_set(&der_sequence(&concat2(
        &der_oid(OID_EMAIL_ADDRESS),
        &der_ia5(email.as_bytes()),
    )));
    let gn_rdn = der_set(&der_sequence(&concat2(
        &der_oid(OID_GIVEN_NAME),
        &der_utf8(display_name.as_bytes()),
    )));
    let subject = der_sequence(&concat3(&cn_rdn, &email_rdn, &gn_rdn));

    let algorithm = der_sequence(&der_oid(OID_ECDSA_WITH_SHA256));
    let certification_request_info = der_sequence(&concat(&[
        &der_integer(&[0]), // version 0
        &subject,
        // `spki` is already the complete SubjectPublicKeyInfo SEQUENCE from the
        // generated key; wrapping it again would nest a SEQUENCE inside the
        // subjectPKInfo slot and openssl would refuse the whole CSR.
        spki,
        &attributes,
    ]));

    let signature = keypair
        .sign(rng, &certification_request_info)
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;

    let signed = der_sequence(&concat(&[
        &certification_request_info,
        &algorithm,
        &der_bit_string(signature.as_ref()),
    ]));
    Ok(signed)
}

// ---------------------------------------------------------------------------
// Hand-rolled DER writer — the mirror of the reader in provisioning.rs. Every
// length is bounded by construction (content is already materialized), and the
// encodings emitted here are the ones a real openssl CSR uses, verified in the
// ignored LOAM_MQTT_TEST tier against `openssl req -verify`.
// ---------------------------------------------------------------------------

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

fn der_sequence(content: &[u8]) -> Vec<u8> {
    der(0x30, content)
}

fn der_set(content: &[u8]) -> Vec<u8> {
    der(0x31, content)
}

fn der_oid(content: &[u8]) -> Vec<u8> {
    der(0x06, content)
}

fn der_utf8(content: &[u8]) -> Vec<u8> {
    der(0x0c, content)
}

fn der_ia5(content: &[u8]) -> Vec<u8> {
    der(0x16, content)
}

fn der_octet_string(content: &[u8]) -> Vec<u8> {
    der(0x04, content)
}

fn der_integer(content: &[u8]) -> Vec<u8> {
    der(0x02, content)
}

fn der_bit_string(content: &[u8]) -> Vec<u8> {
    let mut body = vec![0x00]; // unused bits
    body.extend_from_slice(content);
    der(0x03, &body)
}

/// A context tag (`[n] IMPLICIT`). The SAN URI GeneralName is a primitive
/// tagged value (`0x86` = `[6]`), so its tag keeps bit 0x20 clear.
fn der_implicit(tag: u8, content: &[u8]) -> Vec<u8> {
    der(0x80 | tag, content)
}

/// A CONSTRUCTED context tag (`[n] IMPLICIT`). The CSR's `[0] IMPLICIT
/// Attributes` wraps constructed content, so its tag sets bit 0x20 (`0xa0`).
fn der_implicit_constructed(tag: u8, content: &[u8]) -> Vec<u8> {
    der(0xa0 | tag, content)
}

fn concat2(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    out
}

fn concat3(a: &[u8], b: &[u8], c: &[u8]) -> Vec<u8> {
    concat(&[a, b, c])
}

fn concat(pieces: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pieces.iter().map(|p| p.len()).sum());
    for piece in pieces {
        out.extend_from_slice(piece);
    }
    out
}

/// PEM armor the runtime's own PEM reader accepts.
fn pem_armor(label: &str, der: &[u8]) -> Vec<u8> {
    let encoded = crate::provisioning::base64_encode(der);
    let mut out = Vec::new();
    out.extend_from_slice(format!("-----BEGIN {label}-----\n").as_bytes());
    for chunk in encoded.chunks(64) {
        out.extend_from_slice(chunk);
        out.push(b'\n');
    }
    out.extend_from_slice(format!("-----END {label}-----\n").as_bytes());
    out
}

/// POST `{password, csr}` to the signer and return the signed certificate PEM
/// on success. A 401 is a [`EnrollmentFailure::BadToken`]; any transport
/// failure is [`EnrollmentFailure::SignerUnreachable`]; a 2xx whose body is
/// not a certificate is [`EnrollmentFailure::MalformedSignerResponse`].
///
/// The request is HTTP/1.1 over TLS with the server's certificate verified
/// against `ca_certificate` (the same trust file the broker session uses).
pub fn request_signed_certificate(
    url: &str,
    password: &str,
    csr_pem: &[u8],
    ca_certificate: &[u8],
) -> Result<Vec<u8>, EnrollmentFailure> {
    let (host, port, path) = parse_url(url)?;
    let mut tcp = TcpStream::connect((host.as_str(), port))
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    tcp.set_nodelay(true).ok();

    let mut roots = rustls::RootCertStore::empty();
    let der_certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        crate::provisioning::pem_certificate_ders(ca_certificate)
            .into_iter()
            .map(rustls::pki_types::CertificateDer::from)
            .collect();
    roots.add_parsable_certificates(der_certs);
    if roots.is_empty() {
        return Err(EnrollmentFailure::SignerUnreachable);
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let mut conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let mut stream = rustls::Stream::new(&mut conn, &mut tcp); // needs tcp: &mut

    let request_body = format!(
        "{{\"password\":{},\"csr\":{}}}",
        json_string(password),
        json_string(&String::from_utf8_lossy(csr_pem))
    );
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{request_body}",
        request_body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    stream
        .flush()
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;

    // Response: `HTTP/1.1 <status> ...\r\n<header...>\r\n\r\n<body>`.
    let (status_line, response_body) = match response.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(header_end) => {
            let (head, response_body) = response.split_at(header_end);
            let response_body = &response_body[4..];
            (head, response_body)
        }
        None => return Err(EnrollmentFailure::MalformedSignerResponse),
    };
    // Status line: `HTTP/1.1 <code> <reason>`.
    let status = match status_line
        .splitn(3, |b| *b == b' ')
        .nth(1)
        .and_then(|code| std::str::from_utf8(code).ok()?.parse::<u16>().ok())
    {
        Some(status) => status,
        None => return Err(EnrollmentFailure::MalformedSignerResponse),
    };
    if status == 401 {
        return Err(EnrollmentFailure::BadToken);
    }
    if status == 429 {
        return Err(EnrollmentFailure::BadToken); // rate limit: same retry posture
    }
    if !(200..300).contains(&status) {
        return Err(EnrollmentFailure::MalformedSignerResponse);
    }

    // The signer returns the certificate PEM verbatim; a body that is not one
    // certificate is a malformed response even if the status was 2xx.
    if crate::provisioning::certificate_subject(response_body).is_err() {
        return Err(EnrollmentFailure::MalformedSignerResponse);
    }
    Ok(response_body.to_vec())
}

/// A tiny `https://host:port/path` parser tailored to the signer URL; anything
/// else is unreachable-by-construction. Explicit port wins; a missing port
/// defaults to 8443 per the spec.
fn parse_url(url: &str) -> Result<(String, u16, String), EnrollmentFailure> {
    let rest = url
        .strip_prefix("https://")
        .ok_or(EnrollmentFailure::SignerUnreachable)?;
    let (authority, path) = match rest.split_once('/') {
        Some((auth, path)) => (auth, format!("/{path}")),
        None => (rest, "/v1/enroll".to_owned()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            (host.to_owned(), port.parse::<u16>().unwrap_or(8443))
        }
        _ => (authority.to_owned(), 8443),
    };
    if host.is_empty() {
        return Err(EnrollmentFailure::SignerUnreachable);
    }
    Ok((host, port, path))
}

/// A minimal JSON string literal for a value with no control bytes (passwords
/// from `openssl rand -base64` and PEM are both safe); backslash and quote are
/// still escaped defensively.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn der(tag: u8, body: &[u8]) -> Vec<u8> {
        super::der(tag, body)
    }

    #[test]
    fn round_trips_its_own_ders() {
        let seq = der(0x30, &[1, 2, 3]);
        assert_eq!(&seq, &[0x30, 0x03, 1, 2, 3]);
        // 200 < 0x100: a one-byte long-form length header (0x81, 200).
        let long = vec![7u8; 200];
        let seq = der(0x30, &long);
        assert_eq!(&seq[0..3], &[0x30, 0x81, 200]);
        assert_eq!(&seq[3..], &long[..]);
        // 300 >= 0x100: a two-byte long-form length header (0x82, hi, lo).
        let big = vec![7u8; 300];
        let seq = der(0x30, &big);
        assert_eq!(&seq[0..4], &[0x30, 0x82, 0x01, 0x2c]);
        assert_eq!(&seq[4..], &big[..]);
    }

    #[test]
    fn instance_id_is_a_valid_ulid() {
        let id = generate_instance_id().expect("RNG is available");
        assert_eq!(id.len(), 26);
        assert!(crate::provisioning::is_valid_instance_id(&id));
    }

    #[test]
    fn generate_keypair_and_csr_requires_a_real_email() {
        let err = generate_keypair_and_csr("", "Ada", "01XXXX").unwrap_err();
        assert_eq!(err, EnrollmentFailure::GitIdentityRequired);
    }

    #[test]
    fn json_string_escapes_quotes_and_backslashes() {
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("plain"), "\"plain\"");
    }
}
