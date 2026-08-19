use crate::json::{self, Value};
use chrono::{SecondsFormat, TimeZone, Utc};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DATABASE_NAME: &str = "loam.sqlite3";
// A lifecycle batch rides one bounded stdin frame; producers project each event
// at its observation point so this stays diagnostic metadata, never a payload.
const EVENT_BATCH_MAX_BYTES: usize = 16 * 1024;
const EVENT_BATCH_MAX_EVENTS: usize = 16;
// This is a wait ceiling under contention, not a delay on uncontended operations.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
// The native read hook's own bookkeeping is best-effort and must never slow the
// hook: PreToolUse/PostToolUse fire on every tool boundary, so a native run
// record that waited the full BUSY_TIMEOUT on a locked ledger would stall the
// session. ponytail: short ceiling, dropped on contention — raise only if
// records are provably lost under normal load, never toward the 5s write budget.
const NATIVE_RECORD_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const SCHEMA_VERSION: i64 = 2;
const LEGACY_SCHEMA_VERSION: i64 = 1;
const RETENTION: i64 = 10_000;
const SUPPORTED_TARGETS: [&str; 5] = [
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
];

pub fn run(args: impl Iterator<Item = String>) -> i32 {
    match execute(args.collect()) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("loam hooks: {error}");
            1
        }
    }
}

fn execute(mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err(usage());
    }
    match args.remove(0).as_str() {
        "begin" => begin(parse_begin(args)?),
        "finish" => finish(parse_finish(args)?),
        "event" => record_event(parse_event(args)?),
        "worker-start" => worker_start(parse_worker_start(args)?),
        "worker-finish" => worker_finish(parse_worker_finish(args)?),
        "list" => list(parse_list(args)?),
        _ => Err(usage()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishStatus {
    Succeeded,
    Failed,
    Continued,
}

impl FinishStatus {
    fn parse(value: Option<String>) -> Result<Self, String> {
        match value.as_deref() {
            Some("succeeded") => Ok(Self::Succeeded),
            Some("failed") => Ok(Self::Failed),
            Some("continued") => Ok(Self::Continued),
            Some(_) => Err("status must be succeeded, failed, or continued".to_owned()),
            None => Err("missing --status".to_owned()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Continued => "continued",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishAction {
    SpawnWorker,
    Skip,
    RequestWorker,
}

impl FinishAction {
    fn parse(value: Option<String>) -> Result<Option<Self>, String> {
        value
            .map(|value| match value.as_str() {
                "spawn_worker" => Ok(Self::SpawnWorker),
                "skip" => Ok(Self::Skip),
                "request_worker" => Ok(Self::RequestWorker),
                _ => Err("action must be spawn_worker, skip, or request_worker".to_owned()),
            })
            .transpose()
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SpawnWorker => "spawn_worker",
            Self::Skip => "skip",
            Self::RequestWorker => "request_worker",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerOrigin {
    Direct,
    External,
    Fallback,
}

impl WorkerOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::External => "external",
            Self::Fallback => "fallback",
        }
    }
}

struct BeginArgs {
    root: PathBuf,
    harness: String,
    hook: String,
    workspace: String,
    plugin_version: String,
    session_id: Option<String>,
}

fn parse_begin(args: Vec<String>) -> Result<BeginArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut harness = None;
    let mut hook = None;
    let mut workspace = None;
    let mut plugin_version = None;
    let mut session_id = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--harness" => take_value(&mut harness, &flag, &mut args)?,
            "--hook" => take_value(&mut hook, &flag, &mut args)?,
            "--workspace" => take_value(&mut workspace, &flag, &mut args)?,
            "--plugin-version" => take_value(&mut plugin_version, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let harness = valid_identifier(harness, "harness")?;
    let hook = valid_identifier(hook, "hook")?;
    let workspace = absolute_path(workspace, "workspace")?
        .into_os_string()
        .into_string()
        .map_err(|_| "workspace must be valid UTF-8".to_owned())?;
    let plugin_version = plugin_version.ok_or_else(|| "missing --plugin-version".to_owned())?;
    if !valid_semver(&plugin_version) {
        return Err("plugin version must be MAJOR.MINOR.PATCH with an optional -PRERELEASE (no build metadata)".to_owned());
    }
    let session_id = optional_session_id(session_id)?;
    Ok(BeginArgs {
        root,
        harness,
        hook,
        workspace,
        plugin_version,
        session_id,
    })
}

struct FinishArgs {
    root: PathBuf,
    id: i64,
    status: FinishStatus,
    action: Option<FinishAction>,
    reason: Option<String>,
    detail: Option<String>,
    events_stdin: bool,
}

fn parse_finish(args: Vec<String>) -> Result<FinishArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut id = None;
    let mut status = None;
    let mut action = None;
    let mut reason = None;
    let mut detail = None;
    let mut events_stdin = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--id" => take_value(&mut id, &flag, &mut args)?,
            "--status" => take_value(&mut status, &flag, &mut args)?,
            "--action" => take_value(&mut action, &flag, &mut args)?,
            "--reason" => take_value(&mut reason, &flag, &mut args)?,
            "--detail" => take_value(&mut detail, &flag, &mut args)?,
            "--events-stdin" => events_stdin = true,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let id = positive_id(id)?;
    let status = FinishStatus::parse(status)?;
    let action = FinishAction::parse(action)?;
    match status {
        FinishStatus::Failed
            if detail.as_ref().is_some_and(|value| valid_detail(value))
                && action.is_none()
                && reason.is_none() => {}
        FinishStatus::Succeeded => {
            if detail.as_ref().is_some_and(|value| !valid_detail(value)) {
                return Err("detail must be 1..1024 characters".to_owned());
            }
            match action {
                Some(FinishAction::SpawnWorker) => {
                    reason = optional_identifier(reason, "reason")?;
                }
                Some(FinishAction::Skip) => {
                    reason = Some(valid_identifier(reason, "reason")?);
                }
                _ => return Err("succeeded status requires action spawn_worker or skip".to_owned()),
            }
        }
        FinishStatus::Continued => {
            if action != Some(FinishAction::RequestWorker) {
                return Err("continued status requires action request_worker".to_owned());
            }
            reason = optional_identifier(reason, "reason")?;
            if detail.as_ref().is_some_and(|value| !valid_detail(value)) {
                return Err("detail must be 1..1024 characters".to_owned());
            }
        }
        FinishStatus::Failed => {
            return Err("failed status requires detail of 1..1024 characters".to_owned());
        }
    }
    Ok(FinishArgs {
        root,
        id,
        status,
        action,
        reason,
        detail,
        events_stdin,
    })
}

struct WorkerStartArgs {
    root: PathBuf,
    id: i64,
    origin: WorkerOrigin,
    session_id: Option<String>,
    events_stdin: bool,
}

fn parse_worker_start(args: Vec<String>) -> Result<WorkerStartArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut id = None;
    let mut origin = None;
    let mut session_id = None;
    let mut events_stdin = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--id" => take_value(&mut id, &flag, &mut args)?,
            "--origin" => take_value(&mut origin, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            "--events-stdin" => events_stdin = true,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let origin = match origin.as_deref() {
        None => WorkerOrigin::Direct,
        Some("external") => WorkerOrigin::External,
        Some("fallback") => WorkerOrigin::Fallback,
        Some(_) => return Err("worker origin must be external or fallback".to_owned()),
    };
    let session_id = optional_session_id(session_id)?;
    if origin == WorkerOrigin::External && session_id.is_none() {
        return Err("external worker origin requires --session-id".to_owned());
    }
    Ok(WorkerStartArgs {
        root,
        id: positive_id(id)?,
        origin,
        session_id,
        events_stdin,
    })
}

struct WorkerFinishArgs {
    root: PathBuf,
    id: i64,
    status: String,
    reason: String,
    origin: WorkerOrigin,
    session_id: Option<String>,
    detail: Option<String>,
    events_stdin: bool,
}

fn parse_worker_finish(args: Vec<String>) -> Result<WorkerFinishArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut id = None;
    let mut status = None;
    let mut reason = None;
    let mut origin = None;
    let mut session_id = None;
    let mut detail = None;
    let mut events_stdin = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--id" => take_value(&mut id, &flag, &mut args)?,
            "--status" => take_value(&mut status, &flag, &mut args)?,
            "--reason" => take_value(&mut reason, &flag, &mut args)?,
            "--origin" => take_value(&mut origin, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            "--detail" => take_value(&mut detail, &flag, &mut args)?,
            "--events-stdin" => events_stdin = true,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let status = status.ok_or_else(|| "missing --status".to_owned())?;
    let reason = valid_identifier(reason, "reason")?;
    let valid_result = match status.as_str() {
        "succeeded" => reason == "ok",
        "skipped" => matches!(
            reason.as_str(),
            "disabled" | "too_soon" | "busy" | "nothing_to_do"
        ),
        "failed" => reason == "unavailable",
        _ => return Err("worker status must be succeeded, skipped, or failed".to_owned()),
    };
    if !valid_result {
        return Err("worker status and reason do not match".to_owned());
    }
    if detail.as_ref().is_some_and(|value| !valid_detail(value)) {
        return Err("detail must be 1..1024 characters".to_owned());
    }
    let origin = match origin.as_deref() {
        None => WorkerOrigin::Direct,
        Some("external") => WorkerOrigin::External,
        Some("fallback") => WorkerOrigin::Fallback,
        Some(_) => return Err("worker origin must be external or fallback".to_owned()),
    };
    let session_id = optional_session_id(session_id)?;
    if origin == WorkerOrigin::External && session_id.is_none() {
        return Err("external worker origin requires --session-id".to_owned());
    }
    Ok(WorkerFinishArgs {
        root,
        id: positive_id(id)?,
        status,
        reason,
        origin,
        session_id,
        detail,
        events_stdin,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventKind {
    IngestVisibility,
    VisibilityDelivery,
    IngestPreparation,
    IngestFinalization,
    ClaudeAgentProfile,
    ClaudeRecursionGuard,
    ClaudeAgentView,
    CodexNative,
    Subagent,
}

impl EventKind {
    fn parse(value: Option<String>) -> Result<Self, String> {
        match value.as_deref() {
            Some("ingest_visibility") => Ok(Self::IngestVisibility),
            Some("visibility_delivery") => Ok(Self::VisibilityDelivery),
            Some("ingest_preparation") => Ok(Self::IngestPreparation),
            Some("ingest_finalization") => Ok(Self::IngestFinalization),
            Some("claude_agent_profile") => Ok(Self::ClaudeAgentProfile),
            Some("claude_recursion_guard") => Ok(Self::ClaudeRecursionGuard),
            Some("claude_agent_view") => Ok(Self::ClaudeAgentView),
            Some("codex_native") => Ok(Self::CodexNative),
            Some("subagent") => Ok(Self::Subagent),
            Some(_) => Err("unsupported hook event".to_owned()),
            None => Err("missing --event".to_owned()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::IngestVisibility => "ingest_visibility",
            Self::VisibilityDelivery => "visibility_delivery",
            Self::IngestPreparation => "ingest_preparation",
            Self::IngestFinalization => "ingest_finalization",
            Self::ClaudeAgentProfile => "claude_agent_profile",
            Self::ClaudeRecursionGuard => "claude_recursion_guard",
            Self::ClaudeAgentView => "claude_agent_view",
            Self::CodexNative => "codex_native",
            Self::Subagent => "subagent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventPhase {
    Launch,
    Terminal,
    Continuation,
    Fallback,
    Start,
    Stop,
}

impl EventPhase {
    fn parse(value: Option<String>) -> Result<Option<Self>, String> {
        value
            .map(|value| match value.as_str() {
                "launch" => Ok(Self::Launch),
                "terminal" => Ok(Self::Terminal),
                "continuation" => Ok(Self::Continuation),
                "fallback" => Ok(Self::Fallback),
                "start" => Ok(Self::Start),
                "stop" => Ok(Self::Stop),
                _ => Err("unsupported hook-event phase".to_owned()),
            })
            .transpose()
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Terminal => "terminal",
            Self::Continuation => "continuation",
            Self::Fallback => "fallback",
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventOutcome {
    Started,
    Admitted,
    Ok,
    Partial,
    Failed,
    Emitted,
    Aborted,
    Selected,
    Refused,
    Fallback,
    Returned,
    Taken,
    Skipped,
    Succeeded,
    Observed,
}

impl EventOutcome {
    fn parse(value: Option<String>) -> Result<Self, String> {
        match value.as_deref() {
            Some("started") => Ok(Self::Started),
            Some("admitted") => Ok(Self::Admitted),
            Some("ok") => Ok(Self::Ok),
            Some("partial") => Ok(Self::Partial),
            Some("failed") => Ok(Self::Failed),
            Some("emitted") => Ok(Self::Emitted),
            Some("aborted") => Ok(Self::Aborted),
            Some("selected") => Ok(Self::Selected),
            Some("refused") => Ok(Self::Refused),
            Some("fallback") => Ok(Self::Fallback),
            Some("returned") => Ok(Self::Returned),
            Some("taken") => Ok(Self::Taken),
            Some("skipped") => Ok(Self::Skipped),
            Some("succeeded") => Ok(Self::Succeeded),
            Some("observed") => Ok(Self::Observed),
            Some(_) => Err("unsupported hook-event outcome".to_owned()),
            None => Err("missing --outcome".to_owned()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Admitted => "admitted",
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Emitted => "emitted",
            Self::Aborted => "aborted",
            Self::Selected => "selected",
            Self::Refused => "refused",
            Self::Fallback => "fallback",
            Self::Returned => "returned",
            Self::Taken => "taken",
            Self::Skipped => "skipped",
            Self::Succeeded => "succeeded",
            Self::Observed => "observed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visibility {
    Silent,
    Toast,
    Native,
}

impl Visibility {
    fn parse(value: Option<String>) -> Result<Option<Self>, String> {
        value
            .map(|value| match value.as_str() {
                "silent" => Ok(Self::Silent),
                "toast" => Ok(Self::Toast),
                "native" => Ok(Self::Native),
                _ => Err("visibility must be silent, toast, or native".to_owned()),
            })
            .transpose()
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Toast => "toast",
            Self::Native => "native",
        }
    }
}

const EVENT_REASON: u32 = 1 << 0;
const EVENT_VISIBILITY: u32 = 1 << 1;
const EVENT_LAUNCH_MODE: u32 = 1 << 2;
const EVENT_FALLBACK_MODE: u32 = 1 << 3;
const EVENT_AGENT_TYPE: u32 = 1 << 4;
const EVENT_SESSION_ID: u32 = 1 << 5;
const EVENT_PARENT_SESSION_ID: u32 = 1 << 6;
const EVENT_MANAGER_NAME: u32 = 1 << 7;
const EVENT_MANAGER_ID: u32 = 1 << 8;
const EVENT_LEASE_ID: u32 = 1 << 9;
const EVENT_REQUIRE_VISIBLE: u32 = 1 << 10;
const EVENT_DETAIL: u32 = 1 << 11;
const EVENT_ACTIONABLE_DIGEST: u32 = 1 << 12;
const EVENT_PRE_DIGEST: u32 = 1 << 13;
const EVENT_POST_DIGEST: u32 = 1 << 14;
const EVENT_ACTIONABLE_COUNT: u32 = 1 << 15;
const EVENT_FAILURE_COUNT: u32 = 1 << 16;
const EVENT_DEADLINE_MS: u32 = 1 << 17;
const EVENT_BACKOFF_UNTIL_MS: u32 = 1 << 18;

struct EventArgs {
    root: PathBuf,
    id: i64,
    event: EventKind,
    phase: Option<EventPhase>,
    outcome: EventOutcome,
    reason: Option<String>,
    visibility: Option<Visibility>,
    launch_mode: Option<String>,
    fallback_launch_mode: Option<String>,
    agent_type: Option<String>,
    session_id: Option<String>,
    parent_session_id: Option<String>,
    manager_name: Option<String>,
    manager_id: Option<String>,
    lease_id: Option<String>,
    require_visible_worker: Option<bool>,
    detail: Option<String>,
    actionable_digest: Option<String>,
    pre_digest: Option<String>,
    post_digest: Option<String>,
    actionable_count: Option<i64>,
    failure_count: Option<i64>,
    deadline_ms: Option<i64>,
    backoff_until_ms: Option<i64>,
}

impl EventArgs {
    fn present_fields(&self) -> u32 {
        let fields = [
            (self.reason.is_some(), EVENT_REASON),
            (self.visibility.is_some(), EVENT_VISIBILITY),
            (self.launch_mode.is_some(), EVENT_LAUNCH_MODE),
            (self.fallback_launch_mode.is_some(), EVENT_FALLBACK_MODE),
            (self.agent_type.is_some(), EVENT_AGENT_TYPE),
            (self.session_id.is_some(), EVENT_SESSION_ID),
            (self.parent_session_id.is_some(), EVENT_PARENT_SESSION_ID),
            (self.manager_name.is_some(), EVENT_MANAGER_NAME),
            (self.manager_id.is_some(), EVENT_MANAGER_ID),
            (self.lease_id.is_some(), EVENT_LEASE_ID),
            (self.require_visible_worker.is_some(), EVENT_REQUIRE_VISIBLE),
            (self.detail.is_some(), EVENT_DETAIL),
            (self.actionable_digest.is_some(), EVENT_ACTIONABLE_DIGEST),
            (self.pre_digest.is_some(), EVENT_PRE_DIGEST),
            (self.post_digest.is_some(), EVENT_POST_DIGEST),
            (self.actionable_count.is_some(), EVENT_ACTIONABLE_COUNT),
            (self.failure_count.is_some(), EVENT_FAILURE_COUNT),
            (self.deadline_ms.is_some(), EVENT_DEADLINE_MS),
            (self.backoff_until_ms.is_some(), EVENT_BACKOFF_UNTIL_MS),
        ];
        fields
            .into_iter()
            .filter_map(|(present, field)| present.then_some(field))
            .fold(0, |mask, field| mask | field)
    }

    fn only_fields(&self, allowed: u32) -> Result<(), String> {
        if self.present_fields() & !allowed == 0 {
            Ok(())
        } else {
            Err("hook event contains fields not admitted by its type".to_owned())
        }
    }
}

fn parse_event(args: Vec<String>) -> Result<EventArgs, String> {
    let mut args = args.into_iter();
    let root = absolute_path(args.next(), "global root")?;
    let mut id = None;
    let mut event = None;
    let mut phase = None;
    let mut outcome = None;
    let mut reason = None;
    let mut visibility = None;
    let mut launch_mode = None;
    let mut fallback_launch_mode = None;
    let mut agent_type = None;
    let mut session_id = None;
    let mut parent_session_id = None;
    let mut manager_name = None;
    let mut manager_id = None;
    let mut lease_id = None;
    let mut require_visible_worker = None;
    let mut detail = None;
    let mut actionable_digest = None;
    let mut pre_digest = None;
    let mut post_digest = None;
    let mut actionable_count = None;
    let mut failure_count = None;
    let mut deadline_ms = None;
    let mut backoff_until_ms = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--id" => take_value(&mut id, &flag, &mut args)?,
            "--event" => take_value(&mut event, &flag, &mut args)?,
            "--phase" => take_value(&mut phase, &flag, &mut args)?,
            "--outcome" => take_value(&mut outcome, &flag, &mut args)?,
            "--reason" => take_value(&mut reason, &flag, &mut args)?,
            "--visibility" => take_value(&mut visibility, &flag, &mut args)?,
            "--launch-mode" => take_value(&mut launch_mode, &flag, &mut args)?,
            "--fallback-launch-mode" => take_value(&mut fallback_launch_mode, &flag, &mut args)?,
            "--agent-type" => take_value(&mut agent_type, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            "--parent-session-id" => take_value(&mut parent_session_id, &flag, &mut args)?,
            "--manager-name" => take_value(&mut manager_name, &flag, &mut args)?,
            "--manager-id" => take_value(&mut manager_id, &flag, &mut args)?,
            "--lease-id" => take_value(&mut lease_id, &flag, &mut args)?,
            "--require-visible-worker" => {
                take_value(&mut require_visible_worker, &flag, &mut args)?
            }
            "--detail" => take_value(&mut detail, &flag, &mut args)?,
            "--actionable-digest" => take_value(&mut actionable_digest, &flag, &mut args)?,
            "--pre-digest" => take_value(&mut pre_digest, &flag, &mut args)?,
            "--post-digest" => take_value(&mut post_digest, &flag, &mut args)?,
            "--actionable-count" => take_value(&mut actionable_count, &flag, &mut args)?,
            "--failure-count" => take_value(&mut failure_count, &flag, &mut args)?,
            "--deadline-ms" => take_value(&mut deadline_ms, &flag, &mut args)?,
            "--backoff-until-ms" => take_value(&mut backoff_until_ms, &flag, &mut args)?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let id = positive_id(id)?;
    let fields = EventFields {
        event,
        phase,
        outcome,
        reason,
        visibility,
        launch_mode,
        fallback_launch_mode,
        agent_type,
        session_id,
        parent_session_id,
        manager_name,
        manager_id,
        lease_id,
        require_visible_worker,
        detail,
        actionable_digest,
        pre_digest,
        post_digest,
        actionable_count,
        failure_count,
        deadline_ms,
        backoff_until_ms,
    };
    build_event(root, id, fields)
}

/// The raw string form of one event's fields, before typed parsing. The argv
/// `event` command and the `--events-stdin` batch both fill this, so a single
/// `build_event` owns the typed matrix and neither entry path validates twice.
#[derive(Default)]
struct EventFields {
    event: Option<String>,
    phase: Option<String>,
    outcome: Option<String>,
    reason: Option<String>,
    visibility: Option<String>,
    launch_mode: Option<String>,
    fallback_launch_mode: Option<String>,
    agent_type: Option<String>,
    session_id: Option<String>,
    parent_session_id: Option<String>,
    manager_name: Option<String>,
    manager_id: Option<String>,
    lease_id: Option<String>,
    require_visible_worker: Option<String>,
    detail: Option<String>,
    actionable_digest: Option<String>,
    pre_digest: Option<String>,
    post_digest: Option<String>,
    actionable_count: Option<String>,
    failure_count: Option<String>,
    deadline_ms: Option<String>,
    backoff_until_ms: Option<String>,
}

fn build_event(root: PathBuf, id: i64, fields: EventFields) -> Result<EventArgs, String> {
    let mut parsed = EventArgs {
        root,
        id,
        event: EventKind::parse(fields.event)?,
        phase: EventPhase::parse(fields.phase)?,
        outcome: EventOutcome::parse(fields.outcome)?,
        reason: optional_identifier(fields.reason, "reason")?,
        visibility: Visibility::parse(fields.visibility)?,
        launch_mode: optional_identifier(fields.launch_mode, "launch-mode")?,
        fallback_launch_mode: optional_identifier(
            fields.fallback_launch_mode,
            "fallback-launch-mode",
        )?,
        agent_type: fields.agent_type,
        session_id: optional_event_identity(fields.session_id, "session id")?,
        parent_session_id: optional_event_identity(fields.parent_session_id, "parent session id")?,
        manager_name: optional_event_identity(fields.manager_name, "manager name")?,
        manager_id: optional_event_identity(fields.manager_id, "manager id")?,
        lease_id: optional_event_identity(fields.lease_id, "lease id")?,
        require_visible_worker: fields
            .require_visible_worker
            .map(|value| match value.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err("require-visible-worker must be true or false".to_owned()),
            })
            .transpose()?,
        detail: fields.detail,
        actionable_digest: optional_digest(fields.actionable_digest, "actionable digest")?,
        pre_digest: optional_digest(fields.pre_digest, "pre digest")?,
        post_digest: optional_digest(fields.post_digest, "post digest")?,
        actionable_count: optional_non_negative_i64(fields.actionable_count, "actionable count")?,
        failure_count: optional_non_negative_i64(fields.failure_count, "failure count")?,
        deadline_ms: optional_positive_i64(fields.deadline_ms, "deadline")?,
        backoff_until_ms: optional_positive_i64(fields.backoff_until_ms, "backoff deadline")?,
    };
    if parsed
        .detail
        .as_ref()
        .is_some_and(|value| !valid_detail(value))
    {
        return Err("detail must be 1..1024 characters".to_owned());
    }
    parsed.agent_type = parsed
        .agent_type
        .map(|value| match value.as_str() {
            "loam_ingestor" | "loam:ingestor" => Ok(value),
            _ => Err("agent type must be loam_ingestor or loam:ingestor".to_owned()),
        })
        .transpose()?;
    validate_event(&parsed)?;
    Ok(parsed)
}

/// Builds one event from a batch JSON object, enforcing the closed key set and
/// each field's scalar type before the shared `build_event` matrix runs. String
/// fields must be JSON strings, counts/deadlines JSON numbers, and the policy
/// flag a JSON boolean, so an array, object, or null can never slip through.
fn event_from_json(root: PathBuf, id: i64, value: &Value) -> Result<EventArgs, String> {
    let Value::Object(entries) = value else {
        return Err("batch event must be a JSON object".to_owned());
    };
    let mut fields = EventFields::default();
    for (key, item) in entries {
        let slot = match key.as_str() {
            "event" => &mut fields.event,
            "phase" => &mut fields.phase,
            "outcome" => &mut fields.outcome,
            "reason" => &mut fields.reason,
            "visibility" => &mut fields.visibility,
            "launch_mode" => &mut fields.launch_mode,
            "fallback_launch_mode" => &mut fields.fallback_launch_mode,
            "agent_type" => &mut fields.agent_type,
            "session_id" => &mut fields.session_id,
            "parent_session_id" => &mut fields.parent_session_id,
            "manager_name" => &mut fields.manager_name,
            "manager_id" => &mut fields.manager_id,
            "lease_id" => &mut fields.lease_id,
            "require_visible_worker" => &mut fields.require_visible_worker,
            "detail" => &mut fields.detail,
            "actionable_digest" => &mut fields.actionable_digest,
            "pre_digest" => &mut fields.pre_digest,
            "post_digest" => &mut fields.post_digest,
            "actionable_count" => &mut fields.actionable_count,
            "failure_count" => &mut fields.failure_count,
            "deadline_ms" => &mut fields.deadline_ms,
            "backoff_until_ms" => &mut fields.backoff_until_ms,
            _ => return Err(format!("batch event has unknown field {key}")),
        };
        if slot.is_some() {
            return Err(format!("batch event repeats field {key}"));
        }
        *slot = Some(json_event_field(key, item)?);
    }
    build_event(root, id, fields)
}

fn json_event_field(key: &str, item: &Value) -> Result<String, String> {
    match key {
        "actionable_count" | "failure_count" | "deadline_ms" | "backoff_until_ms" => match item {
            Value::Number(literal) => Ok(literal.clone()),
            _ => Err(format!("batch event field {key} must be a number")),
        },
        "require_visible_worker" => match item {
            Value::Bool(flag) => Ok(if *flag { "true" } else { "false" }.to_owned()),
            _ => Err(format!("batch event field {key} must be a boolean")),
        },
        _ => match item {
            Value::String(text) => Ok(text.clone()),
            _ => Err(format!("batch event field {key} must be a string")),
        },
    }
}

/// Reads the bounded `{"schema":1,"events":[...]}` frame from stdin and builds
/// every event up front, so a single malformed member rejects the whole batch
/// before any insert. Byte and count bounds are enforced before parsing.
fn read_events_batch(root: &Path, id: i64) -> Result<Vec<EventArgs>, String> {
    let mut input = String::new();
    std::io::stdin()
        .take((EVENT_BATCH_MAX_BYTES + 1) as u64)
        .read_to_string(&mut input)
        .map_err(|error| format!("cannot read event batch: {error}"))?;
    if input.len() > EVENT_BATCH_MAX_BYTES {
        return Err("event batch exceeds 16 KiB".to_owned());
    }
    let document = json::parse(&input)?;
    let Value::Object(entries) = &document else {
        return Err("event batch must be a JSON object".to_owned());
    };
    for (key, _) in entries {
        if key != "schema" && key != "events" {
            return Err(format!("event batch has unknown field {key}"));
        }
    }
    match document.get("schema") {
        Some(Value::Number(literal)) if literal == "1" => {}
        _ => return Err("event batch schema must be 1".to_owned()),
    }
    let events = document
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "event batch events must be an array".to_owned())?;
    if events.is_empty() || events.len() > EVENT_BATCH_MAX_EVENTS {
        return Err("event batch must carry 1..16 events".to_owned());
    }
    events
        .iter()
        .map(|event| event_from_json(root.to_path_buf(), id, event))
        .collect()
}

/// Applies a pre-built batch in its own immediate transaction, all-or-nothing.
/// A single insert failure drops every event in the batch.
fn apply_event_batch(root: &Path, events: &[EventArgs]) -> Result<(), String> {
    let mut connection = writable_store(root)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    for event in events {
        insert_event_tx(&transaction, event)?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

/// The matrix prevents diagnostic metadata from becoming an unbounded payload channel.
fn validate_event(args: &EventArgs) -> Result<(), String> {
    let active_visibility = matches!(
        args.visibility,
        Some(Visibility::Toast | Visibility::Native)
    );
    match (args.event, args.phase, args.outcome) {
        (EventKind::IngestVisibility, Some(EventPhase::Launch), EventOutcome::Started)
        | (
            EventKind::IngestVisibility,
            Some(EventPhase::Terminal),
            EventOutcome::Ok | EventOutcome::Partial | EventOutcome::Failed,
        ) => {
            args.only_fields(
                EVENT_VISIBILITY
                    | EVENT_LAUNCH_MODE
                    | EVENT_SESSION_ID
                    | EVENT_PARENT_SESSION_ID
                    | EVENT_MANAGER_NAME
                    | EVENT_MANAGER_ID
                    | EVENT_LEASE_ID,
            )?;
            if !active_visibility || args.launch_mode.is_none() {
                return Err(
                    "visibility events require active visibility and launch mode".to_owned(),
                );
            }
        }
        (
            EventKind::VisibilityDelivery,
            Some(EventPhase::Launch | EventPhase::Terminal),
            EventOutcome::Emitted | EventOutcome::Failed | EventOutcome::Aborted,
        ) => {
            let allowed = EVENT_VISIBILITY
                | EVENT_LAUNCH_MODE
                | if args.outcome == EventOutcome::Emitted {
                    0
                } else {
                    EVENT_DETAIL
                };
            args.only_fields(allowed)?;
            if !active_visibility || args.launch_mode.is_none() {
                return Err("delivery events require active visibility and launch mode".to_owned());
            }
        }
        (EventKind::IngestPreparation, None, EventOutcome::Admitted) => {
            args.only_fields(
                EVENT_LAUNCH_MODE
                    | EVENT_LEASE_ID
                    | EVENT_ACTIONABLE_DIGEST
                    | EVENT_ACTIONABLE_COUNT
                    | EVENT_DEADLINE_MS,
            )?;
            if args.launch_mode.is_none()
                || args.lease_id.is_none()
                || args.actionable_digest.is_none()
                || args.actionable_count.is_none()
                || args.deadline_ms.is_none()
            {
                return Err("admitted preparation requires its bounded work identity".to_owned());
            }
        }
        (EventKind::IngestPreparation, None, EventOutcome::Skipped) => {
            args.only_fields(
                EVENT_REASON
                    | EVENT_LAUNCH_MODE
                    | EVENT_LEASE_ID
                    | EVENT_ACTIONABLE_DIGEST
                    | EVENT_ACTIONABLE_COUNT
                    | EVENT_DEADLINE_MS,
            )?;
            if args.reason.is_none() {
                return Err("skipped preparation requires a normalized reason".to_owned());
            }
        }
        (
            EventKind::IngestFinalization,
            None,
            EventOutcome::Ok | EventOutcome::Partial | EventOutcome::Failed,
        ) => {
            args.only_fields(
                EVENT_LEASE_ID
                    | EVENT_PRE_DIGEST
                    | EVENT_POST_DIGEST
                    | EVENT_ACTIONABLE_COUNT
                    | EVENT_FAILURE_COUNT
                    | EVENT_BACKOFF_UNTIL_MS,
            )?;
            if args.lease_id.is_none()
                || args.pre_digest.is_none()
                || args.post_digest.is_none()
                || args.actionable_count.is_none()
                || args.failure_count.is_none()
            {
                return Err("finalization requires its bounded progress identity".to_owned());
            }
        }
        (EventKind::ClaudeAgentProfile, None, EventOutcome::Selected) => {
            args.only_fields(
                EVENT_LAUNCH_MODE
                    | EVENT_AGENT_TYPE
                    | EVENT_MANAGER_NAME
                    | EVENT_MANAGER_ID
                    | EVENT_LEASE_ID,
            )?;
            if args.launch_mode.as_deref() != Some("claude_bg")
                || args.agent_type.as_deref() != Some("loam:ingestor")
                || args.manager_name.is_none()
                || args.manager_id.is_none()
                || args.lease_id.is_none()
            {
                return Err(
                    "Claude agent selection requires its exact profile and identity".to_owned(),
                );
            }
        }
        (EventKind::ClaudeRecursionGuard, None, EventOutcome::Refused) => {
            args.only_fields(EVENT_AGENT_TYPE | EVENT_PARENT_SESSION_ID)?;
            if args.agent_type.as_deref() != Some("loam:ingestor") {
                return Err("Claude recursion refusal requires loam:ingestor".to_owned());
            }
        }
        (EventKind::ClaudeAgentView, None, EventOutcome::Fallback | EventOutcome::Refused) => {
            args.only_fields(
                EVENT_REASON
                    | EVENT_VISIBILITY
                    | EVENT_LAUNCH_MODE
                    | EVENT_FALLBACK_MODE
                    | EVENT_LEASE_ID
                    | EVENT_REQUIRE_VISIBLE,
            )?;
            if !matches!(
                args.reason.as_deref(),
                Some("agent_view_disabled" | "agent_view_unavailable" | "agent_view_launch_failed")
            ) || args.launch_mode.as_deref() != Some("claude_bg")
                || args.fallback_launch_mode.as_deref() != Some("claude_print")
                || args.visibility.is_none()
                || args.lease_id.is_none()
                || args.require_visible_worker.is_none()
                || (args.outcome == EventOutcome::Fallback
                    && args.require_visible_worker != Some(false))
                || (args.outcome == EventOutcome::Refused
                    && args.require_visible_worker != Some(true))
            {
                return Err("invalid Claude Agent View downgrade event".to_owned());
            }
        }
        (EventKind::CodexNative, Some(phase), outcome) => {
            let valid = matches!(
                (phase, outcome),
                (EventPhase::Continuation, EventOutcome::Returned)
                    | (EventPhase::Fallback, EventOutcome::Taken)
            );
            args.only_fields(EVENT_VISIBILITY | EVENT_LEASE_ID)?;
            if !valid || args.visibility != Some(Visibility::Native) {
                return Err("invalid Codex native event".to_owned());
            }
        }
        (EventKind::Subagent, Some(EventPhase::Start), EventOutcome::Observed) => {
            args.only_fields(EVENT_AGENT_TYPE | EVENT_SESSION_ID | EVENT_LEASE_ID)?;
            if args.agent_type.is_none() || args.session_id.is_none() {
                return Err("subagent start requires agent type and session id".to_owned());
            }
        }
        (
            EventKind::Subagent,
            Some(EventPhase::Stop),
            EventOutcome::Succeeded
            | EventOutcome::Skipped
            | EventOutcome::Failed
            | EventOutcome::Aborted,
        ) => {
            args.only_fields(
                EVENT_AGENT_TYPE
                    | EVENT_SESSION_ID
                    | EVENT_LEASE_ID
                    | if matches!(args.outcome, EventOutcome::Failed | EventOutcome::Aborted) {
                        EVENT_DETAIL
                    } else {
                        0
                    },
            )?;
            if args.agent_type.is_none() || args.session_id.is_none() {
                return Err("subagent stop requires agent type and session id".to_owned());
            }
        }
        _ => return Err("event, phase, and outcome do not match".to_owned()),
    }
    Ok(())
}

struct ListArgs {
    root: PathBuf,
    harness: Option<String>,
    hook: Option<String>,
    status: Option<String>,
    session_id: Option<String>,
    limit: usize,
}

fn parse_list(mut args: Vec<String>) -> Result<ListArgs, String> {
    let root = if args.first().is_some_and(|value| !value.starts_with("--")) {
        absolute_path(Some(args.remove(0)), "global root")?
    } else {
        installed_global_root()?
    };
    let mut args = args.into_iter();
    let mut harness = None;
    let mut hook = None;
    let mut status = None;
    let mut session_id = None;
    let mut limit = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--harness" => take_value(&mut harness, &flag, &mut args)?,
            "--hook" => take_value(&mut hook, &flag, &mut args)?,
            "--status" => take_value(&mut status, &flag, &mut args)?,
            "--session-id" => take_value(&mut session_id, &flag, &mut args)?,
            "--limit" => take_value(&mut limit, &flag, &mut args)?,
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let harness = optional_identifier(harness, "harness")?;
    let hook = optional_identifier(hook, "hook")?;
    let status = status
        .map(|value| match value.as_str() {
            "started" | "succeeded" | "failed" | "continued" => Ok(value),
            _ => Err("status must be started, succeeded, failed, or continued".to_owned()),
        })
        .transpose()?;
    let session_id = optional_session_id(session_id)?;
    let limit = match limit {
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=1000).contains(value))
            .ok_or_else(|| "limit must be between 1 and 1000".to_owned())?,
        None => 100,
    };
    Ok(ListArgs {
        root,
        harness,
        hook,
        status,
        session_id,
        limit,
    })
}

fn take_value(
    slot: &mut Option<String>,
    flag: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("repeated option {flag}"));
    }
    *slot = Some(
        args.next()
            .ok_or_else(|| format!("missing value for {flag}"))?,
    );
    Ok(())
}

fn positive_id(value: Option<String>) -> Result<i64, String> {
    value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "id must be a positive integer".to_owned())
}

fn absolute_path(value: Option<String>, name: &str) -> Result<PathBuf, String> {
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{name} must be absolute"));
    }
    Ok(path)
}

pub(crate) fn installed_global_root() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let expected_name = if cfg!(windows) { "loam.exe" } else { "loam" };
    let target = executable.parent();
    let version = target.and_then(Path::parent);
    let bin = version.and_then(Path::parent);
    let root = bin.and_then(Path::parent);
    let valid = executable
        .file_name()
        .is_some_and(|name| name == expected_name)
        && target
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| SUPPORTED_TARGETS.contains(&name))
        && version
            .and_then(Path::file_name)
            .is_some_and(|name| name == env!("CARGO_PKG_VERSION"))
        && bin
            .and_then(Path::file_name)
            .is_some_and(|name| name == "bin");
    if valid {
        let root = root
            .map(Path::to_path_buf)
            .ok_or_else(inferred_root_error)?;
        if root.join("install.json").is_file() {
            return Ok(root);
        }
    }
    Err(inferred_root_error())
}

fn inferred_root_error() -> String {
    "cannot infer global root outside the installed runtime layout; pass it explicitly".to_owned()
}

fn optional_identifier(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    value
        .map(|value| valid_identifier(Some(value), name))
        .transpose()
}

fn optional_digest(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    value
        .map(|value| {
            if value.len() == 64
                && value
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                Ok(value)
            } else {
                Err(format!(
                    "{name} must be 64 lowercase hexadecimal characters"
                ))
            }
        })
        .transpose()
}

fn optional_non_negative_i64(value: Option<String>, name: &str) -> Result<Option<i64>, String> {
    value
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value >= 0)
                .ok_or_else(|| format!("{name} must be a non-negative integer"))
        })
        .transpose()
}

fn optional_positive_i64(value: Option<String>, name: &str) -> Result<Option<i64>, String> {
    value
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("{name} must be a positive integer"))
        })
        .transpose()
}

fn valid_identifier(value: Option<String>, name: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| format!("missing --{name}"))?;
    let mut bytes = value.bytes();
    let first = bytes.next();
    if value.len() > 32
        || !matches!(first, Some(b'a'..=b'z'))
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return Err(format!("invalid {name} identifier"));
    }
    Ok(value)
}

fn optional_session_id(value: Option<String>) -> Result<Option<String>, String> {
    value
        .map(|value| {
            if value.is_empty()
                || value.chars().count() > 256
                || value.chars().any(char::is_control)
            {
                return Err("session id must be 1..256 characters without controls".to_owned());
            }
            Ok(value)
        })
        .transpose()
}

fn optional_event_identity(value: Option<String>, name: &str) -> Result<Option<String>, String> {
    value
        .map(|value| {
            if value.is_empty()
                || value.chars().count() > 256
                || value.chars().any(char::is_control)
            {
                return Err(format!("{name} must be 1..256 characters without controls"));
            }
            Ok(value)
        })
        .transpose()
}

fn valid_detail(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 1024
}

// Core semver with an optional semver 2.0.0 prerelease: `-` followed by
// dot-separated identifiers of [0-9A-Za-z-], no empty identifiers, numeric
// identifiers without leading zeros. Build metadata (`+...`) stays rejected.
// An installed @next plugin injects a prerelease plugin version here, so this
// loosening is what keeps session hooks working on prerelease installs.
fn valid_semver(value: &str) -> bool {
    fn core_part(part: &str) -> bool {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    }
    fn identifier(part: &str) -> bool {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !(part.bytes().all(|byte| byte.is_ascii_digit())
                && part.len() > 1
                && part.starts_with('0'))
    }

    let (core, prerelease) = match value.split_once('-') {
        Some((core, rest)) => (core, Some(rest)),
        None => (value, None),
    };
    let mut parts = core.split('.');
    if !core_part(parts.next().unwrap_or_default())
        || !core_part(parts.next().unwrap_or_default())
        || !core_part(parts.next().unwrap_or_default())
        || parts.next().is_some()
    {
        return false;
    }
    match prerelease {
        None => true,
        Some(rest) => {
            if rest.contains('+') {
                return false;
            }
            !rest.is_empty() && rest.split('.').all(identifier)
        }
    }
}

fn begin(args: BeginArgs) -> Result<(), String> {
    let root_is_new = !args.root.exists();
    fs::create_dir_all(&args.root).map_err(|error| error.to_string())?;
    if root_is_new {
        private_permissions(&args.root, 0o700)?;
    }
    let database = args.root.join(DATABASE_NAME);
    let database_is_new = !database.exists();
    let mut connection = Connection::open(&database).map_err(|error| error.to_string())?;
    if database_is_new {
        private_permissions(&database, 0o600)?;
    }
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    transaction
        .execute(
            "INSERT INTO hook_run (started_at_ms, finished_at_ms, harness, hook, status, detail, session_id, workspace, plugin_version, runtime_version)
             VALUES (?1, NULL, ?2, ?3, 'started', NULL, ?4, ?5, ?6, ?7)",
            params![
                Utc::now().timestamp_millis(),
                args.harness,
                args.hook,
                args.session_id,
                args.workspace,
                args.plugin_version,
                env!("CARGO_PKG_VERSION")
            ],
        )
        .map_err(|error| error.to_string())?;
    let id = transaction.last_insert_rowid();
    // Foreign keys are disabled by default per connection, so retention owns child cleanup.
    transaction
        .execute(
            "DELETE FROM hook_event WHERE hook_run_id IN (
                SELECT id FROM hook_run ORDER BY id DESC LIMIT -1 OFFSET ?1
            )",
            params![RETENTION],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM hook_run WHERE id IN (
                SELECT id FROM hook_run ORDER BY id DESC LIMIT -1 OFFSET ?1
            )",
            params![RETENTION],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    println!("{id}");
    Ok(())
}

/// One execution record for a native `loam hook <harness>` invocation, written
/// into the same `hook_run` ledger the Node hooks use. Diagnostic-only: a caller
/// swallows the error so a ledger failure never fails the hook (mirrors the Node
/// bookkeeping contract), and the short busy timeout keeps a locked ledger from
/// slowing it. A single complete row (started + finished + terminal status),
/// unlike the Node begin/finish two-phase, because the native read path has no
/// worker phase to interleave.
pub(crate) struct NativeRun<'a> {
    pub root: &'a Path,
    pub harness: &'a str,
    pub event: &'a str,
    pub session_id: Option<&'a str>,
    pub workspace: &'a str,
    pub status: &'a str,
    pub detail: Option<&'a str>,
    pub plugin_version: &'a str,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
}

pub(crate) fn record_native_run(run: NativeRun) -> Result<(), String> {
    let root_is_new = !run.root.exists();
    fs::create_dir_all(run.root).map_err(|error| error.to_string())?;
    if root_is_new {
        private_permissions(run.root, 0o700)?;
    }
    let database = run.root.join(DATABASE_NAME);
    let database_is_new = !database.exists();
    let mut connection = Connection::open(&database).map_err(|error| error.to_string())?;
    if database_is_new {
        private_permissions(&database, 0o600)?;
    }
    connection
        .busy_timeout(NATIVE_RECORD_BUSY_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    transaction
        .execute(
            "INSERT INTO hook_run (started_at_ms, finished_at_ms, harness, hook, status, detail, session_id, workspace, plugin_version, runtime_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run.started_at_ms,
                run.finished_at_ms,
                run.harness,
                run.event,
                run.status,
                run.detail,
                run.session_id,
                run.workspace,
                run.plugin_version,
                env!("CARGO_PKG_VERSION")
            ],
        )
        .map_err(|error| error.to_string())?;
    // Same retention prune as begin(): the per-turn events fire on every tool
    // boundary, so an unpruned native ledger would grow without bound.
    transaction
        .execute(
            "DELETE FROM hook_event WHERE hook_run_id IN (
                SELECT id FROM hook_run ORDER BY id DESC LIMIT -1 OFFSET ?1
            )",
            params![RETENTION],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM hook_run WHERE id IN (
                SELECT id FROM hook_run ORDER BY id DESC LIMIT -1 OFFSET ?1
            )",
            params![RETENTION],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(())
}

fn finish(args: FinishArgs) -> Result<(), String> {
    let database = args.root.join(DATABASE_NAME);
    if !database.is_file() {
        return Err("hook-run store does not exist".to_owned());
    }
    let mut connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    let action = args.action.map(FinishAction::as_str);
    let changed = transaction
        .execute(
            "UPDATE hook_run
             SET finished_at_ms = ?1, status = ?2, detail = ?3, action = ?4, reason = ?5,
                 worker_status = CASE WHEN ?4 IN ('spawn_worker', 'request_worker') THEN 'requested' ELSE NULL END,
                 worker_origin = CASE WHEN ?4 = 'spawn_worker' THEN 'direct' ELSE NULL END
             WHERE id = ?6 AND status = 'started'",
            params![
                Utc::now().timestamp_millis(),
                args.status.as_str(),
                args.detail,
                action,
                args.reason,
                args.id
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("hook run is missing or already finished".to_owned());
    }
    transaction.commit().map_err(|error| error.to_string())?;
    // Commit the parent finish first, then attach the ordered batch. A batch
    // failure is fail-open: it never rolls back or fails the completed finish.
    if args.events_stdin {
        if let Err(error) = read_events_batch(&args.root, args.id)
            .and_then(|events| apply_event_batch(&args.root, &events))
        {
            eprintln!("loam hooks: finish event batch dropped: {error}");
        }
    }
    Ok(())
}

/// Events use guarded inserts so a valid enum cannot be attached to an incompatible parent.
fn record_event(args: EventArgs) -> Result<(), String> {
    let mut connection = writable_store(&args.root)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    insert_event_tx(&transaction, &args)?;
    transaction.commit().map_err(|error| error.to_string())
}

/// Inserts one validated event under its causal-parent guard inside an existing
/// transaction. Shared by the standalone `event` command and the `--events-stdin`
/// batch so one guard matrix serves both; a missing/incompatible parent errors.
fn insert_event_tx(transaction: &Connection, args: &EventArgs) -> Result<(), String> {
    let guard = match (args.event, args.phase, args.agent_type.as_deref()) {
        (EventKind::IngestVisibility, Some(EventPhase::Launch), _) => {
            "status = 'succeeded' AND action = 'spawn_worker'"
        }
        (EventKind::IngestVisibility, Some(EventPhase::Terminal), _) => {
            "status = 'succeeded' AND action = 'spawn_worker'
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'ingest_visibility'
                   AND phase = 'launch' AND visibility = ?7 AND launch_mode = ?8
             )"
        }
        (EventKind::VisibilityDelivery, Some(_), _) => {
            "status = 'succeeded' AND action = 'spawn_worker'
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'ingest_visibility'
                   AND phase = ?4 AND visibility = ?7 AND launch_mode = ?8
             )"
        }
        (EventKind::IngestPreparation, None, _) => {
            "worker_status = 'running'
             AND ((status = 'succeeded' AND action = 'spawn_worker' AND worker_origin = 'direct')
                  OR (harness = 'codex' AND status = 'continued' AND action = 'request_worker'
                      AND worker_origin IN ('external', 'fallback')))"
        }
        (EventKind::IngestFinalization, None, _) => {
            "worker_status = 'running'
             AND ((status = 'succeeded' AND action = 'spawn_worker' AND worker_origin = 'direct')
                  OR (harness = 'codex' AND status = 'continued' AND action = 'request_worker'
                      AND worker_origin IN ('external', 'fallback')))
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'ingest_preparation'
                   AND outcome = 'admitted' AND lease_id = ?16
                   AND actionable_digest = ?19 AND actionable_count = ?21
             )"
        }
        (EventKind::ClaudeAgentProfile, None, _) | (EventKind::ClaudeAgentView, None, _) => {
            "harness = 'claude' AND status = 'succeeded' AND action = 'spawn_worker'"
        }
        (EventKind::ClaudeRecursionGuard, None, _) => {
            "harness = 'claude' AND status = 'succeeded' AND action = 'skip'
             AND reason = 'disabled'"
        }
        (EventKind::CodexNative, Some(EventPhase::Fallback), _) => {
            "harness = 'codex' AND status = 'continued' AND action = 'request_worker'
             AND worker_origin IS NULL
             AND NOT EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'subagent' AND phase = 'start'
                   AND agent_type = 'loam_ingestor'
             )"
        }
        (EventKind::CodexNative, Some(EventPhase::Continuation), _) => {
            "harness = 'codex' AND status = 'continued' AND action = 'request_worker'"
        }
        (EventKind::Subagent, Some(EventPhase::Start), Some("loam_ingestor")) => {
            "harness = 'codex' AND status = 'continued' AND action = 'request_worker'
             AND NOT EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'codex_native'
                   AND phase = 'fallback' AND outcome = 'taken'
             )"
        }
        (EventKind::Subagent, Some(EventPhase::Start), Some("loam:ingestor")) => {
            "harness = 'claude' AND status = 'succeeded' AND action = 'spawn_worker'"
        }
        (EventKind::Subagent, Some(EventPhase::Stop), Some("loam_ingestor")) => {
            "harness = 'codex' AND status = 'continued' AND action = 'request_worker'
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'subagent' AND phase = 'start'
                   AND agent_type = ?11 AND session_id = ?12
             )"
        }
        (EventKind::Subagent, Some(EventPhase::Stop), Some("loam:ingestor")) => {
            "harness = 'claude' AND status = 'succeeded' AND action = 'spawn_worker'
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'subagent' AND phase = 'start'
                   AND agent_type = ?11 AND session_id = ?12
             )"
        }
        _ => return Err("hook event has no causal parent rule".to_owned()),
    };
    let statement = format!(
        "INSERT INTO hook_event (
             hook_run_id, occurred_at_ms, event, phase, outcome, reason, visibility,
             launch_mode, fallback_launch_mode, require_visible_worker, agent_type,
             session_id, parent_session_id, manager_name, manager_id, lease_id, detail,
             actionable_digest, pre_digest, post_digest, actionable_count, failure_count,
             deadline_ms, backoff_until_ms
         )
         SELECT ?2, ?1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21, ?22, ?23, ?24
         FROM hook_run WHERE id = ?2 AND {guard}"
    );
    let phase = args.phase.map(EventPhase::as_str);
    let visibility = args.visibility.map(Visibility::as_str);
    let changed = transaction
        .execute(
            &statement,
            params![
                Utc::now().timestamp_millis(),
                args.id,
                args.event.as_str(),
                phase,
                args.outcome.as_str(),
                args.reason,
                visibility,
                args.launch_mode,
                args.fallback_launch_mode,
                args.require_visible_worker.map(i64::from),
                args.agent_type,
                args.session_id,
                args.parent_session_id,
                args.manager_name,
                args.manager_id,
                args.lease_id,
                args.detail,
                args.actionable_digest,
                args.pre_digest,
                args.post_digest,
                args.actionable_count,
                args.failure_count,
                args.deadline_ms,
                args.backoff_until_ms,
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("hook event parent is missing or incompatible".to_owned());
    }
    Ok(())
}

fn worker_start(args: WorkerStartArgs) -> Result<(), String> {
    // Build and validate the batch before opening the transaction so a malformed
    // frame aborts with nothing persisted. The external-origin proof
    // (subagent/start/observed) and the transition then commit together in one
    // transaction: if either fails, neither persists.
    let events = if args.events_stdin {
        read_events_batch(&args.root, args.id)?
    } else {
        Vec::new()
    };
    let mut connection = writable_store(&args.root)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    for event in &events {
        insert_event_tx(&transaction, event)?;
    }
    let guard = match args.origin {
        WorkerOrigin::Direct => {
            "status = 'succeeded' AND action = 'spawn_worker' AND worker_origin = 'direct'"
        }
        WorkerOrigin::External => {
            "status = 'continued' AND action = 'request_worker' AND worker_origin IS NULL
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'subagent' AND phase = 'start'
                   AND agent_type = 'loam_ingestor' AND session_id = ?2
             )"
        }
        WorkerOrigin::Fallback => {
            "status = 'continued' AND action = 'request_worker' AND worker_origin IS NULL
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'codex_native'
                   AND phase = 'fallback' AND outcome = 'taken'
             )"
        }
    };
    let statement = format!(
        "UPDATE hook_run
         SET worker_status = 'running', worker_origin = ?4,
             worker_started_at_ms = ?1, worker_session_id = ?2
         WHERE id = ?3 AND worker_status = 'requested' AND {guard}"
    );
    let changed = transaction
        .execute(
            &statement,
            params![
                Utc::now().timestamp_millis(),
                args.session_id,
                args.id,
                args.origin.as_str()
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("worker is missing or not requested".to_owned());
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn worker_finish(args: WorkerFinishArgs) -> Result<(), String> {
    // Attempt the ordered batch first, while the worker is still requested or
    // running, then always attempt the terminal transition below. A batch
    // failure is fail-open and cannot block the terminal transition.
    if args.events_stdin {
        if let Err(error) = read_events_batch(&args.root, args.id)
            .and_then(|events| apply_event_batch(&args.root, &events))
        {
            eprintln!("loam hooks: worker-finish event batch dropped: {error}");
        }
    }
    let mut connection = writable_store(&args.root)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    ensure_write_schema(&transaction)?;
    let guard = match args.origin {
        WorkerOrigin::Direct => {
            "status = 'succeeded' AND action = 'spawn_worker' AND worker_origin = 'direct'"
        }
        WorkerOrigin::External => {
            "status = 'continued' AND action = 'request_worker'
             AND (worker_origin IS NULL OR worker_origin = 'external')
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'subagent' AND phase = 'start'
                   AND agent_type = 'loam_ingestor' AND session_id = ?6
             )"
        }
        WorkerOrigin::Fallback => {
            "status = 'continued' AND action = 'request_worker'
             AND (worker_origin IS NULL OR worker_origin = 'fallback')
             AND EXISTS (
                 SELECT 1 FROM hook_event
                 WHERE hook_run_id = hook_run.id AND event = 'codex_native'
                   AND phase = 'fallback' AND outcome = 'taken'
             )"
        }
    };
    let statement = format!(
        "UPDATE hook_run
         SET worker_status = ?1, worker_finished_at_ms = ?2, worker_reason = ?3,
             worker_detail = ?4, worker_origin = ?7,
             worker_session_id = COALESCE(worker_session_id, ?6)
         WHERE id = ?5 AND worker_status IN ('requested', 'running') AND {guard}"
    );
    let changed = transaction
        .execute(
            &statement,
            params![
                args.status,
                Utc::now().timestamp_millis(),
                args.reason,
                args.detail,
                args.id,
                args.session_id,
                args.origin.as_str()
            ],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("worker is missing or already finished".to_owned());
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn writable_store(root: &Path) -> Result<Connection, String> {
    let database = root.join(DATABASE_NAME);
    if !database.is_file() {
        return Err("hook-run store does not exist".to_owned());
    }
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn list(args: ListArgs) -> Result<(), String> {
    let database = args.root.join(DATABASE_NAME);
    if !database.is_file() {
        return Ok(());
    }
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let version = schema_version(&connection)?;
    if !matches!(version, LEGACY_SCHEMA_VERSION | SCHEMA_VERSION) {
        return Err(format!("unsupported database schema version {version}"));
    }
    let query = if !has_result_columns(&connection)? {
        "SELECT id, started_at_ms, finished_at_ms, harness, hook, status, detail,
                NULL, NULL, session_id, workspace, plugin_version, runtime_version,
                NULL, NULL, NULL, NULL, NULL, NULL, NULL
         FROM hook_run ORDER BY id DESC LIMIT 10000"
    } else if version == LEGACY_SCHEMA_VERSION {
        "SELECT id, started_at_ms, finished_at_ms, harness, hook, status, detail,
                action, reason, session_id, workspace, plugin_version, runtime_version,
                worker_status, NULL, worker_started_at_ms, worker_finished_at_ms,
                worker_reason, worker_detail, worker_session_id
         FROM hook_run ORDER BY id DESC LIMIT 10000"
    } else {
        "SELECT id, started_at_ms, finished_at_ms, harness, hook, status, detail,
                action, reason, session_id, workspace, plugin_version, runtime_version,
                worker_status, worker_origin, worker_started_at_ms, worker_finished_at_ms,
                worker_reason, worker_detail, worker_session_id
         FROM hook_run ORDER BY id DESC LIMIT 10000"
    };
    let mut statement = connection
        .prepare(query)
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(HookRun {
                id: row.get(0)?,
                started_at_ms: row.get(1)?,
                finished_at_ms: row.get(2)?,
                harness: row.get(3)?,
                hook: row.get(4)?,
                status: row.get(5)?,
                detail: row.get(6)?,
                action: row.get(7)?,
                reason: row.get(8)?,
                session_id: row.get(9)?,
                workspace: row.get(10)?,
                plugin_version: row.get(11)?,
                runtime_version: row.get(12)?,
                worker_status: row.get(13)?,
                worker_origin: row.get(14)?,
                worker_started_at_ms: row.get(15)?,
                worker_finished_at_ms: row.get(16)?,
                worker_reason: row.get(17)?,
                worker_detail: row.get(18)?,
                worker_session_id: row.get(19)?,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut emitted = 0;
    for row in rows {
        let run = row.map_err(|error| error.to_string())?;
        if args
            .harness
            .as_ref()
            .is_some_and(|value| value != &run.harness)
            || args.hook.as_ref().is_some_and(|value| value != &run.hook)
            || args
                .status
                .as_ref()
                .is_some_and(|value| value != &run.status)
            || args
                .session_id
                .as_ref()
                .is_some_and(|value| run.session_id.as_ref() != Some(value))
        {
            continue;
        }
        let events = if version == SCHEMA_VERSION {
            events_for(&connection, run.id)?
        } else {
            Vec::new()
        };
        println!("{}", run.json(&events)?);
        emitted += 1;
        if emitted == args.limit {
            break;
        }
    }
    Ok(())
}

struct HookEvent {
    id: i64,
    occurred_at_ms: i64,
    event: String,
    phase: Option<String>,
    outcome: String,
    reason: Option<String>,
    visibility: Option<String>,
    launch_mode: Option<String>,
    fallback_launch_mode: Option<String>,
    require_visible_worker: Option<i64>,
    agent_type: Option<String>,
    session_id: Option<String>,
    parent_session_id: Option<String>,
    manager_name: Option<String>,
    manager_id: Option<String>,
    lease_id: Option<String>,
    detail: Option<String>,
    actionable_digest: Option<String>,
    pre_digest: Option<String>,
    post_digest: Option<String>,
    actionable_count: Option<i64>,
    failure_count: Option<i64>,
    deadline_ms: Option<i64>,
    backoff_until_ms: Option<i64>,
}

impl HookEvent {
    fn json(&self) -> Result<String, String> {
        Ok(format!(
            "{{\"id\":{},\"occurred_at\":\"{}\",\"event\":\"{}\",\"phase\":{},\"outcome\":\"{}\",\"reason\":{},\"visibility\":{},\"launch_mode\":{},\"fallback_launch_mode\":{},\"require_visible_worker\":{},\"agent_type\":{},\"session_id\":{},\"parent_session_id\":{},\"manager_name\":{},\"manager_id\":{},\"lease_id\":{},\"detail\":{},\"actionable_digest\":{},\"pre_digest\":{},\"post_digest\":{},\"actionable_count\":{},\"failure_count\":{},\"deadline_ms\":{},\"backoff_until_ms\":{}}}",
            self.id,
            timestamp(self.occurred_at_ms)?,
            json_escape(&self.event),
            optional_json_string(self.phase.as_deref()),
            json_escape(&self.outcome),
            optional_json_string(self.reason.as_deref()),
            optional_json_string(self.visibility.as_deref()),
            optional_json_string(self.launch_mode.as_deref()),
            optional_json_string(self.fallback_launch_mode.as_deref()),
            optional_bool(self.require_visible_worker),
            optional_json_string(self.agent_type.as_deref()),
            optional_json_string(self.session_id.as_deref()),
            optional_json_string(self.parent_session_id.as_deref()),
            optional_json_string(self.manager_name.as_deref()),
            optional_json_string(self.manager_id.as_deref()),
            optional_json_string(self.lease_id.as_deref()),
            optional_json_string(self.detail.as_deref()),
            optional_json_string(self.actionable_digest.as_deref()),
            optional_json_string(self.pre_digest.as_deref()),
            optional_json_string(self.post_digest.as_deref()),
            optional_i64(self.actionable_count),
            optional_i64(self.failure_count),
            optional_i64(self.deadline_ms),
            optional_i64(self.backoff_until_ms),
        ))
    }
}

fn events_for(connection: &Connection, hook_run_id: i64) -> Result<Vec<HookEvent>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, occurred_at_ms, event, phase, outcome, reason, visibility,
                    launch_mode, fallback_launch_mode, require_visible_worker, agent_type,
                    session_id, parent_session_id, manager_name, manager_id, lease_id, detail,
                    actionable_digest, pre_digest, post_digest, actionable_count, failure_count,
                    deadline_ms, backoff_until_ms
             FROM hook_event WHERE hook_run_id = ?1 ORDER BY id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![hook_run_id], |row| {
            Ok(HookEvent {
                id: row.get(0)?,
                occurred_at_ms: row.get(1)?,
                event: row.get(2)?,
                phase: row.get(3)?,
                outcome: row.get(4)?,
                reason: row.get(5)?,
                visibility: row.get(6)?,
                launch_mode: row.get(7)?,
                fallback_launch_mode: row.get(8)?,
                require_visible_worker: row.get(9)?,
                agent_type: row.get(10)?,
                session_id: row.get(11)?,
                parent_session_id: row.get(12)?,
                manager_name: row.get(13)?,
                manager_id: row.get(14)?,
                lease_id: row.get(15)?,
                detail: row.get(16)?,
                actionable_digest: row.get(17)?,
                pre_digest: row.get(18)?,
                post_digest: row.get(19)?,
                actionable_count: row.get(20)?,
                failure_count: row.get(21)?,
                deadline_ms: row.get(22)?,
                backoff_until_ms: row.get(23)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.map(|row| row.map_err(|error| error.to_string()))
        .collect()
}

struct HookRun {
    id: i64,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    harness: String,
    hook: String,
    status: String,
    detail: Option<String>,
    action: Option<String>,
    reason: Option<String>,
    session_id: Option<String>,
    workspace: String,
    plugin_version: String,
    runtime_version: String,
    worker_status: Option<String>,
    worker_origin: Option<String>,
    worker_started_at_ms: Option<i64>,
    worker_finished_at_ms: Option<i64>,
    worker_reason: Option<String>,
    worker_detail: Option<String>,
    worker_session_id: Option<String>,
}

impl HookRun {
    fn json(&self, events: &[HookEvent]) -> Result<String, String> {
        let events = events
            .iter()
            .map(HookEvent::json)
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        Ok(format!(
            "{{\"schema\":2,\"id\":{},\"started_at\":\"{}\",\"finished_at\":{},\"duration_ms\":{},\"harness\":\"{}\",\"hook\":\"{}\",\"status\":\"{}\",\"detail\":{},\"action\":{},\"reason\":{},\"session_id\":{},\"workspace\":\"{}\",\"plugin_version\":\"{}\",\"runtime_version\":\"{}\",\"worker_status\":{},\"worker_origin\":{},\"worker_started_at\":{},\"worker_finished_at\":{},\"worker_duration_ms\":{},\"worker_reason\":{},\"worker_detail\":{},\"worker_session_id\":{},\"events\":[{}]}}",
            self.id,
            timestamp(self.started_at_ms)?,
            optional_timestamp(self.finished_at_ms)?,
            optional_i64(self.finished_at_ms.map(|value| value - self.started_at_ms)),
            json_escape(&self.harness),
            json_escape(&self.hook),
            json_escape(&self.status),
            optional_json_string(self.detail.as_deref()),
            optional_json_string(self.action.as_deref()),
            optional_json_string(self.reason.as_deref()),
            optional_json_string(self.session_id.as_deref()),
            json_escape(&self.workspace),
            json_escape(&self.plugin_version),
            json_escape(&self.runtime_version),
            optional_json_string(self.worker_status.as_deref()),
            optional_json_string(self.worker_origin.as_deref()),
            optional_timestamp(self.worker_started_at_ms)?,
            optional_timestamp(self.worker_finished_at_ms)?,
            optional_i64(
                self.worker_started_at_ms
                    .zip(self.worker_finished_at_ms)
                    .map(|(started, finished)| finished - started)
            ),
            optional_json_string(self.worker_reason.as_deref()),
            optional_json_string(self.worker_detail.as_deref()),
            optional_json_string(self.worker_session_id.as_deref()),
            events,
        ))
    }
}

fn timestamp(value: i64) -> Result<String, String> {
    Utc.timestamp_millis_opt(value)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| format!("invalid hook-run timestamp {value}"))
}

fn optional_timestamp(value: Option<i64>) -> Result<String, String> {
    value
        .map(|value| timestamp(value).map(|value| format!("\"{value}\"")))
        .unwrap_or_else(|| Ok("null".to_owned()))
}

fn optional_json_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned())
}

fn optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned())
}

fn optional_bool(value: Option<i64>) -> &'static str {
    match value {
        Some(0) => "false",
        Some(_) => "true",
        None => "null",
    }
}

fn schema_version(connection: &Connection) -> Result<i64, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| error.to_string())
}

fn has_result_columns(connection: &Connection) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT count(*) FROM pragma_table_info('hook_run')
             WHERE name IN ('action', 'reason', 'worker_status', 'worker_started_at_ms',
                            'worker_finished_at_ms', 'worker_reason', 'worker_detail',
                            'worker_session_id')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count == 8)
        .map_err(|error| error.to_string())
}

fn has_worker_origin(connection: &Connection) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT count(*) FROM pragma_table_info('hook_run') WHERE name = 'worker_origin'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count == 1)
        .map_err(|error| error.to_string())
}

fn has_event_table(connection: &Connection) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'hook_event'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count == 1)
        .map_err(|error| error.to_string())
}

const CREATE_V2_TABLES: &str = "
    CREATE TABLE hook_run (
        id INTEGER PRIMARY KEY,
        started_at_ms INTEGER NOT NULL,
        finished_at_ms INTEGER,
        harness TEXT NOT NULL,
        hook TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('started', 'succeeded', 'failed', 'continued')),
        detail TEXT,
        action TEXT CHECK (action IS NULL OR action IN ('spawn_worker', 'skip', 'request_worker')),
        reason TEXT,
        session_id TEXT,
        workspace TEXT NOT NULL,
        plugin_version TEXT NOT NULL,
        runtime_version TEXT NOT NULL,
        worker_status TEXT CHECK (worker_status IS NULL OR worker_status IN ('requested', 'running', 'succeeded', 'skipped', 'failed')),
        worker_origin TEXT CHECK (worker_origin IS NULL OR worker_origin IN ('direct', 'external', 'fallback')),
        worker_started_at_ms INTEGER,
        worker_finished_at_ms INTEGER,
        worker_reason TEXT,
        worker_detail TEXT,
        worker_session_id TEXT
    );
    CREATE TABLE hook_event (
        id INTEGER PRIMARY KEY,
        hook_run_id INTEGER NOT NULL,
        occurred_at_ms INTEGER NOT NULL,
        event TEXT NOT NULL CHECK (event IN (
            'ingest_visibility', 'visibility_delivery', 'ingest_preparation',
            'ingest_finalization', 'claude_agent_profile', 'claude_recursion_guard',
            'claude_agent_view', 'codex_native', 'subagent'
        )),
        phase TEXT CHECK (phase IS NULL OR phase IN (
            'launch', 'terminal', 'continuation', 'fallback', 'start', 'stop'
        )),
        outcome TEXT NOT NULL CHECK (outcome IN (
            'started', 'admitted', 'ok', 'partial', 'failed', 'emitted', 'aborted', 'selected',
            'refused', 'fallback', 'returned', 'taken', 'skipped', 'succeeded', 'observed'
        )),
        reason TEXT,
        visibility TEXT CHECK (visibility IS NULL OR visibility IN ('silent', 'toast', 'native')),
        launch_mode TEXT,
        fallback_launch_mode TEXT,
        require_visible_worker INTEGER CHECK (
            require_visible_worker IS NULL OR require_visible_worker IN (0, 1)
        ),
        agent_type TEXT CHECK (
            agent_type IS NULL OR agent_type IN ('loam_ingestor', 'loam:ingestor')
        ),
        session_id TEXT,
        parent_session_id TEXT,
        manager_name TEXT,
        manager_id TEXT,
        lease_id TEXT,
        detail TEXT CHECK (detail IS NULL OR length(detail) BETWEEN 1 AND 1024),
        actionable_digest TEXT CHECK (
            actionable_digest IS NULL OR
            (length(actionable_digest) = 64 AND actionable_digest NOT GLOB '*[^0-9a-f]*')
        ),
        pre_digest TEXT CHECK (
            pre_digest IS NULL OR
            (length(pre_digest) = 64 AND pre_digest NOT GLOB '*[^0-9a-f]*')
        ),
        post_digest TEXT CHECK (
            post_digest IS NULL OR
            (length(post_digest) = 64 AND post_digest NOT GLOB '*[^0-9a-f]*')
        ),
        actionable_count INTEGER CHECK (actionable_count IS NULL OR actionable_count >= 0),
        failure_count INTEGER CHECK (failure_count IS NULL OR failure_count >= 0),
        deadline_ms INTEGER CHECK (deadline_ms IS NULL OR deadline_ms > 0),
        backoff_until_ms INTEGER CHECK (backoff_until_ms IS NULL OR backoff_until_ms > 0)
    );
    CREATE INDEX hook_event_run ON hook_event (hook_run_id, id);
    CREATE UNIQUE INDEX hook_event_once
        ON hook_event (hook_run_id, event, IFNULL(phase, ''))
        WHERE event != 'subagent';
    CREATE UNIQUE INDEX hook_subagent_event_once
        ON hook_event (hook_run_id, event, phase, session_id)
        WHERE event = 'subagent';";

fn complete_sparse_v1(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "ALTER TABLE hook_run ADD COLUMN action TEXT;
             ALTER TABLE hook_run ADD COLUMN reason TEXT;
             ALTER TABLE hook_run ADD COLUMN worker_status TEXT CHECK (worker_status IN ('requested', 'running', 'succeeded', 'skipped', 'failed'));
             ALTER TABLE hook_run ADD COLUMN worker_started_at_ms INTEGER;
             ALTER TABLE hook_run ADD COLUMN worker_finished_at_ms INTEGER;
             ALTER TABLE hook_run ADD COLUMN worker_reason TEXT;
             ALTER TABLE hook_run ADD COLUMN worker_detail TEXT;
             ALTER TABLE hook_run ADD COLUMN worker_session_id TEXT;",
        )
        .map_err(|error| error.to_string())
}

/// SQLite cannot widen a CHECK in place, so v1 is rebuilt under the caller's immediate lock.
fn migrate_v1(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(&format!(
            "ALTER TABLE hook_run RENAME TO hook_run_v1;
             {CREATE_V2_TABLES}
             INSERT INTO hook_run (
                 id, started_at_ms, finished_at_ms, harness, hook, status, detail,
                 action, reason, session_id, workspace, plugin_version, runtime_version,
                 worker_status, worker_origin, worker_started_at_ms, worker_finished_at_ms,
                 worker_reason, worker_detail, worker_session_id
             )
             SELECT id, started_at_ms, finished_at_ms, harness, hook, status, detail,
                    action, reason, session_id, workspace, plugin_version, runtime_version,
                    worker_status,
                    CASE WHEN action = 'spawn_worker' THEN 'direct' ELSE NULL END,
                    worker_started_at_ms, worker_finished_at_ms, worker_reason,
                    worker_detail, worker_session_id
             FROM hook_run_v1;
             DROP TABLE hook_run_v1;
             PRAGMA user_version = 2;"
        ))
        .map_err(|error| error.to_string())
}

fn ensure_write_schema(connection: &Connection) -> Result<(), String> {
    match schema_version(connection)? {
        0 => connection
            .execute_batch(&format!("{CREATE_V2_TABLES} PRAGMA user_version = 2;"))
            .map_err(|error| error.to_string()),
        LEGACY_SCHEMA_VERSION => {
            if !has_result_columns(connection)? {
                complete_sparse_v1(connection)?;
            }
            migrate_v1(connection)
        }
        SCHEMA_VERSION if has_worker_origin(connection)? && has_event_table(connection)? => Ok(()),
        SCHEMA_VERSION => Err("incomplete database schema version 2".to_owned()),
        version => Err(format!("unsupported database schema version {version}")),
    }
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(unix)]
fn private_permissions(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn private_permissions(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

fn usage() -> String {
    "usage: loam hooks begin <global-root> --harness <id> --hook <id> --workspace <absolute-path> --plugin-version <semver> [--session-id <id>]\n       loam hooks finish <global-root> --id <positive-integer> --status <succeeded|failed|continued> [--action <spawn_worker|skip|request_worker>] [--reason <id>] [--detail <diagnostic>]\n       loam hooks event <global-root> --id <positive-integer> --event <type> [--phase <phase>] --outcome <outcome> [typed event fields]\n       loam hooks worker-start <global-root> --id <positive-integer> [--origin <external|fallback>] [--session-id <id>]\n       loam hooks worker-finish <global-root> --id <positive-integer> --status <succeeded|skipped|failed> --reason <id> [--origin <external|fallback>] [--session-id <id>] [--detail <diagnostic>]\n       loam hooks list [<global-root>] [--harness <id>] [--hook <id>] [--status <started|succeeded|failed|continued>] [--session-id <id>] [--limit <1..1000>]".to_owned()
}

#[cfg(test)]
mod native_run_tests {
    use super::{record_native_run, NativeRun, DATABASE_NAME};
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loam-native-run-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // #136 soft-degrade outcome class, at the ledger level: a `continued` run
    // round-trips every field the diagnosis needs, and the duration is the
    // recorded window (finished - started).
    #[test]
    fn a_soft_degraded_invocation_writes_one_complete_continued_row() {
        let root = temp_root("continued");
        record_native_run(NativeRun {
            root: &root,
            harness: "claude",
            event: "user_prompt_submit",
            session_id: Some("sess-1"),
            workspace: "/w/proj",
            status: "continued",
            detail: Some("connector_unreachable"),
            plugin_version: "9.9.9",
            started_at_ms: 1_000,
            finished_at_ms: 1_250,
        })
        .expect("the record writes");

        let connection = Connection::open(root.join(DATABASE_NAME)).unwrap();
        #[allow(clippy::type_complexity)]
        let (harness, hook, status, detail, session, workspace, started, finished, runtime): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            i64,
            i64,
            String,
        ) = connection
            .query_row(
                "SELECT harness, hook, status, detail, session_id, workspace, started_at_ms, finished_at_ms, runtime_version FROM hook_run",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(harness, "claude");
        assert_eq!(hook, "user_prompt_submit");
        assert_eq!(status, "continued");
        assert_eq!(detail.as_deref(), Some("connector_unreachable"));
        assert_eq!(session.as_deref(), Some("sess-1"));
        assert_eq!(workspace, "/w/proj");
        assert_eq!(finished - started, 250, "duration is the recorded window");
        assert_eq!(runtime, env!("CARGO_PKG_VERSION"));
    }

    // #136 fail-open: a store the record cannot open surfaces an error (which
    // the hook caller swallows) rather than panicking or hanging.
    #[test]
    fn an_unwritable_store_surfaces_an_error_the_caller_swallows() {
        let root = temp_root("unwritable");
        // A directory where the DB file must be: every Connection::open fails.
        std::fs::create_dir_all(root.join(DATABASE_NAME)).unwrap();
        let result = record_native_run(NativeRun {
            root: &root,
            harness: "claude",
            event: "SessionStart",
            session_id: None,
            workspace: "/w",
            status: "succeeded",
            detail: None,
            plugin_version: "9.9.9",
            started_at_ms: 1,
            finished_at_ms: 2,
        });
        assert!(
            result.is_err(),
            "an unwritable store must surface an error, not panic"
        );
    }
}

#[cfg(test)]
mod semver_tests {
    use super::valid_semver;

    #[test]
    fn prerelease_versions_are_accepted() {
        for version in [
            "0.13.0-next.0",
            "0.13.0-next.1",
            "0.13.0-rc.1",
            "0.13.0-alpha",
            "0.13.0-alpha.1-beta.2",
            "0.13.0-0",
        ] {
            assert!(valid_semver(version), "expected {version} to be valid");
        }
    }

    #[test]
    fn core_versions_are_accepted() {
        for version in ["0.13.0", "1.0.0", "0.0.1", "10.20.30"] {
            assert!(valid_semver(version), "expected {version} to be valid");
        }
    }

    #[test]
    fn build_metadata_is_rejected() {
        for version in ["0.13.0+build", "0.13.0-next.0+build", "1.2.3+sha.abc"] {
            assert!(!valid_semver(version), "expected {version} to be rejected");
        }
    }

    #[test]
    fn malformed_prereleases_are_rejected() {
        for version in [
            "0.13.0-",
            "0.13.0-next.",
            "0.13.0-.next",
            "0.13.0-next.01",
            "0.13.0-01",
            "0.13.0-next..1",
            "0.13.0-000000",
        ] {
            assert!(!valid_semver(version), "expected {version} to be rejected");
        }
    }

    #[test]
    fn malformed_core_is_rejected() {
        for version in ["", "0.13", "0.13.0.1", "01.13.0", "0.13.0x", "abc"] {
            assert!(!valid_semver(version), "expected {version} to be rejected");
        }
    }
}
