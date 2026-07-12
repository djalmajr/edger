//! Deno module bundling hooks (Edge Runtime `eszip_trait` alignment).
//!
//! The eszip/precompiled artifact-loading surface (`BundleFormat::Eszip` /
//! `Precompiled`, `ModuleBundler::load_eszip` / `load_precompiled`,
//! `load_existing_artifact`, and the `DenoCliBundler::new` constructor) mirrors
//! the Edge Runtime eszip trait for a planned artifact-cache integration. It is
//! deliberate API-shape scaffolding not yet wired to a caller, so allow the
//! resulting dead-code here rather than collapsing the aligned surface.
#![allow(dead_code)]

use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use edger_core::IsolationError;

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
                    if should_try_minimal_relative_fallback(&stderr) {
                        match write_minimal_relative_bundle(&worker_dir, &entrypoint, &output_path)
                        {
                            Ok(()) => return bundle_from_output(&output_path),
                            Err(fallback_err) => {
                                return Err(IsolationError::new(
                                    "DENO_BUNDLE_FAILED",
                                    format!(
                                        "deno bundle exited with status {code:?}; stderr={}; stdout={}; minimal fallback failed: {}",
                                        stderr.trim(),
                                        stdout.trim(),
                                        fallback_err
                                    ),
                                ));
                            }
                        }
                    }
                    return Err(IsolationError::new(
                        "DENO_BUNDLE_FAILED",
                        format!(
                            "deno bundle exited with status {code:?}; stderr={}; stdout={}",
                            stderr.trim(),
                            stdout.trim()
                        ),
                    ));
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
    for module in modules {
        let Some(specifier) = module.get("specifier").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !specifier.starts_with("file:") {
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
            "modules": [
                {"specifier": format!("file://{}", entry.display()), "local": entry},
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
}
