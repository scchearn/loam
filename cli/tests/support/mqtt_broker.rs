use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const START_TIMEOUT: Duration = Duration::from_secs(15);

pub struct BrokerFixture {
    root: PathBuf,
    namespace: String,
    foreign_namespace: String,
    password: String,
    foreign_password: String,
    other_org_password: String,
    password_port: u16,
    mtls_port: u16,
    backend: Backend,
    child: Option<Child>,
    finished: bool,
}

enum Backend {
    Native {
        mosquitto: PathBuf,
        mosquitto_passwd: PathBuf,
    },
    Docker {
        docker: PathBuf,
        image: String,
        container: String,
        user: Option<String>,
    },
}

impl BrokerFixture {
    pub fn provision(label: &str) -> Result<Self, String> {
        if label.is_empty()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("broker fixture label must be ASCII alphanumeric or '-'".to_owned());
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
            .as_nanos();
        let run_id = format!("{}-{nonce:x}", std::process::id());
        let root = std::env::temp_dir().join(format!("loam-mqtt-{label}-{run_id}"));
        fs::create_dir(&root)
            .map_err(|error| format!("create broker fixture {}: {error}", root.display()))?;
        set_private_permissions(&root, true)?;
        fs::create_dir(root.join("persistence")).map_err(|error| {
            format!(
                "create broker persistence directory {}: {error}",
                root.display()
            )
        })?;
        set_private_permissions(&root.join("persistence"), true)?;

        let result = Self::build(root, run_id);
        if let Err(error) = &result {
            eprintln!("broker fixture setup failed; artifacts preserved: {error}");
        }
        result
    }

    fn build(root: PathBuf, run_id: String) -> Result<Self, String> {
        let openssl = required_program("LOAM_OPENSSL_BIN", "openssl")?;
        let backend = Backend::detect(&run_id)?;
        let password_port = reserve_port()?;
        let mtls_port = reserve_port()?;
        if password_port == mtls_port {
            return Err("broker listeners unexpectedly reserved the same port".to_owned());
        }

        write_certificates(&openssl, &root)?;
        let password = format!("loam-{run_id}");
        let foreign_password = format!("foreign-{run_id}");
        let other_org_password = format!("other-org-{run_id}");
        backend.write_password_entry(&root, "actor-a", &password, true)?;
        backend.write_password_entry(&root, "actor-b", &foreign_password, false)?;
        backend.write_password_entry(&root, "actor-c", &other_org_password, false)?;

        let namespace = format!("loam/v1/test-{run_id}");
        let foreign_namespace = format!("loam/v1/foreign-{run_id}");
        let acl =
            include_str!("../fixtures/mqtt/broker/acl.template").replace("@NAMESPACE@", &namespace);
        fs::write(root.join("acl"), acl).map_err(|error| format!("write broker ACL: {error}"))?;
        set_private_permissions(&root.join("acl"), false)?;

        let persistence = format!("{}/", root.join("persistence").display());
        let config = include_str!("../fixtures/mqtt/broker/mosquitto.conf.template")
            .replace("@PASSWORD_PORT@", &password_port.to_string())
            .replace("@MTLS_PORT@", &mtls_port.to_string())
            .replace("@PERSISTENCE@", &persistence)
            .replace("@CA_CERT@", &root.join("ca.crt").display().to_string())
            .replace(
                "@SERVER_CERT@",
                &root.join("server.crt").display().to_string(),
            )
            .replace(
                "@SERVER_KEY@",
                &root.join("server.key").display().to_string(),
            )
            .replace(
                "@PASSWORD_FILE@",
                &root.join("passwords").display().to_string(),
            )
            .replace("@ACL_FILE@", &root.join("acl").display().to_string());
        fs::write(root.join("mosquitto.conf"), config)
            .map_err(|error| format!("write broker configuration: {error}"))?;

        let mut fixture = Self {
            root,
            namespace,
            foreign_namespace,
            password,
            foreign_password,
            other_org_password,
            password_port,
            mtls_port,
            backend,
            child: None,
            finished: false,
        };
        fixture.start()?;
        Ok(fixture)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub fn foreign_namespace(&self) -> &str {
        &self.foreign_namespace
    }

    pub fn foreign_password(&self) -> &str {
        &self.foreign_password
    }

    pub fn other_org_password(&self) -> &str {
        &self.other_org_password
    }

    pub fn password_port(&self) -> u16 {
        self.password_port
    }

    pub fn mtls_port(&self) -> u16 {
        self.mtls_port
    }

    pub fn ca_certificate(&self) -> Result<Vec<u8>, String> {
        read_file(&self.root.join("ca.crt"))
    }

    pub fn client_certificate(&self) -> Result<Vec<u8>, String> {
        read_file(&self.root.join("client.crt"))
    }

    pub fn client_key(&self) -> Result<Vec<u8>, String> {
        read_file(&self.root.join("client.key"))
    }

    pub fn logs(&self) -> Result<String, String> {
        let mut log = String::new();
        File::open(self.root.join("mosquitto.log"))
            .and_then(|mut file| file.read_to_string(&mut log))
            .map_err(|error| format!("read broker log: {error}"))?;
        Ok(log)
    }

    pub fn wait_for_log(&self, needle: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let log = self.logs()?;
            if log.contains(needle) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "broker log did not contain {needle:?}; log follows:\n{log}"
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn restart(&mut self) -> Result<(), String> {
        self.stop()?;
        self.start()
    }

    pub fn enable_isolation(&mut self) -> Result<(), String> {
        self.stop()?;
        let acl = include_str!("../fixtures/mqtt/broker/acl-isolation.template")
            .replace("@ORG_A@", &self.namespace)
            .replace("@ORG_B@", &self.foreign_namespace);
        fs::write(self.root.join("acl"), acl)
            .map_err(|error| format!("write isolation broker ACL: {error}"))?;
        set_private_permissions(&self.root.join("acl"), false)?;
        self.start()
    }

    /// Reload the broker's password and ACL files in place via SIGHUP, without
    /// restarting the process or tearing down any live connection.
    pub fn reload(&mut self) -> Result<(), String> {
        let signalled = {
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| "broker process is not running".to_owned())?;
            self.backend.reload(child)
        };
        signalled?;
        // ponytail: matches the first "Reloading config" in the append-mode log;
        // fine while reload() runs at most once per test. If a test ever reloads
        // twice, capture the log length before signalling and search from there.
        self.wait_for_log("Reloading config")
    }

    /// Revoke a credential's authorization on an already-connected session
    /// without a broker restart: strip the user's ACL grants, delete its
    /// password entry, then SIGHUP-reload so the live session loses access in
    /// place while every other connection stays up.
    pub fn revoke_live(&mut self, username: &str) -> Result<(), String> {
        if username.is_empty()
            || username.starts_with('-')
            || !username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err("credential username is invalid".to_owned());
        }
        let acl_path = self.root.join("acl");
        let current = fs::read_to_string(&acl_path)
            .map_err(|error| format!("read broker ACL for revocation: {error}"))?;
        let filtered = strip_acl_user(&current, username);
        if filtered == current {
            return Err(format!(
                "ACL contained no `user {username}` block to revoke"
            ));
        }
        fs::write(&acl_path, filtered)
            .map_err(|error| format!("write revoked broker ACL: {error}"))?;
        set_private_permissions(&acl_path, false)?;
        self.backend.delete_password_entry(&self.root, username)?;
        self.reload()
    }

    pub fn finish(mut self) -> Result<(), String> {
        self.stop()?;
        validate_cleanup_root(&self.root)?;
        fs::remove_dir_all(&self.root).map_err(|error| {
            format!(
                "remove broker fixture directory {}: {error}",
                self.root.display()
            )
        })?;
        self.finished = true;
        Ok(())
    }

    fn start(&mut self) -> Result<(), String> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("mosquitto.log"))
            .map_err(|error| format!("open broker log: {error}"))?;
        let stderr = log
            .try_clone()
            .map_err(|error| format!("clone broker log handle: {error}"))?;
        self.child = Some(self.backend.spawn(&self.root, log, stderr)?);
        let child = self
            .child
            .as_mut()
            .expect("broker child is present immediately after spawn");
        wait_for_listener(self.password_port, child, &self.root)?;
        wait_for_listener(self.mtls_port, child, &self.root)?;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        self.backend.stop(&mut child)
    }
}

impl Drop for BrokerFixture {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Err(error) = self.stop() {
            eprintln!("broker fixture cleanup failed: {error}");
        }
        eprintln!(
            "broker fixture artifacts preserved for diagnosis: {}",
            self.root.display()
        );
    }
}

impl Backend {
    fn detect(run_id: &str) -> Result<Self, String> {
        let mosquitto = optional_program("LOAM_MOSQUITTO_BIN", "mosquitto");
        let mosquitto_passwd = optional_program("LOAM_MOSQUITTO_PASSWD_BIN", "mosquitto_passwd");
        if let (Some(mosquitto), Some(mosquitto_passwd)) = (mosquitto, mosquitto_passwd) {
            return Ok(Self::Native {
                mosquitto,
                mosquitto_passwd,
            });
        }

        let image = std::env::var("LOAM_MQTT_DOCKER_IMAGE")
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "mosquitto and mosquitto_passwd are required; CI must install them, or local runs may set LOAM_MQTT_DOCKER_IMAGE explicitly".to_owned()
            })?;
        let docker = required_program("LOAM_DOCKER_BIN", "docker")?;
        Ok(Self::Docker {
            docker,
            image,
            container: format!("loam-mqtt-{run_id}"),
            user: numeric_user(),
        })
    }

    fn write_password_entry(
        &self,
        root: &Path,
        username: &str,
        password: &str,
        create: bool,
    ) -> Result<(), String> {
        let password_file = root.join("passwords");
        match self {
            Self::Native {
                mosquitto_passwd, ..
            } => {
                let mut command = Command::new(mosquitto_passwd);
                command.arg("-b");
                if create {
                    command.arg("-c");
                }
                command.arg(&password_file).arg(username).arg(password);
                run_checked(&mut command, "write Mosquitto password entry")
            }
            Self::Docker {
                docker,
                image,
                user,
                ..
            } => {
                let mut command = Command::new(docker);
                command.arg("run").arg("--rm");
                if let Some(user) = user {
                    command.arg("--user").arg(user);
                }
                command
                    .arg("--volume")
                    .arg(format!("{}:{}", root.display(), root.display()))
                    .arg(image)
                    .arg("mosquitto_passwd")
                    .arg("-b");
                if create {
                    command.arg("-c");
                }
                command.arg(&password_file).arg(username).arg(password);
                run_checked(&mut command, "write Mosquitto password entry in Docker")
            }
        }
    }

    fn delete_password_entry(&self, root: &Path, username: &str) -> Result<(), String> {
        let password_file = root.join("passwords");
        match self {
            Self::Native {
                mosquitto_passwd, ..
            } => {
                let mut command = Command::new(mosquitto_passwd);
                command.arg("-D").arg(&password_file).arg(username);
                run_checked(&mut command, "delete Mosquitto password entry")
            }
            Self::Docker {
                docker,
                image,
                user,
                ..
            } => {
                let mut command = Command::new(docker);
                command.arg("run").arg("--rm");
                if let Some(user) = user {
                    command.arg("--user").arg(user);
                }
                command
                    .arg("--volume")
                    .arg(format!("{}:{}", root.display(), root.display()))
                    .arg(image)
                    .arg("mosquitto_passwd")
                    .arg("-D")
                    .arg(&password_file)
                    .arg(username);
                run_checked(&mut command, "delete Mosquitto password entry in Docker")
            }
        }
    }

    fn reload(&self, child: &mut Child) -> Result<(), String> {
        match self {
            Self::Native { .. } => signal_native(child, "HUP"),
            Self::Docker {
                docker, container, ..
            } => {
                let output = Command::new(docker)
                    .arg("kill")
                    .arg("--signal=HUP")
                    .arg(container)
                    .output()
                    .map_err(|error| format!("signal Mosquitto Docker container: {error}"))?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(format_output("signal Mosquitto Docker container", &output))
                }
            }
        }
    }

    fn spawn(&self, root: &Path, stdout: File, stderr: File) -> Result<Child, String> {
        let config = root.join("mosquitto.conf");
        let mut command = match self {
            Self::Native { mosquitto, .. } => {
                let mut command = Command::new(mosquitto);
                command.arg("-c").arg(&config).arg("-v");
                command
            }
            Self::Docker {
                docker,
                image,
                container,
                user,
            } => {
                let mut command = Command::new(docker);
                command
                    .arg("run")
                    .arg("--rm")
                    .arg("--network")
                    .arg("host")
                    .arg("--name")
                    .arg(container);
                if let Some(user) = user {
                    command.arg("--user").arg(user);
                }
                command
                    .arg("--volume")
                    .arg(format!("{}:{}", root.display(), root.display()))
                    .arg(image)
                    .arg("mosquitto")
                    .arg("-c")
                    .arg(&config)
                    .arg("-v");
                command
            }
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| format!("start Mosquitto broker: {error}"))
    }

    fn stop(&self, child: &mut Child) -> Result<(), String> {
        if child
            .try_wait()
            .map_err(|error| format!("inspect Mosquitto process: {error}"))?
            .is_some()
        {
            return Ok(());
        }

        match self {
            Self::Docker {
                docker, container, ..
            } => {
                let output = Command::new(docker)
                    .arg("stop")
                    .arg("--time")
                    .arg("5")
                    .arg(container)
                    .output()
                    .map_err(|error| format!("stop Mosquitto Docker container: {error}"))?;
                if !output.status.success() {
                    child
                        .kill()
                        .map_err(|error| format!("kill Mosquitto Docker process: {error}"))?;
                }
            }
            Self::Native { .. } => terminate_native(child)?,
        }
        child
            .wait()
            .map_err(|error| format!("wait for Mosquitto process: {error}"))?;
        Ok(())
    }
}

/// Remove the `user <username>` paragraph (its header and following topic
/// lines up to the next blank line) from a Mosquitto ACL file, leaving every
/// other user's grants intact. Returns the input unchanged if no such block
/// exists so the caller can detect a no-op revocation.
fn strip_acl_user(acl: &str, username: &str) -> String {
    let header = format!("user {username}");
    let mut out = String::new();
    let mut skipping = false;
    for line in acl.lines() {
        if !skipping && line.trim() == header {
            skipping = true;
            continue;
        }
        if skipping {
            // A blank line ends this user's block; drop it and resume copying.
            skipping = !line.trim().is_empty();
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_certificates(openssl: &Path, root: &Path) -> Result<(), String> {
    fs::write(
        root.join("server.ext"),
        "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n",
    )
    .map_err(|error| format!("write server certificate extensions: {error}"))?;
    fs::write(root.join("client.ext"), "extendedKeyUsage=clientAuth\n")
        .map_err(|error| format!("write client certificate extensions: {error}"))?;

    openssl_checked(
        openssl,
        root,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-nodes",
            "-days",
            "2",
            "-subj",
            "/CN=Loam MQTT Test CA",
            "-keyout",
            "ca.key",
            "-out",
            "ca.crt",
        ],
        "create test CA",
    )?;
    openssl_checked(
        openssl,
        root,
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
        ],
        "create broker certificate request",
    )?;
    openssl_checked(
        openssl,
        root,
        &[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "2",
            "-sha256",
            "-extfile",
            "server.ext",
            "-out",
            "server.crt",
        ],
        "sign broker certificate",
    )?;
    openssl_checked(
        openssl,
        root,
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-nodes",
            "-subj",
            "/CN=mtls-actor",
            "-keyout",
            "client.key",
            "-out",
            "client.csr",
        ],
        "create client certificate request",
    )?;
    openssl_checked(
        openssl,
        root,
        &[
            "x509",
            "-req",
            "-in",
            "client.csr",
            "-CA",
            "ca.crt",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "2",
            "-sha256",
            "-extfile",
            "client.ext",
            "-out",
            "client.crt",
        ],
        "sign client certificate",
    )?;
    for name in ["ca.key", "server.key", "client.key"] {
        set_private_permissions(&root.join(name), false)?;
    }
    Ok(())
}

fn openssl_checked(
    openssl: &Path,
    root: &Path,
    arguments: &[&str],
    description: &str,
) -> Result<(), String> {
    let mut command = Command::new(openssl);
    command.current_dir(root).args(arguments);
    run_checked(&mut command, description)
}

fn run_checked(command: &mut Command, description: &str) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("{description}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format_output(description, &output))
}

fn format_output(description: &str, output: &Output) -> String {
    format!(
        "{description} exited {}: stdout={:?}, stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn wait_for_listener(port: u16, child: &mut Child, root: &Path) -> Result<(), String> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("inspect starting broker: {error}"))?
        {
            let log = fs::read_to_string(root.join("mosquitto.log")).unwrap_or_default();
            return Err(format!("broker exited during startup ({status}):\n{log}"));
        }
        if Instant::now() >= deadline {
            let log = fs::read_to_string(root.join("mosquitto.log")).unwrap_or_default();
            return Err(format!(
                "broker did not listen on 127.0.0.1:{port} within {START_TIMEOUT:?}:\n{log}"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn reserve_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("reserve broker port: {error}"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("read reserved broker port: {error}"))
}

fn required_program(environment: &str, name: &str) -> Result<PathBuf, String> {
    optional_program(environment, name).ok_or_else(|| {
        format!("required executable {name:?} is unavailable; set {environment} to its exact path")
    })
}

fn optional_program(environment: &str, name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(environment).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn validate_cleanup_root(root: &Path) -> Result<(), String> {
    let temporary = std::env::temp_dir();
    let name = root.file_name().and_then(|name| name.to_str());
    if !root.starts_with(&temporary) || !name.is_some_and(|name| name.starts_with("loam-mqtt-")) {
        return Err(format!(
            "refusing to remove unsafe broker fixture path {}",
            root.display()
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if directory { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("set private permissions on {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn signal_native(child: &Child, signal: &str) -> Result<(), String> {
    let output = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.id().to_string())
        .output()
        .map_err(|error| format!("signal Mosquitto process: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format_output("signal Mosquitto process", &output))
    }
}

#[cfg(not(unix))]
fn signal_native(_child: &Child, _signal: &str) -> Result<(), String> {
    Err("SIGHUP reload of a native broker is only supported on Unix".to_owned())
}

#[cfg(unix)]
fn terminate_native(child: &mut Child) -> Result<(), String> {
    let output = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .output()
        .map_err(|error| format!("terminate Mosquitto process: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        child
            .kill()
            .map_err(|error| format!("kill Mosquitto process: {error}"))
    }
}

#[cfg(not(unix))]
fn terminate_native(child: &mut Child) -> Result<(), String> {
    child
        .kill()
        .map_err(|error| format!("kill Mosquitto process: {error}"))
}

#[cfg(unix)]
fn numeric_user() -> Option<String> {
    let uid = Command::new("id").arg("-u").output().ok()?;
    let gid = Command::new("id").arg("-g").output().ok()?;
    if !uid.status.success() || !gid.status.success() {
        return None;
    }
    Some(format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    ))
}

#[cfg(not(unix))]
fn numeric_user() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::strip_acl_user;

    #[test]
    fn strip_acl_user_removes_only_the_named_block() {
        let acl = "user actor-a\ntopic write a/#\ntopic read a/#\n\nuser mtls-actor\ntopic read a/#\n\nuser actor-b\ntopic read b/#\n";
        let stripped = strip_acl_user(acl, "actor-a");
        assert!(!stripped.contains("user actor-a"));
        assert!(!stripped.contains("write a/#"));
        assert!(stripped.contains("user mtls-actor"));
        assert!(stripped.contains("user actor-b"));
        assert!(stripped.contains("read b/#"));
        // A username that is a prefix of another must not match.
        assert_eq!(strip_acl_user(acl, "actor"), acl);
        // A missing user is a detectable no-op.
        assert_eq!(strip_acl_user(acl, "ghost"), acl);
        // The last block without a trailing blank line is still fully removed.
        let trailing = "user keep\ntopic read k/#\n\nuser drop\ntopic read d/#";
        let stripped_trailing = strip_acl_user(trailing, "drop");
        assert!(stripped_trailing.contains("user keep"));
        assert!(!stripped_trailing.contains("user drop"));
        assert!(!stripped_trailing.contains("read d/#"));
        // A username appearing only inside a topic line must not trigger a strip.
        let embedded = "user keep\ntopic read tenant/drop/#\n";
        assert_eq!(strip_acl_user(embedded, "drop"), embedded);
    }
}
