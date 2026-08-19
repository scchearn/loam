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
//! Rustls stack). Encryption is ECDSA P-256 via the already-locked `ring`, so
//! this module adds no new crate.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

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
    /// The signer was reachable but the exchange exceeded its deadline: the
    /// connect, the TLS handshake, or a read/write stalled. Distinct from
    /// [`EnrollmentFailure::SignerUnreachable`] because the operator response
    /// differs — a wedged or overloaded signer is not a missing one — and
    /// because the alternative is the silent hang this class of bug keeps
    /// producing. `stage` names where the deadline expired.
    SignerTimeout { stage: &'static str },
    /// The signer replied 2xx but the body is not one parseable certificate.
    MalformedSignerResponse,
    /// The machine has no git identity to name the CSR subject with.
    GitIdentityRequired,
    /// Local cryptography failed before the signer was contacted.
    LocalCrypto {
        operation: &'static str,
        detail: String,
    },
    /// The trust anchors for verifying the *signer's* certificate could not be
    /// built. Nothing has been dialled: this is a local file or environment
    /// problem, and reporting it as an unreachable signer sent operators to
    /// look at DNS, firewalls, and the broker host instead of at
    /// `SSL_CERT_FILE`. `source` names the rung that was in play.
    TrustAnchorsUnresolved {
        source: &'static str,
        reason: &'static str,
    },
    /// The signer URL is not one this client can dial — not HTTPS, no host, or
    /// a host TLS cannot name. A typo in `LOAM_FEDERATION_SIGNER` or in the
    /// broker endpoint, never a network condition.
    SignerUrlInvalid { detail: &'static str },
    /// The local TLS client could not be built, or a socket option was
    /// refused. A platform or build problem on *this* machine.
    TlsSetupFailed {
        operation: &'static str,
        detail: String,
    },
    /// The machine's identity directory could not be resolved, or the issued
    /// bundle could not be written to it. The certificate may already have
    /// been signed: the signer did its job and the local disk did not.
    IdentityStoreFailed {
        operation: &'static str,
        detail: String,
    },
}

impl EnrollmentFailure {
    pub fn code(&self) -> &'static str {
        match self {
            EnrollmentFailure::BadToken => "bad-token",
            EnrollmentFailure::SignerUnreachable => "signer-unreachable",
            EnrollmentFailure::SignerTimeout { .. } => "signer-timeout",
            EnrollmentFailure::MalformedSignerResponse => "malformed-signer-response",
            EnrollmentFailure::GitIdentityRequired => "git-identity-required",
            EnrollmentFailure::LocalCrypto { .. } => "local-crypto-failure",
            EnrollmentFailure::TrustAnchorsUnresolved { .. } => "trust-anchors-unresolved",
            EnrollmentFailure::SignerUrlInvalid { .. } => "signer-url-invalid",
            EnrollmentFailure::TlsSetupFailed { .. } => "tls-setup-failed",
            EnrollmentFailure::IdentityStoreFailed { .. } => "identity-store-failed",
        }
    }

    /// The one extra fact behind a code: which local step failed and what it
    /// said. Printed in release builds, not only under `debug_assertions` —
    /// the whole point is diagnosing the shipped binary an operator ran, which
    /// is exactly the situation #94 was reported from. Every value here is one
    /// of this module's own strings or a `Debug` of a crypto/IO error; no
    /// token, key, or certificate byte reaches it.
    pub fn detail(&self) -> Option<(&'static str, &str)> {
        match self {
            EnrollmentFailure::LocalCrypto { operation, detail }
            | EnrollmentFailure::TlsSetupFailed { operation, detail }
            | EnrollmentFailure::IdentityStoreFailed { operation, detail } => {
                Some((*operation, detail))
            }
            EnrollmentFailure::TrustAnchorsUnresolved { source, reason } => Some((*source, reason)),
            EnrollmentFailure::SignerUrlInvalid { detail } => Some(("signer-url", detail)),
            EnrollmentFailure::SignerTimeout { stage } => Some(("timeout-stage", stage)),
            EnrollmentFailure::BadToken
            | EnrollmentFailure::SignerUnreachable
            | EnrollmentFailure::MalformedSignerResponse
            | EnrollmentFailure::GitIdentityRequired => None,
        }
    }
}

fn local_crypto_failure(operation: &'static str, error: impl std::fmt::Debug) -> EnrollmentFailure {
    EnrollmentFailure::LocalCrypto {
        operation,
        detail: format!("{error:?}"),
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
/// dependency beyond the already-locked `ring` RNG. The 26-character
/// Crockford-base32 ULID form `is_valid_instance_id` accepts.
pub fn generate_instance_id() -> Result<String, EnrollmentFailure> {
    use crate::sha256::Sha256;
    use ring::rand::SecureRandom;
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut entropy = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut entropy)
        .map_err(|error| local_crypto_failure("instance-id-rng", error))?;
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
    use ring::signature::KeyPair;

    if email.is_empty()
        || email.len() > 1024
        || !email.contains('@')
        || !display_name.chars().all(|c| c != '\n' && c != '\0')
    {
        return Err(EnrollmentFailure::GitIdentityRequired);
    }
    let rng = ring::rand::SystemRandom::new();

    let key_document = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        &rng,
    )
    .map_err(|error| local_crypto_failure("csr-keygen", error))?;
    let keypair = ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        key_document.as_ref(),
        &rng,
    )
    .map_err(|error| local_crypto_failure("csr-key-parse", error))?;
    let spki = der_subject_public_key(keypair.public_key().as_ref());

    let csr = build_csr(&keypair, email, display_name, instance_id, &spki, &rng)?;

    // The generated PKCS#8 document IS the machine's private key: armoring the
    // document we generated (rather than re-serializing the parsed keypair)
    // keeps exactly the bytes that paired with the signed CSR's public key.
    let key_pem = pem_armor("PRIVATE KEY", key_document.as_ref());
    let csr_pem = pem_armor("CERTIFICATE REQUEST", &csr);
    Ok((key_pem, csr_pem))
}

fn der_subject_public_key(public_key: &[u8]) -> Vec<u8> {
    // ring exposes the SEC1 point; PKCS#10 requires the complete SPKI wrapper.
    const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    const OID_PRIME256V1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    let algorithm = der_sequence(&concat2(
        &der_oid(OID_EC_PUBLIC_KEY),
        &der_oid(OID_PRIME256V1),
    ));
    der_sequence(&concat2(&algorithm, &der_bit_string(public_key)))
}

/// Build a PKCS#10 CertificationRequest carrying the SAN as an
/// `extensionRequest` attribute, signed by the machine's own key.
fn build_csr(
    keypair: &ring::signature::EcdsaKeyPair,
    email: &str,
    display_name: &str,
    instance_id: &str,
    spki: &[u8],
    rng: &ring::rand::SystemRandom,
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
        .map_err(|error| local_crypto_failure("csr-sign", error))?;

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

/// Per-operation network deadline for the enrollment exchange: the TCP
/// connect, and every read and write once connected. Ten seconds by default;
/// `LOAM_ENROLL_TIMEOUT_SECONDS` (1..=300) raises it for a genuinely slow link
/// and lets the regression tests prove the bound without waiting for it.
///
/// Without these the exchange had no bound at all: a signer that accepts the
/// connection and then stalls — exactly the production wedge in #93 — hung
/// enrollment forever with no output.
fn enroll_timeout() -> Duration {
    const DEFAULT_SECONDS: u64 = 10;
    let seconds = std::env::var("LOAM_ENROLL_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| (1..=300).contains(seconds))
        .unwrap_or(DEFAULT_SECONDS);
    Duration::from_secs(seconds)
}

/// A read/write deadline alone still admits a slow drip: each successful byte
/// rearms it. This ceiling bounds the whole exchange, so the worst case is a
/// small multiple of the per-operation deadline rather than the header and
/// body size caps divided by one byte per timeout.
fn exchange_deadline() -> Instant {
    Instant::now() + enroll_timeout() * 3
}

fn is_timeout(error: &std::io::Error) -> bool {
    // A socket read/write deadline surfaces as WouldBlock on Unix and TimedOut
    // on Windows; `connect_timeout` reports TimedOut everywhere.
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// Map a transport error to a refusal that says which of the two happened: a
/// signer that is not there, or a signer that is there and not answering.
fn transport_failure(stage: &'static str, error: &std::io::Error) -> EnrollmentFailure {
    if is_timeout(error) {
        EnrollmentFailure::SignerTimeout { stage }
    } else {
        EnrollmentFailure::SignerUnreachable
    }
}

/// Connect to the signer, trying every resolved address, under both the
/// per-attempt timeout and the whole-exchange `deadline`. Resolution failure
/// is genuinely "no such signer", so it stays `SignerUnreachable`; running out
/// of time is a timeout.
///
/// The deadline matters here and not only later: a host with an A and a AAAA
/// record on a black-holed route costs two full per-attempt timeouts, and a
/// wide round-robin record costs one each. Bounding only the reads would have
/// left the connect phase proportional to how many addresses DNS returned,
/// which is the hang this exists to remove. Each attempt is clamped to what is
/// left, so the phase can never overrun the ceiling, and a slow-but-real
/// connect cannot eat the whole budget before the exchange starts either.
///
/// `getaddrinfo` itself has no deadline in `std`, so the resolver's own is the
/// only bound available for that step. That is a smaller exposure than an
/// unbounded connect: a black-holed route hangs the connect indefinitely,
/// whereas a resolver that never answers is already bounded by its own
/// configuration.
fn connect_to_signer(
    host: &str,
    port: u16,
    deadline: Instant,
) -> Result<TcpStream, EnrollmentFailure> {
    use std::net::ToSocketAddrs;

    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| EnrollmentFailure::SignerUnreachable)?;
    let timeout = enroll_timeout();
    let mut timed_out = false;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        match TcpStream::connect_timeout(&address, timeout.min(remaining)) {
            Ok(stream) => return Ok(stream),
            Err(error) if is_timeout(&error) => timed_out = true,
            Err(_) => {}
        }
    }
    Err(if timed_out {
        EnrollmentFailure::SignerTimeout { stage: "connect" }
    } else {
        EnrollmentFailure::SignerUnreachable
    })
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
    roots: &rustls::RootCertStore,
) -> Result<Vec<u8>, EnrollmentFailure> {
    let (host, port, path) = parse_url(url)?;
    let deadline = exchange_deadline();
    let mut tcp = connect_to_signer(&host, port, deadline)?;
    tcp.set_nodelay(true).ok();
    // Armed before the TLS handshake, which `rustls::Stream` performs lazily
    // on the first write, so the handshake is bounded too.
    let timeout = enroll_timeout();
    tcp.set_read_timeout(Some(timeout))
        .and_then(|()| tcp.set_write_timeout(Some(timeout)))
        .map_err(|error| EnrollmentFailure::TlsSetupFailed {
            operation: "socket-deadlines",
            detail: format!("{error}"),
        })?;

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots.clone())
        .with_no_client_auth();

    let server_name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| {
        EnrollmentFailure::SignerUrlInvalid {
            detail: "host-not-a-tls-server-name",
        }
    })?;
    let mut conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|error| EnrollmentFailure::TlsSetupFailed {
            operation: "tls-client",
            detail: format!("{error}"),
        })?;
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
    // The TLS handshake happens here, on the first write, so a signer that
    // accepts the connection and never completes the handshake fails as a
    // `request` timeout rather than hanging. The write timeout is rearmed by
    // every partial write exactly as the read timeout is by every byte, so the
    // ceiling is checked around this too — a peer that accepts the request one
    // byte at a time is the same shape of stall as one that answers that way.
    let write_result = stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush());
    if let Err(error) = write_result {
        return Err(transport_failure("request", &error));
    }
    if Instant::now() >= deadline {
        return Err(EnrollmentFailure::SignerTimeout { stage: "request" });
    }

    let mut response = Vec::new();
    // Read until the header/body separator is present. The body is read once we
    // know its length; the connection may then close without a clean TLS
    // close_notify (common for simple Python/http servers), which after a
    // complete body is not an error.
    let header_end = loop {
        if let Some(header_end) = response.windows(4).position(|w| w == b"\r\n\r\n") {
            break header_end;
        }
        if response.len() > 1 << 16 {
            return Err(EnrollmentFailure::MalformedSignerResponse);
        }
        if Instant::now() >= deadline {
            return Err(EnrollmentFailure::SignerTimeout {
                stage: "response-headers",
            });
        }
        let mut chunk = [0u8; 2048];
        let read = match stream.read(&mut chunk) {
            Ok(0) => break response.len(), // EOF before any separator
            Ok(read) => read,
            Err(error) => return Err(transport_failure("response-headers", &error)),
        };
        response.extend_from_slice(&chunk[..read]);
    };

    // Response: `HTTP/1.1 <status> ...\r\n<header...>\r\n\r\n<body>`.
    let head = &response[..header_end];
    let body_start = header_end + 4;
    // Status line: `HTTP/1.1 <code> <reason>`.
    let status = match head
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

    // Content-Length tells us exactly how many body bytes to expect; read them
    // (and only them) so a later abrupt close is not mistaken for an error.
    let content_length = head
        .split(|b| *b == b'\n')
        .find_map(|line| {
            let line = std::str::from_utf8(line).ok()?;
            line.rsplit_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("Content-Length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .ok_or(EnrollmentFailure::MalformedSignerResponse)?;
    if content_length > 1 << 20 {
        return Err(EnrollmentFailure::MalformedSignerResponse);
    }
    let mut response_body: Vec<u8> = response[body_start..].to_vec();
    while response_body.len() < content_length {
        if Instant::now() >= deadline {
            return Err(EnrollmentFailure::SignerTimeout {
                stage: "response-body",
            });
        }
        let mut chunk = [0u8; 2048];
        let read = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return Err(transport_failure("response-body", &error)),
        };
        response_body.extend_from_slice(&chunk[..read]);
    }
    if response_body.len() != content_length {
        return Err(EnrollmentFailure::MalformedSignerResponse);
    }

    // The signer returns the certificate PEM verbatim; a body that is not one
    // certificate is a malformed response even if the status was 2xx.
    if crate::provisioning::certificate_subject(&response_body).is_err() {
        return Err(EnrollmentFailure::MalformedSignerResponse);
    }
    Ok(response_body)
}

/// A tiny `https://host:port/path` parser tailored to the signer URL; anything
/// else is unreachable-by-construction. Explicit port wins; a missing port
/// defaults to 8443 per the spec.
fn parse_url(url: &str) -> Result<(String, u16, String), EnrollmentFailure> {
    let rest = url
        .strip_prefix("https://")
        .ok_or(EnrollmentFailure::SignerUrlInvalid {
            detail: "not-https",
        })?;
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
        return Err(EnrollmentFailure::SignerUrlInvalid {
            detail: "empty-host",
        });
    }
    Ok((host, port, path))
}

/// A minimal JSON string literal. Control bytes are escaped as JSON escapes
/// (PEM bodies carry `\n`, which the signer must receive literally), and the
/// quote/backslash cases are escaped too.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
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
    fn local_crypto_failure_has_a_distinct_code_and_detail() {
        let failure = local_crypto_failure("csr-sign", ring::error::Unspecified);
        assert_eq!(failure.code(), "local-crypto-failure");
        assert_eq!(failure.detail(), Some(("csr-sign", "Unspecified")));
    }

    /// #94: the pre-network failures used to share `signer-unreachable` with
    /// the network ones, so a local file or environment problem sent the
    /// operator to look at the broker host. Each must be its own code, and
    /// each must carry the fact that names the fix.
    #[test]
    fn every_local_failure_has_its_own_code_and_says_which_step_failed() {
        let failures = [
            EnrollmentFailure::TrustAnchorsUnresolved {
                source: "ssl-cert-file",
                reason: "ca-unresolved",
            },
            EnrollmentFailure::SignerUrlInvalid {
                detail: "not-https",
            },
            EnrollmentFailure::TlsSetupFailed {
                operation: "tls-client",
                detail: "no provider".to_owned(),
            },
            EnrollmentFailure::IdentityStoreFailed {
                operation: "store-bundle",
                detail: "identity-required".to_owned(),
            },
            local_crypto_failure("csr-sign", ring::error::Unspecified),
            EnrollmentFailure::SignerTimeout { stage: "connect" },
        ];
        let mut codes: Vec<&str> = failures.iter().map(EnrollmentFailure::code).collect();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(
            codes.len(),
            total,
            "two local failures sharing a code is the #94 defect itself"
        );
        for failure in &failures {
            assert_ne!(
                failure.code(),
                "signer-unreachable",
                "a local failure must never claim the signer was unreachable: {failure:?}"
            );
            assert!(
                failure.detail().is_some(),
                "a code with no detail is the dead end #94 reported: {failure:?}"
            );
        }
        // The network refusals stay as they are; they have nothing to add.
        assert_eq!(EnrollmentFailure::SignerUnreachable.detail(), None);
        assert_eq!(EnrollmentFailure::BadToken.detail(), None);
    }

    /// #106 review: the exchange ceiling has to bound the connect phase too.
    /// Without the clamp each resolved address got a full per-attempt timeout
    /// before the ceiling was consulted at all, so a host with an A and a AAAA
    /// record on a black-holed route cost two of them, and a wide round-robin
    /// record one each — the bound was proportional to what DNS returned.
    ///
    /// 192.0.2.1 is TEST-NET-1 (RFC 5737): reserved for documentation and
    /// routed nowhere, so a connect to it stalls rather than being refused. A
    /// network that does refuse it quickly makes this pass trivially rather
    /// than flakily.
    #[test]
    fn the_connect_phase_cannot_outrun_the_exchange_deadline() {
        // A deadline with nothing left must stop the phase, not open a fresh
        // full-length attempt.
        let started = Instant::now();
        let failure = connect_to_signer("192.0.2.1", 8443, Instant::now())
            .expect_err("a black-holed address cannot connect");
        assert_eq!(
            failure,
            EnrollmentFailure::SignerTimeout { stage: "connect" }
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an exhausted deadline still spent {:?}",
            started.elapsed()
        );

        // And a small remaining budget is honoured instead of being replaced
        // by the full per-attempt timeout.
        let started = Instant::now();
        let _ = connect_to_signer("192.0.2.1", 8443, Instant::now() + Duration::from_secs(1));
        assert!(
            started.elapsed() < enroll_timeout(),
            "a 1s budget spent {:?}; the attempt was not clamped to it",
            started.elapsed()
        );
    }

    #[test]
    fn json_string_escapes_quotes_newlines_and_backslashes() {
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("plain"), "\"plain\"");
        // PEM bodies carry newlines, which must round-trip as \n escapes, not
        // be scrubbed into a replacement character.
        assert_eq!(json_string("ic\nYQ=="), "\"ic\\nYQ==\"");
    }
}
