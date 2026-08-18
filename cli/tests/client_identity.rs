//! What `client.pem` + `key.pem` may look like, and what a bad pair is told.
//!
//! Both halves of #95. The identity layout contract says PKCS#8, but the
//! commands operators and this repository's own scripts actually run emit
//! three different encodings, and one of them (`openssl genrsa` before
//! OpenSSL 3) is what `deploy/mqtt-broker/pki/issue-client.sh` calls — so
//! refusing it would mean refusing identities loam itself mints.
//!
//! The acceptance is currently inherited from `rustls-pemfile` and `rustls`
//! rather than written here, which is exactly why it needs pinning: nothing
//! else in the tree states it, and a dependency bump or a well-meant
//! tightening would silently reintroduce the refusal that cost hours to
//! diagnose.
//!
//! Fixtures are real openssl output under `support/identity/`, not synthetic
//! DER — the lesson this repository already learned once with CSRs.

use loam::provisioning::build_client_config;

const EC_CLIENT: &str = include_str!("support/identity/ec-client.pem");
const EC_PKCS8: &str = include_str!("support/identity/ec-pkcs8.pem");
const EC_SEC1: &str = include_str!("support/identity/ec-sec1.pem");
const EC_SEC1_WITH_PARAMS: &str = include_str!("support/identity/ec-sec1-with-params.pem");
const EC_UNRELATED_SEC1: &str = include_str!("support/identity/ec-unrelated-sec1.pem");
const RSA_CLIENT: &str = include_str!("support/identity/rsa-client.pem");
const RSA_PKCS1: &str = include_str!("support/identity/rsa-pkcs1.pem");

fn accepts(certificate: &str, key: &str) -> Result<(), &'static str> {
    build_client_config(
        &rustls::RootCertStore::empty(),
        Some((certificate.as_bytes(), key.as_bytes())),
    )
    .map(|_| ())
}

#[test]
fn every_encoding_operators_and_our_own_scripts_produce_is_accepted() {
    // One EC keypair in the three shapes openssl can write it, plus the RSA
    // PKCS#1 shape `pki/issue-client.sh` produces on OpenSSL 1.x and LibreSSL.
    for (name, certificate, key) in [
        (
            "PKCS#8 (openssl pkcs8 -topk8, and our own auto-enrollment)",
            EC_CLIENT,
            EC_PKCS8,
        ),
        (
            "SEC1 (openssl ecparam -genkey -noout, macOS LibreSSL default)",
            EC_CLIENT,
            EC_SEC1,
        ),
        (
            "SEC1 behind the EC PARAMETERS block openssl writes without -noout",
            EC_CLIENT,
            EC_SEC1_WITH_PARAMS,
        ),
        (
            "PKCS#1 (openssl genrsa before OpenSSL 3, our pki/issue-client.sh)",
            RSA_CLIENT,
            RSA_PKCS1,
        ),
    ] {
        assert_eq!(
            accepts(certificate, key),
            Ok(()),
            "{name} must be accepted; refusing it is #95"
        );
    }
}

#[test]
fn a_key_that_is_not_the_certificates_key_says_so() {
    // The hand-placed-identity mistake: two valid files that are not a pair.
    // This used to surface as `connect_probe_failed` with no further word.
    assert_eq!(
        accepts(EC_CLIENT, EC_UNRELATED_SEC1),
        Err("key-cert-mismatch"),
        "a mismatched pair must name the mismatch, not the transport"
    );
}

#[test]
fn an_unusable_key_names_the_key_and_a_bad_certificate_names_the_certificate() {
    // A well-formed SEC1 envelope around bytes that are not a key: the
    // container parses, the key provider refuses it.
    let garbage_key =
        "-----BEGIN EC PRIVATE KEY-----\nbm90IGEga2V5\n-----END EC PRIVATE KEY-----\n";
    assert_eq!(
        accepts(EC_CLIENT, garbage_key),
        Err("key-format-unsupported")
    );
    // No key block at all.
    assert_eq!(accepts(EC_CLIENT, EC_CLIENT), Err("key-format-unsupported"));
    // And the other file's own reason, so the operator knows which to open.
    assert_eq!(accepts("", EC_SEC1), Err("certificate-malformed"));
    assert_eq!(accepts(EC_PKCS8, EC_SEC1), Err("certificate-malformed"));
}

#[test]
fn no_client_authentication_still_builds_a_configuration() {
    // The broker-verifying-only path must keep working; the credential
    // reasons exist for the mTLS path alone.
    assert!(build_client_config(&rustls::RootCertStore::empty(), None).is_ok());
}
