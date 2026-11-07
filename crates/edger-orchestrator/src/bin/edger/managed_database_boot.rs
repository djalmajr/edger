use std::path::{Component, Path, PathBuf};

use edger_orchestrator::ManifestIndex;

fn materialize(path: &Path) -> anyhow::Result<PathBuf> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(path.canonicalize()?)
}

// The unknown future worker is one abstract component. While it remains on
// the stack the existing scan root covers the result; after it is cancelled,
// the rest of the path is resolved by the filesystem from the known root.
fn future_cache_root(scan_root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
    let mut depth = 1usize;
    let mut components = relative.components();
    for component in components.by_ref() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth == 0 {
                    return materialize(&scan_root.join(components.as_path()));
                }
            }
            _ => anyhow::bail!("relative cache path has a root component"),
        }
    }
    Ok(scan_root.to_path_buf())
}

pub(super) fn protection_roots(
    users: &[PathBuf],
    core: &Path,
    overlay: &Path,
    index: &ManifestIndex,
) -> anyhow::Result<Vec<PathBuf>> {
    let cwd = std::env::current_dir()?;
    let mut roots = Vec::new();
    let mut future_roots = Vec::new();
    for user in users {
        let root = user.canonicalize()?;
        future_roots.push(root.clone());
        roots.push(root);
    }
    roots.push(core.canonicalize()?);
    let overlay = materialize(overlay)?;
    future_roots.push(overlay.clone());
    roots.push(overlay);

    let mut workers = Vec::new();
    for worker in index.worker_refs() {
        let root = worker.dir.canonicalize()?;
        workers.push(root.clone());
        roots.push(root);
    }

    let deno = std::env::var("DENO_DIR").ok();
    let home = std::env::var("HOME").ok();
    let cache = std::env::var("EDGER_DENO_CACHE_ROOT").ok();
    for path in edger_isolation::deno_cache_protection_roots(
        deno.as_deref(),
        home.as_deref(),
        None,
        cache.as_deref(),
    ) {
        if path.is_absolute() {
            roots.push(materialize(&path)?);
        } else {
            roots.push(materialize(&cwd.join(&path))?);
            for worker in &workers {
                roots.push(materialize(&worker.join(&path))?);
            }
            for root in &future_roots {
                roots.push(future_cache_root(root, &path)?);
            }
        }
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn future_worker_cache_is_covered_before_and_after_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let scan = tmp.path().join("scan");
        std::fs::create_dir(&scan).unwrap();
        let scan = scan.canonicalize().unwrap();
        assert_eq!(
            future_cache_root(&scan, Path::new("nested/../cache")).unwrap(),
            scan
        );
        assert_eq!(
            future_cache_root(&scan, Path::new("nested/../../shared")).unwrap(),
            scan.join("shared").canonicalize().unwrap()
        );
        assert_eq!(
            future_cache_root(&scan, Path::new("../..")).unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn existing_symlink_is_resolved_before_parent_and_modes_are_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        let worker = tmp.path().join("worker");
        let external = tmp.path().join("external");
        std::fs::create_dir(&worker).unwrap();
        std::fs::create_dir(&external).unwrap();
        std::fs::create_dir(external.join("target")).unwrap();
        symlink(external.join("target"), worker.join("link")).unwrap();
        let created = materialize(&worker.join("link/../new-cache")).unwrap();
        assert_eq!(created, external.join("new-cache").canonicalize().unwrap());
        assert_eq!(
            std::fs::metadata(&created).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&created, std::fs::Permissions::from_mode(0o750)).unwrap();
        materialize(&created).unwrap();
        assert_eq!(
            std::fs::metadata(&created).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }
}
