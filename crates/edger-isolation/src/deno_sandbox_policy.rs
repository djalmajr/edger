use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use edger_core::DenoCacheMode;
#[cfg(feature = "multiproc")]
use edger_core::IsolationError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DenoDirSelection {
    pub(crate) env_dir: Option<PathBuf>,
    pub(crate) read_dirs: Vec<PathBuf>,
}

pub(crate) fn deno_network_permission_args(
    manifest_allow_net: Option<&[String]>,
    env_allow_net: Option<&str>,
) -> Vec<String> {
    if let Some(hosts) = manifest_allow_net {
        let hosts = normalize_hosts(hosts);
        return if hosts.is_empty() {
            Vec::new()
        } else {
            vec![format!("--allow-net={}", hosts.join(","))]
        };
    }

    match env_allow_net.map(str::trim) {
        Some("false" | "0" | "none") => Vec::new(),
        Some("") | Some("true") | Some("1") | None => vec!["--allow-net".to_string()],
        Some(hosts) => vec![format!("--allow-net={hosts}")],
    }
}

/// `--allow-net` for a persistent process: the manifest/env hosts plus the
/// dispatch socket. With `guard_socket` (EDG-38: the version has
/// `egress.enabled`) the guard socket and the never-resolving guard host
/// (port 80, the scheme of the guard URL) are added too: Deno checks a
/// proxy call against the DESTINATION host, not the proxy.
#[cfg(feature = "multiproc")]
pub(crate) fn deno_network_permission_args_with_uds(
    manifest_allow_net: Option<&[String]>,
    env_allow_net: Option<&str>,
    socket_path: &Path,
    guard_socket: Option<&Path>,
) -> Vec<String> {
    let args = deno_network_permission_args(manifest_allow_net, env_allow_net);
    if args.iter().any(|arg| arg == "--allow-net") {
        return args;
    }

    let mut permissions = vec![format!("unix:{}", socket_path.display())];
    if let Some(guard_socket) = guard_socket {
        permissions.push(format!("unix:{}", guard_socket.display()));
        permissions.push(format!("{}:80", crate::egress::EGRESS_GUARD_HOST));
    }
    let permissions = permissions.join(",");
    match args
        .first()
        .and_then(|arg| arg.strip_prefix("--allow-net="))
    {
        Some(hosts) => vec![format!("--allow-net={hosts},{permissions}")],
        None => vec![format!("--allow-net={permissions}")],
    }
}

/// EDG-32: an explicitly configured root (pool/factory option) wins over
/// `EDGER_DENO_CACHE_ROOT`, which wins over the shared
/// `${TMPDIR}/edger-deno-cache` fallback. Paths are kept lexical (no
/// absolutizing, no canonicalizing, no I/O besides reading `temp_dir`).
fn per_worker_cache_root(
    config_cache_root: Option<&Path>,
    env_cache_root: Option<&str>,
) -> PathBuf {
    config_cache_root
        .map(Path::to_path_buf)
        .or_else(|| {
            env_cache_root
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| std::env::temp_dir().join("edger-deno-cache"))
}

/// Conservative superset of the Deno cache roots, for PROTECTION only (the
/// SQLD boot vetoes keys under them). It is NOT a permission: never feed it
/// to `read_allowlist` or grant the PerWorker parent to a guest.
///
/// Order: (1) the PerWorker parent root (the same selection the spawn uses);
/// (2) the exact Shared-mode `read_dirs` (`DENO_DIR` when non-blank, else the
/// three `HOME` subpaths, never both). Performs no file or network I/O.
pub fn deno_cache_protection_roots(
    env_deno_dir: Option<&str>,
    env_home: Option<&str>,
    config_cache_root: Option<&Path>,
    env_cache_root: Option<&str>,
) -> Vec<PathBuf> {
    let mut roots = vec![per_worker_cache_root(config_cache_root, env_cache_root)];
    roots.extend(
        select_deno_dir(
            Path::new(""),
            DenoCacheMode::Shared,
            env_deno_dir,
            env_home,
            None,
            None,
        )
        .read_dirs,
    );
    roots
}

pub(crate) fn select_deno_dir(
    worker_dir: &Path,
    mode: DenoCacheMode,
    env_deno_dir: Option<&str>,
    env_home: Option<&str>,
    config_cache_root: Option<&Path>,
    env_cache_root: Option<&str>,
) -> DenoDirSelection {
    match mode {
        DenoCacheMode::PerWorker => {
            let root = per_worker_cache_root(config_cache_root, env_cache_root);
            // The Deno child runs with `current_dir = worker_dir`, so a
            // relative root means `<worker_dir>/<root>` there. Anchor it here
            // so the symlink check, `create_dir_all`, `DENO_DIR` and
            // `--allow-read` all name the same absolute directory.
            let root = if root.is_absolute() {
                root
            } else {
                worker_dir.join(root)
            };
            let deno_dir = root.join(worker_cache_key(worker_dir));
            DenoDirSelection {
                env_dir: Some(deno_dir.clone()),
                read_dirs: vec![deno_dir],
            }
        }
        DenoCacheMode::Shared => {
            if let Some(deno_dir) = env_deno_dir
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return DenoDirSelection {
                    env_dir: Some(PathBuf::from(deno_dir)),
                    read_dirs: vec![PathBuf::from(deno_dir)],
                };
            }

            let read_dirs = env_home
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(default_deno_cache_dirs)
                .unwrap_or_default();
            DenoDirSelection {
                env_dir: None,
                read_dirs,
            }
        }
    }
}

/// Creates the selected `DENO_DIR` for a spawn. In `PerWorker` mode the host
/// picks the final component (`<cache-root>/<worker-cache-key>`) and hands it
/// to the guest through `--allow-read`, so an existing symlink there would
/// give the guest its target (CWE-59): the spawn is refused with a static
/// message (no path, no target) before anything is created or followed.
/// The test is `symlink_metadata` (never `metadata`) and runs before
/// `create_dir_all`; a real directory is reused and an absent one is created.
/// `Shared` keeps the operator's `DENO_DIR` as is.
///
/// Limit: only this component is checked. Ancestors of the cache root and
/// links inside the cache assume an actor with host write access, which is
/// outside the model (the guest only writes in its own workdir); the check
/// and the creation are not one atomic step.
#[cfg(feature = "multiproc")]
pub(crate) fn prepare_deno_dir(
    selection: &DenoDirSelection,
    mode: DenoCacheMode,
) -> Result<(), IsolationError> {
    let Some(dir) = selection.env_dir.as_deref() else {
        return Ok(());
    };
    if mode == DenoCacheMode::PerWorker {
        if let Ok(meta) = std::fs::symlink_metadata(dir) {
            if meta.file_type().is_symlink() {
                return Err(IsolationError::new(
                    "UDS_DENO_DIR_SYMLINK",
                    "the per-worker Deno cache directory is a symbolic link; the worker is not started",
                ));
            }
        }
    }
    std::fs::create_dir_all(dir).map_err(|err| {
        IsolationError::new(
            "UDS_DENO_DIR",
            format!("create DENO_DIR {}: {err}", dir.display()),
        )
    })
}

#[cfg(feature = "multiproc")]
pub(crate) fn read_allowlist(
    worker_dir: &Path,
    workdir: &Path,
    deno_read_dirs: &[PathBuf],
) -> String {
    let mut paths = vec![
        worker_dir.display().to_string(),
        workdir.display().to_string(),
    ];
    paths.extend(deno_read_dirs.iter().map(|path| path.display().to_string()));
    paths.join(",")
}

fn default_deno_cache_dirs(home: &str) -> Vec<PathBuf> {
    vec![
        PathBuf::from(format!("{home}/Library/Caches/deno")),
        PathBuf::from(format!("{home}/.cache/deno")),
        PathBuf::from(format!("{home}/.deno")),
    ]
}

fn normalize_hosts(hosts: &[String]) -> Vec<String> {
    hosts
        .iter()
        .flat_map(|host| host.split(','))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn worker_cache_key(worker_dir: &Path) -> String {
    let worker_name = worker_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_path_segment)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "worker".to_string());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    worker_dir.hash(&mut hasher);
    format!("{worker_name}-{:016x}", hasher.finish())
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use edger_core::{parse_worker_config, DenoCacheMode, WorkerManifest};

    use super::*;

    #[test]
    fn spawn_args_use_manifest_allow_net_hosts() {
        let config = parse_worker_config(&WorkerManifest {
            allow_net: Some(vec![
                "api.example.com".into(),
                "cdn.example.com:443, jsr.io".into(),
            ]),
            name: "net".into(),
            ..Default::default()
        });

        let args = deno_network_permission_args(config.allow_net.as_deref(), Some("false"));

        assert_eq!(
            args,
            vec!["--allow-net=api.example.com,cdn.example.com:443,jsr.io"]
        );
    }

    #[test]
    fn spawn_args_preserve_open_network_without_allow_net() {
        let config = parse_worker_config(&WorkerManifest {
            name: "open-net".into(),
            ..Default::default()
        });

        let args = deno_network_permission_args(config.allow_net.as_deref(), None);

        assert_eq!(args, vec!["--allow-net"]);
    }

    #[cfg(feature = "multiproc")]
    #[test]
    fn spawn_args_keep_internal_socket_when_network_is_restricted() {
        let socket = Path::new("/tmp/edger/worker.sock");

        assert_eq!(
            deno_network_permission_args_with_uds(
                Some(&["collector:4318".into()]),
                None,
                socket,
                None
            ),
            vec!["--allow-net=collector:4318,unix:/tmp/edger/worker.sock"]
        );
        assert_eq!(
            deno_network_permission_args_with_uds(Some(&[]), None, socket, None),
            vec!["--allow-net=unix:/tmp/edger/worker.sock"]
        );
        assert_eq!(
            deno_network_permission_args_with_uds(None, None, socket, None),
            vec!["--allow-net"]
        );
    }

    /// (EDG-38) with the egress guard the guard socket and the fake host
    /// join the dispatch socket and the manifest hosts; the manifest hosts
    /// are untouched and nothing else is added.
    #[cfg(feature = "multiproc")]
    #[test]
    fn spawn_args_add_the_guard_socket_and_host_with_egress() {
        let socket = Path::new("/tmp/edger/w.sock");
        let guard = Path::new("/tmp/edger/guard.sock");

        assert_eq!(
            deno_network_permission_args_with_uds(
                Some(&["ai-gateway.vercel.sh:443".into()]),
                None,
                socket,
                Some(guard)
            ),
            vec![
                "--allow-net=ai-gateway.vercel.sh:443,unix:/tmp/edger/w.sock,unix:/tmp/edger/guard.sock,edger-egress.invalid:80"
            ]
        );
        // `allowNet: []` (declared, empty): only the two sockets and the host.
        assert_eq!(
            deno_network_permission_args_with_uds(Some(&[]), None, socket, Some(guard)),
            vec![
                "--allow-net=unix:/tmp/edger/w.sock,unix:/tmp/edger/guard.sock,edger-egress.invalid:80"
            ]
        );
    }

    /// (EDG-38) without the guard socket nothing changes: no guard entry
    /// appears, whatever the manifest hosts are.
    #[cfg(feature = "multiproc")]
    #[test]
    fn spawn_args_without_egress_never_mention_the_guard() {
        let socket = Path::new("/tmp/edger/w.sock");
        for allow_net in [
            Some(vec!["collector:4318".to_string()]),
            Some(Vec::new()),
            None,
        ] {
            let args =
                deno_network_permission_args_with_uds(allow_net.as_deref(), None, socket, None);
            assert!(
                args.iter()
                    .all(|arg| !arg.contains("guard.sock") && !arg.contains("edger-egress")),
                "{args:?}"
            );
        }
    }

    /// (EDG-38) an open network (`--allow-net` without hosts) already allows
    /// the guard; the arg stays bare.
    #[cfg(feature = "multiproc")]
    #[test]
    fn spawn_args_keep_an_open_network_bare_with_egress() {
        let socket = Path::new("/tmp/edger/w.sock");
        let guard = Path::new("/tmp/edger/guard.sock");
        assert_eq!(
            deno_network_permission_args_with_uds(None, None, socket, Some(guard)),
            vec!["--allow-net"]
        );
    }

    #[test]
    fn per_worker_deno_dir_uses_distinct_paths() {
        let root = "/tmp/edger-test-deno-cache";
        let first = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::PerWorker,
            None,
            None,
            None,
            Some(root),
        );
        let second = select_deno_dir(
            Path::new("/srv/workers/beta"),
            DenoCacheMode::PerWorker,
            None,
            None,
            None,
            Some(root),
        );

        assert_ne!(first.env_dir, second.env_dir);
        assert!(first.env_dir.unwrap().starts_with(root));
        assert!(second.env_dir.unwrap().starts_with(root));
    }

    // EDG-32: the configured root is the first candidate for the per-worker
    // cache, ahead of the environment variable.
    #[test]
    fn per_worker_deno_dir_prefers_configured_root_over_env() {
        let configured = Path::new("/tmp/edg32-configured-deno-cache");
        let selection = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::PerWorker,
            None,
            None,
            Some(configured),
            Some("/tmp/edg32-env-deno-cache"),
        );

        assert_eq!(
            selection.env_dir.unwrap(),
            configured.join(worker_cache_key(Path::new("/srv/workers/alpha")))
        );
    }

    // EDG-32: without the configured root the previous rule is kept
    // (environment variable, then `${TMPDIR}/edger-deno-cache`).
    #[test]
    fn per_worker_deno_dir_without_configured_root_keeps_env_then_temp() {
        let env_root = Path::new("/tmp/edg32-env-deno-cache");
        let from_env = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::PerWorker,
            None,
            None,
            None,
            Some("/tmp/edg32-env-deno-cache"),
        );
        assert_eq!(
            from_env.env_dir.unwrap(),
            env_root.join(worker_cache_key(Path::new("/srv/workers/alpha")))
        );

        let from_temp = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::PerWorker,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            from_temp.env_dir.unwrap(),
            std::env::temp_dir()
                .join("edger-deno-cache")
                .join(worker_cache_key(Path::new("/srv/workers/alpha")))
        );
    }

    // EDG-32: the configured root only applies to per-worker mode; the shared
    // mode keeps its own rule (`DENO_DIR`, then the default cache dirs).
    #[test]
    fn shared_deno_dir_ignores_configured_root() {
        let selection = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::Shared,
            Some("/tmp/edg32-shared-deno"),
            None,
            Some(Path::new("/tmp/edg32-configured-deno-cache")),
            Some("/tmp/edg32-env-deno-cache"),
        );

        assert_eq!(
            selection.env_dir.as_deref(),
            Some(Path::new("/tmp/edg32-shared-deno"))
        );
    }

    // EDG-54: protection roots contract. Explicit inputs only (no global
    // env mutation); the real spawn selection must sit under / equal them.
    type ProtectionCase = (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static Path>,
        Option<&'static str>,
    );

    fn protection_cases() -> Vec<ProtectionCase> {
        let cfg = Some(Path::new("/tmp/edg54-config-root"));
        let mut cases = Vec::new();
        for deno in [None, Some(""), Some("  	"), Some(" /opt/edg54-deno ")] {
            for home in [None, Some(""), Some("   "), Some("/home/edg54")] {
                for config in [None, cfg] {
                    for env in [None, Some(""), Some("  "), Some(" /tmp/edg54-env-root ")] {
                        cases.push((deno, home, config, env));
                    }
                }
            }
        }
        cases
    }

    #[test]
    fn protection_roots_cover_real_spawn_selection_exactly() {
        for (deno, home, config, env) in protection_cases() {
            let roots = deno_cache_protection_roots(deno, home, config, env);
            let shared = select_deno_dir(
                Path::new("/srv/workers/any"),
                DenoCacheMode::Shared,
                deno,
                home,
                config,
                env,
            );
            // Shared read_dirs are exactly the roots after the parent.
            assert_eq!(roots[1..], shared.read_dirs[..], "{deno:?} {home:?}");
            // Every PerWorker read_dir sits under the protected parent.
            for worker in ["/srv/workers/alpha", "beta", "/a/b/gamma delta"] {
                let per = select_deno_dir(
                    Path::new(worker),
                    DenoCacheMode::PerWorker,
                    deno,
                    home,
                    config,
                    env,
                );
                assert_eq!(per.read_dirs.len(), 1);
                assert!(per.read_dirs[0].starts_with(&roots[0]), "{worker}");
                assert_ne!(per.read_dirs[0], roots[0]);
            }
        }
    }

    #[test]
    fn protection_roots_parent_precedence_config_env_temp() {
        let config = Path::new("/tmp/edg54-config-root");
        let r = deno_cache_protection_roots(None, None, Some(config), Some("/tmp/edg54-env"));
        assert_eq!(r[0], config);
        let r = deno_cache_protection_roots(None, None, None, Some("  /tmp/edg54-env  "));
        assert_eq!(r[0], Path::new("/tmp/edg54-env"));
        for blank in [None, Some(""), Some(" 	 ")] {
            let r = deno_cache_protection_roots(None, None, None, blank);
            assert_eq!(r[0], std::env::temp_dir().join("edger-deno-cache"));
        }
    }

    #[test]
    fn protection_roots_deno_dir_is_exclusive_to_home() {
        let r =
            deno_cache_protection_roots(Some(" /opt/edg54-deno "), Some("/home/edg54"), None, None);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1], Path::new("/opt/edg54-deno"));
        assert!(r.iter().all(|p| !p.starts_with("/home/edg54")));

        let r = deno_cache_protection_roots(Some("  "), Some("/home/edg54"), None, None);
        assert_eq!(
            r[1..],
            [
                PathBuf::from("/home/edg54/Library/Caches/deno"),
                PathBuf::from("/home/edg54/.cache/deno"),
                PathBuf::from("/home/edg54/.deno"),
            ]
        );
    }

    #[test]
    fn protection_roots_absent_or_blank_inputs_yield_only_parent() {
        for home in [None, Some(""), Some("  ")] {
            let r = deno_cache_protection_roots(None, home, None, None);
            assert_eq!(r, vec![std::env::temp_dir().join("edger-deno-cache")]);
        }
    }

    #[test]
    fn protection_roots_never_return_home_or_temp_whole_and_keep_lexical_paths() {
        let temp = std::env::temp_dir();
        for (deno, home, config, env) in protection_cases() {
            for root in deno_cache_protection_roots(deno, home, config, env) {
                assert_ne!(root, temp);
                assert_ne!(root, Path::new("/home/edg54"));
            }
        }
        let r = deno_cache_protection_roots(
            Some("rel/./deno/../dir"),
            None,
            Some(Path::new("rel/cache/../root")),
            None,
        );
        assert_eq!(r[0], Path::new("rel/cache/../root"));
        assert_eq!(r[1], Path::new("rel/./deno/../dir"));
    }

    // R63: a relative root is anchored in `worker_dir` (the Deno child's
    // `current_dir`), so the checked path and the granted path are the same
    // absolute directory.
    #[test]
    fn per_worker_relative_root_is_anchored_in_worker_dir() {
        let worker = Path::new("/srv/workers/alpha");
        let selection = select_deno_dir(
            worker,
            DenoCacheMode::PerWorker,
            None,
            None,
            Some(Path::new("rel-cache")),
            None,
        );

        let expected = worker.join("rel-cache").join(worker_cache_key(worker));
        assert!(expected.is_absolute());
        assert_eq!(selection.env_dir.as_deref(), Some(expected.as_path()));
        assert_eq!(selection.read_dirs, vec![expected]);
    }

    #[test]
    fn per_worker_relative_env_root_is_anchored_in_worker_dir() {
        let worker = Path::new("/srv/workers/alpha");
        let selection = select_deno_dir(
            worker,
            DenoCacheMode::PerWorker,
            None,
            None,
            None,
            Some("rel-env-cache"),
        );

        let expected = worker.join("rel-env-cache").join(worker_cache_key(worker));
        assert_eq!(selection.env_dir.as_deref(), Some(expected.as_path()));
        assert_eq!(selection.read_dirs, vec![expected]);
    }

    #[test]
    fn per_worker_absolute_root_is_not_anchored() {
        let worker = Path::new("/srv/workers/alpha");
        let selection = select_deno_dir(
            worker,
            DenoCacheMode::PerWorker,
            None,
            None,
            Some(Path::new("/tmp/edg54-abs-cache")),
            None,
        );

        let expected = Path::new("/tmp/edg54-abs-cache").join(worker_cache_key(worker));
        assert_eq!(selection.env_dir.as_deref(), Some(expected.as_path()));
        assert_eq!(selection.read_dirs, vec![expected]);
    }

    // Control: `Shared` keeps a relative `DENO_DIR` as written (resolved by
    // the child, no symlink check in that mode).
    #[test]
    fn shared_relative_deno_dir_is_unchanged() {
        let selection = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::Shared,
            Some("rel-deno-dir"),
            None,
            None,
            None,
        );

        assert_eq!(
            selection.env_dir.as_deref(),
            Some(Path::new("rel-deno-dir"))
        );
        assert_eq!(selection.read_dirs, vec![PathBuf::from("rel-deno-dir")]);
    }

    #[cfg(all(unix, feature = "multiproc"))]
    mod prepare_dir {
        use super::*;
        use std::os::unix::fs::symlink;

        const CANARY: &str = "canary-secret-content";

        /// Removes a unique relative root a test names under the host cwd, even
        /// when an assertion fails (it only exists if the bug is present).
        struct HostRootCleanup(PathBuf);

        impl Drop for HostRootCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn unique_relative_root(worker: &Path) -> String {
            format!(
                "edg54-relroot-{}",
                worker.file_name().unwrap().to_string_lossy()
            )
        }

        // R63 control: a real directory under `<worker>/<root>/<key>` is reused.
        #[test]
        fn relative_root_real_directory_in_worker_dir_is_reused() {
            let worker = tempfile::tempdir().unwrap();
            let root = unique_relative_root(worker.path());
            let host_root = std::env::current_dir().unwrap().join(&root);
            let _cleanup = HostRootCleanup(host_root.clone());
            let selection = select_deno_dir(
                worker.path(),
                DenoCacheMode::PerWorker,
                None,
                None,
                Some(Path::new(&root)),
                None,
            );
            let dir = selection.env_dir.clone().unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("kept"), "x").unwrap();

            prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap();

            assert_eq!(std::fs::read_to_string(dir.join("kept")).unwrap(), "x");
            assert!(!host_root.exists());
        }

        // R63 control: an absent directory is created in `worker_dir`, and
        // nothing is created under the host cwd.
        #[test]
        fn relative_root_absent_directory_is_created_in_worker_dir_only() {
            let worker = tempfile::tempdir().unwrap();
            let root = unique_relative_root(worker.path());
            let host_root = std::env::current_dir().unwrap().join(&root);
            let _cleanup = HostRootCleanup(host_root.clone());
            let selection = select_deno_dir(
                worker.path(),
                DenoCacheMode::PerWorker,
                None,
                None,
                Some(Path::new(&root)),
                None,
            );
            let dir = selection.env_dir.clone().unwrap();
            assert!(dir.starts_with(worker.path().join(&root)));

            prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap();

            assert!(std::fs::symlink_metadata(&dir).unwrap().is_dir());
            assert!(!host_root.exists());
        }

        // R63: the symlink sits where the child resolves the relative root
        // (`<worker>/<root>/<key>`), nothing exists under the host cwd.
        #[test]
        fn relative_root_symlink_in_worker_dir_is_refused() {
            let worker = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("canary"), CANARY).unwrap();
            let root = unique_relative_root(worker.path());
            let host_root = std::env::current_dir().unwrap().join(&root);
            let _cleanup = HostRootCleanup(host_root.clone());
            let selection = select_deno_dir(
                worker.path(),
                DenoCacheMode::PerWorker,
                None,
                None,
                Some(Path::new(&root)),
                None,
            );
            let dir = selection.env_dir.clone().unwrap();
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
            symlink(outside.path(), &dir).unwrap();

            let err = prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap_err();

            assert_eq!(err.code, "UDS_DENO_DIR_SYMLINK");
            assert!(!host_root.exists());
            assert_eq!(
                std::fs::read_to_string(outside.path().join("canary")).unwrap(),
                CANARY
            );
        }

        fn per_worker(root: &Path, worker: &Path) -> DenoDirSelection {
            select_deno_dir(
                worker,
                DenoCacheMode::PerWorker,
                None,
                None,
                Some(root),
                None,
            )
        }

        // Mutation captured: dropping the `symlink_metadata` refusal makes
        // `create_dir_all` accept the link and the target stays granted.
        #[test]
        fn per_worker_symlink_is_refused_without_touching_the_target() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("canary"), CANARY).unwrap();
            let selection = per_worker(root.path(), Path::new("/srv/workers/alpha"));
            symlink(outside.path(), selection.env_dir.as_ref().unwrap()).unwrap();

            let err = prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap_err();

            assert_eq!(err.code, "UDS_DENO_DIR_SYMLINK");
            assert!(!err.message.contains(CANARY));
            assert!(!err.message.contains(&outside.path().display().to_string()));
            assert!(!err.message.contains(&root.path().display().to_string()));
            assert_eq!(
                std::fs::read_to_string(outside.path().join("canary")).unwrap(),
                CANARY
            );
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        }

        #[test]
        fn per_worker_dangling_symlink_is_refused_and_not_created_through() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("missing");
            let selection = per_worker(root.path(), Path::new("/srv/workers/alpha"));
            symlink(&target, selection.env_dir.as_ref().unwrap()).unwrap();

            let err = prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap_err();

            assert_eq!(err.code, "UDS_DENO_DIR_SYMLINK");
            assert!(!target.exists());
        }

        #[test]
        fn per_worker_real_directory_is_reused() {
            let root = tempfile::tempdir().unwrap();
            let selection = per_worker(root.path(), Path::new("/srv/workers/alpha"));
            let dir = selection.env_dir.clone().unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("kept"), "x").unwrap();

            prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap();

            assert_eq!(std::fs::read_to_string(dir.join("kept")).unwrap(), "x");
        }

        #[test]
        fn per_worker_absent_directory_is_created() {
            let root = tempfile::tempdir().unwrap();
            let selection = per_worker(root.path(), Path::new("/srv/workers/alpha"));
            let dir = selection.env_dir.clone().unwrap();
            assert!(!dir.exists());

            prepare_deno_dir(&selection, DenoCacheMode::PerWorker).unwrap();

            assert!(std::fs::symlink_metadata(&dir).unwrap().is_dir());
        }

        // Control: Shared mode keeps following the operator's DENO_DIR.
        #[test]
        fn shared_deno_dir_symlink_is_unchanged() {
            let base = tempfile::tempdir().unwrap();
            let real = base.path().join("real");
            std::fs::create_dir(&real).unwrap();
            let link = base.path().join("link");
            symlink(&real, &link).unwrap();
            let link_str = link.display().to_string();
            let selection = select_deno_dir(
                Path::new("/srv/workers/alpha"),
                DenoCacheMode::Shared,
                Some(&link_str),
                None,
                None,
                None,
            );

            prepare_deno_dir(&selection, DenoCacheMode::Shared).unwrap();

            assert_eq!(selection.env_dir.as_deref(), Some(link.as_path()));
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }
}
