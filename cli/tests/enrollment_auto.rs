//! Auto-enrollment integration tests, validated against REAL openssl output.
//!
//! The DER lesson (specs/federation-auto-enrollment.md audit): synthetic DER
//! fixtures diverge from what openssl actually emits, so this tier signs the
//! runtime's own CSR with the deployment's real signing path (`openssl ca` +
//! `copy_extensions = copy`) and then reads the issued cert back with the
//! runtime's own `certificate_subject` / `certificate_instance_id` readers.
//! If the CSR writer diverges from real openssl expectations, exactly one of
//! these assertions fails — and it names the true source (openssl), not a
//! self-consistent mirror.
//!
//! The HTTP tier runs the deployment's signer contract as a subprocess
//! (`support/enroll_signer.py`, a Python 3 stdlib HTTPS endpoint shelling to
//! the same `openssl ca`) and drives `request_signed_certificate` through the
//! real network: happy path, wrong token, unreachable signer.
//!
//! Gated on `LOAM_MQTT_TEST=1` exactly like the real-broker tier: it needs a
//! real `openssl` (and `python3`) installation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Build the DER-testing fixture: a throwaway self-signed CA plus the CA
/// config the deployment uses (`copy_extensions = copy`), so the CSR's
/// claimed SAN is copied verbatim into the signed cert the way the real
/// signer does.
fn provision_ca(root: &Path) {
    fs::create_dir_all(root.join("newcerts")).unwrap();
    fs::write(root.join("index.txt"), "").unwrap();
    fs::write(root.join("index.txt.attr"), "unique_subject = no\n").unwrap();
    fs::write(root.join("serial"), "1000\n").unwrap();
    fs::write(
        root.join("openssl.cnf"),
        "[ ca ]\n\
         default_ca = CA_default\n\
         [ CA_default ]\n\
         dir = .\n\
         database = ./index.txt\n\
         new_certs_dir = ./newcerts\n\
         certificate = ./ca.crt\n\
         private_key = ./ca.key\n\
         serial = ./serial\n\
         default_md = sha256\n\
         default_days = 30\n\
         policy = policy_anything\n\
         copy_extensions = copy\n\
         [ policy_anything ]\n\
         commonName = supplied\n\
         emailAddress = optional\n\
         givenName = optional\n\
         [ v3_client ]\n\
         basicConstraints = CA:FALSE\n\
         keyUsage = critical, digitalSignature\n\
         extendedKeyUsage = clientAuth\n",
    )
    .unwrap();
    openssl(
        root,
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
            "-sha256",
            "-days",
            "2",
            "-subj",
            "/CN=Auto-Enrollment Test CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
        ],
    );
    // A self-signed server cert for the fixture's HTTPS listener. Real
    // deployments use the broker host's Let's Encrypt cert; the trust file the
    // client verifies against is this same CA.
    fs::write(
        root.join("server.ext"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    openssl(
        root,
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-sha256",
            "-subj",
            "/CN=localhost",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
        ],
    );
    openssl(
        root,
        &[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "2",
            "-sha256",
            "-extfile",
            "server.ext",
            "-out",
            "server.crt",
        ],
    );
    fs::write(root.join("password"), "shared-enroll-secret\n").unwrap();
    set_private_permissions(root.join("password"));
}

/// A test-running HTTPS signer: spawns the standard-library Python endpoint
/// (`support/enroll_signer.py`) pointed at this fixture's CA + password, on a
/// freshly reserved local port.
struct SignerFixture {
    root: PathBuf,
    port: u16,
    child: Option<Child>,
}

impl SignerFixture {
    fn start(root: &Path) -> Self {
        let port = reserve_port();
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/enroll_signer.py");
        let log = root.join("signer.log");
        let child = Command::new("python3")
            .arg(&script)
            .arg("--port")
            .arg(port.to_string())
            .env("ENROLL_PKI_DIR", root)
            .env("ENROLL_PASSWORD_FILE", root.join("password"))
            .env("ENROLL_CERT_FILE", root.join("server.crt"))
            .env("ENROLL_KEY_FILE", root.join("server.key"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                fs::File::create(&log).expect("create signer log"),
            ))
            .spawn()
            .expect("spawn enroll signer");
        let mut child = child;
        wait_for_listener(port, &mut child);
        Self {
            root: root.to_path_buf(),
            port,
            child: Some(child),
        }
    }

    fn url(&self) -> String {
        format!("https://127.0.0.1:{}/v1/enroll", self.port)
    }

    fn ca_certificate(&self) -> Vec<u8> {
        fs::read(self.root.join("ca.crt")).expect("read fixture CA")
    }
}

impl Drop for SignerFixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
    listener.local_addr().expect("local addr").port()
}

fn wait_for_listener(port: u16, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().expect("inspect signer process") {
            panic!("enroll signer exited during startup with {status}");
        }
        assert!(
            Instant::now() < deadline,
            "enroll signer did not listen on 127.0.0.1:{port}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn openssl(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("openssl")
        .current_dir(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn openssl")
}

fn verify(description: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{description} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn set_private_permissions(path: PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod 600");
}

#[cfg(not(unix))]
fn set_private_permissions(_path: PathBuf) {}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real openssl installation"]
fn runtime_csr_signs_under_the_real_ca_and_reads_back_identically() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real openssl tier");
        return;
    }

    let label = "autoenroll-roundtrip";
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).unwrap();

    // The machine side mints its own identity: keypair + CSR from a git-like
    // CN with the deployment's SAN shape.
    let (key_pem, csr_pem) = loam::enrollment_auto::generate_keypair_and_csr(
        "ada@example.org",
        "Ada Lovelace",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    )
    .expect("the runtime should generate a keypair and CSR without a broker");

    fs::write(root.join("machine.key"), &key_pem).unwrap();
    fs::write(root.join("machine.csr"), &csr_pem).unwrap();

    // Gate 1: openssl must accept the CSR as self-consistent and verify its
    // signature with the public key embedded in it.
    let verify_out = openssl(&root, &["req", "-verify", "-in", "machine.csr", "-noout"]);
    verify("openssl req -verify", &verify_out);

    // Gate 2: the CA must be able to sign it under the real copy_extensions
    // path and produce a real certificate.
    provision_ca(&root);
    let sign = openssl(
        &root,
        &[
            "ca",
            "-config",
            "openssl.cnf",
            "-batch",
            "-notext",
            "-in",
            "machine.csr",
            "-out",
            "machine.crt",
        ],
    );
    verify("openssl ca sign", &sign);

    let cert_pem = fs::read(root.join("machine.crt")).unwrap();

    // Gate 3: the runtime's own readers recover the identity contract from the
    // openssl-issued cert exactly (CN = principal email, GN = display name,
    // SAN = instance id).
    let subject = loam::provisioning::certificate_subject(&cert_pem)
        .expect("the runtime's subject reader should accept the openssl cert");
    assert_eq!(subject.common_name, "ada@example.org");
    assert_eq!(subject.given_name.as_deref(), Some("Ada Lovelace"));
    let instance_id = loam::provisioning::certificate_instance_id(&cert_pem)
        .expect("the runtime's SAN reader should accept the openssl cert");
    assert_eq!(instance_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");

    fs::remove_dir_all(&root).ok();
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1 and a real openssl installation"]
fn runtime_private_key_and_cert_pair_through_openssl() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real openssl tier");
        return;
    }

    let label = "autoenroll-keymatch";
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).unwrap();

    let (key_pem, csr_pem) = loam::enrollment_auto::generate_keypair_and_csr(
        "ada@example.org",
        "Ada Lovelace",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    )
    .expect("the runtime should generate a keypair and CSR without a broker");
    fs::write(root.join("machine.key"), &key_pem).unwrap();
    fs::write(root.join("machine.csr"), &csr_pem).unwrap();

    // The private key the runtime stored must be usable by real openssl, both
    // to read and to extract the public key that matches the CSR.
    let read_key = openssl(&root, &["pkey", "-in", "machine.key", "-noout"]);
    verify("openssl pkey reads the runtime key", &read_key);

    let pubkey = openssl(
        &root,
        &[
            "pkey",
            "-in",
            "machine.key",
            "-pubout",
            "-out",
            "machine.pub",
        ],
    );
    verify("openssl extracts the runtime public key", &pubkey);
    fs::remove_dir_all(&root).ok();
}

/// A fixture directory, provisioned with a CA + server cert + password, that
/// survives as long as the returned guard holds.
fn fixtured_root(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("loam-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    provision_ca(&root);
    root
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, real openssl, and python3"]
fn request_signed_certificate_happy_path_returns_the_signer_issued_certificate() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real signer tier");
        return;
    }
    let root = fixtured_root("autoenroll-http");
    let _signer = SignerFixture::start(&root);

    let (_key_pem, csr_pem) = loam::enrollment_auto::generate_keypair_and_csr(
        "ada@example.org",
        "Ada Lovelace",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    )
    .expect("the runtime should generate a keypair and CSR");
    let cert = loam::enrollment_auto::request_signed_certificate(
        &_signer.url(),
        "shared-enroll-secret",
        &csr_pem,
        &_signer.ca_certificate(),
    )
    .expect("the signer should issue a certificate for a valid token");

    let subject = loam::provisioning::certificate_subject(&cert)
        .expect("the issued cert should be readable by the runtime reader");
    assert_eq!(subject.common_name, "ada@example.org");
    assert_eq!(subject.given_name.as_deref(), Some("Ada Lovelace"));
    let instance = loam::provisioning::certificate_instance_id(&cert)
        .expect("the issued cert should carry the CSR SAN");
    assert_eq!(instance, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    fs::remove_dir_all(&root).ok();
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, real openssl, and python3"]
fn request_signed_certificate_refuses_a_wrong_token() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real signer tier");
        return;
    }
    let root = fixtured_root("autoenroll-badtoken");
    let _signer = SignerFixture::start(&root);

    let (_key_pem, csr_pem) = loam::enrollment_auto::generate_keypair_and_csr(
        "ada@example.org",
        "Ada Lovelace",
        "01ARZ3NDEKTSV4RRFFQ69G5FBV",
    )
    .expect("the runtime should generate a keypair and CSR");
    let error = loam::enrollment_auto::request_signed_certificate(
        &_signer.url(),
        "wrong-token",
        &csr_pem,
        &_signer.ca_certificate(),
    )
    .expect_err("a wrong token must be a typed bad-token refusal");
    assert_eq!(error, loam::enrollment_auto::EnrollmentFailure::BadToken);
    fs::remove_dir_all(&root).ok();
}

#[test]
#[ignore = "requires LOAM_MQTT_TEST=1, real openssl, and python3"]
fn request_signed_certificate_reports_an_unreachable_signer() {
    if std::env::var("LOAM_MQTT_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set LOAM_MQTT_TEST=1 to require the real signer tier");
        return;
    }
    // No server on this port, so the connect must fail as unreachable.
    let dead_port = reserve_port();
    let root = fixtured_root("autoenroll-unreachable");

    let (_key_pem, csr_pem) = loam::enrollment_auto::generate_keypair_and_csr(
        "ada@example.org",
        "Ada Lovelace",
        "01ARZ3NDEKTSV4RRFFQ69G5FBX",
    )
    .expect("the runtime should generate a keypair and CSR");
    let error = loam::enrollment_auto::request_signed_certificate(
        &format!("https://127.0.0.1:{dead_port}/v1/enroll"),
        "shared-enroll-secret",
        &csr_pem,
        &fs::read(root.join("ca.crt")).unwrap(),
    )
    .expect_err("an unreachable signer must be a typed signer-unreachable refusal");
    assert_eq!(
        error,
        loam::enrollment_auto::EnrollmentFailure::SignerUnreachable
    );
    fs::remove_dir_all(&root).ok();
}
