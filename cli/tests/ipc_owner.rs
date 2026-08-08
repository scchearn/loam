//! The owner-proof gate for the Windows named-pipe endpoint (T7).
//!
//! Every test here runs on a Windows target only; the hosted `windows-2022`
//! runner is where this gate actually executes, together with the alternate-user
//! PowerShell smoke that no in-process test can perform.

#[cfg(windows)]
mod windows_owner {
    use loam::ipc::windows::{bind, connect, is_local_pipe_name};
    use loam::ipc::{read_frame, write_frame, IpcConfig, IpcError};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// A unique, never-created global root. The endpoint name is a digest of the
    /// root and the owning SID, so tests never collide and never touch disk.
    fn run_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        PathBuf::from(format!(
            "C:\\loam-ipc-owner\\{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn windows_same_user_client_is_proven_before_the_codec_runs() {
        let endpoint = bind(&run_dir("same-user")).expect("first instance should bind");
        let name = endpoint.pipe_name().to_owned();
        assert!(is_local_pipe_name(&name));

        let client = std::thread::spawn(move || {
            let mut connection = connect(&name).expect("same-user client should open the pipe");
            let config = IpcConfig::default();
            write_frame(&mut connection, b"ping", &config).expect("client should frame a request");
            connection.flush().ok();
            read_frame(&mut connection, &config).expect("client should read the response")
        });

        let mut served = endpoint
            .accept_verified(Duration::from_secs(10))
            .expect("the same-user client's token SID must equal the connector's");
        let config = IpcConfig::default();
        let request = read_frame(&mut served, &config).expect("server should read one frame");
        assert_eq!(request, b"ping");
        write_frame(&mut served, b"pong", &config).expect("server should answer");
        served.flush().ok();

        assert_eq!(client.join().expect("client thread should finish"), b"pong");
    }

    #[test]
    fn windows_endpoint_is_a_first_instance_singleton() {
        let root = run_dir("singleton");
        let first = bind(&root).expect("first instance should bind");
        // FILE_FLAG_FIRST_PIPE_INSTANCE refuses the second creator outright, so
        // a squatter cannot add an instance to a live connector's name.
        assert_eq!(bind(&root).err(), Some(IpcError::Busy));
        drop(first);
        // Once the owner drops the instance the name is reusable.
        bind(&root).expect("the released name should rebind");
    }

    #[test]
    fn windows_remote_pipe_names_are_refused_before_any_handle_opens() {
        assert_eq!(
            connect("\\\\remote-host\\pipe\\loam-connector-abc").err(),
            Some(IpcError::UnauthorizedPeer)
        );
        assert_eq!(
            connect("loam-connector-abc").err(),
            Some(IpcError::UnauthorizedPeer)
        );
    }

    #[test]
    fn windows_accept_timeout_cancels_cleanly_and_leaves_the_endpoint_usable() {
        let endpoint = bind(&run_dir("accept-timeout")).expect("bind");
        // No client: the overlapped connect must time out, be cancelled, and be
        // drained to terminal completion rather than left pending.
        assert_eq!(
            endpoint.accept_verified(Duration::from_millis(50)).err(),
            Some(IpcError::Timeout)
        );

        let name = endpoint.pipe_name().to_owned();
        let client = std::thread::spawn(move || connect(&name).expect("client should connect"));
        // The proof that nothing was left pending: the same instance still accepts.
        let _served = endpoint
            .accept_verified(Duration::from_secs(10))
            .expect("the endpoint must still accept after a cancelled connect");
        drop(client.join().expect("client thread should finish"));
    }

    #[test]
    fn windows_read_timeout_is_bounded_and_the_connection_survives_it() {
        let endpoint = bind(&run_dir("read-timeout")).expect("bind");
        let name = endpoint.pipe_name().to_owned();
        let (ready, start) = std::sync::mpsc::channel::<()>();
        let client = std::thread::spawn(move || {
            let mut connection = connect(&name).expect("client should connect");
            // Stay silent until the server's read has already timed out.
            start
                .recv()
                .expect("server should signal after its timeout");
            write_frame(&mut connection, b"late", &IpcConfig::default())
                .expect("client should frame the late request");
            connection.flush().ok();
            // Hold the connection open until the server has read it.
            std::thread::sleep(Duration::from_millis(200));
        });

        let mut served = endpoint
            .accept_verified(Duration::from_secs(10))
            .expect("accept")
            .with_io_deadline(Duration::from_millis(100));
        // The overlapped read must give up at its own deadline, cancel, and
        // drain — not block on a silent client.
        let mut probe = [0u8; 4];
        let timed_out = std::io::Read::read(&mut served, &mut probe)
            .expect_err("a silent client must not block the read past its deadline");
        assert_eq!(timed_out.kind(), std::io::ErrorKind::TimedOut);
        ready
            .send(())
            .expect("client thread should still be waiting");
        // The cancelled read left no pending I/O: the same connection reads next.
        let config = IpcConfig::default();
        let mut served = served.with_io_deadline(Duration::from_secs(5));
        let late = read_frame(&mut served, &config).expect("the connection must survive a timeout");
        assert_eq!(late, b"late");
        client.join().expect("client thread should finish");
    }

    /// Support for `.github/scripts/windows-ipc-owner-smoke.ps1`: hold a live
    /// endpoint open and print its name so the script can attempt a same-user
    /// positive control and an alternate-user denial against a real pipe.
    /// Ignored by default — it is a fixture, not an assertion.
    #[test]
    #[ignore = "fixture for the alternate-user PowerShell smoke"]
    fn windows_endpoint_serves_the_alternate_user_smoke() {
        let seconds: u64 = std::env::var("LOAM_IPC_SMOKE_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30);
        let root = std::env::var("LOAM_IPC_SMOKE_ROOT").unwrap_or_else(|_| "C:\\loam-smoke".into());
        let endpoint = bind(&PathBuf::from(root)).expect("smoke endpoint should bind");
        println!("LOAM_PIPE_NAME={}", endpoint.pipe_name());
        let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
        // Serve every arriving client until the script is done; a denied client
        // never reaches this loop, because the DACL refuses it at open time.
        while std::time::Instant::now() < deadline {
            match endpoint.accept_verified(Duration::from_millis(500)) {
                Ok(mut served) => {
                    let config = IpcConfig::default();
                    if read_frame(&mut served, &config).is_ok() {
                        let _ = write_frame(&mut served, b"pong", &config);
                        let _ = served.flush();
                    }
                }
                Err(IpcError::Timeout) => {}
                Err(_) => break,
            }
        }
    }
}

#[cfg(not(windows))]
#[test]
fn windows_ipc_owner_proof_is_verified_on_the_hosted_windows_runner() {
    // The endpoint is `cfg(windows)`: its DACL, first-instance, client-SID, and
    // bounded-overlapped behaviour can only be observed on a Windows target, so
    // this gate reports honestly rather than vacuously here. The platform-free
    // half of the contract — the bounded frame codec both endpoints hand their
    // bytes to — is asserted on every platform.
    let config = loam::ipc::IpcConfig::default();
    assert_eq!(config.max_frame, 256 * 1024);
    assert_eq!(
        loam::ipc::read_frame(&mut [0u8, 0, 0, 0].as_slice(), &config).err(),
        Some(loam::ipc::IpcError::MalformedFrame)
    );
}
