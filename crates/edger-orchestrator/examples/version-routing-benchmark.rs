//! Version-routing microbenchmark (EXECUTED 2026-10-01 under the
//! orchestrator's execution amendment; re-run with a fresh release build,
//! commands below).
//!
//! Measures the isolated cost of route selection
//! (`edger_orchestrator::router::resolve_route`, public API, no production
//! change) for ONE app with V installed versions (V = 1, 10, 100, 1000 by
//! default), in three scenarios per V:
//!
//! * `unpinned-latest`  — unversioned `/bench-app` with NO default pointer:
//!   the `default_versions` lookup misses and the semver fallback picks the
//!   highest of the V versions (honest name: this is the LATEST fallback,
//!   not an "explicit default").
//! * `pinned`           — exact `/bench-app@1.0.<highest>`: exact-match scan
//!   over the V versions (worst case: the target is the last entry).
//! * `default-explicit` — unversioned `/bench-app` with a PERSISTED default
//!   pointer (chosen via the public loader, not a private API): a temp
//!   fixture OUTSIDE the repo carries worker manifests plus
//!   `.edger-defaults/<hex(name)>.json` = `{"name","version"}` and
//!   `load_manifests_from_roots` restores the pointer. The pointer is set to
//!   `1.0.0`, a NON-highest version whenever V > 1, proving the explicit
//!   default is honored (the unpinned route must resolve 1.0.0, not the
//!   highest). All disk I/O (fixture creation, load, cleanup) happens OUTSIDE
//!   the timed region.
//!
//! Plugin cost is measured in dedicated scenarios so it is never attributed
//! to the app path (and vice versa):
//!
//! * The V scenarios are each repeated with and without a registered plugin
//!   base (`/benchplugin`, single version): `plugin_for_path` runs on every
//!   app resolve, so this isolates the plugin-scan overhead on the app hot
//!   path.
//! * `plugin-base` / `plugin-deep` (P = 1, 10, 100 by default): ONE plugin
//!   app with the SAME base and P versions (its own index, separate from the
//!   V app and the path-worker scenarios). `plugin_for_path` matches the base
//!   and `resolve_plugin_worker` scans the P-version entry bucket.
//!
//! Contract details verified in code (this tree):
//! * `plugin_for_path` (manifest_index_stub.rs) returns the remainder WITHOUT
//!   a leading slash: base-only path -> `""`, deep path -> `"deep"`.
//! * `resolve_route` returns `PluginBase { plugin: Box<PluginRef>, remainder }`.
//! * Unversioned resolution: `default_versions` pointer (when the pointed
//!   version is enabled/public/non-staged), else `resolve_semver(available,
//!   None)` = highest semver.
//! * Persisted pointer file: `<user_root>/.edger-defaults/<hex-of-name-bytes>.json`
//!   containing `{"name": ..., "version": ...}` (manifest_loader.rs).
//!
//! Methodology:
//! * `std::hint::black_box` on inputs AND results; the result of
//!   `black_box(...)` is consumed with `let _ =` so the `Result`'s
//!   `#[must_use]` never fires and the call cannot be elided;
//! * fixed warmup (excluded from stats), then N samples x R repetitions
//!   (defaults: warmup 2000, samples 10000, reps 3; the large cases
//!   V = 1000 / P = 100 use samples 2000 so the whole run stays well under
//!   3 minutes);
//! * run under `--release` (reproducible; `--help` exits without measuring):
//!   `CARGO_TARGET_DIR=/tmp/edger-version-release-target-20261001 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo +1.98.0 build --release -p edger-orchestrator --bin edger --example version-routing-benchmark`
//!   `VRB_VERSIONS=1,10,100,1000 VRB_PLUGINS=1,10,100 VRB_SAMPLES=10000 VRB_SAMPLES_BIG=2000 VRB_REPS=3 VRB_WARMUP=2000 VRB_OUT=/tmp/edger-version-bench-results-20261001/routing.json /tmp/edger-version-release-target-20261001/release/examples/version-routing-benchmark`
//! * per-sample wall timing via `Instant` — units are nanoseconds per call
//!   (each sample includes ~2 clock reads; the bias is uniform across
//!   scenarios, so comparisons are valid);
//! * p50/p95/p99 nearest-rank over ALL samples of the scenario;
//! * correctness verification pass OUTSIDE the timed loops: every pinned
//!   version resolves to itself, unpinned resolves to the expected version
//!   (highest without pointer; 1.0.0 with the explicit pointer, which is
//!   asserted to be non-highest when V > 1), plugin remainders are `""`/
//!   `"deep"`, and negative cases (missing version, unknown app, reserved
//!   path) behave.
//!
//! Constraints honored:
//! * No new dependencies (std + the crate's existing public API only:
//!   `router::resolve_route`, `manifest_index_stub::ManifestIndex`,
//!   `manifest_loader::load_manifests_from_roots`, `edger_core` types).
//! * No production code changed and no public API added for the benchmark.
//! * Fixtures live under the OS temp dir (never inside the repo) and every
//!   temp fixture is removed on exit, even on error.
//! * The results file is written under /tmp by default (VRB_OUT), not into
//!   the repo.
//!
//! Env overrides (defaults fixed for reproducibility):
//!   VRB_VERSIONS=1,10,100,1000  VRB_PLUGINS=1,10,100
//!   VRB_SAMPLES=10000  VRB_SAMPLES_BIG=2000  VRB_REPS=3  VRB_WARMUP=2000
//!   VRB_OUT=/tmp/version-routing-benchmark.json

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use edger_core::{WorkerManifest, WorkerOrigin};
use edger_orchestrator::manifest_index_stub::ManifestIndex;
use edger_orchestrator::manifest_loader::load_manifests_from_roots;
use edger_orchestrator::router::{resolve_route, ResolvedRoute};

const APP_NAME: &str = "bench-app";
const PLUGIN_NAME: &str = "bench-plugin";
const PLUGIN_BASE: &str = "/benchplugin";
/// Version the persisted pointer targets: non-highest whenever V > 1.
const EXPLICIT_DEFAULT_VERSION: &str = "1.0.0";

fn version_str(i: usize) -> String {
    format!("1.0.{i}")
}

/// Outcome of a single resolution, reduced to the fields the benchmark and
/// the verification need.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Worker {
        name: String,
        version: String,
        pinned: bool,
    },
    Reserved,
    Plugin {
        version: String,
        remainder: String,
    },
    Homepage,
}

fn resolve(index: &ManifestIndex, path: &str) -> Result<Outcome, String> {
    match resolve_route(path, None, index) {
        Ok(ResolvedRoute::Worker {
            worker,
            version_pinned,
            ..
        }) => Ok(Outcome::Worker {
            name: worker.name,
            version: worker.version,
            pinned: version_pinned,
        }),
        Ok(ResolvedRoute::Reserved { .. }) => Ok(Outcome::Reserved),
        Ok(ResolvedRoute::PluginBase {
            plugin, remainder, ..
        }) => Ok(Outcome::Plugin {
            version: plugin.manifest.version.clone().unwrap_or_default(),
            remainder,
        }),
        Ok(ResolvedRoute::HomepageFallback { .. }) => Ok(Outcome::Homepage),
        Err(err) => Err(err.message),
    }
}

/// Build an in-memory index with `versions` versions of `bench-app` (plus one
/// `bench-plugin` with base `/benchplugin` when `with_plugin`) and return the
/// (index, versions) pair. The dirs do not need to exist: the staged marker
/// check tolerates a missing marker file. No disk I/O.
fn build_index(versions: usize, with_plugin: bool) -> Result<(ManifestIndex, Vec<String>), String> {
    let mut index = ManifestIndex::new();
    let versions_list: Vec<String> = (0..versions).map(version_str).collect();
    for version in &versions_list {
        let dir = PathBuf::from(format!("bench-fixture/{APP_NAME}@{version}"));
        let manifest = WorkerManifest {
            name: APP_NAME.to_string(),
            version: Some(version.clone()),
            ..Default::default()
        };
        index
            .insert_with_origin(dir, manifest, WorkerOrigin::User)
            .map_err(|err| err.message)?;
    }
    if with_plugin {
        let dir = PathBuf::from(format!("bench-fixture/{PLUGIN_NAME}"));
        let manifest = WorkerManifest {
            name: PLUGIN_NAME.to_string(),
            version: Some("1.0.0".to_string()),
            base: Some(PLUGIN_BASE.to_string()),
            ..Default::default()
        };
        index
            .insert_with_origin(dir, manifest, WorkerOrigin::User)
            .map_err(|err| err.message)?;
    }
    Ok((index, versions_list))
}

/// Build an in-memory index with `plugin_versions` versions of the plugin app
/// (same name, SAME base, its own bucket). No disk I/O.
fn build_plugin_index(plugin_versions: usize) -> Result<(ManifestIndex, Vec<String>), String> {
    let mut index = ManifestIndex::new();
    let versions_list: Vec<String> = (0..plugin_versions).map(version_str).collect();
    for version in &versions_list {
        let dir = PathBuf::from(format!("bench-fixture/{PLUGIN_NAME}@{version}"));
        let manifest = WorkerManifest {
            name: PLUGIN_NAME.to_string(),
            version: Some(version.clone()),
            base: Some(PLUGIN_BASE.to_string()),
            ..Default::default()
        };
        index
            .insert_with_origin(dir, manifest, WorkerOrigin::User)
            .map_err(|err| err.message)?;
    }
    Ok((index, versions_list))
}

/// Same file naming the loader uses for persisted default pointers
/// (manifest_loader.rs, private helper mirrored here so the fixture matches
/// the real contract): hex of the name bytes + ".json".
fn pointer_file_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len() * 2 + 5);
    for byte in name.bytes() {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded.push_str(".json");
    encoded
}

/// Temp fixture for the explicit-default scenario, OUTSIDE the repo.
/// Removed on drop, even on error.
struct TempFixture {
    root: PathBuf,
    workers_root: PathBuf,
}

impl TempFixture {
    fn new(tag: &str) -> std::io::Result<Self> {
        let root = std::env::temp_dir().join(format!("edger-vrb-{tag}-{}", std::process::id()));
        let workers_root = root.join("workers");
        std::fs::create_dir_all(&workers_root)?;
        Ok(Self { root, workers_root })
    }

    fn write_worker(&self, i: usize) -> std::io::Result<()> {
        let version = version_str(i);
        let dir = self.workers_root.join(format!("{APP_NAME}-{i:03}"));
        std::fs::create_dir_all(&dir)?;
        let manifest = format!("name: {APP_NAME}\nversion: \"{version}\"\nkind: fetch\n");
        std::fs::write(dir.join("manifest.yaml"), manifest)
    }

    fn write_pointer(&self, name: &str, version: &str) -> std::io::Result<()> {
        let dir = self.workers_root.join(".edger-defaults");
        std::fs::create_dir_all(&dir)?;
        let payload = format!("{{\"name\": \"{name}\", \"version\": \"{version}\"}}\n");
        std::fs::write(dir.join(pointer_file_name(name)), payload)
    }

    fn load_index(&self) -> Result<ManifestIndex, String> {
        load_manifests_from_roots(&[], None, std::slice::from_ref(&self.workers_root))
            .map_err(|err| err.message)
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_worker(
    index: &ManifestIndex,
    path: &str,
    expected_name: &str,
    expected_version: &str,
    expected_pinned: bool,
) -> Result<(), String> {
    match resolve(index, path) {
        Ok(Outcome::Worker {
            name,
            version,
            pinned,
        }) if name == expected_name && version == expected_version && pinned == expected_pinned => {
            Ok(())
        }
        other => Err(format!(
            "verify: {path} resolved to {other:?}, expected Worker {expected_name}@{expected_version} pinned={expected_pinned}"
        )),
    }
}

/// Verification for the V-app scenarios (pointerless index). Panics with
/// context on any mismatch. Run OUTSIDE the timed loops.
fn verify_app(index: &ManifestIndex, versions: &[String], with_plugin: bool) -> usize {
    let mut pinned_ok = 0usize;
    for version in versions {
        let path = format!("/{APP_NAME}@{version}");
        assert_worker(index, &path, APP_NAME, version, true).unwrap_or_else(|err| panic!("{err}"));
        pinned_ok += 1;
    }
    let highest = versions.last().expect("at least one version");
    // No pointer: the unversioned route must fall back to the highest.
    assert_worker(index, "/bench-app", APP_NAME, highest, false)
        .unwrap_or_else(|err| panic!("{err}"));
    // Negative cases: missing version and unknown app must be rejected, not
    // silently mapped to another version.
    assert!(
        resolve(index, &format!("{APP_NAME}@9.9.9")).is_err(),
        "verify: unknown version must be rejected"
    );
    assert!(
        resolve(index, "/no-such-app").is_err(),
        "verify: unknown app must be rejected"
    );
    // Reserved platform paths must stay reserved regardless of V.
    assert!(
        matches!(resolve(index, "/health"), Ok(Outcome::Reserved)),
        "verify: /health must stay reserved"
    );
    if with_plugin {
        verify_plugin_common(index, "1.0.0");
        // The plugin must not hijack the worker route.
        assert_worker(
            index,
            &format!("{APP_NAME}@{highest}"),
            APP_NAME,
            highest,
            true,
        )
        .unwrap_or_else(|err| panic!("{err}"));
    }
    pinned_ok
}

/// Shared plugin verification: base -> remainder "" (no leading slash),
/// deep -> remainder "deep". Panics on mismatch.
fn verify_plugin_common(index: &ManifestIndex, expected_version: &str) {
    match resolve(index, PLUGIN_BASE) {
        Ok(Outcome::Plugin {
            version,
            remainder,
        }) if version == expected_version && remainder.is_empty() => {}
        other => panic!(
            "verify: plugin base {PLUGIN_BASE} resolved to {other:?}, expected plugin@{expected_version} remainder=\"\""
        ),
    }
    match resolve(index, &format!("{PLUGIN_BASE}/deep")) {
        Ok(Outcome::Plugin {
            version,
            remainder,
        }) if version == expected_version && remainder == "deep" => {}
        other => panic!(
            "verify: plugin deep {PLUGIN_BASE}/deep resolved to {other:?}, expected plugin@{expected_version} remainder=\"deep\""
        ),
    }
}

/// Verification for the plugin P scenarios. Returns the version the plugin
/// route resolved to (dir-bound: the first inserted version, not the highest)
/// so the report can state the real behavior.
fn verify_plugin(index: &ManifestIndex, versions: &[String]) -> String {
    // plugin_for_path returns the first matching plugin ref; entries are
    // inserted in ascending version order and the sort is stable for equal
    // base lengths, so the route binds to the first inserted version.
    let bound = versions.first().expect("at least one plugin version");
    verify_plugin_common(index, bound);
    assert!(
        resolve(index, "/no-such-app").is_err(),
        "verify: unknown app must be rejected"
    );
    assert!(
        matches!(resolve(index, "/health"), Ok(Outcome::Reserved)),
        "verify: /health must stay reserved"
    );
    bound.clone()
}

/// Verification for the explicit-default scenario: the persisted pointer
/// (1.0.0) must be restored, and when V > 1 it must NOT be the highest —
/// that is the proof the explicit default is honored over the latest
/// fallback.
fn verify_explicit_default(index: &ManifestIndex, versions: &[String]) -> Result<bool, String> {
    let pointer = index.default_version(APP_NAME).ok_or_else(|| {
        "verify: default_version pointer missing after loader restore".to_string()
    })?;
    if pointer != EXPLICIT_DEFAULT_VERSION {
        return Err(format!(
            "verify: pointer is {pointer}, expected {EXPLICIT_DEFAULT_VERSION}"
        ));
    }
    let highest = versions.last().expect("at least one version");
    let non_highest = pointer != *highest;
    // The unversioned route must serve the pointed version.
    assert_worker(index, "/bench-app", APP_NAME, &pointer, false).map_err(|err| err.to_string())?;
    // Pinned routes are unaffected by the pointer.
    assert_worker(
        index,
        &format!("{APP_NAME}@{highest}"),
        APP_NAME,
        highest,
        true,
    )
    .map_err(|err| err.to_string())?;
    Ok(non_highest)
}

/// Timed measurement: warmup (excluded), then `reps` x `samples` single-call
/// timings in ns. Inputs and results are black-boxed; the black-boxed result
/// is consumed with `let _ =` so `#[must_use]` on `Result` never fires.
fn measure(
    index: &ManifestIndex,
    path: &str,
    warmup: usize,
    samples: usize,
    reps: usize,
) -> Vec<u64> {
    for _ in 0..warmup {
        let _ = black_box(resolve(black_box(index), black_box(path)));
    }
    let mut out = Vec::with_capacity(samples * reps);
    for _ in 0..reps {
        for _ in 0..samples {
            let start = Instant::now();
            let _ = black_box(resolve(black_box(index), black_box(path)));
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            out.push(elapsed_ns);
        }
    }
    out
}

fn percentile_nearest_rank(sorted: &[u64], p: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p as f64 / 100.0 * sorted.len() as f64).ceil() as usize;
    let idx = rank.min(sorted.len()).max(1);
    sorted[idx - 1]
}

fn stats_json(samples_ns: &[u64]) -> serde_json::Value {
    let mut sorted: Vec<u64> = samples_ns.to_vec();
    sorted.sort_unstable();
    let sum: u128 = sorted.iter().map(|value| *value as u128).sum();
    let mean_ns = sum as f64 / sorted.len() as f64;
    let p50 = percentile_nearest_rank(&sorted, 50);
    let p95 = percentile_nearest_rank(&sorted, 95);
    let p99 = percentile_nearest_rank(&sorted, 99);
    serde_json::json!({
        "min_ns": sorted.first().copied().unwrap_or(0),
        "p50_ns": p50,
        "p95_ns": p95,
        "p99_ns": p99,
        "max_ns": sorted.last().copied().unwrap_or(0),
        "mean_ns": mean_ns,
        "p50_us": p50 as f64 / 1000.0,
        "p95_us": p95 as f64 / 1000.0,
        "p99_us": p99 as f64 / 1000.0,
    })
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .filter(|value: &usize| *value > 0)
        .unwrap_or(default)
}

fn env_list(name: &str, default: Vec<usize>) -> Vec<usize> {
    let raw = match std::env::var(name) {
        Ok(raw) if !raw.trim().is_empty() => raw,
        _ => return default,
    };
    let parsed: Vec<usize> = raw
        .split(',')
        .filter_map(|part| part.trim().parse().ok())
        .collect();
    if parsed.is_empty() {
        default
    } else {
        parsed
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("Usage: version-routing-benchmark [--help]\nConfiguration: VRB_VERSIONS, VRB_PLUGINS, VRB_SAMPLES, VRB_SAMPLES_BIG, VRB_REPS, VRB_WARMUP, VRB_OUT. Run without arguments to measure.");
        return;
    }
    if !args.is_empty() {
        eprintln!("Unknown arguments; use --help. No measurements were run.");
        std::process::exit(2);
    }
    let versions_list = env_list("VRB_VERSIONS", vec![1, 10, 100, 1000]);
    let plugins_list = env_list("VRB_PLUGINS", vec![1, 10, 100]);
    let samples = env_usize("VRB_SAMPLES", 10_000);
    let samples_big = env_usize("VRB_SAMPLES_BIG", 2_000);
    let reps = env_usize("VRB_REPS", 3);
    let warmup = env_usize("VRB_WARMUP", 2_000);
    let out_path =
        std::env::var("VRB_OUT").unwrap_or_else(|_| "/tmp/version-routing-benchmark.json".into());

    eprintln!(
        "version-routing microbenchmark: versions={versions_list:?} plugins={plugins_list:?} samples={samples} samples_big={samples_big} reps={reps} warmup={warmup} (run under --release)"
    );

    let mut scenarios = Vec::new();

    // V-app scenarios: unpinned-latest, pinned, default-explicit.
    for versions in &versions_list {
        let big = *versions >= 1000;
        let samples_this = if big { samples_big } else { samples };
        let warmup_this = if big { warmup.min(1_000) } else { warmup };

        for with_plugin in [false, true] {
            let (index, versions_built) =
                build_index(*versions, with_plugin).unwrap_or_else(|err| {
                    panic!("build_index failed (V={versions}, plugin={with_plugin}): {err}")
                });
            let pinned_ok = verify_app(&index, &versions_built, with_plugin);
            let highest = version_str(versions - 1);

            for (route, path) in [
                ("unpinned-latest", "/bench-app".to_string()),
                ("pinned", format!("/{APP_NAME}@{highest}")),
            ] {
                let path_str = path.as_str();
                let samples_ns = measure(&index, path_str, warmup_this, samples_this, reps);
                let stats = stats_json(&samples_ns);
                scenarios.push(serde_json::json!({
                    "scenario": "v-app",
                    "route": route,
                    "v": versions,
                    "plugin_base": with_plugin,
                    "path": path_str,
                    "pinned_target": if route == "pinned" { Some(highest.clone()) } else { None },
                    "default_pointer": serde_json::Value::Null,
                    "samples_per_rep": samples_this,
                    "reps": reps,
                    "total_samples": samples_this * reps,
                    "pinned_versions_verified": pinned_ok,
                    "stats_ns": stats,
                }));
                eprintln!(
                    "V={versions:<4} route={route:<15} plugin={with_plugin:<5} p50={}ns p95={}ns p99={}ns (n={})",
                    stats["p50_ns"],
                    stats["p95_ns"],
                    stats["p99_ns"],
                    samples_ns.len()
                );
            }
        }

        // default-explicit: public loader + persisted pointer (disk I/O
        // strictly outside the timed region). The fixture dir is per-V so a
        // non-ascending VRB_VERSIONS list cannot leave stale worker dirs
        // from a previous iteration.
        let fixture = TempFixture::new(&format!("default-explicit-{versions}"))
            .unwrap_or_else(|err| panic!("temp fixture creation failed (V={versions}): {err}"));
        for i in 0..*versions {
            if let Err(err) = fixture.write_worker(i) {
                panic!("fixture write failed (V={versions}, i={i}): {err}");
            }
        }
        if let Err(err) = fixture.write_pointer(APP_NAME, EXPLICIT_DEFAULT_VERSION) {
            panic!("pointer write failed (V={versions}): {err}");
        }
        let index = fixture
            .load_index()
            .unwrap_or_else(|err| panic!("load_manifests_from_roots failed (V={versions}): {err}"));
        let versions_built: Vec<String> = (0..*versions).map(version_str).collect();
        let non_highest =
            verify_explicit_default(&index, &versions_built).unwrap_or_else(|err| panic!("{err}"));
        let samples_ns = measure(&index, "/bench-app", warmup_this, samples_this, reps);
        let stats = stats_json(&samples_ns);
        scenarios.push(serde_json::json!({
            "scenario": "v-app",
            "route": "default-explicit",
            "v": versions,
            "plugin_base": false,
            "path": "/bench-app",
            "pinned_target": serde_json::Value::Null,
            "default_pointer": EXPLICIT_DEFAULT_VERSION,
            "pointer_non_highest": non_highest,
            "samples_per_rep": samples_this,
            "reps": reps,
            "total_samples": samples_ns.len(),
            "pinned_versions_verified": versions_built.len(),
            "stats_ns": stats,
        }));
        eprintln!(
            "V={versions:<4} route={:<15} plugin=false p50={}ns p95={}ns p99={}ns (n={}, pointer={} non_highest={non_highest})",
            "default-explicit",
            stats["p50_ns"],
            stats["p95_ns"],
            stats["p99_ns"],
            samples_ns.len(),
            EXPLICIT_DEFAULT_VERSION
        );
    }

    // Plugin P scenarios: same base, P versions, dedicated index.
    for plugins in &plugins_list {
        let big = *plugins >= 100;
        let samples_this = if big { samples_big } else { samples };
        let warmup_this = if big { warmup.min(1_000) } else { warmup };
        let (mut index, versions_built) = build_plugin_index(*plugins)
            .unwrap_or_else(|err| panic!("build_plugin_index failed (P={plugins}): {err}"));
        let bound_version = verify_plugin(&index, &versions_built);
        // An unrelated worker route must scan every plugin candidate before
        // resolving its own bucket; matching the first base does not measure it.
        index
            .insert_with_origin(
                PathBuf::from("bench-fixture/unrelated-worker"),
                WorkerManifest {
                    name: APP_NAME.to_string(),
                    version: Some("1.0.0".to_string()),
                    ..Default::default()
                },
                WorkerOrigin::User,
            )
            .expect("insert unrelated worker");
        assert_worker(&index, "/bench-app@1.0.0", APP_NAME, "1.0.0", true)
            .expect("unrelated worker must resolve through the plugin scan");
        for (route, path) in [
            ("plugin-base", PLUGIN_BASE.to_string()),
            ("plugin-deep", format!("{PLUGIN_BASE}/deep")),
            ("worker-past-plugins", "/bench-app@1.0.0".to_string()),
        ] {
            let path_str = path.as_str();
            let samples_ns = measure(&index, path_str, warmup_this, samples_this, reps);
            let stats = stats_json(&samples_ns);
            scenarios.push(serde_json::json!({
                "scenario": "plugin",
                "route": route,
                "p": plugins,
                "plugin_base": PLUGIN_BASE,
                "path": path_str,
                "plugin_versions_bound": bound_version,
                "samples_per_rep": samples_this,
                "reps": reps,
                "total_samples": samples_ns.len(),
                "stats_ns": stats,
            }));
            eprintln!(
                "P={plugins:<4} route={route:<12} bound={bound_version:<8} p50={}ns p95={}ns p99={}ns (n={})",
                stats["p50_ns"],
                stats["p95_ns"],
                stats["p99_ns"],
                samples_ns.len()
            );
        }
    }

    let result = serde_json::json!({
        "benchmark": "version-routing-micro",
        "app": APP_NAME,
        "plugin": PLUGIN_NAME,
        "plugin_base": PLUGIN_BASE,
        "explicit_default_pointer": EXPLICIT_DEFAULT_VERSION,
        "entry_point": "edger_orchestrator::router::resolve_route (public API, no production change)",
        "config": {
            "versions": versions_list,
            "plugins": plugins_list,
            "samples_per_rep_small": samples,
            "samples_per_rep_big": samples_big,
            "reps": reps,
            "warmup": warmup,
            "units": "nanoseconds per call, wall clock (Instant), release profile expected",
            "notes": [
                "unpinned-latest = NO pointer: default_versions lookup miss + highest-semver fallback",
                "default-explicit = persisted pointer via public load_manifests_from_roots; pointer is 1.0.0 (non-highest when V > 1)",
                "plugin remainders have no leading slash: base -> \"\", deep -> \"deep\"",
                "plugin routes bind to the first inserted version (dir-bound), not the highest",
                "fixtures under the OS temp dir, removed on exit; disk I/O outside the timed region",
            ],
        },
        "scenarios": scenarios,
    });
    let mut text = serde_json::to_string_pretty(&result).expect("json serialize");
    text.push('\n');
    std::fs::write(&out_path, &text)
        .unwrap_or_else(|err| panic!("failed to write {out_path}: {err}"));
    eprintln!("wrote {out_path}");
}
