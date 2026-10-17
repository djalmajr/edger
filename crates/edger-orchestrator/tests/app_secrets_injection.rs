//! Injeção de segredos de app no env do processo (EDG-38): o valor chega
//! SÓ no env do processo do worker que declara o nome em `secrets:` —
//! outro worker do mesmo pod não o vê, e um worker estático com
//! `publicEnv` nunca o expõe (o valor nunca passa por `WorkerConfig.env`).
//!
//! Roda com o `deno` real (mesma estratégia de
//! `process_dispatch_integration.rs`).

use std::fs;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{AppSecretResolver, ExecutionKind};
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    app_secrets::AppSecretVault, build_pipeline, load_manifests_from_dirs, load_master_key_file,
    ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

const SECRET_VALUE: &str = "s3cret-byok-injected-4c81";

struct ProcessFactory {
    /// EDG-32: per-worker Deno cache root dentro do tempdir do teste.
    deno_cache_root: std::path::PathBuf,
    /// (EDG-38) resolvedor do cofre (o vault real, com a chave mestra).
    app_secrets: Option<Arc<AppSecretVault>>,
}

impl IsolateFactory for ProcessFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => {
                // fetch / routes / spa all go through the persistent process isolate
                // (SPA serving stays pure-Rust inside it).
                let mut isolate = DenoProcessIsolate::new()
                    .with_per_worker_deno_cache_root(self.deno_cache_root.clone());
                if let Some(vault) = &self.app_secrets {
                    let resolver: Arc<dyn AppSecretResolver> = vault.clone();
                    isolate = isolate.with_app_secrets(worker_ref.name.clone(), resolver);
                }
                Box::new(isolate)
            }
        }
    }
}

fn state(root: std::path::PathBuf, app_secrets: Option<Arc<AppSecretVault>>) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(ProcessFactory {
            deno_cache_root: root.join(".edger-test-deno-cache"),
            app_secrets,
        }),
    );
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

async fn dispatch(app: Router, uri: &str) -> (StatusCode, String) {
    let res = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn worker(root: &std::path::Path, name: &str, manifest: &str, files: &[(&str, &str)]) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.yaml"), manifest).unwrap();
    for (file, contents) in files {
        fs::write(dir.join(file), contents).unwrap();
    }
}

/// O worker lê a variável do próprio env: presente → o valor; ausente →
/// "ABSENT".
const ENV_READER: &str = r#"Deno.serve(async () => {
  const byok = Deno.env.get("BYOK_MASTER_KEY");
  const missing = Deno.env.get("MISSING_SECRET");
  return Response.json({
    byok: byok ?? "ABSENT",
    missing: missing ?? "ABSENT",
  });
});"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn injection_reaches_only_the_worker_that_declares_the_secret() {
    let root = tempfile::tempdir().unwrap();

    // Cofre real: chave mestra em arquivo, banco no tempdir.
    let key_file = root.path().join("secrets-key");
    fs::write(&key_file, [7u8; 32]).unwrap();
    let key = load_master_key_file(&key_file).unwrap();
    let vault = AppSecretVault::open(root.path().join("app-secrets.db"), key).unwrap();
    vault.set("pinar", "BYOK_MASTER_KEY", SECRET_VALUE).unwrap();
    // `MISSING_SECRET` NÃO existe no cofre: declarada, ausente — o worker
    // sobe normalmente sem a variável (o Pinar trata como "indisponível").

    // `pinar` declara os dois segredos; `hello` declara NENHUM.
    worker(
        root.path(),
        "pinar",
        "name: pinar\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nsecrets:\n  - BYOK_MASTER_KEY\n  - MISSING_SECRET\n",
        &[("index.ts", ENV_READER)],
    );
    worker(
        root.path(),
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        &[("index.ts", ENV_READER)],
    );
    // `second` declara o MESMO nome `BYOK_MASTER_KEY` — mas o par
    // (worker, nome) dele não existe no cofre: não recebe nada do par do
    // `pinar` (a chave da injeção é (worker, nome), não só o nome).
    worker(
        root.path(),
        "second",
        "name: second\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nsecrets:\n  - BYOK_MASTER_KEY\n",
        &[("index.ts", ENV_READER)],
    );
    // `panel` é estático (spa) e DECLARA o segredo: o serviço de HTML é
    // puro-Rust e nunca injeta, e o `public_runtime_env` só lê
    // `config.env` — que nunca carrega o valor (o valor só existe no env
    // do processo Deno, no spawn).
    worker(
        root.path(),
        "panel",
        "name: panel\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: spa\nsecrets:\n  - BYOK_MASTER_KEY\n",
        &[(
            "index.html",
            "<!doctype html><html><head></head><body>panel</body></html>",
        )],
    );

    // Nota da revisão da S1 (achado 3): o NOME do segredo NUNCA pode
    // aparecer no `public_env` do MESMO manifest — a validação do load
    // recusa (o valor chegaria ao browser). Prova do load-time:
    let leaky_root = tempfile::tempdir().unwrap();
    let leaky = leaky_root.path().join("leaky@1.0.0");
    fs::create_dir_all(&leaky).unwrap();
    fs::write(
        leaky.join("manifest.yaml"),
        "name: leaky\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: spa\nsecrets:\n  - BYOK_MASTER_KEY\npublic_env:\n  - BYOK_MASTER_KEY\n",
    )
    .unwrap();
    fs::write(
        leaky.join("index.html"),
        "<!doctype html><html><head></head><body>x</body></html>",
    )
    .unwrap();
    let err = edger_orchestrator::load_manifests_from_dirs(&[leaky_root.path().to_path_buf()])
        .unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(
        err.message.contains("publicEnv"),
        "o load deve recusar: {err}"
    );

    let app = build_pipeline(state(root.path().to_path_buf(), Some(Arc::new(vault))));

    // O worker que declara recebe o valor injetado no env do processo.
    let (status, body) = dispatch(app.clone(), "/pinar").await;
    assert_eq!(status, StatusCode::OK, "pinar body: {body}");
    assert!(
        body.contains(&format!("\"byok\":\"{SECRET_VALUE}\"")),
        "the declared secret must reach the worker env: {body}"
    );
    // O segredo ausente não bloqueia o spawn: `missing` continua "ABSENT".
    assert!(body.contains("\"missing\":\"ABSENT\""), "{body}");

    // Outro worker do mesmo pod NÃO vê a variável (env é por processo).
    let (status, body) = dispatch(app.clone(), "/hello").await;
    assert_eq!(status, StatusCode::OK, "hello body: {body}");
    assert!(
        body.contains("\"byok\":\"ABSENT\""),
        "the non-declaring worker must not see the secret: {body}"
    );
    assert!(!body.contains(SECRET_VALUE), "{body}");

    // Outro worker que declara o MESMO nome: o par (worker, nome) do
    // `second` não existe no cofre — ele não recebe o valor do `pinar` e
    // sobe normalmente (declarar não exige que o segredo exista).
    let (status, body) = dispatch(app.clone(), "/second").await;
    assert_eq!(status, StatusCode::OK, "second body: {body}");
    assert!(
        body.contains("\"byok\":\"ABSENT\""),
        "a worker declaring the same name must not receive another worker's value: {body}"
    );
    assert!(!body.contains(SECRET_VALUE), "{body}");

    // Worker estático que declara o segredo: o HTML servido nunca carrega
    // o valor — nem pelo `public_env` (o valor não passa por
    // `WorkerConfig.env`, então o `public_runtime_env` não o alcança).
    let (status, body) = dispatch(app, "/panel").await;
    assert_eq!(status, StatusCode::OK, "panel body: {body}");
    assert!(body.contains("panel"), "{body}");
    assert!(
        !body.contains(SECRET_VALUE),
        "the SPA must not leak the secret: {body}"
    );
}
