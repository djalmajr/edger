//! Deploy-time pre-compression of immutable assets (EDG-4).
//!
//! Fingerprinted assets are immutable per version, so their compressed
//! variants can be produced once at deploy time — at the maximum quality —
//! and served verbatim instead of being recompressed on every request:
//!
//! - `<file>.br` — brotli, quality 11, lgwin 22;
//! - `<file>.gz` — gzip, level 9.
//!
//! Only assets that the serving paths classify as IMMUTABLE (the same
//! `cache_control_for` rules, never duplicated here) and whose content type
//! is compressible are candidates. A variant is kept only when it is
//! strictly smaller than the original; a variant the package already ships
//! is kept as-is. The total number of GENERATED variant bytes is bounded by
//! the deploy expanded-byte budget: when it is exhausted, generation stops
//! (the remaining files keep real-time compression) and the report says so.
//! A file that fails to compress is skipped with a `warn` log and never
//! fails the deploy.
//!
//! The function runs on the extracted staging directory, before the atomic
//! swap, so the variants travel with the install and a rollback removes
//! them together with the target directory.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use brotli::CompressorWriter;
use edger_core::{ExecutionKind, WorkerConfig};
use flate2::{write::GzEncoder, Compression};

/// Minimum source size (bytes) for a pre-compressed variant: mirrors the
/// real-time compression floor (`MIN_COMPRESSIBLE_BYTES`).
pub const MIN_VARIANT_SOURCE_BYTES: u64 = 1024;

/// Maximum source size (bytes) for a pre-compressed variant.
pub const MAX_VARIANT_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

const BROTLI_QUALITY: u32 = 11;
const BROTLI_LGWIN: u32 = 22;
const GZIP_LEVEL: u32 = 9;

/// A pre-compressed variant of an original file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantEncoding {
    /// `.br` — brotli.
    Brotli,
    /// `.gz` — gzip.
    Gzip,
}

impl VariantEncoding {
    pub const ALL: [Self; 2] = [Self::Brotli, Self::Gzip];

    /// File suffix appended to the original name (`.br` / `.gz`).
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Brotli => ".br",
            Self::Gzip => ".gz",
        }
    }

    /// `content-encoding` value served with the variant.
    pub fn content_encoding(self) -> &'static str {
        match self {
            Self::Brotli => "br",
            Self::Gzip => "gzip",
        }
    }
}

/// Report of one `precompress_worker_assets` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrecompressReport {
    /// Files that passed every eligibility gate (immutable, compressible
    /// content type, size window).
    pub eligible_files: usize,
    /// `.br`/`.gz` files written by this run.
    pub variants_generated: usize,
    /// Variant files the package already shipped (kept as-is, not counted
    /// against the budget).
    pub variants_existing: usize,
    /// Generated variants discarded because they were not smaller than the
    /// original.
    pub variants_rejected: usize,
    /// Total bytes of the variants written by this run.
    pub generated_bytes: u64,
    /// The budget was exhausted mid-run: generation stopped and the
    /// remaining files keep real-time compression.
    pub budget_exhausted: bool,
}

/// Path of the `<original>` + `<suffix>` variant file.
pub fn variant_path(original: &Path, encoding: VariantEncoding) -> PathBuf {
    let mut name = original.as_os_str().to_os_string();
    name.push(encoding.suffix());
    PathBuf::from(name)
}

/// True when the content type (as produced by `content_type_for`) is worth
/// compressing: text (never HTML — entries are transformed at runtime),
/// javascript, json, xml, svg and wasm. Raster images, fonts, archives,
/// pdf and octet-stream are never candidates.
pub fn is_compressible_content_type(content_type: &str) -> bool {
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    (media_type.starts_with("text/") && !media_type.starts_with("text/html"))
        || matches!(
            media_type,
            "application/javascript"
                | "application/json"
                | "application/xml"
                | "image/svg+xml"
                | "application/wasm"
        )
}

/// Generate `.br`/`.gz` variants of the immutable assets under the extracted
/// worker directory `worker_dir` (staging) for `kind` + `config`.
///
/// Only `StaticSpa` and `Fullstack` kinds are processed; every other kind
/// returns an empty report. `budget_bytes` bounds the TOTAL bytes of
/// generated variants. This function never fails the deploy: per-file errors
/// are logged (`warn`) and skipped. All writes stay inside the canonicalized
/// staging root (the fullstack `clientDir` is validated with the SAME rules
/// the serving path uses) and symlinks are never followed or written to.
pub fn precompress_worker_assets(
    worker_dir: &Path,
    kind: &ExecutionKind,
    config: &WorkerConfig,
    budget_bytes: u64,
) -> PrecompressReport {
    let mut report = PrecompressReport::default();
    // Containment anchor: every source read and every variant write must end
    // up inside this canonicalized staging directory.
    let staging_root = match worker_dir.canonicalize() {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(
                worker_dir = %worker_dir.display(),
                error = %err,
                "pre-compression: cannot canonicalize worker dir, no variants generated"
            );
            return report;
        }
    };
    let candidates = match kind {
        ExecutionKind::StaticSpa { .. } => collect_static_spa_candidates(&staging_root),
        ExecutionKind::Fullstack { .. } => collect_fullstack_candidates(&staging_root, config),
        _ => return report,
    };
    for path in candidates {
        if report.budget_exhausted {
            break;
        }
        process_file(&path, &staging_root, budget_bytes, &mut report);
    }
    report
}

/// Static SPA candidates: every file that the serving rule classifies as
/// immutable (fingerprinted `assets/<name>-<hash>`, never HTML).
fn collect_static_spa_candidates(staging_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_regular_files(staging_root, &mut files);
    files
        .into_iter()
        .filter(|path| crate::static_spa::is_immutable_asset(path))
        .collect()
}

/// Fullstack candidates: every file under `<worker_dir>/<clientDir>/assets/`
/// (the same `/assets/` prefix rule the serving path uses for immutability).
///
/// `clientDir` is validated exactly like the serving path
/// (`fullstack::resolve_client_root`): relative, no `..`/root/prefix
/// components, and its canonical form must stay inside the worker dir. An
/// invalid or escaping `clientDir` generates NOTHING (warned, never an
/// error) instead of writing variants outside staging.
fn collect_fullstack_candidates(staging_root: &Path, config: &WorkerConfig) -> Vec<PathBuf> {
    let Some(fullstack) = config.fullstack.as_ref() else {
        return Vec::new();
    };
    let Some(client_dir) = fullstack.client_dir.as_deref() else {
        return Vec::new();
    };
    if crate::fullstack::path_has_forbidden_components(client_dir)
        || Path::new(client_dir).is_absolute()
    {
        tracing::warn!(
            client_dir,
            "pre-compression: clientDir must stay inside the worker dir, no variants generated"
        );
        return Vec::new();
    }
    let client_root = match staging_root.join(client_dir).canonicalize() {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(
                client_dir,
                error = %err,
                "pre-compression: invalid clientDir, no variants generated"
            );
            return Vec::new();
        }
    };
    if !client_root.starts_with(staging_root) {
        tracing::warn!(
            client_dir,
            "pre-compression: clientDir escapes the worker dir, no variants generated"
        );
        return Vec::new();
    }
    let mut files = Vec::new();
    walk_regular_files(&client_root, &mut files);
    files
        .into_iter()
        .filter(|path| {
            let relative = path
                .strip_prefix(&client_root)
                .map(|relative| relative.to_path_buf())
                .unwrap_or_default();
            crate::fullstack::is_immutable_asset_path(&format!("/{}", relative.display()))
        })
        .collect()
}

/// Walk regular files under `root` (recursive). Dot-prefixed entries are
/// internal to the install (`.edger-revision`, framework internals) and are
/// never candidate assets. Symlinks are NEVER followed (file or directory
/// links are skipped, their targets never walked): a link is not a package
/// asset, and following it would let the walk — and the variant writes —
/// escape the staging root.
fn walk_regular_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = path
            .file_name()
            .map(|name| name.to_string_lossy().starts_with('.'))
            .unwrap_or(false);
        if hidden {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_regular_files(&path, out);
        } else if file_type.is_file() {
            out.push(path);
        }
    }
}

fn process_file(
    path: &Path,
    staging_root: &Path,
    budget_bytes: u64,
    report: &mut PrecompressReport,
) {
    // The source must be a REGULAR file (never a symlink) and its directory
    // (canonicalized) must still be inside the validated staging root.
    let source_meta = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        _ => {
            tracing::warn!(
                path = %path.display(),
                "pre-compression: not a regular file (or a symlink), skipped"
            );
            return;
        }
    };
    let source_contained = path
        .parent()
        .and_then(|dir| dir.canonicalize().ok())
        .is_some_and(|dir| dir.starts_with(staging_root));
    if !source_contained {
        tracing::warn!(
            path = %path.display(),
            "pre-compression: source escapes the staging root, skipped"
        );
        return;
    }
    let size = source_meta.len();
    if !(MIN_VARIANT_SOURCE_BYTES..=MAX_VARIANT_SOURCE_BYTES).contains(&size) {
        return;
    }
    let content_type = crate::static_spa::content_type_for(path);
    if !is_compressible_content_type(content_type) {
        return;
    }
    report.eligible_files += 1;
    let Ok(original) = fs::read(path) else {
        tracing::warn!(
            path = %path.display(),
            "pre-compression: cannot read file, skipped"
        );
        return;
    };
    for encoding in VariantEncoding::ALL {
        if report.budget_exhausted {
            break;
        }
        let variant = variant_path(path, encoding);
        // The variant sits next to the source (same directory), so its
        // canonical directory is the validated source dir; the destination
        // itself must not be a symlink — a pre-existing link is neither a
        // package variant nor a write target.
        let variant_meta = match fs::symlink_metadata(&variant) {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                tracing::warn!(
                    variant = %variant.display(),
                    error = %err,
                    "pre-compression: cannot stat variant path, skipped"
                );
                continue;
            }
        };
        let is_symlink = variant_meta
            .as_ref()
            .is_some_and(|meta| meta.file_type().is_symlink());
        if is_symlink {
            tracing::warn!(
                variant = %variant.display(),
                "pre-compression: variant path is a symlink, skipped"
            );
            continue;
        }
        // The package already ships this variant (framework build): keep it.
        if variant_meta.is_some_and(|meta| meta.is_file()) {
            report.variants_existing += 1;
            continue;
        }
        let Ok(compressed) = compress(&original, encoding) else {
            tracing::warn!(
                path = %path.display(),
                encoding = encoding.content_encoding(),
                "pre-compression: compression failed, file kept real-time"
            );
            continue;
        };
        // A variant must beat the original, or it costs bandwidth for
        // nothing.
        if compressed.len() >= original.len() {
            report.variants_rejected += 1;
            continue;
        }
        if report.generated_bytes + compressed.len() as u64 > budget_bytes {
            // Budget exhausted: stop generating; the remaining files fall
            // back to real-time compression.
            report.budget_exhausted = true;
            break;
        }
        if let Err(err) = fs::write(&variant, &compressed) {
            tracing::warn!(
                path = %variant.display(),
                error = %err,
                "pre-compression: cannot write variant, file kept real-time"
            );
            continue;
        }
        report.variants_generated += 1;
        report.generated_bytes += compressed.len() as u64;
    }
}

fn compress(data: &[u8], encoding: VariantEncoding) -> Result<Vec<u8>, std::io::Error> {
    match encoding {
        VariantEncoding::Brotli => {
            let mut encoder = CompressorWriter::new(Vec::new(), 0, BROTLI_QUALITY, BROTLI_LGWIN);
            encoder.write_all(data)?;
            // `into_inner` finishes the brotli stream (BROTLI_OPERATION_FINISH).
            Ok(encoder.into_inner())
        }
        VariantEncoding::Gzip => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::new(GZIP_LEVEL));
            encoder.write_all(data)?;
            Ok(encoder.finish()?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::fs;
    use std::io::Read;

    fn static_kind() -> ExecutionKind {
        ExecutionKind::StaticSpa { inject_base: true }
    }

    fn fullstack_config(client_dir: Option<&str>) -> WorkerConfig {
        let manifest = edger_core::WorkerManifest {
            name: "fullstack-demo".into(),
            adapter: Some("tanstack".into()),
            client_dir: client_dir.map(str::to_string),
            kind: Some("fullstack".into()),
            ssr_entrypoint: Some("server/server.js".into()),
            ..edger_core::WorkerManifest::default()
        };
        edger_core::parse_worker_config(&manifest)
    }

    fn brotli_decode(bytes: &[u8]) -> Vec<u8> {
        let mut decoder = brotli::Decompressor::new(bytes, 0);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        out
    }

    fn gzip_decode(bytes: &[u8]) -> Vec<u8> {
        let mut decoder = GzDecoder::new(bytes);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        out
    }

    /// Deterministic pseudo-random bytes: incompressible, so any variant is
    /// larger than the source.
    fn incompressible(size: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E3779B97F4A7C15;
        (0..size)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    fn js_body(size: usize) -> Vec<u8> {
        let mut body = String::from("/* edger bundle */\nconst a = 1;\n");
        while body.len() < size {
            body.push_str(&format!(
                "export const p{} = 'padding padding padding';\n",
                body.len()
            ));
        }
        body.into_bytes()
    }

    #[test]
    fn selects_only_eligible_immutable_assets() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        // Eligible: fingerprinted asset, compressible type, >= 1 KiB.
        fs::write(root.path().join("assets/app-a1b2c3d4.js"), js_body(2048)).unwrap();
        // Not immutable: un-hashed name (served max-age=300).
        fs::write(root.path().join("assets/controller.js"), js_body(2048)).unwrap();
        // Never: HTML entries are transformed at runtime.
        fs::write(root.path().join("index.html"), "<html>".repeat(300)).unwrap();
        // Never: raster image.
        fs::write(root.path().join("logo.png"), [0x89u8, 0x50u8].repeat(1024)).unwrap();
        // Not immutable: hash-like name outside assets/.
        fs::write(root.path().join("app-a1b2c3d4.js"), js_body(2048)).unwrap();
        // Too small: below the 1 KiB floor.
        fs::write(root.path().join("assets/mini-a1b2c3d4.js"), "tiny").unwrap();

        let report = precompress_worker_assets(
            root.path(),
            &static_kind(),
            &edger_core::parse_worker_config(&edger_core::WorkerManifest::default()),
            u64::MAX,
        );

        assert_eq!(report.eligible_files, 1, "{report:?}");
        assert_eq!(report.variants_generated, 2, "{report:?}");
        assert_eq!(report.variants_rejected, 0, "{report:?}");
        assert!(!report.budget_exhausted);

        let original = fs::read(root.path().join("assets/app-a1b2c3d4.js")).unwrap();
        let br = fs::read(root.path().join("assets/app-a1b2c3d4.js.br")).unwrap();
        let gz = fs::read(root.path().join("assets/app-a1b2c3d4.js.gz")).unwrap();
        assert!(br.len() < original.len());
        assert!(gz.len() < original.len());
        assert_eq!(brotli_decode(&br), original);
        assert_eq!(gzip_decode(&gz), original);
        for absent in [
            "assets/controller.js.br",
            "assets/controller.js.gz",
            "index.html.br",
            "logo.png.br",
            "app-a1b2c3d4.js.br",
            "assets/mini-a1b2c3d4.js.br",
        ] {
            assert!(
                !root.path().join(absent).exists(),
                "{absent} must not exist"
            );
        }
    }

    #[test]
    fn keeps_package_provided_variants() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("assets/app-a1b2c3d4.js"), js_body(2048)).unwrap();
        let shipped = b"shipped-by-framework-br";
        fs::write(root.path().join("assets/app-a1b2c3d4.js.br"), shipped).unwrap();

        let report = precompress_worker_assets(
            root.path(),
            &static_kind(),
            &edger_core::parse_worker_config(&edger_core::WorkerManifest::default()),
            u64::MAX,
        );

        assert_eq!(report.variants_existing, 1, "{report:?}");
        assert_eq!(report.variants_generated, 1, "{report:?}"); // only the .gz
                                                                // The package variant is kept byte-for-byte.
        assert_eq!(
            fs::read(root.path().join("assets/app-a1b2c3d4.js.br")).unwrap(),
            shipped
        );
        assert!(root.path().join("assets/app-a1b2c3d4.js.gz").is_file());
    }

    #[test]
    fn discards_variant_not_smaller_than_original() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        let data = incompressible(4096);
        fs::write(root.path().join("assets/blob-a1b2c3d4.js"), &data).unwrap();

        let report = precompress_worker_assets(
            root.path(),
            &static_kind(),
            &edger_core::parse_worker_config(&edger_core::WorkerManifest::default()),
            u64::MAX,
        );

        assert_eq!(report.eligible_files, 1, "{report:?}");
        assert_eq!(report.variants_rejected, 2, "{report:?}");
        assert_eq!(report.variants_generated, 0, "{report:?}");
        assert!(!root.path().join("assets/blob-a1b2c3d4.js.br").exists());
        assert!(!root.path().join("assets/blob-a1b2c3d4.js.gz").exists());
    }

    #[test]
    fn stops_generating_at_the_budget() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("assets/one-a1b2c3d4.js"), js_body(2048)).unwrap();
        fs::write(root.path().join("assets/two-d4c3b2a1.js"), js_body(2048)).unwrap();

        // A budget that fits one variant and no more.
        let report = precompress_worker_assets(
            root.path(),
            &static_kind(),
            &edger_core::parse_worker_config(&edger_core::WorkerManifest::default()),
            1,
        );

        assert!(report.budget_exhausted, "{report:?}");
        assert_eq!(report.variants_generated, 0, "{report:?}");
        assert!(report.generated_bytes <= 1);
        assert!(!root.path().join("assets/one-a1b2c3d4.js.br").exists());
        assert!(!root.path().join("assets/two-d4c3b2a1.js.gz").exists());

        // A budget that fits one file's two variants (in whatever order the
        // walk visits the files), and no more.
        let root2 = tempfile::tempdir().unwrap();
        fs::create_dir_all(root2.path().join("assets")).unwrap();
        fs::write(root2.path().join("assets/one-a1b2c3d4.js"), js_body(2048)).unwrap();
        fs::write(root2.path().join("assets/two-d4c3b2a1.js"), js_body(2048)).unwrap();
        let reference = compress(&js_body(2048), VariantEncoding::Brotli).unwrap();
        let reference_gz = compress(&js_body(2048), VariantEncoding::Gzip).unwrap();
        let budget = reference.len() as u64 + reference_gz.len() as u64;
        let report = precompress_worker_assets(
            root2.path(),
            &static_kind(),
            &edger_core::parse_worker_config(&edger_core::WorkerManifest::default()),
            budget,
        );
        assert!(report.budget_exhausted, "{report:?}");
        assert_eq!(report.variants_generated, 2, "{report:?}");
        assert_eq!(
            report.generated_bytes,
            reference.len() as u64 + reference_gz.len() as u64
        );
        // The budget fits exactly ONE file's two variants (in whatever order
        // the walk visits them): one file is fully varianted, the other has
        // nothing.
        let one_br = root2.path().join("assets/one-a1b2c3d4.js.br");
        let one_gz = root2.path().join("assets/one-a1b2c3d4.js.gz");
        let two_br = root2.path().join("assets/two-d4c3b2a1.js.br");
        let two_gz = root2.path().join("assets/two-d4c3b2a1.js.gz");
        let one_full = one_br.is_file() && one_gz.is_file();
        let two_full = two_br.is_file() && two_gz.is_file();
        assert!(one_full ^ two_full, "exactly one file gets both variants");
        if one_full {
            assert!(!two_br.exists() && !two_gz.exists());
        } else {
            assert!(!one_br.exists() && !one_gz.exists());
        }
    }

    #[test]
    fn fullstack_walk_covers_only_client_dir_assets() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("client/assets")).unwrap();
        fs::write(
            root.path().join("client/assets/app-a1b2c3d4.js"),
            js_body(2048),
        )
        .unwrap();
        // Same immutable shape, outside clientDir: not a candidate.
        fs::create_dir_all(root.path().join("stray/assets")).unwrap();
        fs::write(
            root.path().join("stray/assets/app-a1b2c3d4.js"),
            js_body(2048),
        )
        .unwrap();
        let config = fullstack_config(Some("client"));

        let report = precompress_worker_assets(
            root.path(),
            &ExecutionKind::Fullstack {
                adapter: "tanstack".into(),
            },
            &config,
            u64::MAX,
        );

        assert_eq!(report.eligible_files, 1, "{report:?}");
        assert_eq!(report.variants_generated, 2, "{report:?}");
        assert!(root
            .path()
            .join("client/assets/app-a1b2c3d4.js.br")
            .is_file());
        assert!(!root.path().join("stray/assets/app-a1b2c3d4.js.br").exists());
    }

    #[test]
    fn absolute_or_parent_client_dir_generates_nothing_outside_staging() {
        let base = tempfile::tempdir().unwrap();
        let staging = base.path().join("staging");
        fs::create_dir_all(staging.join("client")).unwrap();
        let outside = base.path().join("outside");
        fs::create_dir_all(outside.join("assets")).unwrap();
        fs::write(outside.join("assets/app-a1b2c3d4.js"), js_body(2048)).unwrap();
        let kind = ExecutionKind::Fullstack {
            adapter: "tanstack".into(),
        };

        // An absolute clientDir pointing at the outside dir.
        let config = fullstack_config(Some(outside.to_str().unwrap()));
        let report = precompress_worker_assets(&staging, &kind, &config, u64::MAX);
        assert_eq!(report, PrecompressReport::default(), "{report:?}");

        // A relative clientDir with `..` that escapes the staging to the
        // outside dir.
        let config = fullstack_config(Some("../outside"));
        let report = precompress_worker_assets(&staging, &kind, &config, u64::MAX);
        assert_eq!(report, PrecompressReport::default(), "{report:?}");

        // Nothing was created outside the staging.
        assert!(!outside.join("assets/app-a1b2c3d4.js.br").exists());
        assert!(!outside.join("assets/app-a1b2c3d4.js.gz").exists());
        assert!(!staging.join("outside").exists());
    }

    #[test]
    fn symlinks_inside_assets_are_never_walked_or_varianted() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::create_dir_all(outside.path().join("dir")).unwrap();
        fs::write(root.path().join("assets/real-a1b2c3d4.js"), js_body(2048)).unwrap();
        fs::write(outside.path().join("linked.js"), js_body(2048)).unwrap();
        fs::write(outside.path().join("dir/inner-a1b2c3d4.js"), js_body(2048)).unwrap();
        // A file link and a directory link inside assets/: neither is a
        // package asset and neither target may be reached.
        std::os::unix::fs::symlink(
            outside.path().join("linked.js"),
            root.path().join("assets/linked-d4c3b2a1.js"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("dir"),
            root.path().join("assets/linked-dir"),
        )
        .unwrap();

        let config = edger_core::parse_worker_config(&edger_core::WorkerManifest::default());
        let report = precompress_worker_assets(root.path(), &static_kind(), &config, u64::MAX);

        assert_eq!(report.eligible_files, 1, "{report:?}");
        assert_eq!(report.variants_generated, 2, "{report:?}");
        assert!(root.path().join("assets/real-a1b2c3d4.js.br").is_file());
        // The links are not candidates and their targets stay untouched.
        assert!(!root.path().join("assets/linked-d4c3b2a1.js.br").exists());
        assert!(!outside.path().join("linked.js.br").exists());
        assert!(!outside.path().join("dir/inner-a1b2c3d4.js.br").exists());
    }

    #[test]
    fn existing_symlink_variant_is_skipped_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("assets/app-a1b2c3d4.js"), js_body(2048)).unwrap();
        let external = b"external-variant-bytes";
        fs::write(outside.path().join("external.br"), external).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("external.br"),
            root.path().join("assets/app-a1b2c3d4.js.br"),
        )
        .unwrap();

        let config = edger_core::parse_worker_config(&edger_core::WorkerManifest::default());
        let report = precompress_worker_assets(root.path(), &static_kind(), &config, u64::MAX);

        // The symlinked .br is neither "existing" nor a write target: only
        // the .gz is generated.
        assert_eq!(report.variants_existing, 0, "{report:?}");
        assert_eq!(report.variants_generated, 1, "{report:?}");
        // The symlink is intact and still points at the untouched file.
        assert!(root
            .path()
            .join("assets/app-a1b2c3d4.js.br")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read(outside.path().join("external.br")).unwrap(),
            external
        );
        assert!(root.path().join("assets/app-a1b2c3d4.js.gz").is_file());
    }

    #[test]
    fn other_kinds_are_untouched() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("assets/app-a1b2c3d4.js"), js_body(2048)).unwrap();
        let config = edger_core::parse_worker_config(&edger_core::WorkerManifest::default());
        for kind in [
            ExecutionKind::FetchHandler,
            ExecutionKind::RoutesTable,
            ExecutionKind::WasmModule { entry: None },
        ] {
            let report = precompress_worker_assets(root.path(), &kind, &config, u64::MAX);
            assert_eq!(report, PrecompressReport::default(), "{kind:?}");
        }
        assert!(!root.path().join("assets/app-a1b2c3d4.js.br").exists());
    }

    #[test]
    fn variants_are_deterministic() {
        // Deterministic variants: two runs over the same bytes give the same
        // file (no timestamp/filename embedded in the gzip header).
        let data = js_body(2048);
        for encoding in VariantEncoding::ALL {
            assert_eq!(
                compress(&data, encoding).unwrap(),
                compress(&data, encoding).unwrap()
            );
        }
    }

    #[test]
    fn content_type_eligibility_table() {
        for (content_type, expected) in [
            ("text/css; charset=utf-8", true),
            ("application/javascript; charset=utf-8", true),
            ("application/json; charset=utf-8", true),
            ("application/xml", true),
            ("image/svg+xml", true),
            ("application/wasm", true),
            ("text/html; charset=utf-8", false),
            ("image/png", false),
            ("image/jpeg", false),
            ("font/woff2", false),
            ("application/zip", false),
            ("application/gzip", false),
            ("application/x-brotli", false),
            ("application/pdf", false),
            ("application/octet-stream", false),
        ] {
            assert_eq!(
                is_compressible_content_type(content_type),
                expected,
                "{content_type}"
            );
        }
    }
}
