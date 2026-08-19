//! Windows IPC endpoint: a named pipe whose security descriptor names
//! only the current logon session, and whose connected client's token SID must
//! equal the connector's before any frame byte is read.
//!
//! Three barriers guard the endpoint, in this order:
//!
//! 1. **Creation.** The pipe is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`
//!    (so a squatter cannot pre-create the name and a second connector cannot
//!    share it), `PIPE_REJECT_REMOTE_CLIENTS` (no network client), and an
//!    explicit **protected** DACL (`D:P`) with exactly one allow ACE for the
//!    current logon SID. No default, inherited, or `Everyone`/`Anonymous`
//!    security descriptor is ever used.
//! 2. **Peer proof.** After `ConnectNamedPipe`, the server impersonates the
//!    client, opens the impersonation token as itself, reads its `TokenUser`
//!    SID, reverts unconditionally, and compares that SID with the connector
//!    process token's SID via `EqualSid`. A failed impersonation or token query
//!    is a rejection, never a continue-under-server-token.
//! 3. **Bounded I/O.** Connect, read, and write are overlapped operations with
//!    explicit deadlines. On timeout the operation is cancelled with
//!    `CancelIoEx` and then *awaited to terminal completion*, so the
//!    `OVERLAPPED` and its buffer are never freed while the kernel may still
//!    write to them. If that drain does not itself reach terminal completion,
//!    the process aborts rather than freeing memory the kernel still owns —
//!    the rule is absolute, not bounded by a second timeout.
//!
//! Raw FFI (approved Route A). Each declaration carries the Win32 signature
//! it was matched against; a mis-declared signature or constant is the one risk
//! this route carries, so the surface is narrow, documented, and covered by the
//! Windows-target tests in `cli/tests/ipc_owner.rs` plus the alternate-user
//! PowerShell smoke on the hosted Windows runner.
//!
//! Consumed by the connector loop on Windows, which retires this allow once
//! the endpoint is wired to the running service.
#![allow(dead_code)]

use core::ffi::c_void;
use std::path::Path;
use std::time::Duration;

use super::IpcError;

type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

// --- Win32 constants, matched to the SDK headers ---------------------------

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
const PIPE_READMODE_BYTE: u32 = 0x0000_0000;
const PIPE_WAIT: u32 = 0x0000_0000;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
/// Exactly one instance: the endpoint is a singleton per logon session.
const MAX_INSTANCES: u32 = 1;
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const DEFAULT_TIMEOUT_MS: u32 = 0;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_PIPE_BUSY: u32 = 231;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_IO_PENDING: u32 = 997;
const WAIT_TIMEOUT: u32 = 258;

const SDDL_REVISION_1: u32 = 1;
const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser` and `::TokenGroups`.
const TOKEN_USER_CLASS: i32 = 1;
const TOKEN_GROUPS_CLASS: i32 = 2;
/// `SE_GROUP_LOGON_ID` marks the logon-session SID inside `TokenGroups`.
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

/// How long a cancelled overlapped operation may take to reach terminal
/// completion before we give up on reclaiming its buffers. Cancellation is
/// prompt in practice; this bound exists so no path can block forever.
const CANCEL_DRAIN_MS: u32 = 5_000;

const ERROR_CANNOT_IMPERSONATE: u32 = 1368;
/// `CreateFileW` security-quality-of-service flags: the client states its
/// impersonation level instead of inheriting a default.
const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
const SECURITY_IMPERSONATION: u32 = 0x0002_0000;
/// How long the accept path waits for a connected client to become
/// impersonatable (see [`impersonate_client`]), and how often it retries.
const IMPERSONATE_WAIT_MS: u64 = 2_000;
const IMPERSONATE_POLL: Duration = Duration::from_millis(10);

// --- Win32 structures, matched to the SDK headers ---------------------------

// typedef struct _SECURITY_ATTRIBUTES { DWORD nLength;
//   LPVOID lpSecurityDescriptor; BOOL bInheritHandle; } SECURITY_ATTRIBUTES;
#[repr(C)]
struct SecurityAttributes {
    length: u32,
    security_descriptor: *mut c_void,
    inherit_handle: i32,
}

// typedef struct _OVERLAPPED { ULONG_PTR Internal; ULONG_PTR InternalHigh;
//   union { struct { DWORD Offset; DWORD OffsetHigh; }; PVOID Pointer; };
//   HANDLE hEvent; } OVERLAPPED;
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: Handle,
}

// typedef struct _SID_AND_ATTRIBUTES { PSID Sid; DWORD Attributes; }
#[repr(C)]
#[derive(Clone, Copy)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

// typedef struct _TOKEN_USER { SID_AND_ATTRIBUTES User; } TOKEN_USER;
#[repr(C)]
struct TokenUser {
    user: SidAndAttributes,
}

// typedef struct _TOKEN_GROUPS { DWORD GroupCount;
//   SID_AND_ATTRIBUTES Groups[ANYSIZE_ARRAY]; } TOKEN_GROUPS;
#[repr(C)]
struct TokenGroups {
    group_count: u32,
    groups: [SidAndAttributes; 1],
}

// --- Win32 declarations -----------------------------------------------------

#[link(name = "kernel32")]
extern "system" {
    // HANDLE CreateNamedPipeW(LPCWSTR, DWORD, DWORD, DWORD, DWORD, DWORD, DWORD,
    //                         LPSECURITY_ATTRIBUTES);
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        security_attributes: *const SecurityAttributes,
    ) -> Handle;
    // BOOL ConnectNamedPipe(HANDLE, LPOVERLAPPED);
    fn ConnectNamedPipe(pipe: Handle, overlapped: *mut Overlapped) -> i32;
    // BOOL DisconnectNamedPipe(HANDLE);
    fn DisconnectNamedPipe(pipe: Handle) -> i32;
    // HANDLE CreateFileW(LPCWSTR, DWORD, DWORD, LPSECURITY_ATTRIBUTES, DWORD,
    //                    DWORD, HANDLE);
    fn CreateFileW(
        name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *const SecurityAttributes,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;
    // BOOL SetNamedPipeHandleState(HANDLE, LPDWORD, LPDWORD, LPDWORD);
    fn SetNamedPipeHandleState(
        pipe: Handle,
        mode: *const u32,
        max_collection_count: *const u32,
        collect_data_timeout: *const u32,
    ) -> i32;
    // BOOL ReadFile(HANDLE, LPVOID, DWORD, LPDWORD, LPOVERLAPPED);
    fn ReadFile(
        file: Handle,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    // BOOL WriteFile(HANDLE, LPCVOID, DWORD, LPDWORD, LPOVERLAPPED);
    fn WriteFile(
        file: Handle,
        buffer: *const c_void,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    // BOOL FlushFileBuffers(HANDLE);
    fn FlushFileBuffers(file: Handle) -> i32;
    // HANDLE CreateEventW(LPSECURITY_ATTRIBUTES, BOOL, BOOL, LPCWSTR);
    fn CreateEventW(
        security_attributes: *const SecurityAttributes,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> Handle;
    // BOOL GetOverlappedResultEx(HANDLE, LPOVERLAPPED, LPDWORD, DWORD, BOOL);
    fn GetOverlappedResultEx(
        file: Handle,
        overlapped: *mut Overlapped,
        bytes_transferred: *mut u32,
        milliseconds: u32,
        alertable: i32,
    ) -> i32;
    // BOOL CancelIoEx(HANDLE, LPOVERLAPPED);
    fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> i32;
    // HANDLE GetCurrentProcess(void);
    fn GetCurrentProcess() -> Handle;
    // HANDLE GetCurrentThread(void);
    fn GetCurrentThread() -> Handle;
    // DWORD GetLastError(void);
    fn GetLastError() -> u32;
    // BOOL CloseHandle(HANDLE);
    fn CloseHandle(object: Handle) -> i32;
    // HLOCAL LocalFree(HLOCAL);
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

#[link(name = "advapi32")]
extern "system" {
    // BOOL OpenProcessToken(HANDLE, DWORD, PHANDLE);
    fn OpenProcessToken(process: Handle, desired_access: u32, token: *mut Handle) -> i32;
    // BOOL OpenThreadToken(HANDLE, DWORD, BOOL, PHANDLE);
    fn OpenThreadToken(
        thread: Handle,
        desired_access: u32,
        open_as_self: i32,
        token: *mut Handle,
    ) -> i32;
    // BOOL GetTokenInformation(HANDLE, TOKEN_INFORMATION_CLASS, LPVOID, DWORD,
    //                          PDWORD);
    fn GetTokenInformation(
        token: Handle,
        information_class: i32,
        information: *mut c_void,
        information_length: u32,
        return_length: *mut u32,
    ) -> i32;
    // BOOL ImpersonateNamedPipeClient(HANDLE);
    fn ImpersonateNamedPipeClient(pipe: Handle) -> i32;
    // BOOL RevertToSelf(void);
    fn RevertToSelf() -> i32;
    // BOOL EqualSid(PSID, PSID);
    fn EqualSid(first: *const c_void, second: *const c_void) -> i32;
    // BOOL ConvertSidToStringSidW(PSID, LPWSTR *);
    fn ConvertSidToStringSidW(sid: *const c_void, string_sid: *mut *mut u16) -> i32;
    // BOOL ConvertStringSecurityDescriptorToSecurityDescriptorW(LPCWSTR, DWORD,
    //   PSECURITY_DESCRIPTOR *, PULONG);
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: *const u16,
        revision: u32,
        security_descriptor: *mut *mut c_void,
        security_descriptor_size: *mut u32,
    ) -> i32;
}

// --- RAII ------------------------------------------------------------------

/// Owns one kernel handle and closes it exactly once.
struct OwnedHandle(Handle);

impl OwnedHandle {
    fn raw(&self) -> Handle {
        self.0
    }
}

// Safe: a Win32 handle is a process-wide kernel reference with no thread
// affinity, and `OwnedHandle` is the sole owner, so moving one between threads
// cannot duplicate or invalidate it.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
            // Safe: we own this handle and drop runs once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Owns one `LocalAlloc`-family allocation (a converted security descriptor or
/// SID string) and frees it exactly once.
struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Safe: the pointer came from a Convert*W call that documents
            // LocalFree as its release function, and drop runs once.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Reverts impersonation on every path, including panics and early returns.
struct Impersonation;

impl Drop for Impersonation {
    fn drop(&mut self) {
        // Safe: no arguments; reverting when not impersonating is harmless.
        unsafe { RevertToSelf() };
    }
}

/// An overlapped operation and its dedicated manual-reset event.
struct Operation {
    overlapped: Overlapped,
    _event: OwnedHandle,
}

impl Operation {
    fn new() -> Result<Self, IpcError> {
        // Safe: a manual-reset, initially unsignalled, unnamed event.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(IpcError::Internal);
        }
        Ok(Self {
            overlapped: Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event,
            },
            _event: OwnedHandle(event),
        })
    }
}

// --- identity ---------------------------------------------------------------

fn last_error() -> u32 {
    // Safe: no arguments.
    unsafe { GetLastError() }
}

/// Read one token information class into an 8-byte-aligned buffer. `Vec<u64>`
/// backing guarantees the alignment every Win32 token structure requires.
fn token_information(token: Handle, class: i32) -> Result<Vec<u64>, IpcError> {
    let mut needed: u32 = 0;
    // Safe: a null buffer with zero length is the documented size query.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(reject_peer("token-size"));
    }
    let error = last_error();
    if error != ERROR_INSUFFICIENT_BUFFER {
        return Err(reject_peer("token-size"));
    }
    let words = (needed as usize).div_ceil(8);
    let mut buffer = vec![0u64; words];
    // Safe: the buffer is at least `needed` bytes, correctly aligned, and the
    // call only writes into it.
    let ok = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            (words * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(reject_peer("token-read"));
    }
    Ok(buffer)
}

/// The SID string (`S-1-5-…`) of a SID pointer that lives in `buffer`.
fn sid_string(sid: *const c_void) -> Result<String, IpcError> {
    let mut raw: *mut u16 = std::ptr::null_mut();
    // Safe: `sid` points into a live token buffer; the out-pointer is local.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut raw) };
    if ok == 0 || raw.is_null() {
        return Err(IpcError::Internal);
    }
    let owned = LocalAllocation(raw.cast());
    let mut length = 0usize;
    // Safe: the returned string is NUL-terminated by contract.
    while unsafe { *raw.add(length) } != 0 {
        length += 1;
    }
    // Safe: `length` counted the units before the terminator.
    let units = unsafe { std::slice::from_raw_parts(raw, length) };
    let text = String::from_utf16(units).map_err(|_| IpcError::Internal)?;
    drop(owned);
    Ok(text)
}

/// The connector process token's user SID, as a string.
fn process_user_sid() -> Result<String, IpcError> {
    let mut token: Handle = std::ptr::null_mut();
    // Safe: pseudo-handle, query access only, local out-pointer.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return Err(IpcError::Internal);
    }
    let token = OwnedHandle(token);
    let buffer = token_information(token.raw(), TOKEN_USER_CLASS)?;
    // Safe: TokenUser fills the buffer with a TOKEN_USER whose SID points into it.
    let user = unsafe { &*(buffer.as_ptr() as *const TokenUser) };
    sid_string(user.user.sid)
}

/// The current logon-session SID, if the process token carries one. Interactive
/// logons always do; a service token may not, in which case the caller falls
/// back to the user SID (still same-user only, just not session-scoped).
fn logon_sid() -> Result<Option<String>, IpcError> {
    let mut token: Handle = std::ptr::null_mut();
    // Safe: pseudo-handle, query access only, local out-pointer.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return Err(IpcError::Internal);
    }
    let token = OwnedHandle(token);
    let buffer = token_information(token.raw(), TOKEN_GROUPS_CLASS)?;
    // Safe: TokenGroups fills the buffer with a TOKEN_GROUPS header followed by
    // `group_count` SID_AND_ATTRIBUTES entries, all inside the same allocation.
    let groups = unsafe { &*(buffer.as_ptr() as *const TokenGroups) };
    let count = groups.group_count as usize;
    let entries = unsafe { std::slice::from_raw_parts(groups.groups.as_ptr(), count) };
    for entry in entries {
        // `SE_GROUP_LOGON_ID` is two bits, so a test for "either bit" would
        // accept a group that carries only one of them and hand the endpoint's
        // ACE to the wrong subject. Masked equality asks for both, while
        // ignoring the enabled/mandatory attributes the real entry also sets.
        if (entry.attributes & SE_GROUP_LOGON_ID) == SE_GROUP_LOGON_ID {
            return Ok(Some(sid_string(entry.sid)?));
        }
    }
    Ok(None)
}

/// The SID the endpoint's DACL grants: the logon session when there is one,
/// otherwise the process user. Public so a gate can name the subject of the
/// ACE it is asserting about; it reveals only the calling process's own
/// identity, which that process can read anyway.
pub fn endpoint_sid() -> Result<String, IpcError> {
    match logon_sid()? {
        Some(sid) => Ok(sid),
        None => process_user_sid(),
    }
}

// --- naming and security ----------------------------------------------------

/// `D:P` is a *protected* DACL: no inherited ACE can widen it. The single ACE
/// grants `GA` (all access) to one SID and nobody else — no `Everyone` (`WD`),
/// no `Anonymous` (`AN`), no default descriptor.
pub fn security_descriptor_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

/// The endpoint name for a global root and its owning SID. Both are digested so
/// no path or identity is exposed in a name any process can enumerate.
pub fn pipe_name_for(run_dir: &Path, sid: &str) -> String {
    let mut digest = crate::sha256::Sha256::default();
    digest.update(run_dir.to_string_lossy().as_bytes());
    digest.update(b"\0");
    digest.update(sid.as_bytes());
    let digest = digest.finish();
    format!("\\\\.\\pipe\\loam-connector-{}", &digest[..32])
}

/// Only the local-machine pipe namespace is addressable. A UNC name such as
/// `\\host\pipe\…` is refused before any handle is opened, so a remote endpoint
/// can never be dialled even by a caller that supplies its own name.
pub fn is_local_pipe_name(name: &str) -> bool {
    name.starts_with("\\\\.\\pipe\\") && name.len() > "\\\\.\\pipe\\".len()
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

// --- endpoint ---------------------------------------------------------------

/// The owner-only named-pipe endpoint. Dropping it closes the pipe instance,
/// which releases the name for a later connector.
pub struct OwnedEndpoint {
    pipe: OwnedHandle,
    name: String,
    /// The converted security descriptor must outlive the pipe handle.
    _security_descriptor: LocalAllocation,
}

impl OwnedEndpoint {
    pub fn pipe_name(&self) -> &str {
        &self.name
    }

    /// Wait for a client, prove its token SID, and hand back a byte stream.
    /// The proof runs here, so a caller holding a [`VerifiedConn`] cannot read a
    /// frame from an unproven peer.
    pub fn accept_verified(&self, deadline: Duration) -> Result<VerifiedConn<'_>, IpcError> {
        self.connect_client(deadline)?;
        match verify_peer(self.pipe.raw()) {
            Ok(()) => Ok(VerifiedConn {
                endpoint: self,
                deadline,
            }),
            Err(error) => {
                self.disconnect();
                Err(error)
            }
        }
    }

    /// One bounded overlapped `ConnectNamedPipe`.
    fn connect_client(&self, deadline: Duration) -> Result<(), IpcError> {
        let mut operation = Operation::new()?;
        // Safe: the pipe handle is live and the OVERLAPPED outlives the call
        // below, which either completes it or cancels and drains it.
        let started = unsafe { ConnectNamedPipe(self.pipe.raw(), &mut operation.overlapped) };
        if started != 0 {
            return Ok(());
        }
        match last_error() {
            // The client connected between CreateNamedPipeW and ConnectNamedPipe.
            ERROR_PIPE_CONNECTED => Ok(()),
            ERROR_IO_PENDING => {
                finish(self.pipe.raw(), &mut operation.overlapped, deadline).map(|_| ())
            }
            _ => Err(IpcError::Internal),
        }
    }

    fn disconnect(&self) {
        // Safe: the handle is live; both calls are idempotent enough to run on
        // any teardown path.
        unsafe {
            FlushFileBuffers(self.pipe.raw());
            DisconnectNamedPipe(self.pipe.raw());
        }
    }
}

/// A connection whose client token SID has already been proven equal to the
/// connector's. Exposes only `Read`/`Write`, never the pipe handle.
pub struct VerifiedConn<'a> {
    endpoint: &'a OwnedEndpoint,
    deadline: Duration,
}

impl VerifiedConn<'_> {
    /// Use a different deadline for reads and writes than the one the accept
    /// used — the codec's read deadline is much shorter than a lifecycle wait.
    pub fn with_io_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }
}

impl Drop for VerifiedConn<'_> {
    fn drop(&mut self) {
        self.endpoint.disconnect();
    }
}

impl std::io::Read for VerifiedConn<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut operation = Operation::new().map_err(io_error)?;
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // Safe: the buffer and OVERLAPPED both live until `finish` returns,
        // which cancels and drains before giving up on them.
        let started = unsafe {
            ReadFile(
                self.endpoint.pipe.raw(),
                buffer.as_mut_ptr().cast(),
                length,
                std::ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        if started == 0 && last_error() != ERROR_IO_PENDING {
            return Err(io_error(IpcError::Internal));
        }
        finish(
            self.endpoint.pipe.raw(),
            &mut operation.overlapped,
            self.deadline,
        )
        .map(|transferred| transferred as usize)
        .map_err(io_error)
    }
}

impl std::io::Write for VerifiedConn<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let mut operation = Operation::new().map_err(io_error)?;
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // Safe: same lifetime argument as `read`.
        let started = unsafe {
            WriteFile(
                self.endpoint.pipe.raw(),
                buffer.as_ptr().cast(),
                length,
                std::ptr::null_mut(),
                &mut operation.overlapped,
            )
        };
        if started == 0 && last_error() != ERROR_IO_PENDING {
            return Err(io_error(IpcError::Internal));
        }
        finish(
            self.endpoint.pipe.raw(),
            &mut operation.overlapped,
            self.deadline,
        )
        .map(|transferred| transferred as usize)
        .map_err(io_error)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Safe: the handle is live.
        if unsafe { FlushFileBuffers(self.endpoint.pipe.raw()) } == 0 {
            return Err(io_error(IpcError::Internal));
        }
        Ok(())
    }
}

fn io_error(error: IpcError) -> std::io::Error {
    let kind = match error {
        IpcError::Timeout => std::io::ErrorKind::TimedOut,
        IpcError::UnauthorizedPeer => std::io::ErrorKind::PermissionDenied,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, error.code())
}

/// Await one overlapped operation. On timeout, cancel it and **wait for its
/// terminal completion** before returning, so the caller may free the
/// `OVERLAPPED` and its buffer with no pending kernel writes outstanding.
fn finish(
    handle: Handle,
    overlapped: &mut Overlapped,
    deadline: Duration,
) -> Result<u32, IpcError> {
    let milliseconds = u32::try_from(deadline.as_millis()).unwrap_or(u32::MAX);
    let mut transferred: u32 = 0;
    // Safe: the handle and OVERLAPPED are live for the whole call.
    let completed =
        unsafe { GetOverlappedResultEx(handle, overlapped, &mut transferred, milliseconds, 0) };
    if completed != 0 {
        return Ok(transferred);
    }
    if last_error() != WAIT_TIMEOUT {
        return Err(IpcError::Internal);
    }
    // Safe: cancellation only *requests* completion; the drain below is what
    // makes the buffers reclaimable.
    unsafe { CancelIoEx(handle, overlapped) };
    let mut drained: u32 = 0;
    // Safe: same live handle and OVERLAPPED.
    let returned =
        unsafe { GetOverlappedResultEx(handle, overlapped, &mut drained, CANCEL_DRAIN_MS, 0) };
    if returned != 0 {
        // The operation completed before the cancellation reached it — a client
        // connected, or bytes moved, in that window. That is a real completion,
        // not a timeout: dropping it would disconnect a peer that is already
        // attached.
        return Ok(drained);
    }
    if !drain_reached_terminal_completion(returned, last_error()) {
        // The kernel may still write into this OVERLAPPED and into the caller's
        // buffer, and returning would free both. There is no bounded amount of
        // further waiting that makes that safe, so end the process instead:
        // "never free while I/O is in flight" stays absolute, not best-effort.
        std::process::abort();
    }
    Err(IpcError::Timeout)
}

/// Whether a post-`CancelIoEx` `GetOverlappedResultEx` proves the operation is
/// terminally complete. Success is terminal; so is any failure other than
/// `WAIT_TIMEOUT` (a cancelled operation reports `ERROR_OPERATION_ABORTED`).
/// `WAIT_TIMEOUT` alone means the request is still pending in the kernel.
/// `WAIT_IO_COMPLETION` cannot occur: every wait here is non-alertable.
fn drain_reached_terminal_completion(returned: i32, error: u32) -> bool {
    returned != 0 || error != WAIT_TIMEOUT
}

/// A rejected peer is a security event, so each rejection names the stage that
/// produced it and the Win32 error behind it. Bounded and value-free: no
/// payload, path, or SID is ever printed.
fn reject_peer(stage: &str) -> IpcError {
    eprintln!(
        "loam ipc: peer rejected at {stage} (win32 {})",
        last_error()
    );
    IpcError::UnauthorizedPeer
}

/// Impersonate the connected client, waiting for the moment impersonation
/// becomes possible. On a byte-mode pipe the client's identity is not available
/// until it has written its first bytes, so a client that connects and *then*
/// writes loses a race the server has to absorb: `ImpersonateNamedPipeClient`
/// fails with `ERROR_CANNOT_IMPERSONATE` until then. The wait is bounded and
/// reads nothing — a peer that never writes is rejected, and the codec still
/// cannot run before the SID proof.
fn impersonate_client(pipe: Handle) -> Result<(), IpcError> {
    let give_up = std::time::Instant::now() + Duration::from_millis(IMPERSONATE_WAIT_MS);
    loop {
        // Safe: the pipe has a connected client.
        if unsafe { ImpersonateNamedPipeClient(pipe) } != 0 {
            return Ok(());
        }
        if last_error() != ERROR_CANNOT_IMPERSONATE || std::time::Instant::now() >= give_up {
            return Err(reject_peer("impersonate"));
        }
        std::thread::sleep(IMPERSONATE_POLL);
    }
}

/// Impersonate the connected client, read its token user SID, revert, and
/// require SID equality with the connector process. Every failure is a
/// rejection: the server never continues under its own token.
fn verify_peer(pipe: Handle) -> Result<(), IpcError> {
    impersonate_client(pipe)?;
    // From here every exit reverts, including the error paths below.
    let _revert = Impersonation;

    let mut token: Handle = std::ptr::null_mut();
    // `open_as_self` is TRUE so the token is opened with the *process* security
    // context rather than the client's, which is the documented pattern while
    // impersonating.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    if opened == 0 {
        return Err(reject_peer("open-thread-token"));
    }
    let token = OwnedHandle(token);
    let client = token_information(token.raw(), TOKEN_USER_CLASS)?;
    // Safe: TokenUser fills the buffer with a TOKEN_USER pointing into it.
    let client_sid = unsafe { &*(client.as_ptr() as *const TokenUser) }.user.sid;

    let mut process_token: Handle = std::ptr::null_mut();
    // Safe: pseudo-handle, query access only, local out-pointer.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut process_token) };
    if ok == 0 {
        return Err(reject_peer("open-process-token"));
    }
    let process_token = OwnedHandle(process_token);
    let server = token_information(process_token.raw(), TOKEN_USER_CLASS)?;
    // Safe: as above.
    let server_sid = unsafe { &*(server.as_ptr() as *const TokenUser) }.user.sid;

    // Safe: both SIDs point into live, correctly aligned token buffers.
    if unsafe { EqualSid(client_sid, server_sid) } == 0 {
        return Err(reject_peer("sid-mismatch"));
    }
    Ok(())
}

/// Create the endpoint for `run_dir`: first instance only, remote clients
/// rejected, and a protected DACL naming only the current logon SID.
pub fn bind(run_dir: &Path) -> Result<OwnedEndpoint, IpcError> {
    let sid = endpoint_sid()?;
    let name = pipe_name_for(run_dir, &sid);
    let sddl = wide(&security_descriptor_sddl(&sid));

    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // Safe: the SDDL string is NUL-terminated and the out-pointer is local; the
    // resulting descriptor is owned by LocalAllocation below.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 || descriptor.is_null() {
        return Err(IpcError::Internal);
    }
    let descriptor = LocalAllocation(descriptor);

    let attributes = SecurityAttributes {
        length: core::mem::size_of::<SecurityAttributes>() as u32,
        security_descriptor: descriptor.0,
        inherit_handle: 0,
    };
    let wide_name = wide(&name);
    // Safe: the name and attributes outlive the call; the descriptor outlives
    // the returned handle because both move into OwnedEndpoint.
    let pipe = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            MAX_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            DEFAULT_TIMEOUT_MS,
            &attributes,
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        // A live first instance elsewhere means another connector owns the
        // endpoint; anything else is a real failure.
        return Err(match last_error() {
            ERROR_ACCESS_DENIED | ERROR_ALREADY_EXISTS | ERROR_PIPE_BUSY => IpcError::Busy,
            _ => IpcError::Internal,
        });
    }
    Ok(OwnedEndpoint {
        pipe: OwnedHandle(pipe),
        name,
        _security_descriptor: descriptor,
    })
}

/// Open the local endpoint as a client. Used by the CLI side and by the
/// same-user positive control; a non-local pipe name is refused outright.
pub fn connect(pipe_name: &str) -> Result<ClientConn, IpcError> {
    if !is_local_pipe_name(pipe_name) {
        return Err(IpcError::UnauthorizedPeer);
    }
    let wide_name = wide(pipe_name);
    // Safe: the name outlives the call; no sharing, no template handle.
    let handle = unsafe {
        CreateFileW(
            wide_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            // Ask explicitly for an impersonation-capable connection. Without
            // SECURITY_SQOS_PRESENT the level is whatever the system defaults
            // to, and an identification- or anonymous-level client cannot be
            // proven by the server's SID check.
            SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(match last_error() {
            ERROR_PIPE_BUSY => IpcError::Busy,
            ERROR_ACCESS_DENIED => IpcError::UnauthorizedPeer,
            ERROR_INVALID_HANDLE => IpcError::Internal,
            _ => IpcError::Internal,
        });
    }
    let handle = OwnedHandle(handle);
    let mode = PIPE_READMODE_BYTE | PIPE_WAIT;
    // Safe: the handle is live and the mode is a local value the call only reads.
    if unsafe { SetNamedPipeHandleState(handle.raw(), &mode, std::ptr::null(), std::ptr::null()) }
        == 0
    {
        return Err(IpcError::Internal);
    }
    Ok(ClientConn { handle })
}

/// The client half: synchronous byte I/O over an already-opened local pipe.
pub struct ClientConn {
    handle: OwnedHandle,
}

impl std::io::Read for ClientConn {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut read: u32 = 0;
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // Safe: synchronous handle, so the call completes before returning.
        let ok = unsafe {
            ReadFile(
                self.handle.raw(),
                buffer.as_mut_ptr().cast(),
                length,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io_error(IpcError::Internal));
        }
        Ok(read as usize)
    }
}

impl std::io::Write for ClientConn {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let mut written: u32 = 0;
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // Safe: synchronous handle, so the call completes before returning.
        let ok = unsafe {
            WriteFile(
                self.handle.raw(),
                buffer.as_ptr().cast(),
                length,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io_error(IpcError::Internal));
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Safe: the handle is live.
        if unsafe { FlushFileBuffers(self.handle.raw()) } == 0 {
            return Err(io_error(IpcError::Internal));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dacl_is_protected_and_names_exactly_one_sid() {
        let sddl = security_descriptor_sddl("S-1-5-5-0-123456");
        assert_eq!(sddl, "D:P(A;;GA;;;S-1-5-5-0-123456)");
        // No inherited widening, no world/anonymous ACE, exactly one ACE.
        assert!(sddl.starts_with("D:P("));
        assert_eq!(sddl.matches("(A;").count(), 1);
        for forbidden in ["WD", "AN", "BU", "AU"] {
            assert!(
                !sddl.contains(&format!(";{forbidden})")),
                "{forbidden} granted"
            );
        }
    }

    #[test]
    fn only_a_still_pending_drain_is_non_terminal() {
        const ERROR_OPERATION_ABORTED: u32 = 995;
        // Completed after cancellation, and the cancelled-op result, are both
        // terminal — the buffers are reclaimable.
        assert!(drain_reached_terminal_completion(1, 0));
        assert!(drain_reached_terminal_completion(
            0,
            ERROR_OPERATION_ABORTED
        ));
        assert!(drain_reached_terminal_completion(0, ERROR_INVALID_HANDLE));
        // Still pending: the caller must not free, so `finish` aborts.
        assert!(!drain_reached_terminal_completion(0, WAIT_TIMEOUT));
    }

    #[test]
    fn the_pipe_name_is_local_and_carries_no_path_or_identity() {
        let name = pipe_name_for(Path::new("C:\\Users\\example\\.loam"), "S-1-5-5-0-123456");
        assert!(is_local_pipe_name(&name));
        assert!(!name.contains("example"));
        assert!(!name.contains("S-1-5"));
        // Distinct roots and distinct SIDs both yield distinct endpoints.
        assert_ne!(
            name,
            pipe_name_for(Path::new("C:\\Users\\other\\.loam"), "S-1-5-5-0-123456")
        );
        assert_ne!(
            name,
            pipe_name_for(Path::new("C:\\Users\\example\\.loam"), "S-1-5-5-0-654321")
        );
    }

    #[test]
    fn a_remote_pipe_name_is_refused_before_any_handle_is_opened() {
        assert!(!is_local_pipe_name("\\\\host\\pipe\\loam-connector-abc"));
        assert!(!is_local_pipe_name("\\\\.\\pipe\\"));
        assert!(!is_local_pipe_name("loam-connector-abc"));
        assert_eq!(
            connect("\\\\host\\pipe\\loam-connector-abc").err(),
            Some(IpcError::UnauthorizedPeer)
        );
    }
}
