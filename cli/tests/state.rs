use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn temporary_workspace() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("loam-state-{nonce}-{serial}"));
    fs::create_dir(&path).expect("temporary workspace should be created");
    path
}

/// A `loam state` invocation with every hcom detection site neutralised: no
/// launcher env marker, no PATH, and a HOME that holds no `.local/bin`. Without
/// this the probe would answer from the *developer's* machine and the pinned
/// aggregates would flip depending on who ran the suite.
fn state_command(workspace: &std::path::Path) -> Command {
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let mut command = Command::new(binary);
    command
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .env_remove("HCOM_TOOL")
        .env_remove("HCOM_INSTALL_DIR")
        .env("PATH", "")
        .env("HOME", workspace)
        .env("USERPROFILE", workspace);
    command
}

fn plan_with(criteria: &str) -> String {
    format!(
        "---\nstatus: in-progress\n---\n\n## Acceptance criteria\n\n{criteria}\n\n## Tasks\n\n### T1\n\n- **Status:** [x]\n"
    )
}

fn state_hints(plan: &str) -> String {
    let workspace = temporary_workspace();
    // Without a resolvable wiki root `aggregate` short-circuits to minimal
    // state and no workflow hint is reached at all.
    fs::create_dir(workspace.join("wiki")).expect("wiki directory should be created");
    fs::write(workspace.join("wiki/index.md"), "# Index\n").expect("index should be written");
    fs::create_dir(workspace.join("plans")).expect("plans directory should be created");
    fs::write(workspace.join("plans/demo.md"), plan).expect("plan should be written");
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    String::from_utf8(output.stdout).expect("state output should be UTF-8")
}

#[test]
fn plan_with_complete_tasks_and_open_criteria_is_reconcilable() {
    // [>] and the unrecognized [~] count as open; [-] counts as resolved.
    let stdout = state_hints(&plan_with(
        "- [x] met. Evidence: tests.\n- [-] superseded.\n- [ ] pending.\n- [>] needs re-check.\n- [~] unrecognized.",
    ));
    assert!(
        stdout.contains(r#""kind":"plan_reconcilable""#),
        "expected reconcilable hint, got {stdout}"
    );
    assert!(
        stdout.contains(r#""kind":"plan_reconcilable","group":"workflow","severity":"info","message":"Every task is complete but acceptance criteria are unresolved.","command":"/loam::amending-plan","evidence":{"plans":["plans/demo.md"]}"#),
        "unexpected hint shape in {stdout}"
    );
}

#[test]
fn many_reconcilable_plans_collapse_into_one_hint() {
    let workspace = temporary_workspace();
    fs::create_dir(workspace.join("wiki")).expect("wiki directory should be created");
    fs::write(workspace.join("wiki/index.md"), "# Index\n").expect("index should be written");
    fs::create_dir(workspace.join("plans")).expect("plans directory should be created");
    for name in ["alpha.md", "beta.md", "gamma.md"] {
        fs::write(
            workspace.join("plans").join(name),
            plan_with("- [ ] pending."),
        )
        .expect("plan should be written");
    }
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");
    let output = Command::new(binary)
        .args(["state", "--fast", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");

    assert_eq!(
        stdout.matches("plan_reconcilable").count(),
        1,
        "three drifting plans must produce one hint, got {stdout}"
    );
    assert!(
        stdout.contains(r#""plans":["plans/alpha.md","plans/beta.md","plans/gamma.md"]"#),
        "every affected plan must be listed, got {stdout}"
    );
}

#[test]
fn plan_with_all_criteria_resolved_is_not_reconcilable() {
    let stdout = state_hints(&plan_with("- [x] met. Evidence: tests.\n- [-] superseded."));
    assert!(
        !stdout.contains("plan_reconcilable"),
        "resolved plan should emit no hint, got {stdout}"
    );
}

#[test]
fn plan_with_pending_tasks_is_not_reconcilable() {
    let plan = plan_with("- [ ] pending.").replace("- **Status:** [x]", "- **Status:** [ ]");
    let stdout = state_hints(&plan);
    assert!(
        !stdout.contains("plan_reconcilable"),
        "unfinished plan should emit no hint, got {stdout}"
    );
}

#[test]
fn state_fast_without_wiki_returns_minimal_fallback() {
    let workspace = temporary_workspace();
    let output = state_command(&workspace).output().expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        output.status.success(),
        "loam failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The runtime self-reports its compiled version as the first key (T1); derive
    // it from the crate version rather than hard-coding, so a bump stays green.
    let body = r#"{"wiki_root":"","exists":false,"qmd_ready":false,"hcom_ready":false,"latest_checkpoint":null,"recent_checkpoints":[],"checkpoint_count":0,"git_status":null,"drift_count":null,"hints":[{"kind":"memory_missing","group":"maintenance","severity":"info","message":"No memory substrate found; scaffold a wiki to begin.","command":"/loam::scaffolding-wiki <goal>","evidence":{}}]}"#;
    let expected = format!(
        "{{\"version\":\"{}\",{}",
        env!("CARGO_PKG_VERSION"),
        &body[1..]
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("state output should be UTF-8")
            .trim(),
        expected
    );
}

#[test]
fn full_state_pending_hint_uses_stable_content_identity() {
    let workspace = temporary_workspace();
    fs::create_dir_all(workspace.join("src")).expect("src should be created");
    fs::create_dir_all(workspace.join("wiki/code")).expect("code graph should be created");
    fs::write(workspace.join("wiki/index.md"), "# Index\n").expect("index should be written");
    fs::write(workspace.join("src/known.rs"), "fn known() {}\n").expect("source should be written");
    fs::write(
        workspace.join("wiki/code/known.md"),
        "---\nsource_path: src/known.rs\ningested_at: \"1\"\ncontent_id: sha256:ff93b8b31f63b372f27a4c10588f9fa4c5735a16b7d7ec3d059cb5066b15c344\nsource_state: fallback\n---\n",
    )
    .expect("code page should be written");
    let binary = std::env::var("CARGO_BIN_EXE_loam").expect("cargo should provide the loam binary");

    let current = Command::new(&binary)
        .args(["state", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::write(workspace.join("src/known.rs"), "fn other() {}\n").expect("source should change");
    let changed = Command::new(binary)
        .args(["state", workspace.to_str().unwrap()])
        .output()
        .expect("loam should run");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    let current = String::from_utf8(current.stdout).expect("state output should be UTF-8");
    let changed = String::from_utf8(changed.stdout).expect("state output should be UTF-8");
    assert!(!current.contains("code_ingest_pending"), "{current}");
    assert!(changed.contains("code_ingest_pending"), "{changed}");
    assert!(changed.contains(r#""pending_count":1"#), "{changed}");
}

// ---- optional hcom integration (detection-only) ---------------------------

/// A stand-in `hcom` that records every invocation. Detection must not run it
/// when a cheaper rung already answered, and the recording is what proves it.
#[cfg(unix)]
fn fake_hcom(directory: &std::path::Path, marker: &std::path::Path, exit_code: u8) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(directory).expect("bin directory should be created");
    let path = directory.join("hcom");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\necho ran >> {}\necho 'hcom 0.7.25'\nexit {exit_code}\n",
            marker.display()
        ),
    )
    .expect("fake hcom should be written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("fake hcom should be executable");
}

#[test]
#[cfg(unix)]
fn an_hcom_launched_session_reports_ready_without_spawning_anything() {
    // The session-start budget guarantee: HCOM_TOOL answers the probe once the
    // binary has been confirmed by stat, so the health check never runs. The fake
    // on PATH exits non-zero — if the probe reached it, readiness would flip to
    // false and the marker would exist.
    let workspace = temporary_workspace();
    let bin = workspace.join("bin");
    let marker = workspace.join("spawned");
    fake_hcom(&bin, &marker, 1);
    let output = state_command(&workspace)
        .env("PATH", &bin)
        .env("HCOM_TOOL", "claude")
        .output()
        .expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    let spawned = marker.exists();
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(stdout.contains(r#""hcom_ready":true"#), "{stdout}");
    assert!(
        !spawned,
        "the launcher env marker must short-circuit before any spawn"
    );
}

#[test]
#[cfg(unix)]
fn hcom_installed_on_path_is_health_checked_and_ready() {
    let workspace = temporary_workspace();
    let bin = workspace.join("bin");
    let marker = workspace.join("spawned");
    fake_hcom(&bin, &marker, 0);
    let output = state_command(&workspace)
        .env("PATH", &bin)
        .output()
        .expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    let spawned = marker.exists();
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(stdout.contains(r#""hcom_ready":true"#), "{stdout}");
    assert!(
        spawned,
        "a resolved binary must be health-checked, not assumed working"
    );
}

#[test]
#[cfg(unix)]
fn hcom_in_the_installer_default_directory_is_found_off_path() {
    // Both official installers, uv and pip all land here; PATH may not carry it
    // in a non-login shell, so the ladder checks the site directly.
    let workspace = temporary_workspace();
    let marker = workspace.join("spawned");
    fake_hcom(&workspace.join(".local/bin"), &marker, 0);
    let output = state_command(&workspace).output().expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(stdout.contains(r#""hcom_ready":true"#), "{stdout}");
}

#[test]
#[cfg(unix)]
fn hcom_under_the_install_dir_override_is_found() {
    let workspace = temporary_workspace();
    let root = workspace.join("opt/hcom");
    let marker = workspace.join("spawned");
    fake_hcom(&root.join("bin"), &marker, 0);
    let output = state_command(&workspace)
        .env("HCOM_INSTALL_DIR", &root)
        .output()
        .expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(stdout.contains(r#""hcom_ready":true"#), "{stdout}");
}

#[test]
#[cfg(unix)]
fn a_broken_hcom_binary_is_not_ready() {
    // Present but unhealthy is not ready: the briefing must not promise a tool
    // the skills would then fail to use.
    let workspace = temporary_workspace();
    let bin = workspace.join("bin");
    let marker = workspace.join("spawned");
    fake_hcom(&bin, &marker, 3);
    let output = state_command(&workspace)
        .env("PATH", &bin)
        .output()
        .expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(stdout.contains(r#""hcom_ready":false"#), "{stdout}");
}

#[test]
fn hcom_absent_from_every_install_site_is_not_installed() {
    let workspace = temporary_workspace();
    fs::create_dir(workspace.join("wiki")).expect("wiki directory should be created");
    fs::write(workspace.join("wiki/index.md"), "# Index\n").expect("index should be written");
    let output = state_command(&workspace).output().expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    // Present in the full aggregate too, not only the wiki-less fallback.
    assert!(stdout.contains(r#""hcom_ready":false"#), "{stdout}");
}

#[test]
fn an_hcom_marker_without_an_installed_hcom_is_not_ready() {
    // HCOM_TOOL is an identity marker, not a liveness one: it survives in the
    // environment after hcom is removed, and a user can export it from a shell
    // rc. Believing it alone would put `hcom: ready` in front of every skill on
    // a machine with no hcom, and they would all discover the truth at the first
    // send — exactly what the injected line exists to prevent.
    let workspace = temporary_workspace();
    let output = state_command(&workspace)
        .env("HCOM_TOOL", "claude")
        .output()
        .expect("loam should run");
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(stdout.contains(r#""hcom_ready":false"#), "{stdout}");
}

#[test]
#[cfg(unix)]
fn a_wedged_hcom_binary_cannot_hold_session_start_hostage() {
    // The health check is the one spawn on the session-start path with no gate in
    // front of it, so its cost has to be bounded by us rather than by the binary:
    // a PATH entry on a stalled mount, or a shim waiting on a lock, would
    // otherwise block the hook for as long as it likes. Unbound, this run takes
    // as long as the fake sleeps — well past the five-second hook budget.
    use std::os::unix::fs::PermissionsExt;
    let workspace = temporary_workspace();
    let bin = workspace.join("bin");
    fs::create_dir_all(&bin).expect("bin directory should be created");
    let path = bin.join("hcom");
    // PATH is scrubbed to the fixture directory, so `sleep` has to be found the
    // long way round — otherwise the fake exits instantly and proves nothing.
    fs::write(
        &path,
        "#!/bin/sh
PATH=/usr/bin:/bin
exec sleep 30
echo 'hcom 0.7.25'
",
    )
    .expect("wedged hcom should be written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("wedged hcom should be executable");

    let started = std::time::Instant::now();
    let output = state_command(&workspace)
        .env("PATH", &bin)
        .output()
        .expect("loam should run");
    let elapsed = started.elapsed();
    let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
    fs::remove_dir_all(&workspace).expect("temporary workspace should be removed");

    assert!(
        elapsed.as_secs() < 5,
        "state --fast waited {elapsed:?} on a wedged hcom, past the hook budget"
    );
    // Expiry is not-ready, not "assume it works": a tool that cannot answer in a
    // second is not one the briefing should promise.
    assert!(stdout.contains(r#""hcom_ready":false"#), "{stdout}");
}

#[test]
#[cfg(unix)]
fn a_binary_sitting_in_the_workspace_is_never_resolved_or_run() {
    // The hook runs with CWD set to the workspace, so any rung that produces a
    // RELATIVE candidate turns "detect hcom" into "execute a file out of the
    // repository someone just cloned". Two env shapes produce one: an empty
    // element in PATH (`PATH=/usr/bin:`, a common shell-rc accident) resolves the
    // bare name, and an empty HOME resolves `.local/bin/<name>`. Both are stat'd
    // against the CWD unless the ladder insists on absolute directories.
    // One workspace per shape: the bait has to be reachable ONLY through the
    // relative candidate, or an absolute rung would answer and prove nothing.
    let empty_home = temporary_workspace();

    // `PATH=/usr/bin:` — the empty element resolves the bare name against CWD.
    let bare = temporary_workspace();
    let bare_marker = bare.join("spawned");
    fake_hcom(&bare, &bare_marker, 0);

    // `HOME=""` — `.local/bin/<name>` is relative, so it lands under CWD too.
    let under_home = temporary_workspace();
    let home_marker = under_home.join("spawned");
    fake_hcom(&under_home.join(".local/bin"), &home_marker, 0);

    for (label, workspace, marker, path, home) in [
        (
            "an empty PATH element",
            &bare,
            &bare_marker,
            "/usr/bin:",
            empty_home.to_str().unwrap(),
        ),
        ("an empty HOME", &under_home, &home_marker, "/usr/bin", ""),
    ] {
        let output = state_command(workspace)
            .current_dir(workspace)
            .env("PATH", path)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .output()
            .expect("loam should run");
        let stdout = String::from_utf8(output.stdout).expect("state output should be UTF-8");
        assert!(
            stdout.contains(r#""hcom_ready":false"#),
            "{label} resolved a workspace file as hcom: {stdout}"
        );
        assert!(
            !marker.exists(),
            "{label} ran a workspace file — the ladder must never spawn a relative candidate"
        );
    }
    for workspace in [&empty_home, &bare, &under_home] {
        fs::remove_dir_all(workspace).expect("temporary workspace should be removed");
    }
}
