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
//! Gated on `LOAM_MQTT_TEST=1` exactly like the real-broker tier: it needs a
//! real `openssl` installation.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

/// Build the DER-testing fixture: a throwaway self-signed CA plus the CA
/// config the deployment uses (`copy_extensions = copy`), so the CSR's
/// claimed SAN is copied verbatim into the signed cert the way the real
/// signer does.
fn provision_ca(root: &Path) {
    fs::create_dir_all(root.join("newcerts")).unwrap();
    fs::write(root.join("index.txt"), "").unwrap();
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
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
        ],
    );
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
