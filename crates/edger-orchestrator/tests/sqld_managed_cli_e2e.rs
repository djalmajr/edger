#![cfg(target_os = "linux")]

use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    /// Set only when `crash` observed the SIGKILL it sent.
    crashed: bool,
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
            crashed: false,
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

    fn stop(&mut self) -> io::Result<()> {
        self.stop_with(false)
    }

    /// SIGKILL of the child itself after its identity is re-proven; the PID
    /// is ours and unreaped, so it cannot be reused. Success means the child
    /// was alive when asked AND its observed termination is exactly SIGKILL;
    /// a child that already exited (whatever the reason) is an error, and so
    /// is any other termination status. Returns the observed status.
    fn crash(&mut self) -> io::Result<ExitStatus> {
        use std::os::unix::process::ExitStatusExt;
        if let Some(status) = self.poll()? {
            return Err(io::Error::other(format!(
                "child {} had already exited before the crash command: {status}",
                self.pid
            )));
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
            if let Some(status) = self.poll()? {
                return if status.signal() == Some(9) {
                    self.crashed = true;
                    Ok(status)
                } else {
                    Err(io::Error::other(format!(
                        "child {} ended with {status} instead of SIGKILL",
                        self.pid
                    )))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("child survived SIGKILL"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `allow_term` accepts a child that dies of the SIGTERM sent here: EdgeR
    /// installs its handler only after boot, so a stop during boot ends it by
    /// the default disposition.
    fn stop_with(&mut self, allow_term: bool) -> io::Result<()> {
        use std::os::unix::process::ExitStatusExt;
        if let Some(status) = self.poll()? {
            // Already gone: that is a clean stop only if `crash` proved the
            // SIGKILL, or the status is a success (or a SIGTERM when allowed).
            return if self.crashed
                || status.success()
                || (allow_term && status.signal() == Some(15))
            {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "child {} had already ended unexpectedly: {status}",
                    self.pid
                )))
            };
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
/// Child mode of the same `#[ignore]`d test: the value must start with
/// `CHILD_SENTINEL` (`block` or `exit:<code>` follows). Without it the test
/// never behaves as a child.
const CHILD_ENV: &str = "EDGER_TEST_CRASH_CHILD";
const CHILD_SENTINEL: &str = "edger-crash-child-v1:";

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
            "crash" => {
                match edger.crash() {
                    Ok(status) => say(format!("crashed {status}")),
                    Err(error) => say(format!("problem crash failed: {}", one_line(error))),
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

/// Child of the crash-helper tests: this very test executable, so the tests
/// need no external binary. `block` waits until it is killed (bounded, so a
/// leaked child still ends by itself); `exit:<code>` leaves at once.
fn crash_child(mode: &str) {
    match mode {
        "block" => {
            for _ in 0..300 {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        other => match other.strip_prefix("exit:").map(str::parse::<i32>) {
            Some(Ok(code)) => std::process::exit(code),
            _ => panic!("unknown crash child mode {other:?}"),
        },
    }
}

/// Re-entered by the parent tests only. Without the sentinel and the spec the
/// parent writes, it does nothing: it never starts anything on its own.
#[test]
#[ignore = "helper process: started by the prewarm tests with their sentinel and spec"]
fn subreaper_helper_process() {
    if let Some(mode) = std::env::var(CHILD_ENV)
        .ok()
        .and_then(|value| value.strip_prefix(CHILD_SENTINEL).map(str::to_owned))
    {
        crash_child(&mode);
        return;
    }
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
}

struct SubreaperTree {
    edger_pid: u32,
    edger_stderr: PathBuf,
    finished: bool,
    helper: OwnedChild,
    helper_stderr: PathBuf,
    input: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    reader: Option<JoinHandle<()>>,
    stray: Arc<Mutex<Vec<String>>>,
}

impl SubreaperTree {
    fn start(command: &Command, root: &std::path::Path, label: &str) -> io::Result<Self> {
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
            root.join(format!("{label}-spec.json")),
            root.join(format!("{label}.stderr")),
            root.join(format!("{label}-helper.stderr")),
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
        let mut tree = Self {
            edger_pid: 0,
            edger_stderr,
            finished: false,
            helper,
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

    /// The helper's proof of the SIGKILL: `crashed signal: 9 (SIGKILL)`.
    fn crash_edger(&mut self) -> io::Result<String> {
        let answer = self.request("crash")?;
        if let Some(line) = answer
            .iter()
            .find(|line| line.starts_with("crashed ") && line.contains("signal: 9"))
        {
            Ok(line.clone())
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

// ---------------------------------------------------------------------------
// EDG-54 / part A: the real `edger` binary, the real SQLD 0.24.32, the real
// Deno and the real Go/WASM artifact, wired only through the environment and
// the root admin API. Every process of the stack (SQLD and every EdgeR run,
// with everything they spawn) lives under its own subreaper helper, so the
// cleanup is proven from the kernel's process tree, never from a host scan.
// ---------------------------------------------------------------------------

const ROOT_KEY: &str = "fixture-root-key";
const ADMIN_KEY: &str = "fixture-admin-key";
const QUOTA: u64 = 16 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

fn signer_key() -> [u8; 32] {
    std::array::from_fn(|i| 0x80 + i as u8)
}

fn vault_key() -> [u8; 32] {
    std::array::from_fn(|i| 0xc0 + i as u8)
}

fn require_file(var: &str) -> PathBuf {
    let value = std::env::var_os(var)
        .unwrap_or_else(|| panic!("{var} must name the real runtime; this test never skips"));
    let path = PathBuf::from(value);
    assert!(path.is_file(), "{var} does not name a file");
    path
}

/// Sources of the Go CRUD fixture whose hashes the provenance manifest must
/// carry (the module is built from exactly these three files).
const CRUD_FIXTURE_SOURCES: [&str; 3] = ["abi.go", "main.go", "go.mod"];

/// The one build command the manifest may declare: the command documented in
/// the header of `tests/fixtures/go-wasm-crud/main.go`.
const CRUD_FIXTURE_BUILD: &str = "GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o go-wasm-crud.wasm .";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Parsed `<module>.provenance`: every record at most once, nothing else.
struct CrudProvenance {
    build: Option<String>,
    hashes: Vec<(String, String)>,
}

/// Strict parser for the provenance manifest. Besides the records only the
/// terminator of the last line is accepted: a blank line anywhere, a line that
/// is neither `<64 lowercase hex>  <name>` nor `build: <command>`, a name with
/// a path, a name outside `allowed`, a repeated name and a repeated `build:`
/// line are all errors.
fn parse_crud_provenance(text: &str, allowed: &[&str]) -> Result<CrudProvenance, String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut parsed = CrudProvenance {
        build: None,
        hashes: Vec::new(),
    };
    if body.is_empty() {
        return Ok(parsed);
    }
    for (index, line) in body.split('\n').enumerate() {
        let number = index + 1;
        if line.is_empty() {
            return Err(format!("provenance manifest line {number} is blank"));
        }
        if let Some(command) = line.strip_prefix("build: ") {
            if parsed.build.replace(command.to_owned()).is_some() {
                return Err(format!(
                    "provenance manifest has a duplicate `build:` line (line {number})"
                ));
            }
            continue;
        }
        let record = line
            .get(..64)
            .zip(line.get(64..66))
            .zip(line.get(66..))
            .filter(|((hash, separator), name)| {
                hash.bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    && *separator == "  "
                    && !name.is_empty()
            });
        let Some(((hash, _), name)) = record else {
            return Err(format!(
                "provenance manifest line {number} is not `<64 hex>  <name>` or `build: <command>`: `{line}`"
            ));
        };
        if name.contains('/') || name.contains('\\') {
            return Err(format!(
                "provenance manifest line {number} names `{name}` with a path"
            ));
        }
        if !allowed.contains(&name) {
            return Err(format!(
                "provenance manifest line {number} names unexpected `{name}`"
            ));
        }
        if parsed.hashes.iter().any(|(known, _)| known == name) {
            return Err(format!(
                "provenance manifest has a duplicate record for {name} (line {number})"
            ));
        }
        parsed.hashes.push((name.to_owned(), hash.to_owned()));
    }
    Ok(parsed)
}

/// Binds a supplied Go module to the sources in the worktree: the manifest
/// `<module>.provenance` has exactly one `sha256sum` line per fixture source
/// and one for the module itself, plus exactly one `build: <command>` line
/// equal to `CRUD_FIXTURE_BUILD`, and nothing else (see
/// `parse_crud_provenance`). Every hash is recomputed here; a missing
/// manifest, a missing, repeated, unexpected or divergent record, or another
/// build command is an error. On success it returns the module bytes it
/// hashed, so the caller runs what was checked.
fn verify_crud_fixture_provenance(
    module: &std::path::Path,
    sources: &std::path::Path,
) -> Result<Vec<u8>, String> {
    let mut manifest_name = module.as_os_str().to_owned();
    manifest_name.push(".provenance");
    let manifest_path = PathBuf::from(manifest_name);
    let manifest = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "provenance manifest {} is unreadable: {error}",
            manifest_path.display()
        )
    })?;
    let module_name = module
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no usable file name", module.display()))?;
    let mut allowed: Vec<&str> = CRUD_FIXTURE_SOURCES.to_vec();
    allowed.push(module_name);
    let parsed = parse_crud_provenance(&manifest, &allowed)?;
    let mut expected: Vec<(String, String)> = Vec::new();
    for name in CRUD_FIXTURE_SOURCES {
        let bytes = fs::read(sources.join(name))
            .map_err(|error| format!("fixture source {name} is unreadable: {error}"))?;
        expected.push((name.to_owned(), sha256_hex(&bytes)));
    }
    let bytes = fs::read(module)
        .map_err(|error| format!("module {} is unreadable: {error}", module.display()))?;
    expected.push((module_name.to_owned(), sha256_hex(&bytes)));
    for (name, digest) in &expected {
        match parsed.hashes.iter().find(|(known, _)| known == name) {
            None => return Err(format!("provenance manifest has no line for {name}")),
            Some((_, recorded)) if recorded != digest => {
                return Err(format!(
                    "provenance for {name} diverges: manifest has {recorded}, the file hashes to {digest}"
                ));
            }
            Some(_) => {}
        }
    }
    match parsed.build.as_deref() {
        None => Err("provenance manifest has no `build:` line".to_owned()),
        Some(CRUD_FIXTURE_BUILD) => Ok(bytes),
        Some(other) => Err(format!(
            "provenance build command diverges: manifest has `{other}`, the test expects `{CRUD_FIXTURE_BUILD}`"
        )),
    }
}

fn crud_fixture_sources() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go-wasm-crud")
}

#[test]
fn the_crud_fixture_provenance_binds_the_module_to_the_sources() {
    let dir = tempfile::tempdir().unwrap();
    let sources = dir.path().join("sources");
    fs::create_dir(&sources).unwrap();
    for name in CRUD_FIXTURE_SOURCES {
        fs::write(sources.join(name), format!("content of {name}\n")).unwrap();
    }
    let module = dir.path().join("go-wasm-crud.wasm");
    fs::write(&module, b"\0asm module bytes").unwrap();
    let manifest = dir.path().join("go-wasm-crud.wasm.provenance");
    // The records of a valid manifest, in order: three sources, the module and
    // the build line.
    let valid_lines = |build: &str| -> Vec<String> {
        let mut lines = Vec::new();
        for name in CRUD_FIXTURE_SOURCES {
            let bytes = fs::read(sources.join(name)).unwrap();
            lines.push(format!("{}  {name}", sha256_hex(&bytes)));
        }
        let bytes = fs::read(&module).unwrap();
        lines.push(format!("{}  go-wasm-crud.wasm", sha256_hex(&bytes)));
        lines.push(format!("build: {build}"));
        lines
    };
    let write_manifest = |build: &str| {
        fs::write(&manifest, valid_lines(build).join("\n") + "\n").unwrap();
    };
    // Writes the given text as the manifest and returns the refusal.
    let refusal = |text: String| -> String {
        fs::write(&manifest, text).unwrap();
        verify_crud_fixture_provenance(&module, &sources).unwrap_err()
    };
    let stray_hash = "0".repeat(64);

    assert!(
        verify_crud_fixture_provenance(&module, &sources)
            .unwrap_err()
            .contains("is unreadable"),
        "a missing manifest must be refused"
    );

    write_manifest(CRUD_FIXTURE_BUILD);
    assert!(verify_crud_fixture_provenance(&module, &sources).is_ok());

    for name in CRUD_FIXTURE_SOURCES {
        let original = fs::read(sources.join(name)).unwrap();
        fs::write(sources.join(name), b"changed source\n").unwrap();
        let error = verify_crud_fixture_provenance(&module, &sources).unwrap_err();
        assert!(
            error.contains(&format!("provenance for {name} diverges")),
            "a changed {name} must be refused: {error}"
        );
        fs::write(sources.join(name), original).unwrap();
        assert!(verify_crud_fixture_provenance(&module, &sources).is_ok());
    }

    fs::write(&module, b"\0asm another module").unwrap();
    let error = verify_crud_fixture_provenance(&module, &sources).unwrap_err();
    assert!(
        error.contains("provenance for go-wasm-crud.wasm diverges"),
        "a changed module must be refused: {error}"
    );
    fs::write(&module, b"\0asm module bytes").unwrap();
    assert!(verify_crud_fixture_provenance(&module, &sources).is_ok());

    write_manifest("go build -o go-wasm-crud.wasm .");
    let error = verify_crud_fixture_provenance(&module, &sources).unwrap_err();
    assert!(
        error.contains("build command diverges"),
        "another build command must be refused: {error}"
    );

    // Each omission starts from a valid manifest and removes exactly one record.
    let names = ["abi.go", "main.go", "go.mod", "go-wasm-crud.wasm"];
    for (index, name) in names.iter().enumerate() {
        let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
        lines.remove(index);
        assert_eq!(
            refusal(lines.join("\n") + "\n"),
            format!("provenance manifest has no line for {name}"),
            "a manifest without the {name} record"
        );
    }
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.remove(4);
    assert_eq!(
        refusal(lines.join("\n") + "\n"),
        "provenance manifest has no `build:` line"
    );
    assert_eq!(
        refusal(String::new()),
        "provenance manifest has no line for abi.go"
    );

    // Repeated records are refused, whatever the second hash says.
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.push(format!("{stray_hash}  main.go"));
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("duplicate record for main.go (line 6)"),
        "main.go repeated with a conflicting hash: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.push(lines[1].clone());
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("duplicate record for main.go (line 6)"),
        "main.go repeated with the same hash: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.push(lines[3].clone());
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("duplicate record for go-wasm-crud.wasm (line 6)"),
        "the module repeated: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.push(format!("build: {CRUD_FIXTURE_BUILD}"));
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("duplicate `build:` line (line 6)"),
        "build repeated: {error}"
    );

    // Names the manifest may not carry.
    for stray in ["sub/main.go", "/abs/main.go", "../main.go", "sub\\main.go"] {
        let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
        lines.push(format!("{stray_hash}  {stray}"));
        let error = refusal(lines.join("\n") + "\n");
        assert!(
            error.contains("line 6 names") && error.contains("with a path"),
            "an extra record named {stray}: {error}"
        );
    }
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.push(format!("{stray_hash}  other.go"));
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("line 6 names unexpected `other.go`"),
        "an extra record with another name: {error}"
    );

    // Anything else is content the parser must refuse.
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.push("stray trailing text".to_owned());
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("line 6 is not `<64 hex>  <name>`"),
        "a stray line at the end: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines.insert(2, String::new());
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("line 3 is blank"),
        "a blank line in the middle: {error}"
    );
    let error = refusal(valid_lines(CRUD_FIXTURE_BUILD).join("\n") + "\n\n");
    assert!(
        error.contains("line 6 is blank"),
        "a blank line after the terminator: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines[0] = lines[0].replacen("  ", " ", 1);
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("line 1 is not `<64 hex>  <name>`"),
        "a single-space separator: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines[0] = lines[0].to_uppercase();
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("line 1 is not `<64 hex>  <name>`"),
        "uppercase hex: {error}"
    );
    let mut lines = valid_lines(CRUD_FIXTURE_BUILD);
    lines[0].remove(0);
    let error = refusal(lines.join("\n") + "\n");
    assert!(
        error.contains("line 1 is not `<64 hex>  <name>`"),
        "a hash one digit short: {error}"
    );
    let error = refusal(valid_lines(CRUD_FIXTURE_BUILD).join("\r\n") + "\r\n");
    assert!(
        error.contains("line 1 names unexpected `abi.go\r`"),
        "carriage returns: {error}"
    );

    // The terminator of the last line may be present or absent, nothing more.
    fs::write(&manifest, valid_lines(CRUD_FIXTURE_BUILD).join("\n")).unwrap();
    assert!(verify_crud_fixture_provenance(&module, &sources).is_ok());
    write_manifest(CRUD_FIXTURE_BUILD);
    assert!(verify_crud_fixture_provenance(&module, &sources).is_ok());
}

#[test]
#[ignore = "requires the test-only Go CRUD artifact (EDGER_GO_WASM_CRUD_FIXTURE) and its .provenance manifest"]
fn the_supplied_crud_fixture_matches_its_provenance_manifest() {
    let module = require_file("EDGER_GO_WASM_CRUD_FIXTURE");
    if let Err(error) = verify_crud_fixture_provenance(&module, &crud_fixture_sources()) {
        panic!("EDGER_GO_WASM_CRUD_FIXTURE is not built from the worktree sources: {error}");
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

// ───────────────────────────── raw HTTP/1.0 client ─────────────────────────────

#[derive(Debug)]
struct Reply {
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    status: u16,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", self.text()))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn dechunk(raw: &[u8]) -> io::Result<Vec<u8>> {
    let bad = || io::Error::other("invalid chunked body");
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let eol = rest
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .ok_or_else(bad)?;
        let size_text = std::str::from_utf8(&rest[..eol]).map_err(|_| bad())?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| bad())?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err(bad());
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// HTTP/1.0 with `Connection: close`: the servers answer without chunking and
/// the body ends at EOF.
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<Reply> {
    http_with_timeout(port, method, path, headers, body, Duration::from_secs(90))
}

fn http_with_timeout(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    read_timeout: Duration,
) -> io::Result<Reply> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(read_timeout))?;
    let mut head = format!("{method} {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() || method == "POST" || method == "PUT" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|quad| quad == b"\r\n\r\n")
        .ok_or_else(|| io::Error::other(format!("no HTTP head in {} bytes", raw.len())))?;
    let head_text = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head_text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other("invalid status line"))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    }) {
        body = dechunk(&body)?;
    }
    Ok(Reply {
        body,
        headers,
        status,
    })
}

fn pct(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)
}

fn zip_package(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    for (name, content) in files {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

// ───────────────────────────── SQL through the pipeline ─────────────────────────────

/// One `execute` of a Hrana pipeline, whoever sent it (an app through the
/// guard or the test straight to SQLD).
#[derive(Debug)]
struct Sql {
    error: Option<String>,
    raw: String,
    rows: Vec<Vec<String>>,
    status: u64,
}

impl Sql {
    fn parse(status: u64, text: &str) -> Self {
        let mut out = Sql {
            error: None,
            raw: text.to_owned(),
            rows: Vec::new(),
            status,
        };
        if status != 200 {
            return out;
        }
        let value: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("SQLD answered 200 with non-JSON ({error}): {text}"));
        let first = &value["results"][0];
        match first["type"].as_str() {
            Some("ok") => {
                for row in first["response"]["result"]["rows"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                {
                    out.rows.push(
                        row.as_array()
                            .unwrap_or(&Vec::new())
                            .iter()
                            .map(|cell| match cell.get("value") {
                                Some(serde_json::Value::String(text)) => text.clone(),
                                Some(other) => other.to_string(),
                                None => "NULL".to_owned(),
                            })
                            .collect(),
                    );
                }
            }
            Some("error") => out.error = Some(first["error"].to_string()),
            other => panic!("unexpected pipeline entry {other:?}: {text}"),
        }
        out
    }

    fn ok(&self) -> bool {
        self.status == 200 && self.error.is_none()
    }

    /// SQLite answered 200 with the exact "no such table" error for `table`.
    fn missing_table(&self, table: &str) -> bool {
        self.status == 200
            && self
                .error
                .as_deref()
                .is_some_and(|error| error.contains(&format!("no such table: {table}")))
    }

    /// SQLD refused the credential itself (not a malformed request).
    fn auth_refused(&self) -> bool {
        self.status == 401 || self.status == 403
    }

    fn cell(&self) -> &str {
        self.rows
            .first()
            .and_then(|row| row.first())
            .map(String::as_str)
            .unwrap_or_else(|| panic!("no cell in {self:?}"))
    }
}

fn pipeline_body(sql: &str) -> Vec<u8> {
    serde_json::json!({"requests": [{"type": "execute", "stmt": {"sql": sql}}, {"type": "close"}]})
        .to_string()
        .into_bytes()
}

// ───────────────────────────── the stack ─────────────────────────────

const DENO_APP: &str = r#"
const db = Deno.env.get("SQLD_URL") ?? "";
const token = Deno.env.get("SQLD_TOKEN");
async function pipeline(sql, headers) {
  const r = await fetch(new URL("/v2/pipeline", db), {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify({ requests: [{ type: "execute", stmt: { sql } }, { type: "close" }] }),
  });
  return { status: r.status, err: r.headers.get("x-edger-egress-error"), text: await r.text() };
}
export default {
  async fetch(req) {
    const u = new URL(req.url);
    const route = u.pathname.split("/").filter(Boolean).pop() ?? "";
    const sql = u.searchParams.get("sql") ?? "SELECT 1";
    if (route === "state") {
      return Response.json({
        pid: Deno.pid,
        url: db,
        tokenPresent: token !== undefined,
        socket: Deno.env.get("EDGER_EGRESS_SOCKET") !== undefined,
      });
    }
    if (route === "tok") return Response.json({ token: token ?? null });
    if (!db) return Response.json({ status: 0, err: "no-sqld-url", text: "" });
    try {
      if (route === "q") return Response.json(await pipeline(sql, {}));
      if (route === "q-guest") {
        return Response.json(await pipeline(sql, {
          authorization: "Bearer " + (u.searchParams.get("tok") ?? ""),
          "x-namespace-bin": u.searchParams.get("ns") ?? "",
        }));
      }
    } catch (e) {
      return Response.json({ status: 0, err: e.name, text: "" });
    }
    return new Response("not found", { status: 404 });
  },
};
"#;

fn deno_manifest(name: &str, allow_net: &str, extra: &str) -> String {
    format!(
        "name: \"{name}\"\nversion: \"1.0.0\"\nkind: fetch\nentrypoint: index.ts\ntimeout: 30s\nallowNet: {allow_net}\n{extra}"
    )
}

/// Every `*.stderr` log directly under `dir`, searched for each canary text and
/// for the raw signer/vault key bytes. Returns the number of logs read and one
/// line per leak (never the canary itself).
fn scan_logs(dir: &std::path::Path, canaries: &[String]) -> (usize, Vec<String>) {
    let mut scanned = 0;
    let mut leaks = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "stderr") {
            continue;
        }
        let bytes = fs::read(&path).unwrap();
        scanned += 1;
        for canary in canaries {
            if bytes
                .windows(canary.len())
                .any(|window| window == canary.as_bytes())
            {
                leaks.push(format!(
                    "a {}-byte canary is in {}",
                    canary.len(),
                    path.display()
                ));
            }
        }
        for key in [signer_key(), vault_key()] {
            if bytes.windows(32).any(|window| window == key) {
                leaks.push(format!("raw key bytes are in {}", path.display()));
            }
        }
    }
    (scanned, leaks)
}

#[test]
fn the_canary_scan_flags_a_planted_leak_and_passes_a_clean_log() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("clean.stderr"),
        "nothing to see
",
    )
    .unwrap();
    let canaries = vec!["CANARY-TOKEN-91".to_owned(), ADMIN_KEY.to_owned()];
    let (scanned, leaks) = scan_logs(dir.path(), &canaries);
    assert_eq!((scanned, leaks.len()), (1, 0));
    fs::write(
        dir.path().join("edger-9.stderr"),
        "log line CANARY-TOKEN-91 more
",
    )
    .unwrap();
    let mut raw = b"prefix".to_vec();
    raw.extend_from_slice(&vault_key());
    fs::write(dir.path().join("sqld.stderr"), raw).unwrap();
    let (scanned, leaks) = scan_logs(dir.path(), &canaries);
    assert_eq!(scanned, 3);
    assert_eq!(leaks.len(), 2, "{leaks:?}");
}

// ───────────────────────────── transparent HTTP proxy with gates ─────────────────────────────
//
// A proxy of the test sits between the `edger` processes and one SQLD port.
// It forwards every request (one request per connection, `Connection: close`
// toward SQLD) and records `METHOD path -> status`. A `Gate` holds the matching
// request deterministically, either before it reaches SQLD or after SQLD has
// answered and before the answer goes back, until the test releases it
// (`Proceed`, or `Abandon` = the connection is dropped without forwarding /
// replying) or a barrier of `parties` requests is complete.

type Matcher = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GatePoint {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Release {
    Proceed,
    Abandon,
}

struct GateState {
    decision: Option<Release>,
    held: usize,
    timed_out: bool,
}

struct Gate {
    cv: std::sync::Condvar,
    matcher: Matcher,
    parties: usize,
    patience: Duration,
    point: GatePoint,
    state: Mutex<GateState>,
}

impl Gate {
    /// `parties` held requests release the gate by themselves (a barrier);
    /// `usize::MAX` means only `release` opens it. `patience` bounds every hold.
    fn new(
        point: GatePoint,
        parties: usize,
        patience: Duration,
        matcher: impl Fn(&str, &str) -> bool + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            cv: std::sync::Condvar::new(),
            matcher: Box::new(matcher),
            parties,
            patience,
            point,
            state: Mutex::new(GateState {
                decision: None,
                held: 0,
                timed_out: false,
            }),
        })
    }

    fn is_open(&self) -> bool {
        self.state.lock().unwrap().decision.is_some()
    }

    fn hold(&self) -> Release {
        let deadline = Instant::now() + self.patience;
        let mut state = self.state.lock().unwrap();
        state.held += 1;
        if state.held >= self.parties && state.decision.is_none() {
            state.decision = Some(Release::Proceed);
        }
        self.cv.notify_all();
        loop {
            if let Some(decision) = state.decision {
                return decision;
            }
            let now = Instant::now();
            if now >= deadline {
                state.decision = Some(Release::Proceed);
                state.timed_out = true;
                self.cv.notify_all();
                return Release::Proceed;
            }
            state = self.cv.wait_timeout(state, deadline - now).unwrap().0;
        }
    }

    /// Blocks until `count` requests are being held (or the timeout).
    fn wait_held(&self, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        while state.held < count {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self.cv.wait_timeout(state, deadline - now).unwrap().0;
        }
        true
    }

    fn release(&self, how: Release) {
        let mut state = self.state.lock().unwrap();
        if state.decision.is_none() {
            state.decision = Some(how);
        }
        self.cv.notify_all();
    }

    /// The barrier filled up on its own (no patience timeout, no manual release).
    fn barrier_complete(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.held >= self.parties && !state.timed_out
    }
}

struct ProxyInner {
    forwarded: Mutex<Vec<(Option<String>, Option<String>)>>,
    gates: Mutex<Vec<Arc<Gate>>>,
    stop: std::sync::atomic::AtomicBool,
    trace: Mutex<Vec<String>>,
    upstream: u16,
}

struct Proxy {
    thread: Option<std::thread::JoinHandle<()>>,
    inner: Arc<ProxyInner>,
    port: u16,
}

struct ProxyRequest {
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    method: String,
    path: String,
}

fn read_proxy_request(stream: &mut TcpStream) -> io::Result<Option<ProxyRequest>> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let split = loop {
        if let Some(at) = raw.windows(4).position(|quad| quad == b"\r\n\r\n") {
            break at;
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return if raw.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::other("connection closed inside a request head"))
            };
        }
        raw.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (
        first.next().unwrap_or("").to_owned(),
        first.next().unwrap_or("").to_owned(),
    );
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    assert!(
        !headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
        }),
        "the test proxy does not support chunked request bodies ({method} {path})"
    );
    let length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = raw[split + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::other("connection closed inside a request body"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Ok(Some(ProxyRequest {
        body,
        headers,
        method,
        path,
    }))
}

/// The proxy serves one request per connection: tell the client so, or a
/// pooled connection would be reused after it is closed.
fn close_delimited(response: &[u8]) -> Vec<u8> {
    let Some(split) = response.windows(4).position(|quad| quad == b"\r\n\r\n") else {
        return response.to_vec();
    };
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let mut out = String::new();
    for (index, line) in head.split("\r\n").enumerate() {
        let lower = line.to_ascii_lowercase();
        if index > 0 && (lower.starts_with("connection:") || lower.starts_with("keep-alive:")) {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&response[split + 4..]);
    bytes
}

fn proxy_connection(inner: &ProxyInner, mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(120)))?;
    let Some(request) = read_proxy_request(&mut client)? else {
        return Ok(());
    };
    let line = format!("{} {}", request.method, request.path);
    let gate = inner
        .gates
        .lock()
        .unwrap()
        .iter()
        .find(|gate| !gate.is_open() && (gate.matcher)(&request.method, &request.path))
        .cloned();
    if let Some(gate) = gate.as_ref().filter(|gate| gate.point == GatePoint::Before) {
        if gate.hold() == Release::Abandon {
            inner
                .trace
                .lock()
                .unwrap()
                .push(format!("{line} -> not-forwarded"));
            return Ok(());
        }
    }
    let mut upstream = TcpStream::connect((Ipv4Addr::LOCALHOST, inner.upstream))?;
    upstream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut head = format!("{} {} HTTP/1.1\r\n", request.method, request.path);
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if !["connection", "proxy-connection", "keep-alive"].contains(&lower.as_str()) {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str("Connection: close\r\n\r\n");
    upstream.write_all(head.as_bytes())?;
    upstream.write_all(&request.body)?;
    let header_hash = |name: &str| {
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| sha256_hex(value.as_bytes()))
    };
    inner
        .forwarded
        .lock()
        .unwrap()
        .push((header_hash("Authorization"), header_hash("x-namespace-bin")));
    let mut response = Vec::new();
    upstream.read_to_end(&mut response)?;
    let status = String::from_utf8_lossy(&response[..response.len().min(16)])
        .split_whitespace()
        .nth(1)
        .unwrap_or("?")
        .to_owned();
    if let Some(gate) = gate.as_ref().filter(|gate| gate.point == GatePoint::After) {
        // SQLD has answered; mark it before holding the answer back.
        inner
            .trace
            .lock()
            .unwrap()
            .push(format!("{line} => {status} held"));
        if gate.hold() == Release::Abandon {
            inner
                .trace
                .lock()
                .unwrap()
                .push(format!("{line} -> {status} not-delivered"));
            return Ok(());
        }
    }
    inner
        .trace
        .lock()
        .unwrap()
        .push(format!("{line} -> {status}"));
    client.write_all(&close_delimited(&response))?;
    let _ = client.shutdown(std::net::Shutdown::Write);
    Ok(())
}

impl Proxy {
    fn start(upstream: u16) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let inner = Arc::new(ProxyInner {
            forwarded: Mutex::new(Vec::new()),
            gates: Mutex::new(Vec::new()),
            stop: std::sync::atomic::AtomicBool::new(false),
            trace: Mutex::new(Vec::new()),
            upstream,
        });
        let shared = Arc::clone(&inner);
        let thread = std::thread::spawn(move || {
            let mut connections = Vec::new();
            for stream in listener.incoming() {
                if shared.stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let connection = Arc::clone(&shared);
                connections.push(std::thread::spawn(move || {
                    if let Err(error) = proxy_connection(&connection, stream) {
                        connection
                            .trace
                            .lock()
                            .unwrap()
                            .push(format!("proxy-error {}", one_line(error)));
                    }
                }));
            }
            for connection in connections {
                let _ = connection.join();
            }
        });
        Self {
            inner,
            port,
            thread: Some(thread),
        }
    }

    fn forwarded(&self) -> Vec<(Option<String>, Option<String>)> {
        self.inner.forwarded.lock().unwrap().clone()
    }

    fn add_gate(&self, gate: &Arc<Gate>) {
        self.inner.gates.lock().unwrap().push(Arc::clone(gate));
    }

    fn trace(&self) -> Vec<String> {
        self.inner.trace.lock().unwrap().clone()
    }

    /// Entries of the trace that start with `prefix`.
    fn count(&self, prefix: &str) -> usize {
        self.trace()
            .iter()
            .filter(|entry| entry.starts_with(prefix))
            .count()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.inner
            .stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for gate in self.inner.gates.lock().unwrap().iter() {
            gate.release(Release::Abandon);
        }
        // Wake the accept loop so the thread ends.
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn is_create(method: &str, path: &str) -> bool {
    method == "POST" && path.starts_with("/v1/namespaces/edger_") && path.ends_with("/create")
}

fn is_probe(path: &str) -> bool {
    path == "/v1/namespaces/edger_probe/config"
}

fn is_proof_get(method: &str, path: &str) -> bool {
    method == "GET"
        && path.starts_with("/v1/namespaces/edger_")
        && path.ends_with("/config")
        && !is_probe(path)
}

// ───────────────────────────── ledger, namespaces, tokens ─────────────────────────────

/// `edger_` + sha256 of the registry's owner encoding (no RBAC namespace);
/// the same derivation as `sqld_registry::derive_sql_namespace`.
fn derive_namespace(worker: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"edger.sqld-registry.ns.v1\0");
    hasher.update((worker.len() as u32).to_be_bytes());
    hasher.update(worker.as_bytes());
    hasher.update([0u8]);
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("edger_{hex}")
}

#[derive(Debug, Clone)]
struct LedgerRow {
    public_key: [u8; 32],
    quota: i64,
    sql_namespace: String,
    worker: String,
}

fn jwt_claims(token: &str) -> serde_json::Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).expect("a JWT has three parts");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("JWT payload is base64url");
    serde_json::from_slice(&bytes).expect("JWT payload is JSON")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct ExtraEdger {
    label: String,
    tree: SubreaperTree,
}

struct Stack {
    admin_proxy: Option<Proxy>,
    extras: Vec<ExtraEdger>,
    user_proxy: Option<Proxy>,
    admin_port: u16,
    canaries: Vec<String>,
    deno: PathBuf,
    edger: Option<SubreaperTree>,
    edger_logs: Vec<PathBuf>,
    port: u16,
    reports: Vec<(String, CleanupReport)>,
    runs: u32,
    sqld: Option<SubreaperTree>,
    sqld_bin: PathBuf,
    budget: u64,
    user_port: u16,
    root: tempfile::TempDir,
}

fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// Three DISTINCT free ports for one stack. All probe listeners stay open
/// until the three ports are known and only then are released: three calls to
/// `free_port` could get the same port back from the kernel, because each
/// probe is closed before the next one binds.
fn free_ports() -> [u16; 3] {
    let probes: Vec<TcpListener> = (0..3)
        .map(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap())
        .collect();
    let ports: Vec<u16> = probes
        .iter()
        .map(|probe| probe.local_addr().unwrap().port())
        .collect();
    assert!(
        ports[0] != ports[1] && ports[0] != ports[2] && ports[1] != ports[2],
        "the stack ports are distinct"
    );
    [ports[0], ports[1], ports[2]]
}

impl Stack {
    fn new(budget: u64) -> Self {
        let deno = require_file("EDGER_DENO_BIN");
        let sqld_bin = require_file("EDGER_SQLD_BIN");
        let root = tempfile::Builder::new()
            .prefix("edger-cli-e2e-")
            .tempdir()
            .unwrap();
        for dir in [
            "workers",
            "core",
            "state",
            "state/backups",
            "keys",
            "home",
            "sqld",
        ] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        for dir in ["state", "state/backups", "keys", "sqld"] {
            fs::set_permissions(root.path().join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        for (name, content) in [
            ("admin", ADMIN_KEY.as_bytes().to_vec()),
            ("signer", signer_key().to_vec()),
            ("vault", vault_key().to_vec()),
        ] {
            let path = root.path().join("keys").join(name);
            fs::write(&path, content).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("ed25519 pair");
        fs::write(root.path().join("sqld/global.pub"), pair.public_key_pem()).unwrap();
        let mut canaries = vec![ADMIN_KEY.to_owned()];
        for key in [signer_key(), vault_key()] {
            canaries.push(b64(&key));
            canaries.push(key.iter().map(|byte| format!("{byte:02x}")).collect());
        }
        let [admin_port, port, user_port] = free_ports();
        Self {
            admin_port,
            admin_proxy: None,
            extras: Vec::new(),
            user_proxy: None,
            budget,
            canaries,
            deno,
            edger: None,
            edger_logs: Vec::new(),
            port,
            reports: Vec::new(),
            root,
            runs: 0,
            sqld: None,
            sqld_bin,
            user_port,
        }
    }

    /// The data origin EdgeR is configured with (the proxy's when proxied).
    fn user_origin(&self) -> String {
        let port = self.user_proxy.as_ref().map_or(self.user_port, |p| p.port);
        format!("http://127.0.0.1:{port}")
    }

    /// Puts a transparent proxy in front of the SQLD admin and data ports.
    /// Must run before any EdgeR starts.
    fn enable_proxies(&mut self) {
        assert!(self.edger.is_none() && self.admin_proxy.is_none());
        self.admin_proxy = Some(Proxy::start(self.admin_port));
        self.user_proxy = Some(Proxy::start(self.user_port));
    }

    fn admin(&self) -> &Proxy {
        self.admin_proxy.as_ref().expect("proxies are enabled")
    }

    fn data(&self) -> &Proxy {
        self.user_proxy.as_ref().expect("proxies are enabled")
    }

    fn start_sqld(&mut self) {
        let root = self.root.path();
        let mut cmd = Command::new(&self.sqld_bin);
        cmd.arg("--db-path")
            .arg(root.join("sqld/data.sqld"))
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{}", self.user_port))
            .arg("--admin-listen-addr")
            .arg(format!("127.0.0.1:{}", self.admin_port))
            .arg("--admin-auth-key")
            .arg(ADMIN_KEY)
            .arg("--enable-namespaces")
            .arg("--disable-default-namespace")
            .arg("--auth-jwt-key-file")
            .arg(root.join("sqld/global.pub"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.join("sqld"))
            .env("RUST_LOG", "error")
            .current_dir(root.join("sqld"));
        let mut tree =
            SubreaperTree::start(&cmd, root, "sqld").expect("start SQLD under a subreaper");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(reason) = tree.exited() {
                panic!("sqld exited during startup: {reason}");
            }
            if let Ok(reply) = http(
                self.admin_port,
                "GET",
                "/v1/namespaces/edger_probe/config",
                &[("Authorization", &format!("Basic {ADMIN_KEY}"))],
                b"",
            ) {
                assert_eq!(
                    reply.status,
                    404,
                    "admin gate before ready: {}",
                    reply.text()
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the SQLD admin gate never answered"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        self.sqld = Some(tree);
    }

    fn edger_command(&self) -> Command {
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
            .env("ROOT_API_KEY", ROOT_KEY)
            .env("EDGER_CRON_ENABLED", "false")
            .env("EDGER_DENO_BIN", &self.deno)
            .env(
                "EDGER_SQLD_ADMIN_URL",
                format!(
                    "http://127.0.0.1:{}",
                    self.admin_proxy
                        .as_ref()
                        .map_or(self.admin_port, |p| p.port)
                ),
            )
            .env("EDGER_SQLD_ADMIN_KEY_FILE", root.join("keys/admin"))
            .env("EDGER_SQLD_USER_URL", self.user_origin())
            .env("EDGER_SQLD_REGISTRY_DB", root.join("state/registry.db"))
            .env("EDGER_SQLD_SIGNER_KEY_FILE", root.join("keys/signer"))
            .env("EDGER_SQLD_MANAGED_BUDGET_BYTES", self.budget.to_string())
            .env("EDGER_SQLD_BACKUP_DIR", root.join("state/backups"));
        cmd
    }

    /// Starts an EdgeR run over the SAME directories and ports as the last one.
    fn start_edger(&mut self) {
        assert!(self.edger.is_none(), "an EdgeR run is already live");
        self.runs += 1;
        let label = format!("edger-{}", self.runs);
        let mut tree = SubreaperTree::start(&self.edger_command(), self.root.path(), &label)
            .expect("start EdgeR under a subreaper");
        self.edger_logs
            .push(self.root.path().join(format!("{label}.stderr")));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(reason) = tree.exited() {
                panic!("edger exited before ready: {reason}");
            }
            if let Ok(reply) = http(self.port, "GET", "/ready", &[], b"") {
                if reply.status == 200 {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "edger did not become ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        self.edger = Some(tree);
    }

    fn stop_tree(&mut self, label: &str, tree: SubreaperTree) {
        let mut tree = tree;
        let report = tree.stop().expect("subreaper cleanup protocol");
        self.reports.push((label.to_owned(), report));
    }

    fn stop_edger(&mut self) {
        let tree = self.edger.take().expect("a live EdgeR run");
        let label = format!("edger-{}", self.runs);
        self.stop_tree(&label, tree);
    }

    fn restart_edger(&mut self) {
        self.stop_edger();
        let (label, report) = self.reports.last().expect("report of the stopped run");
        assert!(report.ok(), "{label} cleanup unproven: {report:?}");
        self.start_edger();
    }

    /// Stops everything (EdgeR first), then proves every recorded process is
    /// absent and that no canary reached any log.
    fn finish(&mut self) -> Result<(), String> {
        while let Some(extra) = self.extras.pop() {
            let label = extra.label.clone();
            self.stop_tree(&label, extra.tree);
        }
        if self.edger.is_some() {
            self.stop_edger();
        }
        if let Some(tree) = self.sqld.take() {
            self.stop_tree("sqld", tree);
        }
        self.admin_proxy = None;
        self.user_proxy = None;
        let mut problems = Vec::new();
        for (label, report) in &self.reports {
            if !report.ok() {
                problems.push(format!("{label}: cleanup unproven: {report:?}"));
            }
            for (pid, outcome, start, exe) in &report.entries {
                if !["exited", "killed", "gone"].contains(&outcome.as_str()) {
                    problems.push(format!("{label}: {pid} {exe} outcome {outcome}"));
                }
                if let Err(error) = process_absent(*pid, start, Duration::from_secs(5)) {
                    problems.push(format!("{label}: {pid} {exe}: {error}"));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    /// No canary may appear in any EdgeR or helper log of this stack.
    fn assert_logs_hold_no_canary(&self) {
        let (scanned, leaks) = scan_logs(self.root.path(), &self.canaries);
        assert!(leaks.is_empty(), "canaries leaked: {leaks:?}");
        assert!(
            scanned >= 2,
            "expected the EdgeR and SQLD logs, saw {scanned}"
        );
    }

    /// A second EdgeR process over the same directories, ledger and SQLD,
    /// on its own port. It loads the workers installed so far.
    fn start_extra_edger(&mut self) -> u16 {
        let port = free_port();
        let label = format!("edger-extra-{}", self.extras.len() + 1);
        let mut command = self.edger_command();
        command.env("PORT", port.to_string());
        let mut tree = SubreaperTree::start(&command, self.root.path(), &label)
            .expect("start the extra EdgeR under a subreaper");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(reason) = tree.exited() {
                panic!("the extra edger exited before ready: {reason}");
            }
            if http(port, "GET", "/ready", &[], b"").is_ok_and(|reply| reply.status == 200) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the extra edger did not become ready"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.extras.push(ExtraEdger { label, tree });
        port
    }

    /// SIGKILL of the live EdgeR after its identity is re-proven.
    /// Returns the helper's line that carries the observed SIGKILL status.
    fn crash_edger(&mut self) -> String {
        self.edger
            .as_mut()
            .expect("a live EdgeR run")
            .crash_edger()
            .expect("SIGKILL the EdgeR")
    }

    fn ledger_rows(&self) -> Vec<LedgerRow> {
        let db = rusqlite::Connection::open_with_flags(
            self.root.path().join("state/registry.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open the ledger read-only");
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut statement = db
            .prepare(
                "SELECT worker, sql_namespace, public_key, quota_bytes FROM sqld_registry ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                let key: Vec<u8> = row.get(2)?;
                Ok(LedgerRow {
                    worker: row.get(0)?,
                    sql_namespace: row.get(1)?,
                    public_key: key.try_into().expect("32-byte public key"),
                    quota: row.get(3)?,
                })
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The app's own signing key, opened from the ledger with the signer master.
    fn ledger_signing_key(&self, worker: &str) -> edger_orchestrator::sqld_signer::SqldSigningKey {
        use edger_orchestrator::sqld_signer::{SealedSqldSigningKey, SqldSignerOwner};
        let db = rusqlite::Connection::open_with_flags(
            self.root.path().join("state/registry.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let (nonce, ciphertext): (Vec<u8>, Vec<u8>) = db
            .query_row(
                "SELECT key_nonce, key_ciphertext FROM sqld_registry WHERE worker = ?1",
                [worker],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let sealed = SealedSqldSigningKey::from_parts(&nonce, &ciphertext).unwrap();
        edger_orchestrator::sqld_signer::SqldSigningKey::open(
            &sealed,
            &signer_key(),
            &SqldSignerOwner {
                worker,
                rbac_namespace: None,
                generation: 1,
            },
        )
        .expect("the ledger key opens with the signer master")
    }

    fn ledger_row(&self, worker: &str) -> LedgerRow {
        let rows: Vec<LedgerRow> = self
            .ledger_rows()
            .into_iter()
            .filter(|row| row.worker == worker)
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "exactly one ledger row for {worker}: {rows:?}"
        );
        rows.into_iter().next().unwrap()
    }

    /// The `jwt_key` SQLD holds for the namespace, read straight from the admin port.
    fn remote_key(&self, namespace: &str) -> edger_orchestrator::sqld_admin::SqldRemoteKey {
        let reply = http(
            self.admin_port,
            "GET",
            &format!("/v1/namespaces/{namespace}/config"),
            &[("Authorization", &format!("Basic {ADMIN_KEY}"))],
            b"",
        )
        .expect("SQLD admin config");
        assert_eq!(reply.status, 200, "{}", reply.text());
        let value = reply.json();
        let field = value["jwt_key"].as_str().expect("namespace has a jwt_key");
        edger_orchestrator::sqld_admin::parse_remote_public_key(field)
    }

    fn remote_config_raw(&self, namespace: &str) -> String {
        let reply = http(
            self.admin_port,
            "GET",
            &format!("/v1/namespaces/{namespace}/config"),
            &[("Authorization", &format!("Basic {ADMIN_KEY}"))],
            b"",
        )
        .expect("SQLD admin config");
        format!("{} {}", reply.status, reply.text())
    }

    // ── EdgeR over HTTP ──

    fn root_call(&self, method: &str, path: &str, body: &str) -> Reply {
        http(
            self.port,
            method,
            path,
            &[
                ("Authorization", &format!("Bearer {ROOT_KEY}")),
                ("Content-Type", "application/json"),
            ],
            body.as_bytes(),
        )
        .unwrap_or_else(|error| panic!("{method} {path}: {error}"))
    }

    fn install(&self, files: &[(&str, &[u8])]) -> serde_json::Value {
        let package = zip_package(files);
        let reply = http(
            self.port,
            "POST",
            "/api/admin/workers/install",
            &[
                ("Authorization", &format!("Bearer {ROOT_KEY}")),
                ("Content-Type", "application/zip"),
            ],
            &package,
        )
        .expect("install request");
        assert!(
            (200..300).contains(&reply.status),
            "install answered {}: {}",
            reply.status,
            reply.text()
        );
        reply.json()
    }

    fn install_deno(&self, name: &str, allow_net: &str, extra: &str) {
        let manifest = deno_manifest(name, allow_net, extra);
        self.install(&[
            ("manifest.yaml", manifest.as_bytes()),
            ("index.ts", DENO_APP.as_bytes()),
        ]);
    }

    fn put_database(&self, name: &str, quota: u64) -> Reply {
        self.root_call(
            "PUT",
            &format!("/api/admin/workers/{}/database", pct(name)),
            &format!("{{\"quotaBytes\":{quota}}}"),
        )
    }

    fn get_database(&self, name: &str) -> Reply {
        self.root_call(
            "GET",
            &format!("/api/admin/workers/{}/database", pct(name)),
            "",
        )
    }

    /// Binds the app and proves the answer, then activates it by an explicit recycle.
    fn provision(&self, name: &str, quota: u64) -> serde_json::Value {
        let put = self.put_database(name, quota);
        assert_eq!(put.status, 200, "PUT {name}: {}", put.text());
        let body = put.json();
        assert_eq!(body["database"]["state"], "ready", "{body}");
        assert_eq!(body["database"]["quotaBytes"], quota, "{body}");
        assert_eq!(body["activation"], "nextSpawn", "{body}");
        self.recycle(name);
        body
    }

    fn recycle(&self, name: &str) {
        let reply = self.root_call(
            "POST",
            &format!("/api/admin/workers/{}/recycle?version=1.0.0", pct(name)),
            "",
        );
        assert_eq!(reply.status, 200, "recycle {name}: {}", reply.text());
    }

    fn app(&self, name: &str, route: &str) -> Reply {
        self.app_call(self.port, "GET", name, route)
    }

    fn app_call(&self, port: u16, method: &str, name: &str, route: &str) -> Reply {
        http(port, method, &format!("/{name}/{route}"), &[], b"")
            .unwrap_or_else(|error| panic!("{method} /{name}/{route}: {error}"))
    }

    /// SQL run by the app on the EdgeR listening on `port`.
    fn sql_on(&self, port: u16, name: &str, sql: &str) -> Sql {
        let reply = self.app_call(port, "GET", name, &format!("q?sql={}", pct(sql)));
        assert_eq!(reply.status, 200, "{}", reply.text());
        let body = reply.json();
        Sql::parse(
            body["status"].as_u64().unwrap_or(0),
            body["text"].as_str().unwrap_or(""),
        )
    }

    /// A request to an app whose binding is not Ready is refused by the
    /// host (500 `SQLD_BINDING_NOT_READY`): it never runs without the binding.
    fn assert_binding_not_ready(&self, name: &str) {
        let reply = self.app(name, "state");
        assert!(
            reply.status == 500 && reply.text().contains("SQLD_BINDING_NOT_READY"),
            "{name} must be refused while its binding is not ready: {} {}",
            reply.status,
            reply.text()
        );
    }

    fn put_secret(&self, name: &str, secret: &str, value: &str) -> Reply {
        self.root_call(
            "PUT",
            &format!("/api/admin/workers/{}/secrets/{secret}", pct(name)),
            &serde_json::json!({ "value": value }).to_string(),
        )
    }

    fn app_json(&self, name: &str, route: &str) -> serde_json::Value {
        let reply = self.app(name, route);
        assert_eq!(reply.status, 200, "GET /{name}/{route}: {}", reply.text());
        reply.json()
    }

    fn state(&self, name: &str) -> serde_json::Value {
        self.app_json(name, "state")
    }

    /// SQL run by the app itself, through its guarded socket.
    fn sql(&self, name: &str, sql: &str) -> Sql {
        self.sql_route(name, &format!("q?sql={}", pct(sql)))
    }

    fn sql_route(&self, name: &str, route: &str) -> Sql {
        let body = self.app_json(name, route);
        let status = body["status"].as_u64().unwrap_or(0);
        match body["err"].as_str() {
            Some(err) if status != 200 => {
                Sql::parse(status, &format!("guard-error {err}: {}", body["text"]))
            }
            _ => Sql::parse(status, body["text"].as_str().unwrap_or("")),
        }
    }

    fn token_of(&mut self, name: &str) -> String {
        let token = self.app_json(name, "tok")["token"]
            .as_str()
            .unwrap_or_else(|| panic!("{name} has no SQLD_TOKEN in its environment"))
            .to_owned();
        assert!(!token.is_empty());
        self.canaries.push(token.clone());
        token
    }

    /// The test talking to the SQLD data port itself (the isolation controls).
    fn sqld_direct(&self, token: Option<&str>, namespace: &str, sql: &str) -> Sql {
        let bearer = token.map(|token| format!("Bearer {token}"));
        let namespace = b64(namespace.as_bytes());
        let mut headers = vec![
            ("Content-Type", "application/json"),
            ("x-namespace-bin", namespace.as_str()),
        ];
        if let Some(bearer) = bearer.as_deref() {
            headers.push(("Authorization", bearer));
        }
        let reply = http(
            self.user_port,
            "POST",
            "/v2/pipeline",
            &headers,
            &pipeline_body(sql),
        )
        .expect("SQLD data request");
        Sql::parse(u64::from(reply.status), &reply.text())
    }

    fn namespace_of(&self, name: &str) -> String {
        let body = self.get_database(name).json();
        body["database"]["sqlNamespace"]
            .as_str()
            .unwrap_or_else(|| panic!("{name} has no database: {body}"))
            .to_owned()
    }
}

/// A `PUT .../database` sent from its own thread (so a gate can hold it).
fn put_async(port: u16, name: &str, quota: u64) -> std::thread::JoinHandle<io::Result<Reply>> {
    let path = format!("/api/admin/workers/{}/database", pct(name));
    let body = format!("{{\"quotaBytes\":{quota}}}");
    std::thread::spawn(move || {
        http(
            port,
            "PUT",
            &path,
            &[
                ("Authorization", &format!("Bearer {ROOT_KEY}")),
                ("Content-Type", "application/json"),
            ],
            body.as_bytes(),
        )
    })
}

/// Runs `body` on a fresh stack (SQLD, then EdgeR), always stops everything,
/// and only then proves the cleanup and the log scan.
fn scenario(budget: u64, body: impl FnOnce(&mut Stack)) {
    scenario_with(budget, false, body);
}

fn scenario_with(budget: u64, proxied: bool, body: impl FnOnce(&mut Stack)) {
    let mut stack = Stack::new(budget);
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if proxied {
            stack.enable_proxies();
        }
        stack.start_sqld();
        stack.start_edger();
        body(&mut stack);
    }));
    let finish = stack.finish();
    match attempt {
        Err(panic) => {
            if let Err(problems) = finish {
                eprintln!("cleanup after the failure also reported: {problems}");
            }
            std::panic::resume_unwind(panic);
        }
        Ok(()) => {
            finish.expect("stack cleanup proven");
            stack.assert_logs_hold_no_canary();
        }
    }
}

// ───────────────────────────── scenarios ─────────────────────────────

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn deno_app_database_works_end_to_end_and_survives_recycle_and_restart() {
    scenario(64 * MIB, |stack| {
        stack.install_deno("dapp", "[]", "");
        // Negative control: before the binding the live process has nothing.
        let before = stack.state("dapp");
        assert_eq!(before["url"], "");
        assert_eq!(before["tokenPresent"], false);
        assert_eq!(before["socket"], false);
        let unbound = stack.get_database("dapp");
        assert_eq!(unbound.json()["database"], serde_json::Value::Null);
        assert_eq!(unbound.header("cache-control"), Some("no-store"));
        let first_pid = before["pid"].as_u64().unwrap();

        let put = stack.put_database("dapp", QUOTA);
        assert_eq!(put.status, 200, "{}", put.text());
        let body = put.json();
        assert_eq!(body["database"]["state"], "ready");
        assert_eq!(body["activation"], "nextSpawn");
        assert!(body["initialBackup"]["fileName"].is_string(), "{body}");
        // The binding is only for the next spawn: the live process is unchanged.
        let still = stack.state("dapp");
        assert_eq!(still["pid"].as_u64().unwrap(), first_pid);
        assert_eq!(still["url"], "");
        assert_eq!(still["tokenPresent"], false);

        stack.recycle("dapp");
        let active = stack.state("dapp");
        assert_ne!(
            active["pid"].as_u64().unwrap(),
            first_pid,
            "recycle spawned a new process"
        );
        assert_eq!(active["url"], format!("{}/", stack.user_origin()));
        assert_eq!(active["tokenPresent"], true);
        assert_eq!(active["socket"], true);
        stack.token_of("dapp");

        let create = stack.sql(
            "dapp",
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)",
        );
        assert!(create.ok(), "{create:?}");
        for body in ["one", "two", "three"] {
            let insert = stack.sql(
                "dapp",
                &format!("INSERT INTO notes(body) VALUES ('{body}')"),
            );
            assert!(insert.ok(), "{insert:?}");
        }
        let read = stack.sql(
            "dapp",
            "SELECT group_concat(body, ',') FROM (SELECT body FROM notes ORDER BY id)",
        );
        assert_eq!(read.cell(), "one,two,three");

        // Survives a recycle (a brand new Deno process).
        stack.recycle("dapp");
        let recycled = stack.state("dapp");
        assert_ne!(
            recycled["pid"].as_u64().unwrap(),
            active["pid"].as_u64().unwrap()
        );
        assert_eq!(stack.sql("dapp", "SELECT count(*) FROM notes").cell(), "3");

        // Survives a restart of EdgeR over the same directories.
        let namespace = stack.namespace_of("dapp");
        stack.restart_edger();
        assert_eq!(
            stack.namespace_of("dapp"),
            namespace,
            "same ledger entry after restart"
        );
        assert_eq!(
            stack.get_database("dapp").json()["database"]["state"],
            "ready"
        );
        assert_eq!(stack.sql("dapp", "SELECT count(*) FROM notes").cell(), "3");
        let more = stack.sql("dapp", "INSERT INTO notes(body) VALUES ('four')");
        assert!(more.ok(), "{more:?}");
        assert_eq!(stack.sql("dapp", "SELECT count(*) FROM notes").cell(), "4");

        // The data really lives in SQLD under that namespace: with the app's
        // own token the test reads the same rows straight from the data port.
        let token = stack.token_of("dapp");
        let direct = stack.sqld_direct(Some(&token), &namespace, "SELECT count(*) FROM notes");
        assert_eq!(direct.cell(), "4");
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN), the approved Go artifact (EDGER_GO_WASM_FIXTURE) and Deno (EDGER_DENO_BIN)"]
fn go_wasm_app_database_works_end_to_end_and_survives_recycle_and_restart() {
    let wasm = fs::read(require_file("EDGER_GO_WASM_FIXTURE")).unwrap();
    let manifest = |name: &str| {
        format!(
            "name: \"{name}\"\nversion: \"1.0.0\"\nkind: wasm\nentrypoint: index.wasm\nsecrets:\n  - SQLD_TOKEN\nenv:\n  SQLD_URL: \"\"\ntimeout: 30s\nmaxBodySize: 4mb\n"
        )
    };
    const DB_OK: &[u8] = br#"{"ok":true,"value":1}"#;
    const DB_503: &[u8] = br#"{"error":"configuration missing"}"#;
    scenario(64 * MIB, |stack| {
        for name in ["go-app", "go-other"] {
            stack.install(&[
                ("manifest.yaml", manifest(name).as_bytes()),
                ("index.wasm", &wasm),
            ]);
        }
        // Negative control: nothing is bound, the guest sees no configuration.
        for name in ["go-app", "go-other"] {
            let reply = stack.app(name, "db");
            assert_eq!(
                (reply.status, reply.body.as_slice()),
                (503, DB_503),
                "{name}"
            );
        }
        stack.provision("go-app", QUOTA);
        // Positive: the real Go module reached ITS database through the guard
        // with the host-held token; the unbound neighbour did not.
        let bound = stack.app("go-app", "db");
        assert_eq!(
            (bound.status, bound.body.as_slice()),
            (200, DB_OK),
            "{}",
            bound.text()
        );
        let neighbour = stack.app("go-other", "db");
        assert_eq!((neighbour.status, neighbour.body.as_slice()), (503, DB_503));

        stack.recycle("go-app");
        let recycled = stack.app("go-app", "db");
        assert_eq!((recycled.status, recycled.body.as_slice()), (200, DB_OK));

        stack.restart_edger();
        assert_eq!(
            stack.get_database("go-app").json()["database"]["state"],
            "ready"
        );
        let restarted = stack.app("go-app", "db");
        assert_eq!((restarted.status, restarted.body.as_slice()), (200, DB_OK));
        let neighbour = stack.app("go-other", "db");
        assert_eq!((neighbour.status, neighbour.body.as_slice()), (503, DB_503));
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn apps_cannot_cross_namespaces_and_an_unregistered_homonym_gets_nothing() {
    scenario(64 * MIB, |stack| {
        let port = stack.user_port;
        stack.install_deno("app-a", "[]", "");
        stack.install_deno("app-b", "[]", "");
        // Same local name as A, other RBAC namespace, no registration. It
        // declares the token secret and may reach the SQLD port directly.
        stack.install_deno(
            "@other/app-a",
            &format!("['127.0.0.1:{port}']"),
            &format!("secrets:\n  - SQLD_TOKEN\nenv:\n  SQLD_URL: \"http://127.0.0.1:{port}\"\n"),
        );
        stack.provision("app-a", QUOTA);
        stack.provision("app-b", QUOTA);
        let (ns_a, ns_b) = (stack.namespace_of("app-a"), stack.namespace_of("app-b"));
        assert_ne!(ns_a, ns_b);
        let (tok_a, tok_b) = (stack.token_of("app-a"), stack.token_of("app-b"));
        assert_ne!(tok_a, tok_b);

        let canary_a = "A-CANARY-4f1c9d";
        let canary_b = "B-CANARY-77be02";
        assert!(stack.sql("app-a", "CREATE TABLE a_only (v TEXT)").ok());
        assert!(stack.sql("app-b", "CREATE TABLE b_only (v TEXT)").ok());
        assert!(stack
            .sql(
                "app-a",
                &format!("INSERT INTO a_only VALUES ('{canary_a}')")
            )
            .ok());
        assert!(stack
            .sql(
                "app-b",
                &format!("INSERT INTO b_only VALUES ('{canary_b}')")
            )
            .ok());
        stack.canaries.push(canary_a.to_owned());
        stack.canaries.push(canary_b.to_owned());

        // Positive controls: each app reads its own row, and nothing of the other.
        assert_eq!(stack.sql("app-a", "SELECT v FROM a_only").cell(), canary_a);
        assert_eq!(stack.sql("app-b", "SELECT v FROM b_only").cell(), canary_b);
        assert!(stack
            .sql("app-a", "SELECT v FROM b_only")
            .missing_table("b_only"));
        assert!(stack
            .sql("app-b", "SELECT v FROM a_only")
            .missing_table("a_only"));

        // SQLD itself enforces the pair (these controls fail if the isolation
        // were only the host's): the right token+namespace reads, every mix is refused.
        assert_eq!(
            stack
                .sqld_direct(Some(&tok_b), &ns_b, "SELECT v FROM b_only")
                .cell(),
            canary_b
        );
        assert_eq!(
            stack
                .sqld_direct(Some(&tok_a), &ns_a, "SELECT v FROM a_only")
                .cell(),
            canary_a
        );
        for (token, namespace, sql, label) in [
            (
                Some(tok_b.as_str()),
                &ns_a,
                "SELECT v FROM a_only",
                "token B on namespace A",
            ),
            (
                Some(tok_a.as_str()),
                &ns_b,
                "SELECT v FROM b_only",
                "token A on namespace B",
            ),
            (
                None,
                &ns_a,
                "SELECT v FROM a_only",
                "no token on namespace A",
            ),
            (
                Some("not-a-jwt"),
                &ns_b,
                "SELECT v FROM b_only",
                "garbage token on namespace B",
            ),
        ] {
            let refused = stack.sqld_direct(token, namespace, sql);
            assert!(
                refused.auth_refused(),
                "{label}: expected a 401/403, got {refused:?}"
            );
            assert!(refused.rows.is_empty(), "{label}: {refused:?}");
            assert!(
                !refused.raw.contains(canary_a) && !refused.raw.contains(canary_b),
                "{label}"
            );
        }

        // The guest of A forges the namespace and/or carries B's token.
        let forged = |tok: &str, ns: &str, sql: &str| {
            stack.sql_route(
                "app-a",
                &format!("q-guest?tok={}&ns={}&sql={}", pct(tok), pct(ns), pct(sql)),
            )
        };
        for (tok, ns, label) in [
            (tok_a.as_str(), b64(ns_b.as_bytes()), "A token, namespace B"),
            (tok_b.as_str(), b64(ns_b.as_bytes()), "B token, namespace B"),
            (tok_b.as_str(), b64(ns_a.as_bytes()), "B token, namespace A"),
        ] {
            let read_b = forged(tok, &ns, "SELECT v FROM b_only");
            assert!(!read_b.raw.contains(canary_b), "{label} read B: {read_b:?}");
            assert!(
                read_b.missing_table("b_only"),
                "{label}: B's table must not exist for A: {read_b:?}"
            );
            // The host imposed A's namespace and credential: the call lands in A.
            let read_a = forged(tok, &ns, "SELECT v FROM a_only");
            assert_eq!(read_a.cell(), canary_a, "{label}");
            // And it cannot write into B either: B's table stays at one row.
            let write_b = forged(
                tok,
                &ns,
                &format!("INSERT INTO b_only VALUES ('{canary_a}-intruder')"),
            );
            assert!(
                write_b.missing_table("b_only"),
                "{label} wrote into B: {write_b:?}"
            );
        }
        assert_eq!(
            stack.sql("app-b", "SELECT count(*) FROM b_only").cell(),
            "1"
        );
        assert_eq!(
            stack.sql("app-a", "SELECT count(*) FROM a_only").cell(),
            "1"
        );

        // The homonym with another RBAC namespace and no registry entry.
        assert_eq!(
            stack.get_database("@other/app-a").json()["database"],
            serde_json::Value::Null
        );
        let homonym = stack.state("@other/app-a");
        assert_eq!(homonym["tokenPresent"], false, "{homonym}");
        assert_eq!(homonym["socket"], false);
        let direct = stack.sql("@other/app-a", "SELECT v FROM a_only");
        // No namespace header and no token: SQLD resolves the namespace from the
        // host name (`127`), which does not exist, and answers 404 without data.
        assert!(
            direct.status == 404 && direct.raw.contains("doesn't exist") && direct.rows.is_empty(),
            "{direct:?}"
        );
        assert!(!direct.raw.contains(canary_a), "{direct:?}");
        // Even naming A's namespace it carries no credential that SQLD honours.
        let named = stack.sql_route(
            "@other/app-a",
            &format!(
                "q-guest?tok=&ns={}&sql={}",
                pct(&b64(ns_a.as_bytes())),
                pct("SELECT v FROM a_only")
            ),
        );
        assert!(
            named.auth_refused() && !named.raw.contains(canary_a),
            "{named:?}"
        );
        // A's own binding is untouched by the homonym.
        assert_eq!(
            stack.get_database("app-a").json()["database"]["state"],
            "ready"
        );

        // No secret in the metadata answers.
        for name in ["app-a", "app-b"] {
            let text = stack.get_database(name).text();
            for secret in [&tok_a, &tok_b] {
                assert!(
                    !text.contains(secret.as_str()),
                    "token in GET database {name}"
                );
            }
            assert!(!text.contains(ADMIN_KEY));
            assert!(
                !text.contains("jwt") && !text.contains("privateKey"),
                "{text}"
            );
        }
        let put_again = stack.put_database("app-a", QUOTA).text();
        assert!(!put_again.contains(tok_a.as_str()) && !put_again.contains(ADMIN_KEY));
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn a_full_quota_stops_only_its_app_and_conflicts_answer_409() {
    scenario(2 * QUOTA, |stack| {
        stack.install_deno("app-a", "[]", "");
        stack.install_deno("app-b", "[]", "");
        stack.install_deno("app-c", "[]", "");
        stack.provision("app-a", QUOTA);
        stack.provision("app-b", QUOTA);

        assert!(stack.sql("app-a", "CREATE TABLE big (v BLOB)").ok());
        assert!(stack.sql("app-b", "CREATE TABLE small (i INTEGER)").ok());

        // Fill A one MiB at a time while B keeps writing in between.
        let mut accepted_a = 0;
        let mut first_failure: Option<Sql> = None;
        let mut b_writes = 0;
        for round in 0..40 {
            let insert = stack.sql("app-a", "INSERT INTO big VALUES (randomblob(1048576))");
            if insert.ok() {
                assert!(
                    first_failure.is_none(),
                    "A accepted a write after it was full"
                );
                accepted_a += 1;
            } else if first_failure.is_none() {
                first_failure = Some(insert);
            }
            let b = stack.sql("app-b", &format!("INSERT INTO small VALUES ({round})"));
            assert!(b.ok(), "B stopped at round {round}: {b:?}");
            b_writes += 1;
            if first_failure.is_some() && round > 20 {
                break;
            }
        }
        let failure = first_failure.expect("A never reached SQLITE_FULL");
        let message = failure.error.clone().expect("a SQL error");
        eprintln!("quota-evidence: A accepted {accepted_a} MiB inserts, then {message}");
        assert!(
            message.contains("SQLITE_FULL"),
            "unexpected quota error: {message}"
        );
        assert!(
            (8..=16).contains(&accepted_a),
            "A accepted {accepted_a} MiB inserts"
        );
        // Reopened connections (every request is one) keep being refused,
        // reads still work, and B is untouched.
        let again = stack.sql("app-a", "INSERT INTO big VALUES (randomblob(1048576))");
        assert!(
            again
                .error
                .as_deref()
                .is_some_and(|error| error.contains("SQLITE_FULL")),
            "a fresh connection must hit SQLITE_FULL again: {again:?}"
        );
        assert_eq!(
            stack.sql("app-a", "SELECT count(*) FROM big").cell(),
            accepted_a.to_string()
        );
        assert_eq!(
            stack.sql("app-b", "SELECT count(*) FROM small").cell(),
            b_writes.to_string()
        );
        assert!(stack.sql("app-b", "INSERT INTO small VALUES (1000)").ok());

        // Quota is immutable: another quota answers 409 and the record is unchanged.
        let other = stack.put_database("app-a", 2 * QUOTA);
        assert_eq!(other.status, 409, "{}", other.text());
        assert_eq!(other.json()["code"], "SQLD_CONFLICT", "{}", other.text());
        let metadata = stack.get_database("app-a").json();
        assert_eq!(metadata["database"]["quotaBytes"], QUOTA);
        assert_eq!(metadata["database"]["state"], "ready");
        // Positive control: the same quota repeated is accepted.
        let same = stack.put_database("app-a", QUOTA);
        assert_eq!(same.status, 200, "{}", same.text());

        // The budget (two quotas) is spent: a third app is refused and the
        // existing reservations stay.
        let refused = stack.put_database("app-c", QUOTA);
        assert_eq!(refused.status, 409, "{}", refused.text());
        assert_eq!(
            stack.get_database("app-c").json()["database"],
            serde_json::Value::Null
        );
        assert_eq!(
            stack.get_database("app-a").json()["database"]["state"],
            "ready"
        );
        assert_eq!(
            stack.get_database("app-b").json()["database"]["state"],
            "ready"
        );
        stack.restart_edger();
        let again = stack.put_database("app-c", QUOTA);
        assert_eq!(
            again.status,
            409,
            "the reservation survives a restart: {}",
            again.text()
        );
        assert_eq!(
            stack.sql("app-b", "SELECT count(*) FROM small").cell(),
            (b_writes + 1).to_string()
        );
        assert_eq!(
            stack.sql("app-a", "SELECT count(*) FROM big").cell(),
            accepted_a.to_string()
        );
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn backup_is_private_complete_and_restorable() {
    use sha2::{Digest, Sha256};
    let restore = |path: &std::path::Path| {
        let dump = fs::read_to_string(path).expect("dump is UTF-8 SQL");
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(&dump).map(|_| db)
    };
    scenario(64 * MIB, |stack| {
        stack.install_deno("app-a", "[]", "");
        stack.install_deno("app-b", "[]", "");
        let provisioned = stack.provision("app-a", QUOTA);
        stack.provision("app-b", QUOTA);
        let backups = stack.root.path().join("state/backups");

        // Negative control: the initial backup was taken before any data.
        let initial = backups.join(provisioned["initialBackup"]["fileName"].as_str().unwrap());
        let restored = restore(&initial).expect("the initial backup restores into a clean SQLite");
        let integrity: String = restored
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let has_items: i64 = restored
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'items'",
                [],
                |r| r.get(0),
            )
            .expect("sqlite_master is readable");
        assert_eq!(
            has_items, 0,
            "the empty initial dump must not contain the table yet"
        );

        assert!(stack
            .sql(
                "app-a",
                "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)"
            )
            .ok());
        let inserted = stack.sql(
            "app-a",
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<250) INSERT INTO items(label) SELECT 'row-' || x FROM n",
        );
        assert!(inserted.ok(), "{inserted:?}");
        assert!(stack.sql("app-b", "CREATE TABLE other (v TEXT)").ok());
        assert!(stack
            .sql("app-b", "INSERT INTO other VALUES ('B-ONLY-ROW')")
            .ok());

        let reply = stack.root_call("POST", "/api/admin/workers/app-a/database/backup", "");
        assert_eq!(reply.status, 200, "{}", reply.text());
        let artifact = reply.json()["backup"].clone();
        let file = backups.join(artifact["fileName"].as_str().unwrap());
        let namespace = stack.namespace_of("app-a");
        assert!(file
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(&format!("{namespace}-")));
        assert_eq!(artifact["retention"], "complete");
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "backup file mode {mode:o}");
        assert_eq!(
            fs::metadata(&backups).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        let bytes = fs::read(&file).unwrap();
        assert_eq!(artifact["bytes"].as_u64().unwrap(), bytes.len() as u64);
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(artifact["sha256"], digest);
        assert!(
            !String::from_utf8_lossy(&bytes).contains("B-ONLY-ROW"),
            "A's dump holds B's rows"
        );

        // Restored into a clean SQLite: integrity and the exact row count.
        let db = restore(&file).expect("the dump restores into a clean SQLite");
        let integrity: String = db
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let rows: i64 = db
            .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 250);
        assert_eq!(
            stack.sql("app-a", "SELECT count(*) FROM items").cell(),
            rows.to_string()
        );
        let tables: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'other'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "B's table is not in A's dump");

        // Another app, and an app without a database, are handled separately.
        let other = stack.root_call("POST", "/api/admin/workers/app-b/database/backup", "");
        assert_eq!(other.status, 200, "{}", other.text());
        assert_ne!(other.json()["backup"]["fileName"], artifact["fileName"]);
        stack.install_deno("app-c", "[]", "");
        let none = stack.root_call("POST", "/api/admin/workers/app-c/database/backup", "");
        assert_eq!(none.status, 404, "{}", none.text());
        // The backup route is root only.
        let anon = http(
            stack.port,
            "POST",
            "/api/admin/workers/app-a/database/backup",
            &[],
            b"",
        )
        .unwrap();
        assert_eq!(anon.status, 401, "{}", anon.text());
    });
}

// ───────────────────────────── part B ─────────────────────────────

fn go_crud_scenario(wasm: &[u8]) {
    let manifest = |name: &str| {
        format!(
            "name: \"{name}\"\nversion: \"1.0.0\"\nkind: wasm\nentrypoint: index.wasm\nsecrets:\n  - SQLD_TOKEN\nenv:\n  SQLD_URL: \"\"\ntimeout: 30s\nmaxBodySize: 4mb\n"
        )
    };
    const DB_503: &[u8] = br#"{"error":"configuration missing"}"#;
    const OK: &[u8] = br#"{"ok":true}"#;
    scenario(64 * MIB, |stack| {
        for name in ["go-crud-a", "go-crud-b"] {
            stack.install(&[
                ("manifest.yaml", manifest(name).as_bytes()),
                ("index.wasm", wasm),
            ]);
        }
        let call = |stack: &Stack, method: &str, name: &str, route: &str| {
            let reply = stack.app_call(stack.port, method, name, route);
            (reply.status, reply.body)
        };
        let count = |stack: &Stack, name: &str| {
            let (status, body) = call(stack, "GET", name, "notes");
            assert_eq!(status, 200, "{name}: {}", String::from_utf8_lossy(&body));
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            (
                value["count"].as_i64().unwrap(),
                value["last"].as_str().unwrap().to_owned(),
            )
        };
        // Negative control: unbound apps have no configuration at all.
        for name in ["go-crud-a", "go-crud-b"] {
            assert_eq!(
                call(stack, "POST", name, "notes/init"),
                (503, DB_503.to_vec())
            );
            assert_eq!(call(stack, "GET", name, "notes"), (503, DB_503.to_vec()));
        }
        stack.provision("go-crud-a", QUOTA);
        stack.provision("go-crud-b", QUOTA);
        assert_ne!(
            stack.namespace_of("go-crud-a"),
            stack.namespace_of("go-crud-b")
        );
        // Bound but not initialised: the table is missing, the guest answers its static 502.
        assert_eq!(
            call(stack, "GET", "go-crud-a", "notes"),
            (502, br#"{"error":"upstream error"}"#.to_vec())
        );
        // Not a route / not a method: static answers, no SQL.
        assert_eq!(call(stack, "GET", "go-crud-a", "elsewhere").0, 404);
        assert_eq!(call(stack, "DELETE", "go-crud-a", "notes").0, 405);
        assert_eq!(call(stack, "GET", "go-crud-a", "notes/init").0, 405);
        for name in ["go-crud-a", "go-crud-b"] {
            assert_eq!(call(stack, "POST", name, "notes/init"), (200, OK.to_vec()));
            assert_eq!(count(stack, name), (0, String::new()));
        }
        // Two writes into A, one into B: independent counts.
        for _ in 0..2 {
            assert_eq!(
                call(stack, "POST", "go-crud-a", "notes"),
                (200, OK.to_vec())
            );
        }
        assert_eq!(
            call(stack, "POST", "go-crud-b", "notes"),
            (200, OK.to_vec())
        );
        assert_eq!(count(stack, "go-crud-a"), (2, "go-crud-note".to_owned()));
        assert_eq!(count(stack, "go-crud-b"), (1, "go-crud-note".to_owned()));
        // Survives a recycle and a restart of EdgeR.
        stack.recycle("go-crud-a");
        assert_eq!(count(stack, "go-crud-a"), (2, "go-crud-note".to_owned()));
        stack.restart_edger();
        assert_eq!(count(stack, "go-crud-a"), (2, "go-crud-note".to_owned()));
        assert_eq!(count(stack, "go-crud-b"), (1, "go-crud-note".to_owned()));
        assert_eq!(
            call(stack, "POST", "go-crud-a", "notes"),
            (200, OK.to_vec())
        );
        assert_eq!(count(stack, "go-crud-a"), (3, "go-crud-note".to_owned()));
        assert_eq!(count(stack, "go-crud-b"), (1, "go-crud-note".to_owned()));
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and the test-only Go CRUD artifact (EDGER_GO_WASM_CRUD_FIXTURE)"]
fn go_wasm_crud_fixture_writes_reads_and_stays_isolated_per_app() {
    let module = require_file("EDGER_GO_WASM_CRUD_FIXTURE");
    let wasm =
        verify_crud_fixture_provenance(&module, &crud_fixture_sources()).unwrap_or_else(|error| {
            panic!("EDGER_GO_WASM_CRUD_FIXTURE is not built from the worktree sources: {error}")
        });
    go_crud_scenario(&wasm);
}

// ── crash between CREATE and Ready ──

#[derive(Clone, Copy, Debug)]
enum CrashAt {
    /// SQLD has created the namespace; the answer never reaches EdgeR.
    CreateAnswerNotDelivered,
    /// EdgeR got the CREATE answer; the proof GET never leaves EdgeR.
    BeforeTheProofGet,
    /// SQLD has produced the initial dump; EdgeR never receives it.
    DuringTheInitialBackup,
}

fn crash_recovery(at: CrashAt) {
    scenario_with(3 * QUOTA, true, |stack| {
        for name in ["neighbor", "victim", "third", "fourth"] {
            stack.install_deno(name, "[]", "");
        }
        stack.provision("neighbor", QUOTA);
        assert!(stack.sql("neighbor", "CREATE TABLE kept (v TEXT)").ok());
        for value in ["n1", "n2", "n3"] {
            assert!(stack
                .sql("neighbor", &format!("INSERT INTO kept VALUES ('{value}')"))
                .ok());
        }
        let neighbor_token = stack.token_of("neighbor");
        let neighbor_ns = stack.namespace_of("neighbor");
        let victim_ns = derive_namespace("victim");
        let trace_before = stack.admin().trace().len();

        let gate = match at {
            CrashAt::CreateAnswerNotDelivered => {
                let gate = Gate::new(
                    GatePoint::After,
                    usize::MAX,
                    Duration::from_secs(120),
                    is_create,
                );
                stack.admin().add_gate(&gate);
                gate
            }
            CrashAt::BeforeTheProofGet => {
                // The proof GET is the first config GET AFTER the CREATE (an
                // earlier one checks whether the namespace exists).
                let created = std::sync::atomic::AtomicBool::new(false);
                let gate = Gate::new(
                    GatePoint::Before,
                    usize::MAX,
                    Duration::from_secs(120),
                    move |method, path| {
                        if is_create(method, path) {
                            created.store(true, std::sync::atomic::Ordering::SeqCst);
                            false
                        } else {
                            created.load(std::sync::atomic::Ordering::SeqCst)
                                && is_proof_get(method, path)
                        }
                    },
                );
                stack.admin().add_gate(&gate);
                gate
            }
            CrashAt::DuringTheInitialBackup => {
                let gate = Gate::new(
                    GatePoint::After,
                    usize::MAX,
                    Duration::from_secs(120),
                    |m, p| m == "GET" && p == "/dump",
                );
                stack.data().add_gate(&gate);
                gate
            }
        };
        let put = put_async(stack.port, "victim", QUOTA);
        assert!(
            gate.wait_held(1, Duration::from_secs(40)),
            "{at:?}: the gate never held a request; admin trace {:?}; data trace {:?}",
            stack.admin().trace(),
            stack.data().trace()
        );
        // The namespace really exists at SQLD by now (or is about to be proven).
        let trace = stack.admin().trace();
        assert!(
            trace.iter().skip(trace_before).any(|line| {
                line.starts_with("POST /v1/namespaces/edger_")
                    && (line.contains("/create -> 200") || line.contains("/create => 200 held"))
            }),
            "{at:?}: the CREATE did not reach SQLD before the crash: {trace:?}"
        );
        // While the PUT is in flight the app is not ready.
        let during = stack.get_database("victim").json();
        assert!(
            ["intent", "proven"].contains(&during["database"]["state"].as_str().unwrap_or("?")),
            "{at:?}: {during}"
        );

        let killed = stack.crash_edger();
        eprintln!("crash-evidence {at:?}: helper observed {killed}");
        gate.release(Release::Abandon);
        let outcome = put.join().expect("PUT thread");
        assert!(
            outcome.is_err(),
            "{at:?}: the PUT of a killed EdgeR must end with a closed connection, got {outcome:?}"
        );
        // The remote namespace exists with the ledger's key (CREATE reached SQLD).
        let ledger_k0 = stack.ledger_row("victim");
        assert_eq!(ledger_k0.sql_namespace, victim_ns);
        assert_eq!(
            stack.remote_key(&victim_ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(ledger_k0.public_key)
        );
        // Rows written into the namespace in the crash window, with the app's
        // own key opened from the ledger (the test owns the signer master):
        // the recovery must not wipe them.
        let window_token = stack
            .ledger_signing_key("victim")
            .sign_scoped_token(&victim_ns, unix_now(), 3600)
            .unwrap()
            .expose_jwt()
            .to_owned();
        stack.canaries.push(window_token.clone());
        assert!(stack
            .sqld_direct(
                Some(&window_token),
                &victim_ns,
                "CREATE TABLE window_rows (v TEXT)"
            )
            .ok());
        for value in ["window-1", "window-2"] {
            assert!(stack
                .sqld_direct(
                    Some(&window_token),
                    &victim_ns,
                    &format!("INSERT INTO window_rows VALUES ('{value}')")
                )
                .ok());
        }

        stack.restart_edger();
        // Before the new PUT the state is a recovering one, never ready.
        let after_restart = stack.get_database("victim").json();
        let state = after_restart["database"]["state"]
            .as_str()
            .unwrap_or("?")
            .to_owned();
        assert!(
            ["intent", "proven"].contains(&state.as_str()),
            "{at:?}: after the restart and before the new PUT: {after_restart}"
        );
        assert_eq!(after_restart["database"]["sqlNamespace"], victim_ns);
        // The app is refused (fail closed) until the PUT completes: no token, no URL.
        stack.recycle("victim");
        stack.assert_binding_not_ready("victim");

        let second = stack.put_database("victim", QUOTA);
        assert_eq!(second.status, 200, "{at:?}: {}", second.text());
        let second = second.json();
        assert_eq!(second["database"]["state"], "ready");
        assert_eq!(second["database"]["sqlNamespace"], victim_ns);
        assert_eq!(second["database"]["quotaBytes"], QUOTA);
        // Same namespace, same key (never re-keyed), remote key equals the ledger's.
        let ledger_k1 = stack.ledger_row("victim");
        assert_eq!(
            ledger_k1.public_key, ledger_k0.public_key,
            "{at:?}: re-keyed"
        );
        assert_eq!(ledger_k1.sql_namespace, victim_ns);
        assert_eq!(
            stack.remote_key(&victim_ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(ledger_k1.public_key)
        );
        // No second destructive CREATE: exactly one CREATE ever answered 200
        // for the victim; any retry was refused as "already exists".
        let creates: Vec<String> = stack
            .admin()
            .trace()
            .into_iter()
            .skip(trace_before)
            .filter(|line| {
                // `=> ... held` marks an answer SQLD gave and the gate kept back;
                // the final `->` entry is what counts.
                line.starts_with("POST /v1/namespaces/edger_") && line.contains("/create ->")
            })
            .collect();
        eprintln!(
            "crash-evidence {at:?}: state before the new PUT = {state}; CREATE trace after the crash point = {creates:?}"
        );
        let created = creates
            .iter()
            .filter(|line| line.contains("-> 200"))
            .count();
        assert_eq!(created, 1, "{at:?}: CREATE trace {creates:?}");
        for line in &creates {
            assert!(
                line.contains("-> 200") || line.contains("-> 400"),
                "{at:?}: unexpected CREATE answer in {creates:?}"
            );
        }
        // The reservation was counted once: the ledger holds neighbor + victim.
        let rows = stack.ledger_rows();
        assert_eq!(rows.len(), 2, "{at:?}: {rows:?}");
        assert_eq!(
            rows.iter().map(|row| row.quota).sum::<i64>(),
            2 * QUOTA as i64
        );

        // Activate and use it; the neighbor's rows and token survived untouched.
        stack.recycle("victim");
        let bound = stack.state("victim");
        assert_eq!(bound["tokenPresent"], true, "{at:?}: {bound}");
        // The rows written during the crash window survived the recovery.
        assert_eq!(
            stack
                .sql("victim", "SELECT group_concat(v, ',') FROM window_rows")
                .cell(),
            "window-1,window-2",
            "{at:?}"
        );
        assert!(stack
            .sql("victim", "CREATE TABLE after_recovery (v TEXT)")
            .ok());
        assert!(stack
            .sql("victim", "INSERT INTO after_recovery VALUES ('x')")
            .ok());
        assert_eq!(
            stack
                .sql("victim", "SELECT count(*) FROM after_recovery")
                .cell(),
            "1"
        );
        assert_eq!(
            stack.sql("neighbor", "SELECT count(*) FROM kept").cell(),
            "3"
        );
        assert_eq!(stack.token_of("neighbor"), neighbor_token);
        assert_eq!(stack.namespace_of("neighbor"), neighbor_ns);
        assert_eq!(
            stack
                .sqld_direct(
                    Some(&neighbor_token),
                    &neighbor_ns,
                    "SELECT count(*) FROM kept"
                )
                .cell(),
            "3"
        );
        // Budget: 3 quotas in total. Victim counted once => `third` fits, `fourth` does not.
        let third = stack.put_database("third", QUOTA);
        assert_eq!(
            third.status,
            200,
            "{at:?}: reservation counted twice? {}",
            third.text()
        );
        let fourth = stack.put_database("fourth", QUOTA);
        assert_eq!(fourth.status, 409, "{at:?}: {}", fourth.text());
        assert_eq!(
            stack.get_database("fourth").json()["database"],
            serde_json::Value::Null
        );
        // A repeated PUT of the recovered app is idempotent: no CREATE, data intact.
        let seen = stack.admin().count("POST /v1/namespaces/edger_");
        let again = stack.put_database("victim", QUOTA);
        assert_eq!(again.status, 200, "{}", again.text());
        assert_eq!(
            stack.admin().count("POST /v1/namespaces/edger_"),
            seen,
            "{at:?}: a Ready app was CREATEd again"
        );
        assert_eq!(
            stack
                .sql("victim", "SELECT count(*) FROM after_recovery")
                .cell(),
            "1"
        );
        // Backups: the recovered app has a published dump and no leftover temp file.
        let names: Vec<String> = fs::read_dir(stack.root.path().join("state/backups"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name.starts_with(&format!("{victim_ns}-")) && name.ends_with(".sql")),
            "{at:?}: no backup for the victim in {names:?}"
        );
        assert!(
            names.iter().all(|name| !name.starts_with(".tmp-")),
            "{at:?}: {names:?}"
        );
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn crash_after_create_reached_sqld_before_its_answer_returns_recovers_to_ready() {
    crash_recovery(CrashAt::CreateAnswerNotDelivered);
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn crash_after_the_create_answer_before_the_proof_get_recovers_to_ready() {
    crash_recovery(CrashAt::BeforeTheProofGet);
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn crash_during_the_initial_backup_recovers_to_ready() {
    crash_recovery(CrashAt::DuringTheInitialBackup);
}

/// A crash in the middle of a repeated PUT of an app that is already Ready
/// (real rows exist): nothing is lost and nothing is re-created.
#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn crash_during_a_repeated_put_of_a_ready_app_keeps_its_rows_and_key() {
    scenario_with(2 * QUOTA, true, |stack| {
        stack.install_deno("steady", "[]", "");
        stack.provision("steady", QUOTA);
        assert!(stack.sql("steady", "CREATE TABLE rows_kept (v TEXT)").ok());
        for value in ["a", "b", "c", "d"] {
            assert!(stack
                .sql(
                    "steady",
                    &format!("INSERT INTO rows_kept VALUES ('{value}')")
                )
                .ok());
        }
        let ns = stack.namespace_of("steady");
        let before = stack.ledger_row("steady");
        let creates = stack.admin().count("POST /v1/namespaces/edger_");
        let gate = Gate::new(
            GatePoint::Before,
            usize::MAX,
            Duration::from_secs(120),
            is_proof_get,
        );
        stack.admin().add_gate(&gate);
        let put = put_async(stack.port, "steady", QUOTA);
        assert!(
            gate.wait_held(1, Duration::from_secs(40)),
            "the repeated PUT never reached the proof GET: {:?}",
            stack.admin().trace()
        );
        let killed = stack.crash_edger();
        eprintln!("crash-evidence repeated PUT of a Ready app: helper observed {killed}");
        gate.release(Release::Abandon);
        let outcome = put.join().expect("PUT thread");
        assert!(outcome.is_err(), "{outcome:?}");
        stack.restart_edger();
        // GET after the restart: the row is still Ready (nothing was demoted).
        let state = stack.get_database("steady").json();
        assert_eq!(state["database"]["state"], "ready", "{state}");
        assert_eq!(state["database"]["sqlNamespace"], ns);
        assert_eq!(
            stack.sql("steady", "SELECT count(*) FROM rows_kept").cell(),
            "4"
        );
        let again = stack.put_database("steady", QUOTA);
        assert_eq!(again.status, 200, "{}", again.text());
        assert_eq!(
            stack.sql("steady", "SELECT count(*) FROM rows_kept").cell(),
            "4"
        );
        let after = stack.ledger_row("steady");
        assert_eq!(after.public_key, before.public_key);
        assert_eq!(
            stack.remote_key(&ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(after.public_key)
        );
        assert_eq!(
            stack.admin().count("POST /v1/namespaces/edger_"),
            creates,
            "a Ready app must never be CREATEd again"
        );
        assert_eq!(stack.ledger_rows().len(), 1);
    });
}

// ── concurrency between EdgeR processes ──

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn two_edger_processes_race_to_provision_the_same_app() {
    scenario_with(64 * MIB, true, |stack| {
        stack.install_deno("shared", "[]", "");
        let second = stack.start_extra_edger();
        let first = stack.port;
        // Barrier: both PUTs must be at the CREATE at the same time.
        let gate = Gate::new(GatePoint::Before, 2, Duration::from_secs(25), |m, p| {
            is_create(m, p)
        });
        stack.admin().add_gate(&gate);
        let (a, b) = (
            put_async(first, "shared", QUOTA),
            put_async(second, "shared", QUOTA),
        );
        let (a, b) = (
            a.join().unwrap().expect("PUT on the first edger"),
            b.join().unwrap().expect("PUT on the second edger"),
        );
        assert!(
            gate.barrier_complete(),
            "the two PUTs never met at the CREATE: {:?}",
            stack.admin().trace()
        );
        let statuses = [a.status, b.status];
        eprintln!(
            "race-evidence: statuses {statuses:?}; admin trace {:?}",
            stack.admin().trace()
        );
        for reply in [&a, &b] {
            assert!(
                reply.status == 200
                    || (reply.status == 409 && reply.json()["code"] == "SQLD_CONFLICT"),
                "unexpected answer {}: {}",
                reply.status,
                reply.text()
            );
        }
        // A 409 is stable and the retry converges to 200.
        for (reply, port) in [(&a, first), (&b, second)] {
            if reply.status == 409 {
                let retry = http(
                    port,
                    "PUT",
                    "/api/admin/workers/shared/database",
                    &[
                        ("Authorization", &format!("Bearer {ROOT_KEY}")),
                        ("Content-Type", "application/json"),
                    ],
                    format!("{{\"quotaBytes\":{QUOTA}}}").as_bytes(),
                )
                .unwrap();
                assert_eq!(retry.status, 200, "{}", retry.text());
            }
        }
        // One namespace, one key, one reservation.
        let rows = stack.ledger_rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let ns = derive_namespace("shared");
        assert_eq!(rows[0].sql_namespace, ns);
        assert_eq!(
            stack.remote_key(&ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(rows[0].public_key)
        );
        let creates: Vec<String> = stack
            .admin()
            .trace()
            .into_iter()
            .filter(|line| {
                line.starts_with("POST /v1/namespaces/edger_") && line.contains("/create")
            })
            .collect();
        assert_eq!(
            creates
                .iter()
                .filter(|line| line.contains("-> 200"))
                .count(),
            1,
            "{creates:?}"
        );
        // Both processes agree and both serve the same database.
        for port in [first, second] {
            let body = http(
                port,
                "GET",
                "/api/admin/workers/shared/database",
                &[("Authorization", &format!("Bearer {ROOT_KEY}"))],
                b"",
            )
            .unwrap()
            .json();
            assert_eq!(body["database"]["state"], "ready", "{body}");
            assert_eq!(body["database"]["sqlNamespace"], ns);
        }
        stack.recycle("shared");
        assert!(stack
            .sql_on(first, "shared", "CREATE TABLE both (v TEXT)")
            .ok());
        assert!(stack
            .sql_on(first, "shared", "INSERT INTO both VALUES ('from-first')")
            .ok());
        let seen = stack.sql_on(second, "shared", "SELECT v FROM both");
        assert_eq!(seen.cell(), "from-first");
        assert!(stack
            .sql_on(second, "shared", "INSERT INTO both VALUES ('from-second')")
            .ok());
        assert_eq!(
            stack
                .sql_on(first, "shared", "SELECT count(*) FROM both")
                .cell(),
            "2"
        );
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn two_edger_processes_race_for_the_last_budget_slot() {
    scenario_with(QUOTA, true, |stack| {
        stack.install_deno("contender-x", "[]", "");
        stack.install_deno("contender-y", "[]", "");
        let second = stack.start_extra_edger();
        let first = stack.port;
        // Barrier at the first admin probe of each PUT, i.e. right before the
        // budget is reserved in the shared ledger.
        let gate = Gate::new(GatePoint::Before, 2, Duration::from_secs(25), |m, p| {
            m == "GET" && is_probe(p)
        });
        stack.admin().add_gate(&gate);
        let (x, y) = (
            put_async(first, "contender-x", QUOTA),
            put_async(second, "contender-y", QUOTA),
        );
        let (x, y) = (
            x.join().unwrap().expect("PUT x"),
            y.join().unwrap().expect("PUT y"),
        );
        assert!(
            gate.barrier_complete(),
            "the two PUTs never met at the probe: {:?}",
            stack.admin().trace()
        );
        eprintln!("budget-evidence: x={} y={}", x.status, y.status);
        let mut statuses = [x.status, y.status];
        statuses.sort_unstable();
        assert_eq!(statuses, [200, 409], "x: {} y: {}", x.text(), y.text());
        let (winner, loser) = if x.status == 200 {
            ("contender-x", "contender-y")
        } else {
            ("contender-y", "contender-x")
        };
        let loser_reply = if x.status == 409 { &x } else { &y };
        assert_eq!(loser_reply.json()["code"], "SQLD_CONFLICT");
        // Exactly one reservation, owned by the winner; the loser has nothing.
        let rows = stack.ledger_rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].worker, winner);
        assert_eq!(rows[0].quota, QUOTA as i64);
        assert_eq!(
            stack.get_database(loser).json()["database"],
            serde_json::Value::Null
        );
        assert_eq!(
            stack.get_database(winner).json()["database"]["state"],
            "ready"
        );
        // The refusal is stable, from either process.
        for port in [first, second] {
            let retry = http(
                port,
                "PUT",
                &format!("/api/admin/workers/{loser}/database"),
                &[
                    ("Authorization", &format!("Bearer {ROOT_KEY}")),
                    ("Content-Type", "application/json"),
                ],
                format!("{{\"quotaBytes\":{QUOTA}}}").as_bytes(),
            )
            .unwrap();
            assert_eq!(retry.status, 409, "{}", retry.text());
        }
        // Positive control: the winner works end to end.
        stack.recycle(winner);
        assert!(stack.sql(winner, "CREATE TABLE won (v TEXT)").ok());
        assert_eq!(stack.ledger_rows().len(), 1);
    });
}

// ── a namespace that is not the app's ──

#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn a_preexisting_foreign_namespace_is_a_conflict_that_changes_nothing() {
    use edger_orchestrator::sqld_signer::SqldSigningKey;
    scenario(64 * MIB, |stack| {
        stack.install_deno("target-app", "[]", "");
        stack.install_deno("fine-app", "[]", "");
        let ns = derive_namespace("target-app");
        // Created straight at SQLD with a key that is not the ledger's, with data.
        let foreign = SqldSigningKey::generate().expect("foreign key");
        let body = serde_json::json!({
            "jwt_key": foreign.public_key_b64url(),
            "max_db_size": QUOTA,
            "allow_attach": false,
        })
        .to_string();
        let created = http(
            stack.admin_port,
            "POST",
            &format!("/v1/namespaces/{ns}/create"),
            &[
                ("Authorization", &format!("Basic {ADMIN_KEY}")),
                ("Content-Type", "application/json"),
            ],
            body.as_bytes(),
        )
        .unwrap();
        assert_eq!(created.status, 200, "{}", created.text());
        let token = foreign
            .sign_scoped_token(&ns, unix_now(), 3600)
            .expect("foreign token");
        let token = token.expose_jwt().to_owned();
        assert!(stack
            .sqld_direct(Some(&token), &ns, "CREATE TABLE foreign_data (v TEXT)")
            .ok());
        assert!(stack
            .sqld_direct(
                Some(&token),
                &ns,
                "INSERT INTO foreign_data VALUES ('FOREIGN-ROW')"
            )
            .ok());
        let config_before = stack.remote_config_raw(&ns);
        assert_eq!(
            stack.remote_key(&ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(*foreign.public_key())
        );

        let first = stack.put_database("target-app", QUOTA);
        assert_eq!(first.status, 409, "{}", first.text());
        assert_eq!(first.json()["code"], "SQLD_CONFLICT");
        let state = stack.get_database("target-app").json();
        assert_eq!(state["database"]["state"], "conflict", "{state}");
        assert_eq!(state["database"]["sqlNamespace"], ns);
        // Nothing of the foreign namespace changed.
        assert_eq!(
            stack.remote_config_raw(&ns),
            config_before,
            "config changed"
        );
        assert_eq!(
            stack.remote_key(&ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(*foreign.public_key())
        );
        let rows = stack.sqld_direct(Some(&token), &ns, "SELECT v FROM foreign_data");
        assert_eq!(rows.cell(), "FOREIGN-ROW");
        assert_eq!(
            stack
                .sqld_direct(Some(&token), &ns, "SELECT count(*) FROM foreign_data")
                .cell(),
            "1"
        );
        // The ledger key never became the namespace's key.
        let row = stack.ledger_row("target-app");
        assert_ne!(
            stack.remote_key(&ns),
            edger_orchestrator::sqld_admin::SqldRemoteKey::Single(row.public_key)
        );
        // The app gets no token and no binding.
        stack.recycle("target-app");
        stack.assert_binding_not_ready("target-app");
        // A repeated PUT stays a conflict and changes nothing.
        let second = stack.put_database("target-app", QUOTA);
        assert_eq!(second.status, 409, "{}", second.text());
        assert_eq!(
            stack.get_database("target-app").json()["database"]["state"],
            "conflict"
        );
        assert_eq!(stack.remote_config_raw(&ns), config_before);
        assert_eq!(
            stack
                .sqld_direct(Some(&token), &ns, "SELECT count(*) FROM foreign_data")
                .cell(),
            "1"
        );
        stack.recycle("target-app");
        stack.assert_binding_not_ready("target-app");
        // Positive control: an app whose namespace is free provisions normally.
        let fine = stack.put_database("fine-app", QUOTA);
        assert_eq!(fine.status, 200, "{}", fine.text());
        assert_eq!(fine.json()["database"]["state"], "ready");
    });
}

// ── token renewal ──

/// The 12 h TTL and the 1 h renewal window cannot be waited for, and the
/// binary exposes no clock. Using only public surfaces: the ledger's sealed
/// key is opened with the test's own signer master, and tokens that are valid
/// for the right app but near expiry (or already expired) are written to the
/// app's vault through the root secrets API. The next spawn must replace them.
#[test]
#[ignore = "requires the real edger binary, SQLD 0.24.32 (EDGER_SQLD_BIN) and Deno 2.9.8 (EDGER_DENO_BIN)"]
fn a_near_expiry_or_expired_own_token_is_renewed_on_the_next_spawn() {
    use edger_orchestrator::sqld_signer::SqldSigningKey;
    scenario(64 * MIB, |stack| {
        stack.install_deno("renewing", "[]", "");
        stack.provision("renewing", QUOTA);
        let ns = stack.namespace_of("renewing");
        let original = stack.token_of("renewing");
        let original_exp = jwt_claims(&original)["exp"].as_i64().unwrap();
        let now = unix_now();
        assert!(
            (now + 11 * 3600..=now + 12 * 3600 + 60).contains(&original_exp),
            "a fresh token lasts about 12 h, exp {original_exp} now {now}"
        );
        let key = stack.ledger_signing_key("renewing");
        let token_with_remaining = |remaining: i64| {
            // ttl is fixed at 12 h; shift the "now" the token is minted at.
            key.sign_scoped_token(&ns, unix_now() + remaining - 12 * 3600, 12 * 3600)
                .unwrap()
                .expose_jwt()
                .to_owned()
        };
        let spawn_token = |stack: &mut Stack| {
            stack.recycle("renewing");
            let token = stack.token_of("renewing");
            assert!(
                stack.sql("renewing", "SELECT 1").ok(),
                "the injected token must work"
            );
            token
        };

        // Negative control: 6 h left is outside the renewal window, kept as is.
        let healthy = token_with_remaining(6 * 3600);
        assert_eq!(
            stack.put_secret("renewing", "SQLD_TOKEN", &healthy).status,
            200
        );
        stack.canaries.push(healthy.clone());
        assert_eq!(
            spawn_token(stack),
            healthy,
            "a token with 6 h left must not be renewed"
        );

        // Positive: 30 min left (inside the last hour) => replaced by a ~12 h token.
        let near = token_with_remaining(30 * 60);
        assert_eq!(
            stack.put_secret("renewing", "SQLD_TOKEN", &near).status,
            200
        );
        stack.canaries.push(near.clone());
        let renewed = spawn_token(stack);
        assert_ne!(renewed, near, "a near-expiry token must be renewed");
        let exp = jwt_claims(&renewed)["exp"].as_i64().unwrap();
        assert!(exp >= unix_now() + 11 * 3600, "renewed exp {exp}");
        assert_eq!(jwt_claims(&renewed)["p"]["rw"]["ns"][0], ns);

        // Positive: an already expired (but own, authenticated) token is renewed too.
        let expired = token_with_remaining(-3600);
        assert_eq!(
            stack.put_secret("renewing", "SQLD_TOKEN", &expired).status,
            200
        );
        stack.canaries.push(expired.clone());
        let again = spawn_token(stack);
        assert_ne!(again, expired);
        assert!(jwt_claims(&again)["exp"].as_i64().unwrap() >= unix_now() + 11 * 3600);

        // Negative: a token signed by another key is not "own": it is not
        // renewed into a working one, and it is preserved or refused, never
        // silently replaced by a token for this app.
        let stranger = SqldSigningKey::generate()
            .unwrap()
            .sign_scoped_token(&ns, unix_now() - 12 * 3600 + 1800, 12 * 3600)
            .unwrap()
            .expose_jwt()
            .to_owned();
        assert_eq!(
            stack.put_secret("renewing", "SQLD_TOKEN", &stranger).status,
            200
        );
        stack.canaries.push(stranger.clone());
        stack.recycle("renewing");
        let after = stack.app_json("renewing", "tok");
        // Fail closed: the foreign token is neither injected nor replaced by
        // a token of this app.
        assert!(after["token"].is_null(), "{after}");
    });
}

#[test]
#[ignore = "requires the real edger binary, SQLD and Deno; writes approximately 1 GiB"]
fn five_gib_budget_and_one_gib_guest_fill_preserve_other_apps() {
    let started = Instant::now();
    let disk_free = || {
        let output = Command::new("df").args(["-h", "/"]).output().unwrap();
        assert!(output.status.success());
        eprintln!("capacity-free: {}", String::from_utf8_lossy(&output.stdout));
    };
    disk_free();
    let mut data_dir = None;
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scenario(5 * 1024 * MIB, |stack| {
            data_dir = Some(stack.root.path().join("sqld"));
            for (name, quota) in [
                ("large-a", 2048 * MIB),
                ("large-b", 2048 * MIB),
                ("fill", 1024 * MIB),
            ] {
                stack.install_deno(name, "[]", "maxProcesses: 1\nttl: 1h\n");
                stack.provision(name, quota);
                assert!(stack.sql(name, "CREATE TABLE big (v BLOB)").ok());
            }
            let before: Vec<_> = ["large-a", "large-b", "fill"]
                .iter()
                .map(|name| stack.get_database(name).json()["database"].clone())
                .collect();
            stack.install_deno("overflow", "[]", "");
            let refused = stack.put_database(
                "overflow",
                edger_orchestrator::sqld_registry::MIN_QUOTA_BYTES,
            );
            assert_eq!(refused.status, 409, "{}", refused.text());
            assert_eq!(refused.json()["code"], "SQLD_CONFLICT");
            assert!(stack.get_database("overflow").json()["database"].is_null());
            for (name, old) in ["large-a", "large-b", "fill"].iter().zip(before) {
                assert_eq!(stack.get_database(name).json()["database"], old);
            }
            let mut accepted = 0_u64;
            let mut rounds = 0;
            loop {
                assert!(
                    started.elapsed() < Duration::from_secs(780),
                    "fill exceeded 13 minutes"
                );
                let write = stack.sql("fill", "INSERT INTO big VALUES (randomblob(1048576))");
                if !write.ok() {
                    assert!(
                        write
                            .error
                            .as_deref()
                            .is_some_and(|e| e.contains("SQLITE_FULL")),
                        "{write:?}"
                    );
                    break;
                }
                accepted += MIB;
                rounds += 1;
                assert!(accepted <= 1024 * MIB, "quota failed to stop writes");
                for name in ["large-a", "large-b"] {
                    assert!(stack
                        .sql(name, "INSERT INTO big VALUES (randomblob(16))")
                        .ok());
                    assert_eq!(
                        stack.sql(name, "SELECT count(*) FROM big").cell(),
                        rounds.to_string()
                    );
                }
            }
            assert!(
                accepted >= 1000 * MIB,
                "only {accepted} guest bytes written"
            );
            assert_eq!(
                stack.sql("fill", "SELECT sum(length(v)) FROM big").cell(),
                accepted.to_string()
            );
            for name in ["large-a", "large-b"] {
                assert!(stack
                    .sql(name, "INSERT INTO big VALUES (randomblob(16))")
                    .ok());
                assert_eq!(
                    stack.sql(name, "SELECT count(*) FROM big").cell(),
                    (rounds + 1).to_string()
                );
            }
            let size = Command::new("du")
                .arg("-sB1")
                .arg(stack.root.path().join("sqld"))
                .output()
                .unwrap();
            assert!(size.status.success());
            eprintln!(
                "capacity-evidence: guest_bytes={accepted}; sqld_allocated_bytes_and_path={}",
                String::from_utf8_lossy(&size.stdout).trim()
            );
        });
    }));
    disk_free();
    if let Some(path) = data_dir {
        assert!(!path.exists(), "data directory remains: {}", path.display());
        eprintln!("capacity-cleanup: removed {}", path.display());
    }
    if let Err(panic) = attempt {
        std::panic::resume_unwind(panic);
    }
    assert!(
        started.elapsed() < Duration::from_secs(900),
        "capacity test exceeded 15 minutes"
    );
    eprintln!("capacity-duration: seconds={}", started.elapsed().as_secs());
}

#[test]
#[ignore = "requires the real edger binary, SQLD and Deno"]
fn own_token_is_renewed_while_the_same_deno_process_keeps_its_old_env() {
    use edger_orchestrator::app_secrets::AppSecretVault;
    scenario_with(64 * MIB, true, |stack| {
        let name = "live-renewal";
        stack.install_deno(name, "[]", "maxProcesses: 1\nttl: 1h\n");
        stack.provision(name, QUOTA);
        let before = stack.data().forwarded().len();
        assert!(stack.sql(name, "SELECT 1").ok());
        let original = stack.token_of(name);
        let original_hash = sha256_hex(format!("Bearer {original}").as_bytes());
        let initial = stack.data().forwarded();
        assert_eq!(initial.len(), before + 1, "initial SQLD request");
        assert_eq!(initial[before].0.as_ref(), Some(&original_hash));
        let namespace_hash = sha256_hex(b64(stack.namespace_of(name).as_bytes()).as_bytes());
        assert_eq!(initial[before].1.as_ref(), Some(&namespace_hash));
        let pid = stack.state(name)["pid"].clone();
        let process_id = |stack: &Stack| {
            let stats = stack.root_call("GET", "/metrics/stats", "").json();
            let worker = stats["workers"]
                .as_array()
                .unwrap()
                .iter()
                .find(|worker| worker["name"] == name)
                .expect("worker metrics");
            let processes = worker["processes"].as_array().unwrap();
            assert_eq!(processes.len(), 1);
            processes[0]["id"].as_str().expect("process id").to_owned()
        };
        let id = process_id(stack);
        let vault = AppSecretVault::open(
            stack.root.path().join("state/vault.db"),
            zeroize::Zeroizing::new(vault_key()),
        )
        .unwrap();
        let ns = stack.namespace_of(name);
        let key = stack.ledger_signing_key(name);
        // Tokens minted in the same second have identical claims/signatures.
        // Advance past the original issuance before requiring a new digest.
        let issued = jwt_claims(&original)["exp"].as_i64().unwrap() - 12 * 3600;
        let deadline = Instant::now() + Duration::from_secs(5);
        while unix_now() <= issued {
            assert!(
                Instant::now() < deadline,
                "clock did not advance past issuance"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        for remaining in [1800, -3600] {
            let stale = key
                .sign_scoped_token(&ns, unix_now() + remaining - 12 * 3600, 12 * 3600)
                .unwrap()
                .expose_jwt()
                .to_owned();
            stack.canaries.push(stale.clone());
            assert_eq!(stack.put_secret(name, "SQLD_TOKEN", &stale).status, 200);
            let before = stack.data().forwarded().len();
            assert!(
                stack.sql(name, "SELECT 1").ok(),
                "live guard renewal, remaining={remaining}"
            );
            let renewed = vault.get(name, "SQLD_TOKEN").expect("renewed vault token");
            assert!(renewed.as_str() != stale, "vault kept stale token");
            assert!(
                jwt_claims(&renewed)["exp"].as_i64().unwrap()
                    > jwt_claims(&stale)["exp"].as_i64().unwrap()
            );
            stack.canaries.push(renewed.to_string());
            let renewed_hash = sha256_hex(format!("Bearer {}", renewed.as_str()).as_bytes());
            assert_ne!(renewed_hash, original_hash, "renewal changed credential");
            let forwarded = stack.data().forwarded();
            assert_eq!(forwarded.len(), before + 1, "renewal SQLD request");
            assert_eq!(
                forwarded[before].0.as_ref(),
                Some(&renewed_hash),
                "SQLD must receive renewed vault credential, remaining={remaining}"
            );
            assert_eq!(forwarded[before].1.as_ref(), Some(&namespace_hash));
            assert!(stack.token_of(name) == original, "guest env changed");
            assert_eq!(process_id(stack), id);
            assert_eq!(stack.state(name)["pid"], pid);
        }
        let manual = "manual-not-a-managed-jwt";
        stack.canaries.push(manual.to_owned());
        assert_eq!(stack.put_secret(name, "SQLD_TOKEN", manual).status, 200);
        let before = stack.data().forwarded().len();
        assert!(
            !stack.sql(name, "SELECT 1").ok(),
            "manual token must fail closed"
        );
        assert_eq!(
            stack.data().forwarded().len(),
            before,
            "manual token rejected before reaching SQLD"
        );
        assert!(vault.get(name, "SQLD_TOKEN").unwrap().as_str() == manual);
        assert_eq!(process_id(stack), id);
        assert_eq!(stack.state(name)["pid"], pid);
    });
}

#[test]
#[ignore = "one-off large SQLD backup diagnostic; excluded from regression by --skip diagnostic_"]
fn diagnostic_one_gib_backup_progress_and_other_app_latency() {
    let started = Instant::now();
    let mut root = None;
    scenario(2 * 1024 * MIB, |stack| {
        root = Some(stack.root.path().to_owned());
        eprintln!("diagnostic-root: {}", stack.root.path().display());
        for name in ["diagnostic-fill", "diagnostic-other"] {
            stack.install_deno(name, "[]", "maxProcesses: 1\nttl: 1h\n");
            stack.provision(name, 1024 * MIB);
        }
        assert!(stack
            .sql("diagnostic-fill", "CREATE TABLE big (v BLOB)")
            .ok());
        let mut accepted = 0_u64;
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(180),
                "diagnostic fill exceeded three minutes"
            );
            let write = stack.sql(
                "diagnostic-fill",
                "INSERT INTO big VALUES (randomblob(1048576))",
            );
            if !write.ok() {
                assert!(
                    write
                        .error
                        .as_deref()
                        .is_some_and(|e| e.contains("SQLITE_FULL")),
                    "{write:?}"
                );
                break;
            }
            accepted += MIB;
            assert!(accepted <= 1024 * MIB);
        }
        assert!(accepted >= 1000 * MIB);
        assert!(stack.sql("diagnostic-other", "SELECT 1").ok());
        eprintln!("diagnostic-fill: guest_bytes={accepted}");
        let backup_started = Instant::now();
        let backups = stack.root.path().join("state/backups");
        std::thread::scope(|scope| {
            let port = stack.port;
            let backup = scope.spawn(move || {
                let response = http_with_timeout(
                    port,
                    "POST",
                    "/api/admin/workers/diagnostic-fill/database/backup",
                    &[("Authorization", &format!("Bearer {ROOT_KEY}"))],
                    b"",
                    Duration::from_secs(1900),
                );
                let elapsed = backup_started.elapsed();
                match &response {
                    Ok(reply) => {
                        let value: serde_json::Value =
                            serde_json::from_slice(&reply.body).unwrap_or_default();
                        eprintln!(
                            "diagnostic-response: elapsed_ms={} status={} code={}",
                            elapsed.as_millis(),
                            reply.status,
                            value["code"]
                        );
                    }
                    Err(error) => eprintln!(
                        "diagnostic-response: elapsed_ms={} transport_error={error}",
                        elapsed.as_millis()
                    ),
                }
                response
            });
            let mut probes = Vec::new();
            let mut previous = 0_u64;
            for tick in 0..=12 {
                let target = backup_started + Duration::from_secs(tick * 10);
                std::thread::sleep(target.saturating_duration_since(Instant::now()));
                let mut sizes = Vec::new();
                for entry in fs::read_dir(&backups).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_name().to_string_lossy().starts_with(".tmp-") {
                        match entry.metadata() {
                            Ok(metadata) => sizes.push(metadata.len()),
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => panic!("diagnostic temporary metadata: {error}"),
                        }
                    }
                }
                let total: u64 = sizes.iter().sum();
                eprintln!(
                    "diagnostic-progress: elapsed_ms={} tmp_files={} tmp_bytes={} delta_bytes={}",
                    backup_started.elapsed().as_millis(),
                    sizes.len(),
                    total,
                    total as i128 - previous as i128
                );
                previous = total;
                if tick == 1 {
                    for (label, path, admin) in [
                        ("other-query", "/diagnostic-other/q?sql=SELECT%201", false),
                        (
                            "other-admin",
                            "/api/admin/workers/diagnostic-other/database",
                            true,
                        ),
                    ] {
                        probes.push(scope.spawn(move || {
                            let at = Instant::now();
                            let authorization = format!("Bearer {ROOT_KEY}");
                            let headers = if admin { vec![("Authorization", authorization.as_str())] } else { vec![] };
                            match http(port, "GET", path, &headers, b"") {
                                Ok(reply) => {
                                    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap_or_default();
                                    let success = if admin { reply.status == 200 && body["database"]["state"] == "ready" }
                                        else { reply.status == 200 && Sql::parse(body["status"].as_u64().unwrap_or(0), body["text"].as_str().unwrap_or("")).ok() };
                                    eprintln!("diagnostic-probe: kind={label} elapsed_ms={} status={} success={success}", at.elapsed().as_millis(), reply.status);
                                }
                                Err(error) => eprintln!("diagnostic-probe: kind={label} elapsed_ms={} transport_error={error}", at.elapsed().as_millis()),
                            }
                        }));
                    }
                }
            }
            for probe in probes {
                probe.join().unwrap();
            }
            if let Ok(reply) = backup.join().unwrap() {
                if reply.status == 200 {
                    let value = reply.json();
                    let file = backups.join(value["backup"]["fileName"].as_str().unwrap());
                    let bytes = fs::metadata(file).unwrap().len();
                    assert_eq!(value["backup"]["bytes"], bytes);
                    eprintln!("diagnostic-artifact: bytes={bytes}");
                }
            }
        });
        assert!(
            started.elapsed() < Duration::from_secs(2100),
            "diagnostic exceeded 35 minutes"
        );
    });
    let root = root.unwrap();
    assert!(!root.exists());
    eprintln!("diagnostic-cleanup: removed {}", root.display());
}

// ── the crash helper itself (this test executable as the child: no edger, no runtime, no external binary) ──

fn child_tree(mode: &str, label: &str) -> (tempfile::TempDir, SubreaperTree) {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            HELPER_TEST,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, format!("{CHILD_SENTINEL}{mode}"))
        .current_dir(root.path());
    let tree = SubreaperTree::start(&command, root.path(), label).expect("start under a subreaper");
    (root, tree)
}

fn wait_until_exited(tree: &mut SubreaperTree) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(reason) = tree.exited() {
            return reason;
        }
        assert!(Instant::now() < deadline, "the child never exited");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn crash_is_refused_when_the_child_already_exited_on_its_own() {
    for (script, mode, clean) in [("exit 0", "exit:0", true), ("exit 3", "exit:3", false)] {
        let (_root, mut tree) = child_tree(mode, "exited-alone");
        let reason = wait_until_exited(&mut tree);
        let error = tree
            .crash_edger()
            .expect_err("a crash command must fail for a child that is already gone");
        assert!(
            error
                .to_string()
                .contains("already exited before the crash command"),
            "`{script}` ({reason}): {error}"
        );
        // The stop afterwards tells a clean exit from an unexpected one.
        let report = tree.stop().expect("subreaper cleanup protocol");
        assert_eq!(
            report.ok(),
            clean,
            "`{script}`: an unexpected exit status must not count as a clean stop: {report:?}"
        );
    }
}

#[test]
fn crash_reports_success_only_for_an_observed_sigkill() {
    let (_root, mut tree) = child_tree("block", "sleeper");
    let proof = tree.crash_edger().expect("SIGKILL of a live child");
    assert!(proof.contains("signal: 9"), "{proof}");
    let report = tree.stop().expect("subreaper cleanup protocol");
    assert!(report.ok(), "{report:?}");
    // The kill happened once: a second command finds the child gone and fails.
}
