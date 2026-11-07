//! Isolated executable for the fixture that overrides EDGER_DENO_BIN.
//! Other UDS tests run in another process and cannot observe this override.

#![cfg(all(feature = "multiproc", unix))]

use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use edger_core::SerializedRequest;
use edger_isolation::DenoWorkerProcess;

fn write_worker(dir: &Path, body: &str) {
    fs::write(dir.join("index.ts"), body).unwrap();
}

fn request(method: &str, uri: &str, body: Option<&[u8]>) -> SerializedRequest {
    SerializedRequest {
        method: method.into(),
        uri: uri.into(),
        headers: vec![],
        body: body.map(bytes::Bytes::copy_from_slice),
        request_id: "uds-test".into(),
        base_href: None,
    }
}

#[cfg(unix)]
struct EnvVarGuard {
    key: &'static str,
    old: Option<OsString>,
}

#[cfg(unix)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let old = env::var_os(key);
        env::set_var(key, value);
        Self { key, old }
    }
}

#[cfg(unix)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.old {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[cfg(unix)]
fn find_deno_executable() -> Option<PathBuf> {
    if let Ok(executable) = env::var("EDGER_DENO_BIN") {
        if !executable.trim().is_empty() {
            let path = PathBuf::from(executable);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    if let Some(path_var) = env::var_os("PATH") {
        if let Some(path) = env::split_paths(&path_var)
            .map(|dir| dir.join("deno"))
            .find(|path| path.is_file())
        {
            return Some(path);
        }
    }
    env::var("HOME")
        .ok()
        .map(|home| Path::new(&home).join(".deno/bin/deno"))
        .filter(|path| path.is_file())
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn write_no_bundle_deno_wrapper(dir: &Path, real_deno: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let wrapper = dir.join("deno-no-bundle");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = "bundle" ]; then
  echo "unexpected deno bundle invocation" >&2
  exit 97
fi
exec {} "$@"
"#,
            shell_quote(real_deno)
        ),
    )
    .expect("write deno wrapper");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");
    wrapper
}

// Mutation captured: restoring unconditional `deno bundle` in spawn would hit
// the wrapper's blocked bundle command and the worker would fail before ready.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn single_file_worker_uses_direct_entrypoint_when_bundler_command_is_blocked() {
    let Some(real_deno) = find_deno_executable() else {
        eprintln!("skipping single_file_worker_uses_direct_entrypoint_when_bundler_command_is_blocked: deno executable not found");
        return;
    };
    let wrapper_dir = tempfile::tempdir().unwrap();
    let wrapper = write_no_bundle_deno_wrapper(wrapper_dir.path(), &real_deno);
    let _env_guard = EnvVarGuard::set("EDGER_DENO_BIN", wrapper.as_os_str());

    let dir = tempfile::tempdir().unwrap();
    // EDG-32: keep the per-worker Deno cache inside this test's own tempdir.
    let deno_cache = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve(() => new Response("fast path"));"#,
    );

    let mut worker = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        None,
        Some(deno_cache.path()),
    )
    .await
    .expect("single-file worker should spawn without deno bundle");

    let res = worker
        .request(request("GET", "/", None))
        .await
        .expect("single-file request");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.unwrap().as_ref(), b"fast path");
}
