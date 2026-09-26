//! D36: export de estado online — `GET /api/admin/state/export`.
//!
//! ZIP consistente montado com o processo de pé: roots de usuário, overlay
//! de core, cópia do banco de chaves via `VACUUM INTO` e manifesto com as
//! origens. Exclui estado interno (`.edger/` do topo), o banco bruto e
//! sidecars, transitórios de deploy e symlinks (contados). Só root: 403 com
//! key não-root e 401 sem credencial.

use std::fs;
use std::io::{Read, Write};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_core::{root_principal, CreateApiKeyRequest};
use edger_isolation::WasmIsolate;
use edger_orchestrator::{
    build_pipeline, load_manifests_from_roots, ApiKeyService, ControlAuth, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct WasmFactory;

impl IsolateFactory for WasmFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
    }
}

fn zip_package(files: &[(&str, &str)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn zero_zip(version: &str) -> Vec<u8> {
    zip_package(&[
        (
            "manifest.yaml",
            &format!(
                "name: zero-app\nversion: \"{version}\"\nvisibility: public\nentrypoint: index.ts\nkind: fetch\nhosts:\n  - zero.example\n"
            ),
        ),
        ("index.ts", "export default () => new Response('ok');"),
    ])
}

/// Cenário completo: user root com worker instalado+promovido, overlay com
/// core worker e store de chaves persistido em `.edger/` da user root. O
/// `app` (pipeline) segura o store; o bundled local vive só o boot scan
/// (raiz bundled não entra no export — vem da imagem).
struct ExportHarness {
    overlay: tempfile::TempDir,
    user: tempfile::TempDir,
    non_root_raw_key: String,
}

impl ExportHarness {
    fn new() -> (axum::Router, Self) {
        let bundled = tempfile::tempdir().unwrap();
        let overlay = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();

        // Core worker no overlay (nome reservado do catálogo: cpanel).
        let core_worker = overlay.path().join("cpanel@1.0.0");
        fs::create_dir_all(&core_worker).unwrap();
        fs::write(
            core_worker.join("manifest.yaml"),
            "name: cpanel\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        )
        .unwrap();
        fs::write(
            core_worker.join("index.ts"),
            "export default () => new Response('core');",
        )
        .unwrap();

        // Store de chaves no layout do chart: dentro de `.edger/` da user
        // root (excluída da raiz; entra como `api-keys.db` no zip).
        let db_dir = user.path().join(".edger");
        fs::create_dir_all(&db_dir).unwrap();
        let keys =
            Arc::new(ApiKeyService::open(db_dir.join("api-keys.db")).expect("api keys store"));
        let created = keys
            .create(
                &root_principal(),
                CreateApiKeyRequest {
                    name: "export-scoped".into(),
                    permissions: vec!["workers:read".into()],
                    namespaces: vec!["*".into()],
                    workers: vec!["*".into()],
                    expires_at: None,
                    role: None,
                },
            )
            .expect("key create");

        let index = load_manifests_from_roots(
            &[bundled.path().to_path_buf()],
            Some(&overlay.path().to_path_buf()),
            &[user.path().to_path_buf()],
        )
        .unwrap();
        let server = ServerState::new_unready();
        let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(WasmFactory));
        server.mark_ready(pool.clone());
        let state = OrchestratorState {
            server,
            pool,
            index,
            auth: ControlAuth::with_static_key("test-root").with_key_service(keys.clone()),
        };
        let app = build_pipeline(state);
        (
            app,
            Self {
                non_root_raw_key: created.raw_key,
                overlay,
                user,
            },
        )
    }
}

async fn send_export(
    app: &axum::Router,
    auth: Option<&str>,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let mut builder = Request::builder().uri("/api/admin/state/export");
    if let Some(key) = auth {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body.to_vec(), headers)
}

#[tokio::test]
async fn state_export_zip_has_state_and_excludes_internals() {
    let (app, harness) = ExportHarness::new();
    let user_root = harness.user.path();
    let overlay_root = harness.overlay.path();
    let db_path = user_root.join(".edger").join("api-keys.db");

    // Worker instalado e promovido (ponteiro no `.edger-defaults/`).
    let (status, _, text) = send_admin(
        &app,
        "POST",
        "/api/admin/workers/install",
        zero_zip("1.0.0"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _, text) = send_admin(
        &app,
        "POST",
        "/api/admin/workers/zero-app/promote?version=1.0.0",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Transitórios e estado interno que devem ficar de FORA do zip.
    let swap = user_root.join(".edger-swaps").join("gone@0.9.0");
    fs::create_dir_all(&swap).unwrap();
    fs::write(swap.join("tombstone.json"), "{}").unwrap();
    let install_scratch = user_root.join(".edger-install-abc");
    fs::create_dir_all(&install_scratch).unwrap();
    fs::write(install_scratch.join("partial.ts"), "x").unwrap();
    fs::write(
        user_root
            .join("zero-app")
            .join(".edger-revision-deadbeef.tmp"),
        "tmp",
    )
    .unwrap();
    fs::write(user_root.join(".edger-defaults").join("ptr.tmp"), "tmp").unwrap();
    fs::write(user_root.join(".edger").join("lixo.txt"), "interno").unwrap();
    // Symlink: pulado e contado (nada de seguir destino).
    let link_target = user_root.join("zero-app").join("index.ts");
    std::os::unix::fs::symlink(&link_target, user_root.join("link-to-index")).unwrap();

    let (status, body, headers) = send_export(&app, Some("test-root")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/zip"
    );
    // Revisão P2: o backup (hashes de API keys) nunca pode ser cacheado.
    assert_eq!(
        headers.get("cache-control").unwrap().to_str().unwrap(),
        "no-store"
    );
    let disposition = headers
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"edger-state-")
            && disposition.ends_with(".zip\""),
        "{disposition}"
    );

    let mut names: Vec<String> = Vec::new();
    let mut manifest_json = String::new();
    let mut db_bytes: Vec<u8> = Vec::new();
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body)).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().to_string();
            if file.is_dir() {
                continue;
            }
            match name.as_str() {
                "edger-state.json" => {
                    file.read_to_string(&mut manifest_json).unwrap();
                }
                "api-keys.db" => {
                    file.read_to_end(&mut db_bytes).unwrap();
                }
                _ => {}
            }
            names.push(name);
        }
    }

    // Presenças obrigatórias.
    assert!(names.contains(&"edger-state.json".to_string()));
    assert!(names.contains(&"api-keys.db".to_string()));
    assert!(
        names.contains(&"user-roots/0/zero-app/manifest.yaml".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"user-roots/0/zero-app/index.ts".to_string()));
    assert!(
        names.iter().any(|name| {
            name.starts_with("user-roots/0/.edger-defaults/") && name.ends_with(".json")
        }),
        "pointer de default ausente: {names:?}"
    );
    assert!(names.contains(&"core-overlay/cpanel@1.0.0/index.ts".to_string()));

    // Exclusões: estado interno, banco bruto + sidecars, transitórios.
    for name in &names {
        assert!(!name.contains(".edger/"), "estado interno vazou: {name}");
        assert!(
            !name.contains(".edger-swaps") && !name.contains(".edger-install-"),
            "transitório vazou: {name}"
        );
        assert!(
            !name.ends_with(".tmp") || !name.starts_with("user-roots/0/.edger-defaults/"),
            "tmp de pointer vazou: {name}"
        );
        assert!(
            !name.contains(".edger-revision-"),
            "tmp de revisão vazou: {name}"
        );
    }
    assert!(
        !names.iter().any(|name| name.contains("link-to-index")),
        "symlink vazou: {names:?}"
    );

    // O banco no zip é uma cópia abrível com a key criada.
    let check = tempfile::NamedTempFile::new().unwrap();
    fs::write(check.path(), &db_bytes).unwrap();
    let conn = rusqlite::Connection::open(check.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM api_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);

    let manifest: serde_json::Value = serde_json::from_str(&manifest_json).unwrap();
    assert_eq!(manifest["format"], 1);
    assert!(!manifest["edgerVersion"].as_str().unwrap().is_empty());
    // RFC3339 UTC (offset +00:00 ou Z).
    let created_at = manifest["createdAt"].as_str().unwrap();
    assert!(created_at.contains('T') && (created_at.ends_with('Z') || created_at.contains('+')));
    assert_eq!(
        manifest["userRoots"][0].as_str().unwrap(),
        user_root.to_string_lossy().as_ref()
    );
    assert_eq!(
        manifest["coreWorkerOverlayDir"].as_str().unwrap(),
        overlay_root.to_string_lossy().as_ref()
    );
    assert_eq!(
        manifest["apiKeysDb"].as_str().unwrap(),
        db_path.to_string_lossy().as_ref()
    );
    // Revisão ponto 9: o manifesto grava caminhos ABSOLUTOS (o restore não
    // depende do cwd do processo que fez o export).
    for root in manifest["userRoots"].as_array().unwrap() {
        let path = std::path::PathBuf::from(root.as_str().unwrap());
        assert!(path.is_absolute(), "userRoots não absoluto: {path:?}");
    }
    let overlay = manifest["coreWorkerOverlayDir"].as_str().unwrap();
    assert!(
        std::path::PathBuf::from(overlay).is_absolute(),
        "coreWorkerOverlayDir não absoluto: {overlay}"
    );
    let db = manifest["apiKeysDb"].as_str().unwrap();
    assert!(
        std::path::PathBuf::from(db).is_absolute(),
        "apiKeysDb não absoluto: {db}"
    );
    assert_eq!(manifest["skippedSymlinks"], 1);
}

#[tokio::test]
async fn state_export_requires_root() {
    let (app, harness) = ExportHarness::new();

    // Sem credencial: 401.
    let (status, _, _) = send_export(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Key não-root (mesmo com permissão de leitura): 403.
    let (status, _, _) = send_export(&app, Some(&harness.non_root_raw_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Helper: rota admin com credencial de root.
async fn send_admin(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
) -> (StatusCode, serde_json::Value, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .header("content-type", "application/zip")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, text)
}
