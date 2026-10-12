//! Deno module bundling hooks (Edge Runtime `eszip_trait` alignment).
//!
//! The eszip/precompiled artifact-loading surface (`BundleFormat::Eszip` /
//! `Precompiled`, `ModuleBundler::load_eszip` / `load_precompiled`,
//! `load_existing_artifact`, and the `DenoCliBundler::new` constructor) mirrors
//! the Edge Runtime eszip trait for a planned artifact-cache integration. It is
//! deliberate API-shape scaffolding not yet wired to a caller, so allow the
//! resulting dead-code here rather than collapsing the aligned surface.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use edger_core::IsolationError;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::watch;

/// Loaded module bundle metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBundle {
    pub path: String,
    pub format: BundleFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    Eszip,
    JavaScript,
    Precompiled,
}

/// Loads or builds worker module artifacts.
pub trait ModuleBundler: Send + Sync {
    fn bundle_entrypoint(
        &self,
        worker_dir: &Path,
        entrypoint: &Path,
        output_dir: &Path,
    ) -> Result<ModuleBundle, IsolationError>;

    fn load_eszip(&self, path: &str) -> Result<ModuleBundle, IsolationError>;
    fn load_precompiled(&self, path: &str) -> Result<ModuleBundle, IsolationError>;
}

/// Bundles a worker entrypoint into one ESM artifact through the Deno CLI.
#[derive(Debug, Clone, Default)]
pub struct DenoCliBundler {
    executable: Option<String>,
}

impl DenoCliBundler {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: Some(executable.into()),
        }
    }

    pub fn executable_candidates(&self) -> Vec<String> {
        if let Some(executable) = &self.executable {
            return vec![executable.clone()];
        }
        deno_executable_candidates()
    }
}

impl ModuleBundler for DenoCliBundler {
    fn bundle_entrypoint(
        &self,
        worker_dir: &Path,
        entrypoint: &Path,
        output_dir: &Path,
    ) -> Result<ModuleBundle, IsolationError> {
        let worker_dir = worker_dir.canonicalize().map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_WORKER_DIR",
                format!("invalid worker_dir: {err}"),
            )
        })?;
        let entrypoint = entrypoint.canonicalize().map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_ENTRYPOINT",
                format!("invalid entrypoint: {err}"),
            )
        })?;
        if !entrypoint.starts_with(&worker_dir) {
            return Err(IsolationError::new(
                "DENO_BUNDLE_ENTRYPOINT_DENIED",
                "entrypoint must stay inside worker_dir",
            ));
        }

        std::fs::create_dir_all(output_dir).map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_OUTPUT_DIR",
                format!("failed to create bundle output dir: {err}"),
            )
        })?;
        let output_path = output_dir.join(bundle_file_name(&entrypoint));
        let candidates = self.executable_candidates();
        let mut spawn_errors = Vec::new();

        for executable in &candidates {
            match validate_bundle_graph(executable, &worker_dir, &entrypoint) {
                Ok(()) => {}
                Err(BundleCommandError::Spawn(err))
                    if err.kind() == std::io::ErrorKind::NotFound =>
                {
                    spawn_errors.push(format!("{executable}: {err}"));
                    continue;
                }
                Err(BundleCommandError::Spawn(err)) => {
                    return Err(IsolationError::new(
                        "DENO_BUNDLE_GRAPH_FAILED",
                        format!("failed to inspect bundle graph with {executable}: {err}"),
                    ));
                }
                Err(BundleCommandError::Status {
                    code,
                    stderr,
                    stdout,
                }) => {
                    return Err(IsolationError::new(
                        "DENO_BUNDLE_GRAPH_DENIED",
                        format!(
                            "bundle dependency graph rejected with status {code:?}; stderr={}; stdout={}",
                            stderr.trim(),
                            stdout.trim()
                        ),
                    ));
                }
            }
            match run_bundle_command(executable, &worker_dir, &entrypoint, &output_path) {
                Ok(()) => {
                    return bundle_from_output(&output_path);
                }
                Err(BundleCommandError::Spawn(err))
                    if err.kind() == std::io::ErrorKind::NotFound =>
                {
                    spawn_errors.push(format!("{executable}: {err}"));
                }
                Err(BundleCommandError::Spawn(err)) => {
                    return Err(IsolationError::new(
                        "DENO_BUNDLE_SPAWN_FAILED",
                        format!("failed to spawn {executable}: {err}"),
                    ));
                }
                Err(BundleCommandError::Status {
                    code,
                    stderr,
                    stdout,
                }) => {
                    if !should_try_minimal_relative_fallback(&stderr) {
                        return Err(IsolationError::new(
                            "DENO_BUNDLE_FAILED",
                            format!(
                                "deno bundle exited with status {code:?}; stderr={}; stdout={}",
                                stderr.trim(),
                                stdout.trim()
                            ),
                        ));
                    }
                    // EDG-14: a transient esbuild fetch failure (registry
                    // connect error under load, cold cache) recovers on a
                    // bounded retry, so the real `deno bundle` output wins
                    // before the minimal relative-module fallback is used.
                    std::thread::sleep(BUNDLE_RETRY_DELAY);
                    match run_bundle_command(executable, &worker_dir, &entrypoint, &output_path) {
                        Ok(()) => {
                            return bundle_from_output(&output_path);
                        }
                        Err(BundleCommandError::Spawn(err))
                            if err.kind() == std::io::ErrorKind::NotFound =>
                        {
                            spawn_errors.push(format!("{executable}: {err}"));
                            continue;
                        }
                        Err(BundleCommandError::Spawn(err)) => {
                            return Err(IsolationError::new(
                                "DENO_BUNDLE_SPAWN_FAILED",
                                format!("failed to spawn {executable} on retry: {err}"),
                            ));
                        }
                        Err(BundleCommandError::Status {
                            code: retry_code,
                            stderr: retry_stderr,
                            stdout: retry_stdout,
                        }) => {
                            if should_try_minimal_relative_fallback(&retry_stderr) {
                                match write_minimal_relative_bundle(
                                    &worker_dir,
                                    &entrypoint,
                                    &output_path,
                                ) {
                                    Ok(()) => return bundle_from_output(&output_path),
                                    Err(fallback_err) => {
                                        return Err(IsolationError::new(
                                            "DENO_BUNDLE_FAILED",
                                            format!(
                                                "deno bundle exited with status {retry_code:?}; stderr={}; stdout={}; minimal fallback failed: {}",
                                                retry_stderr.trim(),
                                                retry_stdout.trim(),
                                                fallback_err
                                            ),
                                        ));
                                    }
                                }
                            }
                            return Err(IsolationError::new(
                                "DENO_BUNDLE_FAILED",
                                format!(
                                    "deno bundle exited with status {retry_code:?}; stderr={}; stdout={}",
                                    retry_stderr.trim(),
                                    retry_stdout.trim()
                                ),
                            ));
                        }
                    }
                }
            }
        }

        Err(IsolationError::new(
            "DENO_BUNDLE_UNAVAILABLE",
            format!(
                "failed to spawn deno bundler; tried {}; errors={}",
                candidates.join(", "),
                spawn_errors.join("; ")
            ),
        ))
    }

    fn load_eszip(&self, path: &str) -> Result<ModuleBundle, IsolationError> {
        load_existing_artifact(path, BundleFormat::Eszip)
    }

    fn load_precompiled(&self, path: &str) -> Result<ModuleBundle, IsolationError> {
        load_existing_artifact(path, BundleFormat::Precompiled)
    }
}

fn validate_bundle_graph(
    executable: &str,
    worker_dir: &Path,
    entrypoint: &Path,
) -> Result<(), BundleCommandError> {
    let mut command = Command::new(executable);
    command
        .arg("info")
        .arg("--json")
        .current_dir(worker_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(config_path) = deno_config_path(worker_dir) {
        command.arg("--config").arg(config_path);
    }
    command.arg(entrypoint);

    let output = command.output().map_err(BundleCommandError::Spawn)?;
    if !output.status.success() {
        return Err(BundleCommandError::Status {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        });
    }
    validate_dependency_graph_json(worker_dir, &output.stdout).map_err(|message| {
        BundleCommandError::Status {
            code: output.status.code(),
            stderr: message,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        }
    })
}

fn validate_dependency_graph_json(worker_dir: &Path, raw: &[u8]) -> Result<(), String> {
    let graph: serde_json::Value = serde_json::from_slice(raw)
        .map_err(|err| format!("Deno dependency graph JSON is invalid: {err}"))?;
    let modules = graph
        .get("modules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Deno dependency graph has no modules array".to_string())?;
    // D29: a dependência só de tipo (JSDoc `import(...)`) aparece no grafo do
    // `deno info` mas o `deno bundle` a descarta e o runtime nunca a carrega;
    // módulos só alcançáveis por arestas de tipo não devem falhar aqui.
    let roots: HashSet<String> = graph
        .get("roots")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect();
    let mut code_targets: HashSet<String> = modules
        .iter()
        .filter_map(|module| module.get("dependencies"))
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .filter_map(|dependency| {
            dependency
                .get("code")
                .and_then(|code| code.get("specifier"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect();
    if let Some(redirects) = graph
        .get("redirects")
        .and_then(serde_json::Value::as_object)
    {
        let redirected: HashSet<String> = redirects
            .iter()
            .filter(|(from, _)| code_targets.contains(from.as_str()))
            .filter_map(|(_, to)| to.as_str().map(str::to_string))
            .collect();
        code_targets.extend(redirected);
    }
    for module in modules {
        let Some(specifier) = module.get("specifier").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !specifier.starts_with("file:") {
            continue;
        }
        if !roots.contains(specifier) && !code_targets.contains(specifier) {
            continue;
        }
        let local = module
            .get("local")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("local module has no filesystem path: {specifier}"))?;
        let local = Path::new(local)
            .canonicalize()
            .map_err(|err| format!("invalid local module {local}: {err}"))?;
        if !local.starts_with(worker_dir) {
            return Err(format!(
                "local module escapes worker_dir: {}",
                local.display()
            ));
        }
    }
    Ok(())
}

pub fn entry_needs_bundle(worker_dir: &Path, entrypoint: &Path) -> Result<bool, IsolationError> {
    let worker_dir = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_WORKER_DIR",
            format!("invalid worker_dir: {err}"),
        )
    })?;
    let entrypoint = entrypoint.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_ENTRYPOINT",
            format!("invalid entrypoint: {err}"),
        )
    })?;
    if !entrypoint.starts_with(&worker_dir) {
        return Err(IsolationError::new(
            "DENO_BUNDLE_ENTRYPOINT_DENIED",
            "entrypoint must stay inside worker_dir",
        ));
    }

    let source = std::fs::read_to_string(&entrypoint).map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_ENTRYPOINT_READ",
            format!("failed to read entrypoint: {err}"),
        )
    })?;
    if source_has_relative_module_specifier(&source) {
        return Ok(true);
    }

    worker_dir_has_extra_source_file(&worker_dir, &entrypoint)
}

fn worker_dir_has_extra_source_file(
    worker_dir: &Path,
    entrypoint: &Path,
) -> Result<bool, IsolationError> {
    let entries = std::fs::read_dir(worker_dir).map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_WORKER_DIR",
            format!("failed to read worker_dir: {err}"),
        )
    })?;
    for entry in entries {
        let path = entry
            .map_err(|err| {
                IsolationError::new(
                    "DENO_BUNDLE_WORKER_DIR",
                    format!("failed to read worker_dir entry: {err}"),
                )
            })?
            .path();
        if !path.is_file() || !is_source_file(&path) {
            continue;
        }
        let path = path.canonicalize().map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_WORKER_DIR",
                format!("invalid worker source file: {err}"),
            )
        })?;
        if path != entrypoint {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("cjs" | "cts" | "js" | "jsx" | "mjs" | "mts" | "ts" | "tsx")
    )
}

#[derive(Debug, PartialEq, Eq)]
enum JsToken<'a> {
    Punct(u8),
    String(&'a str),
    Word(&'a str),
}

fn source_has_relative_module_specifier(source: &str) -> bool {
    let tokens = js_tokens(source);
    for index in 0..tokens.len() {
        if token_is_word(&tokens[index], "import") {
            if next_string_is_relative(&tokens, index + 1) {
                return true;
            }
            if matches!(tokens.get(index + 1), Some(JsToken::Punct(b'(')))
                && next_string_is_relative(&tokens, index + 2)
            {
                return true;
            }
            if statement_has_relative_from(&tokens, index + 1) {
                return true;
            }
        } else if token_is_word(&tokens[index], "export")
            && statement_has_relative_from(&tokens, index + 1)
        {
            return true;
        }
    }
    false
}

fn statement_has_relative_from(tokens: &[JsToken<'_>], start: usize) -> bool {
    let mut index = start;
    while let Some(token) = tokens.get(index) {
        if matches!(token, JsToken::Punct(b';')) {
            return false;
        }
        if token_is_word(token, "from") && next_string_is_relative(tokens, index + 1) {
            return true;
        }
        index += 1;
    }
    false
}

fn next_string_is_relative(tokens: &[JsToken<'_>], index: usize) -> bool {
    matches!(tokens.get(index), Some(JsToken::String(specifier)) if is_relative_specifier(specifier))
}

fn token_is_word(token: &JsToken<'_>, expected: &str) -> bool {
    matches!(token, JsToken::Word(word) if *word == expected)
}

fn is_relative_specifier(specifier: &str) -> bool {
    specifier.starts_with("./") || specifier.starts_with("../")
}

fn js_tokens(source: &str) -> Vec<JsToken<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\n' | b'\r' | b'\t' => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'\'' | b'"' => {
                let quote = bytes[index];
                let start = index + 1;
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == quote {
                        break;
                    }
                    index += 1;
                }
                let end = index.min(bytes.len());
                if index < bytes.len() {
                    index += 1;
                }
                tokens.push(JsToken::String(&source[start..end]));
            }
            b'`' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == b'`' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            byte if is_ident_start(byte) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_ident_continue(bytes[index]) {
                    index += 1;
                }
                tokens.push(JsToken::Word(&source[start..index]));
            }
            byte => {
                tokens.push(JsToken::Punct(byte));
                index += 1;
            }
        }
    }
    tokens
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

/// Per-version bundle cache (EDG-20): the directory that holds the cached
/// bundle keys INSIDE the worker version dir, so it stays under the worker's
/// own `--allow-read` and disappears with the version.
pub const BUNDLE_CACHE_DIR: &str = ".edger-bundle-cache";
/// Prefix of the per-version cache keys (`v1-<sha256 hex>`).
pub const BUNDLE_KEY_PREFIX: &str = "v1-";
/// Prefix of the staging dirs inside the cache dir (a build writes there and
/// atomically renames to the final key dir). Both are excluded from the
/// content fingerprint, as is the cache dir itself.
pub const BUNDLE_STAGING_PREFIX: &str = ".tmp-";
/// Bumping this constant invalidates every cached bundle (it is part of the
/// key) whenever the bundler argument set changes.
const BUNDLE_ARGS_VERSION: &str = "v1";
/// Prefix of the legacy per-process bundle tempdirs (also excluded from the
/// fingerprint).
const LEGACY_BUNDLE_TMP_PREFIX: &str = ".edger-bundle-";
/// Prefix of the legacy bridge scripts written into the worker dir by the
/// v1 bridge (excluded from the fingerprint).
const BRIDGE_SCRIPT_PREFIX: &str = ".edger-bridge-";

/// Process-wide bundle-cache outcomes for `/metrics`
/// (`edger_worker_bundle_total{outcome=...}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BundleCacheStats {
    /// Spawns that reused an existing bundle (from the cache dir or from a
    /// single-flight build another spawn was waiting on): `deno bundle` did
    /// not run for them.
    pub hit_total: u64,
    /// Spawns that ran `deno bundle` and published the result to the cache.
    pub miss_total: u64,
    /// Bundle builds that failed (a failed build is never cached).
    pub error_total: u64,
    /// Spawns that ran `deno bundle` in the legacy per-process fallback
    /// (worker dir not writable: nothing was cached).
    pub uncached_total: u64,
}

#[derive(Debug, Default)]
struct BundleCacheCounters {
    hit: AtomicU64,
    miss: AtomicU64,
    error: AtomicU64,
    uncached: AtomicU64,
}

static BUNDLE_CACHE_COUNTERS: BundleCacheCounters = BundleCacheCounters {
    hit: AtomicU64::new(0),
    miss: AtomicU64::new(0),
    error: AtomicU64::new(0),
    uncached: AtomicU64::new(0),
};

/// Snapshot of the process-wide bundle-cache counters.
pub fn bundle_cache_stats() -> BundleCacheStats {
    BundleCacheStats {
        hit_total: BUNDLE_CACHE_COUNTERS.hit.load(Ordering::Acquire),
        miss_total: BUNDLE_CACHE_COUNTERS.miss.load(Ordering::Acquire),
        error_total: BUNDLE_CACHE_COUNTERS.error.load(Ordering::Acquire),
        uncached_total: BUNDLE_CACHE_COUNTERS.uncached.load(Ordering::Acquire),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundleOutcome {
    Hit,
    Miss,
    Error,
    Uncached,
}

pub(crate) fn record_bundle_outcome(outcome: BundleOutcome) {
    let counter = match outcome {
        BundleOutcome::Hit => &BUNDLE_CACHE_COUNTERS.hit,
        BundleOutcome::Miss => &BUNDLE_CACHE_COUNTERS.miss,
        BundleOutcome::Error => &BUNDLE_CACHE_COUNTERS.error,
        BundleOutcome::Uncached => &BUNDLE_CACHE_COUNTERS.uncached,
    };
    counter.fetch_add(1, Ordering::AcqRel);
}

fn is_bundle_excluded_name(name: &str) -> bool {
    name == BUNDLE_CACHE_DIR
        || name.starts_with(LEGACY_BUNDLE_TMP_PREFIX)
        || name.starts_with(BRIDGE_SCRIPT_PREFIX)
}

/// SHA-256 hex of every file under `worker_dir` (relative path + size +
/// mtime), sorted by path. The bundle cache dir itself, the legacy
/// `.edger-bundle-*` tempdirs and the `.edger-bridge-*` scripts are excluded
/// (edger-owned artifacts must never invalidate the key). Editing a file in
/// place changes size or mtime and therefore the key (dev dirs and the
/// WebIDE edit files in place).
pub fn fingerprint_worker_dir(worker_dir: &Path) -> Result<String, IsolationError> {
    let root = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_FINGERPRINT",
            format!("invalid worker_dir: {err}"),
        )
    })?;
    let mut files: Vec<(String, u64, i64, i32)> = Vec::new();
    collect_fingerprint_files(&root, &root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (rel, size, secs, nanos) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(b"|");
        hasher.update(size.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(secs.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(nanos.to_string().as_bytes());
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_fingerprint_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, u64, i64, i32)>,
) -> Result<(), IsolationError> {
    let entries = std::fs::read_dir(dir).map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_FINGERPRINT",
            format!("failed to read {}: {err}", dir.display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_FINGERPRINT",
                format!("failed to read dir entry: {err}"),
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_bundle_excluded_name(&name) {
            continue; // cache dir, legacy bundle tempdirs and bridge scripts:
                      // edger-owned artifacts never count into the key
        }
        let link_meta = match std::fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(_) => continue, // vanished mid-walk: skip, the next walk re-keys
        };
        let is_dir = link_meta.file_type().is_dir();
        if is_dir {
            // Symlinked dirs are not followed: the bundler resolves them at
            // bundle time and their targets are already covered by the
            // (file:) graph validation.
            collect_fingerprint_files(root, &entry.path(), out)?;
            continue;
        }
        // Files (including symlinks to files): metadata follows the target.
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
        let (secs, nanos) = mtime.map_or((0, 0), |age| {
            (age.as_secs() as i64, age.subsec_nanos() as i32)
        });
        let rel = entry
            .path()
            .strip_prefix(root)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| name.into_owned());
        out.push((rel, meta.len(), secs, nanos));
    }
    Ok(())
}

/// The per-version bundle cache key: content fingerprint of the worker dir
/// plus the deno executable version and the bundler argument set, so the
/// key changes whenever anything that affects the bundle output changes.
pub fn worker_bundle_key(worker_dir: &Path, deno_version: &str) -> Result<String, IsolationError> {
    let worker_dir = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new("DENO_BUNDLE_KEY", format!("invalid worker_dir: {err}"))
    })?;
    let fingerprint = fingerprint_worker_dir(&worker_dir)?;
    let mut hasher = Sha256::new();
    hasher.update(worker_dir.as_os_str().as_encoded_bytes());
    hasher.update(b"\0");
    hasher.update(deno_version.as_bytes());
    hasher.update(b"\0");
    hasher.update(BUNDLE_ARGS_VERSION.as_bytes());
    hasher.update(b"\0");
    hasher.update(fingerprint.as_bytes());
    Ok(format!("{BUNDLE_KEY_PREFIX}{:x}", hasher.finalize()))
}

static DENO_VERSION_MEMO: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// `deno --version` of the first candidate that spawns, computed once per
/// process lifetime and memoized per executable. A candidate that cannot be
/// spawned is memoized as `unknown` (its absence does not change within the
/// process lifetime).
pub fn deno_version_for_candidates(candidates: &[String]) -> String {
    let memo = DENO_VERSION_MEMO.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = memo.lock().expect("deno version memo lock");
    for candidate in candidates {
        if let Some(version) = guard.get(candidate) {
            // A memoized `unknown` means this executable could not be
            // probed: keep trying the next candidate (the bundler skips
            // unspawnable candidates the same way).
            if version != "unknown" {
                return version.clone();
            }
            continue;
        }
        let output = std::process::Command::new(candidate)
            .arg("--version")
            .output();
        let version = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => "unknown".to_string(),
        };
        guard.insert(candidate.clone(), version.clone());
        if version != "unknown" {
            return version;
        }
    }
    "unknown".to_string()
}

/// A resolved worker entry for a multiproc spawn. `dir` is `Some` ONLY in
/// the legacy per-process fallback (worker dir not writable): it keeps the
/// bundle file alive for the process lifetime. `None` when the bundle was
/// read from or published to the per-version cache dir.
#[derive(Debug)]
pub struct BundleEntry {
    pub bundle: ModuleBundle,
    pub dir: Option<TempDir>,
}

#[derive(Clone, Debug)]
enum BundleJobState {
    Building,
    Finished(Result<ModuleBundle, IsolationError>),
}

/// One in-flight bundle build. Concurrent spawns of the same key subscribe
/// here instead of bundling again.
struct BundleJob {
    outcome: watch::Sender<BundleJobState>,
}

impl BundleJob {
    fn new() -> Self {
        let (outcome, _rx) = watch::channel(BundleJobState::Building);
        Self { outcome }
    }

    fn finish(&self, result: Result<ModuleBundle, IsolationError>) {
        let _ = self.outcome.send(BundleJobState::Finished(result));
    }
}

static BUNDLE_JOBS: OnceLock<Mutex<HashMap<String, Arc<BundleJob>>>> = OnceLock::new();

fn register_bundle_job(key: &str) -> (Arc<BundleJob>, bool) {
    let jobs = BUNDLE_JOBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut jobs = jobs.lock().expect("bundle job registry lock");
    match jobs.get(key) {
        Some(job) => (Arc::clone(job), false),
        None => {
            let job = Arc::new(BundleJob::new());
            jobs.insert(key.to_string(), Arc::clone(&job));
            (job, true)
        }
    }
}

fn remove_bundle_job(key: &str, job: &Arc<BundleJob>) {
    let jobs = BUNDLE_JOBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut jobs = jobs.lock().expect("bundle job registry lock");
    if jobs
        .get(key)
        .is_some_and(|existing| Arc::ptr_eq(existing, job))
    {
        jobs.remove(key);
    }
}

async fn wait_for_bundle_job(
    job: &BundleJob,
    timeout: Duration,
) -> Result<ModuleBundle, IsolationError> {
    let mut rx = job.outcome.subscribe();
    let finished = tokio::time::timeout(timeout, async {
        loop {
            match rx.borrow_and_update().clone() {
                BundleJobState::Finished(result) => return result,
                BundleJobState::Building => {}
            }
            if rx.changed().await.is_err() {
                return Err(IsolationError::new(
                    "DENO_BUNDLE_WAIT_LOST",
                    "bundle builder went away without a result",
                ));
            }
        }
    })
    .await;
    match finished {
        Ok(result) => result,
        Err(_) => Err(IsolationError::new(
            "DENO_BUNDLE_WAIT_TIMEOUT",
            "timed out waiting for the in-flight bundle build",
        )),
    }
}

/// A reusable bundle file: a regular file as it is (never a symlink), whose
/// canonical path is INSIDE the canonical worker dir's cache dir
/// (`worker_dir` must be canonical). A planted or swapped symlink — or any
/// path that resolves outside the worker dir — is rejected: the caller
/// treats it as a miss and never executes it.
fn validated_bundle_file(worker_dir: &Path, path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    let cache_root = worker_dir.join(BUNDLE_CACHE_DIR);
    if !canonical.starts_with(cache_root) {
        return None;
    }
    Some(canonical.to_string_lossy().into_owned())
}

/// `remove_dir_all` guarded by a symlink check: never removes through a path
/// whose `symlink_metadata` is a symlink (a swapped entry must not be
/// followed, its target left alone).
fn remove_real_dir(path: &Path) {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_symlink()) {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Error code for "the cache root stopped being the real worker dir while
/// building": the caller takes the per-process fallback (counted
/// `uncached`) instead of failing the spawn.
const BUNDLE_ROOT_LOST_CODE: &str = "DENO_BUNDLE_ROOT_LOST";

/// The once-per-worker-dir `warn` reason when the cache root is lost mid-
/// build and the per-process fallback is taken.
const ROOT_LOST_FALLBACK_REASON: &str =
    "the bundle cache dir stopped being a real directory while building; the per-process bundle is used instead";

fn root_lost_error(stage: &str) -> IsolationError {
    IsolationError::new(
        BUNDLE_ROOT_LOST_CODE,
        format!(
            "the bundle cache dir is no longer the real worker dir (observed {stage}); the per-process bundle is used instead"
        ),
    )
}

/// The cache root is STILL the verified real directory: `symlink_metadata`
/// says a real dir (not a symlink) AND `canonicalize()` equals the expected
/// `<canonical worker dir>/.edger-bundle-cache` (`worker_dir` must be
/// canonical). Called right before every operation that would follow the
/// root (staging mkdir, publish rename, prune) so a root swapped for a
/// symlink after the probe is never written into, renamed through or
/// pruned.
fn cache_root_still_real(worker_dir: &Path, cache_root: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(cache_root) else {
        return false;
    };
    if meta.is_symlink() || !meta.is_dir() {
        return false;
    }
    cache_root
        .canonicalize()
        .is_ok_and(|canon| canon == worker_dir.join(BUNDLE_CACHE_DIR))
}

/// The staging dir returned by `tempdir_in` is a real directory (not a
/// symlink) whose canonical parent is the verified cache root (which must
/// itself still be the expected `<canonical worker dir>/.edger-bundle-cache`).
fn staging_dir_verified(worker_dir: &Path, cache_root: &Path, staging: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(staging) else {
        return false;
    };
    if meta.is_symlink() || !meta.is_dir() {
        return false;
    }
    let Ok(staging_canon) = staging.canonicalize() else {
        return false;
    };
    let Ok(cache_canon) = cache_root.canonicalize() else {
        return false;
    };
    staging_canon.parent() == Some(&cache_canon) && cache_canon == worker_dir.join(BUNDLE_CACHE_DIR)
}

/// Remove a staging dir only when it can be verified (by
/// `symlink_metadata`) to be a real directory inside the real worker dir;
/// a swapped staging path is left alone (its target is unknown — the
/// `TempDir` handle's drop then unlinks the symlink without following it).
fn cleanup_staging(worker_dir: &Path, staging: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(staging) else {
        return;
    };
    if meta.is_symlink() || !meta.is_dir() {
        return;
    }
    let Ok(canon) = staging.canonicalize() else {
        return;
    };
    if !canon.starts_with(worker_dir) {
        return;
    }
    let _ = std::fs::remove_dir_all(staging);
}

/// Classify a failed publish rename (first publish or collision retry):
/// when the cache root is no longer the real directory, the swap made the
/// rename fail (`ENOENT`) — return the root-lost error so the caller takes
/// the per-process fallback; otherwise keep the regular publish error.
fn classify_publish_rename_error(
    worker_dir: &Path,
    cache_root: &Path,
    rename_err: &std::io::Error,
) -> IsolationError {
    if !cache_root_still_real(worker_dir, cache_root) {
        return root_lost_error("during the publish rename");
    }
    IsolationError::new(
        "DENO_BUNDLE_PUBLISH",
        format!("failed to publish the bundle cache: {rename_err}"),
    )
}

/// Bundle and publish into the verified cache root (EDG-20). `worker_dir`
/// must be canonical and the cache root a verified real directory (the
/// caller probed it). The root is re-verified right before the staging
/// mkdir, before the publish rename, before the collision retry and before
/// the prune: a root swapped for a symlink after the probe is never written
/// into, renamed through or pruned — the `DENO_BUNDLE_ROOT_LOST` error then
/// makes the caller take the per-process fallback.
fn build_and_publish(
    bundler: &DenoCliBundler,
    worker_dir: &Path,
    entrypoint: &Path,
    key: &str,
    cache_root: &Path,
) -> Result<ModuleBundle, IsolationError> {
    // Fresh check right before the staging mkdir: the probe ran earlier and
    // the root may have been swapped since.
    if !cache_root_still_real(worker_dir, cache_root) {
        return Err(root_lost_error("before the staging dir"));
    }
    let mut staging_builder = tempfile::Builder::new();
    staging_builder.prefix(BUNDLE_STAGING_PREFIX);
    let staging = staging_builder.tempdir_in(cache_root).map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_STAGING",
            format!("failed to create bundle staging dir: {err}"),
        )
    })?;
    // Before anything is written into it: the staging dir must be a real
    // directory whose canonical parent is the verified cache root.
    if !staging_dir_verified(worker_dir, cache_root, staging.path()) {
        cleanup_staging(worker_dir, staging.path());
        return Err(root_lost_error("after the staging mkdir"));
    }
    // A failed bundle is never cached: drop the staging dir and surface the
    // same error as before.
    bundler
        .bundle_entrypoint(worker_dir, entrypoint, staging.path())
        .inspect_err(|_err| {
            cleanup_staging(worker_dir, staging.path());
        })?;
    let final_dir = cache_root.join(key);
    // Fresh check right before the publish rename.
    if !cache_root_still_real(worker_dir, cache_root) {
        cleanup_staging(worker_dir, staging.path());
        return Err(root_lost_error("before the publish rename"));
    }
    match std::fs::rename(staging.path(), &final_dir) {
        Ok(()) => publish_from_staging(worker_dir, entrypoint, cache_root, key),
        Err(first_err) => {
            // A concurrent publish already owns this key with a VALID
            // bundle: adopt it.
            let expected = final_dir.join(bundle_file_name(entrypoint));
            if let Some(path) = validated_bundle_file(worker_dir, &expected) {
                remove_real_dir(staging.path());
                return Ok(ModuleBundle {
                    path,
                    format: BundleFormat::JavaScript,
                });
            }
            // An invalid entry (a planted symlink, a crashed publish)
            // occupies the key dir: clear it — only a REAL, non-symlink
            // directory at the exact key we are about to publish — and retry
            // the rename once. A symlinked entry is never removed through.
            // The collision path runs later than the pre-rename check, so
            // re-verify the root before clearing or retrying.
            let occupied = std::fs::symlink_metadata(&final_dir)
                .ok()
                .is_some_and(|meta| !meta.is_symlink() && meta.is_dir());
            if occupied && cache_root_still_real(worker_dir, cache_root) {
                remove_real_dir(&final_dir);
                match std::fs::rename(staging.path(), &final_dir) {
                    Ok(()) => {
                        return publish_from_staging(worker_dir, entrypoint, cache_root, key);
                    }
                    Err(second_err) => {
                        cleanup_staging(worker_dir, staging.path());
                        return Err(classify_publish_rename_error(
                            worker_dir,
                            cache_root,
                            &second_err,
                        ));
                    }
                }
            }
            cleanup_staging(worker_dir, staging.path());
            Err(classify_publish_rename_error(
                worker_dir, cache_root, &first_err,
            ))
        }
    }
}

/// After a successful rename: validate the published path (it must be a
/// regular file inside the verified cache root — a path that escaped is
/// never executed) and prune the older keys of this version (never through
/// a root that is no longer the real directory).
fn publish_from_staging(
    worker_dir: &Path,
    entrypoint: &Path,
    cache_root: &Path,
    key: &str,
) -> Result<ModuleBundle, IsolationError> {
    let final_dir = cache_root.join(key);
    let published = final_dir.join(bundle_file_name(entrypoint));
    let Some(path) = validated_bundle_file(worker_dir, &published) else {
        // The rename landed but the path does not validate inside the
        // worker dir: the root was swapped between the rename and now. No
        // prune, no removal through it; the caller takes the fallback.
        return Err(IsolationError::new(
            BUNDLE_ROOT_LOST_CODE,
            "the published bundle path did not validate inside the worker dir",
        ));
    };
    // Fresh check before the prune (the rename ran earlier).
    if !cache_root_still_real(worker_dir, cache_root) {
        return Err(root_lost_error("before the prune"));
    }
    prune_old_bundle_keys(cache_root, key);
    Ok(ModuleBundle {
        path,
        format: BundleFormat::JavaScript,
    })
}

/// Remove the older keys of this version (a version keeps only its current
/// key). Staging dirs are left for their builders. Only REAL directories
/// whose name matches the key format are removed — never through a symlink
/// (the cache root itself or any entry), never any other name.
fn prune_old_bundle_keys(cache_root: &Path, keep: &str) {
    let Ok(root_meta) = std::fs::symlink_metadata(cache_root) else {
        return;
    };
    if root_meta.is_symlink() || !root_meta.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == keep || name.starts_with(BUNDLE_STAGING_PREFIX) {
            continue;
        }
        if !is_bundle_key_name(&name) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.is_symlink() || !meta.is_dir() {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// True when `name` is a per-version cache key (`v1-` + sha256 hex).
fn is_bundle_key_name(name: &str) -> bool {
    let Some(hex) = name.strip_prefix(BUNDLE_KEY_PREFIX) else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

static CACHE_DISABLED_WARNED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

/// One `warn` per worker dir explaining why the per-version bundle cache is
/// disabled for it (the per-process bundle behavior is kept instead).
fn warn_bundle_cache_disabled(worker_dir: &Path, reason: &str) {
    let warned = CACHE_DISABLED_WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut warned = warned.lock().expect("bundle cache warn lock");
    if warned.insert(worker_dir.to_path_buf()) {
        tracing::warn!(
            worker_dir = %worker_dir.display(),
            reason,
            "edger: the per-version bundle cache is disabled for this worker (per-process bundle behavior kept)"
        );
    }
}

fn create_bundle_tempdir(
    worker_dir: &Path,
    fallback_dir: &Path,
) -> Result<TempDir, IsolationError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(LEGACY_BUNDLE_TMP_PREFIX);
    builder
        .tempdir_in(worker_dir)
        .or_else(|_| {
            let mut fallback = tempfile::Builder::new();
            fallback.prefix("edger-bundle-");
            fallback.tempdir_in(fallback_dir)
        })
        .map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_TMP",
                format!("failed to create bundle tempdir: {err}"),
            )
        })
}

/// State of `<worker>/.edger-bundle-cache` before the cache is created,
/// used, published into or pruned (EDG-20). A symlink (or anything but a
/// real directory) is never followed or modified; a missing dir is created
/// with `create_dir` (not `create_dir_all`) and re-checked after creation,
/// so a race that swaps in a symlink is not followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheRootProbe {
    /// A real directory the cache may use (pre-existing or just created).
    Usable,
    /// Missing and not creatable (e.g. the worker dir is not writable).
    Uncreatable,
    /// Exists but is a symlink or not a directory: never follow or modify it.
    Ineligible,
}

fn probe_cache_root(worker_dir: &Path) -> Result<(PathBuf, CacheRootProbe), IsolationError> {
    let worker_dir = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_CACHE_DIR",
            format!("cannot canonicalize the worker dir: {err}"),
        )
    })?;
    let root = worker_dir.join(BUNDLE_CACHE_DIR);
    let meta = match std::fs::symlink_metadata(&root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Create with `create_dir` (one level, after the check)...
            match std::fs::create_dir(&root) {
                Ok(()) => {}
                Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Ok((worker_dir, CacheRootProbe::Uncreatable)),
            }
            // ...and re-check AFTER creation.
            match std::fs::symlink_metadata(&root) {
                Ok(meta) => meta,
                Err(_) => return Ok((worker_dir, CacheRootProbe::Uncreatable)),
            }
        }
        Err(_) => return Ok((worker_dir, CacheRootProbe::Uncreatable)),
    };
    let probe = if meta.is_symlink() || !meta.is_dir() {
        CacheRootProbe::Ineligible
    } else {
        CacheRootProbe::Usable
    };
    Ok((worker_dir, probe))
}

impl DenoCliBundler {
    /// Resolve the worker entry for a multiproc spawn through the per-version
    /// bundle cache (EDG-20). Everything that touches the filesystem or the
    /// bundler runs in `tokio::task::spawn_blocking` (no `std::process` on an
    /// async runtime thread); only the single-flight wait (when another spawn
    /// is already building this key) is awaited, bounded by `timeout` (the
    /// worker's spawn timeout, as for every other spawn phase). A hit reuses
    /// the published bundle; a miss builds and publishes atomically; a failed
    /// build is never cached and surfaces the same error as before. When the
    /// cache root is not usable — the worker dir is not writable, or
    /// `.edger-bundle-cache` is a symlink / non-directory (never followed) —
    /// the legacy per-process tempdir behavior is kept (logged once at `warn`
    /// per worker dir).
    pub async fn resolve_bundle_entry(
        &self,
        worker_dir: &Path,
        entrypoint: &Path,
        fallback_dir: &Path,
        timeout: Duration,
    ) -> Result<BundleEntry, IsolationError> {
        let worker_dir = worker_dir.to_path_buf();
        let entrypoint = entrypoint.to_path_buf();
        let fallback_dir = fallback_dir.to_path_buf();

        // 1. Fingerprint + key, off the runtime (the `deno --version`
        // probe of the first call is a process spawn).
        let key_bundler = self.clone();
        let key_worker_dir = worker_dir.clone();
        let key = tokio::task::spawn_blocking(move || {
            let candidates = key_bundler.executable_candidates();
            let deno_version = deno_version_for_candidates(&candidates);
            worker_bundle_key(&key_worker_dir, &deno_version)
        })
        .await
        .map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_BLOCKING",
                format!("bundle key computation failed: {err}"),
            )
        })??;

        // 2. Probe the cache root, off the runtime: canonical worker dir +
        // a symlink check on `.edger-bundle-cache` + `create_dir` when
        // missing (re-checked after creation). A symlinked or non-directory
        // cache root is never followed: the per-process fallback is used.
        let probe_worker_dir = worker_dir.clone();
        let (canonical_worker_dir, root_probe) =
            tokio::task::spawn_blocking(move || probe_cache_root(&probe_worker_dir))
                .await
                .map_err(|err| {
                    IsolationError::new(
                        "DENO_BUNDLE_BLOCKING",
                        format!("bundle cache root probe failed: {err}"),
                    )
                })??;
        if root_probe != CacheRootProbe::Usable {
            let reason = if root_probe == CacheRootProbe::Ineligible {
                "the .edger-bundle-cache path is a symlink or not a directory; it is never followed"
            } else {
                "the .edger-bundle-cache path cannot be created (the worker dir is probably not writable)"
            };
            return self
                .fallback_bundle(&canonical_worker_dir, &entrypoint, &fallback_dir, reason)
                .await;
        }

        let cache_root = canonical_worker_dir.join(BUNDLE_CACHE_DIR);

        // 3. Already published? (a cheap stat: the hot path after the first
        // process of the version). A reusable entry must be a regular file
        // inside the verified cache root — never a symlink, never outside.
        let entry_file = cache_root.join(&key).join(bundle_file_name(&entrypoint));
        if let Some(path) = validated_bundle_file(&canonical_worker_dir, &entry_file) {
            record_bundle_outcome(BundleOutcome::Hit);
            return Ok(BundleEntry {
                bundle: ModuleBundle {
                    path,
                    format: BundleFormat::JavaScript,
                },
                dir: None,
            });
        }

        // 4. Single flight: one build per key, the others wait (bounded by
        // the worker's spawn timeout).
        let (job, is_builder) = register_bundle_job(&key);
        if !is_builder {
            let bundle = match wait_for_bundle_job(&job, timeout).await {
                Ok(bundle) => bundle,
                // The builder found the cache root replaced while building:
                // take the same per-process fallback (counted `uncached`)
                // instead of failing the spawn.
                Err(err) if err.code == BUNDLE_ROOT_LOST_CODE => {
                    return self
                        .fallback_bundle(
                            &canonical_worker_dir,
                            &entrypoint,
                            &fallback_dir,
                            ROOT_LOST_FALLBACK_REASON,
                        )
                        .await;
                }
                Err(err) => return Err(err),
            };
            record_bundle_outcome(BundleOutcome::Hit);
            return Ok(BundleEntry { bundle, dir: None });
        }

        // 5. The build, off the runtime.
        let build_bundler = self.clone();
        let build_worker_dir = canonical_worker_dir.clone();
        let build_entrypoint = entrypoint.clone();
        let build_key = key.clone();
        let build_cache_root = cache_root.clone();
        let build_entry_file = entry_file.clone();
        let build = tokio::task::spawn_blocking(
            move || -> Result<(ModuleBundle, bool, u64), IsolationError> {
                let started = Instant::now();
                // Re-check: the entry may have been published between the
                // step-3 lookup and this point (leftover from a previous
                // process). Same validation: regular file inside the
                // verified cache root, never a symlink.
                if let Some(path) = validated_bundle_file(&build_worker_dir, &build_entry_file) {
                    return Ok((
                        ModuleBundle {
                            path,
                            format: BundleFormat::JavaScript,
                        },
                        false,
                        0,
                    ));
                }
                let bundle = build_and_publish(
                    &build_bundler,
                    &build_worker_dir,
                    &build_entrypoint,
                    &build_key,
                    &build_cache_root,
                )?;
                let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                Ok((bundle, true, elapsed_ms))
            },
        )
        .await;
        let build = match build {
            Ok(inner) => inner,
            // The blocking task panicked or the runtime was torn down: the
            // build produced nothing. Finish the job (so waiters fail with
            // the same typed error instead of hanging until their bound)
            // and drop it — the next spawn retries.
            Err(join_err) => {
                let err = IsolationError::new(
                    "DENO_BUNDLE_BLOCKING",
                    format!("bundle build task failed: {join_err}"),
                );
                record_bundle_outcome(BundleOutcome::Error);
                job.finish(Err(err.clone()));
                remove_bundle_job(&key, &job);
                return Err(err);
            }
        };

        let (bundle, rebuilt, elapsed_ms) = match build {
            Ok((bundle, rebuilt, elapsed_ms)) => {
                job.finish(Ok(bundle.clone()));
                (bundle, rebuilt, elapsed_ms)
            }
            // The cache root stopped being the real directory while building:
            // nothing was published through it. Finish the job (a waiter
            // takes the same path) and take the per-process fallback —
            // counted `uncached` — so the spawn still succeeds.
            Err(err) if err.code == BUNDLE_ROOT_LOST_CODE => {
                job.finish(Err(err.clone()));
                remove_bundle_job(&key, &job);
                return self
                    .fallback_bundle(
                        &canonical_worker_dir,
                        &entrypoint,
                        &fallback_dir,
                        ROOT_LOST_FALLBACK_REASON,
                    )
                    .await;
            }
            Err(err) => {
                record_bundle_outcome(BundleOutcome::Error);
                job.finish(Err(err.clone()));
                remove_bundle_job(&key, &job);
                return Err(err);
            }
        };
        if rebuilt {
            record_bundle_outcome(BundleOutcome::Miss);
            tracing::info!(
                worker_dir = %worker_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                key = %key.chars().take(16).collect::<String>(),
                duration_ms = elapsed_ms,
                "edger: bundle cache miss: built the worker bundle"
            );
        } else {
            record_bundle_outcome(BundleOutcome::Hit);
        }
        remove_bundle_job(&key, &job);
        Ok(BundleEntry { bundle, dir: None })
    }

    /// The legacy per-process fallback bundle (EDG-20): today's behavior,
    /// off the runtime. Used when the cache root is not usable (not
    /// writable, or a symlink / non-directory that is never followed).
    /// Single-flight is NOT applied: the bundle file lives in a per-process
    /// tempdir that no other spawn may read. Counted as `uncached`.
    async fn fallback_bundle(
        &self,
        worker_dir: &Path,
        entrypoint: &Path,
        fallback_dir: &Path,
        warn_reason: &str,
    ) -> Result<BundleEntry, IsolationError> {
        warn_bundle_cache_disabled(worker_dir, warn_reason);
        let fb_bundler = self.clone();
        let fb_worker_dir = worker_dir.to_path_buf();
        let fb_entrypoint = entrypoint.to_path_buf();
        let fb_fallback_dir = fallback_dir.to_path_buf();
        let result = tokio::task::spawn_blocking(move || {
            let dir = create_bundle_tempdir(&fb_worker_dir, &fb_fallback_dir)?;
            let bundle =
                fb_bundler.bundle_entrypoint(&fb_worker_dir, &fb_entrypoint, dir.path())?;
            Ok(BundleEntry {
                bundle,
                dir: Some(dir),
            })
        })
        .await
        .map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_BLOCKING",
                format!("bundle fallback failed: {err}"),
            )
        })?;
        match result {
            Ok(entry) => {
                record_bundle_outcome(BundleOutcome::Uncached);
                Ok(entry)
            }
            Err(err) => {
                record_bundle_outcome(BundleOutcome::Error);
                Err(err)
            }
        }
    }
}

fn bundle_from_output(output_path: &Path) -> Result<ModuleBundle, IsolationError> {
    let path = output_path.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_OUTPUT",
            format!("invalid bundle output: {err}"),
        )
    })?;
    Ok(ModuleBundle {
        path: path.to_string_lossy().into_owned(),
        format: BundleFormat::JavaScript,
    })
}

enum BundleCommandError {
    Spawn(std::io::Error),
    Status {
        code: Option<i32>,
        stderr: String,
        stdout: String,
    },
}

fn run_bundle_command(
    executable: &str,
    worker_dir: &Path,
    entrypoint: &Path,
    output_path: &Path,
) -> Result<(), BundleCommandError> {
    let mut command = Command::new(executable);
    command
        .arg("bundle")
        .arg("--no-check")
        .arg("--platform")
        .arg("deno")
        .arg("--packages")
        .arg("bundle")
        .arg("--inline-imports=true")
        .arg("--output")
        .arg(output_path)
        .current_dir(worker_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(config_path) = deno_config_path(worker_dir) {
        command.arg("--config").arg(config_path);
    }
    command.arg(entrypoint);

    let output = command.output().map_err(BundleCommandError::Spawn)?;
    if output.status.success() {
        return Ok(());
    }

    Err(BundleCommandError::Status {
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// EDG-14: bounded delay before the single retry of `deno bundle` after a
/// transient esbuild fetch failure, before the minimal relative-module
/// fallback is used.
const BUNDLE_RETRY_DELAY: Duration = Duration::from_millis(300);

fn should_try_minimal_relative_fallback(stderr: &str) -> bool {
    stderr.contains("@esbuild") || stderr.contains("registry.npmjs.org")
}

fn write_minimal_relative_bundle(
    worker_dir: &Path,
    entrypoint: &Path,
    output_path: &Path,
) -> Result<(), IsolationError> {
    let mut visited = HashSet::new();
    let mut output =
        String::from("// Generated by EdgeR's minimal relative-module bundler fallback.\n");
    append_module(worker_dir, entrypoint, &mut visited, &mut output)?;
    std::fs::write(output_path, output).map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_FALLBACK_WRITE",
            format!("failed to write fallback bundle: {err}"),
        )
    })
}

fn append_module(
    worker_dir: &Path,
    module_path: &Path,
    visited: &mut HashSet<PathBuf>,
    output: &mut String,
) -> Result<(), IsolationError> {
    let module_path = module_path.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_FALLBACK_MODULE",
            format!("invalid module path: {err}"),
        )
    })?;
    if !module_path.starts_with(worker_dir) {
        return Err(IsolationError::new(
            "DENO_BUNDLE_FALLBACK_DENIED",
            "relative import must stay inside worker_dir",
        ));
    }
    if !visited.insert(module_path.clone()) {
        return Ok(());
    }

    let source = std::fs::read_to_string(&module_path).map_err(|err| {
        IsolationError::new(
            "DENO_BUNDLE_FALLBACK_READ",
            format!("failed to read {}: {err}", module_path.display()),
        )
    })?;
    let mut body = Vec::new();
    for line in source.lines() {
        if let Some(specifier) = import_specifier(line) {
            if specifier.starts_with("./") || specifier.starts_with("../") {
                let dependency = resolve_relative_import(&module_path, specifier)?;
                append_module(worker_dir, &dependency, visited, output)?;
                continue;
            }
            return Err(IsolationError::new(
                "DENO_BUNDLE_FALLBACK_IMPORT",
                format!("unsupported non-relative import in fallback: {specifier}"),
            ));
        }
        body.push(strip_export(line));
    }

    output.push_str("\n// ");
    output.push_str(&module_path.to_string_lossy());
    output.push('\n');
    for line in body {
        output.push_str(&line);
        output.push('\n');
    }
    Ok(())
}

fn import_specifier(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("import ")?;
    let specifier = if rest.starts_with('"') || rest.starts_with('\'') {
        rest
    } else {
        rest.split_once(" from ")?.1.trim_start()
    };
    quoted_prefix(specifier)
}

fn quoted_prefix(value: &str) -> Option<&str> {
    let quote = value.as_bytes().first().copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let remainder = &value[1..];
    let end = remainder.find(char::from(quote))?;
    Some(&remainder[..end])
}

fn strip_export(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];
    if let Some(rest) = trimmed.strip_prefix("export async function ") {
        return format!("{indent}async function {rest}");
    }
    for prefix in [
        "export function ",
        "export class ",
        "export const ",
        "export let ",
        "export var ",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return format!("{indent}{}{rest}", prefix.trim_start_matches("export "));
        }
    }
    if trimmed.starts_with("export {") {
        return String::new();
    }
    line.to_string()
}

fn resolve_relative_import(from: &Path, specifier: &str) -> Result<PathBuf, IsolationError> {
    let base = from.parent().ok_or_else(|| {
        IsolationError::new(
            "DENO_BUNDLE_FALLBACK_IMPORT",
            "module path must have a parent directory",
        )
    })?;
    let candidate = base.join(specifier);
    if candidate.is_file() {
        return Ok(candidate);
    }
    for extension in ["ts", "js", "mjs"] {
        let path = candidate.with_extension(extension);
        if path.is_file() {
            return Ok(path);
        }
    }
    for index in ["index.ts", "index.js", "index.mjs"] {
        let path = candidate.join(index);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(IsolationError::new(
        "DENO_BUNDLE_FALLBACK_IMPORT",
        format!("relative import not found: {specifier}"),
    ))
}

fn load_existing_artifact(
    path: &str,
    format: BundleFormat,
) -> Result<ModuleBundle, IsolationError> {
    let path = Path::new(path).canonicalize().map_err(|err| {
        IsolationError::new("DENO_BUNDLE_ARTIFACT", format!("invalid artifact: {err}"))
    })?;
    Ok(ModuleBundle {
        path: path.to_string_lossy().into_owned(),
        format,
    })
}

fn bundle_file_name(entrypoint: &Path) -> String {
    let stem = entrypoint
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("worker");
    format!("{stem}.bundle.mjs")
}

fn deno_config_path(worker_dir: &Path) -> Option<PathBuf> {
    ["deno.json", "deno.jsonc"]
        .iter()
        .map(|name| worker_dir.join(name))
        .find(|path| path.is_file())
}

pub(crate) fn default_deno_executable() -> String {
    deno_executable_candidates()
        .into_iter()
        .next()
        .unwrap_or_else(|| "deno".into())
}

fn deno_executable_candidates() -> Vec<String> {
    if let Ok(executable) = env::var("EDGER_DENO_BIN") {
        if !executable.trim().is_empty() {
            return vec![executable];
        }
    }

    let mut candidates = Vec::from(["deno".to_string()]);
    if !binary_exists_on_path("deno") {
        if let Ok(home) = env::var("HOME") {
            let path = Path::new(&home).join(".deno/bin/deno");
            if path.is_file() {
                candidates.push(path.to_string_lossy().into_owned());
            }
        }
    }
    candidates
}

fn binary_exists_on_path(binary: &str) -> bool {
    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path_var).any(|dir| dir.join(binary).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small two-file worker whose entrypoint imports a relative module:
    /// enough to exercise the key without a real `deno` binary.
    fn write_worker_fixture(root: &Path) -> PathBuf {
        let worker = root.join("bundle-app");
        std::fs::create_dir_all(&worker).unwrap();
        std::fs::write(
            worker.join("index.ts"),
            r#"import { message } from "./lib.ts";
export default () => new Response(message);
"#,
        )
        .unwrap();
        std::fs::write(worker.join("lib.ts"), "export const message = 'ok';\n").unwrap();
        worker
    }

    #[test]
    fn bundle_key_same_tree_same_key() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let deno_version = "deno 2.9.7 (stable, release, aarch64-apple-darwin)";
        let key_a = worker_bundle_key(&worker, deno_version).unwrap();
        let key_b = worker_bundle_key(&worker, deno_version).unwrap();
        assert_eq!(key_a, key_b, "same tree + same deno version = same key");
        assert!(
            key_a.starts_with(BUNDLE_KEY_PREFIX),
            "key keeps its version prefix: {key_a}"
        );
        // 3-byte prefix + 64 hex chars of sha256.
        assert_eq!(key_a.len(), BUNDLE_KEY_PREFIX.len() + 64);
    }

    #[test]
    fn bundle_key_invalidate_on_edit() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let deno_version = "deno 2.9.7";
        let before = worker_bundle_key(&worker, deno_version).unwrap();

        // Content change with a DIFFERENT size: invalidates via size (and
        // mtime).
        std::fs::write(
            worker.join("lib.ts"),
            "export const message = 'edited-differently-longer';\n",
        )
        .unwrap();
        let after_size_change = worker_bundle_key(&worker, deno_version).unwrap();
        assert_ne!(
            before, after_size_change,
            "editing a source file must invalidate"
        );

        // Same-SIZE content change: invalidates via mtime (dev dirs and the
        // WebIDE edit files in place; APFS keeps nanosecond mtimes).
        let same_size = "export const message = 'ok-xyz';\n";
        std::fs::write(worker.join("lib.ts"), same_size).unwrap();
        let after_same_size = worker_bundle_key(&worker, deno_version).unwrap();
        assert_ne!(
            after_size_change, after_same_size,
            "same-size edit invalidates via mtime"
        );

        // Edit the entrypoint too.
        std::fs::write(
            worker.join("index.ts"),
            "import { message } from './lib.ts';\nexport default () => new Response(message + 'x');\n",
        )
        .unwrap();
        let after_entry_change = worker_bundle_key(&worker, deno_version).unwrap();
        assert_ne!(after_same_size, after_entry_change);
    }

    #[test]
    fn bundle_key_invalidate_on_add_and_remove() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let deno_version = "deno 2.9.7";
        let base = worker_bundle_key(&worker, deno_version).unwrap();

        std::fs::write(worker.join("extra.ts"), "export const extra = 1;\n").unwrap();
        let with_extra = worker_bundle_key(&worker, deno_version).unwrap();
        assert_ne!(base, with_extra, "adding a file must invalidate");

        std::fs::remove_file(worker.join("extra.ts")).unwrap();
        let after_remove = worker_bundle_key(&worker, deno_version).unwrap();
        assert_eq!(
            base, after_remove,
            "removing the added file returns to the base key (path+size+mtime are unchanged for the survivors)"
        );

        std::fs::remove_file(worker.join("lib.ts")).unwrap();
        let without_lib = worker_bundle_key(&worker, deno_version).unwrap();
        assert_ne!(base, without_lib, "removing a file must invalidate");
    }

    #[test]
    fn bundle_key_changes_with_deno_version() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let key_a = worker_bundle_key(&worker, "deno 2.9.7").unwrap();
        let key_b = worker_bundle_key(&worker, "deno 2.10.0").unwrap();
        assert_ne!(key_a, key_b, "a different deno version must change the key");
    }

    #[test]
    fn bundle_key_excludes_cache_dir_and_legacy_tempdirs() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let deno_version = "deno 2.9.7";
        let base = worker_bundle_key(&worker, deno_version).unwrap();

        // The per-version cache dir with a published key.
        let cache_key = worker_bundle_key(&worker, deno_version).unwrap();
        std::fs::create_dir_all(worker.join(BUNDLE_CACHE_DIR).join(&cache_key)).unwrap();
        std::fs::write(
            worker
                .join(BUNDLE_CACHE_DIR)
                .join(&cache_key)
                .join("index.bundle.mjs"),
            "// bundle",
        )
        .unwrap();
        // A legacy per-process bundle tempdir inside the worker dir.
        let legacy = worker.join(".edger-bundle-abc123");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("index.bundle.mjs"), "// bundle").unwrap();
        // A legacy bridge script.
        std::fs::write(worker.join(".edger-bridge-zzz.mjs"), "// script").unwrap();

        let after = worker_bundle_key(&worker, deno_version).unwrap();
        assert_eq!(
            base, after,
            "edger-owned artifacts must not invalidate the key"
        );
    }

    #[test]
    fn deno_version_for_candidates_memoizes_once_per_process() {
        let root = tempfile::tempdir().unwrap();
        // A fake deno: a shell script that prints its version. Memoization is
        // per process lifetime, so rewriting the script must NOT change the
        // memoized value for the same executable path.
        let fake = root.path().join("fake-deno");
        std::fs::write(&fake, "#!/bin/sh\necho 'fake 1.0.0'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let candidates = vec![fake.to_string_lossy().into_owned()];
        let first = deno_version_for_candidates(&candidates);
        assert_eq!(first, "fake 1.0.0");
        std::fs::write(&fake, "#!/bin/sh\necho 'fake 2.0.0'\n").unwrap();
        let second = deno_version_for_candidates(&candidates);
        assert_eq!(second, "fake 1.0.0", "computed once per process lifetime");

        // A candidate that cannot be spawned is memoized as `unknown`.
        let missing = vec![root.path().join("nope-deno").to_string_lossy().into_owned()];
        assert_eq!(deno_version_for_candidates(&missing), "unknown");
    }

    #[test]
    fn bundle_cache_counters_record_outcomes() {
        let before = bundle_cache_stats();
        record_bundle_outcome(BundleOutcome::Miss);
        record_bundle_outcome(BundleOutcome::Hit);
        record_bundle_outcome(BundleOutcome::Error);
        record_bundle_outcome(BundleOutcome::Uncached);
        let after = bundle_cache_stats();
        assert_eq!(after.miss_total, before.miss_total + 1);
        assert_eq!(after.hit_total, before.hit_total + 1);
        assert_eq!(after.error_total, before.error_total + 1);
        assert_eq!(after.uncached_total, before.uncached_total + 1);
    }

    #[test]
    fn dependency_graph_rejects_sibling_worker_file() {
        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("alpha");
        let sibling = root.path().join("beta");
        std::fs::create_dir_all(&worker).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let entry = worker.join("index.ts");
        let secret = sibling.join("secret.ts");
        std::fs::write(&entry, "export default () => null;").unwrap();
        std::fs::write(&secret, "export const secret = 1;").unwrap();
        let worker = worker.canonicalize().unwrap();
        let graph = serde_json::json!({
            "roots": [format!("file://{}", entry.display())],
            "modules": [
                {
                    "specifier": format!("file://{}", entry.display()),
                    "local": entry,
                    "dependencies": [
                        {
                            "specifier": "../beta/secret.ts",
                            "code": {
                                "specifier": format!("file://{}", secret.display())
                            }
                        }
                    ]
                },
                {"specifier": format!("file://{}", secret.display()), "local": secret}
            ]
        });

        let error =
            validate_dependency_graph_json(&worker, graph.to_string().as_bytes()).unwrap_err();
        assert!(error.contains("escapes worker_dir"));
    }

    #[test]
    fn dependency_graph_allows_worker_files_and_remote_cache() {
        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("alpha");
        std::fs::create_dir_all(&worker).unwrap();
        let entry = worker.join("index.ts");
        std::fs::write(&entry, "export default () => null;").unwrap();
        let worker = worker.canonicalize().unwrap();
        let graph = serde_json::json!({
            "modules": [
                {"specifier": format!("file://{}", entry.display()), "local": entry},
                {"specifier": "https://example.test/mod.ts", "local": "/tmp/deno-cache/mod.ts"}
            ]
        });

        validate_dependency_graph_json(&worker, graph.to_string().as_bytes()).unwrap();
    }

    // D29: Q2-1 do levantamento — dependência só de tipo (JSDoc) para um
    // alvo fora do worker que não resolve: o módulo-alvo tem `error` e sem
    // `local`, e a aresta é `type`. O bundle descarta o tipo, então o grafo
    // é válido.
    #[test]
    fn type_only_dependency_outside_worker_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("worker");
        std::fs::create_dir_all(&worker).unwrap();
        let entry = worker.join("index.js");
        std::fs::write(&entry, "Deno.serve(() => null);").unwrap();
        let worker = worker.canonicalize().unwrap();
        let type_target = format!("file://{}/types/index", root.path().display());
        let graph = serde_json::json!({
            "roots": [format!("file://{}", entry.display())],
            "modules": [
                {
                    "specifier": type_target,
                    "error": format!("Module not found \"{type_target}\".")
                },
                {
                    "kind": "esm",
                    "dependencies": [
                        {
                            "specifier": "../types/index",
                            "type": {
                                "specifier": type_target,
                                "span": {
                                    "start": {"line": 0, "character": 18},
                                    "end": {"line": 0, "character": 34}
                                }
                            }
                        }
                    ],
                    "local": entry,
                    "size": 172,
                    "mediaType": "JavaScript",
                    "specifier": format!("file://{}", entry.display())
                }
            ],
            "redirects": {},
            "npmPackages": {}
        });

        validate_dependency_graph_json(&worker, graph.to_string().as_bytes()).unwrap();
    }

    // D29: Q2-3 do levantamento — import de tipo com extensão explícita
    // resolve para um `.d.ts` fora do worker_dir (`local` + `mediaType: "Dts"`);
    // como a aresta é `type`, o módulo é ignorado e o grafo é válido.
    #[test]
    fn type_only_dependency_resolved_outside_worker_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("worker");
        let types_dir = root.path().join("types");
        std::fs::create_dir_all(&worker).unwrap();
        std::fs::create_dir_all(&types_dir).unwrap();
        let entry = worker.join("index.js");
        let dts = types_dir.join("index.d.ts");
        std::fs::write(&entry, "Deno.serve(() => null);").unwrap();
        std::fs::write(&dts, "export type X = { ok: boolean };\n").unwrap();
        let worker = worker.canonicalize().unwrap();
        let graph = serde_json::json!({
            "roots": [format!("file://{}", entry.display())],
            "modules": [
                {
                    "kind": "esm",
                    "local": dts,
                    "size": 30,
                    "mediaType": "Dts",
                    "specifier": format!("file://{}", dts.display())
                },
                {
                    "kind": "esm",
                    "dependencies": [
                        {
                            "specifier": "../types/index.d.ts",
                            "type": {
                                "specifier": format!("file://{}", dts.display()),
                                "span": {
                                    "start": {"line": 0, "character": 18},
                                    "end": {"line": 0, "character": 44}
                                }
                            }
                        }
                    ],
                    "local": entry,
                    "size": 172,
                    "mediaType": "JavaScript",
                    "specifier": format!("file://{}", entry.display())
                }
            ],
            "redirects": {},
            "npmPackages": {}
        });

        validate_dependency_graph_json(&worker, graph.to_string().as_bytes()).unwrap();
    }

    // D29: mesma forma do caso resolvido acima, mas com aresta `code`: o
    // alvo fica no conjunto de alvos de código e continua sendo recusado.
    #[test]
    fn code_dependency_outside_worker_is_still_rejected() {
        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("worker");
        let types_dir = root.path().join("types");
        std::fs::create_dir_all(&worker).unwrap();
        std::fs::create_dir_all(&types_dir).unwrap();
        let entry = worker.join("index.js");
        let dts = types_dir.join("index.d.ts");
        std::fs::write(&entry, "Deno.serve(() => null);").unwrap();
        std::fs::write(&dts, "export type X = { ok: boolean };\n").unwrap();
        let worker = worker.canonicalize().unwrap();
        let graph = serde_json::json!({
            "roots": [format!("file://{}", entry.display())],
            "modules": [
                {
                    "kind": "esm",
                    "local": dts,
                    "size": 30,
                    "mediaType": "Dts",
                    "specifier": format!("file://{}", dts.display())
                },
                {
                    "kind": "esm",
                    "dependencies": [
                        {
                            "specifier": "../types/index.d.ts",
                            "code": {
                                "specifier": format!("file://{}", dts.display()),
                                "span": {
                                    "start": {"line": 0, "character": 18},
                                    "end": {"line": 0, "character": 44}
                                }
                            }
                        }
                    ],
                    "local": entry,
                    "size": 172,
                    "mediaType": "JavaScript",
                    "specifier": format!("file://{}", entry.display())
                }
            ],
            "redirects": {},
            "npmPackages": {}
        });

        let error =
            validate_dependency_graph_json(&worker, graph.to_string().as_bytes()).unwrap_err();
        assert!(error.contains("escapes worker_dir"), "{error}");
    }

    /// A recursive snapshot of a tree (regular files as (relpath, bytes)) —
    /// proves a tree is byte-for-byte unchanged.
    fn tree_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
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

    /// A cache root swapped for a symlink AFTER the probe (the state
    /// `build_and_publish` sees) is refused before any write: the publish
    /// step returns `DENO_BUNDLE_ROOT_LOST` (no rename, no prune, no
    /// staging), the victim tree is byte-for-byte unchanged, and the
    /// per-process fallback that the caller takes on this error resolves
    /// the entry as `uncached`.
    #[cfg(unix)]
    #[tokio::test]
    async fn swapped_cache_root_publish_is_rejected_and_falls_back() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let worker = worker.canonicalize().unwrap_or_else(|_| worker.clone());
        let cache_root = worker.join(BUNDLE_CACHE_DIR);
        std::fs::create_dir(&cache_root).unwrap();
        // The victim: a sibling dir with a subdirectory and a file.
        let victim = root.path().join("victim-swap");
        std::fs::create_dir_all(victim.join("inner")).unwrap();
        std::fs::write(victim.join("KEEP_ME.txt"), b"keep-me").unwrap();
        std::fs::write(victim.join("inner").join("nested.txt"), b"nested").unwrap();
        let before = tree_snapshot(&victim);
        // The swap: the verified real dir is replaced by a symlink OUTSIDE
        // the worker dir (the state the probe cannot see after it returns).
        std::fs::remove_dir(&cache_root).unwrap();
        std::os::unix::fs::symlink(&victim, &cache_root).unwrap();

        let bundler = DenoCliBundler::default();
        let entrypoint = worker.join("index.ts");
        let key = worker_bundle_key(&worker, "deno 2.9.7 (test)").unwrap();
        let err = build_and_publish(&bundler, &worker, &entrypoint, &key, &cache_root)
            .expect_err("a swapped cache root must not be written into");
        assert_eq!(
            err.code, BUNDLE_ROOT_LOST_CODE,
            "the typed root-lost error makes the caller fall back: {err}"
        );
        // The victim tree is byte-for-byte unchanged; nothing was renamed or
        // pruned through the link.
        let after = tree_snapshot(&victim);
        assert_eq!(
            before, after,
            "the symlink target is byte-for-byte unchanged"
        );
        for (rel, _bytes) in &after {
            assert!(
                !rel.starts_with("v1-"),
                "a key dir appeared in the victim: {rel}"
            );
        }
        // No staging dir was created (the check runs before the mkdir), and
        // the symlink itself was not replaced or deleted.
        let worker_names: Vec<String> = std::fs::read_dir(&worker)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            worker_names
                .iter()
                .all(|name| !name.starts_with(BUNDLE_STAGING_PREFIX)),
            "no staging dir was left behind: {worker_names:?}"
        );
        assert!(
            std::fs::symlink_metadata(&cache_root).unwrap().is_symlink(),
            "the symlink was not replaced or deleted"
        );

        // The fallback the caller takes on this error: a per-process bundle,
        // counted `uncached`.
        let before_stats = bundle_cache_stats();
        let entry = bundler
            .fallback_bundle(&worker, &entrypoint, root.path(), ROOT_LOST_FALLBACK_REASON)
            .await
            .expect("the fallback resolves the entry");
        let after_stats = bundle_cache_stats();
        assert!(
            entry.dir.is_some(),
            "the fallback keeps a per-process tempdir"
        );
        assert!(
            !entry.bundle.path.contains(BUNDLE_CACHE_DIR),
            "the fallback bundle does not go through the cache dir: {}",
            entry.bundle.path
        );
        assert_eq!(
            after_stats.uncached_total - before_stats.uncached_total,
            1,
            "the fallback counts as uncached"
        );
        assert_eq!(
            after_stats.miss_total - before_stats.miss_total,
            0,
            "nothing was published"
        );
    }

    /// The fresh root/staging checks see a swap that happened after the
    /// probe: `cache_root_still_real` and `staging_dir_verified` are true
    /// while the root is the real directory and false once it is a symlink.
    #[cfg(unix)]
    #[test]
    fn fresh_checks_reject_a_swapped_cache_root() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let worker = worker.canonicalize().unwrap_or_else(|_| worker.clone());
        let cache_root = worker.join(BUNDLE_CACHE_DIR);
        std::fs::create_dir(&cache_root).unwrap();
        let staging = cache_root.join(format!("{BUNDLE_STAGING_PREFIX}test-staging"));
        std::fs::create_dir(&staging).unwrap();
        // Real root: both checks pass.
        assert!(
            cache_root_still_real(&worker, &cache_root),
            "real root is verified"
        );
        assert!(
            staging_dir_verified(&worker, &cache_root, &staging),
            "staging inside the real root is verified"
        );
        // Swapped root (after the probe): both checks fail.
        let victim = root.path().join("victim-fresh");
        std::fs::create_dir_all(&victim).unwrap();
        // cache_root is the real dir this test created (staging inside):
        // remove it whole, then swap in the symlink.
        std::fs::remove_dir_all(&cache_root).unwrap();
        std::os::unix::fs::symlink(&victim, &cache_root).unwrap();
        assert!(
            !cache_root_still_real(&worker, &cache_root),
            "a symlinked root is not the real dir"
        );
        assert!(
            !staging_dir_verified(&worker, &cache_root, &staging),
            "a staging dir reached through a symlinked root is not verified"
        );
    }

    /// A publish rename that fails while the cache root is no longer the
    /// real directory is root-lost (the caller takes the per-process
    /// fallback); with the real root it stays the regular publish error.
    #[cfg(unix)]
    #[test]
    fn classify_publish_rename_error_sees_the_swapped_root() {
        let not_found = std::io::Error::from(std::io::ErrorKind::NotFound);
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let worker = worker.canonicalize().unwrap_or_else(|_| worker.clone());
        let cache_root = worker.join(BUNDLE_CACHE_DIR);
        std::fs::create_dir(&cache_root).unwrap();
        // The swap: the verified real dir is replaced by a symlink to a
        // victim outside the worker dir (a rename that fails in this state
        // is the root being swapped, not a publish failure).
        let victim = root.path().join("victim-rename-classify");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::remove_dir(&cache_root).unwrap();
        std::os::unix::fs::symlink(&victim, &cache_root).unwrap();
        let err = classify_publish_rename_error(&worker, &cache_root, &not_found);
        assert_eq!(
            err.code, BUNDLE_ROOT_LOST_CODE,
            "swapped root: the rename failure is root-lost: {err}"
        );
        // The real root: the regular publish error.
        std::fs::remove_file(&cache_root).unwrap();
        std::fs::create_dir(&cache_root).unwrap();
        let err = classify_publish_rename_error(&worker, &cache_root, &not_found);
        assert_eq!(
            err.code, "DENO_BUNDLE_PUBLISH",
            "real root: the regular publish error: {err}"
        );
    }

    /// EDG-14: a fake `deno` executable for the retry tests. `info` answers
    /// with a valid two-module graph; `bundle` fails its first
    /// `fail_bundle_attempts` runs with the esbuild fetch error (the
    /// minimal-fallback trigger) and counts every `bundle` attempt in
    /// `root/bundle-attempts`, so a retry is observable.
    #[cfg(unix)]
    fn write_fake_deno(
        root: &Path,
        worker: &Path,
        entry: &Path,
        fail_bundle_attempts: u32,
    ) -> PathBuf {
        let lib = worker.join("lib.ts");
        let state = root.join("bundle-attempts");
        let graph = serde_json::json!({
            "roots": [format!("file://{}", entry.display())],
            "modules": [
                {
                    "specifier": format!("file://{}", entry.display()),
                    "local": entry,
                    "dependencies": [
                        {"code": {"specifier": format!("file://{}", lib.display())}}
                    ]
                },
                {
                    "specifier": format!("file://{}", lib.display()),
                    "local": lib,
                    "dependencies": []
                }
            ]
        });
        let script = format!(
            r#"#!/bin/sh
STATE='{state}'
FAILS={fails}
n=$(cat "$STATE" 2>/dev/null || printf 0)
if [ "$1" = "info" ]; then
  printf '%s' '{graph}'
  exit 0
fi
n=$((n + 1))
printf '%s' "$n" > "$STATE"
out=
prev=
for a in "$@"; do
  if [ "$prev" = "--output" ]; then out="$a"; fi
  prev="$a"
done
if [ "$n" -le "$FAILS" ]; then
  printf 'error: Failed loading https://registry.npmjs.org/@esbuild/darwin-arm64 for package "@esbuild/darwin-arm64"\nCaused by:\n    client error (Connect)\n' 1>&2
  exit 1
fi
printf 'var recovered = true;\n' > "$out"
exit 0
"#,
            state = state.display(),
            fails = fail_bundle_attempts,
            graph = graph,
        );
        let fake = root.join("fake-deno");
        std::fs::write(&fake, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    // EDG-14: a transient esbuild fetch failure (the fallback trigger)
    // recovers on the single bounded retry: the bundle is the real
    // `deno bundle` output and the command ran exactly twice.
    #[cfg(unix)]
    #[test]
    fn bundle_retries_once_after_transient_esbuild_fetch_failure() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let entry = worker.join("index.ts");
        let state = root.path().join("bundle-attempts");
        let fake = write_fake_deno(root.path(), &worker, &entry, 1);
        let output_dir = root.path().join("out");
        let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());

        let bundle = bundler
            .bundle_entrypoint(&worker, &entry, &output_dir)
            .expect("a transient esbuild failure must recover on the single retry");
        assert_eq!(
            std::fs::read_to_string(Path::new(&bundle.path)).unwrap(),
            "var recovered = true;\n",
            "the retry must produce the real bundle, not the fallback"
        );
        assert_eq!(
            std::fs::read_to_string(&state).unwrap(),
            "2",
            "the bundle command must run exactly twice (first attempt fails, retry wins)"
        );
    }

    // EDG-14: a persistent esbuild failure (retry fails too) still keeps the
    // minimal relative-module fallback, reached only after the single retry.
    #[cfg(unix)]
    #[test]
    fn bundle_falls_back_after_retry_still_fails() {
        let root = tempfile::tempdir().unwrap();
        let worker = write_worker_fixture(root.path());
        let entry = worker.join("index.ts");
        let state = root.path().join("bundle-attempts");
        let fake = write_fake_deno(root.path(), &worker, &entry, 99);
        let output_dir = root.path().join("out");
        let bundler = DenoCliBundler::new(fake.to_string_lossy().into_owned());

        let bundle = bundler
            .bundle_entrypoint(&worker, &entry, &output_dir)
            .expect("a persistent esbuild failure must keep the minimal fallback");
        let source = std::fs::read_to_string(Path::new(&bundle.path)).unwrap();
        assert!(
            source.starts_with("// Generated by EdgeR's minimal relative-module bundler fallback."),
            "the persistent failure must produce the fallback bundle: {source}"
        );
        assert!(
            source.contains("const message = 'ok';"),
            "the fallback keeps the relative module bodies: {source}"
        );
        assert_eq!(
            std::fs::read_to_string(&state).unwrap(),
            "2",
            "the fallback must be reached only after the single retry"
        );
    }
}
