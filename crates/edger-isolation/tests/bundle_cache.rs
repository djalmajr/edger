//! EDG-20: per-version bundle cache — single-flight, invalidation and the
//! per-process fallback, through the public API (real `deno` on PATH).
//!
//! Each test uses its own worker dir (the key includes the worker dir path)
//! and reads the process-wide counters as a DELTA, under a file-wide lock so
//! the parallel tests of this binary cannot interleave their outcomes.

#![cfg(feature = "multiproc")]

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use edger_isolation::{
    bundle_cache_stats, deno_version_for_candidates, worker_bundle_key, BundleCacheStats,
    DenoCliBundler,
};

/// The tests of this binary share the process-wide bundle counters: serialize
/// every test that asserts on a counter delta. A `tokio` mutex (not `std`) so
/// the guard may be held across the `await` points of the async tests, the
/// same pattern as `TEST_MUTATION_PLANE_LOCK` in the orchestrator deploy
/// tests.
static COUNTER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn counter_guard() -> tokio::sync::MutexGuard<'static, ()> {
    COUNTER_LOCK.lock().await
}

/// A small two-file worker whose entrypoint imports a relative module: the
/// multiproc spawn path must bundle it (`entry_needs_bundle` = true).
fn write_worker(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"import { greeting } from "./greeting.ts";
Deno.serve(() => new Response(greeting));
"#,
    )
    .unwrap();
    fs::write(
        dir.join("greeting.ts"),
        "export const greeting = 'hello-cache';\n",
    )
    .unwrap();
    dir
}

fn entrypoint(worker: &Path) -> PathBuf {
    worker.join("index.ts")
}

const SPAWN_TIMEOUT: Duration = Duration::from_secs(60);

/// Two CONCURRENT resolutions of the same version run the bundler ONCE: the
/// builder records a `miss`, the other spawn reuses the in-flight result (or
/// the published entry) and records a `hit`. A third sequential resolution
/// is a direct cache hit without any new build.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_bundle_requests_build_once() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "bundle-app");
    let fallback = tempfile::tempdir().unwrap();
    let bundler = Arc::new(DenoCliBundler::default());

    let before = bundle_cache_stats();
    let (res_a, res_b) = {
        let (a, b) = (Arc::clone(&bundler), Arc::clone(&bundler));
        let (worker_a, worker_b) = (worker.clone(), worker.clone());
        let (entry_a, entry_b) = (entrypoint(&worker), entrypoint(&worker));
        tokio::join!(
            a.resolve_bundle_entry(&worker_a, &entry_a, fallback.path(), SPAWN_TIMEOUT),
            b.resolve_bundle_entry(&worker_b, &entry_b, fallback.path(), SPAWN_TIMEOUT),
        )
    };
    let (res_a, res_b) = (res_a.unwrap(), res_b.unwrap());
    assert!(
        res_a.bundle.path.ends_with("index.bundle.mjs"),
        "the resolved entry is the bundle file: {}",
        res_a.bundle.path
    );
    assert_eq!(
        res_a.bundle.path, res_b.bundle.path,
        "both spawns share the SAME published bundle"
    );
    assert!(
        res_a.dir.is_none() && res_b.dir.is_none(),
        "the cache lives in the worker dir (no per-process tempdir)"
    );
    // The bundle is published inside the per-version cache dir.
    assert!(
        res_a.bundle.path.contains(&format!(
            "{}{}",
            std::path::MAIN_SEPARATOR,
            ".edger-bundle-cache"
        )),
        "the bundle path is under .edger-bundle-cache: {}",
        res_a.bundle.path
    );

    let after = bundle_cache_stats();
    assert_eq!(
        after.miss_total - before.miss_total,
        1,
        "the bundler ran exactly once for the two concurrent spawns"
    );
    assert_eq!(
        after.hit_total - before.hit_total,
        1,
        "the other spawn reused the build"
    );
    assert_eq!(after.error_total - before.error_total, 0);
    assert_eq!(after.uncached_total - before.uncached_total, 0);

    // A third, sequential resolution: direct cache hit, no new build.
    let entry_c = entrypoint(&worker);
    let res_c = bundler
        .resolve_bundle_entry(&worker, &entry_c, fallback.path(), SPAWN_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(res_c.bundle.path, res_a.bundle.path);
    let after_c = bundle_cache_stats();
    assert_eq!(
        after_c.miss_total - after.miss_total,
        0,
        "no rebuild for the hit"
    );
    assert_eq!(
        after_c.hit_total - after.hit_total,
        1,
        "the third spawn is a cache hit"
    );
}

/// A failed bundle is NOT cached (the next spawn retries) and counts in
/// `error`: a relative import that does not resolve makes `deno info`
/// reject the graph.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_bundle_is_not_cached_and_counts_error() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "broken-app");
    fs::write(
        worker.join("index.ts"),
        r#"import { missing } from "./does-not-exist.ts";
Deno.serve(() => new Response(missing));
"#,
    )
    .unwrap();
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::default();
    let entry = entrypoint(&worker);

    let before = bundle_cache_stats();
    let err = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .unwrap_err();
    let after = bundle_cache_stats();
    assert_eq!(
        after.error_total - before.error_total,
        1,
        "the failed build counts as error"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "no successful build"
    );
    // Nothing was published: the cache dir has no key dir for this worker.
    let cache = worker.join(".edger-bundle-cache");
    let published = if cache.is_dir() {
        std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .any(|e| !e.file_name().to_string_lossy().starts_with(".tmp-"))
    } else {
        false
    };
    assert!(!published, "a failed bundle must not be cached: {err}");

    // Retry semantics: fix the file and the NEXT spawn builds successfully
    // (the failure was not remembered).
    fs::write(
        worker.join("greeting.ts"),
        "export const greeting = 'fixed';\n",
    )
    .unwrap();
    fs::write(
        worker.join("index.ts"),
        r#"import { greeting } from "./greeting.ts";
Deno.serve(() => new Response(greeting));
"#,
    )
    .unwrap();
    let entry = entrypoint(&worker);
    let before_retry = bundle_cache_stats();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("the retry after the fix must succeed");
    let after_retry = bundle_cache_stats();
    assert_eq!(
        after_retry.miss_total - before_retry.miss_total,
        1,
        "the retry builds"
    );
    assert!(res.bundle.path.ends_with("index.bundle.mjs"));
}

/// A single-flight waiter whose bound (the worker's spawn timeout) expires
/// while the build is still running fails with a typed error and does NOT
/// count as a bundle outcome; the build itself still completes and publishes
/// (the next spawn is a hit).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_single_flight_wait_times_out_without_poisoning_the_build() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "slow-app");
    let fallback = tempfile::tempdir().unwrap();
    let bundler = Arc::new(DenoCliBundler::default());
    let (entry_a, entry_b) = (entrypoint(&worker), entrypoint(&worker));

    let before = bundle_cache_stats();
    let (a, b) = (Arc::clone(&bundler), Arc::clone(&bundler));
    let (worker_a, worker_b) = (worker.clone(), worker.clone());
    // One long bound, one that cannot cover a real `deno bundle`.
    let (res_long, res_short) = tokio::join!(
        a.resolve_bundle_entry(&worker_a, &entry_a, fallback.path(), SPAWN_TIMEOUT),
        b.resolve_bundle_entry(
            &worker_b,
            &entry_b,
            fallback.path(),
            Duration::from_millis(1)
        )
    );
    assert!(
        res_long.is_ok(),
        "the long-bounded spawn (builder or patient waiter) succeeds: {res_long:?}"
    );
    match &res_short {
        Ok(_) => {} // the 1 ms bound won the race against a fast build: fine
        Err(err) => assert_eq!(
            err.code, "DENO_BUNDLE_WAIT_TIMEOUT",
            "the impatient waiter fails with the typed wait error, got: {err}"
        ),
    }
    let after = bundle_cache_stats();
    assert_eq!(
        after.miss_total - before.miss_total,
        1,
        "the build ran exactly once"
    );
    // The build completed (and published) despite the waiter's timeout.
    let entry_c = entrypoint(&worker);
    bundler
        .resolve_bundle_entry(&worker, &entry_c, fallback.path(), SPAWN_TIMEOUT)
        .await
        .unwrap();
    let after_c = bundle_cache_stats();
    assert_eq!(
        after_c.hit_total - after.hit_total,
        1,
        "the published entry is reusable"
    );
    assert_eq!(
        after_c.miss_total - after.miss_total,
        0,
        "no rebuild: the timed-out waiter did not poison the build"
    );
}

/// Editing a source file IN PLACE invalidates the key: the next resolution
/// is a fresh `miss`, and the older key is pruned from the cache dir (one
/// key per version).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_place_edit_invalidates_the_key_and_prunes_the_old_one() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "edit-app");
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::default();
    let entry = entrypoint(&worker);

    let before = bundle_cache_stats();
    let first = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .unwrap();
    // The published key dir (exactly one).
    let cache = worker.join(".edger-bundle-cache");
    let key_dirs: Vec<String> = std::fs::read_dir(&cache)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with(".tmp-"))
        .collect();
    assert_eq!(key_dirs.len(), 1, "one key dir after the first build");

    // Edit the module in place (dev dirs and the WebIDE do exactly this).
    fs::write(
        worker.join("greeting.ts"),
        "export const greeting = 'edited-in-place';\n",
    )
    .unwrap();
    let entry = entrypoint(&worker);
    let second = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .unwrap();
    let after = bundle_cache_stats();
    assert_eq!(
        after.miss_total - before.miss_total,
        2,
        "edit => new key => new build"
    );
    assert_ne!(
        first.bundle.path, second.bundle.path,
        "the edited tree published a different key"
    );
    let key_dirs: Vec<String> = std::fs::read_dir(&cache)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with(".tmp-"))
        .collect();
    assert_eq!(key_dirs.len(), 1, "the older key was pruned: {key_dirs:?}");
    let surviving = std::path::Path::new(&second.bundle.path)
        .parent()
        .and_then(|parent| parent.file_name())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        key_dirs[0], surviving,
        "the surviving key is the edited one"
    );
}

/// A worker dir that is NOT writable keeps the legacy per-process behavior:
/// the bundle lands in a per-process tempdir (kept alive by `BundleEntry::dir`),
/// nothing is cached, and the outcome counts as `uncached`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unwritable_worker_dir_falls_back_to_per_process_bundle() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "readonly-app");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o555)).unwrap();
        // A real non-writable check (root would still be able to write).
        let probe = worker.join(".write-probe");
        if fs::write(&probe, b"x").is_ok() {
            let _ = fs::remove_file(&probe);
            eprintln!("skipping unwritable_worker_dir_falls_back_to_per_process_bundle: running with write-through permissions (root)");
            return;
        }
    }
    let fallback = tempfile::tempdir().unwrap();
    let bundler = DenoCliBundler::default();
    let entry = entrypoint(&worker);

    let before = bundle_cache_stats();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("the fallback path bundles as before");
    let after = bundle_cache_stats();
    assert!(
        res.dir.is_some(),
        "the fallback keeps a per-process tempdir for the bundle"
    );
    assert!(
        res.bundle.path.contains(fallback.path().to_str().unwrap())
            || res.bundle.path.starts_with(worker.to_str().unwrap()),
        "the fallback bundle lives in a tempdir: {}",
        res.bundle.path
    );
    assert!(
        !res.bundle.path.contains(".edger-bundle-cache"),
        "the fallback must not touch the cache dir: {}",
        res.bundle.path
    );
    assert_eq!(
        after.uncached_total - before.uncached_total,
        1,
        "counts as uncached"
    );
    assert_eq!(
        after.miss_total - before.miss_total,
        0,
        "nothing was cached"
    );
    assert!(
        !worker.join(".edger-bundle-cache").exists(),
        "no cache dir was created in the unwritable worker dir"
    );
    // Restore permissions so the tempdir cleanup can remove the worker dir.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// `bundle_cache_stats` is a plain snapshot of the process-wide counters
/// (no I/O): repeated reads are stable without any bundle activity.
#[test]
fn bundle_cache_stats_snapshot_is_stable() {
    let a = bundle_cache_stats();
    let b = bundle_cache_stats();
    assert_eq!(a, b);
    let _ = BundleCacheStats::default();
}

/// A recursive snapshot of a tree: every regular file as (relative path,
/// contents), sorted — used to prove a tree is byte-for-byte unchanged.
fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_dir() {
                walk(&path, root, out);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    walk(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A `.edger-bundle-cache` that is a SYMLINK to a sibling directory is never
/// followed or modified: the resolution takes the per-process fallback
/// (counted `uncached`), the victim tree is byte-for-byte unchanged, no
/// `v1-*` key appears in it, and the symlink itself survives.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn symlinked_cache_dir_is_never_followed_and_falls_back() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    // The victim: a sibling dir with a subdirectory and a file.
    let victim = root.path().join("victim");
    fs::create_dir_all(victim.join("inner")).unwrap();
    fs::write(victim.join("KEEP_ME.txt"), b"keep-me").unwrap();
    fs::write(victim.join("inner").join("nested.txt"), b"nested").unwrap();
    // A real worker that needs bundling, whose cache root is a symlink
    // OUTSIDE it (a RUNTIME_WORKER_DIRS checkout or out-of-band copy can
    // hold this).
    let worker = write_worker(root.path(), "link-worker");
    std::os::unix::fs::symlink(&victim, worker.join(".edger-bundle-cache")).unwrap();

    let before = snapshot_tree(&victim);
    let stats_before = bundle_cache_stats();
    let bundler = DenoCliBundler::default();
    let entry = entrypoint(&worker);
    let fallback = tempfile::tempdir().unwrap();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("a symlinked cache dir falls back to the per-process bundle");
    let stats_after = bundle_cache_stats();

    assert!(
        res.dir.is_some(),
        "the fallback keeps a per-process tempdir: {}",
        res.bundle.path
    );
    assert!(
        !res.bundle.path.contains(".edger-bundle-cache"),
        "the fallback bundle does not go through the cache dir: {}",
        res.bundle.path
    );
    assert_eq!(
        stats_after.uncached_total - stats_before.uncached_total,
        1,
        "the fallback counts as uncached"
    );
    assert_eq!(
        stats_after.miss_total - stats_before.miss_total,
        0,
        "nothing was published into the cache"
    );
    // The victim tree is byte-for-byte unchanged; no v1-* appeared in it.
    let after = snapshot_tree(&victim);
    assert_eq!(
        before, after,
        "the symlink target is byte-for-byte unchanged"
    );
    for (rel, _bytes) in &after {
        assert!(
            !rel.starts_with("v1-"),
            "a cache key appeared in the victim: {rel}"
        );
    }
    assert!(
        std::fs::symlink_metadata(worker.join(".edger-bundle-cache"))
            .unwrap()
            .is_symlink(),
        "the symlink itself was not replaced or deleted"
    );
}

/// A SELF symlink (`.edger-bundle-cache -> .`) would let a prune delete the
/// worker's own directory children: the probe refuses it, the worker dir is
/// left intact, and the per-process fallback is used.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn self_symlinked_cache_dir_keeps_the_worker_dir() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "self-link-worker");
    // A real subdirectory that a self-linked prune would delete.
    fs::create_dir_all(worker.join("src")).unwrap();
    fs::write(worker.join("src").join("lib.ts"), "export const lib = 1;\n").unwrap();
    std::os::unix::fs::symlink(".", worker.join(".edger-bundle-cache")).unwrap();

    let stats_before = bundle_cache_stats();
    let bundler = DenoCliBundler::default();
    let entry = entrypoint(&worker);
    let fallback = tempfile::tempdir().unwrap();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("a self-linked cache dir falls back to the per-process bundle");
    let stats_after = bundle_cache_stats();

    assert!(
        res.dir.is_some(),
        "the fallback keeps a per-process tempdir: {}",
        res.bundle.path
    );
    assert_eq!(
        stats_after.uncached_total - stats_before.uncached_total,
        1,
        "the fallback counts as uncached"
    );
    // The worker's own tree survives the self-link.
    assert!(
        worker.join("src").is_dir(),
        "the worker's own subdirectory survives"
    );
    assert_eq!(
        fs::read_to_string(worker.join("src").join("lib.ts")).unwrap(),
        "export const lib = 1;\n"
    );
    assert!(worker.join("index.ts").exists());
    assert!(worker.join("greeting.ts").exists());
}

/// A planted SYMLINK at the exact published bundle path (pointing outside
/// the worker dir) is never adopted or executed: the build replaces it with
/// the real published file, and the outside target is left alone.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planted_symlink_bundle_file_is_not_used() {
    let _guard = counter_guard().await;
    let root = tempfile::tempdir().unwrap();
    let worker = write_worker(root.path(), "planted-worker");
    // The exact key the spawn path computes for this worker dir.
    let bundler = DenoCliBundler::default();
    let deno_version = deno_version_for_candidates(&bundler.executable_candidates());
    let key = worker_bundle_key(&worker, &deno_version).unwrap();
    // A file OUTSIDE the worker dir, planted as the published bundle.
    let outside = root.path().join("outside-worker.mjs");
    fs::write(&outside, b"/* evil */").unwrap();
    let key_dir = worker.join(".edger-bundle-cache").join(&key);
    fs::create_dir_all(&key_dir).unwrap();
    std::os::unix::fs::symlink(&outside, key_dir.join("index.bundle.mjs")).unwrap();

    let stats_before = bundle_cache_stats();
    let entry = entrypoint(&worker);
    let fallback = tempfile::tempdir().unwrap();
    let res = bundler
        .resolve_bundle_entry(&worker, &entry, fallback.path(), SPAWN_TIMEOUT)
        .await
        .expect("a planted symlink is not a usable bundle, but the spawn succeeds");
    let stats_after = bundle_cache_stats();

    assert_eq!(
        fs::read(&outside).unwrap(),
        b"/* evil */".to_vec(),
        "the outside target was not modified"
    );
    assert!(
        res.bundle.path != outside.to_string_lossy(),
        "the resolved bundle is not the planted symlink target"
    );
    // The resolved bundle is the REAL published file: a regular file (not a
    // symlink) inside the worker's own cache dir.
    let bundle_meta = std::fs::symlink_metadata(Path::new(&res.bundle.path)).unwrap();
    assert!(
        bundle_meta.is_file() && !bundle_meta.is_symlink(),
        "the resolved bundle is a regular file: {}",
        res.bundle.path
    );
    assert!(
        res.bundle.path.contains(".edger-bundle-cache"),
        "the bundle is the cache build: {}",
        res.bundle.path
    );
    assert!(
        res.dir.is_none(),
        "the cache is usable: no per-process tempdir"
    );
    assert_eq!(
        stats_after.miss_total - stats_before.miss_total,
        1,
        "the build ran (the planted entry was not adopted)"
    );
    assert_eq!(
        stats_after.hit_total - stats_before.hit_total,
        0,
        "the planted symlink was never adopted as a hit"
    );
}
