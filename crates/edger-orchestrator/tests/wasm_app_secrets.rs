//! (EDG-52 06b/07c) Real-vault, real-engine integration: the SQLite +
//! ChaCha20 `AppSecretVault` as the async `AppSecretResolver` of a real
//! WASM isolate (wasmtime WASIp1), over the 06a-validated environ-dump
//! fixture (the guest dumps `environ_get` into the response body; headers
//! @40012, body @40043).
//!
//! (EDG-52 07c) The isolate is assembled by `assemble_wasm_isolate` — the
//! SAME shared factory function the `edger` bin's `WasmModule` branch
//! delegates to: the host-trusted `WorkerRef` identity (never
//! request/URI/headers), the real vault as the resolver, and no egress
//! guard (these scenarios have no egress grant, so the guest HTTP import
//! stays unbound and default-denied by the runtime). It proves the vault →
//! resolver → per-execution WASI env path through the real factory
//! assembly, not a manual copy of it. It is not the EDG-52 Go/sqld/HTTP
//! E2E (07c2 adds the WAT → guard → TLS e2e through the same assembly).
//!
//! Canary discipline: the guest returns the env dump for the assertions;
//! the canary values are fake, and failure messages never embed the dump or
//! the response body (only lengths).

use std::sync::Arc;

use edger_core::{
    parse_worker_config, Isolate, SerializedRequest, WorkerConfig, WorkerManifest, WorkerRef,
};
use edger_orchestrator::app_secrets::AppSecretVault;
use edger_orchestrator::assemble_wasm_isolate;
use zeroize::Zeroizing;

/// (EDG-52 06a) The 06a-validated env-dump fixture, reused verbatim: the
/// guest calls `environ_sizes_get` + `environ_get` and answers with the
/// 12-byte frame, the 31-byte header JSON @ 40012 and the body
/// `[u32 count][u32 buf_size][NUL-terminated NAME=VALUE entries]` @ 40043.
const ENV_DUMP_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "environ_sizes_get"
            (func $sizes (param i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "environ_get"
            (func $get (param i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "[[\"content-type\",\"text/plain\"]]")
          (global $heap (mut i32) (i32.const 49000))

          (func (export "edger_alloc") (param $len i32) (result i32)
            (local $ptr i32)
            global.get $heap
            local.set $ptr
            global.get $heap
            local.get $len
            i32.add
            global.set $heap
            local.get $ptr
          )

          (func $copy (param $dst i32) (param $src i32) (param $len i32)
            (local $i i32)
            loop $copy_loop
              local.get $i
              local.get $len
              i32.lt_u
              if
                local.get $dst
                local.get $i
                i32.add
                local.get $src
                local.get $i
                i32.add
                i32.load8_u
                i32.store8
                local.get $i
                i32.const 1
                i32.add
                local.set $i
                br $copy_loop
              end
            end
          )

          (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
            (local $status i32)
            (local $count i32)
            (local $buf_size i32)
            (local $body_len i32)
            (local $frame_len i32)

            ;; environ_sizes_get(count_ptr=1000, size_ptr=1004) -> errno
            i32.const 1000
            i32.const 1004
            call $sizes
            local.set $status
            local.get $status
            i32.eqz
            if
            else
              unreachable
            end

            i32.const 1000
            i32.load
            local.set $count
            i32.const 1004
            i32.load
            local.set $buf_size

            ;; Guard: the deterministic test env fits the 8 KiB scratch
            ;; buffer @ 8000 (memory is one 64 KiB page).
            local.get $buf_size
            i32.const 8192
            i32.gt_s
            if
              unreachable
            end

            ;; environ_get(env_ptrs=2000, env_buf=8000) -> errno
            i32.const 2000
            i32.const 8000
            call $get
            local.set $status
            local.get $status
            i32.eqz
            if
            else
              unreachable
            end

            ;; Response frame @ 40000 (12-byte header):
            ;;   u16 status = 200 @ 40000
            ;;   u32 headers_len = 31 @ 40004
            ;;   u32 body_len = 8 + buf_size @ 40008
            i32.const 40000
            i32.const 200
            i32.store16
            i32.const 40004
            i32.const 31
            i32.store
            local.get $buf_size
            i32.const 8
            i32.add
            local.set $body_len
            i32.const 40008
            local.get $body_len
            i32.store

            ;; Headers (the 31-byte JSON) @ 40012; body @ 40043:
            ;; [u32 count][u32 buf_size][entries from 8000]
            i32.const 40012
            i32.const 512
            i32.const 31
            call $copy

            i32.const 40043
            local.get $count
            i32.store
            i32.const 40047
            local.get $buf_size
            i32.store
            i32.const 40051
            i32.const 8000
            local.get $buf_size
            call $copy

            i32.const 40000
            i64.extend_i32_u
            i32.const 12
            local.get $body_len
            i32.add
            i32.const 31
            i32.add
            local.set $frame_len
            local.get $frame_len
            i64.extend_i32_u
            i64.const 32
            i64.shl
            i64.or
          )
        )
"#;

/// Walks the fixture body the way a guest `getenv` would: the NUL-terminated
/// `NAME=VALUE` entries in buffer order; for a duplicated name the FIRST
/// entry wins. Failure messages carry only lengths (canary discipline).
fn parse_env_body(body: &[u8]) -> Vec<(String, String)> {
    assert!(body.len() >= 8, "body too short ({} bytes)", body.len());
    let buf_size = u32::from_le_bytes(body[4..8].try_into().expect("u32 slice")) as usize;
    assert!(
        body.len() >= 8 + buf_size,
        "body shorter than the declared env buffer (body {} bytes, declared {buf_size})",
        body.len()
    );
    let mut out = Vec::new();
    for entry in body[8..8 + buf_size].split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = String::from_utf8(entry.to_vec()).expect("utf-8 entry");
        let (name, value) = entry.split_once('=').expect("NAME=VALUE entry");
        out.push((name.to_owned(), value.to_owned()));
    }
    out
}

fn first_value<'a>(entries: &'a [(String, String)], name: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn occurrences(entries: &[(String, String)], name: &str) -> usize {
    entries.iter().filter(|(key, _)| key == name).count()
}

fn sample_request() -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "wasm-vault-secrets".into(),
        base_href: None,
    }
}

/// The fixture manifest of one scenario (the app name the vault scopes its
/// records under, the declared secrets and the public env).
fn fixture_manifest(name: &str, secrets: &[&str], env: &[(&str, &str)]) -> WorkerManifest {
    let mut manifest = WorkerManifest {
        name: name.into(),
        kind: Some("wasm".into()),
        entrypoint: Some("index.wat".into()),
        ..Default::default()
    };
    manifest.secrets = Some(secrets.iter().map(|name| name.to_string()).collect());
    manifest.env = Some(
        env.iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    );
    manifest
}

/// A worker dir holding the fixture as `index.wat` + its manifest; the
/// isolate loads the module through the same worker-dir loader the
/// production path uses (`.wat` is compiled by the isolation crate's `wat`
/// dependency — no fixture changes).
fn wasm_fixture(
    name: &str,
    secrets: &[&str],
    env: &[(&str, &str)],
) -> (tempfile::TempDir, WorkerManifest) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.wat"), ENV_DUMP_WAT).unwrap();
    (dir, fixture_manifest(name, secrets, env))
}

/// The host-trusted `WorkerRef` the bin factory consumes for a fixture: the
/// identity fields the production loader derives from the manifest (name,
/// version `latest` — the manifest default, no namespace, the `WasmModule`
/// kind `parse_worker_config` already normalizes) and the same per-call
/// `WorkerConfig` the scenario always used (`parse_worker_config` + the
/// worker dir the loader sets at load time + the 30 s timeout default).
/// This assembly never re-validates the manifest, so the scenario whose
/// manifest the manifest validation would reject (the env/secrets collision
/// case below — the runtime's directly-built-config path) goes through the
/// same helper.
fn trusted_worker_ref(dir: &std::path::Path, manifest: &WorkerManifest) -> WorkerRef {
    let mut config = parse_worker_config(manifest);
    config.worker_dir = Some(dir.to_path_buf());
    config.timeout_ms = 30_000;
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: manifest.name.clone(),
        version: manifest
            .version
            .clone()
            .unwrap_or_else(|| "latest".to_string()),
        dir: dir.to_path_buf(),
        namespace: None,
        kind: config
            .kind
            .clone()
            .unwrap_or(edger_core::ExecutionKind::FetchHandler),
        config,
    }
}

/// Fake 32-byte master key (tempdir-only; never real key material).
fn fake_key(byte: u8) -> Zeroizing<[u8; 32]> {
    Zeroizing::new([byte; 32])
}

/// A REAL vault (SQLite file in the tempdir) under the fake key.
fn real_vault(dir: &tempfile::TempDir, key_byte: u8) -> AppSecretVault {
    AppSecretVault::open(dir.path().join("app-secrets.db"), fake_key(key_byte)).unwrap()
}

/// Runs one execution on the assembled isolate (the concrete `WasmIsolate`
/// the shared factory returns — the bin Boxes that same object at its
/// `Box<dyn Isolate>` dispatch) and returns the guest's env dump.
async fn dump_env(isolate: &mut dyn Isolate, config: &WorkerConfig) -> Vec<(String, String)> {
    let res = isolate
        .execute_wasm(sample_request(), config)
        .await
        .expect("the execution must succeed");
    assert_eq!(res.status, 200);
    parse_env_body(res.body.expect("the fixture answers with a body").as_ref())
}

/// The declared secret resolved for the bound worker (identity from the
/// `with_app_secrets` binding — never the request/URI/headers) reaches the
/// guest of THAT worker only: the other app's value and the undeclared name
/// never arrive, and the public env is preserved without duplication.
#[tokio::test]
async fn a_declared_secret_reaches_only_the_bound_app() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(real_vault(&dir, 1));
    vault.set("wasm-app-A", "APP_SECRET", "canary-A").unwrap();
    vault.set("wasm-app-B", "APP_SECRET", "canary-B").unwrap();
    vault.set("wasm-app-A", "UNDECLARED", "canary-U").unwrap();

    let (wat_dir, manifest) = wasm_fixture(
        "wasm-app-A",
        &["APP_SECRET"],
        &[("PUB_ONLY", "public-canary")],
    );
    let worker_ref = trusted_worker_ref(wat_dir.path(), &manifest);
    let mut isolate = assemble_wasm_isolate(&worker_ref, Some(vault.clone()), None);
    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert_eq!(first_value(&env, "APP_SECRET"), Some("canary-A"));
    assert_eq!(occurrences(&env, "APP_SECRET"), 1);
    assert_eq!(first_value(&env, "PUB_ONLY"), Some("public-canary"));
    assert!(
        !env.iter().any(|(_, value)| *value == "canary-B"),
        "app B's value must not reach app A's guest"
    );
    assert!(
        first_value(&env, "UNDECLARED").is_none(),
        "an undeclared name must never be resolved"
    );

    // The other app, same vault, same worker dir: its own value, and never
    // app A's.
    let manifest_b = fixture_manifest(
        "wasm-app-B",
        &["APP_SECRET"],
        &[("PUB_ONLY", "public-canary")],
    );
    let worker_ref_b = trusted_worker_ref(wat_dir.path(), &manifest_b);
    let mut isolate_b = assemble_wasm_isolate(&worker_ref_b, Some(vault.clone()), None);
    let env_b = dump_env(&mut isolate_b, &worker_ref_b.config).await;
    assert_eq!(first_value(&env_b, "APP_SECRET"), Some("canary-B"));
    assert!(
        !env_b.iter().any(|(_, value)| *value == "canary-A"),
        "app A's value must not reach app B's guest"
    );
    drop(wat_dir);
}

/// Rotation and removal in the REAL vault are re-resolved per execution on
/// the SAME isolate: the new value is visible on the next call, a removed
/// secret is absent, and no stale value from an earlier execution's Store
/// is reused.
#[tokio::test]
async fn rotation_and_removal_are_re_resolved_per_execution_on_the_same_isolate() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(real_vault(&dir, 1));
    vault
        .set("wasm-app", "ROTATED_SECRET", "canary-v1")
        .unwrap();
    let (wat_dir, manifest) = wasm_fixture("wasm-app", &["ROTATED_SECRET"], &[]);
    let worker_ref = trusted_worker_ref(wat_dir.path(), &manifest);
    let mut isolate = assemble_wasm_isolate(&worker_ref, Some(vault.clone()), None);

    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert_eq!(first_value(&env, "ROTATED_SECRET"), Some("canary-v1"));

    // Rotation on the SAME isolate + vault: the next execution sees the new
    // value.
    vault
        .set("wasm-app", "ROTATED_SECRET", "canary-v2")
        .unwrap();
    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert_eq!(first_value(&env, "ROTATED_SECRET"), Some("canary-v2"));

    // Removal: the variable is absent on the next execution.
    vault.delete("wasm-app", "ROTATED_SECRET").unwrap();
    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert!(first_value(&env, "ROTATED_SECRET").is_none());
    drop(wat_dir);
}

/// A secret name that collides with a public env entry: the private value
/// REPLACES the public entry (one entry, no duplicate) — the guest sees the
/// secret, not the public value.
#[tokio::test]
async fn a_secret_wins_over_a_public_env_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(real_vault(&dir, 1));
    vault
        .set("wasm-app", "SHARED_NAME", "canary-secret")
        .unwrap();
    // The manifest declares the SAME name in `env` and `secrets`.
    let (wat_dir, manifest) = wasm_fixture(
        "wasm-app",
        &["SHARED_NAME"],
        &[("SHARED_NAME", "canary-public")],
    );
    let worker_ref = trusted_worker_ref(wat_dir.path(), &manifest);
    let mut isolate = assemble_wasm_isolate(&worker_ref, Some(vault.clone()), None);
    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert_eq!(first_value(&env, "SHARED_NAME"), Some("canary-secret"));
    assert_eq!(
        occurrences(&env, "SHARED_NAME"),
        1,
        "the secret must replace the public entry, not duplicate it"
    );
    drop(wat_dir);
}

/// Absent and unavailable vaults fail closed without failing the execution:
/// (a) available vault without the record, (b) unavailable vault (no master
/// key), (c) a record that does not decrypt under the rotated key — the
/// execution answers 200, the variable is absent, the public env is intact.
#[tokio::test]
async fn absent_and_unavailable_vaults_fail_closed_without_failing_the_execution() {
    // (a) Available vault, absent record.
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(real_vault(&dir, 1));
    let (wat_dir, manifest) = wasm_fixture(
        "wasm-app",
        &["ABSENT_SECRET"],
        &[("PUB_ONLY", "public-canary")],
    );
    let worker_ref = trusted_worker_ref(wat_dir.path(), &manifest);
    let mut isolate = assemble_wasm_isolate(&worker_ref, Some(vault.clone()), None);
    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert!(first_value(&env, "ABSENT_SECRET").is_none());
    assert_eq!(first_value(&env, "PUB_ONLY"), Some("public-canary"));

    // (b) Unavailable vault (no master key).
    let unavailable: Arc<dyn edger_core::AppSecretResolver> =
        Arc::new(AppSecretVault::unavailable());
    let (wat_dir2, manifest2) = wasm_fixture(
        "wasm-app",
        &["ABSENT_SECRET"],
        &[("PUB_ONLY", "public-canary")],
    );
    let worker_ref2 = trusted_worker_ref(wat_dir2.path(), &manifest2);
    let mut isolate2 = assemble_wasm_isolate(&worker_ref2, Some(unavailable), None);
    let env2 = dump_env(&mut isolate2, &worker_ref2.config).await;
    assert!(first_value(&env2, "ABSENT_SECRET").is_none());
    assert_eq!(first_value(&env2, "PUB_ONLY"), Some("public-canary"));

    // (c) A record that does not decrypt under the (rotated) master key:
    // fail closed through the REAL vault decrypt — the value never surfaces.
    let dir3 = tempfile::tempdir().unwrap();
    let writer = real_vault(&dir3, 1);
    writer
        .set("wasm-app", "ROTATED_AWAY", "canary-old")
        .unwrap();
    let reader = Arc::new(real_vault(&dir3, 2)); // the key changed
    let (wat_dir3, manifest3) = wasm_fixture("wasm-app", &["ROTATED_AWAY"], &[]);
    let worker_ref3 = trusted_worker_ref(wat_dir3.path(), &manifest3);
    let mut isolate3 = assemble_wasm_isolate(&worker_ref3, Some(reader), None);
    let env3 = dump_env(&mut isolate3, &worker_ref3.config).await;
    assert!(first_value(&env3, "ROTATED_AWAY").is_none());
    drop((wat_dir, wat_dir2, wat_dir3));
}

/// Secret injection leaves the public config surface intact with the real
/// vault: the per-call `WorkerConfig` (env/secrets) is not mutated by the
/// execution, and neither the `WasiConfig` (derived `Debug` is public) nor
/// the `WorkerConfig`'s `Debug` ever carries the resolved value.
#[tokio::test]
async fn config_and_debug_surfaces_stay_intact_with_the_real_vault() {
    let canary = "cfg-canary-value";
    let dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(real_vault(&dir, 1));
    vault.set("wasm-app", "CFG_SECRET", canary).unwrap();
    let (wat_dir, manifest) = wasm_fixture(
        "wasm-app",
        &["CFG_SECRET"],
        &[("PUB_ONLY", "public-canary")],
    );
    let worker_ref = trusted_worker_ref(wat_dir.path(), &manifest);
    let env_before = worker_ref.config.env.clone();
    let secrets_before = worker_ref.config.secrets.clone();
    let mut isolate = assemble_wasm_isolate(&worker_ref, Some(vault.clone()), None);
    let env = dump_env(&mut isolate, &worker_ref.config).await;
    assert_eq!(first_value(&env, "CFG_SECRET"), Some(canary));
    // The per-call config is not mutated by the injection.
    assert_eq!(worker_ref.config.env, env_before);
    assert_eq!(worker_ref.config.secrets, secrets_before);
    // No public surface (WasiConfig Debug, WorkerConfig Debug) carries the
    // resolved value.
    let wasi_debug = format!("{:?}", isolate.wasi_config());
    let config_debug = format!("{:?}", worker_ref.config);
    assert!(
        !wasi_debug.contains(canary),
        "the WasiConfig Debug must not carry the value"
    );
    assert!(
        !config_debug.contains(canary),
        "the WorkerConfig Debug must not carry the value"
    );
    drop(wat_dir);
}
