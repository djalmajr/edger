//! Deno CLI bundler coverage for multi-file workers.

#![cfg(feature = "deno")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use edger_isolation::deno::{entry_needs_bundle, BundleFormat, DenoCliBundler, ModuleBundler};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("multi_file_worker")
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

fn run_bundle(bundle_path: &Path, bundler: &DenoCliBundler) -> String {
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
  return {
    finished: Promise.resolve(),
    ref() {},
    shutdown() {},
    unref() {},
  };
};
try {
  await import(bundleUrl);
} finally {
  Deno.serve = originalServe;
}
if (typeof capturedHandler !== "function") {
  throw new Error("no Deno.serve handler captured");
}
const response = await capturedHandler(new Request("https://example.test/"));
console.log(await response.text());
"#,
    )
    .expect("write harness");

    let read_allowlist = format!(
        "{},{}",
        harness_dir.path().display(),
        bundle_path.parent().expect("bundle has parent").display()
    );
    for executable in bundler.executable_candidates() {
        match Command::new(&executable)
            .arg("run")
            .arg("--no-prompt")
            .arg(format!("--allow-read={read_allowlist}"))
            .arg(&harness_path)
            .arg(file_url(bundle_path))
            .output()
        {
            Ok(output) if output.status.success() => {
                return String::from_utf8_lossy(&output.stdout).trim().to_string();
            }
            Ok(output) => {
                panic!(
                    "bundle harness failed with status {:?}; stderr={}; stdout={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim(),
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => panic!("failed to spawn {executable}: {err}"),
        }
    }
    panic!("deno executable not found for bundle harness");
}

// Mutation captured: making bundle detection unconditional turns this
// single-file worker into a bundle candidate and loses the fast path.
#[test]
fn entry_needs_bundle_is_false_for_single_file_without_relative_imports() {
    let worker_dir = tempfile::tempdir().expect("create worker dir");
    let entrypoint = worker_dir.path().join("index.ts");
    fs::write(
        &entrypoint,
        r#"Deno.serve(() => new Response("single file"));"#,
    )
    .expect("write entrypoint");

    assert!(
        !entry_needs_bundle(worker_dir.path(), &entrypoint).expect("detect bundle need"),
        "single-file workers without relative imports must use the direct import fast path"
    );
}

// Mutation captured: ignoring static relative imports would let runtime import
// resolution leak past the worker packaging boundary instead of bundling.
#[test]
fn entry_needs_bundle_is_true_for_relative_imports() {
    let worker_dir = tempfile::tempdir().expect("create worker dir");
    let entrypoint = worker_dir.path().join("index.ts");
    fs::write(
        &entrypoint,
        r#"import { message } from "./message.ts";
Deno.serve(() => new Response(message));
"#,
    )
    .expect("write entrypoint");

    assert!(
        entry_needs_bundle(worker_dir.path(), &entrypoint).expect("detect bundle need"),
        "relative imports must keep using the bundler path"
    );
}

// Mutation captured: only checking import text misses multi-source workers
// whose extra source file should still opt into the conservative bundle path.
#[test]
fn entry_needs_bundle_is_true_for_extra_source_file() {
    let worker_dir = tempfile::tempdir().expect("create worker dir");
    let entrypoint = worker_dir.path().join("index.ts");
    fs::write(
        &entrypoint,
        r#"Deno.serve(() => new Response("single import graph"));"#,
    )
    .expect("write entrypoint");
    fs::write(
        worker_dir.path().join("helper.ts"),
        "export const unused = 1;",
    )
    .expect("write extra source");

    assert!(
        entry_needs_bundle(worker_dir.path(), &entrypoint).expect("detect bundle need"),
        "extra worker source files must keep using the bundle path"
    );
}

#[test]
fn deno_cli_bundler_inlines_relative_imports() {
    let worker_dir = fixture_dir();
    let entrypoint = worker_dir.join("index.ts");
    let output_dir = tempfile::tempdir().expect("create bundle output dir");
    let bundler = DenoCliBundler::default();

    let bundle = match bundler.bundle_entrypoint(&worker_dir, &entrypoint, output_dir.path()) {
        Ok(bundle) => bundle,
        Err(err) if err.code == "DENO_BUNDLE_UNAVAILABLE" => {
            eprintln!(
                "skipping deno_cli_bundler_inlines_relative_imports: {}",
                err.message
            );
            return;
        }
        Err(err) => panic!("bundle failed: {err}"),
    };

    assert_eq!(bundle.format, BundleFormat::JavaScript);
    let bundle_path = Path::new(&bundle.path);
    assert!(bundle_path.is_file(), "bundle artifact must exist");

    let raw_entrypoint = fs::read_to_string(&entrypoint).expect("read raw entrypoint");
    assert!(
        raw_entrypoint.contains("./message.ts"),
        "fixture must prove the raw entrypoint still has a relative import"
    );
    let bundled_source = fs::read_to_string(bundle_path).expect("read bundle");
    assert!(
        bundled_source.contains("hello from dependency"),
        "bundle must include the imported module body"
    );
    assert!(
        !bundled_source.contains("./message.ts"),
        "bundle must not leave the relative import for runtime resolution"
    );
    assert_eq!(run_bundle(bundle_path, &bundler), "hello from dependency");
}

#[test]
fn deno_cli_bundler_rejects_cross_worker_relative_import() {
    let workers = tempfile::tempdir().expect("create workers root");
    let worker_dir = workers.path().join("alpha");
    let sibling_dir = workers.path().join("beta");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::create_dir_all(&sibling_dir).unwrap();
    let entrypoint = worker_dir.join("index.ts");
    fs::write(
        &entrypoint,
        r#"import { secret } from "../beta/secret.ts";
Deno.serve(() => new Response(secret));
"#,
    )
    .unwrap();
    fs::write(
        sibling_dir.join("secret.ts"),
        "export const secret = 'private';",
    )
    .unwrap();
    let output_dir = tempfile::tempdir().expect("create bundle output dir");

    let error = DenoCliBundler::default()
        .bundle_entrypoint(&worker_dir, &entrypoint, output_dir.path())
        .unwrap_err();

    assert_eq!(error.code, "DENO_BUNDLE_GRAPH_DENIED");
    assert!(error.message.contains("escapes worker_dir"));
}
