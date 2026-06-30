//! Load Wasm modules from worker directories (story 07.05).

use std::borrow::Cow;
use std::path::{Component, Path};

use edger_core::IsolationError;

pub fn load_wasm_from_worker_dir(
    worker_dir: &Path,
    entry: &str,
) -> Result<Vec<u8>, IsolationError> {
    let entry_path = Path::new(entry);
    if entry_path.is_absolute() || has_parent_traversal(entry_path) {
        return Err(IsolationError::new(
            "WASM_PATH",
            "entrypoint must be a relative path within the worker dir",
        ));
    }
    let full = worker_dir.join(entry_path);
    let canonical_worker = worker_dir
        .canonicalize()
        .map_err(|e| IsolationError::new("WASM_PATH", e.to_string()))?;
    let canonical_file = full
        .canonicalize()
        .map_err(|e| IsolationError::new("WASM_PATH", e.to_string()))?;
    if !canonical_file.starts_with(&canonical_worker) {
        return Err(IsolationError::new(
            "WASM_PATH",
            "entrypoint escapes worker directory",
        ));
    }
    let bytes = std::fs::read(&canonical_file)
        .map_err(|e| IsolationError::new("WASM_READ", e.to_string()))?;
    if canonical_file.extension().and_then(|ext| ext.to_str()) == Some("wat") {
        return wat::parse_bytes(&bytes)
            .map(Cow::into_owned)
            .map_err(|e| IsolationError::new("WAT_COMPILE", e.to_string()));
    }
    Ok(bytes)
}

fn has_parent_traversal(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_parent_dir_entry() {
        let err = load_wasm_from_worker_dir(Path::new("/tmp"), "../escape.wasm").unwrap_err();
        assert_eq!(err.code, "WASM_PATH");
    }

    #[test]
    fn loads_file_inside_worker_dir() {
        let dir = tempfile::tempdir().unwrap();
        let wasm_path = dir.path().join("index.wasm");
        fs::write(&wasm_path, b"\0asm\x01\0\0\0").unwrap();
        let bytes = load_wasm_from_worker_dir(dir.path(), "index.wasm").unwrap();
        assert_eq!(bytes.len(), 8);
    }

    #[test]
    fn compiles_wat_file_inside_worker_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.wat"), "(module)").unwrap();
        let bytes = load_wasm_from_worker_dir(dir.path(), "index.wat").unwrap();
        assert_eq!(&bytes[..4], b"\0asm");
    }
}
