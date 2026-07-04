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

pub(crate) fn select_deno_dir(
    worker_dir: &Path,
    mode: DenoCacheMode,
    env_deno_dir: Option<&str>,
    env_home: Option<&str>,
    env_cache_root: Option<&str>,
) -> DenoDirSelection {
    match mode {
        DenoCacheMode::PerWorker => {
            let root = env_cache_root
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
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

    #[test]
    fn per_worker_deno_dir_uses_distinct_paths() {
        let root = "/tmp/edger-test-deno-cache";
        let first = select_deno_dir(
            Path::new("/srv/workers/alpha"),
            DenoCacheMode::PerWorker,
            None,
            None,
            Some(root),
        );
        let second = select_deno_dir(
            Path::new("/srv/workers/beta"),
            DenoCacheMode::PerWorker,
            None,
            None,
            Some(root),
        );

        assert_ne!(first.env_dir, second.env_dir);
        assert!(first.env_dir.unwrap().starts_with(root));
        assert!(second.env_dir.unwrap().starts_with(root));
    }
}
