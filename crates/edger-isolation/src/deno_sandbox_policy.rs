use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use edger_core::DenoCacheMode;

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
            // EDG-32: an explicitly configured root (pool/factory option)
            // wins over `EDGER_DENO_CACHE_ROOT`, which wins over the shared
            // `${TMPDIR}/edger-deno-cache` fallback.
            let root = config_cache_root
                .map(Path::to_path_buf)
                .or_else(|| {
                    env_cache_root
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(|| std::env::temp_dir().join("edger-deno-cache"));
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

fn worker_cache_key(worker_dir: &Path) -> String {
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
}
