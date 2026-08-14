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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Io(String),
    InvalidRuntimePath,
    ManagerFailed {
        code: i32,
    },
    NotUtf8,
    /// A manager subprocess did not exit within its bound and was killed. Names
    /// the program so a wedged `launchctl`/`systemctl` surfaces instead of
    /// hanging the caller forever (macOS `launchctl kickstart` on a job with a
    /// non-zero last exit is the observed case).
    Timeout {
        program: String,
        seconds: u64,
    },
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::Io(why) => write!(f, "service io error: {why}"),
            ServiceError::InvalidRuntimePath => write!(f, "runtime path is not absolute/UTF-8"),
            ServiceError::ManagerFailed { code } => write!(f, "service manager exited {code}"),
            ServiceError::NotUtf8 => write!(f, "path is not representable as UTF-8"),
            ServiceError::Timeout { program, seconds } => {
                write!(
                    f,
                    "service manager {program} did not exit within {seconds}s and was killed"
                )
            }
        }
    }
}

/// The manager runner seam. `RealRunner` shells out; tests inject a fake.
pub trait CommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError>;
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
    fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError> {
        run_bounded(command, MANAGER_TIMEOUT)
    }
}

/// Spawn a manager command and wait for it with a wall-clock bound. On expiry
/// the child is killed and reaped (so no zombie accumulates, the observed
/// launchctl leak) and a typed `Timeout` is returned. stdio is sent to null:
/// only the exit code is consulted, and draining pipes across the poll loop is
/// unnecessary. `std` has no `wait_timeout`, so this is a `try_wait` poll loop —
/// no new dependency.
fn run_bounded(
    command: &ManagerCommand,
    timeout: std::time::Duration,
) -> Result<i32, ServiceError> {
    use std::process::Stdio;
    let mut child = std::process::Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| ServiceError::Io(error.to_string()))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code().unwrap_or(-1)),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    // Reap so the killed child does not linger as a zombie.
                    let _ = child.wait();
                    return Err(ServiceError::Timeout {
                        program: command.program.clone(),
                        seconds: timeout.as_secs(),
                    });
                }
                std::thread::sleep(MANAGER_POLL);
            }
            Err(error) => return Err(ServiceError::Io(error.to_string())),
        }
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

/// The disabled systemd `--user` unit. `Restart=on-failure`, no lingering, not
/// `WantedBy` any target so it stays dormant until explicitly enabled.
pub fn render_systemd_unit(ctx: &ServiceContext) -> Result<String, ServiceError> {
    let runtime = absolute_utf8(&ctx.runtime_path)?;
    Ok(format!(
        "[Unit]\n\
         Description=Loam federation connector\n\
         After=network.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={runtime} federation service run --global-root {root}\n\
         Restart=on-failure\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        root = absolute_utf8(&ctx.global_root)?,
    ))
}

/// The disabled macOS LaunchAgent plist. No `RunAtLoad`, no `KeepAlive` until
/// enabled — install writes it and `launchctl` bootstraps it only after first
/// enrollment. An `EnvironmentVariables` dict carries the `LOAM_*` overrides
/// the connector needs (launchd does not expand `$HOME`, so values are absolute
/// or literal).
pub fn render_launchagent_plist(ctx: &ServiceContext) -> Result<String, ServiceError> {
    let runtime = absolute_utf8(&ctx.runtime_path)?;
    let root = absolute_utf8(&ctx.global_root)?;
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
         \t<key>KeepAlive</key><false/>\n\
         {environment}\
         </dict>\n</plist>\n"
    ))
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
    runner.run(&status_command(ctx))
}

/// Enable and start the connector after the first enrollment (T10 activation).
/// Idempotent — safe to call when already active. On Linux the systemd
/// discoverability symlink must be in place first: `enable --now` on a unit
/// systemd cannot see silently no-ops, so a missing symlink is refused with a
/// clear error instead.
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
    run_all(runner, &enable_start_commands(ctx))
}

/// Disable and stop the connector after the final disconnect (T11). Idempotent.
pub fn disable_stop<R: CommandRunner>(
    runner: &R,
    ctx: &ServiceContext,
) -> Result<(), ServiceError> {
    run_all(runner, &disable_stop_commands(ctx))
}

#[cfg(target_os = "linux")]
fn enable_start_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![
        // Reload so the freshly-symlinked unit is visible to the manager.
        ManagerCommand::new("systemctl", &["--user", "daemon-reload"]),
        ManagerCommand::new(
            "systemctl",
            &["--user", "enable", "--now", "loam-connector.service"],
        ),
    ]
}

#[cfg(target_os = "linux")]
fn disable_stop_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![ManagerCommand::new(
        "systemctl",
        &["--user", "disable", "--now", "loam-connector.service"],
    )]
}

#[cfg(target_os = "macos")]
fn enable_start_commands(ctx: &ServiceContext) -> Vec<ManagerCommand> {
    let plist = definition_path(ctx).to_string_lossy().into_owned();
    vec![
        ManagerCommand::new("launchctl", &["bootstrap", &launchd::gui_domain(), &plist]),
        ManagerCommand::new("launchctl", &["enable", &launchd::gui_service()]),
        // The plist is dormant (`RunAtLoad`/`KeepAlive` false), so bootstrapping
        // it only *loads* the job. Activation after the first enrollment has to
        // start it explicitly — systemd gets that from `enable --now` and
        // schtasks from `/Run`.
        ManagerCommand::new("launchctl", &["kickstart", "-k", &launchd::gui_service()]),
    ]
}

#[cfg(target_os = "macos")]
fn disable_stop_commands(_ctx: &ServiceContext) -> Vec<ManagerCommand> {
    vec![ManagerCommand::new(
        "launchctl",
        &["bootout", &launchd::gui_service()],
    )]
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn enable_start_commands(ctx: &ServiceContext) -> Vec<ManagerCommand> {
    let name = task_name(&ctx.instance_id);
    vec![
        ManagerCommand::new("schtasks", &["/Change", "/TN", &name, "/ENABLE"]),
        ManagerCommand::new("schtasks", &["/Run", "/TN", &name]),
    ]
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
    // surfaced as a typed Timeout naming the program — never hang the caller
    // (the macOS `launchctl kickstart` wedge). `sleep 30` is the never-exits
    // stub; the bound is short so the test is fast, and the elapsed assertion
    // proves the kill actually fired rather than the sleep completing.
    #[cfg(unix)]
    #[test]
    fn a_wedged_manager_subprocess_is_killed_at_the_bound_and_typed() {
        let wedged = ManagerCommand::new("sleep", &["30"]);
        let bound = std::time::Duration::from_millis(200);
        let started = std::time::Instant::now();
        let result = run_bounded(&wedged, bound);
        let elapsed = started.elapsed();

        assert_eq!(
            result,
            Err(ServiceError::Timeout {
                program: "sleep".to_owned(),
                seconds: 0,
            }),
            "a subprocess past its bound must surface a typed Timeout naming the program"
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
            result,
            Ok(3),
            "a command that exits within the bound must surface its real code"
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
        fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError> {
            self.recorded.borrow_mut().push(command.clone());
            Ok(0)
        }
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
        for command in enable_start_commands(&ctx("launchd-domain")) {
            assert!(command.args.iter().all(|a| !a.contains('$')));
        }
    }

    #[test]
    fn systemd_unit_is_dormant_and_references_the_absolute_runtime() {
        let context = ctx("systemd");
        let runtime = runtime_string(&context);
        let unit = render_systemd_unit(&context).unwrap();
        assert!(unit.contains(&format!(
            "ExecStart={runtime} federation service run --global-root"
        )));
        assert!(unit.contains("Restart=on-failure"));
        // No socket activation, no auto-start beyond an explicit enable.
        assert!(!unit.contains("RunAtLoad"));
    }

    #[test]
    fn launchagent_plist_is_dormant() {
        let context = ctx("launchd");
        let runtime = runtime_string(&context);
        let plist = render_launchagent_plist(&context).unwrap();
        assert!(plist.contains("<key>RunAtLoad</key><false/>"));
        assert!(plist.contains("<key>KeepAlive</key><false/>"));
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
        fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError> {
            if command.args.iter().any(|arg| arg == "disable") {
                let target = systemd_symlink_target(&self.dir);
                let _ = std::fs::remove_file(target);
            }
            Ok(0)
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
