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
    let mut candidates: Vec<String> = Vec::new();
    if let Some(path) = override_path.map(str::trim).filter(|v| !v.is_empty()) {
        candidates.push(path.to_owned());
    }
    candidates.extend(SYSTEM_TRUST_BUNDLES.iter().map(|path| (*path).to_owned()));
    for candidate in candidates {
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
    match ca_ref.map(str::trim).filter(|value| !value.is_empty()) {
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
        let path = std::env::temp_dir().join(format!("loam-provisioning-{label}-{unique}"));
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
        let backend = Backend::Custom(script.to_string_lossy().into_owned());
        (directory, backend)
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

        // No bundle anywhere is also a refusal rather than an empty store.
        let missing = directory.join("absent.pem");
        assert!(
            system_trust_anchors(Some(&missing.to_string_lossy())).is_err()
                || std::path::Path::new(SYSTEM_TRUST_BUNDLES[0]).exists()
        );

        let _ = std::fs::remove_dir_all(directory);
        let _ = std::fs::remove_dir_all(failing_dir);
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
