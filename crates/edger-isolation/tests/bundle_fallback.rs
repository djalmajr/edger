//! EDG-30: a minimal relative-module fallback bundle is NEVER published to
//! the per-version cache, and the fallback FAILS when a relative import
//! survives in the output — now validated with Deno's own dependency graph
//! (`deno info --json` of the output file, amendment 1) instead of a
//! hand-written scanner. Amendment 2 tightens the graph rule: a correct
//! fallback embeds every relative module, so ANY `file:` code dependency
//! left in the output (different from the output file itself, the graph
//! root) refuses it — whether or not the target exists or resolves, since
//! the rule no longer depends on the inspection dir (the staging dir is
//! one level deeper than the per-process dir the bundle is finally
//! materialized in). Amendment 3 limits that rule to the modules
//! REACHABLE from the graph roots by CODE edges only: a subgraph behind a
//! type edge never executes, so its own surviving `file:` code edges cannot
//! refuse a working fallback.
//!
//! The real `deno bundle` registry failure is simulated with a FAKE deno
//! executable (no network): `--version` answers, `info --json` answers a
//! graph and `bundle` always fails with the registry stderr (the EDG-14
//! minimal-fallback trigger). The deterministic rejection test's fake
//! answers the graph of the OUTPUT file with an errored `file:` target;
//! the real-den tests' fake delegates `info --json` to the REAL deno
//! (skipped when no real deno executable is available — same pattern as
//! `deno_bundler.rs`), so the validation is proven against the real
//! parser. The last test uses the REAL deno to prove the next spawn with
//! the real bundler available builds the real bundle and publishes it.
//!
//! The tests of this binary share the process-wide bundle counters:
//! serialize every test that asserts on a counter delta (same pattern as
//! `bundle_cache.rs`).

#![cfg(feature = "multiproc")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use edger_isolation::{bundle_cache_stats, DenoCliBundler};

static COUNTER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn counter_guard() -> tokio::sync::MutexGuard<'static, ()> {
    COUNTER_LOCK.lock().await
}

const SPAWN_TIMEOUT: Duration = Duration::from_secs(60);

/// The `v1-*` key dirs inside the worker's `.edger-bundle-cache` (staging
/// dirs excluded).
fn cache_key_dirs(worker: &Path) -> Vec<String> {
    let cache = worker.join(".edger-bundle-cache");
    let Ok(entries) = std::fs::read_dir(&cache) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("v1-"))
        .collect()
}

/// The leftover `.tmp-*` staging dirs inside the worker's cache root.
fn cache_staging_dirs(worker: &Path) -> Vec<String> {
    let cache = worker.join(".edger-bundle-cache");
    let Ok(entries) = std::fs::read_dir(&cache) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".tmp-"))
        .collect()
}

/// A fake `deno` executable (no network): `--version` answers, `info
/// --json` answers a valid empty graph for the worker entry and a BROKEN
/// graph for the fallback OUTPUT file (a code edge to an errored `file:`
/// target, the shape the real `deno info` produces for a surviving
/// relative import), and `bundle` ALWAYS fails with the registry stderr,
/// counting every `bundle` attempt in `state`.
#[cfg(unix)]
fn write_fake_deno_broken_output_graph(root: &Path, state: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        r#"#!/bin/sh
STATE='{state}'
case "$1" in
  --version)
    printf 'deno 2.9.7 (fake, edg-30 test)\n'
    exit 0
    ;;
  info)
    target="$3"
    name=$(basename "$target")
    dir=$(dirname "$target")
    if [ "$name" = "index.bundle.mjs" ]; then
      json="{{\"roots\":[\"file://$dir/index.bundle.mjs\"],\"modules\":[{{\"kind\":\"esm\",\"local\":\"$dir/index.bundle.mjs\",\"size\":1,\"mediaType\":\"Mjs\",\"specifier\":\"file://$dir/index.bundle.mjs\",\"dependencies\":[{{\"specifier\":\"./assets/chunk.js\",\"code\":{{\"specifier\":\"file://$dir/assets/chunk.js\"}}}}]}},{{\"specifier\":\"file://$dir/assets/chunk.js\",\"error\":\"Module not found \\\"file://$dir/assets/chunk.js\\\".\"}}],\"redirects\":{{}},\"npmPackages\":{{}}}}"
      printf '%s' "$json"
      exit 0
    fi
    printf '%s' '{{"roots":[],"modules":[]}}'
    exit 0
    ;;
  bundle)
    n=$(cat "$STATE" 2>/dev/null || printf 0)
    n=$((n + 1))
    printf '%s' "$n" > "$STATE"
    printf '%s\n' 'error: Failed loading https://registry.npmjs.org/@esbuild/darwin-arm64 for package "@esbuild/darwin-arm64"' 1>&2
    exit 1
    ;;
  *)
    printf 'fake deno: unsupported args\n' 1>&2
    exit 1
    ;;
esac
"#,
        state = state.display(),
    );
    let fake = root.join("fake-deno");
    fs::write(&fake, script).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

/// A fake `deno` executable (no network) whose `info --json` DELEGATES to
/// the REAL deno (the graph validation is then proven against the real
/// parser) and whose `bundle` ALWAYS fails with the registry stderr,
/// counting every `bundle` attempt in `state`.
#[cfg(unix)]
fn write_fake_deno_delegating_info(root: &Path, state: &Path, real_deno: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        r#"#!/bin/sh
STATE='{state}'
REAL_DENO='{real_deno}'
case "$1" in
  --version)
    printf 'deno 2.9.7 (fake, edg-30 real-info test)\n'
    exit 0
    ;;
  info)
    exec "$REAL_DENO" "$@"
    ;;
  bundle)
    n=$(cat "$STATE" 2>/dev/null || printf 0)
    n=$((n + 1))
    printf '%s' "$n" > "$STATE"
    printf '%s\n' 'error: Failed loading https://registry.npmjs.org/@esbuild/darwin-arm64 for package "@esbuild/darwin-arm64"' 1>&2
    exit 1
    ;;
  *)
    printf 'fake deno: unsupported args\n' 1>&2
    exit 1
    ;;
esac
"#,
        state = state.display(),
        real_deno = real_deno,
    );
    let fake = root.join("fake-deno");
    fs::write(&fake, script).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

/// The first real `deno` executable available (same candidates the bundler
/// uses); `None` when no real deno can be spawned — the real-den tests are
/// skipped in that case (same pattern as `deno_bundler.rs`).
fn real_deno_executable() -> Option<String> {
    DenoCliBundler::default()
        .executable_candidates()
        .into_iter()
        .find(|candidate| {
            std::process::Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
}

/// A fake `deno` executable (no network): `--version` answers, `info --json`
/// answers a valid empty graph and `bundle` ALWAYS fails with the registry
/// stderr, counting every `bundle` attempt in `state`.
#[cfg(unix)]
fn write_fake_deno(root: &Path, state: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        r#"#!/bin/sh
STATE='{state}'
case "$1" in
  --version)
    printf 'deno 2.9.7 (fake, edg-30 test)\n'
    exit 0
    ;;
  info)
    printf '%s' '{{"roots":[],"modules":[]}}'
    exit 0
    ;;
  bundle)
    n=$(cat "$STATE" 2>/dev/null || printf 0)
    n=$((n + 1))
    printf '%s' "$n" > "$STATE"
    printf '%s\n' 'error: Failed loading https://registry.npmjs.org/@esbuild/darwin-arm64 for package "@esbuild/darwin-arm64"' 1>&2
    exit 1
    ;;
  *)
    printf 'fake deno: unsupported args\n' 1>&2
    exit 1
    ;;
esac
"#,
        state = state.display(),
    );
    let fake = root.join("fake-deno");
    fs::write(&fake, script).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

/// A two-file worker the minimal relative-module fallback CAN resolve (the
/// line-based `import ... from` form).
fn write_simple_worker(root: &Path, name: &str) -> PathBuf {
    let worker = root.join(name);
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join("index.ts"),
        r#"import { greeting } from "./greeting.ts";
Deno.serve(() => new Response(greeting));
"#,
    )
    .unwrap();
    fs::write(
        worker.join("greeting.ts"),
        "export const greeting = 'hello-fallback';\n",
    )
    .unwrap();
    worker
}

/// EDG-30 item 2: a minified entrypoint whose relative import the line-based
/// fallback cannot strip (`import{a as b}from"./assets/chunk.js"`, the
/// Vite/rolldown chunk form) makes the fallback FAIL with
/// `DENO_BUNDLE_FAILED` — the original registry cause and the unresolved
/// `file:` specifier cited by the `deno info` graph of the output file —
/// and leaves NO cache entry.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn minified_relative_import_fails_the_fallback_without_cache_entry() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = root.path().join("minified-app");
    fs::create_dir_all(worker.join("assets")).unwrap();
    fs::write(
        worker.join("assets").join("chunk.js"),
        "export const a = 'chunk-ok';\n",
    )
    .unwrap();
    // Vite/rolldown-style minified entrypoint: no space after
    // `import`/`from` — the form the line-based fallback does not strip.
    fs::write(
        worker.join("index.js"),
        r#"import{a as b}from"./assets/chunk.js";Deno.serve(()=>new Response(b));"#,
    )
    .unwrap();
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_broken_output_graph(root.path(), &state);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());

    let before = bundle_cache_stats();
    let err = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .unwrap_err();
    let after = bundle_cache_stats();

    assert_eq!(
        err.code, "DENO_BUNDLE_FAILED",
        "the unresolved relative import fails the bundle: {err}"
    );
    assert!(
        err.message.contains("assets/chunk.js"),
        "the unresolved file: specifier from the output graph is cited: {err}"
    );
    assert!(
        err.message.contains("registry.npmjs.org"),
        "the original `deno bundle` cause (the registry stderr) is kept: {err}"
    );
    assert_eq!(
        after.error_total - before.error_total,
        1,
        "the failed fallback counts as error"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "nothing was published"
    );
    assert_eq!(
        after.uncached_total - before.uncached_total,
        0,
        "a failed fallback is not a per-process bundle"
    );
    assert!(
        cache_key_dirs(&worker).is_empty(),
        "no cache entry is created for the failed fallback"
    );
    assert!(
        cache_staging_dirs(&worker).is_empty(),
        "no staging dir was left behind"
    );
    assert_eq!(
        fs::read_to_string(&state).unwrap(),
        "2",
        "the real bundler was tried once plus the single EDG-14 retry"
    );
}

/// EDG-30 item 1: a worker the minimal fallback actually RESOLVES still
/// succeeds when the real bundler fails with the registry error (the request
/// gets the fallback bundle, kept alive by a per-process tempdir), but NO
/// cache entry is created — the next spawn retries the real `deno bundle`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolved_fallback_bundle_works_without_cache_entry() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_simple_worker(root.path(), "simple-app");
    let entry = worker.join("index.ts");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno(root.path(), &state);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());

    let before = bundle_cache_stats();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("the fallback resolves this simple worker");
    let after = bundle_cache_stats();

    assert!(
        res.dir.is_some(),
        "the fallback bundle is a per-process file kept alive by its tempdir: {}",
        res.bundle.path
    );
    assert!(
        !res.bundle.path.contains(".edger-bundle-cache"),
        "the fallback bundle is not read from (or written to) the cache: {}",
        res.bundle.path
    );
    let source = fs::read_to_string(Path::new(&res.bundle.path)).unwrap();
    assert!(
        source.contains("const greeting = 'hello-fallback';"),
        "the fallback inlines the relative module body: {source}"
    );
    assert!(
        !source.contains("./greeting.ts"),
        "no relative import specifier survives the fallback output: {source}"
    );
    assert_eq!(
        after.uncached_total - before.uncached_total,
        1,
        "a fallback bundle that is not cached counts as uncached"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "nothing was published"
    );
    assert!(
        cache_key_dirs(&worker).is_empty(),
        "no cache entry is created although the request succeeded"
    );
    assert!(
        cache_staging_dirs(&worker).is_empty(),
        "no staging dir was left behind"
    );
    assert_eq!(
        fs::read_to_string(&state).unwrap(),
        "2",
        "the real bundler was tried once plus the single EDG-14 retry"
    );
    // The resolved fallback bundle must RUN (no runtime `Module not found`):
    // import it with the real deno when one is available.
    let mut runtime_proven = false;
    for executable in DenoCliBundler::default().executable_candidates() {
        let harness_dir = tempfile::tempdir().expect("create harness tempdir");
        let harness_path = harness_dir.path().join("harness.mjs");
        fs::write(
            &harness_path,
            r#"
const bundleUrl = Deno.args[0];
let capturedHandler = null;
const originalServe = Deno.serve;
Deno.serve = (arg) => {
  if (typeof arg === "function") {
    capturedHandler = arg;
  } else if (arg && typeof arg.fetch === "function") {
    capturedHandler = arg.fetch.bind(arg);
  } else if (arg && typeof arg.handler === "function") {
    capturedHandler = arg.handler.bind(arg);
  }
  return { finished: Promise.resolve(), ref() {}, shutdown() {}, unref() {} };
};
try {
  await import(bundleUrl);
} finally {
  Deno.serve = originalServe;
}
if (typeof capturedHandler !== "function") throw new Error("no Deno.serve handler captured");
const response = await capturedHandler(new Request("https://example.test/"));
console.log(await response.text());
"#,
        )
        .expect("write harness");
        let read_allowlist = format!(
            "{},{}",
            harness_dir.path().display(),
            Path::new(&res.bundle.path)
                .parent()
                .expect("bundle has parent")
                .display()
        );
        match std::process::Command::new(&executable)
            .arg("run")
            .arg("--no-prompt")
            .arg(format!("--allow-read={read_allowlist}"))
            .arg(&harness_path)
            .arg(format!("file://{}", res.bundle.path))
            .output()
        {
            Ok(output) if output.status.success() => {
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    "hello-fallback",
                    "the fallback bundle serves the request"
                );
                runtime_proven = true;
                break;
            }
            Ok(output) => {
                panic!(
                    "the fallback bundle must run: status {:?}; stderr={}; stdout={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim(),
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => panic!("failed to spawn {executable}: {err}"),
        }
    }
    if !runtime_proven {
        eprintln!("skipping the runtime proof: no real deno executable found");
    }
}

/// EDG-30 item 1 (next spawn): after a registry failure served the
/// (unpublished) fallback bundle, the next spawn with the real `deno bundle`
/// available builds the real bundle and PUBLISHES it to the per-version
/// cache; the spawn after that is a cache hit without a new build.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_bundler_publishes_to_the_cache_after_the_fallback_failure() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_simple_worker(root.path(), "real-app");
    let entry = worker.join("index.ts");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno(root.path(), &state);
    let fallback = tempfile::tempdir().unwrap();

    // 1. The registry failure: the fallback bundle serves the request and
    //    nothing is cached.
    let fake_bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());
    let before_fake = bundle_cache_stats();
    let fallback_res = fake_bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("the fallback serves the request while the registry is down");
    let after_fake = bundle_cache_stats();
    assert_eq!(
        after_fake.uncached_total - before_fake.uncached_total,
        1,
        "the fallback counts as uncached"
    );
    assert!(
        cache_key_dirs(&worker).is_empty(),
        "the fallback bundle was not published: the cache stays entry-less"
    );
    drop(fallback_res);

    // 2. The real `deno bundle` is available again: the next spawn builds
    //    the real bundle and publishes it.
    let real = DenoCliBundler::default();
    let before_real = bundle_cache_stats();
    let res = match real
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
    {
        Ok(res) => res,
        Err(err) if err.code == "DENO_BUNDLE_UNAVAILABLE" => {
            eprintln!("skipping real_bundler_publishes...: no real deno available: {err}");
            return;
        }
        Err(err) => panic!("the real bundle must succeed: {err}"),
    };
    let after_real = bundle_cache_stats();
    assert!(
        res.bundle.path.contains(".edger-bundle-cache"),
        "the real bundle is published to the cache: {}",
        res.bundle.path
    );
    assert!(
        res.dir.is_none(),
        "the published bundle needs no per-process tempdir"
    );
    assert_eq!(
        after_real.miss_total - before_real.miss_total,
        1,
        "the real build counts as miss"
    );
    let keys = cache_key_dirs(&worker);
    assert_eq!(keys.len(), 1, "exactly one cache entry: {keys:?}");

    // 3. The spawn after that is a cache hit without a new build.
    let before_hit = bundle_cache_stats();
    let res_hit = real
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("the published entry is reusable");
    let after_hit = bundle_cache_stats();
    assert_eq!(
        res_hit.bundle.path, res.bundle.path,
        "the same published bundle is reused"
    );
    assert_eq!(
        after_hit.hit_total - before_hit.hit_total,
        1,
        "the next spawn is a cache hit"
    );
    assert_eq!(
        after_hit.miss_total - before_hit.miss_total,
        0,
        "no rebuild for the hit"
    );
}

// ---------------------------------------------------------------------------
// EDG-30 amendment 1: the fallback output is validated with Deno's own
// dependency graph (`deno info --json`, the real parser) instead of the
// hand-written scanner. The fake deno's `info --json` DELEGATES to the real
// deno, so the validation is proven against the real parser (skipped when no
// real deno executable is available — same pattern as `deno_bundler.rs`).
// ---------------------------------------------------------------------------

/// EDG-30 amendment 1: a worker whose code contains a method call
/// (`o.import("./asset")`) and template TEXT (`` `import("./asset")` ``)
/// that merely LOOKS like a relative import. The real `deno info` does not
/// flag either as a dependency, so the fallback output is ACCEPTED and served
/// (the removed scanner's false positives), with NO cache entry.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_accepts_method_call_and_template_text_without_cache_entry() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!("skipping real_den_info_accepts_method_call...: no real deno executable found");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = root.path().join("decoys-app");
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join("index.js"),
        r#"const o={import(x){return x}};o.import("./asset");
const s=`import("./asset")`;
Deno.serve(() => new Response(`decoys-ok:${o.import("./asset")}:${s.length}`));
"#,
    )
    .unwrap();
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());

    let before = bundle_cache_stats();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("method calls and template text are NOT imports: the real deno info accepts the fallback output");
    let after = bundle_cache_stats();

    assert!(
        res.dir.is_some(),
        "the fallback bundle is a per-process file kept alive by its tempdir: {}",
        res.bundle.path
    );
    assert!(
        !res.bundle.path.contains(".edger-bundle-cache"),
        "the fallback bundle is not read from (or written to) the cache: {}",
        res.bundle.path
    );
    assert_eq!(
        after.uncached_total - before.uncached_total,
        1,
        "a fallback bundle that is not cached counts as uncached"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "nothing was published"
    );
    assert!(
        cache_key_dirs(&worker).is_empty(),
        "no cache entry is created although the request succeeded"
    );
    assert!(
        cache_staging_dirs(&worker).is_empty(),
        "no staging dir was left behind"
    );
    assert_eq!(
        fs::read_to_string(&state).unwrap(),
        "2",
        "the real bundler was tried once plus the single EDG-14 retry"
    );
    // The accepted fallback bundle must RUN under the real deno (proving the
    // two decoy forms are valid code, not just unflagged).
    let mut runtime_proven = false;
    for executable in DenoCliBundler::default().executable_candidates() {
        let harness_dir = tempfile::tempdir().expect("create harness tempdir");
        let harness_path = harness_dir.path().join("harness.mjs");
        fs::write(
            &harness_path,
            r#"
const bundleUrl = Deno.args[0];
let capturedHandler = null;
const originalServe = Deno.serve;
Deno.serve = (arg) => {
  if (typeof arg === "function") {
    capturedHandler = arg;
  } else if (arg && typeof arg.fetch === "function") {
    capturedHandler = arg.fetch.bind(arg);
  } else if (arg && typeof arg.handler === "function") {
    capturedHandler = arg.handler.bind(arg);
  }
  return { finished: Promise.resolve(), ref() {}, shutdown() {}, unref() {} };
};
try {
  await import(bundleUrl);
} finally {
  Deno.serve = originalServe;
}
if (typeof capturedHandler !== "function") throw new Error("no Deno.serve handler captured");
const response = await capturedHandler(new Request("https://example.test/"));
console.log(await response.text());
"#,
        )
        .expect("write harness");
        let read_allowlist = format!(
            "{},{}",
            harness_dir.path().display(),
            Path::new(&res.bundle.path)
                .parent()
                .expect("bundle has parent")
                .display()
        );
        match std::process::Command::new(&executable)
            .arg("run")
            .arg("--no-prompt")
            .arg(format!("--allow-read={read_allowlist}"))
            .arg(&harness_path)
            .arg(format!("file://{}", res.bundle.path))
            .output()
        {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                assert!(
                    text.starts_with("decoys-ok:./asset:"),
                    "the fallback bundle serves the request: {text}"
                );
                runtime_proven = true;
                break;
            }
            Ok(output) => {
                panic!(
                    "the fallback bundle must run: status {:?}; stderr={}; stdout={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim(),
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => panic!("failed to spawn {executable}: {err}"),
        }
    }
    assert!(
        runtime_proven,
        "the real deno ran the accepted fallback bundle"
    );
}

/// A worker entry that survives the minimal fallback with a REAL relative
/// import in one of the forms the removed scanner MISSED (a false negative):
/// the `deno info` graph of the OUTPUT file (via the real deno) must refuse
/// the fallback with `DENO_BUNDLE_FAILED`, citing the specifier, and leave no
/// cache entry.
#[cfg(unix)]
fn write_survivor_worker(
    root: &Path,
    name: &str,
    entry: &str,
    dep_rel: &str,
    dep_body: &str,
) -> PathBuf {
    let worker = root.join(name);
    fs::create_dir_all(&worker).unwrap();
    fs::write(worker.join("index.js"), entry).unwrap();
    let dep = worker.join(dep_rel);
    if let Some(parent) = dep.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(dep, dep_body).unwrap();
    worker
}

/// The common REFUSAL assertions (EDG-30 amendment 1): `DENO_BUNDLE_FAILED`
/// with the original registry cause and the specifier cited by the `deno
/// info` graph, nothing published, exactly two `deno bundle` attempts.
#[cfg(unix)]
async fn assert_fallback_refused(
    bundler: &DenoCliBundler,
    worker: &Path,
    entry: &Path,
    fallback: &Path,
    state: &Path,
    expected_in_message: &str,
) {
    let before = bundle_cache_stats();
    let err = bundler
        .resolve_bundle_entry(worker, entry, fallback, SPAWN_TIMEOUT)
        .await
        .unwrap_err();
    let after = bundle_cache_stats();

    assert_eq!(
        err.code, "DENO_BUNDLE_FAILED",
        "the unresolved relative import fails the bundle: {err}"
    );
    assert!(
        err.message.contains(expected_in_message),
        "the unresolved file: specifier is cited: {err}"
    );
    assert!(
        err.message.contains("registry.npmjs.org"),
        "the original `deno bundle` cause (the registry stderr) is kept: {err}"
    );
    assert_eq!(
        after.error_total - before.error_total,
        1,
        "the failed fallback counts as error"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "nothing was published"
    );
    assert_eq!(
        after.uncached_total - before.uncached_total,
        0,
        "a failed fallback is not a per-process bundle"
    );
    assert!(
        cache_key_dirs(worker).is_empty(),
        "no cache entry is created for the failed fallback"
    );
    assert!(
        cache_staging_dirs(worker).is_empty(),
        "no staging dir was left behind"
    );
    assert_eq!(
        fs::read_to_string(state).unwrap(),
        "2",
        "the real bundler was tried once plus the single EDG-14 retry"
    );
}

/// EDG-30 amendment 1: a comment BETWEEN the tokens
/// (`import(/* chunk */"./x.js")`) survives the fallback and the real `deno
/// info` graph of the output refuses it (the removed scanner's false
/// negative).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_refuses_comment_dynamic_import_without_cache_entry() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!("skipping real_den_info_refuses_comment...: no real deno executable found");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = write_survivor_worker(
        root.path(),
        "comment-app",
        r#"const p=import(/* chunk */"./x.js");console.log((await p).x);
Deno.serve(() => new Response("comment"));
"#,
        "x.js",
        "export const x = 'comment-ok';\n",
    );
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());
    assert_fallback_refused(&bundler, &worker, &entry, fallback.path(), &state, "x.js").await;
}

/// EDG-30 amendment 1: a TEMPLATE-LITERAL dynamic import (``
/// import(`./x.js`) ``) survives the fallback and the real `deno info` graph
/// of the output refuses it (the removed scanner's false negative).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_refuses_template_dynamic_import_without_cache_entry() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!("skipping real_den_info_refuses_template...: no real deno executable found");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = write_survivor_worker(
        root.path(),
        "template-app",
        r#"const p=import(`./x.js`);console.log((await p).x);
Deno.serve(() => new Response("template"));
"#,
        "x.js",
        "export const x = 'template-ok';\n",
    );
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());
    assert_fallback_refused(&bundler, &worker, &entry, fallback.path(), &state, "x.js").await;
}

/// EDG-30 amendment 1: an ESCAPED specifier (`import("\x2e/x.js")`) survives
/// the fallback and the real `deno info` graph of the output refuses it (the
/// removed scanner's false negative).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_refuses_escaped_dynamic_import_without_cache_entry() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!("skipping real_den_info_refuses_escaped...: no real deno executable found");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = write_survivor_worker(
        root.path(),
        "escaped-app",
        r#"const p=import("\x2e/x.js");console.log((await p).x);
Deno.serve(() => new Response("escaped"));
"#,
        "x.js",
        "export const x = 'escaped-ok';\n",
    );
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());
    assert_fallback_refused(&bundler, &worker, &entry, fallback.path(), &state, "x.js").await;
}

/// EDG-30 amendment 1: the MINIFIED static import
/// (`import{a as b}from"./assets/x.js"`) survives the fallback and the real
/// `deno info` graph of the output refuses it (the removed scanner's false
/// negative).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_refuses_minified_static_import_without_cache_entry() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!("skipping real_den_info_refuses_minified...: no real deno executable found");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = write_survivor_worker(
        root.path(),
        "minified2-app",
        r#"import{a as b}from"./assets/x.js";Deno.serve(()=>new Response(b));"#,
        "assets/x.js",
        "export const a = 'minified-ok';\n",
    );
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());
    assert_fallback_refused(
        &bundler,
        &worker,
        &entry,
        fallback.path(),
        &state,
        "assets/x.js",
    )
    .await;
}

/// EDG-30 amendment 2 (the reviewer's relocation probe, now permanent):
/// the staging dir (`<worker>/.edger-bundle-cache/.tmp-*/`) is one level
/// DEEPER than the per-process dir the fallback bundle is finally
/// materialized in (`<worker>/.edger-bundle-*/`), so a surviving
/// `import("../../x.js")` RESOLVES from the staging dir (the amendment-1
/// rule passed it) but points OUTSIDE the worker from the final dir
/// (runtime `Module not found`). Under the amendment-2 rule ANY `file:`
/// code dependency different from the output file itself refuses the
/// fallback, whether or not it exists or resolves: `DENO_BUNDLE_FAILED`
/// citing the specifier, NO cache entry.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_refuses_surviving_file_dependency_that_resolves_in_staging() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!(
            "skipping real_den_info_refuses_surviving_file_dependency...: no real deno executable found"
        );
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = root.path().join("relocation-app");
    fs::create_dir_all(worker.join("nested/deep")).unwrap();
    fs::write(
        worker.join("index.js"),
        r#"import { load } from "./nested/deep/a.js";
Deno.serve(async () => new Response((await load()).x));
"#,
    )
    .unwrap();
    // The module body KEEPS the dynamic import (the fallback only strips
    // the line-based static form): from the staging dir `../../x.js`
    // resolves to the worker root, from the final per-process dir it
    // points outside the worker.
    fs::write(
        worker.join("nested/deep/a.js"),
        r#"export function load() { return import("../../x.js"); }
"#,
    )
    .unwrap();
    fs::write(worker.join("x.js"), "export const x = 'relocation-ok';\n").unwrap();
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());
    assert_fallback_refused(&bundler, &worker, &entry, fallback.path(), &state, "x.js").await;
}

/// Runs the SAME `deno info --json <file>` command `validate_fallback_output
/// runs (the real parser, from the directory the file is executed in) and
/// returns the parsed graph.
#[cfg(unix)]
fn real_den_info_graph(real: &str, bundle_path: &Path) -> serde_json::Value {
    let dir = bundle_path
        .parent()
        .expect("the bundle has a parent directory");
    let file = bundle_path.file_name().expect("the bundle has a file name");
    let output = std::process::Command::new(real)
        .arg("info")
        .arg("--json")
        .current_dir(dir)
        .arg(file.to_string_lossy().as_ref())
        .output()
        .expect("spawn the real deno info");
    assert!(
        output.status.success(),
        "the real deno info must read the accepted fallback graph: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    serde_json::from_slice(&output.stdout).expect("parse the real deno info graph")
}

/// EDG-30 amendment 3 (the reviewer's type-only subgraph probe, now
/// PERMANENT): the entry references `./types.js` ONLY through a JSDoc type
/// (`typeof import('types').v`, mapped by `deno.json`), and `types.js`
/// re-exports `v` from `./helper.js`. In the real `deno info` graph the
/// root -> types.js edge is a TYPE edge and the types.js -> helper.js edge
/// is a CODE edge, but the subgraph never executes (the bundle drops
/// type-only imports). The refusal rule applies only to the modules
/// REACHABLE from the roots by code edges: the fallback is SERVED, the
/// handler answers, and no cache entry is left.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_den_info_accepts_type_reachable_subgraph_without_cache_entry() {
    let _guard = counter_guard().await;
    let Some(real) = real_deno_executable() else {
        eprintln!("skipping real_den_info_accepts_type_reachable_subgraph...: no real deno executable found");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let worker = root.path().join("type-only-app");
    fs::create_dir_all(&worker).unwrap();
    // The import map resolves the JSDoc `import('types')` reference to the
    // local `types.js` (the reviewer's fixture).
    fs::write(
        worker.join("deno.json"),
        r#"{"imports": {"types": "./types.js"}}"#,
    )
    .unwrap();
    fs::write(
        worker.join("index.js"),
        r#"/** @type {typeof import('types').v} */
const v = 42;
Deno.serve(() => new Response("type-only-ok"));
"#,
    )
    .unwrap();
    fs::write(
        worker.join("types.js"),
        "export { v } from \"./helper.js\";\n",
    )
    .unwrap();
    fs::write(worker.join("helper.js"), "export const v = 42;\n").unwrap();
    let entry = worker.join("index.js");
    let state = root.path().join("bundle-attempts");
    let fake = write_fake_deno_delegating_info(root.path(), &state, &real);
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());

    let before = bundle_cache_stats();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("a subgraph reachable only by a type edge never executes: the fallback is served");
    let after = bundle_cache_stats();

    assert!(
        res.dir.is_some(),
        "the fallback bundle is a per-process file kept alive by its tempdir: {}",
        res.bundle.path
    );
    assert!(
        !res.bundle.path.contains(".edger-bundle-cache"),
        "the fallback bundle is not read from (or written to) the cache: {}",
        res.bundle.path
    );
    let source = fs::read_to_string(Path::new(&res.bundle.path)).unwrap();
    assert!(
        source.contains("typeof import('types').v"),
        "the fallback keeps the JSDoc type reference (the type edge the real parser sees): {source}"
    );
    assert!(
        !source.contains("helper.js"),
        "the type-only subgraph is NOT embedded (it never executes): {source}"
    );
    assert_eq!(
        after.uncached_total - before.uncached_total,
        1,
        "a fallback bundle that is not cached counts as uncached"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "nothing was published"
    );
    assert!(
        cache_key_dirs(&worker).is_empty(),
        "no cache entry is created although the request succeeded"
    );
    assert!(
        cache_staging_dirs(&worker).is_empty(),
        "no staging dir was left behind"
    );
    assert_eq!(
        fs::read_to_string(&state).unwrap(),
        "2",
        "the real bundler was tried once plus the single EDG-14 retry"
    );

    // The graph the validator saw MUST be the reviewer's shape — a root
    // type edge to types.js and a types.js -> helper.js code edge —
    // otherwise the test would pass vacuously. `deno info` resolves the
    // paths through the /var <-> /private/var symlink, so match on the
    // path suffix of both the `file:` specifier and the `local` field.
    let graph = real_den_info_graph(&real, Path::new(&res.bundle.path));
    let bundle = res.bundle.path.clone();
    let bundle_local = std::fs::canonicalize(&bundle)
        .expect("the accepted bundle exists on disk")
        .to_string_lossy()
        .into_owned();
    let is_bundle_module = |module: &serde_json::Value| -> bool {
        for key in ["specifier", "local"] {
            let Some(value) = module.get(key).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let value = value.strip_prefix("file://").unwrap_or(value);
            if value == bundle || value == bundle_local {
                return true;
            }
        }
        false
    };
    let modules = graph
        .get("modules")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    let root_module = modules
        .iter()
        .find(|module| is_bundle_module(module))
        .unwrap_or_else(|| panic!("the bundle module is missing from the real graph: {graph}"));
    let dependencies = root_module
        .get("dependencies")
        .and_then(serde_json::Value::as_array)
        .expect("the bundle module carries its dependencies");
    let type_edge = dependencies
        .iter()
        .find(|dependency| {
            dependency
                .get("type")
                .and_then(|type_edge| type_edge.get("specifier"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|target| target.ends_with("types.js"))
        })
        .unwrap_or_else(|| panic!("the root must carry a TYPE edge to types.js: {root_module}"));
    assert!(
        type_edge.get("code").is_none(),
        "the root edge to types.js is type-only (no code edge): {type_edge}"
    );
    assert!(
        dependencies
            .iter()
            .all(|dependency| dependency.get("code").is_none()),
        "the root has NO code edge: its only edge is the type edge: {root_module}"
    );
    let types_module = modules
        .iter()
        .find(|module| {
            module
                .get("local")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|local| local.ends_with("types.js"))
        })
        .unwrap_or_else(|| panic!("types.js is missing from the real graph: {graph}"));
    assert!(
        types_module
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .unwrap()
            .iter()
            .any(|dependency| {
                dependency
                    .get("code")
                    .and_then(|code| code.get("specifier"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|target| target.ends_with("helper.js"))
            }),
        "types.js must carry the CODE edge to helper.js (the edge the old rule counted): {types_module}"
    );

    // The accepted fallback bundle must RUN under the real deno and answer
    // the request (the reviewer's runtime proof).
    let harness_dir = tempfile::tempdir().expect("create harness tempdir");
    let harness_path = harness_dir.path().join("harness.mjs");
    fs::write(
        &harness_path,
        r#"
const bundleUrl = Deno.args[0];
let capturedHandler = null;
const originalServe = Deno.serve;
Deno.serve = (arg) => {
  if (typeof arg === "function") {
    capturedHandler = arg;
  } else if (arg && typeof arg.fetch === "function") {
    capturedHandler = arg.fetch.bind(arg);
  } else if (arg && typeof arg.handler === "function") {
    capturedHandler = arg.handler.bind(arg);
  }
  return { finished: Promise.resolve(), ref() {}, shutdown() {}, unref() {} };
};
try {
  await import(bundleUrl);
} finally {
  Deno.serve = originalServe;
}
if (typeof capturedHandler !== "function") throw new Error("no Deno.serve handler captured");
const response = await capturedHandler(new Request("https://example.test/"));
console.log(await response.text());
"#,
    )
    .expect("write harness");
    let read_allowlist = format!(
        "{},{}",
        harness_dir.path().display(),
        Path::new(&res.bundle.path)
            .parent()
            .expect("bundle has parent")
            .display()
    );
    let output = std::process::Command::new(&real)
        .arg("run")
        .arg("--no-prompt")
        .arg(format!("--allow-read={read_allowlist}"))
        .arg(&harness_path)
        .arg(format!("file://{}", res.bundle.path))
        .output()
        .expect("spawn the real deno");
    assert!(
        output.status.success(),
        "the accepted fallback bundle must run: status {:?}; stderr={}; stdout={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim(),
        String::from_utf8_lossy(&output.stdout).trim()
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "type-only-ok",
        "the handler answers through the type-only subgraph worker"
    );
}
