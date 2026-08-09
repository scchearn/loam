//! Slice C Unix IPC endpoint: an owner-only domain socket whose accepted peer's
//! effective UID must match the connector before any frame byte is read.
//!
//! Two barriers guard the endpoint. First, filesystem permissions: the run
//! directory is `0700` and owned by us, the socket is `0600`, and neither may be
//! a symlink or a non-socket we did not create. Second — and load-bearing,
//! because pathname-socket permission behaviour is not portable — a kernel
//! peer-credential check after `accept`: Linux `SO_PEERCRED`, macOS `getpeereid`,
//! compared against the connector's own `geteuid`. A peer whose EUID differs is
//! rejected as `UnauthorizedPeer` before the codec ever runs.
//!
//! Raw FFI (approved Route A). Each declaration is matched to its man-page
//! signature; a mis-declared signature is the one risk this route carries, so the
//! bindings are narrow, documented, and covered by a same-user positive test
//! plus the cross-user smoke.
//!
//! Consumed by the connector loop (T9), which retires this allow once the
//! endpoint is wired to the running service.
#![allow(dead_code)]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use super::IpcError;

const RUN_DIR_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;
const SOCKET_NAME: &str = "connector.sock";

// --- raw FFI, matched to man pages -----------------------------------------

// `uid_t geteuid(void);` — uid_t is `unsigned int` (u32) on Linux and macOS.
extern "C" {
    fn geteuid() -> u32;
}

#[cfg(target_os = "linux")]
mod sys {
    use core::ffi::c_void;

    // `struct ucred { pid_t pid; uid_t uid; gid_t gid; };` — pid_t is i32,
    // uid_t/gid_t are u32 on Linux.
    #[repr(C)]
    pub struct Ucred {
        pub pid: i32,
        pub uid: u32,
        pub gid: u32,
    }

    // `int getsockopt(int sockfd, int level, int optname, void *optval,
    //                 socklen_t *optlen);` — int is i32, socklen_t is u32.
    extern "C" {
        pub fn getsockopt(
            sockfd: i32,
            level: i32,
            optname: i32,
            optval: *mut c_void,
            optlen: *mut u32,
        ) -> i32;
    }

    // SOL_SOCKET and SO_PEERCRED on Linux (x86_64/aarch64 and the common ABIs).
    pub const SOL_SOCKET: i32 = 1;
    pub const SO_PEERCRED: i32 = 17;
}

#[cfg(target_os = "macos")]
mod sys {
    // `int getpeereid(int socket, uid_t *euid, gid_t *egid);`
    extern "C" {
        pub fn getpeereid(socket: i32, euid: *mut u32, egid: *mut u32) -> i32;
    }
}

fn connector_euid() -> u32 {
    // Safe: geteuid takes no arguments and cannot fail.
    unsafe { geteuid() }
}

/// The accepted peer's effective UID, via the platform's kernel peer-credential
/// mechanism. Never trusts a payload-declared identity.
#[cfg(target_os = "linux")]
fn peer_euid(stream: &UnixStream) -> Result<u32, IpcError> {
    let mut cred = sys::Ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut len = core::mem::size_of::<sys::Ucred>() as u32;
    // Safe: fd is a live connected socket, the pointers are to correctly sized,
    // aligned local storage, and getsockopt does not retain them.
    let rc = unsafe {
        sys::getsockopt(
            stream.as_raw_fd(),
            sys::SOL_SOCKET,
            sys::SO_PEERCRED,
            (&mut cred as *mut sys::Ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 || len as usize != core::mem::size_of::<sys::Ucred>() {
        return Err(IpcError::UnauthorizedPeer);
    }
    Ok(cred.uid)
}

#[cfg(target_os = "macos")]
fn peer_euid(stream: &UnixStream) -> Result<u32, IpcError> {
    let mut euid: u32 = u32::MAX;
    let mut egid: u32 = u32::MAX;
    // Safe: fd is a live connected socket; both out-pointers are valid local
    // storage that getpeereid only writes.
    let rc = unsafe { sys::getpeereid(stream.as_raw_fd(), &mut euid, &mut egid) };
    if rc != 0 {
        return Err(IpcError::UnauthorizedPeer);
    }
    Ok(euid)
}

/// Verify the accepted peer shares the connector's effective UID. This is the
/// authentication boundary; it must run before any frame is read.
pub fn verify_peer(stream: &UnixStream) -> Result<(), IpcError> {
    if peer_euid(stream)? == connector_euid() {
        Ok(())
    } else {
        Err(IpcError::UnauthorizedPeer)
    }
}

/// An owner-only endpoint that unlinks its socket on drop.
pub struct OwnedEndpoint {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl OwnedEndpoint {
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Accept one connection and prove its peer before handing back a byte
    /// stream. The kernel peer-credential check runs here, so a caller that only
    /// ever touches a [`VerifiedConn`] cannot read a frame from an unverified
    /// peer. Keeps all `UnixStream` handling inside this module.
    pub fn accept_verified(&self) -> Result<VerifiedConn, IpcError> {
        let (stream, _addr) = self.listener.accept().map_err(|_| IpcError::Internal)?;
        verify_peer(&stream)?;
        Ok(VerifiedConn { stream })
    }
}

/// A connection whose peer has already been proven to share the connector's
/// effective UID. Exposes only `Read`/`Write`, never the underlying socket type,
/// so higher layers never name a `UnixStream`.
pub struct VerifiedConn {
    stream: UnixStream,
}

impl std::io::Read for VerifiedConn {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buffer)
    }
}

impl std::io::Write for VerifiedConn {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

impl Drop for OwnedEndpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Bind the owner-only endpoint under `run_dir`, creating the directory `0700`
/// and the socket `0600`. Rejects an unsafe directory or a socket path that is a
/// symlink, a non-socket, or owned by another user. A stale socket we own is
/// removed and rebound; a live one means another connector already holds the
/// endpoint (`Busy`).
pub fn bind(run_dir: &Path) -> Result<OwnedEndpoint, IpcError> {
    prepare_run_dir(run_dir)?;
    let socket_path = run_dir.join(SOCKET_NAME);
    reconcile_existing_socket(&socket_path)?;

    let listener = UnixListener::bind(&socket_path).map_err(|_| IpcError::Internal)?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(SOCKET_MODE))
        .map_err(|_| IpcError::Internal)?;
    Ok(OwnedEndpoint {
        listener,
        socket_path,
    })
}

/// Open the connector's endpoint as a client, proving the *server's* effective
/// UID before a byte is written — the same peer-credential check the server runs
/// on us, so a socket planted by another user is refused rather than talked to.
/// The deadline bounds both directions: a connector that stops answering costs
/// the caller that long, never the session.
pub fn connect(run_dir: &Path, deadline: std::time::Duration) -> Result<ClientConn, IpcError> {
    let stream = UnixStream::connect(run_dir.join(SOCKET_NAME)).map_err(|_| IpcError::Internal)?;
    verify_peer(&stream)?;
    stream
        .set_read_timeout(Some(deadline))
        .map_err(|_| IpcError::Internal)?;
    stream
        .set_write_timeout(Some(deadline))
        .map_err(|_| IpcError::Internal)?;
    Ok(ClientConn { stream })
}

/// The client half. Like [`VerifiedConn`] it exposes only `Read`/`Write`, so no
/// caller outside this module ever names a socket type.
pub struct ClientConn {
    stream: UnixStream,
}

impl std::io::Read for ClientConn {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buffer)
    }
}

impl std::io::Write for ClientConn {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn prepare_run_dir(run_dir: &Path) -> Result<(), IpcError> {
    match std::fs::symlink_metadata(run_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(IpcError::UnauthorizedPeer);
            }
            if meta.uid() != connector_euid() {
                return Err(IpcError::UnauthorizedPeer);
            }
            // Force owner-only mode even if it drifted.
            std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(RUN_DIR_MODE))
                .map_err(|_| IpcError::Internal)?;
        }
        Err(_) => {
            std::fs::create_dir_all(run_dir).map_err(|_| IpcError::Internal)?;
            std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(RUN_DIR_MODE))
                .map_err(|_| IpcError::Internal)?;
        }
    }
    Ok(())
}

fn reconcile_existing_socket(socket_path: &Path) -> Result<(), IpcError> {
    let meta = match std::fs::symlink_metadata(socket_path) {
        Ok(meta) => meta,
        Err(_) => return Ok(()), // nothing there — clean bind
    };
    // Never follow a symlink at the socket path, and never adopt a non-socket or
    // one owned by another user.
    if meta.file_type().is_symlink() {
        return Err(IpcError::UnauthorizedPeer);
    }
    if !is_socket(&meta) {
        return Err(IpcError::UnauthorizedPeer);
    }
    if meta.uid() != connector_euid() {
        return Err(IpcError::UnauthorizedPeer);
    }
    // Owned socket: is a connector live behind it?
    match UnixStream::connect(socket_path) {
        Ok(_) => Err(IpcError::Busy), // another connector holds the endpoint
        Err(_) => {
            // Stale — remove and let the caller rebind.
            std::fs::remove_file(socket_path).map_err(|_| IpcError::Internal)?;
            Ok(())
        }
    }
}

fn is_socket(meta: &std::fs::Metadata) -> bool {
    const S_IFMT: u32 = 0o170000;
    const S_IFSOCK: u32 = 0o140000;
    meta.mode() & S_IFMT == S_IFSOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn temp_run_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loam-ipc-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }

    #[test]
    fn binds_owner_only_directory_and_socket() {
        let run = temp_run_dir("modes");
        let endpoint = bind(&run).expect("bind");
        let dir_mode = std::fs::metadata(&run).unwrap().mode() & 0o777;
        let sock_mode = std::fs::symlink_metadata(endpoint.socket_path())
            .unwrap()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, RUN_DIR_MODE);
        assert_eq!(sock_mode, SOCKET_MODE);
        drop(endpoint);
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn same_user_peer_is_authorized() {
        // A real connection from this same process shares our EUID, so the
        // kernel peer-credential check must accept it.
        let run = temp_run_dir("same-user");
        let endpoint = bind(&run).expect("bind");
        let path = endpoint.socket_path().to_path_buf();

        let client = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(&path).expect("connect");
            stream.write_all(b"hi").ok();
            let mut buf = [0u8; 2];
            let _ = stream.read(&mut buf);
        });

        let (server_stream, _) = endpoint.listener().accept().expect("accept");
        assert_eq!(verify_peer(&server_stream), Ok(()));
        drop(server_stream);
        client.join().ok();
        drop(endpoint);
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_symlink_at_the_socket_path_is_refused() {
        let run = temp_run_dir("symlink");
        prepare_run_dir(&run).unwrap();
        let socket_path = run.join(SOCKET_NAME);
        std::os::unix::fs::symlink("/tmp/elsewhere", &socket_path).unwrap();
        assert_eq!(bind(&run).err(), Some(IpcError::UnauthorizedPeer));
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_non_socket_at_the_socket_path_is_refused() {
        let run = temp_run_dir("nonsocket");
        prepare_run_dir(&run).unwrap();
        std::fs::write(run.join(SOCKET_NAME), b"not a socket").unwrap();
        assert_eq!(bind(&run).err(), Some(IpcError::UnauthorizedPeer));
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_stale_owned_socket_is_reclaimed() {
        let run = temp_run_dir("stale");
        prepare_run_dir(&run).unwrap();
        let socket_path = run.join(SOCKET_NAME);
        // A raw listener leaves its pathname socket file behind on drop (std does
        // not unlink), and nothing accepts once it is gone — the stale condition.
        let listener = UnixListener::bind(&socket_path).unwrap();
        drop(listener);
        assert!(socket_path.exists(), "stale socket file should linger");
        // bind() must detect the unconnectable stale socket, remove it, rebind.
        let rebound = bind(&run).expect("rebind after stale cleanup");
        assert!(rebound.socket_path().exists());
        drop(rebound);
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_live_endpoint_is_a_singleton() {
        let run = temp_run_dir("singleton");
        let first = bind(&run).expect("first bind");
        // A second bind against the same live socket must report Busy.
        assert_eq!(bind(&run).err(), Some(IpcError::Busy));
        drop(first);
        let _ = std::fs::remove_dir_all(&run);
    }
}
