//! (EDG-52 09SQLDPrep) The REAL SQLD fixture over the REAL Go module —
//! prepared in this slice, not executed here.
//!
//! Two artifacts pin this test to production material, both immutable:
//!
//! * the `sqld` binary of libsql-server-v0.24.32 (pinned at
//!   `40c272de85ee4e62d722c5ccae5da2e76b4253a1`), named by `EDGER_SQLD_BIN`
//!   (operator runtime:
//!   `/root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld`);
//! * the REAL Go build artifact `index.wasm` of
//!   `workers/examples/go-wasm-http`, named by `EDGER_GO_WASM_FIXTURE`
//!   (its SHA-256 `8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0`
//!   is checked; the artifact is never rebuilt here).
//!
//! This slice ships the test COMPILABLE plus FOUR OFFLINE proofs, all
//! non-ignored and in-process only (no child, no WSL, no Go, no new
//! binary): the crypto proof
//! (`fixture_tokens_use_scoped_eddsa_claims`: rcgen + ring +
//! jsonwebtoken); the privacy-scan proof
//! (`privacy_scan_helpers_refuse_secrets_and_keep_labels_static`:
//! synthetic JSON values and byte scans of the helpers the real fixture
//! uses); the REAL-store pagination/retention proof
//! (`real_store_pages_retention_and_privacy_scan`: two real
//! `OperationalStore`s at capacity 1024 — 401 retained events of one
//! identity span 3 pages of 200, the SAME scan helper covers every
//! page, and an oldest-event canary on page 3 is refused with the
//! static label; retained events only, no concurrent-growth claim) and
//! the local-file BOUNDARY proof
//! (`child_log_bytes_enforces_the_read_bound`: a `tempfile` file of
//! EXACTLY `LOG_READ_MAX` is read in full by the SAME `child_log_bytes`
//! and `LOG_READ_MAX + 1` panics with the static bound label —
//! `Read::take(max + 1)` statically enforces the read cap, a claimed
//! concurrent growth is NOT tested). The real fixture
//! (`real_go_vault_sqld_namespaces_and_denials`) is `#[ignore]` —
//! "requires operator-approved real SQLD and built Go artifact" — and is
//! NOT executed in this slice, not even with `--include-ignored`.
//!
//! The real fixture also PREPARES the privacy assertions R18 left open
//! (its earlier checks covered only the config/Debug surfaces): after
//! the final engine probes and the explicit child shutdown, EVERY event
//! the store returns (all serialized string fields AND object keys, the
//! query paginated to the end) and the captured child stdout/stderr
//! (read AFTER the child is confirmed stopped, bounded 1 MiB, plain AND
//! JSON-escaped forms) are checked against the fixture secret values —
//! the three JWTs, the literal admin key, the full private PEM and its
//! `PRIVATE KEY` header. Failure labels are STATIC: a failure never
//! prints a secret, an event or a log body. These assertions are
//! PREPARED and COMPILED only: the absence of secrets in the ACTUAL
//! events and logs is UNEXECUTED in this slice (the engine never runs
//! here). The scan covers exactly the named secret values in plain and
//! JSON-escaped form; it makes NO claim of exhaustive secret encodings
//! (base64, hashes, fragments, re-serializations) or of root/host
//! memory protection — the store output and the two captured files are
//! the only surfaces asserted.
//!
//! The real fixture runs the real server, the real vault and the real Go
//! module in ONE fixture (disjoint proofs are not joined from elsewhere):
//!
//! * the `sqld` child on two DISTINCT ephemeral loopback ports with
//!   `--db-path <temp>/data.sqld --http-listen-addr 127.0.0.1:<USERPORT>
//!   --admin-listen-addr 127.0.0.1:<ADMINPORT> --admin-auth-key
//!   canary-admin-go-sqld --enable-namespaces --disable-default-namespace
//!   --auth-jwt-key-file <temp>/ed25519.pub`; the child env is CLEARED
//!   (`PATH=/usr/bin:/bin`, `HOME=<temp>`, `RUST_LOG=error`) — it never
//!   inherits `SQLD_PRIMARY_GRPC_URL`, AWS or any production env, and no
//!   `--primary-grpc-url` is passed, so the server is a Primary (lazy
//!   creation false at `NamespaceStore::new`). `tokio` child with
//!   `kill_on_drop(true)` (the RAII fallback on panic), explicit
//!   kill+wait on every normal path, bounded startup 30 s with the child
//!   exit CHECKED (not slept through) and the admin HTTP bounded 3 s.
//!   stdout/stderr go to files in the tempdir (never the test output, so
//!   no JWT can leak into a log).
//! * admin (literal `Basic canary-admin-go-sqld`, NEVER base64, never
//!   Bearer): `POST /v1/namespaces/nsa1/create` and `/nsb1/create` with
//!   `{}` answer 200; `GET` config of an unknown namespace answers 404
//!   without side effect; a wrong admin key answers 401. The duplicate
//!   re-create of the existing `nsa1` (400 with the JSON error string)
//!   is probed AFTER the marker data exists, not before it.
//! * user (`POST /v2/pipeline`, `Authorization: Bearer <JWT EdDSA>`,
//!   `x-namespace-bin` = base64 STANDARD_NO_PAD of the namespace — the
//!   codec the pinned server decodes): missing auth answers 401 (the
//!   probe still carries the KNOWN namespace's `x-namespace-bin`: the
//!   pinned server resolves the namespace from that header first and,
//!   without it, from the FIRST LABEL of `Host` — `127` for
//!   `127.0.0.1:<port>` — answering the unknown-namespace 404 BEFORE
//!   the auth gate, which is the correct pinned behavior, not the
//!   denial under test); token A
//!   with the header of B answers 404 `NamespaceDoesntExist` BEFORE any
//!   Hrana processing (never 403, never a 200 with an in-band protocol
//!   error); token B with its own header succeeds (proving B exists).
//!   The raw user API creates the `marker` table in each namespace,
//!   inserts a DIFFERENT value per namespace, and SELECTs confirm the
//!   isolation (A sees only A's value, B only B's). The 400
//!   duplicate-create probe runs only AFTER those inserts and verified
//!   SELECTs: immediately after the 400 + error text a raw authenticated
//!   `SELECT v FROM marker` of A returns 200 with the exact MARKER_A
//!   (the data-preservation read; the final reads are retained too). A
//!   protocol error entry in the results is never accepted as a query
//!   success.
//! * Go REAL: the artifact is loaded from the env-named worker dir as
//!   `index.wasm`; the REAL example `manifest.yaml` is parsed against the
//!   `WorkerManifest` schema BEFORE the fixture overrides (the `SQLD_URL`
//!   env value pointed at the real user origin, the 60 s guest budget and
//!   the 4 mb body cap). App A is `go-wasm-http` with an RBAC namespace
//!   DISTINCT from the SQLD namespace `nsa1`; app B is `go-wasm-http-b`
//!   with its own RBAC namespace and the SQLD namespace `nsb1`. The real
//!   `AppSecretVault` (SQLite in the tempdir, synthetic 32-byte test
//!   master key) stores `SQLD_TOKEN` = JWT A / JWT B per app, PRIVATE.
//!   The SAME `assemble_wasm_isolate_with_database` and the STRICT guard
//!   (default block tables, NO allow CIDR, no general grant) run every
//!   execution.
//! * A `/db` answers 200 with the EXACT `{"ok":true,"value":1}` (the real
//!   query, validated by the real module); the public env, the per-call
//!   config and the WasiConfig Debug carry no JWT and no admin key. B
//!   with grant B answers 200. B assembled with grant A answers the
//!   static 502 and records the `EGRESS_DATABASE_DENIED` 403 event
//!   carrying B's context — the observed HOST owner denial; the owner
//!   check precedes the dial by the reviewed host control flow (the
//!   grant's exact owner/origin check runs before any credential lookup,
//!   DNS or dial), and the fixture does not independently measure SQLD
//!   accepts (a read-only marker SELECT can leave data unchanged after a
//!   dial, so the marker reads are data evidence, not dial counters). A
//!   grant whose owner stays A but whose SQLD namespace is NS_B (the
//!   SAME ref/isolate inputs of A, the SAME user origin, credential name
//!   and vault holding token A scoped ONLY to NS_A) answers the static
//!   502 and records the UPSTREAM 404 (outcome ok, status 404, no code)
//!   — the SQLD auth refusal caused solely by the wrong namespace. The
//!   marker visibility remains the raw SQLD client's proof: the
//!   scoped-JWT positive/negative Go controls connect Go to the
//!   namespace auth (the guest runs the fixed `SELECT 1`, never the
//!   marker). A no-grant assembly refuses the import with
//!   `WASM_IMPORT_DENIED` (not 502) and records zero new guard events.
//! * Removing the A secret from the vault leaves the credential absent
//!   in the guest env, and the Go module answers the static 503 itself
//!   before any fetch-import call (the observed evidence: the guest 503
//!   and zero new guard events; the A marker read is data evidence);
//!   restoring a DIFFERENT token (fresh exp, UTC + 601 s) on the SAME
//!   isolate/module cache answers 200 again — the proof is the actual
//!   query plus the delete/restore under the real composition and the
//!   cache control, NOT a byte-level auth-rotation claim: the real server
//!   does not revoke, so the old token may still validate there.
//!
//! Run (WSL Ubuntu-26.04, after the operator approves the binary and the
//! artifact is built; this slice never runs it):
//!
//!     EDGER_SQLD_BIN=/root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld \
//!     EDGER_GO_WASM_FIXTURE=/mnt/c/Users/dj4lm/repo.git/djalmajr/edger/.soho/w16/go-wasm/go-ns-host-root-index.wasm \
//!       cargo test -p edger-orchestrator --test go_wasm_sqld -- --ignored --test-threads=1
//!
//! The guest budget is the 60 s manifest timeout; the outer bound of
//! every execution is 120 s (the COLD Go module of ~12 MiB measured
//! ~16 s). The future external watchdog is 300 s `--kill-after 5`.

use std::future::Future;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::engine::Engine;
use edger_core::{
    parse_worker_config, ExecutionKind, Isolate, SerializedRequest, WorkerManifest, WorkerRef,
};
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventInput, OperationalEventLevel, OperationalEventQuery,
    OperationalEventSource, OperationalStore, OperationalStoreConfig,
};
use edger_orchestrator::{
    assemble_wasm_isolate_with_database, AppSecretVault, EgressDatabaseGrant, EgressGuard,
    EgressGuardConfig, EgressWorkerContext,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use zeroize::Zeroizing;

/// The outer bound of every execution (the COLD Go module of ~12 MiB
/// measured ~16 s; the guest budget is the 60 s manifest timeout).
const EXEC: Duration = Duration::from_secs(120);
/// The total deadline of the server startup wait (child exit checked at
/// every round; the admin HTTP of each probe is bounded separately).
const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
/// The bound of every admin HTTP probe (connect and read).
const ADMIN_HTTP: Duration = Duration::from_secs(3);
/// The bound of the user pipeline probes (real SQL on the real engine).
const USER_HTTP: Duration = Duration::from_secs(10);
/// The JWT validity of the fixture (UTC + 600 s; always finite).
const TOKEN_TTL: i64 = 600;
/// The app identity of the positive isolate (the real manifest name).
const APP_A: &str = "go-wasm-http";
/// The app identity of the second isolate (a distinct app, its own
/// RBAC namespace, the SQLD namespace `NS_B`).
const APP_B: &str = "go-wasm-http-b";
/// The SQLD namespace of the grant of app A (deliberately NOT a multiple
/// of 3 bytes: the NO_PAD and the padded profiles differ for it).
const NS_A: &str = "nsa1";
/// The SQLD namespace of the grant of app B.
const NS_B: &str = "nsb1";
/// The RBAC namespace of app A (distinct from the SQLD `NS_A`).
const RBAC_A: &str = "nsGo";
/// The RBAC namespace of app B (distinct from `RBAC_A` and from `NS_B`).
const RBAC_B: &str = "nsGoB";
/// The literal admin key sent as `Authorization: Basic <key>` (no
/// base64, not Bearer) — the contract of the pinned admin gate.
const ADMIN_KEY: &str = "canary-admin-go-sqld";
/// The vault credential name the Go module resolves per execution.
const CREDENTIAL: &str = "SQLD_TOKEN";
/// The identity of the immutable Go artifact (the SHA-256 the fixture
/// must match, hex lowercase).
const GO_FIXTURE_SHA: &str = "8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0";
/// The single global public key file the server reads (ONE file for the
/// whole fixture; every token is signed by its in-memory private key).
const PUB_KEY_FILE: &str = "ed25519.pub";
/// The unknown namespace of the admin readiness/denial probes (never
/// created; the 404 is the ready signal and the no-side-effect proof).
const PROBE_NS: &str = "go-sqld-unknown";
/// The synthetic 32-byte vault master key (test material, never a
/// production secret; the vault is encrypted with it at rest).
const VAULT_MASTER: [u8; 32] = [0x51; 32];
/// The Go module's static bodies (from `main.go`; never an upstream
/// body, URL, token or error text).
const DB_OK_BODY: &str = r#"{"ok":true,"value":1}"#;
const DB_502_BODY: &str = r#"{"error":"upstream error"}"#;
const DB_503_BODY: &str = r#"{"error":"configuration missing"}"#;
/// The marker values written per namespace (different by construction).
const MARKER_A: &str = "111";
const MARKER_B: &str = "222";
/// The read bound of one captured child log (1 MiB; a violation fails
/// with a static label — the content is never dumped).
const LOG_READ_MAX: usize = 1 << 20;
/// The private-key PEM header the fixture refuses anywhere (the public
/// key header is a different line: `PUBLIC KEY`).
const PRIV_PEM_HEADER: &str = "-----BEGIN PRIVATE KEY-----";
/// The static failure label of the event scan (it never names a secret,
/// an event or a field).
const EVENT_SECRET_LABEL: &str =
    "an operational event exposes a fixture secret (static label; no value printed)";
/// The static failure label of the child-log scan (it never names a
/// secret or the log content).
const LOG_SECRET_LABEL: &str =
    "a captured child log carries a fixture secret (static label; content not dumped)";
/// The static failure label of a child log that grew past the bound
/// between the stat and the read.
const LOG_BOUND_LABEL: &str =
    "a captured child log exceeds the 1 MiB read bound (static label; content not dumped)";

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(EXEC, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{what} exceeded the {EXEC:?} outer bound"),
    }
}

/// The STRICT guard: the default block tables and NO allow CIDR (the
/// classifier refuses the loopback for the GENERAL egress; only the
/// private grant admits it). No `tls_roots` change: the server is plain
/// HTTP on loopback.
fn strict_guard(store: &OperationalStore) -> std::sync::Arc<EgressGuard> {
    std::sync::Arc::new(
        EgressGuard::new(EgressGuardConfig::default()).with_event_store(store.clone()),
    )
}

/// The `worker.egress` events of the store.
fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    let query = OperationalEventQuery {
        kind: Some("worker.egress".into()),
        limit: Some(500),
        ..Default::default()
    };
    store.query(query).events
}

/// One exact field of the guard's stable `key=value` telemetry
/// (`build_message`: `host=… port=… [ip=…] bytes=… redirects=…
/// [reason=…]`, whitespace-separated): each part is split on the FIRST
/// `=` and the name must equal the key, so a value never matches by
/// substring.
fn message_field(message: &Option<String>, key: &str) -> Option<String> {
    message.as_ref().and_then(|message| {
        message.split_whitespace().find_map(|part| {
            part.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value.to_owned())
        })
    })
}

/// Every string one serialized value exposes (the object keys AND the
/// string values, recursively): the exact surface a store consumer
/// receives — the surface the privacy scan covers.
fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                out.push(key);
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

/// Refuses one serialized event that exposes any `secrets` value in a
/// string field or an object key (recursively). Returns the static
/// label on a hit (it never names the secret, the field or the event);
/// `None` when clean.
fn event_privacy_error(value: &Value, secrets: &[&str]) -> Option<&'static str> {
    let mut strings = Vec::new();
    collect_strings(value, &mut strings);
    let hit = strings.iter().any(|text| {
        secrets
            .iter()
            .any(|secret| !secret.is_empty() && text.contains(secret))
    });
    hit.then_some(EVENT_SECRET_LABEL)
}

/// The JSON-escaped form of a string (serde_json's escaping, without the
/// surrounding quotes): the form a JSON logger writes, where a newline
/// is the two characters `\` `n`.
fn json_escaped(secret: &str) -> String {
    let quoted = serde_json::to_string(secret).expect("a string serializes to JSON");
    quoted[1..quoted.len() - 1].to_string()
}

/// Refuses one captured child log (`bytes`) that carries any `secrets`
/// value in PLAIN or JSON-escaped form (newline escaping cannot bypass
/// the multi-line PEM check). Returns the static label on a hit (it
/// never names the secret or the log content); `None` when clean.
fn log_privacy_error(bytes: &[u8], secrets: &[&str]) -> Option<&'static str> {
    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        let plain = has_bytes(bytes, secret.as_bytes());
        let escaped = has_bytes(bytes, json_escaped(secret).as_bytes());
        if plain || escaped {
            return Some(LOG_SECRET_LABEL);
        }
    }
    None
}

/// Byte-level substring test (the log content is not assumed UTF-8).
fn has_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// One captured child log, read AFTER the child is confirmed stopped
/// (the buffers have drained). The read is bounded twice: the metadata
/// size is checked first (a violation fails with a static label — the
/// content is never dumped), and the read itself is capped at max+1 so
/// growth between the stat and the read is caught by the read, not
/// trusted from the metadata alone.
fn child_log_bytes(path: &Path) -> Vec<u8> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > LOG_READ_MAX as u64 {
            panic!("{LOG_BOUND_LABEL}");
        }
    }
    let file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("the captured child log cannot be opened: {e}"));
    let mut bytes = Vec::new();
    let read = Read::take(file, (LOG_READ_MAX as u64) + 1)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("the captured child log cannot be read: {e}"));
    if read > LOG_READ_MAX {
        panic!("{LOG_BOUND_LABEL}");
    }
    bytes
}

/// One `OperationalEventInput` for the offline store proofs (the input
/// has NO `Default`, so every field is listed): the SAME identity for
/// every event (namespace, worker and version — the store's
/// `identity_matches`), the `Runtime`/`Info` enums, and a
/// caller-provided message that is retained verbatim (short, under the
/// 500-char message cap, matching no redaction needle).
fn store_probe_input(message: String) -> OperationalEventInput {
    OperationalEventInput {
        source: OperationalEventSource::Runtime,
        kind: "fixture.probe".into(),
        level: OperationalEventLevel::Info,
        namespace: Some(RBAC_A.into()),
        worker: Some(APP_A.into()),
        version: Some("v1".into()),
        process_id: None,
        request_id: None,
        trace_id: None,
        outcome: Some("ok".into()),
        status: None,
        duration_ms: None,
        timeout_ms: None,
        method: None,
        path: None,
        content_type: None,
        code: None,
        message: Some(message),
        truncated: None,
        dropped_count: None,
    }
}

/// The store's FULL privacy scan — the loop the real fixture runs over
/// the REAL store, extracted for the offline store proofs: EVERY event
/// the store returns (no kind/request filter, the query paginated to
/// the end with the 200 page limit; the before-cursor is exclusive, so
/// a page is never repeated), each serialized to JSON and checked over
/// EVERY string field AND object key (a multi-line PEM cannot hide
/// behind JSON escaping). Returns the total event count after scanning
/// to exhaustion; on a hit it returns the static label (it never names
/// the secret, a field or the event).
fn scan_store_privacy(store: &OperationalStore, secrets: &[&str]) -> Result<usize, &'static str> {
    let mut before: Option<u64> = None;
    let mut event_count = 0usize;
    loop {
        let page = store.query(OperationalEventQuery {
            before,
            limit: Some(200),
            ..Default::default()
        });
        for event in &page.events {
            let serialized =
                serde_json::to_value(event).expect("an operational event serializes to JSON");
            if let Some(label) = event_privacy_error(&serialized, secrets) {
                return Err(label);
            }
            event_count += 1;
        }
        match page.next_cursor {
            Some(cursor) => before = Some(cursor),
            None => break,
        }
    }
    Ok(event_count)
}

/// The host request of one `/db` execution: the `request_id` is the HOST
/// identity the guard's events must carry (the guest frame carries none).
fn host_request(id: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/db".into(),
        headers: vec![],
        body: None,
        request_id: id.into(),
        base_href: None,
    }
}

/// The fixture discipline for the Go artifact: `EDGER_GO_WASM_FIXTURE`
/// names the REAL build. Absent env var, missing file, bad wasm magic,
/// an implausibly small file or a SHA mismatch is a clear panic — never a
/// skip, never an Ok.
fn fixture_wasm_path() -> std::path::PathBuf {
    let raw = std::env::var("EDGER_GO_WASM_FIXTURE").unwrap_or_else(|_| {
        panic!("EDGER_GO_WASM_FIXTURE must name the real Go build artifact (index.wasm); this test never skips")
    });
    let path = std::path::PathBuf::from(&raw);
    assert!(path.is_file(), "the fixture artifact is missing: {raw}");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("cannot read the fixture artifact {raw}: {e}"));
    assert_eq!(
        &bytes[..4],
        b"\0asm",
        "the fixture artifact is not a wasm binary (bad magic)"
    );
    assert!(
        bytes.len() > (1 << 20),
        "the fixture artifact is implausibly small ({} bytes) — not the real Go build",
        bytes.len()
    );
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        sha, GO_FIXTURE_SHA,
        "the fixture artifact identity is immutable (SHA-256 checked)"
    );
    path
}

/// The REAL manifest of the example, parsed against the `WorkerManifest`
/// schema with the same saphyr options the production loader uses (the
/// override happens AFTER this parses — a schema break cannot be hidden).
fn real_manifest() -> WorkerManifest {
    let example_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/examples/go-wasm-http");
    let path = example_dir.join("manifest.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the real example manifest at {path:?}: {e}"));
    let options = serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
        with_snippet: false,
    };
    serde_saphyr::from_str_with_options(&text, options).unwrap_or_else(|e| {
        panic!("the real example manifest fails the WorkerManifest schema: {e}")
    })
}

/// The fixture worker dir: the REAL Go artifact copied as `index.wasm`
/// (the production loader reads the raw `.wasm` bytes — a product, not
/// WAT).
fn go_worker_dir(fixture: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(fixture, dir.path().join("index.wasm"))
        .expect("copies the real Go artifact as the worker entrypoint");
    dir
}

/// The fixture overrides of the parsed manifest: the `SQLD_URL` env value
/// (the non-secret service base the Go module validates — the REAL user
/// origin here, never the admin port), the 60 s guest budget and the 4 mb
/// body cap.
fn fixture_manifest(base: WorkerManifest, sqld_url: &str) -> WorkerManifest {
    let mut manifest = base;
    manifest
        .env
        .as_mut()
        .expect("the real manifest declares the env map")
        .insert("SQLD_URL".to_string(), sqld_url.to_string());
    manifest.timeout = Some("60s".to_string());
    manifest.max_body_size = Some("4mb".to_string());
    manifest
}

/// The host-trusted `WorkerRef` the factory consumes: the identity fields
/// the production loader derives from the real manifest (name, the
/// declared version, the RBAC namespace) plus the same per-call
/// `WorkerConfig` (the worker dir set at load time).
fn fixture_worker_ref(manifest: &WorkerManifest, dir: &Path, namespace: Option<&str>) -> WorkerRef {
    let mut config = parse_worker_config(manifest);
    config.worker_dir = Some(dir.to_path_buf());
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: manifest.name.clone(),
        version: manifest
            .version
            .clone()
            .expect("the real manifest declares a version"),
        dir: dir.to_path_buf(),
        namespace: namespace.map(str::to_owned),
        kind: config.kind.clone().unwrap_or(ExecutionKind::FetchHandler),
        config,
    }
}

/// The single fixture key pair, generated OFFLINE by the test (rcgen +
/// ring, in-process): the public PEM is the ONE global file the server
/// reads; the private PEM stays in memory (never logged, never written,
/// never printed — it only signs the scoped tokens).
struct FixtureKeys {
    pub_pem: String,
    priv_pem: Zeroizing<String>,
}

fn generate_fixture_keys() -> FixtureKeys {
    let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
        .expect("ring generates an Ed25519 key pair offline");
    FixtureKeys {
        pub_pem: pair.public_key_pem(),
        priv_pem: Zeroizing::new(pair.serialize_pem()),
    }
}

fn ed_encoding_key(keys: &FixtureKeys) -> EncodingKey {
    EncodingKey::from_ed_pem(keys.priv_pem.as_bytes())
        .expect("the test private key is a valid Ed25519 PEM")
}

/// Signs the EXACT fixture claims: `exp` (finite, UTC + 600 s) plus the
/// `p` scope with EXACTLY `rw` + `ddl`, each EXACTLY the single NS — no
/// legacy `id`/`a`, no unscoped form, no tags, no aud/iss.
fn sign_scoped_token(keys: &FixtureKeys, ns: &str, exp: i64) -> String {
    let claims = json!({
        "exp": exp,
        "p": {
            "rw": { "ns": [ns] },
            "ddl": { "ns": [ns] }
        }
    });
    sign_raw_claims(keys, &claims)
}

/// Signs arbitrary claims with the fixture key (the negatives of the
/// single-NS invariant: unscoped, legacy, two-NS, tagged).
fn sign_raw_claims(keys: &FixtureKeys, claims: &Value) -> String {
    encode(
        &Header::new(Algorithm::EdDSA),
        claims,
        &ed_encoding_key(keys),
    )
    .expect("EdDSA signing succeeds offline")
}

/// Verifies a JWT against the fixture public key: the default
/// `Validation` of `EdDSA` REQUIRES `exp` (the fixture tokens always
/// carry it). The fixture supplies ONLY a finite `exp` plus the scoped
/// `p`: the absence of `iat`/`iss`/`aud`/`sub` in these tokens does NOT
/// establish that the server never validates them — any claim about the
/// pinned SQLD's validation of other claims is outside this test.
fn decode_eddsa_token(token: &str, pub_pem: &str) -> Result<Value, jsonwebtoken::errors::Error> {
    let decoding = DecodingKey::from_ed_pem(pub_pem.as_bytes())?;
    let validation = Validation::new(Algorithm::EdDSA);
    Ok(decode::<Value>(token, &decoding, &validation)?.claims)
}

/// One string field of the JWT header (base64url segment 0).
fn jwt_header_field(token: &str, field: &str) -> String {
    let (head, _) = token.split_once('.').expect("a JWT has a header segment");
    let bytes =
        base64::engine::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, head)
            .expect("the JWT header is base64url");
    let value: Value = serde_json::from_slice(&bytes).expect("the JWT header is JSON");
    value
        .get(field)
        .and_then(Value::as_str)
        .expect("the header field is a string")
        .to_owned()
}

/// Flips ONE interior character of the signature (the full 6 bits of an
/// interior base64url char decode; only the final char of a 64-byte
/// Ed25519 signature is truncated, so the tamper always changes a byte).
fn tamper_signature(token: &str) -> String {
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(segments.len(), 3, "a JWT has exactly three segments");
    let mut signature: Vec<u8> = segments[2].as_bytes().to_vec();
    let index = 10;
    signature[index] = if signature[index] == b'A' { b'B' } else { b'A' };
    let signature = String::from_utf8(signature).expect("the signature stays ascii");
    format!("{}.{}.{}", segments[0], segments[1], signature)
}

/// The exact fixture claims shape (the pinned SQLD `p` contract): the
/// top level is EXACTLY `exp` + `p`; `p` carries EXACTLY the `rw` and
/// `ddl` scopes; each scope carries EXACTLY the single NS and NO tags.
fn assert_fixture_claims(claims: &Value, ns: &str, exp: i64) {
    let top = claims.as_object().expect("the claims are a JSON object");
    let mut keys: Vec<&str> = top.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["exp", "p"],
        "the fixture claims carry exactly exp + p (no legacy id/a, no aud/iss/sub/tags)"
    );
    assert_eq!(
        claims.get("exp").and_then(Value::as_i64),
        Some(exp),
        "the exp is the exact finite value"
    );
    assert!(
        exp > chrono::Utc::now().timestamp(),
        "the exp is in the future"
    );
    let p = claims
        .get("p")
        .and_then(Value::as_object)
        .expect("p is an object");
    let mut p_keys: Vec<&str> = p.keys().map(String::as_str).collect();
    p_keys.sort_unstable();
    assert_eq!(
        p_keys,
        ["ddl", "rw"],
        "p carries exactly the rw + ddl scopes (no ro/rwa/roa)"
    );
    for scope in ["rw", "ddl"] {
        let scope_value = p.get(scope).expect("the scope is present");
        let mut scope_keys: Vec<&str> = scope_value
            .as_object()
            .expect("the scope is an object")
            .keys()
            .map(String::as_str)
            .collect();
        scope_keys.sort_unstable();
        assert_eq!(scope_keys, ["ns"], "the scope carries exactly ns (no tags)");
        let ns_list = scope_value
            .get("ns")
            .and_then(Value::as_array)
            .expect("the scope ns is a list");
        assert_eq!(
            ns_list.len(),
            1,
            "exactly ONE namespace (the single-NS rule)"
        );
        assert_eq!(
            ns_list.first().and_then(Value::as_str),
            Some(ns),
            "the single namespace is exact"
        );
    }
}

/// The fixture acceptance predicate: true ONLY for the exact single-NS
/// scoped shape (the invariant the tokens must keep). The unscoped
/// `{}`, the legacy `id`/`a` form, a second namespace, tags, another
/// scope or the wrong NS all return false.
fn is_fixture_scoped(claims: &Value, ns: &str) -> bool {
    let Some(top) = claims.as_object() else {
        return false;
    };
    if top.get("id").is_some() || top.get("a").is_some() || top.get("tags").is_some() {
        return false;
    }
    let Some(exp) = top.get("exp").and_then(Value::as_i64) else {
        return false;
    };
    if exp <= 0 {
        return false;
    }
    let Some(p) = top.get("p").and_then(Value::as_object) else {
        return false;
    };
    let mut keys: Vec<&str> = p.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["ddl", "rw"] {
        return false;
    }
    for scope in ["rw", "ddl"] {
        let Some(scope_value) = p.get(scope).and_then(Value::as_object) else {
            return false;
        };
        let Some(ns_list) = scope_value.get("ns").and_then(Value::as_array) else {
            return false;
        };
        if ns_list.len() != 1 || ns_list[0].as_str() != Some(ns) {
            return false;
        }
        if scope_value.keys().any(|key| key.as_str() != "ns") {
            return false;
        }
    }
    true
}

/// The `x-namespace-bin` value: base64 STANDARD_NO_PAD of the namespace
/// bytes — the codec the pinned SQLD
/// `try_namespace_from_metadata` decodes exactly.
fn no_pad(ns: &str) -> String {
    base64::engine::Engine::encode(
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        ns.as_bytes(),
    )
}

/// The fixed pipeline request body (execute + close, like the Go
/// module): the SQL is the argument of the raw probes, never the
/// visitor's.
fn pipeline_body(sql: &str) -> String {
    json!({
        "requests": [
            { "type": "execute", "stmt": { "sql": sql } },
            { "type": "close" }
        ]
    })
    .to_string()
}

/// A reqwest client for the probes: localhost with `no_proxy`, NO
/// redirects, explicit timeouts (the admin client is the 3 s bound, the
/// user client the 10 s read bound).
fn probe_client(read_timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(ADMIN_HTTP)
        .read_timeout(read_timeout)
        .build()
        .expect("the probe client builds")
}

/// One admin probe (literal `Basic <key>`, never base64, never Bearer).
/// `None` = the transport is not ready yet (connection refused while the
/// child boots); the strict wrapper panics on it.
async fn admin_probe_result(
    client: &reqwest::Client,
    admin_base: &str,
    method: &str,
    path: &str,
    key: &str,
    json_body: Option<&str>,
) -> Option<(u16, Value)> {
    let url = format!("{admin_base}{path}");
    let mut req = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        other => panic!("unexpected admin method {other}"),
    };
    req = req.header("authorization", format!("Basic {key}"));
    if let Some(body) = json_body {
        req = req
            .header("content-type", "application/json")
            .body(body.to_owned());
    }
    let resp = req.send().await.ok()?;
    let status = resp.status().as_u16();
    let text = resp.text().await.ok()?;
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    Some((status, value))
}

/// The strict admin probe (post-ready): a transport error is a panic.
async fn admin_probe(
    client: &reqwest::Client,
    admin_base: &str,
    method: &str,
    path: &str,
    key: &str,
    json_body: Option<&str>,
) -> (u16, Value) {
    admin_probe_result(client, admin_base, method, path, key, json_body)
        .await
        .unwrap_or_else(|| panic!("the admin probe request failed (post-ready): {method} {path}"))
}

/// One RAW `POST /v2/pipeline` probe to the REAL server (no guest, no
/// grant): the `ns`/`token` arguments mirror what the host bridge
/// imposes (the NO_PAD header and the fresh `Bearer` credential). `None`
/// for the token exercises the missing-auth contract; the namespace
/// header must stay present on that probe: the pinned server resolves
/// the namespace from the metadata header first and otherwise from the
/// first label of `Host`, so a header-less probe of `127.0.0.1:<port>`
/// resolves the unknown namespace `127` and answers the 404 before the
/// auth gate (the pinned `namespace_from_headers` order). A protocol
/// error entry in the results is never accepted as a query success.
async fn pipeline_probe(
    client: &reqwest::Client,
    user_base: &str,
    ns: Option<&str>,
    token: Option<&str>,
    sql: &str,
) -> (u16, Value) {
    let mut req = client
        .post(format!("{user_base}/v2/pipeline"))
        .header("content-type", "application/json")
        .body(pipeline_body(sql));
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(ns) = ns {
        req = req.header("x-namespace-bin", no_pad(ns));
    }
    let resp = req
        .send()
        .await
        .unwrap_or_else(|e| panic!("the user pipeline request failed: {e}"));
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .unwrap_or_else(|e| panic!("the user pipeline body failed: {e}"));
    let value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, value)
}

/// Every result entry must be `ok`: a `not_found`/`error` protocol entry
/// is never a query success (it would hide the very denial the fixture
/// proves).
fn assert_pipeline_ok(value: &Value, label: &str) {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{label}: the answer has no results array"));
    assert!(
        results.len() >= 2,
        "{label}: the answer needs the execute + close results"
    );
    for (index, result) in results.iter().enumerate() {
        assert_eq!(
            result.get("type").and_then(Value::as_str),
            Some("ok"),
            "{label}: result {index} is not ok (a protocol error is not a query success)"
        );
    }
}

/// The SELECT shape the contract fixes: the first cell of the first row
/// is the exact integer value (the same shape the Go module validates).
fn assert_select_row(value: &Value, expected: &str, label: &str) {
    assert_pipeline_ok(value, label);
    let rows = value
        .pointer("/results/0/response/result/rows")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{label}: the SELECT answer has no rows"));
    let row = rows
        .first()
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{label}: the SELECT answer has an empty row list"));
    let cell = row
        .first()
        .unwrap_or_else(|| panic!("{label}: the SELECT row is empty"));
    assert_eq!(
        cell.get("type").and_then(Value::as_str),
        Some("integer"),
        "{label}: the cell type is integer"
    );
    assert_eq!(
        cell.get("value").and_then(Value::as_str),
        Some(expected),
        "{label}: the cell value"
    );
}

/// Two DISTINCT ephemeral loopback ports for one process. Both probe
/// listeners stay open until both ports are known and only then are released:
/// two calls to a one-port helper could get the same port back from the
/// kernel, because the first probe is already closed when the second binds.
async fn ephemeral_port_pair() -> (u16, u16) {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds the first loopback port probe");
    let second = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds the second loopback port probe");
    (
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port(),
    )
}

/// The REAL `sqld` child of the fixture. `kill_on_drop(true)` is the
/// RAII fallback for panics (the drop kills the process); every normal
/// path calls `shutdown` (explicit kill + wait). stdout/stderr go to
/// files under the tempdir — never the test output — so no JWT can leak
/// into a log. The child env is CLEARED: `PATH=/usr/bin:/bin`, `HOME`
/// under the tempdir, `RUST_LOG=error`; it never inherits
/// `SQLD_PRIMARY_GRPC_URL`, AWS or any production env, and no
/// `--primary-grpc-url` argument is passed (Primary ⇒ lazy creation
/// false at `NamespaceStore::new` in the pinned server).
struct SqldRuntime {
    child: Option<tokio::process::Child>,
    user_port: u16,
    admin_port: u16,
}

impl SqldRuntime {
    async fn spawn(bin: &str, temp: &Path, pub_pem: &str, user_port: u16, admin_port: u16) -> Self {
        // The ONE global public key file the server reads.
        let pub_path = temp.join(PUB_KEY_FILE);
        std::fs::write(&pub_path, pub_pem.as_bytes())
            .expect("writes the single global public key file");
        let stdout = std::fs::File::create(temp.join("sqld-stdout.log"))
            .expect("the child stdout file opens under the tempdir");
        let stderr = std::fs::File::create(temp.join("sqld-stderr.log"))
            .expect("the child stderr file opens under the tempdir");
        let child = Command::new(bin)
            .arg("--db-path")
            .arg(temp.join("data.sqld"))
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{user_port}"))
            .arg("--admin-listen-addr")
            .arg(format!("127.0.0.1:{admin_port}"))
            .arg("--admin-auth-key")
            .arg(ADMIN_KEY)
            .arg("--enable-namespaces")
            .arg("--disable-default-namespace")
            .arg("--auth-jwt-key-file")
            .arg(&pub_path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", temp)
            .env("RUST_LOG", "error")
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|e| panic!("cannot spawn the real sqld {bin:?}: {e}"));
        Self {
            child: Some(child),
            user_port,
            admin_port,
        }
    }

    /// Bounded startup (30 s total, the admin HTTP of each probe 3 s):
    /// the admin gate answers the unknown-namespace config GET with 404
    /// under the CORRECT key; the child exit is CHECKED at every round
    /// (an early exit is a failure, never a sleep). Any other status
    /// before the 404 is a contract break (e.g. 401 with the correct key).
    async fn wait_until_ready(&mut self, client: &reqwest::Client) {
        let admin_base = format!("http://127.0.0.1:{}", self.admin_port);
        let deadline = tokio::time::Instant::now() + STARTUP_DEADLINE;
        loop {
            if let Some(child) = self.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => panic!(
                        "the sqld child exited during startup: {status:?} (logs under the fixture tempdir)"
                    ),
                    Ok(None) => {}
                    Err(e) => panic!("cannot poll the sqld child exit status: {e}"),
                }
            }
            match admin_probe_result(
                client,
                &admin_base,
                "GET",
                &format!("/v1/namespaces/{PROBE_NS}/config"),
                ADMIN_KEY,
                None,
            )
            .await
            {
                Some((404, _)) => return,
                Some((status, _)) => {
                    panic!("the admin gate answered {status} before ready (expected the unknown-namespace 404)")
                }
                None => {
                    if tokio::time::Instant::now() >= deadline {
                        panic!("the sqld child did not answer the admin gate within {STARTUP_DEADLINE:?}");
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    }

    /// Explicit kill + wait (every normal path ends here; the panic
    /// path is covered by `kill_on_drop`).
    async fn shutdown(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let status = child.wait().await.expect("the owned sqld child is reaped");
            eprintln!("go-sqld: the sqld child stopped: {status:?}");
        }
    }
}

/// (EDG-52 09SQLDPrep) The OFFLINE crypto/JSON proof of the fixture
/// tokens: everything runs in-process (rcgen + ring + jsonwebtoken),
/// with no child, no WSL, no Go and no new binary. The key pair is
/// generated by the test; the public PEM is written to the single
/// global file the server will read and read back; the private PEM stays
/// in memory and nothing secret is logged. Both tokens verify under EdDSA
/// with the exact claims shape (`p.rw.ns`/`p.ddl.ns` exactly the single
/// NS, finite exp, no legacy `id`/`a`, no unscoped form, no tags, no
/// aud/iss); the negatives (another key pair, a tampered signature, an
/// expired exp, the unscoped/legacy/two-NS/tagged forms) are all
/// refused; and the `x-namespace-bin` codec round-trips under the pinned
/// NO_PAD profile while the padded profile is refused.
#[test]
fn fixture_tokens_use_scoped_eddsa_claims() {
    let keys = generate_fixture_keys();

    // The single global public key file (the server reads exactly this).
    let dir = tempfile::tempdir().expect("the crypto tempdir opens");
    let pub_path = dir.path().join(PUB_KEY_FILE);
    std::fs::write(&pub_path, keys.pub_pem.as_bytes()).expect("writes the global public key file");
    let read_back = std::fs::read_to_string(&pub_path).expect("reads the public key file back");
    assert_eq!(
        read_back, keys.pub_pem,
        "the global file is exactly the public PEM"
    );

    let exp = chrono::Utc::now().timestamp() + TOKEN_TTL; // UTC + 600 s, finite
    let token_a = sign_scoped_token(&keys, NS_A, exp);
    let token_b = sign_scoped_token(&keys, NS_B, exp);
    assert_ne!(token_a, token_b, "the A and B tokens differ");

    // The header is exactly EdDSA (verified by reading the signed bytes,
    // not by trusting the encoder).
    for token in [&token_a, &token_b] {
        assert_eq!(
            jwt_header_field(token, "alg"),
            "EdDSA",
            "the signed header names EdDSA"
        );
    }

    // Decode + verify under EdDSA (the control: both verify), then the
    // EXACT claims shape per namespace.
    let claims_a = decode_eddsa_token(&token_a, &keys.pub_pem)
        .expect("token A verifies under EdDSA with the fixture public key");
    let claims_b = decode_eddsa_token(&token_b, &keys.pub_pem)
        .expect("token B verifies under EdDSA with the fixture public key");
    assert_fixture_claims(&claims_a, NS_A, exp);
    assert_fixture_claims(&claims_b, NS_B, exp);
    assert!(
        is_fixture_scoped(&claims_a, NS_A),
        "control: token A is the exact scoped shape"
    );
    assert!(
        is_fixture_scoped(&claims_b, NS_B),
        "control: token B is the exact scoped shape"
    );

    // Negative: token A does NOT verify under ANOTHER key pair (EdDSA
    // binds the signer).
    let other = generate_fixture_keys();
    assert!(
        decode_eddsa_token(&token_a, &other.pub_pem).is_err(),
        "token A is refused by another key pair"
    );

    // Negative: a tampered signature is refused.
    assert!(
        decode_eddsa_token(&tamper_signature(&token_a), &keys.pub_pem).is_err(),
        "a tampered signature is refused"
    );

    // Negative: an expired exp (past the 60 s leeway) is rejected, not
    // accepted — the fixture exp is finite and enforced.
    let expired = sign_scoped_token(&keys, NS_A, chrono::Utc::now().timestamp() - 120);
    assert!(
        matches!(
            decode_eddsa_token(&expired, &keys.pub_pem),
            Err(e) if e.kind() == &jsonwebtoken::errors::ErrorKind::ExpiredSignature
        ),
        "an expired token is rejected"
    );

    // The single-NS invariant against its drifts (every one signed with
    // the SAME key, so only the claims shape differs): the unscoped
    // `{}` is the legacy full-access form — it verifies cryptographically
    // but must NEVER pass the fixture acceptance.
    let unscoped = sign_raw_claims(&keys, &json!({ "exp": exp }));
    let decoded_unscoped =
        decode_eddsa_token(&unscoped, &keys.pub_pem).expect("the unscoped token verifies");
    assert!(
        !is_fixture_scoped(&decoded_unscoped, NS_A),
        "an unscoped (legacy/global) token is refused by the fixture acceptance"
    );
    let legacy = sign_raw_claims(&keys, &json!({ "exp": exp, "id": NS_A, "a": "ro" }));
    let decoded_legacy =
        decode_eddsa_token(&legacy, &keys.pub_pem).expect("the legacy token verifies");
    assert!(
        !is_fixture_scoped(&decoded_legacy, NS_A),
        "the legacy id/a form is refused by the fixture acceptance"
    );
    let two_ns = sign_raw_claims(
        &keys,
        &json!({ "exp": exp, "p": { "rw": { "ns": [NS_A, NS_B] }, "ddl": { "ns": [NS_A] } } }),
    );
    assert!(
        !is_fixture_scoped(
            &decode_eddsa_token(&two_ns, &keys.pub_pem).expect("verifies"),
            NS_A
        ),
        "a second namespace in rw is refused"
    );
    let tagged = sign_raw_claims(
        &keys,
        &json!({ "exp": exp, "p": { "rw": { "ns": [NS_A], "tags": ["x"] }, "ddl": { "ns": [NS_A] } } }),
    );
    assert!(
        !is_fixture_scoped(
            &decode_eddsa_token(&tagged, &keys.pub_pem).expect("verifies"),
            NS_A
        ),
        "tags in the scope are refused"
    );
    assert!(
        !is_fixture_scoped(&claims_a, NS_B),
        "token A is not scoped to B (the wrong NS is refused)"
    );

    // The `x-namespace-bin` codec the pinned SQLD decodes: STANDARD_NO_PAD
    // round-trips both namespaces, and the padded STANDARD profile is
    // refused by that decoder (the namespaces are 4 bytes — not a
    // multiple of 3 — so the two profiles really differ).
    let no_pad_profile = base64::engine::general_purpose::STANDARD_NO_PAD;
    let standard_profile = base64::engine::general_purpose::STANDARD;
    for ns in [NS_A, NS_B] {
        let header = Engine::encode(&no_pad_profile, ns.as_bytes());
        assert_eq!(
            no_pad_profile.decode(header.as_bytes()),
            Ok(ns.as_bytes().to_vec()),
            "the pinned NO_PAD decoder accepts the header for {ns:?}"
        );
        let padded = Engine::encode(&standard_profile, ns.as_bytes());
        assert_ne!(padded, header, "for a 4-byte namespace the profiles differ");
        assert!(
            no_pad_profile.decode(padded.as_bytes()).is_err(),
            "the pinned NO_PAD decoder refuses the padded profile for {ns:?}"
        );
    }
}

/// (EDG-52 09SQLDPrep) The OFFLINE proof of the privacy scan helpers
/// the real fixture uses (the R18 gap): a nested event field carrying a
/// synthetic canary and a multi-line key, an object KEY carrying the
/// canary (keys are scanned), a plain child-log body and a
/// JSON-escaped child-log body (the key's newlines as the two
/// characters `\` `n`, hand-built so the case is independent of the
/// escaper) are all REFUSED; clean payloads are ACCEPTED; the header
/// constant is pinned against the real offline generator; and every
/// error label is STATIC — none carries the canary or the key.
/// In-process only: no SQLD, no Deno, no Go, no network, no new
/// process.
#[test]
fn privacy_scan_helpers_refuse_secrets_and_keep_labels_static() {
    const CANARY: &str = "canary-secret-sqld-privacy-test";
    const MULTI_LINE_KEY: &str =
        "-----BEGIN PRIVATE KEY-----\nsynthetic-priv\n-----END PRIVATE KEY-----\n";
    const MULTI_LINE_KEY_ESCAPED: &str =
        "-----BEGIN PRIVATE KEY-----\\nsynthetic-priv\\n-----END PRIVATE KEY-----\\n";
    let secrets = [CANARY, MULTI_LINE_KEY];

    // The header constant is pinned against the real offline generator
    // (the same rcgen + ring key generation the crypto test uses): the
    // real fixture's secret list relies on it.
    let keys = generate_fixture_keys();
    assert!(
        keys.priv_pem.starts_with(PRIV_PEM_HEADER),
        "the generated private PEM carries the pinned header"
    );

    // 1. A nested string VALUE (two levels deep) carrying BOTH the
    // canary and the multi-line key: refused.
    let nested_event = json!({
        "kind": "worker.egress",
        "context": {
            "detail": { "text": format!("prefix {CANARY} then {MULTI_LINE_KEY}") }
        }
    });
    assert!(
        event_privacy_error(&nested_event, &secrets).is_some(),
        "the nested event field with the canary and the multi-line key is refused"
    );
    assert!(
        event_privacy_error(
            &json!({ "context": [{ "text": MULTI_LINE_KEY }] }),
            &[MULTI_LINE_KEY],
        )
        .is_some(),
        "a multi-line key alone in a nested event is refused"
    );

    // 2. An object KEY carrying the canary (keys are scanned): refused.
    let mut keyed = serde_json::Map::new();
    keyed.insert("kind".to_string(), json!("worker.egress"));
    keyed.insert(CANARY.to_string(), json!("a value without a secret"));
    let keyed_event = Value::Object(keyed);
    assert!(
        event_privacy_error(&keyed_event, &secrets).is_some(),
        "the object key carrying the canary is refused"
    );

    // 3. A plain child-log body (real newlines): refused.
    let plain_log = format!("boot line\n{CANARY}\n{MULTI_LINE_KEY}tail").into_bytes();
    assert!(
        log_privacy_error(&plain_log, &secrets).is_some(),
        "the plain log with the canary and the multi-line key is refused"
    );

    // 4. A JSON-escaped child-log body (the key's newlines as the two
    // characters `\` `n`, hand-built): refused — the plain form of the
    // key is NOT a substring of it, so only the escaped-form check
    // catches it (newline escaping cannot bypass).
    let escaped_log = format!("boot\\n{MULTI_LINE_KEY_ESCAPED}tail").into_bytes();
    assert!(
        log_privacy_error(&escaped_log, &secrets).is_some(),
        "the JSON-escaped log is refused (newline escaping cannot bypass)"
    );

    // 5. Clean payloads are accepted.
    let clean_event = json!({
        "kind": "worker.egress",
        "namespace": RBAC_A,
        "message": "host=127.0.0.1 port=43127 bytes=16 redirects=0",
        "ids": ["sqld-req-a-1"]
    });
    assert!(
        event_privacy_error(&clean_event, &secrets).is_none(),
        "a clean event is accepted"
    );
    let clean_log = b"listening on 127.0.0.1:0\nready in 12 ms\n".to_vec();
    assert!(
        log_privacy_error(&clean_log, &secrets).is_none(),
        "a clean log is accepted"
    );

    // 6. Every error label is static: none carries the canary or the
    // multi-line key.
    for label in [
        event_privacy_error(&nested_event, &secrets),
        event_privacy_error(&keyed_event, &secrets),
        log_privacy_error(&plain_log, &secrets),
        log_privacy_error(&escaped_log, &secrets),
    ] {
        let label = label.expect("the hostile case is refused (asserted above)");
        assert!(
            !label.contains(CANARY),
            "the label never carries the canary"
        );
        assert!(
            !label.contains(MULTI_LINE_KEY),
            "the label never carries the multi-line key"
        );
    }
}

/// (EDG-52 09SQLDPrep) The OFFLINE proof of the REAL store's multiple
/// pages and retention behind the SAME privacy scan the real fixture
/// runs (the R19 gap): two real `OperationalStore`s at capacity 1024
/// (NEVER the 200 per-identity default, which would evict the oldest
/// data before the proof). Store A holds 401 CLEAN events of ONE
/// identity: `stats.size` 401 / `evicted` 0, the first page 200 with a
/// `next_cursor` (the data spans 3 pages of 200), and the SAME
/// `scan_store_privacy` returns `Ok(401)` — every page, not just the
/// first. Store B's OLDEST event carries the short synthetic canary
/// (retained verbatim by the 500-char message cap), followed by 400
/// CLEAN events: no eviction, the first page is clean with a
/// `next_cursor`, and the scan REFUSES the old canary on page 3 with
/// the static label. In-process only: no SQLD, no Deno, no Go, no
/// network, no new process. A failure never prints an event or the
/// canary.
#[test]
fn real_store_pages_retention_and_privacy_scan() {
    const CANARY: &str = "sqld-oldest-canary-boundary";
    let capacity = OperationalStoreConfig {
        global_capacity: 1024,
        per_identity_capacity: 1024,
    };

    // Store A: 401 CLEAN events, ONE identity (namespace/worker/version
    // all equal — the per-identity capacity of 1024 is the eviction
    // path under test; the 200 default would evict at the 201st).
    // 401 > 2*200, so the data spans 3 pages of 200.
    let store_a = OperationalStore::new(capacity);
    for index in 1..=401u32 {
        store_a
            .record(store_probe_input(format!("clean probe event {index}")))
            .expect("the in-process store records (no mutex poisoning)");
    }
    let first_a = store_a.query(OperationalEventQuery {
        limit: Some(200),
        ..Default::default()
    });
    assert_eq!(
        first_a.stats.size, 401,
        "all 401 events are retained (no eviction)"
    );
    assert_eq!(
        first_a.stats.evicted, 0,
        "nothing is evicted at capacity 1024"
    );
    assert_eq!(
        first_a.events.len(),
        200,
        "the first page holds 200 events (the query's clamped limit)"
    );
    assert!(
        first_a.next_cursor.is_some(),
        "a next cursor exists (the 401 events span more than one page)"
    );
    assert_eq!(
        scan_store_privacy(&store_a, &[CANARY]),
        Ok(401),
        "the scan covers all 3 pages (401 events), not just the first"
    );

    // Store B: the OLDEST event's message is EXACTLY the short canary
    // (under the 500-char cap, no redaction needle — retained
    // verbatim), followed by 400 CLEAN events. The canary is id 1 and
    // sits on page 3 (pages are the 200 newest first: 401..=202,
    // 201..=2, 1).
    let store_b = OperationalStore::new(capacity);
    assert_eq!(
        store_b.record(store_probe_input(CANARY.to_string())),
        Some(1),
        "the canary is the first (oldest) event"
    );
    for index in 1..=400u32 {
        store_b
            .record(store_probe_input(format!("clean probe event {index}")))
            .expect("the in-process store records (no mutex poisoning)");
    }
    let first_b = store_b.query(OperationalEventQuery {
        limit: Some(200),
        ..Default::default()
    });
    assert_eq!(
        first_b.stats.size, 401,
        "all 401 events are retained (no eviction)"
    );
    assert_eq!(
        first_b.stats.evicted, 0,
        "nothing is evicted at capacity 1024"
    );
    assert_eq!(
        first_b.stats.oldest_id,
        Some(1),
        "the canary (id 1) is still the oldest event"
    );
    assert_eq!(
        first_b.events.len(),
        200,
        "the first page holds 200 events (the query's clamped limit)"
    );
    assert!(
        first_b.next_cursor.is_some(),
        "a next cursor exists (the 401 events span more than one page)"
    );
    for event in &first_b.events {
        let serialized =
            serde_json::to_value(event).expect("an operational event serializes to JSON");
        assert!(
            event_privacy_error(&serialized, &[CANARY]).is_none(),
            "{EVENT_SECRET_LABEL}"
        );
    }
    // The SAME scan helper must REFUSE the old canary on page 3 (pages
    // 1-2 hold the 400 clean events, asserted above; the canary is id
    // 1): the static label, never the canary, an event or a field.
    match scan_store_privacy(&store_b, &[CANARY]) {
        Ok(_) => panic!("the scan accepted the store that holds the canary (no page-3 refusal)"),
        Err(label) => assert_eq!(
            label, EVENT_SECRET_LABEL,
            "the refusal carries the static label"
        ),
    }
}

/// (EDG-52 09SQLDPrep) The OFFLINE proof of the local-file BOUNDARY of
/// the SAME `child_log_bytes` the real fixture reads with (the R19
/// gap): a `tempfile::TempDir` file of EXACTLY `LOG_READ_MAX` bytes of
/// safe dummy bytes is read in full (exactly `LOG_READ_MAX` bytes; the
/// data is never printed), and a file of `LOG_READ_MAX + 1` bytes
/// makes the SAME helper panic with the static `LOG_BOUND_LABEL`,
/// observed under `std::panic::catch_unwind` — the message is compared
/// to the label and otherwise never printed (no content, path or
/// canary dump; the global panic hook is untouched). This proves the
/// boundary behavior, not a claimed concurrent-growth race: the
/// `Read::take(max + 1)` cap still statically enforces the read.
/// In-process only: no signals, no threads, no new process; the
/// `TempDir` cleans itself up.
#[test]
fn child_log_bytes_enforces_the_read_bound() {
    let dir = tempfile::tempdir().expect("the boundary tempdir opens");

    // A file of EXACTLY the bound: the SAME helper reads it in full
    // (the dummy bytes are never printed).
    let at_bound = dir.path().join("at-bound.log");
    std::fs::write(&at_bound, vec![0xA5u8; LOG_READ_MAX]).expect("writes the at-bound file");
    let bytes = child_log_bytes(&at_bound);
    assert_eq!(
        bytes.len(),
        LOG_READ_MAX,
        "a file of exactly the bound is read in full (the data is not printed)"
    );

    // A file ONE byte past the bound: the SAME helper must fail with
    // the static label. `catch_unwind` observes the panic and the
    // message is checked — never printed (an unexpected `Ok` would
    // dump the large bytes, so the outcome is matched, not
    // `expect_err`-ed); the bytes, the path and the tempdir contents
    // stay inside the `TempDir`, which removes them.
    let past_bound = dir.path().join("past-bound.log");
    std::fs::write(&past_bound, vec![0x5Au8; LOG_READ_MAX + 1])
        .expect("writes the past-bound file");
    match std::panic::catch_unwind(|| {
        let _ = child_log_bytes(&past_bound);
    }) {
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
            assert_eq!(
                message,
                Some(LOG_BOUND_LABEL),
                "the bound violation fails with the static label"
            );
        }
        Ok(_) => panic!("a file past the bound is refused (the read is capped)"),
    }
}

/// (EDG-52 09SQLDPrep) The REAL SQLD fixture: the real `sqld` child
/// (namespaces on, default namespace off, the fixture Ed25519 key file),
/// the real `AppSecretVault` (SQLite, synthetic test master) and the REAL
/// Go artifact through the SAME `assemble_wasm_isolate_with_database`
/// and the STRICT guard — all in ONE fixture. Missing
/// `EDGER_SQLD_BIN`/`EDGER_GO_WASM_FIXTURE`, a missing file, a bad magic,
/// a SHA mismatch or a bad manifest PANIC clearly — never skip, never Ok.
/// IGNORED: it requires the operator-approved binary and artifact and is
/// NOT executed in this slice (not even with `--include-ignored`).
#[tokio::test]
#[ignore = "requires operator-approved real SQLD and built Go artifact"]
async fn real_go_vault_sqld_namespaces_and_denials() {
    // Preconditions (never skip, never Ok).
    let sqld_bin = std::env::var("EDGER_SQLD_BIN").unwrap_or_else(|_| {
        panic!(
            "EDGER_SQLD_BIN must name the real sqld binary (operator runtime: /root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld); this test never skips"
        )
    });
    let fixture = fixture_wasm_path();
    let manifest_base = real_manifest();
    assert_eq!(
        manifest_base.name, APP_A,
        "the fixture uses the real manifest identity"
    );
    assert_eq!(
        manifest_base.entrypoint.as_deref(),
        Some("index.wasm"),
        "the fixture loads the real entrypoint"
    );

    // The fixture tokens (the offline contract re-verified on the very
    // tokens the real server will see).
    let keys = generate_fixture_keys();
    let exp_a = chrono::Utc::now().timestamp() + TOKEN_TTL; // UTC + 600 s
    let token_a = sign_scoped_token(&keys, NS_A, exp_a);
    let token_b = sign_scoped_token(&keys, NS_B, exp_a);
    assert_fixture_claims(
        &decode_eddsa_token(&token_a, &keys.pub_pem).expect("token A verifies"),
        NS_A,
        exp_a,
    );
    assert_fixture_claims(
        &decode_eddsa_token(&token_b, &keys.pub_pem).expect("token B verifies"),
        NS_B,
        exp_a,
    );

    // Two DISTINCT ephemeral loopback ports (never production ports,
    // never the admin 9090 the grant shape refuses).
    let (user_port, admin_port) = ephemeral_port_pair().await;
    assert_ne!(user_port, admin_port, "the two fixture ports are distinct");
    assert_ne!(
        user_port, 9090,
        "the grant shape refuses the known admin port"
    );

    let dir = tempfile::tempdir().expect("the fixture tempdir opens");
    let mut rt =
        SqldRuntime::spawn(&sqld_bin, dir.path(), &keys.pub_pem, user_port, admin_port).await;
    let admin_client = probe_client(ADMIN_HTTP);
    rt.wait_until_ready(&admin_client).await;

    let admin_base = format!("http://127.0.0.1:{}", rt.admin_port);
    let user_base = format!("http://127.0.0.1:{}", rt.user_port);
    let user_client = probe_client(USER_HTTP);

    // Admin — wrong key first (the gate is up: 401, the refusal of the
    // literal Basic contract), then the creates and the no-side-effect
    // 404 of the unknown namespace. The 400 duplicate re-create runs
    // LATER, after the marker data exists (the data-preservation
    // sequence of the fixture).
    let (status, _) = admin_probe(
        &admin_client,
        &admin_base,
        "POST",
        &format!("/v1/namespaces/{NS_A}/create"),
        "canary-admin-wrong-go-sqld",
        Some("{}"),
    )
    .await;
    assert_eq!(status, 401, "the wrong admin key is refused (401)");

    for ns in [NS_A, NS_B] {
        let (status, _) = admin_probe(
            &admin_client,
            &admin_base,
            "POST",
            &format!("/v1/namespaces/{ns}/create"),
            ADMIN_KEY,
            Some("{}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "the create of {ns} succeeds (200, no special status)"
        );
    }

    let (status, _) = admin_probe(
        &admin_client,
        &admin_base,
        "GET",
        &format!("/v1/namespaces/{PROBE_NS}/config"),
        ADMIN_KEY,
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "the unknown namespace config is 404 (no side effect)"
    );

    // User raw API — the denials first: missing auth is 401 (the probe
    // carries the `x-namespace-bin` of the KNOWN `NS_A` so ONLY the auth
    // is missing: without that header the pinned server takes the
    // namespace from the first label of `Host` — `127` from
    // `127.0.0.1:<port>` — and answers the unknown-namespace 404
    // BEFORE the auth gate, the correct pinned behavior, not a denial);
    // token A with the header of B is the 404 `NamespaceDoesntExist`
    // BEFORE any Hrana processing (never 403, never a 200 with an
    // in-band error); token B with its own header succeeds (proving B
    // exists).
    let (status, _) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_A),
        None,
        "SELECT 1 AS value",
    )
    .await;
    assert_eq!(status, 401, "the missing user auth is refused (401)");

    let (status, body) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_B),
        Some(token_a.as_str()),
        "SELECT 1 AS value",
    )
    .await;
    assert_eq!(
        status, 404,
        "token A against the B namespace is refused BEFORE Hrana (404, not 403, not a 200 error)"
    );
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("doesn't exist")),
        "the denial surfaces as the pinned NamespaceDoesntExist JSON error"
    );

    let (status, body) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_B),
        Some(token_b.as_str()),
        "SELECT 1 AS value",
    )
    .await;
    assert_eq!(
        status, 200,
        "token B against its own namespace succeeds (B exists)"
    );
    assert_select_row(&body, "1", "the B SELECT 1");

    // The raw user API creates the marker table and inserts a DIFFERENT
    // value per namespace; the SELECTs confirm the isolation.
    for (ns, token, marker) in [(NS_A, &token_a, MARKER_A), (NS_B, &token_b, MARKER_B)] {
        let (status, body) = pipeline_probe(
            &user_client,
            &user_base,
            Some(ns),
            Some(token.as_str()),
            "CREATE TABLE marker (v INTEGER)",
        )
        .await;
        assert_eq!(status, 200, "the marker table is created in {ns}");
        assert_pipeline_ok(&body, &format!("the marker create in {ns}"));
        let (status, body) = pipeline_probe(
            &user_client,
            &user_base,
            Some(ns),
            Some(token.as_str()),
            &format!("INSERT INTO marker VALUES ({marker})"),
        )
        .await;
        assert_eq!(status, 200, "the marker value is inserted in {ns}");
        assert_pipeline_ok(&body, &format!("the marker insert in {ns}"));
        let (status, body) = pipeline_probe(
            &user_client,
            &user_base,
            Some(ns),
            Some(token.as_str()),
            "SELECT v FROM marker",
        )
        .await;
        assert_eq!(status, 200, "the marker is selected in {ns}");
        assert_select_row(&body, marker, &format!("the marker isolation in {ns}"));
    }

    // The duplicate-create probe AFTER both namespaces hold their marker
    // (the data-preservation sequence): before this repeat, A's data
    // already exists (the inserts and the verified SELECTs above). A
    // destructive re-create that cleared the namespace while answering
    // the expected 400 would be caught by the raw read immediately below.
    let (status, body) = admin_probe(
        &admin_client,
        &admin_base,
        "POST",
        &format!("/v1/namespaces/{NS_A}/create"),
        ADMIN_KEY,
        Some("{}"),
    )
    .await;
    assert_eq!(
        status, 400,
        "the re-create of the existing namespace is refused (400)"
    );
    assert!(
        body.get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("already exists")),
        "the 400 carries the JSON error string"
    );
    // Immediately after the 400 + error text: the raw authenticated read
    // of A's marker still answers 200 with the exact value.
    let (status, body) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_A),
        Some(token_a.as_str()),
        "SELECT v FROM marker",
    )
    .await;
    assert_eq!(
        status, 200,
        "A's marker read right after the duplicate-create 400 succeeds"
    );
    assert_select_row(
        &body,
        MARKER_A,
        "A's marker immediately after the duplicate-create 400 (data preserved)",
    );

    // Go REAL: the artifact from the env-named dir, the real manifest
    // (parsed BEFORE the overrides), the real vault, the SAME assembly
    // and the STRICT guard.
    let vault = std::sync::Arc::new(
        AppSecretVault::open(
            dir.path().join("app-secrets.db"),
            Zeroizing::new(VAULT_MASTER),
        )
        .expect("the real vault opens in the tempdir"),
    );
    vault
        .set(APP_A, CREDENTIAL, &token_a)
        .expect("stores JWT A privately");
    vault
        .set(APP_B, CREDENTIAL, &token_b)
        .expect("stores JWT B privately");

    let store = OperationalStore::default();
    let guard = strict_guard(&store);

    // App A: the real manifest name, the RBAC namespace DISTINCT from
    // the SQLD namespace, the grant of NS_A.
    let manifest_a = fixture_manifest(real_manifest(), &user_base);
    let worker_a = go_worker_dir(&fixture);
    let ref_a = fixture_worker_ref(&manifest_a, worker_a.path(), Some(RBAC_A));
    assert!(
        ref_a.config.egress.is_none(),
        "no general egress grant (the manifest declares none)"
    );
    assert_ne!(
        RBAC_A, NS_A,
        "the RBAC namespace is distinct from the SQLD namespace"
    );
    let owner_a = EgressWorkerContext {
        worker: APP_A.to_string(),
        version: ref_a.version.clone(),
        namespace: ref_a.namespace.clone(),
        process_id: String::new(),
    };
    let grant_a = std::sync::Arc::new(
        EgressDatabaseGrant::new(
            &owner_a,
            &format!("{user_base}/"),
            NS_A,
            CREDENTIAL,
            Some(vault.clone()),
        )
        .expect("valid grant A"),
    );
    let mut isolate_a = assemble_wasm_isolate_with_database(
        &ref_a,
        Some(vault.clone()),
        Some(std::sync::Arc::clone(&guard)),
        Some(std::sync::Arc::clone(&grant_a)),
    );

    // A /db — COLD: the real query on the real engine, the exact body.
    let res = within(
        "the cold /db A execution",
        isolate_a.execute_wasm(host_request("sqld-req-a-1"), &ref_a.config),
    )
    .await
    .expect("the private grant + the strict guard admit the import");
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_OK_BODY.as_bytes()),
        "the exact validated integer 1, without any credential"
    );
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("sqld-req-a-1"))
        .expect("one event for the host request id");
    assert_eq!(event.worker.as_deref(), Some(APP_A));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some(RBAC_A));
    assert_eq!(event.process_id, None);
    assert_eq!(event.outcome.as_deref(), Some("ok"));
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);

    // Namespace-auth NEGATIVE control (the future real run): the SAME
    // ref_a/isolate inputs (no public env mutation, no guest or token
    // change — the vault still holds token A scoped ONLY to NS_A), the
    // SAME owner (owner A: APP_A + RBAC_A), the SAME user origin,
    // credential name and vault — but a grant whose SQLD namespace is
    // NS_B. The host owner check PASSES, the credential resolves and the
    // upstream is dialed with token A and the NS_B `x-namespace-bin`
    // header: the pinned server's scoped-JWT gate refuses token A
    // against NS_B with the SAME 404 `NamespaceDoesntExist` the raw user
    // API section above proves for token A + the header of B — a SQLD
    // auth refusal caused solely by the wrong namespace, NOT a host
    // owner denial. The guard records the forwarded upstream answer as
    // an ordinary response (outcome `ok`, status 404, `code` None — the
    // code is set on refusals only), and the Go module accepts only a
    // 2xx upstream answer, so the guest answers the static 502. The
    // marker visibility remains the raw SQLD client's proof: these
    // scoped-JWT positive/negative Go controls connect Go to the
    // namespace auth (the guest runs the fixed `SELECT 1`, never the
    // marker).
    let grant_a_wrong_ns = std::sync::Arc::new(
        EgressDatabaseGrant::new(
            &owner_a,
            &format!("{user_base}/"),
            NS_B,
            CREDENTIAL,
            Some(vault.clone()),
        )
        .expect("valid grant A with the SQLD namespace of B"),
    );
    let mut isolate_a_wrong_ns = assemble_wasm_isolate_with_database(
        &ref_a,
        Some(vault.clone()),
        Some(std::sync::Arc::clone(&guard)),
        Some(std::sync::Arc::clone(&grant_a_wrong_ns)),
    );
    let res = within(
        "the wrong-namespace /db execution",
        isolate_a_wrong_ns.execute_wasm(host_request("sqld-req-a-wrong-ns"), &ref_a.config),
    )
    .await
    .expect("the import is admitted (the owner matches); the refusal is upstream");
    assert_eq!(res.status, 502);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_502_BODY.as_bytes()),
        "the static 502: the guest accepts only a 2xx upstream answer"
    );
    let upstream_404 = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("sqld-req-a-wrong-ns"))
        .expect("one event for the host request id");
    assert_eq!(upstream_404.worker.as_deref(), Some(APP_A));
    assert_eq!(upstream_404.version.as_deref(), Some("1.0.0"));
    assert_eq!(
        upstream_404.namespace.as_deref(),
        Some(RBAC_A),
        "the context of A (the grant's owner), not B"
    );
    assert_eq!(upstream_404.process_id, None);
    assert_eq!(
        upstream_404.outcome.as_deref(),
        Some("ok"),
        "the forwarded upstream answer is an ordinary response, not a refusal"
    );
    assert_eq!(
        upstream_404.status,
        Some(404),
        "the upstream HTTP 404 (the scoped-JWT namespace refusal)"
    );
    assert_eq!(
        upstream_404.code, None,
        "no guard refusal code: NOT the host owner denial (EGRESS_DATABASE_DENIED 403)"
    );
    let upstream_404_port: u16 = message_field(&upstream_404.message, "port")
        .expect("the upstream telemetry carries the destination port")
        .parse()
        .expect("the port telemetry is a plain number");
    assert_eq!(
        upstream_404_port, rt.user_port,
        "the event names the exact SQLD user port dialed"
    );

    // App B: the distinct app name and RBAC namespace, the grant of NS_B.
    let mut manifest_b = fixture_manifest(real_manifest(), &user_base);
    manifest_b.name = APP_B.to_string();
    let worker_b = go_worker_dir(&fixture);
    let ref_b = fixture_worker_ref(&manifest_b, worker_b.path(), Some(RBAC_B));
    let owner_b = EgressWorkerContext {
        worker: APP_B.to_string(),
        version: ref_b.version.clone(),
        namespace: ref_b.namespace.clone(),
        process_id: String::new(),
    };
    let grant_b = std::sync::Arc::new(
        EgressDatabaseGrant::new(
            &owner_b,
            &format!("{user_base}/"),
            NS_B,
            CREDENTIAL,
            Some(vault.clone()),
        )
        .expect("valid grant B"),
    );
    let mut isolate_b = assemble_wasm_isolate_with_database(
        &ref_b,
        Some(vault.clone()),
        Some(std::sync::Arc::clone(&guard)),
        Some(std::sync::Arc::clone(&grant_b)),
    );

    // B /db with grant B — the real query in its own namespace.
    let res = within(
        "the /db B execution",
        isolate_b.execute_wasm(host_request("sqld-req-b-1"), &ref_b.config),
    )
    .await
    .expect("grant B admits the import for app B");
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_OK_BODY.as_bytes()),
        "the exact validated integer 1 for B"
    );
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("sqld-req-b-1"))
        .expect("one event for the host request id");
    assert_eq!(event.worker.as_deref(), Some(APP_B));
    assert_eq!(event.namespace.as_deref(), Some(RBAC_B));
    assert_eq!(event.outcome.as_deref(), Some("ok"));
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);

    // B assembled with grant A (a FOREIGN owner): the observed evidence
    // is the guest static 502 plus the HOST owner denial event
    // (EGRESS_DATABASE_DENIED 403, the context of B, no upstream status
    // of a SQLD answer) — the owner check precedes the dial by the
    // reviewed host control flow (the grant's exact owner/origin check
    // runs before any credential lookup, DNS or dial). This fixture does
    // not independently measure SQLD accepts: the marker read below is
    // data evidence, not a dial counter.
    let worker_c = go_worker_dir(&fixture);
    let ref_c = fixture_worker_ref(&manifest_b, worker_c.path(), Some(RBAC_B));
    let mut isolate_c = assemble_wasm_isolate_with_database(
        &ref_c,
        Some(vault.clone()),
        Some(std::sync::Arc::clone(&guard)),
        Some(std::sync::Arc::clone(&grant_a)),
    );
    let res = within(
        "the cross-owner /db execution",
        isolate_c.execute_wasm(host_request("sqld-req-cross-1"), &ref_c.config),
    )
    .await
    .expect("the guest runs; the refusal is at the bridge, not the import");
    assert_eq!(res.status, 502);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_502_BODY.as_bytes()),
        "the static 502: no upstream body, no URL, no token"
    );
    let denied = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("sqld-req-cross-1"))
        .expect("the bridge refusal records one event");
    assert_eq!(denied.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(denied.status, Some(403));
    assert_eq!(denied.outcome.as_deref(), Some("blocked"));
    assert_eq!(denied.worker.as_deref(), Some(APP_B));
    assert_eq!(
        denied.namespace.as_deref(),
        Some(RBAC_B),
        "the ctx of B, not the grant's owner"
    );
    assert_eq!(denied.process_id, None);
    let denied_port: u16 = message_field(&denied.message, "port")
        .expect("the owner-denial telemetry carries the destination port")
        .parse()
        .expect("the port telemetry is a plain number");
    assert_eq!(
        denied_port, rt.user_port,
        "the denial names the exact destination port"
    );
    let (_, check) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_A),
        Some(token_a.as_str()),
        "SELECT v FROM marker",
    )
    .await;
    assert_select_row(
        &check,
        MARKER_A,
        "the A marker after the cross-owner refusal (data unchanged)",
    );

    // No grant at all (the guard present): the import is refused at the
    // policy with WASM_IMPORT_DENIED (never the guest 502) and ZERO new
    // guard events.
    let worker_d = go_worker_dir(&fixture);
    let ref_d = fixture_worker_ref(&manifest_a, worker_d.path(), Some(RBAC_A));
    let mut isolate_d = assemble_wasm_isolate_with_database(
        &ref_d,
        Some(vault.clone()),
        Some(std::sync::Arc::clone(&guard)),
        None,
    );
    let events_before = egress_events(&store).len();
    let err = within(
        "the no-grant /db execution",
        isolate_d.execute_wasm(host_request("sqld-req-nogrant-1"), &ref_d.config),
    )
    .await
    .expect_err("without any grant the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
    let events_after = egress_events(&store);
    assert_eq!(events_after.len(), events_before, "zero new guard events");
    assert!(
        events_after
            .iter()
            .all(|e| e.request_id.as_deref() != Some("sqld-req-nogrant-1")),
        "no event for the pre-fetch denial"
    );

    // Removing the A secret from the vault: the per-execution resolution
    // leaves the credential absent in the guest env, and the Go module
    // answers the static 503 ITSELF before any fetch-import call (the
    // observed evidence: the guest 503 and zero new guard events; the
    // marker read below is data evidence, not a dial counter).
    assert!(
        vault
            .delete(APP_A, CREDENTIAL)
            .expect("the vault deletes the record"),
        "JWT A was present before the removal"
    );
    let events_before = egress_events(&store).len();
    let res = within(
        "the /db A no-secret execution",
        isolate_a.execute_wasm(host_request("sqld-req-a-nosecret"), &ref_a.config),
    )
    .await
    .expect("the execution still succeeds (the static 503 is a guest answer)");
    assert_eq!(res.status, 503);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_503_BODY.as_bytes()),
        "the static 503: no token, no URL, no error text"
    );
    let events_after = egress_events(&store);
    assert_eq!(
        events_after.len(),
        events_before,
        "no egress event before the fetch"
    );
    assert!(
        events_after
            .iter()
            .all(|e| e.request_id.as_deref() != Some("sqld-req-a-nosecret")),
        "no egress event for the pre-fetch refusal"
    );
    let (_, check) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_A),
        Some(token_a.as_str()),
        "SELECT v FROM marker",
    )
    .await;
    assert_select_row(&check, MARKER_A, "the A marker after the secret removal");

    // Restoring a DIFFERENT token (fresh exp, UTC + 601 s) on the SAME
    // isolate/module cache: the actual query succeeds again. This is the
    // query + delete/restore proof under the real composition and the
    // cache control — NOT a byte-level auth-rotation claim (the real
    // server does not revoke; the old token may still validate there).
    let exp_a2 = chrono::Utc::now().timestamp() + TOKEN_TTL + 1; // the DIFFERENT finite exp
    assert_ne!(exp_a2, exp_a, "the restored token carries a different exp");
    let token_a2 = sign_scoped_token(&keys, NS_A, exp_a2);
    assert_fixture_claims(
        &decode_eddsa_token(&token_a2, &keys.pub_pem).expect("token A2 verifies"),
        NS_A,
        exp_a2,
    );
    vault
        .set(APP_A, CREDENTIAL, &token_a2)
        .expect("restores JWT A2");
    let res = within(
        "the /db A restored execution",
        isolate_a.execute_wasm(host_request("sqld-req-a-restored"), &ref_a.config),
    )
    .await
    .expect("the same isolate/module cache serves the restored credential");
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_OK_BODY.as_bytes()),
        "the actual query again, on the SAME isolate/module cache"
    );

    // Privacy through the REAL getters: the WasiConfig Debug, the
    // per-call config and the public env carry no JWT and no admin key.
    let wasi_debug = format!("{:?}", isolate_a.wasi_config());
    let config_debug = format!("{:?}", ref_a.config);
    for secret in [
        token_a.as_str(),
        token_a2.as_str(),
        token_b.as_str(),
        ADMIN_KEY,
    ] {
        assert!(
            !wasi_debug.contains(secret),
            "the WasiConfig Debug carries no secret value"
        );
        assert!(
            !config_debug.contains(secret),
            "the per-call config carries no secret value"
        );
        assert!(
            ref_a
                .config
                .public_env
                .iter()
                .all(|name| name.as_str() != secret),
            "the public env names carry no secret value"
        );
    }

    // The final isolation read (after every execution): A still its own
    // value, B still its own — the data-preservation evidence for the
    // 400 duplicate re-create (a destructive re-create would clear the
    // namespace). The denial proofs are the guest statuses and the guard
    // events, not these reads (a read-only SELECT can leave data
    // unchanged even after a dial).
    let (_, final_a) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_A),
        Some(token_a.as_str()),
        "SELECT v FROM marker",
    )
    .await;
    assert_select_row(&final_a, MARKER_A, "the final A marker read");
    let (_, final_b) = pipeline_probe(
        &user_client,
        &user_base,
        Some(NS_B),
        Some(token_b.as_str()),
        "SELECT v FROM marker",
    )
    .await;
    assert_select_row(&final_b, MARKER_B, "the final B marker read");

    rt.shutdown().await;

    // PRIVACY over the REAL store and the captured child logs (the R18
    // gap: the earlier checks covered only the config/Debug surfaces).
    // Like the rest of this fixture, executed only after operator
    // approval: PREPARED and COMPILED here, UNEXECUTED in this slice.
    let secrets = [
        token_a.as_str(),
        token_a2.as_str(),
        token_b.as_str(),
        ADMIN_KEY,
        keys.priv_pem.as_str(),
        PRIV_PEM_HEADER,
    ];

    // 1. EVERY event the store returns (no kind/request filter, the
    // query paginated to the end; every serialized string field AND
    // object key, so a multi-line PEM cannot hide behind JSON escaping):
    // no fixture secret anywhere. The SAME helper the offline store
    // proofs run (`scan_store_privacy`); on a hit it panics with the
    // static label — a failure never prints a value, an event or a
    // field.
    let event_count = match scan_store_privacy(&store, &secrets) {
        Ok(count) => count,
        Err(label) => panic!("{label}"),
    };
    assert!(
        event_count > 0,
        "the store holds the guard events recorded above (a vacuous scan proves nothing)"
    );

    // 2. The captured child stdout/stderr, read AFTER the child is
    // confirmed stopped (the buffers have drained): no fixture secret in
    // plain or JSON-escaped form. Bounded reads; a bound violation or a
    // hit fails with a static label and never dumps the content.
    for name in ["sqld-stdout.log", "sqld-stderr.log"] {
        let bytes = child_log_bytes(&dir.path().join(name));
        assert!(
            log_privacy_error(&bytes, &secrets).is_none(),
            "{LOG_SECRET_LABEL}"
        );
    }
}

// ---------------------------------------------------------------------
// (EDG-52 pool-real) The REAL Go module through the public HTTP pipeline
// and the real `WorkerPool`.
//
// What this proves: HTTP requests arrive over real TCP (reqwest ->
// `axum::serve` on `127.0.0.1:0`), enter `build_pipeline` (the same
// router the binary mounts), are resolved against a manifest index
// loaded from disk by `load_manifests_from_dirs`, dispatched by the real
// `WorkerPool`, and reach the REAL Go artifact through an
// `IsolateFactory` that calls `assemble_wasm_isolate_with_database`
// with a HOST-created `EgressDatabaseGrant` bound to the exact
// `WorkerRef` identity (name, version, namespace). The SQLD, the vault
// and the guard are the real ones of the fixture above.
//
// What this does NOT prove (gap, EDG-54): `bin/edger.rs`
// (`RuntimeIsolateFactory::create_isolate`) still calls
// `assemble_wasm_isolate` WITHOUT a `database_grant`. The factory below
// is a TEST factory; the host-side provisioning that creates grants for
// the real binary does not exist yet, so nothing here shows the product
// binary can reach SQLD.
// ---------------------------------------------------------------------

/// The namespaced legitimate app: the loader derives the `WorkerRef`
/// name `@nsgo/go-wasm-http` and the namespace `@nsgo` from this manifest
/// name (route `/@nsgo/go-wasm-http/...`). The old fixture constants are
/// untouched.
const APP_NS: &str = "@nsgo/go-wasm-http";
const APP_NS_SCOPE: &str = "@nsgo";

/// The served pipeline: aborted on drop (the failure path), gracefully
/// shut down on the normal path.
struct ServedPipeline {
    base: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ServedPipeline {
    async fn start(app: axum::Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds the ephemeral pipeline port");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await
                .expect("the pipeline server runs");
        });
        Self {
            base,
            shutdown: Some(tx),
            task: Some(task),
        }
    }

    async fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("the pipeline server stops gracefully")
                .expect("the pipeline server task joins");
        }
    }
}

impl Drop for ServedPipeline {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The test `IsolateFactory`: the grant of an app comes ONLY from a
/// host-created map keyed by the trusted `WorkerRef` name — never from
/// a request, a header, a URL or a manifest field.
struct GrantFactory {
    vault: std::sync::Arc<AppSecretVault>,
    guard: std::sync::Arc<EgressGuard>,
    grants: std::collections::HashMap<String, std::sync::Arc<EgressDatabaseGrant>>,
}

impl edger_worker::IsolateFactory for GrantFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        let vault: std::sync::Arc<dyn edger_core::AppSecretResolver> = self.vault.clone();
        Box::new(assemble_wasm_isolate_with_database(
            worker_ref,
            Some(vault),
            Some(std::sync::Arc::clone(&self.guard)),
            self.grants.get(&worker_ref.name).cloned(),
        ))
    }
}

/// The manifest text of one pipeline app: the REAL example manifest with
/// only the name, the SQLD base URL and the 60 s guest budget replaced
/// (each replacement is asserted to apply, so a manifest drift fails).
fn pipeline_manifest_text(name: &str, sqld_url: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/examples/go-wasm-http/manifest.yaml");
    let text = std::fs::read_to_string(&path).expect("reads the real example manifest");
    for needle in [
        "name: go-wasm-http\n",
        "  SQLD_URL: \"\"\n",
        "timeout: 30s\n",
    ] {
        assert!(
            text.contains(needle),
            "the real manifest drifted: {needle:?}"
        );
    }
    text.replace("name: go-wasm-http\n", &format!("name: \"{name}\"\n"))
        .replace(
            "  SQLD_URL: \"\"\n",
            &format!("  SQLD_URL: \"{sqld_url}\"\n"),
        )
        .replace("timeout: 30s\n", "timeout: 60s\n")
}

/// One request over TCP to the served pipeline; returns status, headers
/// and the whole body. The caller decides every header (the public
/// requests carry no `authorization`).
async fn pipeline_call(
    client: &reqwest::Client,
    base: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, reqwest::header::HeaderMap, bytes::Bytes) {
    let method = reqwest::Method::from_bytes(method.as_bytes()).expect("a valid method");
    let mut req = client.request(method, format!("{base}{uri}"));
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let res = within("the pipeline request", req.body(body.to_owned()).send())
        .await
        .unwrap_or_else(|_| panic!("the TCP pipeline request failed"));
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = within("the pipeline body", res.bytes())
        .await
        .unwrap_or_else(|_| panic!("the TCP pipeline body read failed"));
    (status, headers, bytes)
}

/// The egress events of ONE app (the worker name the guard recorded).
fn app_egress_events(store: &OperationalStore, app: &str) -> Vec<OperationalEvent> {
    egress_events(store)
        .into_iter()
        .filter(|e| e.worker.as_deref() == Some(app))
        .collect()
}

fn remember_pipeline_response(seen: &mut Vec<Vec<u8>>, body: &[u8], secrets: &[&str]) {
    // Check before any assertion can render a response into failure output.
    assert!(
        log_privacy_error(body, secrets).is_none(),
        "a pipeline response carries a fixture secret"
    );
    seen.push(body.to_vec());
}

#[tokio::test]
async fn pipeline_transport_failure_does_not_render_the_request_query() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the local fault listener binds");
    let address = listener.local_addr().expect("the listener address");
    let server = tokio::spawn(async move {
        let (connection, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("the fixture accepts promptly")
            .expect("the local accept succeeds");
        drop(connection);
        drop(listener);
    });
    let request = tokio::spawn(async move {
        pipeline_call(
            &probe_client(Duration::from_millis(250)),
            &format!("http://{address}"),
            "GET",
            "/db?token=synthetic-query-privacy-canary",
            &[],
            "",
        )
        .await
    });
    let result = within("the failed TCP request", request).await;
    within("the fault listener shutdown", server)
        .await
        .expect("the listener task finishes");
    let failure = result.expect_err("the connection closes without a response");
    assert!(failure.is_panic(), "the static failure branch is observed");
    let payload = failure.into_panic();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied());
    assert_eq!(message, Some("the TCP pipeline request failed"));
}

#[tokio::test]
#[ignore = "requires operator-approved real SQLD and built Go artifact"]
async fn real_go_through_pipeline_and_pool_with_sqld() {
    const APP_CROSS: &str = "go-wasm-http-cross";
    const APP_WRONG_NS: &str = "go-wasm-http-wrongns";
    const APP_NO_GRANT: &str = "go-wasm-http-nogrant";

    let sqld_bin = std::env::var("EDGER_SQLD_BIN").unwrap_or_else(|_| {
        panic!("EDGER_SQLD_BIN must name the real sqld binary; this test never skips")
    });
    let fixture = fixture_wasm_path();

    let keys = generate_fixture_keys();
    let exp = chrono::Utc::now().timestamp() + TOKEN_TTL;
    let token_a = sign_scoped_token(&keys, NS_A, exp);
    let token_b = sign_scoped_token(&keys, NS_B, exp);

    let (user_port, admin_port) = ephemeral_port_pair().await;
    assert_ne!(user_port, admin_port);

    let dir = tempfile::tempdir().expect("the fixture tempdir opens");
    let mut rt =
        SqldRuntime::spawn(&sqld_bin, dir.path(), &keys.pub_pem, user_port, admin_port).await;
    let admin_client = probe_client(ADMIN_HTTP);
    rt.wait_until_ready(&admin_client).await;
    let admin_base = format!("http://127.0.0.1:{}", rt.admin_port);
    let user_base = format!("http://127.0.0.1:{}", rt.user_port);
    for ns in [NS_A, NS_B] {
        let (status, _) = admin_probe(
            &admin_client,
            &admin_base,
            "POST",
            &format!("/v1/namespaces/{ns}/create"),
            ADMIN_KEY,
            Some("{}"),
        )
        .await;
        assert_eq!(status, 200, "the create of {ns} succeeds");
    }

    // The worker root: five apps, each a dir with the generated manifest
    // and the real artifact as index.wasm.
    let root = tempfile::tempdir().expect("the worker root opens");
    for app in [APP_NS, APP_B, APP_CROSS, APP_WRONG_NS, APP_NO_GRANT] {
        let app_dir = root.path().join(app.replace(['@', '/'], "_"));
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(
            app_dir.join("manifest.yaml"),
            pipeline_manifest_text(app, &user_base),
        )
        .unwrap();
        std::fs::copy(&fixture, app_dir.join("index.wasm")).unwrap();
    }
    let index = edger_orchestrator::load_manifests_from_dirs(&[root.path().to_path_buf()])
        .expect("the real manifests load from disk");

    // Host-created grants bound to the exact WorkerRef identity of each
    // loaded app (name, version, namespace) read from the index.
    let vault = std::sync::Arc::new(
        AppSecretVault::open(
            dir.path().join("app-secrets.db"),
            Zeroizing::new(VAULT_MASTER),
        )
        .expect("the real vault opens"),
    );
    // A, B: own tokens. CROSS runs with grant A (the owner is A). WRONG_NS
    // holds token A but its grant names the SQLD namespace of B.
    // NO_GRANT has a token but no grant at all.
    vault.set(APP_NS, CREDENTIAL, &token_a).unwrap();
    vault.set(APP_B, CREDENTIAL, &token_b).unwrap();
    vault.set(APP_CROSS, CREDENTIAL, &token_b).unwrap();
    vault.set(APP_WRONG_NS, CREDENTIAL, &token_a).unwrap();
    vault.set(APP_NO_GRANT, CREDENTIAL, &token_a).unwrap();

    let owner_of = |app: &str| -> EgressWorkerContext {
        let worker = index
            .admin_workers()
            .into_iter()
            .find(|w| w.name == app)
            .unwrap_or_else(|| panic!("the index lacks {app}"));
        EgressWorkerContext {
            worker: worker.name.clone(),
            version: worker.version.clone(),
            namespace: worker.namespace.clone(),
            process_id: String::new(),
        }
    };
    let make_grant = |owner: &EgressWorkerContext, ns: &str| {
        std::sync::Arc::new(
            EgressDatabaseGrant::new(
                owner,
                &format!("{user_base}/"),
                ns,
                CREDENTIAL,
                Some(vault.clone()),
            )
            .expect("valid grant"),
        )
    };
    let grant_a = make_grant(&owner_of(APP_NS), NS_A);
    let mut grants = std::collections::HashMap::new();
    grants.insert(APP_NS.to_string(), grant_a.clone());
    grants.insert(APP_B.to_string(), make_grant(&owner_of(APP_B), NS_B));
    // The foreign grant: owner A, handed to the CROSS app by the host.
    grants.insert(APP_CROSS.to_string(), grant_a);
    grants.insert(
        APP_WRONG_NS.to_string(),
        make_grant(&owner_of(APP_WRONG_NS), NS_B),
    );
    // APP_NO_GRANT intentionally has no entry.

    let server = edger_orchestrator::ServerState::new_unready();
    let store = server.operational_events();
    let guard = strict_guard(&store);
    let factory = std::sync::Arc::new(GrantFactory {
        vault: vault.clone(),
        guard,
        grants,
    });
    let pool = edger_worker::WorkerPool::with_factory(edger_worker::PoolConfig::default(), factory);
    server.mark_ready(pool.clone());
    let state = edger_orchestrator::OrchestratorState {
        server,
        pool,
        index,
        auth: edger_orchestrator::ControlAuth::with_static_key("test-root"),
    };
    let router = edger_orchestrator::build_pipeline(state);
    let mut served = ServedPipeline::start(router).await;
    let base = served.base.clone();
    eprintln!(
        "pool-real: pipeline TCP {base}, sqld user port {user_port}, admin port {admin_port}"
    );
    // The read bound covers the COLD Go module (~16 s) inside the 120 s outer bound.
    let client = probe_client(Duration::from_secs(100));
    // NO `authorization` on any public request below: the root key stays
    // configured on the host only (control plane).
    let no_auth: [(&str, &str); 0] = [];

    // Every response observed, for the final privacy scan.
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let secrets = [
        token_a.as_str(),
        token_b.as_str(),
        ADMIN_KEY,
        keys.priv_pem.as_str(),
        PRIV_PEM_HEADER,
    ];

    // 1. /inspect through pipeline + pool: method, path, query, body.
    let (status, headers, body) = pipeline_call(
        &client,
        &base,
        "POST",
        "/@nsgo/go-wasm-http/inspect/sub?a=1&b=x%20y",
        &[("x-example", "pool-real"), ("content-type", "text/plain")],
        "hello-pool",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(
        status,
        200,
        "inspect body: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json")));
    let inspect: Value = serde_json::from_slice(&body).expect("inspect answers JSON");
    assert_eq!(inspect["method"], "POST");
    assert_eq!(inspect["path"], "/inspect/sub");
    assert_eq!(inspect["rawQuery"], "a=1&b=x%20y");
    assert_eq!(inspect["xExample"], "pool-real");
    assert_eq!(
        inspect["body"],
        base64::engine::general_purpose::STANDARD.encode("hello-pool")
    );

    // 2. /db for A through pipeline + pool: the exact body. Hostile
    // headers/query naming B's namespace, another origin and a token must
    // grant nothing: the grant is the host's.
    let (status, _, body) = pipeline_call(
        &client,
        &base,
        "GET",
        "/@nsgo/go-wasm-http/db",
        &no_auth,
        "",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 200);
    assert_eq!(body.as_ref(), DB_OK_BODY.as_bytes());
    let a_events = app_egress_events(&store, APP_NS);
    assert_eq!(a_events.len(), 1);
    assert_eq!(a_events[0].outcome.as_deref(), Some("ok"));
    assert_eq!(a_events[0].status, Some(200));
    assert_eq!(a_events[0].code, None);
    assert_eq!(a_events[0].namespace.as_deref(), Some(APP_NS_SCOPE));
    assert_eq!(a_events[0].worker.as_deref(), Some(APP_NS));
    assert_eq!(a_events[0].version.as_deref(), Some("1.0.0"));

    let hostile_ns = no_pad(NS_B);
    let (status, _, body) = pipeline_call(
        &client,
        &base,
        "GET",
        &format!(
            "/@nsgo/go-wasm-http/db?ns={NS_B}&url=http://127.0.0.1:{admin_port}&token={token_b}"
        ),
        &[
            ("x-namespace-bin", hostile_ns.as_str()),
            ("x-sqld-url", "http://127.0.0.1:1/"),
            ("x-forwarded-host", "evil.example"),
        ],
        "",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 200);
    assert_eq!(body.as_ref(), DB_OK_BODY.as_bytes());
    let a_events = app_egress_events(&store, APP_NS);
    assert_eq!(a_events.len(), 2);
    for event in &a_events {
        assert_eq!(event.status, Some(200), "still A's own namespace");
        let port: u16 = message_field(&event.message, "port")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(port, rt.user_port, "no guest-chosen destination");
    }

    // 3. /db for B: its own grant answers 200 with its own event.
    let (status, _, body) =
        pipeline_call(&client, &base, "GET", "/go-wasm-http-b/db", &no_auth, "").await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 200);
    assert_eq!(body.as_ref(), DB_OK_BODY.as_bytes());
    let b_events = app_egress_events(&store, APP_B);
    assert_eq!(b_events.len(), 1);
    assert_eq!(b_events[0].status, Some(200));
    assert_eq!(b_events[0].code, None);

    // 4. Cross-owner: CROSS is handed A's grant by the host; the bridge
    // refuses (owner mismatch): static 502 + the host denial event.
    let (status, _, body) = pipeline_call(
        &client,
        &base,
        "GET",
        "/go-wasm-http-cross/db",
        &no_auth,
        "",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 502);
    assert_eq!(body.as_ref(), DB_502_BODY.as_bytes());
    let cross = app_egress_events(&store, APP_CROSS);
    assert_eq!(cross.len(), 1);
    assert_eq!(cross[0].code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(cross[0].status, Some(403));
    assert_eq!(cross[0].outcome.as_deref(), Some("blocked"));

    // 5. Wrong SQLD namespace: owner matches, token A (scoped to NS_A) is
    // refused upstream for NS_B (404); the guest answers the static 502.
    let (status, _, body) = pipeline_call(
        &client,
        &base,
        "GET",
        "/go-wasm-http-wrongns/db",
        &no_auth,
        "",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 502);
    assert_eq!(body.as_ref(), DB_502_BODY.as_bytes());
    let wrong = app_egress_events(&store, APP_WRONG_NS);
    assert_eq!(wrong.len(), 1);
    assert_eq!(wrong[0].outcome.as_deref(), Some("ok"));
    assert_eq!(wrong[0].status, Some(404));
    assert_eq!(wrong[0].code, None);

    // 6. No grant: the import is denied before any egress event.
    let events_before = egress_events(&store).len();
    let (status, _, body) = pipeline_call(
        &client,
        &base,
        "GET",
        "/go-wasm-http-nogrant/db",
        &no_auth,
        "",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 500, "the import-denied response status");
    let denied: Value = serde_json::from_slice(&body).expect("the denial is JSON");
    assert_eq!(denied["code"], "WORKER_ERROR");
    assert!(
        denied["message"]
            .as_str()
            .is_some_and(|message| message.contains("WASM_IMPORT_DENIED")),
        "the response identifies the denied import, not another worker failure"
    );
    let dispatch = store
        .query(OperationalEventQuery {
            worker: Some(APP_NO_GRANT.into()),
            kind: Some("dispatch".into()),
            limit: Some(50),
            ..Default::default()
        })
        .events;
    assert_eq!(dispatch.len(), 1, "exactly one no-grant dispatch");
    assert_eq!(dispatch[0].code.as_deref(), Some("WORKER_ERROR"));
    assert!(
        dispatch[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("WASM_IMPORT_DENIED")),
        "the dispatch event identifies the import-denial cause"
    );
    assert_eq!(
        egress_events(&store).len(),
        events_before,
        "zero new guard events"
    );
    assert!(app_egress_events(&store, APP_NO_GRANT).is_empty());

    // Control/data plane separation: the control plane refuses the same
    // unauthenticated TCP client and accepts the host root key.
    let (status, _, body) =
        pipeline_call(&client, &base, "GET", "/api/admin/workers", &no_auth, "").await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 401, "the control plane needs the root key");
    let (status, _, body) = pipeline_call(
        &client,
        &base,
        "GET",
        "/api/admin/workers",
        &[("authorization", "Bearer test-root")],
        "",
    )
    .await;
    remember_pipeline_response(&mut seen, &body, &secrets);
    assert_eq!(status, 200, "the root key opens the control plane");

    served.stop().await;
    rt.shutdown().await;

    // The same store receives pipeline dispatch and guard egress events.
    // Neither event family may make this privacy scan vacuous.
    assert!(!egress_events(&store).is_empty(), "guard events present");
    assert!(
        !store
            .query(OperationalEventQuery {
                kind: Some("dispatch".into()),
                limit: Some(1),
                ..Default::default()
            })
            .events
            .is_empty(),
        "pipeline dispatch events present"
    );
    // Privacy: responses, both event families and child logs carry no secret.
    for body in &seen {
        assert!(
            log_privacy_error(body, &secrets).is_none(),
            "a pipeline response carries a fixture secret (static label)"
        );
    }
    match scan_store_privacy(&store, &secrets) {
        Ok(count) => assert!(count > 0, "a vacuous scan proves nothing"),
        Err(label) => panic!("{label}"),
    }
    for name in ["sqld-stdout.log", "sqld-stderr.log"] {
        let bytes = child_log_bytes(&dir.path().join(name));
        assert!(
            log_privacy_error(&bytes, &secrets).is_none(),
            "{LOG_SECRET_LABEL}"
        );
    }
}
