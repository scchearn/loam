//! Dormant per-user service definitions for the three native managers.
//!
//! The definitions are installed **disabled**: install renders the manager's
//! definition and registers it, but does not start it. A per-user connector is
//! enabled/started only after the first enrollment, and the
//! empty state stays dormant. This module never starts the connector, never
//! contacts a broker, and never creates the SQLite store.
//!
//! The machine's instance identity is no longer minted here: the client
//! certificate's SAN is the single identity source (`federation-enrollment-
//! simplification.md`). The service context carries the instance id supplied by
//! the caller, which derives it from the resolved certificate at connect time.
//!
//! Manager invocation goes through a [`CommandRunner`] seam consumed by generics
//! (no trait object, so the crate no-dispatch tripwire stays green); tests inject
//! a recording fake. The real managers — systemd `--user`, macOS LaunchAgent via
//! `launchctl`, Windows Task Scheduler via `schtasks` — are proven by the hosted
//! `ubuntu`/`macos-14`/`windows-2022` service-smoke legs, not by unit tests.
//!
//! Cross-platform module: each platform's render/argv helpers are dead on the
//! other platforms, so dead-code analysis is disabled module-wide rather than
//! scattered per-`cfg`. The lifecycle entry points are consumed by the
//! `federation service` CLI.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A recorded manager command: program plus arguments. Rendered by the pure
/// platform builders and executed by a [`CommandRunner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ManagerCommand {
    fn new(program: &str, args: &[&str]) -> Self {
        ManagerCommand {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }
}

/// Everything a definition needs: where Loam is installed, the stable instance
/// id, and the absolute current runtime the definition must reference.
#[derive(Debug, Clone)]
pub struct ServiceContext {
    pub global_root: PathBuf,
    pub instance_id: String,
    pub runtime_path: PathBuf,
    /// The systemd `--user` unit directory (`~/.config/systemd/user/` or
    /// `$XDG_CONFIG_HOME/systemd/user`). `None` when no user config dir can be
    /// resolved — the Linux symlink step then no-ops. Carried explicitly so
    /// tests inject a temp dir instead of touching a real user config.
    pub systemd_user_dir: Option<PathBuf>,
}

/// What one manager subprocess reported: its exit status plus a bounded capture
/// of everything it wrote. The text is the manager's own diagnosis —
/// `Bootstrap failed: 5: Input/output error`, `Load failed: 133: …` — and it is
/// what turns an opaque `connect_activation_failed` into a 30-second diagnosis
/// (#128). Both streams are captured because launchctl splits its reporting
/// across them inconsistently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerOutput {
    pub code: i32,
    pub detail: String,
}

impl ManagerOutput {
    /// A silent success — the shape a fake runner returns when it has nothing to
    /// say and the shape most manager commands really produce.
    pub fn ok() -> Self {
        ManagerOutput {
            code: 0,
            detail: String::new(),
        }
    }

    pub fn with_code(code: i32) -> Self {
        ManagerOutput {
            code,
            detail: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Io(String),
    InvalidRuntimePath,
    /// A manager step reported a nonzero status where the lifecycle required
    /// success. Names the exact command and the manager's own message, so the
    /// failure is diagnosable without re-running it by hand (#128).
    ManagerFailed {
        command: String,
        code: i32,
        detail: String,
    },
    NotUtf8,
    /// A manager subprocess did not exit within its bound and was killed. Names
    /// the full command — not just the program — because "launchctl hung" does
    /// not say *which* launchctl invocation hung, and the three steps of an
    /// activation wedge for entirely different reasons (#124).
    Timeout {
        command: String,
        seconds: u64,
    },
    /// The start step ran, but the service was observably dead afterwards: the
    /// manager never loaded it, or it is cycling on a nonzero exit. Carries the
    /// start command, the status the start step itself reported, and what the
    /// manager observed — the dead-service-behind-a-connected-outcome case
    /// (#101).
    StartRefused {
        command: String,
        code: i32,
        observed: String,
    },
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::Io(why) => write!(f, "service io error: {why}"),
            ServiceError::InvalidRuntimePath => write!(f, "runtime path is not absolute/UTF-8"),
            ServiceError::ManagerFailed {
                command,
                code,
                detail,
            } => {
                write!(f, "service manager exited {code}: `{command}`")?;
                if !detail.is_empty() {
                    write!(f, " — {detail}")?;
                }
                Ok(())
            }
            ServiceError::NotUtf8 => write!(f, "path is not representable as UTF-8"),
            ServiceError::Timeout { command, seconds } => {
                write!(
                    f,
                    "service manager did not exit within {seconds}s and was killed: `{command}`"
                )
            }
            ServiceError::StartRefused {
                command,
                code,
                observed,
            } => {
                write!(
                    f,
                    "the service did not start: `{command}` exited {code} and the manager reports {observed}"
                )
            }
        }
    }
}

/// The manager runner seam. `RealRunner` shells out; tests inject a fake.
pub trait CommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<ManagerOutput, ServiceError>;
}

/// Every manager subprocess is bound to this wall-clock ceiling. A wedged
/// `launchctl`/`systemctl` that never exits (launchd job with a non-zero last
/// exit is the observed case) is killed at the bound and surfaced as a typed
/// `Timeout`, rather than blocking service activation forever.
const MANAGER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How often the bounded wait polls the child. Small enough that a fast command
/// still returns promptly; large enough that the poll loop is free.
const MANAGER_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Executes manager commands as argv-vector subprocesses — never through a
/// shell, so no argument is ever word-split or interpreted.
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, command: &ManagerCommand) -> Result<ManagerOutput, ServiceError> {
        run_bounded(command, MANAGER_TIMEOUT)
    }
}

/// How much of a manager's own reporting is kept. Enough for launchctl's
/// multi-line "Bootstrap failed" block or a `launchctl print` job dump's opening
/// state lines; small enough that no error message is ever a log dump.
const MAX_MANAGER_DETAIL: usize = 4096;

/// Spawn a manager command and wait for it with a wall-clock bound. On expiry
/// the child is killed and reaped (so no zombie accumulates, the observed
/// launchctl leak) and a typed `Timeout` naming the command is returned. `std`
/// has no `wait_timeout`, so this is a `try_wait` poll loop — no new dependency.
///
/// Both output streams are redirected to one scratch file rather than to pipes:
/// a pipe that fills while the poll loop is not draining it would block the
/// child and turn a chatty command (`launchctl print` dumps a whole job) into a
/// false timeout. A file cannot fill. If the scratch file cannot be created the
/// command still runs, just without a captured detail — losing the diagnosis is
/// never a reason to lose the lifecycle step.
fn run_bounded(
    command: &ManagerCommand,
    timeout: std::time::Duration,
) -> Result<ManagerOutput, ServiceError> {
    use std::process::Stdio;
    let mut capture = ScratchCapture::create();
    let (out, err) = match capture.as_ref().and_then(ScratchCapture::stdio_pair) {
        Some((out, err)) => (Stdio::from(out), Stdio::from(err)),
        None => (Stdio::null(), Stdio::null()),
    };
    let mut child = std::process::Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|error| ServiceError::Io(error.to_string()))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(ManagerOutput {
                    code: status.code().unwrap_or(-1),
                    detail: capture.take().map(ScratchCapture::read).unwrap_or_default(),
                })
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    // Reap so the killed child does not linger as a zombie.
                    let _ = child.wait();
                    return Err(ServiceError::Timeout {
                        command: command_line(command),
                        seconds: timeout.as_secs(),
                    });
                }
                std::thread::sleep(MANAGER_POLL);
            }
            Err(error) => return Err(ServiceError::Io(error.to_string())),
        }
    }
}

/// A private scratch file that collects one manager subprocess's two output
/// streams, read back once the child has exited and removed on drop.
///
/// `create_new` is the whole trust story: the temp dir is world-writable, so the
/// file is only ever created fresh — an attacker-planted path (a symlink at the
/// name we picked) makes the create fail and capture degrade to none, never an
/// append into someone else's file.
struct ScratchCapture {
    path: PathBuf,
    file: std::fs::File,
}

impl ScratchCapture {
    fn create() -> Option<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "loam-manager-{}-{nanos}-{}.log",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .ok()?;
        Some(ScratchCapture { path, file })
    }

    /// Two independent handles onto the same file, so stdout and stderr both
    /// land in it in the order the child wrote them.
    fn stdio_pair(&self) -> Option<(std::fs::File, std::fs::File)> {
        Some((self.file.try_clone().ok()?, self.file.try_clone().ok()?))
    }

    /// The captured text, bounded and whitespace-trimmed. Invalid UTF-8 is
    /// replaced rather than refused: a mangled byte must not cost the diagnosis.
    fn read(self) -> String {
        let bytes = std::fs::read(&self.path).unwrap_or_default();
        let end = bytes.len().min(MAX_MANAGER_DETAIL);
        String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
    }
}

impl Drop for ScratchCapture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The command as an operator would retype it. Used in every typed error, so a
/// wedged or refused step names itself.
fn command_line(command: &ManagerCommand) -> String {
    if command.args.is_empty() {
        command.program.clone()
    } else {
        format!("{} {}", command.program, command.args.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Stable instance identity
// ---------------------------------------------------------------------------
//
// The instance id is the client certificate's SAN suffix, derived at connect
// time by `provisioning` (`certificate_instance_id`) and carried here in the
// [`ServiceContext`]. Nothing in this module mints, reads, or persists an
// instance id: the certificate is the single source, so the `instance_id` file
// and its divergence class are gone.

// ---------------------------------------------------------------------------
// Pure platform renderings (all always compiled, so every rendering is unit
// testable on any host; only the *selection* is cfg-gated)
// ---------------------------------------------------------------------------

const SERVICE_LABEL: &str = "io.loam.connector";

/// The disabled systemd `--user` unit. `Restart=on-failure` respawns the
/// connector when its liveness watchdog exits nonzero (code 75) — an inert
/// connector exits 0 and is deliberately not respawned. `RestartSec` spaces the
/// respawns so a fast-exit loop cannot trip systemd's start-limit, and both
/// streams are routed to the journal so the runtime breadcrumbs are captured —
/// stderr carries them, and stdout is captured too so a stray write or a
/// library's own output is not the one thing that vanishes (#103). Not
/// `WantedBy` any target beyond `[Install]`, so it stays dormant until enabled;
/// `disable --now` stops it, which wins over `Restart=`.
pub fn render_systemd_unit(ctx: &ServiceContext) -> Result<String, ServiceError> {
    let runtime = absolute_utf8(&ctx.runtime_path)?;
    Ok(format!(
        "[Unit]\n\
         Description=Loam federation connector\n\
         After=network.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={runtime} federation service run --global-root {root}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         StandardOutput=journal\n\
         StandardError=journal\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        root = absolute_utf8(&ctx.global_root)?,
    ))
}

/// The macOS LaunchAgent plist. `RunAtLoad` is `false` so bootstrapping only
/// *loads* the job — it stays dormant until first enrollment kickstarts it, and
/// `bootout` (disable) removes it entirely, which wins over `KeepAlive`.
/// `KeepAlive` is `{SuccessfulExit = false}` so once running, launchd respawns
/// the connector when its liveness watchdog exits nonzero (code 75) — the
/// self-heal the incident proved on this platform — while an inert connector that
/// exits 0 is left down. `StandardErrorPath` captures the runtime breadcrumbs and
/// `StandardOutPath` captures stdout, so a stray write or a library's own output
/// is not the one thing that vanishes (#103); launchd writes neither stream
/// anywhere unless the plist names a path. An `EnvironmentVariables` dict carries the `LOAM_*` overrides the
/// connector needs (launchd does not expand `$HOME`, so values are absolute or
/// literal).
pub fn render_launchagent_plist(ctx: &ServiceContext) -> Result<String, ServiceError> {
    let runtime = absolute_utf8(&ctx.runtime_path)?;
    let root = absolute_utf8(&ctx.global_root)?;
    let stdout_path = launchagent_capture_path(&ctx.global_root, "connector.stdout.log")?;
    let stderr_path = launchagent_capture_path(&ctx.global_root, "connector.stderr.log")?;
    let environment = launchagent_environment();
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key><string>{SERVICE_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\
         \t\t<string>{runtime}</string>\n\
         \t\t<string>federation</string>\n\t\t<string>service</string>\n\t\t<string>run</string>\n\
         \t\t<string>--global-root</string>\n\t\t<string>{root}</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key><false/>\n\
         \t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key><false/>\n\t</dict>\n\
         \t<key>StandardOutPath</key><string>{stdout_path}</string>\n\
         \t<key>StandardErrorPath</key><string>{stderr_path}</string>\n\
         {environment}\
         </dict>\n</plist>\n"
    ))
}

/// An absolute output-capture path for the LaunchAgent, under the global root
/// (which install has already created). launchd creates the file itself, so only
/// the parent needs to exist.
fn launchagent_capture_path(global_root: &Path, name: &str) -> Result<String, ServiceError> {
    let path = global_root.join(name);
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ServiceError::Io("global root path is not valid UTF-8".into()))
}

/// The `EnvironmentVariables` plist block: every `LOAM_*` variable set in this
/// process's environment, as `<string>` values (never `<data>`). Empty dict
/// when none are set. launchd does not expand `$HOME`, so the values are
/// passed through verbatim — an absolute path stays absolute.
///
/// `LOAM_SECRET_BACKEND` is deliberately excluded: the OS secret store is
/// removed, and a plist carrying the dead variable would resurrect the store
/// in the service environment (`federation-enrollment-simplification.md`).
fn launchagent_environment() -> String {
    let mut names: Vec<String> = std::env::vars()
        .filter(|(name, _)| name.starts_with("LOAM_") && name != "LOAM_SECRET_BACKEND")
        .map(|(name, _)| name)
        .collect();
    names.sort();
    if names.is_empty() {
        return "\t<key>EnvironmentVariables</key>\n\t<dict/>\n".to_owned();
    }
    let mut block = "\t<key>EnvironmentVariables</key>\n\t<dict>\n".to_owned();
    for name in names {
        if let Ok(value) = std::env::var(&name) {
            block.push_str(&format!("\t\t<key>{name}</key><string>{value}</string>\n"));
        }
    }
    block.push_str("\t</dict>\n");
    block
}

/// launchd domain naming. Manager commands are argv vectors run without a
/// shell (deliberately — no shell command construction), so a literal
/// `gui/$(id -u)` reaches `launchctl` unexpanded and is rejected as a bad
/// request. The real effective uid is rendered in instead.
#[cfg(target_os = "macos")]
mod launchd {
    // `uid_t geteuid(void);` — uid_t is `unsigned int` (u32) on macOS.
    extern "C" {
        fn geteuid() -> u32;
    }

    /// `gui/<uid>` — the per-user GUI domain `launchctl bootstrap` targets.
    pub fn gui_domain() -> String {
        // Safe: geteuid takes no arguments and cannot fail.
        format!("gui/{}", unsafe { geteuid() })
    }

    /// `gui/<uid>/<label>` — one service inside that domain.
    pub fn gui_service() -> String {
        format!("{}/{}", gui_domain(), super::SERVICE_LABEL)
    }
}

/// The Windows Task Scheduler create command for a current-user logon task
/// running with least privilege. `schtasks /Create` has no disable flag, so the
/// task is disabled by a separate `/Change` step (see
/// [`task_scheduler_disable_command`]). Rendered as argv for `schtasks.exe`.
pub fn task_scheduler_create_command(ctx: &ServiceContext) -> Result<ManagerCommand, ServiceError> {
    let runtime = absolute_utf8(&ctx.runtime_path)?;
    let root = absolute_utf8(&ctx.global_root)?;
    // The task action; quoting is handled by schtasks/argv, never a shell.
    let action = format!("\"{runtime}\" federation service run --global-root \"{root}\"");
    Ok(ManagerCommand {
        program: "schtasks".into(),
        args: vec![
            "/Create".into(),
            "/TN".into(),
            task_name(&ctx.instance_id),
            "/TR".into(),
            action,
            "/SC".into(),
            "ONLOGON".into(),
            "/RL".into(),
            "LIMITED".into(),
            "/F".into(),
        ],
    })
}

/// Disable the created task so it stays dormant until first enrollment.
/// `/Change /DISABLE` is the valid way — `/Create` has no disable flag.
pub fn task_scheduler_disable_command(ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand {
        program: "schtasks".into(),
        args: vec![
            "/Change".into(),
            "/TN".into(),
            task_name(&ctx.instance_id),
            "/DISABLE".into(),
        ],
    }
}

fn task_name(instance_id: &str) -> String {
    format!("Loam\\connector-{instance_id}")
}

// ---------------------------------------------------------------------------
// Platform command sets (pure argv builders)
// ---------------------------------------------------------------------------

/// The definition file path for the current platform's manager.
pub fn definition_path(ctx: &ServiceContext) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        ctx.global_root
            .join("systemd")
            .join("loam-connector.service")
    }
    #[cfg(target_os = "macos")]
    {
        ctx.global_root
            .join("launchagents")
            .join(format!("{SERVICE_LABEL}.plist"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Windows keeps its definition inside Task Scheduler, not a file.
        ctx.global_root.join("windows-task.marker")
    }
}

#[cfg(target_os = "linux")]
fn install_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![
        // Reload so the freshly-written unit is visible, but do NOT enable/start.
        ManagerCommand::new("systemctl", &["--user", "daemon-reload"]),
        // Leave it explicitly disabled until the first enrollment.
        ManagerCommand::new(
            "systemctl",
            &["--user", "disable", "loam-connector.service"],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn uninstall_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![
        ManagerCommand::new("systemctl", &["--user", "stop", "loam-connector.service"]),
        ManagerCommand::new(
            "systemctl",
            &["--user", "disable", "loam-connector.service"],
        ),
        ManagerCommand::new("systemctl", &["--user", "daemon-reload"]),
    ]
}

#[cfg(target_os = "linux")]
fn status_command(_ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand::new(
        "systemctl",
        &["--user", "is-enabled", "loam-connector.service"],
    )
}

#[cfg(target_os = "macos")]
fn install_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    // Dormant install writes the plist (done by the caller) and does NOT
    // bootstrap it; `launchctl bootstrap` happens only after first enrollment.
    Vec::new()
}

#[cfg(target_os = "macos")]
fn uninstall_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![ManagerCommand::new(
        "launchctl",
        &["bootout", &launchd::gui_service()],
    )]
}

#[cfg(target_os = "macos")]
fn status_command(_ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand::new("launchctl", &["print", &launchd::gui_service()])
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn install_commands(ctx: &ServiceContext) -> Vec<ManagerCommand> {
    // Create the logon task, then disable it (schtasks /Create has no disable
    // flag). Both commands are unit-tested cross-platform.
    match task_scheduler_create_command(ctx) {
        Ok(create) => vec![create, task_scheduler_disable_command(ctx)],
        Err(_) => vec![ManagerCommand::new("schtasks", &["/Query"])],
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn uninstall_commands(ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![ManagerCommand::new(
        "schtasks",
        &["/Delete", "/TN", &task_name(&ctx.instance_id), "/F"],
    )]
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn status_command(ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand::new("schtasks", &["/Query", "/TN", &task_name(&ctx.instance_id)])
}

// ---------------------------------------------------------------------------
// systemd user-unit discoverability (Linux)
// ---------------------------------------------------------------------------
//
// systemd --user searches `~/.config/systemd/user/` (or `$XDG_CONFIG_HOME/
// systemd/user/`), never the versioned unit under the global root. The unit
// stays where loam owns it; a symlink makes it discoverable. `install` places
// it, `enable_start` verifies it, `uninstall` removes it. The target dir is
// carried in [`ServiceContext`] so tests inject a temp dir instead of touching
// a real user config.

/// The symlink target inside the systemd user dir.
fn systemd_symlink_target(dir: &Path) -> PathBuf {
    dir.join("loam-connector.service")
}

/// Create a symlink, cross-platform. `std::fs::symlink` is Unix-only; Windows
/// needs the file variant. The systemd user dir is only ever used on Linux, but
/// this module compiles on every platform.
#[cfg(unix)]
fn make_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn make_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(source, target)
}

/// Place the discoverability symlink, idempotently. A symlink already pointing
/// at the versioned unit is a no-op; a stale or wrong symlink is replaced; a
/// real file at the target is refused rather than clobbered.
fn ensure_systemd_symlink(ctx: &ServiceContext, dir: &Path) -> Result<(), ServiceError> {
    let source = definition_path(ctx);
    let target = systemd_symlink_target(dir);
    std::fs::create_dir_all(dir).map_err(|e| ServiceError::Io(e.to_string()))?;
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current =
                std::fs::read_link(&target).map_err(|e| ServiceError::Io(e.to_string()))?;
            if current == source {
                return Ok(());
            }
            std::fs::remove_file(&target).map_err(|e| ServiceError::Io(e.to_string()))?;
        }
        Ok(_) => {
            return Err(ServiceError::Io(format!(
                "{} exists and is not a symlink; refusing to replace it",
                target.display()
            )));
        }
        Err(_) => {}
    }
    make_symlink(&source, &target).map_err(|e| ServiceError::Io(e.to_string()))
}

/// Whether the discoverability symlink is in place for this context.
fn systemd_symlink_present(ctx: &ServiceContext, dir: &Path) -> bool {
    let source = definition_path(ctx);
    let target = systemd_symlink_target(dir);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::read_link(&target)
            .map(|current| current == source)
            .unwrap_or(false),
        _ => false,
    }
}

/// The clear error `enable_start` reports when the symlink is absent.
fn systemd_symlink_missing_error() -> ServiceError {
    ServiceError::Io(
        "the systemd user unit is not symlinked into ~/.config/systemd/user/; \
         run `loam federation service install` first"
            .into(),
    )
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Install the dormant definition: write the definition file (where the platform
/// uses one), place the systemd discoverability symlink (Linux), and run the
/// disabled-registration manager commands. Never starts the connector.
/// Idempotent: re-installing overwrites the definition and re-runs the same
/// disabled registration.
pub fn install<R: CommandRunner>(runner: &R, ctx: &ServiceContext) -> Result<(), ServiceError> {
    write_definition(ctx)?;
    run_all(runner, &install_commands(ctx))?;
    // The manager's own `disable` removes every symlink to the unit from the
    // unit path — including the discoverability symlink just placed (systemd
    // `disable` undoes everything `enable` created, and the search-path symlink
    // is one of them). Re-assert it after the manager commands so a dormant
    // install still leaves the unit discoverable for the later `enable --now`.
    #[cfg(target_os = "linux")]
    if let Some(dir) = &ctx.systemd_user_dir {
        ensure_systemd_symlink(ctx, dir)?;
    }
    Ok(())
}

/// Remove the definition, its file, and (Linux) the discoverability symlink.
/// Idempotent and never contacts a broker.
pub fn uninstall<R: CommandRunner>(runner: &R, ctx: &ServiceContext) -> Result<(), ServiceError> {
    // Manager teardown first (best-effort), then the file.
    let _ = run_all(runner, &uninstall_commands(ctx));
    let path = definition_path(ctx);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| ServiceError::Io(e.to_string()))?;
    }
    #[cfg(target_os = "linux")]
    if let Some(dir) = &ctx.systemd_user_dir {
        // Only our own symlink: one pointing elsewhere is the operator's.
        if systemd_symlink_present(ctx, dir) {
            let _ = std::fs::remove_file(systemd_symlink_target(dir));
        }
    }
    Ok(())
}

/// Query the manager for the definition's state without starting anything.
pub fn status<R: CommandRunner>(runner: &R, ctx: &ServiceContext) -> Result<i32, ServiceError> {
    runner.run(&status_command(ctx)).map(|output| output.code)
}

/// One manager command plus what the lifecycle expects of it.
///
/// Only the step that actually *starts* the connector is held to an
/// expectation. Every other step stays exit-code-tolerant on purpose, and that
/// tolerance is load-bearing: `launchctl bootout` reports nonzero on a machine
/// where nothing is loaded yet, `launchctl bootstrap` reports nonzero on an
/// idempotent re-activation, and `systemctl --user disable` reports nonzero on a
/// fresh machine. A blanket strict check would break documented idempotency
/// (#101).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagerStep {
    command: ManagerCommand,
    start: bool,
}

fn step(command: ManagerCommand) -> ManagerStep {
    ManagerStep {
        command,
        start: false,
    }
}

fn start_step(command: ManagerCommand) -> ManagerStep {
    ManagerStep {
        command,
        start: true,
    }
}

/// Enable and start the connector after the first enrollment (T10 activation).
/// Idempotent — safe to call when already active. On Linux the systemd
/// discoverability symlink must be in place first: `enable --now` on a unit
/// systemd cannot see silently no-ops, so a missing symlink is refused with a
/// clear error instead.
///
/// The start step is followed by a bounded confirmation: a start command that
/// exits nonzero and leaves the service dead used to be reported as a successful
/// activation, so `connect` printed a connected outcome over a connector that
/// never ran (#101). The exit status of the start step alone cannot decide it —
/// an inert connector legitimately exits 0 and stays down — so the manager is
/// asked what it observed instead.
pub fn enable_start<R: CommandRunner>(
    runner: &R,
    ctx: &ServiceContext,
) -> Result<(), ServiceError> {
    #[cfg(target_os = "linux")]
    if let Some(dir) = &ctx.systemd_user_dir {
        if !systemd_symlink_present(ctx, dir) {
            return Err(systemd_symlink_missing_error());
        }
    }
    let mut started: Option<(String, i32)> = None;
    for step in enable_start_commands(ctx) {
        let output = runner.run(&step.command)?;
        if step.start {
            started = Some((command_line(&step.command), output.code));
        }
    }
    match started {
        Some((command, code)) => confirm_started(runner, ctx, &command, code),
        // A platform with no distinguishable start step has nothing to confirm.
        None => Ok(()),
    }
}

/// Disable and stop the connector after the final disconnect (T11). Idempotent.
pub fn disable_stop<R: CommandRunner>(
    runner: &R,
    ctx: &ServiceContext,
) -> Result<(), ServiceError> {
    run_all(runner, &disable_stop_commands(ctx))
}

/// How long the start confirmation watches the manager. Long enough for launchd
/// to record a job that dies on spawn (the exit-78 respawn cycle), short enough
/// that it is invisible next to connect's own broker probe.
const START_CONFIRM_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// How often the confirmation asks the manager. Coarse on purpose: every probe
/// is a real manager subprocess, and a tight loop of them is exactly the kind of
/// launchctl traffic the runners are unhappy about (#124).
const START_CONFIRM_POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// What the manager observed about the service after the start step.
enum StartVerdict {
    /// Observably alive. Ends the watch early — there is nothing left to catch.
    Alive,
    /// Observably dead — the manager does not have the service, or it is cycling
    /// on a nonzero exit. Carries what to put in the error.
    Dead(String),
    /// Nothing says it failed, and nothing says it is up either. Not a failure:
    /// an inert connector exits 0 by design, and calling that dead would refuse
    /// every activation on a machine with an empty registry.
    NoFailure,
}

/// Watch the manager for the confirmation budget and refuse the activation the
/// moment the service is observably dead. A probe the runner cannot even execute
/// is not evidence of death — the start itself succeeded — so a hard runner
/// error ends the watch quietly rather than failing an activation on it.
fn confirm_started<R: CommandRunner>(
    runner: &R,
    ctx: &ServiceContext,
    command: &str,
    code: i32,
) -> Result<(), ServiceError> {
    let deadline = std::time::Instant::now() + START_CONFIRM_BUDGET;
    let probe = start_probe_command(ctx);
    loop {
        let Ok(output) = runner.run(&probe) else {
            return Ok(());
        };
        match start_verdict(&output) {
            StartVerdict::Alive => return Ok(()),
            StartVerdict::Dead(observed) => {
                return Err(ServiceError::StartRefused {
                    command: command.to_owned(),
                    code,
                    observed,
                })
            }
            StartVerdict::NoFailure => {}
        }
        if std::time::Instant::now() >= deadline {
            return Ok(());
        }
        std::thread::sleep(START_CONFIRM_POLL);
    }
}

#[cfg(target_os = "linux")]
fn enable_start_commands(_ctx: &ServiceContext) -> Vec<ManagerStep> {
    vec![
        // Reload so the freshly-symlinked unit is visible to the manager.
        step(ManagerCommand::new(
            "systemctl",
            &["--user", "daemon-reload"],
        )),
        start_step(ManagerCommand::new(
            "systemctl",
            &["--user", "enable", "--now", "loam-connector.service"],
        )),
    ]
}

/// systemd's own record of how the unit last finished. `show` always exits 0, so
/// the evidence is the property, not the status — and asking for the result
/// rather than for `is-active` is what keeps an inert connector out of the
/// failure class: it exits 0 by design, leaving the unit inactive with
/// `Result=success`.
#[cfg(target_os = "linux")]
fn start_probe_command(_ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand::new(
        "systemctl",
        &[
            "--user",
            "show",
            "-p",
            "Result",
            "-p",
            "ActiveState",
            "loam-connector.service",
        ],
    )
}

#[cfg(target_os = "linux")]
fn start_verdict(output: &ManagerOutput) -> StartVerdict {
    if let Some(result) = unit_result(&output.detail) {
        if result != "success" {
            return StartVerdict::Dead(format!("the unit's result is {result}"));
        }
    }
    if unit_property(&output.detail, "ActiveState") == Some("active") {
        return StartVerdict::Alive;
    }
    StartVerdict::NoFailure
}

/// The `Result=` value out of `systemctl show`, or `None` when systemd did not
/// report one — which is not evidence of a failure.
fn unit_result(properties: &str) -> Option<&str> {
    unit_property(properties, "Result")
}

/// One `systemctl show` property value, or `None` when systemd did not report
/// it — which is not evidence of anything.
fn unit_property<'a>(properties: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    properties
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "linux")]
fn disable_stop_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![ManagerCommand::new(
        "systemctl",
        &["--user", "disable", "--now", "loam-connector.service"],
    )]
}

/// launchd respawns from its **in-memory** job spec, so a rewritten plist is
/// invisible to it until the job is reloaded: after a runtime update the
/// definition, the ledger and verification all named the new version while the
/// process was still executing the old binary (#131). `bootout` is the only way
/// to drop that spec, so activation is a real reload — bootout, then bootstrap
/// of the definition on disk — not a respawn of whatever launchd already holds.
/// The same sequence is what loads a definition on a machine where no job exists
/// at all, which is the first-run dead end connect hit on fresh darwin (#128).
///
/// The cost is that re-activating an already-healthy connector bounces it. That
/// is deliberate: the callers of this function (first activation, the installer's
/// post-update refresh, drift repair) all want the running process to match the
/// definition on disk, and only a reload can promise that.
/// Always compiled, and taking the domain/service targets as arguments, so the
/// ordering is unit-testable on any host — the same rule the plist renderer
/// follows. Only the *selection* below is cfg-gated.
fn launchagent_enable_start_steps(plist: &str, domain: &str, service: &str) -> Vec<ManagerStep> {
    vec![
        step(ManagerCommand::new("launchctl", &["bootout", service])),
        // `disable` writes a persistent override that makes a later `bootstrap`
        // fail outright, so clearing it has to precede the load, not follow it.
        step(ManagerCommand::new("launchctl", &["enable", service])),
        step(ManagerCommand::new(
            "launchctl",
            &["bootstrap", domain, plist],
        )),
        // The plist is dormant (`RunAtLoad` false), so bootstrapping it only
        // *loads* the job; the start is explicit, as it is for systemd's
        // `enable --now` and schtasks' `/Run`.
        //
        // Deliberately NOT `kickstart -k`: `-k` kills the current instance and
        // forces its respawn through launchd's ThrottleInterval (10s by
        // default), and kickstart blocks for that whole window — which is
        // exactly the 10s "launchctl did not exit and was killed" wedge the
        // hosted macos runners hit (#124). After the bootout above there is no
        // instance left to kill, so `-k` bought nothing but the wedge.
        start_step(ManagerCommand::new("launchctl", &["kickstart", service])),
    ]
}

#[cfg(target_os = "macos")]
fn enable_start_commands(ctx: &ServiceContext) -> Vec<ManagerStep> {
    launchagent_enable_start_steps(
        &definition_path(ctx).to_string_lossy(),
        &launchd::gui_domain(),
        &launchd::gui_service(),
    )
}

#[cfg(target_os = "macos")]
fn start_probe_command(_ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand::new("launchctl", &["print", &launchd::gui_service()])
}

/// launchd's own job dump is the evidence. A print that fails means the job is
/// not in the domain at all — the start could not have worked. A print that
/// reports a nonzero last exit means the job ran and died, which with
/// `KeepAlive`/`SuccessfulExit=false` is the respawn cycle that reported a
/// connected outcome over a dead connector (#101, observed last exit 78).
#[cfg(target_os = "macos")]
fn start_verdict(output: &ManagerOutput) -> StartVerdict {
    if output.code != 0 {
        return StartVerdict::Dead("the job is not loaded in the domain".to_owned());
    }
    if let Some(status) = last_exit_status(&output.detail) {
        if status != 0 {
            return StartVerdict::Dead(format!("the job's last exit status is {status}"));
        }
    }
    if output
        .detail
        .lines()
        .any(|line| line.trim() == "state = running")
    {
        return StartVerdict::Alive;
    }
    StartVerdict::NoFailure
}

/// The last exit status out of a `launchctl print` dump, or `None` when the job
/// has not exited yet or launchd words it in a way this does not recognise.
/// launchd has spelled the key both `last exit code` and `last exit status`
/// across releases and writes non-numeric values (`(never exited)`) too, so both
/// spellings are read and anything non-numeric is simply not evidence.
fn last_exit_status(dump: &str) -> Option<i32> {
    dump.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key != "last exit code" && key != "last exit status" {
                return None;
            }
            value.trim().parse::<i32>().ok()
        })
        .next_back()
}

#[cfg(target_os = "macos")]
fn disable_stop_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![ManagerCommand::new(
        "launchctl",
        &["bootout", &launchd::gui_service()],
    )]
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn enable_start_commands(ctx: &ServiceContext) -> Vec<ManagerStep> {
    let name = task_name(&ctx.instance_id);
    vec![
        step(ManagerCommand::new(
            "schtasks",
            &["/Change", "/TN", &name, "/ENABLE"],
        )),
        start_step(ManagerCommand::new("schtasks", &["/Run", "/TN", &name])),
    ]
}

/// Task Scheduler's own registration check. It proves the task the `/Run` step
/// addressed exists and is queryable; the task's *last result* is only in
/// `/Query /V` output, which reading needs the Windows service parity work
/// tracked separately with #100 — so the verdict here is deliberately the weaker
/// one rather than a parse this platform has no coverage for.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_probe_command(ctx: &ServiceContext) -> ManagerCommand {
    ManagerCommand::new("schtasks", &["/Query", "/TN", &task_name(&ctx.instance_id)])
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_verdict(output: &ManagerOutput) -> StartVerdict {
    if output.code != 0 {
        StartVerdict::Dead("the task is not registered".to_owned())
    } else {
        StartVerdict::NoFailure
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn disable_stop_commands(ctx: &ServiceContext) -> Vec<ManagerCommand> {
    let name = task_name(&ctx.instance_id);
    vec![
        ManagerCommand::new("schtasks", &["/End", "/TN", &name]),
        ManagerCommand::new("schtasks", &["/Change", "/TN", &name, "/DISABLE"]),
    ]
}

fn write_definition(ctx: &ServiceContext) -> Result<(), ServiceError> {
    let content = current_definition(ctx)?;
    if let Some(content) = content {
        let path = definition_path(ctx);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ServiceError::Io(e.to_string()))?;
        }
        std::fs::write(&path, content).map_err(|e| ServiceError::Io(e.to_string()))?;
    }
    Ok(())
}

/// The definition content for the current platform, or `None` when the platform
/// keeps its definition inside the manager (Windows Task Scheduler).
fn current_definition(ctx: &ServiceContext) -> Result<Option<String>, ServiceError> {
    #[cfg(target_os = "linux")]
    {
        Ok(Some(render_systemd_unit(ctx)?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Some(render_launchagent_plist(ctx)?))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = ctx;
        Ok(None)
    }
}

fn run_all<R: CommandRunner>(runner: &R, commands: &[ManagerCommand]) -> Result<(), ServiceError> {
    for command in commands {
        // A best-effort disable/reload may report non-zero on a fresh machine
        // (nothing to disable yet); only a hard runner error propagates. The
        // hosted smoke asserts the real observable state, not each exit code.
        // The one step that IS held to an expectation is the start, and it is
        // checked against the manager's observation in `enable_start`.
        let _ = runner.run(command)?;
    }
    Ok(())
}

fn absolute_utf8(path: &Path) -> Result<String, ServiceError> {
    if !path.is_absolute() {
        return Err(ServiceError::InvalidRuntimePath);
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or(ServiceError::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // A subprocess that never exits on its own must be killed at the bound and
    // surfaced as a typed Timeout naming the whole command — never hang the
    // caller (the macOS `launchctl kickstart` wedge). "launchctl hung" does not
    // say WHICH launchctl invocation hung, which is the whole diagnostic value
    // of the message (#124), so the argv has to be in it. `sleep 30` is the
    // never-exits stub; the bound is short so the test is fast, and the elapsed
    // assertion proves the kill actually fired rather than the sleep completing.
    #[cfg(unix)]
    #[test]
    fn a_wedged_manager_subprocess_is_killed_at_the_bound_and_named_in_full() {
        let wedged = ManagerCommand::new("sleep", &["30"]);
        let bound = std::time::Duration::from_millis(200);
        let started = std::time::Instant::now();
        let result = run_bounded(&wedged, bound);
        let elapsed = started.elapsed();

        assert_eq!(
            result,
            Err(ServiceError::Timeout {
                command: "sleep 30".to_owned(),
                seconds: 0,
            }),
            "a subprocess past its bound must surface a typed Timeout naming the whole command"
        );
        assert!(
            result.unwrap_err().to_string().contains("sleep 30"),
            "the rendered message must carry the argv, not just the program"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "the bound must fire near its deadline, not wait for the child to exit: {elapsed:?}"
        );
    }

    // The bound must not penalize a command that returns promptly: a fast exit
    // inside the window resolves to its real code, not a Timeout.
    #[cfg(unix)]
    #[test]
    fn a_prompt_manager_subprocess_returns_its_real_exit_code() {
        let quick = ManagerCommand::new("sh", &["-c", "exit 3"]);
        let result = run_bounded(&quick, std::time::Duration::from_secs(10));
        assert_eq!(
            result.map(|output| output.code),
            Ok(3),
            "a command that exits within the bound must surface its real code"
        );
    }

    // The manager's own words are the diagnosis an opaque activation failure was
    // missing (#128): both streams are captured, in write order, so launchctl's
    // "Bootstrap failed: …" is never the thing that vanishes.
    #[cfg(unix)]
    #[test]
    fn a_manager_subprocess_captures_both_of_its_output_streams() {
        let chatty = ManagerCommand::new("sh", &["-c", "echo to-stdout; echo to-stderr 1>&2"]);
        let output = run_bounded(&chatty, std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(output.code, 0);
        assert!(
            output.detail.contains("to-stdout") && output.detail.contains("to-stderr"),
            "both streams must reach the captured detail; got: {}",
            output.detail
        );
    }

    // A command that writes far more than a pipe buffer would hold must still
    // exit and be captured: the capture goes to a file precisely so a chatty
    // `launchctl print` cannot deadlock into a false timeout. The stored detail
    // is bounded so no error message is ever a log dump.
    #[cfg(unix)]
    #[test]
    fn a_chatty_manager_subprocess_neither_wedges_nor_dumps_its_whole_output() {
        let flood = ManagerCommand::new("sh", &["-c", "i=0; while [ $i -lt 20000 ]; do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; i=$((i+1)); done"]);
        let output = run_bounded(&flood, std::time::Duration::from_secs(10))
            .expect("a command writing past a pipe buffer must still complete");
        assert_eq!(output.code, 0, "the flood must exit cleanly, not be killed");
        assert!(
            output.detail.len() <= MAX_MANAGER_DETAIL,
            "the captured detail must stay bounded; got {} bytes",
            output.detail.len()
        );
    }

    // The scratch file is an implementation detail, not litter: one per manager
    // command, in a shared temp dir, would accumulate forever.
    #[test]
    fn a_capture_file_is_removed_when_its_capture_is_dropped() {
        let capture = ScratchCapture::create().expect("a scratch capture in the temp dir");
        let path = capture.path.clone();
        assert!(path.exists(), "the capture file exists while it is held");
        drop(capture);
        assert!(
            !path.exists(),
            "the capture file must not survive its owner"
        );
    }

    struct FakeRunner {
        recorded: RefCell<Vec<ManagerCommand>>,
    }

    impl FakeRunner {
        fn new() -> Self {
            FakeRunner {
                recorded: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, command: &ManagerCommand) -> Result<ManagerOutput, ServiceError> {
            self.recorded.borrow_mut().push(command.clone());
            Ok(ManagerOutput::ok())
        }
    }

    // --- #131/#128: the darwin activation is a real reload ---

    /// The steps as an operator would read them, for the always-compiled
    /// launchd builder. Domain and service are literals so this runs on any
    /// host: only the *selection* of this builder is macOS-only.
    fn launchagent_lines() -> Vec<String> {
        launchagent_enable_start_steps(
            "/root/launchagents/io.loam.connector.plist",
            "gui/501",
            "gui/501/io.loam.connector",
        )
        .iter()
        .map(|step| command_line(&step.command))
        .collect()
    }

    #[test]
    fn the_launchd_activation_boots_the_old_job_out_before_bootstrapping_the_new_definition() {
        let lines = launchagent_lines();
        let position = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no `{needle}` step in {lines:?}"))
        };
        // launchd respawns from its in-memory job spec: without the bootout the
        // rewritten plist is never read and the old runtime keeps executing
        // (#131). Bootstrapping the definition is also what loads it on a
        // machine where no job exists at all (#128).
        assert!(
            position("bootout") < position("bootstrap"),
            "the load must follow a bootout, or launchd keeps the stale job spec: {lines:?}"
        );
        // A persistent `disable` override makes bootstrap fail outright, so the
        // enable has to clear it first.
        assert!(
            position("enable") < position("bootstrap"),
            "the enable must precede the bootstrap: {lines:?}"
        );
        assert!(
            position("bootstrap") < position("kickstart"),
            "the job must be loaded before it is started: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("bootstrap gui/501 /root/launchagents/")),
            "the bootstrap must name the rendered definition on disk: {lines:?}"
        );
    }

    #[test]
    fn the_launchd_start_never_uses_the_kill_flag_that_wedges_on_the_respawn_throttle() {
        let lines = launchagent_lines();
        // `kickstart -k` kills the running instance and forces its respawn
        // through launchd's ThrottleInterval; kickstart blocks for that whole
        // window, which is the 10s "launchctl did not exit" wedge (#124). After
        // the bootout there is no instance to kill anyway.
        assert!(
            !lines.iter().any(|line| line.contains("kickstart -k")),
            "the start step must not pass -k: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "launchctl kickstart gui/501/io.loam.connector"),
            "the start step must still start the dormant job: {lines:?}"
        );
    }

    #[test]
    fn only_the_start_step_carries_an_expectation() {
        let steps = launchagent_enable_start_steps("/p.plist", "gui/501", "gui/501/label");
        let started: Vec<&ManagerStep> = steps.iter().filter(|step| step.start).collect();
        assert_eq!(
            started.len(),
            1,
            "exactly one step is the start; bootout/enable/bootstrap stay \
             exit-code-tolerant because they report nonzero on idempotent \
             re-activation and on fresh machines (#101)"
        );
        assert!(started[0].command.args.contains(&"kickstart".to_owned()));
    }

    // --- #101: a start that leaves the service dead is not a success ---

    #[test]
    fn a_nonzero_last_exit_in_a_launchd_dump_is_a_dead_service() {
        // The observed shape: kickstart cycling with last-exit 78 while the job
        // never ran, which used to be reported as a successful activation.
        let dump = "state = not running\n\tlast exit code = 78\n";
        assert_eq!(last_exit_status(dump), Some(78));
        // A clean inert exit is NOT a failure: the connector exits 0 by design
        // when the registry is empty.
        assert_eq!(last_exit_status("\tlast exit code = 0\n"), Some(0));
        // launchd's older spelling, and its non-numeric value, both handled.
        assert_eq!(last_exit_status("\tlast exit status = 75\n"), Some(75));
        assert_eq!(
            last_exit_status("\tlast exit code = (never exited)\n"),
            None
        );
        assert_eq!(last_exit_status("state = running\n"), None);
    }

    #[test]
    fn a_failed_unit_result_is_a_dead_service_but_a_clean_one_is_not() {
        assert_eq!(unit_result("Result=exit-code\n"), Some("exit-code"));
        assert_eq!(unit_result("Result=success\n"), Some("success"));
        // No property reported is not evidence of a failure.
        assert_eq!(unit_result(""), None);
        assert_eq!(unit_result("Result=\n"), None);
    }

    /// A runner that reports every lifecycle command as a clean success but
    /// answers the start probe with the current platform's "the service is
    /// dead" evidence.
    struct DeadServiceRunner {
        probe: ManagerCommand,
    }

    impl CommandRunner for DeadServiceRunner {
        fn run(&self, command: &ManagerCommand) -> Result<ManagerOutput, ServiceError> {
            if command == &self.probe {
                return Ok(dead_probe_output());
            }
            Ok(ManagerOutput::ok())
        }
    }

    /// What each manager says when the service it was asked to start is dead.
    #[cfg(target_os = "linux")]
    fn dead_probe_output() -> ManagerOutput {
        ManagerOutput {
            code: 0,
            detail: "Result=exit-code".into(),
        }
    }

    #[cfg(target_os = "macos")]
    fn dead_probe_output() -> ManagerOutput {
        ManagerOutput {
            code: 0,
            detail: "\tlast exit code = 78".into(),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn dead_probe_output() -> ManagerOutput {
        ManagerOutput::with_code(1)
    }

    #[test]
    fn a_start_that_leaves_the_service_dead_is_refused_not_reported_as_activated() {
        let context = ctx("dead-probe");
        let runner = DeadServiceRunner {
            probe: start_probe_command(&context),
        };
        #[cfg(target_os = "linux")]
        install(&FakeRunner::new(), &context).unwrap();
        let error = enable_start(&runner, &context)
            .expect_err("a dead service behind a successful start must not report activated");
        let rendered = error.to_string();
        assert!(
            matches!(error, ServiceError::StartRefused { .. }),
            "the refusal must be typed, not a bare io error: {rendered}"
        );
        // The message has to carry both halves of the diagnosis: which command
        // was the start, and what the manager observed afterwards.
        assert!(
            rendered.contains("did not start"),
            "the refusal must say the service did not start: {rendered}"
        );
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(context.systemd_user_dir.as_ref().unwrap());
    }

    #[test]
    fn a_healthy_start_is_confirmed_without_a_manager_query_failing_it() {
        let context = ctx("live-probe");
        let runner = FakeRunner::new();
        install(&runner, &context).unwrap();
        // The all-clean runner is the inert connector: nothing reports a
        // failure, so activation succeeds and the probe never invents one.
        enable_start(&runner, &context).expect("a clean manager must confirm the start");
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(context.systemd_user_dir.as_ref().unwrap());
    }

    fn ctx(label: &str) -> ServiceContext {
        let root = std::env::temp_dir().join(format!(
            "loam-svc8-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        ServiceContext {
            global_root: root,
            instance_id: "0123456789abcdef0123456789abcdef".into(),
            // Absolute on every platform (temp_dir is absolute on Windows too),
            // so `absolute_utf8` accepts it in the render tests.
            runtime_path: std::env::temp_dir().join("loam-runtime").join("loam"),
            // A temp systemd user dir, so the Linux symlink step never touches
            // a real user config in tests.
            systemd_user_dir: Some(std::env::temp_dir().join(format!(
                    "loam-svc8-{label}-systemd-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ))),
        }
    }

    fn runtime_string(context: &ServiceContext) -> String {
        context.runtime_path.to_str().unwrap().to_owned()
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_launchd_domain_carries_a_real_uid_not_a_shell_substitution() {
        let domain = launchd::gui_domain();
        let uid = domain.strip_prefix("gui/").expect("gui/<uid> domain");
        assert!(!uid.is_empty() && uid.chars().all(|c| c.is_ascii_digit()));
        assert!(!domain.contains('$') && !domain.contains('('));
        assert_eq!(launchd::gui_service(), format!("{domain}/{SERVICE_LABEL}"));
        // The commands the real manager runs carry the same expanded domain.
        for step in enable_start_commands(&ctx("launchd-domain")) {
            assert!(step.command.args.iter().all(|a| !a.contains('$')));
        }
    }

    #[test]
    fn systemd_unit_is_dormant_and_respawns_the_watchdog_exit() {
        let context = ctx("systemd");
        let runtime = runtime_string(&context);
        let unit = render_systemd_unit(&context).unwrap();
        assert!(unit.contains(&format!(
            "ExecStart={runtime} federation service run --global-root"
        )));
        // Respawn on the watchdog's nonzero exit; an inert exit(0) is left down.
        assert!(unit.contains("Restart=on-failure"));
        // Spacing so a fast-exit loop cannot trip the start-limit, and stderr
        // breadcrumbs are captured in the journal.
        assert!(unit.contains("RestartSec="));
        assert!(unit.contains("StandardError=journal"));
        // Both streams, not just stderr: an unexpected stdout write is captured
        // rather than discarded (#103).
        assert!(unit.contains("StandardOutput=journal"));
        // No socket activation, no auto-start beyond an explicit enable.
        assert!(!unit.contains("RunAtLoad"));
    }

    #[test]
    fn launchagent_plist_is_dormant_and_respawns_only_a_nonzero_exit() {
        let context = ctx("launchd");
        let runtime = runtime_string(&context);
        let plist = render_launchagent_plist(&context).unwrap();
        // Dormant until kickstarted; bootout (disable) removes it entirely.
        assert!(plist.contains("<key>RunAtLoad</key><false/>"));
        // KeepAlive with SuccessfulExit=false: respawn a nonzero (watchdog) exit,
        // leave a clean inert exit(0) down. This is the macOS half of the
        // incident's self-heal, absent before.
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>SuccessfulExit</key><false/>"));
        assert!(!plist.contains("<key>KeepAlive</key><false/>"));
        // Runtime breadcrumbs are captured to files under the global root.
        // launchd discards a stream the plist does not name, so both are named
        // (#103) — stderr carries the breadcrumbs, stdout catches strays.
        assert!(plist.contains("<key>StandardErrorPath</key>"));
        assert!(plist.contains("connector.stderr.log"));
        assert!(plist.contains("<key>StandardOutPath</key>"));
        assert!(plist.contains("connector.stdout.log"));
        assert!(plist.contains(&format!("<string>{runtime}</string>")));
        assert!(plist.contains(SERVICE_LABEL));
    }

    #[test]
    fn launchagent_plist_carries_an_environment_variables_key() {
        let context = ctx("launchd-env");
        let plist = render_launchagent_plist(&context).unwrap();
        assert!(
            plist.contains("<key>EnvironmentVariables</key>"),
            "the plist must carry an EnvironmentVariables key"
        );
        // The dict is present even when empty (launchd accepts an empty dict).
        assert!(plist.contains("<dict/>") || plist.contains("</dict>"));
    }

    #[test]
    fn launchagent_environment_renders_loam_vars_as_strings_and_drops_the_secret_backend() {
        // The renderer reads the process environment, so the test sets one
        // LOAM_* variable and asserts it appears as a <string>, never <data>.
        // The variable is removed afterwards so a parallel test never sees it.
        // `LOAM_SECRET_BACKEND` is the dead secret-store switch: the renderer
        // must never carry it into a service environment.
        std::env::set_var("LOAM_SECRET_BACKEND", "security");
        std::env::set_var("LOAM_TEST_VAR", "visible");
        let block = launchagent_environment();
        std::env::remove_var("LOAM_SECRET_BACKEND");
        std::env::remove_var("LOAM_TEST_VAR");
        assert!(
            block.contains("<key>LOAM_TEST_VAR</key><string>visible</string>"),
            "LOAM_* vars must render as <string> values; got: {block}"
        );
        assert!(
            !block.contains("LOAM_SECRET_BACKEND"),
            "the removed secret backend must never reach a service environment: {block}"
        );
        assert!(!block.contains("<data>"), "values must never be <data>");
    }

    #[test]
    fn task_scheduler_create_is_least_privilege_logon_without_invalid_disable_flag() {
        let create = task_scheduler_create_command(&ctx("schtasks")).unwrap();
        assert_eq!(create.program, "schtasks");
        assert!(create.args.contains(&"/Create".to_owned()));
        assert!(create.args.contains(&"LIMITED".to_owned()));
        assert!(create.args.contains(&"ONLOGON".to_owned()));
        // /Create has no /DISABLE flag — disabling is a separate /Change.
        assert!(!create.args.contains(&"/DISABLE".to_owned()));
    }

    #[test]
    fn task_scheduler_disable_uses_change() {
        let disable = task_scheduler_disable_command(&ctx("schtasks"));
        assert_eq!(disable.program, "schtasks");
        assert!(disable.args.contains(&"/Change".to_owned()));
        assert!(disable.args.contains(&"/DISABLE".to_owned()));
    }

    #[test]
    fn a_relative_runtime_path_is_rejected() {
        let mut context = ctx("relative");
        context.runtime_path = PathBuf::from("bin/loam");
        assert_eq!(
            render_systemd_unit(&context),
            Err(ServiceError::InvalidRuntimePath)
        );
    }

    #[test]
    fn install_writes_the_definition_and_never_starts() {
        let context = ctx("install");
        let runner = FakeRunner::new();
        install(&runner, &context).unwrap();
        // On this platform a definition file was written (Linux/macOS).
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(definition_path(&context).exists());
        // No manager command starts the service.
        for command in runner.recorded.borrow().iter() {
            let joined = command.args.join(" ");
            assert!(
                !joined.contains("start") && !joined.contains("bootstrap"),
                "install must not start the service: {joined}"
            );
        }
        let _ = std::fs::remove_dir_all(&context.global_root);
    }

    #[test]
    fn install_is_idempotent() {
        let context = ctx("idempotent");
        let runner = FakeRunner::new();
        install(&runner, &context).unwrap();
        // Re-installing overwrites without error.
        install(&runner, &context).unwrap();
        let _ = std::fs::remove_dir_all(&context.global_root);
    }

    // --- T6: systemd user-unit discoverability symlink (Linux) ---

    #[cfg(target_os = "linux")]
    #[test]
    fn install_symlinks_the_unit_into_the_systemd_user_dir() {
        let context = ctx("symlink");
        let runner = FakeRunner::new();
        install(&runner, &context).unwrap();
        let dir = context.systemd_user_dir.as_ref().unwrap();
        let target = systemd_symlink_target(dir);
        assert!(
            std::fs::symlink_metadata(&target)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "install must symlink the unit into the systemd user dir"
        );
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            definition_path(&context),
            "the symlink must point at the versioned unit under the global root"
        );
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn install_replaces_a_stale_symlink_and_is_idempotent() {
        let context = ctx("symlink-stale");
        let runner = FakeRunner::new();
        let dir = context.systemd_user_dir.as_ref().unwrap();
        let target = systemd_symlink_target(dir);
        std::fs::create_dir_all(dir).unwrap();
        // A stale symlink pointing elsewhere must be replaced, not followed.
        std::os::unix::fs::symlink("/nowhere/loam-connector.service", &target).unwrap();
        install(&runner, &context).unwrap();
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            definition_path(&context),
            "a stale symlink must be replaced"
        );
        // Idempotent: a second install leaves the correct symlink alone.
        install(&runner, &context).unwrap();
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            definition_path(&context)
        );
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn install_refuses_to_clobber_a_real_file_at_the_target() {
        let context = ctx("symlink-file");
        let runner = FakeRunner::new();
        let dir = context.systemd_user_dir.as_ref().unwrap();
        let target = systemd_symlink_target(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&target, "operator-owned").unwrap();
        assert!(
            install(&runner, &context).is_err(),
            "a real file at the target must be refused, not clobbered"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "operator-owned",
            "the operator's file must survive"
        );
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enable_start_succeeds_when_the_symlink_is_present() {
        let context = ctx("enable-ok");
        let runner = FakeRunner::new();
        install(&runner, &context).unwrap();
        enable_start(&runner, &context).unwrap();
        let joined: Vec<String> = runner
            .recorded
            .borrow()
            .iter()
            .map(|c| c.args.join(" "))
            .collect();
        assert!(
            joined.iter().any(|line| line.contains("daemon-reload")),
            "enable_start must daemon-reload before enable --now"
        );
        assert!(
            joined
                .iter()
                .any(|line| line.contains("enable") && line.contains("--now")),
            "enable_start must run enable --now"
        );
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(context.systemd_user_dir.as_ref().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enable_start_fails_clearly_when_the_symlink_is_missing() {
        let context = ctx("enable-missing");
        let runner = FakeRunner::new();
        // No install: the symlink was never placed.
        let error = enable_start(&runner, &context).unwrap_err();
        assert!(
            error.to_string().contains("not symlinked"),
            "the error must name the missing symlink; got: {error}"
        );
        assert!(
            runner.recorded.borrow().is_empty(),
            "no manager command may run when the symlink is missing"
        );
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(context.systemd_user_dir.as_ref().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uninstall_removes_the_symlink_but_not_a_foreign_one() {
        let context = ctx("uninstall-symlink");
        let runner = FakeRunner::new();
        install(&runner, &context).unwrap();
        let dir = context.systemd_user_dir.as_ref().unwrap();
        let target = systemd_symlink_target(dir);
        assert!(target.exists(), "install placed the symlink");
        uninstall(&runner, &context).unwrap();
        assert!(
            !target.exists(),
            "uninstall must remove the discoverability symlink"
        );

        // A symlink pointing elsewhere is the operator's; uninstall leaves it.
        let foreign = std::fs::symlink_metadata(&target).is_ok();
        if !foreign {
            std::fs::create_dir_all(dir).unwrap();
            std::os::unix::fs::symlink("/operator/unit.service", &target).unwrap();
            uninstall(&runner, &context).unwrap();
            assert_eq!(
                std::fs::read_link(&target).unwrap(),
                PathBuf::from("/operator/unit.service"),
                "a foreign symlink must survive uninstall"
            );
        }
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A runner that simulates systemd's `disable` behavior: it removes the
    /// discoverability symlink from the unit path, exactly as `systemctl
    /// disable` undoes everything `enable` created. This is the regression the
    /// cross-test caught — `install` placed the symlink, then the manager's
    /// `disable` removed it, and `enable_start`'s presence check failed.
    #[cfg(target_os = "linux")]
    struct DisableRemovesSymlinkRunner {
        dir: std::path::PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl CommandRunner for DisableRemovesSymlinkRunner {
        fn run(&self, command: &ManagerCommand) -> Result<ManagerOutput, ServiceError> {
            if command.args.iter().any(|arg| arg == "disable") {
                let target = systemd_symlink_target(&self.dir);
                let _ = std::fs::remove_file(target);
            }
            Ok(ManagerOutput::ok())
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn install_reasserts_the_symlink_after_the_manager_disable_removes_it() {
        let context = ctx("symlink-reassert");
        let dir = context.systemd_user_dir.as_ref().unwrap().clone();
        let runner = DisableRemovesSymlinkRunner { dir: dir.clone() };
        install(&runner, &context).unwrap();
        let target = systemd_symlink_target(&dir);
        assert!(
            std::fs::symlink_metadata(&target)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "install must re-assert the symlink after the manager's disable removed it"
        );
        assert_eq!(
            std::fs::read_link(&target).unwrap(),
            definition_path(&context),
            "the re-asserted symlink must point at the versioned unit"
        );
        // And enable_start must now succeed — the presence check passes.
        enable_start(&runner, &context).unwrap();
        let _ = std::fs::remove_dir_all(&context.global_root);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
