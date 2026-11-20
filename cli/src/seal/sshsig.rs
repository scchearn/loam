//! SSHSIG signing and verification through `ssh-keygen -Y`.
//!
//! There is deliberately no native SSHSIG parser: the reader's own
//! `ssh-keygen` is the verifier, in the fixed namespace [`NAMESPACE`]. A
//! missing or too-old binary is a [`Capability::Unavailable`] answer, never an
//! error, so callers can cap the verdict at `evidence` instead of failing.

use chrono::{DateTime, Utc};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

/// The SSHSIG namespace every loam seal signature lives in.
pub const NAMESPACE: &str = "loam-seal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshsigError {
    /// The binary could not be run at all.
    Spawn(String),
    /// ssh-keygen ran and refused: bad signature, wrong namespace, no match.
    Rejected(String),
    /// Local setup problem (scratch files, bad timestamp).
    Local(String),
}

impl fmt::Display for SshsigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SshsigError::Spawn(m) | SshsigError::Rejected(m) | SshsigError::Local(m) => {
                f.write_str(m)
            }
        }
    }
}

/// A verified signature: the principal that matched and the key fingerprint
/// ssh-keygen reported, when it printed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub principal: String,
    pub fingerprint: Option<String>,
}

/// `LOAM_SSH_KEYGEN` when set and non-empty, else `ssh-keygen` from PATH.
pub fn keygen_binary() -> PathBuf {
    match std::env::var("LOAM_SSH_KEYGEN") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => PathBuf::from("ssh-keygen"),
    }
}

/// Capability of the configured binary, probed once per binary path.
pub fn capability() -> Capability {
    capability_of(&keygen_binary())
}

/// Probe `bin` once: generate a throwaway key, sign, `find-principals`, and
/// `verify` with `-O verify-time`. Any failure (missing binary, no `-Y`, no
/// `verify-time`) is `Unavailable`.
pub fn capability_of(bin: &Path) -> Capability {
    static CACHE: Mutex<Vec<(PathBuf, Capability)>> = Mutex::new(Vec::new());
    {
        let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, found)) = cache.iter().find(|(path, _)| path == bin) {
            return *found;
        }
    }
    let result = if probe(bin) {
        Capability::Available
    } else {
        Capability::Unavailable
    };
    CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((bin.to_path_buf(), result));
    result
}

fn probe(bin: &Path) -> bool {
    let Ok(scratch) = Scratch::new("probe") else {
        return false;
    };
    let key = scratch.path().join("k");
    let generated = run(
        bin,
        &[
            "-q".as_ref(),
            "-t".as_ref(),
            "ed25519".as_ref(),
            "-N".as_ref(),
            "".as_ref(),
            "-C".as_ref(),
            "probe".as_ref(),
            "-f".as_ref(),
            key.as_os_str(),
        ],
        None,
    );
    if !generated.map(|o| o.status.success()).unwrap_or(false) {
        return false;
    }
    let Ok(public) = fs::read_to_string(key.with_extension("pub")) else {
        return false;
    };
    let mut parts = public.split_whitespace();
    let (Some(kind), Some(material)) = (parts.next(), parts.next()) else {
        return false;
    };
    let allowed = scratch.path().join("allowed");
    if fs::write(
        &allowed,
        format!("probe namespaces=\"{NAMESPACE}\" {kind} {material}\n"),
    )
    .is_err()
    {
        return false;
    }
    let Ok(signature) = sign(bin, &key, b"probe") else {
        return false;
    };
    let now = Utc::now().format("%Y%m%d%H%MZ").to_string();
    find_principals(bin, &allowed, &signature, &now).is_ok()
        && verify(bin, &allowed, "probe", &signature, b"probe", &now, None).is_ok()
}

/// Sign `data` with the private key at `key`, returning the armored SSHSIG.
pub fn sign(bin: &Path, key: &Path, data: &[u8]) -> Result<String, SshsigError> {
    let output = run(
        bin,
        &[
            "-Y".as_ref(),
            "sign".as_ref(),
            "-n".as_ref(),
            NAMESPACE.as_ref(),
            "-f".as_ref(),
            key.as_os_str(),
        ],
        Some(data),
    )?;
    if !output.status.success() {
        return Err(SshsigError::Rejected(diagnostic(&output)));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| SshsigError::Rejected("ssh-keygen produced a non-UTF-8 signature".into()))
}

/// Verify `signature` over `data` for `principal` against `allowed_signers`,
/// as of `verify_time` (`YYYYMMDDHHMMZ`, see [`verify_time_utc`]), optionally
/// against a `revoked` key list.
pub fn verify(
    bin: &Path,
    allowed_signers: &Path,
    principal: &str,
    signature: &str,
    data: &[u8],
    verify_time: &str,
    revoked: Option<&Path>,
) -> Result<Verified, SshsigError> {
    let scratch = Scratch::new("verify").map_err(SshsigError::Local)?;
    let sig_path = scratch.write("sig", signature.as_bytes())?;
    let time_option = format!("verify-time={verify_time}");
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "-Y".as_ref(),
        "verify".as_ref(),
        "-f".as_ref(),
        allowed_signers.as_os_str(),
        "-I".as_ref(),
        principal.as_ref(),
        "-n".as_ref(),
        NAMESPACE.as_ref(),
        "-s".as_ref(),
        sig_path.as_os_str(),
        "-O".as_ref(),
        time_option.as_ref(),
    ];
    if let Some(revoked) = revoked {
        args.push("-r".as_ref());
        args.push(revoked.as_os_str());
    }
    let output = run(bin, &args, Some(data))?;
    if !output.status.success() {
        return Err(SshsigError::Rejected(diagnostic(&output)));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let fingerprint = stdout
        .split_whitespace()
        .find(|token| token.starts_with("SHA256:"))
        .map(str::to_owned);
    Ok(Verified {
        principal: principal.to_owned(),
        fingerprint,
    })
}

/// Whether `signature` is cryptographically valid over `data` in
/// [`NAMESPACE`], regardless of who signed it or whether that key is trusted
/// (`ssh-keygen -Y check-novalidate`). Lets a reader tell a forged signature
/// from a genuine one by a key it does not trust.
pub fn check_novalidate(bin: &Path, signature: &str, data: &[u8]) -> Result<(), SshsigError> {
    let scratch = Scratch::new("novalidate").map_err(SshsigError::Local)?;
    let sig_path = scratch.write("sig", signature.as_bytes())?;
    let output = run(
        bin,
        &[
            "-Y".as_ref(),
            "check-novalidate".as_ref(),
            "-n".as_ref(),
            NAMESPACE.as_ref(),
            "-s".as_ref(),
            sig_path.as_os_str(),
        ],
        Some(data),
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(SshsigError::Rejected(diagnostic(&output)))
    }
}

/// Principals in `allowed_signers` whose key made `signature`.
pub fn find_principals(
    bin: &Path,
    allowed_signers: &Path,
    signature: &str,
    verify_time: &str,
) -> Result<Vec<String>, SshsigError> {
    let scratch = Scratch::new("find").map_err(SshsigError::Local)?;
    let sig_path = scratch.write("sig", signature.as_bytes())?;
    let time_option = format!("verify-time={verify_time}");
    let output = run(
        bin,
        &[
            "-Y".as_ref(),
            "find-principals".as_ref(),
            "-O".as_ref(),
            time_option.as_ref(),
            "-f".as_ref(),
            allowed_signers.as_os_str(),
            "-s".as_ref(),
            sig_path.as_os_str(),
        ],
        None,
    )?;
    if !output.status.success() {
        return Err(SshsigError::Rejected(diagnostic(&output)));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Convert a stamp to the UTC `YYYYMMDDHHMMZ` form `-O verify-time` takes.
/// Accepts loam's canonical `YYYY-MM-DD HH:MM ±HH:MM` (and the unambiguous
/// `±HHMM`), plus RFC 3339 and seconds-bearing variants. A stamp without an
/// offset is an error.
pub fn verify_time_utc(stamp: &str) -> Result<String, SshsigError> {
    let parsed = DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M %:z")
        .or_else(|_| DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M %z"))
        .or_else(|_| DateTime::parse_from_rfc3339(stamp))
        .or_else(|_| DateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S%z"))
        .or_else(|_| DateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S %z"))
        .map_err(|_| SshsigError::Local(format!("unparseable timestamp `{stamp}`")))?;
    Ok(parsed.with_timezone(&Utc).format("%Y%m%d%H%MZ").to_string())
}

fn diagnostic(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    }
    if text.is_empty() {
        text = format!("ssh-keygen exited with {}", output.status);
    }
    text
}

fn run(bin: &Path, args: &[&std::ffi::OsStr], stdin: Option<&[u8]>) -> Result<Output, SshsigError> {
    let mut child = Command::new(bin)
        .args(args)
        // A passphrase-protected key must fail fast, not block on a prompt.
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env_remove("DISPLAY")
        .env_remove("SSH_ASKPASS")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SshsigError::Spawn(format!("cannot run {}: {e}", bin.display())))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A verifier that exits early closes the pipe; that is its verdict to
        // report, not ours.
        let _ = pipe.write_all(data);
    }
    child
        .wait_with_output()
        .map_err(|e| SshsigError::Spawn(format!("cannot wait for {}: {e}", bin.display())))
}

/// Unique scratch directory, removed (with retries for Windows) on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Result<Scratch, String> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for attempt in 0..16u32 {
            let dir = std::env::temp_dir().join(format!(
                "loam-sshsig-{label}-{}-{n}-{nanos}-{attempt}",
                std::process::id()
            ));
            // `create_dir` refuses an existing directory, so a pre-planted one
            // in the shared temp dir is never reused.
            match fs::create_dir(&dir) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
                    }
                    return Ok(Scratch(dir));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot create scratch dir {}: {e}", dir.display())),
            }
        }
        Err("cannot create a unique scratch dir".to_owned())
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, SshsigError> {
        let path = self.0.join(name);
        fs::write(&path, bytes)
            .map_err(|e| SshsigError::Local(format!("cannot write {}: {e}", path.display())))?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for _ in 0..5 {
            if fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// A `ssh-keygen` that passes [`capability_of`], for tests that make real
/// signatures. Tries `LOAM_SSH_KEYGEN`, the Git-for-Windows bundled binary,
/// then PATH. `None` means the test must assert the degraded path instead.
#[cfg(test)]
fn keygen_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(value) = std::env::var("LOAM_SSH_KEYGEN") {
        if !value.trim().is_empty() {
            candidates.push(PathBuf::from(value.trim()));
        }
    }
    if cfg!(windows) {
        candidates.push(PathBuf::from(
            r"C:\Program Files\Git\usr\bin\ssh-keygen.exe",
        ));
    }
    candidates.push(PathBuf::from("ssh-keygen"));
    candidates
}

/// Any `ssh-keygen` that can generate an ed25519 key and print its public half
/// (what `trust init` needs), even when it is too old for SSHSIG verification.
#[cfg(test)]
pub(crate) fn keygen_any_for_tests() -> Option<PathBuf> {
    if let Some(bin) = keygen_for_tests() {
        return Some(bin);
    }
    let found = keygen_candidates().into_iter().find(|bin| {
        let Ok(scratch) = Scratch::new("anykg") else {
            return false;
        };
        let key = scratch.path().join("probe");
        let made = Command::new(bin)
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
        made && Command::new(bin)
            .args(["-y", "-f"])
            .arg(&key)
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if found.is_none() && std::env::var("LOAM_REQUIRE_SSHSIG").is_ok_and(|v| v == "1") {
        panic!("LOAM_REQUIRE_SSHSIG=1 but no ssh-keygen able to generate keys was found");
    }
    found
}

#[cfg(test)]
pub(crate) fn keygen_for_tests() -> Option<PathBuf> {
    let found = keygen_candidates()
        .into_iter()
        .find(|bin| capability_of(bin) == Capability::Available);
    // Needs OpenSSH 9.1+ (`verify-time` with a `Z` suffix); ubuntu-22.04 ships
    // 8.9, so there the real-signature tests would silently assert nothing.
    // CI sets LOAM_REQUIRE_SSHSIG=1 where a capable ssh-keygen must exist.
    if found.is_none() && std::env::var("LOAM_REQUIRE_SSHSIG").is_ok_and(|v| v == "1") {
        panic!("LOAM_REQUIRE_SSHSIG=1 but no capable ssh-keygen (OpenSSH >= 9.1) was found");
    }
    found
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A stub ssh-keygen that supports nothing: exits 1 for every call.
    #[cfg(windows)]
    pub fn stub_without_y(dir: &Path) -> PathBuf {
        let path = dir.join("stub-keygen.cmd");
        fs::write(
            &path,
            "@echo off\r\necho unknown option -Y 1>&2\r\nexit /b 1\r\n",
        )
        .unwrap();
        path
    }

    #[cfg(not(windows))]
    pub fn stub_without_y(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("stub-keygen");
        fs::write(&path, "#!/bin/sh\necho 'unknown option -Y' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> Scratch {
        Scratch::new(label).unwrap()
    }

    fn make_key(bin: &Path, dir: &Path, name: &str) -> (PathBuf, String) {
        let key = dir.join(name);
        let out = Command::new(bin)
            .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
            .arg(&key)
            .output()
            .unwrap();
        assert!(out.status.success());
        let public = fs::read_to_string(key.with_extension("pub")).unwrap();
        let mut parts = public.split_whitespace();
        let line = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
        (key, line)
    }

    #[test]
    fn stub_lacking_y_is_unavailable_not_an_error() {
        let dir = tmp("stub");
        let stub = test_support::stub_without_y(dir.path());
        assert_eq!(capability_of(&stub), Capability::Unavailable);
        assert_eq!(
            capability_of(Path::new("/definitely/not/a/binary")),
            Capability::Unavailable
        );
    }

    #[test]
    fn verify_time_converts_offsets_to_utc() {
        assert_eq!(
            verify_time_utc("2026-11-20T10:30:00+02:00").unwrap(),
            "202611200830Z"
        );
        assert_eq!(
            verify_time_utc("2026-11-20T10:30:00+0200").unwrap(),
            "202611200830Z"
        );
        assert_eq!(
            verify_time_utc("2026-11-20T23:30:00-05:00").unwrap(),
            "202611210430Z"
        );
        assert_eq!(
            verify_time_utc("2026-11-20 14:30 +02:00").unwrap(),
            "202611201230Z"
        );
        assert_eq!(
            verify_time_utc("2026-11-20 14:30 +0200").unwrap(),
            "202611201230Z"
        );
        assert_eq!(
            verify_time_utc("2026-11-20 23:30 -05:00").unwrap(),
            "202611210430Z"
        );
        assert!(verify_time_utc("2026-11-20T10:30:00").is_err());
        assert!(verify_time_utc("2026-11-20 14:30").is_err());
    }

    #[test]
    fn signed_blob_verifies_in_namespace_and_fails_in_another() {
        let Some(bin) = keygen_for_tests() else {
            eprintln!("no capable ssh-keygen; degraded path covered by the stub test");
            return;
        };
        let dir = tmp("roundtrip");
        let (key, public) = make_key(&bin, dir.path(), "signer");
        let allowed = dir.path().join("allowed");
        fs::write(
            &allowed,
            format!("agent.one@local namespaces=\"loam-seal\",valid-after=\"20200101\" {public}\n"),
        )
        .unwrap();
        let signature = sign(&bin, &key, b"payload").unwrap();
        let now = verify_time_utc("2026-11-20T10:00:00+00:00").unwrap();

        let ok = verify(
            &bin,
            &allowed,
            "agent.one@local",
            &signature,
            b"payload",
            &now,
            None,
        )
        .unwrap();
        assert_eq!(ok.principal, "agent.one@local");
        assert!(ok.fingerprint.is_some_and(|f| f.starts_with("SHA256:")));

        assert!(matches!(
            verify(
                &bin,
                &allowed,
                "agent.one@local",
                &signature,
                b"tampered",
                &now,
                None
            ),
            Err(SshsigError::Rejected(_))
        ));
        // Namespace is pinned: a signature made for another namespace fails.
        let other = Command::new(&bin)
            .args(["-Y", "sign", "-n", "other-ns", "-f"])
            .arg(&key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut other = other;
        other.stdin.take().unwrap().write_all(b"payload").unwrap();
        let other_sig = String::from_utf8(other.wait_with_output().unwrap().stdout).unwrap();
        assert!(verify(
            &bin,
            &allowed,
            "agent.one@local",
            &other_sig,
            b"payload",
            &now,
            None
        )
        .is_err());
    }

    #[test]
    fn find_principals_returns_the_signer() {
        let Some(bin) = keygen_for_tests() else {
            return;
        };
        let dir = tmp("find");
        let (key, public) = make_key(&bin, dir.path(), "signer");
        let (_, other_public) = make_key(&bin, dir.path(), "other");
        let allowed = dir.path().join("allowed");
        fs::write(
            &allowed,
            format!(
                "someone.else@org namespaces=\"loam-seal\" {other_public}\nagent.two@local namespaces=\"loam-seal\" {public}\n"
            ),
        )
        .unwrap();
        let signature = sign(&bin, &key, b"x").unwrap();
        assert_eq!(
            find_principals(&bin, &allowed, &signature, "202601010000Z").unwrap(),
            vec!["agent.two@local".to_owned()]
        );
    }

    #[test]
    fn revoked_key_and_closed_window_fail_verification() {
        let Some(bin) = keygen_for_tests() else {
            return;
        };
        let dir = tmp("revoked");
        let (key, public) = make_key(&bin, dir.path(), "signer");
        let allowed = dir.path().join("allowed");
        fs::write(
            &allowed,
            format!("p@local namespaces=\"loam-seal\",valid-after=\"20200101\",valid-before=\"20250101\" {public}\n"),
        )
        .unwrap();
        let signature = sign(&bin, &key, b"x").unwrap();
        let inside = verify_time_utc("2024-06-01T00:00:00+00:00").unwrap();
        let after = verify_time_utc("2026-06-01T00:00:00+00:00").unwrap();
        assert!(verify(&bin, &allowed, "p@local", &signature, b"x", &inside, None).is_ok());
        assert!(verify(&bin, &allowed, "p@local", &signature, b"x", &after, None).is_err());

        let revoked = dir.path().join("revoked");
        fs::write(&revoked, format!("{public}\n")).unwrap();
        assert!(verify(
            &bin,
            &allowed,
            "p@local",
            &signature,
            b"x",
            &inside,
            Some(&revoked)
        )
        .is_err());
    }
}
