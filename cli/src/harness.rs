//! Slice D T2: the `loam hook <harness>` read path.
//!
//! One private CLI entry point behind every harness's session event. It reads
//! that harness's native event JSON on stdin, canonicalizes the workspace,
//! checks enrollment, asks the running connector for its bounded snapshot over
//! owner-authenticated local IPC, and writes that harness's native response
//! envelope on stdout.
//!
//! **Structurally publish-incapable.** This module imports no transport, no
//! envelope publish, and no emit symbol; it issues exactly one IPC operation —
//! [`Operation::SnapshotGet`], a read — and never reads a transcript field for
//! intent. `tests::the_read_path_has_no_publish_surface` asserts that at the
//! source level, in the crate-capability-guard style `envelope.rs` established.
//!
//! **Fail open, never fail closed.** This entry point replaces the retired Node
//! integration, which also produced the *baseline* Loam context every harness
//! depends on — the skill body, the native runtime command, and the workspace
//! state block. A missing, crashed, or version-mismatched connector therefore
//! costs the federation section only: the complete baseline is still emitted,
//! with federation marked degraded. Nothing here starts the service.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ipc::{self, IpcConfig, IpcError, Operation};
use crate::json::Value;

/// The four harnesses in `## Harness surface map`. A closed enum: an unknown id
/// is refused, never dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    OpenCode,
    Claude,
    Codex,
    Cursor,
}

impl Harness {
    pub fn parse(id: &str) -> Option<Harness> {
        match id {
            "opencode" => Some(Harness::OpenCode),
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "cursor" => Some(Harness::Cursor),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Harness::OpenCode => "opencode",
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Cursor => "cursor",
        }
    }

    /// Wrap one rendered context body in this harness's native response
    /// envelope. This is the entire per-harness surface: the body is identical
    /// across all four, and only the key spelling differs — Claude's camelCase
    /// `additionalContext` against Cursor's snake_case `additional_context`.
    pub fn envelope(&self, body: &str) -> String {
        match self {
            Harness::Claude => Value::Object(vec![(
                "hookSpecificOutput".into(),
                Value::Object(vec![
                    ("hookEventName".into(), Value::String("SessionStart".into())),
                    ("additionalContext".into(), Value::String(body.to_owned())),
                ]),
            )])
            .to_json(),
            Harness::Cursor => Value::Object(vec![(
                "additional_context".into(),
                Value::String(body.to_owned()),
            )])
            .to_json(),
            // OpenCode's in-process mapper prepends the plain body as a text
            // part. Codex's shape is unconfirmed until the T7 gate, so it gets
            // the same plain body rather than an invented envelope key.
            Harness::OpenCode | Harness::Codex => body.to_owned(),
        }
    }
}

/// The calibration knobs from the spec, injected rather than hardcoded because a
/// session-start hook competes with everything else injecting context.
#[derive(Debug, Clone)]
pub struct HookConfig {
    pub timeout: Duration,
    pub item_budget_bytes: usize,
    pub max_items: usize,
    pub max_frame_bytes: usize,
}

impl Default for HookConfig {
    fn default() -> Self {
        HookConfig {
            timeout: Duration::from_secs(2),
            item_budget_bytes: 4096,
            max_items: 5,
            max_frame_bytes: 256 * 1024,
        }
    }
}

/// Where this run finds its installation. Plain data: the crate capability guard
/// bars a stored callable, so nothing here is swappable behavior — only paths a
/// test can point somewhere else.
#[derive(Debug, Clone)]
pub struct HookPaths {
    pub global_root: PathBuf,
    pub skills_root: PathBuf,
    pub runtime: Option<PathBuf>,
    pub cwd: PathBuf,
}

impl HookPaths {
    pub fn from_env() -> HookPaths {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_default();
        HookPaths {
            global_root: absolute_env("LOAM_HOME")
                .unwrap_or_else(|| home.join(".agents").join("loam")),
            skills_root: absolute_env("LOAM_SKILLS_ROOT")
                .unwrap_or_else(|| home.join(".agents").join("skills")),
            runtime: std::env::current_exe().ok(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    fn registry(&self) -> PathBuf {
        self.global_root.join("loam.sqlite3")
    }

    fn run_dir(&self) -> PathBuf {
        self.global_root.join("run")
    }
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    let value = PathBuf::from(std::env::var_os(name)?);
    value.is_absolute().then_some(value)
}

/// Why a stdin frame was refused. Each maps to a bounded, value-free diagnostic:
/// the offending payload is never echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TooLarge,
    NotUtf8,
    NotJson,
    NotObject,
}

impl FrameError {
    pub fn code(&self) -> &'static str {
        match self {
            FrameError::TooLarge => "frame_too_large",
            FrameError::NotUtf8 => "frame_not_utf8",
            FrameError::NotJson => "frame_not_json",
            FrameError::NotObject => "frame_not_an_object",
        }
    }
}

/// Parse one harness event frame. The size is checked *before* the bytes are
/// interpreted, and no branch here allocates anything keyed by untrusted
/// content. An absent frame is an empty event, not a malformed one: a harness
/// that fires with no stdin must still receive its baseline context.
pub fn parse_frame(bytes: &[u8], config: &HookConfig) -> Result<Value, FrameError> {
    if bytes.len() > config.max_frame_bytes {
        return Err(FrameError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| FrameError::NotUtf8)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Vec::new()));
    }
    let value = crate::json::parse(trimmed).map_err(|_| FrameError::NotJson)?;
    match value {
        Value::Object(_) => Ok(value),
        _ => Err(FrameError::NotObject),
    }
}

/// The workspace the harness is reporting, mirroring the key inventory the
/// retired Node adapters accepted so no harness silently loses its root.
pub fn workspace_from_frame(frame: &Value, fallback: &Path) -> PathBuf {
    let candidate = frame
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| frame.get("workspaceRoot").and_then(Value::as_str))
        .or_else(|| {
            frame
                .get("workspace")
                .and_then(|w| w.get("root"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            frame
                .get("session")
                .and_then(|s| s.get("cwd"))
                .and_then(Value::as_str)
        });
    match candidate {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => fallback.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Federation section
// ---------------------------------------------------------------------------

/// What the federation half of the context can honestly say this run.
#[derive(Debug, Clone, PartialEq)]
pub enum Federation {
    /// The workspace resolves but is not enrolled in a project.
    Unenrolled,
    /// The connector could not answer. The reason is a stable code, never a
    /// path, payload, or remote URL.
    Degraded(&'static str),
    /// A bounded snapshot the connector served.
    Snapshot(Value),
}

/// Ask the connector for its snapshot. Any failure — absent endpoint, refused
/// peer, timeout, malformed response, protocol mismatch — degrades rather than
/// propagating, and no branch here starts the service.
fn request_body(workspace: &str) -> String {
    Value::Object(vec![
        ("version".into(), Value::Number("1".into())),
        ("request_id".into(), Value::String("hook".into())),
        ("workspace".into(), Value::String(workspace.to_owned())),
        (
            "operation".into(),
            Value::String(Operation::SnapshotGet.as_str().to_owned()),
        ),
        ("payload".into(), Value::Object(Vec::new())),
    ])
    .to_json()
}

fn query_snapshot(paths: &HookPaths, workspace: &str, config: &HookConfig) -> Federation {
    let request = request_body(workspace);
    let ipc_config = IpcConfig {
        read_deadline: config.timeout,
        lifecycle_deadline: config.timeout,
        ..IpcConfig::default()
    };
    let body = match call_connector(&paths.run_dir(), request.as_bytes(), &ipc_config) {
        Ok(body) => body,
        Err(IpcError::UnauthorizedPeer) => return Federation::Degraded("connector_unauthorized"),
        Err(IpcError::Timeout) => return Federation::Degraded("connector_timeout"),
        Err(_) => return Federation::Degraded("connector_unreachable"),
    };
    interpret_response(&body)
}

/// Interpret one connector response frame. Kept separate from the socket so the
/// malformed-response and version-mismatch classes are testable without a
/// running connector.
pub fn interpret_response(body: &[u8]) -> Federation {
    let Ok(text) = std::str::from_utf8(body) else {
        return Federation::Degraded("connector_malformed_response");
    };
    let Ok(value) = crate::json::parse(text) else {
        return Federation::Degraded("connector_malformed_response");
    };
    match value.get("version") {
        Some(Value::Number(literal)) if literal == "1" => {}
        Some(_) => return Federation::Degraded("connector_version_mismatch"),
        None => return Federation::Degraded("connector_malformed_response"),
    }
    match value.get("status").and_then(Value::as_str) {
        Some("ok") => match value.get("result") {
            Some(result @ Value::Object(_)) if result.get("items").is_some() => {
                Federation::Snapshot(result.clone())
            }
            _ => Federation::Degraded("connector_malformed_response"),
        },
        Some("error") => match value
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
        {
            Some("workspace_unenrolled") => Federation::Unenrolled,
            Some(_) => Federation::Degraded("connector_refused"),
            None => Federation::Degraded("connector_malformed_response"),
        },
        _ => Federation::Degraded("connector_malformed_response"),
    }
}

#[cfg(unix)]
fn call_connector(run_dir: &Path, request: &[u8], config: &IpcConfig) -> Result<Vec<u8>, IpcError> {
    let mut connection = ipc::unix::connect(run_dir, config.read_deadline)?;
    ipc::write_frame(&mut connection, request, config)?;
    ipc::read_frame(&mut connection, config)
}

#[cfg(windows)]
fn call_connector(run_dir: &Path, request: &[u8], config: &IpcConfig) -> Result<Vec<u8>, IpcError> {
    // The named-pipe client half is synchronous and carries no per-call
    // deadline, so the bound here is the connector's own response discipline
    // rather than a timer. If that ever proves insufficient, the fix belongs in
    // `ipc/windows.rs` beside the other overlapped-I/O handling, not here.
    let sid = ipc::windows::endpoint_sid()?;
    let name = ipc::windows::pipe_name_for(run_dir, &sid);
    let mut connection = ipc::windows::connect(&name)?;
    ipc::write_frame(&mut connection, request, config)?;
    ipc::read_frame(&mut connection, config)
}

/// Render the federation half of the context. T3 replaces the item body with the
/// shared injection-safe renderer and the default-DENY allowlist; the section
/// framing and the budget live here.
fn federation_section(federation: &Federation, config: &HookConfig) -> String {
    let mut lines = vec!["## Federation".to_owned(), String::new()];
    match federation {
        Federation::Unenrolled => {
            lines.push(
                "federation: unenrolled — this workspace has joined no project. Run `loam federation connect` to join one."
                    .to_owned(),
            );
        }
        Federation::Degraded(reason) => {
            lines.push(format!(
                "federation: degraded ({reason}) — collaboration state is unavailable this turn; the local context above is complete and current."
            ));
        }
        Federation::Snapshot(result) => {
            let items = result
                .get("items")
                .and_then(Value::as_array)
                .unwrap_or_default();
            let project = result
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            lines.push(format!(
                "federation: live · project: {} · items: {}",
                clean_text(project, 256),
                items.len()
            ));
            // Oldest first, and the oldest are what a budget overflow drops —
            // visibly, never silently.
            let dropped = items.len().saturating_sub(config.max_items);
            if dropped > 0 {
                lines.push(String::new());
                lines.push(format!(
                    "[loam:truncated] {dropped} older item(s) omitted to stay inside the context budget."
                ));
            }
            for item in items.iter().skip(dropped) {
                lines.push(String::new());
                lines.push(item_line(item, config.item_budget_bytes));
            }
        }
    }
    lines.join("\n")
}

fn item_line(item: &Value, budget: usize) -> String {
    let field = |key: &str| clean_text(item.get(key).and_then(Value::as_str).unwrap_or(""), 256);
    let sender = item
        .get("from")
        .and_then(|from| from.get("principal_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    format!(
        "- from {} · {} — {}",
        clean_text(sender, 256),
        field("type"),
        clean_text(
            item.get("summary").and_then(Value::as_str).unwrap_or(""),
            budget
        )
    )
}

// ---------------------------------------------------------------------------
// Baseline context — the half a down connector must never cost
// ---------------------------------------------------------------------------

const CONTEXT_LIMIT: usize = 4096;

/// Scrub control characters and bound the length, mirroring the retired
/// `context.mjs::cleanText` byte for byte so no harness sees a shape change.
fn clean_text(value: &str, limit: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\r' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect();
    if cleaned.chars().count() > limit {
        let head: String = cleaned.chars().take(limit.saturating_sub(1)).collect();
        format!("{head}…")
    } else {
        cleaned
    }
}

fn strip_frontmatter(content: &str) -> String {
    let trimmed = content
        .strip_prefix("---\n")
        .or(content.strip_prefix("---\r\n"));
    match trimmed.and_then(|rest| rest.split_once("\n---")) {
        Some((_, after)) => after.trim_start_matches(['\r', '\n']).trim().to_owned(),
        None => content.trim().to_owned(),
    }
}

fn quote_runtime(path: &Path) -> String {
    let value = clean_text(&path.display().to_string(), CONTEXT_LIMIT);
    if value.is_empty() {
        return String::new();
    }
    if cfg!(windows) {
        format!("& '{}'", value.replace('\'', "''"))
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn plugin_version(global_root: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(global_root.join("install.json")) else {
        return String::new();
    };
    crate::json::parse(&text)
        .ok()
        .and_then(|value| {
            value
                .get("plugin_version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn skill_body(skills_root: &Path) -> String {
    match std::fs::read_to_string(skills_root.join("loam-using").join("SKILL.md")) {
        Ok(content) => strip_frontmatter(&content),
        Err(_) => String::new(),
    }
}

/// The `## Workspace state` block, reproducing `context.mjs::formatWorkspaceState`
/// against the same `loam state` aggregate the Node path shelled out for.
fn workspace_state_section(workspace: &Path) -> String {
    let aggregate = crate::state::aggregate(workspace, true);
    let state = crate::json::parse(&aggregate).unwrap_or(Value::Object(Vec::new()));
    let display = clean_text(&workspace.display().to_string(), CONTEXT_LIMIT);
    let mut lines = vec![format!("Workspace: {display}")];

    let wiki_root = state.get("wiki_root").and_then(Value::as_str).unwrap_or("");
    let exists = matches!(state.get("exists"), Some(Value::Bool(true)));
    if exists && !wiki_root.is_empty() {
        let qmd_ready = matches!(state.get("qmd_ready"), Some(Value::Bool(true)));
        let mut wiki = vec![format!("Wiki: {}", clean_text(wiki_root, CONTEXT_LIMIT))];
        wiki.push(
            if qmd_ready {
                "qmd: ready"
            } else {
                "qmd: not installed"
            }
            .to_owned(),
        );
        let collection = state
            .get("collection")
            .and_then(Value::as_str)
            .unwrap_or("");
        if qmd_ready && !collection.is_empty() {
            wiki.push(format!(
                "collection: {}",
                clean_text(collection, CONTEXT_LIMIT)
            ));
        }
        lines.push(wiki.join(" · "));
    } else {
        lines.push("Wiki: none".to_owned());
    }

    let checkpoint_count = number(&state, "checkpoint_count");
    if checkpoint_count > 0 {
        if let Some(latest) = state.get("latest_checkpoint").filter(|v| !v.is_null()) {
            lines.push(format!(
                "Checkpoints: {checkpoint_count} (latest: \"{}\" — {})",
                clean_text(
                    latest.get("title").and_then(Value::as_str).unwrap_or(""),
                    CONTEXT_LIMIT
                ),
                clean_text(
                    latest
                        .get("captured_at")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    CONTEXT_LIMIT
                ),
            ));
        }
    }

    let drift = number(&state, "drift_count");
    if drift > 0 {
        lines.push(format!("Code graph drift: {drift}"));
    }

    if let Some(hints) = state.get("hints").and_then(Value::as_array) {
        if !hints.is_empty() {
            lines.push(String::new());
            lines.push("Signals:".to_owned());
            for hint in hints {
                lines.push(hint_line(hint));
            }
        }
    }

    format!("## Workspace state\n\n{}", lines.join("\n"))
}

fn hint_line(hint: &Value) -> String {
    let text = |key: &str| {
        clean_text(
            hint.get(key).and_then(Value::as_str).unwrap_or(""),
            CONTEXT_LIMIT,
        )
    };
    let evidence = match hint.get("evidence") {
        Some(Value::Object(entries)) if !entries.is_empty() => {
            let pairs: Vec<String> = entries
                .iter()
                .map(|(key, value)| {
                    let rendered = match value {
                        Value::String(text) => text.clone(),
                        other => other.to_json(),
                    };
                    format!("{key}: {}", clean_text(&rendered, CONTEXT_LIMIT))
                })
                .collect();
            format!(" ({})", pairs.join(", "))
        }
        _ => String::new(),
    };
    let command = match hint.get("command").and_then(Value::as_str) {
        Some(value) if !value.is_empty() => format!(" → {}", clean_text(value, CONTEXT_LIMIT)),
        _ => String::new(),
    };
    format!(
        "- [loam:hint] {} — {}{evidence}{command}",
        text("kind"),
        text("message")
    )
}

fn number(state: &Value, key: &str) -> i64 {
    match state.get(key) {
        Some(Value::Number(literal)) => literal.parse().unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

/// Build the complete context body for one hook invocation: the full baseline
/// first, then the federation section. The baseline is unconditional — it is
/// what the retired Node integration produced, and losing it because a broker
/// connector is down would be a strictly worse session than before federation
/// existed.
pub fn compose_body(
    paths: &HookPaths,
    config: &HookConfig,
    frame: &Value,
    federation_override: Option<Federation>,
) -> String {
    let workspace = workspace_from_frame(frame, &paths.cwd);
    let federation = federation_override
        .unwrap_or_else(|| resolve_federation(paths, config, workspace.as_path()));

    let version = plugin_version(&paths.global_root);
    let heading = if version.is_empty() {
        "You have loam.".to_owned()
    } else {
        format!("You have loam (v{}).", clean_text(&version, 128))
    };
    let command = match &paths.runtime {
        Some(path) => format!("Native runtime command: {}", quote_runtime(path)),
        None => String::new(),
    };

    let sections = [
        heading,
        skill_body(&paths.skills_root),
        command,
        workspace_state_section(&workspace),
        federation_section(&federation, config),
    ];
    let content = sections
        .iter()
        .filter(|section| !section.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("<LOAM_IMPORTANT>\n\n{content}\n\n</LOAM_IMPORTANT>")
}

/// Canonicalize, prove enrollment locally, then read the snapshot. Resolving
/// enrollment here means an unenrolled or non-Git workspace never opens the
/// endpoint at all — the cheapest possible "no federation" answer.
fn resolve_federation(paths: &HookPaths, config: &HookConfig, workspace: &Path) -> Federation {
    let Ok(physical) = crate::enrollment::PhysicalWorkspace::resolve(workspace) else {
        return Federation::Unenrolled;
    };
    let key = crate::enrollment::identity_key(&physical);
    let enrolled = match crate::enrollment::open_readonly(&paths.registry()) {
        Ok(Some(connection)) => matches!(crate::enrollment::lookup(&connection, &key), Ok(Some(_))),
        Ok(None) => false,
        Err(_) => return Federation::Degraded("registry_unreadable"),
    };
    if !enrolled {
        return Federation::Unenrolled;
    }
    query_snapshot(paths, &physical.display_path, config)
}

/// `loam hook <harness>` — read stdin, write the harness-native envelope on
/// stdout. Always exits 0 on a served harness: a hook that fails the session is
/// worse than a hook that says less.
pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(id) = args.next() else {
        usage();
        return 1;
    };
    let Some(harness) = Harness::parse(&id) else {
        usage();
        return 1;
    };
    let mut paths = HookPaths::from_env();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--workspace" => match args.next() {
                Some(value) => paths.cwd = PathBuf::from(value),
                None => {
                    usage();
                    return 1;
                }
            },
            _ => {
                usage();
                return 1;
            }
        }
    }

    let config = HookConfig::default();
    let mut input = Vec::new();
    // Bound the read itself: one byte over the limit is enough to refuse, so a
    // hostile producer cannot make this grow without bound.
    {
        use std::io::Read;
        let mut reader = std::io::stdin().take((config.max_frame_bytes + 1) as u64);
        if reader.read_to_end(&mut input).is_err() {
            return refuse(harness, "frame_unreadable");
        }
    }

    let frame = match parse_frame(&input, &config) {
        Ok(frame) => frame,
        Err(error) => return refuse(harness, error.code()),
    };

    println!(
        "{}",
        harness.envelope(&compose_body(&paths, &config, &frame, None))
    );
    0
}

/// A refused frame renders no payload and mutates nothing: the diagnostic is a
/// stable code on stderr and the harness receives an empty envelope, never a
/// half-built context assembled from input we could not parse.
fn refuse(harness: Harness, code: &str) -> i32 {
    eprintln!("loam hook: {code}");
    println!("{}", harness.envelope(""));
    0
}

fn usage() {
    eprintln!("Usage: loam hook <opencode|claude|codex|cursor> [--workspace <absolute-path>]");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> HookPaths {
        HookPaths {
            global_root: PathBuf::from("/nonexistent-loam-root"),
            skills_root: PathBuf::from("/nonexistent-skills-root"),
            runtime: Some(PathBuf::from("/opt/loam/bin/loam")),
            cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    #[test]
    fn the_read_path_has_no_publish_surface() {
        // The defining safety property of this slice, asserted structurally in
        // the style `envelope.rs` established: no publish, transport, or emit
        // symbol is reachable from the read path, and the only IPC operation it
        // names is the read.
        let source = include_str!("harness.rs");
        let production = source
            .split("mod tests {")
            .next()
            .expect("module should contain its test boundary");
        for forbidden in [
            "crate::transport",
            "crate::federation",
            "MqttTransport",
            "MqttSession",
            "DeliveryProcessor",
            "publish(",
            "emit(",
            "rumqttc",
            // Reading a transcript field for intent is the rejected
            // "let a hook reply when the text asks for one" alternative.
            "\"transcript\"",
            "\"messages\"",
        ] {
            assert!(
                !production.contains(forbidden),
                "the read path acquired a write surface: {forbidden}"
            );
        }
        // An ALLOWLIST, not a denylist: enumerate every `Operation::` the read
        // path names and require each one to be the read. A frozen list of
        // forbidden variant names goes green the moment a new write operation is
        // added to the enum — `Operation::FederationEmit` (T5) is exactly that
        // case — so the invariant is stated as "SnapshotGet is the only
        // reachable operation" and costs nothing to maintain.
        let named: Vec<&str> = production
            .match_indices("Operation::")
            .map(|(index, _)| {
                let rest = &production[index + "Operation::".len()..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                &rest[..end]
            })
            .collect();
        assert!(
            !named.is_empty(),
            "the read path must name the read operation explicitly"
        );
        for variant in &named {
            assert_eq!(
                *variant, "SnapshotGet",
                "the read path named a non-read IPC operation: Operation::{variant}"
            );
        }
        // The import must not pull the enum in under another name either.
        assert!(production.contains("Operation::SnapshotGet"));
        for reachable in ["crate::connector", "crate::service", "run_service"] {
            assert!(
                !production.contains(reachable),
                "the read path reached the connector's own surface: {reachable}"
            );
        }
    }

    #[test]
    fn every_harness_id_maps_and_unknown_ids_are_refused() {
        for id in ["opencode", "claude", "codex", "cursor"] {
            assert_eq!(Harness::parse(id).map(|h| h.as_str()), Some(id));
        }
        assert_eq!(Harness::parse("copilot"), None);
        assert_eq!(Harness::parse(""), None);
    }

    #[test]
    fn the_two_context_key_spellings_cannot_drift_into_each_other() {
        // The whole envelope-mapping surface: Claude's camelCase against
        // Cursor's snake_case, asserted together so a rename touches both.
        let claude = Harness::Claude.envelope("BODY");
        let cursor = Harness::Cursor.envelope("BODY");
        assert!(claude.contains("\"additionalContext\":\"BODY\""));
        assert!(!claude.contains("additional_context"));
        assert!(cursor.contains("\"additional_context\":\"BODY\""));
        assert!(!cursor.contains("additionalContext"));
        // The two plain-body harnesses carry the identical body, unwrapped.
        assert_eq!(Harness::OpenCode.envelope("BODY"), "BODY");
        assert_eq!(Harness::Codex.envelope("BODY"), "BODY");
    }

    #[test]
    fn frame_classes_are_refused_without_echoing_the_payload() {
        let config = HookConfig {
            max_frame_bytes: 32,
            ..HookConfig::default()
        };
        assert_eq!(parse_frame(&[b'x'; 33], &config), Err(FrameError::TooLarge));
        assert_eq!(
            parse_frame(&[0xff, 0xfe], &config),
            Err(FrameError::NotUtf8)
        );
        assert_eq!(parse_frame(b"{not json", &config), Err(FrameError::NotJson));
        assert_eq!(parse_frame(b"[1,2]", &config), Err(FrameError::NotObject));
        assert_eq!(
            parse_frame(b"\"text\"", &config),
            Err(FrameError::NotObject)
        );
        // An absent frame is an empty event: the harness still gets its
        // baseline rather than losing all Loam context to a silent producer.
        assert_eq!(parse_frame(b"", &config), Ok(Value::Object(Vec::new())));
        assert_eq!(
            parse_frame(b"  \n ", &config),
            Ok(Value::Object(Vec::new()))
        );
    }

    #[test]
    fn the_workspace_comes_from_every_key_the_node_adapters_accepted() {
        let fallback = Path::new("/fallback");
        let cases = [
            (r#"{"cwd":"/a"}"#, "/a"),
            (r#"{"workspaceRoot":"/b"}"#, "/b"),
            (r#"{"workspace":{"root":"/c"}}"#, "/c"),
            (r#"{"session":{"cwd":"/d"}}"#, "/d"),
            (r#"{}"#, "/fallback"),
            (r#"{"cwd":""}"#, "/fallback"),
        ];
        for (frame, expected) in cases {
            let parsed = crate::json::parse(frame).unwrap();
            assert_eq!(
                workspace_from_frame(&parsed, fallback),
                PathBuf::from(expected),
                "frame {frame}"
            );
        }
    }

    #[test]
    fn every_connector_failure_class_degrades_rather_than_propagating() {
        for (body, reason) in [
            (&b"not json"[..], "connector_malformed_response"),
            (&[0xff][..], "connector_malformed_response"),
            (
                br#"{"version":2,"request_id":"hook","status":"ok","result":{"items":[]}}"#
                    .as_slice(),
                "connector_version_mismatch",
            ),
            (
                br#"{"request_id":"hook","status":"ok","result":{"items":[]}}"#.as_slice(),
                "connector_malformed_response",
            ),
            (
                br#"{"version":1,"request_id":"hook","status":"ok","result":{}}"#.as_slice(),
                "connector_malformed_response",
            ),
            (
                br#"{"version":1,"request_id":"hook","status":"error","error":{"code":"busy","diagnostic":"busy"}}"#.as_slice(),
                "connector_refused",
            ),
        ] {
            assert_eq!(
                interpret_response(body),
                Federation::Degraded(reason),
                "body {}",
                String::from_utf8_lossy(body)
            );
        }

        assert_eq!(
            interpret_response(
                br#"{"version":1,"request_id":"hook","status":"error","error":{"code":"workspace_unenrolled","diagnostic":"x"}}"#
            ),
            Federation::Unenrolled
        );
        assert!(matches!(
            interpret_response(
                br#"{"version":1,"request_id":"hook","status":"ok","result":{"schema":1,"project_id":"p","items":[]}}"#
            ),
            Federation::Snapshot(_)
        ));
    }

    #[test]
    fn a_degraded_connector_still_emits_the_complete_baseline() {
        // The sharpest regression risk in the slice: retiring the Node
        // integration removed `formatContext`, so a degraded federation must
        // never cost the baseline every harness already depended on.
        let paths = paths();
        let frame = Value::Object(Vec::new());
        let body = compose_body(
            &paths,
            &HookConfig::default(),
            &frame,
            Some(Federation::Degraded("connector_unreachable")),
        );
        assert!(body.starts_with("<LOAM_IMPORTANT>"));
        assert!(body.ends_with("</LOAM_IMPORTANT>"));
        assert!(body.contains("You have loam"), "{body}");
        assert!(
            body.contains("Native runtime command: '/opt/loam/bin/loam'"),
            "{body}"
        );
        assert!(body.contains("## Workspace state"), "{body}");
        assert!(body.contains("Workspace: "), "{body}");
        assert!(
            body.contains("federation: degraded (connector_unreachable)"),
            "{body}"
        );
        // Not a federation-only stub: the baseline sections come first and the
        // federation section is the last of several, not the whole document.
        assert!(
            body.find("## Workspace state") < body.find("## Federation"),
            "{body}"
        );
    }

    #[test]
    fn the_budget_truncates_oldest_first_and_says_so() {
        let items: Vec<Value> = (0..8)
            .map(|index| {
                crate::json::parse(&format!(
                    r#"{{"source":"s","type":"io.loam.message","summary":"item-{index}","from":{{"principal_id":"employee-{index}"}}}}"#
                ))
                .unwrap()
            })
            .collect();
        let snapshot = Value::Object(vec![
            ("project_id".into(), Value::String("loam".into())),
            ("items".into(), Value::Array(items)),
        ]);
        let config = HookConfig {
            max_items: 3,
            item_budget_bytes: 64,
            ..HookConfig::default()
        };
        let section = federation_section(&Federation::Snapshot(snapshot.clone()), &config);
        assert!(section.contains("items: 8"), "{section}");
        assert!(
            section.contains("[loam:truncated] 5 older item(s) omitted"),
            "{section}"
        );
        // Oldest dropped — visibly — and newest kept.
        assert!(!section.contains("item-0"), "{section}");
        assert!(!section.contains("item-4"), "{section}");
        assert!(section.contains("item-5"), "{section}");
        assert!(section.contains("item-7"), "{section}");

        // The per-item budget clips rather than drops: the item is still there,
        // shortened, with the elision visible.
        let clipped = federation_section(
            &Federation::Snapshot(snapshot),
            &HookConfig {
                item_budget_bytes: 4,
                ..config
            },
        );
        assert!(clipped.contains("ite…"), "{clipped}");
        assert!(!clipped.contains("item-7"), "{clipped}");
    }

    #[cfg(unix)]
    #[test]
    fn the_client_speaks_the_protocol_and_an_absent_endpoint_only_degrades() {
        // The positive control for every degraded reason code below: unless the
        // client is shown talking to a real endpoint, "connector_unreachable"
        // could just as well mean "this code never worked".
        let run_dir = std::path::PathBuf::from("/tmp").join(format!(
            "loam-hook-client-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let endpoint = ipc::unix::bind(&run_dir).expect("bind");
        let config = IpcConfig::default();
        let server = std::thread::spawn(move || {
            let mut connection = endpoint.accept_verified().expect("accept");
            let request = ipc::read_frame(&mut connection, &IpcConfig::default()).expect("request");
            let parsed =
                crate::json::parse(std::str::from_utf8(&request).expect("utf8")).expect("json");
            // The one operation this path is allowed to issue.
            assert_eq!(
                parsed.get("operation").and_then(Value::as_str),
                Some("federation.snapshot")
            );
            let response = ipc::ok_response(
                "hook",
                crate::json::parse(r#"{"schema":1,"project_id":"loam","items":[]}"#).unwrap(),
            );
            ipc::write_frame(&mut connection, &response, &IpcConfig::default()).expect("respond");
            // Hold the endpoint until the client has read the response.
            std::thread::sleep(std::time::Duration::from_millis(50));
        });

        let body = call_connector(&run_dir, request_body("/w").as_bytes(), &config)
            .expect("the client reaches a real endpoint");
        assert!(matches!(interpret_response(&body), Federation::Snapshot(_)));
        server.join().expect("server thread");

        // Same client, no endpoint: degraded, never an error that reaches the
        // session and never a fabricated snapshot.
        let empty = std::path::PathBuf::from("/tmp").join("loam-hook-no-such-endpoint");
        let paths = HookPaths {
            global_root: empty,
            ..paths()
        };
        assert_eq!(
            query_snapshot(&paths, "/w", &HookConfig::default()),
            Federation::Degraded("connector_unreachable")
        );
    }

    #[test]
    fn frontmatter_is_stripped_and_control_characters_are_scrubbed() {
        assert_eq!(
            strip_frontmatter("---\nname: x\n---\n# Body\n\ntext\n"),
            "# Body\n\ntext"
        );
        assert_eq!(strip_frontmatter("# Body\n"), "# Body");
        assert_eq!(clean_text("a\u{0}b", 64), "a\u{fffd}b");
        assert_eq!(clean_text("abcdef", 3), "ab…");
        assert_eq!(clean_text("line\nbreak", 64), "line\nbreak");
    }
}
