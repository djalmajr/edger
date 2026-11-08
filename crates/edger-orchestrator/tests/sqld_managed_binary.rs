#![cfg(target_os = "linux")]

use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// Hold once per parent test, before listeners and until all child cleanup is done.
// Do not lock in Fixture::new: one test can own multiple fixtures at the same time.
static TCP_FIXTURE_TESTS: Mutex<()> = Mutex::new(());

fn tcp_fixture_test_guard() -> MutexGuard<'static, ()> {
    TCP_FIXTURE_TESTS.lock().unwrap_or_else(|poison| {
        let guard = poison.into_inner();
        TCP_FIXTURE_TESTS.clear_poison();
        guard
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    start_ticks: String,
    exe: PathBuf,
    cwd: PathBuf,
    command: Vec<u8>,
}

fn proc_stat(pid: u32) -> io::Result<(String, u32, String)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let tail = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("invalid proc stat"))?
        .1;
    let fields: Vec<_> = tail.split_whitespace().collect();
    let state = fields
        .first()
        .ok_or_else(|| io::Error::other("missing proc state"))?
        .to_string();
    let parent = fields
        .get(1)
        .ok_or_else(|| io::Error::other("missing proc parent"))?
        .parse()
        .map_err(|_| io::Error::other("invalid proc parent"))?;
    let start = fields
        .get(19)
        .ok_or_else(|| io::Error::other("missing proc start"))?
        .to_string();
    Ok((state, parent, start))
}

fn identity(pid: u32) -> io::Result<Identity> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let (_, _, start_ticks) = proc_stat(pid)?;
    Ok(Identity {
        start_ticks,
        exe: fs::read_link(root.join("exe"))?,
        cwd: fs::read_link(root.join("cwd"))?,
        command: fs::read(root.join("cmdline"))?,
    })
}

struct OwnedChild {
    child: Child,
    pid: u32,
    expected_exe: PathBuf,
    expected_command: Vec<u8>,
    identity: Option<Identity>,
}

impl OwnedChild {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let expected_exe = fs::canonicalize(command.get_program())?;
        let mut expected_command = Vec::new();
        for arg in std::iter::once(command.get_program()).chain(command.get_args()) {
            expected_command.extend_from_slice(arg.as_bytes());
            expected_command.push(0);
        }
        let child = command.spawn()?;
        let pid = child.id();
        let mut owned = Self {
            child,
            pid,
            expected_exe,
            expected_command,
            identity: None,
        };
        // Between fork and exec (and while a very short-lived child exits) the
        // kernel exposes the parent image or an empty cmdline. The child is our
        // own unreaped PID, so it cannot be reused; retry the identity read for
        // a bounded window until it matches or the exit becomes observable.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let last = match identity(pid) {
                Ok(found)
                    if found.exe == owned.expected_exe
                        && found.command == owned.expected_command =>
                {
                    owned.identity = Some(found);
                    return Ok(owned);
                }
                Ok(found) => format!("{found:?}"),
                Err(error) => error.to_string(),
            };
            match owned.child.try_wait() {
                Ok(Some(_)) => return Ok(owned),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Ok(None) => {
                    return Err(io::Error::other(format!(
                        "cannot identify running child {pid}: {last}"
                    )))
                }
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "cannot identify child {pid}: {last}; try_wait: {error}"
                    )))
                }
            }
        }
    }

    fn poll(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Stderr of a child that already exited, for assertion messages.
    fn exited_stderr(&mut self) -> String {
        let mut text = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    }

    fn stop(&mut self) -> io::Result<()> {
        self.stop_with(false)
    }

    /// `allow_term` accepts a child that dies of the SIGTERM sent here: EdgeR
    /// installs its handler only after boot, so a stop during boot ends it by
    /// the default disposition.
    fn stop_with(&mut self, allow_term: bool) -> io::Result<()> {
        if self.poll()?.is_some() {
            return Ok(());
        }
        let expected = self
            .identity
            .clone()
            .ok_or_else(|| io::Error::other(format!("no identity for live child {}", self.pid)))?;
        let actual = identity(self.pid)?;
        if actual != expected
            || actual.exe != self.expected_exe
            || actual.command != self.expected_command
        {
            return Err(io::Error::other(format!(
                "child {} identity changed: {actual:?}",
                self.pid
            )));
        }
        let pid = rustix::process::Pid::from_raw(self.pid as i32)
            .ok_or_else(|| io::Error::other("invalid child PID"))?;
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .map_err(|error| io::Error::other(format!("SIGTERM {}: {error}", self.pid)))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.poll()? {
                use std::os::unix::process::ExitStatusExt;
                return if status.success() || (allow_term && status.signal() == Some(15)) {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "child {} did not exit cleanly after SIGTERM: {status}",
                        self.pid
                    )))
                };
            }
            if Instant::now() >= deadline {
                if identity(self.pid)? != expected {
                    return Err(io::Error::other(format!(
                        "child {} identity changed before fallback",
                        self.pid
                    )));
                }
                self.child.kill()?;
                let kill_deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if self.poll()?.is_some() {
                        return Err(io::Error::other(format!(
                            "child {} required SIGKILL after timeout",
                            self.pid
                        )));
                    }
                    if Instant::now() >= kill_deadline {
                        return Err(io::Error::other(format!(
                            "child {} did not exit after SIGKILL",
                            self.pid
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("fallback cleanup failed for child {}: {error}", self.pid);
        }
    }
}

fn with_child<T>(command: &mut Command, run: impl FnOnce(&mut OwnedChild) -> T) -> T {
    let mut child = OwnedChild::spawn(command).expect("spawn/identify own edger child");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&mut child)));
    let cleanup = child.stop();
    match (result, cleanup) {
        (Ok(value), Ok(())) => value,
        (Err(panic), Ok(())) => std::panic::resume_unwind(panic),
        (Ok(_), Err(error)) => panic!("child cleanup failed: {error}"),
        (Err(panic), Err(error)) => {
            eprintln!("child cleanup also failed: {error}");
            std::panic::resume_unwind(panic)
        }
    }
}

fn rejects_before_vault(settings: &[(&str, &str)], expected: &str) {
    let root = tempfile::tempdir().unwrap();
    let vault = root.path().join("vault-dir").join("vault.db");
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = port.local_addr().unwrap();
    drop(port);
    let mut command = Command::new(env!("CARGO_BIN_EXE_edger"));
    command
        .env_clear()
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("TMPDIR", root.path())
        .env("PATH", "")
        .env("PORT", addr.port().to_string())
        .env("EDGER_APP_SECRETS_DB", &vault)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command.envs(settings.iter().copied());
    with_child(&mut command, |child| {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.poll().expect("try_wait failed") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "child {} did not exit before timeout",
                child.pid
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        child
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(!status.success(), "unexpected success: {stderr}");
        assert!(stderr.contains(expected), "unexpected error: {stderr}");
        assert!(
            !vault.parent().unwrap().exists(),
            "vault directory was created"
        );
        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    });
}

#[test]
fn partial_and_invalid_configuration_fail_before_vault() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    rejects_before_vault(
        &[("EDGER_SQLD_ADMIN_URL", "http://127.0.0.1:1111")],
        "managed sqld configuration is incomplete",
    );
    rejects_before_vault(
        &[("EDGER_SQLD_ADMIN_URL", "")],
        "a managed sqld setting is empty",
    );
}

struct Fixture {
    root: tempfile::TempDir,
    admin: TcpListener,
    port: u16,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in [
            "workers/app",
            "core",
            "state",
            "state/backups",
            "keys",
            "home",
        ] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        // The loader requires an existing 0700 backup directory and a private
        // registry directory; the fixture provides both.
        for dir in ["state", "state/backups", "keys"] {
            fs::set_permissions(root.path().join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(
            root.path().join("workers/app/manifest.yaml"),
            "name: app\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\n",
        )
        .unwrap();
        fs::write(
            root.path().join("workers/app/index.ts"),
            "export default { fetch: () => new Response('ok') };\n",
        )
        .unwrap();
        for (name, content) in [
            ("admin", b"fixture-admin-key".as_slice()),
            ("signer", &[2u8; 32]),
            ("vault", &[3u8; 32]),
        ] {
            let path = root.path().join("keys").join(name);
            fs::write(&path, content).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let admin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        admin.set_nonblocking(true).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        Self { root, admin, port }
    }

    fn command(&self, configured: bool) -> Command {
        let root = self.root.path();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_edger"));
        cmd.env_clear()
            .current_dir(root)
            .env("HOME", root.join("home"))
            .env("USERPROFILE", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("home"))
            .env("TMPDIR", root)
            .env("PATH", "")
            .env("PORT", self.port.to_string())
            .env("EDGER_BIND", "127.0.0.1")
            .env("RUNTIME_WORKER_DIRS", root.join("workers"))
            .env("EDGER_CORE_WORKER_DIR", root.join("core"))
            .env("EDGER_CORE_WORKER_OVERLAY_DIR", root.join("overlay"))
            .env("EDGER_APP_SECRETS_DB", root.join("state/vault.db"))
            .env("EDGER_API_KEYS_DB", root.join("state/api.db"))
            .env("EDGER_SECRETS_KEY_FILE", root.join("keys/vault"))
            .env("ROOT_API_KEY", "fixture-root-key")
            .env("EDGER_CRON_ENABLED", "false")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if configured {
            cmd.env(
                "EDGER_SQLD_ADMIN_URL",
                format!("http://{}", self.admin.local_addr().unwrap()),
            )
            .env("EDGER_SQLD_ADMIN_KEY_FILE", root.join("keys/admin"))
            .env("EDGER_SQLD_USER_URL", "http://127.0.0.1:1")
            .env("EDGER_SQLD_REGISTRY_DB", root.join("state/registry.db"))
            .env("EDGER_SQLD_SIGNER_KEY_FILE", root.join("keys/signer"))
            .env("EDGER_SQLD_MANAGED_BUDGET_BYTES", "67108864")
            .env("EDGER_SQLD_BACKUP_DIR", root.join("state/backups"));
        }
        cmd
    }

    fn ready(&self, child: &mut OwnedChild) {
        self.ready_until(|| {
            let status = child.poll().expect("try_wait failed")?;
            Some(format!("{status}: {}", child.exited_stderr()))
        });
    }

    /// Waits for `/ready`; `exited` reports why EdgeR is gone, if it is.
    fn ready_until(&self, mut exited: impl FnMut() -> Option<String>) {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(reason) = exited() {
                panic!("edger exited before ready: {reason}");
            }
            if let Some(reply) = self.request("/ready", false) {
                if reply.starts_with("HTTP/1.1 200") {
                    return;
                }
            }
            assert!(Instant::now() < end, "edger did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn request(&self, path: &str, auth: bool) -> Option<String> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(100)).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let authorization = if auth {
            "Authorization: Bearer fixture-root-key\r\n"
        } else {
            ""
        };
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Connection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        Some(response)
    }

    fn no_admin_connection(&self) {
        assert_eq!(
            self.admin.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn off_and_configured_boot_and_metadata_do_not_contact_sqld() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    for configured in [false, true] {
        let fx = Fixture::new();
        with_child(&mut fx.command(configured), |child| {
            fx.ready(child);
            let response = fx.request("/api/admin/workers/app/database", true).unwrap();
            if configured {
                assert!(response.starts_with("HTTP/1.1 200"), "{response}");
                assert!(response.contains("\"database\":null"), "{response}");
                assert!(
                    response.contains("\"activation\":\"nextSpawn\""),
                    "{response}"
                );
            } else {
                assert!(response.starts_with("HTTP/1.1 503"), "{response}");
                assert!(!fx.root.path().join("overlay").exists());
            }
            fx.no_admin_connection();
        });
    }
}

fn expect_boot_rejection(fx: &Fixture, cmd: &mut Command, expected: &str) {
    with_child(cmd, |child| {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                fx.request("/ready", false).is_none(),
                "invalid boot answered HTTP"
            );
            if let Some(status) = child.poll().expect("try_wait failed") {
                assert!(!status.success());
                break;
            }
            assert!(Instant::now() < end, "invalid boot did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fx.request("/ready", false).is_none(),
            "invalid boot left HTTP listening"
        );
        let mut stderr = String::new();
        child
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(stderr.contains(expected), "unexpected error: {stderr}");
        fx.no_admin_connection();
    });
}

#[test]
fn configured_without_vault_is_rejected_before_http() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let mut cmd = fx.command(true);
    cmd.env("EDGER_SECRETS_KEY_FILE", fx.root.path().join("missing"));
    expect_boot_rejection(
        &fx,
        &mut cmd,
        "managed sqld requires an available app vault",
    );

    let fx = Fixture::new();
    let mut cmd = fx.command(true);
    cmd.env("EDGER_APP_SECRETS_DB", fx.root.path().join("state"));
    expect_boot_rejection(
        &fx,
        &mut cmd,
        "managed sqld requires an available app vault",
    );
}

#[test]
fn invalid_complete_configuration_fails_before_vault() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let vault = fx.root.path().join("not-created/vault.db");
    let mut cmd = fx.command(true);
    cmd.env("EDGER_SQLD_MANAGED_BUDGET_BYTES", "invalid")
        .env("EDGER_APP_SECRETS_DB", &vault);
    expect_boot_rejection(&fx, &mut cmd, "the managed sqld budget is invalid");
    assert!(!vault.parent().unwrap().exists());
}

#[test]
fn configured_boot_does_not_create_a_missing_worker_root_for_the_vault() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let missing = fx.root.path().join("missing-workers");
    let mut cmd = fx.command(true);
    cmd.env("RUNTIME_WORKER_DIRS", &missing)
        .env_remove("EDGER_APP_SECRETS_DB");
    expect_boot_rejection(&fx, &mut cmd, "managed sqld requires existing worker roots");
    assert!(!missing.exists());
}

#[test]
fn key_in_worker_root_or_cache_is_rejected_before_http() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    for (path, name, bytes) in [
        (
            "workers/admin-key",
            "EDGER_SQLD_ADMIN_KEY_FILE",
            b"fixture-admin-key".as_slice(),
        ),
        (
            "cache/admin-key",
            "EDGER_SQLD_ADMIN_KEY_FILE",
            b"fixture-admin-key".as_slice(),
        ),
        (
            "workers/signer-key",
            "EDGER_SQLD_SIGNER_KEY_FILE",
            &[2u8; 32],
        ),
        ("cache/signer-key", "EDGER_SQLD_SIGNER_KEY_FILE", &[2u8; 32]),
        ("workers/vault-key", "EDGER_SECRETS_KEY_FILE", &[3u8; 32]),
        ("cache/vault-key", "EDGER_SECRETS_KEY_FILE", &[3u8; 32]),
    ] {
        let fx = Fixture::new();
        let path = fx.root.path().join(PathBuf::from(path));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut cmd = fx.command(true);
        cmd.env(name, &path)
            .env("EDGER_DENO_CACHE_ROOT", fx.root.path().join("cache"));
        expect_boot_rejection(
            &fx,
            &mut cmd,
            "a key file lies inside a guest-readable root",
        );
    }
}

#[test]
fn key_symlink_position_and_indexed_worker_target_are_protected() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    use std::os::unix::fs::symlink;
    {
        let fx = Fixture::new();
        let link = fx.root.path().join("workers/key-link");
        symlink(fx.root.path().join("keys/admin"), &link).unwrap();
        let mut cmd = fx.command(true);
        cmd.env("EDGER_SQLD_ADMIN_KEY_FILE", link);
        expect_boot_rejection(
            &fx,
            &mut cmd,
            "a key file lies inside a guest-readable root",
        );
    }
    {
        let fx = Fixture::new();
        let target = fx.root.path().join("external-worker");
        fs::create_dir(&target).unwrap();
        fs::write(
            target.join("manifest.yaml"),
            "name: other\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\n",
        )
        .unwrap();
        fs::write(target.join("index.ts"), "export default {};\n").unwrap();
        let key = target.join("key");
        fs::write(&key, b"fixture-admin-key").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, fx.root.path().join("workers/other")).unwrap();
        let mut cmd = fx.command(true);
        cmd.env("EDGER_SQLD_ADMIN_KEY_FILE", key);
        expect_boot_rejection(
            &fx,
            &mut cmd,
            "a key file lies inside a guest-readable root",
        );
    }
}

#[test]
fn relative_cache_protects_a_future_workers_parent() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let future = fx.root.path().join("future-workers");
    let cache = future.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let key = cache.join("key");
    fs::write(&key, b"fixture-admin-key").unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let roots = format!(
        "{}:{}",
        fx.root.path().join("workers").display(),
        future.display()
    );
    let mut cmd = fx.command(true);
    cmd.current_dir(fx.root.path().join("home"))
        .env("RUNTIME_WORKER_DIRS", roots)
        .env("EDGER_SQLD_ADMIN_KEY_FILE", key)
        .env("EDGER_DENO_CACHE_ROOT", "../cache");
    expect_boot_rejection(
        &fx,
        &mut cmd,
        "a key file lies inside a guest-readable root",
    );
}

#[test]
fn configured_boot_creates_private_roots_without_chmod_existing_roots() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let existing = fx.root.path().join("shared-cache");
    let new_cache = fx.root.path().join("new-cache");
    fs::create_dir(&existing).unwrap();
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o750)).unwrap();
    let mut cmd = fx.command(true);
    cmd.env("DENO_DIR", &existing)
        .env("EDGER_DENO_CACHE_ROOT", &new_cache);
    with_child(&mut cmd, |child| {
        fx.ready(child);
        for path in [fx.root.path().join("overlay"), new_cache] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(existing).unwrap().permissions().mode() & 0o777,
            0o750
        );
        fx.no_admin_connection();
    });
}

fn private_admin_key(path: &std::path::Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"fixture-admin-key").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn indexed_disabled_and_staged_symlink_targets_are_protected() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    use std::os::unix::fs::symlink;
    for staged in [false, true] {
        let fx = Fixture::new();
        let target = fx.root.path().join("external-worker");
        fs::create_dir(&target).unwrap();
        fs::write(
            target.join("manifest.yaml"),
            "name: other\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\n",
        )
        .unwrap();
        fs::write(target.join("index.ts"), "export default {};\n").unwrap();
        if staged {
            fs::write(
                target.join(".edger-revision"),
                "fixture-revision\nstaged=true\n",
            )
            .unwrap();
        } else {
            fs::write(target.join(".edger-enabled"), "enabled=false\n").unwrap();
        }
        symlink(&target, fx.root.path().join("workers/other")).unwrap();
        with_child(&mut fx.command(true), |child| {
            fx.ready(child);
            let response = fx.request("/api/admin/workers", true).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            let body = response.split_once("\r\n\r\n").unwrap().1;
            let body: serde_json::Value = serde_json::from_str(body).expect("workers JSON");
            let other = body["workers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["name"] == "other")
                .expect("other is indexed");
            assert_eq!(other["staged"], staged);
            assert_eq!(other["status"], if staged { "loaded" } else { "disabled" });
        });
        let key = target.join("admin-key");
        private_admin_key(&key);
        let mut cmd = fx.command(true);
        cmd.env("EDGER_SQLD_ADMIN_KEY_FILE", key);
        expect_boot_rejection(
            &fx,
            &mut cmd,
            "a key file lies inside a guest-readable root",
        );
    }
}

#[test]
fn shared_default_protects_only_the_three_home_subpaths() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    for relative in ["Library/Caches/deno", ".cache/deno", ".deno"] {
        let fx = Fixture::new();
        let key = fx.root.path().join("home").join(relative).join("admin-key");
        private_admin_key(&key);
        let mut cmd = fx.command(true);
        cmd.env("EDGER_SQLD_ADMIN_KEY_FILE", key)
            .env_remove("DENO_DIR");
        expect_boot_rejection(
            &fx,
            &mut cmd,
            "a key file lies inside a guest-readable root",
        );
    }
    let fx = Fixture::new();
    let key = fx.root.path().join("home/admin-key");
    private_admin_key(&key);
    let mut cmd = fx.command(true);
    cmd.env("EDGER_SQLD_ADMIN_KEY_FILE", key)
        .env_remove("DENO_DIR");
    with_child(&mut cmd, |child| {
        fx.ready(child);
        fx.no_admin_connection();
    });
}

#[test]
fn relative_existing_worker_cache_resolves_symlink_before_parent() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let external = fx.root.path().join("external");
    fs::create_dir_all(external.join("target")).unwrap();
    symlink(
        external.join("target"),
        fx.root.path().join("workers/app/link"),
    )
    .unwrap();
    let key = external.join("cache/admin-key");
    private_admin_key(&key);
    let mut control = fx.command(true);
    control.env("EDGER_DENO_CACHE_ROOT", "link/../cache");
    with_child(&mut control, |child| {
        fx.ready(child);
        fx.no_admin_connection();
    });
    let mut cmd = fx.command(true);
    cmd.env("EDGER_DENO_CACHE_ROOT", "link/../cache")
        .env("EDGER_SQLD_ADMIN_KEY_FILE", key);
    expect_boot_rejection(
        &fx,
        &mut cmd,
        "a key file lies inside a guest-readable root",
    );
}

#[test]
fn relative_vault_master_is_the_evidence_and_symlink_location_is_protected() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let mut control = fx.command(true);
    control.env("EDGER_SECRETS_KEY_FILE", "keys/vault");
    with_child(&mut control, |child| {
        fx.ready(child);
        fx.no_admin_connection();
    });
    fs::write(fx.root.path().join("keys/signer"), [3u8; 32]).unwrap();
    let mut same = fx.command(true);
    same.env("EDGER_SECRETS_KEY_FILE", "keys/vault");
    expect_boot_rejection(
        &fx,
        &mut same,
        "the signer master equals the app secrets master",
    );

    for placement in ["workers/vault-link", "cache/vault-link"] {
        let fx = Fixture::new();
        let link = fx.root.path().join(placement);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(fx.root.path().join("keys/vault"), &link).unwrap();
        let mut cmd = fx.command(true);
        cmd.env("EDGER_SECRETS_KEY_FILE", placement)
            .env("EDGER_DENO_CACHE_ROOT", fx.root.path().join("cache"));
        expect_boot_rejection(
            &fx,
            &mut cmd,
            "a key file lies inside a guest-readable root",
        );
    }
}

#[test]
fn future_cache_resolving_to_filesystem_root_refuses_key_without_chmod() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    use std::os::unix::fs::symlink;
    let fx = Fixture::new();
    let mode = fs::metadata("/").unwrap().permissions().mode() & 0o7777;
    let scan = fx.root.path().join("future-scan");
    let mut deep = fx.root.path().join("deep");
    for _ in 0..(fx.root.path().components().count() + 3) {
        deep.push("level");
    }
    let cwd = deep.join("cwd");
    let worker = deep.join("worker");
    fs::create_dir_all(&scan).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join("manifest.yaml"),
        "name: deep-app\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\n",
    )
    .unwrap();
    fs::write(worker.join("index.ts"), "export default {};\n").unwrap();
    symlink(&worker, scan.join("app")).unwrap();
    let relative = vec![".."; scan.canonicalize().unwrap().components().count()].join("/");
    let key = fx.root.path().join("keys/vault").canonicalize().unwrap();
    // Only the future-child overapproximation reaches / in this fixture:
    // the host cwd and actual indexed worker resolve substantially deeper.
    assert!(!key.starts_with(cwd.join(&relative).canonicalize().unwrap()));
    assert!(!key.starts_with(worker.join(&relative).canonicalize().unwrap()));
    let mut cmd = fx.command(true);
    cmd.current_dir(&cwd)
        .env("RUNTIME_WORKER_DIRS", &scan)
        .env("EDGER_DENO_CACHE_ROOT", relative);
    expect_boot_rejection(
        &fx,
        &mut cmd,
        "a key file lies inside a guest-readable root",
    );
    assert_eq!(
        fs::metadata("/").unwrap().permissions().mode() & 0o7777,
        mode
    );
}

struct ImportWitness {
    address: SocketAddr,
    count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    candidate: Arc<Mutex<Option<u32>>>,
    release: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ImportWitness {
    fn new() -> Self {
        Self::new_with_gate(false)
    }

    fn new_with_gate(gated: bool) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let candidate = Arc::new(Mutex::new(None));
        let release = Arc::new(AtomicBool::new(!gated));
        let (hits, done) = (Arc::clone(&count), Arc::clone(&stop));
        let (seen, proceed) = (Arc::clone(&candidate), Arc::clone(&release));
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        hits.fetch_add(1, Ordering::SeqCst);
                        socket.set_nonblocking(true).expect("witness nonblocking");
                        let mut header = Vec::new();
                        let mut chunk = [0u8; 1024];
                        let deadline = Instant::now() + Duration::from_secs(1);
                        while Instant::now() < deadline
                            && header.len() < 8192
                            && !header.windows(4).any(|part| part == b"\r\n\r\n")
                        {
                            match socket.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(size) => header.extend_from_slice(&chunk[..size]),
                                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(Duration::from_millis(5))
                                }
                                Err(_) => break,
                            }
                        }
                        if let Ok(text) = std::str::from_utf8(&header) {
                            if let Some(pid) = text
                                .lines()
                                .next()
                                .and_then(|line| line.strip_prefix("GET /?pid="))
                                .and_then(|rest| rest.split_once(' '))
                                .and_then(|(pid, _)| pid.parse::<u32>().ok())
                            {
                                *seen.lock().expect("witness PID mutex") = Some(pid);
                            }
                        }
                        let deadline = Instant::now() + Duration::from_secs(20);
                        while !proceed.load(Ordering::SeqCst)
                            && !done.load(Ordering::SeqCst)
                            && Instant::now() < deadline
                        {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        socket.set_nonblocking(false).expect("witness blocking");
                        socket
                            .set_write_timeout(Some(Duration::from_secs(1)))
                            .expect("witness timeout");
                        let _ = socket
                            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("witness accept: {error}"),
                }
            }
        });
        Self {
            address,
            count,
            stop,
            candidate,
            release,
            thread: Some(thread),
        }
    }
    fn hits(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
    fn candidate(&self) -> Option<u32> {
        *self.candidate.lock().expect("witness PID mutex")
    }
    fn release(&self) {
        self.release.store(true, Ordering::SeqCst);
    }
}

impl Drop for ImportWitness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() && !std::thread::panicking() {
                panic!("import witness thread failed");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Kernel-tree ownership for the tests in which EdgeR starts a Deno process.
//
// The network PID beacon cannot be the only way to find a Deno child: there is
// a window in which Deno exists but has not reported its PID yet. These tests
// therefore run EdgeR under a helper PROCESS (this same test binary, re-entered
// through the `#[ignore]`d `subreaper_helper_process` test, which does nothing
// without the sentinel and spec written by the parent). The helper becomes a
// Linux child subreaper before it starts EdgeR, so every descendant of EdgeR,
// including one adopted after EdgeR dies, stays in a tree that only this
// fixture owns. The tree is read from the kernel (`/proc/<helper>/task/*/children`),
// never from a scan of the host's PIDs or command lines.
// ---------------------------------------------------------------------------

const SENTINEL_ENV: &str = "EDGER_TEST_SUBREAPER_TOKEN";
const SPEC_ENV: &str = "EDGER_TEST_SUBREAPER_SPEC";
const HELPER_TEST: &str = "subreaper_helper_process";
const PROTOCOL: &str = "SUBREAPER ";

fn is_gone(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(3)
}

fn one_line(text: impl std::fmt::Display) -> String {
    text.to_string().replace(['\n', '\r'], " ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Descendant {
    exe: String,
    parent: u32,
    pid: u32,
    start: String,
    state: String,
}

/// Direct children of `pid` across every one of its threads. A missing
/// `children` interface is an error, never an empty answer.
fn children_of(pid: u32) -> io::Result<Vec<u32>> {
    let mut found = Vec::new();
    let tasks = match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => tasks,
        Err(error) if is_gone(&error) => return Ok(found),
        Err(error) => return Err(error),
    };
    for task in tasks {
        let task = match task {
            Ok(task) => task,
            Err(error) if is_gone(&error) => continue,
            Err(error) => return Err(error),
        };
        let file = task.path().join("children");
        match fs::read_to_string(&file) {
            Ok(text) => {
                for item in text.split_whitespace() {
                    found.push(
                        item.parse()
                            .map_err(|_| io::Error::other("invalid children entry"))?,
                    );
                }
            }
            Err(error) if is_gone(&error) => {
                if task.path().exists() {
                    return Err(io::Error::other(format!(
                        "{} is unavailable: {error}",
                        file.display()
                    )));
                }
            }
            Err(error) => return Err(error),
        }
    }
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// Every descendant (zombies included) of `root`, found through the kernel's
/// own parent/child links only.
fn descendants(root: u32) -> io::Result<Vec<Descendant>> {
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = vec![root];
    let mut found = Vec::new();
    while let Some(parent) = queue.pop() {
        for pid in children_of(parent)? {
            if !seen.insert(pid) {
                continue;
            }
            let (state, ppid, start) = match proc_stat(pid) {
                Ok(stat) => stat,
                Err(error) if is_gone(&error) => continue,
                Err(error) => return Err(error),
            };
            let exe = match fs::read_link(format!("/proc/{pid}/exe")) {
                Ok(path) => path.display().to_string(),
                Err(_) if state == "Z" => String::new(),
                Err(error) => format!("unreadable:{}", one_line(error).replace(' ', "_")),
            };
            found.push(Descendant {
                exe,
                parent: ppid,
                pid,
                start,
                state,
            });
            queue.push(pid);
        }
    }
    found.sort_by_key(|item| item.pid);
    Ok(found)
}

fn say(line: impl AsRef<str>) {
    let mut out = io::stdout().lock();
    // libtest leaves "test name ... " unterminated on stdout; start a fresh line.
    let _ = writeln!(
        out,
        "
{PROTOCOL}{}",
        line.as_ref()
    );
    let _ = out.flush();
}

impl OwnedChild {
    /// SIGKILL of the child itself after its identity is re-proven (the
    /// crash scenario); the PID is ours and unreaped, so it cannot be reused.
    fn crash(&mut self) -> io::Result<()> {
        if self.poll()?.is_some() {
            return Ok(());
        }
        let expected = self
            .identity
            .clone()
            .ok_or_else(|| io::Error::other(format!("no identity for live child {}", self.pid)))?;
        if identity(self.pid)? != expected {
            return Err(io::Error::other(format!(
                "child {} identity changed before the crash",
                self.pid
            )));
        }
        self.child.kill()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.poll()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("child survived SIGKILL"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Helper side of the cleanup: EdgeR first (SIGTERM, identity-checked), then
/// whatever the kernel still shows below this process. Only direct children of
/// the helper are signalled: their PID stays pinned by the unreaped zombie, and
/// the start time is re-read immediately before the signal.
fn helper_cleanup(edger: &mut OwnedChild, me: u32) -> bool {
    let mut ok = true;
    match edger.stop_with(true) {
        Ok(()) => say("edger-stopped"),
        Err(error) => {
            ok = false;
            say(format!("problem edger stop failed: {}", one_line(error)));
        }
    }
    let mut known: std::collections::BTreeMap<u32, (String, String)> =
        std::collections::BTreeMap::new();
    let mut outcome: std::collections::BTreeMap<u32, &'static str> =
        std::collections::BTreeMap::new();
    let mut killed = std::collections::BTreeSet::new();
    let started = Instant::now();
    let grace = started + Duration::from_secs(5);
    let deadline = started + Duration::from_secs(30);
    loop {
        let list = match descendants(me) {
            Ok(list) => list,
            Err(error) => {
                ok = false;
                say(format!(
                    "problem tree enumeration failed: {}",
                    one_line(error)
                ));
                break;
            }
        };
        if list.is_empty() {
            break;
        }
        for item in &list {
            let slot = known
                .entry(item.pid)
                .or_insert_with(|| (item.start.clone(), item.exe.clone()));
            if slot.1.is_empty() {
                slot.1 = item.exe.clone();
            }
            if item.parent != me {
                continue;
            }
            let Some(target) = rustix::process::Pid::from_raw(item.pid as i32) else {
                continue;
            };
            match rustix::process::waitpid(Some(target), rustix::process::WaitOptions::NOHANG) {
                Ok(Some(_)) => {
                    outcome.insert(
                        item.pid,
                        if killed.contains(&item.pid) {
                            "killed"
                        } else {
                            "exited"
                        },
                    );
                }
                Ok(None) if Instant::now() >= grace && !killed.contains(&item.pid) => {
                    match proc_stat(item.pid) {
                        Ok((state, parent, start))
                            if parent == me && start == item.start && state != "Z" =>
                        {
                            match rustix::process::kill_process(
                                target,
                                rustix::process::Signal::KILL,
                            ) {
                                Ok(()) => {
                                    killed.insert(item.pid);
                                }
                                Err(error) => {
                                    ok = false;
                                    say(format!("problem SIGKILL {}: {error}", item.pid));
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) if is_gone(&error) => {}
                        Err(error) => {
                            ok = false;
                            say(format!("problem proc {}: {}", item.pid, one_line(error)));
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    ok = false;
                    say(format!("problem waitpid {}: {error}", item.pid));
                }
            }
        }
        if Instant::now() >= deadline {
            ok = false;
            say("problem descendants remain after the cleanup deadline");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let empty = matches!(descendants(me), Ok(list) if list.is_empty());
    if !empty {
        ok = false;
    }
    for (pid, (start, exe)) in &known {
        let result = match outcome.get(pid) {
            Some(result) => *result,
            None => match proc_stat(*pid) {
                Err(error) if is_gone(&error) => "gone",
                Ok((_, _, current)) if current != *start => "gone",
                _ => {
                    ok = false;
                    "alive"
                }
            },
        };
        say(format!("entry {pid} {result} {start} {exe}"));
    }
    say(format!("tree-empty {empty}"));
    ok
}

fn helper_main(token: &str, spec_path: &std::path::Path) -> io::Result<()> {
    let bad = |what: &str| io::Error::other(format!("invalid subreaper spec: {what}"));
    let spec: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(spec_path)?).map_err(io::Error::other)?;
    if spec["token"].as_str() != Some(token) {
        return Err(bad("token"));
    }
    let program = spec["program"].as_str().ok_or_else(|| bad("program"))?;
    let me = rustix::process::getpid();
    rustix::process::set_child_subreaper(Some(me)).map_err(io::Error::from)?;
    let mut command = Command::new(program);
    command.env_clear();
    for arg in spec["args"].as_array().ok_or_else(|| bad("args"))? {
        command.arg(arg.as_str().ok_or_else(|| bad("arg"))?);
    }
    for (key, value) in spec["env"].as_object().ok_or_else(|| bad("env"))? {
        command.env(key, value.as_str().ok_or_else(|| bad("env value"))?);
    }
    command
        .current_dir(spec["cwd"].as_str().ok_or_else(|| bad("cwd"))?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(
            spec["stderr"].as_str().ok_or_else(|| bad("stderr"))?,
        )?);
    let mut edger = OwnedChild::spawn(&mut command)?;
    let me = me.as_raw_nonzero().get() as u32;
    say(format!("ready {} {me}", edger.pid));
    let status = |edger: &mut OwnedChild| match edger.poll() {
        Ok(None) => "edger alive".to_string(),
        Ok(Some(status)) => format!("edger exited {status}"),
        Err(error) => format!("edger poll-error {}", one_line(error)),
    };
    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        match line.trim() {
            "status" => {
                say(status(&mut edger));
                say("end");
            }
            "tree" => {
                say(status(&mut edger));
                match descendants(me) {
                    Ok(list) => {
                        for item in list {
                            say(format!(
                                "d {} {} {} {} {}",
                                item.pid, item.parent, item.start, item.state, item.exe
                            ));
                        }
                    }
                    Err(error) => say(format!(
                        "problem tree enumeration failed: {}",
                        one_line(error)
                    )),
                }
                say("end");
            }
            "crash" => {
                match edger.crash() {
                    Ok(()) => say("crashed"),
                    Err(error) => say(format!("problem crash failed: {}", one_line(error))),
                }
                say("end");
            }
            "stubborn" => {
                // A shell with a background sleep: killing the shell leaves the
                // sleep to be adopted by this subreaper (a second-generation orphan).
                match Command::new("/bin/sh")
                    .args(["-c", "/bin/sleep 600 & wait"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(child) => say(format!("stubborn {}", child.id())),
                    Err(error) => say(format!(
                        "problem stubborn spawn failed: {}",
                        one_line(error)
                    )),
                }
                say("end");
            }
            "stop" => break,
            _ => {
                say("problem unknown command");
                say("end");
            }
        }
    }
    let ok = helper_cleanup(&mut edger, me);
    say(if ok { "done ok" } else { "done failed" });
    if ok {
        Ok(())
    } else {
        Err(io::Error::other("subreaper cleanup reported problems"))
    }
}

/// Re-entered by the parent tests only. Without the sentinel and the spec the
/// parent writes, it does nothing: it never starts anything on its own.
#[test]
#[ignore = "helper process: started by the prewarm tests with their sentinel and spec"]
fn subreaper_helper_process() {
    let (Some(token), Some(spec)) = (std::env::var(SENTINEL_ENV).ok(), std::env::var_os(SPEC_ENV))
    else {
        eprintln!("subreaper helper: no sentinel/spec, nothing to run");
        return;
    };
    helper_main(&token, std::path::Path::new(&spec)).expect("subreaper helper");
}

#[derive(Debug, Default)]
struct CleanupReport {
    done_ok: bool,
    edger_stopped: bool,
    entries: Vec<(u32, String, String, String)>,
    helper_exit_ok: bool,
    problems: Vec<String>,
    tree_empty: bool,
}

impl CleanupReport {
    fn ok(&self) -> bool {
        self.done_ok
            && self.edger_stopped
            && self.helper_exit_ok
            && self.tree_empty
            && self.problems.is_empty()
    }

    fn outcome(&self, pid: u32, start: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.0 == pid && entry.2 == start)
            .map(|entry| entry.1.as_str())
    }
}

struct Snapshot {
    alive: bool,
    entries: Vec<Descendant>,
}

struct SubreaperTree {
    edger_pid: u32,
    edger_stderr: PathBuf,
    finished: bool,
    helper: OwnedChild,
    helper_pid: u32,
    helper_stderr: PathBuf,
    input: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    reader: Option<JoinHandle<()>>,
    stray: Arc<Mutex<Vec<String>>>,
}

impl SubreaperTree {
    fn start(command: &Command, root: &std::path::Path) -> io::Result<Self> {
        let text = |value: &std::ffi::OsStr| {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| io::Error::other("non-UTF-8 command part"))
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let token = format!("subreaper-{}-{nanos}", std::process::id());
        let (spec_path, edger_stderr, helper_stderr) = (
            root.join("subreaper-spec.json"),
            root.join("edger.stderr"),
            root.join("subreaper-helper.stderr"),
        );
        let mut env = serde_json::Map::new();
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                env.insert(text(key)?, serde_json::Value::String(text(value)?));
            }
        }
        let mut args = Vec::new();
        for arg in command.get_args() {
            args.push(text(arg)?);
        }
        let cwd = command
            .get_current_dir()
            .ok_or_else(|| io::Error::other("command has no cwd"))?;
        let spec = serde_json::json!({
            "args": args,
            "cwd": cwd.to_str().ok_or_else(|| io::Error::other("non-UTF-8 cwd"))?,
            "env": env,
            "program": text(command.get_program())?,
            "stderr": edger_stderr.to_str().ok_or_else(|| io::Error::other("non-UTF-8 path"))?,
            "token": token,
        });
        fs::write(&spec_path, spec.to_string())?;
        fs::set_permissions(&spec_path, fs::Permissions::from_mode(0o600))?;
        let mut helper_command = Command::new(std::env::current_exe()?);
        helper_command
            .args([
                HELPER_TEST,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env(SENTINEL_ENV, &token)
            .env(SPEC_ENV, &spec_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&helper_stderr)?);
        let mut helper = OwnedChild::spawn(&mut helper_command)?;
        let input = helper.child.stdin.take();
        let stdout = helper
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("helper stdout missing"))?;
        let (sender, lines) = std::sync::mpsc::channel();
        let stray = Arc::new(Mutex::new(Vec::new()));
        let stray_lines = Arc::clone(&stray);
        let reader = std::thread::spawn(move || {
            for line in io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match line.strip_prefix(PROTOCOL) {
                    Some(rest) => {
                        if sender.send(rest.to_owned()).is_err() {
                            break;
                        }
                    }
                    None => stray_lines.lock().expect("stray output mutex").push(line),
                }
            }
        });
        let helper_pid = helper.pid;
        let mut tree = Self {
            edger_pid: 0,
            edger_stderr,
            finished: false,
            helper,
            helper_pid,
            helper_stderr,
            input,
            lines,
            reader: Some(reader),
            stray,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match tree.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    let mut parts = line.split_whitespace();
                    if parts.next() == Some("ready") {
                        tree.edger_pid = parts
                            .next()
                            .and_then(|part| part.parse().ok())
                            .ok_or_else(|| io::Error::other("invalid ready line"))?;
                        return Ok(tree);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if tree.helper.poll()?.is_some() || Instant::now() >= deadline {
                        return Err(io::Error::other(format!(
                            "subreaper helper did not become ready: {}",
                            tree.helper_log()
                        )));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other(format!(
                        "subreaper helper closed its channel: {}",
                        tree.helper_log()
                    )));
                }
            }
        }
    }

    fn helper_log(&self) -> String {
        let stray = self
            .stray
            .lock()
            .map(|lines| lines.join(" | "))
            .unwrap_or_default();
        format!(
            "stderr=[{}] stdout=[{}]",
            one_line(fs::read_to_string(&self.helper_stderr).unwrap_or_default()),
            one_line(stray)
        )
    }

    fn request(&mut self, command: &str) -> io::Result<Vec<String>> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| io::Error::other("helper input is closed"))?;
        writeln!(input, "{command}")?;
        input.flush()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut answer = Vec::new();
        loop {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) if line == "end" => return Ok(answer),
                Ok(line) => answer.push(line),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => {
                    return Err(io::Error::other(format!(
                        "no answer to `{command}`: {}",
                        self.helper_log()
                    )))
                }
            }
        }
    }

    /// `Some(reason)` once EdgeR itself has exited, with its stderr.
    fn exited(&mut self) -> Option<String> {
        let answer = self.request("status").expect("helper status");
        let first = answer.first().cloned().unwrap_or_default();
        if first == "edger alive" {
            return None;
        }
        Some(format!(
            "{first}: {}",
            one_line(fs::read_to_string(&self.edger_stderr).unwrap_or_default())
        ))
    }

    fn snapshot(&mut self) -> io::Result<Snapshot> {
        let answer = self.request("tree")?;
        let mut snapshot = Snapshot {
            alive: false,
            entries: Vec::new(),
        };
        for line in answer {
            if line == "edger alive" {
                snapshot.alive = true;
            } else if let Some(rest) = line.strip_prefix("d ") {
                let invalid = || io::Error::other(format!("invalid tree line: {line}"));
                let parts: Vec<&str> = rest.splitn(5, ' ').collect();
                if parts.len() < 4 {
                    return Err(invalid());
                }
                snapshot.entries.push(Descendant {
                    exe: parts.get(4).copied().unwrap_or("").to_owned(),
                    parent: parts[1].parse().map_err(|_| invalid())?,
                    pid: parts[0].parse().map_err(|_| invalid())?,
                    start: parts[2].to_owned(),
                    state: parts[3].to_owned(),
                });
            } else if line.starts_with("problem ") {
                return Err(io::Error::other(line));
            }
        }
        Ok(snapshot)
    }

    fn crash_edger(&mut self) -> io::Result<()> {
        let answer = self.request("crash")?;
        if answer.iter().any(|line| line == "crashed") {
            Ok(())
        } else {
            Err(io::Error::other(format!("crash failed: {answer:?}")))
        }
    }

    fn stop(&mut self) -> io::Result<CleanupReport> {
        if self.finished {
            return Err(io::Error::other("subreaper tree already stopped"));
        }
        self.finished = true;
        if let Some(mut input) = self.input.take() {
            let _ = writeln!(input, "stop");
            let _ = input.flush();
        }
        let mut report = CleanupReport::default();
        let deadline = Instant::now() + Duration::from_secs(75);
        let mut complete = false;
        while !complete {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => match line.split_once(' ').unwrap_or((line.as_str(), "")) {
                    ("edger-stopped", _) => report.edger_stopped = true,
                    ("tree-empty", value) => report.tree_empty = value == "true",
                    ("problem", rest) => report.problems.push(rest.to_owned()),
                    ("entry", rest) => {
                        let parts: Vec<&str> = rest.splitn(4, ' ').collect();
                        match parts.first().and_then(|part| part.parse::<u32>().ok()) {
                            Some(pid) if parts.len() == 4 => {
                                report.entries.push((
                                    pid,
                                    parts[1].to_owned(),
                                    parts[2].to_owned(),
                                    parts[3].to_owned(),
                                ));
                            }
                            _ => report.problems.push(format!("invalid entry line: {line}")),
                        }
                    }
                    ("done", value) => {
                        report.done_ok = value == "ok";
                        complete = true;
                    }
                    _ => {}
                },
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => {
                    report.problems.push(format!(
                        "helper ended without a done line: {}",
                        self.helper_log()
                    ));
                    break;
                }
            }
        }
        let exit_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.helper.poll()? {
                Some(status) => {
                    report.helper_exit_ok = status.success();
                    break;
                }
                None if Instant::now() >= exit_deadline => {
                    report
                        .problems
                        .push("subreaper helper did not exit; terminating it".to_owned());
                    let _ = self.helper.stop();
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(report)
    }
}

impl Drop for SubreaperTree {
    fn drop(&mut self) {
        if !self.finished {
            match self.stop() {
                Ok(report) if report.ok() => {}
                Ok(report) => eprintln!("subreaper fallback cleanup reported problems: {report:?}"),
                Err(error) => eprintln!("subreaper fallback cleanup failed: {error}"),
            }
        }
    }
}

/// Runs `body`, then always stops the tree, whether the body returned,
/// panicked or timed out. The cleanup result is returned to the caller.
fn run_guarded(
    tree: &mut SubreaperTree,
    body: impl FnOnce(&mut SubreaperTree),
) -> (
    Result<(), Box<dyn std::any::Any + Send>>,
    io::Result<CleanupReport>,
) {
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(tree)));
    let cleanup = tree.stop();
    (attempt, cleanup)
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "non-text panic".to_owned()
    }
}

fn process_absent(pid: u32, start: &str, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match proc_stat(pid) {
            Err(error) if is_gone(&error) => return Ok(()),
            Err(error) => return Err(error),
            Ok((_, _, current)) if current != start => return Ok(()),
            Ok((state, _, _)) if Instant::now() >= deadline => {
                return Err(io::Error::other(format!(
                    "process {pid} is still present in state {state}"
                )));
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// The network PID is only a candidate: the process must be a Deno
/// started by this EdgeR, in the helper's tree, with the expected exe, cwd
/// and argv, and be stable while `/proc` is read.
fn verify_deno(
    pid: u32,
    edger_pid: u32,
    deno: &std::ffi::OsStr,
    worker: &std::path::Path,
    root: &std::path::Path,
    entry: &Descendant,
) -> io::Result<Identity> {
    if pid <= 1 || pid == edger_pid || pid == std::process::id() {
        return Err(io::Error::other("untrusted canary PID"));
    }
    if entry.pid != pid || entry.parent != edger_pid {
        return Err(io::Error::other(
            "canary PID is not an EdgeR child in the subreaper tree",
        ));
    }
    let before = proc_stat(pid)?;
    if before.1 != edger_pid || before.2 != entry.start {
        return Err(io::Error::other(
            "canary PID differs from the subreaper tree",
        ));
    }
    let actual = identity(pid)?;
    let after = proc_stat(pid)?;
    if after.1 != before.1 || after.2 != before.2 || actual.start_ticks != before.2 {
        return Err(io::Error::other("Deno identity changed while reading proc"));
    }
    let expected_exe = fs::canonicalize(deno)?;
    let worker = worker.canonicalize()?;
    let root = root.canonicalize()?;
    let argv: Vec<&[u8]> = actual
        .command
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect();
    let entry_url = format!(
        "file://{}",
        worker.join("index.ts").canonicalize()?.to_string_lossy()
    );
    let harness = argv.windows(3).any(|parts| {
        let harness = std::path::Path::new(std::ffi::OsStr::from_bytes(parts[0]));
        let socket = std::path::Path::new(std::ffi::OsStr::from_bytes(parts[1]));
        harness
            .file_name()
            .is_some_and(|name| name == "harness.mjs")
            && socket.file_name().is_some_and(|name| name == "w.sock")
            && harness.parent() == socket.parent()
            && harness
                .canonicalize()
                .is_ok_and(|path| path.starts_with(&root))
            && harness
                .parent()
                .and_then(|dir| dir.file_name())
                .is_some_and(|name| name.as_bytes().starts_with(b"edger-uds-"))
            && parts[2] == entry_url.as_bytes()
    });
    if actual.exe != expected_exe
        || actual.cwd != worker
        || argv.first().copied() != Some(deno.as_bytes())
        || argv.get(1).copied() != Some(b"run".as_slice())
        || !argv.contains(&b"--no-check".as_slice())
        || !harness
    {
        return Err(io::Error::other(format!(
            "canary PID {pid} failed exe/cwd/argv validation"
        )));
    }
    Ok(actual)
}

fn wait_candidate(
    witness: &ImportWitness,
    tree: &mut SubreaperTree,
    timeout: Duration,
) -> Result<u32, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = witness.candidate() {
            return Ok(pid);
        }
        if let Some(reason) = tree.exited() {
            return Err(format!("EdgeR exited before the prewarm canary: {reason}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("prewarm PID timeout after {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The persistent Deno that EdgeR starts (the one running the harness), seen
/// through the subreaper tree before any network beacon.
fn wait_for_tree_deno(
    tree: &mut SubreaperTree,
    deno_exe: &std::path::Path,
    timeout: Duration,
) -> Descendant {
    let deadline = Instant::now() + timeout;
    loop {
        let snapshot = tree.snapshot().expect("subreaper snapshot");
        assert!(
            snapshot.alive,
            "EdgeR exited before its Deno appeared: {}",
            one_line(fs::read_to_string(&tree.edger_stderr).unwrap_or_default())
        );
        for entry in &snapshot.entries {
            if entry.parent == tree.edger_pid
                && entry.state != "Z"
                && std::path::Path::new(&entry.exe) == deno_exe
            {
                let command = fs::read(format!("/proc/{}/cmdline", entry.pid)).unwrap_or_default();
                if command.windows(11).any(|part| part == b"harness.mjs") {
                    return entry.clone();
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "EdgeR never started its persistent Deno in the subreaper tree (edger {}, deno {}): {:?}",
            tree.edger_pid, deno_exe.display(), snapshot.entries
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn real_deno() -> std::ffi::OsString {
    std::env::var_os("EDGER_DENO_BIN")
        .expect("EDGER_DENO_BIN must name the real Deno executable for this test")
}

#[test]
#[ignore = "requires the real Deno 2.9.8 executable named by EDGER_DENO_BIN"]
fn configured_edger_prewarms_canary_without_a_worker_request() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let witness = ImportWitness::new_with_gate(true);
    let deno = real_deno();
    let worker = fx.root.path().join("workers/app");
    let address = witness.address;
    fs::write(worker.join("index.ts"), format!(
        "await fetch('http://{address}/?pid=' + Deno.pid);\nexport default {{ fetch() {{ return new Response('ok'); }} }};\n"
    )).unwrap();
    fs::write(worker.join("manifest.yaml"), format!(
        "name: app\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\nminProcesses: 1\nmaxProcesses: 1\nttl: 60s\nallowNet: ['{address}']\n"
    )).unwrap();
    let mut cmd = fx.command(true);
    cmd.env("EDGER_DENO_BIN", &deno);
    let mut tree =
        SubreaperTree::start(&cmd, fx.root.path()).expect("start EdgeR under the subreaper helper");
    let mut observed: Option<Descendant> = None;
    let (attempt, cleanup) = run_guarded(&mut tree, |tree| {
        let pid = wait_candidate(&witness, tree, Duration::from_secs(20)).unwrap_or_else(|error| {
            panic!(
                "{error}: address={address}, deno={deno:?}, worker={}",
                worker.display()
            )
        });
        let snapshot = tree.snapshot().expect("subreaper snapshot");
        let entry = snapshot
            .entries
            .into_iter()
            .find(|entry| entry.pid == pid)
            .unwrap_or_else(|| panic!("candidate PID {pid} is not in the subreaper tree"));
        verify_deno(pid, tree.edger_pid, &deno, &worker, fx.root.path(), &entry)
            .unwrap_or_else(|error| panic!("prewarm identity: pid={pid}, error={error}"));
        observed = Some(entry);
        witness.release();
        // Only readiness and health are requested; no worker route is called.
        fx.ready_until(|| tree.exited());
        let health = fx.request("/health", false).expect("health response");
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert_eq!(witness.hits(), 1, "expected one prewarm import");
        fx.no_admin_connection();
    });
    witness.release();
    if let Err(panic) = attempt {
        eprintln!("prewarm cleanup after the failure: {cleanup:?}");
        std::panic::resume_unwind(panic);
    }
    let report = cleanup.expect("subreaper cleanup protocol");
    assert!(report.ok(), "prewarm cleanup unproven: {report:?}");
    let entry = observed.expect("the Deno was verified");
    let outcome = report
        .outcome(entry.pid, &entry.start)
        .expect("the verified Deno is in the cleanup report");
    assert!(
        ["exited", "killed", "gone"].contains(&outcome),
        "unexpected outcome {outcome}"
    );
    process_absent(entry.pid, &entry.start, Duration::from_secs(5))
        .expect("verified Deno is absent");
}

/// The cleanup fallback itself: a descendant that does not leave by itself is
/// killed only after the grace period, with its identity re-read, and a child of
/// that descendant is adopted and cleaned in a later round.
#[test]
fn subreaper_kills_a_stubborn_descendant_tree_after_the_grace_period() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let mut tree = SubreaperTree::start(&fx.command(true), fx.root.path())
        .expect("start EdgeR under the subreaper helper");
    let mut seen: Vec<Descendant> = Vec::new();
    let (attempt, cleanup) = run_guarded(&mut tree, |tree| {
        fx.ready_until(|| tree.exited());
        let answer = tree.request("stubborn").expect("stubborn request");
        let shell: u32 = answer
            .iter()
            .find_map(|line| line.strip_prefix("stubborn ")?.parse().ok())
            .expect("shell pid");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = tree.snapshot().expect("subreaper snapshot");
            let sleeper = snapshot
                .entries
                .iter()
                .find(|entry| entry.parent == shell && entry.exe.ends_with("/sleep"))
                .cloned();
            let shell_entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.pid == shell)
                .cloned();
            if let (Some(shell_entry), Some(sleeper)) = (shell_entry, sleeper) {
                assert_eq!(
                    shell_entry.parent, tree.helper_pid,
                    "the shell is a direct child of the subreaper"
                );
                seen = vec![shell_entry, sleeper];
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the stubborn tree never appeared: {:?}",
                snapshot.entries
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    if let Err(panic) = attempt {
        eprintln!("stubborn cleanup after the failure: {cleanup:?}");
        std::panic::resume_unwind(panic);
    }
    let report = cleanup.expect("subreaper cleanup protocol");
    assert!(report.ok(), "stubborn cleanup unproven: {report:?}");
    for entry in &seen {
        let outcome = report
            .outcome(entry.pid, &entry.start)
            .expect("the stubborn process is in the cleanup report");
        assert_eq!(
            outcome, "killed",
            "{} must be killed by the fallback: {report:?}",
            entry.exe
        );
        process_absent(entry.pid, &entry.start, Duration::from_secs(5))
            .expect("stubborn process is absent");
    }
}

/// A worker whose import blocks on a gated fetch that never carries a PID:
/// Deno exists, but no PID beacon ever arrives. `fail` forces the failure; the
/// whole tree must still be gone, with its absence proven, before returning.
fn hanging_prewarm_cleanup(fail: fn(&mut SubreaperTree, &ImportWitness), crash_first: bool) {
    let fx = Fixture::new();
    let witness = ImportWitness::new_with_gate(true);
    let deno = real_deno();
    let worker = fx.root.path().join("workers/app");
    let address = witness.address;
    fs::write(worker.join("index.ts"), format!(
        "await fetch('http://{address}/hold');\nexport default {{ fetch() {{ return new Response('ok'); }} }};\n"
    )).unwrap();
    fs::write(worker.join("manifest.yaml"), format!(
        "name: app\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\nminProcesses: 1\nmaxProcesses: 1\nttl: 60s\ntimeout: 60s\nallowNet: ['{address}']\n"
    )).unwrap();
    let mut cmd = fx.command(true);
    cmd.env("EDGER_DENO_BIN", &deno);
    let mut tree =
        SubreaperTree::start(&cmd, fx.root.path()).expect("start EdgeR under the subreaper helper");
    let deno_exe = fs::canonicalize(&deno).unwrap();
    let helper_pid = tree.helper_pid;
    let mut spawned: Option<Descendant> = None;
    let (attempt, cleanup) = run_guarded(&mut tree, |tree| {
        let found = wait_for_tree_deno(tree, &deno_exe, Duration::from_secs(20));
        spawned = Some(found.clone());
        assert!(
            witness.candidate().is_none(),
            "no PID beacon may exist in this window"
        );
        if crash_first {
            tree.crash_edger().expect("crash EdgeR");
            // The Deno must now be adopted by the subreaper helper, still in
            // the fixture's own tree.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let snapshot = tree.snapshot().expect("subreaper snapshot");
                match snapshot.entries.iter().find(|entry| entry.pid == found.pid && entry.start == found.start) {
                    Some(entry) if entry.parent == helper_pid => break,
                    Some(_) => {}
                    None => panic!("the orphaned Deno left the tree on its own, so the adoption cannot be exercised"),
                }
                assert!(
                    Instant::now() < deadline,
                    "the orphaned Deno was not adopted by the subreaper"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        fail(tree, &witness);
    });
    witness.release();
    let panic = attempt.expect_err("the scenario must fail on purpose");
    let text = panic_text(panic.as_ref());
    assert!(
        text.starts_with("forced"),
        "unexpected failure instead of the forced one: {text}"
    );
    let report = cleanup.expect("subreaper cleanup protocol");
    assert!(report.ok(), "forced-failure cleanup unproven: {report:?}");
    let found = spawned.expect("the Deno was seen in the tree");
    let outcome = report
        .outcome(found.pid, &found.start)
        .expect("the Deno is in the cleanup report");
    assert!(
        ["exited", "killed", "gone"].contains(&outcome),
        "unexpected outcome {outcome}"
    );
    if crash_first {
        // Adoption was observed above (parent == helper); the orphan then ends
        // by itself or by the subreaper's fallback, never by vanishing from the tree.
        assert!(
            outcome == "exited" || outcome == "killed",
            "adopted orphan outcome {outcome}: {report:?}"
        );
    }
    process_absent(found.pid, &found.start, Duration::from_secs(5))
        .expect("Deno is absent after the forced failure");
}

#[test]
#[ignore = "requires the real Deno 2.9.8 executable named by EDGER_DENO_BIN"]
fn timeout_before_the_pid_beacon_still_cleans_edger_and_deno() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    hanging_prewarm_cleanup(
        |tree, witness| {
            let error = wait_candidate(witness, tree, Duration::from_millis(300))
                .expect_err("no beacon is ever sent");
            panic!("forced timeout before the beacon: {error}");
        },
        false,
    );
}

#[test]
#[ignore = "requires the real Deno 2.9.8 executable named by EDGER_DENO_BIN"]
fn panic_after_spawn_still_cleans_edger_and_deno() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    hanging_prewarm_cleanup(|_, _| panic!("forced panic after the Deno spawn"), false);
}

#[test]
#[ignore = "requires the real Deno 2.9.8 executable named by EDGER_DENO_BIN"]
fn edger_crash_leaves_an_adopted_deno_that_the_subreaper_still_cleans() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    hanging_prewarm_cleanup(|_, _| panic!("forced failure after the EdgeR crash"), true);
}

#[test]
#[ignore = "requires the real Deno 2.9.8 executable named by EDGER_DENO_BIN"]
fn invalid_boot_never_prewarms_or_imports_the_canary() {
    let _tcp_fixture_guard = tcp_fixture_test_guard();
    let fx = Fixture::new();
    let witness = ImportWitness::new();
    let deno = std::env::var_os("EDGER_DENO_BIN")
        .expect("EDGER_DENO_BIN must name the real Deno executable for this test");
    let address = witness.address;
    let script = fx.root.path().join("workers/app/index.ts");
    fs::write(&script, format!(
        "await fetch('http://{address}/');\nexport default {{ fetch() {{ return new Response('ok'); }} }};\n"
    )).unwrap();
    fs::write(fx.root.path().join("workers/app/manifest.yaml"), format!(
        "name: app\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\nminProcesses: 1\nttl: 60s\nallowNet: ['{address}']\n"
    )).unwrap();

    // Positive control uses the real Deno executable and the exact canary
    // module, without starting an extra server or a persistent Deno worker.
    let mut direct = Command::new(&deno);
    direct
        .env_clear()
        .current_dir(fx.root.path())
        .env("HOME", fx.root.path().join("home"))
        .env("USERPROFILE", fx.root.path().join("home"))
        .env("XDG_CONFIG_HOME", fx.root.path().join("home"))
        .env("TMPDIR", fx.root.path())
        .env("PATH", "")
        .env("DENO_NO_UPDATE_CHECK", "1")
        .env("DENO_DIR", fx.root.path().join("deno-control-cache"))
        .arg("run")
        .arg(format!("--allow-net={address}"))
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    with_child(&mut direct, |child| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.poll().expect("Deno try_wait failed") {
                let mut stderr = String::new();
                child
                    .child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .unwrap();
                assert!(status.success(), "Deno canary failed ({address}): {stderr}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Deno canary timed out: {address}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    assert_eq!(
        witness.hits(),
        1,
        "the real Deno control did not import the canary"
    );

    fs::write(fx.root.path().join("keys/signer"), [3u8; 32]).unwrap();
    let mut invalid = fx.command(true);
    invalid.env("EDGER_DENO_BIN", deno);
    expect_boot_rejection(
        &fx,
        &mut invalid,
        "the signer master equals the app secrets master",
    );
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        assert_eq!(
            witness.hits(),
            1,
            "invalid boot imported/prewarmed the canary"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
