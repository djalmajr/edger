//! EDG-55 S4: the host-held SSE proved through the REAL `edger` binary, with a
//! real Go/WASM guest (`workers/examples/sse-go-wasm`) and a real Deno guest
//! (`workers/examples/sse-deno`).
//!
//! The claim under test: many browsers are served WITHOUT an execution held
//! per connection. Every app runs with `maxProcesses: 1`, so an ordinary
//! request answering while 20 subscribers are open is the proof that no
//! connection keeps the single process (or WASM instance) busy.
//!
//! The real-runtime tests are `#[ignore]` and never skip: a missing
//! `EDGER_DENO_BIN` / `EDGER_GO_WASM_SSE_FIXTURE` is a panic in an
//! `--ignored` run. Every process of a scenario (the `edger` binary and
//! whatever it spawns) lives under its own Linux child-subreaper helper, so
//! the cleanup is proven from the kernel's process tree. Authorization and
//! revocation use the real path: `EDGER_SECRETS_KEY_FILE`, the admin secret
//! route and the version `recycle`; there is no hook in production code.
//!
//! The ordinary (non-ignored) tests at the end check the test's own readers
//! (chunk decoder, SSE frame parser, metric reader) against known inputs.
//!
//! Run (WSL Linux):
//! `EDGER_DENO_BIN=... EDGER_GO_WASM_SSE_FIXTURE=... cargo test --offline --locked \
//!   -p edger-orchestrator --test sse_binary_e2e -- --ignored --test-threads=1`
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

// ---------------------------------------------------------------------------
// Process-tree cleanup (copied from the EDG-54 `sqld_managed_cli_e2e.rs`
// model): the `edger` binary is started by an `#[ignore]`d helper test of this
// same file that becomes a Linux child subreaper first, so every descendant of
// EdgeR, including one adopted after EdgeR dies, stays in a tree that only this
// fixture owns. The tree is read from the kernel
// (`/proc/<helper>/task/*/children`), never from a scan of the host's PIDs or
// command lines.
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
        .stdout(fs::File::create(
            spec["stdout"].as_str().ok_or_else(|| bad("stdout"))?,
        )?)
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
}

struct SubreaperTree {
    edger_pid: u32,
    edger_stderr: PathBuf,
    edger_stdout: PathBuf,
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
        let (spec_path, edger_stderr, edger_stdout, helper_stderr) = (
            root.join(format!("{label}-spec.json")),
            root.join(format!("{label}.stderr")),
            root.join(format!("{label}.stdout")),
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
            "stdout": edger_stdout.to_str().ok_or_else(|| io::Error::other("non-UTF-8 path"))?,
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
            edger_stdout,
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
        // The `edger` log goes to stdout, startup failures to stderr.
        Some(format!(
            "{first}: {} {}",
            one_line(fs::read_to_string(&self.edger_stderr).unwrap_or_default()),
            one_line(fs::read_to_string(&self.edger_stdout).unwrap_or_default())
        ))
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
// EDG-55 S4 proper: constants, small HTTP/SSE readers, the stack and the
// scenarios.
// ---------------------------------------------------------------------------

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};

const ROOT_KEY: &str = "sse-e2e-root-key";
const TOKEN_ONE: &str = "sse-e2e-token-one";
const TOKEN_TWO: &str = "sse-e2e-token-two";
const ROOM: &str = "room.1";
/// `x-edger-sse-publish` headers one publish response may carry.
const MAX_DIRECTIVES: usize = 8;
const STEP: Duration = Duration::from_secs(20);
/// The declared bound for SIGTERM with open streams to end the binary.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);

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

/// The exact command that built the Go module (the Windows toolchain builds
/// it, the test runs in WSL, so the link is a provenance manifest).
const GO_BUILD_COMMAND: &str = "GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o sse-go-wasm.wasm .";

/// The sources of the Go example that the provenance manifest covers.
const GO_SOURCES: [&str; 3] = ["abi.go", "main.go", "go.mod"];

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Reads `<module>.provenance` whole and checks it strictly. It holds exactly
/// one `<64 lowercase hex>  <name>` record for each of `abi.go`, `main.go`,
/// `go.mod` and the module file name (a bare name, no path), exactly one
/// `build: <command>` line equal to `GO_BUILD_COMMAND`, and nothing else: no
/// blank line, no duplicate, no unexpected name, no extra content. Only the
/// terminator of the last line is accepted. Every hash is recomputed over the
/// example sources and the module; the module bytes returned are the very
/// bytes that were hashed.
fn verified_go_module(module: &Path, example_dir: &Path) -> Result<Vec<u8>, String> {
    let manifest_path = PathBuf::from(format!("{}.provenance", module.display()));
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "provenance manifest {} is missing or unreadable: {error}",
            manifest_path.display()
        )
    })?;
    let wasm = fs::read(module).map_err(|error| format!("cannot read the Go module: {error}"))?;
    let module_name = module
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or("the Go module path has no file name")?;
    let mut expected: Vec<(String, String)> = Vec::new();
    for name in GO_SOURCES {
        let bytes = fs::read(example_dir.join(name))
            .map_err(|error| format!("cannot read the example source {name}: {error}"))?;
        expected.push((name.to_owned(), sha256_hex(&bytes)));
    }
    expected.push((module_name, sha256_hex(&wasm)));

    // One terminator at the end of the last line is fine; anything else that
    // makes an empty line (a blank line, a second terminator) is refused.
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let mut build: Option<&str> = None;
    let mut hashes: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for (index, line) in body.split('\n').enumerate() {
        let number = index + 1;
        if line.is_empty() {
            return Err(format!("provenance line {number} is blank"));
        }
        if let Some(command) = line.strip_prefix("build: ") {
            if build.replace(command).is_some() {
                return Err("provenance has two build lines".into());
            }
            continue;
        }
        let malformed = || {
            format!("provenance line {number} is not `<64 lowercase hex>  <name>` or `build: <command>`")
        };
        let (hash, rest) = line.split_at_checked(64).ok_or_else(malformed)?;
        let name = rest.strip_prefix("  ").ok_or_else(malformed)?;
        if !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || name.is_empty()
            || name.contains(char::is_whitespace)
        {
            return Err(malformed());
        }
        if name.contains('/') || name.contains('\\') {
            return Err(format!("provenance name {name} has a path"));
        }
        if !expected.iter().any(|(known, _)| known == name) {
            return Err(format!("provenance has an unexpected name {name}"));
        }
        if hashes.insert(name, hash).is_some() {
            return Err(format!("provenance has two lines for {name}"));
        }
    }
    let build = build.ok_or("provenance has no `build:` line")?;
    if build != GO_BUILD_COMMAND {
        return Err(format!(
            "provenance build line differs from the expected command: {build}"
        ));
    }
    for (name, actual) in &expected {
        match hashes.get(name.as_str()) {
            None => return Err(format!("provenance has no line for {name}")),
            Some(recorded) if recorded != actual => {
                return Err(format!(
                    "{name} differs from the provenance (it changed since the build): recorded {recorded}, found {actual}"
                ))
            }
            Some(_) => {}
        }
    }
    Ok(wasm)
}

/// The Go module named by `EDGER_GO_WASM_SSE_FIXTURE`, bound to the example
/// sources of this worktree by its provenance manifest.
fn go_module() -> Vec<u8> {
    let module = require_file("EDGER_GO_WASM_SSE_FIXTURE");
    verified_go_module(&module, &Backend::Go.example_dir())
        .unwrap_or_else(|error| panic!("the Go module is not bound to the sources: {error}"))
}

fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ───────────────────────────── plain HTTP/1.0 client ─────────────────────────────

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
        header_of(&self.headers, name)
    }
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn dechunk(raw: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = ChunkDecoder::default();
    let out = decoder.feed(raw);
    if decoder.ended {
        Ok(out)
    } else {
        Err(io::Error::other("invalid or truncated chunked body"))
    }
}

/// HTTP/1.0 with `Connection: close`: the body ends at EOF.
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<Reply> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
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
    let (status, headers) = parse_head(&raw[..split])?;
    let mut body = raw[split + 4..].to_vec();
    if is_chunked(&headers) {
        body = dechunk(&body)?;
    }
    Ok(Reply {
        body,
        headers,
        status,
    })
}

fn parse_head(head: &[u8]) -> io::Result<(u16, Vec<(String, String)>)> {
    let text = String::from_utf8_lossy(head).into_owned();
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other("invalid status line"))?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    Ok((status, headers))
}

fn is_chunked(headers: &[(String, String)]) -> bool {
    header_of(headers, "transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
}

fn zip_package(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    for (name, content) in files {
        writer
            .start_file(name.as_str(), zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

// ───────────────────────────── SSE readers ─────────────────────────────

/// Incremental `Transfer-Encoding: chunked` decoder. `ended` is true only
/// once the whole message is in: the zero-size chunk line, the trailer section
/// (read and discarded) and the final empty line (CRLF).
#[derive(Default)]
struct ChunkDecoder {
    ended: bool,
    raw: Vec<u8>,
    /// The zero-size chunk was read; trailer lines (if any) and the final
    /// empty line are still to come.
    trailers: bool,
}

impl ChunkDecoder {
    fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        self.raw.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if self.ended {
                self.raw.clear();
                break;
            }
            let Some(eol) = self.raw.windows(2).position(|pair| pair == b"\r\n") else {
                break;
            };
            if self.trailers {
                let last = eol == 0;
                self.raw.drain(..eol + 2);
                self.ended = last;
                continue;
            }
            let line = String::from_utf8_lossy(&self.raw[..eol]).into_owned();
            let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)
                .unwrap_or_else(|_| panic!("invalid chunk size line {line:?}"));
            if size == 0 {
                self.raw.drain(..eol + 2);
                self.trailers = true;
                continue;
            }
            if self.raw.len() < eol + 2 + size + 2 {
                break;
            }
            out.extend_from_slice(&self.raw[eol + 2..eol + 2 + size]);
            self.raw.drain(..eol + 2 + size + 2);
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ev {
    data: String,
    event: Option<String>,
    id: Option<String>,
}

impl Ev {
    fn new(data: &str, event: Option<&str>, id: Option<&str>) -> Self {
        Self {
            data: data.to_owned(),
            event: event.map(str::to_owned),
            id: id.map(str::to_owned),
        }
    }
}

/// How a held stream ended, as the reader saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum End {
    /// The HTTP body finished: the terminating zero-length chunk arrived.
    Completed,
    /// The socket reached EOF before the terminating chunk (truncated body).
    PrematureEof,
    /// A read failed (reset, timeout, ...).
    ReadError(String),
    /// The reader thread died without reporting an end.
    ReaderFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Frame {
    Comment(String),
    End(End),
    Event(Ev),
    Retry(u64),
}

#[derive(Default)]
struct FrameParser {
    text: Vec<u8>,
}

impl FrameParser {
    fn feed(&mut self, data: &[u8]) -> Vec<Frame> {
        self.text.extend_from_slice(data);
        let mut frames = Vec::new();
        while let Some(end) = self.text.windows(2).position(|pair| pair == b"\n\n") {
            let block = String::from_utf8(self.text[..end].to_vec()).expect("UTF-8 SSE block");
            self.text.drain(..end + 2);
            frames.push(parse_block(&block));
        }
        frames
    }
}

fn parse_block(block: &str) -> Frame {
    let mut data: Vec<&str> = Vec::new();
    let (mut event, mut id, mut retry) = (None, None, None);
    for line in block.split('\n') {
        if let Some(comment) = line.strip_prefix(':') {
            return Frame::Comment(comment.trim().to_owned());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => data.push(value),
            "event" => event = Some(value.to_owned()),
            "id" => id = Some(value.to_owned()),
            "retry" => retry = value.parse().ok(),
            other => panic!("unexpected SSE field {other:?} in {block:?}"),
        }
    }
    match retry {
        Some(ms) if data.is_empty() && event.is_none() && id.is_none() => Frame::Retry(ms),
        _ => Frame::Event(Ev {
            data: data.join("\n"),
            event,
            id,
        }),
    }
}

/// One `edger_sse_*` sample: the sum of the lines of `name` whose labels carry
/// the worker (and the reason, when given). A missing series reads as 0.
fn metric_value(text: &str, name: &str, worker: &str, reason: Option<&str>) -> f64 {
    let worker_label = format!("worker=\"{worker}\"");
    let reason_label = reason.map(|reason| format!("reason=\"{reason}\""));
    text.lines()
        .filter(|line| line.starts_with(name) && line[name.len()..].starts_with('{'))
        .filter(|line| line.contains(&worker_label))
        .filter(|line| {
            reason_label
                .as_ref()
                .is_none_or(|label| line.contains(label))
        })
        .filter_map(|line| line.rsplit_once(' ')?.1.parse::<f64>().ok())
        .sum()
}

/// A held SSE connection. The head is parsed on `open`; for a 200 a reader
/// thread decodes the stream into frames, for anything else the body is read
/// to the end.
struct Sub {
    body: Vec<u8>,
    comments: usize,
    end: Option<End>,
    events: Vec<Ev>,
    headers: Vec<(String, String)>,
    retry: Option<u64>,
    rx: Option<Receiver<Frame>>,
    status: u16,
    stream: TcpStream,
}

/// The reader thread of a held stream: decodes chunks into SSE frames and
/// ends with exactly one `Frame::End` saying how the body finished.
fn run_reader<R: Read>(mut reader: R, first: Vec<u8>, sender: mpsc::Sender<Frame>) {
    let mut decoder = ChunkDecoder::default();
    let mut parser = FrameParser::default();
    let mut pending = first;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let decoded = decoder.feed(&pending);
        for frame in parser.feed(&decoded) {
            if sender.send(frame).is_err() {
                return;
            }
        }
        if decoder.ended {
            let _ = sender.send(Frame::End(End::Completed));
            return;
        }
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(Frame::End(End::PrematureEof));
                return;
            }
            Err(error) => {
                let _ = sender.send(Frame::End(End::ReadError(error.to_string())));
                return;
            }
            Ok(read) => pending = buffer[..read].to_vec(),
        }
    }
}

impl Sub {
    fn open(port: u16, path: &str, token: Option<&str>) -> Sub {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
        Sub::open_on(stream, path, token, true)
    }

    fn open_on(mut stream: TcpStream, path: &str, token: Option<&str>, read: bool) -> Sub {
        stream.set_read_timeout(Some(STEP)).unwrap();
        let mut head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: close\r\n"
        );
        if let Some(token) = token {
            head.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut raw = Vec::new();
        let mut buffer = [0u8; 4096];
        let split = loop {
            if let Some(split) = raw.windows(4).position(|quad| quad == b"\r\n\r\n") {
                break split;
            }
            let read = stream.read(&mut buffer).expect("response head");
            assert!(read > 0, "connection closed before the response head");
            raw.extend_from_slice(&buffer[..read]);
        };
        let (status, headers) = parse_head(&raw[..split]).expect("response head");
        let rest = raw[split + 4..].to_vec();
        let mut sub = Sub {
            body: Vec::new(),
            comments: 0,
            end: None,
            events: Vec::new(),
            headers,
            retry: None,
            rx: None,
            status,
            stream,
        };
        if status != 200 {
            let mut all = rest;
            sub.stream.read_to_end(&mut all).expect("error body");
            sub.body = if is_chunked(&sub.headers) {
                dechunk(&all).expect("chunked error body")
            } else {
                all
            };
            sub.end = Some(End::Completed);
            return sub;
        }
        assert_eq!(
            header_of(&sub.headers, "content-type"),
            Some("text/event-stream"),
            "{:?}",
            sub.headers
        );
        assert!(is_chunked(&sub.headers), "{:?}", sub.headers);
        if !read {
            return sub;
        }
        sub.stream.set_read_timeout(None).unwrap();
        let reader = sub.stream.try_clone().expect("clone the stream");
        let (sender, receiver) = mpsc::channel();
        sub.rx = Some(receiver);
        std::thread::spawn(move || run_reader(reader, rest, sender));
        sub
    }

    fn record(&mut self, frame: Frame) {
        match frame {
            Frame::Comment(_) => self.comments += 1,
            Frame::End(end) => self.end = Some(end),
            Frame::Event(event) => self.events.push(event),
            Frame::Retry(ms) => self.retry = Some(ms),
        }
    }

    /// One frame, if it arrives within `wait`.
    fn pump(&mut self, wait: Duration) -> bool {
        let Some(receiver) = self.rx.as_ref() else {
            return false;
        };
        match receiver.recv_timeout(wait) {
            Ok(frame) => {
                self.record(frame);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => {
                // The thread always sends its `End` first; a channel that
                // closes without one means the thread died.
                if self.end.is_none() {
                    self.end = Some(End::ReaderFailed);
                }
                false
            }
        }
    }

    fn wait_events(&mut self, count: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while self.events.len() < count {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero() && self.end.is_none(),
                "have {} of {count} events (end={:?}); last: {:?}",
                self.events.len(),
                self.end,
                self.events.last()
            );
            self.pump(left.min(Duration::from_millis(200)));
        }
    }

    /// Reads for `span` and returns how many events arrived meanwhile.
    fn quiet(&mut self, span: Duration) -> usize {
        let before = self.events.len();
        let end = Instant::now() + span;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            self.pump(left.min(Duration::from_millis(100)));
        }
        self.events.len() - before
    }

    /// Waits for the stream to end; `None` when it is still open at the deadline.
    fn wait_end(&mut self, timeout: Duration) -> Option<End> {
        let deadline = Instant::now() + timeout;
        while self.end.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            self.pump(left.min(Duration::from_millis(200)));
        }
        self.end.clone()
    }

    /// The stream must end with the terminating chunk of the HTTP body: a
    /// truncated body, a read error or a dead reader thread is a failure.
    fn expect_completed(&mut self, timeout: Duration, what: &str) {
        match self.wait_end(timeout) {
            Some(End::Completed) => {}
            Some(other) => {
                panic!("{what}: the stream ended with {other:?}, not the terminating chunk")
            }
            None => panic!("{what}: the stream did not end within {timeout:?}"),
        }
    }

    fn has_sse_headers(&self) -> bool {
        self.headers
            .iter()
            .any(|(name, _)| name.to_ascii_lowercase().starts_with("x-edger-sse-"))
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {:?}", self.body))
    }
}

impl Drop for Sub {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// A client with a tiny receive buffer: it sends the request, reads the head
/// and then never reads again.
fn open_stalled(port: u16, path: &str, token: &str) -> Sub {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime for the small-buffer socket");
    let stream = runtime.block_on(async {
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket.set_recv_buffer_size(4096).expect("SO_RCVBUF");
        socket
            .connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
            .await
            .expect("connect")
            .into_std()
            .expect("std stream")
    });
    stream.set_nonblocking(false).unwrap();
    Sub::open_on(stream, path, Some(token), false)
}

// ───────────────────────────── the guests ─────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Backend {
    Deno,
    Go,
}

fn replace_once(text: &str, from: &str, to: &str) -> String {
    assert_eq!(
        text.matches(from).count(),
        1,
        "the example manifest must hold exactly one `{from}`"
    );
    text.replace(from, to)
}

impl Backend {
    fn base(self) -> &'static str {
        match self {
            Backend::Deno => "sse-deno",
            Backend::Go => "sse-go-wasm",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Backend::Deno => "deno",
            Backend::Go => "go",
        }
    }

    fn example_dir(self) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../workers/examples")
            .join(self.base())
    }

    /// The example package with its `sse:` block removed (the manifest keeps
    /// the name `name`); the worker is then a plain one for the host.
    fn package_without_sse(self, name: &str) -> Vec<(String, Vec<u8>)> {
        let mut files = self.package(name, "5m", 256);
        let manifest = String::from_utf8(files[0].1.clone()).unwrap();
        let cut = manifest.find("\nsse:\n").expect("the sse block");
        files[0].1 = manifest.as_bytes()[..=cut].to_vec();
        assert!(!String::from_utf8_lossy(&files[0].1).contains("sse:"));
        files
    }

    /// The example package, with the manifest's name, SSE ttl and ceiling
    /// replaced (each must appear exactly once in the example manifest).
    fn package(self, name: &str, ttl: &str, max_connections: u32) -> Vec<(String, Vec<u8>)> {
        let manifest =
            fs::read_to_string(self.example_dir().join("manifest.yaml")).expect("example manifest");
        let manifest = replace_once(
            &manifest,
            &format!("name: {}", self.base()),
            &format!("name: {name}"),
        );
        let manifest = replace_once(&manifest, "  ttl: 5m", &format!("  ttl: {ttl}"));
        let manifest = replace_once(
            &manifest,
            "  maxConnections: 256",
            &format!("  maxConnections: {max_connections}"),
        );
        let mut files = vec![("manifest.yaml".to_owned(), manifest.into_bytes())];
        match self {
            Backend::Deno => files.push((
                "index.ts".to_owned(),
                fs::read(self.example_dir().join("index.ts")).expect("example source"),
            )),
            Backend::Go => files.push(("index.wasm".to_owned(), go_module())),
        }
        files
    }
}

fn event_json(data: &str, event: Option<&str>, id: Option<&str>) -> serde_json::Value {
    event_json_on(ROOM, data, event, id)
}

fn event_json_on(
    topic: &str,
    data: &str,
    event: Option<&str>,
    id: Option<&str>,
) -> serde_json::Value {
    let mut value = serde_json::json!({"topic": topic, "data": data});
    if let Some(event) = event {
        value["event"] = event.into();
    }
    if let Some(id) = id {
        value["id"] = id.into();
    }
    value
}

// ───────────────────────────── the stack ─────────────────────────────

struct Stack {
    backend: Backend,
    deno: Option<PathBuf>,
    edger: Option<SubreaperTree>,
    env: Vec<(String, String)>,
    port: u16,
    reports: Vec<CleanupReport>,
    root: tempfile::TempDir,
    runs: u32,
}

impl Stack {
    fn new(backend: Backend, env: &[(&str, &str)]) -> Self {
        let deno = match backend {
            Backend::Deno => Some(require_file("EDGER_DENO_BIN")),
            Backend::Go => {
                go_module();
                None
            }
        };
        let root = tempfile::Builder::new()
            .prefix("edger-sse-e2e-")
            .tempdir()
            .unwrap();
        for dir in ["workers", "core", "state", "keys", "home"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        for dir in ["state", "keys"] {
            fs::set_permissions(root.path().join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let key = root.path().join("keys/vault");
        fs::write(&key, vault_key()).unwrap();
        fs::set_permissions(key, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            backend,
            deno,
            edger: None,
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            port: free_port(),
            reports: Vec::new(),
            root,
            runs: 0,
        }
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
            .env("EDGER_SSE_HEARTBEAT_MS", "1000");
        if let Some(deno) = &self.deno {
            cmd.env("EDGER_DENO_BIN", deno);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd
    }

    fn start(&mut self) {
        assert!(self.edger.is_none(), "an EdgeR run is already live");
        self.runs += 1;
        let label = format!("edger-{}", self.runs);
        let mut tree = SubreaperTree::start(&self.edger_command(), self.root.path(), &label)
            .expect("start EdgeR under a subreaper");
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

    /// Stops everything, then proves every recorded process is absent and
    /// that no token reached a log.
    fn finish(&mut self) -> Result<(), String> {
        let mut problems = Vec::new();
        if let Some(mut tree) = self.edger.take() {
            match tree.stop() {
                Ok(report) => self.reports.push(report),
                Err(error) => problems.push(format!("subreaper stop failed: {error}")),
            }
        }
        for report in &self.reports {
            if !report.ok() {
                problems.push(format!("cleanup unproven: {report:?}"));
            }
            for (pid, outcome, start, exe) in &report.entries {
                if !["exited", "killed", "gone"].contains(&outcome.as_str()) {
                    problems.push(format!("{pid} {exe} outcome {outcome}"));
                }
                if let Err(error) = process_absent(*pid, start, Duration::from_secs(5)) {
                    problems.push(format!("{pid} {exe}: {error}"));
                }
            }
        }
        for entry in fs::read_dir(self.root.path()).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_none_or(|ext| ext != "stderr" && ext != "stdout")
            {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            for secret in [TOKEN_ONE, TOKEN_TWO, ROOT_KEY] {
                if bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes())
                {
                    problems.push(format!("a secret value is in {}", path.display()));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
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

    fn put_secret(&self, name: &str, value: &str) {
        let reply = self.root_call(
            "PUT",
            &format!("/api/admin/workers/{name}/secrets/SSE_ACCESS_TOKEN"),
            &serde_json::json!({ "value": value }).to_string(),
        );
        assert_eq!(reply.status, 200, "secret of {name}: {}", reply.text());
    }

    fn install(&self, files: &[(String, Vec<u8>)]) {
        let reply = http(
            self.port,
            "POST",
            "/api/admin/workers/install",
            &[
                ("Authorization", &format!("Bearer {ROOT_KEY}")),
                ("Content-Type", "application/zip"),
            ],
            &zip_package(files),
        )
        .expect("install request");
        assert!(
            (200..300).contains(&reply.status),
            "install answered {}: {}",
            reply.status,
            reply.text()
        );
    }

    /// Writes the secret first (it is applied by the first spawn), then
    /// installs the example under `name`.
    fn deploy(&self, name: &str, ttl: &str, max_connections: u32, token: &str) {
        self.put_secret(name, token);
        self.install(&self.backend.package(name, ttl, max_connections));
    }

    fn recycle(&self, name: &str) {
        let reply = self.root_call(
            "POST",
            &format!("/api/admin/workers/{name}/recycle?version=1.0.0"),
            "",
        );
        assert_eq!(reply.status, 200, "recycle {name}: {}", reply.text());
    }

    /// The real revocation path: a new secret value, then a recycle.
    fn rotate(&self, name: &str, token: &str) {
        self.put_secret(name, token);
        self.recycle(name);
    }

    fn get(&self, app: &str, route: &str, token: Option<&str>) -> Reply {
        let bearer = token.map(|token| format!("Bearer {token}"));
        let mut headers = Vec::new();
        if let Some(bearer) = bearer.as_deref() {
            headers.push(("Authorization", bearer));
        }
        http(self.port, "GET", &format!("/{app}/{route}"), &headers, b"")
            .unwrap_or_else(|error| panic!("GET /{app}/{route}: {error}"))
    }

    fn publish(&self, app: &str, token: Option<&str>, events: &[serde_json::Value]) -> Reply {
        self.publish_body(app, token, &serde_json::json!({ "events": events }))
    }

    /// `POST /{app}/publish` with an arbitrary control body.
    fn publish_body(&self, app: &str, token: Option<&str>, body: &serde_json::Value) -> Reply {
        let bearer = token.map(|token| format!("Bearer {token}"));
        let mut headers = vec![("Content-Type", "application/json")];
        if let Some(bearer) = bearer.as_deref() {
            headers.push(("Authorization", bearer));
        }
        http(
            self.port,
            "POST",
            &format!("/{app}/publish"),
            &headers,
            body.to_string().as_bytes(),
        )
        .unwrap_or_else(|error| panic!("POST /{app}/publish: {error}"))
    }

    /// Publishes `events` in order, in as few responses as the backend can
    /// carry (the contract allows 8 per response).
    fn publish_ok(&self, app: &str, events: &[serde_json::Value]) {
        for chunk in events.chunks(MAX_DIRECTIVES) {
            let reply = self.publish(app, Some(TOKEN_ONE), chunk);
            assert_eq!(reply.status, 200, "publish: {}", reply.text());
            assert_eq!(reply.json()["published"], chunk.len(), "{}", reply.text());
            for (name, _) in &reply.headers {
                assert!(
                    !name.to_ascii_lowercase().starts_with("x-edger-sse-"),
                    "x-edger-sse-* reached the publisher: {:?}",
                    reply.headers
                );
            }
        }
    }

    fn open(&self, app: &str, token: Option<&str>) -> Sub {
        Sub::open(self.port, &format!("/{app}/events"), token)
    }

    fn open_many(&self, app: &str, token: &str, count: usize) -> Vec<Sub> {
        (0..count)
            .map(|_| {
                let sub = self.open(app, Some(token));
                assert_eq!(sub.status, 200, "subscribe: {:?}", sub.body);
                sub
            })
            .collect()
    }

    fn stats(&self) -> serde_json::Value {
        let reply = self.root_call("GET", "/metrics/stats", "");
        assert_eq!(reply.status, 200, "{}", reply.text());
        reply.json()
    }

    /// (processes of the app, how many of them report an active request).
    fn processes_of(&self, app: &str) -> (usize, usize) {
        let stats = self.stats();
        let (mut total, mut active) = (0, 0);
        for worker in stats["workers"].as_array().expect("workers") {
            if worker["name"].as_str() != Some(app) && worker["app"].as_str() != Some(app) {
                continue;
            }
            for process in worker["processes"].as_array().expect("processes") {
                assert!(
                    process.get("activeRequest").is_some(),
                    "the reader must see the activeRequest key: {process}"
                );
                total += 1;
                if !process["activeRequest"].is_null() {
                    active += 1;
                }
            }
        }
        (total, active)
    }

    fn metrics_text(&self) -> String {
        let reply = self.root_call("GET", "/metrics", "");
        assert_eq!(reply.status, 200);
        reply.text()
    }

    fn metric(&self, name: &str, worker: &str, reason: Option<&str>) -> f64 {
        metric_value(&self.metrics_text(), name, worker, reason)
    }

    fn connections(&self, app: &str) -> f64 {
        self.metric("edger_sse_connections", app, None)
    }

    fn disconnects(&self, app: &str, reason: &str) -> f64 {
        self.metric("edger_sse_disconnects_total", app, Some(reason))
    }

    fn wait_connections(&self, app: &str, expected: f64, timeout: Duration) {
        wait_until(
            &format!("edger_sse_connections{{{app}}} == {expected}"),
            timeout,
            || self.connections(app) == expected,
        );
    }

    /// SIGTERM to the EdgeR process (its identity checked against the
    /// binary under test just before the signal).
    fn sigterm_edger(&mut self) {
        let tree = self.edger.as_ref().expect("a live EdgeR run");
        let pid = tree.edger_pid;
        let exe = fs::read_link(format!("/proc/{pid}/exe")).expect("edger exe");
        assert_eq!(
            exe,
            fs::canonicalize(env!("CARGO_BIN_EXE_edger")).unwrap(),
            "pid {pid} is not the binary under test"
        );
        let target = rustix::process::Pid::from_raw(pid as i32).expect("pid");
        rustix::process::kill_process(target, rustix::process::Signal::TERM).expect("SIGTERM");
    }

    /// Waits for the EdgeR process to exit; returns the helper's status line.
    fn wait_edger_exit(&mut self, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let tree = self.edger.as_mut().expect("a live EdgeR run");
        loop {
            if let Some(reason) = tree.exited() {
                return reason;
            }
            assert!(
                Instant::now() < deadline,
                "edger did not exit within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Runs `body` on a fresh stack, always stops everything, and only then
/// proves the cleanup and the secret scan.
fn scenario(backend: Backend, env: &[(&str, &str)], body: impl FnOnce(&mut Stack)) {
    let mut stack = Stack::new(backend, env);
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stack.start();
        body(&mut stack);
    }));
    let finished = stack.finish();
    if let Err(panic) = attempt {
        if let Err(problems) = finished {
            eprintln!("cleanup problems after the failure: {problems}");
        }
        std::panic::resume_unwind(panic);
    }
    finished.expect("the stack must stop cleanly");
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

// ───────────────────────────── scenarios ─────────────────────────────

fn ev_json(event: &Ev) -> serde_json::Value {
    event_json(&event.data, event.event.as_deref(), event.id.as_deref())
}

/// No subscriber of `subs` received an event during `span`.
fn assert_none_received(subs: &mut [Sub], span: Duration, what: &str) {
    let mut first = true;
    for sub in subs.iter_mut() {
        let wait = if first {
            span
        } else {
            Duration::from_millis(20)
        };
        first = false;
        assert_eq!(sub.quiet(wait), 0, "{what}: an event arrived");
    }
}

// 1 fan-out, 2 no retained execution, 3 publish done, 4 authorization,
// 5 isolation, 7 disconnect (and 11 streaming regression on Deno).
fn core_scenarios(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-core-{}", backend.suffix());
        let other = format!("sse-core-{}-b", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        stack.deploy(&other, "5m", 256, TOKEN_ONE);

        // The 20 authorized subscribers, held at once.
        let mut subs = stack.open_many(&app, TOKEN_ONE, 20);
        stack.wait_connections(&app, 20.0, STEP);
        for sub in &subs {
            assert_eq!(header_of(&sub.headers, "cache-control"), Some("no-store"));
            assert_eq!(header_of(&sub.headers, "x-accel-buffering"), Some("no"));
            assert!(!sub.has_sse_headers(), "{:?}", sub.headers);
        }

        // 4. Authorization: no token and a wrong token are the guest's 401,
        // no connection is added and nothing is delivered.
        for token in [None, Some("wrong-token")] {
            let denied = stack.open(&app, token);
            assert_eq!(denied.status, 401, "{token:?}");
            assert_eq!(denied.json()["error"], "unauthorized");
            assert!(!denied.has_sse_headers(), "{:?}", denied.headers);
            let publish = stack.publish(&app, token, &[event_json("denied", None, None)]);
            assert_eq!(publish.status, 401, "{token:?}");
        }
        assert_eq!(stack.connections(&app), 20.0, "connection count changed");
        assert_eq!(
            stack.metric("edger_sse_events_published_total", &app, None),
            0.0
        );
        assert_none_received(&mut subs, Duration::from_millis(1200), "after denials");

        // 1. Fan-out: every subscriber gets every event, in order, intact.
        let expected: Vec<Ev> = (1..=12)
            .map(|i| match i {
                3 => Ev::new("line one\nline two", Some("msg"), Some("3")),
                5 => Ev::new("ação ✓ naïve", None, Some("5")),
                _ => Ev::new(
                    &format!("payload-{i}"),
                    (i % 2 == 0).then_some("tick"),
                    Some(&i.to_string()),
                ),
            })
            .collect();
        for batch in expected.chunks(6) {
            let events: Vec<_> = batch.iter().map(ev_json).collect();
            stack.publish_ok(&app, &events);
        }
        for (index, sub) in subs.iter_mut().enumerate() {
            sub.wait_events(12, STEP);
            assert_eq!(sub.events[..12], expected[..], "subscriber {index}");
        }

        // 3. The publish is over when its response arrives: no active request
        // right after it, and nowhere near the worker timeout (30 s).
        let mut durations = Vec::new();
        for round in 0..5 {
            let started = Instant::now();
            let reply = stack.publish(
                &app,
                Some(TOKEN_ONE),
                &[event_json(&format!("done-{round}"), None, None)],
            );
            durations.push(started.elapsed());
            assert_eq!(reply.status, 200, "{}", reply.text());
            let (processes, active) = stack.processes_of(&app);
            assert!(processes >= 1, "the reader found no process of {app}");
            assert_eq!(active, 0, "round {round}: a request is still active");
        }
        let slowest = durations.iter().max().copied().unwrap();
        assert!(slowest < Duration::from_secs(5), "{durations:?}");
        eprintln!("OBSERVED {backend:?} publish response times: {durations:?}");
        for sub in &mut subs {
            sub.wait_events(17, STEP);
        }

        // 2. No execution is held per connection: with 20 open and a single
        // process, the reader sees no active request and `/plain` answers.
        let started = Instant::now();
        let mut samples = 0;
        let mut slowest_plain = Duration::ZERO;
        while samples < 5 || started.elapsed() < Duration::from_secs(10) {
            let (processes, active) = stack.processes_of(&app);
            assert!(processes >= 1, "sample {samples}: no process of {app}");
            assert_eq!(active, 0, "sample {samples}: an execution is retained");
            let began = Instant::now();
            let plain = stack.get(&app, "plain", None);
            slowest_plain = slowest_plain.max(began.elapsed());
            assert_eq!(plain.status, 200, "{}", plain.text());
            assert!(began.elapsed() < Duration::from_secs(5));
            assert_eq!(stack.connections(&app), 20.0);
            samples += 1;
            std::thread::sleep(Duration::from_millis(1500));
        }
        eprintln!(
            "OBSERVED {backend:?} idle samples: {samples} over {:?}, slowest /plain {slowest_plain:?}",
            started.elapsed()
        );
        assert!(subs[0].quiet(Duration::from_millis(1500)) == 0);
        assert!(
            subs[0].comments >= 1,
            "no heartbeat reached a held connection"
        );

        // Positive control of the reader, on both backends: while the guest is
        // really busy the SAME reader must see a non-null activeRequest, and
        // null afterwards. Deno: an ordinary stream (11, the streaming
        // regression, with 20 held). Go: `/slow?ms=3000`.
        {
            let port = stack.port;
            let path = match backend {
                Backend::Deno => format!("/{app}/stream?n=8&gap=400"),
                Backend::Go => format!("/{app}/slow?ms=3000"),
            };
            let began = Instant::now();
            let busy = std::thread::spawn(move || http(port, "GET", &path, &[], b""));
            let mut seen_active = 0;
            let mut samples = 0;
            while !busy.is_finished() {
                let (_, active) = stack.processes_of(&app);
                samples += 1;
                if active > 0 {
                    seen_active += 1;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let reply = busy.join().unwrap().expect("busy request");
            let took = began.elapsed();
            assert_eq!(reply.status, 200, "{}", reply.text());
            match backend {
                Backend::Deno => {
                    let want: String = (0..8).map(|i| format!("chunk-{i}\n")).collect();
                    assert_eq!(reply.text(), want);
                }
                Backend::Go => {
                    assert_eq!(reply.json()["slow"], true);
                    assert!(took >= Duration::from_millis(2500), "/slow took {took:?}");
                }
            }
            assert!(
                seen_active >= 1,
                "positive control: the reader never saw an active request ({samples} samples)"
            );
            eprintln!(
                "OBSERVED {backend:?} busy request {took:?}: active in {seen_active} of {samples} samples"
            );
            let (_, active) = stack.processes_of(&app);
            assert_eq!(active, 0, "the process is still busy after the request");
            assert_eq!(stack.connections(&app), 20.0);
            assert_none_received(
                &mut subs,
                Duration::from_millis(500),
                "during the busy request",
            );
        }

        // 5. Isolation: the same topic in another app of the same backend.
        let mut others = stack.open_many(&other, TOKEN_ONE, 2);
        let before = subs[0].events.len();
        stack.publish_ok(&app, &[event_json("only-a", None, None)]);
        for sub in &mut subs {
            sub.wait_events(before + 1, STEP);
            assert_eq!(sub.events.last().unwrap().data, "only-a");
        }
        assert_none_received(&mut others, Duration::from_millis(1500), "B after A");
        stack.publish_ok(&other, &[event_json("only-b", None, None)]);
        for sub in &mut others {
            sub.wait_events(1, STEP);
            assert_eq!(sub.events[0].data, "only-b");
        }
        assert_none_received(&mut subs, Duration::from_millis(1000), "A after B");
        // A publisher with no directive delivers nothing to anyone.
        let empty = stack.publish(&app, Some(TOKEN_ONE), &[]);
        assert_eq!(empty.status, 200);
        assert_eq!(empty.json()["published"], 0);
        assert_none_received(&mut subs, Duration::from_millis(1000), "empty publish (A)");
        assert_none_received(&mut others, Duration::from_millis(100), "empty publish (B)");
        assert_eq!(
            stack.metric("edger_sse_events_published_total", &app, None),
            (12 + 5 + 1) as f64
        );
        assert_eq!(
            stack.metric("edger_sse_events_published_total", &other, None),
            1.0
        );

        // 7. Disconnect: the count and `reason="client"` follow the clients.
        let client_before = stack.disconnects(&app, "client");
        let closing: Vec<Sub> = subs.drain(..8).collect();
        drop(closing);
        stack.wait_connections(&app, 12.0, Duration::from_secs(15));
        wait_until("8 client disconnects", Duration::from_secs(15), || {
            stack.disconnects(&app, "client") == client_before + 8.0
        });
        let remaining = subs[0].events.len();
        stack.publish_ok(&app, &[event_json("after-close", None, None)]);
        for sub in &mut subs {
            sub.wait_events(remaining + 1, STEP);
            assert_eq!(sub.events.last().unwrap().data, "after-close");
        }
        drop(subs);
        stack.wait_connections(&app, 0.0, Duration::from_secs(15));
        wait_until("20 client disconnects", Duration::from_secs(15), || {
            stack.disconnects(&app, "client") == client_before + 20.0
        });
        drop(others);
        stack.wait_connections(&other, 0.0, Duration::from_secs(15));
        // Negative control: nothing was counted under another reason.
        for reason in ["ttl", "lagged", "shutdown"] {
            assert_eq!(stack.disconnects(&app, reason), 0.0, "{reason}");
        }
    });
}

/// Several directives in ONE publish response (the contract allows 1 to 8):
/// all of them reach every subscriber, in order, and no directive header
/// reaches the publisher. Same assertion on both backends.
fn several_directives_in_one_response(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-multi-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let mut subs = stack.open_many(&app, TOKEN_ONE, 2);
        let events: Vec<_> = (1..=3)
            .map(|i| event_json(&format!("multi-{i}"), Some("msg"), Some(&i.to_string())))
            .collect();
        let reply = stack.publish(&app, Some(TOKEN_ONE), &events);
        assert_eq!(
            reply.status,
            200,
            "three directives in one response: {}",
            reply.text()
        );
        assert_eq!(reply.json()["published"], 3);
        assert!(reply
            .headers
            .iter()
            .all(|(name, _)| !name.to_ascii_lowercase().starts_with("x-edger-sse-")));
        for sub in &mut subs {
            sub.wait_events(3, STEP);
            let data: Vec<&str> = sub.events.iter().map(|event| event.data.as_str()).collect();
            assert_eq!(data, ["multi-1", "multi-2", "multi-3"]);
        }
    });
}

/// Eight directives of about 4 KiB of data each, in one response, are
/// delivered to every subscriber, in order and intact.
fn large_batch(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-large-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let mut subs = stack.open_many(&app, TOKEN_ONE, 2);
        let events: Vec<_> = (0..8)
            .map(|i| {
                event_json(
                    &format!("{i}-{}", "x".repeat(4000)),
                    Some("big"),
                    Some(&i.to_string()),
                )
            })
            .collect();
        let reply = stack.publish(&app, Some(TOKEN_ONE), &events);
        assert_eq!(reply.status, 200, "{}", reply.text());
        assert_eq!(reply.json()["published"], 8);
        for sub in &mut subs {
            sub.wait_events(8, STEP);
            for (index, event) in sub.events.iter().enumerate() {
                assert!(event.data.starts_with(&format!("{index}-")), "{index}");
                assert_eq!(event.data.len(), 4002, "{index}");
                assert_eq!(event.event.as_deref(), Some("big"));
                assert_eq!(event.id.as_deref(), Some(index.to_string().as_str()));
            }
        }
    });
}

/// A batch whose response head is over 64 KiB fails closed: nothing is
/// published, no `x-edger-sse-*` reaches the client, and the status is an
/// error. Every directive is exactly 6144 JSON bytes (28 of envelope plus
/// 6116 of data), so its base64url value is 8192 bytes, the largest value the
/// host takes; eight of them are more than 64 KiB of head.
fn head_over_64_kib(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-head-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let mut subs = stack.open_many(&app, TOKEN_ONE, 2);
        // Control: one such directive (8 KiB of head) is delivered.
        stack.publish_ok(
            &app,
            &[event_json(&format!("0{}", "y".repeat(6115)), None, None)],
        );
        for sub in &mut subs {
            sub.wait_events(1, STEP);
        }
        let huge: Vec<_> = (0..8)
            .map(|i| event_json(&format!("{i}{}", "y".repeat(6115)), None, None))
            .collect();
        let over = stack.publish(&app, Some(TOKEN_ONE), &huge);
        let code = serde_json::from_slice::<serde_json::Value>(&over.body)
            .ok()
            .and_then(|body| body["code"].as_str().map(str::to_owned));
        eprintln!(
            "OBSERVED {backend:?} head over 64 KiB: status {} code {code:?} body {:?}",
            over.status,
            over.text()
        );
        for sub in &mut subs {
            assert_eq!(
                sub.quiet(Duration::from_millis(700)),
                0,
                "an over-limit head published events"
            );
        }
        assert_eq!(
            stack.metric("edger_sse_events_published_total", &app, None),
            1.0,
            "only the control directive counts"
        );
        assert!(
            over.status >= 400,
            "an over-limit head must fail, got {}: {}",
            over.status,
            over.text()
        );
        if matches!(backend, Backend::Deno) {
            // The host refuses the head: 502 with the static directive code.
            assert_eq!(over.status, 502, "{}", over.text());
            assert_eq!(code.as_deref(), Some("SSE_INVALID_DIRECTIVE"));
        }
        assert!(over
            .headers
            .iter()
            .all(|(name, _)| !name.to_ascii_lowercase().starts_with("x-edger-sse-")));
        // The stack is intact: a normal publish still works.
        stack.publish_ok(&app, &[event_json("still-works", None, None)]);
        for sub in &mut subs {
            sub.wait_events(2, STEP);
            assert_eq!(sub.events[1].data, "still-works");
        }
    });
}

/// Deno: repeated and comma-joined directive headers at the host border.
fn deno_directive_header_edges() {
    scenario(Backend::Deno, &[], |stack| {
        let app = "sse-edge-deno";
        let port = stack.port;
        stack.deploy(app, "5m", 256, TOKEN_ONE);
        let mut sub = stack.open(app, Some(TOKEN_ONE));
        assert_eq!(sub.status, 200);
        let value = |data: &str| {
            use base64::Engine as _;
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(event_json(data, None, None).to_string())
        };

        // Control: one raw value is delivered.
        let ok = stack.publish_body(
            app,
            Some(TOKEN_ONE),
            &serde_json::json!({"raw": [value("raw-ok")]}),
        );
        assert_eq!(ok.status, 200, "{}", ok.text());
        sub.wait_events(1, STEP);
        assert_eq!(sub.events[0].data, "raw-ok");
        // Two raw values: both arrive, in order.
        let two = stack.publish_body(
            app,
            Some(TOKEN_ONE),
            &serde_json::json!({"raw": [value("raw-1"), value("raw-2")]}),
        );
        assert_eq!(two.status, 200, "{}", two.text());
        sub.wait_events(3, STEP);
        assert_eq!(sub.events[1].data, "raw-1");
        assert_eq!(sub.events[2].data, "raw-2");

        // An empty item ("valid, ") is refused and nothing is delivered.
        for bad in [
            format!("{}, ", value("trailing")),
            format!("{},,{}", value("a"), value("b")),
            format!(", {}", value("leading")),
        ] {
            let reply =
                stack.publish_body(app, Some(TOKEN_ONE), &serde_json::json!({"raw": [bad]}));
            assert_eq!(reply.status, 502, "{bad:?}: {}", reply.text());
            assert_eq!(reply.json()["code"], "SSE_INVALID_DIRECTIVE", "{bad:?}");
        }
        assert_eq!(sub.quiet(Duration::from_millis(800)), 0, "an event leaked");
        assert_eq!(
            stack.metric("edger_sse_events_published_total", app, None),
            3.0
        );

        // A repeated subscribe header arrives as a list: both topics count.
        let mut both = Sub::open(
            port,
            &format!("/{app}/events?topics=room.1&topics=room.2"),
            Some(TOKEN_ONE),
        );
        assert_eq!(both.status, 200, "{:?}", both.body);
        stack.publish_ok(app, &[event_json_on("room.2", "on-two", None, None)]);
        stack.publish_ok(app, &[event_json_on("room.1", "on-one", None, None)]);
        both.wait_events(2, STEP);
        assert_eq!(both.events[0].data, "on-two");
        assert_eq!(both.events[1].data, "on-one");
        sub.wait_events(4, STEP);
        assert_eq!(sub.events[3].data, "on-one", "room.1 only for the first");
        assert_eq!(sub.quiet(Duration::from_millis(300)), 0, "room.2 leaked");
        // Eight topics in total are fine, nine are not.
        let topics = |count: usize| {
            (1..=count)
                .map(|i| format!("topics=t{i}"))
                .collect::<Vec<_>>()
                .join("&")
        };
        let eight = Sub::open(
            port,
            &format!("/{app}/events?{}", topics(8)),
            Some(TOKEN_ONE),
        );
        assert_eq!(eight.status, 200, "{:?}", eight.body);
        let nine = Sub::open(
            port,
            &format!("/{app}/events?{}", topics(9)),
            Some(TOKEN_ONE),
        );
        assert_eq!(nine.status, 502, "{:?}", nine.body);
        assert_eq!(nine.json()["code"], "SSE_INVALID_DIRECTIVE");
    });
}

/// Go/WASM keeps repeated header pairs, so a repeated
/// `x-edger-sse-subscribe` reaches the host as repeated and is refused.
fn go_repeated_subscribe_header() {
    scenario(Backend::Go, &[], |stack| {
        let app = "sse-edge-go";
        let port = stack.port;
        stack.deploy(app, "5m", 256, TOKEN_ONE);
        let one = Sub::open(
            port,
            &format!("/{app}/events?topics=room.1"),
            Some(TOKEN_ONE),
        );
        assert_eq!(one.status, 200, "control: {:?}", one.body);
        let two = Sub::open(
            port,
            &format!("/{app}/events?topics=room.1&topics=room.2"),
            Some(TOKEN_ONE),
        );
        assert_eq!(two.status, 502, "{:?}", two.body);
        assert_eq!(two.json()["code"], "SSE_INVALID_DIRECTIVE");
        assert!(!two.has_sse_headers());
        stack.wait_connections(app, 1.0, STEP);
    });
}

/// Deno, worker WITHOUT `sse`: a repeated `x-edger-sse-publish` reaches the
/// client as separate lines with the value intact, with no error and no event;
/// repeated `set-cookie` still arrives on separate lines.
fn deno_repeated_headers_without_sse() {
    scenario(Backend::Deno, &[], |stack| {
        let app = "sse-plain-deno";
        stack.put_secret(app, TOKEN_ONE);
        stack.install(&Backend::Deno.package_without_sse(app));
        let events = [
            event_json("one", Some("msg"), Some("1")),
            event_json("two", None, None),
        ];
        let reply = stack.publish(app, Some(TOKEN_ONE), &events);
        assert_eq!(reply.status, 200, "{}", reply.text());
        assert_eq!(reply.json()["published"], 2);
        let lines: Vec<&str> = reply
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-edger-sse-publish"))
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(lines.len(), 2, "two lines, not one comma-joined: {lines:?}");
        for (line, event) in lines.iter().zip(&events) {
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(line)
                .unwrap_or_else(|error| panic!("{line:?} is not intact base64url: {error}"));
            let decoded: serde_json::Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(&decoded, event);
        }
        assert_eq!(
            stack.metric("edger_sse_events_published_total", app, None),
            0.0,
            "a worker without sse publishes nothing"
        );
        // Regression control: repeated set-cookie, separate lines as before.
        let cookies = stack.get(app, "cookies", None);
        assert_eq!(cookies.status, 200);
        let lines: Vec<&str> = cookies
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(lines, ["a=1; Path=/", "b=2; Path=/"]);
    });
}

// 6. A subscriber that stops reading is retired as `lagged`; the others keep
// receiving, in order and without loss. The slot is not required to return
// while the dead socket lasts (accepted limit), so only the retirement and
// the other readers are asserted.
fn stalled_client(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-stall-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let mut readers = stack.open_many(&app, TOKEN_ONE, 3);
        let stalled = open_stalled(stack.port, &format!("/{app}/events"), TOKEN_ONE);
        assert_eq!(stalled.status, 200);
        stack.wait_connections(&app, 4.0, STEP);
        assert_eq!(stack.disconnects(&app, "lagged"), 0.0);

        // Volume enough to beat the loopback buffers of the stopped socket.
        let padding = "x".repeat(4000);
        let mut sent = 0usize;
        let mut batches = 0usize;
        loop {
            assert!(
                sent < 8000,
                "the stopped client was not retired after {sent} events"
            );
            let events: Vec<_> = (0..8)
                .map(|offset| event_json(&format!("seq-{}-{padding}", sent + offset), None, None))
                .collect();
            stack.publish_ok(&app, &events);
            sent += 8;
            batches += 1;
            if batches.is_multiple_of(4) && stack.disconnects(&app, "lagged") >= 1.0 {
                break;
            }
        }
        eprintln!("OBSERVED {backend:?} lagged after {sent} events");
        assert_eq!(stack.disconnects(&app, "lagged"), 1.0);

        // The others lost nothing and kept the order.
        for (index, reader) in readers.iter_mut().enumerate() {
            reader.wait_events(sent, Duration::from_secs(60));
            for (position, event) in reader.events.iter().enumerate() {
                assert!(
                    event.data.starts_with(&format!("seq-{position}-")),
                    "reader {index}: event {position} out of order"
                );
            }
        }
        // And they are not delayed: a marker after the retirement arrives fast.
        let published = Instant::now();
        stack.publish_ok(&app, &[event_json("marker-after-lag", None, None)]);
        for reader in &mut readers {
            reader.wait_events(sent + 1, Duration::from_secs(5));
            assert_eq!(reader.events.last().unwrap().data, "marker-after-lag");
        }
        let delay = published.elapsed();
        assert!(delay < Duration::from_secs(5), "{delay:?}");
        eprintln!("OBSERVED {backend:?} marker delay after lagged: {delay:?}");
        // No other reason was counted for these clients.
        assert_eq!(stack.disconnects(&app, "client"), 0.0);
        assert_eq!(stack.disconnects(&app, "ttl"), 0.0);

        // The slot of the stopped socket may be held while its TCP lasts (the
        // accepted limit) but must come back once the TCP ends.
        let held = stack.connections(&app);
        drop(stalled);
        let released = Instant::now();
        let mut gauge = held;
        while released.elapsed() < Duration::from_secs(30) {
            gauge = stack.connections(&app);
            if gauge == 3.0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(
            gauge,
            3.0,
            "the slot of the stopped client did not return within 30 s of its TCP ending (gauge {gauge}, {held} before the close)"
        );
        eprintln!(
            "OBSERVED {backend:?} slot before close {held}, returned after close ({:?})",
            released.elapsed()
        );
        // The three healthy readers are still connected, and a new
        // subscription succeeds in the returned slot.
        for (index, reader) in readers.iter_mut().enumerate() {
            assert_eq!(
                reader.quiet(Duration::from_millis(100)),
                0,
                "reader {index}"
            );
            assert_eq!(reader.end, None, "reader {index} lost its connection");
        }
        let fresh = stack.open(&app, Some(TOKEN_ONE));
        assert_eq!(fresh.status, 200, "{:?}", fresh.body);
        stack.wait_connections(&app, 4.0, STEP);
        drop(fresh);
    });
}

// 8. TTL ends the stream with `retry:`, the reconnect runs the guest again
// and resubscribes; after the secret is replaced and the version recycled
// the old credentials get 401.
fn ttl_and_revocation(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-ttl-{}", backend.suffix());
        stack.deploy(&app, "5s", 256, TOKEN_ONE);
        let mut first = stack.open(&app, Some(TOKEN_ONE));
        assert_eq!(first.status, 200);
        let opened = Instant::now();
        stack.publish_ok(&app, &[event_json("before-ttl", None, None)]);
        first.wait_events(1, STEP);
        first.expect_completed(Duration::from_secs(15), "ttl of the first subscription");
        eprintln!("OBSERVED {backend:?} ttl body end: {:?}", first.end);
        let lived = opened.elapsed();
        assert!(
            lived >= Duration::from_secs(4) && lived <= Duration::from_secs(12),
            "ttl 5s ended the stream after {lived:?}"
        );
        let retry = first.retry.expect("a `retry:` frame before the end");
        assert!((1000..=5000).contains(&retry), "retry {retry}");
        eprintln!("OBSERVED {backend:?} ttl 5s ended after {lived:?}, retry {retry} ms");
        wait_until("the ttl disconnect", STEP, || {
            stack.disconnects(&app, "ttl") == 1.0
        });
        stack.wait_connections(&app, 0.0, STEP);

        // The reconnect executes the guest again and subscribes again.
        let mut second = stack.open(&app, Some(TOKEN_ONE));
        assert_eq!(second.status, 200);
        let second_opened = Instant::now();
        stack.publish_ok(&app, &[event_json("after-reconnect", None, None)]);
        second.wait_events(1, STEP);
        assert_eq!(second.events[0].data, "after-reconnect");

        // Revocation: replace the secret, recycle the version.
        stack.rotate(&app, TOKEN_TWO);
        let denied = stack.open(&app, Some(TOKEN_ONE));
        assert_eq!(denied.status, 401, "{:?}", denied.body);
        let denied_publish = stack.publish(&app, Some(TOKEN_ONE), &[event_json("x", None, None)]);
        assert_eq!(denied_publish.status, 401);
        let fresh = stack.open(&app, Some(TOKEN_TWO));
        assert_eq!(fresh.status, 200);

        // The live subscription is cut by its TTL: that is the revocation latency.
        second.expect_completed(Duration::from_secs(15), "ttl of the revoked subscription");
        let latency = second_opened.elapsed();
        assert!(latency <= Duration::from_secs(12), "{latency:?}");
        assert!(second.retry.is_some());
        eprintln!("OBSERVED {backend:?} revoked subscriber lived {latency:?} past its open");
        let again = stack.open(&app, Some(TOKEN_ONE));
        assert_eq!(again.status, 401, "old credentials after the TTL");
        let mut renewed = stack.open(&app, Some(TOKEN_TWO));
        assert_eq!(renewed.status, 200);
        let reply = stack.publish(
            &app,
            Some(TOKEN_TWO),
            &[event_json("with-new-token", None, None)],
        );
        assert_eq!(reply.status, 200, "{}", reply.text());
        renewed.wait_events(1, STEP);
        assert_eq!(renewed.events[0].data, "with-new-token");
        wait_until("two ttl disconnects", STEP, || {
            stack.disconnects(&app, "ttl") >= 2.0
        });
        drop(fresh);
    });
}

// 9. A full app answers 503 `SSE_CAPACITY` with `Retry-After: 5` and the
// existing subscribers are intact; with the broker off, 503
// `SSE_UNAVAILABLE` after the guest authorized.
fn capacity_and_unavailable(backend: Backend) {
    scenario(
        backend,
        &[("EDGER_SSE_MAX_CONNECTIONS_PER_APP", "3")],
        |stack| {
            let app = format!("sse-cap-{}", backend.suffix());
            stack.deploy(&app, "5m", 256, TOKEN_ONE);
            let mut held = stack.open_many(&app, TOKEN_ONE, 3);
            let over = stack.open(&app, Some(TOKEN_ONE));
            assert_eq!(over.status, 503, "{:?}", over.body);
            assert_eq!(over.json()["code"], "SSE_CAPACITY");
            assert_eq!(header_of(&over.headers, "retry-after"), Some("5"));
            assert!(!over.has_sse_headers());
            wait_until("the capacity rejection", STEP, || {
                stack.metric("edger_sse_rejected_total", &app, Some("capacity")) == 1.0
            });
            assert_eq!(stack.connections(&app), 3.0);
            stack.publish_ok(&app, &[event_json("after-full", None, None)]);
            for sub in &mut held {
                sub.wait_events(1, STEP);
                assert_eq!(sub.events[0].data, "after-full");
            }
            // The guest decides first: a wrong token on a full app is a 401.
            let denied = stack.open(&app, Some("wrong-token"));
            assert_eq!(denied.status, 401);
            assert_eq!(
                stack.metric("edger_sse_rejected_total", &app, Some("capacity")),
                1.0
            );
            // A freed slot is reusable.
            drop(held.pop());
            stack.wait_connections(&app, 2.0, Duration::from_secs(15));
            let reopened = stack.open(&app, Some(TOKEN_ONE));
            assert_eq!(reopened.status, 200, "{:?}", reopened.body);
        },
    );
    scenario(backend, &[("EDGER_SSE_MAX_CONNECTIONS", "0")], |stack| {
        let app = format!("sse-off-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let unavailable = stack.open(&app, Some(TOKEN_ONE));
        assert_eq!(unavailable.status, 503, "{:?}", unavailable.body);
        assert_eq!(unavailable.json()["code"], "SSE_UNAVAILABLE");
        assert!(!unavailable.has_sse_headers());
        // The guest ran and decided first: a wrong token is still 401.
        let denied = stack.open(&app, Some("wrong-token"));
        assert_eq!(denied.status, 401);
        // Publish routes keep answering normally and drop the directives.
        let reply = stack.publish(&app, Some(TOKEN_ONE), &[event_json("lost", None, None)]);
        assert_eq!(reply.status, 200, "{}", reply.text());
        assert_eq!(reply.json()["published"], 1);
        assert_eq!(
            stack.metric("edger_sse_rejected_total", &app, Some("unavailable")),
            1.0
        );
        assert_eq!(stack.connections(&app), 0.0);
        assert_eq!(
            stack.metric("edger_sse_events_published_total", &app, None),
            0.0
        );
    });
}

// 10. Go/WASM only: a `fullstack` app with `apiPrefix`: the SPA is served by
// the host and the subscription is a route under the prefix of the same app.
fn fullstack_go() {
    scenario(Backend::Go, &[], |stack| {
        let name = "sse-fs";
        stack.put_secret(name, TOKEN_ONE);
        let manifest = "name: sse-fs\nversion: \"1.0.0\"\nkind: fullstack\nadapter: wasm\nclientDir: dist\nwasmBackend:\n  entrypoint: backend/app.wasm\n  apiPrefix: /api\nvisibility: public\nnameRoute: true\nallowNet: []\negress:\n  enabled: true\nsecrets:\n  - SSE_ACCESS_TOKEN\ntimeout: 30s\nmaxBodySize: 4mb\nmaxProcesses: 1\nsse:\n  subscribe: [\"/api/events\"]\n  publish: [\"/api/publish\"]\n";
        stack.install(&[
            ("manifest.yaml".to_owned(), manifest.as_bytes().to_vec()),
            (
                "dist/index.html".to_owned(),
                b"<html><body>sse spa</body></html>".to_vec(),
            ),
            ("backend/app.wasm".to_owned(), go_module()),
        ]);
        let port = stack.port;

        // The SPA is served by the host; it never subscribes.
        let spa = http(port, "GET", "/sse-fs/", &[], b"").expect("SPA request");
        assert_eq!(spa.status, 200, "{}", spa.text());
        assert!(spa.text().contains("sse spa"), "{}", spa.text());
        assert_eq!(stack.connections(name), 0.0);

        // Authorization and subscription under the API prefix, same app.
        let denied = Sub::open(port, "/sse-fs/api/events", None);
        assert_eq!(denied.status, 401, "{:?}", denied.body);
        let mut sub = Sub::open(port, "/sse-fs/api/events", Some(TOKEN_ONE));
        assert_eq!(sub.status, 200, "{:?}", sub.body);
        stack.wait_connections(name, 1.0, STEP);

        // The SPA is still served with a subscriber held, and a plain route
        // under the prefix is an ordinary answer.
        let spa = http(port, "GET", "/sse-fs/", &[], b"").expect("SPA request");
        assert_eq!(spa.status, 200);
        let plain = http(port, "GET", "/sse-fs/api/plain", &[], b"").expect("plain request");
        assert_eq!(plain.status, 200, "{}", plain.text());
        assert_eq!(plain.json()["plain"], true);
        assert_eq!(stack.connections(name), 1.0);

        // Publish through the prefix reaches the subscriber.
        let body =
            serde_json::json!({"events": [event_json("through-prefix", Some("msg"), Some("1"))]})
                .to_string();
        let publish = http(
            port,
            "POST",
            "/sse-fs/api/publish",
            &[
                ("Authorization", &format!("Bearer {TOKEN_ONE}")),
                ("Content-Type", "application/json"),
            ],
            body.as_bytes(),
        )
        .expect("publish request");
        assert_eq!(publish.status, 200, "{}", publish.text());
        assert!(publish.header("x-edger-sse-publish").is_none());
        sub.wait_events(1, STEP);
        assert_eq!(
            sub.events[0],
            Ev::new("through-prefix", Some("msg"), Some("1"))
        );
        let unauthorized = http(
            port,
            "POST",
            "/sse-fs/api/publish",
            &[("Content-Type", "application/json")],
            body.as_bytes(),
        )
        .expect("publish request");
        assert_eq!(unauthorized.status, 401);
        assert_eq!(sub.quiet(Duration::from_millis(800)), 0);
    });
}

// 12. SIGTERM with open streams: the binary exits within the declared
// deadline, the clients see the end of the connection and nothing is left.
fn shutdown_with_open_streams(backend: Backend) {
    scenario(backend, &[], |stack| {
        let app = format!("sse-down-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let mut subs = stack.open_many(&app, TOKEN_ONE, 6);
        stack.wait_connections(&app, 6.0, STEP);
        stack.publish_ok(&app, &[event_json("before-term", None, None)]);
        for sub in &mut subs {
            sub.wait_events(1, STEP);
        }
        // Positive control of the wait: nothing ends the streams before the signal.
        assert_eq!(subs[0].wait_end(Duration::from_millis(500)), None);

        let started = Instant::now();
        stack.sigterm_edger();
        for (index, sub) in subs.iter_mut().enumerate() {
            let left = SHUTDOWN_DEADLINE.saturating_sub(started.elapsed());
            sub.expect_completed(left, &format!("SIGTERM, client {index}"));
        }
        eprintln!(
            "OBSERVED {backend:?} SIGTERM body end of all {} clients: {:?}",
            subs.len(),
            subs.iter().map(|sub| sub.end.clone()).collect::<Vec<_>>()
        );
        let clients_ended = started.elapsed();
        let status = stack.wait_edger_exit(SHUTDOWN_DEADLINE.saturating_sub(started.elapsed()));
        let exited = started.elapsed();
        assert!(status.contains("exit status: 0"), "{status}");
        assert!(exited < SHUTDOWN_DEADLINE, "{exited:?}");
        eprintln!(
            "OBSERVED {backend:?} SIGTERM: clients saw the end after {clients_ended:?}, edger exited after {exited:?}"
        );

        // Nothing of the tree survives its root: the helper (the subreaper)
        // has no descendant left, and the port refuses.
        // A zombie is an exited process the subreaper has not reaped yet
        // (it reaps in its cleanup); only a live descendant counts as left over.
        let helper = stack.edger.as_ref().unwrap().helper.pid;
        let mut live_seen = Vec::new();
        loop {
            let live: Vec<Descendant> = descendants(helper)
                .expect("the process tree is readable")
                .into_iter()
                .filter(|item| item.state != "Z")
                .collect();
            if live.is_empty() {
                break;
            }
            live_seen = live;
            assert!(
                started.elapsed() < SHUTDOWN_DEADLINE,
                "{:?} after SIGTERM a process is still alive: {live_seen:?}",
                started.elapsed()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!(
            "OBSERVED {backend:?} no live descendant {:?} after SIGTERM (live at the first look after the exit: {live_seen:?})",
            started.elapsed()
        );
        assert!(
            TcpStream::connect_timeout(
                &SocketAddr::from((Ipv4Addr::LOCALHOST, stack.port)),
                Duration::from_secs(2)
            )
            .is_err(),
            "the port still accepts connections"
        );
    });
}

// A client that stopped reading keeps a write pending on its socket; the
// broker cannot unblock it. `EDGER_HTTP_DRAIN_MS` bounds the HTTP drain: at
// the deadline the shutdown goes on (cron, pool) and the process exits 0.
// Control: with no stopped client the process leaves at once, long before
// the deadline.
fn http_drain_with_a_stalled_client(backend: Backend) {
    const DRAIN_MS: &str = "4000";
    const DRAIN: Duration = Duration::from_millis(4000);
    // The margin over the limit for the rest of the sequence (broker, cron,
    // pool) to run and the process to exit.
    const MARGIN: Duration = Duration::from_secs(6);
    let env = [
        ("EDGER_HTTP_DRAIN_MS", DRAIN_MS),
        ("EDGER_LOG", "edger=info,edger_orchestrator=info"),
    ];

    // Control: healthy readers only; the drain is quick and never waits for the limit.
    scenario(backend, &env, |stack| {
        let app = format!("sse-drain-ok-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let mut readers = stack.open_many(&app, TOKEN_ONE, 3);
        stack.publish_ok(&app, &[event_json("before-term", None, None)]);
        for reader in &mut readers {
            reader.wait_events(1, STEP);
        }
        let started = Instant::now();
        stack.sigterm_edger();
        let status = stack.wait_edger_exit(DRAIN + MARGIN);
        let took = started.elapsed();
        assert!(status.contains("exit status: 0"), "{status}");
        assert!(
            took < Duration::from_millis(2500),
            "no stopped client: the exit must not wait for the limit, took {took:?}"
        );
        assert!(status.contains("HTTP server stopped"), "{status}");
        assert!(
            !status.contains("HTTP drain deadline reached"),
            "the limit fired without a stopped client: {status}"
        );
        eprintln!("OBSERVED {backend:?} drain control exit after {took:?}");
    });

    // A stopped client whose socket is full: the limit applies.
    scenario(backend, &env, |stack| {
        let app = format!("sse-drain-stall-{}", backend.suffix());
        stack.deploy(&app, "5m", 256, TOKEN_ONE);
        let stalled = open_stalled(stack.port, &format!("/{app}/events"), TOKEN_ONE);
        assert_eq!(stalled.status, 200);
        stack.wait_connections(&app, 1.0, STEP);
        // Fill the socket: the broker retires the subscriber as `lagged` only
        // once hyper stopped polling it, i.e. the write is pending.
        let padding = "x".repeat(4000);
        let mut sent = 0usize;
        let mut batches = 0usize;
        loop {
            assert!(
                sent < 8000,
                "the stopped client was not retired after {sent} events"
            );
            let events: Vec<_> = (0..8)
                .map(|offset| event_json(&format!("seq-{}-{padding}", sent + offset), None, None))
                .collect();
            stack.publish_ok(&app, &events);
            sent += 8;
            batches += 1;
            if batches.is_multiple_of(4) && stack.disconnects(&app, "lagged") >= 1.0 {
                break;
            }
        }
        // The retired subscriber still holds its slot: its TCP is alive.
        assert_eq!(stack.connections(&app), 1.0);

        let started = Instant::now();
        stack.sigterm_edger();
        let status = stack.wait_edger_exit(DRAIN + MARGIN);
        let took = started.elapsed();
        assert!(status.contains("exit status: 0"), "{status}");
        assert!(
            took >= DRAIN - Duration::from_millis(500),
            "the stopped client was not waited for: exit after {took:?}"
        );
        assert!(took < DRAIN + MARGIN, "exit after {took:?}");
        assert!(status.contains("HTTP drain deadline reached"), "{status}");
        // The sequence went on past the deadline: cron and pool.
        assert!(
            status.contains("HTTP server stopped; shutting down cron scheduler and worker pool"),
            "{status}"
        );
        eprintln!("OBSERVED {backend:?} drain with a stopped client: exit after {took:?}");
        drop(stalled);
    });
}

// ───────────────────────────── the runs ─────────────────────────────

const NEEDS: &str = "requires the real edger binary, the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE) and Deno (EDGER_DENO_BIN)";

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_core_scenarios() {
    core_scenarios(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_core_scenarios() {
    core_scenarios(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_stalled_client() {
    stalled_client(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_stalled_client() {
    stalled_client(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_ttl_and_revocation() {
    ttl_and_revocation(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_ttl_and_revocation() {
    ttl_and_revocation(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_capacity_and_unavailable() {
    capacity_and_unavailable(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_capacity_and_unavailable() {
    capacity_and_unavailable(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_fullstack_under_the_api_prefix() {
    fullstack_go();
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_http_drain_is_bounded_with_a_stalled_client() {
    http_drain_with_a_stalled_client(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_http_drain_is_bounded_with_a_stalled_client() {
    http_drain_with_a_stalled_client(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_shutdown_with_open_streams() {
    shutdown_with_open_streams(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_shutdown_with_open_streams() {
    shutdown_with_open_streams(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_several_publish_directives_in_one_response() {
    several_directives_in_one_response(Backend::Go);
}

/// Three directives in one response on the Deno backend. The Deno harness
/// used to fold repeated response headers into one comma-joined value; it now
/// splits `x-edger-sse-publish` into one pair per directive (see the
/// "Cabeçalhos repetidos no Deno" section of the SSE contract).
#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_several_publish_directives_in_one_response() {
    several_directives_in_one_response(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_eight_large_directives_are_delivered() {
    large_batch(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_eight_large_directives_are_delivered() {
    large_batch(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_response_head_over_64_kib_fails_closed() {
    head_over_64_kib(Backend::Go);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_response_head_over_64_kib_fails_closed() {
    head_over_64_kib(Backend::Deno);
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_directive_header_edges_at_the_host_border() {
    deno_directive_header_edges();
}

#[test]
#[ignore = "requires the real edger binary and the Go/WASM module (EDGER_GO_WASM_SSE_FIXTURE)"]
fn go_wasm_repeated_subscribe_header_is_refused() {
    go_repeated_subscribe_header();
}

#[test]
#[ignore = "requires the real edger binary and Deno (EDGER_DENO_BIN)"]
fn deno_repeated_headers_without_sse_reach_the_client() {
    deno_repeated_headers_without_sse();
}

#[test]
fn the_real_runtime_tests_are_all_ignored_and_the_suite_names_what_they_need() {
    // Without the runtimes an ordinary run must not start anything: the real
    // tests are `#[ignore]`d and an `--ignored` run panics on a missing file.
    assert!(NEEDS.contains("EDGER_GO_WASM_SSE_FIXTURE") && NEEDS.contains("EDGER_DENO_BIN"));
    let guard = std::panic::catch_unwind(|| require_file("EDGER_SSE_E2E_NO_SUCH_VARIABLE"));
    assert!(
        guard.is_err(),
        "a missing runtime must be a failure, never a skip"
    );
}

/// A scratch example dir + module + provenance manifest in a temp dir.
fn provenance_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let example = root.join("example");
    fs::create_dir_all(&example).unwrap();
    for name in GO_SOURCES {
        fs::write(example.join(name), format!("source of {name}")).unwrap();
    }
    let module = root.join("module.wasm");
    fs::write(&module, b"\0asm fake module").unwrap();
    write_provenance(&example, &module, GO_BUILD_COMMAND);
    (example, module)
}

fn write_provenance(example: &Path, module: &Path, build: &str) {
    let mut text = String::new();
    for name in GO_SOURCES {
        let bytes = fs::read(example.join(name)).unwrap();
        text.push_str(&format!("{}  {name}\n", sha256_hex(&bytes)));
    }
    let wasm = fs::read(module).unwrap();
    let module_name = module.file_name().unwrap().to_string_lossy();
    text.push_str(&format!("{}  {module_name}\n", sha256_hex(&wasm)));
    text.push_str(&format!("build: {build}\n"));
    fs::write(format!("{}.provenance", module.display()), text).unwrap();
}

/// The lines of a freshly written, valid manifest.
fn valid_provenance_lines(example: &Path, module: &Path) -> Vec<String> {
    write_provenance(example, module, GO_BUILD_COMMAND);
    let text = fs::read_to_string(format!("{}.provenance", module.display())).unwrap();
    text.strip_suffix('\n')
        .expect("the generated manifest ends with one terminator")
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn store_provenance(module: &Path, lines: &[String], terminator: &str) {
    fs::write(
        format!("{}.provenance", module.display()),
        format!("{}{terminator}", lines.join("\n")),
    )
    .unwrap();
}

#[test]
fn the_provenance_binds_the_module_to_the_sources_and_the_build_command() {
    let dir = tempfile::tempdir().unwrap();
    let (example, module) = provenance_fixture(dir.path());
    // Control: an intact manifest passes and yields the hashed module bytes,
    // with or without the terminator of its last line.
    assert_eq!(
        verified_go_module(&module, &example).unwrap(),
        b"\0asm fake module"
    );
    let valid = valid_provenance_lines(&example, &module);
    assert_eq!(valid.len(), 5, "three sources, the module, the build line");
    store_provenance(&module, &valid, "");
    assert!(verified_go_module(&module, &example).is_ok());

    // A changed source is refused, each of the three.
    for name in GO_SOURCES {
        let path = example.join(name);
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"a regression").unwrap();
        let error = verified_go_module(&module, &example).unwrap_err();
        assert!(
            error.contains(name) && error.contains("differs"),
            "{name}: {error}"
        );
        fs::write(&path, original).unwrap();
        assert!(verified_go_module(&module, &example).is_ok());
    }

    // A changed module is refused.
    fs::write(&module, b"another module").unwrap();
    let error = verified_go_module(&module, &example).unwrap_err();
    assert!(
        error.contains("module.wasm") && error.contains("differs"),
        "{error}"
    );
    fs::write(&module, b"\0asm fake module").unwrap();

    // A changed build line is refused (a valid manifest, only the command).
    write_provenance(&example, &module, "go build -o module.wasm .");
    let error = verified_go_module(&module, &example).unwrap_err();
    assert!(error.contains("build line differs"), "{error}");

    // Each omission starts from a valid manifest and removes exactly one
    // record; the diagnostic names it.
    for name in ["abi.go", "main.go", "go.mod", "module.wasm"] {
        let mut lines = valid_provenance_lines(&example, &module);
        let before = lines.len();
        lines.retain(|line| !line.ends_with(&format!("  {name}")));
        assert_eq!(lines.len(), before - 1, "exactly one record for {name}");
        store_provenance(&module, &lines, "\n");
        let error = verified_go_module(&module, &example).unwrap_err();
        assert_eq!(error, format!("provenance has no line for {name}"));
    }
    let mut lines = valid_provenance_lines(&example, &module);
    lines.retain(|line| !line.starts_with("build: "));
    assert_eq!(lines.len(), 4);
    store_provenance(&module, &lines, "\n");
    assert_eq!(
        verified_go_module(&module, &example).unwrap_err(),
        "provenance has no `build:` line"
    );

    // Independent hostile shapes, each from a valid manifest. Index 1 is
    // `main.go`, 3 the module, 4 the build line.
    let hash_of = |line: &str| line[..64].to_owned();
    let mut cases: Vec<(&str, Vec<String>, &str)> = Vec::new();
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push(format!("{}  main.go", "0".repeat(64)));
    cases.push((
        "main.go repeated with a conflicting hash",
        lines,
        "two lines for main.go",
    ));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push(lines[3].clone());
    cases.push((".wasm repeated", lines, "two lines for module.wasm"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push(format!("{}  sub/extra.go", hash_of(&lines[0])));
    cases.push((
        "extra record with a path",
        lines,
        "name sub/extra.go has a path",
    ));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push(format!("{}  ../main.go", hash_of(&lines[1])));
    cases.push((
        "record with a parent path",
        lines,
        "name ../main.go has a path",
    ));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push(format!("{}  other.go", hash_of(&lines[0])));
    cases.push(("unexpected bare name", lines, "unexpected name other.go"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push("trailing garbage".to_owned());
    cases.push(("stray trailing line", lines, "line 6 is not"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.push(lines[4].clone());
    cases.push(("build repeated", lines, "two build lines"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines.insert(2, String::new());
    cases.push(("blank line in the middle", lines, "line 3 is blank"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines[0] = lines[0][..63].to_owned() + "  abi.go";
    cases.push(("63 hex digits", lines, "line 1 is not"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines[1] = lines[1].replacen(&hash_of(&lines[1])[..1], "G", 1);
    cases.push(("a non-hex digit", lines, "line 2 is not"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines[2] = lines[2].replacen("  ", " ", 1);
    cases.push(("one space instead of two", lines, "line 3 is not"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines[0] = lines[0].replacen("  ", "\t\t", 1);
    cases.push(("tabs instead of spaces", lines, "line 1 is not"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines[0] = lines[0].to_ascii_uppercase().replace("ABI.GO", "abi.go");
    cases.push(("uppercase hex", lines, "line 1 is not"));
    let mut lines = valid_provenance_lines(&example, &module);
    lines[1].push_str("  trailing");
    cases.push(("extra content after the name", lines, "line 2 is not"));
    for (label, lines, want) in &cases {
        store_provenance(&module, lines, "\n");
        let error = verified_go_module(&module, &example).expect_err(label);
        assert!(
            error.contains(want),
            "{label}: wanted `{want}`, got `{error}`"
        );
    }
    // A second terminator (a blank last line) and CRLF endings are extra content.
    let valid = valid_provenance_lines(&example, &module);
    store_provenance(&module, &valid, "\n\n");
    assert_eq!(
        verified_go_module(&module, &example).unwrap_err(),
        "provenance line 6 is blank"
    );
    store_provenance(&module, &valid, "\n");
    let crlf = fs::read_to_string(format!("{}.provenance", module.display()))
        .unwrap()
        .replace('\n', "\r\n");
    fs::write(format!("{}.provenance", module.display()), crlf).unwrap();
    assert!(verified_go_module(&module, &example).is_err());
    // Control: the valid manifest is accepted again after all of this.
    store_provenance(&module, &valid, "\n");
    assert!(verified_go_module(&module, &example).is_ok());

    // A missing manifest is refused.
    fs::remove_file(format!("{}.provenance", module.display())).unwrap();
    let error = verified_go_module(&module, &example).unwrap_err();
    assert!(error.contains("missing or unreadable"), "{error}");
}

/// A one-shot fake SSE server: sends the response head, then runs `script`.
fn fake_stream_server(script: impl FnOnce(&mut TcpStream) + Send + 'static) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap_or(0) == 0 {
                return;
            }
            request.push(byte[0]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .unwrap();
        script(&mut stream);
    });
    port
}

#[test]
fn the_reader_tells_a_completed_body_from_a_broken_connection() {
    let wait = Duration::from_secs(5);
    // Completed: a frame, then the terminating chunk.
    let port = fake_stream_server(|stream| {
        stream.write_all(b"9\r\ndata: a\n\n\r\n0\r\n\r\n").unwrap();
    });
    let mut sub = Sub::open(port, "/x", None);
    sub.wait_events(1, wait);
    assert_eq!(sub.wait_end(wait), Some(End::Completed));
    sub.expect_completed(wait, "completed body");

    // Completed with a trailer section (read and discarded) as well.
    let port = fake_stream_server(|stream| {
        stream
            .write_all(b"9\r\ndata: a\n\n\r\n0\r\nX-Trailer: v\r\n\r\n")
            .unwrap();
    });
    let mut sub = Sub::open(port, "/x", None);
    sub.wait_events(1, wait);
    sub.expect_completed(wait, "body with a trailer");

    // Premature EOF after a whole chunk, in the middle of one, right after
    // the zero-size chunk line, in the middle of the final CRLF, in the middle
    // of a trailer and after a whole trailer line without the final CRLF.
    for script in [
        b"9\r\ndata: a\n\n\r\n".as_slice(),
        b"9\r\ndata: a\n\n\r\n20\r\ndata: tru".as_slice(),
        b"9\r\ndata: a\n\n\r\n0\r\n".as_slice(),
        b"9\r\ndata: a\n\n\r\n0\r\n\r".as_slice(),
        b"9\r\ndata: a\n\n\r\n0\r\nX-Trailer: v".as_slice(),
        b"9\r\ndata: a\n\n\r\n0\r\nX-Trailer: v\r\n".as_slice(),
    ] {
        let port = fake_stream_server(move |stream| stream.write_all(script).unwrap());
        let mut sub = Sub::open(port, "/x", None);
        assert_eq!(sub.wait_end(wait), Some(End::PrematureEof));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sub.expect_completed(wait, "truncated body")
        }));
        assert!(
            outcome.is_err(),
            "a truncated body must not count as completed"
        );
    }

    // A reader thread that dies (a chunk size line it cannot read).
    let port = fake_stream_server(|stream| stream.write_all(b"zz\r\ndata\r\n").unwrap());
    let mut sub = Sub::open(port, "/x", None);
    assert_eq!(sub.wait_end(wait), Some(End::ReaderFailed));

    // A read error is its own result.
    struct Failing;
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::ConnectionReset))
        }
    }
    let (sender, receiver) = mpsc::channel();
    run_reader(Failing, Vec::new(), sender);
    match receiver.recv().unwrap() {
        Frame::End(End::ReadError(message)) => assert!(!message.is_empty()),
        other => panic!("{other:?}"),
    }
    // Control: a still-open stream has no end.
    let port = fake_stream_server(|stream| {
        stream.write_all(b"9\r\ndata: a\n\n\r\n").unwrap();
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sub = Sub::open(port, "/x", None);
    sub.wait_events(1, wait);
    assert_eq!(sub.wait_end(Duration::from_millis(300)), None);
}

#[test]
fn the_chunk_decoder_handles_any_split_and_ends_on_the_zero_chunk() {
    let wire = b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
    for split in 0..wire.len() {
        let mut decoder = ChunkDecoder::default();
        let mut out = decoder.feed(&wire[..split]);
        out.extend(decoder.feed(&wire[split..]));
        assert_eq!(out, b"hello world", "split at {split}");
        assert!(decoder.ended);
    }
    // A trailer section is read and discarded; the body is done only after
    // its final empty line, at any split.
    let wire = b"5\r\nhello\r\n0\r\nX-Trailer: v\r\nX-Other: w\r\n\r\n";
    for split in 0..wire.len() {
        let mut decoder = ChunkDecoder::default();
        let mut out = decoder.feed(&wire[..split]);
        out.extend(decoder.feed(&wire[split..]));
        assert_eq!(out, b"hello", "split at {split}");
        assert!(decoder.ended, "split at {split}");
    }
    // Every prefix that stops before the final CRLF is not an end.
    for incomplete in [
        b"0\r\n".as_slice(),
        b"0\r\n\r".as_slice(),
        b"0\r\nX-T: 1".as_slice(),
        b"0\r\nX-T: 1\r".as_slice(),
        b"0\r\nX-T: 1\r\n".as_slice(),
        b"0\r\nX-T: 1\r\nX-U: 2\r\n".as_slice(),
    ] {
        let mut decoder = ChunkDecoder::default();
        decoder.feed(incomplete);
        assert!(!decoder.ended, "{incomplete:?} is not a complete message");
        assert!(dechunk(incomplete).is_err(), "{incomplete:?}");
    }
    for complete in [b"0\r\n\r\n".as_slice(), b"0\r\nX-T: 1\r\n\r\n".as_slice()] {
        let mut decoder = ChunkDecoder::default();
        decoder.feed(complete);
        assert!(decoder.ended, "{complete:?}");
        assert!(dechunk(complete).is_ok(), "{complete:?}");
    }
    let mut cut = ChunkDecoder::default();
    assert_eq!(cut.feed(b"5\r\nhel"), Vec::<u8>::new());
    assert!(!cut.ended, "a truncated chunk is not an end");
    assert!(dechunk(b"5\r\nhel").is_err());
}

#[test]
fn the_frame_parser_reads_events_comments_and_retry() {
    let wire = b": ping\n\nretry: 2500\n\nevent: msg\nid: 7\ndata: a\ndata: b\n\ndata: x\n\n";
    for split in 0..wire.len() {
        let mut parser = FrameParser::default();
        let mut frames = parser.feed(&wire[..split]);
        frames.extend(parser.feed(&wire[split..]));
        assert_eq!(
            frames,
            vec![
                Frame::Comment("ping".into()),
                Frame::Retry(2500),
                Frame::Event(Ev::new("a\nb", Some("msg"), Some("7"))),
                Frame::Event(Ev::new("x", None, None)),
            ],
            "split at {split}"
        );
    }
    let mut parser = FrameParser::default();
    assert!(parser.feed(b"data: unfinished").is_empty());
}

#[test]
fn the_metric_reader_sums_by_worker_and_reason_and_reads_a_missing_series_as_zero() {
    let text = "# HELP edger_sse_connections x\n# TYPE edger_sse_connections gauge\n\
edger_sse_connections{worker=\"a\"} 3\n\
edger_sse_connections{worker=\"ab\"} 9\n\
edger_sse_disconnects_total{worker=\"a\",reason=\"client\"} 2\n\
edger_sse_disconnects_total{worker=\"a\",reason=\"lagged\"} 1\n\
edger_sse_disconnects_total{worker=\"b\",reason=\"client\"} 7\n";
    assert_eq!(metric_value(text, "edger_sse_connections", "a", None), 3.0);
    assert_eq!(
        metric_value(text, "edger_sse_disconnects_total", "a", Some("client")),
        2.0
    );
    assert_eq!(
        metric_value(text, "edger_sse_disconnects_total", "a", None),
        3.0
    );
    assert_eq!(
        metric_value(text, "edger_sse_connections", "none", None),
        0.0
    );
    assert_eq!(
        metric_value(text, "edger_sse_rejected_total", "a", Some("capacity")),
        0.0
    );
}
