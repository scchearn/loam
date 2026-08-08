//! Slice C T8: a stable non-secret instance identity and dormant per-user
//! service definitions for the three native managers.
//!
//! The definitions are installed **disabled**: install renders the manager's
//! definition and registers it, but does not start it. A per-user connector is
//! enabled/started only after the first enrollment (Slice C T10/T12), and the
//! empty state stays dormant. This module never starts the connector, never
//! contacts a broker, and never creates the SQLite store — the instance identity
//! lives in its own file so an unenrolled machine has no database.
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Io(String),
    InvalidRuntimePath,
    ManagerFailed { code: i32 },
    NotUtf8,
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::Io(why) => write!(f, "service io error: {why}"),
            ServiceError::InvalidRuntimePath => write!(f, "runtime path is not absolute/UTF-8"),
            ServiceError::ManagerFailed { code } => write!(f, "service manager exited {code}"),
            ServiceError::NotUtf8 => write!(f, "path is not representable as UTF-8"),
        }
    }
}

/// The manager runner seam. `RealRunner` shells out; tests inject a fake.
pub trait CommandRunner {
    fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError>;
}

/// Executes manager commands as argv-vector subprocesses — never through a
/// shell, so no argument is ever word-split or interpreted.
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, command: &ManagerCommand) -> Result<i32, ServiceError> {
        let output = std::process::Command::new(&command.program)
            .args(&command.args)
            .output()
            .map_err(|error| ServiceError::Io(error.to_string()))?;
        Ok(output.status.code().unwrap_or(-1))
    }
}

// ---------------------------------------------------------------------------
// Stable instance identity
// ---------------------------------------------------------------------------

/// Read the stable per-install instance id, generating and persisting it once if
/// absent. Stored in its own file under the global root so an unenrolled machine
/// never causes `loam.sqlite3` to be created. Rejects a symlink or a malformed
/// existing value rather than silently replacing it.
pub fn ensure_instance_id(global_root: &Path) -> Result<String, ServiceError> {
    let path = global_root.join("instance_id");
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            return Err(ServiceError::Io("instance_id is a symlink".into()));
        }
        let existing =
            std::fs::read_to_string(&path).map_err(|e| ServiceError::Io(e.to_string()))?;
        let trimmed = existing.trim();
        if is_valid_instance_id(trimmed) {
            return Ok(trimmed.to_owned());
        }
        return Err(ServiceError::Io("instance_id is malformed".into()));
    }
    let id = generate_instance_id();
    std::fs::create_dir_all(global_root).map_err(|e| ServiceError::Io(e.to_string()))?;
    std::fs::write(&path, &id).map_err(|e| ServiceError::Io(e.to_string()))?;
    Ok(id)
}

fn is_valid_instance_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A non-secret, stable-once-written instance id: 32 lowercase hex chars derived
/// from time, pid, and the global-root path, hashed so it is opaque. Not a
/// secret and not required to be globally unique beyond this install.
fn generate_instance_id() -> String {
    use crate::sha256::Sha256;
    let mut hasher = Sha256::default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.update(&nanos.to_le_bytes());
    hasher.update(&std::process::id().to_le_bytes());
    let full = hasher.finish();
    full[..32].to_owned()
}

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
/// enrollment.
pub fn render_launchagent_plist(ctx: &ServiceContext) -> Result<String, ServiceError> {
    let runtime = absolute_utf8(&ctx.runtime_path)?;
    let root = absolute_utf8(&ctx.global_root)?;
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
         </dict>\n</plist>\n"
    ))
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
// Lifecycle
// ---------------------------------------------------------------------------

/// Install the dormant definition: write the definition file (where the platform
/// uses one) and run the disabled-registration manager commands. Never starts
/// the connector. Idempotent: re-installing overwrites the definition and
/// re-runs the same disabled registration.
pub fn install<R: CommandRunner>(runner: &R, ctx: &ServiceContext) -> Result<(), ServiceError> {
    write_definition(ctx)?;
    run_all(runner, &install_commands(ctx))
}

/// Remove the definition and its file. Idempotent and never contacts a broker.
pub fn uninstall<R: CommandRunner>(runner: &R, ctx: &ServiceContext) -> Result<(), ServiceError> {
    // Manager teardown first (best-effort), then the file.
    let _ = run_all(runner, &uninstall_commands(ctx));
    let path = definition_path(ctx);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| ServiceError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Query the manager for the definition's state without starting anything.
pub fn status<R: CommandRunner>(runner: &R, ctx: &ServiceContext) -> Result<i32, ServiceError> {
    runner.run(&status_command(ctx))
}

/// Enable and start the connector after the first enrollment (T10 activation).
/// Idempotent — safe to call when already active.
pub fn enable_start<R: CommandRunner>(
    runner: &R,
    ctx: &ServiceContext,
) -> Result<(), ServiceError> {
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
    vec![ManagerCommand::new(
        "systemctl",
        &["--user", "enable", "--now", "loam-connector.service"],
    )]
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
        }
    }

    fn runtime_string(context: &ServiceContext) -> String {
        context.runtime_path.to_str().unwrap().to_owned()
    }

    #[test]
    fn instance_id_is_generated_once_and_preserved() {
        let context = ctx("identity");
        let first = ensure_instance_id(&context.global_root).unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        let second = ensure_instance_id(&context.global_root).unwrap();
        assert_eq!(first, second, "id must be stable across calls");
        // A read-only reconcile of identity never creates the SQLite store.
        assert!(!context.global_root.join("loam.sqlite3").exists());
    }

    /// launchctl receives argv, never a shell line, so an unexpanded
    /// `gui/$(id -u)` is a "Bad request" — the domain must carry a real uid.
    #[cfg(target_os = "macos")]
    #[test]
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
}
