//! Slice C local IPC v1: bounded length-prefixed framing, the request/response
//! model, the closed operation enum, and stable typed errors.
//!
//! This module is platform-independent: it owns the wire *format* and its
//! rejection rules, never a socket. The owner-authenticated Unix socket
//! (`ipc/unix.rs`, T6) and Windows named pipe (`ipc/windows.rs`, T7) bring their
//! own peer-credential proof and apply the deadlines this module's config
//! carries; they hand raw frames here only after that proof. Nothing in this
//! module trusts a caller: an oversized frame is refused before allocation, a
//! malformed request mutates nothing and reflects no payload, and diagnostics
//! are bounded and value-free.
//!
//! Consumed by the platform endpoints (T6/T7) and the connector loop (T9), which
//! retire this module-level allow once the codec is wired to a live socket.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::time::Duration;

use crate::json::Value;

/// Protocol version this build speaks.
pub const PROTOCOL_VERSION: i64 = 1;

/// Bounds and deadlines, injected so tests can shrink them. Defaults match the
/// connector spec: 256 KiB frames, a 250 ms read/status deadline, and a 2 s
/// attach/detach deadline.
#[derive(Debug, Clone)]
pub struct IpcConfig {
    pub max_frame: usize,
    pub read_deadline: Duration,
    pub lifecycle_deadline: Duration,
    pub max_request_id: usize,
    pub max_workspace: usize,
    pub max_diagnostic: usize,
}

impl Default for IpcConfig {
    fn default() -> Self {
        IpcConfig {
            max_frame: 256 * 1024,
            read_deadline: Duration::from_millis(250),
            lifecycle_deadline: Duration::from_secs(2),
            max_request_id: 128,
            max_workspace: 4096,
            max_diagnostic: 512,
        }
    }
}

/// The closed Slice C operation enum. Unknown or Slice-D/E-future operations
/// return [`IpcError::UnknownOperation`]; there is no string-to-handler registry
/// and no generic payload dispatch. T18 extends this enum by a named variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    StatusGet,
    ProjectAttach,
    ProjectDetach,
}

impl Operation {
    fn parse(name: &str) -> Option<Operation> {
        match name {
            "status.get" => Some(Operation::StatusGet),
            "project.attach" => Some(Operation::ProjectAttach),
            "project.detach" => Some(Operation::ProjectDetach),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::StatusGet => "status.get",
            Operation::ProjectAttach => "project.attach",
            Operation::ProjectDetach => "project.detach",
        }
    }
}

/// One bounded request. `request_id` is a diagnostic correlation only and never
/// selects a project or grants authority.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub request_id: String,
    pub workspace: String,
    pub operation: Operation,
    pub payload: Value,
}

/// Stable typed errors. Diagnostics attached elsewhere are bounded and never
/// echo the request payload, secrets, remote URLs, or untrusted paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcError {
    UnauthorizedPeer,
    UnsupportedVersion,
    MalformedFrame,
    FrameTooLarge,
    InvalidRequest,
    UnknownOperation,
    WorkspaceUnenrolled,
    ProjectBindingMismatch,
    Busy,
    Timeout,
    Internal,
}

impl IpcError {
    /// Stable machine code, safe to send on the wire.
    pub fn code(&self) -> &'static str {
        match self {
            IpcError::UnauthorizedPeer => "unauthorized_peer",
            IpcError::UnsupportedVersion => "unsupported_version",
            IpcError::MalformedFrame => "malformed_frame",
            IpcError::FrameTooLarge => "frame_too_large",
            IpcError::InvalidRequest => "invalid_request",
            IpcError::UnknownOperation => "unknown_operation",
            IpcError::WorkspaceUnenrolled => "workspace_unenrolled",
            IpcError::ProjectBindingMismatch => "project_binding_mismatch",
            IpcError::Busy => "busy",
            IpcError::Timeout => "timeout",
            IpcError::Internal => "internal",
        }
    }
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Read exactly one length-prefixed frame: a four-byte big-endian length, then
/// that many bytes. The length is checked against `max_frame` **before** any
/// body buffer is allocated, so an oversize or hostile length cannot force a
/// large allocation. A zero length or a truncated read is a malformed frame.
pub fn read_frame<R: Read>(reader: &mut R, config: &IpcConfig) -> Result<Vec<u8>, IpcError> {
    let mut length_bytes = [0u8; 4];
    read_exact(reader, &mut length_bytes)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 {
        return Err(IpcError::MalformedFrame);
    }
    if length > config.max_frame {
        return Err(IpcError::FrameTooLarge);
    }
    let mut body = vec![0u8; length];
    read_exact(reader, &mut body)?;
    Ok(body)
}

/// Write one length-prefixed frame. Refuses to frame a body larger than the
/// configured bound rather than emitting a length a reader must reject.
pub fn write_frame<W: Write>(
    writer: &mut W,
    body: &[u8],
    config: &IpcConfig,
) -> Result<(), IpcError> {
    if body.len() > config.max_frame {
        return Err(IpcError::FrameTooLarge);
    }
    let length = u32::try_from(body.len()).map_err(|_| IpcError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(|_| IpcError::MalformedFrame)?;
    writer
        .write_all(body)
        .map_err(|_| IpcError::MalformedFrame)?;
    Ok(())
}

fn read_exact<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<(), IpcError> {
    reader
        .read_exact(buffer)
        .map_err(|_| IpcError::MalformedFrame)
}

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

const REQUEST_KEYS: &[&str] = &["version", "request_id", "workspace", "operation", "payload"];

/// Parse one bounded request frame. Enforces UTF-8, JSON, the exact field
/// inventory (no duplicate or unknown key), protocol version, the closed
/// operation enum, and bounded diagnostic/workspace fields. Never allocates a
/// lookup keyed by untrusted content beyond the parsed value and never mutates
/// anything.
pub fn parse_request(bytes: &[u8], config: &IpcConfig) -> Result<Request, IpcError> {
    if bytes.len() > config.max_frame {
        return Err(IpcError::FrameTooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| IpcError::MalformedFrame)?;
    let value = crate::json::parse(text).map_err(|_| IpcError::MalformedFrame)?;
    let entries = match &value {
        Value::Object(entries) => entries,
        _ => return Err(IpcError::InvalidRequest),
    };
    check_exact_keys(entries)?;

    require_version(entries)?;
    let request_id = bounded_string(entries, "request_id", config.max_request_id)?;
    let workspace = bounded_string(entries, "workspace", config.max_workspace)?;
    let operation_name = string_field(entries, "operation")?;
    let operation = Operation::parse(&operation_name).ok_or(IpcError::UnknownOperation)?;
    let payload = match entries.iter().find(|(k, _)| k == "payload").map(|(_, v)| v) {
        Some(Value::Object(_)) => entries
            .iter()
            .find(|(k, _)| k == "payload")
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Null),
        Some(Value::Null) | None => Value::Object(vec![]),
        Some(_) => return Err(IpcError::InvalidRequest),
    };

    Ok(Request {
        request_id,
        workspace,
        operation,
        payload,
    })
}

fn check_exact_keys(entries: &[(String, Value)]) -> Result<(), IpcError> {
    for (index, (key, _)) in entries.iter().enumerate() {
        if entries[..index].iter().any(|(prior, _)| prior == key) {
            return Err(IpcError::InvalidRequest);
        }
        if !REQUEST_KEYS.contains(&key.as_str()) {
            return Err(IpcError::InvalidRequest);
        }
    }
    Ok(())
}

fn require_version(entries: &[(String, Value)]) -> Result<(), IpcError> {
    match entries.iter().find(|(k, _)| k == "version").map(|(_, v)| v) {
        Some(Value::Number(literal)) if literal == "1" => Ok(()),
        Some(Value::Number(_)) => Err(IpcError::UnsupportedVersion),
        _ => Err(IpcError::InvalidRequest),
    }
}

fn string_field(entries: &[(String, Value)], key: &str) -> Result<String, IpcError> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(IpcError::InvalidRequest)
}

fn bounded_string(entries: &[(String, Value)], key: &str, max: usize) -> Result<String, IpcError> {
    let value = string_field(entries, key)?;
    if value.is_empty() || value.len() > max || value.chars().any(|c| c.is_control()) {
        return Err(IpcError::InvalidRequest);
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Response building
// ---------------------------------------------------------------------------

/// Build a success response frame body carrying exactly one `result`.
pub fn ok_response(request_id: &str, result: Value) -> Vec<u8> {
    Value::Object(vec![
        (
            "version".into(),
            Value::Number(PROTOCOL_VERSION.to_string()),
        ),
        ("request_id".into(), Value::String(request_id.to_owned())),
        ("status".into(), Value::String("ok".into())),
        ("result".into(), result),
    ])
    .to_json()
    .into_bytes()
}

/// Build an error response frame body carrying exactly one `error`, with a
/// bounded diagnostic that never includes untrusted input. `request_id` may be
/// empty when the frame could not be parsed far enough to recover it.
pub fn error_response(request_id: &str, error: &IpcError, config: &IpcConfig) -> Vec<u8> {
    let mut diagnostic = error.code().to_owned();
    diagnostic.truncate(config.max_diagnostic);
    Value::Object(vec![
        (
            "version".into(),
            Value::Number(PROTOCOL_VERSION.to_string()),
        ),
        ("request_id".into(), Value::String(request_id.to_owned())),
        ("status".into(), Value::String("error".into())),
        (
            "error".into(),
            Value::Object(vec![
                ("code".into(), Value::String(error.code().to_owned())),
                ("diagnostic".into(), Value::String(diagnostic)),
            ]),
        ),
    ])
    .to_json()
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_request_bytes() -> Vec<u8> {
        br#"{"version":1,"request_id":"r-1","workspace":"/home/x/proj","operation":"status.get","payload":{}}"#
            .to_vec()
    }

    #[test]
    fn valid_request_round_trips() {
        let request = parse_request(&valid_request_bytes(), &IpcConfig::default()).expect("valid");
        assert_eq!(request.request_id, "r-1");
        assert_eq!(request.operation, Operation::StatusGet);
        assert_eq!(request.workspace, "/home/x/proj");
    }

    #[test]
    fn frame_round_trips_and_rejects_oversize_before_allocation() {
        let config = IpcConfig {
            max_frame: 64,
            ..IpcConfig::default()
        };
        let body = b"hello";
        let mut framed = Vec::new();
        write_frame(&mut framed, body, &config).expect("write");
        let mut cursor = std::io::Cursor::new(framed);
        assert_eq!(read_frame(&mut cursor, &config).unwrap(), body);

        // A length header claiming 1 MiB with the small bound must be refused
        // without reading (or allocating) a body.
        let mut hostile = (1024u32 * 1024).to_be_bytes().to_vec();
        hostile.extend_from_slice(b"not-that-long");
        let mut cursor = std::io::Cursor::new(hostile);
        assert_eq!(
            read_frame(&mut cursor, &config),
            Err(IpcError::FrameTooLarge)
        );
    }

    #[test]
    fn zero_length_and_truncated_frames_are_malformed() {
        let config = IpcConfig::default();
        let mut zero = std::io::Cursor::new(0u32.to_be_bytes().to_vec());
        assert_eq!(
            read_frame(&mut zero, &config),
            Err(IpcError::MalformedFrame)
        );

        let mut truncated = (10u32).to_be_bytes().to_vec();
        truncated.extend_from_slice(b"abc"); // only 3 of 10
        let mut cursor = std::io::Cursor::new(truncated);
        assert_eq!(
            read_frame(&mut cursor, &config),
            Err(IpcError::MalformedFrame)
        );
    }

    #[test]
    fn non_utf8_and_non_json_are_malformed() {
        let config = IpcConfig::default();
        assert_eq!(
            parse_request(&[0xff, 0xfe], &config),
            Err(IpcError::MalformedFrame)
        );
        assert_eq!(
            parse_request(b"{not json", &config),
            Err(IpcError::MalformedFrame)
        );
    }

    #[test]
    fn unknown_and_duplicate_fields_are_invalid() {
        let config = IpcConfig::default();
        let unknown = br#"{"version":1,"request_id":"r","workspace":"w","operation":"status.get","payload":{},"extra":1}"#;
        assert_eq!(
            parse_request(unknown, &config),
            Err(IpcError::InvalidRequest)
        );
        let duplicate = br#"{"version":1,"version":1,"request_id":"r","workspace":"w","operation":"status.get","payload":{}}"#;
        assert_eq!(
            parse_request(duplicate, &config),
            Err(IpcError::InvalidRequest)
        );
    }

    #[test]
    fn wrong_version_and_unknown_operation_have_distinct_errors() {
        let config = IpcConfig::default();
        let v2 = br#"{"version":2,"request_id":"r","workspace":"w","operation":"status.get","payload":{}}"#;
        assert_eq!(
            parse_request(v2, &config),
            Err(IpcError::UnsupportedVersion)
        );
        let unknown_op = br#"{"version":1,"request_id":"r","workspace":"w","operation":"session.register-inject","payload":{}}"#;
        assert_eq!(
            parse_request(unknown_op, &config),
            Err(IpcError::UnknownOperation)
        );
    }

    #[test]
    fn overlong_request_id_is_rejected() {
        let config = IpcConfig {
            max_request_id: 4,
            ..IpcConfig::default()
        };
        let long = br#"{"version":1,"request_id":"toolong","workspace":"w","operation":"status.get","payload":{}}"#;
        assert_eq!(parse_request(long, &config), Err(IpcError::InvalidRequest));
    }

    #[test]
    fn error_response_diagnostic_is_bounded_and_value_free() {
        let config = IpcConfig {
            max_diagnostic: 8,
            ..IpcConfig::default()
        };
        let body = error_response("r-1", &IpcError::ProjectBindingMismatch, &config);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("project_binding_mismatch"));
        // The diagnostic field itself is truncated to the bound.
        let parsed = crate::json::parse(&text).unwrap();
        let diag = parsed
            .get("error")
            .and_then(|e| e.get("diagnostic"))
            .and_then(Value::as_str)
            .unwrap();
        assert!(diag.len() <= 8);
    }

    #[test]
    fn responses_carry_exactly_one_of_result_or_error() {
        let ok = String::from_utf8(ok_response("r", Value::Object(vec![]))).unwrap();
        assert!(ok.contains("\"result\"") && !ok.contains("\"error\""));
        let err =
            String::from_utf8(error_response("r", &IpcError::Busy, &IpcConfig::default())).unwrap();
        assert!(err.contains("\"error\"") && !err.contains("\"result\""));
    }
}
