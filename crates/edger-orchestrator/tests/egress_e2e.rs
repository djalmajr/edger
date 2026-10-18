//! Guarda de saída ligado ao spawn do processo Deno (EDG-38 S2b), com o
//! `deno` real.
//!
//! O que se prova, de ponta a ponta (pool -> processo persistente -> socket
//! do guarda -> destino TLS local):
//! - um worker com `egress.enabled` e `allowNet: []` faz o pedido do contrato
//!   (`Deno.createHttpClient({ proxy: { transport: "unix" } })` + `fetch` ao
//!   host falso com `x-edger-egress-url`) e recebe a resposta, inclusive em
//!   streaming (o primeiro pedaço chega antes de o destino liberar o resto);
//! - o MESMO worker, tentando `fetch` direto ao destino, recebe `NotCapable`;
//! - um destino recusado dá 403 com `x-edger-egress-error`;
//! - um worker SEM `egress` não tem `EDGER_EGRESS_SOCKET` (nem
//!   `EDGER_EGRESS_HOST`), e um `env` do manifest não consegue forjá-los;
//! - uma versão staged com egress também ganha o socket (versão explícita);
//! - depois de um recycle não sobra task nem socket;
//! - o evento `worker.egress` carrega o id do processo e nunca o header
//!   `Authorization` (nome nem valor);
//! - no modo bridge (`DenoIsolate`) um `env:` do manifest não forja a
//!   presença do guarda (os dois nomes ficam reservados nos dois backends);
//! - o `/metrics` do processo traz os contadores do guarda depois de um
//!   pedido pelo guarda (com Deno) e não traz nada do guarda sem o setter
//!   (sem Deno, nos dois renderizadores do `/metrics`).
//!
//! O destino é um servidor TLS em `127.0.0.1` (CA e certificado gerados em
//! runtime), liberado SÓ pelo `allow_cidrs` do guarda de teste. Nenhuma
//! conexão sai da máquina e nenhum IP interno real é discado.
//!
//! Requer `deno` no PATH. Ignorados por padrão; rode explicitamente:
//! `cargo test -p edger-orchestrator --test egress_e2e -- --include-ignored`

use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use bytes::Bytes;
use edger_core::egress::EgressCidr;
use edger_core::ExecutionKind;
use edger_isolation::{
    DenoProcessIsolate, EgressListenerContext, EgressListenerSink, EgressServeFuture, WasmIsolate,
};
use edger_orchestrator::observability::{OperationalEventQuery, OperationalStore};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, EgressGuard, EgressGuardConfig,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use http_body_util::BodyExt;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::ServerConfig;
use rustls::sign::CertifiedKey;
use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

/// Valor que o app manda em `Authorization`: nunca pode aparecer em evento.
const BEARER_SECRET: &str = "sk-user-byok-7f3a91c2";

// ---------------------------------------------------------------------------
// PKI e destino TLS de teste
// ---------------------------------------------------------------------------

struct TestPki {
    ca_der: Vec<u8>,
    server_der: Vec<u8>,
    server_key_der: Vec<u8>,
}

fn test_pki() -> &'static TestPki {
    static PKI: OnceLock<TestPki> = OnceLock::new();
    PKI.get_or_init(|| {
        let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "edger-egress-e2e");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);

        let mut server_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        server_params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::from([127, 0, 0, 1])));
        server_params.is_ca = IsCa::ExplicitNoCa;
        server_params
            .distinguished_name
            .push(DnType::CommonName, "edger-egress-e2e-server");
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().unwrap();
        let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();

        TestPki {
            ca_der: ca_cert.der().to_vec(),
            server_der: server_cert.der().to_vec(),
            server_key_der: server_key.serialize_der(),
        }
    })
}

/// Destino TLS: `/ok` responde com `Content-Length`; `/sse` manda o primeiro
/// evento, espera `sse_release` e manda o segundo; `/echo-auth` devolve o
/// valor do `Authorization` recebido (prova que o header chegou ao destino).
#[derive(Clone)]
struct Upstream {
    addr: SocketAddr,
    sse_release: Arc<Notify>,
}

async fn spawn_upstream() -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let pki = test_pki();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = PrivateKeyDer::try_from(pki.server_key_der.clone()).unwrap();
    let certified_key = CertifiedKey::from_der(
        vec![CertificateDer::from(pki.server_der.clone())],
        key,
        &provider,
    )
    .unwrap();
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(StaticCert(Arc::new(certified_key))));
    let acceptor = Arc::new(TlsAcceptor::from(Arc::new(server_config)));
    let upstream = Upstream {
        addr,
        sse_release: Arc::new(Notify::new()),
    };
    let shared = upstream.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let shared = shared.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                serve_upstream_conn(&mut tls, &shared).await;
            });
        }
    });
    upstream
}

#[derive(Debug)]
struct StaticCert(Arc<CertifiedKey>);

impl rustls::server::ResolvesServerCert for StaticCert {
    fn resolve(&self, _client_hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

type ServerTls = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;

async fn serve_upstream_conn(tls: &mut ServerTls, upstream: &Upstream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let mut scratch = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut scratch);
    if !matches!(req.parse(&buf[..end]), Ok(httparse::Status::Complete(_))) {
        return;
    }
    let path = req.path.unwrap_or("/").split('?').next().unwrap_or("/");
    let authorization = req
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("authorization"))
        .map(|header| String::from_utf8_lossy(header.value).into_owned())
        .unwrap_or_default();
    match path {
        "/ok" => write_response(tls, b"hello-egress").await,
        "/echo-auth" => write_response(tls, authorization.as_bytes()).await,
        "/sse" => {
            let head = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
            if tls.write_all(head).await.is_err() {
                return;
            }
            write_chunk(tls, b"data: one\n\n").await;
            upstream.sse_release.notified().await;
            write_chunk(tls, b"data: two\n\n").await;
            let _ = tls.write_all(b"0\r\n\r\n").await;
        }
        _ => write_response(tls, b"nope").await,
    }
    let _ = tls.shutdown().await;
}

async fn write_response(tls: &mut ServerTls, body: &[u8]) {
    let mut out = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    let _ = tls.write_all(&out).await;
}

async fn write_chunk(tls: &mut ServerTls, data: &[u8]) {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    let _ = tls.write_all(&out).await;
    let _ = tls.flush().await;
}

// ---------------------------------------------------------------------------
// O sink do guarda, com contagem de tasks vivas
// ---------------------------------------------------------------------------

/// Envolve o `EgressGuard` real e conta as futures de `serve` vivas: o
/// contador sobe quando o backend pede o serviço e desce quando a future é
/// descartada (a task foi abortada ou terminou).
struct CountingSink {
    guard: EgressGuard,
    live: Arc<AtomicUsize>,
}

struct LiveToken(Arc<AtomicUsize>);

impl Drop for LiveToken {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl EgressListenerSink for CountingSink {
    fn serve(
        &self,
        listener: tokio::net::UnixListener,
        context: EgressListenerContext,
    ) -> EgressServeFuture {
        self.live.fetch_add(1, Ordering::SeqCst);
        let token = LiveToken(self.live.clone());
        let inner = EgressListenerSink::serve(&self.guard, listener, context);
        Box::pin(async move {
            let _token = token;
            inner.await;
        })
    }
}

// ---------------------------------------------------------------------------
// Fábrica de isolates, pipeline e helpers HTTP
// ---------------------------------------------------------------------------

struct ProcessFactory {
    deno_cache_root: PathBuf,
    /// `None` = nenhum serviço de guarda ligado ao backend.
    sink: Option<Arc<dyn EgressListenerSink>>,
}

impl IsolateFactory for ProcessFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => {
                let mut isolate = DenoProcessIsolate::new()
                    .with_per_worker_deno_cache_root(self.deno_cache_root.clone());
                if let Some(sink) = &self.sink {
                    isolate = isolate.with_egress(
                        Arc::clone(sink),
                        worker_ref.name.clone(),
                        worker_ref.version.clone(),
                        worker_ref.namespace.clone(),
                    );
                }
                Box::new(isolate)
            }
        }
    }
}

struct Fixture {
    app: Router,
    /// O mesmo `ServerState` do pipeline (`server::router` serve o outro
    /// renderizador do `/metrics`).
    server: ServerState,
    upstream: Upstream,
    live: Arc<AtomicUsize>,
    events: OperationalStore,
    _root: tempfile::TempDir,
}

/// Cria o ambiente: destino TLS, guarda de teste (só `127.0.0.1` liberado,
/// única raiz de confiança = a CA de teste), pool com Deno real e os workers
/// que `write` instala na raiz. `with_sink = false` liga o backend SEM serviço
/// de guarda.
async fn fixture(with_sink: bool, write: impl FnOnce(&Path, u16)) -> Fixture {
    let upstream = spawn_upstream().await;
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(test_pki().ca_der.clone()))
        .unwrap();
    let events = OperationalStore::default();
    let guard = EgressGuard::new(EgressGuardConfig {
        block_cidrs: Vec::new(),
        allow_cidrs: vec![EgressCidr::parse("127.0.0.1").unwrap()],
        resolver: Arc::new(edger_orchestrator::SystemEgressResolver),
        tls_roots: roots,
    })
    .with_event_store(events.clone());
    let metrics = guard.metrics().clone();
    let live = Arc::new(AtomicUsize::new(0));
    let sink: Option<Arc<dyn EgressListenerSink>> = with_sink.then(|| {
        Arc::new(CountingSink {
            guard,
            live: live.clone(),
        }) as Arc<dyn EgressListenerSink>
    });

    let root = tempfile::tempdir().unwrap();
    write(root.path(), upstream.addr.port());
    let server = ServerState::new_unready();
    if with_sink {
        // Como o boot: os contadores do guarda vão para o `/metrics`.
        server.set_egress_metrics(metrics);
    }
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(ProcessFactory {
            deno_cache_root: root.path().join(".edger-test-deno-cache"),
            sink,
        }),
    );
    server.mark_ready(pool.clone());
    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index: load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    };
    Fixture {
        app: build_pipeline(state),
        server,
        upstream,
        live,
        events,
        _root: root,
    }
}

fn worker(root: &Path, dir_name: &str, manifest: &str, revision: Option<&str>) {
    let dir = root.join(dir_name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.yaml"), manifest).unwrap();
    fs::write(dir.join("index.ts"), WORKER_SOURCE).unwrap();
    if let Some(revision) = revision {
        fs::write(dir.join(".edger-revision"), revision).unwrap();
    }
}

/// `/env`: o que o app vê. `/via?target=`: o pedido do contrato, resposta em
/// streaming. `/direct?target=`: `fetch` direto ao destino.
const WORKER_SOURCE: &str = r#"Deno.serve(async (req) => {
  const url = new URL(req.url);
  const sock = Deno.env.get("EDGER_EGRESS_SOCKET");
  const host = Deno.env.get("EDGER_EGRESS_HOST");
  if (url.pathname === "/env") {
    return Response.json({ sock: sock ?? null, host: host ?? null });
  }
  const target = url.searchParams.get("target");
  if (url.pathname === "/via") {
    const client = Deno.createHttpClient({
      proxy: { transport: "unix", path: sock },
    });
    const res = await fetch(`http://${host}/`, {
      headers: {
        "x-edger-egress-url": target,
        "authorization": "Bearer sk-user-byok-7f3a91c2",
      },
      client,
    });
    return new Response(res.body, {
      status: res.status,
      headers: {
        "x-egress-error": res.headers.get("x-edger-egress-error") ?? "",
        "content-type": res.headers.get("content-type") ?? "text/plain",
      },
    });
  }
  if (url.pathname === "/direct") {
    try {
      const res = await fetch(target);
      return Response.json({ ok: true, status: res.status });
    } catch (err) {
      return Response.json({ ok: false, name: err.name, message: String(err.message) });
    }
  }
  return new Response("not found", { status: 404 });
});
"#;

fn egress_manifest(name: &str, version: &str, port: u16) -> String {
    format!(
        "name: {name}\nversion: \"{version}\"\nentrypoint: index.ts\nkind: fetch\nallowNet: []\negress:\n  enabled: true\n  ports: [443, {port}]\n"
    )
}

async fn send(app: &Router, method: &str, uri: &str) -> axum::http::Response<Body> {
    tokio::time::timeout(
        Duration::from_secs(40),
        app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the request completed (the budget includes the deno spawn)")
    .unwrap()
}

async fn text_of(res: axum::http::Response<Body>) -> (StatusCode, String) {
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn json_of(app: &Router, uri: &str) -> serde_json::Value {
    let (status, body) = text_of(send(app, "GET", uri).await).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    serde_json::from_str(&body).unwrap_or_else(|err| panic!("{uri}: {err}: {body}"))
}

fn target(upstream: &Upstream, path: &str) -> String {
    format!("https://127.0.0.1:{}{path}", upstream.addr.port())
}

async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------
// Testes
// ---------------------------------------------------------------------------

/// O pedido do contrato funciona; o corpo chega em streaming; o mesmo worker
/// não consegue sair direto; um destino recusado dá 403 com o header de erro.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn the_contract_request_streams_and_a_direct_fetch_is_not_capable() {
    let fx = fixture(true, |root, port| {
        worker(
            root,
            "egress-app@1.0.0",
            &egress_manifest("egress-app", "1.0.0", port),
            None,
        );
    })
    .await;

    // O app vê o socket e o host falso.
    let env = json_of(&fx.app, "/egress-app/env").await;
    assert!(
        env["sock"].as_str().unwrap().ends_with("/guard.sock"),
        "{env}"
    );
    assert_eq!(env["host"], "edger-egress.invalid");

    // O pedido do contrato: status, corpo e o Authorization chegou ao destino.
    let ok = target(&fx.upstream, "/ok");
    let (status, body) = text_of(
        send(
            &fx.app,
            "GET",
            &format!("/egress-app/via?target={}", urlencode(&ok)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "hello-egress");
    let echo = target(&fx.upstream, "/echo-auth");
    let (status, body) = text_of(
        send(
            &fx.app,
            "GET",
            &format!("/egress-app/via?target={}", urlencode(&echo)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, format!("Bearer {BEARER_SECRET}"));

    // Streaming: o primeiro evento chega ANTES de o destino liberar o resto.
    let sse = target(&fx.upstream, "/sse");
    let res = send(
        &fx.app,
        "GET",
        &format!("/egress-app/via?target={}", urlencode(&sse)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body();
    let first = next_data(&mut body).await;
    assert_eq!(first, "data: one\n\n", "first piece before the release");
    fx.upstream.sse_release.notify_one();
    let mut rest = String::new();
    while rest.len() < "data: two\n\n".len() {
        rest.push_str(&next_data(&mut body).await);
    }
    assert_eq!(rest, "data: two\n\n");
    // Lê até o fim: o fim limpo do stream devolve o slot do processo.
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(frame) = body.frame().await {
            frame.expect("a clean end of the stream");
        }
    })
    .await
    .expect("the stream ends");

    // O mesmo worker, direto ao destino: NotCapable (o allowNet é vazio; o
    // guarda não abre a rede).
    let direct = json_of(
        &fx.app,
        &format!("/egress-app/direct?target={}", urlencode(&ok)),
    )
    .await;
    assert_eq!(direct["ok"], false, "{direct}");
    assert_eq!(direct["name"], "NotCapable", "{direct}");

    // Destino recusado: nunca é discado (metadados de nuvem), 403 + código.
    let refused = send(
        &fx.app,
        "GET",
        &format!(
            "/egress-app/via?target={}",
            urlencode("https://169.254.169.254/")
        ),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        refused.headers().get("x-egress-error").unwrap(),
        "EGRESS_HOST_BLOCKED"
    );

    // O evento `worker.egress` leva o id do processo (o mesmo do spawn) e
    // nunca o Authorization: nem o valor, nem o nome do header.
    let query = OperationalEventQuery {
        kind: Some("worker.egress".into()),
        limit: Some(100),
        ..Default::default()
    };
    let events = fx.events.query(query).events;
    assert!(
        events.len() >= 4,
        "ok, echo-auth, sse and the refusal: {events:?}"
    );
    for event in &events {
        assert_eq!(event.worker.as_deref(), Some("egress-app"));
        assert_eq!(event.version.as_deref(), Some("1.0.0"));
        let process_id = event.process_id.as_deref().expect("process id");
        assert!(
            process_id.contains('-') && process_id.chars().next().unwrap().is_ascii_digit(),
            "{process_id}"
        );
        let serialized = serde_json::to_string(event).unwrap().to_ascii_lowercase();
        assert!(!serialized.contains(&BEARER_SECRET.to_ascii_lowercase()));
        assert!(!serialized.contains("authorization"));
    }
}

/// O `/metrics` do processo traz os contadores do guarda depois de pedidos
/// pelo guarda (um aceito, um recusado), nos DOIS renderizadores
/// (`pipeline.rs` e `server::router`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn the_metrics_endpoint_carries_the_guard_counters_after_a_call() {
    let fx = fixture(true, |root, port| {
        worker(
            root,
            "egress-app@1.0.0",
            &egress_manifest("egress-app", "1.0.0", port),
            None,
        );
    })
    .await;
    let ok = target(&fx.upstream, "/ok");
    let (status, body) = text_of(
        send(
            &fx.app,
            "GET",
            &format!("/egress-app/via?target={}", urlencode(&ok)),
        )
        .await,
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "hello-egress"));
    let refused = send(
        &fx.app,
        "GET",
        &format!(
            "/egress-app/via?target={}",
            urlencode("https://169.254.169.254/")
        ),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let pipeline_metrics = text_of(send(&fx.app, "GET", "/metrics").await).await.1;
    let server_metrics = text_of(
        edger_orchestrator::router(fx.server.clone())
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await
    .1;
    for (which, metrics) in [("pipeline", pipeline_metrics), ("server", server_metrics)] {
        for line in [
            "edger_egress_requests_total{worker=\"egress-app\",outcome=\"ok\"} 1",
            "edger_egress_requests_total{worker=\"egress-app\",outcome=\"blocked\"} 1",
            "edger_egress_requests_total{worker=\"egress-app\",outcome=\"error\"} 0",
            "edger_egress_response_bytes_total{worker=\"egress-app\"} 12",
        ] {
            assert!(
                metrics.lines().any(|l| l == line),
                "{which} /metrics lacks {line:?}:\n{metrics}"
            );
        }
    }
}

/// Sem o setter (nenhum guarda configurado) o `/metrics` sai como era, sem
/// nada do guarda, nos dois renderizadores; com o setter, as linhas saem.
/// Sem Deno: nenhum worker é invocado.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_metrics_endpoint_has_nothing_of_the_guard_without_the_setter() {
    async fn both(server: &ServerState) -> (String, String) {
        let root = tempfile::tempdir().unwrap();
        let pool = WorkerPool::with_factory(
            PoolConfig::default(),
            Arc::new(ProcessFactory {
                deno_cache_root: root.path().join(".edger-test-deno-cache"),
                sink: None,
            }),
        );
        server.mark_ready(pool.clone());
        let app = build_pipeline(OrchestratorState {
            server: server.clone(),
            pool,
            index: load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap(),
            auth: ControlAuth::with_static_key("test-root"),
        });
        let from_pipeline = text_of(send(&app, "GET", "/metrics").await).await.1;
        let from_server = text_of(
            edger_orchestrator::router(server.clone())
                .oneshot(
                    Request::builder()
                        .uri("/metrics")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await
        .1;
        (from_pipeline, from_server)
    }

    let bare = ServerState::new_unready();
    let (pipeline_metrics, server_metrics) = both(&bare).await;
    for metrics in [&pipeline_metrics, &server_metrics] {
        assert!(!metrics.contains("egress"), "no guard, no line:\n{metrics}");
        // Controle: o resto do `/metrics` está lá.
        assert!(metrics.contains("edger_"), "{metrics}");
    }

    let wired = ServerState::new_unready();
    let counters = edger_orchestrator::EgressMetrics::default();
    counters.record("some-worker", "ok", 5);
    wired.set_egress_metrics(counters);
    let (pipeline_metrics, server_metrics) = both(&wired).await;
    for metrics in [&pipeline_metrics, &server_metrics] {
        assert!(
            metrics.lines().any(
                |l| l == "edger_egress_requests_total{worker=\"some-worker\",outcome=\"ok\"} 1"
            ),
            "{metrics}"
        );
    }
}

/// Sem `egress` não há socket nem variáveis; e um `env` do manifest não forja
/// a presença do guarda. Controle: com `egress` o socket existe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_worker_without_egress_has_no_socket_and_cannot_fake_one() {
    let fx = fixture(true, |root, port| {
        worker(
            root,
            "plain@1.0.0",
            "name: plain\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
            None,
        );
        worker(
            root,
            "forger@1.0.0",
            "name: forger\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nenv:\n  EDGER_EGRESS_SOCKET: /tmp/forged.sock\n  EDGER_EGRESS_HOST: forged.example\n",
            None,
        );
        worker(
            root,
            "guarded@1.0.0",
            &egress_manifest("guarded", "1.0.0", port),
            None,
        );
    })
    .await;

    for name in ["plain", "forger"] {
        let env = json_of(&fx.app, &format!("/{name}/env")).await;
        assert!(env["sock"].is_null(), "{name}: {env}");
        assert!(env["host"].is_null(), "{name}: {env}");
    }
    assert_eq!(fx.live.load(Ordering::SeqCst), 0, "no guard task started");

    let env = json_of(&fx.app, "/guarded/env").await;
    assert!(env["sock"].is_string(), "control: {env}");
    assert_eq!(fx.live.load(Ordering::SeqCst), 1);
}

/// Sem serviço ligado ao backend, a versão com `egress.enabled` sobe normal
/// (warn) e SEM socket nem variáveis.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn without_a_guard_service_the_worker_starts_without_socket_or_vars() {
    let fx = fixture(false, |root, port| {
        worker(
            root,
            "nosink@1.0.0",
            &egress_manifest("nosink", "1.0.0", port),
            None,
        );
    })
    .await;
    let env = json_of(&fx.app, "/nosink/env").await;
    assert!(env["sock"].is_null(), "{env}");
    assert!(env["host"].is_null(), "{env}");
}

/// Uma versão staged com egress também ganha o socket quando invocada com a
/// versão explícita (o spawn não olha se a versão é a default).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_staged_version_with_egress_gets_the_socket_when_pinned() {
    let fx = fixture(true, |root, port| {
        worker(
            root,
            "multi@1.0.0",
            "name: multi\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
            None,
        );
        worker(
            root,
            "multi@2.0.0",
            &egress_manifest("multi", "2.0.0", port),
            Some("revision-2\nstaged=true\n"),
        );
    })
    .await;

    // A default (sem egress) não tem socket.
    let default_env = json_of(&fx.app, "/multi/env").await;
    assert!(default_env["sock"].is_null(), "{default_env}");
    // A staged, com a versão explícita, tem — e serve o pedido do contrato.
    let staged_env = json_of(&fx.app, "/multi@2.0.0/env").await;
    assert!(
        staged_env["sock"]
            .as_str()
            .unwrap()
            .ends_with("/guard.sock"),
        "{staged_env}"
    );
    assert_eq!(staged_env["host"], "edger-egress.invalid");
    let ok = target(&fx.upstream, "/ok");
    let (status, body) = text_of(
        send(
            &fx.app,
            "GET",
            &format!("/multi@2.0.0/via?target={}", urlencode(&ok)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "hello-egress");
}

/// Depois de um recycle não sobra task de serviço nem socket no disco; o
/// processo seguinte ganha um socket novo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_recycle_leaves_no_task_and_no_socket() {
    let fx = fixture(true, |root, port| {
        worker(
            root,
            "recycled@1.0.0",
            &egress_manifest("recycled", "1.0.0", port),
            None,
        );
    })
    .await;

    let first = json_of(&fx.app, "/recycled/env").await;
    let first_sock = PathBuf::from(first["sock"].as_str().unwrap());
    assert!(first_sock.exists(), "the guard socket is on disk");
    assert_eq!(fx.live.load(Ordering::SeqCst), 1, "one guard task");

    let res = send(
        &fx.app,
        "POST",
        "/api/admin/workers/recycled/recycle?version=1.0.0",
    )
    .await;
    let (status, body) = text_of(res).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let live = fx.live.clone();
    wait_for("the guard task to end after the recycle", move || {
        live.load(Ordering::SeqCst) == 0
    })
    .await;
    let gone = first_sock.clone();
    wait_for("the guard socket to leave the disk", move || !gone.exists()).await;

    // O próximo pedido sobe outro processo, com outro socket e outra task.
    let second = json_of(&fx.app, "/recycled/env").await;
    let second_sock = PathBuf::from(second["sock"].as_str().unwrap());
    assert_ne!(first_sock, second_sock);
    assert!(second_sock.exists());
    assert_eq!(fx.live.load(Ordering::SeqCst), 1);
}

/// Quando o bind do socket do guarda falha, o worker sobe normal, sem
/// socket e sem variáveis, e sai UM warn por worker e versão (um segundo
/// spawn da mesma versão não repete). O bind falha de verdade: um `TMPDIR`
/// em que `w.sock` cabe no limite do `sun_path` e `guard.sock` (4 caracteres
/// a mais) não. O cenário roda num processo filho deste mesmo binário, para
/// o `TMPDIR` não vazar para os outros testes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_failed_guard_bind_leaves_no_socket_and_warns_once() {
    // `sun_path`: 104 bytes no macOS, 108 no Linux (o NUL conta).
    let max_socket_path: usize = if cfg!(target_os = "macos") { 103 } else { 107 };
    // <TMPDIR>/edger-uds-XXXXXX/w.sock
    let tmpdir_len = max_socket_path - "/edger-uds-XXXXXX/w.sock".len();
    let base = std::env::temp_dir();
    let base_len = base.as_os_str().len();
    assert!(
        base_len + 2 < tmpdir_len,
        "the test temp dir {} is too long for this scenario",
        base.display()
    );
    let holder = tempfile::tempdir().unwrap();
    let padded_name = "p".repeat(tmpdir_len - holder.path().as_os_str().len() - 1);
    let long_tmp = holder.path().join(padded_name);
    assert_eq!(long_tmp.as_os_str().len(), tmpdir_len);
    fs::create_dir_all(&long_tmp).unwrap();

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "bind_failure_child",
            "--include-ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(BIND_FAILURE_CHILD, "1")
        .env("TMPDIR", &long_tmp)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child failed:\n{stdout}\n{stderr}"
    );
    assert_eq!(
        stderr.matches("egress guard unavailable").count(),
        1,
        "one warn per worker and version:\n{stderr}"
    );
    assert!(stderr.contains("could not be bound"), "{stderr}");
}

const BIND_FAILURE_CHILD: &str = "EDGER_E2E_BIND_FAILURE_CHILD";

/// O corpo do cenário acima; sem a variável de controle é um no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "child of a_failed_guard_bind_leaves_no_socket_and_warns_once"]
async fn bind_failure_child() {
    if std::env::var_os(BIND_FAILURE_CHILD).is_none() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    let fx = fixture(true, |root, port| {
        worker(
            root,
            "unbindable@1.0.0",
            &egress_manifest("unbindable", "1.0.0", port),
            None,
        );
    })
    .await;
    for _ in 0..2 {
        let env = json_of(&fx.app, "/unbindable/env").await;
        assert!(env["sock"].is_null(), "{env}");
        assert!(env["host"].is_null(), "{env}");
        assert_eq!(fx.live.load(Ordering::SeqCst), 0, "no guard task");
        let res = send(
            &fx.app,
            "POST",
            "/api/admin/workers/unbindable/recycle?version=1.0.0",
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        // Espera o recycle terminar o processo antes do próximo spawn.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// O bridge (`EDGER_JS_RUNTIME=bridge`) não tem guarda: mesmo com
/// `egress.enabled` e com `env:` do manifest forjando os dois nomes, o Deno
/// do bridge não vê nenhum deles (o filtro de nomes reservados é o mesmo do
/// runtime de processo). Controle: uma variável comum do mesmo `env:` chega.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn the_bridge_cannot_forge_the_guard_presence() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("index.ts"),
        r#"Deno.serve(() => Response.json({
  sock: Deno.env.get("EDGER_EGRESS_SOCKET") ?? null,
  host: Deno.env.get("EDGER_EGRESS_HOST") ?? null,
  plain: Deno.env.get("PLAIN_VAR") ?? null,
}));"#,
    )
    .unwrap();
    let manifest = edger_core::WorkerManifest {
        name: "bridge-forger".into(),
        entrypoint: Some("index.ts".into()),
        ..edger_core::WorkerManifest::default()
    };
    let mut config = edger_core::parse_worker_config(&manifest);
    config.worker_dir = Some(root.path().to_path_buf());
    config.allow_net = Some(vec![]);
    config.egress = Some(edger_core::EgressSettings {
        ports: vec![443],
        connect_timeout_ms: 5_000,
        headers_timeout_ms: 30_000,
        idle_timeout_ms: 30_000,
        total_timeout_ms: 300_000,
        max_response_bytes: 100 * 1024 * 1024,
        max_concurrent: 8,
    });
    config
        .env
        .insert("EDGER_EGRESS_SOCKET".into(), "/tmp/forged.sock".into());
    config
        .env
        .insert("EDGER_EGRESS_HOST".into(), "edger-egress.invalid".into());
    config.env.insert("PLAIN_VAR".into(), "kept".into());

    let mut isolate = edger_isolation::DenoIsolate::new(edger_isolation::DenoFacade::new());
    let response = edger_core::Isolate::execute_fetch(
        &mut isolate,
        edger_core::SerializedRequest {
            method: "GET".into(),
            uri: "/".into(),
            headers: vec![],
            body: None,
            request_id: "bridge-forger".into(),
            base_href: None,
        },
        &config,
    )
    .await
    .unwrap();
    let seen: serde_json::Value =
        serde_json::from_slice(response.body.expect("a body").as_ref()).unwrap();
    assert!(seen["sock"].is_null(), "{seen}");
    assert!(seen["host"].is_null(), "{seen}");
    assert_eq!(seen["plain"], "kept", "control: {seen}");
}

/// Um segredo resolvido com o nome de uma variável do guarda é descartado no
/// spawn do runtime de processo (o segredo passa pelo mesmo filtro que o
/// `env:` do manifest). Controle: um segredo comum chega.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_secret_named_like_a_guard_variable_is_dropped() {
    struct AnySecret;
    #[async_trait::async_trait]
    impl edger_core::AppSecretResolver for AnySecret {
        async fn resolve_secret(&self, _worker: &str, name: &str) -> Option<String> {
            Some(format!("value-of-{name}"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("index.ts"),
        r#"Deno.serve(() => Response.json({
  sock: Deno.env.get("EDGER_EGRESS_SOCKET") ?? null,
  host: Deno.env.get("EDGER_EGRESS_HOST") ?? null,
  plain: Deno.env.get("PLAIN_SECRET") ?? null,
}));"#,
    )
    .unwrap();
    let manifest = edger_core::WorkerManifest {
        name: "secret-forger".into(),
        entrypoint: Some("index.ts".into()),
        kind: Some("fetch".into()),
        ..edger_core::WorkerManifest::default()
    };
    let mut config = edger_core::parse_worker_config(&manifest);
    config.worker_dir = Some(root.path().to_path_buf());
    config.secrets = vec![
        "EDGER_EGRESS_SOCKET".into(),
        "EDGER_EGRESS_HOST".into(),
        "PLAIN_SECRET".into(),
    ];
    let mut isolate = DenoProcessIsolate::new()
        .with_per_worker_deno_cache_root(root.path().join("cache"))
        .with_app_secrets("secret-forger", Arc::new(AnySecret));
    let response = edger_core::Isolate::execute_fetch(
        &mut isolate,
        edger_core::SerializedRequest {
            method: "GET".into(),
            uri: "/".into(),
            headers: vec![],
            body: None,
            request_id: "secret-forger".into(),
            base_href: None,
        },
        &config,
    )
    .await
    .unwrap();
    let seen: serde_json::Value =
        serde_json::from_slice(response.body.expect("a body").as_ref()).unwrap();
    assert!(seen["sock"].is_null(), "{seen}");
    assert!(seen["host"].is_null(), "{seen}");
    assert_eq!(seen["plain"], "value-of-PLAIN_SECRET", "control: {seen}");
}

async fn next_data(body: &mut Body) -> String {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(20), body.frame())
            .await
            .expect("a body frame arrives")
            .expect("the body did not end")
            .expect("a readable frame");
        if let Ok(data) = frame.into_data() {
            let data: Bytes = data;
            if !data.is_empty() {
                return String::from_utf8_lossy(&data).into_owned();
            }
        }
    }
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}
